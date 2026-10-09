# S9 step 8 — Global `AppState` lock-scope audit

**Scope:** `src-tauri/src/lib.rs` only. Read-only audit; **no code changed** by this
task. This file is the deliverable.

## The bar

`SharedState = Arc<Mutex<AppState>>` is **one** mutex, and `AppState.members` is a
`HashMap` of every open workspace member. Every command reaches its member through
`member(&guard, ..)` / `members.values().next()`, so **any** command that keeps the
global guard alive while it does slow work blocks **every other member's** commands —
including the keystroke-hot `search` / `hybrid_search` / `quick_answer`, which need the
guard only for the microseconds it takes to clone an `Arc` (see below). A 300 ms
filesystem walk in member A's `get_tree` becomes a 300 ms stall on member B's
next-keystroke search. That is the failure this audit hunts for.

## In-repo mitigation templates (already applied to the hot path — copy these)

- **Read template — clone the dedicated read handle, drop the guard, work outside.**
  `search` (lib.rs:1295), `hybrid_search` (1349) and `quick_answer` (4817) hold the
  guard only to `active.search_db.clone()` (an `Arc<Mutex<Db>>` opened read-only, its
  **own** mutex — see the `search_db` field docs at lib.rs:48), then drop it and run the
  FTS/KNN/excerpt work inside `spawn_blocking` on that handle. WAL lets it read
  concurrently with writers. **This is the template referenced by the task.**
- **Write/heavy template — snapshot, drop the guard, run on a detached thread with its
  own `Db::open` handle.** `reindex` (1890), `hydrate_file` (1720), `import_classify`
  (2884), `refresh_knowledge_model` (5058), `start_research` (5690), the initial scan in
  `activate` (895-925), and the `extraction`/`ocr`/`background_hydrate` workers all do
  this. A committed write on the private handle is visible to the global handle's reads.

The offenders below are the commands that do **not** yet follow one of these.

## Findings

| Command (lib.rs) | Heavy work done while holding the global guard | Held-guard duration risk | Proposed fix |
|---|---|---|---|
| **`get_tree`** (1258) | `db.list_files()` (full file-table scan) **+ a full recursive `ignore::WalkBuilder` walk of the project root**, `is_dir()`/`strip_prefix` per entry, building a `FolderInfo` per directory. | **HIGH.** Scales with tree size; a large project is hundreds of ms of disk-walk **under the global lock**. Directly blocks other members' keystroke `search` (which only wants to clone an Arc). Called on every tree refresh. | Apply the **read template**. Snapshot `project.clone()` + `search_db.clone()` under the guard, drop it, run `list_files()` + the `WalkBuilder` walk inside `spawn_blocking`. The walk touches only the filesystem + the read handle. |
| **`review_inbox`** (4003) | `UserState::load` (file IO); `recipe::list` (**directory walk + read/parse every recipe file from disk**); per-recipe `db.list_runs`; `db.runs_with_status`; `db.list_files` (full scan); `db.list_open_review_items`; `db.runs_finished_since`; `db.list_recent_resolved_review_items`. All under one guard. | **HIGH.** Many DB scans **plus** per-recipe disk reads, serialized under the global lock. Grows with recipe + file + run counts. | Apply the **read template**. Snapshot `project`, `search_db.clone()`, `base_dir` under the guard, drop it, run all the recipe/disk/DB reads on the read-only handle in `spawn_blocking`. None of these are writes. |
| **`save_file`** (1790) | `std::fs::write` (write full editor buffer to disk) **+ `scan::refresh_path`** (re-extract + re-index the file — `extract` can take **seconds** on a large doc) + `UserState::load`/`save` (file IO). | **HIGH.** Runs on **every editor save**; the extract/index step is the exact "long CPU + file IO under the lock" case. Freezes all members mid-type. | Needs the **writer** handle, so use the **write template**, not `search_db`. After the `fs::write`, drop the guard and do `scan::refresh_path` on a private `Db::open` handle (mirror the re-index block in `hydrate_file`, lib.rs:1760-1766), then reconcile / emit. The small user-state IO can follow off-lock. |
| **`move_file`** (2606) — **directory case** | Second guard section (2657) holds the lock through `db.remove_folder` **+ `scan::reindex`** — a full clear + recursive rescan of the whole project. | **HIGH.** A folder move triggers a project-wide rescan **under the guard**. | Apply the **write template** — mirror the `reindex` command exactly: after `remove_folder`, spawn the `scan::reindex` on a detached thread with its own `Db::open` and a `reindex_running`-style guard, drop the lock, emit `index-updated` on completion. (The single-file branch — two `refresh_path` calls — is borderline; fix opportunistically.) |
| **`activate`** (482) | Under the guard before it is dropped at 877: `Registry::load`/`save` (file IO), `Db::open` (migration probe), `db.refresh_stored_kinds` (UPDATE over all rows), `db.backfill_extractions` (scan all files + enqueue), `UserState` baseline (`list_files` scan + file IO), plus opening two more `Db` handles and constructing every engine. | **MEDIUM.** One-time **per project open**, but with multiple members it stalls the **already-open** members for the whole migration/backfill/setup. The heavy *initial scan* is already off-lock (895-925); the migrations are not. | Move the one-shot DB migration/backfill (`refresh_stored_kinds`, `backfill_extractions`, baseline) — which only touch the freshly opened handle, not other members' state — off the guard (before acquiring it, or onto the detached scan thread). Keep only the `members.insert` under the lock. |
| **`unread_files`** (4240) / **`mark_all_seen`** (4272) | `db.list_files()` (full scan) + `index_versions` + `UserState::load`/`save` (file IO). | **MEDIUM.** Scales with file count; a full-table scan under the global lock on a UI-triggered call. | Read `list_files()` via a `search_db.clone()` off the guard (read template); the user-state read/write is small and can stay or follow off-lock. |
| **`maybe_generate_digest`** (4628) | `digest::gather(&active.db, since)` — a DB scan gathering a day of activity — under the guard. (The Claude call is **already** off-lock: guard dropped at 4687.) | **LOW–MEDIUM.** Only the `gather` scan is under the lock; runs at most once/day + on open + on manual refresh. | Optional: snapshot + drop, run `digest::gather` on a read-only handle before dropping into the model thread. Low urgency given call frequency. |
| **`delete_file`** (2696) | Second guard section (2711): `deindex_removed` → `db.remove_folder` (bulk delete for a folder) or `refresh_path` (parse for a file). | **LOW–MEDIUM.** Moderate for a large folder; the trash op itself is already off-lock. | Optional: for the folder case, do `remove_folder` on a private handle off the guard. Accept the single-file case as cheap. |
| **`import_commit`** (2942) | Second guard section (2989): a single `scan::refresh_path` (parse one imported file). | **LOW.** One file, bounded. | Accept, or fold into the same write-template cleanup as `save_file` if touched. |
| Chat commands: `list_chats` (5384), `chat_transcript` (5392), `create_chat` (5400), `send_chat_message` pre-drop (5429), `rename_chat` (5485), `set_chat_pinned`/`set_chat_model`/`archive_chat`/`enter_terminal_mode`/`leave_terminal_mode`/`chat_pty_*` | Hold the **global** guard purely to reach `active`, then take a **nested** `active.chat_db.lock()` for a small query (one `get_chat`, append one row, list one chat's messages). `send_chat_message` correctly `drop(guard)` before the heavy `engine.send` (5469). | **LOW.** Small, bounded queries — but they hold the global lock across a nested DB lock, so a burst still serializes against other members. Also a fixed lock-ordering (global → chat_db) worth keeping consistent. | Accept as cheap for now. If revisited, clone `chat_db` (`Arc`) under the guard, drop the guard, then run the small query — removes the nested-lock hold entirely. |
| `read_file` (1655) / `read_file_bytes` (1664) | — | **NONE.** `resolve_path` (1648) takes the guard only to compute a path join and drops it; the `fs::read` runs **after** the guard is gone. | No action — already correct. |
| `save_ingest`/`delete_ingest`/`get_ingest`/`list_ingests`, `record_*`, `set_*_feature`, `run_ingest`, `cancel_run`, `sync_*`, `resolve_review_item`, `mark_seen` | Small recipe-file IO / config writes / engine signals / single-row DB ops under the guard. | **LOW / accept.** Bounded, small. | Accept as cheap. |

## Prioritized follow-up task list

1. **`get_tree` → read template (HIGH).** Snapshot `project` + `search_db.clone()`, drop
   guard, move `list_files()` + the `WalkBuilder` walk into `spawn_blocking`. Highest
   blast radius: a UI-frequent, size-scaling filesystem walk directly contending with
   keystroke search.
2. **`save_file` → write template (HIGH).** Move `fs::write` + `scan::refresh_path`
   (extract/index) off the guard onto a private `Db::open` handle (mirror
   `hydrate_file`). Highest frequency of the heavy offenders — every save.
3. **`review_inbox` → read template (HIGH).** Snapshot + drop, run the recipe disk walk
   and all the DB scans on the read-only handle in `spawn_blocking`.
4. **`move_file` directory case → write template (HIGH).** Reuse the `reindex` command's
   detached-thread + `Db::open` + `*_running` guard pattern for the `scan::reindex`.
5. **`activate` migrations → off-lock (MEDIUM).** Run `refresh_stored_kinds` /
   `backfill_extractions` / baseline against the fresh handle outside the guard; keep only
   `members.insert` under the lock. Removes the cross-member freeze on opening a new member.
6. **`unread_files` / `mark_all_seen` → read the file list off-lock (MEDIUM).** Use a
   `search_db.clone()` for `list_files()`.
7. **`maybe_generate_digest` `gather`, `delete_file` folder case (LOW–MEDIUM).**
   Opportunistic; move the DB scan/bulk-delete to a read/private handle when nearby.
8. **Chat commands nested-lock cleanup (LOW).** Optionally clone `chat_db` and drop the
   global guard before the small query, keeping the global → chat_db ordering consistent.

**One verification build** (per the session constraint) should follow after the fixes
above are implemented, not per-item.

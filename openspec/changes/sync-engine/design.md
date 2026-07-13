# Design: sync-engine

## Context

Changes 1–4 shipped indexing/watching, AI ingests, chats, and the Review
inbox with its stored-item substrate (`review_items`: kind, title, body,
source_ref, payload JSON). All shared project state is text on disk
(§3), so sync is file transport plus conflict review — no app-level
protocol. The prototype fixes the UX: a title-bar dot for sync state, a
Review detail with side-by-side "who changed what" cards and "Ken's
take", and a Settings "Sync & collaboration" card.

## Goals / Non-Goals

**Goals:**
- Git projects sync themselves: pull on focus, commit+push after saves,
  conflicts filed to Review with an AI-drafted merge. Zero git
  vocabulary in default UI copy.
- Shared-drive projects get passive damage detection: conflicted-copy
  files land in Review with plain keep/open choices.
- Everything testable in `cargo test` against local git fixtures and the
  fake claude.

**Non-Goals:**
- Setting up git for the user (init, remotes, auth) — Ken only drives
  repos that already exist; a non-repo project is a shared-drive
  project.
- Rebase, force-push, history surgery, or touching user git config.
- Concurrent-edit divergence detection on shared drives beyond
  conflicted-copy files (needs content hashing history; later).
- "Ask <teammate> in chat" from the prototype's conflict card (needs
  chat identity; later).

## Decisions

1. **Shell out to `git`, all in `sync.rs`.** Ken runs the user's `git`
   binary with their config and credentials — auth, proxies, and LFS
   behave exactly as the user's own git does. Every invocation goes
   through one `run_git(root, args)` helper; the module exposes pure
   verbs (`is_git_repo`, `remote_and_branch`, `pull`,
   `commit_and_push`, `ensure_excludes`) plus a `SyncEngine` worker.
   Commands never prompt: `GIT_TERMINAL_PROMPT=0` so a missing
   credential fails fast into the attention state instead of hanging.
2. **Pull = merge, and a conflicted merge is aborted, not left open.**
   `git pull --no-rebase --no-edit`; on conflict, capture each unmerged
   path's ours/theirs via `git show :2:/:3:` then `git merge --abort`.
   A half-merged working tree full of conflict markers is unusable and
   frightening for the target user; an aborted merge restores their
   files exactly, and the conflict lives on as data in Review. The next
   pull is throttled anyway, so the aborted merge doesn't thrash.
3. **Push path**: debounced 30s after watcher changes (earliest-deadline
   like the ingest engine, so a steady stream can't postpone forever):
   `add -A` → commit `Ken: update knowledge` (skipped when nothing
   staged) → push (skipped when no remote). A rejected push (remote
   moved) pulls once — which may itself file conflicts — then retries
   the push once; a second failure sets `attention` with the stderr as
   plain-language-wrapped detail.
4. **SyncEngine worker (ken-core).** One thread per project, mirroring
   `IngestEngine`: mpsc of `PullNow | SyncNow | Changed(paths) |
   Shutdown`, 200ms tick, reads `project.json` fresh per action so the
   auto toggle applies without restart. Config gate: sync is active when
   the root is a git repo with a remote and `extra.sync.auto != false`.
   Non-git projects skip all git work but still get conflicted-copy
   detection from `Changed`. Notices flow through one callback:
   `State(state, detail)` and `ReviewChanged` — the app layer maps them
   to Tauri events (`sync-state`, `review-changed`).
5. **Conflicts persist in `review_items`** (change 4's substrate), kind
   `conflict`, payload
   `{"path", "ours", "theirs", "draft", "draftStatus"}` with
   `draftStatus: pending | ready | failed`. The engine dedupes on open
   items per path (re-pulling the same conflict doesn't double-file).
6. **AI merge drafts, one at a time.** A dedicated worker thread drains
   a queue of item ids; for each it spawns `claude -p` headless
   (the `run_headless` style: `--output-format json`,
   `--permission-mode acceptEdits`, cwd = project root) with a prompt
   containing both versions and `STAGING_DIR=<root>/.ken/.staging/
   sync-merge/<id>`; the agent writes the complete merged file to
   `STAGING_DIR/<rel path>`, which the worker reads back into the
   payload and deletes. Missing CLI or a failed run → `draftStatus:
   failed`; the item still works (keep mine / take theirs / edit).
   The fake claude's existing STAGING_DIR contract covers this in tests.
7. **Resolution is a write + resolve + let sync flow.**
   `resolve_conflict(itemId, resolution, content?)`: `accept-draft`
   (falls back to ours when no draft), `keep-mine` (ours), `take-theirs`
   (theirs), `manual` (explicit content). The chosen text is written to
   the project file, indexed immediately, and the item resolved; the
   watcher-driven push picks it up — no special git path. "Edit
   manually" in the UI is `accept-draft` + open in Files.
8. **Conflicted-copy detection is a pure function.**
   `conflicted_copy_original(file_name)` matches a parenthesized marker
   segment whose content contains "conflicted copy" (Dropbox, incl.
   "(Bob's conflicted copy 2026-07-12)") or starts with "case conflict",
   case-insensitively, and returns the name with the segment stripped.
   Parentheses are required — bare substrings like
   "conflicted-copy-analysis.md" or OneDrive's "-<Hostname>" suffix are
   too false-positive-prone. Detection runs over watcher/scan changed
   paths in the engine; items are kind `conflict-copy`, payload
   `{"copyPath", "originalPath"}` (originalPath null when the stripped
   name doesn't exist on disk). Actions: `keep-copy` (copy content over
   the original — or rename when there is no original — then delete the
   copy), `keep-original` (delete the copy), open both (frontend-only).
9. **Inbox passthrough.** `review_inbox` now surfaces a stored item's
   real kind when it is `conflict` / `conflict-copy` (else `stored` as
   before) and carries `payload` on the DTO. Severity order becomes
   approval > conflicts > stored > broken-recipe > failed-file > stale.
   Ids stay `item-<rowid>` so the client's `numericId` keeps working.
10. **Sync state surface.** `sync-state` event `{state, detail?}`:
    `off` (not git / auto off), `synced`, `syncing` (pull or push in
    flight), `attention` (conflicts filed or push/pull failed; detail is
    plain language). Title-bar dot: green, pulsing accent, `--danger`
    with tooltip. Scan/index state keeps priority-merging into the same
    dot (busy while indexing).
11. **Settings card** per prototype: git chip + "origin main · updates
    flow automatically · conflicts go to Review" (remote/branch in mono
    as secondary detail — the one place git-adjacent words may appear),
    auto toggle (writes `extra.sync = {auto}`), Sync now button
    (forces pull + push, bypassing throttle/debounce). Non-git: "shared
    drive" chip + "Ken watches for conflicting copies — they land in
    Review."

## Risks / Trade-offs

- [`git add -A` commits everything, including junk the user dropped in]
  → acceptable: the folder *is* the knowledge base and text-only by
  design; excludes keep Ken's own transient files out. Users with
  `.gitignore` get it honoured for free.
- [Merge commits need author identity; fresh machines may lack
  `user.name`] → commit failure surfaces as `attention` with the
  underlying message; Ken never writes config itself. Test repos set
  local identity explicitly.
- [Abort-then-refile loses the merge's non-conflicting hunks until the
  next pull] → the next successful pull (after resolution + push) merges
  them; correctness is preserved because resolution content is committed
  on top.
- [Draft worker holds full file contents in payload JSON] → knowledge
  files are small text; payloads live in the local DB only.
- [Focus-pull can race the push debounce] → both funnel through the one
  engine thread; git operations are serialized by construction.

## Migration Plan

No schema change (payload column exists since v4). New `project.json`
`sync` key is additive and optional — older Ken versions preserve it via
`extra` passthrough. Rollback = git revert; review items of unknown kinds
degrade to `stored` in older frontends.

## Open Questions

None blocking. Backoff tuning for failed pushes and shared-drive
divergence detection are follow-ups.

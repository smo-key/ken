# Tasks: ken-families

## 1. ken-core

- [x] 1.1 `family.rs`: `FamilyManifest` (serde, `#[serde(default)]`,
      flattened `extra`, `template: u32` schema version with a
      supported-version check — newer than supported ⇒ typed
      "needs a newer Ken" error, no sync), member ids, pure
      `lane_check(member_id, path, is_new_file) -> Result<(), LaneViolation>`
      implementing D3 rules 1–3. Unit tests: own lane, foreign
      inbox new-file, foreign inbox edit (deny), foreign board
      (deny), manifest member append, newer-template refusal.
      — Typed error is `UnsupportedTemplate { found, supported }`
      (its own type + `From<..> for Error`, since `error.rs` is
      another session's file and a string in `Error::Other` would
      leave the "no sync/ingest/write" decision to string matching).
      `check_supported()` is deliberately separate from `parse()`:
      reading the manifest is how you *discover* the version.
      A missing `template` key reads as 1, not 0 (tolerance on the
      read side only). `lane_check` is exactly D3 rules 1–3 and
      therefore **denies `shared/`**; the two cases a
      `(member_id, path, is_new)` triple cannot express got
      constructors on a `Lane` struct instead of extra parameters:
      `Lane::owner` (D3 makes the manifest's first member the single
      writer of `shared/` — a manifest fact, not a path fact) and
      `Lane::bootstrap` (the create-family template commit into an
      empty repo; new files only, so it can never edit an existing
      repo). Rule 3's *content* half — "append-only" — is
      unexpressible from a path, so it is a second pure function,
      `manifest_append_only(before, after)`. Member ids are
      path-safety-critical, so `normalize_member_id` is its own
      function rather than `tasks::slugify`, whose `"task"` fallback
      would silently name a person "task" and let two unnamed
      members collide on one id.
- [x] 1.2 Inbox item type: parse/serialize via the ken-tasks
      frontmatter patch core (reuse, don't fork); tolerant parse —
      unknown keys and malformed frontmatter surface as raw items,
      never errors. Round-trip property tests.
      — Serialization goes through `tasks::patch_text("", edits, body)`
      (empty input ⇒ the core builds the whole block), with
      `tasks::scalar_lines`/`seq_lines` for every value including the
      nested `task:` payload, which is just those same renderers
      indented two spaces — so quoting rules can't drift between a
      created item and a patched one. Verified that `patch_text`'s
      multi-line-extent consumption is safe here: it stops at the
      next column-0 key, so patching `status` never swallows a
      following `task:` block whatever the key order. `status`
      transitions reuse `tasks::apply_edits` on disk, inheriting S6's
      optimistic-concurrency retry. No `proptest` in this workspace's
      dependency tree (and Cargo.toml belongs to no one this phase),
      so the round-trip "property" test is a hand-rolled sweep:
      3 kinds x 6 awkward titles x 4 bodies x 4 status transitions.
      Parse mirrors `tasks::parse_frontmatter`'s ladder (typed →
      tolerant `Mapping` → raw) and can never return `Err`.
- [x] 1.3 `family_sync.rs`: `trait GitTransport { fetch, integrate,
      commit_paths, push, head_status }`; `SystemGit` shelling to
      `git` (detect on PATH once, cache result); `FakeTransport`
      with an in-memory remote for tests.
      — `commit_paths` is a **provided** trait method (lane check +
      delegate to a required `write_and_commit` primitive) so no
      implementation can forget the check; see 1.4. Added `exists`
      and `read` to the trait: `exists` is what makes `is_new` ground
      truth instead of caller intent. `git_available()` caches the
      `git --version` probe in a `OnceLock` and returns the failure
      reason verbatim for the "unavailable {reason}" state.
      S8's per-clone config is `CLONE_CONFIG` + `clone_config_args()`
      *and* `configure_clone()` — both halves are required, and the
      real-git test proved why: applying `core.autocrlf=false` only
      after a checkout made with the machine's global
      `autocrlf=true` makes every file look modified and
      `git pull --rebase` then refuses to run outright ("You have
      unstaged changes"). S8 called autocrlf cosmetic; on the
      config-flip path it is a hard stop.
- [x] 1.4 Sync loop as a pure state machine over `GitTransport`:
      poll → fetch → rebase-integrate → push pending; non-fast-
      forward push retries once after re-integrate; any conflict ⇒
      `ConnectionState::Conflict` and loop halt. `commit_paths`
      calls `lane_check` on every staged path. Tests against
      `FakeTransport` incl. concurrent-sender inbox delivery and
      the conflict-halt path.
      — `SyncEngine` holds only a `ConnectionState`; src-tauri owns
      the interval and the clock. Halted (`Conflict`/`Unavailable`)
      short-circuits before any transport call — asserted by a fetch
      counter — and also refuses `commit`, so no new local commits
      pile up on a clone the user is about to resolve by hand. Only
      `resolved()` clears a conflict, and it deliberately does *not*
      clear `Unavailable`. A second push rejection is `Error`
      (transient, next poll retries), not a third try and not a halt.
      Batch commits are all-or-nothing: one bad path refuses the
      whole batch, including the legal paths in it.
      **S8/D1 refinement, recorded:** on conflict `SystemGit` runs
      `git rebase --abort` before reporting. That is S8's settled
      recovery step, not auto-resolution — the local commits come
      back untouched, nothing is pushed, the remote is never
      touched — and without it the clone is left mid-rebase where
      every subsequent git command (including the ones a user needs
      to inspect it) fails.
- [x] 1.5 Accept flow (pure): inbox task item + member id → new
      board task file content (fresh ULID, provenance log line) +
      patched inbox item (`accepted`). Tests.
      — Returns both file contents (`AcceptedTask`) rather than
      writing, so the caller lands them in one commit; both paths
      are inside the accepting member's own lane, so accept can
      never fail a lane check. Provenance uses
      `tasks::compose_log_entry` (same `## Log` shape as
      `task_complete`). Accepted work lands in `backlog`, the intake
      column — acceptance means "this is mine now", not "I started
      it". Refuses malformed items, non-task kinds, already-accepted
      items, and an invalid member id. `push_back_item` is here too
      (D4): a push-back is a *new* message in the sender's inbox,
      lane rule 2, never a modification of their file.
- [x] 1.6 Built-in tier rule set for family members (`shared/**`
      full; `members/**`, `family.json` search-only) registered in
      `kenignore.rs`'s rule-set mechanism.
      — **Seam decision: a separate `family::family_builtin_rules()`,
      not `kenignore::built_in_rule_sets()`** — the same call the
      ken-memory 1.6 note made, for the same reason and now with a
      second data point. `built_in_rule_sets()` is parameterless and
      `scan.rs` folds it into *every* project's classify call, so
      these patterns would leak onto ordinary member projects:
      `members/**` search-only would demote any repo that happens to
      have a `members/` folder, and `shared/**` full would *override
      a user's own `.kenignore`* for any repo with a `shared/` folder
      (built-ins fold before user rules, so a promotion to Full there
      is not something a user `~` line can take back — worse than
      memory's case, which only demoted). These rules are scoped to
      one clone of one family, which a parameterless every-project
      hook cannot express. `kenignore.rs`'s only change is its doc
      comment: it was still claiming ken-memory/ken-tasks "do not
      exist in this codebase yet", so it now names both per-member
      seams and explains why the global hook stays empty.
- [x] 1.7 Template scaffold: pure function producing the initial
      file set (manifest with `template: 1`, member folders,
      README stub and `conventions.md` — the Ken behavior
      contract: lane rules restated, inbox etiquette, what
      belongs in shared/, how push-back works — in shared/) from
      a name + member list.
      — `scaffold_family(name, id, members) -> Vec<ScaffoldFile>`;
      `id` is caller-supplied (the `memory.rs`/`tasks.rs`
      caller-owns-nondeterminism convention) so the connection store
      knows the family id before the repo exists. Member folders get
      `.gitkeep` files — git tracks files, not folders, so without
      them a fresh clone would have no member areas at all. Refuses
      an empty name, an empty roster, an invalid member id, or a
      duplicate id. `conventions.md` states the lane rules as rules
      ("Ken refuses commits that break it — a refusal is a bug
      report"), leads inbox etiquette with "delivery is not
      assignment", and is asserted on by the scaffold test so it
      can't rot into a stub.

## 2. src-tauri

- [x] 2.1 Connection store in app settings: remote URL, family id,
      my member id, live-sync, poll interval, attached workspace
      id. `kenFamilies` flag gate on all of it.
      — Registered `kenFamilies` in `crates/ken-core/src/features.rs` as
      `FlagScope::Global` (registry count 7->8), NOT AND-ed with
      `workspace_enabled` the way `kenMemory`/`kenTasks`/`federatedKg`/
      `kgRouting` are — the proposal calls it a plain "global flag";
      families are a standalone collaboration bus that works with no
      workspace open, unlike those four's "workspace-level, requires
      workspace". Storage judgment call (recorded in full on
      `FamilyConnection`'s doc comment): `AppSettings` has one structured
      field (`features`) and a flattened `extra` map; adding a typed field
      means editing `settings.rs`, outside this task's touch-boundary
      (owned by another session). Connections live wholesale as one JSON
      array under `AppSettings::extra["familyConnections"]` — the same
      forward-compat channel `extra` already exists for, just repurposed
      as this build's actual storage for a key `settings.rs` itself never
      models. Trade-off: reads are all-or-nothing (one malformed entry
      drops the whole array) rather than ken-tasks/ken-families' own
      per-file tolerance — accepted because this file is Ken-written only,
      never hand-edited or shared with a teammate.
- [x] 2.2 Clone/create/join commands (clone into
      `<app data>/ken/families/<family-id>/`), surfacing git stderr
      on failure; "unavailable" state when git missing.
      — `family_create`/`family_join`. `family_sync.rs` has no seam for
      the *first* `git init`/`git clone`/`git remote add` (D1 pins
      `SystemGit` to driving an already-cloned tree), so a small local
      `family_git` helper runs those three directly, surfacing stderr
      **verbatim** (unlike `family_sync`'s own private, 400-char-capped
      `short_detail`) — task 2.2's literal wording. `family_join` clones
      into a temp folder first (the target path is keyed by the
      manifest's own family id, unknowable before the clone completes),
      then renames into place; a manifest declaring an unsupported
      `template` still creates the connection (in `Unavailable` state via
      `family_engine_initial_state`) rather than failing the join outright
      — matches D2's "connection shows unavailable", and skips the
      member-append write entirely for that case ("no sync, ingest, or
      write runs against the clone"). Known rough edge, not fixed here: an
      existing on-disk clone from a `family_remove`d (forgotten, not
      deleted) connection makes a re-join fail with "already joined"
      rather than adopting it.
- [x] 2.3 Poll scheduler: per-connection timer while flag+live-sync
      on; "Sync now" command; emits events for new inbox items,
      board changes, sync errors.
      — `spawn_family_poll` mirrors `spawn_task_board_watch`'s one-
      thread-per-resource shape (short sleep slices so a stop lands
      quickly, not after a full up-to-30-min interval); `reconcile_
      family_pollers` (not incremental per-call start/stop) recomputes
      the wanted poller set from `kenFamilies` + each connection's
      `liveSync` and is called after every mutation that could change
      either — `set_global_feature`, create/join/remove/set_live_sync/
      set_poll_interval, and once at startup from `.setup()`. One
      `SyncEngine` per family id lives in `AppState::family_engines`,
      created lazily and shared by the poller AND every on-demand command
      (`family_sync_now`, accept/dismiss commits) — a fresh engine per
      call would lose `Conflict`/`Unavailable` between ticks. Event
      shape judgment call: one `family-sync` event
      (`{familyId, report: SyncReport, unreadInboxCount}`) per tick/
      command, app-global (mirrors `board-state`'s "no single owning
      project" choice) — "new inbox items" is `unreadInboxCount` for the
      frontend to diff tick-to-tick rather than a stateful server-side
      diff (out of this task's scope); "board changes" for THIS device's
      own board additionally fires `emit_board_state` (already gated by
      `kenTasks` + workspace-open + `task_homes_scan`'s new family-board
      loop), since a board only changes locally through this device's own
      accept/dismiss actions (D1: one clone per device) or an
      attached-workspace's merged read.
- [x] 2.4 Accept / dismiss commands driving 1.5 and inbox status
      patches; accepted tasks appear via the ken-tasks board
      read path (family board = third home).
      — `family_accept_task` (drives `family::accept_task`, one commit for
      both the new board file and the patched inbox item),
      `family_set_item_status` (seen/archived via `family::set_status_
      text` + commit — refuses `accepted`, reserved for `family_accept_
      task` so the board file and the status always land together), and
      `family_push_back` (drives `family::push_back_item`, a lane-rule-2
      create in the sender's inbox; deliberately does NOT also change the
      original item's status — D4 leaves that a separate action). All
      writes go through `GitTransport::commit_paths`, never `family::
      apply_inbox_status`'s direct-disk write — `commit_paths` is
      documented as "the only sanctioned way to write to a family repo",
      and a direct write would leave the change uncommitted until some
      later commit happened to re-stage the same path. Third-home wiring:
      `tasks::TaskHome` has no variant reaching `<clone>/members/<me>/
      board/` (`TaskHome::Project` always resolves to `<project_root>/
      .ken/tasks`, and `tasks.rs` is outside this task's touch-boundary
      to extend), so `task_homes_scan`/`board_state_dto` gained `base_dir`
      + `app_settings` parameters (threaded through all 9 call sites) and
      a new `list_family_board_tasks` helper that calls the SAME `tasks::
      parse_task` `tasks::list_tasks` itself uses — only the tiny
      non-recursive directory-listing glue is duplicated, not the
      frontmatter parser. Reuses `HomeKind::Project` (not a new `Family`
      variant, also outside the touch-boundary) with the family's display
      name as `default_project`.
- [x] 2.5 Attach-to-workspace: register/unregister the clone as a
      `kind: family` member with the ingest engine; tier rules
      from 1.6 apply.
      — `activate_family_pseudo_member`, same shape as ken-memory's
      `activate_memory_pseudo_member`: a real `Project` rooted at the
      clone directory itself, id = the manifest's own family id (so
      `ken://<family-id>/<rel-path>` addressing and `list_projects`'
      `kind: "family"` fall out of existing per-project machinery), with
      `family::family_builtin_rules()` written out as a generated
      `.kenignore` through the same disk-read channel (`render_builtin_
      kenignore` -> `Project::kenignore_rules()` -> `scan::scan`) —
      required because `family_builtin_rules()` is deliberately NOT
      folded into `kenignore::built_in_rule_sets()` (1.6's own note: that
      hook is parameterless and applies to every project). The generated
      `.kenignore`/`.ken/project.json` are never staged by any
      `PendingWrite`, so `write_and_commit`'s explicit-paths-only `git
      add` never commits them — the "no-human repo" guarantee holds.
      Attach/detach persist immediately (`family_attach_workspace`/
      `family_detach_workspace`); attach also activates right away if the
      target workspace is the one currently open, and `open_workspace_
      inner` gained its own loop (mirroring the `kenMemory` block
      immediately above it) so attached connections activate on every
      later open too. Detach removes the `MemberRuntime` from `AppState::
      members`, running its `Drop` impls (workers, watcher) same as any
      other member close.

## 3. ken-mcp

- [x] 3.1 `family_list()` — connections, members, sync state.
      — read-only, no fetch; `head_status` reports ahead/behind/
      dirty/rebase from the local clone.
- [x] 3.2 `family_inbox(family?)` — my items with status.
      — read-only; never transitions an item's status (the no-auto-
      accept lock applies to reads too: looking is not accepting).
- [x] 3.3 `family_send(family, to, kind, title, body, task?)` —
      lane-checked write + push; documents that acceptance is the
      recipient's, delivery ≠ assignment.
      — writes go through `commit_paths`, so the new-file-in-foreign-
      inbox lane is checked by core rather than asserted by the tool.
      Both the description and the result message repeat that
      delivery is not assignment.
- [x] 3.4 Family boards join the existing task claim-and-complete
      tools (claims restricted to own board by lanes).
      — every manifest member's board is READ (team visibility) but
      writes call `family::lane_check` before anything touches disk,
      so a claim aimed at a teammate's board is refused by the lane
      rules, not by convention;
      `task_update_lane_refuses_a_foreign_board_write_but_allows_own_board`
      proves it. Successful own-board writes commit+push via
      SyncEngine, degrading to a warning suffix on push failure.
      Deviations: `TaskHome`/`Task::address` (owned by ken-tasks,
      out of boundary) cannot express `members/<id>/board/`, so
      family boards are scanned with the same public `parse_task`
      primitive and addressed locally as
      `ken://<family-id>/members/<id>/board/<file>` per D6 — see the
      TaskHome::Family follow-up. An optional `as` argument was added
      to disambiguate identity in a multi-member family; every
      on-disk connection contributes board homes regardless of
      workspace attachment (known simplification).
      `task_create` was deliberately NOT extended to family boards:
      per D4 board tasks originate only via accept.

## 4. Frontend

- [x] 4.1 Settings → Families page: connection list with state,
      create/join flows, live-sync + interval controls, attach
      picker.
      — new `src/lib/families.svelte.ts` store (mirrors `memory.svelte.ts`)
      + a `{#if families.enabled}` section inline in `SettingsScreen.svelte`
      (Memory's own placement pattern). Conflict shows a "resolve conflict"
      button (`family_resolve_conflict`); Unavailable shows the reason with
      no action, git-specific reasons get an install-git hint. Attach
      picker judgment call: the app only ever has one workspace open at a
      time and there is no list-workspaces command, so it offers
      "Attach to '<open workspace>'" / "Detach" against `app.workspace`
      only, not a picker across multiple workspaces.
- [x] 4.2 Notification tray: unread badge, grouped items, inline
      accept/dismiss.
      — `src/family/FamilyTray.svelte`, a nav-rail popover (pattern copied
      from `WorkspaceSwitcher.svelte`) triggered by a bell button in
      `NavRail.svelte`, badged with `families.totalUnread`. Groups by
      family; each item offers Accept (`family_accept_task`, task kind
      only), Push back (`family_push_back`, inline note field), Dismiss
      (`family_set_item_status` → archived). Accept is never called except
      from that one button's click handler — no auto-accept path exists.
- [x] 4.3 Tasks tab: per-family filter chip; family tasks render
      with a family marker; drag-drop status writes route to the
      family home.
      — Verified `task_homes_scan` (src-tauri/src/lib.rs) reuses
      `HomeKind::Project` for family boards (no `Family` variant exists),
      so `Task.home` can't distinguish them; `Task.homeDir` (the real
      scanned directory, `families/<id>/members/<member>/board`) is the
      only field that does — `families.familyForTask` matches on that.
      Filter chip is a `<select>` in the existing filters row (same
      pattern as the goal/kind filters), family badge is a chip on
      `TaskCard.svelte`. Drag-drop deviation, not implemented: the
      frontend's `task_update` command writes the file directly with no
      git commit for family homes (confirmed by grep — only `crates/
      ken-mcp`'s task 3.4 claim/complete path calls `commit_paths` for
      board writes; the Tauri `task_update` used by drag-drop does not).
      An uncommitted change would make the next `git pull --rebase` refuse
      ("You have unstaged changes", per S8), silently halting sync for the
      whole connection — a correctness risk, not just a missing feature.
      So family task cards are rendered non-draggable with a tooltip
      explaining why, and `TasksScreen`'s drop handler refuses the same
      write defensively. Deferred: a `family_task_set_status`-shaped Tauri
      command mirroring `family_accept_task`'s commit-then-push, needed
      before drag-drop can safely reach family boards.

## 5. Verification

- [ ] 5.1 Flag off ⇒ byte-identical: no clones, no timers, no
      settings page, no tools registered; existing suites pass
      untouched.
- [ ] 5.2 Two-Ken simulation over `FakeTransport` (and once over a
      real local bare repo): A sends task → B polls, sees unread,
      accepts → task on B's board → B completes → A polls and sees
      done on B's board. No conflicts, no lost writes.
- [ ] 5.3 Lane property test: no reachable code path commits a
      path outside the local member's allowed set.
- [ ] 5.4 git-missing degradation: feature inert, one clear
      unavailable state, no dialogs.
- [ ] 5.5 Attached-family search: a `shared/` doc is findable via
      hybrid search and its entities federate; an inbox item is
      findable but mints no entities (tier check).

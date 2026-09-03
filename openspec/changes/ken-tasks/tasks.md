# Tasks: ken-tasks

## 1. ken-core

- [x] 1.1 `tasks.rs` (new): task frontmatter model (`id` ulid,
      `title`, `status`, `kind`, `assignee`, `project`, `tags`,
      `due?`, `board`, `created`, `updated`; `#[serde(default)]`,
      flattened extras), parse/serialize preserving unknown keys +
      key order + body bytes; filename `<ulid>-<slug>.md` with `id`
      authoritative; register in `lib.rs`
      - Reads use the `#[serde(default)]` + `#[serde(flatten)] extra`
        struct as specified; **writes never touch it** — serialization
        is the raw line splitter (1.2 / S6), so `extra` is read-side
        display only. This is the opposite call from `memory.rs`,
        deliberately: task files are patched key-by-key, memory files
        are whole-file rewrites.
      - Judgment: enum-valued keys (`status`/`kind`/`board`) are typed
        `String` in the serde struct so one bad hand edit degrades to a
        tray entry instead of failing the whole file's parse.
      - Judgment: when the *typed* parse fails (e.g. `tags: a, b`
        written as a scalar), we fall back to a tolerant
        `serde_yaml::Mapping` read rather than `memory.rs`'s
        `unwrap_or_default()` — blanking a whole task file over one
        malformed key is worse than a slightly slower rescue path.
      - Judgment: no `ulid` crate exists in the workspace, so
        `ulid_from_parts`/`new_ulid` encode Crockford base32 over a
        48-bit ms timestamp + 80 bits of `Uuid::new_v4` (already a
        dependency). `NewTask.id` / `NewGoal.id` are caller-supplied
        when present — the same caller-owns-nondeterminism convention
        `memory.rs` uses for `today`; every test passes explicit ids.
      - Judgment: absent `status`/`kind`/`board` read as
        `backlog`/`human`/`main` (the documented defaults); only a
        *present but unrecognized* value is flagged. A file with no
        `id` falls back to its filename stem so it still boards.
      - Judgment: local `slugify` (cap 40, fallback `task`) rather
        than `research::slugify` (cap 50, fallback the literal
        `research`, a confusing task filename).
- [x] 1.2 `tasks.rs`: patch rewrite core — `apply_patch(file,
      patch)` rewrites only named keys + `updated`; invalid `status`
      in a parsed file ⇒ `NeedsAttention`, never a crash or silent
      rewrite; optimistic-concurrency guard per S6 (mtime/hash
      precondition before write, retry on mismatch); raw
      line-splitter core, not serde_yaml (S6); handle multi-line
      values on patched keys (S6 prototype TODO)
      - S6 candidate A implemented as `patch_text`: only the physical
        lines of named keys are replaced; comments, key order,
        indentation, quoting, unknown keys, CRLF terminators and the
        body pass through verbatim. No-frontmatter files get a block
        prepended (candidate B's unfixable case).
      - **S6 multi-line TODO closed**: replacing a key consumes its
        whole physical extent — following indented lines, column-0
        `-` sequence items, and interior blank lines — so block
        scalars and block sequences swap as a unit with no orphaned
        continuation lines. Trailing blanks before the next key stay
        put. Only column-0 `key:` lines are targets, so a nested
        `meta.status` is never mistaken for the top-level one.
      - Concurrency guard: `apply_edits` fingerprints (len + mtime +
        XxHash64 of contents), re-fingerprints immediately before the
        write, and retries the whole read-patch-write cycle on
        mismatch (`PATCH_MAX_ATTEMPTS = 4`). Content hash is the real
        check — Windows mtime granularity is too coarse to trust
        alone. A byte-identical result skips the write entirely, so
        no-op updates never wake the watcher.
      - Judgment: `TaskPatch` has no "remove key" variant. Clearing a
        value means setting it empty (`assignee: ''`), which keeps the
        line and therefore the file's key order stable.
      - Judgment (stronger than the letter of the spec): `apply_patch`
        *refuses* a file whose on-disk `status` is unrecognized unless
        the patch itself sets `status`. "The file is not rewritten"
        then holds for every automatic path (drag-drop, rollover,
        claim), while an explicit fix is still allowed. Same guard on
        `apply_goal_patch`.
      - Judgment: values are single-quoted unless unambiguously plain
        (leading ASCII letter, only alnum/`_-./ `, not a YAML keyword)
        — so dates and `ken://…` ids are quoted, matching what
        serde_yaml emits for memory files. Embedded newlines in a
        patched scalar fold to spaces; the core is line-based and a
        raw newline would desynchronize the block.
- [x] 1.3 `tasks.rs`: home scanning + aggregation (workspace home +
      any `<project>/.ken/tasks/`), `project` defaulting from
      per-repo home; filter matching (status/project/tag/assignee/
      kind) shared by UI and `task_list`
      - `list_tasks` is non-recursive, so `archive/` and `goals/`
        subfolders are excluded for free. A missing folder reads as
        no tasks (per-repo home not opted in), never an error.
      - `scan_tasks` dedupes across homes by `id`, not path — `id` is
        identity, so a file copied between homes collapses to one
        card instead of two.
      - Judgment: `AssigneeFilter::{Unassigned, Named}` rather than
        `Option<String>` — D4's worked example needs "unclaimed"
        (`assignee` empty) as a first-class query, and a magic
        reserved name would collide with a real assignee.
        `AssigneeFilter::parse` maps `none`/`unassigned`/empty for
        tool arguments.
      - Judgment: `TaskFilter` also carries `board`, which 1.3 doesn't
        list — the daily view is "just a filter" (D5), so it belongs
        in the shared struct rather than a parallel one.
      - Judgment: text comparisons are case-insensitive; a task with
        an unrecognized `status` matches *no* status filter (it's in
        the tray, not in a column).
- [x] 1.4 `tasks.rs`: archive pathing (`tasks/archive/YYYY-MM/`
      within the task's own home); `task_complete` log append
      (`## Log` + timestamp) and journal summary line composition
      - `archive_target`/`archive_task` resolve against the task's own
        `home_dir`, so a per-repo task archives inside its repo.
        `archive_month` validates strict `YYYY-MM-DD`.
      - Judgment: a name collision in the target month (archive →
        restore → archive) gets a `-2`, `-3`, … suffix instead of
        clobbering history.
      - `complete_task` sets `status: done`, bumps `updated`, and
        appends the report under `## Log` in **one** guarded write, so
        a concurrent claim can't lose the log. The append is a closure
        over the current body (adds the heading only if absent), so a
        concurrency retry recomputes it correctly.
      - Judgment: dates are compared/validated as strings — ISO dates
        sort lexicographically, so `memory.rs`'s `days_from_civil`
        machinery isn't needed or duplicated here.
      - `journal_summary_line` is pure composition and takes the
        `ken://` *host* from the caller (`WORKSPACE_ADDRESS_ID` or the
        project's uuid) — this module has no `Project` handle. Rel
        path is `tasks/<file>` or `.ken/tasks/<file>`. The excerpt is
        capped at 160 chars: the journal line is a pointer, the full
        report lives in `## Log`.
- [x] 1.5 `tasks.rs`: daily rollover detection (board daily, status
      ≠ done, `updated` < today) and the three resolutions (roll /
      promote / archive) as pure transitions
      - `resolve_rollover` returns a `RolloverAction` (patch or move)
        and touches nothing; `apply_rollover` executes it. So the
        prompt can be previewed/batched/tested before any write.
      - Judgment: `Roll` is an empty `TaskPatch` — `apply_patch`
        always rewrites `updated`, and bumping it *is* rolling
        forward. Nothing else changes, so `board: daily` survives.
      - Judgment: a task whose `updated` isn't a valid ISO date IS a
        candidate (unknown staleness ⇒ ask, which is the whole
        posture of the prompt); a task with an unrecognized `status`
        is NOT (it belongs to the tray, and rolling it would mean
        writing to a file we don't understand).
      - Repeated rollover needs no stored counter: rolling sets
        `updated` to today, so the task drops out until the next day
        and reappears then. Tested across three consecutive days.
- [x] 1.6 `tasks.rs`: goal file model (`tasks/goals/` in the
      workspace home; `id`, `title`, `status active|done|dropped`,
      `created`, `updated`, body; same patch core), `goal` in
      filter matching, derived progress counts (done/total per
      goal), unknown goal id ⇒ `NeedsAttention`; `backlog` as the
      default status on create
      - `apply_goal_patch` shares `apply_edits`, so goal files get the
        same byte fidelity, the same concurrency guard, and the same
        never-rewrite-what-we-don't-understand refusal.
      - `goal_progress`/`goal_progress_all` are computed over the
        scanned board; nothing is ever written. Judgment: a task with
        an unrecognized status counts toward `total` but never
        `done` — it's real work, just not placeable.
      - `needs_attention` is one pure function over (tasks, goals)
        covering all four cases (invalid status/kind/board, unknown
        goal id), cheap enough to recompute on every watcher event.
      - `create_task` defaults `status` to `backlog`; `due`/`goal`
        keys are omitted entirely when unset rather than written
        empty.
- [x] 1.7 Tests: round-trip with unknown keys and hand-edited
      bodies; patch touches only intended keys; filename rename
      doesn't change identity; filter table tests incl. goal;
      progress count cases; archive path cases; rollover cases
      incl. repeated rollover; invalid-status and unknown-goal
      trays
      - 44 tests, all green (`cargo test -p ken-core --lib -- tasks::`
        → `test result: ok. 44 passed; 0 failed; 0 ignored; 0
        measured; 602 filtered out; finished in 0.19s`). No new
        compiler warnings.
      - The corpus const is built with `concat!` rather than a raw or
        `\`-continued string literal: `\`-continuation silently eats
        the next line's leading whitespace (which quietly destroyed
        the nested-key and block-sequence cases on the first run), and
        a raw literal would inherit the checkout's line endings and
        break the explicit CRLF test.

## 2. src-tauri

- [x] 2.1 Board state: scan on workspace open, watchers on both
      home kinds, self-write dedupe by content hash; `board-state`
      events; flag-gated by `kenTasks`
      - `task_homes_scan`/`board_state_dto` scan the workspace home plus
        every resolvable member's `<project>/.ken/tasks/`, straight from
        the manifest (`ws.members`) — not `AppState::members` — so dormant
        members are scanned exactly like resident ones (a task home needs
        only a root + display name on disk, not a live `MemberRuntime`).
      - Judgment: no OS-level file watch. `notify` (the crate `ken-core::
        watch` uses) is a `ken-core`-only Cargo dependency; `src-tauri`
        has none and `Cargo.toml` is outside this task's touch-boundary.
        `spawn_task_board_watch` instead follows the SAME polling
        discipline `activate()`'s `.kenignore` watcher already uses in
        this same file: a sleep loop (1.2s, `TASK_BOARD_POLL_INTERVAL`)
        over a content-hash snapshot of every watched `.md` file. Content
        hash, not mtime, for the same reason `tasks::apply_edits`'
        concurrency guard gives: Windows mtime granularity is too coarse
        to trust alone.
      - Judgment: self-write dedupe is a per-path registry
        (`AppState::task_recent_writes: HashMap<PathBuf, Option<u64>>`),
        not a single whole-board hash. Every mutating command writes its
        file, registers the resulting content hash (`None` for a path it
        just made disappear — archive/rollover-archive), and calls
        `emit_board_state` itself for instant UI feedback. The poller
        diffs its previous tick against the current one path-by-path;
        each changed path is checked against the registry first — a
        match means "already announced" (consumed, no second emit) —
        and anything left over (an agent's write, a hand edit, a `git
        pull`, or a command whose own emit hasn't landed yet) triggers
        exactly one recompute + emit for the whole tick. A truly no-op
        rewrite (byte-identical output) never reaches the poller at all,
        because `tasks::apply_edits` itself skips writing when the
        patched text equals the original — this registry handles the
        remaining case: a *real* write a command has already broadcast.
      - `WorkspaceState::task_watch: Option<StopOnDrop>` — `None` when
        `kenTasks` resolves off at workspace-open time (never spawned:
        "off => no watchers"); dropping `WorkspaceState` (close/switch)
        stops the poller automatically, no separate teardown call.
- [x] 2.2 Commands: `task_create`, `task_list`, `task_update`,
      `task_complete`, `task_archive`, `board_get`; drag-drop uses
      `task_update` (status only); goal commands `goal_create`,
      `goal_update`, `goal_list` (chat tools wrap the same); board
      state carries per-goal progress counts
      - All nine commands registered, all flag-gated by `kenTasks`
        (`KEN_TASKS_DISABLED_MSG`) and require an open workspace.
      - `board_get`/`board-state` DTO: `{ tasks, goals, needsAttention,
        progress }`, where `progress` is `BTreeMap<goalId, {done,total}>`
        — the frontend session's exact contract, since `Task`/`Goal`/
        `NeedsAttention`/`Progress` already serialize camelCase from
        `ken_core::tasks`.
      - `task_create`/`resolve_daily_candidate` take an optional
        `projectId` (member's project id, not in tasks.md 2.2's literal
        list but needed to route into a per-repo home); omitted ⇒
        workspace home.
      - Chat-tool wrappers ("chat tools wrap the same core" — proposal)
        are out of touch-boundary this phase (`chat.rs`); not built here.
      - Judgment: `task_archive`/`resolve_daily_rollover`'s `Archive`
        branch reparse the moved file with `default_project =
        task.project` (the caller's already-resolved value) rather than
        re-deriving the owning home's default — always correct, since if
        the file's own `project:` key was explicit the default is never
        consulted, and if it was empty `task.project` already equals
        what the default would resolve to again.
- [x] 2.3 Daily flow: "plan my day" chat path drafting candidates
      from `read_journal` + recent ingest activity with approval
      cards; new-day rollover prompt with per-task roll / promote /
      archive
      - `plan_daily_tasks`/`resolve_daily_candidate` mirror `distill_
        journal`/`resolve_distill_candidate` exactly: a `daily-plan-state`
        event (`planning`→`ready{candidates}`/`error{reason}`),
        server-cached candidates (`AppState::daily_plan_candidates`),
        approve-by-`key`. Recent activity = each *resident* member's
        `db.runs_finished_since(now - 24h)` (status `fresh`) — dormant
        members aren't activated just for this.
      - Scope judgment: recognizing the literal phrase "plan my day" in a
        chat message is `chat.rs`/frontend wiring, outside this phase's
        touch-boundary and outside this task's own checklist (which lists
        only src-tauri commands). `plan_daily_tasks` is the command that
        surface is expected to call.
      - Judgment: dismissing a daily candidate keeps no durable
        "don't re-propose" record (unlike `resolve_distill_candidate`'s
        `UserState::ignored` tagging) — design's daily-board rules give
        no dedupe-across-runs contract for this, only on-request
        population + the rollover ritual, so a dismissed candidate just
        won't reappear until the next run drafts something new.
      - Judgment on "first open of a new day" (design D5): no persisted
        last-check marker was added. `daily_rollover_candidates` is
        purely derived (`board: daily`, not done, `updated < today`) and
        self-correcting — `Roll` bumps `updated` to today so a resolved
        task drops out immediately, `Promote`/`Archive` remove it from
        the daily board entirely — so a frontend calling it once per
        workspace-open gets "first open of the day" behavior for free in
        the common case, without src-tauri tracking dates itself. A
        second same-day open with an unresolved prompt sees it resurface
        rather than silently vanishing — safe, not data-losing.
- [x] 2.4 ken-memory integration: `task_complete` writes the
      journal summary line when `kenMemory` is on; skipped cleanly
      when off
      - Host for the `ken://` address: `memory::WORKSPACE_ADDRESS_ID` for
        a workspace-home task, else the owning member's project id
        (`owning_member_id`, matched by `task.home_dir` against each
        member's `project_tasks_dir` — not by the task's own `project:`
        value, which can name anything and isn't reliably the owning
        member).
      - Judgment: the journal write is best-effort — a failure there is
        logged and swallowed, not propagated, so a secondary-integration
        hiccup can't undo the primary action (the task IS done, the `##
        Log` entry IS written).
- [x] 2.5 Indexing tier: per-repo `.ken/tasks/` added to built-in
      search-only rules (kenignore defaults); workspace home covered
      by pseudo-member rules
      - Workspace home: already fully covered, no code needed —
        `memory::workspace_builtin_rules()` (in `crates/ken-core/src/
        memory.rs`, out of this task's touch-boundary, and apparently
        already anticipating this change) already carries
        `Rule { tier: Tier::SearchOnly, pattern: "/tasks/" }`, verified
        by its own `workspace_builtin_rules_classify_as_designed` test
        classifying `tasks/todo.md` as `SearchOnly`. The pseudo-member's
        project root IS `.ken-workspace/`, so `tasks/` relative to that
        root has no dot-prefixed component and both `scan.rs`'s
        `WalkBuilder(.hidden(true))` and `watch.rs`'s `relevant_path`
        walk it normally.
      - **BLOCKED, per-repo home**: `<project>/.ken/tasks/` is never
        indexed at any tier. Root cause: `scan.rs`'s `WalkBuilder` sets
        `.hidden(true)`, and `watch.rs`'s `relevant_path` excludes any
        path with a dot-prefixed component — both apply to a *real*
        project's root, where `.ken/tasks/…` genuinely has a hidden
        `.ken` component (unlike the pseudo-member, whose root already
        IS the dot-folder). This is the exact same root cause already
        recorded against per-project `.ken/memory/` in `memory_write`'s
        doc comment (ken-memory task 2.2) — one shared gap, not two.
        `scan.rs`/`watch.rs` are outside this task's touch-boundary; no
        kenignore rule can reach a file the walker never visits in the
        first place. A per-repo task is fully functional (created,
        edited, boarded, patched, archived, journaled) — it's just
        invisible to FTS/semantic search and `ken://` search-resolved
        addresses until that walker-level exclusion is fixed in a change
        that can touch `scan.rs`/`watch.rs`.
- [x] 2.6 `kenTasks` flag: registered in `crates/ken-core/src/
      features.rs` (`FlagScope::Workspace`, default off, same shape/
      wording pattern as `kenMemory`/`federatedKg`/`kgRouting`);
      registry test count 6→7 plus a `kenTasks` scope/default
      assertion. `ken_tasks_enabled` (src-tauri) AND-ed with
      `workspace_enabled`, same durable-home deviation the sibling
      workspace-scoped flags already document (no workspace-manifest
      feature layer exists yet, so `settings.json`'s global layer is
      the only home). Off ⇒ every task/goal/daily command returns
      `KEN_TASKS_DISABLED_MSG`, the board poller is never spawned
      (checked once at workspace-open time, mirroring `kenMemory`'s
      own not-live-toggled precedent — flipping the flag takes effect
      on the next workspace open, not mid-session), and no `tasks/`/
      `tasks/goals/`/`.ken/tasks/` folder is ever created (creation
      only ever happens inside `tasks::create_task`/`create_goal`,
      which only a flag-gated command reaches).

## 3. ken-mcp

- [x] 3.1 `task_create`, `task_list(filter)`, `task_update(id,
      patch)`, `task_complete(id, report)` delegating to core; tool
      descriptions document the claim convention (check unclaimed →
      set assignee + doing) and the complete→journal flow;
      `task_list` filter includes `goal` (no separate goal tools);
      absent when `kenTasks` is off
      — a `TaskHomes` resolver spans the workspace home plus every
      registered project's `.ken/tasks/`, so id lookup for
      update/complete finds a task in any home. Added `home?`
      ("workspace" | project name) to `task_create` — D2's hybrid
      homes need a selector and the proposal named none; mirrors
      `memory_write`'s `scope`. Enum values are validated in the tool
      layer so loose strings never reach core. `task_complete`'s
      journal-write failure degrades to a warning appended to the
      success message rather than failing the call, since the task is
      already done and logged by that point.
- [x] 3.2 Tests: schema round-trips; claim sets exactly
      assignee+status; complete appends log; flag off ⇒ not listed
      — 6 new tests (26 unit + 2 stdio green). The claim test asserts
      the S6 byte-fidelity contract properly: the file diff is a
      subset of {assignee, status, updated} AND a hand-added unknown
      key survives. Also covers cross-home id lookup and the
      kenMemory-off branch writing no journal file.

## 4. Frontend

- [x] 4.1 `api.ts`: task/board types, command wrappers,
      `board-state` listener
      - Types mirror the Rust DTOs field-for-field (`Task`, `Goal`,
        `NeedsAttention`/`AttentionReason`, `Progress`, `BoardStateDto`,
        `TaskFilter`/`TaskPatch`/`GoalPatch`, `AssigneeFilter`, `Rollover`,
        `DailyCandidate`/`DailyPlanStateEvent`); all 13 commands wrapped
        (`taskCreate/List/Update/Complete/Archive`, `boardGet`,
        `goalCreate/Update/List`, `planDailyTasks`, `resolveDailyCandidate`,
        `dailyRolloverCandidates`, `resolveDailyRollover`); `onBoardState` +
        `onDailyPlanState` listeners.
      - Judgment: `AssigneeFilter` has no explicit `tag`/`content` on the
        Rust side, so it's modeled as serde's default externally-tagged
        shape (`"unassigned" | { named: string }`), not an internally-tagged
        object like the event enums.
- [x] 4.2 Tasks tab in left sidebar (workspace mode, flag on):
      Kanban columns by status with `backlog` leftmost as the
      intake column, cards show title/project/tags/assignee/kind
      badge + goal chip; filters for project/tag/assignee/kind/
      goal; group-by-goal board mode with derived n/m progress per
      goal; goal create/edit dialog; needs-attention tray
      - `src/lib/tasks.svelte.ts` (flag-gated store, mirrors
        `memory.svelte.ts`/`workspaceKg.svelte.ts`) + `src/screens/
        TasksScreen.svelte` + `src/tasks/{TaskCard,GoalDialog,
        NeedsAttentionTray,DailyPlanCards,RolloverPrompt}.svelte`. Nav item
        only renders when `app.workspace` is open AND `kenTasks` resolves
        on (`NavRail.svelte`); `app.svelte.ts`'s `Screen` union gained
        `"tasks"`, wired into `Shell.svelte`'s lazy-mount pane list.
      - Judgment: filtering is client-side against the already-live
        `board.tasks` (mirrors `ken_core::tasks::matches` exactly) rather
        than round-tripping through `task_list` per keystroke — `board_get`/
        `board-state` already carry the full set; `taskList` still exists
        as a faithful wrapper per 4.1 but the UI doesn't call it.
- [x] 4.3 Drag-drop between columns → `task_update`; card click
      opens the task file in the normal document view
      - Drag-drop sends `task_update(id, { status })` only — never a
        whole-task patch (S6 byte-fidelity contract).
      - Judgment (honest-disabled-state, per the session brief): opening a
        task file hits the same design collision `kenAddress.ts` already
        documents for `ken://workspace/...` — the `.ken-workspace/`
        pseudo-member is never a workspace-manifest member, so no
        `focus_project`/`read_file` path can open a file inside it today.
        Workspace-home tasks show a disabled tooltip pointing at that note
        instead of a dead/broken click. Per-repo tasks resolve by matching
        `task.project` (the display name a per-repo task defaults to when
        it omits its own `project:` key, design D2) against an open
        workspace member's name, then open `.ken/tasks/<file>` — `Task::
        address_rel_path`'s own formula, reconstructed client-side with no
        extra backend round trip. A task whose explicit `project:` was
        hand-set to something that doesn't match any open member's name
        falls back to the same honest-disabled state rather than guessing.
- [x] 4.4 Daily board view (`board: daily`); daily proposal
      approval cards; rollover prompt UI
      - Daily is the same Kanban render, filtered to `board: "daily"`
        (design D5: "just a filter"). `DailyPlanCards.svelte` triggers
        `plan_daily_tasks` only on click (never autonomous) and renders
        approve/dismiss cards from `daily-plan-state`. `RolloverPrompt.svelte`
        renders `daily_rollover_candidates` with independent per-task roll/
        promote/archive buttons; the panel self-empties as each resolves
        (no client-side dismiss bookkeeping needed — the list is purely
        server-derived).
- [x] 4.5 Archive action on done cards
      - `TaskCard.svelte` shows an "Archive" button only when
        `status === "done"`, calling `task_archive`.

## 5. Verification

- [ ] 5.1 cargo test --workspace, pnpm test, pnpm check, pnpm build
      green
- [ ] 5.2 Flag off: no tab, no tools, no watchers, no folders —
      byte-identical
- [ ] 5.3 Manual: create tasks in both homes, filter, drag through
      the lifecycle; edit a file by hand mid-session and see the
      board update; claim + complete a task from an MCP client and
      see the log entry, done column, and journal line; run a
      rollover morning with all three resolutions; create a goal,
      tag tasks from both homes, group by goal, and watch the n/m
      progress update as tasks complete
- [ ] 5.4 Round-trip abuse: hand-add unknown frontmatter + drag the
      card twice; confirm the file diff is only status/updated each
      time

# Tasks: workspace

## 1. ken-core

- [x] 1.1 `workspace.rs` (new): `WorkspaceConfig { name, id,
      members, #[serde(flatten)] extra }`; atomic save / tolerant
      load mirroring `project.rs`; `Workspace::create(parent, name,
      member_names)` (mkdir `.ken-workspace`, write manifest,
      `Project::create` per member), `Workspace::open(parent)`
      (missing members → `MemberStatus::Missing`, never fatal);
      register in `lib.rs`. Note: manifest write uses temp-file+rename
      (like `profiler::ProjectProfile::save`), not `Project::save`'s
      plain write — spec.md says "written atomically". Also added
      `MemberStatus::Invalid(String)` beyond the spec's two states, so
      one corrupt member's `project.json` never fails opening the rest.
- [x] 1.2 `workspace.rs`: `discover_candidates(parent) ->
      Vec<Candidate { name, existing, file_count, markers }>` — one
      level, skip hidden/`.ken-workspace`/`node_modules`/`target`.
      Note: `file_count`/`markers` are also one-level-deep (immediate
      children of the candidate only, not recursive), matching D5's
      "shallow and dumb" intent; reuses `profiler::REPO_MARKERS` and
      `scan::is_junk_dir_name`.
- [x] 1.3 workspace tests: create-then-open round trip, unknown-key
      preservation, adopt existing manifest, missing member reported,
      relative-member resolution after parent rename (tempdir),
      discovery fixture folder (existing project, plain repo, junk
      dirs excluded). All 6 scenarios covered in `workspace.rs`'s
      `#[cfg(test)]` module.
- [x] 1.4 `registry.rs`: recent-workspaces list (path, name, last
      focused member id, opened_at) beside recent projects; tests.
      Added `RecentWorkspaceEntry`/`RecentWorkspaceStatus` +
      `add_workspace`/`remove_workspace`/`workspace_statuses`,
      mirroring the existing project entry API 1:1; the pre-existing
      `last_workspace: Option<Uuid>` field is unchanged and still
      serves as "which one to reopen on launch", same relationship
      `last_project` has to `projects`.

## 2. src-tauri — extraction commit (no behavior change)

- [ ] 2.1 Extract `ActiveProject` fields into `ProjectHandle`;
      introduce `enum AppMode { Single(ProjectHandle), Workspace
      {...} }` with only `Single` constructed; replace every
      `state.active.as_mut()...` with `focused(&mut state)`;
      **all existing tests green before proceeding**
      NOTE (not this session's scope — annotated only): superseded by
      the already-committed S9 refactor, which gave `AppState` a
      `members` map + `focused` field directly rather than the
      `AppMode { Single | Workspace }` enum design.md D2 describes.
      This session's `workspace.rs`/`registry.rs` layer does not
      construct or assume that enum, so it is unaffected either way;
      whoever picks up section 2/3 should re-derive the routing
      helper (`focused(&mut AppState)`) against the committed
      `members`-map shape instead of introducing D2's enum.

## 3. src-tauri — workspace mode

- [x] 3.1 `open_workspace(parent)` / `create_workspace(parent, name,
      members)`: build `ProjectHandle` per member (lazy beyond 12,
      LRU), ingest semaphore (max 2 concurrent), start watchers;
      `workspace-state` and `member-status` events.
      DONE (`src-tauri/src/lib.rs`). Built on the committed S9 shape
      (`AppState::members` map + `focused`), NOT design.md D2's
      `AppMode` enum (tasks.md 2.1 superseded note): added a
      `WorkspaceState { ws: ken_core::workspace::Workspace, lru:
      Vec<Uuid> }` field on `AppState` — it holds the whole resolved
      manifest (so dormant members keep their project id + root for
      lazy activation) plus the resident-runtime LRU. Both commands
      funnel through `open_workspace_inner`, which activates every
      resolvable member up to `WORKSPACE_RESIDENT_CAP` (12) via the
      EXISTING `activate(clear_others=false)` path; members beyond 12
      stay dormant (tracked-not-resident) and open on first focus.
      JUDGMENT — ingest semaphore: `std` has no semaphore, so added an
      `IngestGate` (`Mutex<usize permits>` + `Condvar`, RAII
      `IngestPermit` releases on drop), one instance on
      `AppState::ingest_gate`, cap `WORKSPACE_INGEST_CONCURRENCY` (2).
      The ONE change to `activate` is its initial-scan thread now
      `acquire()`s a permit around `scan::scan`; single-project always
      has 2 free permits so it never waits (zero behavior change),
      while a workspace staggers N scans two at a time. Events:
      `workspace-state` (app-global, tag-shaped like
      `WorkspaceKgStateEvent`: `opening`/`open`/`focus`/`closed`) and
      `member-status` (via `emit_member`, so the envelope adds
      `project_id`; tag `active`/`dormant`).
- [x] 3.2 `workspace_overview()` (members + status + counts),
      `focus_project(id)`, `discover_workspace_candidates(parent)`;
      close path tears down all handles; register commands.
      DONE. `workspace_overview` returns the manifest header + every
      member with `status` (active/dormant/missing/invalid), `reason`
      (invalid parse error), and `file_count` (via `Db::file_count`,
      populated only for active members — snapshotted under one short
      lock, counted off-lock per lock-audit). `focus_project` routes
      through `focus_member_inner`: activates a dormant target, touches
      the LRU, and evicts the LRU front when residents exceed the cap —
      eviction is `members.remove(&id)`, the SAME runtime-drop teardown
      `close_member` uses. `discover_workspace_candidates` is a thin
      flag-gated wrapper over `ken_core::workspace::discover_candidates`
      (`Candidate` is already Serialize/camelCase). Added a
      `close_workspace` command (the brief's "close path"): `members
      .clear()` drops all runtimes, `workspace` is reset to `None`, and
      recents get `add_workspace(last focused, now)`. All commands
      registered in `invoke_handler`.
- [x] 3.3 `search_all_projects(query, limit)`: per-member FTS,
      round-robin rank interleave, member-name labels.
      DONE. Fans `Db::search` out over ACTIVE members only, following
      the `search`/`hybrid_search` lock template (clone each read-only
      `search_db` Arc under a brief lock, release the guard, run FTS on
      the blocking pool). Merge is round-robin by rank POSITION (design
      D6: BM25 scores aren't cross-corpus comparable) — position 0 of
      every member, then 1, … capped at `limit`. Each hit is labeled
      with `project_id` + `member_name`. Dormant members are skipped and
      reported in `member_status` (honest per-member list —
      `searched`/`dormant`/`missing`/`invalid` — mirroring
      `route_search`'s `member_status`).
- [x] 3.4 Global `workspace` flag read; flag off → workspace commands
      return a friendly "feature disabled" error.
      DONE. Every new command opens with a `workspace_enabled(&guard
      .app_settings)` check (same gate `open_member`/`close_member`
      already use) returning `WORKSPACE_DISABLED_MSG` (launcher-ready
      wording). Registry: `add_workspace` + `last_workspace = Some(id)`
      on open/create AND close (via `record_workspace_recent` /
      inline), so recents always carry the last focused member.
      JUDGMENT — recents persisted on open/create/close only (not on
      every `focus_project`) to avoid per-focus registry IO; the close
      capture reflects the final focus, satisfying the spec's
      "close → reopen restores focus" scenario. Restore-focus on open
      reads `Registry::workspaces[id].last_focused` and routes it
      through `focus_member_inner` (activating it if it was a
      beyond-cap dormant member).

## 4. Frontend

- [x] 4.1 `api.ts`: workspace types (`Candidate`, `MemberStatus`,
      `WorkspaceOverview`), command wrappers, event listeners
      DONE. Added `Candidate`/`WorkspaceMember`/`WorkspaceOverview`/
      `WorkspaceStateEvent`/`MemberStatusEvent`/`SearchAllProjectsResult`
      types (`src/lib/api.ts`) mirroring the Rust DTOs field-for-field
      (verified by grepping the current `lib.rs`, which already has
      `open_workspace`/`create_workspace`/`workspace_overview`/
      `focus_project`/`discover_workspace_candidates`/`close_workspace`/
      `search_all_projects` — section 3 landed since this tasks.md was last
      touched); wrappers for all 7 commands + `onWorkspaceState`/
      `onMemberStatus` listeners.
- [x] 4.2 Launcher: "Open a workspace" (flag-gated) → folder pick →
      candidate checklist (existing pre-checked, marker captions,
      include toggles) → name → Features disclosure → create;
      recent workspaces section
      DONE except recents. `src/onboarding/ProjectPicker.svelte` gained a
      parallel wizard next to the existing folder flow, gated on
      `app.workspaceFlagEnabled`. DEVIATION: no recent-workspaces section —
      `Registry` has recent-workspace entries (task 1.4) but no Tauri
      command reads them back (grepped `lib.rs` for `workspace_statuses`/
      `registry.workspaces`: nothing registered); deferred rather than
      invented, see final report.
- [x] 4.3 Nav-rail project switcher: workspace name, member list with
      status dots, click/`Ctrl+P` cycle to `focus_project`; screens
      reload their stores on focus-change event
      DONE. New `src/shell/WorkspaceSwitcher.svelte` popover (triggered from
      a new nav-rail button, `src/shell/NavRail.svelte`) lists
      `app.members` with status dots; click and `Ctrl+P`
      (`app.cycleFocusedMember`, wired in `Shell.svelte`) call
      `app.focusMember` → `focus_project`. `app.svelte.ts`'s `members`/
      `focused` getters now derive from real `WorkspaceOverview` data when
      a workspace is open (Single-mode path untouched — same `[project]`
      shape as before). DEVIATION: no command returns a full `ProjectInfo`
      for a workspace member other than the one just opened/created —
      `focus_project`/`workspace_overview` only carry `name`/`projectId`/
      `status`. `loadFocusedMemberState` reconstructs `root` exactly
      (`workspace.root + "/" + member.name`, since members are stored as
      parent-relative folder names) but defaults `excluded`/`ingestRunner`
      (no per-member read path exists) rather than carrying over the
      previous member's values; Settings' exclude list / ingest-runner
      toggle may show these defaults for a non-initial focused member until
      a per-member info command exists. Backend truth is unaffected — every
      mutating command already resolves "the project" via `state.focused`.
      Screen reload reuses the exact refresh list `activated()` runs for a
      plain project switch (tabs/favorites/recents, background/transcribe/
      semantic-index settings, ignored/unread, tree, review badge).
- [x] 4.4 ⌘K "All projects" scope toggle (workspace mode only):
      labeled results, selection switches focus then opens
      DONE, including the kg-routing upgrade (kg-routing task 4.2, same UI
      slot). `src/search/SearchOverlay.svelte` gained a "This project" /
      "All projects" toggle visible only when `app.workspace` is set;
      results normalize into one `DisplayHit` shape from whichever endpoint
      answered (`search_all_projects` keyword fan-out, or `route_search`
      when `kgRouting` is also on) — member badge per hit, `kg://`
      breadcrumb chips, "routed"/"keyword" scope chip, a plan line
      ("Routed to N projects"/"Routed via the knowledge graph to N
      projects"/"Searched N projects"), per-member coverage notes for
      dormant/missing/invalid or index-building/unavailable members, and a
      `routed-search-state` progress line while searching. Selecting a hit
      in an unfocused member calls `app.focusMember` before opening.

## 5. Verification

- [ ] 5.1 cargo test --workspace, pnpm test, pnpm check, pnpm build
      green; extraction commit verified independently
- [ ] 5.2 Manual: create a workspace over the real parent folder
      (7 members), watch staggered ingests, switch focus, all-projects
      search, close/reopen restores focus

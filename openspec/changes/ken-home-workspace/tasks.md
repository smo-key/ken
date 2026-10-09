# Tasks: ken-home-workspace

Layered like the changes before it: ken-core first (pure, fixture-tested),
then src-tauri, then the frontend, then verification. Each layer compiles
and tests green before the next starts.

## 1. ken-core — composition (pure)

- [x] 1.1 Add a workspace-digest composer taking, per member, its name and
  its stored digest for today (`Option<Digest>`), plus the board summary
  from `pipeline::compose_digest`. Returns a structured roll-up: per-member
  entries with body and sources, members marked not-yet-written, and the
  board section. Pure — no I/O, no clock, no AI call. Callers supply the
  rows and the local date.
- [x] 1.2 Tests: a mixed workspace (some members with digests, one
  without); every member missing a digest; an empty workspace; a member
  whose digest has no `SOURCES:` line (must reuse `parse_digest`'s
  tolerance rather than re-parsing).
- [x] 1.3 Confirm no change is needed in `digest.rs` — confirmed, untouched.
  Composer lives in a new `workspace_digest.rs` — per-project
  generation, the ≥07:00 gate, the quiet-day fallback and
  one-row-per-local-day all stay exactly as they are. Record in the final
  report if this turns out false.

## 2. src-tauri — reach and enumeration

- [x] 2.1 `route_search`: build targets from `Workspace::members`
  (`MemberStatus::Ok` only) instead of `AppState::members`. Reuse the live
  `Arc<Mutex<Db>>` for resident members; open a short-lived
  `Db::open(base, project_id)` for dormant ones and drop it after the
  query. Do not activate, register, or evict anything. Delete the now-stale
  deviation comment about no manifest existing.
- [x] 2.2 Treat a failed open or over-budget search as
  `MemberStatus::Unavailable` in the existing per-member report — never an
  error return, never a block. Confirm `merge_routed`'s report already
  distinguishes searched-but-empty from skipped; extend only if it does
  not.
- [x] 2.3 Accept an optional scope on the search command. `None` plans
  normally; `Some(project_id)` short-circuits to
  `RoutePlan { targets: vec![id], reason: Named }` without opening the KG.
  Same result shape either way.
- [x] 2.4 New workspace-digest command: read each member's stored digest
  for the local date, call `pipeline::compose_digest`, hand both to 1.1.
  Must not call the generator or touch the in-flight guard.
- [x] 2.5 New members-overview command: per manifest member, its
  `MemberStatus`, index state, unread count, failed-file count, and
  resolved root. Reports `unread: 0` for a never-baselined member rather
  than its whole tree, and writes no baseline — an overview must not mark
  files as seen.
  - [ ] 2.5a **Still open:** `loadFocusedMemberState`'s deviation note is
    unchanged. The command exists and Home uses it, but rewiring
    `excluded`/`ingestRunner` to read through it is a separate edit in
    `app.svelte.ts` that touches the Settings path.
- [x] 2.6 Flag gating: members strip and workspace digest behind
  `workspace_enabled`, daily board behind `ken_tasks_enabled`,
  cross-member search behind `kg_routing_enabled`. No new flag in
  `features.rs`. Each command returns its existing friendly disabled
  message when gated off.

## 3. Frontend — Home

- [x] 3.1 `api.ts`: types and wrappers for the workspace digest, members
  overview, and the search scope parameter.
- [x] 3.2 `HomeScreen.svelte`: workspace blocks added as additive
  sections, each behind `workspaceHome.enabled` (i.e. a workspace being
  open), so single-project Home is untouched. New
  `WorkspaceDigestCard.svelte`, and `workspaceHome.svelte.ts` as the
  store.
- [x] 3.3 Scope control — implemented in `SearchOverlay.svelte`, which is
  what actually owns search; `HomeSearch.svelte` is only the ⌘K trigger.
  The existing project/all toggle now defaults to **all** in a workspace,
  and gains a member picker that pins routing to one member (dormant
  members included). Per-result attribution already existed from
  kg-routing.
- [x] 3.4 Members strip — new `MembersStrip.svelte`, rendered in
  `HomeStatus`'s slot when a workspace is open and `HomeStatus` otherwise.
  Collapses above 4 members to the needs-a-look subset, expandable.
  Dormant members render a hollow dot: healthy, just not loaded.
- [x] 3.5 Daily board — Home shows the workspace board's `needsAttention`
  (which already spans members), capped at 6 rows, behind `kenTasks`.
- [ ] 3.6 **Not done — deferred.** Scope chip in Files and Tasks. The
  search-side narrowing (3.3) covers the "ask one project" case, but
  per-tab scope is a state refactor around `app.project` rather than a UI
  addition, and every screen and store reads it. Deferring it deliberately
  rather than half-wiring it; the decision that there is ONE focus with an
  opt-in override still stands (design D7).

## 4. Verification

- [x] 4.1 `cargo check -p ken-app` clean; `workspace_digest` tests 6/6;
  `npm run check` 0 errors over 792 files; release build succeeds. The
  four `sync::tests` failures on Windows were confirmed pre-existing
  against a detached HEAD worktree before this change started.
- [ ] 4.2 Manual: with a multi-member workspace, search from Home without
  focusing a member and confirm results arrive from a dormant one, and
  that it is still dormant afterwards.
- [ ] 4.3 Manual: move a member folder aside, reopen the workspace, and
  confirm it appears as unresolved in the strip and is absent from search
  targets.
- [ ] 4.4 Manual: turn `workspace`, `kenTasks` and `kgRouting` off and
  confirm Home is visually identical to before this change.
- [ ] 4.5 Manual: confirm opening Home does not create or refresh any
  member's digest row.
- [ ] 4.6 Final report: deviations, anything deferred, and whether the
  latency budget held with all members dormant.

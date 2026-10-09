# Tasks: multi-workspace

Layered like the changes before it: the state reshape first and alone,
then reach, then the surfaces, then MCP, then verification. Each layer
compiles and tests green before the next starts. Task 1 is deliberately
behavior-neutral — it should be reviewable as "same product, different
shape".

## 1. src-tauri — the state reshape (no behavior change)

- [ ] 1.1 Replace `AppState.workspace: Option<WorkspaceState>` with
  `workspaces: HashMap<Uuid, WorkspaceState>` and
  `focused_workspace: Option<Uuid>`. **Remove the old field rather than
  keeping an alias** — every call site must become a compile error so the
  compiler enumerates the blast radius (design D1).
- [ ] 1.2 Add the focused-workspace accessors beside the existing member
  ones (`fn focused_workspace(&self) -> Option<&WorkspaceState>` and its
  mut form), and rewrite each site the compiler flagged in 1.1 to say
  explicitly whether it means *the focused workspace* or *the workspace
  owning this member*. Record any site where the answer was not obvious.
- [ ] 1.3 Move the resident-member LRU from `WorkspaceState` to
  `AppState` so the cap is global (design D5). Eviction picks the
  least-recently-focused resident regardless of which workspace owns it.
- [ ] 1.4 Keep `task_watch` and `pipeline_known_running` on
  `WorkspaceState`, constructed on focus and dropped on defocus. Confirm
  `StopOnDrop` still stops the poller on defocus, not only on close.
- [ ] 1.5 Tests: with one workspace open every existing workspace test
  passes unchanged; the LRU evicts a resident of workspace A when
  workspace B pushes past the cap; defocus stops the poller and clears
  `pipeline_known_running`.

## 2. src-tauri — open, close, focus

- [ ] 2.1 `open_workspace_inner`: stop calling `members.clear()` and
  `workspace = None`. Resolve the manifest, insert the `WorkspaceState`,
  focus it. Members of other open workspaces stay resident.
- [ ] 2.2 Opening an already-open workspace is a focus change — do not
  re-read the manifest, do not tear down any runtime (design D2).
- [ ] 2.3 `close_workspace`: close the focused workspace, drop its
  members' runtimes, and fall focus to another open workspace if one
  remains (`None` if not) — mirroring the existing member-close rule.
- [ ] 2.4 New `focus_workspace(id)` command: switch focus, construct the
  new focused workspace's watchers, drop the old one's. No member
  activation as a side effect.
- [ ] 2.5 New `list_workspaces` command: every open workspace with id,
  name, root, member count, and which is focused. Reads state only —
  no disk, no manifest re-read.
- [ ] 2.6 `workspace_overview` and `workspace_members_overview` keep
  meaning *the focused workspace* and keep their present shape.
- [ ] 2.7 Tests: open A then B leaves A's manifest intact and its members
  resolvable; closing B focuses A; closing the last workspace leaves
  `focused_workspace: None`; re-opening an open workspace does not
  re-resolve it.

## 3. Reach — search across open workspaces

- [ ] 3.1 Extend routing's plan entry point to take a set of workspaces:
  run `plan_route` once per in-scope workspace against that workspace's
  own KG handle, concatenate the target lists (design D4). Pure —
  no I/O beyond the KG reads `plan_route` already does.
- [ ] 3.2 `route_search`: accept the workspace tier alongside the existing
  member `scope` per the design D3 table. Unset means the focused
  workspace, preserving today's behavior exactly. Reject
  all-workspaces-plus-member-id as a usage error.
- [ ] 3.3 Target opening is unchanged from `ken-home-workspace`: reuse the
  live handle for a resident member, open a short-lived
  `Db::open(base, project_id)` for anything dormant — including members of
  non-focused workspaces — and drop it after the query. No activation, no
  registration, no eviction.
- [ ] 3.4 Carry the owning workspace's id and name on each merged hit and
  in the per-member status report, so results can be attributed (D6) and
  an unavailable member can be told from an unavailable workspace.
- [ ] 3.5 Tests: a member of an open, non-focused workspace returns hits
  and is still dormant afterwards; a member of a *closed* workspace is not
  searched; scope pinned to one workspace does not consult another's KG;
  two members with the same name in different workspaces are
  distinguishable in the results.

## 4. Frontend — switcher and scope

- [ ] 4.1 `app.svelte.ts`: workspace state becomes a list plus a focused
  id, fed by `list_workspaces`. Existing single-workspace reads resolve
  through the focused entry.
- [ ] 4.2 A workspace tier above the Home members strip: open workspaces,
  which is focused, one click to focus another. Hidden entirely when only
  one workspace is open.
- [ ] 4.3 Search scope control becomes three tiers — workspace, group,
  member (design D9), with **members grouped by default** rather than
  flattened. Workspace tier: focused (default), a named workspace, or all
  open. Group tier: populated from `workspace_groups`, which already
  returns derived folder groups with no configuration. Member tier as
  today, but nested under its group. Narrowest pin wins. Each tier is
  hidden when it would offer only one choice, so a single workspace with
  no derived groups degrades to today's member picker.
- [ ] 4.3a **Every tier carries an explicit "all" entry**, listed beside
  the individual ones rather than expressed as the absence of a selection.
  Today there is no visible way to widen back out once a member is pinned,
  which is the option missing from the current picker.
- [ ] 4.3b **Ungrouping is configuration, not the default.** A
  per-workspace setting flattens the list for someone who wants the
  `Shattered-Realms*` folders listed apart; absent that setting,
  `derived_groups()` decides the shape. Persist it with the workspace, not
  the session.
- [ ] 4.3c **Ship the grouped picker first.** It needs no backend work —
  `route_search` already accepts `group`, and `derived_groups()` already
  yields `Hytale` and `Personal` for the Code workspace. This is the
  shortest path to the stated goal (a folder of related repos answering
  as one cluster) and is independently useful before any of tasks 1–3
  land.
- [ ] 4.4 Results from an all-workspaces search show the workspace name
  beside the member name. Single-workspace results are unchanged.
- [ ] 4.5 The picker gains "open another workspace" as a distinct action
  from "switch to", so opening a second workspace is not phrased as
  replacing the first.
- [ ] 4.6 Tests: with one workspace open, Home and search render
  identically to today; with two, the switcher focuses without a picker
  round-trip.

## 5. MCP — scope parity

- [ ] 5.1 `ken-mcp`: add `--workspace <path>`, mutually exclusive with
  `--project`. Both together exits with the usage error and code 2, like
  today's unknown-argument path.
- [ ] 5.2 Scope resolution: pinned project, else the pinned workspace's
  members, else every workspace in the registry (widening today's
  every-project meaning, design D8).
- [ ] 5.3 Update the usage string and `--version` output. Tools
  themselves unchanged — addresses are already workspace-agnostic.
- [ ] 5.4 Tests: `--workspace` scopes search to that workspace's members;
  `--project` still pins to one project; both flags is a usage error;
  unscoped reaches members of two registered workspaces.

## 6. Verification

- [ ] 6.1 With the `workspace` flag off, nothing in this change is
  reachable and every surface renders as it does today.
- [ ] 6.2 With the flag on and exactly one workspace open, behavior is
  byte-identical to before this change — the switcher and the workspace
  scope tier are both hidden.
- [ ] 6.3 Manual: open `Documents` and `Code` together, search a term
  that exists in both, confirm hits from both with correct attribution,
  and confirm the non-focused workspace's members are still dormant
  afterwards.
- [ ] 6.4 Manual: register `ken-mcp --workspace Documents` with Claude
  Code and confirm a session sees only that workspace.
- [ ] 6.5 Update the README's workspace section — it currently describes
  opening *a* workspace — and note the Windows build prerequisites
  discovered while getting here (libclang via LLVM, the Vulkan SDK, and
  `CMAKE_GENERATOR=Ninja`), which are absent from it today.

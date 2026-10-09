# Proposal: multi-workspace

## Why

`AppState` holds `workspace: Option<WorkspaceState>` — one, or none.
`open_workspace_inner` opens the next one by first calling
`guard.members.clear()` and `guard.workspace = None`, so opening a second
workspace is not "also open"; it is "instead". Every member runtime of
the first workspace is torn down, its task-board poller dropped with the
`WorkspaceState`, and its search reach goes with it.

That was the right shape when a workspace was one folder of repos. It
stops being right the moment the machine has two folders that mean
different things: a knowledge base (`Documents/`) and a code root
(`Code/`). Those are not two ways of looking at the same corpus, they are
two corpora, and the questions that matter most span them — "what did the
protocol doc say, and where is it implemented" reads one member from each.
Today that question cannot be asked. It requires closing one workspace,
opening the other, and holding the first answer in your head.

The reach to fix it already exists and is already paid for.
`ken-home-workspace` established that a member's index is openable by
project id from the app base dir (`Db::open(base, project_id)`), and used
that to search **dormant** members — members with no live runtime — without
activating them. A member of a *non-focused workspace* is exactly a
dormant member. The storage layer already supports the thing the state
layer forbids.

`Registry::workspaces` is likewise already a `Vec` — the persistence for
several workspaces exists and is used only as a recents list.

## What Changes

- **`AppState` holds many workspaces, one focused.**
  `workspace: Option<WorkspaceState>` becomes a map keyed by workspace id
  plus `focused_workspace: Option<Uuid>` — the same
  `members` + `focused` shape the S9 refactor gave members, one level up.
  `open_workspace` stops clearing state that does not belong to the
  workspace being opened; it adds. Closing the focused workspace drops
  focus to another open one, mirroring member close.
- **Search reach spans open workspaces, with no new storage.**
  `route_search` gains a workspace tier above its existing member
  `scope`: the focused workspace (today's behavior, and the default), one
  named workspace, or all open workspaces. Cross-workspace targets are
  opened by project id exactly as dormant members already are — no
  activation, no watcher, no eviction of a resident.
- **Routing federates per workspace rather than building a super-graph.**
  `plan_route` runs once per in-scope workspace against that workspace's
  own `kg.sqlite` and the plans are unioned. No global knowledge graph, no
  migration, each workspace's graph stays independently rebuildable.
  Honest limit: entity links stop at the workspace boundary in v1, so the
  graph cannot itself discover that a doc in `Documents` is about a repo
  in `Code` — broadcast still reaches it, the graph just will not
  privilege it.
- **One global resident-member budget.** The LRU that caps resident
  members becomes global across all open workspaces rather than per
  workspace, so opening a second workspace does not double memory and
  watchers. A member of a non-focused workspace is an ordinary eviction
  candidate.
- **Only the focused workspace runs live watchers.** The task-board
  poller and pipeline run-ledger watch belong to the focused workspace;
  defocusing drops them. Non-focused workspaces are searchable and
  readable, not live. This is the deliberate cost of keeping two
  workspaces open cheap.
- **A workspace switcher, not a workspace reopen.** The Home members
  strip gains a workspace tier above it: open workspaces, which is
  focused, and one click to focus another — without the picker
  round-trip that today means closing and reopening.
- **`ken-mcp --workspace <path>`.** The sidecar scopes to a project
  today (`--project`) or, unscoped, to the registry. It gains
  `--workspace <path>`, mutually exclusive with `--project`, so an agent
  session can be pinned to `Documents` or `Code`. Unscoped comes to mean
  every workspace in the registry rather than every project.

## Capabilities

### New Capabilities
- `multi-workspace`: several workspaces open at once with one focused —
  the state shape, focus and close semantics, the global resident budget,
  watcher ownership, and the switcher contract.

### Modified Capabilities
- `workspace-search`: routed search reaches members of open,
  non-focused workspaces; scope becomes a two-level address (workspace,
  then member); planning runs per workspace and merges.
- `mcp-server`: a `--workspace` scope alongside `--project`, and the
  meaning of unscoped widens from projects to workspaces.

## Impact

- `crates/ken-core`: no storage change. `workspace.rs` gains nothing
  structural — `Workspace::open` already resolves one manifest, and
  several resolved `Workspace` values is a caller concern. Routing gains
  a per-workspace plan-and-union entry point beside `plan_route`.
- `src-tauri`: the `AppState` reshape and its blast radius —
  `open_workspace_inner`, `close_workspace`, `workspace_overview`,
  `workspace_members_overview`, `route_search` target enumeration, the
  LRU, and every helper that reaches through `guard.workspace` on the
  assumption there is at most one. New: a workspace-list command, a
  focus-workspace command.
- `crates/ken-mcp`: argument parsing, and the scope resolution that
  currently answers "which single project do I mean".
- Frontend: a workspace tier in the Home members strip and in the search
  scope control; `app.svelte.ts` workspace state becomes a list plus a
  focused id; `api.ts` types and wrappers.
- Flags: **no new flag.** This changes what the existing `workspace` flag
  buys. With it off, nothing here is reachable; with it on and exactly one
  workspace open, every surface renders as it does today.
- Tests: opening a second workspace leaves the first's members resolvable
  and its manifest intact; search finds a member of a non-focused
  workspace without activating it; the global cap evicts across
  workspaces; defocus stops watchers; close-with-focus falls to another
  open workspace; `--workspace` and `--project` together is a usage error.

## Risks

- **Blast radius in `src-tauri/src/lib.rs`.** `guard.workspace` appears
  at 67 call sites, each assuming singularity. The reshape is mechanical
  but wide, and a missed call site fails as "acts on the wrong workspace"
  rather than as a compile error wherever an `Option` is replaced by a
  lookup that still returns an `Option`. Task 1.1 exists to make the
  compiler find all 67: remove the field rather than keep it as a
  convenience alias.
- **Memory with two large workspaces.** `Code/` includes
  `hytale-shared-source` at ~4.9 GB. The global cap bounds resident
  members, but indexing breadth is now user-visible in a way it was not:
  two workspaces means two manifests' worth of members eligible for
  search. Mitigation is the existing per-database search budget, which
  already degrades a slow member to "unavailable" rather than blocking.
- **Scope UI complexity.** A search box with two nested scopes is a
  worse search box if the default is wrong. The default stays the focused
  workspace precisely so the common case types nothing.

# Proposal: workspace

## Why

Ken is strictly one-folder-one-project: the Tauri layer holds
`AppState { active: Option<ActiveProject> }` (`src-tauri/src/lib.rs:99`)
and every command operates on that single project. The user's reality is
a parent folder ("Hytale Code/") containing 5+ sibling projects
(FrozenMemories, ItemSearch, JobManager, ShatterdRealmsTools,
ShatteredRealms, agent-desktop, ken). Working across them today means
closing and reopening projects and losing all cross-project context.
This change introduces the **workspace**: a parent folder whose child
projects are open simultaneously — each with its own DB, engine, and
watcher — with a project switcher and an all-projects search. It is the
structural prerequisite for `federated-kg` and `kg-routing`.

## What Changes

- **New ken-core module `workspace.rs`**:
  - `WorkspaceConfig { name, id: Uuid, members: Vec<String>,
    #[serde(flatten)] extra }` stored at
    `<parent>/.ken-workspace/workspace.json` — same
    adopt-if-exists/atomic-write discipline as `project.rs`. Members
    are parent-relative folder names; missing members are reported,
    not fatal.
  - `discover_candidates(parent) -> Vec<Candidate>`: one level deep;
    a candidate is any subfolder that is not hidden/excluded; each is
    tagged `existing` (has `.ken/project.json`) or `new`, with file
    count and repo markers (`.git`, `Cargo.toml`, `package.json`) for
    the selection UI.
  - `Workspace::create(parent, name, member_names)` — creates
    `.ken-workspace/`, writes the manifest, runs `Project::create`
    (which already adopts existing configs) for each member;
    `Workspace::open(parent)` loads manifest + members, skipping and
    reporting broken ones.
- **src-tauri refactor — the heart of this change**: `ActiveProject`'s
  per-project state (db handle, engine, watcher, running flags) is
  extracted into a `ProjectHandle` struct; `AppState` becomes
  `{ mode: Single(ProjectHandle) | Workspace { config, projects:
  HashMap<Uuid, ProjectHandle>, focused: Uuid } }` (an enum, so the
  single-project code path stays obvious and regression-safe). Every
  existing command resolves "the project" as: Single → it; Workspace →
  `focused`. New commands: `open_workspace(parent)`,
  `create_workspace(parent, name, members)`,
  `workspace_overview()` (members + per-member status),
  `focus_project(id)`, `discover_workspace_candidates(parent)`.
  All member engines/watchers run concurrently; the process-wide
  llama.cpp queue already serializes LLM work, and ingest concurrency
  is capped at 2 members at a time to keep the disk sane.
- **Registry**: recent-workspaces list stored beside the existing
  recent-projects registry; reopening a workspace restores the last
  focused project.
- **Folder-select experience**: the launcher gains "Open a workspace" —
  pick a parent folder → candidate checklist (pre-checked for
  `existing`, repo-marker captions, per-candidate include toggle) →
  name it → create. The Features disclosure (from the flag mechanism)
  appears here. Gated by the global `workspace` flag; flag off hides
  the entry point entirely.
- **In-app**: a compact project switcher in the nav rail (workspace
  name + member list, keyboard `Ctrl+P` cycling); every screen keeps
  working per-focused-project unchanged. ⌘K gains an "All projects"
  scope toggle that fans the existing search out over member DBs and
  labels hits with the member name (FTS-only here; hybrid fan-out
  belongs to `kg-routing`).

## Capabilities

### New Capabilities
- `workspace`: manifest, discovery, multi-open lifecycle, switcher,
  all-projects keyword search, recents.

### Modified Capabilities
- `project-lifecycle`: open/close paths are routed through the
  Single/Workspace enum; single-project behavior is unchanged.

## Impact

- `crates/ken-core`: new `workspace.rs`; `registry.rs` recents
  addition; no DB schema change (workspace holds no derived data —
  that arrives with `federated-kg`).
- `src-tauri`: the `ActiveProject` → `ProjectHandle` extraction +
  `AppState` enum (large, mechanical, the riskiest part — do it as
  its own commit with all tests green before any new commands);
  new workspace commands + events (`workspace-state`,
  `member-status`).
- Frontend: launcher workspace flow; switcher component; ⌘K scope
  toggle; `api.ts` types/wrappers.
- Tests: manifest round-trip/adopt/missing-member; discovery fixtures;
  enum-refactor regression (all existing command tests pass in Single
  mode); two-member workspace opens, both engines ingest, focused
  switching, all-projects search merges and labels.

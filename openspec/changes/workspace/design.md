# Design: workspace

## Context

`src-tauri/src/lib.rs` holds one `ActiveProject` (line 42: db, engine,
watcher, per-feature running flags) inside
`AppState { active: Option<ActiveProject> }` (line 99), guarded by a
mutex; ~every command locks it and operates on `active`. ken-core is
already fully per-project (a `Project`, a `Db`, an `Engine` are
self-contained), so multi-open is almost entirely a src-tauri and
frontend problem.

## Goals / Non-Goals

- Goals: N projects open concurrently under one parent; zero behavior
  change in single-project mode; a shareable text manifest; the
  selection UX where feature flags live.
- Non-Goals: nested workspaces; members outside the parent folder;
  cross-project *semantic* search (kg-routing); any cross-project
  derived data (federated-kg); per-member window/tab UI.
  *(Amended by `workspace-group-folders`: a member may now sit one level
  down inside a group folder — `SR/ShatteredRealms` — which is still not
  a nested workspace; the workspace stays flat, only member paths gained
  one segment. See that change's D6.)*

## Decisions

### D1. `.ken-workspace/` beside the members, not app data

The manifest is user-visible, git-shareable text — same philosophy as
`.ken/project.json` ("shared text source of truth"). Derived
workspace data (the federated KG DB, later) also lives here but is
disposable. Members are stored as **relative folder names** so the
manifest survives the parent being moved or synced to a teammate.

### D2. `AppState` becomes an enum, not a Vec

```rust
enum AppMode {
    Single(ProjectHandle),
    Workspace { config: WorkspaceConfig,
                projects: HashMap<Uuid, ProjectHandle>,
                focused: Uuid },
}
struct AppState { mode: Option<AppMode>, ... }
```

Rejected: `projects: Vec<ProjectHandle>` with `len()==1` meaning
single — it makes the flag-off path a special case of new code, so a
workspace bug could break single-project Ken. With the enum, flag-off
never constructs `Workspace` and the old path stays literally the old
code. `ProjectHandle` is a pure extraction of today's `ActiveProject`
fields — **no logic changes in the extraction commit**.

### D3. Command routing via one helper

A single `fn focused(&mut AppState) -> Result<&mut ProjectHandle>`
replaces today's `active.as_mut().ok_or(...)` idiom everywhere. Every
existing command changes only that line. Commands that are inherently
workspace-level (`workspace_overview`, `focus_project`) are new and
match on the enum explicitly. This keeps the refactor mechanical and
reviewable by a smaller model.

### D4. Resource governance

- **LLM**: already a process-wide singleton queue — nothing to do;
  member background jobs (digests, knowledge model) naturally
  interleave at Background priority.
- **Ingest**: a workspace-level semaphore caps concurrent member
  ingests at 2. Watchers all run (cheap); their debounced triggers
  queue behind the semaphore.
- **Memory**: DB handles are cheap; keep all members open. If a
  workspace exceeds 12 members, open lazily on first focus (simple
  LRU close beyond 12) — implemented from day one since it's ~30
  lines, avoiding a later scramble.

### D5. Discovery is shallow and dumb on purpose

One level deep, no recursion, no LLM. Tag with repo markers and file
counts; the human (and later the profiler) decides. Hidden folders,
`.ken-workspace`, and obvious junk (`node_modules`, `target`) are
never candidates.

### D6. All-projects ⌘K is a fan-out merge, FTS-only

Query each member's existing FTS search serially (they're
millisecond-fast), concat, sort by BM25 rank position interleave
(round-robin by member rank, not raw score — BM25 scores aren't
comparable across corpora), label with member name. Hybrid/semantic
fan-out is explicitly deferred to `kg-routing` where RRF handles the
merge properly.

## Risks / Trade-offs

- **The extraction refactor touches nearly every command** — mitigated
  by the two-commit rule (D2/D3): commit 1 is pure extraction with all
  existing tests green; commit 2 adds workspace mode.
- **Watcher storms** (e.g. `git checkout` across 7 repos) — the
  existing per-project debounce + the ingest semaphore bound the blast
  radius; worst case is sequential re-ingests, which is correct.
- **A member folder deleted while open** — member status goes
  `missing`, its handle is closed, the workspace stays up; reported in
  `workspace_overview`.

## Migration

No DB migration. `.ken-workspace/` appears only when a workspace is
created. Single-project users see zero change; the launcher entry
point is hidden without the global `workspace` flag.

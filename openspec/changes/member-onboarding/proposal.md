# Proposal: member-onboarding

## Why

A workspace of ten members, indexed for two days, has **zero** entities,
zero events, and no `kg.sqlite`. Every layer above the file scan is empty:

| layer | state | table |
| --- | --- | --- |
| file scan | 63,449 + 15,172 + 1,642 + … | `files` |
| chunks / embeddings | **0 everywhere** | `chunks` |
| entities / edges / events | **0 everywhere** | `entities`, `entity_edges`, `events` |
| workspace knowledge graph | **file does not exist** | `kg.sqlite` |

`federatedKg` and `kgRouting` are both **on**. They are inert, because
routing has no graph to route with and the graph has no member models to
federate. Ken is doing keyword search and nothing else, while presenting
as a system that clusters and routes.

There are two independent causes, and neither is a missing feature.

**Cause one: the incremental extraction worker is idle.**
`extraction_worker` pauses unless `local_llm::llm_status()` is `Ready`,
and `Ready` means "the selected model file is on disk". No model is
installed — `<app-data>/ken/models` does not exist — so the worker has
been sleeping in a two-second loop since the day the workspace opened,
and `extractions` has never been processed for any member. The knowledge
model is composed from those extractions, so it has nothing to compose.

**Cause zero: the Advanced language model cannot do the job it is offered for.**
The catalogue's Language tiers are `Qwen3-4B-Instruct-2507` (Recommended)
and `Qwen3-8B` (Advanced). The 4B is the **Instruct** variant and answers
directly; plain `Qwen3-8B` is the hybrid reasoning model and emits
`<think>` before answering. `generate_json` cannot parse that, so choosing
Advanced produces `no JSON object found in the model output` on nearly
every file — measured at 47 errors to 1 success before the model was
switched back. A model offered as "smarter answers" must not be one that
breaks the JSON contract every extraction depends on: either strip the
reasoning block before parsing, or select a non-thinking build for the
Advanced tier.

**Cause one-and-a-half: the worker exits for every member but one.**
`extraction_worker`'s loop resolves the active project with
`guard.members.values().next()` and returns unless that member's id equals
its own. With a single open project that is always true. With a workspace
of ten members it is true for whichever member `HashMap` iteration happens
to yield first, so the other nine workers exit on their first tick — and
with no workspace open, `values().next()` is `None` and all ten exit. The
same single-project assumption `AppState.workspace` carries, one layer
down.

**Cause two: one missing call.**

`knowledge_model::should_auto_build` is a complete, pure, unit-tested
decision function, and it already handles exactly this case:

```rust
if s.never_built {
    // A project with no model gets one even if nothing changed today.
    return settled(FIRST_BUILD_SETTLE);
}
```

`AutoBuildTracker` is fully wired for **input**: `changed()` fires from the
watcher and from every file mutation command, `build_started` and
`build_finished` bracket each build, and one tracker is constructed per
member and stored on `MemberRuntime`. But **`should_build()` has no caller
outside `knowledge_model.rs`'s own tests.** The tracker accumulates state
that nothing ever reads, and the automatic first build the constants were
written for (`FIRST_BUILD_SETTLE` = 60s, `MIN_AUTO_INTERVAL` = 30min) has
never fired for any project.

So the honest description of today is: knowledge models are built only by
`refresh_knowledge_model`, by hand, on the focused member. With ten members
that is ten deliberate acts, on ten separate focus changes, and the
workspace graph only appears 30s after a burst of them completing. Nobody
was ever going to do that, which is why the graph does not exist.

## What Changes

- **Wire the tick that already has its decision function.** A slow timer
  per resident member snapshots `AutoBuildContext` and calls
  `AutoBuildTracker::should_build`; when true, it starts the same
  `KnowledgeBuild` job `refresh_knowledge_model` starts. No new policy —
  `should_auto_build` is the policy and it is already written and tested.
- **Onboarding is an explicit state, not a side effect.** A member joining
  a workspace enters a queue: scan → knowledge model → contribute to the
  workspace graph. The existing 30s `schedule_workspace_kg_debounce`
  already fires the graph build off member completions, so the graph
  follows for free once models start landing.
- **Dormant members are onboarded too.** A tick that only ever sees
  resident members would leave most of a ten-member workspace unanalysed
  forever, because the resident cap keeps most members dormant. Onboarding
  admits a dormant member long enough to build its model, then releases
  it — the same "open by id, do the work, drop it" shape
  `ken-home-workspace` established for searching dormant members.
- **A visible queue.** The members strip gains per-member onboarding state
  — queued, scanning, mapping, ready, failed, skipped — so "why is search
  bad" has an answer on screen instead of in a log.
- **Incremental, because the incremental engine already exists.** There is
  no file limit and no "this member is too big" skip. `extraction_worker`
  is already one background worker per project, walking the `extractions`
  queue **one file at a time**, at the local model's background priority,
  yielding to interactive work and requeueing errored rows when the model
  recovers. `extraction_coverage()` already returns `(analyzed, total)`.
  Onboarding surfaces that coverage as progress; it does not gate on size.
  A 63,449-file member is simply a member that takes longer, and can be
  watched doing it.
- **A workspace can arrive already configured.** For a team, the config is
  committed and the index is not: `.ken/project.json` is git-tracked and
  carries the project **id** — which is what makes `ken://` addresses and
  index filenames mean the same thing on every machine — and `.kenignore`
  travels with the repo. A teammate cloning has every tier decision and
  every project identity already, and needs only the local index built.
  Onboarding SHALL treat "configured but unindexed" as its own case,
  adopt the committed config unchanged, and never mint a new id for a
  project that already has one.

## Capabilities

### New Capabilities
- `member-onboarding`: the queue a member passes through on joining a
  workspace, the automatic first knowledge build, dormant-member
  onboarding, the per-member visible state, and the corpus budget.

### Modified Capabilities
- none. This changes no existing contract; it connects two that already
  exist.

## Impact

- `src-tauri`: one tick thread and its snapshot plumbing; onboarding queue
  state on `AppState`; admit-and-release for dormant members; a
  members-overview field per member. `should_auto_build` and
  `AutoBuildTracker` are untouched.
- `crates/ken-core`: none expected. If `AutoBuildContext` turns out to
  need a field the app cannot supply, that is the only anticipated change.
- Frontend: onboarding state in the members strip; a "map this member now"
  action that reuses `refresh_knowledge_model`.
- Flags: no new flag. The build itself already requires the Claude Code
  CLI (`should_auto_build` returns false without it), and the graph half
  stays behind `federatedKg`.
- Tests: a never-built member builds once its scan settles; a second tick
  inside `MIN_AUTO_INTERVAL` does not; a dormant member is onboarded and
  is dormant again afterwards; a member over the corpus budget is skipped
  with a reason; ten members produce one graph build, not ten.

## Risks

- **Cost.** This turns "nothing happens" into "a Claude session per member",
  which is the point, but it must be legible and bounded before it is
  automatic. The budget check and the visible queue are that.
- **The tick must stay silent during a burst.** `should_auto_build` was
  written for exactly this and its tests assert it; the tick must not
  add its own retry on top.

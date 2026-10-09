# Tasks: member-onboarding

Task 1 is the bug fix and is worth landing on its own: it is one tick
calling one function that is already written and tested. Everything after
it makes the result visible and bounded.

## 0. The Advanced language model breaks JSON extraction

- [ ] 0.1 `generate_json` fails on `Qwen3-8B` because it is the hybrid
  reasoning model and emits `<think>` before the answer, while the
  Recommended `Qwen3-4B-Instruct-2507` answers directly. Measured on a real
  corpus: **47 errors to 1 success**, all `no JSON object found in the
  model output` or `trailing characters at line 1 column 5`.
- [ ] 0.2 Fix by stripping a leading reasoning block before parsing —
  cheap, and it makes every future thinking model work — and/or curate a
  non-thinking build for the Advanced tier.
- [ ] 0.3 Until then the Advanced Language entry is a trap: it is offered
  as "smarter answers, needs more memory" and silently fails the contract
  extraction depends on. Say so in the catalogue blurb if the fix lands
  later than the warning can.
- [ ] 0.4 Tests: a model output wrapped in a reasoning block parses; the
  existing plain-JSON path is unchanged.

## 1. src-tauri — call the decision function that exists

- [ ] 1.1 Add a slow tick (30s) that, for each resident member, builds an
  `AutoBuildContext { claude_available, in_flight, indexed_files,
  never_built }` and calls `AutoBuildTracker::should_build`. `never_built`
  comes from `Db::knowledge_model_built_at().is_none()`.
- [ ] 1.2 On true, construct the same `KnowledgeBuild` that
  `refresh_knowledge_model` builds, with `quiet_failure: true`, and hand it
  to `start_knowledge_build`. Reuse the existing `knowledge_running` guard;
  do not add a second one.
- [ ] 1.3 Do **not** re-implement or tune any threshold in the app layer.
  `should_auto_build` is the policy. If a constant is wrong, change it in
  `knowledge_model.rs` where its tests live.
- [ ] 1.4 Confirm `AutoBuildTracker::scanning` is actually cleared when the
  initial scan finishes — the tracker starts `scanning: true` and a build
  may not run against a half-walked folder. If nothing clears it today,
  that is a second half of this same bug.
- [ ] 1.6 **`extraction_worker` exits for every member but one.** Its loop
  resolves the active project with `guard.members.values().next()` and
  `return`s unless that member's id equals its own `project_id`. With one
  open project that is always true; with a workspace of ten it is true for
  whichever member `HashMap` iteration happens to yield, so the other nine
  workers exit on their first tick and their extraction queues never move.
  With no workspace open at all, `values().next()` is `None` and every
  worker exits. Look up the worker's OWN project by id instead of taking
  the first member — the same single-project assumption `AppState.workspace`
  carries, one layer down.
- [ ] 1.7 Tests: with N resident members, N extraction workers each process
  their own queue; a worker whose project closes exits; a worker whose
  project is merely not-first does not.
- [ ] 1.5 Tests: a never-built member with a settled scan builds once; a
  second tick inside `MIN_AUTO_INTERVAL` does not; a member with no Claude
  CLI never builds; a burst of changes produces exactly one build.

## 2. src-tauri — the onboarding queue

- [ ] 2.1 Per-member onboarding state on `AppState`: `Queued`, `Scanning`,
  `Mapping`, `Ready`, `Failed(reason)`, `Skipped(reason)`. Derived from
  signals that already exist (scan completion, `knowledge-model-state`)
  rather than a parallel source of truth.
- [ ] 2.2 A member joining a workspace enters the queue. Members already in
  a manifest at open are enqueued too, so an existing workspace is
  onboarded rather than only new additions.
- [ ] 2.3 One build at a time across the whole workspace, resident or not.
- [ ] 2.4 Emit onboarding transitions as a member-scoped event so the
  frontend can render without polling.
- [ ] 2.5 Tests: joining enqueues; an existing workspace enqueues every
  unbuilt member at open; two members never build concurrently.

## 3. src-tauri — dormant members

- [ ] 3.1 Onboard a dormant member by opening its index by project id,
  running the build, and releasing it. Do not activate it, do not start
  its watcher, do not evict a resident.
- [ ] 3.2 Admission for onboarding must not reorder the LRU — it is not a
  focus.
- [ ] 3.3 Tests: a dormant member gets a model and is dormant afterwards;
  the LRU order is unchanged; a `Missing`/`Invalid` member is `Skipped`
  with that reason, never an error.

## 4. Incremental progress, and the idle worker

- [ ] 4.1 **Surface `extraction_coverage()` as onboarding progress** —
  `Mapping 12,431 / 63,449` — per member. No file limit, no size-based
  skip: the worker is already one file at a time at background priority.
- [ ] 4.2 Make the **idle local model** visible and actionable. The
  extraction worker sleeps whenever `llm_status()` is not `Ready`, and
  `NotInstalled` is the state on a fresh machine, so today it sleeps
  forever in silence. Onboarding SHALL report `Waiting for the local
  model` as a distinct state with the install action attached, not
  `Queued` forever.
- [ ] 4.3 Confirm what the extraction worker actually needs installed and
  how big it is, and put that number in front of the user before the
  download rather than after.
- [ ] 4.4 Tests: coverage is reported while a member is mapping; a member
  with no local model reports waiting, not queued; installing the model
  resumes the queue without a restart (the worker's `was_ready` edge
  already requeues errored rows — assert it).

## 4b. Configured-but-unindexed (the team case)

- [ ] 4b.1 Detect the case: `.ken/project.json` present, no local index.
  Adopt the committed config unchanged.
- [ ] 4b.2 **Never mint a new project id** for a project that has one.
  Assert this — a regenerated id silently forks the address space and
  makes every shared `ken://` and `mod:`/`tools:` locator resolve wrong.
- [ ] 4b.3 The first scan honours the committed `.kenignore`, so a
  teammate never indexes what the repo already says to skip.
- [ ] 4b.4 A workspace manifest committed at the parent adopts unchanged,
  the same adopt-if-exists discipline `Workspace::create` already applies.
- [ ] 4b.5 Tests: cloning a configured project builds an index under the
  **same** id; the committed `.kenignore` applies on the first scan, not
  the second; adopting a manifest does not rewrite it.

## 5. Frontend

- [ ] 5.1 Onboarding state per member in the members strip, with the reason
  visible for `Skipped` and `Failed`.
- [ ] 5.2 A "map this member now" action wired to the existing
  `refresh_knowledge_model`, which already ignores every threshold.
- [ ] 5.3 Nothing new when the workspace has no unbuilt members — a strip
  that always shows a queue is noise.

## 6. Verification

- [ ] 6.1 On a workspace of ten members with no models, every eligible
  member ends `Ready` without anyone triggering a build by hand, and
  `kg.sqlite` exists afterwards.
- [ ] 6.2 Exactly one workspace-KG build results, not ten — the existing
  30s debounce collapses the burst.
- [ ] 6.3 With `federatedKg` off, models still build and no graph is
  written.
- [ ] 6.4 Re-running with everything `Ready` starts no builds.
- [ ] 6.5 Record the real numbers — members onboarded, files eligible per
  member, wall-clock, and whether any member had to be skipped — since the
  claim this change is making is precisely that the numbers stop being
  zero.

# Design: member-onboarding

## Context

Three pieces already exist and do not touch each other:

1. `knowledge_model::should_auto_build` — a pure decision function with
   four constants (`FIRST_BUILD_SETTLE` 60s, `CHANGE_QUIET` 5min,
   `MIN_AUTO_INTERVAL` 30min, `MAX_DEFER` 30min) and a `never_built`
   branch written specifically for first contact with a project.
2. `AutoBuildTracker` — per-member bookkeeping, fed by the watcher and by
   every mutation command, read by nobody.
3. `schedule_workspace_kg_debounce` — fires a workspace graph build 30s
   after member knowledge models complete.

(1) and (2) are joined by a call that was never written. (3) is waiting on
(1) and (2) to produce something. This change writes that call and makes
the resulting process visible.

## Goals / Non-Goals

- **Goals**: a member that joins a workspace ends up analysed without
  anyone remembering to ask; the existing decision function is the only
  policy; dormant members are not permanently excluded; the cost is
  visible and bounded before it is spent.
- **Non-Goals**: changing `should_auto_build`'s policy or constants;
  building the semantic index (a separate flag and a model download);
  a new extraction engine; parallel builds; re-running models on a
  schedule.

## Decisions

### D1. The tick calls the function that already exists

A slow timer (30s, matching the KG debounce granularity) walks resident
members, snapshots `AutoBuildContext { claude_available, in_flight,
indexed_files, never_built }`, and calls `tracker.should_build(ctx, now)`.
On true it constructs the same `KnowledgeBuild` that
`refresh_knowledge_model` constructs, with `quiet_failure: true`.

Rejected: a fresh policy in the app layer. The policy is written, pure and
tested; a second one in `lib.rs` would drift from it immediately and could
not be unit-tested at all.

### D2. Onboarding is a queue with observable states

`Queued -> Scanning -> Mapping -> Ready`, plus terminal `Failed(reason)`
and `Skipped(reason)`. This is not new machinery — scanning already has a
signal, `knowledge-model-state` already emits building/ready/error — it is
naming the sequence so the members strip can render it and so "search is
bad" has a diagnosis.

`Skipped` is a first-class outcome, not an error: no Claude CLI, member
`Missing`/`Invalid`. Each carries the reason as text. Size is never a
reason (D4). A member waiting on the local model is `Waiting`, which is
distinct from `Queued` — the difference between "your turn is coming" and
"nothing will happen until you install something".

### D3. Dormant members get onboarded, then released

The resident cap keeps most of a ten-member workspace dormant, so a tick
that only sees residents would analyse two or three members and quietly
never touch the rest — which reads as "Ken doesn't work" rather than
"seven members are dormant".

Onboarding therefore admits a dormant member (open its index by project
id, the shape `ken-home-workspace` established for dormant search), runs
its build, and releases it. One at a time, never concurrently with a
resident build, and admission for onboarding does not count as a focus so
it must not disturb the LRU order.

Rejected: onboarding every member at open. Ten Claude sessions on opening
a workspace is a bill and a stampede.

### D4. Incremental already exists; surface it, do not cap it

**No file limit, and no size-based skip.** `extraction_worker` is already
one background worker per project stepping through the `extractions`
queue a file at a time, at the local model's background priority, standing
aside for interactive work and requeueing errored rows when the model
recovers. `extraction_coverage()` already returns `(analyzed, total)`.

So a large member is not a problem to gate on; it is a member with a long
progress bar. Onboarding's job is to make that progress legible —
`Mapping 12,431 / 63,449` — not to refuse it. A cap would also be a
guarantee we cannot keep: the member Ken most needs mapped may well be the
biggest one.

`.kenignore` remains the way to say "map the source, not the 34k generated
asset configs", because `Tier::SearchOnly` is already excluded from
knowledge-model extraction. That is a statement about relevance, made once
per repo and committed, not a runtime budget.

Rejected: a pre-flight count with a skip threshold (an earlier draft of
this design). It answers a question nobody asked, invents a knob to tune,
and turns the largest and most valuable member into the one that never
gets analysed.

### D7. Configured-but-unindexed is its own case, and the common one for teams

What is committed and what is local:

| artefact | travels with the repo | rebuilt locally |
| --- | --- | --- |
| `.ken/project.json` (incl. project **id**) | yes | no |
| `.kenignore` | yes | no |
| `.ken-workspace/workspace.json` | yes, when the parent is a repo | no |
| `<app-data>/ken/index/<id>.db` | **no** | yes |
| `kg.sqlite` | **no** | yes |

The project id being committed is load-bearing: it is why `ken://` addresses
and index filenames mean the same thing on every machine, and why a
teammate's `mod:src/...` locator resolves to the same project as yours.

So the team case is not "set this up", it is "build the local index for a
setup that already exists". Onboarding SHALL adopt committed config
unchanged and SHALL NOT mint a new id for a project that has one — doing so
would silently fork the address space and make every shared locator wrong.
The first scan SHALL honour the committed `.kenignore`, so a teammate never
indexes 2 GB of assets that the repo already said to skip.

### D5. The graph is already automatic

Nothing new. `schedule_workspace_kg_debounce` fires 30s after member
knowledge-model completions, and re-checks `federatedKg` when the timer
lands. Ten members completing inside a window collapse into one build,
which is what the debounce was built for.

### D6. No new flag

`should_auto_build` already returns false without the Claude CLI, so the
"is this even possible" gate exists. The graph half is already behind
`federatedKg`. Adding an `autoKnowledge` flag would mean shipping a
default-off switch for the behaviour the constants were written to
provide, i.e. shipping the bug again.

## Risks / Trade-offs

- **A tick that fires too eagerly spends money.** Mitigated by using the
  existing policy unchanged (`MIN_AUTO_INTERVAL` is 30 minutes), by the
  single-in-flight guard, and by the pre-flight count.
- **Dormant admission could churn the LRU.** Mitigated by admitting
  outside the focus path and releasing immediately.
- **A large member holds the queue for a long time.** One build at a time
  is deliberate, but a 63k-file member could occupy it for hours. The
  answer is ordering and visible progress, not exclusion (D4): map the
  small, heavily-cited members first so the workspace becomes useful
  early, and let the large one finish behind them. Whether ordering should
  be by size, by citation count, or by recent focus is worth deciding from
  use rather than here.

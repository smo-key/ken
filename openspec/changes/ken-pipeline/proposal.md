# Proposal: ken-pipeline

## Why

`ken-tasks` shipped a board whose columns are *states*
(`backlog|todo|doing|review|done`). What the user actually runs is a
**work pipeline**: "a personal ticket board whose columns are a work
pipeline, where each lane has its own agent that Ken kicks off
automatically."

The lanes, in order:

```
Ideas backlog → Backlog → To Do → Investigation → Refinement →
Programmer → Tester → Architect review → QA tester →
Sign-off (human) → Documentation ⟲ back into Ideas backlog
```

plus a **Blocked** lane that any ticket can be parked in — waiting on
another ticket, an external decision, an upstream release, or a
person.

There are two failure edges — Tester failure returns the ticket to
Programmer, Architect-review failure returns it to Refinement (or
Programmer) — and one generative edge: Documentation is terminal
*and* a producer, updating docs then inspecting the finished ticket
for new opportunities and filing them back into the Ideas backlog.
That loop is the flywheel: finished work proposes the next work.

Four things make this a feature rather than "rename some columns":

1. **Twelve lanes do not fit a five-value typed enum.** `TaskStatus`
   is typed in `ken-core`, in the Tauri commands, in `ken-mcp`'s
   schemas, and in the board UI. Different repos also want different
   pipelines. The lane set has to become data.
2. **Lanes carry agents.** Each lane declares an agent identity, a
   default model type, and whether Ken may start it without asking.
   A ticket carries its own scope boundary and verification command
   so a lane's agent has a hard edge to work inside.
3. **Work gets stuck, and stuck work needs one home.** A ticket can
   be blocked by another ticket (possibly in another project) or by
   a stated external reason. Blocked work must record where it will
   resume, must never be picked up by a lane agent, and must never
   silently start an agent the moment its blocker clears.
4. **The backlog gets big.** Ken needs MCP tools that pull tickets
   **by lane, paginated** — the backlog must never be handed to a
   model whole — plus tools to advance, claim, watch, ask "what is
   stuck and why", and produce a strong daily update ("these are
   waiting on your review", "these unblocked overnight").

Same core principle as every other feature here: **tickets are
markdown files; the pipeline is a derived view.** Lane definitions
are markdown+frontmatter too, human-editable, diffable, and read
through the byte-fidelity patch core `ken-tasks` already ships.

## What Changes

- **Pipeline definition files** — `.ken-workspace/pipelines/<id>.md`,
  markdown + frontmatter, human-owned. One file defines one ordered
  lane set. Each lane declares: `id` (the value that appears in a
  ticket's `status`), `name`, `maps_to` (which classic `TaskStatus`
  it projects onto), `agent`, `model`, `kickoff`
  (`auto|confirm|manual`), `on_pass`, `on_fail`, and flags
  (`human`, `terminal`, `generative`, `blocked`). Pipeline-level:
  `concurrency_cap`, `bounce_cap`, `auto` master switch.
  A `default.md` reproducing the lanes above is scaffolded on first
  enable.
- **Ticket frontmatter grows** (all additive, all riding the existing
  `#[serde(flatten)] extra` map so today's Ken round-trips them
  untouched): `pipeline`, `model`, `agent`, `scope` (path globs —
  the file boundary), `verify` (the command that proves the lane's
  work), `bounces`, `return_lane`, `blocked_by`, `block_reason`,
  `blocked_at`, `parent`, `spawned_by`, `projects` (multi-project
  tickets), `target` (`web|tauri|none`, for demo tooling). `status`
  keeps its existing key and now holds a lane id.
- **A Blocked lane with blocking rules.** Moving a ticket to Blocked
  records the `return_lane` it will resume in, so the board never
  forgets where the work belonged. `blocked_by` holds **ULIDs of
  other tickets** — globally unique, so a dependency works across
  task homes and projects where a path would not; `block_reason`
  holds free text for everything else (an external decision, an
  upstream release, a person); the two coexist. Setting a block runs
  **write-time cycle detection** — an edge that would close a
  dependency cycle is refused with the cycle path, never discovered
  later. **No lane agent ever picks up a blocked ticket**, enforced
  in the single admission function both the UI and MCP claims go
  through. When blockers clear, the ticket returns to its
  `return_lane` **through that lane's normal confirmation gate** —
  never straight into a run — and is called out in the daily update
  as "unblocked overnight".
- **Status vocabulary becomes board-scoped.** A ticket with no
  `pipeline` key validates against the classic five exactly as
  today. A ticket with a `pipeline` key validates its `status`
  against that pipeline's lane ids; the lane's `maps_to` supplies
  the classic `TaskStatus` so the needs-attention tray, the classic
  Kanban, `task_list`'s status filter, and the daily board keep
  working unchanged. No existing task file is rewritten.
- **Run records** — `.ken-workspace/runs/YYYY-MM/<ulid>.md`, one
  append-only record per agent run: ticket, lane, agent, model,
  scope, verify command and its result, start/end, outcome
  (`pass|fail|blocked`), artifacts. The *queue*, *what is running*,
  and *what is stuck* are derived from these plus the tickets, and
  are rebuildable.
- **Kickoff with gates**: every lane declares whether it auto-runs
  or requires confirmation; a workspace-wide concurrency cap limits
  simultaneous runs (default 1 — one heavy build at a time is this
  machine's real limit); a ticket missing `scope` or `verify` can
  never auto-run; a blocked ticket is refused before any gate is
  even evaluated.
- **Bounce cap**: `Tester → Programmer` and `Architect → Refinement`
  increment a per-ticket `bounces` counter. Exceeding `bounce_cap`
  **blocks** the ticket (`block_reason: exceeded retry cap`) and
  escalates it to the human instead of looping — reusing the Blocked
  mechanism rather than inventing a second notion of stuck, so there
  is exactly one place a human looks for work that is not moving.
- **Sign-off is a human lane**: Accept advances the ticket;
  Accept-with-comments spawns a **new child ticket** into To Do
  carrying the comment (`parent:` back-reference) *and* lets the
  parent proceed.
- **QA tester lane does two jobs**: (1) durable — verify long-term /
  end-to-end test plans exist and cover the change, writing them
  into the repo if missing; (2) throwaway — optionally produce a
  walkthrough, screenshots, or a recorded demo *purely for human
  review*, written only under `.ken-workspace/artifacts/<ticket>/`
  and marked `durable: false` so they can never rot into the real
  suite. Recording tooling is chosen per ticket `target`.
- **Documentation lane closes the loop**: updates docs, then
  proposes new ideas. Each proposal is deduped against existing
  tickets (semantic search / federated KG when on, FTS + title match
  when off) and must cite the ticket that spawned it via
  `spawned_by:` before it lands in the Ideas lane.
- **MCP tools** (seven, additive to the four task tools):
  `pipeline_list(lane, …, limit, cursor)` — paginated, compact rows,
  never bodies, with `blocked` / `blocked_by` / `newly_unblocked`
  filters because "what is stuck and why" is one of the highest-value
  questions Ken can answer about a large backlog; `pipeline_get(id)`;
  `pipeline_claim(id, agent)`; `pipeline_advance(id, outcome,
  report)`; `pipeline_block(id, {blocked_by?, reason?})` /
  unblock; `pipeline_runs(filter)`; `pipeline_digest(day?)`.
- **Ken chat tools**: same seven, thin wrappers over the same core.
- **Pipeline board view**: horizontally scrolling lanes, per-project
  filter chips, card symbol top-left (colour carries lane, so the
  symbol carries project identity), model/agent/bounce badges, a
  blocked badge naming the return lane and blockers, a run tray, and
  the Sign-off accept dialog.
- **Project identity + links**: each project declares a short
  `symbol` (and optional colour); `workspace.json` gains a `links`
  array declaring that two projects feed off each other (e.g.
  ShatteredRealms ↔ ShatteredRealmsTools), seeded into the federated
  KG as `manifest`-provenance edges.
- **Flag**: `kenPipeline` (workspace-level, requires `workspace` and
  `kenTasks`). Off ⇒ no pipeline view, no pipeline tools, no
  folders, no runners — byte-identical to today's ken-tasks board.

## Capabilities

### New Capabilities
- `ken-pipeline`: per-board lane definitions with classic-status
  projection, lane agents with model defaults, gated kickoff with a
  concurrency cap, ticket scope/verify boundaries, bounce-capped
  loop-backs, a Blocked lane with recorded return lanes, ticket-ULID
  dependencies, write-time cycle detection and gated unblocking,
  human sign-off with comment-spawned child tickets, QA
  durable-vs-throwaway split, documentation→ideas dedupe loop, run
  records, seven MCP pipeline tools, pipeline board view, project
  symbols and workspace project links.

### Modified Capabilities
- `ken-tasks`: `status` validates against a board-scoped lane set
  when a ticket names a pipeline; needs-attention validates against
  the lane definition and gains unknown-blocker / unknown-return-lane
  reasons; `TaskFilter` gains `lane`, `pipeline`, and `blocked`.
- `mcp`: seven new pipeline tools alongside the four task tools.
- `chat`: pipeline tools; kickoff-confirmation and sign-off cards.
- `ken-memory`: `pipeline_digest` writes its daily update through
  `journal_append` when `kenMemory` is on.
- `federated-kg`: workspace-declared project links become
  `manifest`-provenance cross-member edges.
- `semantic-index` / `kg-routing`: consumed read-only by idea dedupe.

## Impact

- `crates/ken-core`: new `pipeline.rs` — lane-definition model and
  parse, lane resolution and `maps_to` projection, transition
  resolution (`on_pass`/`on_fail`), bounce accounting, the block
  model (return lane, ULID dependency graph, write-time cycle
  detection, unblock evaluation), gate/concurrency admission logic,
  run-record model and pathing, sign-off child-ticket composition,
  artifact manifest model, digest composition, dedupe candidate
  scoring — all pure and table-testable. `tasks.rs` gains lane-aware
  validation and three filter fields.
- `src-tauri`: pipeline definition load + watcher; run queue and
  admission; kickoff commands (manual first, auto later behind the
  pipeline `auto` switch); block/unblock commands and
  on-terminal-transition blocker re-evaluation; pipeline board state
  + events; sign-off and confirmation flows; flag gate.
- `crates/ken-mcp`: seven tool registrations with pagination
  discipline.
- Frontend: pipeline board view, Blocked column with return-lane and
  blocker badges, block/unblock dialog, run tray, confirmation
  dialog, sign-off dialog, artifact viewer, project filter + link
  toggle.
- Tests: lane-definition round-trip; ticket with unknown lane ⇒
  tray, never rewritten; `maps_to` projection keeps classic
  consumers correct; transition and bounce-cap tables; admission
  (gate + cap + missing scope/verify + **blocked refusal**);
  cycle-detection tables incl. long chains; unblock returns to
  `return_lane` behind a gate and starts nothing; child-ticket
  composition; dedupe scoring with and without the index; flag-off
  byte-identical.

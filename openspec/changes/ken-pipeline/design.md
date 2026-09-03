# Design: ken-pipeline

## Context

`ken-tasks` is shipped and load-bearing. `crates/ken-core/src/tasks.rs`
holds a typed five-value `TaskStatus` (`Backlog|Todo|Doing|Review|
Done`), `TaskKind` (`Human|Ai`), `BoardKind` (`Main|Daily`),
`TaskFilter{status,project,tag,assignee,kind,goal,board}`, a
needs-attention tray, goals with derived progress, archive pathing,
daily rollover, and — per spike S6 — a byte-fidelity patch core
(`patch_text` / `apply_patch`) that rewrites only named frontmatter
keys, reads with `serde_yaml` but writes with a raw line splitter, and
guards writes with an XxHash64 content-hash precondition. `src-tauri`
exposes thirteen task/goal/board commands plus a content-hash board
poller with per-path self-write dedupe emitting `board-state`.
`ken-mcp` exposes `task_create`/`task_list`/`task_update`/
`task_complete` with a documented claim convention. Eight flags are
registered in `crates/ken-core/src/features.rs`.

ken-pipeline sits **on top of** all of that. It adds no new file
format primitives: lane definitions, run records, and artifact
manifests are all markdown + frontmatter read and written through the
same patch core. What it adds is a **vocabulary that is data instead
of a Rust enum**, an **execution loop** with gates, a **single
mechanism for stuck work** (one Blocked lane, no parallel "halted"
state), and the **MCP surface**
that lets an agent work the loop without ever being handed the whole
backlog.

Known gaps in the shipped code. **Two of the three were fixed after
this plan was first written** (commit `ae6fb8a`, 2026-08-03):

- ~~There is no `TaskHome::Family` variant; family boards use a
  duplicated lister.~~ **FIXED** — `TaskHome::Family` / `HomeKind::Family`
  exist, the duplicate lister is gone, and family boards go through the
  shared `scan_tasks`. `Task.home` can now genuinely be `"family"`.
- ~~`<project>/.ken/` is dot-excluded, so per-repo task and memory
  folders are not indexed.~~ **FIXED** — `.ken/memory/**` and
  `.ken/tasks/**` are allowlisted across the walker, the watcher, and
  `refresh_path`. See the update note on D15.
- Family drag-drop is still disabled because the Tauri `task_update`
  path does not commit to git. **Still out of scope** — do not re-solve
  it here; it is why family-board pipelines remain a non-goal.

## Goals / Non-Goals

- Goals: a lane set that is per-board configuration rather than a
  typed enum; one agent per lane with an explicit model default and
  an explicit permission to start; loop-backs that cannot ping-pong
  forever; **exactly one place a human looks for stuck work**,
  covering both dependency blocks and retry-cap escalations; a
  documentation→ideas loop that produces signal instead of noise; an
  MCP surface that pages by lane and can answer "what is stuck and
  why"; a human sign-off gate that captures comments without blocking
  the parent; a cross-project board where project identity is visible
  at a glance.
- Non-Goals: replacing the classic Kanban (it stays, and pipeline
  tickets project onto it); scheduling/estimation/burndown; a build
  system (`verify` is a string Ken hands to an agent, not something
  Ken runs); multi-user pipelines; family-board pipelines (D15);
  making Ken itself write implementation code — Ken dispatches and
  records, agent-desktop implements (EXECUTION.md, Roles).

## Decisions

### D1. Lanes are a per-board definition file, not an enum (locked)

The user's pipeline has twelve lanes (eleven flow lanes plus
Blocked) and different repos want different pipelines; `TaskStatus`
has five variants typed into ken-core, the Tauri commands, `ken-mcp`'s
JSON schemas, and the board UI. Widening the enum would (a) still be
wrong for the next repo, and (b) force every consumer to know about
lanes it does not care about.

A **pipeline definition** is a markdown+frontmatter file at
`.ken-workspace/pipelines/<pipeline-id>.md`, human-owned, read
through the ken-tasks patch core:

```yaml
---
id: default
name: Standard delivery pipeline
auto: false            # master switch, D6 — off until manual E2E passes
concurrency_cap: 1     # D3
bounce_cap: 3          # D4
lanes:
  - id: ideas
    name: Ideas backlog
    maps_to: backlog
    agent: none
    kickoff: manual
    on_pass: backlog
  - id: backlog
    name: Backlog
    maps_to: backlog
    agent: none
    kickoff: manual
    on_pass: todo
  - id: todo
    name: To Do
    maps_to: todo
    agent: none
    kickoff: manual
    on_pass: investigation
  - id: investigation
    name: Investigation
    maps_to: doing
    agent: investigator
    model: sonnet
    kickoff: confirm
    on_pass: refinement
  - id: refinement
    name: Refinement
    maps_to: doing
    agent: refiner
    model: opus
    kickoff: confirm
    on_pass: programmer
  - id: programmer
    name: Programmer
    maps_to: doing
    agent: programmer
    model: sonnet
    kickoff: confirm
    writes_code: true
    on_pass: tester
  - id: tester
    name: Tester
    maps_to: review
    agent: tester
    model: sonnet
    kickoff: confirm
    on_pass: architect
    on_fail: programmer          # bounce edge
  - id: architect
    name: Architect review
    maps_to: review
    agent: architect
    model: opus
    kickoff: confirm
    on_pass: qa
    on_fail: refinement          # bounce edge
  - id: qa
    name: QA tester
    maps_to: review
    agent: qa
    model: sonnet
    kickoff: confirm
    on_pass: signoff
    on_fail: programmer
  - id: signoff
    name: Sign-off
    maps_to: review
    human: true                  # D11
    kickoff: manual
    on_pass: documentation
    on_fail: refinement
  - id: documentation
    name: Documentation
    maps_to: done
    agent: documenter
    model: sonnet
    kickoff: confirm
    terminal: true
    generative: true             # D7 — files ideas back into `ideas`
  - id: blocked
    name: Blocked
    maps_to: doing
    agent: none
    kickoff: manual
    blocked: true                # D5 — exactly one lane may set this
---
Free markdown: what this pipeline is for, per-lane briefs the
agents read, and the conventions each lane's agent must follow.
```

The lane `id` is exactly the string that appears in a ticket's
`status` frontmatter key — no second key, no rename of anything
`ken-tasks` already writes. Lane order in the file **is** column
order in the UI. The body is not decoration: it is the per-lane
brief that the lane's agent is given at kickoff, so the pipeline's
behaviour is editable by a human in a text editor.

`kickoff: manual` lanes with `agent: none` are pure holding
columns (Ideas, Backlog, To Do, Blocked) — no agent, no run, no risk.

Rejected: lanes as rows in a DB (breaks files-are-truth, invisible
to git); lanes as an enum with a `custom(String)` escape hatch (the
worst of both — still typed, still wrong per repo); one lane file
per lane (twelve files to reorder one column).

### D2. One `status` key; lanes project onto classic statuses (locked)

This is the migration decision, and it is deliberately conservative.

- A ticket **without** a `pipeline:` key is validated exactly as
  today: `TaskStatus::parse` against the five, unrecognised ⇒
  needs-attention tray. **Nothing about today's behaviour changes,
  and no existing task file is ever rewritten.**
- A ticket **with** `pipeline: <id>` is validated against that
  pipeline's lane ids. `Task` gains `lane: Option<String>` (the
  resolved lane id) and keeps `status: Option<TaskStatus>` — now
  filled from the matched lane's **`maps_to`** value rather than
  from `TaskStatus::parse`. `status_raw` keeps holding exactly what
  the file said, as it does today.
- Therefore every existing consumer keeps working untouched: the
  classic Kanban shows a pipeline ticket in the column its lane maps
  to; `task_list`'s `status` filter still matches; the daily board
  and rollover logic still see a `TaskStatus`; goal progress still
  counts `Done`.
- The **needs-attention tray is what changes**: `needs_attention`
  gains the board's lane set as an input.
  `AttentionReason::InvalidStatus` keeps its meaning for
  pipeline-less tickets, and a new `AttentionReason::UnknownLane`
  covers "ticket names pipeline P, whose definition has no lane
  `<x>`", plus `AttentionReason::UnknownPipeline` for a `pipeline:`
  id with no definition file. Both are surfaced, never rewritten —
  the same rule `apply_patch` already enforces for invalid status.
- `TaskFilter` gains `lane: Option<String>`,
  `pipeline: Option<String>`, and `blocked: Option<BlockedFilter>`
  (D5); all `Option`, all defaulted, so every existing caller and
  JSON schema round-trips unchanged.

Cost, stated plainly: the typed vocabulary stops being a compile-time
guarantee for pipeline tickets. Validation moves from `rustc` to a
runtime check against a file the user can edit. The mitigation is
that the check has exactly one home (`pipeline::resolve_lane`), it is
pure and table-tested, and its failure mode is the tray — the same
soft-landing ken-tasks already ships for hand edits.

**Migration is a no-op on disk.** There is no rewrite pass, no
schema bump, no file move. An existing board becomes a pipeline board
by (1) creating a definition file and (2) adding `pipeline:` to the
tickets you want to move over, one at a time. Mixed boards are a
supported steady state, not a transition window.

### D3. Per-lane confirmation gates, a concurrency cap, and a ticket-carried scope (locked — user confirmed)

Auto-kickoff on card move is the highest blast-radius idea in this
feature: a mis-drag would spawn an agent that writes code. Four
independent brakes, all required:

1. **Per-lane `kickoff`** — `manual` (never starts itself),
   `confirm` (Ken proposes a run; a dialog showing lane, agent,
   model, scope, and verify command must be accepted), or `auto`
   (starts on entry). `confirm` is the default for any lane with an
   agent; `auto` requires the lane to opt in *and* the pipeline's
   `auto: true` master switch (D6).
2. **`concurrency_cap`** — a workspace-wide cap on simultaneous
   runs, default **1**. This project learned the hard way that one
   heavy build at a time is the machine's real limit. Admission is
   a pure function over the run ledger: over cap ⇒ the run is
   `queued`, not started, and the tray says so.
3. **Ticket-carried boundary** — every successful agent session in
   this project had a hard file boundary, a verification command,
   and foreground-only builds. So a ticket carries `scope:` (a list
   of path globs) and `verify:` (the command that proves the lane's
   work). **A ticket missing either can never auto-run**: its gate
   is downgraded to `confirm` and it is listed in the tray with the
   reason. Ken does not run `verify` itself — it hands the string to
   the lane's agent and records the reported result in the run.
4. **Blocked refusal** — a blocked ticket (whether by a dependency,
   a stated reason, or the retry cap) is refused admission outright,
   before any gate is even evaluated (D5).

Rejected: a global "are you sure?" preference (one blanket answer for
twelve lanes of wildly different risk); trusting the agent to
respect scope without recording it (the record is what makes a bad
run diagnosable afterwards).

### D4. Hard cap on loop-backs; exceeding it blocks the ticket (locked — user confirmed)

`Tester → Programmer` and `Architect → Refinement` can ping-pong
forever burning tokens, and each individual hop looks reasonable —
which is exactly why the failure is silent.

- A transition is **backward** when the target lane's index in the
  definition is lower than the source's. Backward transitions
  increment the ticket's `bounces` counter (frontmatter, plain
  integer) and append a line to the ticket's `## Log`.
- When `bounces` would exceed the pipeline's `bounce_cap`
  (default 3), the transition is **refused**. Instead the ticket is
  **blocked via the D5 mechanism** with
  `block_reason: exceeded retry cap (<n> bounces)` and
  `return_lane` set to the lane it was bouncing to, and it is
  escalated to the human — surfaced in the digest's stuck group and
  called out on the sign-off review queue.

There is deliberately **no second "halted" concept**. A ticket that
blew the retry cap and a ticket waiting on an upstream release are
the same thing to the human who has to unstick them, so they use the
same fields, the same lane, the same filters, and the same digest
group. One place to look (D5).

Rejected: a token-budget cap instead of a hop cap (Ken cannot see
another agent's token spend); an exponential-backoff retry (delays
the burn instead of stopping it); a separate `halted: true` flag
parallel to blocking (two notions of stuck, two places to look, two
things to keep in sync).

### D5. Blocked: one lane, a recorded return lane, and no auto-run (locked scope — user asked for a lane)

Blocked is, strictly speaking, **orthogonal state rather than a
pipeline position**: a ticket blocked while in Programmer is still
logically *at* Programmer, and a plain Blocked column throws that
away — you come back and no longer know where it belonged. The user
asked for a lane, so there is a lane; the orthogonality is preserved
by making the return position part of the ticket.

**Fields** (all on the ticket, all riding the existing `extra`
flatten so today's Ken round-trips them):

| Key | Meaning |
|---|---|
| `status: blocked` | the lane, so it is a real column |
| `return_lane: <lane-id>` | **required** — the lane it will resume in |
| `blocked_by: [<ulid>, …]` | hard dependencies on other tickets |
| `block_reason: <text>` | free text for everything else |
| `blocked_at: <iso>` | when, so the digest can age it |

`blocked_by` and `block_reason` **coexist**: a ticket can be waiting
on ticket X *and* on a vendor's release, and both must be visible or
clearing one will look like it should unstick the ticket.

**`blocked_by` holds ULIDs, never paths.** The combined board spans
task homes and projects, so a path is neither stable nor unique; the
ULID already in every ticket's `id` is globally unique and survives
the file being renamed, moved between homes, or archived. A
`blocked_by` id matching no ticket surfaces as
`AttentionReason::UnknownBlocker` — surfaced, never rewritten,
exactly like an unknown goal id in ken-tasks D7.

**Blocking is a lane transition like any other**, so it goes through
the same patch core: `pipeline_block(id, {blocked_by?, reason?})`
records `return_lane` = the ticket's current lane *before* the move,
then sets `status: blocked`. Moving to Blocked without a
`return_lane` is impossible by construction — the command computes
it; a hand edit that produces `status: blocked` with no
`return_lane` lands in the tray.

**Unblocking never auto-runs an agent.** When every entry in
`blocked_by` has reached a terminal lane and `block_reason` is
cleared, the ticket returns to `return_lane` — and **re-enters
through that lane's normal gate (D3)**. Concretely: `kickoff:
confirm` lanes propose a run and wait; even `kickoff: auto` lanes are
treated as `confirm` for a ticket arriving via unblock, because an
overnight dependency completion silently spawning a code-writing
agent is precisely the blast radius D3 exists to control. The
unblocked-but-not-yet-started state is a first-class digest group:
**"these unblocked overnight"** leads the daily update right after
"waiting on your review".

**Cycle detection is at write time, not discovery time.** Setting
`blocked_by` runs a depth-first walk over the existing block graph;
if the new edge would close a cycle (A→B→A, or any longer chain) the
write is **refused** with the cycle path in the error. The graph is
small (blocked tickets only) and the walk is pure and table-tested.
Refusing at write time is the whole point: a cycle discovered later
is a board that has quietly stopped moving.

**Hard invariant: no lane agent ever picks up a blocked ticket.**
Stated as its own spec requirement with its own scenario, because it
is the rule most likely to be violated by a future automation change.
Enforcement is single-homed in `admit()` (D3 brake 4): blocked ⇒
`Refused`, checked *before* gate mode, before the cap, before
anything else. `pipeline_claim` re-runs `admit()` server-side so an
external agent cannot route around the UI. Any future auto-transition
work (D6/2.10) inherits the refusal for free because it calls the
same function.

**Rendering: both a column and a badge (recommended).** The Blocked
lane is a real column so the user gets what they asked for and stuck
work is impossible to miss. In addition, the card renders a badge
naming its `return_lane` ("blocked · returns to Programmer"), and the
`return_lane`'s column shows a muted ghost placeholder for tickets
blocked out of it, so the pipeline still reads as "this much work is
sitting at Programmer". OPEN-9 tracks whether the ghost placeholder
earns its complexity; the column and badge are not in question.

Rejected: blocked as a boolean flag with no lane (the user asked for
a lane, and stuck work that stays in-column is easy to scroll past);
blocked as a per-lane sub-column (twelve sub-columns, one
mechanism, no benefit); auto-unblock straight into a run (see above).

### D6. Manual kickoff ships before auto-transitions (locked sequencing)

Get one real ticket through every lane by hand-clicking first;
automate transitions second. Failure modes in this pipeline only
appear once real work flows through it — a lane brief that reads
fine is not a lane brief that produces a usable run.

Concretely: the pipeline definition's `auto` master switch ships
**false** and the frontend ships no auto-transition path in the
first pass. `tasks.md` layer 2 implements kickoff as an explicit
command only; layer 2's auto-transition step is gated behind
Verification 5.4's manual end-to-end scenario passing. This is a
sequencing decision, not a permanent limitation.

### D7. Idea dedupe before landing; the Ideas lane is inert

The Documentation→Ideas loop is the flywheel and will become noise.
Rules:

- A generated idea is a normal ticket with `status: ideas`,
  `origin: generated`, and a **required `spawned_by:` citation** —
  the id of the ticket whose completion produced it. An idea without
  a citation is refused at creation.
- Before landing, the proposal is deduped against existing tickets:
  `semantic_search` + `kg_search` when `semanticIndex` / `federatedKg`
  are on, falling back to FTS (`search_knowledge`) plus a normalized
  title match when they are off. Task homes already index at the
  **search-only** tier (ken-tasks D6), so tickets are searchable
  without minting KG entities. Above the similarity threshold, the
  idea is **not** created; instead a line is appended to the matched
  ticket's `## Log` noting the near-duplicate and its source.
- Dedupe scope defaults to the ticket's project plus any project
  **linked** to it (D12) — linked projects are exactly where a
  duplicate is likeliest to already exist.
- The Ideas lane is **inert by construction**: `agent: none`,
  `kickoff: manual`. Nothing an idea does can start a run, so
  auto-landing an idea has no blast radius. Grooming it onto Backlog
  is a human action.

### D8. Model assignment defaults come from evidence

Not vibes: EXECUTION.md's rubric plus the recorded per-model verdicts
from this project's own sessions. Per-lane default, overridable per
ticket via the ticket's `model:` key.

| Lane | Default | Why |
|---|---|---|
| Investigation | sonnet | well-specified reading and reporting |
| Refinement | **opus** | design-heavy, decides the shape of the work |
| Programmer | sonnet | the default implementer |
| Tester | sonnet | writes and runs real tests; judgment on failures |
| Architect review | **opus** | risk-bearing; the lane that catches bad shapes |
| QA tester | sonnet | test-plan judgment; drops to haiku for a pure coverage check |
| Documentation | sonnet | generative half (idea proposals) needs judgment |
| *mechanical checks* | **haiku** | prescriptive sweeps with one example defining the rest |

Ideas / Backlog / To Do / Blocked / Sign-off have no agent and
therefore no model.

Escalation stays one-way and cheap (EXECUTION.md): a lane agent that
hits ambiguity halts its run rather than pushing through, and the
ticket's `model` can be bumped for the retry.

### D9. QA lane: durable test plans in the repo, throwaway artifacts outside it

The QA lane does two distinct jobs and they have **different write
destinations**, enforced rather than merely documented:

1. **Durable** — verify long-term / end-to-end test plans exist and
   cover the change; write them if missing. These go **into the
   member repo**, inside the ticket's `scope`, like any other code
   change.
2. **Throwaway** — optionally produce artifacts purely for human
   review: a written walkthrough, screenshots, or a recorded demo
   video. These go **only** under
   `.ken-workspace/artifacts/<ticket-id>/`, never into a member
   repo, and the folder carries a `manifest.md` with
   `durable: false`, `ticket`, `created`, `expires`, and a list of
   the artifacts. The QA lane's brief states the boundary and the
   run record records which destination each output went to.

This is the same shape as ken-families' write lanes: one writer, one
destination, enforced in code — a throwaway artifact physically
cannot land in the real suite because the real suite is in a
different tree. Artifacts may be linked into documentation (a
walkthrough is genuinely useful there); linking copies the file into
the docs deliberately rather than promoting the throwaway folder.

Expiry is surfaced, never automatic: past `expires`, the artifact
folder shows in the tray with a prune action. Ken does not delete a
human's review material on a timer.

### D10. Recording/demo tooling is target-dependent

Playwright (trace viewer + video) is the right tool for web targets
but **cannot drive Ken itself** — Ken is Tauri v2, whose official
automation path is `tauri-driver` over WebDriver. Mandating one tool
would make the QA lane wrong for half the workspace.

So the ticket carries `target:`:

- `web` ⇒ Playwright, trace + video into the artifact folder.
- `tauri` ⇒ `tauri-driver` over WebDriver; screenshots + a written
  walkthrough. (Note for the executing model: `tauri-driver` on
  Windows needs a matching Edge WebDriver; verify availability
  before promising video for this target.)
- `none` (default) ⇒ written walkthrough only. Always available,
  never blocked on a driver.

The QA lane's brief selects the recipe from `target`. An unavailable
driver degrades to `none` with a note in the run record — it never
fails the lane.

### D11. Sign-off is a human lane; accept-with-comments spawns a child

`human: true` on a lane means: no agent, no kickoff, and the card
renders a review dialog instead of a run button. Sign-off offers two
outcomes:

- **Accept** ⇒ advance to Documentation.
- **Accept with comments** ⇒ *both* things happen: a **new child
  ticket** is created in the To Do lane carrying the comment as its
  body, `parent: <this ticket id>`, the same `project`/`projects`
  and `pipeline`, and no inherited `scope`/`verify` (the child is
  new work and must earn its own boundary — D3); **and** the parent
  advances to Documentation. The comment is also appended to the
  parent's `## Log` so the parent's history is self-contained.
- (Reject is the lane's `on_fail` edge — back to Refinement, and it
  counts as a bounce like any other backward transition.)

The child ticket is what stops sign-off from being a re-work
bottleneck: a comment does not block the thing that is already done.
If the parent genuinely *cannot* ship without the child, that is a
`blocked_by` edge (D5) — an explicit dependency, not an implicit one.

### D12. Project identity: a symbol per project, links in the manifest

Cards convey status by **colour**, so colour cannot also carry
project. Each project therefore declares a short `symbol` (1–3
characters or an emoji) rendered **top-left** on every card. Tickets
may span projects: `project` stays the primary (so today's project
filter is unchanged) and `projects:` lists the rest; a multi-project
card renders its primary symbol with a "+n" affordance.

Project links: `workspace.json` gains a `links` array —
`[{ from, to, relation, note? }]` — carried by the manifest's
existing `#[serde(flatten)] extra` map, so older Ken round-trips it
untouched. `federated-kg` seeds these as member-level cross-project
edges with a new provenance value `"manifest"` alongside the existing
`"imported" | "cooccur" | "llm"`. Leaning on federated-kg is the
right call because cross-project entity edges already exist there —
an explicit workspace-level link is a **stronger hint to a system
that already federates**, not a new subsystem. Links drive: dedupe
scope (D7), the board's "include linked projects" filter toggle,
cross-project `blocked_by` suggestions ("this change probably needs a
matching ticket in X"), and board filtering.

### D13. Run records are an append-only ledger; queue state is derived

One markdown+frontmatter file per run at
`.ken-workspace/runs/YYYY-MM/<ulid>.md`: `id`, `ticket`, `pipeline`,
`lane`, `agent`, `model`, `scope`, `verify`, `started`, `ended`,
`outcome` (`queued|running|pass|fail|blocked|cancelled`),
`artifacts`, with the agent's report as the body.

The ledger is the only durable execution state. **What is running,
what is queued, what is blocked, and what is waiting on a human are
all derived** from the ledger plus the tickets, exactly like the board
is derived from task files — so a crashed Ken restarts by re-reading
the folder, and `pipeline_runs` is a pure query. A `running` record
with no live process after restart is reported as `stale`, surfaced
in the tray, and never silently marked `pass`.

Monthly folders match ken-tasks' `archive/YYYY-MM/` convention and
keep the directory from becoming unlistable.

### D14. The runner seam (partly OPEN — see Open Questions)

Ken does not write implementation code (EXECUTION.md, Roles), and it
has no agent-spawning capability today. So the lane definition
declares a `runner`:

- **`mcp` (default, v1)** — a *pull* model. Kickoff writes a run
  record with `outcome: queued` and marks the ticket ready. An
  external agent calls `pipeline_runs({state: "queued"})`, then
  `pipeline_claim(id, agent)`, does the work inside `scope`, and
  reports via `pipeline_advance(id, outcome, report)`. Ken spawns no
  processes; every brake in D3 still applies because admission
  happens at `pipeline_claim`, not at the agent's discretion — which
  is also what makes the D5 blocked invariant hold for external
  agents.
- **`command`** — Ken spawns a configured process per run. This is
  the half that makes "Ken kicks it off automatically" literal, and
  it is the one thing in this design that lets Ken start a program.
  **Not implemented in v1 — locked by the user's ruling (OPEN-1,
  2026-08-03).** v1 is the `mcp` pull runner only: Ken never spawns a
  process. `command` requires its own change and risk review.

Everything else in this design is runner-agnostic on purpose: swapping
`mcp` for `command` later changes one module, not the model.

### D15. Pipeline tickets live in the workspace home in v1

> **UPDATE (2026-08-03, commit `ae6fb8a`): the gap this decision works
> around is now CLOSED.** `.ken/memory/**` and `.ken/tasks/**` are
> allowlisted through `scan::is_ken_allowlisted_path` and reachable via
> all three entry points (walker, watcher, `refresh_path`). The
> "one-line relaxation" noted at the end of this decision is therefore
> available immediately: per-repo pipeline tickets *can* be indexed in
> v1. The workspace-home default below still stands as the recommended
> v1 scoping — one home is simpler to reason about while the pipeline
> is new — but it is now a **preference, not a constraint**, and the
> executing model may relax it without waiting on anything. Re-read the
> original rationale below with that in mind.

`<project>/.ken/` *was* dot-excluded by `scan.rs` and `watch.rs`, so
per-repo task folders were not indexed. Idea dedupe (D7) depends on
tickets being searchable; per-repo pipeline tickets would have been
invisible to it and dedupe would silently under-match.

So in v1 **pipeline tickets are created in the workspace home**
(`.ken-workspace/tasks/`), where the ken-memory pseudo-member indexes
them at search-only tier. Per-repo tickets keep working exactly as
today for classic (non-pipeline) tasks. This is a scoping choice
around a known gap, not a fix for it: when the dot-exclusion gap is
closed, per-repo pipeline tickets become a one-line relaxation.
Ticket→project association is carried by `project`/`projects`, not by
which folder the file is in, so nothing about the model assumes the
workspace home — including `blocked_by`, which is a ULID precisely so
it keeps working across homes (D5).

### D16. Ideas are short, optional, and promoted by a human (locked)

User ruling, 2026-08-03. Resolves OPEN-4 and settles what the
documentation→ideas flywheel actually produces.

1. **An idea is a few sentences, not a ticket brief.** The
   documentation lane writes a title and a short body — enough to
   recognise the thought later. It does not write `scope`, `verify`,
   estimates, or a plan. Those are what *promotion* and the
   Investigation/Refinement lanes are for. A generated idea that
   arrives pre-planned is a lie about how much thinking has happened.
2. **Not every ticket produces ideas.** Proposing is an explicit,
   optional act by the documentation agent, never an obligation and
   never a required field on its report. A pipeline run that ends
   with no ideas is the normal case, not a failure to notice
   something.
3. **Ideas auto-land in the `ideas` lane** (inert by construction:
   `agent: none`, `kickoff: manual`), so nothing runs and the blast
   radius of a bad idea is one row in a list. Dedupe and the
   `spawned_by` citation (D7) still gate what lands.
4. **Promotion is the human editorial act.** A dedicated Ideas
   surface lists the short ideas with their source ticket, and
   promoting one advances it `ideas → backlog` — the lane's existing
   `on_pass` edge, so this needs no new transition machinery. Promote
   is deliberately *not* a kickoff: it moves an idea into the normal
   intake column, where it queues like any other work.

The point of the separation: the flywheel should cost almost nothing
to feed and require deliberate attention to act on. Cheap to capture,
explicit to commit.

## Risks / Trade-offs

- **Runaway token spend.** Thirteen lanes × auto-kickoff ×
  loop-backs is an unbounded bill, and every individual hop looks
  reasonable. Mitigations are structural, not advisory: `bounce_cap`
  (D4) stops ping-pong; `concurrency_cap` (D3) bounds parallel burn;
  `auto: false` by default (D6) means nothing runs unasked; the
  digest reports run counts per ticket so a ticket burning ten runs
  is visible on day one. **Residual risk: a long lane chain still
  runs seven agents for one ticket.** Accepted knowingly; the digest
  is the tripwire.
- **An agent writing code from a drag gesture.** The scariest
  failure: a mis-drop starts a programmer run against the wrong
  ticket. Mitigations: `confirm` is the default gate for every lane
  with an agent; the confirmation dialog shows scope and verify
  before anything starts; no `scope`/`verify` ⇒ no auto-run at all
  (D3); blocked tickets are refused before the gate is even read
  (D5); the run record captures exactly what was authorised.
  **Residual risk: a user who clicks through confirmations learns
  nothing from them.** Partly mitigated by showing the *diff of
  intent* (lane, model, scope) rather than a generic "are you sure".
- **Silent unblock spawning work overnight.** A dependency completing
  at 2am must not mean a code-writing agent ran at 2am. Mitigated by
  D5's rule that unblocked tickets always re-enter through
  `confirm`, even into an `auto` lane, plus the dedicated
  "unblocked overnight" digest group. **Residual risk: if OPEN-1
  later admits the `command` runner, this rule becomes the load-
  bearing one — it must be re-tested in that change.**
- **Blocking becoming a parking lot.** A Blocked column with no
  ageing turns into where tickets go to die, and `blocked_by` chains
  make it worse — one stuck ticket can freeze five. Mitigations:
  `blocked_at` ages every entry and the digest sorts stuck work
  oldest-first; write-time cycle detection (D5) stops the pathological
  case; blockers are shown transitively in the tray so the *root*
  blocker is visible rather than the nearest one. **Residual risk:
  ageing is a report, not a forcing function.**
- **Pipeline state drifting from git reality.** A ticket in
  `tester` whose branch was reverted, force-pushed, or never
  committed is a lie the board tells confidently — and ken-families
  already hit the related gap (the Tauri `task_update` path does not
  commit to git). ken-pipeline does **not** try to become a git
  integration. It reduces the lie surface instead: every run record
  stores the `verify` command and its reported result, so "this lane
  passed" is always backed by a named check rather than a lane
  position; `stale` runs after a restart are reported, not assumed
  passed (D13). **Residual risk: real. Recommend a `commit:` field
  on run records (OPEN-6) so a lane's claim can be pinned to a
  revision.**
- **Throwaway test artifacts rotting into the real suite.** A demo
  script that becomes a flaky CI test is a permanent tax.
  Mitigation is physical (D9): throwaway output lands only under
  `.ken-workspace/artifacts/`, marked `durable: false`, with an
  `expires` date and a prune action. **Residual risk: someone copies
  one into the repo by hand** — which is at least a deliberate,
  reviewable act.
- **The human becoming the bottleneck at sign-off.** Seven
  agent lanes feeding one human reviewer is a queue with one
  server, and Blocked adds a second human-only queue behind it.
  Mitigations: accept-with-comments never blocks the parent (D11);
  the digest leads with "waiting on your review" then "unblocked
  overnight" then stuck work, each ordered oldest-first; blocked and
  merely-awaiting-review are visually distinct. **Residual risk: the
  queue still grows faster than the human. This is the feature's real
  limiting factor and should be measured, not designed away** — if
  sign-off depth or blocked age grows monotonically for a week, the
  pipeline is over-automated for its reviewer.
- **Editing a lane definition under in-flight tickets.** Renaming or
  deleting a lane orphans every ticket sitting in it — and orphans
  every `return_lane` pointing at it. Handled by the tray, not by
  rewriting files: orphans surface as `UnknownLane` /
  `UnknownReturnLane` (D2, D5) with a "move to lane…" action. Never
  auto-migrated — silent bulk status rewrites are exactly what the
  patch core exists to prevent.
- **Lane briefs drifting from reality.** The definition body is
  prompt material; a stale brief produces confidently wrong runs.
  Partly mitigated by the Documentation lane (which sees the whole
  ticket and can propose a brief fix as an idea), but there is no
  automatic check. Worth a periodic human read.

## Migration

**None on disk, by construction (D2).** No rewrite pass, no schema
bump, no file moves, no new required frontmatter key on any existing
file. A board becomes a pipeline board when a definition file exists
and a ticket names it; tickets without `pipeline:` keep the classic
five-value vocabulary forever. Mixed boards are supported
indefinitely.

First enable scaffolds `.ken-workspace/pipelines/default.md` with the
twelve lanes above and creates `runs/` and `artifacts/` lazily on
first use. Flag off ⇒ no pipeline view, no pipeline tools, no
definition load, no watcher, no folders, and the ken-tasks board
behaves byte-identically to today.

## Open Questions (resolve before/during build; recommendation stated)

- ~~**OPEN-1 — May Ken spawn processes?**~~ **RESOLVED (user ruling,
  2026-08-03): v1 ships the `mcp` pull runner only.** Ken exposes
  claimable work; external agents connect and pull it. Ken spawns no
  processes in this feature. The `command` runner stays designed-for
  but unimplemented, and becomes its own change with its own risk
  review — see D14. Consequence to carry into that future change:
  D5's "unblocked tickets always re-enter through `confirm`" is
  currently a convenience; under `command` it becomes the
  load-bearing safety rule and must be re-tested as such.
- **OPEN-2 — Where does a project's `symbol` live?**
  **Recommendation: `<project>/.ken/project.json`**, carried by the
  existing `ProjectConfig.extra` flatten, so the symbol ships with
  the repo and round-trips through older Ken. Alternative
  (workspace-level symbol map) keeps it all in one file but makes a
  project's identity a property of one user's workspace.
- **OPEN-3 — Default cap values.** **Recommendation:
  `concurrency_cap: 1`** (matches the "one heavy build at a time"
  lesson) and **`bounce_cap: 3`** (enough for a genuine
  fix-retest-fix, short enough to catch a loop on the same day).
  Both are per-pipeline data, so changing them is a file edit.
- ~~**OPEN-4 — Do generated ideas land automatically or via an
  approval card?**~~ **RESOLVED (user ruling, 2026-08-03): auto-land
  into the Ideas lane, reviewed on a dedicated Ideas surface.** See
  D16 — the ruling also settled what an idea *is* (a few sentences,
  not a ticket brief), that generating them is optional per ticket,
  and that promotion is the human editorial act that turns one into
  real work.
- **OPEN-5 — Artifact retention.** **Recommendation: `expires` =
  created + 30 days, surfaced as a prune action in the tray, never
  auto-deleted** (D9). Ken deleting a human's review material on a
  timer is the wrong default.
- **OPEN-6 — Should run records pin a git revision?** A `commit:`
  field would let "tester passed" be checked against the code that
  actually shipped, directly attacking the drift risk above.
  **Recommendation: yes, as an optional field the agent reports** —
  read-only `git rev-parse`, no commit/push from Ken, so it does not
  touch the ken-families write-lane question at all.
- **OPEN-7 — Do pipeline tickets appear on the classic Kanban?**
  **Recommendation: yes** (D2's `maps_to` makes it free), with a
  board filter to hide them for users who want the classic board
  clean. Hiding them entirely would split one board into two
  disjoint worlds. Note that Blocked's `maps_to: doing` means blocked
  tickets show as `doing` on the classic board — arguably wrong, but
  the alternative (`maps_to: review`) is no better, and the classic
  board has no blocked concept to map onto.
- **OPEN-8 — Family boards and pipelines.** Out of scope here
  (there is no `TaskHome::Family`, and family drag-drop is already
  disabled pending the git-commit path). **Recommendation: state it
  as an explicit non-goal** and revisit only after ken-families'
  write path commits.
- **OPEN-9 — Does the ghost placeholder earn its complexity?** D5
  recommends rendering blocked tickets as a real Blocked column
  **plus** a return-lane badge **plus** a muted ghost in the lane
  they were blocked out of. The column and badge are settled; the
  ghost is the part that adds UI state for a readability gain.
  **Recommendation: ship column + badge first, add the ghost only if
  the board stops reading correctly without it.**
- **OPEN-10 — Should a blocker's completion be detected by polling
  or on transition?** **Recommendation: on transition** — when any
  ticket reaches a terminal lane, resolve the (small) set of tickets
  naming it in `blocked_by` and re-evaluate them, rather than
  scanning the whole board on a timer. A full re-evaluation still
  runs on workspace open so nothing is missed across restarts.

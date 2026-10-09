# ken-pipeline Specification

## ADDED Requirements

### Requirement: Lanes are defined per board in a definition file

A pipeline SHALL be defined by one markdown + frontmatter file at
`.ken-workspace/pipelines/<pipeline-id>.md`, read and written through
the ken-tasks patch core. It SHALL declare `id`, `name`, `auto`,
`concurrency_cap`, `bounce_cap`, and an ordered `lanes` list. Each
lane SHALL declare `id`, `name`, `maps_to` (one of
`backlog|todo|doing|review|done`), and MAY declare `agent`, `model`,
`kickoff` (`manual|confirm|auto`), `on_pass`, `on_fail`,
`writes_code`, `human`, `terminal`, `generative`, and `runner`. Lane
order in the file SHALL be column order in the board. The file body
SHALL be the per-lane brief material given to lane agents at kickoff.
Unknown keys and the body SHALL survive every programmatic rewrite
byte-for-byte.

#### Scenario: reordering lanes is a text edit
- **WHEN** a user swaps two lane entries in the definition file
- **THEN** the pipeline board's columns appear in the new order with
  no ticket file changed

#### Scenario: a definition supports twelve lanes
- **WHEN** the scaffolded `default.md` defines ideas, backlog, todo,
  investigation, refinement, programmer, tester, architect, qa,
  signoff and documentation
- **THEN** all lanes render as columns and no ken-core enum lists
  them

### Requirement: Lane vocabulary is board-scoped and projects onto classic statuses

A ticket without a `pipeline` frontmatter key SHALL be validated
exactly as before this change, against
`backlog|todo|doing|review|done`. A ticket with `pipeline: <id>`
SHALL have its `status` validated against that pipeline's lane ids,
SHALL expose the matched lane id as `lane`, and SHALL derive its
`TaskStatus` from the lane's `maps_to`. `status_raw` SHALL continue
to hold exactly what the file said. A `status` matching no lane SHALL
surface as `UnknownLane` in the needs-attention tray, and a
`pipeline` id with no definition file SHALL surface as
`UnknownPipeline`; neither SHALL cause a crash and neither file SHALL
be rewritten. `TaskFilter` SHALL gain optional `lane` and `pipeline`
fields without changing the meaning of any existing field. No
existing task file SHALL be rewritten by this change.

#### Scenario: pipeline ticket still lands on the classic board
- **WHEN** a ticket sits in the `architect` lane whose `maps_to` is
  `review`
- **THEN** the classic Kanban shows it in the `review` column and
  `task_list({status: "review"})` returns it

#### Scenario: unknown lane is surfaced, not destroyed
- **WHEN** a ticket names a pipeline whose definition has no lane
  matching the ticket's `status`
- **THEN** the ticket appears in the needs-attention tray with an
  `UnknownLane` reason and its file is unchanged on disk

#### Scenario: mixed board is a supported steady state
- **WHEN** the board holds both pipeline tickets and tickets with no
  `pipeline` key
- **THEN** both render, both filter correctly, and neither is
  migrated or rewritten

### Requirement: Kickoff is gated per lane, capped, and scope-bound

Each lane SHALL declare a `kickoff` mode. `manual` lanes SHALL never
start a run by themselves. `confirm` lanes SHALL require an explicit
confirmation that displays lane, agent, model, the ticket's `scope`,
and its `verify` command before any run starts. `auto` lanes SHALL
start on lane entry only when the pipeline's `auto` master switch is
true. A workspace-wide `concurrency_cap` SHALL bound simultaneous
running runs; runs beyond the cap SHALL be recorded as `queued` and
SHALL NOT start. A ticket lacking `scope` or `verify` SHALL NOT
auto-run under any lane setting: its gate SHALL be downgraded to
`confirm` and the reason SHALL be reported.

#### Scenario: a mis-drag cannot start a code-writing agent
- **WHEN** a card is dragged into the `programmer` lane and that
  lane's `kickoff` is `confirm`
- **THEN** no run starts until the confirmation dialog is accepted

#### Scenario: the cap queues rather than parallelises
- **WHEN** `concurrency_cap` is 1 and a second run is kicked off
  while one is running
- **THEN** the second run record exists with `outcome: queued` and no
  second agent is started

#### Scenario: an unbounded ticket cannot auto-run
- **WHEN** a ticket with no `verify` value enters an `auto` lane
- **THEN** the run is not started, the ticket is listed as needing a
  boundary, and the gate is reported as downgraded to `confirm`

### Requirement: Loop-backs are counted and hard-capped

A transition to a lane earlier in the definition order SHALL be
treated as a bounce: it SHALL increment the ticket's `bounces`
counter and append a line to the ticket's `## Log`. When a bounce
would take `bounces` past the pipeline's `bounce_cap`, the transition
SHALL be refused and the ticket SHALL instead be **blocked** using
the blocked mechanism below, with `block_reason` naming the exceeded
retry cap, `return_lane` set to the lane it was bouncing to, and an
escalation to the human. There SHALL NOT be a second, parallel
"halted" state: retry-cap escalation and dependency blocking SHALL
use the same fields, lane, filters, and digest group. Unblocking a
retry-capped ticket SHALL reset `bounces` to zero.

#### Scenario: tester/programmer ping-pong terminates
- **WHEN** a ticket bounces from `tester` back to `programmer` more
  times than `bounce_cap` allows
- **THEN** the ticket is blocked with a retry-cap reason and
  `return_lane: programmer`, rather than entering `programmer` again

#### Scenario: stuck work has one home
- **WHEN** the board holds one ticket blocked on a dependency and one
  blocked by the retry cap
- **THEN** both appear in the same Blocked lane, match the same
  `blocked` filter, and appear in the same digest group

### Requirement: Blocked lane with a recorded return lane

A pipeline definition MAY mark exactly one lane `blocked: true`. A
ticket moved into that lane SHALL record `return_lane` — the lane it
occupied immediately before the move and the lane it will resume in.
A ticket in the blocked lane without a `return_lane` SHALL surface in
the needs-attention tray as `UnknownReturnLane` and SHALL NOT be
rewritten. A ticket MAY carry `blocked_by` (a list of ticket ULIDs)
and `block_reason` (free text) simultaneously; both SHALL be
displayed, and clearing one SHALL NOT unblock the ticket while the
other still applies. `blocked_by` SHALL hold ticket ULIDs and SHALL
NOT hold file paths, so a dependency SHALL remain valid across task
homes, projects, renames, and archiving. A `blocked_by` id matching
no ticket SHALL surface as `UnknownBlocker` in the tray, never crash,
and never be rewritten. `blocked_at` SHALL record when the block was
set so stuck work can be aged.

#### Scenario: the pipeline remembers where the work belonged
- **WHEN** a ticket in the `programmer` lane is blocked
- **THEN** its file records `return_lane: programmer` and the card
  shows that it will return there

#### Scenario: dependency and reason coexist
- **WHEN** a ticket is blocked on ticket X and on "waiting for the
  upstream 2.0 release"
- **THEN** both are shown, and completing X alone does not unblock it

#### Scenario: a cross-project dependency survives a move
- **WHEN** the blocking ticket is renamed and moved to another task
  home
- **THEN** the `blocked_by` ULID still resolves to it

### Requirement: Blocks are cycle-checked at write time

Setting or extending `blocked_by` SHALL walk the existing block graph
and SHALL refuse any edge that would close a dependency cycle, of any
length, reporting the cycle path. The refusal SHALL happen at the
moment the block is set, not on later discovery, and the ticket file
SHALL be left unchanged.

#### Scenario: a direct cycle is refused
- **WHEN** A is blocked by B and the user tries to block B by A
- **THEN** the write is refused with the cycle path and neither file
  changes

#### Scenario: a long chain is refused
- **WHEN** A blocks B blocks C and the user tries to block A by C
- **THEN** the write is refused with the full chain reported

### Requirement: No lane agent ever picks up a blocked ticket

A blocked ticket SHALL be refused admission by every path that could
start work on it — UI kickoff, MCP `pipeline_claim`, and any future
auto-transition — with the refusal applied **before** the lane's gate
mode, the concurrency cap, or any other condition is evaluated. The
refusal SHALL be implemented in the single shared admission function
so no caller can route around it.

#### Scenario: an external agent cannot claim a blocked ticket
- **WHEN** an agent calls `pipeline_claim` on a blocked ticket
- **THEN** the call is refused and no run record is created

#### Scenario: blocked beats an auto lane
- **WHEN** a blocked ticket sits in a lane whose `kickoff` is `auto`
  and the pipeline's `auto` switch is true
- **THEN** no run is started and no run record is created

### Requirement: Unblocking returns through the gate, never into a run

When every ticket named in `blocked_by` has reached a terminal lane
and `block_reason` is cleared, the ticket SHALL return to its
`return_lane`. That re-entry SHALL pass through the return lane's
normal confirmation gate; a ticket re-entering by unblock SHALL be
treated as `confirm` even when the lane's `kickoff` is `auto`, so
unblocking SHALL NEVER start an agent by itself. Newly unblocked
tickets SHALL be reported as their own group in the daily update.
Blocker completion SHALL be re-evaluated when a ticket reaches a
terminal lane and again on workspace open, so nothing is missed
across restarts.

#### Scenario: an overnight dependency does not write code overnight
- **WHEN** a blocking ticket completes while the user is away
- **THEN** the dependent ticket sits in its return lane awaiting
  confirmation and no run was started

#### Scenario: the daily update leads with what freed up
- **WHEN** two tickets unblocked since the last digest
- **THEN** the digest reports them as "unblocked overnight" with
  their return lanes

### Requirement: Sign-off is a human lane with a comment-spawned child

A lane marked `human: true` SHALL have no agent and no kickoff, and
SHALL present a review action. Accepting SHALL advance the ticket
along `on_pass`. Accepting with comments SHALL create a new ticket in
the `todo` lane carrying the comment as its body, `parent` set to the
reviewed ticket's id, the same `pipeline` and project association,
and no inherited `scope` or `verify`; the comment SHALL also be
appended to the parent's `## Log`, and the parent SHALL advance along
`on_pass` in the same action. Rejecting SHALL follow `on_fail` and
SHALL count as a bounce.

#### Scenario: comments do not block the parent
- **WHEN** the user accepts a ticket with a comment
- **THEN** a child ticket exists in `todo` with `parent` set, and the
  parent has advanced to `documentation`

#### Scenario: the child earns its own boundary
- **WHEN** a child ticket is created from a sign-off comment
- **THEN** it has no `scope` and no `verify`, and therefore cannot
  auto-run until one is supplied

### Requirement: QA lane separates durable test plans from throwaway artifacts

The QA lane SHALL support two outputs. Durable long-term or
end-to-end test plans SHALL be written into the member repository
inside the ticket's `scope`. Throwaway review artifacts (walkthrough,
screenshots, recorded demo) SHALL be written only under
`.ken-workspace/artifacts/<ticket-id>/`, SHALL NOT be written into
any member repository, and SHALL be accompanied by a `manifest.md`
carrying `durable: false`, `ticket`, `created`, `expires`, and the
artifact list. Expired artifact folders SHALL be surfaced with a
prune action and SHALL NOT be deleted automatically. Recording
tooling SHALL be selected from the ticket's `target` field (`web` ⇒
Playwright trace/video, `tauri` ⇒ `tauri-driver` over WebDriver,
`none` ⇒ written walkthrough); an unavailable driver SHALL degrade to
a written walkthrough with a note in the run record, never fail the
lane.

#### Scenario: throwaway output cannot reach the real suite
- **WHEN** the QA lane produces a demo recording
- **THEN** the file exists under `.ken-workspace/artifacts/` with
  `durable: false` and no file was created under any project's test
  directories

#### Scenario: Ken's own UI is not driven by Playwright
- **WHEN** a ticket's `target` is `tauri`
- **THEN** the recipe selected is `tauri-driver` over WebDriver, not
  Playwright

### Requirement: Documentation lane closes the loop with deduped, cited ideas

The documentation lane SHALL be terminal and generative: after
updating documentation it SHALL inspect the finished ticket and
propose new ideas. Every proposed idea SHALL carry `origin:
generated` and a `spawned_by` citation naming the ticket that
produced it; a proposal without a citation SHALL be refused. Each
proposal SHALL be deduped against existing tickets before landing —
using semantic search and the federated knowledge graph when those
flags are on, and FTS plus normalized title matching when they are
not — with the scope defaulting to the ticket's project plus any
linked projects. A proposal above the similarity threshold SHALL NOT
create a ticket; instead a near-duplicate note SHALL be appended to
the matched ticket's `## Log`. Ideas SHALL land in a lane with no
agent, so landing an idea SHALL never start a run.

#### Scenario: the loop closes
- **WHEN** a ticket completes the documentation lane
- **THEN** documentation is updated and any surviving idea exists as
  a ticket in the ideas lane citing the completed ticket

#### Scenario: a duplicate idea does not become a ticket
- **WHEN** a proposed idea matches an existing ticket above the
  threshold
- **THEN** no new ticket is created and the existing ticket's `## Log`
  records the near-duplicate

#### Scenario: dedupe degrades without the index
- **WHEN** `semanticIndex` and `federatedKg` are off
- **THEN** dedupe still runs via FTS and title matching, and ideas
  still require a citation

### Requirement: Runs are an append-only ledger and queue state is derived

Each run SHALL be one markdown + frontmatter file at
`.ken-workspace/runs/YYYY-MM/<ulid>.md` recording `id`, `ticket`,
`pipeline`, `lane`, `agent`, `model`, `scope`, `verify`, `started`,
`ended`, `outcome` (`queued|running|pass|fail|blocked|cancelled`) and
`artifacts`, with the agent's report as the body. Running, queued,
blocked, and waiting-on-human state SHALL be derived from the ledger
and the tickets and SHALL be fully rebuildable from the folder. A
`running` record with no live run after a restart SHALL be reported
as stale and SHALL NOT be recorded as passed.

#### Scenario: restart rebuilds the queue
- **WHEN** Ken restarts with queued and running records on disk
- **THEN** the run tray shows the same queue without any separate
  state store

#### Scenario: an interrupted run is not silently passed
- **WHEN** Ken is killed during a run and restarted
- **THEN** that run is reported stale and its ticket has not advanced

### Requirement: MCP pipeline tools page by lane and never dump the backlog

`ken-mcp` SHALL expose `pipeline_list`, `pipeline_get`,
`pipeline_claim`, `pipeline_advance`, `pipeline_block`,
`pipeline_runs`, and `pipeline_digest`, sharing one core with the UI
and chat tools. `pipeline_list` SHALL filter by lane, pipeline,
project, model, assignee, and block state — including `blocked`,
`blocked_by: <ticket-id>`, and `newly_unblocked` — SHALL return
compact rows (id, title, lane, project symbol, model, assignee,
block summary, updated) and never ticket bodies; and SHALL enforce a
default page size with a hard maximum and a cursor for continuation.
`pipeline_get` SHALL be the only tool returning a full ticket, and
SHALL include the ticket's blockers and return lane.
`pipeline_advance` SHALL resolve the target lane from the
definition's `on_pass`/`on_fail`, apply bounce accounting, and append
the report to the ticket's `## Log`. `pipeline_block` SHALL set or
clear blocks, recording `return_lane` and enforcing cycle detection.
`pipeline_digest` SHALL produce a grouped daily update leading with
items awaiting human review, then newly unblocked tickets, then
blocked tickets ordered oldest-first by `blocked_at`, then tickets
that moved, new ideas, and stale runs; when `kenMemory` is on it
SHALL also write the digest through `journal_append`. With
`kenPipeline` off all seven tools SHALL be absent.

#### Scenario: a huge backlog is never fed whole to a model
- **WHEN** an agent calls `pipeline_list` on a lane holding a
  thousand tickets with no limit argument
- **THEN** at most one default page of compact rows is returned with
  a cursor for the next page, and no ticket bodies are included

#### Scenario: the daily update leads with the human's queue
- **WHEN** `pipeline_digest` runs with three tickets in the sign-off
  lane, two newly unblocked, and one blocked by the retry cap
- **THEN** the first group is the three awaiting review, the second
  is the two unblocked, and the retry-capped ticket appears in the
  blocked group with its reason

#### Scenario: what is stuck and why is one call
- **WHEN** an agent calls `pipeline_list({blocked: true})`
- **THEN** it receives compact rows naming each ticket's blockers,
  block reason, return lane, and how long it has been blocked

#### Scenario: claiming respects the cap
- **WHEN** an agent claims a queued run while `concurrency_cap` is
  already reached
- **THEN** the claim is refused and the run stays queued

#### Scenario: a dependency is set by ticket id
- **WHEN** an agent calls `pipeline_block` naming a blocking ticket's
  ULID from another project
- **THEN** the block is recorded, `return_lane` is captured, and the
  dependent ticket leaves every agent's claimable queue

### Requirement: Project identity by symbol and explicit project links

Each project SHALL be able to declare a short display `symbol`, which
SHALL render at the top-left of every card carrying that project;
card colour SHALL convey lane, not project. A ticket SHALL be able to
span multiple projects via a `projects` list while `project` remains
the primary used by the existing project filter. `workspace.json`
SHALL be able to declare a `links` array of `{from, to, relation,
note?}` entries carried by its existing forward-compatible extra map;
`federated-kg` SHALL seed these as cross-project edges with
`manifest` provenance. Linked projects SHALL be included in idea
dedupe scope and SHALL be selectable as a board filter option.

#### Scenario: project is readable at a glance
- **WHEN** the combined board shows tickets from four projects
- **THEN** each card shows its project symbol top-left and lane is
  conveyed by colour

#### Scenario: linked projects share dedupe scope
- **WHEN** ShatteredRealms and ShatteredRealmsTools are linked and an
  idea is generated from a ShatteredRealms ticket
- **THEN** dedupe searches both projects before the idea lands

### Requirement: Flag-scoped activation

With `kenPipeline` off, Ken SHALL show no pipeline board, register no
pipeline tools on either surface, load no pipeline definitions, run
no runners or watchers for pipeline folders, and create no
`pipelines/`, `runs/`, or `artifacts/` folders — behaviour SHALL be
byte-identical to the shipped ken-tasks board. `kenPipeline` SHALL
require both `workspace` and `kenTasks`.

#### Scenario: flag off is inert
- **WHEN** `kenPipeline` is disabled and a workspace with existing
  task files is opened
- **THEN** the Tasks tab, MCP tool list, board behaviour, and
  filesystem are unchanged from pre-feature behaviour

#### Scenario: pipeline requires tasks
- **WHEN** `kenPipeline` is enabled while `kenTasks` is off
- **THEN** the pipeline surface stays inactive and the dependency is
  reported

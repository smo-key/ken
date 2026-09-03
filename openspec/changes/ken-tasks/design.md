# Design: ken-tasks

## Context

Ken already renders markdown, watches folders, and re-derives state
from files. The board is "just" another derived view: task files in,
Kanban out. The genuinely new pieces are the frontmatter contract,
the write-back path (drag-drop and tools rewrite files), and the
daily-board loop. Depends on `workspace`; integrates with
`ken-memory` (journal reporting, pseudo-member indexing) when that
flag is on, but functions without it.

## Goals / Non-Goals

- Goals: one task format usable by humans in any editor, Ken, and
  agent-desktop; board always reconstructable from files alone;
  claiming/completion over MCP with zero bespoke protocol; daily
  board that stays lightweight.
- Non-Goals: sprints, estimates, burndown, dependencies between
  tasks (add later as frontmatter keys if wanted — unknown keys
  already survive); multi-user sync/locking (single user + agents,
  last-writer-wins is fine); building an Obsidian importer (it's a
  migration *task*, not code); notifications.

## Decisions

### D1. One file per task, frontmatter is the state machine

Rejected: tasks as rows in the workspace DB (breaks
files-are-truth; invisible to git and editors) and one big
`tasks.md` (merge hell for concurrent agent writes; no per-task
addressability). One file per task means: agent A claiming task X
and agent B completing task Y touch different files; every task has
a `ken://` address; archive is `mv`.

Filename: `<ulid>-<title-slug>.md`. The `id` in frontmatter is
authoritative; the filename is for humans. All reads key on `id`,
so renaming a file (fixing a typo'd slug) changes nothing.

Rewrites go through one core function that parses, patches only the
named keys, and re-serializes preserving unknown keys, key order,
and the body byte-for-byte. Hand-added frontmatter and hand-edited
bodies survive every drag-drop. Spike S6 validates this round-trip
under watcher load.

### D2. Hybrid homes, one aggregated board (locked)

`.ken-workspace/tasks/` is the default; `<project>/.ken/tasks/` is
opt-in per repo (just create the folder — its existence is the
opt-in, mirroring `.kenignore`). The board scans both and treats
home as invisible plumbing; the `project` frontmatter key (not the
home) drives the project filter, so a workspace-home task can point
at a project and a per-repo task needs no `project` key (defaulted
from its home). Archive stays within the task's own home
(`tasks/archive/YYYY-MM/`) so per-repo history ships with the repo.

### D3. Board is derived state with a write-back edge

On workspace open (flag on), src-tauri scans task homes into an
in-memory board model and watches both folders; external edits
(agent-desktop, git pull, hand edits) flow in through the watcher
like any file change. UI mutations are commands that call the D1
rewrite core, then let the watcher event round-trip confirm — the
file is always written first; the board never holds state the files
don't. Drag-drop writes exactly `status` + `updated`.

### D4. MCP is the claim/complete protocol

No new protocol: the four tools are the lifecycle. `task_list`
filter matches the UI filters, so an agent can ask "unclaimed ai
tasks for ShatteredRealms" (`kind: ai`, `assignee: none`,
`project: ShatteredRealms`, `status: todo`). Claiming =
`task_update(id, { assignee, status: doing })`. `task_complete(id,
report)` sets `done`, appends the report to the task body under a
`## Log` heading with a timestamp, and (when `kenMemory` is on)
writes a one-line journal summary linking the task's `ken://`
address — the day-to-day record the daily board and distillation
feed on. Tool descriptions spell this flow out so agents follow it
unprompted. Chat tools wrap the same core (kg-routing D4 pattern).

### D5. Daily board is a filter plus two rituals

`board: daily` is just a frontmatter value — daily tasks are normal
task files, same homes, same tools, shown on their own view.
The rules that make it useful are behavioral:

- **Population**: on request ("plan my day") — and only then — Ken
  drafts daily candidates from `read_journal` output + recent
  activity (most recent ingest completions), each as an approval
  card; approve creates the file. Same propose/approve pattern as
  memory promotion; nothing autonomous.
- **Rollover**: on the first workspace open of a new day, unfinished
  daily tasks (status ≠ done, `updated` before today) surface a
  prompt with three per-task choices: roll forward (bump `updated`),
  promote to main (`board: main`), or archive. Repeated rollovers
  are a signal the task wasn't a daily-sized item.

Rejected: a separate daily-task format or folder — one format, one
lifecycle, one set of tools.

### D6. Tasks index search-only

Task homes are search-only tier: findable ("what was that ticket
about mob spawning?") but never knowledge-model-extracted — a
hundred tickets naming an entity shouldn't outweigh the codebase.
Workspace home gets this via the pseudo-member's built-in rules
(ken-memory D3); per-repo homes get a built-in `~.ken/tasks/` rule
in the kenignore defaults. With `kenMemory` off the workspace home
is simply unindexed — the board never depends on the index.

### D7. Overarching goals: files in `tasks/goals/`, tags on tasks

Goals give the board its long arc: a handful of named outcomes
("ship multi-project Ken") that day-to-day tasks tag under.
Rejected: goals as a special kind of task or as nested tasks — a
goal has no assignee and no claim lifecycle, so forcing it through
the task state machine buys nothing.

- A goal is a frontmatter file in `tasks/goals/` under the
  **workspace home only**: `<ulid>-<slug>.md` with `id`, `title`,
  `status` (`active|done|dropped`), `created`, `updated`, and a
  free-markdown description body. Same patch core, same tolerant
  parse as tasks.
- Tasks reference a goal via a `goal:` frontmatter key holding the
  goal's id — a plain reference, so per-repo tasks can tag
  workspace goals. An id matching no goal file surfaces in the
  needs-attention tray, never crashes, never gets rewritten.
- The board groups and filters by goal. Goal progress is derived —
  n done / m total among tasks tagging it — and never stored.
- Backlog is the intake column: capture defaults to `backlog`,
  grooming moves items to `todo`, and goal grouping is what keeps
  a large backlog navigable.
- Goal CRUD via UI and chat. MCP gets **no new tool**: `task_list`
  grows a `goal` filter, which is all an agent needs — agents work
  tasks, humans steer goals.

Non-goals: nested goals, dependencies between goals.

## Risks / Trade-offs

- **Concurrent writes to one task** — two agents claiming the same
  task race. Spike S6 measured last-writer-wins losing ~17% of an
  uncoordinated writer's updates under churn, so the patch core
  ships with an optimistic-concurrency guard in v1: mtime/hash
  precondition check immediately before write, retry on mismatch.
  The claim convention (check `assignee` empty before claiming)
  stays stated in tool descriptions.
- **Watcher feedback loop** — Ken's own writes come back as watcher
  events; the board model dedupes by content hash so self-writes
  are no-ops. S6 covers this.
- **Frontmatter drift from hand edits** — tolerant parse: an
  invalid `status` renders in a "needs attention" tray rather than
  crashing or being silently rewritten.
- **Daily board turning into nag-ware** — population is on-request
  only and rollover asks rather than auto-rolls.

## Migration

None in code. Obsidian/ticket-doc migration is executed as content
work: Ken drafts one conversion task per source doc into the board
itself; agent-desktop or the user works them off. Flag off ⇒ no
tab, no tools, no watchers, no folders.

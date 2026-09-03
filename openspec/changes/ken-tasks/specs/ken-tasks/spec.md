# ken-tasks Specification

## ADDED Requirements

### Requirement: Tasks are single-file markdown with frontmatter

Each task SHALL be one markdown file named `<ulid>-<slug>.md` with
frontmatter `id`, `title`, `status`
(`backlog|todo|doing|review|done`), `kind` (`human|ai`),
`assignee`, `project`, `tags`, optional `due`, `board`
(`main|daily`), `created`, `updated`. The `id` SHALL be
authoritative over the filename. Unknown frontmatter keys and the
body SHALL survive every programmatic rewrite byte-for-byte;
patches SHALL rewrite only the named keys plus `updated`.

#### Scenario: drag-drop touches two keys
- **WHEN** a card is dragged from `todo` to `doing`
- **THEN** the file diff shows only the `status` and `updated`
  lines changed

#### Scenario: renamed file keeps its identity
- **WHEN** the user renames a task file to fix its slug
- **THEN** the board still shows one task under the same `id`

#### Scenario: invalid hand edit is surfaced, not destroyed
- **WHEN** a hand-edited file contains `status: blocked`
- **THEN** the task appears in a needs-attention tray and the file
  is not rewritten

### Requirement: Hybrid homes with one aggregated board

Tasks SHALL live in `.ken-workspace/tasks/` by default and
optionally in `<project>/.ken/tasks/` (folder existence is the
opt-in). The board SHALL aggregate all homes; the project filter
SHALL key on the `project` frontmatter field, defaulting to the
owning project for per-repo tasks. Archiving SHALL move the file to
`tasks/archive/YYYY-MM/` inside its own home.

#### Scenario: per-repo task ships with its repo
- **WHEN** a task in `ShatteredRealms/.ken/tasks/` is archived
- **THEN** the file moves to
  `ShatteredRealms/.ken/tasks/archive/2026-07/` and remains in that
  repo's git history

#### Scenario: workspace task filters by project
- **WHEN** a workspace-home task has `project: ItemSearch` and the
  board filters to ItemSearch
- **THEN** the task is shown

### Requirement: Board is derived and live

The board SHALL be reconstructable from task files alone. Task
homes SHALL be watched; external edits (agents, git, hand edits)
SHALL appear on the board without user action, and Ken's own writes
SHALL not cause visible churn. UI mutations SHALL write the file
first — the board SHALL never hold state the files do not.

#### Scenario: agent edit appears live
- **WHEN** agent-desktop rewrites a task file while the Tasks tab
  is open
- **THEN** the card updates without a refresh

### Requirement: MCP task lifecycle tools

`ken-mcp` SHALL expose `task_create`, `task_list(filter)`,
`task_update(id, patch)`, and `task_complete(id, report)` sharing
one core with the UI and chat tools. `task_list` SHALL filter by
status, project, tag, assignee, kind, and goal. `task_complete` SHALL set
`done`, append the report under `## Log` with a timestamp, and —
when `kenMemory` is enabled — write a one-line journal summary
citing the task's `ken://` address. Tool descriptions SHALL state
the claim convention (verify unclaimed, then set `assignee` +
`doing`). With `kenTasks` off the tools SHALL be absent.

#### Scenario: agent claims and completes
- **WHEN** an agent lists `kind: ai` unclaimed tasks, claims one
  via `task_update`, and later calls `task_complete` with findings
- **THEN** the card moves todo → doing → done, the findings are in
  the task's `## Log`, and the journal holds a summary line

### Requirement: Daily board with proposals and rollover

Tasks with `board: daily` SHALL render on a separate daily view
using the same files, homes, and tools. Daily candidates SHALL be
drafted only on user request, from recent journal content and
ingest activity, each requiring approval before a file is created.
On the first open of a new day, unfinished daily tasks SHALL
surface a rollover prompt offering roll forward, promote to main,
or archive — never auto-resolving.

#### Scenario: nothing populates itself
- **WHEN** a new day starts and the user does not ask Ken to plan
  the day
- **THEN** no new daily task files exist

#### Scenario: rollover asks, per task
- **WHEN** two daily tasks are unfinished at the next day's first
  open
- **THEN** each gets an independent roll / promote / archive choice

### Requirement: Overarching goals

Goals SHALL be frontmatter files in `tasks/goals/` under the
workspace home, named `<ulid>-<slug>.md`, with `id`, `title`,
`status` (`active|done|dropped`), `created`, `updated`, and a
markdown description body, patched via the same rewrite core.
Tasks SHALL reference a goal via a `goal:` frontmatter key holding
the goal id; per-repo tasks MAY reference workspace goals. Task
creation without an explicit status SHALL default to `backlog`,
the board's intake column. The board SHALL support grouping and
filtering by goal, deriving goal progress as done/total counts —
never storing it. A `goal:` id matching no goal file SHALL surface
in the needs-attention tray, never crash, never be rewritten. No
new MCP tool SHALL be added for goals; the `task_list` `goal`
filter is the agent surface.

#### Scenario: goal progress is derived
- **WHEN** three of five tasks tagging a goal reach `done`
- **THEN** the goal shows 3/5 and no progress value exists in any
  file

#### Scenario: per-repo task tags a workspace goal
- **WHEN** a task in `<project>/.ken/tasks/` sets `goal:` to a
  workspace goal id and the board groups by goal
- **THEN** the task appears under that goal's group

#### Scenario: unknown goal id is surfaced
- **WHEN** a task references a goal id with no matching goal file
- **THEN** the task renders in the needs-attention tray and its
  file is not rewritten

### Requirement: Tasks index search-only

All task homes SHALL be indexed at the search-only tier: chunked,
FTS'd, and embedded, but never knowledge-model-extracted. The board
SHALL function fully even when no index exists.

#### Scenario: tickets never mint entities
- **WHEN** twenty tasks mention the same system name
- **THEN** no KG entity originates from task files

### Requirement: Flag-scoped activation

With `kenTasks` off, Ken SHALL show no Tasks tab, register no task
tools on either surface, create no folders, and run no watchers —
byte-identical to pre-feature behavior. `kenTasks` SHALL require
`workspace`.

#### Scenario: flag off is inert
- **WHEN** `kenTasks` is disabled and a workspace is opened
- **THEN** the sidebar, MCP tool list, and filesystem are unchanged
  from pre-feature behavior

# Proposal: ken-tasks

## Why

Task tracking today is scattered: "filling up the codebases each
with these ticket docs has been not the best and obsidian has
terrible ticket support." The user wants "an overall task board into
Ken that Ken can update as well and keep track of, where we can
archive the completed ones" — bubbled into "a new tasks tab on the
left that can be filtered by project or overall," with "kanban
styling, tag support, assignees, human vs AI tasks."

Tasks are also the Ken ↔ agent-desktop handoff. Ken is the knowledge
layer — it documents features and "can make tasks for deeper code
diving" (e.g. decompiled Hytale logic); agent-desktop does the
coding. The board is the contract: Ken writes a task, agent-desktop
claims it over MCP, completes it, and reports back (findings go to
the journal via `ken-memory`'s `journal_append`).

Same core principle as everything else: **tasks are markdown files;
the board is a derived view.** Human-editable, diffable, and
migratable — the existing Obsidian ticket docs can be converted into
this format by hand or by an agent task, no importer code needed.

## What Changes

- **Task files** — markdown + frontmatter, one file per task:
  - Frontmatter: `id` (ulid), `title`, `status`
    (`backlog|todo|doing|review|done`), `kind` (`human|ai`),
    `assignee`, `project`, `tags`, `due?`, `goal?`, `board`
    (`main|daily`), `created`, `updated`. Unknown keys preserved.
  - Body: free markdown — description, acceptance criteria, and an
    appended work log (`task_complete` reports land here).
- **Task homes — hybrid (locked)**:
  - `.ken-workspace/tasks/` — the default home for all tasks.
  - `<project>/.ken/tasks/` — optional per-repo home for tasks that
    should live and ship with that codebase.
  - The board aggregates both; `archive` moves a file to
    `tasks/archive/YYYY-MM/` within its own home.
- **Overarching goals** — frontmatter files in `tasks/goals/` under
  the workspace home, `<ulid>-<slug>.md` (`id`, `title`, `status`,
  `created`, `updated`; description in the body). Tasks tag a goal
  via the `goal:` key — a plain id reference, so per-repo tasks can
  tag workspace goals. The board groups and filters by goal; goal
  progress is derived (n done / m total), never stored. Goal CRUD
  via UI and chat; `task_list` gains a `goal` filter — no new MCP
  tool.
- **Tasks tab** (left sidebar, workspace mode): Kanban columns =
  statuses, with `backlog` as the leftmost intake column — captures
  land there and are groomed onto `todo`; goal grouping keeps a big
  backlog navigable; filters for project, tag, assignee, and kind;
  drag-drop
  between columns rewrites only `status` + `updated` in the file
  (all other content untouched). A file watcher on both task homes
  keeps the board live when files change externally.
- **Daily board**: a second board view over `board: daily` tasks —
  "quick things or discussions to have, tasks to do that can be
  completed within that day. Less deep work, more day-to-day
  management and reminders." Ken proposes daily items from the
  journal and recent activity (approval card, same pattern as memory
  promotion). At end of day, unfinished daily tasks get a rollover
  prompt: roll forward, promote to the main board, or archive.
- **MCP tools** (the agent handoff surface):
  - `task_create(title, body, fields?)`
  - `task_list(filter?)` — status/project/tag/assignee/kind/goal
  - `task_update(id, patch)` — claiming = set `assignee` + `doing`
  - `task_complete(id, report)` — status `done`, report appended to
    the task log and summarized as a journal line
- **Ken chat tools**: same four, thin wrappers over the same core.
- **Indexing**: task homes are ingested **search-only** (findable
  via FTS/semantic search; never knowledge-model-extracted — ticket
  churn must not mint KG entities). Workspace-home tasks ride the
  `ken-memory` pseudo-member; per-repo tasks are inside their member
  already.
- **Obsidian migration**: a content task, not app code — Ken drafts
  per-doc conversion tasks; agent-desktop (or the user) executes
  them into task files / memory files / project docs.
- **Flag**: `kenTasks` (workspace-level, requires `workspace`). Off
  ⇒ no tab, no tools, no folders — byte-identical to today.

## Capabilities

### New Capabilities
- `ken-tasks`: task file format, hybrid homes, Kanban + daily
  boards, overarching goals with board grouping, drag-drop status
  flow, archive, chat/MCP task tools, daily proposals + rollover.

### Modified Capabilities
- `mcp`: four new task tools.
- `chat`: task tools available; daily-proposal approval cards.
- `ken-memory`: `task_complete` writes a journal summary line;
  workspace task home is part of the pseudo-member (search-only).

## Impact

- `crates/ken-core`: new `tasks.rs` — frontmatter model,
  parse/serialize preserving unknown keys, status transition +
  archive path logic, filter matching, goal file model + derived
  goal progress, daily rollover logic; all pure and table-testable.
- `src-tauri`: task home watchers; board state commands + events;
  task tool commands; daily proposal + rollover prompts; flag gate.
- `crates/ken-mcp`: the four task tool registrations.
- Frontend: Tasks tab (Kanban board, daily view, filters, goal
  grouping + progress, drag-drop); task file rendering;
  approval/rollover cards.
- Tests: frontmatter round-trip with unknown keys + hand edits;
  status transition rewrite touches only `status`/`updated`; filter
  logic; archive pathing (YYYY-MM, same home); rollover cases;
  flag-off byte-identical.

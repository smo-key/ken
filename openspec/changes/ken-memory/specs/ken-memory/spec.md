# ken-memory Specification

## ADDED Requirements

### Requirement: Memories are addressable markdown files

Every memory SHALL be a markdown file in `.ken-workspace/memory/`
(workspace scope) or `<project>/.ken/memory/` (project scope) with
tolerant frontmatter (`description`, `projects`, `created`,
`updated`; unknown keys preserved on rewrite). A file with no
frontmatter SHALL still be a valid memory whose description falls
back to its first line. Every memory SHALL be addressable
(`ken://workspace/memory/<file>` or
`ken://<project-id>/.ken/memory/<file>`) and openable like any
document.

#### Scenario: hand-created file is a memory
- **WHEN** the user drops a plain markdown file with no frontmatter
  into `.ken-workspace/memory/`
- **THEN** it is ingested, injectable, and its first line serves as
  its description

#### Scenario: rewrite preserves unknown keys
- **WHEN** `memory_write` replaces the body of a memory whose
  frontmatter contains a key Ken does not model
- **THEN** the rewritten file retains that key unchanged

### Requirement: Two-tier lifecycle with archive roll

Long-term memory folders SHALL be fully indexed (chunks, embeddings,
knowledge model). The journal
(`.ken-workspace/journal/YYYY-MM-DD.md`) and its archive SHALL be
indexed search-only: chunked, FTS'd, embedded, never
knowledge-model-extracted. On workspace open, journal files older
than 30 days SHALL move to `journal/archive/` keeping their
filenames; the roll SHALL be idempotent.

#### Scenario: journal never mints entities
- **WHEN** a journal file mentions a new concept repeatedly
- **THEN** no KG entity is created from the journal; the concept
  reaches the KG only via an approved long-term memory or a source
  document

#### Scenario: archived journal stays findable
- **WHEN** a 45-day-old journal entry matches a search query
- **THEN** the hit resolves to the file under `journal/archive/`
  with a working `ken://workspace/...` address

#### Scenario: roll is idempotent
- **WHEN** the archive roll runs twice in a row
- **THEN** the second run moves nothing and reports no changes

### Requirement: Workspace pseudo-member

`.ken-workspace/` SHALL be ingested by the existing per-project
engine under a reserved id derived deterministically from the
workspace id, addressed as `ken://workspace/<rel-path>`. It SHALL
participate in ⌘K fan-out and routing Broadcast, and SHALL be
excluded from the member list, the profiler, and federation.
Built-in tier rules SHALL apply: `memory/` full; `journal/`,
`journal/archive/`, `tasks/` search-only; `workspace.json` and
`kg.sqlite` ignored.

#### Scenario: memory answers a broadcast query
- **WHEN** an all-projects search matches a workspace memory
- **THEN** the memory appears in merged results as a
  `ken://workspace/...` hit alongside member hits

#### Scenario: pseudo-member is not a member
- **WHEN** the member list, profiler run, or federation pass
  enumerates projects
- **THEN** the workspace pseudo-member is absent from all three

### Requirement: Budgeted context injection

Workspace chat SHALL inject a `## Memories` block containing all
workspace-scope memories plus the focused project's memories,
ordered by `updated` descending, within a 4,000-character budget.
Only whole files SHALL be injected; a file that would exceed the
remaining budget SHALL contribute its description line instead. The
journal SHALL NOT be injected; a `read_journal(days_back?)` tool
SHALL return recent journal content on demand.

#### Scenario: over-budget memory degrades to its description
- **WHEN** the accumulated block is near the budget and the next
  memory's full text would exceed it
- **THEN** that memory contributes only its description line and
  injection continues

#### Scenario: journal costs context only when asked
- **WHEN** a chat session starts
- **THEN** no journal text is in context until the model calls
  `read_journal`

### Requirement: One write core, two tool surfaces

`memory_write(scope, slug, content)` and
`journal_append(text, project?, tags?)` SHALL be implemented once in
ken-core and exposed as both Ken chat tools and `ken-mcp` tools with
identical semantics. Creating a memory whose slug already exists
SHALL error, never silently overwrite. `journal_append` SHALL append
a timestamped `## HH:MM` entry to today's file, creating it if
absent. The file watcher SHALL pick up all writes and reindex.

#### Scenario: agent-desktop reports back
- **WHEN** an MCP client calls `journal_append` with a task report
  citing `ken://` addresses
- **THEN** today's journal file gains the entry and it becomes
  searchable after reindex

#### Scenario: slug collision refused
- **WHEN** `memory_write` is called in create mode with an existing
  slug
- **THEN** the call errors and the existing file is untouched

### Requirement: Approval-gated promotion

Distillation SHALL run only on demand or as an offer when an archive
roll occurs. The prompt SHALL include the journal window and
existing memory descriptions as a dedupe guard; parsing SHALL be
tolerant (garbage output ⇒ zero candidates) and capped at 5
candidates per run. Each candidate SHALL render as an approval card;
approval writes via the memory write core, dismissal records the
slug so it is not re-proposed. Nothing SHALL write to `memory/`
without explicit user approval.

#### Scenario: garbage model output proposes nothing
- **WHEN** the distillation model returns unparseable text
- **THEN** zero candidates are shown and no files are written

#### Scenario: dismissed candidate stays dismissed
- **WHEN** the user dismisses a candidate and distillation runs
  again over the same window
- **THEN** that slug is not proposed again

### Requirement: Flag-scoped activation

With `kenMemory` off, Ken SHALL create no memory/journal folders,
register no memory tools on either surface, spin up no workspace
pseudo-member, and inject nothing — byte-identical to pre-feature
behavior. `kenMemory` SHALL require `workspace`; without
`semanticIndex`, memory search degrades to FTS-only but tools and
injection still function.

#### Scenario: flag off is inert
- **WHEN** `kenMemory` is disabled and a workspace is opened
- **THEN** no `.ken-workspace/memory/` or `journal/` folders are
  created and the MCP tool list contains no memory tools

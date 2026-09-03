# Proposal: ken-memory

## Why

Ken currently knows only what the indexed folders say. The user wants
Ken to also carry **its own working memory**: "tools that just edit
its own memories into the knowledge base... that aren't tied to
documents, so it really builds up its own ways of working... update
based on meetings, based on conversations... recent things that
happen to really help you on your day to day." Two distinct needs
came out of discussion:

1. **Long-term memory** — how the user likes to work, overarching
   conventions and decisions. Slowly changing, curated, always
   relevant.
2. **Short-term memory** — what happened recently (meetings, agent
   reports, day-to-day churn). High volume, decaying relevance, but
   worth keeping findable after it ages out.

Both tiers must obey Ken's core principle: **files are the source of
truth, indexes are derived.** Memories are markdown files the user
can read, edit, and diff — Ken ingests them exactly like any other
document, so no new index machinery is required and every memory is
`ken://`-addressable and citeable.

## What Changes

- **Long-term memory folders** (curated, one concern per file):
  - `.ken-workspace/memory/` — workspace-wide: ways of working,
    cross-project conventions, standing decisions.
  - `<project>/.ken/memory/` — project-scoped: best practices and
    conventions for that codebase ("storing all our mobs in one
    location, storing all our items in one location... best
    practices and ways of working for each").
  - Format: markdown + frontmatter (`description`, `projects`,
    `created`, `updated`) + `[[links]]` to other memories and
    `ken://` addresses.
- **Short-term journal**: `.ken-workspace/journal/YYYY-MM-DD.md` —
  append-heavy daily log. Meeting outcomes, decisions in flight,
  agent-desktop completion reports, anything "recent things that
  happen." After 30 days a journal file rolls to
  `journal/archive/`, which is indexed **search-only** (see
  `kenignore`): still findable via FTS/semantic search, but excluded
  from knowledge-model extraction so old noise never pollutes the KG.
- **Workspace pseudo-member**: `.ken-workspace/` contents (memory,
  journal, tasks) are ingested by the existing per-project engine
  into their own derived DB under a reserved id, addressed as
  `ken://workspace/<rel-path>`. It participates in search fan-out
  and routing broadcast but is never listed as a normal member and
  is skipped by the profiler and federation.
- **Context injection**: workspace memories plus the focused
  project's memories are injected into chat context (bounded budget,
  most recently updated first). Today's and yesterday's journal are
  reachable via a chat tool, not injected wholesale.
- **Write tools — both surfaces (locked)**:
  - Ken chat tools: `memory_write(scope, slug, content)`,
    `journal_append(text, project?, tags?)`.
  - `ken-mcp` tools with the same names/semantics, so agent-desktop
    can report back ("completes it, writes its findings back to the
    journal"). Both delegate to one core implementation; the file
    watcher picks up writes and reindexes.
- **Promotion — Ken proposes, you approve (locked)**: a distillation
  pass (on demand, and suggested when journal files archive) drafts
  candidate long-term memories from recurring journal themes. Each
  candidate is shown as an approval card; nothing is ever written to
  `memory/` without explicit approval.
- **Flag**: `kenMemory` (workspace-level, requires `workspace`).
  Off ⇒ no folders created, no tools, no injection — byte-identical
  to today. Without `semanticIndex`, memories degrade to FTS-only
  search but injection and tools still work.

## Capabilities

### New Capabilities
- `ken-memory`: two-tier memory files, workspace pseudo-member
  ingestion, journal + archive lifecycle, chat/MCP write tools,
  context injection, approval-gated promotion.

### Modified Capabilities
- `chat`: system context gains injected memories; new memory/journal
  tools.
- `mcp`: two new write tools.
- `search` / `kg-routing`: workspace pseudo-member joins fan-out and
  broadcast.

## Impact

- `crates/ken-core`: new `memory.rs` (file conventions, frontmatter
  parse/serialize, injection budgeting, distillation prompt/parse —
  all pure); reserved workspace project id; archive roll job.
- `src-tauri`: pseudo-member engine wiring; memory tool commands;
  promotion approval command + events.
- `crates/ken-mcp`: `memory_write`, `journal_append` registrations
  delegating to core.
- Frontend: memory tool call rendering in chat; promotion approval
  cards; memory files open like any document.
- Tests: frontmatter round-trip; injection budget ordering;
  distillation parse fixtures (garbage ⇒ no candidates); archive
  roll idempotence; flag-off byte-identical.

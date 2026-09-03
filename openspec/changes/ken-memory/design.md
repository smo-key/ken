# Design: ken-memory

## Context

Everything Ken indexes today is a folder of files feeding a derived
SQLite DB (`engine.rs`: one ingest engine per project, debounced
watcher, rebuildable). Memory reuses that machinery wholesale: the
only genuinely new things are file conventions, a reserved
pseudo-member, write tools, and the promotion flow. Depends on
`workspace` (the `.ken-workspace/` home exists) and layers onto
`semantic-index` and `kenignore` when present.

## Goals / Non-Goals

- Goals: memories as plain markdown (hand-editable, diffable,
  addressable); zero new index machinery; long-term always in
  context, short-term findable forever; agent-desktop can write back;
  user approves anything Ken promotes.
- Non-Goals: automatic memory extraction from every chat turn (only
  explicit tool calls and the approval-gated distillation write);
  per-project journals (one workspace journal, entries tag
  projects); memory-specific search UI (memories are documents —
  ⌘K/chat/routing already cover them); vector-DB or KG storage of
  memories as first-class rows (files only).

## Decisions

### D1. Memories are files; the index stays derived

Rejected: a `memories` table written by tools. It would fork the
source-of-truth story — Ken's model is "the folder is truth,
`rebuild()` regenerates everything." Markdown files mean the user
can read, edit, delete, and git-manage memories, other tools
(Obsidian, agent-desktop) can too, and ingestion is the existing
pipeline. A memory file:

```markdown
---
description: one-line hook used when budgeting injection
projects: [ShatteredRealms]      # empty/omitted = workspace-wide
created: 2026-07-24
updated: 2026-07-24
---
Body: the memory itself. Link related notes with [[slug]] and cite
sources as ken://<project-id>/<rel-path>.
```

Frontmatter parsing is tolerant (`#[serde(default)]`, unknown keys
preserved on rewrite) — a hand-created file with no frontmatter is
still a valid memory (description falls back to first line).

### D2. Two tiers with different lifecycles

- **Long-term** (`.ken-workspace/memory/`, `<project>/.ken/memory/`):
  small, curated, one concern per file, slowly changing. Fully
  indexed (chunks + embeddings + knowledge model) — these SHOULD
  feed entities/links; they are the "ways of working" layer.
- **Short-term** (`.ken-workspace/journal/YYYY-MM-DD.md`): one file
  per day, append-only in practice. Journal (current and archive) is
  **search-only** tier via built-in rules on the pseudo-member:
  chunked, FTS'd, embedded, but never knowledge-model-extracted —
  daily churn must not mint KG entities. Recency access is direct
  file read (D4), not KG.

Archive roll: on workspace open, journal files older than 30 days
move to `journal/archive/` (same filenames — the move is the only
mutation, idempotent, watcher reindexes). 30 days is a constant in
`memory.rs`, not config, until real use argues otherwise.

### D3. The workspace pseudo-member

`.ken-workspace/` is ingested as a project with a reserved id
(`workspace` literal in addresses: `ken://workspace/<rel-path>`),
derived DB at the usual `db_path(base, <reserved-uuid>)`. Reuses
`engine.rs` unchanged — one more engine instance. Rules:

- Included in ⌘K fan-out and routing Broadcast (memories are often
  exactly what a manager-shaped query wants).
- Never shown in the member list, never profiled, never federated
  (its long-term memories' entities live in its own per-project KG;
  federation of memory entities is a possible later delta, not v1).
- Built-in tier rules (not a user-editable `.kenignore`):
  `memory/` full, `journal/` and `journal/archive/` search-only,
  `tasks/` search-only (see `ken-tasks`), `workspace.json` and
  `kg.sqlite` ignored.

### D4. Injection is budgeted, journal is a tool

Chat system context gets a `## Memories` block: all workspace
memories + focused project's memories, ordered by `updated` desc,
truncated to a 4,000-char budget (whole files only; over-budget
files summarized to their `description` line). Cheap, deterministic,
no retrieval step — long-term memory is small by construction, and
curation (not ranking) keeps it small. The journal is NOT injected:
a `read_journal(days_back?)` chat tool returns recent days on
demand, so "what happened yesterday" costs context only when asked.

### D5. One write core, two tool surfaces (locked)

`memory.rs` owns `write_memory(scope, slug, content)` (create or
replace body + bump `updated`; slug collision on create ⇒ error,
never silent overwrite) and `append_journal(text, project?, tags?)`
(append a `## HH:MM` entry to today's file, creating it). Ken chat
tools and `ken-mcp` tools are thin wrappers over these — same
validation, same events, one test surface (mirrors kg-routing D4).
MCP `journal_append` is how agent-desktop closes the loop: task
findings land as journal entries citing `ken://` addresses.

### D6. Promotion: Ken proposes, user approves (locked)

Distillation runs on demand ("distill my journal") and is offered
when an archive roll happens. The prompt gets the rolling window of
journal text + existing memory descriptions (dedupe guard) and must
return candidates as `{ slug, description, body, sources }`;
tolerant parsing, garbage ⇒ zero candidates, capped at 5 per run.
Each candidate renders as an approval card (target folder, full
body, source journal links); **approve** writes the file via D5,
**dismiss** records the slug in user-state so it isn't re-proposed.
Nothing autonomous ever writes to `memory/`.

## Risks / Trade-offs

- **Journal file churn vs watcher** — many small appends debounce
  fine (existing engine debounces); archive roll moves ≤ a few
  files/day. Spike S6 covers frontmatter/watch round-trip.
- **Injection bloat** — hard budget + curation posture; promotion
  keeps long-term small because it's approval-gated, not automatic.
- **Two agents writing the same day-file** — appends are
  open-append-close, last-writer-wins per entry; entries are
  timestamped sections so interleaving is harmless.
- **Reserved id collisions** — the workspace pseudo-member uuid is a
  fixed namespace-uuid of the workspace id; documented in README
  contracts.

## Migration

None. Flag off ⇒ no folders, no pseudo-member, no tools, no
injection. Deleting `memory/`/`journal/` files and the derived DB
loses nothing that wasn't visibly in those files.

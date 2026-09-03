# kenignore Specification

## ADDED Requirements

### Requirement: Three-tier classification

Every path in a project SHALL be classified `full`, `search-only`,
or `ignore` before ingestion. Full-tier content SHALL be chunked,
FTS-indexed, embedded, and eligible for knowledge-model extraction.
Search-only content SHALL be chunked, FTS-indexed, and embedded but
SHALL never enter knowledge-model extraction, entity/event minting,
profiler doc sampling, or federation. Ignore-tier paths SHALL
produce no index rows.

#### Scenario: search-only is findable but never load-bearing
- **WHEN** `~decompiled/` is in `.kenignore` and a query targets a
  decompiled class name
- **THEN** search returns the file, and no KG entity or profiler
  sample originates from `decompiled/`

### Requirement: .kenignore syntax and precedence

A `.kenignore` file at the project root SHALL use gitignore pattern
semantics where a plain pattern means ignore, a `~` prefix means
search-only, and a `!` prefix negates back to full, resolved
last-match-wins. Built-in tier rules SHALL be evaluated before the
user file so user lines can override them. `project.json`
`excluded` and built-in hard-ignores SHALL take precedence over
everything — no `!` SHALL resurrect them. Unmatched paths SHALL
default to full. Malformed lines SHALL be skipped with a warning
while the rest of the file applies.

#### Scenario: last match wins
- **WHEN** `.kenignore` contains `~decompiled/` followed by
  `!decompiled/notes/**`
- **THEN** `decompiled/Foo.java` is search-only and
  `decompiled/notes/entry-points.md` is full

#### Scenario: hard-ignore cannot be negated
- **WHEN** `.kenignore` contains `!node_modules/**`
- **THEN** `node_modules/` remains unindexed

### Requirement: Presence is the opt-in

`.kenignore` SHALL be respected whenever the file exists, with no
feature flag. Absent the file, classification SHALL yield behavior
and index content byte-identical to pre-feature Ken. Edits to
`.kenignore` SHALL be detected by the watcher and SHALL trigger
reindexing scoped to paths whose tier changed.

#### Scenario: no file, no change
- **WHEN** a project has no `.kenignore`
- **THEN** a fresh ingest produces index content identical to
  pre-feature behavior

#### Scenario: rule edit reindexes only what changed
- **WHEN** `~generated/` is removed from `.kenignore`
- **THEN** only paths under `generated/` are re-enqueued

## MODIFIED Requirements

### Requirement: Knowledge-model extraction input (knowledge-model)

Knowledge-model extraction SHALL select only full-tier content.
Search-only files SHALL not consume `EXTRACT_CHAR_BUDGET` and SHALL
not mint entities or events.

#### Scenario: ten thousand decompiled files stay out of the KG
- **WHEN** a project has 10k search-only decompiled files and 200
  full-tier source files
- **THEN** extraction input is drawn from the 200 only

### Requirement: Profiler doc sampling and .kenignore drafting
(project-profiler)

Profiler doc sampling SHALL select only full-tier content. When the
`profiler` flag is on, the analysis phase SHALL additionally draft
a proposed `.kenignore` (build outputs to ignore;
generated/decompiled/vendored to search-only) presented for review
before any write. When a `.kenignore` already exists, the profiler
SHALL propose only additions as a diff and SHALL never delete,
reorder, or overwrite user lines.

#### Scenario: draft on a fresh project
- **WHEN** the profiler analyzes a project with `target/` and
  `decompiled/` and no `.kenignore`
- **THEN** a draft proposing `target/` (ignore) and `~decompiled/`
  is shown, and no file is written until approval

#### Scenario: existing file is sacred
- **WHEN** the profiler runs against a project with a hand-written
  `.kenignore`
- **THEN** the user's lines are untouched and any suggestions
  appear as an additions-only diff

### Requirement: Search across tiers (semantic-index)

FTS, KNN, hybrid search, and routing SHALL query full and
search-only tiers alike, and results SHALL carry their tier so the
UI can badge search-only hits.

#### Scenario: hybrid search spans tiers
- **WHEN** a hybrid query matches one full-tier and one search-only
  chunk
- **THEN** both appear in results, the latter badged search-only

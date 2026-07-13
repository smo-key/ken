# knowledge-views

## ADDED Requirements

### Requirement: Knowledge model storage (DB v6)
The per-project database SHALL migrate from schema version 5 to 6,
adding `entities(id, kind, name, summary, sources)` — `kind` one of
`person|organization|topic|decision|other`, `sources` a JSON array of
project-relative paths — `entity_edges(id, a, b, label)` with both ends
referencing `entities(id) ON DELETE CASCADE`, and
`events(id, date, category, text, source)` with `date` a best-effort
`yyyy-mm-dd` string. The build timestamp SHALL be stored in the
existing `meta` table under `knowledge_model_built_at`.
`replace_knowledge_model` SHALL replace the whole model (entities,
edges, events, timestamp) in a single transaction;
`list_entities_with_edges`, `list_events` (newest first), and
`knowledge_model_built_at` SHALL read it back.

#### Scenario: v5 database migrates to v6
- **WHEN** a database at schema version 5 is opened
- **THEN** the entities, entity_edges, and events tables exist and are
  usable, and earlier data (files, digests, runs, …) survives

#### Scenario: Replace is atomic and idempotent
- **WHEN** `replace_knowledge_model` is called twice with the same
  entities, edges, and events
- **THEN** the stored model matches the input exactly after each call —
  no duplicated rows, no orphaned edges — and
  `knowledge_model_built_at` carries the latest timestamp

#### Scenario: No model yet reads as empty
- **WHEN** the model has never been built
- **THEN** the lists are empty and `knowledge_model_built_at` is None

### Requirement: Extraction prompt
`compose_extraction_prompt(files, today)` SHALL produce a prompt that
directs the agent to read the project's source material (listing the
indexed files, capped at 200 paths with an honest "…and N more"),
carries today's date, and demands ONLY a JSON object of the exact shape
`{"entities": [{"kind", "name", "summary", "sources",
"connections": [{"to", "label"}]}], "events": [{"date", "category",
"text", "source"}]}` — at most ~40 entities and ~60 events, only ones
actually grounded in the material, dates best effort from filenames or
content, events with no inferable date omitted.

#### Scenario: Prompt carries the contract
- **WHEN** the prompt is composed
- **THEN** it contains the JSON shape (entities with kind/name/summary/
  sources/connections, events with date/category/text/source), the
  entity and event caps, the groundedness rule, the omit-undated-events
  rule, today's date, and the file list

#### Scenario: File list is capped
- **WHEN** more than 200 indexed files exist
- **THEN** the prompt lists 200 and says how many more there are

### Requirement: Tolerant extraction parsing
`parse_extraction` SHALL accept any answer containing a JSON object —
code fences and surrounding prose stripped — and salvage what it can:
unknown fields ignored; entities without a usable name dropped; kinds
outside the enum coerced to `other`; events with a missing/invalid
`yyyy-mm-dd` date or empty text dropped; categories normalized to one
lowercase word; caps (40 entities / 60 events) enforced.
`connections.to` SHALL be resolved against the answer's own entity
names case-insensitively; dangling references and self references are
dropped, and duplicate pairs (either direction) collapse to one edge.
Only an answer with no parseable JSON object is an error.

#### Scenario: Fenced JSON parses
- **WHEN** the answer wraps the JSON object in ```json fences or prose
- **THEN** parsing succeeds with the same result as the bare object

#### Scenario: Dangling connection is dropped
- **WHEN** an entity's connection names an entity that isn't in the
  answer
- **THEN** the model parses without that edge, keeping everything else

#### Scenario: Connections resolve case-insensitively
- **WHEN** a connection says "priya n." and the entity is "Priya N."
- **THEN** the edge is created

#### Scenario: Bad dates drop the event, not the batch
- **WHEN** an event carries no date or a malformed one
- **THEN** that event is dropped and the remaining events survive

#### Scenario: No JSON at all is an error
- **WHEN** the answer contains no JSON object
- **THEN** parsing fails with a plain-language error

### Requirement: Building the knowledge model
`build_knowledge_model(binary, project, db, today, cancel)` SHALL
compose the extraction prompt from the DB's indexed files, run one
headless session via `assistant::oneshot` with a 10-minute timeout,
parse the result, store it with `replace_knowledge_model`, and return
the entity/edge/event counts. Cancellation, timeout, and failure SHALL
surface as plain-language errors, and a failed build SHALL leave the
previously stored model untouched.

#### Scenario: Successful build stores the model
- **WHEN** the session completes with a parseable extraction
- **THEN** the DB holds exactly that model, `knowledge_model_built_at`
  is set, and the returned counts match

#### Scenario: Failed build keeps the old model
- **WHEN** the session fails or the answer holds no JSON
- **THEN** an error is returned and the previously stored model is
  still intact

### Requirement: Refresh trigger and read command
The app SHALL expose `refresh_knowledge_model()` — manual only in v1 —
which errors immediately when Claude Code is missing or a build is
already running, then builds on a worker thread, emitting
`knowledge-model-state` events: `building` when the work starts,
`ready` on success, `error` with plain-language detail on failure. A
`knowledge_model()` command SHALL return
`{entities, edges, events, builtAt}` (camelCase; `builtAt` null until
the first build).

#### Scenario: Refresh reports through events
- **WHEN** `refresh_knowledge_model` starts a build that succeeds
- **THEN** listeners see `building` and then `ready`, and a subsequent
  `knowledge_model()` returns the fresh model

#### Scenario: Failure carries detail
- **WHEN** the build fails
- **THEN** a `knowledge-model-state` event with state `error` carries a
  plain-language detail

#### Scenario: Concurrent refresh is refused
- **WHEN** `refresh_knowledge_model` is called while a build is running
- **THEN** it returns a friendly error and the running build continues

### Requirement: Shared knowledge store
Both screens SHALL read one Svelte store (`knowledge.svelte.ts`) that
loads the model on first visit, re-loads it when a `ready` event
arrives, exposes `refresh()`, and carries `building`, `error`, and
whether Claude Code is available.

#### Scenario: First visit loads the model
- **WHEN** the user first opens Map or Timeline
- **THEN** the store fetches `knowledge_model()` once and both screens
  render from it

#### Scenario: Ready event refreshes the view
- **WHEN** a `knowledge-model-state` `ready` event arrives
- **THEN** the store re-fetches and the open screen repaints

### Requirement: Map screen
The Map screen SHALL render the entity graph on a full-bleed `--surface`
canvas: edges as 1.5px `--border-strong` SVG lines, entities as pill
nodes positioned by a deterministic layout — the highest-degree entity
ink-solid in the center, its neighbors on an inner ring, other
connected entities on an outer ring, and degree-0 entities on the
periphery with a dashed amber "mentioned but unconnected" treatment —
with angles seeded from a stable hash of the entity name so the layout
is identical across reloads. `decision` nodes use the mono label style.
Clicking a node opens its first source in Files; hovering raises the
node and shows its summary. A legend bottom-left explains the dashed
treatment. The header row shows serif "Map", a built-at caption, and a
"Refresh map" button reflecting the building state.

#### Scenario: Layout is deterministic
- **WHEN** the same model is laid out twice
- **THEN** every node gets identical coordinates, the highest-degree
  entity is centered, and degree-0 entities sit in the peripheral band

#### Scenario: Node click opens the source
- **WHEN** the user clicks a node that has at least one source path
- **THEN** that file opens in the Files screen

#### Scenario: Unconnected entities read as such
- **WHEN** an entity has no edges
- **THEN** its node is dashed amber on the periphery and the legend
  explains it

### Requirement: Timeline screen
The Timeline screen SHALL render the event stream newest-first: a serif
"Timeline" header with a "View as of…" chip toggling a date input that
keeps only events with `date <=` the chosen day; a search field
filtering events client-side by case-insensitive substring, showing
"N of M events" and amber-highlighting matches in event text via
escape-then-mark injection; category chips derived from the data (All
active by default as an ink-solid pill, plus distinct categories
ordered by count, at most 6); and a vertical line with dot markers —
the newest visible event's dot accent-ringed — where each event shows a
date overline, a category mini-pill, the event sentence, and a mono
source chip that opens the file in Files.

#### Scenario: Search filters and highlights
- **WHEN** the user types a query matching 4 of 61 events
- **THEN** only those 4 render, the count reads "4 of 61 events", and
  the matched substring is highlighted — with any HTML in the event
  text escaped, never executed

#### Scenario: View as of filters by date
- **WHEN** the user picks a past date
- **THEN** only events dated on or before it remain

#### Scenario: Category chips derive from the data
- **WHEN** the model's events span several categories
- **THEN** chips show All plus up to 6 distinct categories by count,
  and selecting one filters the stream

### Requirement: Empty and building states
Both screens SHALL show, when no model has ever been built, a centered
card — "Ken hasn't mapped this project yet" — with a primary Refresh
button, plus an honest note when Claude Code is missing. While a build
runs, the refresh control SHALL show a building state, and errors SHALL
surface in plain language.

#### Scenario: Never built shows the empty card
- **WHEN** `builtAt` is null
- **THEN** the centered card with the primary Refresh button renders
  instead of the view

#### Scenario: Missing Claude is stated honestly
- **WHEN** Claude Code is not installed
- **THEN** the empty card explains the knowledge views need it

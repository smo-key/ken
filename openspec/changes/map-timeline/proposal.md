# Proposal: map-timeline

## Why

Ken's design promises two knowledge views (§4.9): a **Map** — a graph of
the people, organizations, topics, and decisions in the project and how
they connect — and a **Timeline** — a chronological event stream with
search, category chips, source chips, and a "View as of…" control. Both
are read models over a lightweight entity/event extraction that Ken
maintains internally: an ingest whose output is structured data in the
LOCAL DB, not a project file. Change 9 of 10 ships the extraction, the
storage, and both screens on top of the oneshot assistant and DB
plumbing that already exist.

## What Changes

- **DB schema v6** (SCHEMA_VERSION 5 → 6): three new tables —
  `entities(id, kind, name, summary, sources)` where `kind` is
  `person|organization|topic|decision|other` and `sources` is a JSON
  array of project-relative paths; `entity_edges(id, a, b, label)` with
  both ends `REFERENCES entities(id) ON DELETE CASCADE`; and
  `events(id, date, category, text, source)` where `date` is
  best-effort `yyyy-mm-dd`. The build timestamp lives in the existing
  `meta` table under `knowledge_model_built_at`. New CRUD:
  `replace_knowledge_model` (one transaction, full replace — extraction
  is corpus-wide, not incremental, in v1), `list_entities_with_edges`,
  `list_events`, `knowledge_model_built_at`.
- **New ken-core module `knowledge_model.rs`**:
  - `compose_extraction_prompt(files, today)` — instructs the agent to
    read the project's source material (listed, capped at 200 paths)
    and output ONLY a JSON object `{"entities": [{"kind", "name",
    "summary", "sources", "connections": [{"to", "label"}]}],
    "events": [{"date", "category", "text", "source"}]}` — at most ~40
    entities / ~60 events, only ones actually grounded in the material,
    dates best effort (from filenames or content; events with no
    inferable date are omitted).
  - `parse_extraction(raw)` — tolerant: as long as the answer contains
    a JSON object it parses (code fences stripped, unknown fields
    ignored, malformed records dropped rather than failing).
    `connections.to` is resolved by case-insensitive name match;
    dangling and self references are dropped; duplicate pairs collapse
    to one edge.
  - `build_knowledge_model(binary, project, db, today, cancel)` —
    compose → `assistant::oneshot` (10-minute timeout) → parse →
    `replace_knowledge_model` → counts.
- **Trigger — manual only in v1**: a Tauri command
  `refresh_knowledge_model` runs the build on a spawned thread and
  emits `knowledge-model-state` events (`building` / `ready` /
  `error {detail}`); `knowledge_model()` returns
  `{entities, edges, events, builtAt}`. Both screens share one store,
  `src/lib/knowledge.svelte.ts` (loads on first visit, exposes
  `refresh()`).
- **Map screen** (replacing the placeholder): full-bleed `--surface`
  canvas; SVG edges (1.5px `--border-strong`); nodes are pill divs
  positioned over the SVG by a deterministic TS layout — no physics
  dependency: the highest-degree entity sits ink-solid in the center,
  its neighbors on an inner ring, other connected entities on an outer
  ring, and degree-0 entities in a peripheral band with the dashed
  amber "mentioned but unconnected" treatment; angles are seeded from a
  stable hash of the name so the layout survives reloads. `decision`
  nodes use the mono label style. Click opens the entity's first source
  in Files; hover raises the node and shows the summary. Legend bottom
  left; header row with serif "Map", a built-at caption, and a
  "Refresh map" button that reflects the building state. Empty state:
  a centered "Ken hasn't mapped this project yet" card with a primary
  Refresh button and an honest note when Claude Code is missing.
- **Timeline screen** (replacing the placeholder): serif "Timeline"
  header with a "View as of…" chip toggling a date input (events with
  `date <= chosen` remain); a search field filtering events client-side
  by substring with an "N of M events" count and amber highlight on
  matches (escape-then-mark, same pattern as the search overlay);
  category chips derived from the data (All + distinct categories,
  count-ordered, max 6); a vertical timeline, newest first, with the
  newest dot accent-ringed, a date overline + category mini-pill per
  event, the event sentence, and a mono source chip that opens the file
  in Files. Same empty/refresh/building states as Map.
- Nav rail: unchanged — Map and Timeline simply stop being
  placeholders.

## Capabilities

### New Capabilities
- `knowledge-views`: the entity/event extraction (schema, prompt,
  parser, build), the refresh trigger and read command, and the Map and
  Timeline screens.

### Modified Capabilities

_None — the knowledge model is an additive layer over the existing DB,
oneshot assistant, and app shell._

## Impact

- `crates/ken-core`: `db.rs` v6 migration + knowledge-model CRUD; new
  `knowledge_model.rs`; `lib.rs` module line.
- `src-tauri`: `knowledge_running` flag on `ActiveProject`;
  `refresh_knowledge_model` + `knowledge_model` commands;
  `knowledge-model-state` event.
- Frontend: `api.ts` types + wrappers + event listener; new
  `src/lib/knowledge.svelte.ts` store and `src/lib/knowledge.ts` pure
  helpers (deterministic map layout, safe match highlighting) with
  vitest coverage; `MapScreen.svelte` and `TimelineScreen.svelte`
  rebuilt from the prototype.
- Tests: v5→v6 migration, replace-is-atomic-and-idempotent, compose +
  parse fixtures (fences, dangling refs, bad dates), an end-to-end
  build against the fake claude via `oneshot_result`, and frontend
  layout/highlight unit tests. All existing tests stay green.

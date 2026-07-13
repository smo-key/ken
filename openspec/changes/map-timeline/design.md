# Design: map-timeline

## Context

The digest already proved the shape this change needs: compose a prompt
from what the index knows, run `assistant::oneshot` on a worker thread,
parse the answer tolerantly, store the result in the local DB, and let
a small runes store paint it. The knowledge model is the same loop with
a structured (JSON) answer and two read-only screens over it. Nothing
here writes project files.

## Goals / Non-Goals

**Goals**

- One manual "Refresh" produces a corpus-wide entity/event extraction
  stored entirely in the local DB — Map and Timeline are pure read
  models over it.
- Deterministic, dependency-free map layout: the same model always
  renders the same picture.
- Honest states everywhere: empty (never built), building, error,
  Claude-missing.

**Non-Goals**

- No incremental extraction, no automatic refresh triggers, no
  file-watch coupling — v1 rebuilds the whole model on demand.
- No graph physics library; no pan/zoom.
- No new write paths into the project folder (per §4.9).

## Decisions

### Storage (DB v6)

- Three plain tables; `entity_edges` references `entities` with
  `ON DELETE CASCADE`, so a full replace only has to clear `entities`
  and `events`. `sources` is a JSON text column — the list is tiny,
  read-only, and never queried by element.
- `knowledge_model_built_at` lives in the existing `meta` table: it is
  a single scalar, and `meta` already exists in every schema version,
  so the migration only adds tables.
- `replace_knowledge_model(entities, events, built_at)` is one
  transaction: clear, insert entities (capturing rowids), insert edges
  by mapping the parser's entity indices to those rowids, insert
  events, stamp the meta key. Atomic and idempotent by construction —
  the extraction is corpus-wide, so replace-all is the correct
  semantics and needs no diffing.

### Extraction (`knowledge_model.rs`)

- The prompt demands ONLY a JSON object and spells the exact shape,
  with caps (~40 entities / ~60 events), a groundedness rule (never
  invent), and best-effort dates (omit events with no inferable date).
  The indexed file list is capped at 200 paths like the digest caps its
  list — enough to orient the agent, which can read files itself.
- `parse_extraction` finds the first `{` and the last `}` and parses
  that slice — code fences and prose fall away for free. Everything
  after that is salvage, not validation: unknown fields ignored,
  records missing a usable name/text/date dropped, kinds outside the
  enum coerced to `other`, categories normalized to one lowercase word,
  caps enforced. `connections.to` resolves case-insensitively against
  the entity names in the same answer; dangling and self references are
  dropped and duplicate pairs (either direction) collapse — the DB
  never sees an edge it can't draw.
- `build_knowledge_model(binary, project, db, today, cancel)` takes
  `today` from the caller (ken-core has no clock/locale dependency;
  src-tauri already formats local dates for the digest). Timeout 10
  minutes — corpus-wide reading is slower than a digest. Cancel /
  timeout / failure map to plain-language errors; only a Completed +
  parsed answer touches the DB, so a failed refresh never destroys the
  previous model.

### Trigger & commands

- Manual only in v1: `refresh_knowledge_model` mirrors the digest's
  worker pattern — an `AtomicBool` guard per active project (second
  click while building is a friendly error), `knowledge-model-state`
  events (`building` → `ready`/`error {detail}`), DB opened fresh on
  the thread. Missing Claude is an immediate guided error, since the
  only caller is an explicit button.
- `knowledge_model()` returns the whole model in one call
  (`{entities, edges, events, builtAt}`); the model is small by
  construction (caps), so no pagination.

### Frontend

- One store (`knowledge.svelte.ts`) for both screens: loads on first
  visit, re-loads on `ready`, exposes `refresh()`, carries
  `building`/`error`/`claudeFound`. Screens call `visit()` on mount so
  a project switch (which remounts screens) re-reads the new DB.
- Layout is a pure function in `knowledge.ts` (unit-testable, no DOM):
  degree from edges; highest-degree entity centered ink-solid;
  its neighbors on an inner ellipse, other connected entities on an
  outer ellipse, degree-0 entities on a peripheral band with clamped
  coordinates; within a ring, nodes are ordered and angle-jittered by
  an FNV-1a hash of the name — stable across reloads with no stored
  positions. Edges render in one SVG (`viewBox 0 0 100 100`,
  `preserveAspectRatio="none"`, `vector-effect: non-scaling-stroke`
  keeps the 1.5px stroke honest); nodes are absolutely positioned pill
  divs over it, exactly like the prototype.
- Timeline filtering is all client-side (the model is capped): search
  is a case-insensitive substring over the event text, highlighted with
  the same escape-then-mark discipline as `SearchOverlay.renderSnippet`
  (`highlightMatches` escapes the text and injects only its own
  `<mark>` tags); category chips derive from the data (All + distinct
  categories, count-ordered, max 6); "View as of…" keeps events with
  `date <= chosen` — dates are `yyyy-mm-dd` strings, so string
  comparison is date comparison.

## Risks / Trade-offs

- **The model may answer with prose around the JSON or slightly wrong
  fields.** The first-`{`-to-last-`}` slice plus per-record salvage
  keeps every recoverable extraction; a totally JSON-free answer is a
  visible, retryable error and the old model survives.
- **Full replace loses nothing durable** — the model is derived data,
  rebuildable from the corpus at any time, same recovery story as the
  index itself.
- **Hash-seeded ring layout can still put two long labels near each
  other.** Accepted for v1: determinism and zero dependencies beat
  perfect spacing at these sizes (≤40 nodes).

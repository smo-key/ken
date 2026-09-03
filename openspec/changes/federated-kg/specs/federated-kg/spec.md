# federated-kg Specification

## ADDED Requirements

### Requirement: Workspace KG is derived and disposable

The workspace KG SHALL live in `.ken-workspace/kg.sqlite` with its own
schema version, built exclusively by reading member DBs (read-only),
and SHALL be safe to delete at any time — the next build reproduces an
equivalent graph (identical when LLM passes are disabled).

#### Scenario: delete and rebuild
- **WHEN** kg.sqlite is deleted and a rebuild runs with LLM passes
  disabled
- **THEN** the resulting global entities, links, and edges are
  identical to the previous deterministic build

#### Scenario: member DBs untouched
- **WHEN** any workspace KG build runs
- **THEN** no member DB receives a write

### Requirement: Two-tier entity resolution, conservative by default

Local entities SHALL merge into one global entity when normalized name
(casefold, trimmed, collapsed whitespace/punctuation) and kind match.
Near-miss pairs (shared token, edit distance ≤ 2, containment) MAY be
adjudicated by the local LLM at Background priority, capped at 100
pairs per build; any parse failure, timeout, or non-affirmative answer
SHALL result in no merge. Merges never span kinds.

#### Scenario: exact-name merge across members
- **WHEN** two members each have a `topic` entity normalizing to
  "shattered realms"
- **THEN** one global entity exists with two `entity_links` rows

#### Scenario: doubt keeps entities separate
- **WHEN** the adjudication output for a near-miss pair is unparseable
- **THEN** both entities remain separate global entities

### Requirement: Every global entity is a wiki page

Every global entity SHALL have a non-empty summary (single-link ⇒
copied local summary; multi-link ⇒ LLM merge capped at 400 chars with
longest-local-summary fallback), edges traversable in both directions,
and `doc_pointers` resolving to `ken://<project-id>/<rel-path>`
addresses. `workspace_kg_entity(id)` SHALL return summary, out-links,
back-links, and per-project pointers in one payload.

#### Scenario: wiki page assembles both directions
- **WHEN** entity A has an edge A→B and entity C has an edge C→A
- **THEN** `workspace_kg_entity(A)` lists B under out-links and C
  under back-links

#### Scenario: pointers resolve
- **WHEN** any `doc_pointers` row is followed
- **THEN** it opens an existing file in the owning member (or is
  marked stale if the file no longer exists — never a crash)

### Requirement: Cross-project edges with provenance

Edges SHALL carry provenance `imported` (from a member's own
entity_edges), `cooccur` (linked local entities sharing source files
across members), or `llm` (typed relations from the linking pass,
capped at 50 pairs per build, fallback relation "related").

#### Scenario: co-occurrence produces a cross-project edge
- **WHEN** merged entities X and Y have source files in different
  members that mention both
- **THEN** a `cooccur` edge X—Y exists with weight ≥ 1

### Requirement: Incremental rebuild via watermarks

Builds SHALL cache a per-member snapshot keyed by that member's
`knowledge_model_built_at`, re-read only members whose watermark
advanced, and re-merge all snapshots. Member knowledge-model
completion in workspace mode SHALL trigger a debounced (30 s) build;
builds run one at a time and are cancellable.

#### Scenario: unchanged members are skipped
- **WHEN** only member A's knowledge model was rebuilt since the last
  workspace-KG build
- **THEN** the build re-reads member A only and completes with all
  other snapshots served from cache

### Requirement: Flag-gated with LLM-free operation

With `federatedKg` off, kg.sqlite SHALL be neither created nor read
and per-project maps behave as today. With the flag on and no model
available, builds SHALL complete using deterministic passes only,
marked `llm_passes: false`, and upgrade on a later build.

#### Scenario: build without a model
- **WHEN** a build runs before any local model is downloaded
- **THEN** it reaches `ready` with deterministic merges, fallback
  summaries, and no `llm`-provenance edges

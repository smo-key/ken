# Proposal: federated-kg

## Why

Each Ken project already builds a private knowledge graph
(`entities`, `entity_edges`, `events` in the per-project DB, built by
`knowledge_model.rs`). In a workspace those graphs are silos: the
"ShatteredRealms" entity in the game repo and the one in
ShatterdRealmsTools are unrelated rows. This change builds the
**workspace knowledge graph** at `.ken-workspace/kg.sqlite`: a derived,
rebuildable federation of the member graphs where same-named concepts
merge into global entities, cross-project relationships become edges,
and every global entity is a Karpathy-style wiki page — summary at the
node, dense bidirectional links, and addressable pointers
(`kg://<entity-id>`, `ken://<project-id>/<rel-path>`) down into the
projects that mention it. This is the map layer that `kg-routing` will
navigate.

## What Changes

- **New workspace DB `.ken-workspace/kg.sqlite`** (own schema version,
  fully derived — deleting it and rebuilding is always safe):
  - `global_entities(id, name, kind, summary, updated_at)` — kinds
    reuse the per-project set (person|organization|topic|decision|
    other).
  - `entity_links(global_id, project_id, local_entity_id, local_name)`
    — the federation mapping; one global entity ↔ N per-project
    entities.
  - `global_edges(src_global_id, dst_global_id, relation, weight,
    provenance)` — provenance = `imported` (existed inside one
    project), `cooccur` (name co-mention across projects), or `llm`
    (linking pass).
  - `doc_pointers(global_id, project_id, rel_path, snippet)` — top
    source files per entity per project, for wiki-page "mentioned in"
    sections and for `kg-routing` to jump from entity → project →
    files.
  - `meta(key, value)` — schema version, per-member
    `knowledge_model_built_at` watermarks for incremental rebuild.
- **New ken-core module `federation.rs`**:
  - `build_workspace_kg(members, db) -> BuildReport` — reads each
    member's entities/edges (read-only), merges, writes kg.sqlite.
  - Entity resolution, two tiers: (1) deterministic — normalized name
    (casefold, trim, collapse whitespace/punctuation) + same kind ⇒
    merge; (2) bounded LLM adjudication — near-miss candidate pairs
    (shared token, edit distance, alias patterns) batched to the
    local model at Background priority, hard cap 100 pairs per build,
    tolerant parsing, "no" is the default on any doubt or failure.
  - Cross-project edges: `cooccur` edges where linked local entities
    share source files across members; one optional LLM linking pass
    proposes typed relations for the top co-occurring pairs (cap 50),
    provenance `llm`.
  - Global summaries: entity with 1 link ⇒ copy the local summary;
    N links ⇒ Background LLM merge of the local summaries (cap 400
    chars), fallback = longest local summary. Every global entity
    always has a summary (principle 3).
  - Incremental: rebuild recomputes only members whose
    `knowledge_model_built_at` moved past the stored watermark;
    global merge re-runs over cached per-member snapshots.
- **src-tauri**: `build_workspace_kg` command (auto-triggered
  debounced when a member's knowledge model finishes, manual "Rebuild
  workspace graph" action), `workspace-kg-state` events
  (`building`/`ready`/`unavailable` + progress), `workspace_kg_
  overview`, `workspace_kg_entity(id)` (the wiki page payload:
  summary, out-links, back-links, per-project doc pointers),
  `workspace_kg_search(query)` (FTS over global entity names +
  summaries).
- **Frontend**: workspace Map view — reuses the existing per-project
  map component with project-colored nodes (member hue), merged
  entities badged with member count; entity detail panel = the wiki
  page (summary, linked entities both directions, "mentioned in"
  pointers that open the file in the owning member via focus switch).
- **Flag**: `federatedKg` (workspace-level, in workspace.json
  features). Requires `workspace`. Off ⇒ kg.sqlite never created or
  read; per-project maps untouched.

## Capabilities

### New Capabilities
- `federated-kg`: workspace KG schema/build/resolution/incremental,
  wiki-page reads, workspace map UI.

### Modified Capabilities
- `knowledge-views`: member knowledge-model completion emits a
  workspace-level trigger when in workspace mode.

## Impact

- `crates/ken-core`: new `federation.rs` + `workspace_kg_db.rs`
  (schema, CRUD, watermarks); no changes to per-project schema.
- `src-tauri`: build orchestration (Background, cancellable, one
  build at a time — mirroring the per-project engine discipline),
  commands + events.
- Frontend: Map view workspace mode, entity wiki panel, rebuild
  action in workspace settings.
- Tests: resolution tiers (exact merge, near-miss adjudication
  fixtures, doubt ⇒ no merge), federation over two fixture member
  DBs, incremental skip via watermarks, derived-rebuild determinism
  (delete kg.sqlite ⇒ identical graph modulo LLM passes disabled),
  pointer integrity (every doc_pointer resolves to a member path).

# semantic-index Specification

## ADDED Requirements

### Requirement: Per-project vector storage in the existing DB

The per-project SQLite DB SHALL store chunk text in a `chunks` table
(`UNIQUE(path, seq)`, per-chunk `content_hash`) and embeddings in a
`sqlite-vec` `vec_chunks` virtual table whose rowids equal `chunks.id`,
with `embed_model`, `embed_dim`, and `semantic_built_at` recorded in
`meta`. The schema migration to v12 SHALL succeed even when sqlite-vec
is unavailable.

#### Scenario: v11 database migrates cleanly
- **WHEN** a v11 project DB is opened by the new build
- **THEN** schema version is 12, all pre-existing rows survive, and
  `chunks` is empty

#### Scenario: extension unavailable degrades, never errors
- **WHEN** sqlite-vec fails to initialize on a connection
- **THEN** `vec_available` is false, `hybrid_search` returns exactly
  the FTS-only results, and a single `semantic-index-state`
  `unavailable {reason}` event is emitted for the project

### Requirement: Chunking is deterministic and profile-driven

`chunk_file(rel_path, text, profile)` SHALL be a pure function
producing ordered chunks with stable `content_hash` values, using prose
mode (heading/paragraph-aware, ~350-token target, ~15% overlap) or code
mode (~500-token blocks) selected by the profile, defaulting by file
extension, capped at 200 chunks per file.

#### Scenario: identical input, identical chunks
- **WHEN** the same text and profile are chunked twice
- **THEN** chunk boundaries, seq numbers, and hashes are identical

#### Scenario: unchanged file costs no embeddings
- **WHEN** an ingest re-processes a file whose chunk hash list is
  unchanged
- **THEN** no embed calls are made and its `vec_chunks` rows are
  untouched

### Requirement: Local embedding behind a trait seam

Embeddings SHALL be produced by an `Embedder` trait with a
llama.cpp-backed implementation (default `nomic-embed-text-v1.5`,
768-dim, document/query prefixes applied internally) and a
deterministic fake for tests. Corpus embedding SHALL run at Background
priority; query embedding at Interactive priority.

#### Scenario: model change invalidates the index
- **WHEN** a project opens and `meta.embed_model` differs from the
  configured model
- **THEN** the semantic index is marked stale and rebuilt in the
  background while FTS search remains available throughout

### Requirement: Hybrid search merges FTS and KNN by FTS-priority fill

With the `semanticIndex` flag on, search SHALL embed the query, take
top-k from FTS5 and from `vec_chunks` KNN, and merge them with
"FTS-priority fill" (design B4): FTS hit order is preserved as-is,
then any KNN hits not already present are appended below it (not
re-ranked or interleaved with FTS). Results are grouped by path (best
chunk is the snippet), and each result is labeled `keyword`,
`semantic`, or `both`. With the flag off, search behavior SHALL be
byte-identical to the current FTS-only path.

> Note: reciprocal rank fusion (k=60) was the original design but was
> rejected per spike S7 (lost to FTS5-only on the golden mini-set); see
> `spikes/S7b-retrieval-fixes.md` and `design.md`'s "D4. Hybrid merge"
> section for the full history. Implemented in
> `crates/ken-core/src/search.rs`'s `merge_hits`.

#### Scenario: semantic-only hit is findable and labeled
- **WHEN** a document contains "exponential wait between attempts" and
  the user searches a paraphrase that FTS does not match but the fake
  embedder maps to the same vector
- **THEN** the document appears in results labeled `semantic`

#### Scenario: flag off is exactly today
- **WHEN** `semanticIndex` is off for the project
- **THEN** the search command returns the same results and shape as the
  pre-change FTS implementation, with no embed calls

### Requirement: The semantic index is entirely derived

`rebuild_semantic_index` SHALL regenerate `chunks` and `vec_chunks`
from indexed contents alone, be cancellable via the engine's cancel
token, report `building {done,total}` / `ready` progress events, and
run as a Background step of the existing one-at-a-time ingest engine.

#### Scenario: rebuild after deletion restores the index
- **WHEN** `chunks` and `vec_chunks` are dropped and a rebuild runs
- **THEN** hybrid search results are equivalent to before the deletion

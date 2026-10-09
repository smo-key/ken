# Proposal: semantic-index

## Why

Ken's only retrieval today is SQLite FTS5 keyword search (`db.rs`, schema
v11). Keyword search cannot answer "where do we handle retry backoff?"
when the doc says "exponential wait between attempts," and it gives the
chat/quick-answer features nothing better than BM25 to ground on. This
change adds a **per-project semantic index**: every indexed file is
chunked and embedded with a local GGUF model, vectors live in the same
per-project SQLite DB via `sqlite-vec`, and a **hybrid search** (FTS5 +
KNN merged by reciprocal rank fusion) becomes available behind the
`semanticIndex` flag. It is the retrieval foundation the later
`kg-routing` feature fans out to, but it is useful standalone in
single-project Ken.

## What Changes

- **DB schema v12** (SCHEMA_VERSION 11 → 12): two new tables —
  `chunks(id INTEGER PK, path TEXT NOT NULL, seq INTEGER NOT NULL,
  text TEXT NOT NULL, token_est INTEGER NOT NULL, content_hash TEXT
  NOT NULL, UNIQUE(path, seq))` and the `sqlite-vec` virtual table
  `vec_chunks` (`embedding float[<dim>]`, rowid = `chunks.id`). New
  `meta` keys: `embed_model`, `embed_dim`, `semantic_built_at`.
  The virtual table is created lazily (only when the flag is on and the
  extension loaded), so v12 migration itself never requires sqlite-vec.
- **sqlite-vec loading**: `db.rs` gains `load_vec_extension(conn) ->
  bool` using the statically linked `sqlite-vec` crate
  (`sqlite_vec::sqlite3_vec_init` via `rusqlite`'s
  `load_extension`-free auto init). Failure is non-fatal: recorded, and
  all semantic APIs return graceful "unavailable".
- **New ken-core module `embedder.rs`**: `trait Embedder { fn embed(&
  mut self, texts: &[String]) -> Result<Vec<Vec<f32>>>; fn dim(&self)
  -> usize; fn model_id(&self) -> String; }` with a llama.cpp-backed
  implementation (embedding mode, default model
  `nomic-embed-text-v1.5` GGUF, 768-dim) and a deterministic
  `FakeEmbedder` for tests. Model file management mirrors the existing
  chat-model download/lookup in `local_llm.rs`. Embedding batches run
  at **Background** priority so interactive chat always preempts.
- **New ken-core module `chunker.rs`**: pure function
  `chunk_file(rel_path, text, profile) -> Vec<Chunk>` — prose mode
  (heading/paragraph-aware, target ~350 token-estimate, ~15% overlap)
  and code mode (blank-line/brace-block windows, target ~500). The
  `profile` argument is the `IndexProfile` type introduced here with a
  hard-coded `IndexProfile::default_for(rel_path)` (by extension);
  the later `project-profiler` feature only *supplies* better profiles.
- **Ingest integration** (`engine.rs` + `index.rs` path): after a file's
  `contents` row is written, if the flag is on, its chunks are diffed by
  `content_hash` and stale ones re-embedded; deletes cascade. A full
  `rebuild_semantic_index(db, embedder, cancel)` mirrors `rebuild()` —
  entirely derived. Progress surfaces through the existing
  `IngestEvent`-style event (`semantic-index-state`:
  `building {done,total}` / `ready` / `unavailable {reason}`).
- **Search**: `db.rs` gains `semantic_search(conn, query_vec, k)` and
  a pure `rrf_merge(fts_hits, vec_hits, k) -> Vec<HybridHit>`
  (reciprocal rank fusion, k=60 constant). New Tauri command
  `hybrid_search(query, limit)` embeds the query at **Interactive**
  priority, runs both retrievals, merges, and returns
  `{path, snippet, score, source: "keyword"|"semantic"|"both"}`.
  The ⌘K search overlay shows hybrid results with a subtle "semantic"
  chip on non-keyword hits when the flag is on; flag off = today's FTS
  results, byte-identical.
- **Flag**: `semanticIndex` (per-project; see
  `features/multi-project/README.md` for the flag mechanism). Toggling
  on schedules a background build; toggling off leaves tables in place
  but stops embedding and hides hybrid results.

## Capabilities

### New Capabilities
- `semantic-index`: chunking, local embeddings, sqlite-vec storage,
  hybrid search, rebuild story, and flag gating.

### Modified Capabilities
- `search`: ⌘K results become hybrid when the flag is on.

## Impact

- `crates/ken-core`: `db.rs` v12 migration + vec load + search CRUD;
  new `embedder.rs`, `chunker.rs`; `Cargo.toml` adds `sqlite-vec`.
- `src-tauri`: `hybrid_search` command; `semantic-index-state` event;
  flag read on project open; background build scheduling.
- Frontend: `api.ts` types/wrappers/listener; search overlay chip +
  settings toggle in the Features disclosure.
- Tests: v11→v12 migration; chunker fixtures (prose headings, code,
  overlap, hash stability); RRF merge unit tests; end-to-end
  index-then-search with `FakeEmbedder`; extension-unavailable
  degradation; flag-off is unchanged FTS.

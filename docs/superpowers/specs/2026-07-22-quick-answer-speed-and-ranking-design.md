# Quick-answer speed + search ranking — Phase 1 design

Date: 2026-07-22. Approved via brainstorming session.

## Goals

- Quick answer: first streamed tokens < 1 s after typing stops (local model path), with richer source context (paragraph excerpts instead of ~12-token FTS snippets).
- Search: better ordering — phrase/proximity boost for strong matches, bounded recency boost for recently modified files. Semantic (embedding) ranking is Phase 2, out of scope here.

## Changes

### 1. Frontend timing — `src/search/SearchOverlay.svelte`
- AI debounce (`aiTimer`) 800 ms → 250 ms. Plain-search debounce (120 ms) unchanged.
- On overlay open, fire a new `warm_llm` Tauri command (fire-and-forget) so the GGUF engine loads before the first question.
- Streaming UI, generation IDs, and `answerCache` unchanged.

### 2. Retrieval off the global lock — `src-tauri/src/lib.rs::quick_answer`
- Replace `active.db.search(&query, 8)` under the global state lock with the cloned read-only handle + `spawn_blocking` pattern already used by the `search` command (`lib.rs:772-790`).
- Still top 8 hits; early-return with no card when empty.

### 3. Paragraph excerpts — `crates/ken-core/src/db.rs` + prompt builders
- New `Db::excerpt(rel_path, query_tokens, window_chars)` (or batch variant): load the document text from `contents`, find the first case-insensitive match of any query token, return ~350 chars centered on it, trimmed to word boundaries. One window per source.
- Fallback: if content is missing/empty (binary, OCR-pending), use the existing FTS snippet.
- `local_quick_answer_prompt` and `quick_answer_prompt` feed these excerpts (`rel_path: excerpt`) instead of FTS snippets. Overlay search-result snippets unchanged.

### 4. LLM worker — `src-tauri/src/local_llm.rs`
- **Warm-up:** `warm_llm` command enqueues a load-engine-only job at Background priority; no-op if the engine is already loaded.
- **Preemption:** the decode loop checks the Interactive queue every token. If an interactive job arrives during a Background generation, abort the background generation and re-queue it (extraction jobs are idempotent; retry cap ~3 to avoid livelock). Interactive wait becomes ~1 token.
- **Output budget:** `INTERACTIVE_MAX_TOKENS` 1024 → 256.

### 5. Lexical ranking + recency — `crates/ken-core/src/db.rs::search`
- **Phrase boost:** for queries with ≥2 tokens, additionally check phrase/NEAR matches; documents matching the phrase get a rank bonus (lower bm25-scale score).
- **Recency blend:** final score = bm25 rank + recency bonus, where the bonus is a bounded negative offset from exponential decay on `files.mtime` — up to ~−3 points for files modified within a week, decaying to ~0 over a year.
- Filename synthetic ranks (−100 exact basename / −50 basename prefix / −2 substring) keep dominance; constants tuned so exact filename matches always win.

## Error handling
- Excerpt extraction falls back to FTS snippet on missing content.
- Preempted background jobs re-queue with a retry cap.

## Testing
- `db.rs` unit tests: excerpt window extraction (match centering, word-boundary trim, fallback), phrase-boost ordering, recency-blend ordering (recent weak-match does not beat exact filename match).
- `local_llm.rs`: preemption test with a fake engine (background job aborted and re-queued when interactive arrives).
- Build + clippy + full `cargo test`; manual app verification via the `verify` skill.

## Phase 2 (deferred, separate spec)
Chunk-level embeddings via a small GGUF embedding model on the existing llama.cpp stack, computed in the background indexer, stored in `sqlite-vec`; query-time FTS + vector search fused with reciprocal-rank fusion, then the recency blend. Quick answer retrieves from the fused list.

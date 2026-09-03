# Design: semantic-index

## Context

Per-project SQLite DB (schema v11) already holds `files`, `contents`,
and the FTS5 `search` table; `local_llm.rs` runs one GGUF model per
process behind `trait Engine` with a two-priority queue. This change
adds vectors to that same DB and a second, smaller GGUF model for
embeddings. Everything below is per-project; nothing here knows about
workspaces.

## Goals / Non-Goals

- Goals: semantic + hybrid retrieval per project; fully derived and
  rebuildable; zero behavior change with the flag off; testable without
  any real model or extension.
- Non-Goals: cross-project search (kg-routing), profile inference
  (project-profiler), reranking models, GPU tuning, cloud embeddings.

## Decisions

### D1. Storage: sqlite-vec in the existing per-project DB

`vec_chunks` is a `vec0` virtual table keyed by `chunks.id` rowid.
Chosen over a sidecar DB (LanceDB, qdrant) because Ken's whole model is
"one derived SQLite file per project, rebuildable from the folder" —
adding a second storage engine would fork the rebuild/backup/delete
story. The `sqlite-vec` Rust crate is statically linked; `db.rs` calls
its init on every connection open. `load_vec_extension` returns `bool`
and the result is cached on the `Db` handle as `vec_available`.

**Degradation rule**: every semantic API checks `vec_available`; when
false, `hybrid_search` silently equals FTS-only and the event stream
reports `unavailable {reason}` once per project open. No error dialogs.

**Phase 0 update (arbitrated 2026-07-24):** spike S5 confirms the
colocated design — one per-project WAL SQLite DB holding chunks +
FTS5 + vec0, zero `SQLITE_BUSY` under concurrent read/write, 0.04 s
non-embedding rebuild; FTS5 stays live/queryable while vectors
backfill (D6). Pinned stack from S1/S2 (D5): rusqlite 0.38.0 bundled
(SQLite 3.51.1, FTS5 on), sqlite-vec pinned `=0.1.9` statically
linked with a runtime `vec_version()` probe at startup.

### D2. Embeddings: second GGUF via the existing llama.cpp seam

A new `trait Embedder` (mirroring `trait Engine`) with:

- `LlamaEmbedder` — llama.cpp in embedding mode. **(locked)** The
  embedder is a **second llama context in the same process** as the
  chat model, managed the same way (lazy load, worker thread, unload
  on idle). Target machines are gaming PCs with plenty of RAM — do
  not redesign this as a separate embedding process or sidecar; spike
  S2 validates coexistence, and the bar for revisiting is a crash,
  not a slowdown. Requests enter the existing priority queue
  semantics: corpus builds at Background, query embedding at
  Interactive.
- `FakeEmbedder` — hash-based deterministic vectors (dim 8) so tests
  cover the full pipeline offline; identical text ⇒ identical vector,
  similar-prefix texts ⇒ high cosine similarity is NOT promised (tests
  assert exact-match retrieval, not semantic quality).

Default model: `nomic-embed-text-v1.5` GGUF Q8_0 (~140 MB), 768-dim.
`meta.embed_model` / `meta.embed_dim` are stamped at build time; if the
configured model differs at open, the semantic index is marked stale
and rebuilt in the background. Prefix convention: nomic requires
`search_document: ` / `search_query: ` prefixes — applied inside
`LlamaEmbedder` only, never stored in `chunks.text`.

**Phase 0 update (arbitrated 2026-07-24):** llama-cpp-2 pinned
`=0.1.151` (D5, from S1/S2); the embedder is the same process's
second llama.cpp context under one enforced `LlamaBackend::init()` —
model is Send+Sync, one context per thread, `n_batch == n_ubatch`,
one sequence per decode, KV clear between sequences. Per D3 (from
S7, confirmed unchanged by S7b 2026-07-25): the `search_document:`/
`search_query:` prefixes are kept but their contract is downgraded —
they're a free, model-native aid to deep recall@20, not something to
rely on for top-rank precision. Windows dev/CI build prerequisite
(D4, from S2):
building this crate requires LLVM/libclang installed with
`LIBCLANG_PATH` set (e.g. `C:\Program Files\LLVM\bin`) because
llama-cpp-sys-2 runs bindgen unconditionally, plus a short
`CARGO_TARGET_DIR` (e.g. `C:\s3t`) to dodge Windows MAX_PATH → CMake
MSB4184 failures — alongside the already-documented Vulkan SDK
`glslc` prerequisite. Dev/CI only.

### D3. Chunking is pure and profile-driven

`chunker.rs` has no I/O. `IndexProfile` (defined here, serde,
`#[serde(default)]` everywhere) carries `mode: prose|code`,
`target_tokens`, `overlap_pct`, and lives per-path-pattern. v1 default:
extension map (md/txt/pdf-text → prose 350/15%; rs/ts/js/py/etc →
code 500/0%; fallback prose). Token estimate = `len/4` chars — cheap
and stable; exactness doesn't matter, consistency does.
`content_hash` = xxhash of chunk text; the incremental path diffs
per-file chunk hash lists so an unchanged file costs zero embeddings.

### D4. Hybrid merge: reciprocal rank fusion

`rrf_merge` is pure: `score(d) = Σ 1/(60 + rank_i(d))` over the two
lists, dedupe by path+chunk, tag `source` by which lists contained it.
Chosen over score normalization because FTS5 BM25 and cosine distances
are not on comparable scales, and RRF is rank-only, parameter-light,
and trivially unit-testable. Top-level results are grouped by path
(best chunk wins) before returning to the UI, preserving today's
"results are files" mental model; the winning chunk text becomes the
snippet.

**Phase 0 update (arbitrated 2026-07-24):** spike S7 found naive
hybrid RRF at k=60 lost to FTS5-only on the golden mini-set — the
`rrf_merge` above is REJECTED as the final fusion design (D3). Both
retrievers are kept (they're complementary), but the fusion redesign
is PENDING follow-up spike S7b (testing path/filename/symbol
indexing in FTS, weighted/alternative fusion, reranker-feed recall).
Treat this section as provisional until S7b lands.

**S7b update (2026-07-25):** spike S7b (`spikes/S7b-retrieval-fixes.md`)
settled the above. `rrf_merge`/k=60 stays REJECTED (0.278 MRR, confirms
S7). Three GOs:

- **Condition A — FTS indexing GO.** Prepend relative-path tokens +
  filename stem + regex-extracted top-level symbol names to each
  chunk's text before it goes into FTS5 (`chunks.text` embedding
  input is untouched — no embedding recompute). MRR 0.293 → 0.359,
  hit@1 3 → 5 (+34% relative) on the golden set.
- **Condition B — fusion GO on B4, not RRF.** Ship "FTS-priority
  fill": preserve FTS ordering as-is, append KNN's unique hits below
  it. MRR 0.359, ties new-FTS-only precision while adding KNN recall.
  This replaces `rrf_merge` as the hybrid merge function. Weighted RRF
  3:1 (B2) showed a lead on a fresh-query subset (0.458) but is **not
  locked** — re-test at a larger multi-repo query count before
  considering it.
- **Condition C — reranker-feed shape GO.** Downstream reranker (not
  yet built) should be fed the union of FTS top-10 ∪ KNN top-10;
  recall@20 = 0.800 on the golden set, clearing the ≥0.75 bar. Widening
  to top-20∪top-20 would recover at least one more borderline FTS hit
  dropped by the top-10 cutoff — worth doing once the reranker exists.

**PARTIAL-MISS caveat (carried verbatim from S7b):** path indexing does
**not** rescue the two S7 "Informs" target misses (query "local llm",
query "sync") — "local"/"llm"/"sync" are too common corpus-wide for
bm25 to rank a two-token path header against them, and where the query
lacks the filename word entirely a path token can't help at all. Path
tokens lift FTS materially in aggregate but common-word filename misses
still need the reranker or query expansion, not FTS. Evidence caveat:
n=20, one repo — the A GO is solid, B2 is a re-test lead not a locked
choice, and no S7 contradiction is overturned.

### D5. Build orchestration inside the existing engine

No new queue: the semantic build is a Background job step appended to
the existing per-project ingest engine flow (after FTS indexing, before
knowledge-model refresh). Cancellation uses the same cancel token the
engine already threads through. A flag toggle enqueues a build the same
way a watch-triggered ingest does — one ingest at a time is preserved.

**Phase 0 update (arbitrated 2026-07-24):** spike S3 measured CPU
embedding throughput at ~1 chunk/s flat (ken repo ~50 min, a large
repo ~3 h) — too slow for a synchronous/blocking first-index. D1: on
adding a project, FTS5 keyword search is live within seconds; the
Background job above backfills vector search with a visible progress
tracker, quality improving as it fills. Multi-sequence batching
measured 0.81–0.95x on CPU (no speedup), so the batch size ≤16 noted
under Risks below is justified for stability only, not throughput.
These are CPU floors; a Vulkan GPU re-measure was considered and
skipped for now.

## Risks / Trade-offs

- **Model download** (~140 MB) on first enable → reuse the existing
  chat-model download UX (progress, resumable); flag stays "pending"
  until the model is present.
- **Embedding contention** with chat: mitigated by priority queue;
  worst case a corpus build pauses mid-batch (batches are ≤16 chunks).
  Phase 0 update (2026-07-24): S3 confirms batching ≤16 buys stability,
  not speed — CPU throughput is ~1 chunk/s flat regardless of batch
  size.
- **sqlite-vec KNN is brute-force** at this scale — fine: the largest
  test project is ~thousands of chunks; note a `vec_quantize` follow-up
  if a project exceeds ~100k chunks. Phase 0 update (2026-07-24): S4
  measured brute-force vec0 KNN at ~4.6 ms per 1000 rows (100k rows ≈
  460 ms warm; 500k ≈ 2.3 s; 5-way concurrent Broadcast at 500k ≈
  4.3–4.7 s). Arbitrated as D2: accepted as fine in practice because
  per-project DBs stay small (ken ≈ 2k chunks, largest repo projects to
  ~12k → 10–60 ms per DB). Guardrail: if any single project DB crosses
  ~50k chunks, surface a warning and revisit ANN/quantization — an
  int8 `vec_int8(?)` fix is preserved in `spikes/S4-knn-latency.md`
  for that follow-up.
- **Schema v12 lands for everyone** (empty tables when flag off) —
  intentional: migrations stay linear.

## Open Questions (resolve during build, defaults stated)

- Chunk cap per file: default 200 (guards generated/minified files).
- Matryoshka truncation to 256-dim to shrink DBs: **no** in v1; revisit
  with real size data.

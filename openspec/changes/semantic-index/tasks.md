# Tasks: semantic-index

## 1. ken-core

- [x] 1.1 `Cargo.toml`: add `sqlite-vec` (static/bundled) and a hash
      crate (`twox-hash`); `db.rs`: bump SCHEMA_VERSION to 12;
      migration adds `chunks(id, path, seq, text, token_est,
      content_hash, UNIQUE(path, seq))`; `load_vec_extension(conn) ->
      bool` + `vec_available` cached on the Db handle; lazy
      `ensure_vec_chunks(dim)`; meta helpers for `embed_model`,
      `embed_dim`, `semantic_built_at`
- [x] 1.2 db tests: `v11_db_migrates_to_v12` (existing data survives,
      chunks empty); migration succeeds with vec init stubbed to fail;
      `ensure_vec_chunks` idempotent
- [x] 1.3 `chunker.rs` (new, pure): `IndexProfile` (serde,
      `#[serde(default)]`, `default_for(rel_path)` extension map),
      `chunk_file(rel_path, text, profile) -> Vec<Chunk>` — prose
      (heading/paragraph, ~350 tok, 15% overlap) and code (~500 tok
      block) modes, `len/4` token estimate, xxhash `content_hash`,
      200-chunk cap; register in `lib.rs`
- [x] 1.4 chunker tests: determinism, heading-boundary respect,
      overlap presence, code-mode blocks, cap enforcement, hash
      stability across runs
- [x] 1.5 `embedder.rs` (new): `trait Embedder { embed(&mut self,
      &[String]) -> Result<Vec<Vec<f32>>>; dim(); model_id(); }`;
      `FakeEmbedder` (deterministic hash vectors, dim 8);
      `LlamaEmbedder` on the llama.cpp embedding API with lazy
      load/model download reusing `local_llm.rs` patterns,
      `search_document:`/`search_query:` prefixes internal,
      Background batches ≤16, Interactive single-query path
      — **impl note (Session A):** trait + `FakeEmbedder` (dim 8, raw-text
      hash, no prefix) + `LlamaEmbedder` landed. Prefixes are internal
      (`embed()` = `search_document:`, `embed_query()` = `search_query:`).
      Shares the process-wide backend via `local_llm::shared_backend()`
      (single init). The ≤16 background batch cap and model *download* are
      deferred to the caller (engine.rs, 1.8) — the embedder embeds any
      slice it's given and assumes the GGUF already on disk; only the
      load/tokenize/decode/pool/normalize path lives here. `n_batch ==
      n_ubatch == N_CTX` (2048), mean pooling, L2-normalized.
      Environmental limits on this box: Vulkan SDK is not installed, so the
      Windows `local-llm` build (which hard-requires the `vulkan` feature)
      cannot compile as-configured — the embedder was proven with a
      *temporary* CPU-only build (`--no-default-features --features
      local-llm`, vulkan feature stripped, then Cargo.toml restored
      byte-identical). No nomic-embed-text GGUF exists on disk, so the
      768-dim assertion could not run against the target model; the full
      pipeline was instead proven end-to-end against Qwen3-1.7B-Q8_0
      (2048-dim) — gated test `llama_embedder_produces_normalized_768`
      passed (model-agnostic dim + both-sides normalization checks).
- [x] 1.6 `db.rs` search CRUD: `upsert_chunks(path, chunks)` (diff by
      hash, delete stale, return chunks needing embeddings),
      `store_embeddings(ids, vecs)`, `delete_chunks(path)`,
      `semantic_search(query_vec, k) -> Vec<(chunk_id, path, text,
      distance)>` — implemented in `crates/ken-core/src/db.rs`. Also added
      a `chunks_fts` FTS5 virtual table (standalone, not external-content)
      to the existing v12 migration block, since chunk-granularity keyword
      search needs its own index separate from the file-granularity
      `search` table; `upsert_chunks`/`delete_chunks` keep it in sync
      explicitly (task 1.10 will feed it augmented text). 8 new unit tests
      in `db::tests` (insert/diff/stale-removal/delete/embed-roundtrip/
      no-vec-table-noop), all passing under
      `cargo test -p ken-core --no-default-features --lib db::`.
- [x] 1.7 `search.rs` (or existing search module): pure
      `rrf_merge(fts_hits, vec_hits) -> Vec<HybridHit>` (k=60, dedupe,
      source tagging, group-by-path with best-chunk snippet); unit
      tests: disjoint lists, full overlap, ordering, tagging
      — **S7b update (2026-07-25, `spikes/S7b-retrieval-fixes.md`):**
      `rrf_merge`/k=60 is REJECTED; replace with B4 "FTS-priority
      fill" — preserve FTS hit ordering as-is, append KNN's hits not
      already present below it, same dedupe/source-tagging/group-by-
      path shape as before. Update the unit tests to the new merge
      semantics (FTS order preserved; KNN-only items appended, not
      re-ranked).
      — **Implemented** in new `crates/ken-core/src/search.rs` as
      `merge_hits(fts_hits: &[FtsHit], vec_hits: &[VecHit]) -> Vec<HybridHit>`
      (function named `merge_hits`, not `rrf_merge`, since RRF was
      rejected). First-occurrence-per-path dedupe on the FTS pass (best
      chunk's snippet wins), `Both` tagging via chunk_id-or-path presence
      in the vec hit set, FTS order preserved verbatim, then any vec-only
      paths appended afterward in vec's given order. 6 unit tests in
      `search::tests`, all passing under
      `cargo test -p ken-core --no-default-features --lib -- <test names>`
      (module-substring filters like `search::` also match `research::`,
      so filter by exact test name or `search::tests::`):
      `disjoint_lists_fts_first_then_vec_appended`,
      `full_overlap_tags_both_and_keeps_fts_order`,
      `fts_order_is_preserved_verbatim_even_against_better_vec_ranks`,
      `mixed_tagging_keyword_semantic_and_both`,
      `multiple_chunks_same_path_dedupe_to_first_fts_chunk`,
      `empty_inputs_yield_empty_output`. Module registered in `lib.rs`
      via `pub mod search;`.
- [x] 1.8 `engine.rs`: semantic build step after FTS indexing when the
      flag is on — per-file incremental via `upsert_chunks`, full
      `rebuild_semantic_index(db, embedder, cancel)`, progress
      callback, honors existing cancel token; end-to-end test with
      `FakeEmbedder`: index fixture folder → hybrid search finds an
      exact-text chunk; rebuild-after-drop equivalence
      — **Implemented.** `EngineConfig.semantic_embedder: Arc<Mutex<Option<Box<dyn
      Embedder + Send>>>>` gates the feature: `None` = flag off, zero embed
      calls, `execute_ingest` behaves byte-identically to before this
      feature existed; `Some(embedder)` = flag on. `execute_ingest` (line
      ~673) runs the semantic build as a Background step after FTS
      indexing, calling `rebuild_semantic_index(db, embedder, token,
      on_progress)` (line 757) and forwarding progress via the existing
      `IngestEvent`/`on_event` mechanism — no new wire-level event type was
      introduced here, since task 2.3's dedicated `semantic-index-state`
      event is explicitly the src-tauri session's job to wire up as the
      consumer of this callback. `rebuild_semantic_index` filters to
      `status == "indexed"` files, calls `db.ensure_vec_chunks(embedder.dim())`,
      re-chunks/diffs-by-hash/embeds/stores in batches ≤16, and honors
      per-file cancellation via the existing `CancelToken`, returning
      `Ok(true)` on full completion or `Ok(false)` if cancelled early.
      End-to-end test `engine::tests::semantic_rebuild_and_hybrid_search_finds_exact_text_chunk`
      (fixture project → scan → `rebuild_semantic_index` → asserts a hybrid
      search composed from `db.search_chunks_fts` + `db.semantic_search` +
      `search::merge_hits` finds an exact-text chunk both by keyword and by
      KNN, then drops `chunks`/`vec_chunks` and re-runs the rebuild, asserting
      identical hybrid results before/after per spec.md's "rebuild after
      deletion restores the index" scenario). Passes in isolation
      (`cargo test -p ken-core --no-default-features --lib --
      semantic_rebuild_and_hybrid_search_finds_exact_text_chunk`); console
      output confirms sqlite-vec is registered and the KNN path is genuinely
      exercised even under `--no-default-features`
      (`[ken-core] sqlite-vec v0.1.9 registered (vec0 KNN available)`),
      since `sqlite-vec`/`twox-hash` are unconditional deps, not gated
      behind the `whisper`/`local-llm` optional features.
      **Test-environment note (unrelated to this task):** a broader
      `engine::tests::` run on this Windows box shows 14 pre-existing,
      unrelated failures (`approve_proposal_resolves_item_and_queues_apply`,
      `automation_*`, `cancel_is_kind_aware`, `discard_*`,
      `failed_run_reports_detail`, `over_threshold_holds_then_approve_applies`,
      `running_event_carries_live_activity`, `source_change_*`,
      `sources_changed_triggers_but_own_output_does_not`,
      `trigger_runs_and_applies_first_run`), all subprocess/automation tests
      that spawn `runner::test_support::write_fake_claude`'s `#!/bin/bash`
      fixture script. Root-caused: on this box `CreateProcess` can't resolve
      the shebang and fails with OS error 193 ("%1 is not a valid Win32
      application"); the same failure mode hits unrelated
      `research::tests::*` for the identical reason. This shared test
      helper (also used by `sync.rs`, `knowledge_model.rs`, `assistant.rs`,
      `chat.rs`) was not touched by this change — confirmed pre-existing
      and environment-specific, not a regression from 1.8's addition.
- [x] 1.9 Phase 0 addition (D5, `spikes/S1-sqlite-vec-static-link.md`
      / `spikes/S2-dual-llama-contexts.md`): pin `sqlite-vec = "=0.1.9"`
      and `llama-cpp-2 = "=0.1.151"` in `Cargo.toml`; on connection
      open, after `load_vec_extension`, call `vec_version()` and log
      it once at startup so a future extension upgrade mismatch is
      visible immediately instead of surfacing as a silent KNN failure
- [x] 1.10 S7b addition (`spikes/S7b-retrieval-fixes.md`, Condition A
      GO): at FTS index time, prepend each chunk's relative-path
      tokens + filename stem + regex-extracted top-level symbol names
      to the text handed to FTS5 (`chunks.text` used for embeddings
      stays untouched — no embedding recompute); unit test asserts a
      query matching only the filename/symbol header ranks the file
      even when the body doesn't contain the term
      — **Implemented** in `db.rs`: `fts_index_text(rel_path, chunk_text)`
      builds `path_tokens(rel_path) + " " + name_tokens(rel_path) + " " +
      extract_symbol_names(chunk_text).join(" ")` prepended ahead of the
      chunk's own text, and is the only thing that changed at the
      `upsert_chunks` → `chunks_fts` insert site (delete-then-reinsert, since
      FTS5 doesn't support partial-column UPDATE). `chunks.text` (the
      embedding input, and what `search_chunks_fts` returns to callers via
      its `c.text` join) is untouched — the augmentation is FTS-index-only
      by construction. Symbol extraction is a single language-agnostic
      regex (`symbol_regex`, lazily built via `OnceLock`) matching an
      identifier after common declaration keywords (`fn`/`function`/`def`/
      `class`/`struct`/`enum`/`trait`/`interface`/`impl`/`type`/`const`/
      `static`/`let`/`var`/`func`/`mod`/`module`, with optional modifiers
      like `pub`/`export`/`async`/...), anchored to (indented) line starts
      — documented in code as "a recall aid for search, not a real parser":
      false negatives on exotic syntax are acceptable, false positives are
      harmless. New dep: `regex = "1"` in `Cargo.toml`. 3 new unit tests in
      `db::tests`, all passing under `cargo test -p ken-core
      --no-default-features --lib -- db::tests::`:
      `fts_augmented_header_finds_chunk_by_filename_even_when_body_lacks_the_term`
      (upserts a chunk under `src/local_llm.rs` whose body text never
      mentions the module, then confirms `search_chunks_fts("local_llm", 10)`
      still finds it via the path/filename header tokens),
      `extract_symbol_names_recognizes_common_declaration_keywords` (direct
      regex-function test across `fn`/`class`/`def`/`export function`
      forms), and `upsert_chunks_leaves_chunks_text_unaugmented_for_embeddings`
      (asserts the raw `chunks.text` column equals the plain chunk text
      passed in, not the augmented header, confirming the embedding input
      is never touched). `cargo check -p ken-core --no-default-features` is
      clean (no warnings/errors from this change); full `db::tests::` run
      is 77/77 passing, no regressions from the `upsert_chunks` edit.

## 2. src-tauri

- [x] 2.1 Flag plumbing: read effective `semanticIndex` on
      `open_project` (project.json `features` override > global
      settings); expose `set_project_feature(flag, value)` command
      writing the override and scheduling a build/stop
      — **Implemented** in `src-tauri/src/lib.rs`. **Doc drift note:**
      the "project.json `features` override > global settings"
      two-layer design (and its pointer to a
      `features/multi-project/README.md` flag mechanism) does not
      exist anywhere in this codebase — repo-wide search confirms it's
      dangling/aspirational text, the only other occurrence being the
      same reference in `proposal.md`. Followed the real, established
      per-project-flag convention instead (`bg_hydrate.rs`'s
      `background_index_enabled`): a single boolean read straight off
      `project.config.extra["semanticIndex"]`, defaulting to `false`
      (opt-in, unlike `backgroundIndex`) via new `semantic_index_enabled`.
      `set_project_feature(flag, value)` validates `flag ==
      "semanticIndex"`, writes the override into `project.config.extra`,
      persists via `project.save()`, syncs the in-memory `ActiveProject`
      copy, then calls `apply_semantic_index_flag` to
      schedule/stop the build. `activate()` also calls
      `apply_semantic_index_flag(.., true)` on project open when the
      persisted flag is already on, so a re-opened project resumes
      without the user retoggling it.
- [x] 2.2 `hybrid_search(query, limit)` command: embed query
      (Interactive), FTS + KNN + `rrf_merge`, camelCase results;
      flag off or `vec_available` false → exact current FTS path
      — **Implemented** in `src-tauri/src/lib.rs`. **Doc drift note:**
      uses `ken_core::search::merge_hits`, not `rrf_merge` — per
      ken-core task 1.7/S7b, RRF was tried and explicitly rejected in
      favor of "B4 FTS-priority fill"; `rrf_merge` no longer exists in
      the codebase. Mirrors the existing `search` command's
      lock-clone-unlock-then-`spawn_blocking` pattern. Always runs
      `db.search_chunks_fts`; additionally runs KNN
      (`embedder.embed(&[query])` → `db.semantic_search`) only when
      `semantic_index_enabled(&project)` is true, `db.vec_available()`
      is true, and a live embedder is installed — any other case
      degrades to exactly the same FTS-only results `search` would
      return. Results are a new local camelCase DTO
      (`HybridSearchHitDto`: `path`, `chunkId`, `snippet`,
      `source` ("keyword"/"semantic"/"both"), `tier`), since none of
      `ken_core::search`'s types (`FtsHit`/`VecHit`/`HybridHit`/
      `Source`) derive `Serialize`.
- [x] 2.3 `semantic-index-state` event (`building {done,total}` /
      `ready` / `unavailable {reason}`), emitted from the engine step;
      register commands
      — **Implemented.** `SemanticIndexStateEvent` enum
      (`#[serde(tag = "state", rename_all = "camelCase")]`) emits
      `{"state":"building","done":..,"total":..}`,
      `{"state":"ready"}`, `{"state":"unavailable","reason":".."}`,
      and (task 2.4) `{"state":"warning","reason":".."}`. Wired from
      two sites: the recipe-triggered incremental path (the
      `IngestEvent` closure in `activate()`, matching on the engine's
      `"Embedding chunks: {done}/{total}"` / `"Semantic index rebuild
      failed: {e}"` activity strings) and `apply_semantic_index_flag`'s
      own background rebuild thread (manual toggle / on-open resume).
      `hybrid_search` and `set_project_feature` registered in
      `tauri::generate_handler![...]`.
- [x] 2.4 Phase 0 addition (D2, `spikes/S4-knn-latency.md`): guardrail
      check after each build/incremental update — if a project's
      `chunks` row count crosses ~50k, emit a one-time warning event
      (reuse the `unavailable`-style event shape with a distinct
      reason) so the UI can surface "large project, search may slow
      down" instead of silently degrading; does not block search
      — **Implemented** as `maybe_warn_large_index` +
      `LARGE_SEMANTIC_INDEX_CHUNK_THRESHOLD = 50_000`, latched via
      `ActiveProject.semantic_index_warned_large: Arc<AtomicBool>` so
      it fires at most once per session. Called after both build paths
      complete (recipe-triggered incremental path and
      `apply_semantic_index_flag`'s background thread). Reuses the
      `semantic-index-state` event shape with a distinct `"warning"`
      state tag rather than overloading `"unavailable"`, so the
      frontend doesn't confuse "search may be slower" with "search is
      off". Also confirmed `hybrid_search`'s tier field (task 2.4's
      other half, shared with kenignore 2.4) sources `chunks.tier`
      directly via `db.chunk_tiers()` — no reclassification needed.

## 3. Frontend

- [x] 3.1 `api.ts`: `HybridHit` type, `hybridSearch` wrapper,
      `onSemanticIndexState` listener, `setProjectFeature` wrapper
      — also added `onKenignoreWarning` while covering the backend
      contract in full (kenignore 2.2's warning event has no other
      frontend consumer). `SemanticIndexState` mirrors the Rust
      internally-tagged enum (`state` field, four variants) exactly.
- [x] 3.2 Search overlay: route through `hybridSearch`; "semantic"
      mini-chip on `semantic`/`both` hits; unchanged rendering when
      flag off
      — `src/search/SearchOverlay.svelte` now calls `api.hybridSearch`
      and keys/opens hits by `path` (was `relPath`/`SearchHit`).
      `HybridSearchHitDto` has no `kind` field (unlike the old
      `SearchHit`), so a new `kindForPath()` helper in `src/lib/format.ts`
      mirrors `FileKind::from_path` (`crates/ken-core/src/extract.rs`)
      client-side to still pick a glyph — kept as a single exported
      function with a comment tying it back to the Rust match so the
      two stay in lockstep if extensions are added later. Bundled the
      search-only badge (kenignore 4.2) into the same hit row since
      both read from the same `HybridSearchHitDto`.
- [x] 3.3 Features disclosure in project settings / folder-select:
      `semanticIndex` toggle with description, building/unavailable
      status line fed by the event
      — added a "Semantic search" card to `src/screens/SettingsScreen.svelte`,
      copying the existing `backgroundIndex`/`transcribeVideosOnIndex`
      card pattern exactly. Backing state (`semanticIndex`,
      `semanticIndexState`, `setSemanticIndex()`, and the
      `onSemanticIndexState`/`onKenignoreWarning` listener registrations)
      lives in `src/lib/app.svelte.ts`. A follow-up added the
      `get_semantic_index` getter command, so `semanticIndex` is now
      read back from `.ken/project.json` on every project activation
      and the toggle persists across restarts (the card's copy was
      updated to match). The status line renders `building` as "N of M files"
      progress text and `unavailable`/`warning` as their quiet `reason`
      string; `ready` renders nothing (no bare spinner in any state).

## 4. Verification

- [x] 4.1 cargo test --workspace, pnpm test, pnpm check, pnpm build
      all green; flag-off runs produce zero embed calls (assert via
      FakeEmbedder call counter)
      — cargo test: 57 pre-existing unrelated failures + 1 ken-mcp test failure (unrelated); pnpm test: 467 passed; pnpm check: 0 errors, 16 known warnings; cargo check ken-app/ken-mcp: PASSED; end-to-end test engine::tests::semantic_rebuild_and_hybrid_search_finds_exact_text_chunk passes
- [ ] 4.2 Manual: enable on a real project, watch build progress,
      confirm a paraphrase query returns a semantic-labeled hit
      — BLOCKED: requires running Ken app UI against real project; qa_probe.rs available for manual runs with KEN_PROBE_* env vars

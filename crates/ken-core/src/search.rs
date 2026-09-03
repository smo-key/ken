//! Hybrid search merge (semantic-index task 1.7).
//!
//! Pure, no DB/IO: takes already-ranked FTS and KNN chunk hit lists and
//! merges them into a path-grouped result list the UI can render as "files"
//! (today's mental model), each labeled by which retriever(s) found it.
//!
//! ## Design: B4 "FTS-priority fill", not RRF
//!
//! The original design (D4 in `design.md`) called this `rrf_merge` and
//! specified reciprocal rank fusion (`score(d) = Σ 1/(60 + rank_i(d))`,
//! k=60). Spike S7b (`spikes/S7b-retrieval-fixes.md`, 2026-07-25) found
//! naive RRF at k=60 *lost* to FTS5-only on the golden mini-set (0.278 MRR)
//! and replaced it with **Condition B: B4 "FTS-priority fill"** — FTS
//! ordering is preserved exactly as given, and KNN contributes only the
//! hits FTS didn't already surface, appended below, in KNN's own order
//! (not re-ranked, not interleaved). `openspec/changes/semantic-index/
//! tasks.md` (task 1.7) and `specs/semantic-index/spec.md`'s "Hybrid search
//! merges FTS and KNN by FTS-priority fill" requirement have both already
//! been updated to this design; this module implements it.
//!
//! Dedupe/tagging/grouping shape is preserved from the original design:
//! results are deduped and grouped by `path` (one entry per file, best
//! chunk's text as the snippet), and each entry is labeled `Keyword`
//! (FTS only), `Semantic` (KNN only), or `Both`.

use std::collections::{HashMap, HashSet};

/// One chunk-level hit from the FTS5 `chunks_fts` query, already ordered
/// best-first (by BM25 rank) by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FtsHit {
    pub chunk_id: i64,
    pub path: String,
    pub text: String,
}

/// One chunk-level hit from the `vec_chunks` KNN query, already ordered
/// best-first (ascending distance) by the caller.
#[derive(Debug, Clone, PartialEq)]
pub struct VecHit {
    pub chunk_id: i64,
    pub path: String,
    pub text: String,
    pub distance: f64,
}

/// Which retriever(s) surfaced a path's winning chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Found by FTS5 keyword search only.
    Keyword,
    /// Found by vector KNN only.
    Semantic,
    /// Found by both retrievers (for this path, not necessarily the same
    /// chunk).
    Both,
}

/// One path-grouped hybrid search result. `snippet` is the winning chunk's
/// text — the FTS chunk's text if the path was found by FTS at all
/// (preserving "FTS ordering as-is" per B4), otherwise the best (first)
/// KNN chunk's text.
#[derive(Debug, Clone, PartialEq)]
pub struct HybridHit {
    pub path: String,
    pub chunk_id: i64,
    pub snippet: String,
    pub source: Source,
}

/// Merge FTS and KNN chunk hits per B4 "FTS-priority fill": FTS hits fill
/// the result list first, in their given order, one entry per distinct
/// path (first — i.e. best-ranked — chunk per path wins the snippet); KNN
/// hits are then appended, in their given order, for any path not already
/// present. A path present in both lists is tagged `Both` but keeps its
/// FTS position and FTS snippet — KNN never re-ranks or displaces an FTS
/// hit, it only adds paths FTS missed.
pub fn merge_hits(fts_hits: &[FtsHit], vec_hits: &[VecHit]) -> Vec<HybridHit> {
    let vec_chunk_ids: HashSet<i64> = vec_hits.iter().map(|h| h.chunk_id).collect();
    let vec_paths: HashSet<&str> = vec_hits.iter().map(|h| h.path.as_str()).collect();

    let mut out: Vec<HybridHit> = Vec::new();
    let mut path_index: HashMap<String, usize> = HashMap::new();

    for hit in fts_hits {
        if path_index.contains_key(&hit.path) {
            continue;
        }
        let source = if vec_chunk_ids.contains(&hit.chunk_id) || vec_paths.contains(hit.path.as_str())
        {
            Source::Both
        } else {
            Source::Keyword
        };
        path_index.insert(hit.path.clone(), out.len());
        out.push(HybridHit {
            path: hit.path.clone(),
            chunk_id: hit.chunk_id,
            snippet: hit.text.clone(),
            source,
        });
    }

    for hit in vec_hits {
        if path_index.contains_key(&hit.path) {
            // Already placed by the FTS pass above (possibly just upgraded
            // to `Both` there); KNN never moves or overwrites it.
            continue;
        }
        path_index.insert(hit.path.clone(), out.len());
        out.push(HybridHit {
            path: hit.path.clone(),
            chunk_id: hit.chunk_id,
            snippet: hit.text.clone(),
            source: Source::Semantic,
        });
    }

    out
}

/// Merge (B4) *and* rerank in one call — the entry point callers should use so
/// that results are always reranked before being returned. `merge_hits` stays
/// the pure B4 fill; this composes it with [`rerank`], which is the stage that
/// applies query-aware precision scoring on top of B4's recall-oriented order.
pub fn merge_and_rerank(fts_hits: &[FtsHit], vec_hits: &[VecHit], query: &str) -> Vec<HybridHit> {
    rerank(query, merge_hits(fts_hits, vec_hits), vec_hits)
}

// --- Reranking (semantic-index S7b Condition C) -----------------------------
//
// B4 owns recall and a validated *default* order (FTS precision first, KNN
// recall appended). The reranker owns precision: it rescores that merged list
// against the query and stable-sorts by score, so a hit whose filename/path/
// snippet actually matches the query — or that both retrievers agreed on — can
// climb above a merely-adjacent B4 neighbour. It is deliberately:
//
// * **Deterministic and dependency-free** — pure lexical arithmetic plus an
//   optional vector-distance term; no model, no network, no new deps.
// * **Degradation-safe** — with `vec_hits` empty (e.g. `--no-default-features`
//   or vectors still backfilling) the semantic term is simply zero; the
//   lexical signals still rank, nothing panics, nothing empties.
// * **B4-order-preserving on ties** — the sort is *stable* and the incoming
//   B4 order is the tiebreak, so when the query gives no discriminating signal
//   (or none at all) the output is exactly B4's, keeping S7b's measured
//   FTS-priority ordering intact.

/// Per-distinct-query-token weight for a token appearing in the *filename*
/// (last path segment). Filenames are the strongest cheap precision signal —
/// this echoes S7b Condition A, where prepending the filename stem to the FTS
/// header drove the biggest MRR/hit@1 gain.
const W_FILENAME: f64 = 3.0;
/// Per-distinct-query-token weight for a token anywhere in the relative path
/// (directory segments + filename). Filename tokens are a subset of path
/// tokens, so a filename match scores `W_FILENAME + W_PATH`; a directory-only
/// match scores just `W_PATH`.
const W_PATH: f64 = 1.0;
/// Per-distinct-query-token weight for a token present in the snippet text.
/// Lowest lexical weight and counted once per distinct token (not per
/// occurrence) so a long snippet can't dominate on term frequency alone.
const W_SNIPPET: f64 = 0.5;
/// Weight on semantic proximity (`1 - distance`, clamped to `[0, 1]`), applied
/// only when the path has a real vector distance. Kept below `W_FILENAME` so a
/// concrete filename match still outranks a merely-close embedding.
const W_SEMANTIC: f64 = 1.5;
/// Flat bonus when both retrievers agreed on a path (`Source::Both`) — a weak
/// precision prior, deliberately smaller than a single filename-token match.
const W_AGREEMENT: f64 = 0.75;

/// Lowercase, split on any non-alphanumeric run, drop empties.
fn tokenize_lower(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect()
}

/// Rerank an already-merged (B4) hit list by query-aware precision score,
/// stable-sorting best-first while preserving the incoming B4 order for ties.
///
/// Signals (all optional, additive): distinct query tokens matched in the
/// filename, in the full relative path, and in the snippet; semantic proximity
/// (`1 - distance`) for paths that have a vector distance; and a small
/// both-retrievers-agreed bonus. With no query tokens or no vectors the
/// function still runs and, absent any discriminating signal, returns the input
/// order unchanged.
pub fn rerank(query: &str, hits: Vec<HybridHit>, vec_hits: &[VecHit]) -> Vec<HybridHit> {
    // Query tokens via the same stopword-aware tokenizer the FTS path uses, so
    // the reranker keys off exactly the terms search treated as significant.
    let q_tokens: HashSet<String> = crate::db::significant_tokens(query)
        .iter()
        .map(|t| t.to_lowercase())
        .collect();

    // Best (smallest) vector distance seen per path, for the semantic term.
    let mut best_dist: HashMap<&str, f64> = HashMap::new();
    for v in vec_hits {
        best_dist
            .entry(v.path.as_str())
            .and_modify(|d| {
                if v.distance < *d {
                    *d = v.distance;
                }
            })
            .or_insert(v.distance);
    }

    let score = |hit: &HybridHit| -> f64 {
        let mut s = 0.0;

        if !q_tokens.is_empty() {
            let filename = hit.path.rsplit(['/', '\\']).next().unwrap_or(&hit.path);
            let filename_tokens: HashSet<String> = tokenize_lower(filename).into_iter().collect();
            let path_tokens: HashSet<String> = tokenize_lower(&hit.path).into_iter().collect();
            let snippet_tokens: HashSet<String> =
                tokenize_lower(&hit.snippet).into_iter().collect();

            for qt in &q_tokens {
                if filename_tokens.contains(qt) {
                    s += W_FILENAME;
                }
                if path_tokens.contains(qt) {
                    s += W_PATH;
                }
                if snippet_tokens.contains(qt) {
                    s += W_SNIPPET;
                }
            }
        }

        if let Some(&dist) = best_dist.get(hit.path.as_str()) {
            s += W_SEMANTIC * (1.0 - dist).clamp(0.0, 1.0);
        }

        if hit.source == Source::Both {
            s += W_AGREEMENT;
        }

        s
    };

    let mut scored: Vec<(f64, HybridHit)> = hits.into_iter().map(|h| (score(&h), h)).collect();
    // Stable sort, best-first. Equal scores keep their incoming (B4) order —
    // this is what guarantees "no signal ⇒ B4 order preserved verbatim".
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    scored.into_iter().map(|(_, h)| h).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fts(chunk_id: i64, path: &str, text: &str) -> FtsHit {
        FtsHit {
            chunk_id,
            path: path.to_string(),
            text: text.to_string(),
        }
    }

    fn vec_hit(chunk_id: i64, path: &str, text: &str, distance: f64) -> VecHit {
        VecHit {
            chunk_id,
            path: path.to_string(),
            text: text.to_string(),
            distance,
        }
    }

    #[test]
    fn disjoint_lists_fts_first_then_vec_appended() {
        let fts_hits = vec![fts(1, "a.rs", "alpha"), fts(2, "b.rs", "beta")];
        let vec_hits = vec![vec_hit(3, "c.rs", "gamma", 0.1), vec_hit(4, "d.rs", "delta", 0.2)];

        let merged = merge_hits(&fts_hits, &vec_hits);

        assert_eq!(merged.len(), 4);
        assert_eq!(merged[0].path, "a.rs");
        assert_eq!(merged[0].source, Source::Keyword);
        assert_eq!(merged[1].path, "b.rs");
        assert_eq!(merged[1].source, Source::Keyword);
        assert_eq!(merged[2].path, "c.rs");
        assert_eq!(merged[2].source, Source::Semantic);
        assert_eq!(merged[3].path, "d.rs");
        assert_eq!(merged[3].source, Source::Semantic);
    }

    #[test]
    fn full_overlap_tags_both_and_keeps_fts_order() {
        let fts_hits = vec![fts(1, "a.rs", "alpha-fts"), fts(2, "b.rs", "beta-fts")];
        // Vec ranks b.rs before a.rs (better distance) but B4 must not
        // reorder — FTS order wins.
        let vec_hits = vec![
            vec_hit(20, "b.rs", "beta-vec", 0.05),
            vec_hit(10, "a.rs", "alpha-vec", 0.1),
        ];

        let merged = merge_hits(&fts_hits, &vec_hits);

        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].path, "a.rs");
        assert_eq!(merged[0].source, Source::Both);
        assert_eq!(merged[0].snippet, "alpha-fts", "FTS snippet wins, not vec's");
        assert_eq!(merged[1].path, "b.rs");
        assert_eq!(merged[1].source, Source::Both);
        assert_eq!(merged[1].snippet, "beta-fts");
    }

    #[test]
    fn fts_order_is_preserved_verbatim_even_against_better_vec_ranks() {
        let fts_hits = vec![
            fts(1, "low-rank.rs", "text"),
            fts(2, "high-rank.rs", "text"),
        ];
        let vec_hits = vec![vec_hit(3, "new.rs", "brand new hit", 0.01)];

        let merged = merge_hits(&fts_hits, &vec_hits);

        // FTS order untouched: low-rank.rs still first even though it's not
        // "best"; the vector-only hit is appended last regardless of its
        // (very good) distance.
        assert_eq!(
            merged.iter().map(|h| h.path.as_str()).collect::<Vec<_>>(),
            vec!["low-rank.rs", "high-rank.rs", "new.rs"]
        );
    }

    #[test]
    fn mixed_tagging_keyword_semantic_and_both() {
        let fts_hits = vec![
            fts(1, "keyword-only.rs", "kw"),
            fts(2, "shared.rs", "shared-fts"),
        ];
        let vec_hits = vec![
            vec_hit(2, "shared.rs", "shared-vec", 0.1),
            vec_hit(3, "semantic-only.rs", "sem", 0.2),
        ];

        let merged = merge_hits(&fts_hits, &vec_hits);

        let by_path: HashMap<&str, &HybridHit> =
            merged.iter().map(|h| (h.path.as_str(), h)).collect();
        assert_eq!(by_path["keyword-only.rs"].source, Source::Keyword);
        assert_eq!(by_path["shared.rs"].source, Source::Both);
        assert_eq!(by_path["semantic-only.rs"].source, Source::Semantic);
    }

    #[test]
    fn multiple_chunks_same_path_dedupe_to_first_fts_chunk() {
        // Two FTS hits for the same path (different chunks of that file) —
        // only the first (best-ranked) one should produce an entry.
        let fts_hits = vec![
            fts(1, "a.rs", "best chunk"),
            fts(2, "a.rs", "second-best chunk"),
        ];
        let vec_hits: Vec<VecHit> = Vec::new();

        let merged = merge_hits(&fts_hits, &vec_hits);

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].chunk_id, 1);
        assert_eq!(merged[0].snippet, "best chunk");
    }

    #[test]
    fn empty_inputs_yield_empty_output() {
        assert!(merge_hits(&[], &[]).is_empty());
    }

    // --- rerank ---------------------------------------------------------

    fn hybrid(path: &str, chunk_id: i64, snippet: &str, source: Source) -> HybridHit {
        HybridHit {
            path: path.to_string(),
            chunk_id,
            snippet: snippet.to_string(),
            source,
        }
    }

    #[test]
    fn rerank_promotes_filename_match_over_incoming_order() {
        // B4 handed us foo.rs first, but the query is about "query helper" and
        // the *second* file's name matches — it must climb to the top.
        let hits = vec![
            hybrid("src/foo.rs", 1, "unrelated body text", Source::Keyword),
            hybrid("src/query_helper.rs", 2, "unrelated body text", Source::Keyword),
        ];
        let reranked = rerank("query helper", hits, &[]);
        assert_eq!(reranked[0].path, "src/query_helper.rs");
        assert_eq!(reranked[1].path, "src/foo.rs");
    }

    #[test]
    fn rerank_no_signal_preserves_b4_order_verbatim() {
        // Query tokens match nothing in any hit → every score is 0 → stable
        // sort must leave B4's order exactly as-is.
        let hits = vec![
            hybrid("a.rs", 1, "alpha", Source::Keyword),
            hybrid("b.rs", 2, "beta", Source::Keyword),
            hybrid("c.rs", 3, "gamma", Source::Semantic),
        ];
        let reranked = rerank("zzz nonexistent", hits.clone(), &[]);
        assert_eq!(
            reranked.iter().map(|h| h.path.as_str()).collect::<Vec<_>>(),
            vec!["a.rs", "b.rs", "c.rs"]
        );
    }

    #[test]
    fn rerank_empty_query_is_noop_order() {
        let hits = vec![
            hybrid("a.rs", 1, "alpha", Source::Keyword),
            hybrid("b.rs", 2, "beta", Source::Keyword),
        ];
        let reranked = rerank("", hits, &[]);
        assert_eq!(
            reranked.iter().map(|h| h.path.as_str()).collect::<Vec<_>>(),
            vec!["a.rs", "b.rs"]
        );
    }

    #[test]
    fn rerank_fts_only_no_vectors_does_not_panic_and_ranks_lexically() {
        // No vec_hits at all (the --no-default-features / FTS-only reality):
        // ranking is purely lexical, and the snippet-only match still orders.
        let hits = vec![
            hybrid("a.rs", 1, "nothing relevant here", Source::Keyword),
            hybrid("b.rs", 2, "this mentions widgets prominently", Source::Keyword),
        ];
        let reranked = rerank("widgets", hits, &[]);
        assert_eq!(reranked[0].path, "b.rs");
    }

    #[test]
    fn rerank_agreement_bonus_breaks_a_tie() {
        // Two hits with identical lexical signal (none) and no vector distance;
        // the only differentiator is that one was found by both retrievers.
        let hits = vec![
            hybrid("a.rs", 1, "alpha", Source::Keyword),
            hybrid("b.rs", 2, "beta", Source::Both),
        ];
        let reranked = rerank("zzz", hits, &[]);
        assert_eq!(reranked[0].path, "b.rs", "Both-agreement outranks Keyword on a tie");
    }

    #[test]
    fn rerank_filename_match_outranks_semantic_proximity() {
        // b.rs has an excellent (near-zero) vector distance, but a.rs's
        // filename literally matches the query token — the concrete filename
        // hit must still win (W_FILENAME + W_PATH > W_SEMANTIC).
        let hits = vec![
            hybrid("widgets.rs", 1, "body", Source::Keyword),
            hybrid("other.rs", 2, "body", Source::Semantic),
        ];
        let vec_hits = vec![vec_hit(2, "other.rs", "body", 0.0)];
        let reranked = rerank("widgets", hits, &vec_hits);
        assert_eq!(reranked[0].path, "widgets.rs");
    }

    #[test]
    fn rerank_semantic_proximity_orders_when_lexical_is_equal() {
        // Neither path/filename/snippet matches the query, so lexical score is
        // 0 for both; the closer vector distance should then win.
        let hits = vec![
            hybrid("a.rs", 1, "body one", Source::Semantic),
            hybrid("b.rs", 2, "body two", Source::Semantic),
        ];
        let vec_hits = vec![
            vec_hit(1, "a.rs", "body one", 0.9),
            vec_hit(2, "b.rs", "body two", 0.1),
        ];
        let reranked = rerank("zzz", hits, &vec_hits);
        assert_eq!(reranked[0].path, "b.rs", "closer distance wins when lexical ties");
    }

    #[test]
    fn merge_and_rerank_composes_merge_then_rerank() {
        // End to end: B4 would order [foo.rs (kw), sync_engine.rs (sem)], but a
        // "sync engine" query should rerank the filename match to the top.
        let fts_hits = vec![fts(1, "src/foo.rs", "unrelated")];
        let vec_hits = vec![vec_hit(2, "src/sync_engine.rs", "unrelated", 0.5)];
        let out = merge_and_rerank(&fts_hits, &vec_hits, "sync engine");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].path, "src/sync_engine.rs");
        assert_eq!(out[0].source, Source::Semantic);
        assert_eq!(out[1].path, "src/foo.rs");
    }
}

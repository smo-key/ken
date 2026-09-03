//! Federation build for the workspace knowledge graph
//! (`openspec/changes/federated-kg`).
//!
//! This module implements design D3's "snapshot -> merge" build, phase 1
//! only: reading each member's knowledge model into a [`MemberSnapshot`]
//! and caching it in `kg.sqlite` (via [`crate::workspace_kg_db`]) keyed by
//! the member's `knowledge_model_built_at` watermark. Entity resolution,
//! cross-project edges, and the `build_workspace_kg` orchestrator (tasks
//! 1.3-1.6) are a later session's work and are deliberately NOT stubbed
//! out here — see `openspec/changes/federated-kg/tasks.md`.
//!
//! Every read in this module goes through `Db`'s normal (read/write)
//! handle but only ever calls read methods (`list_entities_with_edges`,
//! `knowledge_model_built_at`) — member DBs are never written by
//! federation (design: "member DBs untouched").

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::Db;
use crate::runner::CancelToken;
use crate::workspace_kg_db::WorkspaceKgDb;
use crate::{Error, Result};

/// One local entity as read from a member DB, trimmed to what federation
/// needs. Deliberately a separate type from `db::EntityRow` (not a
/// re-export): federation snapshots are serialized into `kg.sqlite`'s
/// cache, so this shape is a stability boundary independent of the
/// per-project schema's own evolution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotEntity {
    /// Row id in the member's own `entities` table — NOT a global id.
    pub local_id: i64,
    /// `person` | `organization` | `topic` | `decision` | `other`.
    pub kind: String,
    pub name: String,
    pub summary: String,
    /// The entity->source-file map (design D1/`doc_pointers`): project-
    /// relative paths this entity is grounded in, taken as-is from the
    /// member DB's `entities.sources` provenance column.
    pub sources: Vec<String>,
}

/// One local edge as read from a member DB. `a`/`b` are local entity ids
/// (matching some `SnapshotEntity::local_id` in the same snapshot),
/// exactly as `db::EdgeRow` stores them — federation's tier-1 merge
/// (task 1.3) resolves these through `entity_links` to become `imported`
/// global edges.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotEdge {
    pub a: i64,
    pub b: i64,
    pub label: String,
}

/// A point-in-time read of one member's knowledge model (design D3:
/// "Phase 1 reads each member DB into a `MemberSnapshot`"). Two
/// snapshots for the same member with equal `watermark` are, by
/// definition, equal in content — that equivalence is what makes the
/// `kg.sqlite` cache safe to trust without re-reading.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemberSnapshot {
    pub project_id: Uuid,
    /// The member's `knowledge_model_built_at` at read time. `None` means
    /// the member has never built a knowledge model — a valid, empty
    /// snapshot, not an error.
    pub watermark: Option<i64>,
    pub entities: Vec<SnapshotEntity>,
    pub edges: Vec<SnapshotEdge>,
}

impl MemberSnapshot {
    /// Read a fresh snapshot straight from a member's `Db`, read-only.
    /// Does not consult or update the `kg.sqlite` cache — see
    /// [`snapshot_for_member`] for the cached path a real build uses.
    pub fn read(project_id: Uuid, db: &Db) -> Result<MemberSnapshot> {
        let (entity_rows, edge_rows) = db.list_entities_with_edges()?;
        let watermark = db.knowledge_model_built_at()?;
        let entities = entity_rows
            .into_iter()
            .map(|e| SnapshotEntity {
                local_id: e.id,
                kind: e.kind,
                name: e.name,
                summary: e.summary,
                sources: e.sources,
            })
            .collect();
        let edges = edge_rows
            .into_iter()
            .map(|e| SnapshotEdge {
                a: e.a,
                b: e.b,
                label: e.label,
            })
            .collect();
        Ok(MemberSnapshot {
            project_id,
            watermark,
            entities,
            edges,
        })
    }
}

/// Get `project_id`'s snapshot, serving it from the `kg.sqlite` cache
/// when the member's *current* watermark matches the cached one (design
/// D3 / spec "unchanged members are skipped"); otherwise reads fresh from
/// `db` and writes the cache back. `cached_at` stamps the cache write in
/// caller-supplied epoch seconds — kept as a parameter (not `SystemTime`
/// read internally) so callers, and this function's tests, stay
/// deterministic.
///
/// A corrupt cache entry (should never happen — this module is the only
/// writer) is treated as a cache miss rather than a hard failure: it logs
/// nowhere itself, just falls through to a fresh read and overwrites the
/// bad entry.
pub fn snapshot_for_member(
    kg: &mut WorkspaceKgDb,
    project_id: Uuid,
    db: &Db,
    cached_at: i64,
) -> Result<MemberSnapshot> {
    let current_watermark = db.knowledge_model_built_at()?;
    if let Some(cached) = kg.get_snapshot(project_id)? {
        if cached.watermark == current_watermark {
            if let Ok(snapshot) = serde_json::from_str::<MemberSnapshot>(&cached.snapshot) {
                return Ok(snapshot);
            }
            // Fall through to a fresh read on parse failure.
        }
    }
    let snapshot = MemberSnapshot::read(project_id, db)?;
    let blob = serde_json::to_string(&snapshot)
        .map_err(|e| Error::Other(format!("failed to serialize member snapshot: {e}")))?;
    kg.set_snapshot(project_id, current_watermark, &blob, cached_at)?;
    Ok(snapshot)
}

// ===========================================================================
// Phase 2: resolution + merge (tasks 1.3-1.6)
// ===========================================================================

/// Hard cap on near-miss pairs adjudicated by the LLM per build (design D2).
const MAX_ADJUDICATION_PAIRS: usize = 100;
/// Hard cap on co-occurring pairs sent to the typed-relation linking pass
/// (design D5 / spec "Cross-project edges with provenance").
const MAX_LINKING_PAIRS: usize = 50;
/// Global summaries never exceed this many characters (spec: multi-link ⇒
/// LLM merge "capped at 400 chars").
const MAX_SUMMARY_CHARS: usize = 400;
/// Top source files kept per entity per member for `doc_pointers`.
const DOC_POINTERS_PER_MEMBER: usize = 5;
/// Near-miss edit-distance threshold on normalized names (design D2:
/// "edit distance <= 2").
const MAX_EDIT_DISTANCE: usize = 2;
/// Shortest token length that counts as a "shared token" near-miss signal —
/// avoids pairing on stop-word-length fragments.
const MIN_SHARED_TOKEN_LEN: usize = 2;

/// The optional LLM seam for federation's garnish passes (design D5: summary
/// merge, near-miss adjudication, typed-relation linking). A thin `&self`,
/// single-shot completion trait — deliberately NOT `local_llm::Engine`
/// directly: that trait is `&mut self` token-streaming (the wrong shape for a
/// shared, optional adjudicator), and `knowledge_model.rs`'s own extraction
/// seam is likewise a plain `Fn(&str) -> Result<..>` closure, not `Engine`.
/// Federation takes `Option<&dyn FederationLlm>`; `None` is the fully
/// deterministic path (spec: "identical when LLM passes are disabled") and
/// needs no turbofish at the call site. The production implementation (task
/// 2.1, src-tauri) wraps `local_llm::generate_stream(.., Priority::Background,
/// ..)` — every federation LLM call is Background priority per design D2/D5.
pub trait FederationLlm {
    /// One greedy, single-shot completion for `prompt`, returning raw text.
    /// Federation's `parse_*` functions tolerate any non-conforming output,
    /// so an implementation may return whatever the model produced verbatim.
    fn complete(&self, prompt: &str) -> Result<String>;
}

/// A workspace member handed to [`build_workspace_kg`]: its `project_id`
/// (a `ProjectConfig.id`) and a read-only handle to its per-project `Db`.
/// Constructed by the caller (task 2.1) in a deterministic order — global
/// entity ids are assigned in member-then-local order, so a stable `members`
/// slice is what makes delete-and-rebuild reproducible (spec scenario
/// "delete and rebuild").
pub struct Member<'a> {
    pub project_id: Uuid,
    pub db: &'a Db,
}

/// Outcome of one [`build_workspace_kg`] run. Counts are of rows written to
/// `kg.sqlite` this build; `llm_passes` mirrors the `meta` flag (design D5).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildReport {
    /// True if a cancel was observed before the merge was flushed; when true
    /// every other count is 0 and the previous graph is left untouched.
    pub cancelled: bool,
    pub members: usize,
    pub global_entities: usize,
    pub entity_links: usize,
    pub imported_edges: usize,
    pub cooccur_edges: usize,
    pub llm_edges: usize,
    /// Whether an LLM was available for this build's optional passes.
    pub llm_passes: bool,
}

impl BuildReport {
    fn cancelled(members: usize, llm_passes: bool) -> Self {
        BuildReport {
            cancelled: true,
            members,
            global_entities: 0,
            entity_links: 0,
            imported_edges: 0,
            cooccur_edges: 0,
            llm_edges: 0,
            llm_passes,
        }
    }
}

/// Normalize an entity name for tier-1 resolution (spec: "casefold, trimmed,
/// collapsed whitespace/punctuation"): Unicode-lowercased, every run of
/// non-alphanumeric characters collapsed to a single space, leading/trailing
/// space trimmed. `"Shattered  Realms!"` and `"shattered-realms"` both
/// normalize to `"shattered realms"`; `"Priya N."` to `"priya n"`.
pub fn normalize_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_sep = true; // start "in a separator" so leading punctuation trims
    for ch in name.chars() {
        if ch.is_alphanumeric() {
            for lc in ch.to_lowercase() {
                out.push(lc);
            }
            prev_sep = false;
        } else if !prev_sep {
            out.push(' ');
            prev_sep = true;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

/// Tier-1 (deterministic) cluster: one merged concept, its member-local
/// entities, and a display name. Built before tier-2 adjudication may union
/// clusters further.
struct Cluster {
    kind: String,
    name: String,
    /// `(member_index, local_entity_id, local_name)` — member index is into
    /// the `snapshots` slice.
    locals: Vec<(usize, i64, String)>,
}

/// Tier-1 resolution (task 1.3): group local entities by `(normalized name,
/// kind)`. Deterministic — members are visited in slice order, entities in
/// the member DB's own id order (`list_entities_with_edges` is `ORDER BY
/// id`). Returns the clusters plus a `(member, local_id) -> cluster index`
/// map used to resolve edges and co-occurrence.
fn tier1_clusters(snapshots: &[MemberSnapshot]) -> (Vec<Cluster>, HashMap<(usize, i64), usize>) {
    let mut clusters: Vec<Cluster> = Vec::new();
    let mut by_key: HashMap<(String, String), usize> = HashMap::new();
    let mut index: HashMap<(usize, i64), usize> = HashMap::new();
    for (mi, snap) in snapshots.iter().enumerate() {
        for e in &snap.entities {
            let key = (normalize_name(&e.name), e.kind.clone());
            let ci = *by_key.entry(key).or_insert_with(|| {
                clusters.push(Cluster {
                    kind: e.kind.clone(),
                    name: e.name.clone(),
                    locals: Vec::new(),
                });
                clusters.len() - 1
            });
            clusters[ci].locals.push((mi, e.local_id, e.name.clone()));
            index.insert((mi, e.local_id), ci);
        }
    }
    (clusters, index)
}

/// Levenshtein edit distance (pure, small inputs — normalized entity names).
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Do two normalized names share a non-trivial token?
fn shares_token(a: &str, b: &str) -> bool {
    let ta: HashSet<&str> = a.split(' ').filter(|t| t.len() >= MIN_SHARED_TOKEN_LEN).collect();
    b.split(' ')
        .any(|t| t.len() >= MIN_SHARED_TOKEN_LEN && ta.contains(t))
}

/// Is one normalized name a substring of the other?
fn contains_other(a: &str, b: &str) -> bool {
    !a.is_empty() && !b.is_empty() && (a.contains(b) || b.contains(a))
}

/// Near-miss candidate generation (task 1.4): same-kind cluster pairs whose
/// normalized names are close by a cheap signal — shared token, edit distance
/// <= 2, or containment (design D2). Pairs are `(i, j)` cluster indices with
/// `i < j`, generated in ascending order and capped at
/// [`MAX_ADJUDICATION_PAIRS`]. Tier-1 already merged exact `(norm, kind)`
/// matches, so every candidate pair has distinct normalized names.
fn candidate_pairs(clusters: &[Cluster]) -> Vec<(usize, usize)> {
    let norms: Vec<String> = clusters.iter().map(|c| normalize_name(&c.name)).collect();
    let mut pairs = Vec::new();
    for i in 0..clusters.len() {
        for j in (i + 1)..clusters.len() {
            if pairs.len() >= MAX_ADJUDICATION_PAIRS {
                return pairs;
            }
            if clusters[i].kind != clusters[j].kind {
                continue;
            }
            let (a, b) = (&norms[i], &norms[j]);
            if shares_token(a, b) || edit_distance(a, b) <= MAX_EDIT_DISTANCE || contains_other(a, b) {
                pairs.push((i, j));
            }
        }
    }
    pairs
}

/// One near-miss pair as presented to the adjudicator prompt.
pub struct AdjudicationInput<'a> {
    pub kind: &'a str,
    pub name_a: &'a str,
    pub name_b: &'a str,
}

/// The adjudicator's verdict for one pair. `merge` is the only field the
/// build acts on; `canonical_name` is parsed and surfaced for the contract
/// but NOT used to rename the merged entity in v1 — the stored global name
/// stays the tier-1 representative (a member's real local name), keeping the
/// LLM strictly non-load-bearing (design D5) and avoiding an invented name
/// that matches no member.
#[derive(Debug, Clone, PartialEq)]
pub struct Adjudication {
    pub merge: bool,
    pub canonical_name: Option<String>,
}

/// Compose the batched near-miss adjudication prompt (task 1.4). Each pair is
/// numbered `i`; the model is asked for a strict JSON array of
/// `{"i", "merge", "name"}`. Deterministic given the pair list.
pub fn compose_adjudication_prompt(pairs: &[AdjudicationInput<'_>]) -> String {
    let mut s = String::new();
    s.push_str(
        "You decide whether pairs of concept names refer to the SAME real-world entity.\n\
         Merge ONLY when you are confident they are the same thing; when in doubt, do not merge.\n\
         Never merge across different kinds.\n\n\
         Pairs:\n",
    );
    for (i, p) in pairs.iter().enumerate() {
        s.push_str(&format!(
            "{i}. kind={} | A: {} | B: {}\n",
            p.kind, p.name_a, p.name_b
        ));
    }
    s.push_str(
        "\nReply with ONLY a JSON array, one object per pair:\n\
         [{\"i\": 0, \"merge\": true, \"name\": \"canonical name\"}, ...]\n\
         Use \"merge\": false when unsure. No prose.",
    );
    s
}

/// Parse the adjudicator's answer tolerantly (task 1.4). Returns exactly
/// `n_pairs` verdicts, indexed by the `i` field; any pair the model omits,
/// or any output that is not a parseable JSON array, defaults to
/// `merge: false` (spec: "any parse failure, timeout, or non-affirmative
/// answer SHALL result in no merge"). `merge` accepts JSON `true` or an
/// affirmative string ("yes"/"same"/"merge"/...); everything else is no-merge.
pub fn parse_adjudication(raw: &str, n_pairs: usize) -> Vec<Adjudication> {
    let mut out = vec![
        Adjudication {
            merge: false,
            canonical_name: None
        };
        n_pairs
    ];
    let Some(items) = extract_json_array(raw) else {
        return out;
    };
    for item in items {
        let Some(i) = item.get("i").and_then(|v| v.as_u64()) else {
            continue;
        };
        let i = i as usize;
        if i >= n_pairs {
            continue;
        }
        let merge = affirmative(item.get("merge"));
        let canonical_name = item
            .get("name")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        out[i] = Adjudication {
            merge,
            canonical_name,
        };
    }
    out
}

/// One co-occurring pair presented to the typed-relation linking prompt.
pub struct LinkingInput<'a> {
    pub kind_a: &'a str,
    pub name_a: &'a str,
    pub kind_b: &'a str,
    pub name_b: &'a str,
}

/// Compose the batched typed-relation linking prompt (task 1.5). Numbered
/// pairs; the model proposes a short relation phrase per pair.
pub fn compose_linking_prompt(pairs: &[LinkingInput<'_>]) -> String {
    let mut s = String::new();
    s.push_str(
        "For each pair of related concepts, give a SHORT relation phrase (2-4 words) describing\n\
         how A relates to B (e.g. \"depends on\", \"maintained by\", \"part of\"). If unclear, use \"related\".\n\n\
         Pairs:\n",
    );
    for (i, p) in pairs.iter().enumerate() {
        s.push_str(&format!(
            "{i}. A: {} ({}) | B: {} ({})\n",
            p.name_a, p.kind_a, p.name_b, p.kind_b
        ));
    }
    s.push_str(
        "\nReply with ONLY a JSON array:\n\
         [{\"i\": 0, \"relation\": \"depends on\"}, ...]\nNo prose.",
    );
    s
}

/// Parse the linking pass answer tolerantly (task 1.5). Returns `n_pairs`
/// relation strings; any omitted or empty relation, or unparseable output,
/// falls back to `"related"` (design D5 fallback relation).
pub fn parse_linking(raw: &str, n_pairs: usize) -> Vec<String> {
    let mut out = vec!["related".to_string(); n_pairs];
    let Some(items) = extract_json_array(raw) else {
        return out;
    };
    for item in items {
        let Some(i) = item.get("i").and_then(|v| v.as_u64()) else {
            continue;
        };
        let i = i as usize;
        if i >= n_pairs {
            continue;
        }
        if let Some(r) = item.get("relation").and_then(|v| v.as_str()) {
            let r = r.trim();
            if !r.is_empty() {
                out[i] = r.to_string();
            }
        }
    }
    out
}

/// Compose the summary-merge prompt for a multi-link entity (task 1.5).
pub fn compose_summary_prompt(name: &str, kind: &str, summaries: &[&str]) -> String {
    let mut s = format!(
        "Write one merged summary (at most {MAX_SUMMARY_CHARS} characters) for the {kind} \"{name}\",\n\
         combining these per-project descriptions without repeating yourself:\n\n",
    );
    for (i, sum) in summaries.iter().enumerate() {
        s.push_str(&format!("{}. {}\n", i + 1, sum));
    }
    s.push_str("\nReply with ONLY the summary text, no preamble.");
    s
}

/// Truncate to at most `max` characters on a char boundary (never splits a
/// multi-byte char).
fn cap_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.trim().to_string()
    } else {
        s.chars().take(max).collect::<String>().trim().to_string()
    }
}

/// The longest of a set of summaries (first on a tie — deterministic).
fn longest_summary(summaries: &[&str]) -> String {
    let mut best = "";
    let mut best_len = 0usize;
    for s in summaries {
        let n = s.chars().count();
        if n > best_len {
            best_len = n;
            best = s;
        }
    }
    best.to_string()
}

/// An edge planned in memory, referencing final cluster indices; resolved to
/// global ids at flush time.
struct PlannedEdge {
    src: usize,
    dst: usize,
    relation: String,
    weight: f64,
    provenance: &'static str,
}

/// A fully merged concept ready to write: display name, summary, member
/// links, and doc pointers. Cluster index into [`MergePlan::clusters`] is the
/// currency `PlannedEdge` uses.
struct FinalCluster {
    kind: String,
    name: String,
    summary: String,
    /// `(member_index, local_entity_id, local_name)`.
    locals: Vec<(usize, i64, String)>,
    /// `(member_index, rel_path)` top source files for `doc_pointers`.
    pointers: Vec<(usize, String)>,
}

struct MergePlan {
    clusters: Vec<FinalCluster>,
    edges: Vec<PlannedEdge>,
}

/// Union-find over tier-1 cluster indices, used to apply tier-2 merges. Roots
/// are always the smaller index so the representative (and thus the canonical
/// name) is deterministic.
struct UnionFind {
    parent: Vec<usize>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        UnionFind {
            parent: (0..n).collect(),
        }
    }
    fn find(&mut self, mut x: usize) -> usize {
        while self.parent[x] != x {
            self.parent[x] = self.parent[self.parent[x]];
            x = self.parent[x];
        }
        x
    }
    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.parent[ra.max(rb)] = ra.min(rb);
        }
    }
}

/// Phase 2 (tasks 1.3-1.5): resolve tier-1 clusters, apply tier-2 LLM
/// adjudication, then derive summaries, `imported`/`cooccur`/`llm` edges, and
/// doc pointers — all in memory. Pure aside from the `llm` calls; returns
/// `Ok(None)` if `cancel` fires during an LLM pass so the caller leaves the
/// existing graph untouched. `llm = None` is fully deterministic.
fn merge_snapshots(
    snapshots: &[MemberSnapshot],
    llm: Option<&dyn FederationLlm>,
    cancel: &CancelToken,
) -> Result<Option<MergePlan>> {
    // Tier 1.
    let (clusters0, index0) = tier1_clusters(snapshots);

    // Tier 2: LLM adjudication of near-miss pairs (skipped entirely without a
    // model — the deterministic path).
    let mut uf = UnionFind::new(clusters0.len());
    if let Some(llm) = llm {
        let pairs = candidate_pairs(&clusters0);
        if !pairs.is_empty() {
            if cancel.is_cancelled() {
                return Ok(None);
            }
            let inputs: Vec<AdjudicationInput> = pairs
                .iter()
                .map(|&(i, j)| AdjudicationInput {
                    kind: &clusters0[i].kind,
                    name_a: &clusters0[i].name,
                    name_b: &clusters0[j].name,
                })
                .collect();
            let prompt = compose_adjudication_prompt(&inputs);
            // A generation failure is non-fatal (design D2/D5: "no" is the
            // default on any failure) — treat it as all-no-merge.
            let verdicts = match llm.complete(&prompt) {
                Ok(raw) => parse_adjudication(&raw, pairs.len()),
                Err(_) => Vec::new(),
            };
            for (k, &(i, j)) in pairs.iter().enumerate() {
                if verdicts.get(k).map(|v| v.merge).unwrap_or(false) {
                    uf.union(i, j);
                }
            }
        }
    }

    // Coalesce tier-1 clusters by union-find root into final clusters. Visited
    // in ascending index order, so the first cluster seen for a root IS the
    // root (lowest index) — its name/kind seed the final cluster.
    let mut root_to_final: HashMap<usize, usize> = HashMap::new();
    let mut clusters: Vec<FinalCluster> = Vec::new();
    let mut ci0_to_final: Vec<usize> = vec![0; clusters0.len()];
    for (ci0, c) in clusters0.iter().enumerate() {
        let root = uf.find(ci0);
        let fi = *root_to_final.entry(root).or_insert_with(|| {
            clusters.push(FinalCluster {
                kind: c.kind.clone(),
                name: c.name.clone(),
                summary: String::new(),
                locals: Vec::new(),
                pointers: Vec::new(),
            });
            clusters.len() - 1
        });
        clusters[fi].locals.extend(c.locals.iter().cloned());
        ci0_to_final[ci0] = fi;
    }

    // `(member, local_id) -> final cluster index`, composed through tier-2.
    let local_to_final: HashMap<(usize, i64), usize> = index0
        .into_iter()
        .map(|(k, ci0)| (k, ci0_to_final[ci0]))
        .collect();

    // Fast lookup of the source entity behind each local ref (summaries,
    // pointers).
    let mut entity_by_ref: HashMap<(usize, i64), &SnapshotEntity> = HashMap::new();
    for (mi, snap) in snapshots.iter().enumerate() {
        for e in &snap.entities {
            entity_by_ref.insert((mi, e.local_id), e);
        }
    }

    // Summaries + doc pointers per final cluster.
    for c in &mut clusters {
        // Deterministic member-then-local ordering of this cluster's locals.
        let mut ordered = c.locals.clone();
        ordered.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));

        // --- summary (task 1.5): copy on single link, merge/longest on many.
        let summaries: Vec<&str> = ordered
            .iter()
            .filter_map(|(mi, lid, _)| entity_by_ref.get(&(*mi, *lid)))
            .map(|e| e.summary.trim())
            .filter(|s| !s.is_empty())
            .collect();
        c.summary = if ordered.len() <= 1 {
            // Single link ⇒ copy that local's summary verbatim.
            summaries.first().map(|s| cap_chars(s, MAX_SUMMARY_CHARS)).unwrap_or_default()
        } else if summaries.is_empty() {
            String::new()
        } else if let Some(llm) = llm {
            if cancel.is_cancelled() {
                return Ok(None);
            }
            let prompt = compose_summary_prompt(&c.name, &c.kind, &summaries);
            match llm.complete(&prompt) {
                Ok(raw) if !raw.trim().is_empty() => cap_chars(&raw, MAX_SUMMARY_CHARS),
                _ => cap_chars(&longest_summary(&summaries), MAX_SUMMARY_CHARS),
            }
        } else {
            cap_chars(&longest_summary(&summaries), MAX_SUMMARY_CHARS)
        };
        // Spec: every global entity has a non-empty summary. When no member
        // supplied any summary text we fall back to the entity's own name
        // rather than fabricating content (documented judgment call).
        if c.summary.is_empty() {
            c.summary = cap_chars(&c.name, MAX_SUMMARY_CHARS);
        }

        // --- doc pointers (task 1.5): top files per member, in member order.
        let mut members_seen: Vec<usize> = ordered.iter().map(|(mi, _, _)| *mi).collect();
        members_seen.sort_unstable();
        members_seen.dedup();
        for mi in members_seen {
            let mut seen: HashSet<&str> = HashSet::new();
            let mut kept = 0usize;
            for (lmi, lid, _) in ordered.iter().filter(|(m, _, _)| *m == mi) {
                let Some(e) = entity_by_ref.get(&(*lmi, *lid)) else {
                    continue;
                };
                for path in &e.sources {
                    if kept >= DOC_POINTERS_PER_MEMBER {
                        break;
                    }
                    if seen.insert(path.as_str()) {
                        c.pointers.push((mi, path.clone()));
                        kept += 1;
                    }
                }
                if kept >= DOC_POINTERS_PER_MEMBER {
                    break;
                }
            }
        }
    }

    // --- imported edges (task 1.3): member edges resolved through the link
    // map, deduped by (src, dst, relation) with summed weight, self-edges
    // (both endpoints merged into one cluster) dropped.
    let mut imported: BTreeMap<(usize, usize, String), f64> = BTreeMap::new();
    for (mi, snap) in snapshots.iter().enumerate() {
        for e in &snap.edges {
            let (Some(&sa), Some(&sb)) = (
                local_to_final.get(&(mi, e.a)),
                local_to_final.get(&(mi, e.b)),
            ) else {
                continue;
            };
            if sa == sb {
                continue;
            }
            *imported.entry((sa, sb, e.label.clone())).or_insert(0.0) += 1.0;
        }
    }

    // --- co-occurrence (task 1.5): count (member, file) co-mentions between
    // distinct clusters. Two clusters co-occur when the same source file in
    // the same member is cited by a local of each.
    let mut cooccur: BTreeMap<(usize, usize), f64> = BTreeMap::new();
    for (mi, snap) in snapshots.iter().enumerate() {
        // file -> set of final clusters citing it in this member.
        let mut by_file: BTreeMap<&str, HashSet<usize>> = BTreeMap::new();
        for e in &snap.entities {
            let Some(&fi) = local_to_final.get(&(mi, e.local_id)) else {
                continue;
            };
            for path in &e.sources {
                by_file.entry(path.as_str()).or_default().insert(fi);
            }
        }
        for clusters_here in by_file.values() {
            let mut v: Vec<usize> = clusters_here.iter().copied().collect();
            v.sort_unstable();
            for a in 0..v.len() {
                for b in (a + 1)..v.len() {
                    *cooccur.entry((v[a], v[b])).or_insert(0.0) += 1.0;
                }
            }
        }
    }

    // --- optional typed-relation linking pass (task 1.5): top co-occurring
    // pairs get an `llm`-provenance edge with a proposed relation; without a
    // model there are no `llm` edges (the `cooccur` edges' "related" is the
    // fallback).
    let mut llm_relations: HashMap<(usize, usize), String> = HashMap::new();
    if let Some(llm) = llm {
        let mut ranked: Vec<((usize, usize), f64)> =
            cooccur.iter().map(|(k, w)| (*k, *w)).collect();
        // Highest weight first, then by pair for a stable tie-break.
        ranked.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(&b.0))
        });
        ranked.truncate(MAX_LINKING_PAIRS);
        if !ranked.is_empty() {
            if cancel.is_cancelled() {
                return Ok(None);
            }
            let inputs: Vec<LinkingInput> = ranked
                .iter()
                .map(|&((a, b), _)| LinkingInput {
                    kind_a: &clusters[a].kind,
                    name_a: &clusters[a].name,
                    kind_b: &clusters[b].kind,
                    name_b: &clusters[b].name,
                })
                .collect();
            let prompt = compose_linking_prompt(&inputs);
            let relations = match llm.complete(&prompt) {
                Ok(raw) => parse_linking(&raw, ranked.len()),
                Err(_) => vec!["related".to_string(); ranked.len()],
            };
            for (k, &(pair, _)) in ranked.iter().enumerate() {
                llm_relations.insert(pair, relations[k].clone());
            }
        }
    }

    // Assemble edges deterministically: imported, then cooccur, then llm.
    let mut edges: Vec<PlannedEdge> = Vec::new();
    for ((src, dst, relation), weight) in imported {
        edges.push(PlannedEdge {
            src,
            dst,
            relation,
            weight,
            provenance: "imported",
        });
    }
    for (&(src, dst), &weight) in &cooccur {
        edges.push(PlannedEdge {
            src,
            dst,
            relation: "related".to_string(),
            weight,
            provenance: "cooccur",
        });
    }
    for (&(src, dst), relation) in &llm_relations {
        edges.push(PlannedEdge {
            src,
            dst,
            relation: relation.clone(),
            weight: cooccur.get(&(src, dst)).copied().unwrap_or(1.0),
            provenance: "llm",
        });
    }
    // `llm_relations` iterates a HashMap; sort just the llm tail (imported +
    // cooccur were pushed in already-sorted BTreeMap order) for determinism.
    let split = edges.iter().position(|e| e.provenance == "llm").unwrap_or(edges.len());
    edges[split..].sort_by(|a, b| (a.src, a.dst).cmp(&(b.src, b.dst)));

    Ok(Some(MergePlan { clusters, edges }))
}

/// Build (or rebuild) the workspace knowledge graph (task 1.6). Design D3's
/// "snapshot -> merge": phase 1 reads each member's snapshot (served from the
/// `kg.sqlite` cache when its watermark is unchanged — the incremental win),
/// phase 2 does a full in-memory re-merge, and only then flushes to the DB.
///
/// Consistency on cancellation: the entire merge is computed in memory and the
/// existing graph is cleared **only** immediately before the (uninterrupted)
/// write burst. `cancel` is checked before reading each member and before the
/// flush, never during it — so a cancel always leaves the DB in a consistent
/// state: either the previous graph intact (cancelled before flush) or the new
/// graph complete. This is the natural unit of the D3 full-re-merge design.
///
/// Determinism: with `llm = None` and a fixed `now`, repeated builds over the
/// same members produce byte-identical rows (spec: "delete and rebuild").
/// `now` (epoch seconds) stamps `updated_at`/`cached_at`, kept as a parameter
/// — not read from the clock — exactly as [`snapshot_for_member`] does, so
/// callers and tests stay reproducible.
pub fn build_workspace_kg(
    kg: &mut WorkspaceKgDb,
    members: &[Member<'_>],
    llm: Option<&dyn FederationLlm>,
    now: i64,
    cancel: &CancelToken,
) -> Result<BuildReport> {
    let llm_passes = llm.is_some();
    if cancel.is_cancelled() {
        return Ok(BuildReport::cancelled(members.len(), llm_passes));
    }

    // Phase 1: snapshots (cached by watermark).
    let mut snapshots = Vec::with_capacity(members.len());
    for m in members {
        if cancel.is_cancelled() {
            return Ok(BuildReport::cancelled(members.len(), llm_passes));
        }
        snapshots.push(snapshot_for_member(kg, m.project_id, m.db, now)?);
    }

    // Phase 2: merge (in memory). None ⇒ cancelled during an LLM pass.
    let Some(plan) = merge_snapshots(&snapshots, llm, cancel)? else {
        return Ok(BuildReport::cancelled(members.len(), llm_passes));
    };
    if cancel.is_cancelled() {
        return Ok(BuildReport::cancelled(members.len(), llm_passes));
    }

    // Phase 2 flush: clear the previous merged graph and write the new one in
    // one uninterrupted pass (no cancel checks past this point).
    kg.clear_merged()?;
    let mut global_ids: Vec<i64> = Vec::with_capacity(plan.clusters.len());
    let mut entity_links = 0usize;
    for c in &plan.clusters {
        let gid = kg.insert_global_entity(&c.kind, &c.name, &c.summary, now)?;
        global_ids.push(gid);
        for (mi, local_id, local_name) in &c.locals {
            kg.insert_entity_link(gid, snapshots[*mi].project_id, *local_id, local_name)?;
            entity_links += 1;
        }
        for (mi, rel_path) in &c.pointers {
            kg.insert_doc_pointer(gid, snapshots[*mi].project_id, rel_path, "")?;
        }
    }
    let (mut imported_edges, mut cooccur_edges, mut llm_edges) = (0usize, 0usize, 0usize);
    for e in &plan.edges {
        kg.insert_global_edge(
            global_ids[e.src],
            global_ids[e.dst],
            &e.relation,
            e.weight,
            e.provenance,
        )?;
        match e.provenance {
            "imported" => imported_edges += 1,
            "cooccur" => cooccur_edges += 1,
            _ => llm_edges += 1,
        }
    }

    kg.set_meta("llm_passes", if llm_passes { "true" } else { "false" })?;
    kg.set_meta("workspace_kg_built_at", &now.to_string())?;

    Ok(BuildReport {
        cancelled: false,
        members: members.len(),
        global_entities: plan.clusters.len(),
        entity_links,
        imported_edges,
        cooccur_edges,
        llm_edges,
        llm_passes,
    })
}

/// Tolerant JSON-array extractor mirroring `knowledge_model::parse_extraction`
/// (first `[` .. last `]`, ignoring surrounding prose or code fences).
fn extract_json_array(raw: &str) -> Option<Vec<serde_json::Value>> {
    let start = raw.find('[')?;
    let end = raw.rfind(']').filter(|e| *e > start)?;
    match serde_json::from_str::<serde_json::Value>(&raw[start..=end]).ok()? {
        serde_json::Value::Array(a) => Some(a),
        _ => None,
    }
}

/// Is a JSON value an affirmative merge verdict? `true`, or a "yes"-family
/// string; everything else (including `false`, numbers, null, absent) is not.
fn affirmative(v: Option<&serde_json::Value>) -> bool {
    match v {
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::String(s)) => matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "yes" | "y" | "true" | "same" | "merge" | "1"
        ),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{EntityInput, EventInput};

    /// A tiny fixture member DB with two linked entities, mirroring the
    /// `sample_model()` helper in `db.rs`'s own tests (same
    /// `Db::open_in_memory` + `EntityInput`/`replace_knowledge_model`
    /// pattern used there).
    fn fixture_member_db(built_at: i64) -> Db {
        let mut db = Db::open_in_memory().unwrap();
        db.replace_knowledge_model(
            &[
                EntityInput {
                    kind: "topic".into(),
                    name: "Shattered Realms".into(),
                    summary: "A game project.".into(),
                    sources: vec!["design/overview.md".into()],
                    connections: vec![(1, "owned by".into())],
                },
                EntityInput {
                    kind: "person".into(),
                    name: "Priya N.".into(),
                    summary: "Lead designer.".into(),
                    sources: vec!["knowledge/People.md".into()],
                    connections: vec![],
                },
            ],
            &[EventInput {
                date: "2026-07-01".into(),
                category: "decision".into(),
                text: "Kickoff.".into(),
                source: "notes/kickoff.md".into(),
            }],
            built_at,
        )
        .unwrap();
        db
    }

    #[test]
    fn snapshot_read_captures_entities_edges_and_sources() {
        let db = fixture_member_db(100);
        let project_id = Uuid::new_v4();
        let snapshot = MemberSnapshot::read(project_id, &db).unwrap();

        assert_eq!(snapshot.project_id, project_id);
        assert_eq!(snapshot.watermark, Some(100));
        assert_eq!(snapshot.entities.len(), 2);
        assert_eq!(snapshot.edges.len(), 1);

        let topic = snapshot
            .entities
            .iter()
            .find(|e| e.name == "Shattered Realms")
            .unwrap();
        assert_eq!(topic.kind, "topic");
        assert_eq!(topic.sources, vec!["design/overview.md".to_string()]);

        let priya = snapshot.entities.iter().find(|e| e.name == "Priya N.").unwrap();
        let edge = &snapshot.edges[0];
        assert_eq!(edge.a, topic.local_id);
        assert_eq!(edge.b, priya.local_id);
        assert_eq!(edge.label, "owned by");
    }

    #[test]
    fn snapshot_read_on_never_built_member_is_empty_not_error() {
        let db = Db::open_in_memory().unwrap();
        let snapshot = MemberSnapshot::read(Uuid::new_v4(), &db).unwrap();
        assert_eq!(snapshot.watermark, None);
        assert!(snapshot.entities.is_empty());
        assert!(snapshot.edges.is_empty());
    }

    #[test]
    fn snapshot_round_trips_through_json_for_the_cache() {
        // set_snapshot/get_snapshot store an opaque blob; MemberSnapshot
        // must be exactly what round-trips through it.
        let db = fixture_member_db(100);
        let project_id = Uuid::new_v4();
        let snapshot = MemberSnapshot::read(project_id, &db).unwrap();
        let blob = serde_json::to_string(&snapshot).unwrap();
        let back: MemberSnapshot = serde_json::from_str(&blob).unwrap();
        assert_eq!(snapshot, back);
    }

    #[test]
    fn cache_hit_when_watermark_unchanged() {
        let mut kg = WorkspaceKgDb::open_in_memory().unwrap();
        let db = fixture_member_db(100);
        let project_id = Uuid::new_v4();

        let first = snapshot_for_member(&mut kg, project_id, &db, 1_000).unwrap();
        assert_eq!(first.entities.len(), 2);

        // A cached row now exists at watermark 100.
        assert_eq!(kg.get_watermark(project_id).unwrap(), Some(Some(100)));

        // Second call with the SAME db (same watermark) must be a cache
        // hit: verify by checking the cache's cached_at was not bumped
        // (a re-write on a hit would defeat the point of caching).
        let second = snapshot_for_member(&mut kg, project_id, &db, 2_000).unwrap();
        assert_eq!(second, first);
        let row = kg.get_snapshot(project_id).unwrap().unwrap();
        assert_eq!(row.cached_at, 1_000, "cache hit must not rewrite cached_at");
    }

    #[test]
    fn cache_miss_and_refresh_when_watermark_advances() {
        let mut kg = WorkspaceKgDb::open_in_memory().unwrap();
        let project_id = Uuid::new_v4();

        let db_v1 = fixture_member_db(100);
        let first = snapshot_for_member(&mut kg, project_id, &db_v1, 1_000).unwrap();
        assert_eq!(first.watermark, Some(100));

        // Simulate the member rebuilding its knowledge model: a new Db
        // with a later watermark and a changed entity set.
        let mut db_v2 = Db::open_in_memory().unwrap();
        db_v2
            .replace_knowledge_model(
                &[EntityInput {
                    kind: "topic".into(),
                    name: "Shattered Realms".into(),
                    summary: "Updated summary.".into(),
                    sources: vec!["design/overview.md".into(), "design/v2.md".into()],
                    connections: vec![],
                }],
                &[],
                200,
            )
            .unwrap();

        let second = snapshot_for_member(&mut kg, project_id, &db_v2, 2_000).unwrap();
        assert_eq!(second.watermark, Some(200));
        assert_eq!(second.entities.len(), 1);
        assert_eq!(
            second.entities[0].sources,
            vec!["design/overview.md".to_string(), "design/v2.md".to_string()]
        );
        let row = kg.get_snapshot(project_id).unwrap().unwrap();
        assert_eq!(row.cached_at, 2_000, "watermark advance must refresh the cache");
    }

    #[test]
    fn snapshot_for_member_never_writes_to_the_member_db() {
        // `Db` has no interior mutability that would let a read silently
        // write; this test documents the read-only contract by exercising
        // the exact call federation makes and checking the member's own
        // watermark/entity count are unchanged afterward.
        let mut kg = WorkspaceKgDb::open_in_memory().unwrap();
        let db = fixture_member_db(100);
        let project_id = Uuid::new_v4();
        let before = db.knowledge_model_built_at().unwrap();
        let (before_entities, _) = db.list_entities_with_edges().unwrap();

        snapshot_for_member(&mut kg, project_id, &db, 1_000).unwrap();

        let after = db.knowledge_model_built_at().unwrap();
        let (after_entities, _) = db.list_entities_with_edges().unwrap();
        assert_eq!(before, after);
        assert_eq!(before_entities, after_entities);
    }

    // --- tasks 1.3-1.7: resolution + merge -------------------------------

    /// Stable project ids so delete-and-rebuild produces identical rows.
    fn pa() -> Uuid {
        Uuid::from_u128(0xA1)
    }
    fn pb() -> Uuid {
        Uuid::from_u128(0xB2)
    }

    /// Member A: "Shattered Realms" (topic, links to Priya), Priya (person),
    /// Combat System (topic). All three co-mention `design/overview.md`.
    fn member_a() -> Db {
        let mut db = Db::open_in_memory().unwrap();
        db.replace_knowledge_model(
            &[
                EntityInput {
                    kind: "topic".into(),
                    name: "Shattered Realms".into(),
                    summary: "Game.".into(),
                    sources: vec!["design/overview.md".into(), "README.md".into()],
                    connections: vec![(1, "led by".into())],
                },
                EntityInput {
                    kind: "person".into(),
                    name: "Priya N.".into(),
                    summary: "Lead.".into(),
                    sources: vec!["design/overview.md".into()],
                    connections: vec![],
                },
                EntityInput {
                    kind: "topic".into(),
                    name: "Combat System".into(),
                    summary: "Fights.".into(),
                    sources: vec!["design/overview.md".into()],
                    connections: vec![],
                },
            ],
            &[],
            100,
        )
        .unwrap();
        db
    }

    /// Member B: "shattered realms" (topic, merges with A's) and Acme (org);
    /// both co-mention `docs/intro.md`.
    fn member_b() -> Db {
        let mut db = Db::open_in_memory().unwrap();
        db.replace_knowledge_model(
            &[
                EntityInput {
                    kind: "topic".into(),
                    name: "shattered realms".into(),
                    summary: "MMO.".into(),
                    sources: vec!["docs/intro.md".into()],
                    connections: vec![],
                },
                EntityInput {
                    kind: "organization".into(),
                    name: "Acme".into(),
                    summary: "Vendor.".into(),
                    sources: vec!["docs/intro.md".into()],
                    connections: vec![],
                },
            ],
            &[],
            100,
        )
        .unwrap();
        db
    }

    #[test]
    fn normalize_name_table() {
        let cases = [
            ("Shattered Realms", "shattered realms"),
            ("  Priya  N. ", "priya n"),
            ("SHATTERED-REALMS!!", "shattered realms"),
            ("Acme, Inc.", "acme inc"),
            ("Über Tool", "über tool"),
            ("", ""),
            ("...", ""),
        ];
        for (input, want) in cases {
            assert_eq!(normalize_name(input), want, "normalize({input:?})");
        }
    }

    #[test]
    fn edit_distance_basics() {
        assert_eq!(edit_distance("", ""), 0);
        assert_eq!(edit_distance("abc", "abc"), 0);
        assert_eq!(edit_distance("shattered realms", "shatterd realms"), 1);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
    }

    #[test]
    fn candidate_pairs_flags_near_misses_within_kind() {
        // One member, four topics; only the typo pair is a near-miss.
        let snap = MemberSnapshot {
            project_id: pa(),
            watermark: Some(1),
            edges: vec![],
            entities: vec![
                SnapshotEntity { local_id: 1, kind: "topic".into(), name: "Shattered Realms".into(), summary: String::new(), sources: vec![] },
                SnapshotEntity { local_id: 2, kind: "topic".into(), name: "Shatterd Realms".into(), summary: String::new(), sources: vec![] },
                SnapshotEntity { local_id: 3, kind: "topic".into(), name: "Combat System".into(), summary: String::new(), sources: vec![] },
                // Same normalized text as #1 but a different kind ⇒ never a candidate.
                SnapshotEntity { local_id: 4, kind: "person".into(), name: "Shattered Realms".into(), summary: String::new(), sources: vec![] },
            ],
        };
        let (clusters, _) = tier1_clusters(&[snap]);
        assert_eq!(clusters.len(), 4, "no exact-merge collisions across kinds");
        let pairs = candidate_pairs(&clusters);
        // (0,1) shares the "realms" token AND is edit distance 1; nothing else
        // qualifies (Combat shares no token; the person differs in kind).
        assert_eq!(pairs, vec![(0, 1)]);
    }

    #[test]
    fn parse_adjudication_yes_no_garbage() {
        // Affirmative (bool and string) ⇒ merge; everything else ⇒ no merge.
        let yes = parse_adjudication(r#"[{"i":0,"merge":true,"name":"Shattered Realms"}]"#, 1);
        assert!(yes[0].merge);
        assert_eq!(yes[0].canonical_name.as_deref(), Some("Shattered Realms"));

        let yes_str = parse_adjudication(r#"[{"i":0,"merge":"yes"}]"#, 1);
        assert!(yes_str[0].merge);

        let no = parse_adjudication(r#"[{"i":0,"merge":false}]"#, 1);
        assert!(!no[0].merge);

        // Garbage, prose-wrapped garbage, and an omitted pair all ⇒ no merge.
        assert_eq!(parse_adjudication("not json at all", 2), vec![
            Adjudication { merge: false, canonical_name: None },
            Adjudication { merge: false, canonical_name: None },
        ]);
        let hedged = parse_adjudication(r#"Sure! [{"i":0,"merge":"maybe"}]"#, 1);
        assert!(!hedged[0].merge);
        // Out-of-range index is ignored, remaining pair stays no-merge.
        let oob = parse_adjudication(r#"[{"i":9,"merge":true}]"#, 1);
        assert!(!oob[0].merge);
    }

    #[test]
    fn parse_linking_relations_and_fallback() {
        let r = parse_linking(r#"[{"i":0,"relation":"depends on"},{"i":1,"relation":"  "}]"#, 3);
        assert_eq!(r, vec!["depends on", "related", "related"]);
        // Unparseable ⇒ all fallback.
        assert_eq!(parse_linking("garbage", 2), vec!["related", "related"]);
    }

    #[test]
    fn compose_prompts_carry_the_contract() {
        let adj = compose_adjudication_prompt(&[AdjudicationInput { kind: "topic", name_a: "SR", name_b: "Shattered Realms" }]);
        assert!(adj.contains("SR") && adj.contains("Shattered Realms") && adj.contains("JSON"));
        let sum = compose_summary_prompt("Shattered Realms", "topic", &["Game.", "MMO."]);
        assert!(sum.contains("Shattered Realms") && sum.contains("Game.") && sum.contains("400"));
    }

    fn members<'a>(a: &'a Db, b: &'a Db) -> Vec<Member<'a>> {
        vec![
            Member { project_id: pa(), db: a },
            Member { project_id: pb(), db: b },
        ]
    }

    #[test]
    fn federation_over_two_members_merges_links_and_edges() {
        let a = member_a();
        let b = member_b();
        let mut kg = WorkspaceKgDb::open_in_memory().unwrap();
        let report =
            build_workspace_kg(&mut kg, &members(&a, &b), None, 1_000, &CancelToken::new()).unwrap();

        assert_eq!(
            report,
            BuildReport {
                cancelled: false,
                members: 2,
                global_entities: 4,
                entity_links: 5,
                imported_edges: 1,
                cooccur_edges: 4,
                llm_edges: 0,
                llm_passes: false,
            }
        );

        let globals = kg.list_global_entities().unwrap();
        assert_eq!(globals.len(), 4);

        // The merged concept: "Shattered Realms" (topic) with two links; its
        // summary is the longest local (LLM off) and is non-empty.
        let sr = globals.iter().find(|g| g.name == "Shattered Realms").unwrap();
        assert_eq!(kg.list_links_for_global(sr.id).unwrap().len(), 2);
        assert_eq!(sr.summary, "Game.");
        assert!(!sr.summary.is_empty());

        // Every merged single-link entity still has a non-empty summary.
        for g in &globals {
            assert!(!g.summary.is_empty(), "{} has empty summary", g.name);
        }

        // Exactly one imported edge (A: Shattered Realms --led by--> Priya).
        let edges = kg.list_all_edges().unwrap();
        let imported: Vec<_> = edges.iter().filter(|e| e.provenance == "imported").collect();
        assert_eq!(imported.len(), 1);
        assert_eq!(imported[0].relation, "led by");
        assert_eq!(edges.iter().filter(|e| e.provenance == "cooccur").count(), 4);
        assert!(edges.iter().all(|e| e.provenance != "llm"));

        // Pointer integrity: every pointer resolves to a real member source
        // path, and the merged entity carries pointers from both members.
        let known = ["design/overview.md", "README.md", "docs/intro.md"];
        let mut total_pointers = 0usize;
        for g in &globals {
            for p in kg.list_pointers_for_global(g.id).unwrap() {
                assert!(known.contains(&p.rel_path.as_str()), "stray pointer {}", p.rel_path);
                total_pointers += 1;
            }
        }
        assert_eq!(total_pointers, 6);
        let sr_pointers = kg.list_pointers_for_global(sr.id).unwrap();
        assert!(sr_pointers.iter().any(|p| p.project_id == pa().to_string()));
        assert!(sr_pointers.iter().any(|p| p.project_id == pb().to_string()));
    }

    #[test]
    fn delete_and_rebuild_is_identical_with_llm_off() {
        let a = member_a();
        let b = member_b();

        let mut kg1 = WorkspaceKgDb::open_in_memory().unwrap();
        build_workspace_kg(&mut kg1, &members(&a, &b), None, 1_000, &CancelToken::new()).unwrap();

        // A brand-new kg.sqlite (== the file was deleted) rebuilt from the same
        // members at the same `now`.
        let mut kg2 = WorkspaceKgDb::open_in_memory().unwrap();
        build_workspace_kg(&mut kg2, &members(&a, &b), None, 1_000, &CancelToken::new()).unwrap();

        assert_eq!(kg1.list_global_entities().unwrap(), kg2.list_global_entities().unwrap());
        assert_eq!(kg1.list_all_edges().unwrap(), kg2.list_all_edges().unwrap());
        for g in kg1.list_global_entities().unwrap() {
            assert_eq!(
                kg1.list_links_for_global(g.id).unwrap(),
                kg2.list_links_for_global(g.id).unwrap()
            );
            assert_eq!(
                kg1.list_pointers_for_global(g.id).unwrap(),
                kg2.list_pointers_for_global(g.id).unwrap()
            );
        }
        assert_eq!(kg1.get_meta("llm_passes").unwrap().as_deref(), Some("false"));
    }

    #[test]
    fn rebuild_serves_unchanged_members_from_cache() {
        let a = member_a();
        let b = member_b();
        let mut kg = WorkspaceKgDb::open_in_memory().unwrap();

        build_workspace_kg(&mut kg, &members(&a, &b), None, 1_000, &CancelToken::new()).unwrap();
        assert_eq!(kg.get_snapshot(pa()).unwrap().unwrap().cached_at, 1_000);

        // Second build at a later `now`; watermarks are unchanged, so the
        // snapshot cache is a hit and its `cached_at` is NOT rewritten.
        build_workspace_kg(&mut kg, &members(&a, &b), None, 2_000, &CancelToken::new()).unwrap();
        assert_eq!(
            kg.get_snapshot(pa()).unwrap().unwrap().cached_at,
            1_000,
            "unchanged member must be served from cache, not re-read"
        );
    }

    #[test]
    fn cancellation_leaves_the_previous_graph_intact() {
        let a = member_a();
        let b = member_b();
        let mut kg = WorkspaceKgDb::open_in_memory().unwrap();

        // A complete first build.
        build_workspace_kg(&mut kg, &members(&a, &b), None, 1_000, &CancelToken::new()).unwrap();
        assert_eq!(kg.list_global_entities().unwrap().len(), 4);

        // A rebuild whose token is already cancelled: no clear, no partial
        // write — the first build's graph is left fully intact.
        let cancel = CancelToken::new();
        cancel.cancel();
        let report = build_workspace_kg(&mut kg, &members(&a, &b), None, 2_000, &cancel).unwrap();
        assert!(report.cancelled);
        assert_eq!(report.global_entities, 0);
        assert_eq!(kg.list_global_entities().unwrap().len(), 4, "graph must be unchanged");
    }

    /// Deterministic in-process LLM stub (no model, no network): merges the one
    /// near-miss pair and returns a fixed merged summary. Exercises the tier-2
    /// union + summary-merge wiring that `llm = None` never reaches.
    struct MergeYesLlm;
    impl FederationLlm for MergeYesLlm {
        fn complete(&self, prompt: &str) -> Result<String> {
            if prompt.contains("SAME real-world entity") {
                Ok(r#"[{"i":0,"merge":true,"name":"Shattered Realms"}]"#.into())
            } else {
                Ok("Merged: the Shattered Realms game.".into())
            }
        }
    }

    #[test]
    fn llm_adjudication_merges_near_miss_pair() {
        // Two members, each a single topic whose names differ by a typo, so
        // tier-1 leaves them separate and tier-2 adjudication must merge them.
        let mut a = Db::open_in_memory().unwrap();
        a.replace_knowledge_model(
            &[EntityInput {
                kind: "topic".into(),
                name: "Shattered Realms".into(),
                summary: "The game.".into(),
                sources: vec!["a.md".into()],
                connections: vec![],
            }],
            &[],
            100,
        )
        .unwrap();
        let mut b = Db::open_in_memory().unwrap();
        b.replace_knowledge_model(
            &[EntityInput {
                kind: "topic".into(),
                name: "Shatterd Realms".into(),
                summary: "An MMO world.".into(),
                sources: vec!["b.md".into()],
                connections: vec![],
            }],
            &[],
            100,
        )
        .unwrap();

        let mut kg = WorkspaceKgDb::open_in_memory().unwrap();
        let llm = MergeYesLlm;
        let report = build_workspace_kg(
            &mut kg,
            &members(&a, &b),
            Some(&llm as &dyn FederationLlm),
            1_000,
            &CancelToken::new(),
        )
        .unwrap();

        assert!(report.llm_passes);
        assert_eq!(report.global_entities, 1, "the near-miss pair merged");
        assert_eq!(report.entity_links, 2);
        let g = &kg.list_global_entities().unwrap()[0];
        assert_eq!(g.summary, "Merged: the Shattered Realms game.");
        assert_eq!(kg.get_meta("llm_passes").unwrap().as_deref(), Some("true"));
    }
}

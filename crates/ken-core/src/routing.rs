//! Workspace search routing (`openspec/changes/kg-routing`).
//!
//! Given a query, decide which member projects' semantic indexes to search
//! — directly when a member is named, via the workspace KG when it isn't,
//! broadcasting when neither applies — then fan out the existing per-member
//! hybrid search (`crate::search`) and merge the member result lists into
//! one cited list. This module is composition and policy only: no new
//! storage, no new retrieval algorithm (proposal.md, design.md "Goals /
//! Non-Goals").
//!
//! ## Scope of this session (ken-core layer, tasks 1.1-1.4)
//!
//! `plan_route` (task 1.1) is the three-tier decision (design D1: "a pure
//! function with the KG as optional input" — pure in the sense that its
//! only I/O is through the caller-injected `&WorkspaceKgDb` read handle,
//! which is exactly what makes it table-testable with an in-memory
//! fixture). `search_member`/`execute_plan`/`merge_routed` (task 1.3) are
//! the fan-out + cross-member RRF merge. `WorkspaceKgDb::rank_projects_for_entities`
//! (task 1.2) lives in `workspace_kg_db.rs`, additive next to the existing
//! `entity_links`/`doc_pointers` CRUD.
//!
//! ## Deviations from the design doc (recorded per this session's brief:
//! "deferral over invention; record conflicts")
//!
//! * **No `KgHandle` type.** `design.md`/`tasks.md` write `kg: Option<&KgHandle>`
//!   but no such type exists anywhere in the codebase. `workspace_kg_db.rs`
//!   already documents that the design's `workspace.rs` member-enumeration
//!   layer doesn't exist yet either. The concrete, already-real type that
//!   fits the slot is `&WorkspaceKgDb` itself (it has an `open_in_memory`
//!   test constructor, which is exactly what makes the KG a "soft
//!   dependency" fixture-testable per D1) — used directly rather than
//!   inventing a wrapper trait with a single implementor.
//! * **No `ProjectId` newtype.** Every existing member-identifying type in
//!   this crate (`federation::MemberSnapshot::project_id`,
//!   `federation::Member::project_id`, `entity_links.project_id`) uses a
//!   plain `Uuid` (a member's `ProjectConfig.id`). `RoutePlan::targets` does
//!   the same rather than introducing a newtype nothing else in the crate
//!   uses.
//! * **No alias field.** `proposal.md`'s Named tier says "name/alias
//!   containment", but `project::ProjectConfig` has only `name` — there is
//!   no aliases list anywhere in the codebase to read. Named-tier matching
//!   therefore matches `MemberInfo::name` only, normalized (so casing,
//!   punctuation, and whitespace variants of the same name still match —
//!   the closest available approximation of "alias-tolerant"). If a real
//!   alias list lands later, `MemberInfo` can grow a field without changing
//!   `plan_route`'s signature.
//! * **No production single-DB `hybrid_search` exists in ken-core to call.**
//!   Grepping for it turns up only a private test helper in `engine.rs`
//!   (`fn hybrid_search`, `#[cfg(test)]`) with a doc comment saying the real
//!   thing is "task 2.2's `hybrid_search` command" — which lives in
//!   `src-tauri/src/lib.rs`, composing `Db::search_chunks_fts` +
//!   `Db::semantic_search` + `search::merge_and_rerank` itself. `search_member`
//!   below is that same composition, generalized to one member so this
//!   layer (and `ken-mcp`, later) can reuse it instead of re-deriving it a
//!   third time.
//! * **Concurrency is the caller's, not this module's.** `Db` wraps a plain
//!   `rusqlite::Connection`, which is `Send` but not `Sync` — there is no
//!   way to fan a read out across threads over a shared `&Db` the way
//!   `std::thread::scope` needs. ken-core's own precedent
//!   (`std::thread::spawn` in `engine.rs`/`chat.rs`/`watch.rs`/etc.) always
//!   pairs with an owned handle or channel, never a borrowed `!Sync` value
//!   shared across threads; src-tauri's actual multi-project concurrency
//!   wraps each project's `Db` in its own `Arc<Mutex<Db>>` and fans out via
//!   `spawn_blocking`/tokio tasks (see `hybrid_search` in
//!   `src-tauri/src/lib.rs`). Reproducing that here would mean either
//!   requiring every caller to pass `Arc<Mutex<Db>>` (a shape ken-core's
//!   `Db` API doesn't otherwise use) or spawning raw OS threads per search
//!   inside a "pure" module. Instead: `search_member` is the single-member,
//!   synchronous primitive; `execute_plan` composes it in a plain sequential
//!   loop over `plan.targets` (still "embed once, reuse the vector across
//!   members" — the loop just isn't parallel). A caller that wants real
//!   concurrency (src-tauri task 2.1, ken-mcp) calls `search_member` itself
//!   per target under its own thread/task pool and feeds the resulting
//!   [`MemberHits`] to [`merge_routed`], which is pure and has no opinion on
//!   how its inputs were produced. This is the "honest adaptation" the task
//!   brief allowed for.
//! * **KG breadcrumbs are plan-level, not per-hit.** `design.md` D3 shows
//!   `kg://<entity-id> → project` as an illustration of what a breadcrumb
//!   *explains*, not a literal stored string — precise per-hit entity
//!   attribution would need joining each hit's path back through
//!   `doc_pointers` by `(project_id, rel_path)`, which isn't available at
//!   this layer (a hit's path comes from the member's own `chunks` table,
//!   not from `doc_pointers`). `merge_routed` instead attaches every
//!   `RouteReason::KgEntities` id as a `kg://<id>` breadcrumb to every hit
//!   from that plan — correct ("these are the entities that caused this
//!   search to happen") even if not maximally precise ("this exact hit is
//!   about that exact entity"). The UI already owns final breadcrumb
//!   rendering (D3: "the UI resolves ... to open the source"), so it can
//!   compose `member name + these ids` however it likes.

use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use crate::db::Db;
use crate::embedder::Embedder;
use crate::federation::normalize_name;
use crate::search::{self, HybridHit, Source};
use crate::workspace_kg_db::WorkspaceKgDb;
use crate::Result;

/// KG-guided tier cap (design/spec: "ranked ... cap 3").
pub const KG_TARGET_CAP: usize = 3;
/// Broadcast tier cap (design/spec: "capped at 5 by recent-activity order").
pub const BROADCAST_CAP: usize = 5;
/// Cross-member RRF constant — "same constant as `semantic-index`" (D2).
const RRF_K: f64 = 60.0;

/// Why [`plan_route`] chose the targets it did.
#[derive(Debug, Clone, PartialEq)]
pub enum RouteReason {
    /// A member's name matched the query (normalized containment).
    Named,
    /// No member was named; these global entity ids (from the workspace KG)
    /// matched the query and were mapped through `entity_links` to member
    /// targets.
    KgEntities(Vec<i64>),
    /// Neither of the above (no KG, or no KG match): every ready member,
    /// capped and ordered by recent activity.
    Broadcast,
}

/// The outcome of [`plan_route`]: which member projects to search, and why.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutePlan {
    /// Member project ids to search, in priority order (used by
    /// [`merge_routed`] as a tie-break — earlier here wins a scoring tie).
    pub targets: Vec<Uuid>,
    pub reason: RouteReason,
}

/// Per-member outcome of an actual search attempt (design D5: "not-ready
/// members are skipped and reported", never block).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberStatus {
    /// The member was searched and its hits (if any) were merged in.
    Searched,
    /// The member's semantic index isn't ready yet; it was skipped.
    IndexBuilding,
    /// The member couldn't be searched for any other reason (DB error, or —
    /// Broadcast tier's D5 latency budget — a search that blew the
    /// ~150-200ms per-DB budget). Skipped, not blocking.
    Unavailable,
}

/// Plan-time metadata for one workspace member. `index_ready` and
/// `last_activity` are the caller's read of that member's actual state
/// (semantic index built, most recent ingest-completion timestamp per
/// `features/multi-project/README.md`'s "recent activity" contract) — this
/// module does no I/O to discover them itself, keeping `plan_route`
/// fixture-testable.
#[derive(Debug, Clone, PartialEq)]
pub struct MemberInfo {
    pub project_id: Uuid,
    pub name: String,
    /// Whether this member's semantic index is built and searchable. Only
    /// the Broadcast tier's candidate set is filtered on this at plan time
    /// (spec: "ready semantic indexes"); a KG-guided target can be
    /// not-ready — `execute_plan` reports it `IndexBuilding` rather than
    /// `plan_route` silently excluding a KG-selected member.
    pub index_ready: bool,
    /// Most recent ingest-completion timestamp (epoch millis or seconds —
    /// whatever unit the caller is consistent with; this module only ever
    /// compares it to other members' values).
    pub last_activity: i64,
}

/// Three-tier route planning (spec: "Three-tier route planning"). No I/O of
/// its own beyond reads through the caller-supplied `kg` handle — see the
/// module doc's "Deviations" for why `kg: Option<&WorkspaceKgDb>` rather
/// than a `KgHandle` abstraction.
///
/// 1. **Named**: any member whose normalized name is contained in the
///    normalized query. Short-circuits — if this tier finds anything, the
///    KG is never consulted (task 1.4 "named beats KG").
/// 2. **KG-guided**: only reached if `kg.is_some()` and no member was named.
///    Global entities whose normalized name is contained in the normalized
///    query are looked up via `WorkspaceKgDb::rank_projects_for_entities`,
///    ranked by link count then pointer density, mapped back to current
///    members, capped at [`KG_TARGET_CAP`].
/// 3. **Broadcast**: reached when neither tier above produced a target
///    (`kg` is `None`, the KG read failed, no entity matched, or matched
///    entities' projects aren't current members). Every `index_ready`
///    member, most-recent-activity first, capped at [`BROADCAST_CAP`].
pub fn plan_route(query: &str, members: &[MemberInfo], kg: Option<&WorkspaceKgDb>) -> RoutePlan {
    let normalized_query = normalize_name(query);

    let named: Vec<Uuid> = members
        .iter()
        .filter(|m| contains_normalized(&normalized_query, &normalize_name(&m.name)))
        .map(|m| m.project_id)
        .collect();
    if !named.is_empty() {
        return RoutePlan {
            targets: named,
            reason: RouteReason::Named,
        };
    }

    if let Some(kg) = kg {
        if let Some((targets, matched_ids)) = kg_guided_targets(&normalized_query, members, kg) {
            if !targets.is_empty() {
                return RoutePlan {
                    targets,
                    reason: RouteReason::KgEntities(matched_ids),
                };
            }
        }
    }

    let mut ready: Vec<&MemberInfo> = members.iter().filter(|m| m.index_ready).collect();
    ready.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));
    let targets = ready.into_iter().take(BROADCAST_CAP).map(|m| m.project_id).collect();
    RoutePlan {
        targets,
        reason: RouteReason::Broadcast,
    }
}

/// KG-guided tier body, split out of [`plan_route`] for readability. Returns
/// `None` when no global entity matched the query at all (so the caller
/// falls straight through to Broadcast without treating "matched entities
/// but none map to a current member" any differently from "no match" — both
/// end up at Broadcast, just via different `Some((vec![], _))` /`None` paths
/// that `plan_route` treats identically via `!targets.is_empty()`). A read
/// error against `kg` (corrupt DB, etc.) is treated the same as "no KG" —
/// routing must never fail outright over a KG read (design: "KG a soft
/// dependency").
fn kg_guided_targets(
    normalized_query: &str,
    members: &[MemberInfo],
    kg: &WorkspaceKgDb,
) -> Option<(Vec<Uuid>, Vec<i64>)> {
    let entities = kg.list_global_entities().ok()?;
    let matched_ids: Vec<i64> = entities
        .iter()
        .filter(|e| contains_normalized(normalized_query, &normalize_name(&e.name)))
        .map(|e| e.id)
        .collect();
    if matched_ids.is_empty() {
        return None;
    }

    let ranked = kg.rank_projects_for_entities(&matched_ids).ok()?;
    let member_ids: HashSet<Uuid> = members.iter().map(|m| m.project_id).collect();
    let targets: Vec<Uuid> = ranked
        .into_iter()
        .filter_map(|r| Uuid::parse_str(&r.project_id).ok())
        .filter(|pid| member_ids.contains(pid))
        .take(KG_TARGET_CAP)
        .collect();
    Some((targets, matched_ids))
}

/// Word-boundary-aware "does `haystack` contain `needle`" over
/// already-`normalize_name`-normalized (lowercased, single-spaced) text.
/// Padding both sides with a space before `contains` stops a short needle
/// from matching inside a longer token (`"art"` must not match inside
/// `"cart"`) while still letting a multi-word needle (`"shattered realms"`)
/// match as a contiguous run anywhere in the haystack. An empty needle never
/// matches (an unnamed member/entity must not swallow every query).
fn contains_normalized(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let padded_haystack = format!(" {haystack} ");
    let padded_needle = format!(" {needle} ");
    padded_haystack.contains(&padded_needle)
}

/// `ken://<project-id>/<rel-path>` (fixed scheme, `features/multi-project/
/// README.md` "Cross-feature contracts": "Rel-paths are always forward-slash
/// normalized, on Windows too").
fn ken_address(project_id: Uuid, rel_path: &str) -> String {
    format!("ken://{project_id}/{}", rel_path.replace('\\', "/"))
}

/// Run hybrid search for exactly one target member — the same
/// `search_chunks_fts` + `semantic_search` + `search::merge_and_rerank`
/// composition the src-tauri `hybrid_search` command (semantic-index task
/// 2.2) and the `engine.rs` test helper both already use, generalized so
/// this module (and later `ken-mcp`) can call one shared function instead
/// of a third copy. `query_vec` is the query embedded once by the caller
/// (design: "the query is embedded once and reused across all member KNN
/// searches") — pass `None` when semantic search isn't available for this
/// call (no embedder, or `db.vec_available()` is false); the FTS-only path
/// degrades exactly like the Tauri command's.
pub fn search_member(db: &Db, query: &str, query_vec: Option<&[f32]>, limit: usize) -> Result<Vec<HybridHit>> {
    let fts_hits = db.search_chunks_fts(query, limit)?;
    let vec_hits = match query_vec {
        Some(qv) if db.vec_available() => db
            .semantic_search(qv, limit)?
            .into_iter()
            .map(|(chunk_id, path, text, distance)| search::VecHit {
                chunk_id,
                path,
                text,
                distance,
            })
            .collect(),
        _ => Vec::new(),
    };
    Ok(search::merge_and_rerank(&fts_hits, &vec_hits, query))
}

/// A handle `execute_plan` needs to search one planned target: identity plus
/// enough to decide whether to search it at all. Distinct from
/// [`MemberInfo`] (plan-time-only metadata, no `Db`) because this one
/// borrows a live database connection.
pub struct MemberDbHandle<'a> {
    pub project_id: Uuid,
    pub name: &'a str,
    pub db: &'a Db,
    /// Mirrors [`MemberInfo::index_ready`] at execution time — a target can
    /// still be not-ready here even though it was a valid KG-guided pick at
    /// plan time (see `plan_route`'s doc comment).
    pub index_ready: bool,
}

/// One member's contribution to a routed search: either its ranked hits
/// (`status == Searched`) or an explanation of why it has none. This is
/// [`merge_routed`]'s actual input shape — a concurrent caller builds these
/// itself (one per target, via [`search_member`] fanned out however it
/// likes) instead of going through [`execute_plan`]'s sequential loop.
#[derive(Debug, Clone, PartialEq)]
pub struct MemberHits {
    pub project_id: Uuid,
    pub member_name: String,
    pub status: MemberStatus,
    /// Best-first, exactly as returned by [`search_member`]. Empty unless
    /// `status == Searched`.
    pub hits: Vec<HybridHit>,
}

/// One merged, cited search result (spec: "Every result is a cited
/// address").
#[derive(Debug, Clone, PartialEq)]
pub struct RoutedHit {
    pub path: String,
    pub chunk_id: i64,
    pub snippet: String,
    pub source: Source,
    pub project_id: Uuid,
    pub member_name: String,
    /// `ken://<project-id>/<rel-path>`.
    pub address: String,
    /// `kg://<entity-id>` per entity that selected this hit's plan (empty
    /// unless the plan's reason was `KgEntities` — see the module doc's
    /// "KG breadcrumbs are plan-level, not per-hit").
    pub kg_breadcrumbs: Vec<String>,
}

/// Per-member status entry in an [`ExecutionReport`] (spec: "per-member
/// status list").
#[derive(Debug, Clone, PartialEq)]
pub struct MemberStatusEntry {
    pub project_id: Uuid,
    pub member_name: String,
    pub status: MemberStatus,
}

/// Full result of routing + searching: the plan that produced it, the
/// merged cited hits, and what happened per member. Mirrors the
/// `{ plan, results, member_status }` shape `src-tauri`'s `route_search`
/// command (task 2.1) is expected to return.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionReport {
    pub plan: RoutePlan,
    pub results: Vec<RoutedHit>,
    pub member_status: Vec<MemberStatusEntry>,
}

/// Sequential convenience composition of [`search_member`] over every
/// `plan.targets` member found in `targets`, followed by [`merge_routed`].
/// Embeds the query once (`embedder.embed_query`) and reuses the vector for
/// every member, per design. See the module doc's "Concurrency is the
/// caller's, not this module's" for why this loop is sequential rather than
/// fanned out across threads, and how a caller that wants real concurrency
/// should instead call [`search_member`] itself per target and pass the
/// results straight to [`merge_routed`].
///
/// A planned target missing from `targets` (the caller didn't supply a
/// handle for it) is reported `Unavailable` rather than silently dropped —
/// D5's "skipped and reported, never blocking" applies to this gap too.
pub fn execute_plan(
    plan: &RoutePlan,
    targets: &[MemberDbHandle<'_>],
    embedder: &mut dyn Embedder,
    query: &str,
    limit: usize,
) -> ExecutionReport {
    let query_vec = embedder.embed_query(query).ok();

    let mut member_hits: Vec<MemberHits> = Vec::with_capacity(plan.targets.len());
    for project_id in &plan.targets {
        let Some(target) = targets.iter().find(|t| t.project_id == *project_id) else {
            member_hits.push(MemberHits {
                project_id: *project_id,
                member_name: String::new(),
                status: MemberStatus::Unavailable,
                hits: Vec::new(),
            });
            continue;
        };
        if !target.index_ready {
            member_hits.push(MemberHits {
                project_id: *project_id,
                member_name: target.name.to_string(),
                status: MemberStatus::IndexBuilding,
                hits: Vec::new(),
            });
            continue;
        }
        match search_member(target.db, query, query_vec.as_deref(), limit) {
            Ok(hits) => member_hits.push(MemberHits {
                project_id: *project_id,
                member_name: target.name.to_string(),
                status: MemberStatus::Searched,
                hits,
            }),
            Err(_) => member_hits.push(MemberHits {
                project_id: *project_id,
                member_name: target.name.to_string(),
                status: MemberStatus::Unavailable,
                hits: Vec::new(),
            }),
        }
    }

    merge_routed(plan, &member_hits, limit)
}

/// Cross-member RRF merge (design D2, spec "Fan-out hybrid search with
/// rank-only merge"): every `Searched` member's hit list is already ranked
/// best-first; each hit's cross-member score is `1 / (k + rank)` (`k` =
/// [`RRF_K`], `rank` 1-based within its own member's list) — raw scores
/// never enter this computation at all, because [`HybridHit`] doesn't carry
/// one. Ties (identical score — only possible across different members,
/// since ranks strictly decrease within one member's list) are broken by
/// the member's position in `plan.targets` (design: "tie-break by KG-target
/// rank when routed, member recent-activity otherwise" — both are exactly
/// what determined `plan.targets`' order in [`plan_route`], so reusing that
/// order here needs no extra parameter), then by within-member rank.
///
/// Pure: no I/O, no `Db`, no embedder — safe to call directly with fixture
/// [`MemberHits`] in tests.
pub fn merge_routed(plan: &RoutePlan, member_hits: &[MemberHits], limit: usize) -> ExecutionReport {
    let target_order: HashMap<Uuid, usize> = plan.targets.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    let breadcrumbs: Vec<String> = match &plan.reason {
        RouteReason::KgEntities(ids) => ids.iter().map(|id| format!("kg://{id}")).collect(),
        RouteReason::Named | RouteReason::Broadcast => Vec::new(),
    };

    // (score, target_order, within-member rank, hit) — sorted score DESC,
    // then the two tie-breaks ASC.
    let mut candidates: Vec<(f64, usize, usize, RoutedHit)> = Vec::new();
    for mh in member_hits {
        if mh.status != MemberStatus::Searched {
            continue;
        }
        let order = target_order.get(&mh.project_id).copied().unwrap_or(usize::MAX);
        for (i, hit) in mh.hits.iter().enumerate() {
            let rank = i + 1;
            let score = 1.0 / (RRF_K + rank as f64);
            let address = ken_address(mh.project_id, &hit.path);
            candidates.push((
                score,
                order,
                rank,
                RoutedHit {
                    path: hit.path.clone(),
                    chunk_id: hit.chunk_id,
                    snippet: hit.snippet.clone(),
                    source: hit.source,
                    project_id: mh.project_id,
                    member_name: mh.member_name.clone(),
                    address,
                    kg_breadcrumbs: breadcrumbs.clone(),
                },
            ));
        }
    }
    candidates.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.cmp(&b.1))
            .then(a.2.cmp(&b.2))
    });
    let results = candidates.into_iter().take(limit).map(|c| c.3).collect();

    let member_status = member_hits
        .iter()
        .map(|mh| MemberStatusEntry {
            project_id: mh.project_id,
            member_name: mh.member_name.clone(),
            status: mh.status,
        })
        .collect();

    ExecutionReport {
        plan: plan.clone(),
        results,
        member_status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedder::FakeEmbedder;

    fn member(project_id: Uuid, name: &str, index_ready: bool, last_activity: i64) -> MemberInfo {
        MemberInfo {
            project_id,
            name: name.to_string(),
            index_ready,
            last_activity,
        }
    }

    fn hit(path: &str, chunk_id: i64) -> HybridHit {
        HybridHit {
            path: path.to_string(),
            chunk_id,
            snippet: format!("snippet for {path}"),
            source: Source::Keyword,
        }
    }

    // --- plan_route table tests (task 1.4) ---

    #[test]
    fn named_tier_routes_directly_with_no_kg_lookup() {
        let shattered = Uuid::new_v4();
        let other = Uuid::new_v4();
        let members = vec![
            member(shattered, "Shattered Realms", true, 100),
            member(other, "Other Project", true, 200),
        ];
        // No `kg` handle at all: if Named didn't short-circuit before ever
        // touching `kg`, this would panic/error rather than route — passing
        // `None` here is itself part of the assertion that Named never
        // needs it.
        let plan = plan_route("what's new in Shattered Realms lately?", &members, None);
        assert_eq!(plan.targets, vec![shattered]);
        assert_eq!(plan.reason, RouteReason::Named);
    }

    #[test]
    fn kg_guided_tier_ranks_and_caps_at_three() {
        let kg = WorkspaceKgDb::open_in_memory().unwrap();
        let entity = kg
            .insert_global_entity("topic", "Zylographs", "a shared concept", 1)
            .unwrap();

        let p1 = Uuid::new_v4();
        let p2 = Uuid::new_v4();
        let p3 = Uuid::new_v4();
        let p4 = Uuid::new_v4();
        // p1: 1 link, 2 pointers (density leader among 1-link projects).
        kg.insert_entity_link(entity, p1, 1, "Zylographs").unwrap();
        kg.insert_doc_pointer(entity, p1, "a.md", "").unwrap();
        kg.insert_doc_pointer(entity, p1, "b.md", "").unwrap();
        // p2, p3, p4: 1 link, 0 pointers each — four candidates total, cap
        // must trim to 3.
        kg.insert_entity_link(entity, p2, 2, "Zylographs").unwrap();
        kg.insert_entity_link(entity, p3, 3, "Zylographs").unwrap();
        kg.insert_entity_link(entity, p4, 4, "Zylographs").unwrap();

        let members = vec![
            member(p1, "Alpha", true, 400),
            member(p2, "Bravo", true, 300),
            member(p3, "Charlie", true, 200),
            member(p4, "Delta", true, 100),
        ];
        let plan = plan_route("tell me about the zylographs", &members, Some(&kg));
        assert_eq!(plan.targets.len(), 3, "KG-guided tier must cap at 3");
        assert_eq!(plan.targets[0], p1, "highest pointer density must lead");
        assert_eq!(plan.reason, RouteReason::KgEntities(vec![entity]));
    }

    #[test]
    fn broadcast_tier_caps_at_five_by_recent_activity() {
        let members: Vec<MemberInfo> = (0..7)
            .map(|i| member(Uuid::new_v4(), &format!("Project {i}"), true, i))
            .collect();
        let plan = plan_route("nothing named or known here", &members, None);
        assert_eq!(plan.targets.len(), 5, "Broadcast tier must cap at 5");
        assert_eq!(plan.reason, RouteReason::Broadcast);
        // Most-recent-activity first: activity == index, so 6,5,4,3,2.
        let expected: Vec<Uuid> = members.iter().rev().take(5).map(|m| m.project_id).collect();
        assert_eq!(plan.targets, expected);
    }

    #[test]
    fn broadcast_tier_excludes_not_ready_members() {
        let ready = Uuid::new_v4();
        let building = Uuid::new_v4();
        let members = vec![member(ready, "Ready", true, 1), member(building, "Building", false, 2)];
        let plan = plan_route("unmatched query", &members, None);
        assert_eq!(plan.targets, vec![ready]);
    }

    #[test]
    fn kg_unavailable_degrades_to_broadcast() {
        let members = vec![member(Uuid::new_v4(), "Alpha", true, 1)];
        // `kg: None` — flag off / KG not built. Must not error, must not
        // hang: falls straight to Broadcast.
        let plan = plan_route("some query that matches nothing named", &members, None);
        assert_eq!(plan.reason, RouteReason::Broadcast);
    }

    #[test]
    fn kg_match_with_no_current_member_falls_back_to_broadcast() {
        let kg = WorkspaceKgDb::open_in_memory().unwrap();
        let entity = kg.insert_global_entity("topic", "Ghost Project", "s", 1).unwrap();
        // Linked only to a project that is NOT in the current members list
        // (e.g. removed from the workspace since the KG was last built).
        kg.insert_entity_link(entity, Uuid::new_v4(), 1, "Ghost Project").unwrap();

        let broadcastable = Uuid::new_v4();
        let members = vec![member(broadcastable, "Alpha", true, 1)];
        let plan = plan_route("what about the ghost project?", &members, Some(&kg));
        assert_eq!(plan.reason, RouteReason::Broadcast);
        assert_eq!(plan.targets, vec![broadcastable]);
    }

    #[test]
    fn named_beats_kg_even_when_both_would_match() {
        let kg = WorkspaceKgDb::open_in_memory().unwrap();
        let named_project = Uuid::new_v4();
        let kg_project = Uuid::new_v4();
        // A KG entity that would route to `kg_project` if reached.
        let entity = kg.insert_global_entity("topic", "Widgets", "s", 1).unwrap();
        kg.insert_entity_link(entity, kg_project, 1, "Widgets").unwrap();

        let members = vec![
            member(named_project, "Widgets Factory", true, 1),
            member(kg_project, "Somewhere Else", true, 2),
        ];
        // Query names a member directly AND matches the KG entity "Widgets".
        let plan = plan_route("status update from Widgets Factory", &members, Some(&kg));
        assert_eq!(plan.targets, vec![named_project]);
        assert_eq!(plan.reason, RouteReason::Named);
    }

    // --- merge_routed: cross-member RRF ordering (task 1.4) ---

    #[test]
    fn cross_member_merge_is_rank_based_not_score_based() {
        // Spec scenario: member A's top hit merges as rank-1 even though
        // HybridHit carries no raw score at all to compare — there is
        // nothing here but each hit's position in its own member's list.
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let plan = RoutePlan {
            targets: vec![a, b],
            reason: RouteReason::Broadcast,
        };
        let member_hits = vec![
            MemberHits {
                project_id: a,
                member_name: "A".to_string(),
                status: MemberStatus::Searched,
                hits: vec![hit("a/one.md", 1), hit("a/two.md", 2)],
            },
            MemberHits {
                project_id: b,
                member_name: "B".to_string(),
                status: MemberStatus::Searched,
                hits: vec![hit("b/one.md", 10), hit("b/two.md", 11), hit("b/three.md", 12)],
            },
        ];
        let report = merge_routed(&plan, &member_hits, 10);
        // Rank-1 hits from both members tie in score; A leads on
        // target_order (A is plan.targets[0]).
        assert_eq!(report.results[0].path, "a/one.md");
        assert_eq!(report.results[1].path, "b/one.md");
        // Rank-2 hits come next, same tie-break.
        assert_eq!(report.results[2].path, "a/two.md");
        assert_eq!(report.results[3].path, "b/two.md");
        // B's rank-3 hit (no A counterpart) is last.
        assert_eq!(report.results[4].path, "b/three.md");
    }

    #[test]
    fn limit_truncates_merged_results() {
        let a = Uuid::new_v4();
        let plan = RoutePlan {
            targets: vec![a],
            reason: RouteReason::Broadcast,
        };
        let member_hits = vec![MemberHits {
            project_id: a,
            member_name: "A".to_string(),
            status: MemberStatus::Searched,
            hits: vec![hit("one.md", 1), hit("two.md", 2), hit("three.md", 3)],
        }];
        let report = merge_routed(&plan, &member_hits, 2);
        assert_eq!(report.results.len(), 2);
    }

    // --- execute_plan: not-ready member skipped + reported (task 1.4) ---

    #[test]
    fn not_ready_member_is_skipped_and_reported() {
        let ready_id = Uuid::new_v4();
        let building_id = Uuid::new_v4();
        let plan = RoutePlan {
            targets: vec![ready_id, building_id],
            reason: RouteReason::Broadcast,
        };

        let ready_db = fixture_db_with_chunk("notes/found.md", "the quokka naps");
        // Empty DB is fine — `index_ready: false` means it's never touched.
        let building_db = Db::open_in_memory().unwrap();

        let targets = vec![
            MemberDbHandle {
                project_id: ready_id,
                name: "Ready",
                db: &ready_db,
                index_ready: true,
            },
            MemberDbHandle {
                project_id: building_id,
                name: "Building",
                db: &building_db,
                index_ready: false,
            },
        ];
        let mut embedder = FakeEmbedder::new();
        let report = execute_plan(&plan, &targets, &mut embedder, "quokka", 10);

        assert_eq!(report.member_status.len(), 2);
        let ready_status = report
            .member_status
            .iter()
            .find(|s| s.project_id == ready_id)
            .unwrap();
        assert_eq!(ready_status.status, MemberStatus::Searched);
        let building_status = report
            .member_status
            .iter()
            .find(|s| s.project_id == building_id)
            .unwrap();
        assert_eq!(building_status.status, MemberStatus::IndexBuilding);

        // Only the ready member's hit made it into the merged results.
        assert!(report.results.iter().all(|r| r.project_id == ready_id));
        assert!(report.results.iter().any(|r| r.path == "notes/found.md"));
    }

    #[test]
    fn missing_target_handle_is_reported_unavailable() {
        let a = Uuid::new_v4();
        let missing = Uuid::new_v4();
        let plan = RoutePlan {
            targets: vec![a, missing],
            reason: RouteReason::Broadcast,
        };
        let db = fixture_db_with_chunk("x.md", "hello world");
        let targets = vec![MemberDbHandle {
            project_id: a,
            name: "A",
            db: &db,
            index_ready: true,
        }];
        let mut embedder = FakeEmbedder::new();
        let report = execute_plan(&plan, &targets, &mut embedder, "hello", 10);
        let missing_status = report
            .member_status
            .iter()
            .find(|s| s.project_id == missing)
            .unwrap();
        assert_eq!(missing_status.status, MemberStatus::Unavailable);
    }

    // --- citation address integrity (task 1.4) ---

    #[test]
    fn every_hit_carries_a_resolvable_ken_address() {
        let a = Uuid::new_v4();
        let plan = RoutePlan {
            targets: vec![a],
            reason: RouteReason::KgEntities(vec![42]),
        };
        let member_hits = vec![MemberHits {
            project_id: a,
            member_name: "A".to_string(),
            status: MemberStatus::Searched,
            hits: vec![hit("notes\\windows-style.md", 1)],
        }];
        let report = merge_routed(&plan, &member_hits, 10);
        let result = &report.results[0];
        let expected = format!("ken://{a}/notes/windows-style.md");
        assert_eq!(result.address, expected, "backslashes must normalize to forward slashes");

        // Round-trip: strip the scheme, split project id from rel path.
        let rest = result.address.strip_prefix("ken://").unwrap();
        let (project_part, path_part) = rest.split_once('/').unwrap();
        assert_eq!(Uuid::parse_str(project_part).unwrap(), a);
        assert_eq!(path_part, "notes/windows-style.md");

        // KG-routed plan: breadcrumb present and well-formed.
        assert_eq!(result.kg_breadcrumbs, vec!["kg://42".to_string()]);
    }

    #[test]
    fn named_and_broadcast_plans_carry_no_breadcrumbs() {
        let a = Uuid::new_v4();
        let plan = RoutePlan {
            targets: vec![a],
            reason: RouteReason::Named,
        };
        let member_hits = vec![MemberHits {
            project_id: a,
            member_name: "A".to_string(),
            status: MemberStatus::Searched,
            hits: vec![hit("x.md", 1)],
        }];
        let report = merge_routed(&plan, &member_hits, 10);
        assert!(report.results[0].kg_breadcrumbs.is_empty());
    }

    /// A real (tempdir-backed, in-memory-DB) member fixture with one
    /// FTS+KNN-indexed file at `rel_path`, for tests that need
    /// `search_member`/`execute_plan` to do an actual search rather than
    /// working from fixture `HybridHit`s directly (`merge_routed`'s tests
    /// don't need this — it's pure).
    fn fixture_db_with_chunk(rel_path: &str, text: &str) -> Db {
        use crate::project::Project;
        use crate::runner::CancelToken;
        use crate::{engine, scan};

        let project_dir = tempfile::tempdir().unwrap();
        let file_path = project_dir.path().join(rel_path);
        std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
        std::fs::write(&file_path, format!("# note\n{text}\n")).unwrap();
        let project = Project::create(project_dir.path(), "T").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        scan::scan(&project, &mut db).unwrap();
        let mut embedder = FakeEmbedder::new();
        engine::rebuild_semantic_index(&project, &mut db, &mut embedder, &CancelToken::new(), |_, _| {}).unwrap();
        db
    }
}

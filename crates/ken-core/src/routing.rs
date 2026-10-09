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
use std::path::Path;

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
    /// The question asks about the platform the team builds on
    /// ([`asks_about_platform`]): a reference repo's assets then rank as
    /// equals of the team's own files.
    pub platform: bool,
    /// The targets that are the team's knowledge base
    /// ([`MemberInfo::knowledge_base`]): they keep a share of the merged
    /// results ([`KB_LEAD`]). A member the registry calls a team or wiki
    /// repo counts too, so a plan made by hand needs not list it.
    pub knowledge: Vec<Uuid>,
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
    /// A team or wiki repo: the team's knowledge base. Searched on every
    /// unnamed query, beside the capped members. Measured 2026-10-07: with
    /// six members the cap of five dropped the team's wiki, and 0 of 31
    /// questions answered by it reached `route_query`'s results.
    pub knowledge_base: bool,
}

/// Whether a member of these kinds is the team's knowledge base
/// ([`MemberInfo::knowledge_base`]).
pub fn is_knowledge_base(kind: &[crate::registry::RepoKind]) -> bool {
    kind.iter().any(|k| k.reads_entities())
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
///    ranked by link count then pointer density, then the projects of the
///    entities Claude's map or the judge related to them (never
///    co-occurrence), mapped back to current members, capped at
///    [`KG_TARGET_CAP`].
/// 3. **Broadcast**: reached when neither tier above produced a target
///    (`kg` is `None`, the KG read failed, no entity matched, or matched
///    entities' projects aren't current members). Every `index_ready`
///    member, most-recent-activity first, capped at [`BROADCAST_CAP`].
pub fn plan_route(query: &str, members: &[MemberInfo], kg: Option<&WorkspaceKgDb>) -> RoutePlan {
    let normalized_query = normalize_name(query);
    let platform = asks_about_platform(query);

    let named: Vec<Uuid> = members
        .iter()
        .filter(|m| contains_normalized(&normalized_query, &normalize_name(&m.name)))
        .map(|m| m.project_id)
        .collect();
    if !named.is_empty() {
        let knowledge = members.iter().filter(|m| m.knowledge_base && named.contains(&m.project_id)).map(|m| m.project_id).collect();
        return RoutePlan {
            targets: named,
            reason: RouteReason::Named,
            platform,
            knowledge,
        };
    }

    let mut ready: Vec<&MemberInfo> = members.iter().filter(|m| m.index_ready).collect();
    ready.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));
    // The knowledge base is always searched, outside the cap.
    let knowledge: Vec<Uuid> = ready.iter().filter(|m| m.knowledge_base).map(|m| m.project_id).collect();

    if let Some(kg) = kg {
        if let Some((mut targets, matched_ids)) = kg_guided_targets(&normalized_query, members, kg) {
            if !targets.is_empty() {
                // The graph says where to look first, not where not to look:
                // it holds no code repo at all (their files are never mined
                // for entities) and misses what extraction missed. The other
                // ready members follow, up to the broadcast cap, so "how is
                // the Entra ID token validated" still reaches the code.
                for m in &ready {
                    if targets.len() >= BROADCAST_CAP.max(KG_TARGET_CAP) {
                        break;
                    }
                    if !targets.contains(&m.project_id) {
                        targets.push(m.project_id);
                    }
                }
                for id in &knowledge {
                    if !targets.contains(id) {
                        targets.push(*id);
                    }
                }
                return RoutePlan {
                    targets,
                    reason: RouteReason::KgEntities(matched_ids),
                    platform,
                    knowledge,
                };
            }
        }
    }

    let mut targets = knowledge.clone();
    targets.extend(ready.into_iter().filter(|m| !m.knowledge_base).take(BROADCAST_CAP).map(|m| m.project_id));
    RoutePlan {
        targets,
        reason: RouteReason::Broadcast,
        platform,
        knowledge,
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

    // The named entities' projects first, then the projects of what Claude's
    // map or the judge related to them; a co-occurrence edge only says two
    // names share files, so it never steers a route.
    let mut ranked = kg.rank_projects_for_entities(&matched_ids).ok()?;
    let related = kg.strongly_related(&matched_ids).unwrap_or_default();
    ranked.extend(kg.rank_projects_for_entities(&related).unwrap_or_default());
    let member_ids: HashSet<Uuid> = members.iter().map(|m| m.project_id).collect();
    let mut targets: Vec<Uuid> = Vec::new();
    for pid in ranked.into_iter().filter_map(|r| Uuid::parse_str(&r.project_id).ok()) {
        if targets.len() < KG_TARGET_CAP && member_ids.contains(&pid) && !targets.contains(&pid) {
            targets.push(pid);
        }
    }
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

/// How a hit is cited so a reader can go straight to it: `repo:path:line`,
/// with `repo@sha` when the repo is git, and led by `[[Note Name]]` for a
/// page in a team or wiki repo. `line` is None for a chunk stored before
/// lines were recorded, which drops the `:line`.
pub fn locator(repo: &str, rel_path: &str, line: Option<i64>, sha: Option<&str>, wiki_page: bool) -> String {
    let path = rel_path.replace('\\', "/");
    let repo = match sha {
        Some(sha) => format!("{repo}@{sha}"),
        None => repo.to_string(),
    };
    let mut out = match line {
        Some(n) => format!("{repo}:{path}:{n}"),
        None => format!("{repo}:{path}"),
    };
    if wiki_page {
        let is_page = path.ends_with(".md") || path.ends_with(".markdown");
        if let Some(stem) = std::path::Path::new(&path).file_stem().and_then(|s| s.to_str()).filter(|_| is_page) {
            out = format!("[[{stem}]] · {out}");
        }
    }
    out
}

/// The short commit a git repo's working tree is on, read from `.git` files
/// rather than by running git, so a search never waits on a process. None
/// for a repo that is not git, a detached worktree (`.git` is a file), or an
/// unborn branch.
pub fn head_sha(root: &Path) -> Option<String> {
    let git = root.join(".git");
    if !git.is_dir() {
        return None;
    }
    let head = std::fs::read_to_string(git.join("HEAD")).ok()?;
    let head = head.trim();
    let full = match head.strip_prefix("ref: ") {
        None => head.to_string(),
        Some(r) => match std::fs::read_to_string(git.join(r)) {
            Ok(s) => s.trim().to_string(),
            Err(_) => {
                let packed = std::fs::read_to_string(git.join("packed-refs")).ok()?;
                packed
                    .lines()
                    .find_map(|l| l.strip_suffix(r).map(|sha| sha.trim().to_string()))?
            }
        },
    };
    (full.len() >= 7 && full.chars().all(|c| c.is_ascii_hexdigit())).then(|| full[..7].to_string())
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
    search_member_with(db, query, query_vec, limit, None)
}

/// [`search_member`] with the workspace's vocabulary as well as the
/// member's own: a Vocabulary page in the team's wiki then widens the search
/// in every repo, not only in the wiki. Routed searches build it once with
/// [`workspace_vocabulary`] and pass it to every member.
pub fn search_member_with(
    db: &Db,
    query: &str,
    query_vec: Option<&[f32]>,
    limit: usize,
    shared: Option<&crate::vocab::Vocabulary>,
) -> Result<Vec<HybridHit>> {
    let mut fts_hits = db.search_chunks_fts(query, limit)?;
    // The team's other words for what was asked (Vocabulary page, decisions
    // aliases, page aliases): each alternative phrasing runs as its own
    // keyword search, its new chunks after the original's.
    let own = crate::vocab::Vocabulary::cached(db)?;
    let vocab = match shared {
        Some(s) => crate::vocab::Vocabulary::merged([&own, s]),
        None => own,
    };
    for alt in vocab.alternatives(query) {
        for hit in db.search_chunks_fts(&alt, limit)? {
            if !fts_hits.iter().any(|h| h.chunk_id == hit.chunk_id) {
                fts_hits.push(hit);
            }
        }
    }
    // Meaning only from a finished meaning index: a half-built one holds
    // whatever was embedded first (asset JSON, in path order) and handed
    // the meaning bonus to those files alone.
    let query_vec = match query_vec {
        Some(qv) if db.vec_available() && db.semantic_built_at()?.is_some() => Some(qv),
        _ => None,
    };
    let vec_hits: Vec<search::VecHit> = match query_vec {
        Some(qv) => db
            .semantic_search(qv, limit)?
            .into_iter()
            .map(|(chunk_id, path, text, distance)| search::VecHit {
                chunk_id,
                path,
                text,
                distance,
            })
            .collect(),
        None => Vec::new(),
    };
    // Every keyword hit with a stored vector is weighed by meaning too, so
    // relevance is on one scale whichever list a hit came from.
    let mut extra: HashMap<String, f64> = HashMap::new();
    if let Some(qv) = query_vec {
        let missing: Vec<i64> = fts_hits
            .iter()
            .filter(|f| !vec_hits.iter().any(|v| v.chunk_id == f.chunk_id))
            .map(|f| f.chunk_id)
            .collect();
        let dist = db.chunk_distances(qv, &missing)?;
        for f in &fts_hits {
            if let Some(&d) = dist.get(&f.chunk_id) {
                let e = extra.entry(f.path.clone()).or_insert(d);
                if d < *e {
                    *e = d;
                }
            }
        }
    }
    // Each file's authority (`crate::authority`): rules and rulings count for
    // more, tickets and dated evidence for less, inside this member's own
    // ranking. An index from before authority was stored ranks by the page's
    // band instead, as it did.
    let paths: Vec<String> = fts_hits.iter().map(|h| h.path.clone()).chain(vec_hits.iter().map(|h| h.path.clone())).collect();
    let authority = db.authority_weights(&paths)?;
    let mut hits = search::merge_and_rerank_with(&fts_hits, &vec_hits, query, &extra, authority.as_ref().unwrap_or(&HashMap::new()));
    for hit in &mut hits {
        hit.line = db.chunk_line(hit.chunk_id)?;
        hit.page = crate::pagemeta::hit_page(&hit.path, db.page_meta(&hit.path)?);
    }
    // Binding and verified pages count for more, evidence and pages no longer
    // current for less: a nudge on relevance, not a wall. A strict band order
    // put a generic process page that barely matched above the page that
    // answers the question.
    let intent = query_intent(query);
    let now = crate::engine::now_epoch();
    for hit in &mut hits {
        let band = if authority.is_some() { 0.0 } else { band_bonus(hit) };
        hit.score += band + hygiene(hit, now) + intent_bonus(intent, &hit.path);
    }
    hits.sort_by(|a, b| b.score.total_cmp(&a.score));
    follow_links(db, &mut hits)?;
    find_symbols(db, query, intent, &mut hits)?;
    // A ruling a later one supersedes is shown under it (`supersede`).
    crate::supersede::lead_with_successors(db, &mut hits)?;
    // Each page hit says what it is and how fresh: a ruling and its date, a
    // ticket and its status (`authority::hit_label`).
    for hit in &mut hits {
        if let Some(page) = hit.page.as_mut() {
            page.label = crate::authority::hit_label(db, &hit.path, Some(&hit.snippet))?;
        }
    }
    Ok(hits)
}

/// What a definition of a symbol the query names adds: as much as a file
/// name match, so "where is get_current_user" leads with its definition.
pub const W_SYMBOL: f64 = 3.5;

/// The code map as a signal: a query that names a symbol (a word shaped
/// like an identifier: `snake_case`, `camelCase`, `Type::name`) puts where it
/// is defined among the hits, at its line. Plain words are left to the text
/// layers: looked up as symbols, "where", "and" and "set" each pulled five
/// unrelated definitions to the top with a file name's weight.
fn find_symbols(db: &Db, query: &str, intent: Option<Intent>, hits: &mut Vec<HybridHit>) -> Result<()> {
    if intent == Some(Intent::Prose) {
        return Ok(());
    }
    let identifier = |w: &str| {
        w.contains('_') || w.contains("::") || w.chars().zip(w.chars().skip(1)).any(|(a, b)| a.is_lowercase() && b.is_uppercase())
    };
    let words: Vec<String> = query
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':'))
        .map(|w| w.trim_matches(':').to_string())
        .filter(|w| w.len() >= 3)
        .filter(|w| identifier(w))
        .filter(|w| !crate::db::significant_tokens(w).is_empty())
        .collect();
    let mut changed = false;
    for word in words.iter().take(4) {
        let name = word.rsplit("::").next().unwrap_or(word);
        for def in db.code_symbols(Some(name), None, Some(true), 5)? {
            changed = true;
            if let Some(hit) = hits.iter_mut().find(|h| h.path == def.path) {
                hit.score += W_SYMBOL;
                hit.line = Some(def.line);
            } else if let Some((chunk_id, text)) = db.chunk_at_line(&def.path, def.line)? {
                hits.push(HybridHit {
                    path: def.path.clone(),
                    chunk_id,
                    snippet: text,
                    source: search::Source::Keyword,
                    line: Some(def.line),
                    page: None,
                    score: W_SYMBOL,
                });
            }
        }
    }
    if changed {
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
    }
    Ok(())
}

/// What a page linked with a top hit adds: the wiki's own "see also".
pub const W_LINK: f64 = 0.4;
/// Pages the top hits link to or from, joined when search missed them.
const LINKED_ADDED: usize = 3;

/// The wiki's links as a signal: pages linked to or from the best page hits
/// rank a little higher, and a few the search missed join at the end
/// ("Permit to Build" beside the Change Order that names it). Pages only;
/// the text layers already found everything else.
fn follow_links(db: &Db, hits: &mut Vec<HybridHit>) -> Result<()> {
    let is_page = |p: &str| p.ends_with(".md") || p.ends_with(".markdown");
    let top: Vec<String> = hits.iter().filter(|h| is_page(&h.path)).take(3).map(|h| h.path.clone()).collect();
    if top.is_empty() {
        return Ok(());
    }
    let links = db.all_page_links()?;
    if links.is_empty() {
        return Ok(());
    }
    let resolver = crate::links::Resolver::from_db(db)?;
    let mut neighbours: Vec<String> = Vec::new();
    for (from, link) in &links {
        let to = resolver.resolve(link);
        let from_top = top.contains(from);
        for t in &to {
            let other = if from_top && !top.contains(t) {
                Some(t.clone())
            } else if top.contains(t) && !top.contains(from) {
                Some(from.clone())
            } else {
                None
            };
            if let Some(o) = other.filter(|o| !neighbours.contains(o)) {
                neighbours.push(o);
            }
        }
    }
    // A "see also" never passes the page that points to it: a linked page
    // rises among the rest, up to just under the lowest of the top pages.
    let ceiling = hits
        .iter()
        .filter(|h| top.contains(&h.path))
        .map(|h| h.score)
        .fold(f64::INFINITY, f64::min)
        - 1e-6;
    let mut added = 0;
    for n in &neighbours {
        if let Some(hit) = hits.iter_mut().find(|h| &h.path == n) {
            hit.score = (hit.score + W_LINK).min(ceiling.max(hit.score));
        } else if added < LINKED_ADDED {
            if let Some((chunk_id, text)) = db.first_chunk(n)? {
                added += 1;
                hits.push(HybridHit {
                    path: n.clone(),
                    chunk_id,
                    snippet: text,
                    source: search::Source::Keyword,
                    line: db.chunk_line(chunk_id)?,
                    page: crate::pagemeta::hit_page(n, db.page_meta(n)?),
                    score: W_LINK,
                });
            }
        }
    }
    hits.sort_by(|a, b| b.score.total_cmp(&a.score));
    Ok(())
}

/// What a question is after, read from its words: how code works, or what
/// people wrote and decided. A nudge toward those kinds, never a filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    Code,
    Prose,
}

const CODE_WORDS: &[&str] = &[
    "implement", "implemented", "implementation", "function", "method", "class", "struct", "trait", "interface",
    "defined", "definition", "declared", "call", "calls", "called", "caller", "callers", "returns", "endpoint",
    "handler", "module", "import", "imports", "variable", "compile", "exception", "stacktrace", "code", "api",
];
const PROSE_WORDS: &[&str] = &[
    "who", "when", "why", "decided", "decision", "decisions", "meeting", "owner", "owns", "deadline", "budget",
    "fee", "cost", "agreed", "policy", "roadmap", "milestone", "status", "plan", "contract", "client",
];

/// The [`Intent`] a query shows, if it shows one clearly. An identifier in
/// the query (`get_user`, `LlmGateway`, `app::core`, `auth.py`) counts as code.
pub fn query_intent(query: &str) -> Option<Intent> {
    let words: Vec<&str> = query.split(|c: char| c.is_whitespace() || matches!(c, '?' | ',' | '"')).filter(|w| !w.is_empty()).collect();
    let lower = |w: &str| w.to_lowercase();
    let identifier = words.iter().any(|w| {
        let w = w.trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != ':' && c != '.');
        w.contains("::")
            || w.contains("()")
            || (w.contains('_') && w.len() > 3)
            || w.chars().zip(w.chars().skip(1)).any(|(a, b)| a.is_lowercase() && b.is_uppercase())
            || [".rs", ".py", ".ts", ".tsx", ".js", ".go", ".java", ".cs"].iter().any(|e| w.ends_with(e))
    });
    let code = identifier || words.iter().any(|w| CODE_WORDS.contains(&lower(w).as_str()));
    let prose = words.iter().any(|w| PROSE_WORDS.contains(&lower(w).as_str()));
    match (code, prose) {
        (true, false) => Some(Intent::Code),
        (false, true) => Some(Intent::Prose),
        _ => None,
    }
}

/// What a hit's kind adds when it is the kind the question is after.
pub const W_INTENT: f64 = 0.5;

fn intent_bonus(intent: Option<Intent>, path: &str) -> f64 {
    use crate::contenttype::ContentType as T;
    match (intent, crate::contenttype::of(path)) {
        (Some(Intent::Code), T::Code | T::Test) => W_INTENT,
        (Some(Intent::Prose), T::Doc | T::Meeting | T::Spec | T::Ticket) => W_INTENT,
        _ => 0.0,
    }
}

/// Machine-written text (an inlined search index, a data dump in one line)
/// matches everything a little and answers nothing: well under any real hit.
pub const W_BLOB: f64 = 1.5;
/// A page verified this long ago counts a little less.
pub const STALE_DAYS: i64 = 180;
pub const W_STALE: f64 = 0.25;

/// A page still titled with a `{{placeholder}}` is a template to copy: it
/// reads like every question and answers none.
pub const W_TEMPLATE: f64 = 2.0;

/// What text that no person wrote, a template, and a page long unchecked,
/// take off.
fn hygiene(hit: &HybridHit, now: i64) -> f64 {
    let mut s = 0.0;
    if machine_written(&hit.snippet) {
        s -= W_BLOB;
    }
    if hit.page.as_ref().and_then(|p| p.title.as_deref()).is_some_and(|t| t.contains("{{")) {
        s -= W_TEMPLATE;
    }
    let stale = hit
        .page
        .as_ref()
        .and_then(|p| p.verified.as_deref())
        .and_then(|v| crate::drift::days_between(v, now))
        .is_some_and(|d| d > STALE_DAYS);
    if stale {
        s -= W_STALE;
    }
    s
}

/// One very long line, or a long chunk with almost no line breaks: written
/// by a tool, not a person.
pub fn machine_written(text: &str) -> bool {
    let longest = text.lines().map(|l| l.chars().count()).max().unwrap_or(0);
    longest > 1000 || (text.len() > 1500 && text.lines().count() < 4)
}

/// [`search_member`] keeping only hits of the content types asked for (none
/// asked for keeps all). It reads five times deeper when filtering, so a
/// "code only" search in a repo that is mostly docs still fills the page.
pub fn search_member_of(
    db: &Db,
    query: &str,
    query_vec: Option<&[f32]>,
    limit: usize,
    types: &[crate::contenttype::ContentType],
) -> Result<Vec<HybridHit>> {
    search_member_of_with(db, query, query_vec, limit, types, None)
}

/// [`search_member_of`] with the workspace's vocabulary (see
/// [`search_member_with`]).
pub fn search_member_of_with(
    db: &Db,
    query: &str,
    query_vec: Option<&[f32]>,
    limit: usize,
    types: &[crate::contenttype::ContentType],
    shared: Option<&crate::vocab::Vocabulary>,
) -> Result<Vec<HybridHit>> {
    if types.is_empty() {
        return search_member_with(db, query, query_vec, limit, shared);
    }
    let mut hits = search_member_with(db, query, query_vec, limit * 5, shared)?;
    hits.retain(|h| crate::contenttype::is_wanted(&h.path, types));
    hits.truncate(limit);
    Ok(hits)
}

/// One vocabulary for a routed search: every member's stored vocabulary
/// together, so the team wiki's Vocabulary page and decisions aliases widen
/// the search in the code repos too. Members are read once per search.
pub fn workspace_vocabulary<'a>(dbs: impl IntoIterator<Item = &'a Db>) -> crate::vocab::Vocabulary {
    let vocabs: Vec<crate::vocab::Vocabulary> =
        dbs.into_iter().filter_map(|db| crate::vocab::Vocabulary::cached(db).ok()).collect();
    crate::vocab::Vocabulary::merged(vocabs.iter())
}

/// What a reference repo's asset gives up when the question is about the
/// team's own work: less than one filename word. On 2026-10-06 the JSON
/// headers and the `names` column put the game's own `Weapon_Shield_Copper.json`
/// and `Coins.json` above the mod's code for "how much stamina does it cost to
/// take a hit while holding up a weapon or shield" and "how many coins do you
/// get for breaking an item down": an asset named for a question's nouns
/// matches it in name only.
pub const W_REFERENCE_ASSET: f64 = 2.0;

/// Words that put a question on the platform rather than on the team's own
/// work.
const PLATFORM_WORDS: &[&str] =
    &["engine", "platform", "built-in", "builtin", "vanilla", "base game", "out of the box", "upstream", "sdk", "framework"];

/// Whether `query` asks about the platform the team builds on ("does the
/// engine have a boss health bar", "the game's built-in minimap").
pub fn asks_about_platform(query: &str) -> bool {
    let words: String = query.to_lowercase().chars().map(|c| if c.is_alphanumeric() || c == '-' { c } else { ' ' }).collect();
    let words = format!(" {} ", words.split_whitespace().collect::<Vec<_>>().join(" "));
    PLATFORM_WORDS.iter().any(|w| words.contains(&format!(" {w} ")))
}

/// Whether `path`, in a repo of `kinds`, is a reference repo's asset (its
/// JSON, YAML and data files) that `plan` does not ask about: a question
/// that names the repo, or the platform, wants them as much as anything.
fn reference_asset(plan: &RoutePlan, kinds: &[crate::registry::RepoKind], path: &str) -> bool {
    use crate::contenttype::ContentType as T;
    kinds.contains(&crate::registry::RepoKind::Reference)
        && !plan.platform
        && plan.reason != RouteReason::Named
        && matches!(crate::contenttype::of(path), T::Config | T::Data)
}

/// What a page's band adds to its relevance: less than one filename word.
pub const W_BAND: f64 = 0.75;

fn band_bonus(hit: &HybridHit) -> f64 {
    match hit.page.as_ref().map_or(1, |p| p.band) {
        0 => W_BAND,
        2 => -W_BAND,
        _ => 0.0,
    }
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
    /// The line the hit's chunk starts on, when known.
    pub line: Option<i64>,
    /// The human citation, see [`locator`].
    pub locator: String,
    /// For a Markdown page: section, verified or evidence date, and whether
    /// it is retired or generated.
    pub page: Option<crate::pagemeta::HitPage>,
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
    let vocab = workspace_vocabulary(targets.iter().map(|t| t.db));

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
        match search_member_with(target.db, query, query_vec.as_deref(), limit, Some(&vocab)) {
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
    let mut candidates: Vec<(f64, usize, usize, RoutedHit, f64, bool)> = Vec::new();
    for mh in member_hits {
        if mh.status != MemberStatus::Searched {
            continue;
        }
        // Once per member: its commit, its kinds, and whether its pages are
        // notes. A member missing from the registry is cited as plain
        // repo:path:line.
        let (sha, kinds) = match crate::registry::entry_of(mh.project_id) {
            Some((root, kind)) => (head_sha(&root), kind),
            None => (None, Vec::new()),
        };
        let wiki = kinds.iter().any(|k| matches!(k, crate::registry::RepoKind::Team | crate::registry::RepoKind::Wiki));
        let knowledge = plan.knowledge.contains(&mh.project_id) || is_knowledge_base(&kinds);
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
                    line: hit.line,
                    locator: locator(&mh.member_name, &hit.path, hit.line, sha.as_deref(), wiki),
                    page: hit.page.clone(),
                    kg_breadcrumbs: breadcrumbs.clone(),
                },
                hit.score - if reference_asset(plan, &kinds, &hit.path) { W_REFERENCE_ASSET } else { 0.0 },
                knowledge,
            ));
        }
    }
    // Relevance leads across members: every member scores on the same scale
    // (the page band already folded in), so the best answer wins whichever
    // repo holds it. Rank-only fusion gave each repo's first hit the same
    // weight: a code repo's best guess at a contract question tied the
    // contract. The band breaks ties between unscored hits; then RRF, the
    // plan's order and the member's own rank.
    let band = |h: &RoutedHit| h.page.as_ref().map_or(1, |p| p.band);
    candidates.sort_by(|a, b| {
        b.4.total_cmp(&a.4)
            .then(band(&a.3).cmp(&band(&b.3)))
            .then(b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal))
            .then(a.1.cmp(&b.1))
            .then(a.2.cmp(&b.2))
    });
    // One copy of a file kept in several repos (a vendored PDF, a design
    // asset): the same name and the same text is one answer, at its best rank.
    // A longer passage is the same answer wherever it sits (a copied doc, a
    // second checkout); a short one only when the file name matches too.
    let mut seen: std::collections::HashSet<(String, String)> = Default::default();
    let ranked: Vec<Ranked> = candidates
        .into_iter()
        .map(|c| Ranked { rank: c.2, hit: c.3, score: c.4, knowledge: c.5 })
        .filter(|r| {
            let h = &r.hit;
            let name = if h.snippet.trim().len() > 80 {
                String::new()
            } else {
                h.path.rsplit('/').next().unwrap_or(&h.path).to_lowercase()
            };
            seen.insert((name, h.snippet.trim().to_string()))
        })
        .collect();
    let results = keep_knowledge_share(ranked).into_iter().take(limit).collect();

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

/// How far the knowledge base's hits can fall behind another member's: its
/// `k`-th best hit ranks ahead of every other member's `k + KB_LEAD`-th, so
/// the wiki's best answer is never pushed out by each code repo's first few
/// guesses. Measured 2026-10-07 on 31 questions the team's wiki answers:
/// searched alone the wiki ranked the right page 1 to 10, but merged by
/// relevance with five code repos its fifth hit lost to every repo's first,
/// and route_query's top five held the page for 10 of the 31. Reserved
/// places (two of the first five, four of the first ten) were tried too and
/// cost the 45 code questions a top-five answer at every setting that
/// helped the wiki; holding the wiki's rank against each repo's rank did not
/// (see `KB_SHARE_FLOOR`).
pub const KB_LEAD: usize = 3;
/// A knowledge-base hit is held up only if it scores at least this share of
/// the list's best hit: a wiki page that barely matches a question about
/// code does not push the code down.
pub const KB_SHARE_FLOOR: f64 = 0.6;

/// One merged hit with what [`keep_knowledge_share`] needs: its rank in its
/// own member's list, its merge score, and whether it is from the
/// knowledge base.
struct Ranked {
    rank: usize,
    hit: RoutedHit,
    score: f64,
    knowledge: bool,
}

/// Reorder a merged list, best first, so the knowledge base keeps its share
/// ([`KB_LEAD`]): before another member's hit of rank `r`, each
/// knowledge-base member's next hit of rank `r - KB_LEAD` or better is
/// placed, if it scores at least [`KB_SHARE_FLOOR`] of the best. Every hit
/// keeps its order within its own member, and a hit that ranks higher on
/// relevance is never moved down by more than the hits placed ahead of it.
fn keep_knowledge_share(ranked: Vec<Ranked>) -> Vec<RoutedHit> {
    let best = ranked.first().map_or(0.0, |r| r.score);
    let held_up = |r: &Ranked| r.knowledge && (best <= 0.0 || r.score >= KB_SHARE_FLOOR * best);
    if !ranked.iter().any(|r| !r.knowledge) || !ranked.iter().any(held_up) {
        return ranked.into_iter().map(|r| r.hit).collect();
    }
    // Each knowledge-base member's hits that may be held up, by own rank.
    let mut queues: Vec<(Uuid, std::collections::VecDeque<usize>)> = Vec::new();
    for (i, r) in ranked.iter().enumerate().filter(|(_, r)| held_up(r)) {
        match queues.iter_mut().find(|(id, _)| *id == r.hit.project_id) {
            Some((_, q)) => q.push_back(i),
            None => queues.push((r.hit.project_id, std::collections::VecDeque::from([i]))),
        }
    }
    for (_, q) in &mut queues {
        q.make_contiguous().sort_by_key(|&i| ranked[i].rank);
    }
    let mut placed = vec![false; ranked.len()];
    let mut order: Vec<usize> = Vec::with_capacity(ranked.len());
    for i in 0..ranked.len() {
        if placed[i] {
            continue;
        }
        if !ranked[i].knowledge {
            for (_, q) in &mut queues {
                while let Some(&k) = q.front() {
                    if placed[k] {
                        q.pop_front();
                    } else if ranked[i].rank >= ranked[k].rank + KB_LEAD {
                        q.pop_front();
                        placed[k] = true;
                        order.push(k);
                    } else {
                        break;
                    }
                }
            }
        }
        placed[i] = true;
        order.push(i);
    }
    let mut hits: Vec<Option<RoutedHit>> = ranked.into_iter().map(|r| Some(r.hit)).collect();
    order.into_iter().filter_map(|i| hits[i].take()).collect()
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
            knowledge_base: false,
        }
    }

    fn hit(path: &str, chunk_id: i64) -> HybridHit {
        HybridHit {
            path: path.to_string(),
            chunk_id,
            snippet: format!("snippet for {path}"),
            source: Source::Keyword,
            line: None,
            page: None,
            score: 0.0,
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
        assert_eq!(plan.targets[0], p1, "highest pointer density must lead");
        assert_eq!(plan.targets[..3].len(), 3, "the graph ranks at most 3");
        assert_eq!(plan.targets.len(), 4, "then the other ready members, up to the broadcast cap");
        assert_eq!(plan.reason, RouteReason::KgEntities(vec![entity]));
    }

    /// The graph steers by what Claude or the judge related to a named
    /// entity; a co-occurrence edge (two names in the same files) does not.
    #[test]
    fn a_route_follows_claudes_and_judged_edges_not_cooccurrence() {
        let kg = WorkspaceKgDb::open_in_memory().unwrap();
        let named = kg.insert_global_entity("topic", "Balance Studio", "a tools app", 1).unwrap();
        let by_claude = kg.insert_global_entity("repo", "Tools", "the authoring suite", 1).unwrap();
        let by_files = kg.insert_global_entity("topic", "Patchlines", "update channels", 1).unwrap();
        let (p1, p2, p3) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        kg.insert_entity_link(named, p1, 1, "Balance Studio").unwrap();
        kg.insert_entity_link(by_claude, p2, 2, "Tools").unwrap();
        kg.insert_entity_link(by_files, p3, 3, "Patchlines").unwrap();
        kg.insert_global_edge(named, by_claude, "app in", 1.0, "imported").unwrap();
        kg.insert_global_edge(named, by_files, "related", 2.0, "cooccur").unwrap();
        // p2 is the least active member, so only Claude's edge puts it ahead
        // of p4; p3 is still indexing, so the broadcast fill cannot add it
        // either: only the graph could, and its co-occurrence edge must not.
        let p4 = Uuid::new_v4();
        let members = vec![
            member(p1, "Wiki", true, 10),
            member(p2, "Docs", true, 5),
            member(p3, "Ref", false, 900),
            member(p4, "Site", true, 300),
        ];
        let plan = plan_route("what changed in balance studio?", &members, Some(&kg));
        assert_eq!(plan.targets, vec![p1, p2, p4]);
        assert_eq!(plan.reason, RouteReason::KgEntities(vec![named]));
    }

    /// A code repo is never in the graph: a question about an entity the
    /// graph knows still reaches it.
    #[test]
    fn a_graph_route_still_reaches_members_the_graph_does_not_hold() {
        let kg = WorkspaceKgDb::open_in_memory().unwrap();
        let entity = kg.insert_global_entity("topic", "Entra ID", "the identity provider", 1).unwrap();
        let (docs, code) = (Uuid::new_v4(), Uuid::new_v4());
        kg.insert_entity_link(entity, docs, 1, "Entra ID").unwrap();
        let members = vec![member(docs, "Project Documents", true, 100), member(code, "app", true, 50)];
        let plan = plan_route("How are Entra ID tokens validated?", &members, Some(&kg));
        assert_eq!(plan.targets, vec![docs, code], "the graph's member first, the code after");
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
    fn the_knowledge_base_is_searched_outside_the_cap() {
        let mut members: Vec<MemberInfo> = (0..6)
            .map(|i| member(Uuid::new_v4(), &format!("Repo {i}"), true, 10 + i))
            .collect();
        let mut wiki = member(Uuid::new_v4(), "Team-Docs", true, 0);
        wiki.knowledge_base = true;
        members.push(wiki.clone());
        let plan = plan_route("can I merge my own pull request", &members, None);
        assert_eq!(plan.targets.len(), 6, "five capped members plus the knowledge base");
        assert_eq!(plan.targets[0], wiki.project_id, "the knowledge base first, though the least active");
        // A member named in the query still narrows the search to it alone.
        let named = plan_route("what does Repo 3 build", &members, None);
        assert_eq!(named.targets, vec![members[3].project_id]);
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
    fn unscored_hits_merge_by_rank() {
        // Hits with no relevance score to compare (all 0.0) fall back to
        // each hit's position in its own member's list.
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let plan = RoutePlan {
            targets: vec![a, b],
            reason: RouteReason::Broadcast,
            platform: false,
            knowledge: vec![],
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

    /// A query naming a function leads with where it is defined, at its line.
    #[test]
    fn a_symbol_the_query_names_leads_with_its_definition() {
        use crate::project::Project;
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("app")).unwrap();
        std::fs::write(dir.path().join("app/auth.py"), "import jwt\n\n\ndef get_current_user(token):\n    return jwt.decode(token)\n").unwrap();
        std::fs::write(dir.path().join("notes.md"), "# Notes\n\nWe talked about get current user flows a lot, current user this, current user that.\n").unwrap();
        let project = Project::create(dir.path(), "T").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        crate::scan::scan(&project, &mut db).unwrap();
        let hits = search_member(&db, "where is get_current_user defined", None, 8).unwrap();
        assert_eq!(hits.first().map(|h| (h.path.as_str(), h.line)), Some(("app/auth.py", Some(4))), "{hits:?}");
    }

    /// A page the query's words never reach still comes back when the page
    /// that answers links to it.
    #[test]
    fn a_page_linked_from_the_answer_comes_with_it() {
        use crate::project::Project;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Change Order.md"), "# Change Order\n\nBuild starts once the [[Permit]] is issued.\n").unwrap();
        std::fs::write(dir.path().join("Permit.md"), "# Permit\n\nIssued by the client's office by July 17.\n").unwrap();
        std::fs::write(dir.path().join("Other.md"), "# Other\n\nUnrelated notes.\n").unwrap();
        let project = Project::create(dir.path(), "T").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        crate::scan::scan(&project, &mut db).unwrap();
        let hits = search_member(&db, "when does build start", None, 8).unwrap();
        let paths: Vec<&str> = hits.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths.first(), Some(&"Change Order.md"), "{paths:?}");
        assert!(paths.contains(&"Permit.md"), "the linked page joins: {paths:?}");
        assert!(!paths.contains(&"Other.md"), "{paths:?}");
    }

    /// A ruling outranks the closed ticket that says the same, a team file
    /// can turn that around, and code keeps the score it had.
    #[test]
    fn a_ruling_outranks_a_closed_ticket_and_the_team_file_can_say_otherwise() {
        use crate::project::Project;
        let dir = tempfile::tempdir().unwrap();
        for d in ["decisions", "tickets", "src", ".wright"] {
            std::fs::create_dir_all(dir.path().join(d)).unwrap();
        }
        let rulings: String = (1..=6).map(|i| format!("**D-{i:03}** · 2026-10-0{i} · topic — **RULING {i}.** Unrelated ruling number {i}.\n\n")).collect();
        std::fs::write(dir.path().join("decisions/DECISIONS.md"), format!("# DECISIONS\n\n**D-007** · 2026-10-07 · release — **MOD PULL REQUESTS ARE MERGED BY CHRIS.** Chris merges mod pull requests himself.\n\n{rulings}")).unwrap();
        std::fs::write(dir.path().join("tickets/SR-101.md"), "---\nstatus: done\n---\n# SR-101\n\nWho merges mod pull requests? Chris merges mod pull requests himself.\n").unwrap();
        std::fs::write(dir.path().join("src/merge.rs"), "// mod pull requests are merged by Chris himself\nfn merge() {}\n").unwrap();
        let project = Project::create(dir.path(), "T").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        crate::scan::scan(&project, &mut db).unwrap();
        let ask = |db: &Db| search_member(db, "who merges mod pull requests", None, 8).unwrap();
        let hits = ask(&db);
        let pos = |hits: &[HybridHit], p: &str| hits.iter().position(|h| h.path == p).unwrap();
        assert!(pos(&hits, "decisions/DECISIONS.md") < pos(&hits, "tickets/SR-101.md"), "{hits:?}");
        let code = hits.iter().find(|h| h.path == "src/merge.rs").unwrap().score;

        std::fs::write(dir.path().join(".wright/team.json"), r#"{"search": {"weights": {"tickets/": "rule", "decisions/": -3}}}"#).unwrap();
        crate::scan::scan(&project, &mut db).unwrap();
        let hits = ask(&db);
        assert!(pos(&hits, "tickets/SR-101.md") < pos(&hits, "decisions/DECISIONS.md"), "{hits:?}");
        assert_eq!(hits.iter().find(|h| h.path == "src/merge.rs").unwrap().score, code, "code is neutral either way");
    }

    #[test]
    fn a_template_page_ranks_under_a_page_that_answers() {
        use crate::project::Project;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("RULE.md"),
            "---\ntitle: \"{{the-rule-as-a-sentence}}\"\n---\n# {{The rule}}\n\nWhat the client pays when the rule is broken, and the fee for fixing it.\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("SOW.md"), "---\ntitle: SOW\n---\n# SOW\n\nThe client pays a fixed fee of $400,000.\n").unwrap();
        let project = Project::create(dir.path(), "T").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        crate::scan::scan(&project, &mut db).unwrap();
        let hits = search_member(&db, "what fee does the client pay", None, 8).unwrap();
        let paths: Vec<&str> = hits.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths.first(), Some(&"SOW.md"), "{paths:?}");
    }

    #[test]
    fn a_question_says_what_it_is_after_and_machine_text_is_spotted() {
        assert_eq!(query_intent("How is the token validated in the auth handler?"), Some(Intent::Code));
        assert_eq!(query_intent("where is get_current_user defined"), Some(Intent::Code));
        assert_eq!(query_intent("what does LlmGateway.chat return"), Some(Intent::Code));
        assert_eq!(query_intent("who decided the release date"), Some(Intent::Prose));
        assert_eq!(query_intent("what is the fee in the contract"), Some(Intent::Prose));
        assert_eq!(query_intent("tell me about the save format"), None);
        assert_eq!(query_intent("who calls the save function"), None, "both: no nudge");

        assert!(machine_written(&format!("const SEARCH = [{}];", "{\"a\":1},".repeat(300))));
        assert!(!machine_written("fn save() {\n    write();\n}\n"));
        assert!(!machine_written(&"A person wrote this line.\n".repeat(80)));
    }

    /// Relevance decides across members: a strong hit in the second repo
    /// beats a weak first hit in the first, whatever the plan's order.
    #[test]
    fn a_reference_repos_assets_yield_unless_the_question_is_about_the_platform() {
        use crate::registry::RepoKind;
        let plan = |platform, reason| RoutePlan { targets: vec![], reason, platform, knowledge: vec![] };
        let asked = plan(false, RouteReason::Broadcast);
        let shield = "HytaleAssets/Server/Item/Items/Weapon/Shield/Weapon_Shield_Copper.json";
        assert!(reference_asset(&asked, &[RepoKind::Reference], shield));
        assert!(!reference_asset(&asked, &[RepoKind::Reference], "server/core/HytaleServerConfig.java"), "its code competes as before");
        assert!(!reference_asset(&asked, &[RepoKind::Code], "src/main/resources/Server/Item/Items/Coins/Bronze_Coin.json"), "the team's own assets");
        assert!(!reference_asset(&plan(true, RouteReason::Broadcast), &[RepoKind::Reference], shield), "a platform question");
        assert!(!reference_asset(&plan(false, RouteReason::Named), &[RepoKind::Reference], shield), "the question names the repo");

        assert!(asks_about_platform("Does the engine have a boss health bar?"));
        assert!(asks_about_platform("How do we add our own pins to the game's built-in minimap?"));
        assert!(asks_about_platform("Which shapes does it support out of the box?"));
        assert!(!asks_about_platform("How much stamina does it cost to take a hit while holding up a weapon or shield?"));
        assert!(!asks_about_platform("Who leads engineering?"), "whole words only");
        assert!(plan_route("does the engine have flags", &[], None).platform);
    }

    #[test]
    fn scored_hits_merge_by_relevance_across_members() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let plan = RoutePlan { targets: vec![a, b], reason: RouteReason::Broadcast, platform: false, knowledge: vec![] };
        let scored = |path: &str, id: i64, score: f64| HybridHit { score, ..hit(path, id) };
        let member_hits = vec![
            MemberHits {
                project_id: a,
                member_name: "code".into(),
                status: MemberStatus::Searched,
                hits: vec![scored("design.md", 1, 0.6), scored("spec.md", 2, 0.4)],
            },
            MemberHits {
                project_id: b,
                member_name: "notes".into(),
                status: MemberStatus::Searched,
                hits: vec![scored("SOW.md", 10, 4.2), scored("log.md", 11, 0.5)],
            },
        ];
        let paths: Vec<String> = merge_routed(&plan, &member_hits, 10).results.into_iter().map(|r| r.path).collect();
        assert_eq!(paths, vec!["SOW.md", "design.md", "log.md", "spec.md"]);
    }

    /// The knowledge base's k-th hit ranks ahead of the code repo's
    /// (k + 3)-th, its hits in their own order and the code's in theirs; a
    /// wiki hit far weaker than the best is not held up.
    #[test]
    fn the_knowledge_base_keeps_its_share_of_the_merged_list() {
        let (code, wiki) = (Uuid::new_v4(), Uuid::new_v4());
        let plan = RoutePlan { targets: vec![wiki, code], reason: RouteReason::Broadcast, platform: false, knowledge: vec![wiki] };
        let scored = |path: &str, id: i64, score: f64| HybridHit { score, ..hit(path, id) };
        let merged = |wiki_scores: &[f64]| -> Vec<String> {
            let member_hits = vec![
                MemberHits {
                    project_id: code,
                    member_name: "code".into(),
                    status: MemberStatus::Searched,
                    hits: (0..10).map(|i| scored(&format!("src/f{i}.rs"), i, 9.0 - i as f64 * 0.1)).collect(),
                },
                MemberHits {
                    project_id: wiki,
                    member_name: "wiki".into(),
                    status: MemberStatus::Searched,
                    hits: wiki_scores.iter().enumerate().map(|(i, s)| scored(&format!("w{i}.md"), 100 + i as i64, *s)).collect(),
                },
            ];
            merge_routed(&plan, &member_hits, 10).results.into_iter().map(|r| r.path).collect()
        };
        let paths = merged(&[6.0, 5.8, 5.6, 5.5, 5.0]);
        assert_eq!(
            paths,
            vec!["src/f0.rs", "src/f1.rs", "src/f2.rs", "w0.md", "src/f3.rs", "w1.md", "src/f4.rs", "w2.md", "src/f5.rs", "w3.md"]
        );
        // Under half the best score: no reserved place, merged by relevance.
        let paths = merged(&[4.0, 3.0]);
        assert!(paths.iter().all(|p| p.starts_with("src/")), "{paths:?}");
        // A wiki hit that wins on relevance is not held back.
        let paths = merged(&[9.5, 1.0]);
        assert_eq!(paths[0], "w0.md");
        // The plan marks the knowledge base.
        let mut members = vec![member(code, "app", true, 5), member(wiki, "Team-Docs", true, 1)];
        members[1].knowledge_base = true;
        assert_eq!(plan_route("who merges pull requests", &members, None).knowledge, vec![wiki]);
        assert_eq!(plan_route("what does app build", &members, None).knowledge, Vec::<Uuid>::new(), "a named repo alone");
    }

    /// A question in its own words still finds the chunk that says it
    /// differently: all-words finds nothing, any-word does.
    #[test]
    fn a_question_finds_a_chunk_without_every_word() {
        use crate::project::Project;
        use crate::scan;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("auth.py"), "# Authentication dependency: Entra ID JWT validation.\n").unwrap();
        std::fs::write(dir.path().join("other.py"), "def unrelated():\n    return 1\n").unwrap();
        let project = Project::create(dir.path(), "T").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        scan::scan(&project, &mut db).unwrap();
        let hits = search_member(&db, "How are Entra ID JWT tokens validated in the backend?", None, 5).unwrap();
        assert_eq!(hits.first().map(|h| h.path.as_str()), Some("auth.py"), "{hits:?}");
        assert!(hits.iter().all(|h| h.path != "other.py"), "a chunk with none of the words is not a hit");
    }

    #[test]
    fn limit_truncates_merged_results() {
        let a = Uuid::new_v4();
        let plan = RoutePlan {
            targets: vec![a],
            reason: RouteReason::Broadcast,
            platform: false,
            knowledge: vec![],
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
            platform: false,
            knowledge: vec![],
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
            platform: false,
            knowledge: vec![],
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
    fn a_hit_is_cited_by_repo_path_line_and_by_note_name_in_a_wiki() {
        assert_eq!(locator("ken", "src/scan.rs", Some(42), None, false), "ken:src/scan.rs:42");
        assert_eq!(locator("ken", "src\\scan.rs", Some(42), Some("f00218c"), false), "ken@f00218c:src/scan.rs:42");
        assert_eq!(locator("ken", "README.md", None, None, false), "ken:README.md", "no line: no suffix");
        assert_eq!(
            locator("sr-docs", "Engine/Combat Loop.md", Some(7), Some("abc1234"), true),
            "[[Combat Loop]] · sr-docs@abc1234:Engine/Combat Loop.md:7"
        );
        assert_eq!(
            locator("sr-docs", "tools/gen.py", Some(3), None, true),
            "sr-docs:tools/gen.py:3",
            "code in a wiki repo is not a note"
        );
    }

    #[test]
    fn head_sha_reads_a_branch_a_packed_ref_and_a_detached_head() {
        let dir = tempfile::tempdir().unwrap();
        let git = dir.path().join(".git");
        std::fs::create_dir_all(git.join("refs/heads")).unwrap();
        assert_eq!(head_sha(dir.path()), None, "no HEAD yet");
        std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(git.join("refs/heads/main"), "f00218c9b1e2d3a4f5061728394a5b6c7d8e9f01\n").unwrap();
        assert_eq!(head_sha(dir.path()).as_deref(), Some("f00218c"));
        std::fs::remove_file(git.join("refs/heads/main")).unwrap();
        std::fs::write(git.join("packed-refs"), "# pack-refs\n1e132cf00000000000000000000000000000000a refs/heads/main\n").unwrap();
        assert_eq!(head_sha(dir.path()).as_deref(), Some("1e132cf"));
        std::fs::write(git.join("HEAD"), "0f87d6f11111111111111111111111111111111b\n").unwrap();
        assert_eq!(head_sha(dir.path()).as_deref(), Some("0f87d6f"));
        let plain = tempfile::tempdir().unwrap();
        assert_eq!(head_sha(plain.path()), None, "not a git repo");
    }

    #[test]
    fn every_hit_carries_a_resolvable_ken_address() {
        let a = Uuid::new_v4();
        let plan = RoutePlan {
            targets: vec![a],
            reason: RouteReason::KgEntities(vec![42]),
            platform: false,
            knowledge: vec![],
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

    /// Across members, a binding page outranks a better-scoring Research
    /// note or retired page; within a band the RRF order holds.
    #[test]
    fn binding_pages_lead_the_merged_list_across_members() {
        use crate::pagemeta::{hit_page, PageMeta};
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let plan = RoutePlan { targets: vec![a, b], reason: RouteReason::Broadcast, platform: false, knowledge: vec![] };
        let paged = |path: &str, id: i64, meta: Option<PageMeta>| HybridHit {
            page: hit_page(path, meta),
            ..hit(path, id)
        };
        let retired = PageMeta { status: Some("retired".into()), ..Default::default() };
        let member_hits = vec![
            MemberHits {
                project_id: a,
                member_name: "A".into(),
                status: MemberStatus::Searched,
                hits: vec![paged("Research/old.md", 1, None), paged("Current/Project.md", 2, None)],
            },
            MemberHits {
                project_id: b,
                member_name: "B".into(),
                status: MemberStatus::Searched,
                hits: vec![
                    paged("Ways-of-Working/Old-rules.md", 3, Some(retired)),
                    paged("Ways-of-Working/Rules.md", 4, None),
                ],
            },
        ];
        let order: Vec<String> = merge_routed(&plan, &member_hits, 10).results.into_iter().map(|h| h.path).collect();
        assert_eq!(
            order,
            vec!["Ways-of-Working/Rules.md", "Current/Project.md", "Research/old.md", "Ways-of-Working/Old-rules.md"]
        );
    }

    #[test]
    fn named_and_broadcast_plans_carry_no_breadcrumbs() {
        let a = Uuid::new_v4();
        let plan = RoutePlan {
            targets: vec![a],
            reason: RouteReason::Named,
            platform: false,
            knowledge: vec![],
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
    /// A search in the team's word finds a page written in the platform's
    /// word, through the Vocabulary page; the hit carries its line and page.
    #[test]
    fn search_member_finds_a_page_through_the_vocabulary() {
        use crate::project::Project;
        use crate::runner::CancelToken;
        use crate::{engine, scan};
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("Reference")).unwrap();
        std::fs::write(
            dir.path().join("Reference/Vocabulary.md"),
            "# Vocabulary\n\n| our word | the code's or platform's word | shown in |\n|---|---|---|\n| shard | region | Engine/World.md |\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("Platform")).unwrap();
        std::fs::write(dir.path().join("Platform/World.md"), "# World\n\nEach region loads from disk.\n").unwrap();
        let project = Project::create(dir.path(), "T").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        scan::scan(&project, &mut db).unwrap();
        let mut embedder = FakeEmbedder::new();
        engine::rebuild_semantic_index(&project, &mut db, &mut embedder, &CancelToken::new(), |_, _| {}).unwrap();

        let hits = search_member(&db, "shard loads", None, 10).unwrap();
        let world = hits.iter().find(|h| h.path == "Platform/World.md").expect("found via region");
        assert_eq!(world.line, Some(1), "one chunk, from its heading");
        assert_eq!(world.page.as_ref().and_then(|p| p.section), Some("Platform"));
    }

    /// Keyword search over chunks needs no model: a plain scan writes them,
    /// so search, citations and page facts work before any semantic index,
    /// and a later rebuild embeds the chunks the scan already wrote.
    #[test]
    fn a_scan_alone_makes_chunks_searchable_and_a_rebuild_embeds_them() {
        use crate::project::Project;
        use crate::runner::CancelToken;
        use crate::{engine, scan};
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("Platform")).unwrap();
        std::fs::write(dir.path().join("Platform/World.md"), "---\nverified: 2026-09-05\n---\n# World\n\nEach region loads from disk.\n").unwrap();
        let project = Project::create(dir.path(), "T").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        scan::scan(&project, &mut db).unwrap();

        let hits = search_member(&db, "region", None, 10).unwrap();
        let world = hits.iter().find(|h| h.path == "Platform/World.md").expect("found without any model");
        assert!(world.line.is_some());
        assert_eq!(world.page.as_ref().and_then(|p| p.verified.as_deref()), Some("2026-09-05"));

        let missing_before = db.chunks_missing_vectors("Platform/World.md").unwrap().len();
        assert!(missing_before > 0);
        let mut embedder = FakeEmbedder::new();
        engine::rebuild_semantic_index(&project, &mut db, &mut embedder, &CancelToken::new(), |_, _| {}).unwrap();
        if db.vec_available() {
            assert_eq!(db.chunks_missing_vectors("Platform/World.md").unwrap().len(), 0, "the rebuild embedded them");
        }

        // A removed file's chunks go with it.
        std::fs::remove_file(dir.path().join("Platform/World.md")).unwrap();
        scan::scan(&project, &mut db).unwrap();
        assert!(search_member(&db, "region", None, 10).unwrap().is_empty());
    }

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

//! Search across a workspace's sources (knowledge-core decision 20).
//!
//! Every question searches every source, and the hits are merged once, by
//! relevance. A source named in the question narrows the search to it, as
//! does a repo or group the person chose. A reference repo joins only for a
//! question about the platform, when it is named, or when nothing else
//! answers. A source still building its meaning index, or slow to answer, is
//! searched and reported, never dropped.
//!
//! [`plan_route`] decides which sources to search, [`search_target`]
//! searches one and says how it went, and [`merge_routed`] merges every
//! source's hits into one cited list. [`execute_plan`] runs the three in
//! order over plain `Db` handles. The app and the MCP server open the
//! databases their own way and fan the searches out themselves, then call the
//! same three, so every surface ranks through one pipeline (decision 15).

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use uuid::Uuid;

use crate::contenttype::ContentType;
use crate::db::Db;
use crate::embedder::Embedder;
use crate::registry::RepoKind;
use crate::search::{self, HybridHit, Source};
use crate::vocab::Vocabulary;
use crate::Result;

/// A source whose search takes at least this long is reported as slow. Its
/// hits are still merged: a slow source is waited for, not dropped.
pub const SLOW_MILLIS: u64 = 2_000;

/// Why [`plan_route`] chose the sources it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteReason {
    /// The question named these sources, or the caller chose them.
    Named,
    /// Every source, less the reference repos held back.
    Every,
}

/// Which sources a search reads, and why.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutePlan {
    /// The sources to search. A tie between two hits goes to the one whose
    /// source comes first here.
    pub targets: Vec<Uuid>,
    pub reason: RouteReason,
    /// Reference repos held back: searched only when no target answers
    /// ([`needs_reserve`]).
    pub reserve: Vec<Uuid>,
}

impl RoutePlan {
    /// A search of exactly these sources, as the caller chose them (a pinned
    /// repo, a group). Nothing is held back.
    pub fn chosen(targets: Vec<Uuid>) -> RoutePlan {
        RoutePlan { targets, reason: RouteReason::Named, reserve: Vec::new() }
    }
}

/// What [`plan_route`] needs to know about one source.
#[derive(Debug, Clone, PartialEq)]
pub struct MemberInfo {
    pub project_id: Uuid,
    pub name: String,
    /// A reference repo ([`is_reference`]): the platform the team builds on,
    /// not the team's own work.
    pub reference: bool,
}

/// Whether a repo of these kinds is a reference repo: one that is only
/// reference. A repo that is also the team's code or wiki is searched like
/// any other.
pub fn is_reference(kind: &[RepoKind]) -> bool {
    !kind.is_empty() && kind.iter().all(|k| *k == RepoKind::Reference)
}

/// Plan a search for `query` over `members`.
///
/// When the question names sources (a member's name, matched whole words and
/// case-insensitively), only those are searched, reference repos included.
/// Otherwise every source is searched, except that a reference repo is held
/// back in [`RoutePlan::reserve`] unless the question is about the platform
/// ([`asks_about_platform`]).
pub fn plan_route(query: &str, members: &[MemberInfo]) -> RoutePlan {
    let normalized = normalize_name(query);
    let named: Vec<Uuid> = members
        .iter()
        .filter(|m| contains_normalized(&normalized, &normalize_name(&m.name)))
        .map(|m| m.project_id)
        .collect();
    if !named.is_empty() {
        return RoutePlan::chosen(named);
    }
    let platform = asks_about_platform(query);
    let (targets, reserve): (Vec<&MemberInfo>, Vec<&MemberInfo>) = members.iter().partition(|m| platform || !m.reference);
    RoutePlan {
        targets: targets.iter().map(|m| m.project_id).collect(),
        reason: RouteReason::Every,
        reserve: reserve.iter().map(|m| m.project_id).collect(),
    }
}

/// Whether the reference repos the plan held back are searched too: it held
/// some back and no source searched so far answered. A source answers when
/// the question's words found something there. Meaning search always
/// returns its nearest passages, however far, so a source with a meaning
/// index would never come back empty and the reserve would never be asked.
pub fn needs_reserve(plan: &RoutePlan, searched: &[MemberHits]) -> bool {
    !plan.reserve.is_empty() && searched.iter().all(|m| m.hits.iter().all(|h| h.source == Source::Semantic))
}

/// A name as a question would say it, for matching one against the other:
/// lowercased, every run of other characters one space, trimmed.
/// `"Shattered  Realms!"` and `"shattered-realms"` both read
/// `"shattered realms"`.
pub fn normalize_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_sep = true;
    for ch in name.chars() {
        if ch.is_alphanumeric() {
            out.extend(ch.to_lowercase());
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

/// Word-boundary-aware "does `haystack` contain `needle`" over text already
/// normalized by `normalize_name` (lowercased, single-spaced). Padding both
/// with a space stops a short needle matching inside a longer word (`art` in
/// `cart`) while a name of several words still matches as a run. An empty
/// needle never matches.
fn contains_normalized(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    format!(" {haystack} ").contains(&format!(" {needle} "))
}

/// How a hit is cited so a reader can go straight to it: `repo:path:line`,
/// with `repo@sha` when the repo is git, and led by `[[Note Name]]` for a
/// page in a knowledge base. `line` is None for a chunk stored before
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

/// `ken://<project-id>/<rel-path>`, the path forward-slashed on every
/// platform.
fn ken_address(project_id: Uuid, rel_path: &str) -> String {
    format!("ken://{project_id}/{}", rel_path.replace('\\', "/"))
}

/// Search one source's chunk index: keyword search (with the vocabulary's
/// other words for the query) and, when `query_vec` is given and the
/// source's meaning index is built, meaning search, merged and reranked by
/// [`search::merge_hits`] and [`search::rerank_weighted`]. Each hit carries
/// its line, its page facts, its label and, for code, its symbol.
/// `query_vec` is the question embedded once by the caller; None searches by
/// keyword only. A question whose plain words name a definition in the code
/// index ("save world" for `saveWorld`) finds the chunk that holds it too,
/// cited at its line ([`joined_definitions`]).
pub fn search_member(db: &Db, query: &str, query_vec: Option<&[f32]>, limit: usize) -> Result<Vec<HybridHit>> {
    search_member_with(db, query, query_vec, limit, None)
}

/// [`search_member`] with the workspace's vocabulary as well as the
/// member's own: a Vocabulary page in the knowledge base then widens the search
/// in every repo, not only in the wiki. Routed searches build it once with
/// [`workspace_vocabulary`] and pass it to every member.
pub fn search_member_with(
    db: &Db,
    query: &str,
    query_vec: Option<&[f32]>,
    limit: usize,
    shared: Option<&Vocabulary>,
) -> Result<Vec<HybridHit>> {
    let mut fts_hits = db.search_chunks_fts(query, limit)?;
    // The team's other words for what was asked (the Vocabulary page and the
    // decisions log's aliases): each alternative phrasing runs as its own
    // keyword search, its new chunks after the original's.
    let own = Vocabulary::cached(db)?;
    let vocab = match shared {
        Some(s) => Vocabulary::merged([&own, s]),
        None => own,
    };
    for alt in vocab.alternatives(query) {
        for hit in db.search_chunks_fts(&alt, limit)? {
            if !fts_hits.iter().any(|h| h.chunk_id == hit.chunk_id) {
                fts_hits.push(hit);
            }
        }
    }
    // The code index for plain words (decision 26): "save world" names
    // `saveWorld`, `save_world` and `SaveWorld`. The chunk holding each
    // definition the words join into is its file's keyword hit, cited at the
    // definition's own line. It is scored like any other hit; its symbol's
    // words count as name words in the rerank.
    let named = joined_definitions(db, query)?;
    if !named.is_empty() {
        fts_hits.retain(|h| !named.iter().any(|n| n.hit.path == h.path));
        let rest = std::mem::take(&mut fts_hits);
        fts_hits = named.iter().map(|n| n.hit.clone()).chain(rest).collect();
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
    // Score = base plus tier (decision 24): word matches, meaning and
    // agreement, plus each file's tier (`crate::authority`), inside this
    // member's own ranking.
    let paths: Vec<String> = fts_hits.iter().map(|h| h.path.clone()).chain(vec_hits.iter().map(|h| h.path.clone())).collect();
    let authority = db.authority_weights(&paths)?.unwrap_or_default();
    // Each hit's line and, for code, its symbol, before the rerank reads the
    // symbol: a definition the words named is cited at its own line, under
    // its own name when its chunk packs several.
    let mut merged = search::merge_hits(&fts_hits, &vec_hits);
    for hit in &mut merged {
        let (line, symbol) = db.chunk_line_symbol(hit.chunk_id)?;
        match named.iter().find(|n| n.hit.chunk_id == hit.chunk_id) {
            Some(n) => {
                hit.line = Some(n.line);
                hit.symbol = Some(symbol.filter(|s| !s.contains(", ") && s.ends_with(n.name.as_str())).unwrap_or_else(|| n.name.clone()));
            }
            None => {
                hit.line = line;
                hit.symbol = symbol;
            }
        }
    }
    let mut hits = search::rerank_weighted(query, merged, &vec_hits, &extra, &authority);
    for hit in &mut hits {
        hit.page = crate::pagemeta::hit_page(&hit.path, db.page_meta(&hit.path)?);
        if machine_written(&hit.snippet) {
            hit.score -= W_BLOB;
        }
    }
    hits.sort_by(|a, b| b.score.total_cmp(&a.score));
    // A ruling a later one supersedes is shown under it (`supersede`).
    crate::supersede::lead_with_successors(db, &mut hits)?;
    // Every hit says what it is, how much it counts and how fresh: a ruling
    // and its date, a ticket and its tier, a code file and its role
    // (`authority::hit_label`).
    for hit in &mut hits {
        hit.label = crate::authority::hit_label(db, &hit.path, Some(&hit.snippet))?;
    }
    Ok(hits)
}

/// A definition the question's plain words named, as a keyword hit: the
/// chunk that holds it, the definition's line and its name.
struct NamedDefinition {
    hit: search::FtsHit,
    line: i64,
    name: String,
}

/// The definitions a question's plain words join into
/// (`codeindex::definitions_named`), one per file, each with the chunk that
/// holds it. None for a file with no chunks (not indexed, or gone).
fn joined_definitions(db: &Db, query: &str) -> Result<Vec<NamedDefinition>> {
    let mut out: Vec<NamedDefinition> = Vec::new();
    for def in crate::codeindex::definitions_named(db, query, crate::codeindex::NAMED_MAX)? {
        if out.iter().any(|n| n.hit.path == def.path) {
            continue;
        }
        let Some((chunk_id, text)) = db.chunk_at_line(&def.path, def.line)? else { continue };
        out.push(NamedDefinition {
            hit: search::FtsHit { chunk_id, path: def.path, text },
            line: def.line,
            name: def.name,
        });
    }
    Ok(out)
}

/// Machine-written text (an inlined search index, a data dump in one line)
/// matches everything a little and answers nothing: well under any real hit.
pub const W_BLOB: f64 = 1.5;

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
    types: &[ContentType],
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
    types: &[ContentType],
    shared: Option<&Vocabulary>,
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
/// together, so the knowledge base's Vocabulary page and decisions aliases widen
/// the search in the code repos too. Members are read once per search.
pub fn workspace_vocabulary<'a>(dbs: impl IntoIterator<Item = &'a Db>) -> Vocabulary {
    let vocabs: Vec<Vocabulary> = dbs.into_iter().filter_map(|db| Vocabulary::cached(db).ok()).collect();
    Vocabulary::merged(vocabs.iter())
}

/// Words that put a question on the platform rather than on the team's own
/// work.
const PLATFORM_WORDS: &[&str] =
    &["engine", "platform", "built-in", "builtin", "vanilla", "base game", "out of the box", "upstream", "sdk", "framework"];

/// Whether `query` asks about the platform the team builds on ("does the
/// engine have a boss health bar", "the game's built-in minimap"). Such a
/// question searches the reference repos too.
pub fn asks_about_platform(query: &str) -> bool {
    let words: String = query.to_lowercase().chars().map(|c| if c.is_alphanumeric() || c == '-' { c } else { ' ' }).collect();
    let words = format!(" {} ", words.split_whitespace().collect::<Vec<_>>().join(" "));
    PLATFORM_WORDS.iter().any(|w| words.contains(&format!(" {w} ")))
}

/// How one source's search went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberStatus {
    /// Searched by keyword and, where it has a meaning index, by meaning.
    Searched,
    /// Searched by keyword only while its meaning index is built: `embedded`
    /// of its `chunks` passages have a meaning vector so far.
    Building { embedded: i64, chunks: i64 },
    /// Not searched: its index would not open, or the search failed.
    Unavailable,
}

/// How far a source's meaning index has got: `Building` until it is built,
/// `Searched` once it is, or where this build has no vector search at all.
pub fn meaning_status(db: &Db) -> MemberStatus {
    if !db.vec_available() || db.semantic_built_at().ok().flatten().is_some() {
        return MemberStatus::Searched;
    }
    MemberStatus::Building {
        embedded: db.vector_count().unwrap_or(0),
        chunks: db.chunk_count().unwrap_or(0),
    }
}

/// One source's part in a routed search: its ranked hits (best first, as
/// [`search_member`] returns them), how its search went, and how long it
/// took. [`merge_routed`] takes a list of these, however they were made.
#[derive(Debug, Clone, PartialEq)]
pub struct MemberHits {
    pub project_id: Uuid,
    pub member_name: String,
    pub status: MemberStatus,
    pub hits: Vec<HybridHit>,
    /// How long the search took, in milliseconds.
    pub millis: u64,
}

/// Search one source for a routed search and say how it went. `db` None
/// (its index would not open) or a failed search is `Unavailable`; a source
/// whose meaning index is not built yet is searched by keyword and reported
/// `Building` ([`meaning_status`]). The caller filters the hits further if
/// it needs to, before [`needs_reserve`] and [`merge_routed`] read them.
#[allow(clippy::too_many_arguments)]
pub fn search_target(
    project_id: Uuid,
    name: &str,
    db: Option<&Db>,
    query: &str,
    query_vec: Option<&[f32]>,
    limit: usize,
    types: &[ContentType],
    shared: Option<&Vocabulary>,
) -> MemberHits {
    let started = Instant::now();
    let (status, hits) = match db {
        None => (MemberStatus::Unavailable, Vec::new()),
        Some(db) => match search_member_of_with(db, query, query_vec, limit, types, shared) {
            Ok(hits) => (meaning_status(db), hits),
            Err(_) => (MemberStatus::Unavailable, Vec::new()),
        },
    };
    MemberHits {
        project_id,
        member_name: name.to_string(),
        status,
        hits,
        millis: started.elapsed().as_millis() as u64,
    }
}

/// One merged, cited search result.
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
    /// For a Markdown page: section, checked or evidence date, and whether
    /// it is superseded.
    pub page: Option<crate::pagemeta::HitPage>,
    /// `kind · tier or role · date` (`authority::hit_label`).
    pub label: Option<String>,
    /// For a code hit, the definition its chunk holds
    /// ([`search::HybridHit::symbol`]).
    pub symbol: Option<String>,
}

/// How one source's search went, as reported beside the results.
#[derive(Debug, Clone, PartialEq)]
pub struct MemberStatusEntry {
    pub project_id: Uuid,
    pub member_name: String,
    pub status: MemberStatus,
    /// How long its search took, in milliseconds.
    pub millis: u64,
}

impl MemberStatusEntry {
    /// Whether its search took [`SLOW_MILLIS`] or longer.
    pub fn slow(&self) -> bool {
        self.millis >= SLOW_MILLIS
    }
}

/// A routed search's outcome: the plan, the merged cited hits, and how each
/// source searched went.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionReport {
    pub plan: RoutePlan,
    pub results: Vec<RoutedHit>,
    pub member_status: Vec<MemberStatusEntry>,
}

/// A source [`execute_plan`] can search: its id, its name and its index.
pub struct MemberDbHandle<'a> {
    pub project_id: Uuid,
    pub name: &'a str,
    pub db: &'a Db,
}

/// Run a plan over plain `Db` handles, one source after another: embed the
/// question once, search every target ([`search_target`]), then the held-back
/// reference repos if nothing answered ([`needs_reserve`]), and merge
/// ([`merge_routed`]). A planned source missing from `targets` is reported
/// `Unavailable`. A caller that wants the searches to run at once calls the
/// same three itself, as the app does.
pub fn execute_plan(
    plan: &RoutePlan,
    targets: &[MemberDbHandle<'_>],
    embedder: &mut dyn Embedder,
    query: &str,
    limit: usize,
) -> ExecutionReport {
    let query_vec = embedder.embed_query(query).ok();
    let vocab = workspace_vocabulary(targets.iter().map(|t| t.db));
    let search = |ids: &[Uuid]| -> Vec<MemberHits> {
        ids.iter()
            .map(|id| {
                let target = targets.iter().find(|t| t.project_id == *id);
                search_target(
                    *id,
                    target.map_or("", |t| t.name),
                    target.map(|t| t.db),
                    query,
                    query_vec.as_deref(),
                    limit,
                    &[],
                    Some(&vocab),
                )
            })
            .collect()
    };
    let mut member_hits = search(&plan.targets);
    if needs_reserve(plan, &member_hits) {
        member_hits.extend(search(&plan.reserve));
    }
    merge_routed(plan, &member_hits, limit)
}

/// Merge every source's hits into one list, once, by relevance. Every source
/// scores on the same scale (each file's tier already in its score), so the
/// best answer leads whichever repo holds it. A tie goes to the hit ranked
/// higher in its own source, then to the source the plan lists first. The
/// same file kept in two repos counts once, at its best place. Every source
/// searched is in `member_status`, whatever its status.
///
/// Pure: no database or model, so tests can feed it [`MemberHits`] directly.
pub fn merge_routed(plan: &RoutePlan, member_hits: &[MemberHits], limit: usize) -> ExecutionReport {
    let plan_order: HashMap<Uuid, usize> =
        plan.targets.iter().chain(&plan.reserve).enumerate().map(|(i, id)| (*id, i)).collect();

    // (score, rank in its own source, the source's place in the plan, hit).
    let mut candidates: Vec<(f64, usize, usize, RoutedHit)> = Vec::new();
    for mh in member_hits {
        if mh.hits.is_empty() {
            continue;
        }
        // Once per source: its commit, and whether its pages are notes. A
        // source missing from the registry is cited as plain repo:path:line.
        let (sha, kinds) = match crate::registry::entry_of(mh.project_id) {
            Some((root, kind)) => (head_sha(&root), kind),
            None => (None, Vec::new()),
        };
        let wiki = kinds.contains(&RepoKind::Wiki);
        let order = plan_order.get(&mh.project_id).copied().unwrap_or(usize::MAX);
        for (rank, hit) in mh.hits.iter().enumerate() {
            candidates.push((
                hit.score,
                rank,
                order,
                RoutedHit {
                    path: hit.path.clone(),
                    chunk_id: hit.chunk_id,
                    snippet: hit.snippet.clone(),
                    source: hit.source,
                    project_id: mh.project_id,
                    member_name: mh.member_name.clone(),
                    address: ken_address(mh.project_id, &hit.path),
                    line: hit.line,
                    locator: locator(&mh.member_name, &hit.path, hit.line, sha.as_deref(), wiki),
                    page: hit.page.clone(),
                    label: hit.label.clone(),
                    symbol: hit.symbol.clone(),
                },
            ));
        }
    }
    candidates.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    // One copy of a file kept in several repos (a vendored PDF, a design
    // asset): the same name and the same text is one answer, at its best rank.
    // A longer passage is the same answer wherever it sits (a copied doc, a
    // second checkout); a short one only when the file name matches too.
    let mut seen: std::collections::HashSet<(String, String)> = Default::default();
    let results = candidates
        .into_iter()
        .map(|c| c.3)
        .filter(|h| {
            let name = if h.snippet.trim().len() > 80 {
                String::new()
            } else {
                h.path.rsplit('/').next().unwrap_or(&h.path).to_lowercase()
            };
            seen.insert((name, h.snippet.trim().to_string()))
        })
        .take(limit)
        .collect();

    let member_status = member_hits
        .iter()
        .map(|mh| MemberStatusEntry {
            project_id: mh.project_id,
            member_name: mh.member_name.clone(),
            status: mh.status,
            millis: mh.millis,
        })
        .collect();

    ExecutionReport { plan: plan.clone(), results, member_status }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_normalize_as_a_question_says_them() {
        assert_eq!(normalize_name("Shattered  Realms!"), "shattered realms");
        assert_eq!(normalize_name("shattered-realms"), "shattered realms");
        assert_eq!(normalize_name("  Priya N. "), "priya n");
        assert_eq!(normalize_name("!!"), "");
    }
    use crate::embedder::FakeEmbedder;

    fn member(project_id: Uuid, name: &str) -> MemberInfo {
        MemberInfo { project_id, name: name.to_string(), reference: false }
    }

    fn reference(project_id: Uuid, name: &str) -> MemberInfo {
        MemberInfo { reference: true, ..member(project_id, name) }
    }

    fn hit(path: &str, chunk_id: i64) -> HybridHit {
        HybridHit {
            path: path.to_string(),
            chunk_id,
            snippet: format!("snippet for {path}"),
            source: Source::Keyword,
            line: None,
            page: None,
            label: None,
            symbol: None,
            score: 0.0,
        }
    }

    fn searched(project_id: Uuid, name: &str, hits: Vec<HybridHit>) -> MemberHits {
        MemberHits { project_id, member_name: name.to_string(), status: MemberStatus::Searched, hits, millis: 0 }
    }

    fn every(targets: Vec<Uuid>) -> RoutePlan {
        RoutePlan { targets, reason: RouteReason::Every, reserve: Vec::new() }
    }

    // --- plan_route ---

    /// A question that names a source searches it alone.
    #[test]
    fn a_named_source_narrows_the_search() {
        let (shattered, other) = (Uuid::new_v4(), Uuid::new_v4());
        let members = vec![member(shattered, "Shattered Realms"), member(other, "Other Project")];
        let plan = plan_route("what's new in Shattered Realms lately?", &members);
        assert_eq!(plan, RoutePlan::chosen(vec![shattered]));
        assert_eq!(plan.reason, RouteReason::Named);
    }

    /// With no source named, every source is searched: no cap, no order by
    /// activity, the knowledge base no different from the rest.
    #[test]
    fn every_source_is_searched_when_none_is_named() {
        let members: Vec<MemberInfo> = (0..7).map(|i| member(Uuid::new_v4(), &format!("Project {i}"))).collect();
        let plan = plan_route("can I merge my own pull request", &members);
        assert_eq!(plan.reason, RouteReason::Every);
        assert_eq!(plan.targets, members.iter().map(|m| m.project_id).collect::<Vec<_>>());
        assert!(plan.reserve.is_empty());
    }

    /// A reference repo is held back from a question about the team's own
    /// work, joins a question about the platform, and joins when named.
    #[test]
    fn a_reference_repo_joins_for_the_platform_or_when_named() {
        let (code, engine) = (Uuid::new_v4(), Uuid::new_v4());
        let members = vec![member(code, "game"), reference(engine, "hytale-server")];

        let own = plan_route("how much stamina does a block cost", &members);
        assert_eq!((own.targets.clone(), own.reserve.clone()), (vec![code], vec![engine]));

        let platform = plan_route("does the engine have a boss health bar", &members);
        assert_eq!((platform.targets.clone(), platform.reserve.clone()), (vec![code, engine], vec![]));

        let named = plan_route("where does hytale-server load worlds", &members);
        assert_eq!(named, RoutePlan::chosen(vec![engine]));
    }

    #[test]
    fn only_a_repo_that_is_only_reference_is_held_back() {
        assert!(is_reference(&[RepoKind::Reference]));
        assert!(!is_reference(&[RepoKind::Reference, RepoKind::Code]));
        assert!(!is_reference(&[RepoKind::Wiki]));
        assert!(!is_reference(&[]), "a repo whose kind is not said yet is searched");
    }

    #[test]
    fn a_question_about_the_platform_is_spotted() {
        assert!(asks_about_platform("Does the engine have a boss health bar?"));
        assert!(asks_about_platform("How do we add our own pins to the game's built-in minimap?"));
        assert!(asks_about_platform("Which shapes does it support out of the box?"));
        assert!(!asks_about_platform("How much stamina does it cost to take a hit while holding up a weapon or shield?"));
        assert!(!asks_about_platform("Who leads engineering?"), "whole words only");
    }

    /// The held-back reference repos are searched only when nothing else
    /// answered.
    #[test]
    fn the_reserve_is_searched_only_when_nothing_answers() {
        let (code, engine) = (Uuid::new_v4(), Uuid::new_v4());
        let plan = RoutePlan { targets: vec![code], reason: RouteReason::Every, reserve: vec![engine] };
        assert!(needs_reserve(&plan, &[searched(code, "game", vec![])]));
        assert!(!needs_reserve(&plan, &[searched(code, "game", vec![hit("src/a.rs", 1)])]));
        let near = HybridHit { source: Source::Semantic, ..hit("src/a.rs", 1) };
        assert!(needs_reserve(&plan, &[searched(code, "game", vec![near])]), "only the nearest meaning is no answer");
        assert!(!needs_reserve(&every(vec![code]), &[searched(code, "game", vec![])]), "nothing held back");

        let game = fixture_db_with_chunk("src/notes.md", "the quokka naps");
        let server = fixture_db_with_chunk("world/Loader.md", "worlds load from region files");
        let targets = vec![
            MemberDbHandle { project_id: code, name: "game", db: &game },
            MemberDbHandle { project_id: engine, name: "hytale-server", db: &server },
        ];
        let mut embedder = FakeEmbedder::new();
        let answered = execute_plan(&plan, &targets, &mut embedder, "quokka", 10);
        assert!(answered.member_status.iter().all(|s| s.project_id == code), "the reserve stays out");
        let fell_back = execute_plan(&plan, &targets, &mut embedder, "region files", 10);
        assert_eq!(fell_back.member_status.len(), 2);
        assert!(fell_back.results.iter().any(|r| r.project_id == engine && r.path == "world/Loader.md"));
    }

    // --- merge_routed ---

    #[test]
    fn unscored_hits_merge_by_rank() {
        // Hits with no relevance score to compare (all 0.0) fall back to
        // each hit's position in its own source's list, then the plan's order.
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let member_hits = vec![
            searched(a, "A", vec![hit("a/one.md", 1), hit("a/two.md", 2)]),
            searched(b, "B", vec![hit("b/one.md", 10), hit("b/two.md", 11), hit("b/three.md", 12)]),
        ];
        let paths: Vec<String> = merge_routed(&every(vec![a, b]), &member_hits, 10).results.into_iter().map(|r| r.path).collect();
        assert_eq!(paths, vec!["a/one.md", "b/one.md", "a/two.md", "b/two.md", "b/three.md"]);
    }

    /// Relevance decides across sources: a strong hit in the second repo
    /// beats a weak first hit in the first, whatever the plan's order, and a
    /// knowledge base gets no share held for it.
    #[test]
    fn scored_hits_merge_by_relevance_across_members() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let scored = |path: &str, id: i64, score: f64| HybridHit { score, ..hit(path, id) };
        let member_hits = vec![
            searched(a, "code", vec![scored("design.md", 1, 0.6), scored("spec.md", 2, 0.4)]),
            searched(b, "notes", vec![scored("SOW.md", 10, 4.2), scored("log.md", 11, 0.5)]),
        ];
        let paths: Vec<String> = merge_routed(&every(vec![a, b]), &member_hits, 10).results.into_iter().map(|r| r.path).collect();
        assert_eq!(paths, vec!["SOW.md", "design.md", "log.md", "spec.md"]);
    }

    #[test]
    fn limit_truncates_merged_results() {
        let a = Uuid::new_v4();
        let member_hits = vec![searched(a, "A", vec![hit("one.md", 1), hit("two.md", 2), hit("three.md", 3)])];
        let report = merge_routed(&every(vec![a]), &member_hits, 2);
        assert_eq!(report.results.len(), 2);
    }

    /// Every source searched is reported, slow or not, with or without hits.
    #[test]
    fn every_source_is_reported_and_a_slow_one_says_so() {
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let member_hits = vec![
            MemberHits { millis: SLOW_MILLIS + 300, ..searched(a, "big", vec![hit("x.md", 1)]) },
            MemberHits { status: MemberStatus::Unavailable, ..searched(b, "gone", vec![]) },
            MemberHits { status: MemberStatus::Building { embedded: 40, chunks: 100 }, ..searched(c, "new", vec![hit("y.md", 2)]) },
        ];
        let report = merge_routed(&every(vec![a, b, c]), &member_hits, 10);
        assert_eq!(report.member_status.len(), 3);
        assert!(report.member_status[0].slow());
        assert!(!report.member_status[1].slow());
        assert_eq!(report.results.len(), 2, "the slow and the building source's hits both count");
    }

    /// A source still building its meaning index is searched by keyword and
    /// reported with how far it has got; a source whose index would not open
    /// is reported unavailable.
    #[test]
    fn a_building_source_is_searched_and_a_missing_one_reported() {
        let (built, building, missing) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let built_db = fixture_db_with_chunk("notes/found.md", "the quokka naps");
        let building_db = scanned_db("notes/also.md", "another quokka sleeps");
        let targets = vec![
            MemberDbHandle { project_id: built, name: "Built", db: &built_db },
            MemberDbHandle { project_id: building, name: "Building", db: &building_db },
        ];
        let mut embedder = FakeEmbedder::new();
        let report = execute_plan(&every(vec![built, building, missing]), &targets, &mut embedder, "quokka", 10);
        let status = |id: Uuid| report.member_status.iter().find(|s| s.project_id == id).unwrap().status;
        assert_eq!(status(built), MemberStatus::Searched);
        if building_db.vec_available() {
            assert!(matches!(status(building), MemberStatus::Building { embedded: 0, chunks } if chunks > 0), "{:?}", status(building));
        }
        assert_eq!(status(missing), MemberStatus::Unavailable);
        assert!(report.results.iter().any(|r| r.project_id == built));
        assert!(report.results.iter().any(|r| r.project_id == building), "keyword hits while meaning builds");
    }

    // --- one source's search ---

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
    fn machine_text_is_spotted() {
        assert!(machine_written(&format!("const SEARCH = [{}];", "{\"a\":1},".repeat(300))));
        assert!(!machine_written("fn save() {\n    write();\n}\n"));
        assert!(!machine_written(&"A person wrote this line.\n".repeat(80)));
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

    // --- citations ---

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
        let member_hits = vec![searched(a, "A", vec![hit("notes\\windows-style.md", 1)])];
        let report = merge_routed(&every(vec![a]), &member_hits, 10);
        let result = &report.results[0];
        assert_eq!(result.address, format!("ken://{a}/notes/windows-style.md"), "backslashes must normalize to forward slashes");

        // Round-trip: strip the scheme, split project id from rel path.
        let rest = result.address.strip_prefix("ken://").unwrap();
        let (project_part, path_part) = rest.split_once('/').unwrap();
        assert_eq!(Uuid::parse_str(project_part).unwrap(), a);
        assert_eq!(path_part, "notes/windows-style.md");
    }

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
        assert_eq!(world.page.as_ref().and_then(|p| p.section), Some("Reference"), "Platform/ is in Reference");
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
        std::fs::write(dir.path().join("Platform/World.md"), "---\nchecked: 2026-09-05 f00218c\n---\n# World\n\nEach region loads from disk.\n").unwrap();
        let project = Project::create(dir.path(), "T").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        scan::scan(&project, &mut db).unwrap();

        let hits = search_member(&db, "region", None, 10).unwrap();
        let world = hits.iter().find(|h| h.path == "Platform/World.md").expect("found without any model");
        assert!(world.line.is_some());
        assert_eq!(world.page.as_ref().and_then(|p| p.checked.as_deref()), Some("2026-09-05"));

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

    /// A passage is found by the headings it sits under, and a code chunk by
    /// the symbol it holds, ahead of a file that only mentions the words.
    #[test]
    fn a_breadcrumb_or_a_symbol_finds_its_chunk() {
        use crate::project::Project;
        use crate::scan;
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("Design")).unwrap();
        std::fs::write(
            dir.path().join("Design/Payments.md"),
            "# Payments\n\nHow money moves.\n\n## Retry backoff\n\nEach wait doubles, to a minute at most.\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("Design/Other.md"), "# Other\n\nA long note on queues and dashboards, with retry mentioned once and backoff once, among many other things.\n").unwrap();
        std::fs::write(dir.path().join("books.py"), "def reconcile_ledger(month):\n    return month\n").unwrap();
        let project = Project::create(dir.path(), "T").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        scan::scan(&project, &mut db).unwrap();

        let hits = search_member(&db, "retry backoff", None, 10).unwrap();
        assert_eq!(hits.first().map(|h| h.path.as_str()), Some("Design/Payments.md"), "{hits:?}");
        assert_eq!(hits[0].line, Some(5), "the section under the heading");
        let hits = search_member(&db, "reconcile ledger", None, 10).unwrap();
        assert_eq!(hits.first().map(|h| h.path.as_str()), Some("books.py"), "{hits:?}");
    }

    /// A query naming a function leads with where it is defined, at its
    /// line, and the hit names it: the jump slice 2a's nudges gave, back
    /// through the code index's plain-word lookup.
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
        assert_eq!(hits[0].symbol.as_deref(), Some("get_current_user"));
    }

    /// Plain words find a definition in any casing, and the routed hit
    /// carries the symbol on.
    #[test]
    fn plain_words_reach_a_definition_and_the_routed_hit_names_it() {
        use crate::project::Project;
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/world.ts"), "export function loadWorld() {}\n\nexport function saveWorld(w: string) {\n  return w;\n}\n").unwrap();
        std::fs::write(dir.path().join("store.py"), "def save_world(w):\n    return w\n").unwrap();
        let project = Project::create(dir.path(), "T").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        crate::scan::scan(&project, &mut db).unwrap();
        let hits = search_member(&db, "how do we save world", None, 8).unwrap();
        let ts = hits.iter().find(|h| h.path == "src/world.ts").expect("saveWorld");
        assert_eq!(ts.line, Some(3), "{hits:?}");
        assert_eq!(ts.symbol.as_deref(), Some("saveWorld"));
        let py = hits.iter().find(|h| h.path == "store.py").expect("save_world");
        assert_eq!(py.symbol.as_deref(), Some("save_world"));

        let id = Uuid::new_v4();
        let report = merge_routed(&every(vec![id]), &[searched(id, "T", hits)], 10);
        let routed = report.results.iter().find(|r| r.path == "src/world.ts").unwrap();
        assert_eq!(routed.symbol.as_deref(), Some("saveWorld"));
    }

    /// A member with one file at `rel_path`, scanned but with no meaning
    /// index built.
    fn scanned_db(rel_path: &str, text: &str) -> Db {
        use crate::project::Project;
        let project_dir = tempfile::tempdir().unwrap();
        let file_path = project_dir.path().join(rel_path);
        std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
        std::fs::write(&file_path, format!("# note\n{text}\n")).unwrap();
        let project = Project::create(project_dir.path(), "T").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        crate::scan::scan(&project, &mut db).unwrap();
        db
    }

    /// A member with one file at `rel_path`, scanned and its meaning index
    /// built, for tests that need a real search rather than fixture hits.
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

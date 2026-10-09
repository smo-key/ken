//! Which later ruling supersedes or refines which earlier one, and on what
//! point (kb-trust, 2026-10-07). A decisions log is append-only: a ruling
//! that reverses or narrows an earlier one is a new entry, and often nothing
//! marks the old one. Claude answered "build the fix" from D-379 ("the server
//! never overwrites" anchors) because nothing said that D-410, a day later,
//! had ruled anchors are written only by the tools and the fix is not built.
//!
//! The pass reads a decisions log's entries from the index (one chunk per
//! entry), finds entries that may cover the same subject — the topics in each
//! entry's head, the named things they share (`/commands`, files,
//! identifiers), one citing the other, and how close their meaning is — puts
//! them in small clusters, and asks Claude once per cluster which later entry
//! supersedes or refines which earlier one, and on what point. Those pairs,
//! and the ones the log marks itself (`[SUPERSEDED BY D-302]`, `[REFINES]
//! D-425`), are stored in Ken's index, never in the log: search labels a
//! superseded entry's hit and ranks it under its successor, and read_document
//! says so beside the entry.
//!
//! Generic: any log whose entries carry an id and a date (`**D-410** ·
//! 2026-10-06 · topics — **TITLE**`), named by the team file (`decisions`) or
//! called `DECISIONS.md`.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use serde::Serialize;

use crate::authority::Roles;
use crate::chunker::{entry_date, entry_line, entry_start, entry_topics};
use crate::db::Db;
use crate::Result;

/// One dated entry of a log, as the index holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub id: String,
    pub date: String,
    /// The entry's first line in the file (1-based).
    pub line: i64,
    pub chunk_id: i64,
    pub topics: Vec<String>,
    /// The entry's text, from its first line (without the heading it sits
    /// under).
    pub text: String,
}

/// A later entry that supersedes or refines an earlier one on a point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Supersession {
    /// The log both entries are in, project-relative.
    pub log: String,
    pub earlier: String,
    pub later: String,
    /// `supersedes` or `refines`.
    pub relation: String,
    /// What changed, in a few words; empty when only the log's own marker
    /// says so.
    pub point: String,
    /// The later entry's words that settle it.
    pub evidence: String,
    pub earlier_line: i64,
    pub later_line: i64,
    /// `marked` (the log says so), `judged` (Claude read the two), or
    /// `marked+judged`.
    pub source: String,
}

impl Supersession {
    /// How a hit on the earlier entry says it: `superseded by D-410 on who
    /// writes anchors`, `refined by D-437`.
    pub fn earlier_note(&self) -> String {
        let verb = if self.relation == "refines" { "refined by" } else { "superseded by" };
        match self.point.is_empty() {
            true => format!("{verb} {}", self.later),
            false => format!("{verb} {} on {}", self.later, self.point),
        }
    }
}

/// The logs a pass reads in a repo: the decisions log the team file names,
/// else every indexed page called `DECISIONS.md`.
pub fn log_paths(db: &Db, roles: &Roles) -> Result<Vec<String>> {
    Ok(db.authority_candidates(false)?.into_iter().filter(|p| roles.is_decisions_log(p)).collect())
}

/// The dated entries of the log at `log`, oldest first (by date, then by the
/// number in the id). Undated entries (`U-4`) are left out: nothing orders
/// them.
pub fn entries(db: &Db, log: &str) -> Result<Vec<Entry>> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (chunk_id, line, text) in db.chunks_of(log)? {
        let Some(first) = entry_line(&text) else { continue };
        let (Some((id, _)), Some(date)) = (entry_start(first), entry_date(first)) else { continue };
        if !seen.insert(id.clone()) {
            continue;
        }
        let at = text.find(first).unwrap_or(0);
        out.push(Entry { id, date, line: line.unwrap_or(0), chunk_id, topics: entry_topics(first), text: text[at..].trim().to_string() });
    }
    out.sort_by(|a, b| order_key(a).cmp(&order_key(b)));
    Ok(out)
}

fn order_key(e: &Entry) -> (String, u64) {
    (e.date.clone(), id_number(&e.id))
}

fn id_number(id: &str) -> u64 {
    id.rsplit('-').next().and_then(|n| n.parse().ok()).unwrap_or(0)
}

/// An entry id as written in a log: `D-410`, `SR-1067`.
const ID: &str = r"[A-Z][A-Z0-9]{0,7}-\d{1,6}";

/// The pairs a log marks itself: an entry that says it is `[SUPERSEDED BY
/// D-302]` or `[REFINED BY D-258: why]` (it is the earlier), or that it
/// `[SUPERSEDES] D-114` or `[REFINES D-132]` (it is the later). Only pairs
/// whose two entries are both in the log.
pub fn marked(log: &str, entries: &[Entry]) -> Vec<Supersession> {
    let by = regex::Regex::new(&format!(r"\[(?i:(superseded|refined))\s+(?i:by)\s+({ID})\s*(?::\s*([^\]]*))?\]")).expect("pattern");
    let of = regex::Regex::new(&format!(r"\[(?i:(supersedes|refines))\]?\**\s*((?:{ID}[\s,]*(?:and\s+)?)+)")).expect("pattern");
    let ids = regex::Regex::new(ID).expect("pattern");
    let at: HashMap<&str, &Entry> = entries.iter().map(|e| (e.id.as_str(), e)).collect();
    let relation = |verb: &str| if verb.to_ascii_lowercase().starts_with("refine") { "refines" } else { "supersedes" };
    let mut out: Vec<Supersession> = Vec::new();
    let mut push = |earlier: &Entry, later: &Entry, rel: &str, point: &str| {
        if earlier.id == later.id || out.iter().any(|s| s.earlier == earlier.id && s.later == later.id) {
            return;
        }
        out.push(Supersession {
            log: log.to_string(),
            earlier: earlier.id.clone(),
            later: later.id.clone(),
            relation: rel.to_string(),
            point: clip(point.trim(), POINT_MAX),
            evidence: String::new(),
            earlier_line: earlier.line,
            later_line: later.line,
            source: "marked".into(),
        });
    };
    for e in entries {
        for c in by.captures_iter(&e.text) {
            if let Some(later) = at.get(&c[2]) {
                push(e, later, relation(&c[1]), c.get(3).map_or("", |m| m.as_str()));
            }
        }
        for c in of.captures_iter(&e.text) {
            for id in ids.find_iter(&c[2]) {
                if let Some(earlier) = at.get(id.as_str()) {
                    push(earlier, e, relation(&c[1]), "");
                }
            }
        }
    }
    out
}

/// How candidate clusters are made.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tuning {
    /// The least a pair of entries scores ([`scored_pairs`]) to be put in one
    /// cluster.
    pub min_score: f64,
    /// Each entry's best few pairs at most.
    pub neighbours: usize,
    /// Most entries one Claude call reads.
    pub max_cluster: usize,
}

impl Default for Tuning {
    fn default() -> Self {
        Tuning { min_score: 1.0, neighbours: 3, max_cluster: 8 }
    }
}

/// What an entry is about, as sets of words: its head's topics and the named
/// things in its text, each word with how rare it is in the log.
struct Profile {
    topics: HashSet<String>,
    names: HashSet<String>,
    cites: HashSet<String>,
}

fn profile(e: &Entry) -> Profile {
    let word = |w: &str| {
        let w = w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
        // A plural is its singular: "anchors" meets "anchor".
        match w.strip_suffix('s') {
            Some(s) if s.len() > 3 && !s.ends_with('s') => s.to_string(),
            _ => w,
        }
    };
    let topics = e.topics.iter().flat_map(|t| t.split_whitespace().map(word).collect::<Vec<_>>()).filter(|w| w.len() > 2).collect();
    let named = regex::Regex::new(r"`([^`\n]{2,60})`|(/[a-z][a-z0-9_]{2,})|\b([A-Z][a-z]+(?:[A-Z][a-z0-9]+)+)\b|\b([a-z0-9]+(?:_[a-z0-9]+)+)\b|\b([\w-]+\.(?:json|java|md|ts|rs|py|kt|svelte|cs|yml|yaml|toml))\b").expect("pattern");
    let names = named
        .captures_iter(&e.text)
        .filter_map(|c| (1..=5).find_map(|i| c.get(i)).map(|m| m.as_str().trim().to_lowercase()))
        .filter(|n| n.len() > 2)
        .collect();
    let id = regex::Regex::new(ID).expect("pattern");
    let cites = id.find_iter(&e.text).map(|m| m.as_str().to_string()).filter(|c| *c != e.id).collect();
    Profile { topics, names, cites }
}

/// How rare each word is across the log: words in many entries ("lore",
/// "tools") say little about two entries sharing a subject.
fn rarity(sets: &[&HashSet<String>]) -> HashMap<String, f64> {
    let mut df: HashMap<&str, usize> = HashMap::new();
    for s in sets {
        for w in s.iter() {
            *df.entry(w.as_str()).or_default() += 1;
        }
    }
    let n = sets.len().max(1) as f64;
    df.into_iter().map(|(w, d)| (w.to_string(), (n / d as f64).ln().max(0.0))).collect()
}

/// The share of the smaller set's weight two sets have in common, by rarity.
fn overlap(a: &HashSet<String>, b: &HashSet<String>, idf: &HashMap<String, f64>) -> f64 {
    let weight = |s: &HashSet<String>| s.iter().map(|w| idf.get(w).copied().unwrap_or(0.0)).sum::<f64>();
    let shared: f64 = a.intersection(b).map(|w| idf.get(w).copied().unwrap_or(0.0)).sum();
    let least = weight(a).min(weight(b));
    if least <= 0.0 {
        0.0
    } else {
        (shared / least).min(1.0)
    }
}

fn cosine(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0f64, 0.0f64, 0.0f64);
    for (x, y) in a.iter().zip(b) {
        dot += (*x as f64) * (*y as f64);
        na += (*x as f64) * (*x as f64);
        nb += (*y as f64) * (*y as f64);
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

/// What sharing a subject is worth, by signal: meaning is a cosine (about
/// 0.5 to 0.9 for prose), the others shares from 0 to 1.
const W_TOPICS: f64 = 0.35;
const W_NAMES: f64 = 0.35;
const W_CITES: f64 = 0.3;

/// Every pair of entries scored for sharing a subject, best first: (score,
/// earlier index, later index). Pairs that share nothing named and no topic
/// and do not cite each other are left out, whatever their meaning.
pub fn scored_pairs(entries: &[Entry], vectors: &HashMap<i64, Vec<f32>>) -> Vec<(f64, usize, usize)> {
    let profiles: Vec<Profile> = entries.iter().map(profile).collect();
    let topic_idf = rarity(&profiles.iter().map(|p| &p.topics).collect::<Vec<_>>());
    let name_idf = rarity(&profiles.iter().map(|p| &p.names).collect::<Vec<_>>());
    let mut out = Vec::new();
    for i in 0..entries.len() {
        for j in i + 1..entries.len() {
            let (a, b) = (&profiles[i], &profiles[j]);
            let topics = overlap(&a.topics, &b.topics, &topic_idf);
            let names = overlap(&a.names, &b.names, &name_idf);
            let cites = if b.cites.contains(&entries[i].id) || a.cites.contains(&entries[j].id) { 1.0 } else { 0.0 };
            if topics == 0.0 && names == 0.0 && cites == 0.0 {
                continue;
            }
            let meaning = match (vectors.get(&entries[i].chunk_id), vectors.get(&entries[j].chunk_id)) {
                (Some(x), Some(y)) => cosine(x, y),
                _ => 0.5,
            };
            out.push((meaning + W_TOPICS * topics + W_NAMES * names + W_CITES * cites, i, j));
        }
    }
    out.sort_by(|a, b| b.0.total_cmp(&a.0));
    out
}

/// Clusters of entries (indexes into `entries`, oldest first) that may cover
/// one subject: each entry's best [`Tuning::neighbours`] pairs that score at
/// least [`Tuning::min_score`], joined best pair first while a cluster stays
/// within [`Tuning::max_cluster`]. Clusters of one are dropped.
pub fn clusters(entries: &[Entry], vectors: &HashMap<i64, Vec<f32>>, tuning: &Tuning) -> Vec<Vec<usize>> {
    let pairs = scored_pairs(entries, vectors);
    let mut taken = vec![0usize; entries.len()];
    let mut edges = Vec::new();
    for &(score, i, j) in &pairs {
        if score < tuning.min_score {
            break;
        }
        if taken[i] < tuning.neighbours || taken[j] < tuning.neighbours {
            taken[i] += 1;
            taken[j] += 1;
            edges.push((i, j));
        }
    }
    let mut parent: Vec<usize> = (0..entries.len()).collect();
    let mut size = vec![1usize; entries.len()];
    fn root(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    for (i, j) in edges {
        let (a, b) = (root(&mut parent, i), root(&mut parent, j));
        if a != b && size[a] + size[b] <= tuning.max_cluster {
            parent[b] = a;
            size[a] += size[b];
        }
    }
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..entries.len() {
        let r = root(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }
    let mut out: Vec<Vec<usize>> = groups.into_values().filter(|g| g.len() > 1).collect();
    for g in &mut out {
        g.sort_unstable();
    }
    out.sort();
    out
}

/// Longest stretch of an entry a prompt quotes, in characters.
const ENTRY_CHARS: usize = 3_000;
/// Longest point and evidence kept, in characters.
const POINT_MAX: usize = 90;
const EVIDENCE_MAX: usize = 300;

/// The question for one cluster: the entries, oldest first, and the pairs to
/// name as JSON.
pub fn prompt(log: &str, cluster: &[&Entry]) -> String {
    let mut p = format!(
        "These are entries from a team's decisions log, `{log}`. Each is a ruling with an id and a date, \
oldest first. A later entry can supersede an earlier one (reverse or replace what it said) or refine it (narrow \
or amend it) on some point, often without saying so.\n\n\
Name every pair where a LATER entry changes what an EARLIER entry said on the same subject: someone who read only \
the earlier entry would get that point wrong. Do not name entries that only relate, repeat, build on or agree with \
each other. Use only the entries below; do not use any tools.\n\n\
Answer with JSON only:\n\
[{{\"earlier\": \"<id>\", \"later\": \"<id>\", \"relation\": \"supersedes\" or \"refines\", \
\"point\": \"<what changed, at most 10 words>\", \"evidence\": \"<the later entry's words that settle it, quoted, \
at most 30 words>\"}}]\n\
Answer [] when there is none.\n\nEntries:\n"
    );
    for e in cluster {
        let text: String = e.text.chars().take(ENTRY_CHARS).collect();
        let cut = if text.len() < e.text.len() { " […]" } else { "" };
        p.push_str(&format!("\n--- {} · {} (line {})\n{text}{cut}\n", e.id, e.date, e.line));
    }
    p
}

/// The pairs in Claude's answer for `cluster`: both ids in the cluster, the
/// later one later in the log's order.
pub fn parse_judgement(raw: &str, log: &str, cluster: &[&Entry]) -> Vec<Supersession> {
    let Ok(v) = crate::local_llm::parse_json_lenient(raw) else { return Vec::new() };
    let items = match v {
        serde_json::Value::Array(a) => a,
        serde_json::Value::Object(_) => vec![v],
        _ => return Vec::new(),
    };
    let find = |id: &str| cluster.iter().find(|e| e.id.eq_ignore_ascii_case(id.trim()));
    let mut out: Vec<Supersession> = Vec::new();
    for item in items {
        let s = |k: &str| item.get(k).and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
        let (Some(earlier), Some(later)) = (find(&s("earlier")), find(&s("later"))) else { continue };
        if order_key(earlier) >= order_key(later) || out.iter().any(|p| p.earlier == earlier.id && p.later == later.id) {
            continue;
        }
        let relation = if s("relation").to_ascii_lowercase().starts_with("refine") { "refines" } else { "supersedes" };
        out.push(Supersession {
            log: log.to_string(),
            earlier: earlier.id.clone(),
            later: later.id.clone(),
            relation: relation.into(),
            point: clip(s("point").trim_end_matches('.'), POINT_MAX),
            evidence: clip(&s("evidence"), EVIDENCE_MAX),
            earlier_line: earlier.line,
            later_line: later.line,
            source: "judged".into(),
        });
    }
    out
}

fn clip(s: &str, max: usize) -> String {
    match s.chars().count() > max {
        true => format!("{}…", s.chars().take(max).collect::<String>().trim_end()),
        false => s.to_string(),
    }
}

/// The pass over one log: its entries, its clusters, the calls made and the
/// pairs found.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub log: String,
    pub entries: usize,
    /// Entries with a meaning vector (the rest cluster by words alone).
    pub vectors: usize,
    pub clusters: Vec<Vec<String>>,
    pub calls: usize,
    pub failed: usize,
    pub prompt_chars: usize,
    pub marked: usize,
    pub pairs: Vec<Supersession>,
}

/// One Claude call: a prompt in, its answer's text out.
pub type Judge<'a> = dyn Fn(&str) -> Result<String> + Sync + 'a;

/// Run the pass over every decisions log in the repo the index `db` is of
/// (`roles` from its team file): cluster each log's entries and, with a
/// `judge`, ask it once per cluster, `workers` calls at a time; then store
/// the judged and marked pairs in the index, replacing the log's last ones.
/// Without a judge it only reports the clusters and stores nothing.
pub fn run(db: &Db, roles: &Roles, tuning: &Tuning, judge: Option<&Judge<'_>>, workers: usize) -> Result<Vec<Report>> {
    let mut reports = Vec::new();
    for log in log_paths(db, roles)? {
        let entries = entries(db, &log)?;
        if entries.len() < 2 {
            continue;
        }
        let vectors = db.chunk_vectors(&entries.iter().map(|e| e.chunk_id).collect::<Vec<_>>())?;
        let groups = clusters(&entries, &vectors, tuning);
        let prompts: Vec<(Vec<&Entry>, String)> = groups
            .iter()
            .map(|g| {
                let members: Vec<&Entry> = g.iter().map(|&i| &entries[i]).collect();
                let p = prompt(&log, &members);
                (members, p)
            })
            .collect();
        let marked = marked(&log, &entries);
        let mut report = Report {
            log: log.clone(),
            entries: entries.len(),
            vectors: vectors.len(),
            clusters: groups.iter().map(|g| g.iter().map(|&i| entries[i].id.clone()).collect()).collect(),
            prompt_chars: prompts.iter().map(|(_, p)| p.len()).sum(),
            marked: marked.len(),
            ..Default::default()
        };
        let Some(judge) = judge else {
            report.pairs = marked;
            reports.push(report);
            continue;
        };
        let next = AtomicUsize::new(0);
        let found: Mutex<Vec<Supersession>> = Mutex::new(Vec::new());
        let failed = AtomicUsize::new(0);
        std::thread::scope(|s| {
            for _ in 0..workers.max(1) {
                s.spawn(|| loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    let Some((members, p)) = prompts.get(i) else { break };
                    match judge(p) {
                        Ok(answer) => found.lock().unwrap().extend(parse_judgement(&answer, &log, members)),
                        Err(_) => {
                            failed.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                });
            }
        });
        report.calls = prompts.len();
        report.failed = failed.into_inner();
        let mut pairs = found.into_inner().unwrap_or_default();
        for m in marked {
            match pairs.iter_mut().find(|p| p.earlier == m.earlier && p.later == m.later) {
                Some(p) => {
                    p.source = "marked+judged".into();
                    p.relation = m.relation;
                }
                None => pairs.push(m),
            }
        }
        pairs.sort_by(|a, b| (id_number(&a.later), id_number(&a.earlier)).cmp(&(id_number(&b.later), id_number(&b.earlier))));
        db.replace_supersessions(&log, &pairs)?;
        report.pairs = pairs;
        reports.push(report);
    }
    Ok(reports)
}

/// What a hit on entry `id` of `log` adds to its label for the pairs:
/// `superseded by D-410 on who writes anchors` on the earlier entry,
/// `supersedes D-379` on the later. None when no pair touches it, or the
/// index has no pairs (made before the pass, or opened read-only before it
/// was upgraded).
///
/// Tickets get no note. Whether a later ruling contradicts what a ticket
/// decided is a judgement over each ticket and the rulings near it, many
/// more calls than the log's clusters; the cheap stand-in, flagging a ticket
/// that cites a superseded ruling, labelled tickets with refinements on
/// points they never touch (SR-1184 "cites D-337, refined by D-367").
pub fn note(db: &Db, log: &str, id: &str) -> Result<Option<String>> {
    let (by, of) = db.supersessions_of(log, id)?;
    // The latest successor speaks for the rest.
    if let Some(s) = by.iter().max_by_key(|s| id_number(&s.later)) {
        let more = if by.len() > 1 { format!(" (+{} more)", by.len() - 1) } else { String::new() };
        return Ok(Some(format!("{}{more}", s.earlier_note())));
    }
    if !of.is_empty() {
        let ids: Vec<&str> = of.iter().map(|s| s.earlier.as_str()).take(3).collect();
        let more = if of.len() > 3 { format!(" (+{} more)", of.len() - 3) } else { String::new() };
        let verb = if of.iter().all(|s| s.relation == "refines") { "refines" } else { "supersedes" };
        return Ok(Some(format!("{verb} {}{more}", ids.join(", "))));
    }
    Ok(None)
}

/// A hit on a log entry that a later one supersedes brings its successor in
/// just above it (or lifts the successor there when it is already a hit):
/// both stay, since the earlier one still holds on every other point. A
/// search keeps one hit per file, so without this the earlier ruling stood
/// alone for its log.
pub fn lead_with_successors(db: &Db, hits: &mut Vec<crate::search::HybridHit>) -> Result<()> {
    if !db.has_supersessions()? {
        return Ok(());
    }
    let entry_of = |h: &crate::search::HybridHit| entry_line(&h.snippet).and_then(entry_start).map(|(id, _)| id);
    let mut lifted: Vec<(usize, f64)> = Vec::new();
    let mut added: Vec<crate::search::HybridHit> = Vec::new();
    for hit in hits.iter() {
        let Some(id) = entry_of(hit) else { continue };
        let (by, _) = db.supersessions_of(&hit.path, &id)?;
        let Some(s) = by.iter().max_by_key(|s| id_number(&s.later)) else { continue };
        let above = hit.score + 1e-6;
        if let Some(at) = hits.iter().position(|h| h.path == hit.path && entry_of(h).as_deref() == Some(s.later.as_str())) {
            lifted.push((at, above));
            continue;
        }
        if added.iter().any(|h| h.path == hit.path && entry_of(h).as_deref() == Some(s.later.as_str())) {
            continue;
        }
        if let Some((chunk_id, text, line)) = entry_chunk(db, &hit.path, &s.later, s.later_line)? {
            added.push(crate::search::HybridHit {
                path: hit.path.clone(),
                chunk_id,
                snippet: text,
                source: hit.source,
                line: Some(line),
                page: hit.page.clone(),
                score: above,
            });
        }
    }
    if lifted.is_empty() && added.is_empty() {
        return Ok(());
    }
    for (at, score) in lifted {
        if hits[at].score < score {
            hits[at].score = score;
        }
    }
    hits.extend(added);
    hits.sort_by(|a, b| b.score.total_cmp(&a.score));
    Ok(())
}

/// The chunk of `log` that holds entry `id` (chunk id, text, line), for a
/// hit that leads to it.
pub fn entry_chunk(db: &Db, log: &str, id: &str, line: i64) -> Result<Option<(i64, String, i64)>> {
    if line > 0 {
        if let Some((chunk_id, text)) = db.chunk_at_line(log, line)? {
            if entry_line(&text).and_then(entry_start).is_some_and(|(e, _)| e == id) {
                let at = db.chunk_line(chunk_id)?.unwrap_or(line);
                return Ok(Some((chunk_id, text, at)));
            }
        }
    }
    for (chunk_id, at, text) in db.chunks_of(log)? {
        if entry_line(&text).and_then(entry_start).is_some_and(|(e, _)| e == id) {
            return Ok(Some((chunk_id, text, at.unwrap_or(0))));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, date: &str, topics: &str, body: &str) -> Entry {
        let text = format!("**{id}** · {date} · {topics} — **{body}**");
        let line = entry_line(&text).unwrap();
        Entry {
            id: id.into(),
            date: date.into(),
            line: id_number(id) as i64,
            chunk_id: id_number(id) as i64,
            topics: entry_topics(line),
            text,
        }
    }

    fn log() -> Vec<Entry> {
        vec![
            entry("D-379", "2026-10-05", "spawn anchors, world tool", "THE SERVER NEVER OVERWRITES ANCHORS. `/sr_anchor` place and move write the data file."),
            entry("D-380", "2026-10-05", "lore, world", "THE DAWNLANDS HAVE FOUR BIOMES."),
            entry("D-381", "2026-10-05", "loot, chests", "CHEST POOLS ROLL TWICE. See `ChestPool.json`."),
            entry("D-302", "2026-09-30", "loot, chests", "CHEST POOLS ROLL ONCE. [SUPERSEDED BY D-381: rolls per chest]"),
            entry("D-410", "2026-10-06", "anchors, tools", "ANCHORS ARE PLACED ONLY THROUGH THE TOOLS, NEVER `/sr_anchor` PLACE/MOVE; THE MERGE FIX IS NOT BUILT."),
            entry("D-411", "2026-10-06", "lore, world", "SARNATHE HAS FIVE AREAS. **[REFINES]** D-380"),
        ]
    }

    fn sorted(mut e: Vec<Entry>) -> Vec<Entry> {
        e.sort_by(|a, b| order_key(a).cmp(&order_key(b)));
        e
    }

    #[test]
    fn the_log_marks_some_pairs_itself() {
        let entries = sorted(log());
        let m = marked("decisions/DECISIONS.md", &entries);
        let pairs: Vec<(&str, &str, &str, &str)> = m.iter().map(|s| (s.earlier.as_str(), s.later.as_str(), s.relation.as_str(), s.point.as_str())).collect();
        assert_eq!(pairs, vec![("D-302", "D-381", "supersedes", "rolls per chest"), ("D-380", "D-411", "refines", "")]);
        assert!(m.iter().all(|s| s.source == "marked"));
        assert_eq!(m[0].earlier_note(), "superseded by D-381 on rolls per chest");
        assert_eq!(m[1].earlier_note(), "refined by D-411");
    }

    #[test]
    fn entries_sharing_a_subject_cluster_and_the_rest_do_not() {
        let entries = sorted(log());
        let idx = |id: &str| entries.iter().position(|e| e.id == id).unwrap();
        // No vectors: meaning is even, so topics, names and citations decide.
        let groups = clusters(&entries, &HashMap::new(), &Tuning { min_score: 0.6, neighbours: 2, max_cluster: 4 });
        let together = |a: &str, b: &str| groups.iter().any(|g| g.contains(&idx(a)) && g.contains(&idx(b)));
        assert!(together("D-379", "D-410"), "anchors, and `/sr_anchor` in both: {groups:?}");
        assert!(together("D-302", "D-381"), "{groups:?}");
        assert!(!together("D-379", "D-381"), "{groups:?}");
        assert!(groups.iter().all(|g| g.len() <= 4));
        // Close in meaning but nothing shared: never paired.
        let vectors: HashMap<i64, Vec<f32>> = entries.iter().map(|e| (e.chunk_id, vec![1.0, 0.0])).collect();
        let pairs = scored_pairs(&entries, &vectors);
        assert!(!pairs.iter().any(|&(_, i, j)| [i, j] == [idx("D-379"), idx("D-381")] || [i, j] == [idx("D-381"), idx("D-379")]));
    }

    #[test]
    fn a_cluster_is_cut_at_its_size() {
        // Ten rulings on anchors among ten on other things: what all ten share
        // is rare in the log, so every pair of them scores.
        let mut entries: Vec<Entry> = (1..=10).map(|i| entry(&format!("D-{i:03}"), "2026-10-01", "anchors", "ANCHORS `/sr_anchor`.")).collect();
        entries.extend((11..=20).map(|i| entry(&format!("D-{i:03}"), "2026-10-02", &format!("topic{i}"), "SOMETHING ELSE.")));
        let groups = clusters(&entries, &HashMap::new(), &Tuning { min_score: 0.5, neighbours: 9, max_cluster: 4 });
        assert!(groups.iter().all(|g| g.len() <= 4 && g.iter().all(|&i| i < 10)), "{groups:?}");
        assert_eq!(groups.iter().map(Vec::len).sum::<usize>(), 10, "every anchors ruling in a cluster: {groups:?}");
    }

    #[test]
    fn a_judgement_keeps_only_later_over_earlier_within_the_cluster() {
        let entries = sorted(log());
        let cluster: Vec<&Entry> = entries.iter().filter(|e| e.id == "D-379" || e.id == "D-410").collect();
        let p = prompt("decisions/DECISIONS.md", &cluster);
        assert!(p.contains("--- D-379 · 2026-10-05 (line 379)") && p.contains("--- D-410 · 2026-10-06"), "{p}");
        assert!(p.find("D-379 ·").unwrap() < p.find("D-410 ·").unwrap(), "oldest first");
        let answer = r#"Here you go:
```json
[{"earlier": "D-379", "later": "D-410", "relation": "supersedes", "point": "who writes anchors, and whether the merge fix is built.", "evidence": "placed and edited only through the tools"},
 {"earlier": "D-410", "later": "D-379", "relation": "supersedes", "point": "backwards", "evidence": ""},
 {"earlier": "D-379", "later": "D-999", "relation": "refines", "point": "not in the cluster", "evidence": ""}]
```"#;
        let pairs = parse_judgement(answer, "decisions/DECISIONS.md", &cluster);
        assert_eq!(pairs.len(), 1, "{pairs:?}");
        let s = &pairs[0];
        assert_eq!((s.earlier.as_str(), s.later.as_str(), s.earlier_line, s.later_line), ("D-379", "D-410", 379, 410));
        assert_eq!(s.earlier_note(), "superseded by D-410 on who writes anchors, and whether the merge fix is built");
        assert!(parse_judgement("[]", "x", &cluster).is_empty());
        assert!(parse_judgement("no JSON at all", "x", &cluster).is_empty());
    }

    fn indexed_log() -> (Db, String) {
        let mut db = Db::open_in_memory().unwrap();
        let mut text = String::from("# DECISIONS\n\n## THE LOG\n\n### 2026-10-06\n\n");
        for e in sorted(log()).iter().rev() {
            text.push_str(&e.text);
            text.push_str("\n\n");
        }
        let log = "decisions/DECISIONS.md".to_string();
        db.upsert_file(&log, "md", 1, 1, "indexed", None, &text).unwrap();
        let chunks = crate::chunker::chunk_file(&log, &text, &crate::chunker::IndexProfile::default_for(&log));
        db.upsert_chunks(&log, &chunks, crate::kenignore::Tier::Full).unwrap();
        (db, log)
    }

    #[test]
    fn a_pass_reads_the_log_from_the_index_and_stores_the_pairs() {
        let (db, log) = indexed_log();
        let entries = entries(&db, &log).unwrap();
        assert_eq!(entries.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), vec!["D-302", "D-379", "D-380", "D-381", "D-410", "D-411"]);
        let d410 = entries.iter().find(|e| e.id == "D-410").unwrap();
        assert!(d410.text.starts_with("**D-410** · 2026-10-06") && d410.line > 0, "{d410:?}");

        let tuning = Tuning { min_score: 0.6, neighbours: 2, max_cluster: 4 };
        // A dry run reports and stores nothing.
        let dry = run(&db, &Roles::default(), &tuning, None, 1).unwrap();
        assert_eq!((dry.len(), dry[0].calls, dry[0].marked), (1, 0, 2));
        assert!(!dry[0].clusters.is_empty());
        assert!(!db.has_supersessions().unwrap());

        let asked = AtomicUsize::new(0);
        let judge = |p: &str| -> Result<String> {
            asked.fetch_add(1, Ordering::SeqCst);
            Ok(if p.contains("--- D-410") && p.contains("--- D-379") {
                r#"[{"earlier":"D-379","later":"D-410","relation":"supersedes","point":"who writes anchors","evidence":"only through the tools"}]"#.into()
            } else {
                "[]".into()
            })
        };
        let reports = run(&db, &Roles::default(), &tuning, Some(&judge), 2).unwrap();
        assert_eq!(reports[0].calls, asked.load(Ordering::SeqCst));
        let pairs: Vec<(String, String, String)> = db.supersessions(None).unwrap().into_iter().map(|s| (s.earlier, s.later, s.source)).collect();
        assert!(pairs.contains(&("D-379".into(), "D-410".into(), "judged".into())), "{pairs:?}");
        assert!(pairs.contains(&("D-302".into(), "D-381".into(), "marked".into())), "{pairs:?}");

        assert_eq!(note(&db, &log, "D-379").unwrap().as_deref(), Some("superseded by D-410 on who writes anchors"));
        assert_eq!(note(&db, &log, "D-410").unwrap().as_deref(), Some("supersedes D-379"));
        assert_eq!(note(&db, &log, "D-411").unwrap().as_deref(), Some("refines D-380"));
        assert_eq!(note(&db, &log, "D-381").unwrap().as_deref(), Some("supersedes D-302"));
        let chunk = entry_chunk(&db, &log, "D-410", 0).unwrap().unwrap();
        assert!(chunk.1.contains("**D-410**"));

        assert_eq!(note(&db, &log, "D-001").unwrap(), None);

        // Run again: the log's pairs are replaced, not added to.
        run(&db, &Roles::default(), &tuning, Some(&judge), 1).unwrap();
        assert_eq!(db.supersessions(Some(&log)).unwrap().len(), pairs.len());
    }
}

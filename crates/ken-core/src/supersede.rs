//! Which later ruling supersedes or refines which earlier one, and on what
//! point (kb-trust, 2026-10-07). A decisions log is append-only: a ruling
//! that reverses or narrows an earlier one is a new entry, and often nothing
//! marks the old one. Claude answered "build the fix" from D-379 ("the server
//! never overwrites" anchors) because nothing said that D-410, a day later,
//! had ruled anchors are written only by the tools and the fix is not built.
//!
//! Wired in (knowledge-core decision 25):
//!
//! - **Markers on every scan** ([`store_marked`]): the pairs the log marks
//!   itself (`[SUPERSEDED BY D-302]`, `[REFINES] D-425`, `supersedes: D-114`)
//!   are stored in Ken's index, so search labels a superseded entry's hit
//!   and ranks it under its successor, and the MCP `read` says so beside it.
//! - **The Claude pass on new entries only** ([`next_cluster`], [`judged`]):
//!   the log's entries are read from the index (one chunk per entry), put in
//!   small clusters of entries that may cover the same subject (the topics
//!   in each entry's head, the named things they share, one citing the
//!   other, how close their meaning is), and Claude is asked about a cluster
//!   only when it holds an entry the pass has not read. The app asks in the
//!   background, each call a turn of the one background throttle.
//! - **Each pair it finds is a finding**, never written by itself. When a
//!   person confirms one ([`confirm`]), Ken writes `supersedes:` on the later
//!   entry and `[SUPERSEDED BY D-nnn]` on the earlier ([`mark_in_log`]) as
//!   one commit; the next scan stores it as a marker.
//!
//! Generic: any log whose entries carry an id and a date (`**D-410** ·
//! 2026-10-06 · topics — **TITLE**`), named by the settings file
//! (`decisions`) or called `DECISIONS.md`.

use std::collections::{HashMap, HashSet};

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

/// The logs a pass reads in a repo: the decisions log the settings file names,
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
/// `[SUPERSEDES] D-114`, `[REFINES D-132]` or, in the one entry format's own
/// line, `supersedes: D-114` (it is the later). Only pairs whose two entries
/// are both in the log.
pub fn marked(log: &str, entries: &[Entry]) -> Vec<Supersession> {
    let by = regex::Regex::new(&format!(r"\[(?i:(superseded|refined))\s+(?i:by)\s+({ID})\s*(?::\s*([^\]]*))?\]")).expect("pattern");
    let of = regex::Regex::new(&format!(r"(?m)(?:\[|^\s*)(?i:(supersedes|refines))\]?:?\**\s*((?:{ID}[\s,]*(?:and\s+)?)+)")).expect("pattern");
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

/// Store the pairs every decisions log marks itself ([`marked`]), on every
/// scan (decision 25): search labels a superseded entry and leads with its
/// successor from these. They replace what was stored for each log, so a
/// marker taken out of the log is gone from search too. Returns how many
/// pairs were stored.
pub fn store_marked(db: &Db, roles: &Roles) -> Result<usize> {
    let mut n = 0;
    for log in log_paths(db, roles)? {
        let pairs = marked(&log, &entries(db, &log)?);
        n += pairs.len();
        db.replace_supersessions(&log, &pairs)?;
    }
    Ok(n)
}

/// The check state that remembers which entries the Claude pass has read.
const SEEN: &str = "supersede_seen";

fn seen(db: &Db) -> Result<HashMap<String, HashSet<String>>> {
    Ok(db.check_state(SEEN)?.and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default())
}

fn store_seen(db: &Db, seen: &HashMap<String, HashSet<String>>) -> Result<()> {
    db.store_check_state(SEEN, &serde_json::to_string(seen).unwrap_or_else(|_| "{}".into()))
}

/// One Claude call the pass wants: a cluster of one log's entries with at
/// least one the pass has not read yet.
#[derive(Debug, Clone, PartialEq)]
pub struct Pending {
    pub log: String,
    /// The cluster's entries, oldest first.
    pub ids: Vec<String>,
    /// Those the pass had not read.
    pub new: Vec<String>,
    pub prompt: String,
}

/// The next cluster to ask Claude about (decision 25): the pass runs on new
/// entries only, so a cluster is asked about only when it holds an entry the
/// pass has not read; a new entry is clustered with the whole log, so it
/// meets the older rulings it may change. The first time the pass sees a
/// log, every entry is new to it. New entries no cluster takes in are marked
/// read at once. None when nothing waits; no call is needed then.
pub fn next_cluster(db: &Db, roles: &Roles, tuning: &Tuning) -> Result<Option<Pending>> {
    let mut seen_all = seen(db)?;
    let mut changed = false;
    let mut out = None;
    for log in log_paths(db, roles)? {
        let entries = entries(db, &log)?;
        let seen = seen_all.entry(log.clone()).or_default();
        let new: HashSet<&str> = entries.iter().map(|e| e.id.as_str()).filter(|id| !seen.contains(*id)).collect();
        if new.is_empty() {
            continue;
        }
        let vectors = db.chunk_vectors(&entries.iter().map(|e| e.chunk_id).collect::<Vec<_>>())?;
        let groups = clusters(&entries, &vectors, tuning);
        if let Some(g) = groups.iter().find(|g| g.iter().any(|&i| new.contains(entries[i].id.as_str()))) {
            let members: Vec<&Entry> = g.iter().map(|&i| &entries[i]).collect();
            out = Some(Pending {
                log: log.clone(),
                ids: members.iter().map(|e| e.id.clone()).collect(),
                new: members.iter().map(|e| e.id.clone()).filter(|id| new.contains(id.as_str())).collect(),
                prompt: prompt(&log, &members),
            });
            break;
        }
        // No cluster holds a new entry: nothing near it to supersede.
        seen.extend(new.iter().map(|s| s.to_string()));
        changed = true;
    }
    if changed {
        store_seen(db, &seen_all)?;
    }
    Ok(out)
}

/// What the supersession findings name as their source kind.
pub const FINDING_KIND: &str = "supersession";

/// File what Claude found in `pending`'s cluster: each pair is a finding
/// (decision 25), unless the log already marks it or a finding for it was
/// filed before (open, confirmed or dismissed). The cluster's new entries
/// are then read. Returns how many findings were filed.
pub fn judged(db: &mut Db, pending: &Pending, reply: &str, now: i64) -> Result<usize> {
    let entries = entries(db, &pending.log)?;
    let members: Vec<&Entry> = entries.iter().filter(|e| pending.ids.contains(&e.id)).collect();
    let marked = marked(&pending.log, &entries);
    let filed_before = crate::findings::ever(db, FINDING_KIND)?;
    let mut n = 0;
    for s in parse_judgement(reply, &pending.log, &members) {
        let known = marked.iter().any(|m| m.earlier == s.earlier && m.later == s.later)
            || filed_before.iter().any(|f| f.source == pending.log && f.subject == s.later && f.against == s.earlier);
        if known {
            continue;
        }
        let verb = if s.relation == "refines" { "refines" } else { "supersedes" };
        let on = if s.point.is_empty() { String::new() } else { format!(" on {}", s.point) };
        let f = crate::findings::Finding {
            kind: FINDING_KIND.into(),
            source: pending.log.clone(),
            subject: s.later.clone(),
            against: s.earlier.clone(),
            against_kind: "ruling".into(),
            text: format!("{} {verb} {}{on}. Confirm it and Ken writes it into the log.", s.later, s.earlier),
            quote: s.evidence.clone(),
            relation: s.relation.clone(),
        };
        crate::findings::file(db, &f, now)?;
        n += 1;
    }
    let mut seen_all = seen(db)?;
    seen_all.entry(pending.log.clone()).or_default().extend(pending.new.iter().cloned());
    store_seen(db, &seen_all)?;
    Ok(n)
}

/// The lines `id`'s entry spans in a log's lines: from its first line to
/// the next entry or heading. Fenced examples are not entries.
fn entry_span(lines: &[String], id: &str) -> Option<(usize, usize)> {
    let mut fence = false;
    let mut start: Option<usize> = None;
    for (i, l) in lines.iter().enumerate() {
        if l.trim_start().starts_with("```") {
            fence = !fence;
            if start.is_some() {
                return start.map(|s| (s, i));
            }
            continue;
        }
        if fence {
            continue;
        }
        let begins = crate::chunker::entry_start(l).map(|(e, _)| e);
        match (start, begins) {
            (None, Some(e)) if e == id => start = Some(i),
            (Some(s), Some(_)) => return Some((s, i)),
            (Some(s), None) if l.trim_start().starts_with('#') => return Some((s, i)),
            _ => {}
        }
    }
    start.map(|s| (s, lines.len()))
}

/// A decisions log with a confirmed pair written into it (decision 25): the
/// later entry says `supersedes: D-earlier` (or `refines:`), and the earlier
/// one carries `[SUPERSEDED BY D-later]` (or `[REFINED BY …]`). Every entry
/// form reads: the plain and bold one-line forms take the marker at the end
/// of their line and the field on an indented line under it; the one entry
/// format (`## D-nnn · date · topic`) takes the marker on the line after its
/// bold ruling and the field among its own fields. None when an entry is
/// not in the log, or both are already written.
pub fn mark_in_log(text: &str, earlier: &str, later: &str, relation: &str) -> Option<String> {
    let crlf = text.contains("\r\n");
    let t = text.replace("\r\n", "\n");
    let mut lines: Vec<String> = t.lines().map(str::to_string).collect();
    let refines = relation == "refines";
    let marker = format!("[{} BY {later}]", if refines { "REFINED" } else { "SUPERSEDED" });
    let field = if refines { "refines" } else { "supersedes" };
    let (es, ee) = entry_span(&lines, earlier)?;
    let (ls, le) = entry_span(&lines, later)?;
    let has_marker = lines[es..ee].iter().any(|l| l.contains(&marker));
    let field_at = (ls..le).find(|&i| lines[i].trim_start().to_ascii_lowercase().starts_with(&format!("{field}:")));
    let has_field = field_at.is_some_and(|i| regex::Regex::new(&format!(r"\b{}\b", regex::escape(earlier))).is_ok_and(|r| r.is_match(&lines[i])));
    if has_marker && has_field {
        return None;
    }
    let heading = |i: usize, lines: &[String]| lines[i].trim_start().starts_with('#');
    let mark_earlier = |lines: &mut Vec<String>| {
        if has_marker {
            return;
        }
        if heading(es, lines) {
            let after = (es + 1..ee).find(|&i| lines[i].trim_start().starts_with("**")).unwrap_or(es);
            lines.insert(after + 1, marker.clone());
        } else {
            let line = lines[es].trim_end().to_string();
            lines[es] = format!("{line} {marker}");
        }
    };
    let mark_later = |lines: &mut Vec<String>| {
        if has_field {
            return;
        }
        if let Some(i) = field_at {
            let line = lines[i].trim_end().to_string();
            lines[i] = format!("{line}, {earlier}");
            return;
        }
        let last = (ls..le).rev().find(|&i| !lines[i].trim().is_empty()).unwrap_or(ls);
        // The one entry format keeps its fields flush; a one-line entry's
        // fields sit indented under it.
        let indent: String = if heading(ls, lines) {
            String::new()
        } else {
            let own: String = lines[last].chars().take_while(|c| c.is_whitespace()).collect();
            if last > ls && !own.is_empty() { own } else { "  ".to_string() }
        };
        lines.insert(last + 1, format!("{indent}{field}: {earlier}"));
    };
    // The entry further down first, so the other's lines stay where they are.
    if es > ls {
        mark_earlier(&mut lines);
        mark_later(&mut lines);
    } else {
        mark_later(&mut lines);
        mark_earlier(&mut lines);
    }
    let out = lines.join("\n") + "\n";
    Some(if crlf { out.replace('\n', "\r\n") } else { out })
}

/// A person confirmed a supersession finding: Ken writes the pair into its
/// log ([`mark_in_log`]) and commits that one file in the knowledge base at
/// `root` (`git::commit_paths`, never pushed). Returns the commit, None
/// when the log already said so or the knowledge base is not a git repo.
pub fn confirm(root: &std::path::Path, f: &crate::findings::Finding) -> std::result::Result<Option<String>, String> {
    if f.kind != FINDING_KIND {
        return Err("only a supersession is confirmed".into());
    }
    let path = root.join(&f.source);
    let text = std::fs::read_to_string(&path).map_err(|e| format!("could not read {}: {e}", f.source))?;
    let Some(marked) = mark_in_log(&text, &f.against, &f.subject, &f.relation) else {
        let lines: Vec<String> = text.lines().map(str::to_string).collect();
        if let Some(gone) = [&f.against, &f.subject].into_iter().find(|id| entry_span(&lines, id).is_none()) {
            return Err(format!("{gone} is not in {} any more", f.source));
        }
        return Ok(None);
    };
    std::fs::write(&path, marked).map_err(|e| format!("could not write {}: {e}", f.source))?;
    let verb = if f.relation == "refines" { "refines" } else { "supersedes" };
    let message = format!("decisions: {} {verb} {}\n\n{}", f.subject, f.against, f.text);
    crate::git::commit_paths(root, std::slice::from_ref(&f.source), &message)
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
                label: None,
                symbol: None,
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

        // The one entry format says it on its own line.
        let mut later = entry("D-412", "2026-10-07", "loot, chests", "Chest pools roll three times.");
        later.text.push_str("\nWhy: more loot.\nsupersedes: D-381\n");
        let mut with_field = sorted(log());
        with_field.push(later);
        let m = marked("decisions/DECISIONS.md", &with_field);
        assert!(m.iter().any(|s| s.earlier == "D-381" && s.later == "D-412" && s.relation == "supersedes"), "{m:?}");
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
        index(&mut db, &log, &text);
        (db, log)
    }

    fn index(db: &mut Db, log: &str, text: &str) {
        db.upsert_file(log, "md", 1, 1, "indexed", None, text).unwrap();
        let chunks = crate::chunker::chunk_file(log, text, &crate::chunker::IndexProfile::default_for(log));
        db.upsert_chunks(log, &chunks).unwrap();
    }

    #[test]
    fn markers_are_stored_and_the_pass_reads_new_entries_only() {
        let (mut db, log) = indexed_log();
        let roles = Roles::default();
        let entries = entries(&db, &log).unwrap();
        assert_eq!(entries.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), vec!["D-302", "D-379", "D-380", "D-381", "D-410", "D-411"]);
        let d410 = entries.iter().find(|e| e.id == "D-410").unwrap();
        assert!(d410.text.starts_with("**D-410** · 2026-10-06") && d410.line > 0, "{d410:?}");

        // Markers, on every scan: what the log says itself, and nothing else.
        assert_eq!(store_marked(&db, &roles).unwrap(), 2);
        assert_eq!(note(&db, &log, "D-302").unwrap().as_deref(), Some("superseded by D-381 on rolls per chest"));
        assert_eq!(note(&db, &log, "D-381").unwrap().as_deref(), Some("supersedes D-302"));
        assert_eq!(note(&db, &log, "D-411").unwrap().as_deref(), Some("refines D-380"));
        assert_eq!(note(&db, &log, "D-001").unwrap(), None);
        let chunk = entry_chunk(&db, &log, "D-410", 0).unwrap().unwrap();
        assert!(chunk.1.contains("**D-410**"));

        // The pass: every entry is new the first time, and each cluster is
        // asked about once.
        let tuning = Tuning { min_score: 0.6, neighbours: 2, max_cluster: 4 };
        let mut calls = 0;
        while let Some(p) = next_cluster(&db, &roles, &tuning).unwrap() {
            calls += 1;
            assert!(calls < 10, "a cluster asked about twice: {p:?}");
            assert!(!p.new.is_empty() && p.prompt.contains(&format!("--- {}", p.ids[0])));
            let anchors = p.ids.contains(&"D-379".to_string()) && p.ids.contains(&"D-410".to_string());
            let reply = if anchors {
                r#"[{"earlier":"D-379","later":"D-410","relation":"supersedes","point":"who writes anchors","evidence":"only through the tools"},
                    {"earlier":"D-302","later":"D-381","relation":"supersedes","point":"rolls","evidence":""}]"#
            } else {
                "[]"
            };
            judged(&mut db, &p, reply, 5).unwrap();
        }
        assert!(calls >= 1);
        let found: Vec<crate::findings::Finding> = crate::findings::open(&db).unwrap().into_iter().map(|(_, f)| f).collect();
        assert_eq!(found.len(), 1, "a pair the log marks itself is no finding: {found:?}");
        let f = &found[0];
        assert_eq!((f.kind.as_str(), f.subject.as_str(), f.against.as_str(), f.relation.as_str()), ("supersession", "D-410", "D-379", "supersedes"));
        assert_eq!(f.page(), log, "it opens the log");
        assert_eq!(note(&db, &log, "D-379").unwrap(), None, "a judged pair is not in search until a person confirms it");
        assert!(next_cluster(&db, &roles, &tuning).unwrap().is_none(), "nothing new: no call");

        // A new entry: only a cluster holding it is asked about.
        let mut text = db.get_text(&log).unwrap().unwrap();
        text.push_str("**D-412** · 2026-10-07 · anchors, tools — **ANCHORS MOVE ONLY IN THE TOOLS, NEVER `/sr_anchor` MOVE.**\n");
        index(&mut db, &log, &text);
        let p = next_cluster(&db, &roles, &tuning).unwrap().expect("the new entry is read");
        assert_eq!(p.new, vec!["D-412".to_string()]);
        let again = r#"[{"earlier":"D-379","later":"D-410","relation":"supersedes","point":"who writes anchors","evidence":""}]"#;
        judged(&mut db, &p, again, 6).unwrap();
        assert_eq!(crate::findings::open(&db).unwrap().len(), 1, "a pair already filed is not filed again");
        assert!(next_cluster(&db, &roles, &tuning).unwrap().is_none());
    }

    #[test]
    fn a_confirmed_pair_is_written_into_every_entry_form() {
        // The one entry format: the marker under the bold ruling, the field
        // among the entry's own.
        let log = "# Decisions\n\n## D-001 · 2026-10-01 · saves\n\n**Saves are global.**\n\nWhy: simple.\nsources: [[n]]\n\n\
                   ## D-002 · 2026-10-02 · saves\n\n**Saves are per region.**\n\nWhy: size.\nsources: [[m]]\n";
        let out = mark_in_log(log, "D-001", "D-002", "supersedes").unwrap();
        assert!(out.contains("**Saves are global.**\n[SUPERSEDED BY D-002]\n"), "{out}");
        assert!(out.ends_with("sources: [[m]]\nsupersedes: D-001\n"), "{out}");
        assert!(mark_in_log(&out, "D-001", "D-002", "supersedes").is_none(), "already written");
        let rulings = crate::drift::parse_rulings(&out);
        assert!(rulings.iter().find(|r| r.id == "D-001").is_some_and(|r| r.superseded), "{rulings:?}");

        // The bold one-line form: the marker on its line, the field under it.
        let bold = "**D-379** · 2026-10-05 · anchors — **THE SERVER NEVER OVERWRITES.**\n  sources: game:a.rs\n\n\
                    **D-410** · 2026-10-06 · anchors — **ONLY THE TOOLS.**\n";
        let out = mark_in_log(bold, "D-379", "D-410", "supersedes").unwrap();
        assert!(out.starts_with("**D-379** · 2026-10-05 · anchors — **THE SERVER NEVER OVERWRITES.** [SUPERSEDED BY D-410]\n  sources: game:a.rs\n"), "{out}");
        assert!(out.contains("**ONLY THE TOOLS.**\n  supersedes: D-379\n"), "{out}");

        // The plain form, refining, with the field already there.
        let plain = "D-001 - 2026-01-01 - topic - THE RULING.\nD-002 - 2026-01-02 - topic - NARROWER.\n  refines: D-000\n";
        let out = mark_in_log(plain, "D-001", "D-002", "refines").unwrap();
        assert!(out.starts_with("D-001 - 2026-01-01 - topic - THE RULING. [REFINED BY D-002]\n"), "{out}");
        assert!(out.contains("  refines: D-000, D-001\n"), "{out}");
        assert!(mark_in_log(plain, "D-001", "D-999", "supersedes").is_none(), "an entry the log lacks");
    }

    #[test]
    fn confirming_writes_the_log_and_commits_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git").args(["-c", "user.name=T", "-c", "user.email=t@t"]).args(args).current_dir(root).output().unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).to_string()
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(root.join("Decisions")).unwrap();
        let log = "Decisions/DECISIONS.md";
        std::fs::write(root.join(log), "# Decisions\n\n**D-379** · 2026-10-05 · anchors — **NEVER.**\n\n**D-410** · 2026-10-06 · anchors — **TOOLS.**\n").unwrap();
        std::fs::write(root.join("notes.md"), "a person's own work\n").unwrap();
        git(&["add", "--", log]);
        git(&["commit", "-q", "-m", "log"]);
        let f = crate::findings::Finding {
            kind: FINDING_KIND.into(),
            source: log.into(),
            subject: "D-410".into(),
            against: "D-379".into(),
            against_kind: "ruling".into(),
            text: "D-410 supersedes D-379 on who writes anchors.".into(),
            relation: "supersedes".into(),
            ..Default::default()
        };
        assert!(confirm(root, &f).unwrap().is_some());
        let text = std::fs::read_to_string(root.join(log)).unwrap();
        assert!(text.contains("**NEVER.** [SUPERSEDED BY D-410]") && text.contains("**TOOLS.**\n  supersedes: D-379"), "{text}");
        let shown = git(&["show", "--name-only", "--format=%s", "HEAD"]);
        assert!(shown.starts_with("decisions: D-410 supersedes D-379") && !shown.contains("notes.md"), "{shown}");
        assert_eq!(confirm(root, &f).unwrap(), None, "said once");
        let gone = crate::findings::Finding { against: "D-001".into(), ..f.clone() };
        assert!(confirm(root, &gone).is_err());
    }

    #[test]
    fn a_scan_reads_the_markers() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("Decisions")).unwrap();
        let mut log = "# Decisions\n\n## D-001 · 2026-10-01 · saves\n\n**Saves are global.**\n[SUPERSEDED BY D-002]\n\n\
                       ## D-002 · 2026-10-02 · saves\n\n**Saves are per region.**\n\nsupersedes: D-001\n"
            .to_string();
        for n in 3..=6 {
            log.push_str(&format!("\n## D-00{n} · 2026-10-0{n} · topic {n}\n\n**Ruling {n}.**\n"));
        }
        let log = log.as_str();
        std::fs::write(dir.path().join("Decisions/DECISIONS.md"), log).unwrap();
        let project = crate::project::Project::create(dir.path(), "kb").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        crate::scan::scan(&project, &mut db).unwrap();
        assert_eq!(note(&db, "Decisions/DECISIONS.md", "D-001").unwrap().as_deref(), Some("superseded by D-002"));

        // The marker taken out of the log: gone from search on the next scan.
        let bare = log.replace("[SUPERSEDED BY D-002]\n", "").replace("supersedes: D-001\n", "");
        std::fs::write(dir.path().join("Decisions/DECISIONS.md"), bare).unwrap();
        crate::scan::scan(&project, &mut db).unwrap();
        assert_eq!(note(&db, "Decisions/DECISIONS.md", "D-001").unwrap(), None);
    }
}

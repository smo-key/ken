//! The project graph (knowledge-core decision 21): what in a project points
//! at what, from exact sources only, kept in the knowledge base's index.
//!
//! - **Nodes** are written `kind:id`: a page, a note or a ticket by its path
//!   in the knowledge base (`page:Design/Saves.md`), a decisions-log entry by
//!   its id (`decision:D-012`), a code file or symbol by repo and path
//!   (`code:game:src/save.rs`, `symbol:game:src/save.rs#save`), a topic
//!   (`topic:saves`), a repo (`repo:game`), and a commit that cites a ticket
//!   or a ruling (`commit:game@1a2b3c4`).
//! - **Edges**, each with its kind:
//!   - `link`: a page links to another ([`on_scan`]);
//!   - `cites`: a page's `sources:`, or a ruling's sources line, names a code
//!     file, a note or a ruling;
//!   - `changed`: a note changed a page (the ingest card's pages);
//!   - `supersedes` and `refines`: a later ruling over an earlier one (the
//!     markers the log carries, `supersede::store_marked`);
//!   - `in`: a ruling is an entry of its decisions log;
//!   - `code-cites`: a code comment cites a ruling the log has
//!     ([`store_code_citations`]);
//!   - `commit`: a commit names a ticket or a ruling the knowledge base has
//!     ([`read_commits`]);
//!   - `imports` and `uses`: read live from each repo's code index when
//!     [`related`] is asked, never copied here;
//!   - `topic`: a page is about a topic, tagged by Claude in the same call
//!     that writes the page: the ingest plan and the drift re-read ask for
//!     its topics ([`topics_ask`], [`tag_written`]). There is no other
//!     topic extraction, and no model reads anything else.
//! - **Exposed as `related`** ([`related`]): a node's neighbours by edge kind,
//!   nearest first.
//!
//! Each kind of edge is made by one thing (`origin`): the scan of the
//! knowledge base makes the edges its own index holds, again on every scan;
//! the code citations are made again after each drift run; commits are read
//! once each as a repo's branch moves; topic tags are made by a write.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::db::Db;
use crate::ingest::CodeSource;
use crate::links::{Link, LinkKind, Resolver};
use crate::Result;

/// The edges a knowledge base's scan makes, from its own index.
pub const ORIGIN_SCAN: &str = "scan";
/// The edges from code comments citing a ruling.
pub const ORIGIN_CODE: &str = "code";
/// The edges from commits citing a ticket or a ruling.
pub const ORIGIN_COMMITS: &str = "commits";
/// The topic tags a write made.
pub const ORIGIN_TOPICS: &str = "topics";

/// Edge kinds in the order [`related`] lists them at one distance: what
/// replaces or grounds a node first, its looser neighbours last.
pub const EDGE_ORDER: [&str; 11] =
    ["supersedes", "refines", "cites", "changed", "code-cites", "link", "commit", "imports", "uses", "in", "topic"];

/// Most topics one page carries.
pub const MAX_TOPICS: usize = 5;
/// Most commits read from one repo the first time, newest first.
pub const FIRST_COMMITS: usize = 500;
/// Most commits read from one repo at a time after that.
pub const MORE_COMMITS: usize = 2_000;

/// One node of the graph.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub struct Node {
    /// `page`, `note`, `ticket`, `decision`, `code`, `symbol`, `topic`,
    /// `repo` or `commit`.
    pub kind: String,
    pub id: String,
}

impl Node {
    fn new(kind: &str, id: impl Into<String>) -> Node {
        Node { kind: kind.to_string(), id: id.into() }
    }

    /// A file of the knowledge base by its path: a ticket in `tickets/`, a
    /// note (`contenttype::ContentType::Note`), else a page.
    pub fn of_path(path: &str) -> Node {
        let path = path.replace('\\', "/");
        if crate::day::is_ticket_path(&path) {
            Node::new("ticket", path)
        } else if crate::contenttype::of(&path) == crate::contenttype::ContentType::Note {
            Node::new("note", path)
        } else {
            Node::new("page", path)
        }
    }

    pub fn decision(id: &str) -> Node {
        Node::new("decision", id.trim())
    }

    pub fn code(repo: &str, path: &str) -> Node {
        Node::new("code", format!("{repo}:{}", path.replace('\\', "/")))
    }

    pub fn symbol(repo: &str, path: &str, name: &str) -> Node {
        Node::new("symbol", format!("{repo}:{}#{name}", path.replace('\\', "/")))
    }

    pub fn topic(name: &str) -> Node {
        Node::new("topic", topic_name(name))
    }

    pub fn repo(name: &str) -> Node {
        Node::new("repo", name)
    }

    pub fn commit(repo: &str, sha: &str) -> Node {
        Node::new("commit", format!("{repo}@{sha}"))
    }

    /// How it is stored: `kind:id`.
    pub fn key(&self) -> String {
        format!("{}:{}", self.kind, self.id)
    }

    /// A stored key read back. None when it has no known kind.
    pub fn parse(key: &str) -> Option<Node> {
        let (kind, id) = key.split_once(':')?;
        const KINDS: [&str; 9] = ["page", "note", "ticket", "decision", "code", "symbol", "topic", "repo", "commit"];
        (KINDS.contains(&kind) && !id.is_empty()).then(|| Node::new(kind, id))
    }

    /// A file of the knowledge base this node is (a page, a note, a ticket).
    pub fn kb_path(&self) -> Option<&str> {
        matches!(self.kind.as_str(), "page" | "note" | "ticket").then_some(self.id.as_str())
    }

    /// The repo and path of a code file or symbol, and the symbol's name.
    pub fn code_place(&self) -> Option<(&str, &str, Option<&str>)> {
        match self.kind.as_str() {
            "code" => self.id.split_once(':').map(|(repo, path)| (repo, path, None)),
            "symbol" => {
                let (repo, rest) = self.id.split_once(':')?;
                let (path, name) = rest.rsplit_once('#')?;
                Some((repo, path, Some(name)))
            }
            _ => None,
        }
    }
}

/// A topic as it is stored: lowercase, single-spaced, at most 60 characters.
pub fn topic_name(raw: &str) -> String {
    let words: Vec<String> = raw.split_whitespace().map(|w| w.to_lowercase()).collect();
    words.join(" ").trim_matches(|c: char| !c.is_alphanumeric()).chars().take(60).collect()
}

/// One stored edge, between two node keys.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct Edge {
    pub src: String,
    pub dst: String,
    /// One of [`EDGE_ORDER`].
    pub kind: String,
    /// What it says beside the kind: a citation as written, a note's change,
    /// a commit's subject, the line a comment is on. Often empty.
    pub detail: String,
}

impl Edge {
    pub fn new(src: &Node, dst: &Node, kind: &str, detail: &str) -> Edge {
        Edge { src: src.key(), dst: dst.key(), kind: kind.to_string(), detail: detail.trim().to_string() }
    }
}

/// Edges with each (src, dst, kind) once, a node never its own neighbour.
#[derive(Default)]
struct Edges {
    seen: HashSet<(String, String, String)>,
    out: Vec<Edge>,
}

impl Edges {
    fn push(&mut self, src: &Node, dst: &Node, kind: &str, detail: &str) {
        if src == dst {
            return;
        }
        let e = Edge::new(src, dst, kind, detail);
        if self.seen.insert((e.src.clone(), e.dst.clone(), e.kind.clone())) {
            self.out.push(e);
        }
    }
}

/// The node a `sources:` entry names, when it names one exactly: a code file
/// (`repo:path`, `repo@sha:path`), a note or page by `[[name]]` that one
/// page answers, or a ruling id (`D-012`). A name no page answers, or two
/// do, is no edge.
fn source_node(resolver: &Resolver, raw: &str) -> Option<Node> {
    match crate::drift::classify(raw) {
        crate::drift::Citation::Code(c) => Some(Node::code(&c.repo, &c.path)),
        crate::drift::Citation::Cross => {
            let t = raw.trim().trim_matches(|ch| ch == '"' || ch == '\'' || ch == '`').trim();
            if let Some(inner) = t.strip_prefix("[[") {
                let name = inner.split("]]").next().unwrap_or(inner);
                let name = name.split(['|', '#']).next().unwrap_or(name).trim();
                let to = resolver.resolve(&Link { kind: LinkKind::Name, target: name.to_string() });
                (to.len() == 1).then(|| Node::of_path(&to[0]))
            } else {
                let id: String = t.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
                Some(Node::decision(&id))
            }
        }
        crate::drift::Citation::Skip => None,
    }
}

/// The edges the knowledge base's own index holds (`ORIGIN_SCAN`): page
/// links, `sources:` citations, each ruling's entry in its log and its
/// sources, the supersessions the log marks, and the pages each ingested
/// note changed. No model.
pub fn scan_edges(db: &Db, root: &Path) -> Result<Vec<Edge>> {
    let resolver = Resolver::from_db(db)?;
    let mut edges = Edges::default();
    for (from, link) in db.all_page_links()? {
        let to = resolver.resolve(&link);
        // A name two pages claim is ambiguous (a lint finding), not a link;
        // an embedded image is not a page.
        if to.len() == 1 && crate::chunker::is_markdown(&to[0]) {
            edges.push(&Node::of_path(&from), &Node::of_path(&to[0]), "link", "");
        }
    }
    for page in db.page_paths()? {
        let Some(meta) = db.page_meta(&page)? else { continue };
        let from = Node::of_path(&page);
        for s in &meta.sources {
            if let Some(to) = source_node(&resolver, s) {
                edges.push(&from, &to, "cites", s);
            }
        }
    }
    let roles = crate::authority::Roles::of_repo(root);
    for log in crate::supersede::log_paths(db, &roles)? {
        let Some(text) = db.get_text(&log)? else { continue };
        let log_node = Node::of_path(&log);
        for ruling in crate::drift::parse_rulings(&text) {
            let entry = Node::decision(&ruling.id);
            edges.push(&entry, &log_node, "in", "");
            for s in &ruling.sources {
                if let Some(to) = source_node(&resolver, s) {
                    edges.push(&entry, &to, "cites", s);
                }
            }
        }
    }
    for s in db.supersessions(None)? {
        let kind = if s.relation == "refines" { "refines" } else { "supersedes" };
        edges.push(&Node::decision(&s.later), &Node::decision(&s.earlier), kind, &s.point);
    }
    for item in db.review_items_of_kind(crate::ingest::REVIEW_KIND, 100_000)? {
        let Some(card) = crate::ingest::card_of(item.payload.as_deref()) else { continue };
        let note = Node::of_path(&card.placement.note);
        for p in &card.pages {
            edges.push(&note, &Node::of_path(&p.path), "changed", &p.label);
        }
    }
    Ok(edges.out)
}

/// The graph's part of every scan of a knowledge base: its own edges made
/// again ([`scan_edges`]), and the topic tags of pages no longer on disk
/// dropped. Returns how many edges the scan made.
pub fn on_scan(db: &mut Db, root: &Path) -> Result<usize> {
    let edges = scan_edges(db, root)?;
    db.replace_graph_edges(ORIGIN_SCAN, &edges)?;
    let gone: Vec<String> = db
        .graph_edges_of(ORIGIN_TOPICS)?
        .into_iter()
        .map(|e| e.src)
        .collect::<HashSet<_>>()
        .into_iter()
        .filter(|src| Node::parse(src).and_then(|n| n.kb_path().map(|p| !root.join(p).is_file())).unwrap_or(true))
        .collect();
    if !gone.is_empty() {
        db.delete_graph_edges_from(ORIGIN_TOPICS, &gone)?;
    }
    Ok(edges.len())
}

/// The code comments that cite a ruling the log has, as `code-cites` edges
/// (`ORIGIN_CODE`), made again from `citations` (`lint::rulings_in_code`).
/// Returns how many edges.
pub fn store_code_citations(db: &mut Db, citations: &[crate::lint::CodeRuling]) -> Result<usize> {
    let mut edges = Edges::default();
    for c in citations.iter().filter(|c| c.known) {
        edges.push(&Node::code(&c.repo, &c.path), &Node::decision(&c.id), "code-cites", &format!("line {}", c.line));
    }
    db.replace_graph_edges(ORIGIN_CODE, &edges.out)?;
    Ok(edges.out.len())
}

/// The ids the knowledge base has, each with its node: every ticket's id
/// (`day::parse_ticket`: its `id`, else its file name) and every
/// decisions-log entry.
pub fn known_ids(db: &Db, root: &Path) -> Result<HashMap<String, Node>> {
    let mut out = HashMap::new();
    for path in db.page_paths()?.into_iter().filter(|p| crate::day::is_ticket_path(p)) {
        let text = db.get_text(&path)?.unwrap_or_default();
        out.insert(crate::day::parse_ticket(&path, &text).id, Node::of_path(&path));
    }
    let roles = crate::authority::Roles::of_repo(root);
    for log in crate::supersede::log_paths(db, &roles)? {
        if let Some(text) = db.get_text(&log)? {
            for r in crate::drift::parse_rulings(&text) {
                out.insert(r.id.clone(), Node::decision(&r.id));
            }
        }
    }
    Ok(out)
}

/// The `commit` edges for commits citing what `known` holds: each commit's
/// node to the ticket or ruling it names, with its date and subject.
pub fn commit_edges(repo: &str, commits: &[crate::git::Citing], known: &HashMap<String, Node>) -> Vec<Edge> {
    let mut edges = Edges::default();
    for c in commits {
        let from = Node::commit(repo, &c.sha);
        for id in &c.ids {
            if let Some(to) = known.get(id) {
                edges.push(&from, to, "commit", &format!("{} {}", c.date, c.subject));
            }
        }
    }
    edges.out
}

/// Read the commits of each of `repos` (name and checkout; the knowledge base
/// itself among them) that cite a ticket or a ruling the knowledge base at
/// `root` has, as `commit` edges (`ORIGIN_COMMITS`). Each repo is read from
/// the head read last time to its head now, so a commit is read once; the
/// first time, its newest [`FIRST_COMMITS`]. A repo whose head has not
/// moved costs one git call. Only ids known when a commit is read become
/// edges. Returns how many edges were new.
pub fn read_commits(db: &mut Db, root: &Path, repos: &[(String, PathBuf)]) -> Result<usize> {
    let mut known: Option<HashMap<String, Node>> = None;
    let mut added = 0;
    for (name, dir) in repos {
        let Some(head) = crate::git::output(dir, &["rev-parse", "HEAD"]) else { continue };
        let state = format!("graph_commits:{name}");
        let last = db.check_state(&state)?;
        if last.as_deref() == Some(head.as_str()) {
            continue;
        }
        let commits = match &last {
            Some(l) => crate::git::commits_citing(dir, &format!("{l}..{head}"), MORE_COMMITS)
                .or_else(|| crate::git::commits_citing(dir, &head, FIRST_COMMITS)),
            None => crate::git::commits_citing(dir, &head, FIRST_COMMITS),
        };
        let Some(commits) = commits else { continue };
        if !commits.is_empty() {
            if known.is_none() {
                known = Some(known_ids(db, root)?);
            }
            let edges = commit_edges(name, &commits, known.as_ref().unwrap_or(&HashMap::new()));
            added += db.add_graph_edges(ORIGIN_COMMITS, &edges)?;
        }
        db.store_check_state(&state, &head)?;
    }
    Ok(added)
}

// ---------------------------------------------------------------------
// Topic tags, at write time only
// ---------------------------------------------------------------------

/// A page worth tagging: Markdown, not an index, the log, the brief or a
/// template.
pub fn may_be_tagged(path: &str) -> bool {
    if !crate::chunker::is_markdown(path) {
        return false;
    }
    let lower = path.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    !["index.md", "log.md", "start-here.md", "claude.md", "agents.md"].contains(&name)
        && !lower.starts_with("templates/")
        && !lower.starts_with(".ken/")
}

/// The topics in use, most used first, at most `n`.
pub fn topics_in_use(db: &Db, n: usize) -> Result<Vec<String>> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for e in db.graph_edges_of(ORIGIN_TOPICS)? {
        if let Some(t) = Node::parse(&e.dst) {
            *counts.entry(t.id).or_default() += 1;
        }
    }
    let mut out: Vec<(String, usize)> = counts.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Ok(out.into_iter().take(n).map(|(t, _)| t).collect())
}

/// What the prompt that writes a page (the ingest plan, the drift re-read)
/// asks about its topics: short lowercase names of what it is about, the
/// topics already in use offered first so pages about one thing share a tag.
pub fn topics_ask(in_use: &[String]) -> String {
    let in_use = if in_use.is_empty() { "(none yet)".to_string() } else { in_use.join(", ") };
    format!(
        "1 to {MAX_TOPICS} topics: short lowercase names (one to three words) of the features, systems or areas of work \
         it is about, so pages about the same thing share a tag. Use a topic already in use when one fits: {in_use}. \
         Never a person, a date or a word like \"notes\""
    )
}

/// The topics a JSON list gives each page, `[{"page": …, "topics": […]}]`
/// (or that list under `pages`): (path, topics), only for pages in `asked`,
/// each page once, each topic once and at most [`MAX_TOPICS`].
pub fn topics_from(items: &Value, asked: &[String]) -> Vec<(String, Vec<String>)> {
    let items = match items {
        Value::Array(a) => a.clone(),
        Value::Object(o) => o.get("pages").and_then(|v| v.as_array()).cloned().unwrap_or_default(),
        _ => Vec::new(),
    };
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for item in items {
        let page = item.get("page").and_then(Value::as_str).unwrap_or("").trim().replace('\\', "/");
        if !asked.contains(&page) || out.iter().any(|(p, _)| *p == page) {
            continue;
        }
        out.push((page, topic_list(item.get("topics"))));
    }
    out
}

/// A list of topics as stored: each named once, at most [`MAX_TOPICS`].
pub fn topic_list(v: Option<&Value>) -> Vec<String> {
    let mut topics: Vec<String> = Vec::new();
    for t in v.and_then(Value::as_array).into_iter().flatten() {
        let t = topic_name(t.as_str().unwrap_or(""));
        if !t.is_empty() && !topics.contains(&t) && topics.len() < MAX_TOPICS {
            topics.push(t);
        }
    }
    topics
}

/// A page's topic tags from the write that made it: replaced, unless the
/// page is not one worth tagging or no topics came back (it keeps the ones
/// it had).
pub fn tag_written(db: &Db, page: &str, topics: &[String]) -> Result<bool> {
    if !may_be_tagged(page) || topics.is_empty() {
        return Ok(false);
    }
    set_topics(db, page, topics)?;
    Ok(true)
}

/// A page's topic tags, replacing the ones it had.
pub fn set_topics(db: &Db, page: &str, topics: &[String]) -> Result<()> {
    let from = Node::of_path(page);
    let edges: Vec<Edge> = topics
        .iter()
        .map(|t| topic_name(t))
        .filter(|t| !t.is_empty())
        .take(MAX_TOPICS)
        .map(|t| Edge::new(&from, &Node::topic(&t), "topic", ""))
        .collect();
    db.replace_graph_edges_from(ORIGIN_TOPICS, &from.key(), &edges)
}

/// A page's topic tags, by name.
pub fn topics_of(db: &Db, page: &str) -> Result<Vec<String>> {
    let key = Node::of_path(page).key();
    let mut out: Vec<String> = db
        .graph_edges_touching(&key)?
        .into_iter()
        .filter(|e| e.kind == "topic" && e.src == key)
        .filter_map(|e| Node::parse(&e.dst).map(|n| n.id))
        .collect();
    out.sort();
    Ok(out)
}

// ---------------------------------------------------------------------
// Related
// ---------------------------------------------------------------------

/// One neighbour [`related`] found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Neighbour {
    pub node: Node,
    /// The edge's kind, one of [`EDGE_ORDER`].
    pub edge: String,
    /// The edge goes from the node asked about (or the one it came through)
    /// to this one.
    pub outgoing: bool,
    pub detail: String,
    /// 1 for a direct neighbour, 2 for one reached through `via`.
    pub hops: u8,
    pub via: Option<Node>,
}

fn edge_rank(kind: &str) -> usize {
    EDGE_ORDER.iter().position(|k| *k == kind).unwrap_or(EDGE_ORDER.len())
}

/// The neighbour an edge gives `from`, when `from` is one of its ends.
fn across(e: &Edge, from: &str, hops: u8, via: Option<&Node>) -> Option<Neighbour> {
    let (other, outgoing) = if e.src == from {
        (&e.dst, true)
    } else if e.dst == from {
        (&e.src, false)
    } else {
        return None;
    };
    Some(Neighbour { node: Node::parse(other)?, edge: e.kind.clone(), outgoing, detail: e.detail.clone(), hops, via: via.cloned() })
}

/// What the code index says of a code file or symbol, live: the files it
/// imports and that import it, or the file a symbol is in and where it is
/// used.
fn code_neighbours(code: &[CodeSource], node: &Node) -> Result<Vec<Neighbour>> {
    let Some((repo, path, name)) = node.code_place() else { return Ok(Vec::new()) };
    let Some(source) = code.iter().find(|c| c.name == repo) else { return Ok(Vec::new()) };
    let n = |to: Node, edge: &str, outgoing: bool, detail: String| Neighbour {
        node: to,
        edge: edge.into(),
        outgoing,
        detail,
        hops: 1,
        via: None,
    };
    let mut out = Vec::new();
    match name {
        None => {
            let r = crate::codeindex::related(&source.db, path)?;
            out.extend(r.imports.into_iter().map(|f| n(Node::code(repo, &f), "imports", true, String::new())));
            out.extend(r.imported_by.into_iter().map(|f| n(Node::code(repo, &f), "imports", false, String::new())));
        }
        Some(name) => {
            out.push(n(Node::code(repo, path), "in", true, String::new()));
            for u in crate::codeindex::usages(&source.db, name, 60)? {
                let within = u.within.map(|w| format!(" in {w}")).unwrap_or_default();
                out.push(n(Node::code(repo, &u.path), "uses", false, format!("line {}{within}", u.line)));
            }
        }
    }
    Ok(out)
}

/// A node's neighbours (decision 21), nearest first, at most `limit`: its
/// own edges in the knowledge base's graph `kb` and, for a code file or
/// symbol, the code index of its repo among `code`; then the neighbours of
/// those through the stored edges (two pages sharing a topic, the commits
/// on a ruling a page cites). At one distance they go by [`EDGE_ORDER`],
/// then by node. A node is listed once, at its nearest.
pub fn related(kb: Option<&Db>, code: &[CodeSource], node: &Node, limit: usize) -> Result<Vec<Neighbour>> {
    let key = node.key();
    let mut first: Vec<Neighbour> = Vec::new();
    if let Some(db) = kb {
        first.extend(db.graph_edges_touching(&key)?.iter().filter_map(|e| across(e, &key, 1, None)));
    }
    first.extend(code_neighbours(code, node)?);
    sort(&mut first);
    let mut seen: HashSet<String> = HashSet::from([key]);
    let mut out: Vec<Neighbour> = Vec::new();
    for n in first {
        if seen.insert(n.node.key()) {
            out.push(n);
        }
    }
    if let Some(db) = kb {
        let mut second: Vec<Neighbour> = Vec::new();
        for n in &out {
            let k = n.node.key();
            second.extend(db.graph_edges_touching(&k)?.iter().filter_map(|e| across(e, &k, 2, Some(&n.node))));
        }
        sort(&mut second);
        for n in second {
            if seen.insert(n.node.key()) {
                out.push(n);
            }
        }
    }
    out.truncate(limit);
    Ok(out)
}

fn sort(list: &mut [Neighbour]) {
    list.sort_by(|a, b| {
        a.hops
            .cmp(&b.hops)
            .then_with(|| edge_rank(&a.edge).cmp(&edge_rank(&b.edge)))
            .then_with(|| a.node.cmp(&b.node))
    });
}

/// A neighbour in a line: what joins them, the node, and the detail, as the
/// MCP `related` tool and the app say it (`cites code:game:src/save.rs`,
/// `← changed note:…/2026-10-01-standup.md (the save rules)`).
pub fn line_of(n: &Neighbour) -> String {
    let arrow = if n.outgoing { "" } else { "← " };
    let detail = if n.detail.is_empty() { String::new() } else { format!(" ({})", n.detail) };
    let via = n.via.as_ref().map(|v| format!(", through {}", v.key())).unwrap_or_default();
    format!("{arrow}{} {}{detail}{via}", n.edge, n.node.key())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Project;
    use std::fs;

    fn library(files: &[(&str, &str)]) -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        for (path, text) in files {
            let p = dir.path().join(path);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, text).unwrap();
        }
        let project = Project::create(dir.path(), "Lib").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        crate::scan::scan(&project, &mut db).unwrap();
        (dir, db)
    }

    const NOTE: &str = "Research/Ingestion/Ingested/Meetings/2026-10/2026-10-01-standup.md";

    fn saves_library() -> (tempfile::TempDir, Db) {
        let (dir, mut db) = library(&[
            ("START-HERE.md", "# Start\nSee [[Saves]].\n"),
            (
                "Design/Saves.md",
                "---\ntitle: Saves\nsources:\n  - game@abc1234:src/save.rs\n  - \"[[2026-10-01-standup]]\"\n  - D-012\n---\n# Saves\nRegions save apart.\n",
            ),
            (
                "Decisions/DECISIONS.md",
                "# Decisions\n\n## D-012 · 2026-10-01 · saves\n\n**Regions save apart.** [SUPERSEDED BY D-013]\n\nsources: game:src/save.rs\n\n\
                 ## D-013 · 2026-10-05 · saves\n\n**Regions save together.**\n\nsupersedes: D-012\n",
            ),
            (NOTE, "# Standup\nWe talked about saves.\n"),
            ("tickets/SR-101.md", "---\nid: SR-101\nstatus: open\nupdated: 2026-10-06\n---\n# Fix saves\n"),
        ]);
        let card = crate::ingest::Card {
            placement: crate::ingest::Placement {
                raw: "Research/Ingestion/Raw/standup.txt".into(),
                note: NOTE.into(),
                source: "Research/Ingestion/Ingested/Meetings/2026-10/2026-10-01-standup/standup.txt".into(),
            },
            pages: vec![crate::ingest::PageWritten { path: "Design/Saves.md".into(), label: "the save rule".into(), new: false }],
            commit: None,
            commit_error: None,
        };
        db.insert_review_item(crate::ingest::REVIEW_KIND, "Ingested", "", NOTE, Some(&serde_json::to_string(&card).unwrap()), 1)
            .unwrap();
        on_scan(&mut db, dir.path()).unwrap();
        (dir, db)
    }

    fn has(edges: &[Edge], src: &str, dst: &str, kind: &str) -> bool {
        edges.iter().any(|e| e.src == src && e.dst == dst && e.kind == kind)
    }

    #[test]
    fn nodes_read_back_and_know_their_kind() {
        assert_eq!(Node::of_path("tickets/SR-101.md").kind, "ticket");
        assert_eq!(Node::of_path(NOTE).kind, "note");
        assert_eq!(Node::of_path("Design\\Saves.md").key(), "page:Design/Saves.md");
        let s = Node::symbol("game", "src/save.rs", "save");
        assert_eq!(Node::parse(&s.key()), Some(s.clone()));
        assert_eq!(s.code_place(), Some(("game", "src/save.rs", Some("save"))));
        assert_eq!(Node::code("game", "src/a.rs").code_place(), Some(("game", "src/a.rs", None)));
        assert_eq!(Node::parse("nonsense:x"), None);
        assert_eq!(Node::topic("  Save   Games! ").id, "save games");
    }

    /// Every edge kind the knowledge base's own index holds, from exact
    /// sources: links, citations (code, a note, a ruling), a ruling's entry
    /// and its sources, the supersession the log marks, and the note that
    /// changed a page.
    #[test]
    fn a_scan_makes_each_edge_from_exact_sources() {
        let (_dir, db) = saves_library();
        let edges = db.graph_edges_of(ORIGIN_SCAN).unwrap();
        let note = format!("note:{NOTE}");
        assert!(has(&edges, "page:START-HERE.md", "page:Design/Saves.md", "link"), "{edges:#?}");
        assert!(has(&edges, "page:Design/Saves.md", "code:game:src/save.rs", "cites"), "{edges:#?}");
        assert!(has(&edges, "page:Design/Saves.md", &note, "cites"), "{edges:#?}");
        assert!(has(&edges, "page:Design/Saves.md", "decision:D-012", "cites"), "{edges:#?}");
        assert!(has(&edges, "decision:D-012", "page:Decisions/DECISIONS.md", "in"), "{edges:#?}");
        assert!(has(&edges, "decision:D-012", "code:game:src/save.rs", "cites"), "{edges:#?}");
        assert!(has(&edges, "decision:D-013", "decision:D-012", "supersedes"), "{edges:#?}");
        let changed = edges.iter().find(|e| e.src == note && e.kind == "changed").expect("note → page");
        assert_eq!((changed.dst.as_str(), changed.detail.as_str()), ("page:Design/Saves.md", "the save rule"));
        // Nothing else makes a link: a name no page answers is no edge.
        assert!(edges.iter().all(|e| e.src != e.dst));
    }

    /// The scan's edges are made again each time: a link taken out leaves.
    #[test]
    fn a_scan_makes_its_edges_again() {
        let (dir, mut db) = saves_library();
        fs::write(dir.path().join("START-HERE.md"), "# Start\nNothing here.\n").unwrap();
        let project = Project::open(dir.path()).unwrap();
        crate::scan::scan(&project, &mut db).unwrap();
        on_scan(&mut db, dir.path()).unwrap();
        let edges = db.graph_edges_of(ORIGIN_SCAN).unwrap();
        assert!(!has(&edges, "page:START-HERE.md", "page:Design/Saves.md", "link"));
        assert!(has(&edges, "page:Design/Saves.md", "decision:D-012", "cites"));
    }

    /// Code comments citing a ruling the log has are edges; one it lacks is
    /// a finding, not an edge.
    #[test]
    fn code_citations_of_known_rulings_are_edges() {
        let (dir, mut db) = saves_library();
        let mut code = Db::open_in_memory().unwrap();
        code.upsert_file("src/save.rs", "code", 1, 1, "indexed", None, "// D-012 and D-404\nfn save() {}\n").unwrap();
        let sources = [CodeSource { name: "game".into(), db: code }];
        let found = crate::lint::rulings_in_code(&db, dir.path(), &sources).unwrap();
        assert_eq!(store_code_citations(&mut db, &found).unwrap(), 1);
        let edges = db.graph_edges_of(ORIGIN_CODE).unwrap();
        assert_eq!(edges, vec![Edge {
            src: "code:game:src/save.rs".into(),
            dst: "decision:D-012".into(),
            kind: "code-cites".into(),
            detail: "line 1".into(),
        }]);
        assert_eq!(crate::lint::unknown_of(&found).len(), 1, "D-404 is a finding");
    }

    /// A commit's edges go only to ids the knowledge base has: a ticket by
    /// its id, a ruling by its entry. `UTF-8` and an unknown ticket drop out.
    #[test]
    fn commit_edges_keep_to_known_ids() {
        let (dir, db) = saves_library();
        let known = known_ids(&db, dir.path()).unwrap();
        assert_eq!(known.get("SR-101"), Some(&Node::of_path("tickets/SR-101.md")));
        assert_eq!(known.get("D-013"), Some(&Node::decision("D-013")));
        let commits = vec![crate::git::Citing {
            sha: "1a2b3c4".into(),
            date: "2026-10-07".into(),
            subject: "Fix SR-101 per D-013, UTF-8 safe, see SR-999".into(),
            ids: vec!["SR-101".into(), "D-013".into(), "UTF-8".into(), "SR-999".into()],
        }];
        let edges = commit_edges("game", &commits, &known);
        let dsts: Vec<&str> = edges.iter().map(|e| e.dst.as_str()).collect();
        assert_eq!(dsts, vec!["ticket:tickets/SR-101.md", "decision:D-013"]);
        assert!(edges.iter().all(|e| e.src == "commit:game@1a2b3c4" && e.kind == "commit"));
        assert_eq!(edges[0].detail, "2026-10-07 Fix SR-101 per D-013, UTF-8 safe, see SR-999");
    }

    /// Topic tags come from the write that made a page: the reply's list is
    /// stored beside the graph (never in frontmatter). Pages the write did
    /// not make, and pages not worth tagging, are ignored.
    #[test]
    fn topic_tags_are_stored_from_a_write() {
        let (dir, mut db) = saves_library();
        assert!(topics_ask(&[]).contains("(none yet)"));
        let reply: Value = serde_json::from_str(
            r#"[{"page": "Design/Saves.md", "topics": ["Saves", "regions", "saves", "a", "b", "c", "d"]},
                {"page": "Other.md", "topics": ["nope"]}, {"page": "log.md", "topics": ["log"]}]"#,
        )
        .unwrap();
        let asked = vec!["Design/Saves.md".to_string(), "log.md".to_string()];
        let tagged = topics_from(&reply, &asked);
        assert_eq!(tagged.len(), 2, "only the pages asked about");
        for (page, topics) in &tagged {
            tag_written(&db, page, topics).unwrap();
        }
        assert_eq!(topics_of(&db, "Design/Saves.md").unwrap(), vec!["a", "b", "c", "regions", "saves"]);
        assert!(topics_of(&db, "log.md").unwrap().is_empty(), "the log is not tagged");
        assert!(!tag_written(&db, "Design/Saves.md", &[]).unwrap(), "no topics back: the page keeps its tags");
        assert_eq!(topics_of(&db, "Design/Saves.md").unwrap().len(), 5);
        assert_eq!(topics_in_use(&db, 10).unwrap().len(), 5);
        assert!(topics_ask(&topics_in_use(&db, 10).unwrap()).contains("a, b, c, regions, saves"));
        // The file itself is untouched.
        assert!(!fs::read_to_string(dir.path().join("Design/Saves.md")).unwrap().contains("regions"));
        // A scan keeps them; a page that is gone loses them.
        on_scan(&mut db, dir.path()).unwrap();
        assert_eq!(topics_of(&db, "Design/Saves.md").unwrap().len(), 5);
        fs::remove_file(dir.path().join("Design/Saves.md")).unwrap();
        on_scan(&mut db, dir.path()).unwrap();
        assert!(topics_of(&db, "Design/Saves.md").unwrap().is_empty());
    }

    /// `related` lists direct neighbours first, by edge kind, then the ones
    /// reached through them, each node once at its nearest.
    #[test]
    fn related_is_nearest_first_by_edge_kind() {
        let (_dir, db) = saves_library();
        set_topics(&db, "Design/Saves.md", &["saves".into()]).unwrap();
        set_topics(&db, "tickets/SR-101.md", &["saves".into()]).unwrap();
        let got = related(Some(&db), &[], &Node::of_path("Design/Saves.md"), 50).unwrap();
        let lines: Vec<String> = got.iter().map(line_of).collect();
        let pos = |key: &str| got.iter().position(|n| n.node.key() == key).unwrap_or_else(|| panic!("{key} in {lines:#?}"));
        // One hop, by kind: citations, then the link, then the topic.
        assert!(pos("decision:D-012") < pos("page:START-HERE.md"), "{lines:#?}");
        assert!(pos("page:START-HERE.md") < pos("topic:saves"), "{lines:#?}");
        let note = got.iter().find(|n| n.node.kind == "note").unwrap();
        assert_eq!((note.edge.as_str(), note.hops), ("cites", 1), "listed once, by its first kind");
        // Two hops: the ruling that supersedes the one cited, and the ticket
        // that shares a topic.
        let later = &got[pos("decision:D-013")];
        assert_eq!((later.hops, later.edge.as_str(), later.outgoing), (2, "supersedes", false));
        assert_eq!(later.via, Some(Node::decision("D-012")));
        let ticket = &got[pos("ticket:tickets/SR-101.md")];
        assert_eq!((ticket.hops, ticket.via.clone()), (2, Some(Node::topic("saves"))));
        assert!(got.windows(2).all(|w| w[0].hops <= w[1].hops), "{lines:#?}");
        assert!(got.iter().all(|n| n.node.key() != "page:Design/Saves.md"), "never itself");
        assert_eq!(related(Some(&db), &[], &Node::of_path("Design/Saves.md"), 2).unwrap().len(), 2);
    }

    /// A code file's imports and a symbol's uses come live from its repo's
    /// code index.
    #[test]
    fn related_reads_imports_and_uses_from_the_code_index() {
        let mut code = Db::open_in_memory().unwrap();
        for (path, text) in [
            ("app/game.py", "from app.store import save_world\n\ndef play():\n    save_world()\n"),
            ("app/store.py", "def save_world():\n    pass\n"),
        ] {
            code.upsert_file(path, "code", 1, 1, "indexed", None, text).unwrap();
            code.set_code_index(path, &crate::codeindex::map_file(path, text).unwrap()).unwrap();
        }
        let sources = [CodeSource { name: "game".into(), db: code }];
        let got = related(None, &sources, &Node::code("game", "app/store.py"), 10).unwrap();
        assert!(got.iter().any(|n| n.node == Node::code("game", "app/game.py") && n.edge == "imports" && !n.outgoing), "{got:#?}");
        let got = related(None, &sources, &Node::symbol("game", "app/store.py", "save_world"), 10).unwrap();
        assert!(got.iter().any(|n| n.node == Node::code("game", "app/store.py") && n.edge == "in"), "{got:#?}");
        assert!(got.iter().any(|n| n.node == Node::code("game", "app/game.py") && n.edge == "uses"), "{got:#?}");
    }
}

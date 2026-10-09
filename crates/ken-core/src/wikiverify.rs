//! The check a drafted wiki page passes before it is published: every
//! statement it makes (a table row, a list item, a paragraph) is read
//! against the source it cites, as the checkout has it now, by a call that
//! did not draft it. The bar is faithfulness to the team's own sources: a
//! page says what its sources say and uses what they hold. Round three of
//! the Shattered Realms draft (2026-10-07) wrote "all of them in Tools"
//! where a source named one more, gave a reason no source gave, cut a value
//! set short, and asked a person for facts its sources held.
//!
//! Four steps, the last three deterministic around one model call:
//! - A statement with no citation is removed: a `repo:path:line`, a source
//!   label, a `[[page]]` link or a ruling (`D-nnn`). The lede, navigation
//!   lines and the template's own lines are not statements.
//! - Ken gathers each statement's evidence: the lines it cites with a few
//!   around them (a cited heading, its section), the source it names, the
//!   page it links. A statement that something is gone gets Ken's search of
//!   the checkout for it. A `To fill:` item gets the page's front matter,
//!   the team manifest, the pages drafted so far and the checkout.
//! - One call per page judges each statement against its evidence:
//!   supported, contradicted (with what the source says) or unsupported,
//!   and answers a `To fill:` item when the evidence does.
//! - Ken applies the verdicts: a contradicted statement becomes what its
//!   source says, with its citation; an unsupported one and one with no
//!   verdict are removed; an answered item leaves the `To fill:` line. Every
//!   change is logged with its before, after and evidence.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::wikidraft::{PathCheck, Source};
use crate::{Error, Result};

/// One change the check made to a page, for the run's report.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    pub page: String,
    /// `uncited` · `replaced` · `removed` · `answered` · `filled` · `kept-open`
    pub kind: String,
    pub before: String,
    pub after: String,
    pub evidence: String,
}

/// What kind of statement a span of a page is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Row,
    Item,
    Para,
}

/// One statement of a page: the lines it spans (`start..end`, 0-based), its
/// kind and its text, with the heading and table header it sits under.
#[derive(Debug, Clone, PartialEq)]
pub struct Statement {
    pub start: usize,
    pub end: usize,
    pub kind: Kind,
    pub text: String,
    pub heading: String,
    pub header: String,
}

/// What a statement cites.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Cite {
    /// `repo:path`, `repo:path:12`, `repo@sha:path:12-30`.
    File { repo: String, path: String, lines: Option<(usize, usize)> },
    /// A source Ken derived, `repo:(what the code registers…)`.
    Derived { label: String },
    /// `[[Page]]`, `[[Folder/Page|shown]]`, `[[Page#part]]`.
    Page(String),
    /// A ruling of the decisions log, `D-279`.
    Ruling(String),
}

/// A page split into its statements, and where its `To fill:` line is.
#[derive(Debug, Default)]
pub struct Parsed {
    pub statements: Vec<Statement>,
    /// Statements that are not claims: the lede, navigation, the template's
    /// own lines, the byline.
    pub exempt: Vec<Statement>,
    pub fill: Option<usize>,
}

fn is_fence(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("```") || t.starts_with("~~~")
}

fn is_list_item(line: &str) -> bool {
    let t = line.trim_start();
    if t.starts_with("- ") || t.starts_with("* ") || t.starts_with("+ ") {
        return true;
    }
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && t[digits..].starts_with(". ")
}

fn is_table_separator(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('|') && t.contains('-') && t.chars().all(|c| "|-: ".contains(c))
}

/// The `To fill:` line's text after its marker, bold or not.
pub fn fill_text(line: &str) -> Option<&str> {
    let t = line.trim().trim_start_matches(['*', '_']);
    let rest = t.strip_prefix("To fill:")?;
    Some(rest.trim_start_matches(['*', '_']).trim())
}

/// The line after the frontmatter, 0 when there is none.
fn body_start(lines: &[&str]) -> usize {
    if lines.first().map(|l| l.trim_end()) != Some("---") {
        return 0;
    }
    lines.iter().skip(1).position(|l| l.trim_end() == "---").map_or(0, |i| i + 2)
}

/// Whether a line, links taken out, has nothing left to say: `[[#A]] · [[#B]]`.
fn is_navigation(text: &str) -> bool {
    let mut rest = String::new();
    let mut s = text;
    while let Some(i) = s.find("[[") {
        rest.push_str(&s[..i]);
        match s[i..].find("]]") {
            Some(j) => s = &s[i + j + 2..],
            None => {
                s = "";
                break;
            }
        }
    }
    rest.push_str(s);
    text.contains("[[") && !rest.chars().any(char::is_alphanumeric)
}

/// Split a page's body into statements. `template` holds the lines of the
/// template it was drafted from: a line there is the template's, not a claim.
pub fn parse(page: &str, template: &HashSet<String>) -> Parsed {
    let lines: Vec<&str> = page.lines().collect();
    let mut out = Parsed::default();
    let mut heading = String::new();
    let mut header = String::new();
    let mut seen_h2 = false;
    let mut lede_done = false;
    let mut i = body_start(&lines);
    let push = |out: &mut Parsed, s: Statement, lede: bool| {
        let from_template = s.text.lines().all(|l| l.trim().is_empty() || template.contains(l.trim()));
        let byline = s.text.trim_start().starts_with("Drafted by Ken");
        if lede || from_template || byline || is_navigation(&s.text) {
            out.exempt.push(s);
        } else {
            out.statements.push(s);
        }
    };
    while i < lines.len() {
        let line = lines[i];
        let t = line.trim();
        if t.is_empty() {
            i += 1;
            continue;
        }
        if is_fence(line) {
            // A fenced block belongs to the statement just before it.
            let close = (i + 1..lines.len()).find(|j| is_fence(lines[*j])).unwrap_or(lines.len() - 1);
            let joined = out.statements.last_mut().filter(|s| (s.end..i).all(|j| lines[j].trim().is_empty()));
            if let Some(s) = joined {
                s.end = close + 1;
                s.text = lines[s.start..s.end].join("\n");
            }
            i = close + 1;
            continue;
        }
        if t.starts_with('#') {
            let level = t.chars().take_while(|c| *c == '#').count();
            if level >= 2 {
                seen_h2 = true;
            }
            heading = t.to_string();
            header.clear();
            i += 1;
            continue;
        }
        if fill_text(line).is_some() {
            out.fill = Some(i);
            i += 1;
            continue;
        }
        if t.starts_with('|') {
            if lines.get(i + 1).is_some_and(|n| is_table_separator(n)) {
                header = t.to_string();
                i += 2;
                continue;
            }
            if is_table_separator(line) {
                i += 1;
                continue;
            }
            let s = Statement { start: i, end: i + 1, kind: Kind::Row, text: line.to_string(), heading: heading.clone(), header: header.clone() };
            push(&mut out, s, false);
            i += 1;
            continue;
        }
        let kind = if is_list_item(line) { Kind::Item } else { Kind::Para };
        let mut end = i + 1;
        while end < lines.len() {
            let n = lines[end];
            let nt = n.trim();
            let stops = nt.is_empty() || nt.starts_with('#') || nt.starts_with('|') || is_fence(n) || is_list_item(n) || fill_text(n).is_some();
            let continues = kind == Kind::Para || n.starts_with([' ', '\t']);
            if stops || !continues {
                break;
            }
            end += 1;
        }
        let lede = kind == Kind::Para && !seen_h2 && !lede_done;
        if kind == Kind::Para && !seen_h2 {
            lede_done = true;
        }
        let s = Statement { start: i, end, kind, text: lines[i..end].join("\n"), heading: heading.clone(), header: String::new() };
        push(&mut out, s, lede);
        i = end;
    }
    out
}

/// What `text` cites: its backticked `repo:…` tokens for the repos named in
/// `repos`, its `[[links]]` to other pages and its `D-nnn` rulings.
pub fn cites(text: &str, repos: &[&str]) -> Vec<Cite> {
    let mut names: Vec<&str> = repos.to_vec();
    names.sort_by_key(|n| std::cmp::Reverse(n.len()));
    let mut out: Vec<Cite> = Vec::new();
    for token in text.split('`').skip(1).step_by(2) {
        let t = token.trim();
        for r in &names {
            let Some(rest) = t.strip_prefix(r) else { continue };
            let rest = match rest.strip_prefix('@') {
                Some(pinned) => pinned.split_once(':').map(|(_, p)| p),
                None => rest.strip_prefix(':'),
            };
            let Some(rest) = rest else { continue };
            if rest.starts_with('(') {
                out.push(Cite::Derived { label: format!("{r}:{rest}") });
            } else {
                let (path, lines) = match rest.rsplit_once(':') {
                    Some((p, l)) if !l.is_empty() && l.chars().all(|c| c.is_ascii_digit() || c == '-') => {
                        let (a, b) = l.split_once('-').unwrap_or((l, l));
                        let a: usize = a.parse().unwrap_or(0);
                        (p, (a > 0).then(|| (a, b.parse().unwrap_or(a).max(a))))
                    }
                    _ => (rest, None),
                };
                out.push(Cite::File { repo: r.to_string(), path: path.to_string(), lines });
            }
            break;
        }
    }
    let mut s = text;
    while let Some(i) = s.find("[[") {
        let Some(j) = s[i..].find("]]") else { break };
        let inner = &s[i + 2..i + j];
        let target = inner.split(['|', '#']).next().unwrap_or("").trim();
        if !target.is_empty() {
            out.push(Cite::Page(target.to_string()));
        }
        s = &s[i + j + 2..];
    }
    let ruling = regex::Regex::new(r"\bD-\d{1,5}\b").expect("a valid pattern");
    out.extend(ruling.find_iter(text).map(|m| Cite::Ruling(m.as_str().to_string())));
    let mut seen = HashSet::new();
    out.retain(|c| seen.insert(c.clone()));
    out
}

// ------------------------------------------------------------------ evidence

/// One piece of evidence, shown once however many statements use it.
#[derive(Debug, Clone)]
struct Block {
    key: String,
    head: String,
    body: String,
}

/// Characters of one piece of evidence, and of what one statement is shown
/// of a source Ken derived (a long list of registrations or values).
const BLOCK_MAX: usize = 3_000;
const DERIVED_MAX: usize = 1_800;
/// Characters shown of a part of a brief a statement cites, picked by its terms.
const PART_MAX: usize = 4_000;
/// Characters of evidence and statements one call reads; a page with more
/// is checked in as many calls as it needs.
const CALL_MAX: usize = 110_000;

/// The words of a statement worth searching the code for: four letters or
/// more, not a common word, citations and links taken out.
fn terms(text: &str) -> Vec<String> {
    const COMMON: &[&str] = &[
        "about", "above", "after", "again", "also", "always", "among", "another", "around", "because", "been", "before", "being",
        "below", "between", "both", "built", "cannot", "could", "does", "done", "down", "during", "each", "either", "else", "every",
        "first", "from", "given", "gives", "have", "here", "however", "into", "itself", "just", "keeps", "last", "later", "least",
        "less", "like", "list", "listed", "lists", "made", "make", "makes", "many", "more", "most", "much", "must", "need", "needs",
        "never", "next", "none", "only", "other", "others", "over", "page", "part", "rather", "same", "says", "should", "shown",
        "since", "some", "still", "such", "than", "that", "their", "them", "then", "there", "these", "they", "this", "those",
        "though", "through", "under", "until", "upon", "used", "uses", "using", "very", "were", "what", "when", "where", "whether",
        "which", "while", "whole", "with", "within", "without", "would", "your", "repo", "repos", "file", "files", "code", "line",
        "lines", "source", "sources", "currently", "remaining", "work", "todo", "todos", "feature", "features", "will", "yes",
        "true", "false", "null", "each", "one", "person", "people", "team", "fill", "known", "says", "said", "name", "named",
        "names", "called", "calls", "read", "reads", "takes", "holds", "lives", "kept", "comes", "goes", "gets", "sets", "runs",
        "with", "their", "thing", "things", "time", "times", "today", "yet", "isn't", "doesn't", "can't", "it's", "the",
    ];
    let mut plain = String::new();
    for (k, part) in text.split('`').enumerate() {
        if k % 2 == 0 {
            plain.push_str(part);
            plain.push(' ');
        }
    }
    let mut out: Vec<String> = Vec::new();
    for w in plain.split(|c: char| !c.is_alphanumeric()) {
        let l = w.to_lowercase();
        if l.chars().count() >= 4 && !l.chars().all(|c| c.is_ascii_digit()) && !COMMON.contains(&l.as_str()) && !out.contains(&l) {
            out.push(l);
        }
    }
    out.truncate(10);
    out
}

/// The code names a statement uses: each backticked token that reads as a
/// name ([`crate::checkout::as_symbol`]), and each CamelCase word in its prose.
fn code_names(text: &str, repos: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (k, part) in text.split('`').enumerate() {
        let tokens: Vec<&str> = if k % 2 == 1 { vec![part] } else { part.split(|c: char| !(c.is_alphanumeric() || c == '_')).collect() };
        for token in tokens {
            if k % 2 == 1 && repos.iter().any(|r| token.starts_with(&format!("{r}:")) || token.starts_with(&format!("{r}@"))) {
                continue;
            }
            if let Some((name, is_class)) = crate::checkout::as_symbol(token) {
                if (k % 2 == 1 || is_class) && !out.contains(&name) {
                    out.push(name);
                }
            }
        }
    }
    out
}

/// What a statement is about, to pick the lines of a long source that
/// bear on it: `primary`, the words that name its subject (a row's first
/// cell, the names it writes in backticks), and `words`, the rest.
#[derive(Debug, Default)]
struct Terms {
    primary: Vec<String>,
    words: Vec<String>,
}

impl Terms {
    fn of(s: &Statement, repos: &[&str]) -> Terms {
        let mut primary: Vec<String> = Vec::new();
        let add = |w: &str, out: &mut Vec<String>| {
            let w = w.trim().to_lowercase();
            if w.len() >= 3 && !out.contains(&w) {
                out.push(w);
            }
        };
        if s.kind == Kind::Row {
            let first = s.text.trim().trim_start_matches('|').split('|').next().unwrap_or_default();
            for w in first.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
                add(w, &mut primary);
            }
        }
        for token in s.text.split('`').skip(1).step_by(2) {
            if repos.iter().any(|r| token.starts_with(&format!("{r}:")) || token.starts_with(&format!("{r}@"))) {
                continue;
            }
            for w in token.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.' || c == '/')) {
                add(w.trim_matches(['.', '/']), &mut primary);
            }
        }
        Terms { primary, words: terms(&s.text) }
    }
}

/// `lines` (each with its 1-based number) whole when they fit `max`, else
/// the lines that bear most on `terms` (a primary term counts three times a
/// word), each with the heading of the part it is in when `headers` (a
/// `path:` line of a source Ken derived), shown in order while `max` lasts;
/// the first lines when none bears on them. A line longer than half of
/// `max` is cut, marked so.
fn pick_lines(lines: &[(usize, &str)], terms: &Terms, max: usize, headers: bool) -> String {
    let clip = |l: &str| crate::wikidraft::clip_line(l, max / 2);
    let fmt = |v: &[(usize, &str)]| v.iter().map(|(n, l)| format!("{n}: {}", clip(l))).collect::<Vec<_>>().join("\n");
    let all = fmt(lines);
    if all.len() <= max {
        return all;
    }
    let is_header = |l: &str| {
        let t = l.trim_end();
        (t.ends_with(':') && !t.contains(" = ")) || t.contains("statements, in order")
    };
    let lower: Vec<String> = lines.iter().map(|(_, l)| l.to_lowercase()).collect();
    // A term on a fifth of the lines or more picks nothing out.
    let telling = |t: &&String| lower.iter().filter(|l| l.contains(t.as_str())).count() * 5 < lines.len().max(10);
    let primary: Vec<&String> = terms.primary.iter().filter(telling).collect();
    let words: Vec<&String> = terms.words.iter().filter(telling).collect();
    let mut scored: Vec<(usize, usize)> = (0..lines.len())
        .filter_map(|i| {
            let l = &lower[i];
            let n = 3 * primary.iter().filter(|t| l.contains(t.as_str())).count() + words.iter().filter(|t| l.contains(t.as_str())).count();
            (n > 0).then_some((n, i))
        })
        .collect();
    scored.sort_by(|x, y| y.0.cmp(&x.0).then(x.1.cmp(&y.1)));
    let mut keep: BTreeSet<usize> = BTreeSet::new();
    let mut used = 0;
    let mut more = 0;
    let order: Vec<usize> = if scored.is_empty() { (0..lines.len()).collect() } else { scored.iter().map(|(_, i)| *i).collect() };
    for i in order {
        let mut want = vec![i];
        if headers {
            if let Some(h) = (0..i).rev().find(|j| is_header(lines[*j].1)) {
                want.push(h);
            }
        }
        let cost: usize = want.iter().filter(|j| !keep.contains(j)).map(|j| clip(lines[*j].1).len() + 8).sum();
        if used + cost > max {
            more += 1;
            continue;
        }
        used += cost;
        keep.extend(want);
    }
    let mut out = String::new();
    let mut last: Option<usize> = None;
    for i in keep {
        let (n, l) = lines[i];
        if last.is_some_and(|p| p + 1 != i) {
            out.push_str("…\n");
        }
        out.push_str(&format!("{n}: {}\n", clip(l)));
        last = Some(i);
    }
    if more > 0 {
        out.push_str(&format!("(… {more} more lines that bear on it)\n"));
    }
    out.trim_end().to_string()
}

fn hash(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// Whether a path is a doc, read by heading rather than by line.
fn is_doc(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    [".md", ".mdx", ".txt", ".rst", ".adoc"].iter().any(|e| name.ends_with(e)) || ["readme", "changelog", "contributing", "install"].contains(&name.as_str())
}

/// The heading level of a Markdown line, 0 for none.
fn heading_level(line: &str) -> usize {
    let n = line.chars().take_while(|c| *c == '#').count();
    if n > 0 && line[n..].starts_with(' ') {
        n
    } else {
        0
    }
}

/// What one check of a page needs: the checkout, the sources the page was
/// drafted from, the wiki and the pages drafted before it.
pub struct Ctx<'a> {
    pub check: &'a PathCheck,
    pub sources: &'a [Source],
    pub wiki: &'a Path,
    pub drafted: &'a [String],
}

impl Ctx<'_> {
    fn repo_names(&self) -> Vec<&str> {
        self.check.repos.iter().map(|t| t.name.as_str()).collect()
    }

    fn root_of(&self, repo: &str) -> Option<&Path> {
        self.check.repos.iter().find(|t| t.name == repo).map(|t| t.root.as_path())
    }

    /// The code repos whose code is read: not the wiki, not a reference
    /// repo the team only reads.
    fn code_repos(&self) -> Vec<(&str, &Path)> {
        self.check.repos.iter().filter(|t| t.deep && !t.library).map(|t| (t.name.as_str(), t.root.as_path())).collect()
    }

    /// What `git grep -F` finds for `needle` in `repo` ([`crate::wikidraft::grep_repo`]),
    /// remembered for the run.
    fn grep(&self, repo: &str, root: &Path, needle: &str, ignore_case: bool) -> (usize, Vec<String>) {
        let key = format!("{repo}\0{ignore_case}\0{needle}");
        if let Some(hit) = self.check.greps.borrow().get(&key) {
            return hit.clone();
        }
        let hit = crate::wikidraft::grep_repo(repo, root, needle, ignore_case);
        self.check.greps.borrow_mut().insert(key, hit.clone());
        hit
    }

    /// The evidence for one citation.
    fn cite_block(&self, cite: &Cite, terms: &Terms) -> Option<Block> {
        match cite {
            Cite::File { repo, path, lines } => {
                let root = self.root_of(repo)?;
                if path.contains(['*', '<', '{']) {
                    return None;
                }
                let key = format!("file:{repo}:{path}:{lines:?}");
                let Some(text) = fs::read(root.join(path)).ok().map(|b| String::from_utf8_lossy(&b).replace("\r\n", "\n")) else {
                    return Some(Block { key, head: format!("{repo}:{path}"), body: "(no such file in the checkout now)".into() });
                };
                let all: Vec<(usize, &str)> = text.lines().enumerate().map(|(i, l)| (i + 1, l)).collect();
                let wiki = self.check.repos.iter().any(|t| t.library && t.name == *repo);
                let date = if wiki { " (a page of this wiki)" } else { "" };
                let body = match lines {
                    Some((a, b)) if *a <= all.len() => {
                        let a = *a;
                        let level = if is_doc(path) { heading_level(all[a - 1].1) } else { 0 };
                        // A definition cited by its first line: its body.
                        let body_of = || {
                            let map = crate::codemap::map_file(path, &text)?;
                            let at = |s: &&crate::codemap::Symbol| s.is_def && s.end_line > s.line && (s.line - a as i64).abs() <= 2;
                            let s = map.symbols.iter().filter(at).min_by_key(|s| (s.line - a as i64).abs())?;
                            let (from, to) = (usize::try_from(s.line).ok()?.max(1), usize::try_from(s.end_line).ok()?.min(all.len()));
                            Some(pick_lines(&all[from - 1..to.min(from + 150)], terms, BLOCK_MAX, false))
                        };
                        // A doc cited at a part Ken read it in (a brief is read by
                        // section, a long one in parts): that whole part.
                        let part = is_doc(path).then(|| crate::wikidraft::brief_sections(&text).into_iter().find(|p| p.line == a)).flatten();
                        if let Some(p) = part {
                            let end = (a - 1 + p.text.lines().count()).min(all.len());
                            pick_lines(&all[a - 1..end], terms, PART_MAX, false)
                        } else if level > 0 {
                            // A heading cited: its section, to the next heading as high.
                            let end = all[a..].iter().position(|(_, l)| (1..=level).contains(&heading_level(l))).map_or(all.len(), |k| a + k);
                            pick_lines(&all[a - 1..end.min(a - 1 + 120)], terms, BLOCK_MAX, false)
                        } else if let Some(body) = (*b == a).then(body_of).flatten() {
                            body
                        } else {
                            // The cited lines whole, then a few either side.
                            let b = (*b).min(all.len()).min(a + 40);
                            let line = |(n, l): &(usize, &str), max: usize| format!("{n}: {}", crate::wikidraft::clip_line(l, max));
                            let before: Vec<String> = all[a.saturating_sub(4)..a - 1].iter().map(|x| line(x, 400)).collect();
                            let cited: Vec<String> = all[a - 1..b].iter().map(|x| line(x, BLOCK_MAX)).collect();
                            let after: Vec<String> = all[b..(b + 3).min(all.len())].iter().map(|x| line(x, 400)).collect();
                            [before, cited, after].concat().join("\n")
                        }
                    }
                    Some((a, _)) => format!("(the file has {} lines; line {a} is past its end)", all.len()),
                    None => pick_lines(&all, terms, BLOCK_MAX, false),
                };
                let shown = match lines {
                    Some((a, b)) if a != b => format!("{repo}:{path}:{a}-{b}"),
                    Some((a, _)) => format!("{repo}:{path}:{a}"),
                    None => format!("{repo}:{path}"),
                };
                Some(Block { key, head: format!("{shown}{date}"), body })
            }
            Cite::Derived { label } => {
                let bare = |l: &str| match l.split_once(':') {
                    Some((r, rest)) => format!("{}:{rest}", r.split('@').next().unwrap_or(r)),
                    None => l.to_string(),
                };
                let key = format!("derived:{}", bare(label));
                let body = match self.sources.iter().find(|s| bare(&s.label) == bare(label)) {
                    Some(s) => {
                        let lines: Vec<(usize, &str)> = s.text.lines().enumerate().map(|(i, l)| (i + 1, l)).collect();
                        pick_lines(&lines, terms, DERIVED_MAX, true)
                    }
                    None => "(the page was not given a source with this label)".into(),
                };
                Some(Block { key, head: format!("{label} (derived by Ken from the checkout now)"), body })
            }
            Cite::Page(target) => {
                let key = format!("page:{target}");
                let path = self.page_path(target);
                let owns = path.as_deref().map(crate::wikidraft::owns).filter(|o| !o.is_empty());
                let text = path.as_deref().and_then(|p| fs::read_to_string(self.wiki.join(p)).ok());
                let mut body = owns.map(|o| format!("this page owns: {o}\n")).unwrap_or_default();
                match (&path, text) {
                    (Some(_), Some(t)) => {
                        let lines: Vec<(usize, &str)> = t.lines().enumerate().map(|(i, l)| (i + 1, l)).collect();
                        body.push_str(&pick_lines(&lines, terms, 1_500, false));
                    }
                    (Some(_), None) => body.push_str("(not drafted yet)"),
                    (None, _) => body.push_str("(no page of this wiki has that name)"),
                }
                Some(Block { key, head: format!("[[{target}]]{}", path.map(|p| format!(" ({p})")).unwrap_or_default()), body })
            }
            Cite::Ruling(id) => {
                let key = format!("ruling:{id}");
                let mut hits = String::new();
                for dir in ["decisions", "team/decisions"] {
                    for e in fs::read_dir(self.wiki.join(dir)).into_iter().flatten().flatten() {
                        let Ok(t) = fs::read_to_string(e.path()) else { continue };
                        let lines: Vec<&str> = t.lines().collect();
                        let word = regex::Regex::new(&format!(r"\b{}\b", regex::escape(id))).expect("a valid pattern");
                        for (i, l) in lines.iter().enumerate().filter(|(_, l)| word.is_match(l)).take(3) {
                            let name = e.file_name().to_string_lossy().to_string();
                            hits.push_str(&format!("{dir}/{name}:{}: {}\n", i + 1, crate::wikidraft::clip_line(l, 400)));
                        }
                    }
                }
                let body = if hits.is_empty() { "(no ruling of that id in the decisions log)".into() } else { hits.trim_end().to_string() };
                Some(Block { key, head: format!("ruling {id}"), body })
            }
        }
    }

    /// The wiki page a link names: a page on disk or drafted, by path or
    /// by file name, or a page of the library Ken drafts.
    fn page_path(&self, target: &str) -> Option<String> {
        let want = target.trim_end_matches(".md");
        let matches = |p: &str| {
            let p = p.trim_end_matches(".md");
            p == want || p.rsplit('/').next() == Some(want) || p.ends_with(&format!("/{want}"))
        };
        let library = self.check.repos.iter().filter(|t| t.library).flat_map(|t| t.files.iter().map(|f| f.join("/")));
        let known = self.drafted.iter().cloned().chain(library).chain(crate::wikidraft::OWNS.iter().map(|(p, _)| p.to_string()));
        let mut found: Vec<String> = known.filter(|p| p.ends_with(".md") && matches(p)).collect();
        found.sort_by_key(|p| (p.starts_with("Templates/"), p.len()));
        found.into_iter().next()
    }

    /// Where the code has a name, or that it has it nowhere.
    fn name_block(&self, name: &str) -> Block {
        let repos = self.code_repos();
        let mut hits: Vec<String> = Vec::new();
        let mut count = 0;
        for (repo, root) in &repos {
            let (n, lines) = self.grep(repo, root, name, false);
            count += n;
            hits.extend(lines.into_iter().take(4));
        }
        let defined = |l: &String| ["class ", "interface ", "enum ", "record ", "fn ", "def ", "function ", "struct ", "trait ", "type "].iter().any(|k| l.contains(&format!("{k}{name}")));
        hits.sort_by_key(|l| !defined(l));
        hits.truncate(6);
        let names: Vec<&str> = repos.iter().map(|(r, _)| *r).collect();
        let body = if count == 0 {
            format!("no hits: `git grep -F {name}` over the code and data of {} (docs left out) finds nothing", names.join(", "))
        } else {
            format!("{count} lines name it; the first:\n{}", hits.join("\n"))
        };
        Block { key: format!("name:{name}"), head: format!("Ken's search of the code for `{name}`"), body }
    }

    /// Where the code has an item's words: for the three words with the
    /// fewest hits (but some), the first lines that name them.
    fn term_blocks(&self, terms: &[String], repos: &[&str]) -> Vec<Block> {
        let roots: Vec<(&str, &Path)> = self.code_repos().into_iter().filter(|(r, _)| repos.contains(r)).collect();
        let mut ranked: Vec<(usize, &String, Vec<String>)> = Vec::new();
        for t in terms {
            let mut count = 0;
            let mut lines = Vec::new();
            for (repo, root) in &roots {
                let (n, l) = self.grep(repo, root, t, true);
                count += n;
                lines.extend(l.into_iter().take(4));
            }
            if count > 0 {
                ranked.push((count, t, lines));
            }
        }
        ranked.sort_by_key(|(n, _, _)| *n);
        ranked
            .into_iter()
            .take(3)
            .map(|(n, t, mut lines)| {
                lines.truncate(4);
                Block {
                    key: format!("term:{}:{t}", repos.join(",")),
                    head: format!("Ken's search of the code for the word \"{t}\" (any case)"),
                    body: format!("{n} lines; the first:\n{}", lines.join("\n")),
                }
            })
            .collect()
    }

    /// The evidence for one statement, as keys into `blocks`: what each of
    /// its citations shows, and for a statement that something is gone,
    /// Ken's search of the checkout for each name it says is gone.
    fn evidence(&self, s: &Statement, blocks: &mut Vec<Block>) -> Vec<String> {
        let repos = self.repo_names();
        let terms = Terms::of(s, &repos);
        let mut keys: Vec<String> = Vec::new();
        let add = |b: Block, blocks: &mut Vec<Block>, keys: &mut Vec<String>| {
            if !keys.contains(&b.key) {
                keys.push(b.key.clone());
            }
            if !blocks.iter().any(|x| x.key == b.key) {
                blocks.push(b);
            }
        };
        for c in cites(&s.text, &repos) {
            // What a long source shows depends on the statement: one
            // block for each different set of lines.
            if let Some(mut b) = self.cite_block(&c, &terms) {
                b.key = format!("{}#{:x}", b.key, hash(&b.body));
                add(b, blocks, &mut keys);
            }
        }
        if crate::wikidraft::says_gone(&s.text) {
            for name in code_names(&s.text, &repos).into_iter().take(4) {
                add(self.name_block(&name), blocks, &mut keys);
            }
        }
        keys
    }

    /// The evidence for one `To fill:` item: the pages drafted so far, the
    /// files whose names hold its words, and the code that names them.
    fn fill_evidence(&self, item: &str, page: &str, blocks: &mut Vec<Block>) -> Vec<String> {
        let words: Vec<String> = terms(item).into_iter().map(|w| w.trim_end_matches('s').to_string()).filter(|w| w.len() >= 4).collect();
        let mut keys = vec!["front".to_string()];
        if blocks.iter().any(|b| b.key == "team") {
            keys.push("team".into());
        }
        if blocks.iter().any(|b| b.key == "heads") {
            keys.push("heads".into());
        }
        let add = |b: Block, blocks: &mut Vec<Block>, keys: &mut Vec<String>| {
            keys.push(b.key.clone());
            if !blocks.iter().any(|x| x.key == b.key) {
                blocks.push(b);
            }
        };
        // The pages drafted so far: lines that name two of its words (one,
        // for an item of one), each at the start of a word.
        let names: Vec<String> = item.split('`').skip(1).step_by(2).map(str::trim).filter(|t| t.len() >= 3 && !t.contains(' ')).map(String::from).collect();
        let named_in = |l: &str| {
            let low: Vec<String> = l.split(|c: char| !c.is_alphanumeric() && c != '_').map(str::to_lowercase).collect();
            let hits = words.iter().filter(|w| low.iter().any(|x| x.starts_with(w.as_str()))).count();
            let named = names.iter().any(|n| low.iter().any(|x| *x == n.to_lowercase()));
            named || hits >= 2.min(words.len()).max(1)
        };
        let mut lines = String::new();
        for p in self.drafted.iter().filter(|p| p.as_str() != page) {
            let Ok(t) = fs::read_to_string(self.wiki.join(p)) else { continue };
            for (i, l) in t.lines().enumerate() {
                if named_in(l) && !l.trim().is_empty() {
                    let row = format!("{p}:{}: {}\n", i + 1, crate::wikidraft::clip_line(l.trim(), 300));
                    if lines.len() + row.len() > 2_000 {
                        break;
                    }
                    lines.push_str(&row);
                }
            }
        }
        if !lines.is_empty() {
            add(Block { key: format!("pages:{item}"), head: "lines of the pages drafted so far that name its words".into(), body: lines.trim_end().into() }, blocks, &mut keys);
        }
        // Files whose names hold two of its words: a test, a command, a config.
        let mut named: Vec<String> = Vec::new();
        for t in self.check.repos.iter().filter(|t| !t.library) {
            for f in &t.files {
                let last = f.last().map(String::as_str).unwrap_or_default();
                let low = last.to_lowercase();
                if words.len() >= 2 && words.iter().filter(|w| low.contains(w.as_str())).count() >= 2 && named.len() < 8 {
                    named.push(format!("{}:{}", t.name, f.join("/")));
                }
            }
        }
        if !named.is_empty() {
            add(Block { key: format!("named:{item}"), head: "files whose names hold its words".into(), body: named.join("\n") }, blocks, &mut keys);
        }
        // Each name it gives in backticks, as a whole word in any case.
        for n in names.iter().take(3) {
            let mut hits: Vec<String> = Vec::new();
            let mut count = 0;
            for (repo, root) in self.code_repos() {
                let (c, l) = crate::wikidraft::grep_repo_word(repo, root, n);
                count += c;
                hits.extend(l.into_iter().take(5));
            }
            if count > 0 {
                hits.truncate(8);
                let body = format!("{count} lines; the first:\n{}", hits.join("\n"));
                add(Block { key: format!("word:{n}"), head: format!("Ken's search of the code for the word `{n}` (any case)"), body }, blocks, &mut keys);
            }
        }
        let repos: Vec<&str> = self.code_repos().into_iter().map(|(r, _)| r).collect();
        for b in self.term_blocks(&words, &repos) {
            add(b, blocks, &mut keys);
        }
        keys
    }
}

// --------------------------------------------------------------------- items

/// The items of a `To fill:` line, split at its top-level commas, semicolons
/// and "and"s (never inside backticks or brackets), the last full stop off.
pub fn fill_items(text: &str) -> Vec<String> {
    let text = text.trim().trim_end_matches('.');
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let (mut tick, mut depth) = (false, 0i32);
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '`' => tick = !tick,
            '(' | '[' if !tick => depth += 1,
            ')' | ']' if !tick => depth -= 1,
            _ => {}
        }
        let top = !tick && depth <= 0;
        let rest: String = chars[i..].iter().take(5).collect();
        if top && (c == ',' || c == ';') {
            out.push(std::mem::take(&mut cur));
            i += 1;
            continue;
        }
        if top && rest == " and " {
            out.push(std::mem::take(&mut cur));
            i += 5;
            continue;
        }
        cur.push(c);
        i += 1;
    }
    out.push(cur);
    out.into_iter()
        .map(|s| {
            let s = s.trim();
            s.strip_prefix("and ").unwrap_or(s).trim().to_string()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// Whether a statement sends a question to a person: "a person needs to
/// rule", "for a person to fill", "a person must decide".
fn asks_a_person(text: &str) -> bool {
    let l = text.to_lowercase();
    ["a person", "someone"].iter().any(|w| l.contains(w)) && ["to rule", "must rule", "needs to rule", "to decide", "must decide", "to fill", "to confirm"].iter().any(|w| l.contains(w))
}

/// `text` with its citations taken out.
fn uncited(text: &str, repos: &[&str]) -> String {
    let mut out = String::new();
    for (k, part) in text.split('`').enumerate() {
        let citation = k % 2 == 1 && repos.iter().any(|r| part.starts_with(&format!("{r}:")) || part.starts_with(&format!("{r}@")));
        if !citation {
            if k % 2 == 1 {
                out.push('`');
                out.push_str(part);
                out.push('`');
            } else {
                out.push_str(part);
            }
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Items as one line: `a`, `a and b`, `a, b and c`.
pub fn join_items(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [head @ .., last] => format!("{} and {last}", head.join(", ")),
    }
}

/// Whether a `To fill:` item asks for the commit the sources were read at,
/// which Ken writes into `pin:` itself.
fn asks_for_pin(item: &str) -> bool {
    let l = item.to_lowercase();
    l.contains("`pin`") || (l.contains("commit") && (l.contains("read at") || l.contains("were read") || l.contains("pinned commit")))
}

// ------------------------------------------------------------------ the call

/// The call that checks a page's statements, written for a model that did
/// not draft them.
fn prompt(page: &str, items: &[(String, &Statement, Vec<String>)], fills: &[(String, String, Vec<String>)], blocks: &[Block]) -> String {
    let mut s = format!(
        "You are checking one drafted page of a team's wiki, `{page}`, before it is published. Another call drafted it; you \
         did not. The page must say what its sources say. Judge each numbered statement only against the evidence listed for \
         it: the source lines it cites, read from the checkout now, the source it names, the page it links. Whether a source is \
         itself up to date is not yours to judge: a statement that repeats its source faithfully is supported. Never use what \
         you know beyond the evidence.\n\n\
         Verdicts:\n\
         - supported: the cited source says what the statement says, in every detail it gives (each name, number, version, \
         path, member of a list, who and how many).\n\
         - contradicted: the statement misstates its source, or gives only part of a set the source gives whole. Give `text`, \
         the statement as the source has it, in the same form: a table row keeps its columns and their order, a list item its \
         marker, and it keeps the citation in backticks. Keep every part the source supports; leave out what it does not.\n\
         - unsupported: the cited source does not say it: a reason, purpose or consequence nobody gave, a generalisation wider \
         than the source (\"all\", \"only\", \"every\", \"none\" where the source names some), a fact from elsewhere.\n\n\
         Rules:\n\
         - A set (keys, weapons, installers, values, people, places) must be whole as the source gives it: when the source shows \
         more members, write them all.\n\
         - When a statement is right but adds a reason or a wider claim its source does not give, it is contradicted: rewrite it \
         without that part.\n\
         - Something said to be gone, removed or missing is supported only when a source says so or Ken's search of the checkout \
         for it found no hits.\n\
         - A statement that only says which page holds a fact is supported when that page owns the fact.\n\
         - A citation must show the statement: one that shows something else does not support it.\n\n"
    );
    if !fills.is_empty() {
        s.push_str(
            "`To fill:` items: each asks a person for something. Answer it when the evidence listed for it answers it: `answered`, \
             with `text`, one plain sentence that answers it and ends with the citation of the line that shows it in backticks \
             (a `[[page]]` link when another page states it). Say only what that line shows: never a count of Ken's search. \
             Otherwise `open`.\n\n",
        );
    }
    s.push_str(
        "Reply with JSON only, no fences: an array with one object per statement and item, in order, like\n\
         [{\"id\": \"S1\", \"verdict\": \"supported\"},\n \
         {\"id\": \"S2\", \"verdict\": \"contradicted\", \"text\": \"| Bow | 1.2 | `repo:data/x.json:4` |\", \"evidence\": \"x.json:4 has Bow 1.2\"},\n \
         {\"id\": \"S3\", \"verdict\": \"unsupported\", \"evidence\": \"the source gives no reason\"},\n \
         {\"id\": \"T1\", \"verdict\": \"answered\", \"text\": \"The engine is pinned at 37712f0 `repo:.wright/team.json`.\"},\n \
         {\"id\": \"T2\", \"verdict\": \"open\"}]\n\nEVIDENCE\n",
    );
    for (i, b) in blocks.iter().enumerate() {
        s.push_str(&format!("\n[E{}] {}\n{}\n", i + 1, b.head, b.body));
    }
    let ids = |keys: &[String]| -> String {
        keys.iter().filter_map(|k| blocks.iter().position(|b| &b.key == k)).map(|i| format!("E{}", i + 1)).collect::<Vec<_>>().join(", ")
    };
    s.push_str("\nSTATEMENTS\n");
    for (id, st, keys) in items {
        let mut at = String::new();
        if !st.heading.is_empty() {
            at.push_str(&format!(" under `{}`", st.heading));
        }
        if !st.header.is_empty() {
            at.push_str(&format!(", columns `{}`", st.header));
        }
        let ev = ids(keys);
        s.push_str(&format!("\n{id} (line {}{at}; evidence: {}):\n{}\n", st.start + 1, if ev.is_empty() { "none".into() } else { ev }, st.text));
    }
    if !fills.is_empty() {
        s.push_str("\nTO FILL ITEMS\n");
        for (id, item, keys) in fills {
            s.push_str(&format!("\n{id} (evidence: {}): {item}\n", ids(keys)));
        }
    }
    s
}

/// One verdict of the reply.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Verdict {
    pub id: String,
    pub verdict: String,
    pub text: String,
    pub evidence: String,
}

/// The verdicts of a reply: its JSON array, fenced or not. An error when
/// there is none to read.
pub fn verdicts(reply: &str) -> Result<Vec<Verdict>> {
    let (Some(a), Some(b)) = (reply.find('['), reply.rfind(']')) else {
        return Err(Error::Other("the check's reply had no list of verdicts".into()));
    };
    let v: Vec<serde_json::Value> = serde_json::from_str(&reply[a..=b]).map_err(|e| Error::Other(format!("the check's reply: {e}")))?;
    let field = |o: &serde_json::Value, k: &str| match o.get(k) {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Number(n)) => n.to_string(),
        _ => String::new(),
    };
    Ok(v.iter().map(|o| Verdict { id: field(o, "id"), verdict: field(o, "verdict").to_lowercase(), text: field(o, "text"), evidence: field(o, "evidence") }).collect())
}

/// The cells of a table row, outside backticks.
fn cells(row: &str) -> usize {
    let mut n: usize = 0;
    let mut tick = false;
    let mut prev = ' ';
    for c in row.trim().chars() {
        if c == '`' {
            tick = !tick;
        }
        if c == '|' && !tick && prev != '\\' {
            n += 1;
        }
        prev = c;
    }
    n.saturating_sub(1)
}

/// A contradicted statement's replacement, in its statement's form and
/// with a citation, or None when it cannot stand in for it.
fn replacement(st: &Statement, text: &str, repos: &[&str]) -> Option<String> {
    let t = text.trim();
    if t.is_empty() || cites(t, repos).is_empty() {
        return None;
    }
    match st.kind {
        Kind::Row => {
            let row = t.lines().map(str::trim).collect::<Vec<_>>().join(" ");
            (row.starts_with('|') && row.ends_with('|') && cells(&row) == cells(&st.text)).then_some(row)
        }
        Kind::Item => {
            let indent: String = st.text.chars().take_while(|c| c.is_whitespace()).collect();
            let one = t.lines().map(str::trim).collect::<Vec<_>>().join(" ");
            Some(if is_list_item(&one) { format!("{indent}{}", one.trim_start()) } else { format!("{indent}- {one}") })
        }
        Kind::Para => Some(t.lines().map(str::trim).collect::<Vec<_>>().join(" ")),
    }
}

/// A page with every empty table and section taken out: a table whose rows
/// were all removed, then a heading with nothing under it before the next
/// heading as high, and the in-page links to it.
pub fn tidy(page: &str) -> String {
    let mut lines: Vec<Option<&str>> = page.lines().map(Some).collect();
    let n = lines.len();
    // Tables with a header and no rows.
    for i in 0..n {
        if lines[i].is_some_and(|l| l.trim().starts_with('|')) && lines.get(i + 1).copied().flatten().is_some_and(is_table_separator) {
            let has_row = lines.get(i + 2).copied().flatten().is_some_and(|l| l.trim().starts_with('|'));
            if !has_row {
                lines[i] = None;
                lines[i + 1] = None;
            }
        }
    }
    // Headings with nothing under them, deepest first.
    let start = body_start(&page.lines().collect::<Vec<_>>());
    let mut removed: Vec<String> = Vec::new();
    for level in (2..=6).rev() {
        for i in start..n {
            let Some(l) = lines[i] else { continue };
            if heading_level(l.trim_start()) != level {
                continue;
            }
            let mut empty = true;
            for line in lines.iter().skip(i + 1).flatten() {
                let h = heading_level(line.trim_start());
                if h > 0 && h <= level {
                    break;
                }
                if !line.trim().is_empty() && fill_text(line).is_none() {
                    empty = false;
                    break;
                }
            }
            if empty {
                removed.push(l.trim_start().trim_start_matches('#').trim().to_string());
                lines[i] = None;
            }
        }
    }
    let mut out: Vec<String> = Vec::new();
    for l in lines.into_iter().flatten() {
        let mut l = l.to_string();
        for h in &removed {
            for link in [format!("[[#{h}]] · "), format!(" · [[#{h}]]"), format!("[[#{h}]]")] {
                l = l.replace(&link, "");
            }
        }
        if l.trim().is_empty() && out.last().is_some_and(|p: &String| p.trim().is_empty()) {
            continue;
        }
        out.push(l);
    }
    let mut text = out.join("\n");
    if page.ends_with('\n') {
        text.push('\n');
    }
    text
}

/// What the check of one page did.
#[derive(Debug, Default)]
pub struct Checked {
    pub page: String,
    pub changes: Vec<Change>,
    /// One line for the run's log: how many statements, and what became of
    /// them; empty when the page made none.
    pub summary: String,
}

/// Check `page` (its path in the wiki) as drafted, `text`, against the
/// checkout: statements with no citation out, every other one judged by
/// `generate` against its evidence, `To fill:` items answered where the
/// evidence answers them, then the verdicts applied ([`tidy`]). `template`
/// holds the lines of the template the page was drafted from. No call when
/// nothing needs judging. An error when a call fails twice or its reply
/// cannot be read twice: the page is then not published.
pub fn check(
    page: &str,
    text: &str,
    template: &HashSet<String>,
    ctx: &Ctx,
    generate: &mut impl FnMut(&str) -> Result<String>,
) -> Result<(String, Checked)> {
    let repos = ctx.repo_names();
    let parsed = parse(text, template);
    let mut out: Vec<Option<String>> = text.lines().map(|l| Some(l.to_string())).collect();
    let mut done = Checked { page: page.to_string(), ..Default::default() };
    let change = |kind: &str, before: &str, after: &str, evidence: &str| Change {
        page: page.to_string(),
        kind: kind.into(),
        before: before.into(),
        after: after.into(),
        evidence: evidence.into(),
    };
    // No citation, no statement.
    let mut judged: Vec<&Statement> = Vec::new();
    for st in &parsed.statements {
        if cites(&st.text, &repos).is_empty() {
            out[st.start..st.end].iter_mut().for_each(|l| *l = None);
            done.changes.push(change("uncited", &st.text, "", "it cites no source"));
        } else {
            judged.push(st);
        }
    }
    // The To fill line: the pin is Ken's to write; the rest is checked.
    let mut open: Vec<String> = Vec::new();
    let mut asked: Vec<String> = Vec::new();
    if let Some(at) = parsed.fill {
        for item in fill_items(fill_text(text.lines().nth(at).unwrap_or_default()).unwrap_or_default()) {
            if asks_for_pin(&item) {
                done.changes.push(change("filled", &item, "", "Ken writes `pin:` from the commit each repo was read at"));
            } else {
                asked.push(item);
            }
        }
    }
    let mut blocks: Vec<Block> = Vec::new();
    if !asked.is_empty() {
        let lines: Vec<&str> = text.lines().collect();
        let front = lines[..body_start(&lines)].join("\n");
        blocks.push(Block { key: "front".into(), head: format!("the front matter of {page}"), body: front });
        if let Ok(t) = fs::read_to_string(ctx.wiki.join(".wright/team.json")) {
            let lines: Vec<(usize, &str)> = t.lines().enumerate().map(|(i, l)| (i + 1, l)).collect();
            let wiki = ctx.check.repos.iter().find(|t| t.library).map_or("wiki", |t| t.name.as_str());
            blocks.push(Block { key: "team".into(), head: format!("{wiki}:.wright/team.json, the team's manifest"), body: pick_lines(&lines, &Terms::default(), 6_000, false) });
        }
        if !ctx.check.heads.is_empty() {
            let mut heads: Vec<String> = ctx.check.heads.iter().map(|(r, sha)| format!("{r}@{sha}")).collect();
            heads.sort();
            blocks.push(Block { key: "heads".into(), head: "the commit of each repo Ken read".into(), body: heads.join("\n") });
        }
    }
    let mut items: Vec<(String, &Statement, Vec<String>)> = Vec::new();
    for (k, st) in judged.iter().enumerate() {
        let keys = ctx.evidence(st, &mut blocks);
        items.push((format!("S{}", k + 1), st, keys));
    }
    let mut fills: Vec<(String, String, Vec<String>)> = Vec::new();
    for (k, item) in asked.iter().enumerate() {
        let keys = ctx.fill_evidence(item, page, &mut blocks);
        fills.push((format!("T{}", k + 1), item.clone(), keys));
    }
    // A statement that sends a question to a person is a `To fill:` item
    // too: answered, the answer takes its place. On 2026-10-07 Config-Map
    // asked a person to rule on the reload command the code registers.
    let mut asks: HashMap<String, String> = HashMap::new();
    for (sid, st, _) in &items {
        if asks_a_person(&st.text) {
            let question = uncited(&st.text, &repos);
            let keys = ctx.fill_evidence(&question, page, &mut blocks);
            let tid = format!("T{}", fills.len() + 1);
            asks.insert(sid.clone(), tid.clone());
            fills.push((tid, question, keys));
        }
    }
    let mut verdicts_by_id: HashMap<String, Verdict> = HashMap::new();
    let mut calls = 0;
    if !items.is_empty() || !fills.is_empty() {
        for (chunk, chunk_fills) in chunks(&items, &fills, &blocks) {
            let used: Vec<&String> = chunk.iter().flat_map(|(_, _, k)| k).chain(chunk_fills.iter().flat_map(|(_, _, k)| k)).collect();
            let mine: Vec<Block> = blocks.iter().filter(|b| used.contains(&&b.key)).cloned().collect();
            let p = prompt(page, chunk, chunk_fills, &mine);
            let mut got = None;
            let mut last = String::new();
            for _ in 0..2 {
                calls += 1;
                match generate(&p).and_then(|r| verdicts(&r)) {
                    Ok(v) => {
                        got = Some(v);
                        break;
                    }
                    Err(e) => last = e.to_string(),
                }
            }
            let Some(v) = got else {
                return Err(Error::Other(format!("not published: the check of {page} failed twice: {last}")));
            };
            for x in v {
                verdicts_by_id.insert(x.id.trim().to_uppercase(), x);
            }
        }
    }
    let ev_of = |keys: &[String]| -> String {
        keys.iter().filter_map(|k| blocks.iter().find(|b| &b.key == k)).map(|b| b.head.clone()).collect::<Vec<_>>().join("; ")
    };
    let (mut kept, mut replaced, mut removed) = (0, 0, 0);
    for (id, st, keys) in &items {
        let answer = asks.get(id).and_then(|t| verdicts_by_id.get(t)).filter(|v| v.verdict == "answered" && !cites(&v.text, &repos).is_empty());
        if let Some(a) = answer {
            let a = a.text.trim().lines().map(str::trim).collect::<Vec<_>>().join(" ");
            out[st.start] = Some(a.clone());
            out[st.start + 1..st.end].iter_mut().for_each(|l| *l = None);
            done.changes.push(change("answered", &st.text, &a, &ev_of(keys)));
            continue;
        }
        let v = verdicts_by_id.get(id).cloned().unwrap_or_default();
        let evidence = if v.evidence.is_empty() { ev_of(keys) } else { v.evidence.clone() };
        match v.verdict.as_str() {
            "supported" => kept += 1,
            "contradicted" => match replacement(st, &v.text, &repos) {
                Some(new) => {
                    out[st.start] = Some(new.clone());
                    out[st.start + 1..st.end].iter_mut().for_each(|l| *l = None);
                    done.changes.push(change("replaced", &st.text, &new, &evidence));
                    replaced += 1;
                }
                None => {
                    out[st.start..st.end].iter_mut().for_each(|l| *l = None);
                    let why = format!("contradicted, and the code's version could not stand in its place ({}): {evidence}", v.text.trim());
                    done.changes.push(change("removed", &st.text, "", &why));
                    removed += 1;
                }
            },
            other => {
                out[st.start..st.end].iter_mut().for_each(|l| *l = None);
                let why = if other == "unsupported" { evidence } else { format!("no verdict for it; nothing unverified is published ({evidence})") };
                done.changes.push(change("removed", &st.text, "", &why));
                removed += 1;
            }
        }
    }
    // Items answered leave the To fill line; their answers go just above it.
    let mut answers: Vec<String> = Vec::new();
    for (id, item, keys) in fills.iter().filter(|(t, _, _)| !asks.values().any(|a| a == t)) {
        let v = verdicts_by_id.get(id).cloned().unwrap_or_default();
        let has_cite = !cites(&v.text, &repos).is_empty();
        if v.verdict == "answered" && has_cite {
            let a = v.text.trim().lines().map(str::trim).collect::<Vec<_>>().join(" ");
            done.changes.push(change("answered", item, &a, &ev_of(keys)));
            answers.push(a);
        } else {
            open.push(item.clone());
        }
    }
    if let Some(at) = parsed.fill {
        let mut slot = String::new();
        for a in &answers {
            slot.push_str(a);
            slot.push_str("\n\n");
        }
        if !open.is_empty() {
            slot.push_str(&format!("To fill: {}.", join_items(&open)));
        }
        out[at] = (!slot.trim().is_empty()).then(|| slot.trim_end().to_string());
    }
    let mut page_text = out.into_iter().flatten().collect::<Vec<_>>().join("\n");
    if text.ends_with('\n') {
        page_text.push('\n');
    }
    let page_text = tidy(&page_text);
    let uncited = done.changes.iter().filter(|c| c.kind == "uncited").count();
    if items.is_empty() && fills.is_empty() && done.changes.is_empty() {
        return Ok((page_text, done));
    }
    done.summary = format!(
        "{page}: {} statements checked in {calls} call{}: {kept} supported, {replaced} replaced by what the source says, {removed} removed; \
         {uncited} uncited removed; {} of {} to-fill items answered",
        items.len(),
        if calls == 1 { "" } else { "s" },
        done.changes.iter().filter(|c| c.kind == "answered").count(),
        fills.len(),
    );
    Ok((page_text, done))
}

/// The statements and items in calls of at most [`CALL_MAX`] characters of
/// statements and evidence each; one call for most pages.
#[allow(clippy::type_complexity)]
fn chunks<'s, 'a>(
    items: &'s [(String, &'a Statement, Vec<String>)],
    fills: &'s [(String, String, Vec<String>)],
    blocks: &[Block],
) -> Vec<(&'s [(String, &'a Statement, Vec<String>)], &'s [(String, String, Vec<String>)])> {
    let size = |keys: &[String], seen: &mut HashSet<String>| -> usize {
        keys.iter().filter(|k| seen.insert((*k).clone())).filter_map(|k| blocks.iter().find(|b| &b.key == k)).map(|b| b.head.len() + b.body.len() + 8).sum()
    };
    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let (mut from, mut used) = (0, 0);
    for (i, (_, st, keys)) in items.iter().enumerate() {
        let n = st.text.len() + 80 + size(keys, &mut seen);
        if used + n > CALL_MAX && i > from {
            out.push((&items[from..i], &fills[..0]));
            from = i;
            used = 0;
            seen.clear();
            size(keys, &mut seen);
        }
        used += n;
    }
    out.push((&items[from..], fills));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_splits_into_statements_and_what_is_not_one() {
        let template: HashSet<String> = ["| a feature | [[Feature Status]] |".to_string()].into();
        let page = "---\ntitle: X\npin: \"\"\n---\n\n# X\n\nThe lede says what the page is for.\n\n[[#One]] · [[#Two]]\n\n## One\n\n\
            | key | value | shown in |\n|---|---|---|\n| Bow | 1.2 | `game:data/w.json:3` |\n| a feature | [[Feature Status]] |\n\n\
            - Runs on boot `game:src/Main.java:12`\n  and keeps running.\n- No citation here.\n\nA paragraph\nover two lines `game:README.md:4`.\n\
            ```\n./gradlew build\n```\n\nTo fill: who decides, the `pin` and what reloads it.\n";
        let p = parse(page, &template);
        let texts: Vec<&str> = p.statements.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "| Bow | 1.2 | `game:data/w.json:3` |",
                "- Runs on boot `game:src/Main.java:12`\n  and keeps running.",
                "- No citation here.",
                "A paragraph\nover two lines `game:README.md:4`.\n```\n./gradlew build\n```",
            ]
        );
        assert_eq!(p.statements[0].header, "| key | value | shown in |");
        assert_eq!(p.statements[0].heading, "## One");
        assert_eq!(p.exempt.len(), 3, "the lede, the navigation line, the template's row: {:?}", p.exempt);
        assert_eq!(p.fill, Some(page.lines().position(|l| l.starts_with("To fill")).unwrap()));
        assert_eq!(fill_items("who decides, the `pin` and what reloads it (`/a`, `/b`)."), vec!["who decides", "the `pin`", "what reloads it (`/a`, `/b`)"]);
        assert_eq!(join_items(&["a".into(), "b".into(), "c".into()]), "a, b and c");
    }

    #[test]
    fn citations_are_repo_paths_sources_links_and_rulings() {
        let c = cites("x `game@1a2b:src/A.java:12-30` `game:(layout)` `Game-Tools:README.md` `notes` [[Platform/Index|Platform]] [[#Here]] per D-279.", &["game", "Game-Tools"]);
        assert_eq!(
            c,
            vec![
                Cite::File { repo: "game".into(), path: "src/A.java".into(), lines: Some((12, 30)) },
                Cite::Derived { label: "game:(layout)".into() },
                Cite::File { repo: "Game-Tools".into(), path: "README.md".into(), lines: None },
                Cite::Page("Platform/Index".into()),
                Cite::Ruling("D-279".into()),
            ]
        );
    }

    #[test]
    fn a_reply_is_read_fenced_or_not_and_a_row_keeps_its_columns() {
        let v = verdicts("```json\n[{\"id\": \"S1\", \"verdict\": \"Supported\"}, {\"id\": 2, \"verdict\": \"contradicted\", \"text\": \"x\"}]\n```").unwrap();
        assert_eq!((v[0].verdict.as_str(), v[1].id.as_str()), ("supported", "2"));
        assert!(verdicts("I could not check it.").is_err());
        let st = Statement { start: 0, end: 1, kind: Kind::Row, text: "| a | b `g:x.json:1` |".into(), heading: String::new(), header: String::new() };
        assert_eq!(replacement(&st, "| a | c `g:x.json:2` |", &["g"]).as_deref(), Some("| a | c `g:x.json:2` |"));
        assert_eq!(replacement(&st, "| a | c | d `g:x.json:2` |", &["g"]), None, "a row keeps its columns");
        assert_eq!(replacement(&st, "| a | c |", &["g"]), None, "a replacement keeps a citation");
    }

    /// A git checkout of `files` named `name` under `dir`.
    fn checkout(dir: &Path, name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
        let root = dir.join(name);
        for (p, t) in files {
            fs::create_dir_all(root.join(p).parent().unwrap()).unwrap();
            fs::write(root.join(p), t).unwrap();
        }
        for args in [&["init", "-q"][..], &["add", "-A"][..]] {
            let ok = std::process::Command::new("git").args(args).current_dir(&root).status().unwrap().success();
            assert!(ok);
        }
        root
    }

    /// Each statement is judged against what it cites: a misstated value is
    /// replaced with the source's, a claim the source does not make is
    /// removed, an uncited one is removed before the call, the pin is Ken's,
    /// and a `To fill:` item the sources answer is answered. Every change is
    /// logged with its evidence.
    #[test]
    fn a_page_publishes_only_what_its_sources_say() {
        let d = tempfile::tempdir().unwrap();
        let root = checkout(
            d.path(),
            "game",
            &[
                ("data/w.json", "{\n  \"Bow\": 1.2,\n  \"Sword\": 1.0\n}\n"),
                ("CLAUDE.md", "# Game\n\n## Team\n\nChris decides balance.\nChris approves the protected paths.\n"),
            ],
        );
        let wiki = d.path().join("Wiki");
        fs::create_dir_all(&wiki).unwrap();
        let pc = PathCheck::of(&[("game".to_string(), root)]);
        let page = "---\ntitle: W\npin: \"\"\nsources:\n  - game:data/w.json\n---\n\n# W\n\nThe lede says what the page holds.\n\n## Weapons\n\n\
            | weapon | multiplier | shown in |\n|---|---|---|\n| Bow | 1.5 | `game:data/w.json:2` |\n| Sword | 1.0 | `game:data/w.json:3` |\n| Dagger | 0.5 | |\n\n\
            ## Who decides\n\nChris decides everything, all of it in Tools. `game:CLAUDE.md:3`\n\n\
            To fill: the commit the sources were read at (`pin`) and who approves the protected paths.\n";
        let ctx = Ctx { check: &pc, sources: &[], wiki: &wiki, drafted: &[] };
        let mut asked: Vec<String> = Vec::new();
        let mut generate = |p: &str| -> Result<String> {
            asked.push(p.to_string());
            Ok("[{\"id\": \"S1\", \"verdict\": \"contradicted\", \"text\": \"| Bow | 1.2 | `game:data/w.json:2` |\", \"evidence\": \"w.json:2 has Bow 1.2\"},\
                {\"id\": \"S2\", \"verdict\": \"supported\"},\
                {\"id\": \"S3\", \"verdict\": \"unsupported\", \"evidence\": \"the source says balance, and names no Tools\"},\
                {\"id\": \"T1\", \"verdict\": \"answered\", \"text\": \"Chris approves the protected paths. `game:CLAUDE.md:6`\"}]"
                .to_string())
        };
        let (out, done) = check("Reference/W.md", page, &HashSet::new(), &ctx, &mut generate).unwrap();
        assert_eq!(asked.len(), 1, "one call for the page");
        let p = &asked[0];
        assert!(p.contains("2:   \"Bow\": 1.2,") && p.contains("6: Chris approves the protected paths."), "the cited lines, a heading's section: {p}");
        assert!(!p.contains("Dagger"), "an uncited row is never sent: {p}");
        assert!(out.contains("| Bow | 1.2 | `game:data/w.json:2` |") && out.contains("| Sword | 1.0 |"), "{out}");
        assert!(!out.contains("Dagger") && !out.contains("all of it in Tools"), "{out}");
        assert!(out.contains("Chris approves the protected paths. `game:CLAUDE.md:6`") && !out.contains("To fill"), "{out}");
        let kinds: Vec<&str> = done.changes.iter().map(|c| c.kind.as_str()).collect();
        assert_eq!(kinds, vec!["uncited", "filled", "replaced", "removed", "answered"]);
        assert_eq!(done.changes[2].before, "| Bow | 1.5 | `game:data/w.json:2` |");
        assert_eq!(done.changes[2].evidence, "w.json:2 has Bow 1.2");
        assert!(done.summary.contains("3 statements checked in 1 call: 1 supported, 1 replaced"), "{}", done.summary);

        // A statement the reply leaves out is not published; a reply that
        // cannot be read twice publishes nothing.
        let mut silent = |_: &str| -> Result<String> { Ok("[{\"id\": \"S2\", \"verdict\": \"supported\"}]".to_string()) };
        let (out, _) = check("Reference/W.md", page, &HashSet::new(), &ctx, &mut silent).unwrap();
        assert!(!out.contains("| Bow |") && out.contains("| Sword |") && out.contains("To fill: who approves the protected paths."), "{out}");
        let mut calls = 0;
        let mut broken = |_: &str| -> Result<String> {
            calls += 1;
            Ok("I could not check it.".to_string())
        };
        assert!(check("Reference/W.md", page, &HashSet::new(), &ctx, &mut broken).is_err());
        assert_eq!(calls, 2);
    }

    /// A statement that sends a question to a person is answered from the
    /// checkout when it can be, the answer in its place.
    #[test]
    fn a_question_for_a_person_the_checkout_answers_is_answered() {
        let d = tempfile::tempdir().unwrap();
        let root = checkout(
            d.path(),
            "game",
            &[
                ("CLAUDE.md", "# Game\n\nReload configs with `/reload`.\n"),
                ("src/WorldCommand.java", "class WorldCommand {\n  void reload() { register(\"sr_world reload\"); }\n}\n"),
            ],
        );
        let wiki = d.path().join("Wiki");
        fs::create_dir_all(&wiki).unwrap();
        let pc = PathCheck::of(&[("game".to_string(), root)]);
        let ctx = Ctx { check: &pc, sources: &[], wiki: &wiki, drafted: &[] };
        let page = "# C\n\nLede.\n\n## Reload\n\nA person needs to rule on the reload command: the brief says `/reload`. `game:CLAUDE.md:3`\n";
        let mut asked = String::new();
        let mut generate = |p: &str| -> Result<String> {
            asked = p.to_string();
            Ok("[{\"id\": \"S1\", \"verdict\": \"supported\"}, {\"id\": \"T1\", \"verdict\": \"answered\", \"text\": \"The code registers `/sr_world reload`. `game:src/WorldCommand.java:2`\"}]".to_string())
        };
        let (out, done) = check("Reference/Config-Map.md", page, &HashSet::new(), &ctx, &mut generate).unwrap();
        assert!(asked.contains("T1 (evidence:") && asked.contains("A person needs to rule on the reload command"), "{asked}");
        assert!(out.contains("The code registers `/sr_world reload`.") && !out.contains("A person needs to rule"), "{out}");
        assert_eq!(done.changes.iter().map(|c| c.kind.as_str()).collect::<Vec<_>>(), vec!["answered"]);
    }

    /// A statement that something is gone gets Ken's search of the checkout,
    /// and a cited page link gets what that page says.
    #[test]
    fn a_gone_claim_gets_the_search_and_a_link_its_page() {
        let d = tempfile::tempdir().unwrap();
        let root = checkout(d.path(), "game", &[("src/Reset.java", "class Reset {\n  String NAME = \"reset_kill_stats\";\n}\n"), ("CLAUDE.md", "Kill Stats is now Chronicle.\n")]);
        let wiki = d.path().join("Wiki");
        fs::create_dir_all(wiki.join("Conventions")).unwrap();
        fs::write(wiki.join("Conventions/Registries.md"), "# Registries\n\nThe start-up order is in setup().\n").unwrap();
        let pc = PathCheck::of(&[("game".to_string(), root)]);
        let drafted = vec!["Conventions/Registries.md".to_string()];
        let ctx = Ctx { check: &pc, sources: &[], wiki: &wiki, drafted: &drafted };
        let page = "# V\n\nLede.\n\n## Renames\n\n- `kill_stats` is gone: no repo has it now. `game:CLAUDE.md:1`\n- The start-up order is on [[Registries]].\n";
        let mut asked = String::new();
        let mut generate = |p: &str| -> Result<String> {
            asked = p.to_string();
            Ok("[{\"id\": \"S1\", \"verdict\": \"unsupported\"}, {\"id\": \"S2\", \"verdict\": \"supported\"}]".to_string())
        };
        let (out, _) = check("Reference/Vocabulary.md", page, &HashSet::new(), &ctx, &mut generate).unwrap();
        assert!(asked.contains("Ken's search of the code for `kill_stats`") && asked.contains("game:src/Reset.java:2:"), "{asked}");
        assert!(asked.contains("[[Registries]] (Conventions/Registries.md)") && asked.contains("this page owns: the registries"), "{asked}");
        assert!(!out.contains("is gone") && out.contains("on [[Registries]]"), "{out}");
    }

    #[test]
    fn an_emptied_table_and_section_go_with_their_links() {
        let page = "---\nt: x\n---\n# X\n\nLede.\n\n[[#A]] · [[#B]]\n\n## A\n\n| k | v |\n|---|---|\n\n## B\n\nKept `g:x:1`.\n";
        assert_eq!(tidy(page), "---\nt: x\n---\n# X\n\nLede.\n\n[[#B]]\n\n## B\n\nKept `g:x:1`.\n");
    }
}

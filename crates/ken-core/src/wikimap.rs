//! Map an existing wiki into the knowledge base's sections (knowledge-core
//! decision 33, with 7 and 36).
//!
//! Set-up's second step can overlay Ken's structure on a wiki a team already
//! keeps. Its pages then sit wherever the team put them; this module places
//! each in a section (Rules, Decisions, Design, Current, Reference,
//! Research) without a model, by the folders it sits in and the words of
//! its name, and shows the moves as a dry run ([`plan`]). Nothing moves
//! until a person confirms ([`apply`]); nothing is ever deleted. Applying
//! moves the pages, rewrites every path link whose target is certain and
//! reports the rest, adds each moved page to its section's index, writes a
//! `log.md` line and makes one commit of exactly those files.
//!
//! A name link (`[[Page]]`) finds its page by file name wherever it is, so
//! a move does not break it; only path links (`[text](a/b.md)`) are
//! rewritten.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::pagemeta::Section;
use crate::{Error, Result};

/// One page to move, and why it goes there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Move {
    pub from: String,
    pub to: String,
    pub section: String,
    pub why: String,
}

/// The dry run: the moves, and the pages left where they are with why.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub moves: Vec<Move>,
    pub left: Vec<(String, String)>,
}

/// What applying did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Applied {
    pub moved: Vec<Move>,
    /// Pages whose path links were rewritten.
    pub rewritten: Vec<String>,
    /// Path links whose target Ken could not be sure of, as `page: target`,
    /// left as they were.
    pub unresolved: Vec<String>,
    pub commit: Option<String>,
    /// Why there is no commit, when there is none.
    pub commit_error: Option<String>,
}

/// Words that place a page in a section, checked in this order: a page
/// about a decision is a decision even when it sits under `design/`.
const PLACES: &[(Section, &[&str])] = &[
    (Section::Decisions, &["decision", "decisions", "adr", "adrs", "ruling", "rulings"]),
    (
        Section::Rules,
        &["rule", "rules", "convention", "conventions", "guideline", "guidelines", "standard", "standards", "policy", "policies", "process", "processes", "ways-of-working", "conduct"],
    ),
    (Section::Design, &["design", "designs", "architecture", "arch", "spec", "specs", "rfc", "rfcs", "proposal", "proposals"]),
    (
        Section::Current,
        &["roadmap", "status", "project", "projects", "plan", "plans", "milestone", "milestones", "release", "releases", "current", "backlog"],
    ),
    (
        Section::Research,
        &[
            "research", "notes", "meeting", "meetings", "minutes", "findings", "interview", "interviews", "retro", "retros", "retrospective",
            "journal", "spike", "spikes", "experiment", "experiments",
        ],
    ),
];

/// Pages at the wiki's top that stay where they are: the entry point, the
/// brief, the log, a README.
const STAYS: &[&str] = &["start-here.md", "claude.md", "agents.md", "log.md", "readme.md", "index.md"];

/// Folders a wiki never maps: Ken's own, the templates, the tickets, a
/// vault's settings.
const SKIPPED_DIRS: &[&str] = &["templates", "tickets", ".ken", ".git", ".obsidian", "node_modules"];

fn is_page(rel: &str) -> bool {
    let l = rel.to_ascii_lowercase();
    l.ends_with(".md") || l.ends_with(".markdown")
}

/// Every Markdown page under `root`, as forward-slash paths, sorted.
fn pages(root: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let path = e.path();
            if path.is_dir() {
                if name.starts_with('.') || SKIPPED_DIRS.contains(&name.to_ascii_lowercase().as_str()) {
                    continue;
                }
                walk(root, &path, out);
            } else if let Ok(rel) = path.strip_prefix(root) {
                let rel = rel.to_string_lossy().replace('\\', "/");
                if is_page(&rel) {
                    out.push(rel);
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

/// The words of a folder or file name: lower case, split at `-`, `_`,
/// spaces and dots. A hyphenated folder is also one word of its own.
fn words(name: &str) -> Vec<String> {
    let lower = name.to_ascii_lowercase();
    let mut out: Vec<String> = lower.split(['-', '_', ' ', '.']).filter(|w| !w.is_empty()).map(str::to_string).collect();
    out.push(lower.replace(['_', ' '], "-"));
    out
}

fn section_named(ws: &[String]) -> Option<Section> {
    PLACES.iter().find(|(_, names)| ws.iter().any(|w| names.contains(&w.as_str()))).map(|(s, _)| *s)
}

/// Where a page goes, and why: by the first folder that names a section
/// (the path below that folder is kept), else by the words of its file name,
/// else Reference. None when it stays where it is: already in a section, a
/// page at the top that stays, or not a page.
pub fn place(rel: &str) -> Option<Move> {
    if !is_page(rel) || crate::pagemeta::section_of(rel).is_some() {
        return None;
    }
    let parts: Vec<&str> = rel.split('/').collect();
    let (file, dirs) = parts.split_last()?;
    if dirs.is_empty() && STAYS.contains(&file.to_ascii_lowercase().as_str()) {
        return None;
    }
    if dirs.iter().any(|d| SKIPPED_DIRS.contains(&d.to_ascii_lowercase().as_str())) {
        return None;
    }
    let to = |section: Section, rest: &[&str], why: String| Move {
        from: rel.to_string(),
        to: format!("{}/{}", section.name(), rest.join("/")),
        section: section.name().to_string(),
        why,
    };
    for (i, d) in dirs.iter().enumerate() {
        if let Some(section) = section_named(&words(d)) {
            return Some(to(section, &parts[i + 1..], format!("it sits in {d}/")));
        }
    }
    let stem = file.rsplit_once('.').map_or(*file, |(s, _)| s);
    if let Some(section) = section_named(&words(stem)) {
        return Some(to(section, &parts, format!("its name says {}", section.name().to_lowercase())));
    }
    Some(to(Section::Reference, &parts, "nothing names another section".into()))
}

/// The dry run over the wiki at `root`: every page that is not in a section
/// placed in one. A page whose place is taken (a page there already, or two
/// pages bound for one place) is left where it is, with why.
pub fn plan(root: &Path) -> Plan {
    let all = pages(root);
    let mut plan = Plan::default();
    let mut taken: Vec<String> = Vec::new();
    for rel in &all {
        let Some(m) = place(rel) else { continue };
        let clash = root.join(&m.to).exists() || taken.iter().any(|t| t.eq_ignore_ascii_case(&m.to));
        if clash {
            plan.left.push((m.from.clone(), format!("{} is taken", m.to)));
            continue;
        }
        taken.push(m.to.clone());
        plan.moves.push(m);
    }
    plan
}

/// `target` relative to the folder of `from`, normalized.
fn join(from: &str, target: &str) -> String {
    let mut parts: Vec<&str> = from.split('/').collect();
    parts.pop();
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// The path from the page at `from` to `to`, both relative to the wiki.
fn relative(from: &str, to: &str) -> String {
    let from_dirs: Vec<&str> = {
        let mut p: Vec<&str> = from.split('/').collect();
        p.pop();
        p
    };
    let to_parts: Vec<&str> = to.split('/').collect();
    let shared = from_dirs.iter().zip(&to_parts).take_while(|(a, b)| a == b).count();
    let mut out: Vec<&str> = std::iter::repeat_n("..", from_dirs.len() - shared).collect();
    out.extend(&to_parts[shared..]);
    out.join("/")
}

/// Rewrite the path links of the page that was at `old` and is now at `new`
/// (the same when it did not move), given every move (`moved`: old path to
/// new) and whether a path was a file before the moves. A link is rewritten
/// when its target is certain: a file there before. Returns the new text
/// (None when nothing changed) and the links left alone because their
/// target was not there.
pub fn rewrite_links(old: &str, new: &str, text: &str, moved: &HashMap<String, String>, existed: &dyn Fn(&str) -> bool) -> (Option<String>, Vec<String>) {
    let mut out = String::with_capacity(text.len());
    let mut unresolved = Vec::new();
    let mut changed = false;
    let mut rest = text;
    while let Some(off) = rest.find("](") {
        let open = off + 2;
        out.push_str(&rest[..open]);
        let after = &rest[open..];
        let Some(close) = after.find(')') else {
            rest = after;
            break;
        };
        let inside = &after[..close];
        let (target, tail) = match inside.find([' ', '#']) {
            Some(i) => (&inside[..i], &inside[i..]),
            None => (inside, ""),
        };
        let bracketed = target.starts_with('<');
        let bare = target.trim_start_matches('<').trim_end_matches('>');
        let is_url = bare.contains("://") || bare.starts_with("mailto:") || bare.starts_with('/') || bare.is_empty();
        let mut written = inside.to_string();
        if !is_url {
            let decoded = bare.replace("%20", " ");
            let was = join(old, &decoded);
            let now = moved.get(&was).cloned().unwrap_or_else(|| was.clone());
            if old != new || now != was {
                if existed(&was) {
                    let mut rel = relative(new, &now);
                    if bare.contains("%20") {
                        rel = rel.replace(' ', "%20");
                    }
                    let rel = if bracketed { format!("<{rel}>") } else { rel };
                    if rel != target {
                        written = format!("{rel}{tail}");
                        changed = true;
                    }
                } else {
                    unresolved.push(format!("{new}: {bare}"));
                }
            }
        }
        out.push_str(&written);
        out.push(')');
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    (changed.then_some(out), unresolved)
}

/// The page's title for its index line: frontmatter `title`, else its first
/// heading, else empty.
fn title_of(text: &str) -> String {
    crate::pagemeta::parse(text)
        .and_then(|m| m.title)
        .or_else(|| text.lines().find_map(|l| l.strip_prefix("# ")).map(|t| t.trim().to_string()))
        .unwrap_or_default()
}

/// Apply the moves a person confirmed to the wiki at `root`: each page is
/// moved (never over a file, never deleted), path links are rewritten where
/// the target is certain, each moved page joins its section's index, one
/// `log.md` line is written and one commit made of exactly those files.
pub fn apply(root: &Path, moves: &[Move], today: &str) -> Result<Applied> {
    let before: std::collections::HashSet<String> = pages(root).into_iter().collect();
    let existed = |p: &str| before.contains(p) || root.join(p).is_file();
    let mut applied = Applied::default();
    let mut map: HashMap<String, String> = HashMap::new();
    for m in moves {
        let (from, to) = (root.join(&m.from), root.join(&m.to));
        if !from.is_file() || to.exists() {
            continue;
        }
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        fs::rename(&from, &to).map_err(|e| Error::io(&from, e))?;
        map.insert(m.from.clone(), m.to.clone());
        applied.moved.push(m.clone());
    }
    if applied.moved.is_empty() {
        return Ok(applied);
    }
    let back: HashMap<&str, &str> = map.iter().map(|(a, b)| (b.as_str(), a.as_str())).collect();
    let mut touched: Vec<String> = Vec::new();
    for now in pages(root) {
        let old = back.get(now.as_str()).map(|s| s.to_string()).unwrap_or_else(|| now.clone());
        let path = root.join(&now);
        let Ok(text) = fs::read_to_string(&path) else { continue };
        let (fixed, open) = rewrite_links(&old, &now, &text, &map, &existed);
        applied.unresolved.extend(open);
        if let Some(fixed) = fixed {
            fs::write(&path, fixed).map_err(|e| Error::io(&path, e))?;
            applied.rewritten.push(now.clone());
            touched.push(now);
        }
    }
    for m in &applied.moved {
        let text = fs::read_to_string(root.join(&m.to)).unwrap_or_default();
        if let Some(index) = crate::ingest::add_to_index(root, &m.to, &title_of(&text))? {
            touched.push(index);
        }
    }
    touched.push(crate::ingest::append_log(root, today, "map", &format!("{} pages moved into the sections", applied.moved.len()))?);
    let mut paths: Vec<String> = applied.moved.iter().flat_map(|m| [m.from.clone(), m.to.clone()]).collect();
    paths.extend(touched);
    let message = commit_message(&applied);
    match crate::git::commit_paths(root, &paths, &message) {
        Ok(sha) => applied.commit = sha,
        Err(e) => applied.commit_error = Some(e),
    }
    Ok(applied)
}

/// The mapping's commit message: what moved where, and the links left.
fn commit_message(a: &Applied) -> String {
    let mut msg = format!("map: {} pages moved into the sections\n\n", a.moved.len());
    for m in &a.moved {
        msg.push_str(&format!("- {} -> {}\n", m.from, m.to));
    }
    if !a.unresolved.is_empty() {
        msg.push_str(&format!("\nLinks left as they were ({}):\n", a.unresolved.len()));
        for u in a.unresolved.iter().take(30) {
            msg.push_str(&format!("- {u}\n"));
        }
    }
    msg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_are_placed_by_folder_then_name_then_reference() {
        let p = |rel: &str| place(rel).map(|m| (m.to, m.section));
        assert_eq!(p("designs/payments.md"), Some(("Design/payments.md".into(), "Design".into())));
        assert_eq!(p("docs/architecture/save/format.md"), Some(("Design/save/format.md".into(), "Design".into())));
        assert_eq!(p("docs/adr-012-retries.md"), Some(("Decisions/docs/adr-012-retries.md".into(), "Decisions".into())));
        assert_eq!(p("meeting-notes/2026-10-01.md"), Some(("Research/2026-10-01.md".into(), "Research".into())));
        assert_eq!(p("Coding Standards.md"), Some(("Rules/Coding Standards.md".into(), "Rules".into())));
        assert_eq!(p("guides/setup.md"), Some(("Reference/guides/setup.md".into(), "Reference".into())));
        assert_eq!(p("Roadmap.md"), Some(("Current/Roadmap.md".into(), "Current".into())));
        // Left where they are. A section's folder in any case is that
        // section already (`pagemeta::section_of`), and on a disk that
        // ignores case `design/` and `Design/` are one folder.
        for stays in ["START-HERE.md", "README.md", "Design/x.md", "design/x.md", "Rules/y.md", "Templates/Rule.md", "tickets/T-1.md", "logo.png"] {
            assert_eq!(place(stays), None, "{stays}");
        }
    }

    #[test]
    fn links_follow_the_pages_that_moved_and_the_rest_are_reported() {
        let moved: HashMap<String, String> =
            [("design/pay.md".to_string(), "Design/pay.md".to_string()), ("notes/a b.md".to_string(), "Research/a b.md".to_string())].into();
        let existed = |p: &str| ["design/pay.md", "notes/a b.md", "guide.md"].contains(&p);
        // A page that moved: its links to a page that stayed and one that moved.
        let text = "See [the guide](../guide.md#top), [pay](pay.md \"Pay\") and [web](https://x.y/z.md).";
        let (fixed, open) = rewrite_links("design/pay.md", "Design/pay.md", text, &moved, &existed);
        assert_eq!(fixed, None, "the same relative paths after the move: nothing to rewrite");
        assert!(open.is_empty());
        // A page that stayed, linking to pages that moved.
        let text = "[pay](design/pay.md) [notes](notes/a%20b.md) [gone](old/missing.md)";
        let (fixed, open) = rewrite_links("guide.md", "guide.md", text, &moved, &existed);
        assert_eq!(fixed.as_deref(), Some("[pay](Design/pay.md) [notes](Research/a%20b.md) [gone](old/missing.md)"));
        assert!(open.is_empty(), "a page that did not move keeps a link to a missing file as it was, unreported");
        // A page that moved with a link to a file that was never there.
        let (_, open) = rewrite_links("design/pay.md", "Design/pay.md", "[x](nowhere.md)", &moved, &existed);
        assert_eq!(open, vec!["Design/pay.md: nowhere.md"]);
        assert_eq!(relative("Design/pay.md", "guide.md"), "../guide.md");
        assert_eq!(relative("a/b/c.md", "a/d.md"), "../d.md");
    }

    #[test]
    fn the_dry_run_moves_nothing_and_apply_moves_links_indexes_and_logs() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        crate::wikinew::create(&root.join("w"), "Realms", &[], "2026-10-13").unwrap();
        let root = root.join("w");
        fs::create_dir_all(root.join("designs")).unwrap();
        fs::write(root.join("designs/payments.md"), "# Payments\n\nSee [the guide](../guide.md).\n").unwrap();
        fs::write(root.join("guide.md"), "# Guide\n\nRead [payments](designs/payments.md).\n").unwrap();
        // A page already where the dry run would put the guide.
        fs::create_dir_all(root.join("Reference")).unwrap();

        let plan = plan(&root);
        assert!(root.join("designs/payments.md").is_file(), "the dry run moves nothing");
        let tos: Vec<&str> = plan.moves.iter().map(|m| m.to.as_str()).collect();
        assert!(tos.contains(&"Design/payments.md") && tos.contains(&"Reference/guide.md"), "{tos:?}");

        let applied = apply(&root, &plan.moves, "2026-10-13").unwrap();
        assert_eq!(applied.moved.len(), 2);
        assert!(!root.join("designs/payments.md").exists() && root.join("Design/payments.md").is_file());
        assert_eq!(fs::read_to_string(root.join("Reference/guide.md")).unwrap(), "# Guide\n\nRead [payments](../Design/payments.md).\n");
        assert_eq!(fs::read_to_string(root.join("Design/payments.md")).unwrap(), "# Payments\n\nSee [the guide](../Reference/guide.md).\n");
        assert!(fs::read_to_string(root.join("Design/Index.md")).unwrap().contains("[[payments]]"));
        assert!(fs::read_to_string(root.join("log.md")).unwrap().contains("] map | 2 pages moved into the sections"));
        assert!(applied.commit.is_some() || applied.commit_error.is_some());
    }
}

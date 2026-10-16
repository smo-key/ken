//! The RAID register: the knowledge base's risks and issues, kept in one
//! page, `Current/RAID.md`, in one strict entry format like the decisions
//! log's:
//!
//! ```text
//! ## R-001 · 2026-10-14 · saves may corrupt on a crash
//!
//! status: open
//! owner: Ana
//! sources: [[2026-10-14-standup]]
//!
//! What might go wrong and what would show it.
//! ```
//!
//! `R-nnn` is a risk (something that might go wrong), `I-nnn` an issue (a
//! problem now). A risk is open, mitigated or closed; an issue open,
//! resolved or closed. The status and owner lines change in place; an entry
//! is never renumbered. Ingest proposes risks and issues as Inbox cards, and
//! an open question can be raised as an issue; each takes the next number
//! when a person adds it. The Log reads the register ([`crate::raidlog`]).

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Where the register sits in a knowledge base.
pub const RAID_PAGE: &str = "Current/RAID.md";

/// The bundled page, laid down by set-up, and written when an entry is added
/// to a knowledge base that has none yet.
const TEMPLATE: &str = include_str!("../templates/wiki/Current/RAID.md");

/// What the page is for, as its section index lists it.
pub const PURPOSE: &str = "the risks and issues, each with an owner and a status";

/// A risk or an issue.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Something that might go wrong.
    #[default]
    Risk,
    /// A problem now.
    Issue,
}

impl Kind {
    /// The id's prefix: `R` or `I`.
    pub fn prefix(self) -> &'static str {
        match self {
            Kind::Risk => "R",
            Kind::Issue => "I",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Risk => "risk",
            Kind::Issue => "issue",
        }
    }

    fn of_prefix(p: &str) -> Option<Kind> {
        match p {
            "R" => Some(Kind::Risk),
            "I" => Some(Kind::Issue),
            _ => None,
        }
    }
}

/// One entry of the register, as the page holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub kind: Kind,
    /// `R-001`, `I-004`.
    pub id: String,
    /// The day it was raised, `YYYY-MM-DD`.
    pub date: String,
    pub title: String,
    /// Lowercase; `open` when the entry says none.
    pub status: String,
    pub owner: Option<String>,
    pub sources: Vec<String>,
    /// What it is, in the entry's own words.
    pub text: String,
    /// The heading's line in the page (1-based).
    pub line: i64,
}

impl Entry {
    /// A closed entry folds away on the Log.
    pub fn closed(&self) -> bool {
        is_closed(&self.status)
    }
}

/// Whether a status closes an entry. Mitigated and resolved stay in view:
/// someone still watches them until they are closed.
pub fn is_closed(status: &str) -> bool {
    status.trim().eq_ignore_ascii_case("closed")
}

fn is_date(s: &str) -> bool {
    s.len() == 10 && s.as_bytes().iter().enumerate().all(|(i, b)| if i == 4 || i == 7 { *b == b'-' } else { b.is_ascii_digit() })
}

/// An entry's heading, `## R-001 · 2026-10-14 · title`: its kind, number,
/// date and title. Anything else is not an entry.
fn heading(line: &str) -> Option<(Kind, u32, String, String)> {
    let rest = line.strip_prefix("## ")?.trim();
    let mut parts = rest.splitn(3, " · ");
    let (id, date, title) = (parts.next()?.trim(), parts.next()?.trim(), parts.next()?.trim());
    let (prefix, number) = id.split_once('-')?;
    let kind = Kind::of_prefix(prefix)?;
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) || !is_date(date) || title.is_empty() {
        return None;
    }
    Some((kind, number.parse().ok()?, date.to_string(), title.to_string()))
}

/// The lines of `text` outside a fenced block, with their line numbers: the
/// format example in a fence is never an entry.
fn unfenced(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut fence = false;
    text.lines().enumerate().filter(move |(_, l)| {
        if l.trim_start().starts_with("```") {
            fence = !fence;
            return false;
        }
        !fence
    })
}

/// Every entry of the register page, in page order. The field lines
/// (`status:`, `owner:`, `sources:`) are read only before the entry's text
/// starts; a heading of level one or two ends the entry.
pub fn parse(text: &str) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    let mut current: Option<Entry> = None;
    let mut in_text = false;
    for (i, line) in unfenced(text) {
        let t = line.trim_end();
        if t.starts_with("# ") || t.starts_with("## ") {
            out.extend(current.take());
            in_text = false;
            if let Some((kind, n, date, title)) = heading(t) {
                current = Some(Entry {
                    kind,
                    id: format!("{}-{n:03}", kind.prefix()),
                    date,
                    title,
                    status: String::new(),
                    owner: None,
                    sources: Vec::new(),
                    text: String::new(),
                    line: i as i64 + 1,
                });
            }
            continue;
        }
        let Some(e) = current.as_mut() else { continue };
        let l = t.trim();
        if l.is_empty() {
            continue;
        }
        if !in_text {
            if let Some((key, value)) = l.split_once(':') {
                let value = value.trim();
                match key.trim().to_ascii_lowercase().as_str() {
                    "status" => {
                        e.status = value.to_lowercase();
                        continue;
                    }
                    "owner" => {
                        e.owner = (!value.is_empty()).then(|| value.to_string());
                        continue;
                    }
                    "sources" => {
                        e.sources = value.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect();
                        continue;
                    }
                    _ => {}
                }
            }
        }
        in_text = true;
        if !e.text.is_empty() {
            e.text.push(' ');
        }
        e.text.push_str(l);
    }
    out.extend(current);
    for e in &mut out {
        if e.status.is_empty() {
            e.status = "open".into();
        }
    }
    out
}

/// The next number for `kind`: one past the highest the page has used, a
/// heading in an older or broken form counted too, so no number comes back.
fn next_number(text: &str, kind: Kind) -> u32 {
    let prefix = format!("## {}-", kind.prefix());
    unfenced(text)
        .filter_map(|(_, l)| l.trim().strip_prefix(&prefix)?.split(|c: char| !c.is_ascii_digit()).next()?.parse::<u32>().ok())
        .max()
        .unwrap_or(0)
        + 1
}

/// A risk or issue waiting to be added: numbered only when it is, so two
/// added in any order never take the same number.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Draft {
    pub kind: Kind,
    /// A few words.
    pub title: String,
    /// Who watches it, when the source says.
    #[serde(default)]
    pub owner: Option<String>,
    /// What it is, in a line or two.
    #[serde(default)]
    pub text: String,
    /// The day it was raised.
    pub date: String,
    /// The note it was found in, by its file stem; empty for none.
    #[serde(default)]
    pub note: String,
}

/// The register page as a knowledge base with none would start it, dated.
pub fn template(date: &str) -> String {
    TEMPLATE.replace("{{date}}", date)
}

/// `page` with `updated:` moved to `date` when that is later than what it
/// says, so adding an older card never dates the page back.
fn stamp_if_later(page: &str, date: &str) -> String {
    let current = crate::pagemeta::parse(page).and_then(|m| m.updated).unwrap_or_default();
    if current.chars().take(10).collect::<String>().as_str() >= date {
        return page.to_string();
    }
    crate::ingest::stamp_updated(page, date)
}

/// The page with the draft added as its next entry, and the id it took.
/// An empty page starts from the template.
pub fn append(page: &str, d: &Draft) -> (String, String) {
    let page = if page.trim().is_empty() { template(&d.date) } else { page.to_string() };
    let id = format!("{}-{:03}", d.kind.prefix(), next_number(&page, d.kind));
    let one_line = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let title = one_line(&d.title);
    let title = title.trim_end_matches('.');
    let crlf = page.contains("\r\n");
    let mut out = page.replace("\r\n", "\n").trim_end().to_string();
    out.push_str(&format!("\n\n## {id} · {} · {title}\n\nstatus: open\n", d.date));
    match d.owner.as_deref().map(one_line).filter(|o| !o.is_empty()) {
        Some(o) => out.push_str(&format!("owner: {o}\n")),
        None => out.push_str("owner:\n"),
    }
    if !d.note.trim().is_empty() {
        out.push_str(&format!("sources: [[{}]]\n", d.note.trim()));
    }
    let text = one_line(&d.text);
    if !text.is_empty() {
        out.push_str(&format!("\n{text}\n"));
    }
    let out = stamp_if_later(&out, &d.date);
    (if crlf { out.replace('\n', "\r\n") } else { out }, id)
}

/// Add a draft to the knowledge base at `root`, writing the page (made from
/// the template when there is none, and added to its section index then).
/// Returns the id it took and the files written, for the commit.
pub fn add(root: &Path, d: &Draft) -> Result<(String, Vec<String>)> {
    let path = root.join(RAID_PAGE);
    let now = fs::read_to_string(&path).unwrap_or_default();
    let (text, id) = append(&now, d);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
    }
    fs::write(&path, text).map_err(|e| Error::io(&path, e))?;
    let mut written = vec![RAID_PAGE.to_string()];
    if let Some(index) = crate::ingest::add_to_index(root, RAID_PAGE, PURPOSE)? {
        written.push(index);
    }
    Ok((id, written))
}

/// Raise an open question an ingest left as a finding as an issue: one
/// entry, with the note it came from as its source, and one commit of the
/// register (decision 36: only Ken's own change, never pushed). Returns the
/// issue's id and the commit, when one was made.
pub fn raise_question(root: &Path, f: &crate::findings::Finding, date: &str) -> std::result::Result<(String, Option<String>), String> {
    if f.kind != "question" {
        return Err("only an open question is raised as an issue".into());
    }
    let note = if crate::chunker::is_markdown(&f.source) {
        f.source.rsplit('/').next().unwrap_or(&f.source).trim_end_matches(".md").to_string()
    } else {
        String::new()
    };
    let d = Draft {
        kind: Kind::Issue,
        title: crate::ingest::card_title(&f.text),
        owner: None,
        text: f.text.clone(),
        date: date.to_string(),
        note: note.clone(),
    };
    let (id, written) = add(root, &d).map_err(|e| e.to_string())?;
    let from = if note.is_empty() { String::new() } else { format!(" in {note}") };
    let message = format!("raid: raise {id} from an open question{from}\n\n{}", f.text.trim());
    let commit = crate::git::commit_paths(root, &written, &message)?;
    Ok((id, commit))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_template_is_a_register_with_no_entries() {
        assert!(parse(TEMPLATE).is_empty(), "the fenced example is not an entry");
        assert!(template("2026-10-14").contains("updated: 2026-10-14"));
        assert!(!template("2026-10-14").contains("{{"));
    }

    #[test]
    fn entries_read_in_the_one_format_and_nothing_else() {
        let page = "# RAID\n\n```\n## R-009 · 2026-01-01 · example\n```\n\n\
                    ## R-001 · 2026-10-01 · saves may corrupt\n\nstatus: Mitigated\nowner: Ana\nsources: [[standup]], game:src/save.rs\n\n\
                    A crash mid-write loses the region.\nstatus: this is text now.\n\n\
                    ## I-002 · 2026-10-03 · the build is red\n\nWhat broke.\n\n\
                    ## I-3 - 2026-10-04 - not the format\n\nIgnored.\n\n\
                    ## R-002 · someday · no date\n";
        let got = parse(page);
        assert_eq!(got.len(), 2, "{got:#?}");
        let r = &got[0];
        assert_eq!((r.kind, r.id.as_str(), r.date.as_str(), r.title.as_str()), (Kind::Risk, "R-001", "2026-10-01", "saves may corrupt"));
        assert_eq!((r.status.as_str(), r.owner.as_deref(), r.line), ("mitigated", Some("Ana"), 7));
        assert_eq!(r.sources, vec!["[[standup]]", "game:src/save.rs"]);
        assert_eq!(r.text, "A crash mid-write loses the region. status: this is text now.");
        assert!(!r.closed());
        let i = &got[1];
        assert_eq!((i.kind, i.id.as_str(), i.status.as_str(), i.owner.as_deref()), (Kind::Issue, "I-002", "open", None));
        assert!(is_closed("Closed") && !is_closed("resolved"));
    }

    #[test]
    fn an_added_entry_takes_the_next_number_of_its_kind() {
        let d = Draft { kind: Kind::Risk, title: "Saves may  corrupt.".into(), owner: Some("Ana".into()), text: "A crash\nmid-write.".into(), date: "2026-10-14".into(), note: "2026-10-14-standup".into() };
        // From nothing: the template, dated, then R-001.
        let (page, id) = append("", &d);
        assert_eq!(id, "R-001");
        assert!(page.starts_with("---\ntitle: \"RAID\"") && page.contains("updated: 2026-10-14"), "{page}");
        assert!(page.ends_with("## R-001 · 2026-10-14 · Saves may corrupt\n\nstatus: open\nowner: Ana\nsources: [[2026-10-14-standup]]\n\nA crash mid-write.\n"), "{page}");
        // A broken heading still holds its number; an issue counts apart.
        let page = format!("{page}\n## R-007 - broken\n");
        let (page, id) = append(&page, &Draft { owner: None, note: String::new(), text: String::new(), ..d.clone() });
        assert_eq!(id, "R-008");
        assert!(page.ends_with("## R-008 · 2026-10-14 · Saves may corrupt\n\nstatus: open\nowner:\n"), "{page}");
        let (page, id) = append(&page, &Draft { kind: Kind::Issue, date: "2026-10-01".into(), ..d });
        assert_eq!(id, "I-001");
        assert!(page.contains("updated: 2026-10-14"), "an older entry never dates the page back");
        assert_eq!(parse(&page).iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), vec!["R-001", "R-008", "I-001"]);
    }

    #[test]
    fn an_open_question_is_raised_as_an_issue_in_one_commit() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("Current")).unwrap();
        fs::write(root.join("Current/Index.md"), "# Current\n\n## Pages\n\n| page | holds |\n|---|---|\n| [[Project]] | the product |\n").unwrap();
        let git = |args: &[&str]| assert!(std::process::Command::new("git").args(args).current_dir(root).output().unwrap().status.success(), "{args:?}");
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "T"]);
        git(&["add", "-A"]);
        git(&["commit", "-qm", "init"]);
        let f = crate::findings::Finding {
            kind: "question".into(),
            source: "Research/Ingestion/Ingested/Meetings/2026-10/2026-10-14-standup.md".into(),
            text: "Who tells the players?".into(),
            ..Default::default()
        };
        let (id, commit) = raise_question(root, &f, "2026-10-14").unwrap();
        assert_eq!(id, "I-001");
        assert!(commit.is_some());
        let page = fs::read_to_string(root.join(RAID_PAGE)).unwrap();
        let e = parse(&page).pop().unwrap();
        assert_eq!((e.id.as_str(), e.title.as_str(), e.sources.as_slice()), ("I-001", "Who tells the players?", &["[[2026-10-14-standup]]".to_string()][..]));
        assert!(fs::read_to_string(root.join("Current/Index.md")).unwrap().contains("| [[RAID]] | "), "a new page joins its index");
        let out = std::process::Command::new("git").args(["show", "--stat", "--format=%s", "HEAD"]).current_dir(root).output().unwrap();
        let shown = String::from_utf8_lossy(&out.stdout);
        assert!(shown.starts_with("raid: raise I-001 from an open question in 2026-10-14-standup") && shown.contains("Current/RAID.md") && shown.contains("Current/Index.md"), "{shown}");
        let contradiction = crate::findings::Finding { kind: "contradiction".into(), ..f };
        assert!(raise_question(root, &contradiction, "2026-10-14").is_err());
    }
}

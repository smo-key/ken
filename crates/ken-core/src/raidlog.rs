//! The Log: the project's RAID register (risks, actions, issues, decisions)
//! and its milestones, not a feed of every update (knowledge-core decision
//! 37, Chris 2026-10-14). Read from what is already there, no model:
//!
//! - **Risks** and **Issues**: the RAID register, `Current/RAID.md`
//!   ([`crate::raid`]);
//! - **Actions**: the open tickets that have an assignee; a done or
//!   cancelled ticket leaves the list;
//! - **Decisions**: the decisions log's rulings, a superseded or refined one
//!   marked so;
//! - **Milestones**: tickets marked `type: milestone`, the rows of
//!   `Current/Roadmap.md`'s Upcoming and Reached tables, and each repo's
//!   tags as releases.
//!
//! Each item carries its status, owner and date, and where it comes from. A
//! closed item (a closed risk or issue, a superseded or refined ruling, a
//! dropped or cancelled milestone) is marked `closed` so the Log folds it
//! away. Ingests, drift, lint, findings, page changes and commits are left
//! out: they stay in the Inbox, Settings and `log.md`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::db::Db;
use crate::Result;

/// Most tags read from one repo.
const TAGS: usize = 50;
/// Longest title, in characters.
const TITLE_CHARS: usize = 240;
/// Where the roadmap sits in a knowledge base.
pub const ROADMAP_PAGE: &str = "Current/Roadmap.md";
/// The ticket `type` that makes a ticket a milestone.
pub const MILESTONE_TYPE: &str = "milestone";

/// One item of the register.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    /// `R-001`, a ticket id, `D-012`, a tag; empty for a roadmap row.
    pub id: String,
    pub title: String,
    /// `open`, `mitigated`, `in-progress`, `live`, `superseded by D-020`,
    /// `reached`, `released`…; a status a person writes is lowercased.
    pub status: String,
    pub owner: Option<String>,
    /// `YYYY-MM-DD`: raised, last updated, ruled, due or reached; empty when
    /// the source gives none.
    pub date: String,
    /// Folded away on the Log.
    pub closed: bool,
    /// What it is: `risk`, `issue`, `ticket`, `ruling`, `roadmap`, `release`.
    pub kind: String,
    /// The repo it is in, by name, for another repo's release; None for the
    /// knowledge base.
    pub repo: Option<String>,
    /// The file it comes from in the knowledge base, and the line.
    pub path: Option<String>,
    pub line: Option<i64>,
}

impl Item {
    fn new(kind: &str, id: &str, title: &str, status: &str, date: &str) -> Item {
        let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
        let title: String = if title.chars().count() > TITLE_CHARS {
            title.chars().take(TITLE_CHARS - 1).chain(std::iter::once('…')).collect()
        } else {
            title
        };
        Item { kind: kind.into(), id: id.into(), title, status: status.into(), date: date.into(), ..Default::default() }
    }

    fn at(mut self, path: &str, line: Option<i64>) -> Item {
        self.path = Some(path.to_string());
        self.line = line;
        self
    }
}

/// The register, group by group, in the order the Log shows them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Register {
    pub risks: Vec<Item>,
    pub actions: Vec<Item>,
    pub issues: Vec<Item>,
    pub decisions: Vec<Item>,
    pub milestones: Vec<Item>,
}

fn is_date(s: &str) -> bool {
    s.len() == 10 && s.as_bytes().iter().enumerate().all(|(i, b)| if i == 4 || i == 7 { *b == b'-' } else { b.is_ascii_digit() })
}

/// A ruling entry's words: its bold ruling, else its first line, without
/// its id.
fn ruling_text(id: &str, text: &str) -> String {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let ruling = lines
        .iter()
        .find(|l| l.starts_with("**") && !l.starts_with(&format!("**{id}")))
        .or_else(|| lines.first())
        .map(|l| l.trim_matches('*').trim().to_string())
        .unwrap_or_default();
    let ruling = ruling.trim_start_matches('#').trim();
    // An entry with no bold ruling keeps the rest of its first line.
    match ruling.strip_prefix(id) {
        Some(rest) => rest.trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '*' | ':' | '·' | '-' | '—')).to_string(),
        None => ruling.to_string(),
    }
}

/// Who made a ruling, from its quote line: `> "the words" (Ana)` or
/// `> Made by Ana.`
fn ruling_who(text: &str) -> Option<String> {
    let quote = text.lines().map(str::trim).find(|l| l.starts_with('>'))?;
    let q = quote.trim_start_matches('>').trim();
    let who = match q.strip_prefix("Made by ") {
        Some(w) => w.trim().trim_end_matches('.'),
        None => q.strip_suffix(')').and_then(|r| r.rsplit_once('(')).map(|(_, w)| w.trim())?,
    };
    (!who.is_empty()).then(|| who.to_string())
}

/// The cells of a Markdown table row, trimmed, or None for a line that is
/// not one.
fn cells(line: &str) -> Option<Vec<String>> {
    let t = line.trim();
    let inner = t.strip_prefix('|')?.strip_suffix('|')?;
    Some(inner.split('|').map(|c| c.trim().to_string()).collect())
}

/// The roadmap's milestones: each row of its Upcoming table (milestone,
/// delivers, target, state) and its Reached table (milestone, reached), as
/// the template lays them out. A row still holding a placeholder is not one;
/// a row that says "dropped" folds away.
pub fn roadmap(text: &str) -> Vec<Item> {
    let mut out = Vec::new();
    let mut section = String::new();
    let mut header = true;
    for (i, line) in text.lines().enumerate() {
        if let Some(h) = line.trim().strip_prefix("## ") {
            section = h.trim().to_lowercase();
            header = true;
            continue;
        }
        let Some(row) = cells(line) else {
            header = true;
            continue;
        };
        // The first row of a table is its header, the second its rule.
        if header || row.iter().all(|c| c.chars().all(|ch| matches!(ch, '-' | ':' | ' '))) {
            header = false;
            continue;
        }
        if row.iter().any(|c| c.contains("{{")) || row.first().is_none_or(|c| c.is_empty()) {
            continue;
        }
        let first_date = |c: &str| c.get(..10).filter(|d| is_date(d)).unwrap_or("").to_string();
        let item = match section.as_str() {
            "upcoming" => {
                let target = row.get(2).map(|c| first_date(c)).unwrap_or_default();
                let state = row.get(3).filter(|s| !s.is_empty()).map_or("planned".to_string(), |s| s.to_lowercase());
                let mut it = Item::new("roadmap", "", &row[0], &state, &target);
                it.closed = state.contains("dropped");
                it
            }
            "reached" => {
                let reached = row.get(1).cloned().unwrap_or_default();
                let dropped = reached.to_lowercase().contains("dropped");
                let mut it = Item::new("roadmap", "", &row[0], if dropped { "dropped" } else { "reached" }, &first_date(&reached));
                it.closed = dropped;
                it
            }
            _ => continue,
        };
        out.push(item.at(ROADMAP_PAGE, Some(i as i64 + 1)));
    }
    out
}

/// The register of the knowledge base `db` at `root`. `repos` are the
/// workspace's other repos (name and checkout), for their releases;
/// `offset_secs` is the local time's offset from UTC, so a ticket dated by
/// its file falls on the person's own day. Each group is newest first.
pub fn register(db: &Db, root: &Path, repos: &[(String, PathBuf)], offset_secs: i64) -> Result<Register> {
    let mut reg = Register::default();

    // Risks and issues, from the page on disk.
    let raid = std::fs::read_to_string(root.join(crate::raid::RAID_PAGE)).unwrap_or_default();
    for e in crate::raid::parse(&raid) {
        let mut it = Item::new(e.kind.as_str(), &e.id, &e.title, &e.status, &e.date).at(crate::raid::RAID_PAGE, Some(e.line));
        it.owner = e.owner.clone();
        it.closed = e.closed();
        match e.kind {
            crate::raid::Kind::Risk => reg.risks.push(it),
            crate::raid::Kind::Issue => reg.issues.push(it),
        }
    }

    // Actions, and the tickets that mark a milestone.
    for path in db.page_paths()?.into_iter().filter(|p| crate::day::is_ticket_path(p)) {
        let text = db.get_text(&path)?.unwrap_or_default();
        let t = crate::day::parse_ticket(&path, &text);
        let milestone = t.kind.eq_ignore_ascii_case(MILESTONE_TYPE);
        if !milestone && (!t.open || t.assignees.is_empty()) {
            continue;
        }
        let updated = db.page_meta(&path)?.and_then(|m| m.updated).map(|u| u.chars().take(10).collect::<String>());
        let date = match updated.filter(|d| is_date(d)) {
            Some(d) => d,
            None => db.get_file(&path)?.map(|f| crate::day::date_of_epoch(f.mtime + offset_secs)).unwrap_or_default(),
        };
        let status = if t.state.is_empty() { "todo".to_string() } else { t.state.to_lowercase() };
        let mut it = Item::new("ticket", &t.id, &t.title, &status, &date).at(&path, None);
        it.owner = (!t.assignees.is_empty()).then(|| t.assignees.join(", "));
        if milestone {
            // Done is reached and stays in view; cancelled is dropped.
            it.closed = !t.open && !t.state.eq_ignore_ascii_case("done");
            reg.milestones.push(it);
        } else {
            reg.actions.push(it);
        }
    }

    // Decisions: every dated ruling, one the log marks superseded or
    // refined said so and folded.
    let roles = crate::authority::Roles::of_repo(root);
    for log in crate::supersede::log_paths(db, &roles)? {
        let entries = crate::supersede::entries(db, &log)?;
        let mut later: HashMap<String, (String, String)> = HashMap::new();
        for s in crate::supersede::marked(&log, &entries) {
            later.entry(s.earlier.clone()).or_insert((s.relation.clone(), s.later.clone()));
        }
        for e in entries.iter().filter(|e| is_date(&e.date)) {
            let (status, closed) = match later.get(&e.id) {
                Some((rel, by)) if rel == "refines" => (format!("refined by {by}"), true),
                Some((_, by)) => (format!("superseded by {by}"), true),
                None => ("live".to_string(), false),
            };
            let mut it = Item::new("ruling", &e.id, &ruling_text(&e.id, &e.text), &status, &e.date).at(&log, Some(e.line));
            it.owner = ruling_who(&e.text);
            it.closed = closed;
            reg.decisions.push(it);
        }
    }

    // Milestones: the roadmap's rows, then releases (the knowledge base's
    // tags and each repo's).
    if let Some(text) = db.get_text(ROADMAP_PAGE)? {
        reg.milestones.extend(roadmap(&text));
    }
    let own = (String::new(), root.to_path_buf());
    for (name, dir) in std::iter::once(&own).chain(repos.iter()) {
        for t in crate::git::tags(dir, TAGS).unwrap_or_default() {
            let title = if t.subject.is_empty() || t.subject == t.name { t.name.clone() } else { t.subject.clone() };
            let mut it = Item::new("release", &t.name, &title, "released", if is_date(&t.date) { &t.date } else { "" });
            it.repo = (!name.is_empty()).then(|| name.clone());
            reg.milestones.push(it);
        }
    }

    for group in [&mut reg.risks, &mut reg.actions, &mut reg.issues, &mut reg.decisions, &mut reg.milestones] {
        sort(group);
    }
    Ok(reg)
}

/// Newest first, by date (an undated item last), then by id's number,
/// highest first, so the later of two entries on one day leads.
pub fn sort(items: &mut [Item]) {
    let number = |id: &str| id.rsplit('-').next().and_then(|n| n.parse::<u64>().ok()).unwrap_or(0);
    items.sort_by(|a, b| b.date.cmp(&a.date).then_with(|| number(&b.id).cmp(&number(&a.id))));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Project;
    use std::fs;

    #[test]
    fn roadmap_rows_are_milestones_and_placeholders_are_not() {
        let text = "# Roadmap\n\n## Upcoming\n\n| milestone | delivers | target | state |\n|---|---|---|---|\n\
                    | Combat | fights | 2026-11-01 | at risk |\n| {{name}} | x | {{date}} | planned |\n| Trading | trade | soon | |\n\n\
                    ## Reached\n\n| milestone | reached |\n|---|---|\n| Saves | 2026-09-30 |\n| Guilds | dropped, D-020 |\n";
        let got = roadmap(text);
        let rows: Vec<(&str, &str, &str, bool)> = got.iter().map(|i| (i.title.as_str(), i.status.as_str(), i.date.as_str(), i.closed)).collect();
        assert_eq!(
            rows,
            vec![
                ("Combat", "at risk", "2026-11-01", false),
                ("Trading", "planned", "", false),
                ("Saves", "reached", "2026-09-30", false),
                ("Guilds", "dropped", "", true),
            ]
        );
        assert_eq!((got[0].path.as_deref(), got[0].line), (Some(ROADMAP_PAGE), Some(7)));
        assert!(roadmap(include_str!("../templates/wiki/Templates/Roadmap.md")).is_empty(), "the blank has no milestones");
    }

    #[test]
    fn a_rulings_words_and_who_made_it_come_from_its_entry() {
        let entry = "## D-012 · 2026-10-02 · saves\n\n**Regions save apart.**\n\n> \"apart\" (Ana)\n\nWhy: size.\n";
        assert_eq!(ruling_text("D-012", entry), "Regions save apart.");
        assert_eq!(ruling_who(entry).as_deref(), Some("Ana"));
        assert_eq!(ruling_who("**x**\n\n> Made by Ben.\n").as_deref(), Some("Ben"));
        assert_eq!(ruling_who("**x**\n\n> \"just words\"\n"), None);
        assert_eq!(ruling_text("D-3", "**D-3** · 2026-01-01 · saves"), "2026-01-01 · saves");
    }

    /// The register, group by group: risks and issues from the RAID page,
    /// actions from open assigned tickets, decisions with a superseded one
    /// folded, milestones from a milestone ticket and the roadmap. What the
    /// old feed showed (log.md runs, findings, commits) is not in it.
    #[test]
    fn the_register_groups_risks_actions_issues_decisions_and_milestones() {
        let dir = tempfile::tempdir().unwrap();
        for (path, text) in [
            ("log.md", "# Log\n\n## [2026-10-01] ingest | Standup\n"),
            (
                "Decisions/DECISIONS.md",
                "# Decisions\n\n## D-001 · 2026-09-01 · saves\n\n**Worlds save whole.**\n\n\
                 ## D-002 · 2026-10-02 · saves\n\n**Regions save apart.**\n\n> \"apart\" (Ana)\n\nsupersedes: D-001\n",
            ),
            (
                "Current/RAID.md",
                "# RAID\n\n## R-001 · 2026-10-01 · Friday may slip\n\nstatus: open\nowner: Ben\n\n\
                 ## R-002 · 2026-10-03 · Saves may corrupt\n\nstatus: closed\n\n\
                 ## I-001 · 2026-10-04 · The build is red\n\nstatus: resolved\nowner: Ana\n",
            ),
            ("Current/Roadmap.md", "# Roadmap\n\n## Reached\n\n| milestone | reached |\n|---|---|\n| Saves | 2026-09-30 |\n"),
            ("tickets/SR-101.md", "---\nid: SR-101\nstatus: in-progress\nassignee: Ben\nupdated: 2026-10-04\n---\n# Fix saves\n"),
            ("tickets/SR-102.md", "---\nid: SR-102\nstatus: todo\nupdated: 2026-10-05\n---\n# Nobody's yet\n"),
            ("tickets/SR-103.md", "---\nid: SR-103\nstatus: done\nassignee: Ben\nupdated: 2026-10-06\n---\n# Done already\n"),
            ("tickets/SR-104.md", "---\nid: SR-104\nstatus: done\ntype: milestone\nupdated: 2026-10-07\n---\n# Alpha ships\n"),
        ] {
            let p = dir.path().join(path);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, text).unwrap();
        }
        let project = Project::create(dir.path(), "Lib").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        crate::scan::scan(&project, &mut db).unwrap();
        // A finding, which the register leaves out.
        let f = crate::findings::Finding { kind: "orphan".into(), source: "lint".into(), text: "Nothing links here.".into(), ..Default::default() };
        crate::findings::file(&mut db, &f, 1_791_201_600).unwrap();

        let reg = register(&db, dir.path(), &[], 0).unwrap();
        let ids = |g: &[Item]| g.iter().map(|i| (i.id.clone(), i.status.clone(), i.closed)).collect::<Vec<_>>();
        let row = |id: &str, status: &str, closed: bool| (id.to_string(), status.to_string(), closed);
        assert_eq!(ids(&reg.risks), vec![row("R-002", "closed", true), row("R-001", "open", false)]);
        assert_eq!(reg.risks[1].owner.as_deref(), Some("Ben"));
        assert_eq!((reg.risks[1].path.as_deref(), reg.risks[1].line), (Some(crate::raid::RAID_PAGE), Some(3)));
        assert_eq!(ids(&reg.issues), vec![row("I-001", "resolved", false)]);
        assert_eq!(ids(&reg.actions), vec![row("SR-101", "in-progress", false)], "open and assigned only");
        assert_eq!((reg.actions[0].owner.as_deref(), reg.actions[0].date.as_str()), (Some("Ben"), "2026-10-04"));
        assert_eq!(ids(&reg.decisions), vec![row("D-002", "live", false), row("D-001", "superseded by D-002", true)]);
        assert_eq!((reg.decisions[0].title.as_str(), reg.decisions[0].owner.as_deref()), ("Regions save apart.", Some("Ana")));
        assert_eq!(reg.decisions[0].path.as_deref(), Some("Decisions/DECISIONS.md"));
        let milestones: Vec<(&str, &str, &str)> = reg.milestones.iter().map(|i| (i.kind.as_str(), i.title.as_str(), i.status.as_str())).collect();
        assert_eq!(milestones, vec![("ticket", "Alpha ships", "done"), ("roadmap", "Saves", "reached")]);
        assert!(!reg.milestones[0].closed, "a reached milestone stays in view");
    }
}

//! How much a file's word counts in search: its authority (kb-ranking,
//! 2026-10-07). The method already says it (Ways of Working, docs-system):
//! rules and rulings rank ahead of evidence, and a closed ticket is a record
//! of what was done, not what holds. In Shattered Realms' wiki tickets were
//! 37% of the chunks and Research 30%, the decisions, Ways-of-Working,
//! Reference and people pages about 3%, and nothing told search a ruling
//! outweighs a closed ticket.
//!
//! Each indexed file gets a tier when it is indexed, from the section it sits
//! in and its frontmatter `status:`; the tiers that are not neutral are stored
//! (`file_authority`) and the within-member rerank adds the tier's weight to
//! a hit's score (`search::rerank_weighted`). A nudge on relevance, not a
//! wall: a record that answers still beats a rule that barely matches.
//!
//! * **Rule** ([`W_RULE`]): the decisions log, rules and Ways-of-Working
//!   pages, Platform pages, people pages; and, by less ([`W_CURRENT`]), any
//!   other page `status: current`.
//! * **Reference** (0): every other page, and every file that is not a
//!   Markdown page. All code is neutral, so code search does not move.
//! * **Record** ([`W_RECORD`]): tickets, Research and ingested notes (dated
//!   evidence), `status: mirror`, generated pages.
//! * **Closed** ([`W_CLOSED`]): a ticket that is done or cancelled, and a
//!   page retired or superseded.
//!
//! A team overrides them in its team file, `.wright/team.json`:
//!
//! ```json
//! "search": {
//!   "weights": {
//!     "Lore/**": "rule",
//!     "tickets/archive/": "closed",
//!     "Research/Findings/*.json": -2.0
//!   }
//! }
//! ```
//!
//! Each key is a gitignore-style pattern relative to the repo root; each
//! value a tier (`rule`, `reference`, `record`, `closed`) or a number, the
//! score it adds (negative lowers). An override applies to any indexed file,
//! code included, and wins over the defaults; where several patterns match,
//! the longest one wins.

use std::path::Path;

use crate::db::Db;
use crate::pagemeta::{PageMeta, Section};
use crate::Result;

/// What a rule or ruling adds to a hit's score: as much as a page's band
/// did, a little under one filename word.
pub const W_RULE: f64 = 0.75;
/// What a page kept current (`status: current`) adds: less than a ruling.
/// Measured 2026-10-07: at the full rule weight the reference pages kept
/// current (280 in Shattered Realms' wiki) rose above the code that answers
/// "which trigger-volume shapes does the engine support"; at this weight
/// the 45 code questions lost nothing and the wiki questions kept the gain.
pub const W_CURRENT: f64 = 0.2;
/// What a record (a ticket, a Research note) gives up.
pub const W_RECORD: f64 = 0.75;
/// What a closed ticket or a retired page gives up.
pub const W_CLOSED: f64 = 1.25;

/// A file's authority tier (see the module doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Rule,
    Reference,
    Record,
    Closed,
}

impl Tier {
    pub fn name(self) -> &'static str {
        match self {
            Tier::Rule => "rule",
            Tier::Reference => "reference",
            Tier::Record => "record",
            Tier::Closed => "closed",
        }
    }

    pub fn parse(s: &str) -> Option<Tier> {
        match s.trim().to_ascii_lowercase().as_str() {
            "rule" | "rules" | "ruling" => Some(Tier::Rule),
            "reference" | "neutral" => Some(Tier::Reference),
            "record" | "evidence" => Some(Tier::Record),
            "closed" | "retired" => Some(Tier::Closed),
            _ => None,
        }
    }

    /// What this tier adds to a hit's score.
    pub fn weight(self) -> f64 {
        match self {
            Tier::Rule => W_RULE,
            Tier::Reference => 0.0,
            Tier::Record => -W_RECORD,
            Tier::Closed => -W_CLOSED,
        }
    }
}

/// A file's stored authority: its tier, and the score it adds when a team
/// override gave a number rather than a tier.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Authority {
    pub tier: Tier,
    pub weight: f64,
}

impl Authority {
    fn of(tier: Tier) -> Authority {
        Authority { tier, weight: tier.weight() }
    }
}

/// Where a team keeps its kinds of page, from its team file: the decisions
/// log, the people and tickets folders. Defaults when the file does not say
/// (or there is none): a file named `DECISIONS.md`, a `people/` folder, and
/// whatever `contenttype` calls a ticket.
#[derive(Debug, Clone, Default)]
pub struct Roles {
    decisions: Option<String>,
    people: Option<String>,
    tickets: Option<String>,
    overrides: Vec<(String, Override)>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Override {
    Tier(Tier),
    Weight(f64),
}

impl Roles {
    /// The roles in `<root>/.wright/team.json`; the defaults when it is
    /// missing or unreadable.
    pub fn of_repo(root: &Path) -> Roles {
        std::fs::read_to_string(root.join(".wright").join("team.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .map(|v| Roles::from_team_json(&v))
            .unwrap_or_default()
    }

    pub fn from_team_json(v: &serde_json::Value) -> Roles {
        let path = |key: &str| v.get(key).and_then(|p| p.as_str()).map(|p| p.replace('\\', "/").trim_start_matches("./").to_string());
        let mut overrides = Vec::new();
        if let Some(weights) = v.pointer("/search/weights").and_then(|w| w.as_object()) {
            for (pattern, value) in weights {
                let o = match value {
                    serde_json::Value::String(s) => Tier::parse(s).map(Override::Tier),
                    serde_json::Value::Number(n) => n.as_f64().map(Override::Weight),
                    _ => None,
                };
                if let Some(o) = o {
                    overrides.push((pattern.clone(), o));
                }
            }
        }
        // The longest pattern is the most specific: it is tried first.
        overrides.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.0.cmp(&b.0)));
        Roles { decisions: path("decisions"), people: path("people"), tickets: path("tickets"), overrides }
    }

    /// Whether `rel_path` is the team's decisions log: the one the team
    /// file names, else any page called `DECISIONS.md`.
    pub fn is_decisions_log(&self, rel_path: &str) -> bool {
        let path = rel_path.replace('\\', "/").to_ascii_lowercase();
        match &self.decisions {
            Some(d) => path == d.to_ascii_lowercase(),
            None => path.rsplit('/').next() == Some("decisions.md"),
        }
    }

    /// Whether any override could reach a file that is not a page.
    pub fn has_overrides(&self) -> bool {
        !self.overrides.is_empty()
    }

    /// The authority of the file at `rel_path` with this frontmatter.
    pub fn authority(&self, rel_path: &str, meta: Option<&PageMeta>) -> Authority {
        let path = rel_path.replace('\\', "/");
        for (pattern, o) in &self.overrides {
            if glob_matches(pattern, &path) {
                return match *o {
                    Override::Tier(t) => Authority::of(t),
                    Override::Weight(w) => Authority { tier: if w > 0.0 { Tier::Rule } else if w < 0.0 { Tier::Record } else { Tier::Reference }, weight: w },
                };
            }
        }
        self.default_authority(&path, meta)
    }

    fn default_authority(&self, path: &str, meta: Option<&PageMeta>) -> Authority {
        if !crate::chunker::is_markdown(path) {
            return Authority::of(Tier::Reference);
        }
        let lower = path.to_ascii_lowercase();
        let dirs: Vec<&str> = lower.split('/').collect::<Vec<_>>().split_last().map(|(_, d)| d.to_vec()).unwrap_or_default();
        let name = lower.rsplit('/').next().unwrap_or(&lower);
        let under = |folder: &Option<String>, default: &str| match folder {
            Some(f) => {
                let f = f.trim_end_matches('/').to_ascii_lowercase();
                !f.is_empty() && lower.starts_with(&format!("{f}/"))
            }
            None => dirs.contains(&default),
        };
        let status = meta.and_then(|m| m.status.as_deref()).map(status_word).unwrap_or_default();
        if meta.is_some_and(|m| m.retired()) {
            return Authority::of(Tier::Closed);
        }
        let ticket = match &self.tickets {
            Some(_) => under(&self.tickets, "tickets"),
            None => crate::contenttype::of(path) == crate::contenttype::ContentType::Ticket,
        };
        if ticket {
            return Authority::of(if CLOSED_STATUSES.contains(&status.as_str()) { Tier::Closed } else { Tier::Record });
        }
        let section = crate::pagemeta::section_of(path);
        if section == Some(Section::Research) || dirs.iter().any(|d| matches!(*d, "ingested" | "ingestion")) {
            return Authority::of(Tier::Record);
        }
        if status == "mirror" || meta.is_some_and(|m| m.generated) {
            return Authority::of(Tier::Record);
        }
        let decisions_log = match &self.decisions {
            Some(d) => lower == d.to_ascii_lowercase(),
            None => name == "decisions.md",
        };
        if decisions_log
            || matches!(section, Some(Section::WaysOfWorking | Section::Platform))
            || dirs.contains(&"rules")
            || under(&self.people, "people")
        {
            return Authority::of(Tier::Rule);
        }
        if status == "current" {
            return Authority { tier: Tier::Rule, weight: W_CURRENT };
        }
        Authority::of(Tier::Reference)
    }
}

/// A ticket status that means the work is over.
const CLOSED_STATUSES: &[&str] = &["done", "cancelled", "canceled", "closed", "wontfix", "duplicate", "superseded", "rejected"];

/// The first word of a `status:` value, lowercased: `current` for "current,
/// kept by hand", `done` for "Done".
fn status_word(status: &str) -> String {
    status.trim().split(|c: char| !(c.is_alphanumeric() || c == '-')).next().unwrap_or("").to_ascii_lowercase()
}

/// Whether gitignore-style `pattern` matches `path` (or a folder it sits in).
fn glob_matches(pattern: &str, path: &str) -> bool {
    let mut builder = ignore::gitignore::GitignoreBuilder::new("");
    if builder.add_line(None, pattern).is_err() {
        return false;
    }
    builder.build().is_ok_and(|g| g.matched_path_or_any_parents(path, false).is_ignore())
}

// --- the label a hit carries -------------------------------------------------

/// The longest `status:` a label repeats, in characters.
const LABEL_STATUS_MAX: usize = 32;

/// How much a hit counts, as one short line for whoever reads the results
/// (kb-trust, 2026-10-07): the page's tier (`ruling`, `rule`, `current`,
/// `reference`, `record`), its `status:` and its date. An entry of a
/// decisions log carries its own id and date (`ruling D-410 · 2026-10-06`), a
/// ticket says so (`ticket · cancelled · 2026-07-21`). Claude answered from a
/// cancelled ticket over the roadmap page beside it, and from a ruling a later
/// one had settled, because nothing on the hit line said which was which.
///
/// None for a file that is not a Markdown page: all code is neutral, and its
/// kind tag already says what it is. `chunk_text` is the hit's chunk, when
/// the caller has it: it names the log entry the hit is on.
pub fn hit_label(db: &Db, rel_path: &str, chunk_text: Option<&str>) -> Result<Option<String>> {
    if !crate::chunker::is_markdown(rel_path) {
        return Ok(None);
    }
    let meta = db.page_meta(rel_path)?;
    let stored = db.authority_of(rel_path)?;
    let mut label = label_of(rel_path, meta.as_ref(), stored.as_ref().map(|(t, w)| (t.as_str(), *w)), chunk_text);
    // What the supersede pass found: a later ruling over this entry, or the
    // earlier ones this entry changes.
    let entry = chunk_text
        .and_then(crate::chunker::entry_line)
        .and_then(crate::chunker::entry_start)
        .map(|(id, _)| id)
        .filter(|_| label.starts_with("ruling "));
    if let Some(note) = entry.map(|id| crate::supersede::note(db, rel_path, &id)).transpose()?.flatten() {
        label.push_str(" · ");
        label.push_str(&note);
    }
    Ok(Some(label))
}

/// [`hit_label`] from what the index stored: the page's frontmatter and its
/// authority (tier name, weight), None when neutral.
pub fn label_of(rel_path: &str, meta: Option<&PageMeta>, stored: Option<(&str, f64)>, chunk_text: Option<&str>) -> String {
    let tier = stored.and_then(|(t, _)| Tier::parse(t)).unwrap_or(Tier::Reference);
    let weight = stored.map_or(0.0, |(_, w)| w);
    let name = rel_path.rsplit('/').next().unwrap_or(rel_path).to_ascii_lowercase();
    let entry = chunk_text.and_then(crate::chunker::entry_line);
    let ruling = tier == Tier::Rule && (entry.is_some() || name == "decisions.md");
    let ticket = matches!(tier, Tier::Record | Tier::Closed) && crate::contenttype::of(rel_path) == crate::contenttype::ContentType::Ticket;
    let word = if ruling {
        "ruling"
    } else if ticket {
        "ticket"
    } else {
        match tier {
            Tier::Rule if (weight - W_CURRENT).abs() < 1e-9 => "current",
            Tier::Rule => "rule",
            Tier::Reference => "reference",
            Tier::Record | Tier::Closed => "record",
        }
    };
    let mut parts: Vec<String> = Vec::new();
    match entry.filter(|_| ruling).and_then(crate::chunker::entry_start) {
        Some((id, _)) => parts.push(format!("{word} {id}")),
        None => parts.push(word.to_string()),
    }
    // A log entry is dated by its own head; the page's status is the log's.
    if let Some(line) = entry.filter(|_| ruling) {
        parts.extend(crate::chunker::entry_date(line));
        return parts.join(" · ");
    }
    let meta = meta.cloned().unwrap_or_default();
    if let Some(status) = meta.status.as_deref().map(status_clause).filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case(word)) {
        parts.push(status);
    }
    let date = meta
        .updated
        .clone()
        .or_else(|| meta.verified.as_ref().map(|v| format!("verified {v}")))
        .or_else(|| (tier == Tier::Record).then(|| crate::pagemeta::date_in_name(rel_path)).flatten());
    parts.extend(date);
    if meta.retired() && !meta.replaced_by.is_empty() {
        parts.push(format!("replaced by {}", meta.replaced_by.join(", ")));
    }
    if meta.generated {
        parts.push("generated".into());
    }
    parts.join(" · ")
}

/// A `status:` cut to its first clause, for a label: `ruled D-336` for
/// "ruled D-336 (Chris, 2026-10-01)", `reference` for "reference, kept
/// current — 2026-09-25".
fn status_clause(status: &str) -> String {
    let s = status.trim().trim_matches('"');
    let cut = s.find([',', ';', '(', '—', '–']).unwrap_or(s.len());
    let s = s[..cut].trim();
    let s = s.split(" - ").next().unwrap_or(s).trim();
    // An id keeps its capitals (`ruled D-336`) though the index stored the
    // status lowercased.
    let s = &s
        .split(' ')
        .map(|w| match w.split_once('-') {
            Some((k, n)) if !k.is_empty() && k.len() <= 8 && k.chars().all(|c| c.is_ascii_alphabetic()) && !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) => w.to_ascii_uppercase(),
            _ => w.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ");
    if s.chars().count() <= LABEL_STATUS_MAX {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(LABEL_STATUS_MAX).collect::<String>().trim_end())
    }
}

/// Work out and store the authority of the files in `db` (all of them, or
/// only `paths`), with the roles in the repo at `root`. Pages are read from
/// the index (their stored frontmatter), never from disk, so this is cheap
/// enough to run after every scan, and a team file's new override reaches
/// every file on the next one.
pub fn refresh(db: &Db, root: &Path, paths: Option<&[String]>) -> Result<()> {
    refresh_with(db, &Roles::of_repo(root), paths)
}

/// [`refresh`] with the roles already read.
pub fn refresh_with(db: &Db, roles: &Roles, paths: Option<&[String]>) -> Result<()> {
    let files: Vec<String> = match paths {
        Some(p) => p.to_vec(),
        None => db.authority_candidates(roles.has_overrides())?,
    };
    let mut rows: Vec<(String, Authority)> = Vec::new();
    for rel in &files {
        if paths.is_some() && !db.is_indexed(rel)? {
            continue;
        }
        let meta = if crate::chunker::is_markdown(rel) { db.page_meta(rel)? } else { None };
        let a = roles.authority(rel, meta.as_ref());
        if a.weight != 0.0 {
            rows.push((rel.clone(), a));
        }
    }
    db.set_authority(&rows, paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(status: &str) -> PageMeta {
        PageMeta { status: Some(status.into()), ..Default::default() }
    }

    #[test]
    fn rules_lead_records_follow_and_code_is_neutral() {
        let r = Roles::default();
        let tier = |p: &str, m: Option<PageMeta>| r.authority(p, m.as_ref()).tier;
        assert_eq!(tier("decisions/DECISIONS.md", None), Tier::Rule);
        assert_eq!(tier("Ways-of-Working/Lifecycle.md", None), Tier::Rule);
        assert_eq!(tier("Ways-of-Working/Rules/never-weaken-a-test.md", None), Tier::Rule);
        assert_eq!(tier("people/chris.md", None), Tier::Rule);
        assert_eq!(tier("Engine/Reference/World Events.md", Some(meta("current"))), Tier::Rule);
        assert_eq!(r.authority("Engine/Reference/World Events.md", Some(&meta("current"))).weight, W_CURRENT, "kept current, less than a ruling");
        assert_eq!(tier("Engine/Reference/Feature Status.md", Some(meta("reference, kept current"))), Tier::Reference);
        assert_eq!(tier("Design/Game/Design Law.md", None), Tier::Reference);
        assert_eq!(tier("tickets/SR-101.md", Some(meta("todo"))), Tier::Record);
        assert_eq!(tier("tickets/SR-102.md", Some(meta("Done"))), Tier::Closed);
        assert_eq!(tier("tickets/SR-103.md", Some(meta("cancelled"))), Tier::Closed);
        assert_eq!(tier("tickets/SR-104.md", Some(meta("current"))), Tier::Record, "a ticket is a record whatever it says");
        assert_eq!(tier("Research/Findings/spike.md", Some(meta("current"))), Tier::Record, "dated evidence");
        assert_eq!(tier("Inbox/Ingested/Standup - 2026-09-12.md", None), Tier::Record);
        assert_eq!(tier("Engine/Systems/Loot.md", Some(meta("mirror"))), Tier::Record);
        assert_eq!(tier("Ways-of-Working/Old.md", Some(meta("superseded"))), Tier::Closed, "retired beats binding");
        for code in ["src/main/java/Decisions.java", "tickets/gen.py", "people/Person.ts", "Research/data.json"] {
            assert_eq!(r.authority(code, None).weight, 0.0, "{code} is neutral");
        }
    }

    #[test]
    fn a_team_file_names_its_folders_and_overrides_by_pattern() {
        let team = serde_json::json!({
            "decisions": "log/RULINGS.md",
            "tickets": "work/",
            "people": "team/people/",
            "search": { "weights": {
                "Lore/**": "rule",
                "work/archive/": "closed",
                "Research/Findings/*.json": -2.0,
                "src/legacy/": "record",
                "Lore/drafts/": "reference",
                "bad": "loud"
            } }
        });
        let r = Roles::from_team_json(&team);
        let a = |p: &str, m: Option<PageMeta>| r.authority(p, m.as_ref());
        assert_eq!(a("log/RULINGS.md", None).tier, Tier::Rule);
        assert_eq!(a("decisions/DECISIONS.md", None).tier, Tier::Reference, "the team file named another log");
        assert_eq!(a("work/T-1.md", Some(meta("in-progress"))).tier, Tier::Record);
        assert_eq!(a("tickets/SR-1.md", None).tier, Tier::Reference, "not this team's tickets folder");
        assert_eq!(a("team/people/ana.md", None).tier, Tier::Rule);
        assert_eq!(a("Lore/Regions.md", None).tier, Tier::Rule);
        assert_eq!(a("Lore/drafts/Idea.md", None).tier, Tier::Reference, "the longer pattern wins");
        assert_eq!(a("work/archive/T-0.md", None).tier, Tier::Closed);
        assert_eq!(a("Research/Findings/lanes.json", None).weight, -2.0);
        assert_eq!(a("src/legacy/old.rs", None).tier, Tier::Record, "an override reaches code");
        assert_eq!(a("src/new.rs", None).weight, 0.0);
        assert!(r.has_overrides());
        assert!(!Roles::default().has_overrides());
    }

    #[test]
    fn a_label_says_what_a_hit_is_and_how_fresh() {
        let page = |status: Option<&str>, updated: Option<&str>| PageMeta {
            status: status.map(Into::into),
            updated: updated.map(Into::into),
            ..Default::default()
        };
        let entry = "### 2026-10-06\n\n**D-410** · 2026-10-06 · anchors, tools — **SPAWN ANCHORS ARE PLACED ONLY THROUGH THE TOOLS.** Chris.";
        assert_eq!(label_of("decisions/DECISIONS.md", None, Some(("rule", W_RULE)), Some(entry)), "ruling D-410 · 2026-10-06");
        assert_eq!(label_of("decisions/DECISIONS.md", None, Some(("rule", W_RULE)), Some("# DECISIONS\n\nThe permanent record.")), "ruling");
        assert_eq!(label_of("log/RULINGS.md", None, Some(("rule", W_RULE)), Some(entry)), "ruling D-410 · 2026-10-06", "a team's own log, by its entries");
        let cancelled = page(Some("cancelled"), Some("2026-07-21"));
        assert_eq!(label_of("tickets/SR-121.md", Some(&cancelled), Some(("closed", -W_CLOSED)), None), "ticket · cancelled · 2026-07-21");
        let roadmap = page(Some("ruled D-336 (Chris, 2026-10-01)"), Some("2026-10-01"));
        assert_eq!(label_of("Design/Game/ROADMAP-0.1.0.md", Some(&roadmap), None, None), "reference · ruled D-336 · 2026-10-01");
        let current = page(Some("current"), Some("2026-10-07"));
        assert_eq!(label_of("Current/Project.md", Some(&current), Some(("rule", W_CURRENT)), None), "current · 2026-10-07");
        let kept = page(Some("reference, kept current — 2026-09-25"), Some("2026-10-07"));
        assert_eq!(label_of("Engine/Reference/Feature Status.md", Some(&kept), None, None), "reference · 2026-10-07");
        assert_eq!(label_of("Ways-of-Working/Lifecycle.md", None, Some(("rule", W_RULE)), Some("Docs lanes push main.")), "rule");
        assert_eq!(label_of("Research/Ingested/Standup - 2026-09-12.md", None, Some(("record", -W_RECORD)), None), "record · 2026-09-12");
        let verified = PageMeta { verified: Some("2026-09-20".into()), ..Default::default() };
        assert_eq!(label_of("Design/Why.md", Some(&verified), None, None), "reference · verified 2026-09-20");
        let retired = PageMeta { status: Some("superseded".into()), replaced_by: vec!["[[Rules]]".into()], ..Default::default() };
        assert_eq!(label_of("Ways-of-Working/Old.md", Some(&retired), Some(("closed", -W_CLOSED)), None), "record · superseded · replaced by [[Rules]]");
        assert_eq!(status_clause("an extraordinarily long status that runs on and on"), "an extraordinarily long status t…");
        assert_eq!(status_clause("ruled d-336 (chris, 2026-10-01)"), "ruled D-336", "an id keeps its capitals");
        assert_eq!(status_clause("in-progress"), "in-progress");
    }

    #[test]
    fn hit_label_reads_the_index_and_skips_code() {
        let mut db = Db::open_in_memory().unwrap();
        for (p, kind) in [("tickets/SR-1.md", "md"), ("src/main.rs", "code")] {
            db.upsert_file(p, kind, 1, 1, "indexed", None, "x").unwrap();
        }
        db.set_page_meta("tickets/SR-1.md", Some(&PageMeta { status: Some("done".into()), ..Default::default() })).unwrap();
        refresh_with(&db, &Roles::default(), None).unwrap();
        assert_eq!(hit_label(&db, "tickets/SR-1.md", None).unwrap().as_deref(), Some("ticket · done"));
        assert_eq!(hit_label(&db, "src/main.rs", None).unwrap(), None);
    }

    #[test]
    fn refresh_stores_what_is_not_neutral_and_follows_the_index() {
        let mut db = Db::open_in_memory().unwrap();
        for (p, kind) in [("tickets/SR-1.md", "md"), ("decisions/DECISIONS.md", "md"), ("Design/Notes.md", "md"), ("src/main.rs", "code")] {
            db.upsert_file(p, kind, 1, 1, "indexed", None, "x").unwrap();
        }
        db.set_page_meta("tickets/SR-1.md", Some(&meta("done"))).unwrap();
        refresh_with(&db, &Roles::default(), None).unwrap();
        let w = db.authority_weights(&["tickets/SR-1.md".into(), "decisions/DECISIONS.md".into(), "Design/Notes.md".into(), "src/main.rs".into()]).unwrap().unwrap();
        assert_eq!(w.get("tickets/SR-1.md"), Some(&-W_CLOSED));
        assert_eq!(w.get("decisions/DECISIONS.md"), Some(&W_RULE));
        assert_eq!(w.get("Design/Notes.md"), None, "neutral is not stored");
        assert_eq!(w.get("src/main.rs"), None);
        // The ticket reopens: refreshing that one path moves it.
        db.set_page_meta("tickets/SR-1.md", Some(&meta("todo"))).unwrap();
        refresh_with(&db, &Roles::default(), Some(&["tickets/SR-1.md".to_string()])).unwrap();
        let w = db.authority_weights(&["tickets/SR-1.md".into(), "decisions/DECISIONS.md".into()]).unwrap().unwrap();
        assert_eq!(w.get("tickets/SR-1.md"), Some(&-W_RECORD));
        assert_eq!(w.get("decisions/DECISIONS.md"), Some(&W_RULE), "the others stay");
    }
}

//! How much a file's word counts in search: its tier (knowledge-core
//! decisions 23 and 24). Binding knowledge comes first, dated evidence and
//! what no longer holds last; code is neutral.
//!
//! Each indexed file gets a tier when it is indexed, from the section it sits
//! in and its frontmatter `status:`; the tiers that are not neutral are stored
//! (`file_authority`) and the within-member rerank adds the tier's weight to
//! a hit's score (`search::rerank_weighted`). A nudge on relevance, not a
//! wall: a record that answers still beats a binding page that barely
//! matches.
//!
//! * **Binding** ([`W_BINDING`]): the decisions log and `Rules/`.
//! * **Current** ([`W_CURRENT`]): Current and Design pages with
//!   `status: current`.
//! * **Reference** (0): every other page, and every file that is not a
//!   Markdown page. All code is neutral.
//! * **Record** ([`W_RECORD`]): tickets, open or done; notes and Research
//!   (dated evidence); drafts, generated and mirror pages.
//! * **Superseded** ([`W_SUPERSEDED`]): retired pages and cancelled tickets.
//!   A ruling a later one supersedes is labelled so on its hit.
//!
//! A team overrides the tier by path in `.ken/knowledge.json`:
//!
//! ```json
//! "search": {
//!   "weights": {
//!     "Lore/**": "binding",
//!     "tickets/archive/": "superseded",
//!     "Research/Findings/*.json": -2.0
//!   }
//! }
//! ```
//!
//! Each key is a gitignore-style pattern relative to the repo root; each
//! value a tier (`binding`, `current`, `reference`, `record`, `superseded`;
//! the older `rule` and `closed` still read) or a number, the score it adds
//! (negative lowers). An override applies to any indexed file, code included,
//! and wins over the defaults; where several patterns match, the longest one
//! wins.

use std::path::Path;

use crate::db::Db;
use crate::pagemeta::{PageMeta, Section};
use crate::Result;

/// What binding knowledge (a live ruling, a rule) adds to a hit's score: a
/// little under one filename word.
pub const W_BINDING: f64 = 0.75;
/// What a Current or Design page kept current adds.
pub const W_CURRENT: f64 = 0.4;
/// What a record (a ticket, a note, a Research page, a draft) gives up.
pub const W_RECORD: f64 = 0.5;
/// What a retired page or a cancelled ticket gives up.
pub const W_SUPERSEDED: f64 = 1.25;

/// A file's tier (see the module doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Binding,
    Current,
    Reference,
    Record,
    Superseded,
}

impl Tier {
    pub fn name(self) -> &'static str {
        match self {
            Tier::Binding => "binding",
            Tier::Current => "current",
            Tier::Reference => "reference",
            Tier::Record => "record",
            Tier::Superseded => "superseded",
        }
    }

    /// A tier by name, the older names included: `rule` is binding, `closed`
    /// is superseded.
    pub fn parse(s: &str) -> Option<Tier> {
        match s.trim().to_ascii_lowercase().as_str() {
            "binding" | "rule" | "rules" | "ruling" => Some(Tier::Binding),
            "current" => Some(Tier::Current),
            "reference" | "neutral" => Some(Tier::Reference),
            "record" | "evidence" => Some(Tier::Record),
            "superseded" | "closed" | "retired" => Some(Tier::Superseded),
            _ => None,
        }
    }

    /// What this tier adds to a hit's score.
    pub fn weight(self) -> f64 {
        match self {
            Tier::Binding => W_BINDING,
            Tier::Current => W_CURRENT,
            Tier::Reference => 0.0,
            Tier::Record => -W_RECORD,
            Tier::Superseded => -W_SUPERSEDED,
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

/// Where a knowledge base keeps its kinds of page, from its settings: the
/// decisions log and the tickets folder. Defaults when the file does not say
/// (or there is none): a file named `DECISIONS.md`, and whatever
/// `contenttype` calls a ticket.
#[derive(Debug, Clone, Default)]
pub struct Roles {
    decisions: Option<String>,
    tickets: Option<String>,
    overrides: Vec<(String, Override)>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Override {
    Tier(Tier),
    Weight(f64),
}

impl Roles {
    /// The roles in `<root>/.ken/knowledge.json` (or the older
    /// `.wright/team.json`); the defaults when neither is there or readable.
    pub fn of_repo(root: &Path) -> Roles {
        std::fs::read_to_string(root.join(crate::wikinew::KNOWLEDGE_FILE))
            .or_else(|_| std::fs::read_to_string(root.join(".wright").join("team.json")))
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .map(|v| Roles::from_settings(&v))
            .unwrap_or_default()
    }

    pub fn from_settings(v: &serde_json::Value) -> Roles {
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
        Roles { decisions: path("decisions"), tickets: path("tickets"), overrides }
    }

    /// Whether `rel_path` is the decisions log: the one the settings name,
    /// else any page called `DECISIONS.md`.
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
                    Override::Weight(w) => Authority { tier: if w > 0.0 { Tier::Binding } else if w < 0.0 { Tier::Record } else { Tier::Reference }, weight: w },
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
        let status = meta.and_then(|m| m.status.as_deref()).map(status_word).unwrap_or_default();
        if meta.is_some_and(|m| m.retired()) {
            return Authority::of(Tier::Superseded);
        }
        let ticket = match &self.tickets {
            Some(t) => {
                let t = t.trim_end_matches('/').to_ascii_lowercase();
                !t.is_empty() && lower.starts_with(&format!("{t}/"))
            }
            None => crate::contenttype::of(path) == crate::contenttype::ContentType::Ticket,
        };
        if ticket {
            return Authority::of(if CANCELLED_STATUSES.contains(&status.as_str()) { Tier::Superseded } else { Tier::Record });
        }
        let section = crate::pagemeta::section_of(path);
        if section == Some(Section::Research) || dirs.iter().any(|d| matches!(*d, "inbox" | "ingested" | "ingestion" | "notes")) {
            return Authority::of(Tier::Record);
        }
        if matches!(status.as_str(), "draft" | "mirror") || meta.is_some_and(|m| m.generated) {
            return Authority::of(Tier::Record);
        }
        if self.is_decisions_log(path) || matches!(section, Some(Section::Rules | Section::Decisions)) || dirs.contains(&"rules") {
            return Authority::of(Tier::Binding);
        }
        if status == "current" && matches!(section, Some(Section::Current | Section::Design)) {
            return Authority::of(Tier::Current);
        }
        Authority::of(Tier::Reference)
    }
}

/// A ticket status that means the work will not happen: the ticket no longer
/// holds. A ticket done or closed is a record of what was done.
const CANCELLED_STATUSES: &[&str] = &["cancelled", "canceled", "wontfix", "duplicate", "rejected", "superseded"];

/// The first word of a `status:` value, lowercased: `current` for "current,
/// kept by hand", `done` for "Done".
fn status_word(status: &str) -> String {
    status.trim().split(|c: char| !(c.is_alphanumeric() || c == '-')).next().unwrap_or("").to_ascii_lowercase()
}

/// Whether gitignore-style `pattern` matches `path` (or a folder it sits in).
pub(crate) fn glob_matches(pattern: &str, path: &str) -> bool {
    let mut builder = ignore::gitignore::GitignoreBuilder::new("");
    if builder.add_line(None, pattern).is_err() {
        return false;
    }
    builder.build().is_ok_and(|g| g.matched_path_or_any_parents(path, false).is_ignore())
}

// --- the label a hit carries -------------------------------------------------

/// What a hit is and how much it counts, as one short line (decision 17):
/// `kind · tier or role · date`. An entry of the decisions log is a decision
/// with its own id and date (`decision D-410 · binding · 2026-10-06`), and
/// one its entry marks `[SUPERSEDED BY …]` reads superseded; a cancelled
/// ticket reads `ticket · superseded · 2026-07-21`. Claude answered from a
/// cancelled ticket over the roadmap page beside it, and from a ruling a
/// later one had settled, because nothing on the hit line said which was
/// which. A file that is not a page says its role when it has one, else its
/// tier, and the day it last changed: `code · service · 2026-10-01`,
/// `config · reference · 2026-09-14` ([`file_label`]).
///
/// `chunk_text` is the hit's chunk, when the caller has it: it names the log
/// entry the hit is on. None for a path the index does not hold.
pub fn hit_label(db: &Db, rel_path: &str, chunk_text: Option<&str>) -> Result<Option<String>> {
    if !crate::chunker::is_markdown(rel_path) {
        let Some(file) = db.get_file(rel_path)? else {
            return Ok(None);
        };
        let stored = db.authority_of(rel_path)?;
        let role = db.file_role(rel_path)?;
        return Ok(Some(file_label(rel_path, role.as_deref(), stored.as_ref().map(|(t, w)| (t.as_str(), *w)), file.mtime)));
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
        .filter(|_| label.starts_with("decision "));
    if let Some(note) = entry.map(|id| crate::supersede::note(db, rel_path, &id)).transpose()?.flatten() {
        // A later ruling over this one: it no longer binds.
        if note.starts_with("superseded") {
            label = label.replacen(" · binding", " · superseded", 1);
        }
        label.push_str(" · ");
        label.push_str(&note);
    }
    Ok(Some(label))
}

/// [`hit_label`] from what the index stored: the page's frontmatter and its
/// authority (tier name, weight), None when neutral.
pub fn label_of(rel_path: &str, meta: Option<&PageMeta>, stored: Option<(&str, f64)>, chunk_text: Option<&str>) -> String {
    let tier = stored.and_then(|(t, _)| Tier::parse(t)).unwrap_or(Tier::Reference);
    // An entry of the decisions log is dated by its own head.
    if let Some(line) = chunk_text.and_then(crate::chunker::entry_line).filter(|_| tier == Tier::Binding) {
        let id = crate::chunker::entry_start(line).map(|(id, _)| id).unwrap_or_default();
        let stamped = chunk_text.is_some_and(|t| t.contains("[SUPERSEDED BY"));
        let mut parts = vec![format!("decision {id}"), if stamped { Tier::Superseded } else { tier }.name().to_string()];
        parts.extend(crate::chunker::entry_date(line));
        return parts.join(" · ");
    }
    let kind = crate::contenttype::of(rel_path).as_str();
    let meta = meta.cloned().unwrap_or_default();
    let date = meta
        .updated
        .clone()
        .or_else(|| meta.checked.as_ref().map(|c| c.date.clone()))
        .or_else(|| matches!(tier, Tier::Record | Tier::Superseded).then(|| crate::pagemeta::date_in_name(rel_path)).flatten());
    let mut parts = vec![kind.to_string(), tier.name().to_string()];
    parts.extend(date);
    // A retired page names its successor (decision 7).
    if !meta.superseded_by.is_empty() {
        let names: Vec<String> = meta.superseded_by.iter().map(|s| crate::pagemeta::successor_name(s)).collect();
        parts.push(format!("superseded by {}", names.join(", ")));
    }
    parts.join(" · ")
}

/// [`hit_label`] for a file that is not a page: its kind, its role (else its
/// tier) and the day it last changed on disk, from its stored mtime (UTC). A
/// file with no stored time (a sentinel the index uses to read it again)
/// has no date.
pub fn file_label(rel_path: &str, role: Option<&str>, stored: Option<(&str, f64)>, mtime: i64) -> String {
    let tier = stored.and_then(|(t, _)| Tier::parse(t)).unwrap_or(Tier::Reference);
    let middle = role.map(str::trim).filter(|r| !r.is_empty()).map(str::to_string).unwrap_or_else(|| tier.name().to_string());
    let mut parts = vec![crate::contenttype::of(rel_path).as_str().to_string(), middle];
    if mtime > 0 {
        parts.push(crate::day::date_of_epoch(mtime));
    }
    parts.join(" · ")
}

/// Work out and store the authority of the files in `db` (all of them, or
/// only `paths`), with the roles in the repo at `root`. Pages are read from
/// the index (their stored frontmatter), never from disk, so this is cheap
/// enough to run after every scan, and a new override in the settings
/// reaches every file on the next one.
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
    fn five_tiers_from_section_and_status() {
        let r = Roles::default();
        let tier = |p: &str, m: Option<PageMeta>| r.authority(p, m.as_ref()).tier;
        assert_eq!(tier("Decisions/DECISIONS.md", None), Tier::Binding);
        assert_eq!(tier("decisions/DECISIONS.md", None), Tier::Binding);
        assert_eq!(tier("Rules/never-weaken-a-test.md", None), Tier::Binding);
        assert_eq!(tier("Ways-of-Working/Lifecycle.md", None), Tier::Binding, "a legacy folder reads as Rules");
        assert_eq!(tier("people/chris.md", None), Tier::Reference, "no people role");

        assert_eq!(tier("Current/Project.md", Some(meta("current"))), Tier::Current);
        assert_eq!(tier("Design/Why regions.md", Some(meta("current, kept by hand"))), Tier::Current);
        assert_eq!(tier("Current/Project.md", None), Tier::Reference, "current only when it says so");
        assert_eq!(tier("Reference/World.md", Some(meta("current"))), Tier::Reference, "only Current and Design pages");

        assert_eq!(tier("tickets/SR-101.md", Some(meta("todo"))), Tier::Record);
        assert_eq!(tier("tickets/SR-102.md", Some(meta("Done"))), Tier::Record, "done is a record of what was done");
        assert_eq!(tier("tickets/SR-103.md", Some(meta("cancelled"))), Tier::Superseded);
        assert_eq!(tier("tickets/SR-104.md", Some(meta("wontfix"))), Tier::Superseded);
        assert_eq!(tier("tickets/SR-105.md", Some(meta("current"))), Tier::Record, "a ticket is a record whatever it says");
        assert_eq!(tier("Research/Findings/spike.md", Some(meta("current"))), Tier::Record, "dated evidence");
        assert_eq!(tier("Inbox/Ingested/Standup - 2026-09-12.md", None), Tier::Record, "a note");
        assert_eq!(tier("Reference/Loot.md", Some(meta("mirror"))), Tier::Record);
        assert_eq!(tier("Design/Idea.md", Some(meta("draft"))), Tier::Record);
        assert_eq!(tier("Rules/Old.md", Some(meta("superseded"))), Tier::Superseded, "retired beats binding");

        for code in ["src/main/java/Decisions.java", "tickets/gen.py", "Rules/check.ts", "Research/data.json"] {
            assert_eq!(r.authority(code, None).weight, 0.0, "{code} is neutral");
        }
        assert_eq!(r.authority("Rules/x.md", None).weight, W_BINDING);
        assert_eq!(r.authority("Current/Project.md", Some(&meta("current"))).weight, W_CURRENT);
        assert_eq!(r.authority("tickets/T-1.md", None).weight, -W_RECORD);
        assert_eq!(r.authority("tickets/T-1.md", Some(&meta("cancelled"))).weight, -W_SUPERSEDED);
    }

    #[test]
    fn the_settings_name_the_log_and_tickets_and_override_by_pattern() {
        let settings = serde_json::json!({
            "decisions": "log/RULINGS.md",
            "tickets": "work/",
            "search": { "weights": {
                "Lore/**": "binding",
                "work/archive/": "closed",
                "Research/Findings/*.json": -2.0,
                "src/legacy/": "record",
                "Lore/drafts/": "reference",
                "bad": "loud"
            } }
        });
        let r = Roles::from_settings(&settings);
        let a = |p: &str, m: Option<PageMeta>| r.authority(p, m.as_ref());
        assert_eq!(a("log/RULINGS.md", None).tier, Tier::Binding);
        assert_eq!(a("notes/DECISIONS.md", None).tier, Tier::Record, "the settings named another log; this one sits in notes");
        assert_eq!(a("work/T-1.md", Some(meta("in-progress"))).tier, Tier::Record);
        assert_eq!(a("tickets/SR-1.md", None).tier, Tier::Reference, "not this knowledge base's tickets folder");
        assert_eq!(a("Lore/Regions.md", None).tier, Tier::Binding);
        assert_eq!(a("Lore/drafts/Idea.md", None).tier, Tier::Reference, "the longer pattern wins");
        assert_eq!(a("work/archive/T-0.md", None).tier, Tier::Superseded, "the older name closed still reads");
        assert_eq!(a("Research/Findings/lanes.json", None).weight, -2.0);
        assert_eq!(a("src/legacy/old.rs", None).tier, Tier::Record, "an override reaches code");
        assert_eq!(a("src/new.rs", None).weight, 0.0);
        assert!(r.has_overrides());
        assert!(!Roles::default().has_overrides());
        assert_eq!(Tier::parse("rule"), Some(Tier::Binding));
        assert_eq!(Tier::parse("Superseded"), Some(Tier::Superseded));
    }

    #[test]
    fn a_label_says_kind_tier_and_date() {
        let page = |status: Option<&str>, updated: Option<&str>| PageMeta {
            status: status.map(Into::into),
            updated: updated.map(Into::into),
            ..Default::default()
        };
        let binding = Some(("binding", W_BINDING));
        let entry = "### 2026-10-06\n\n**D-410** · 2026-10-06 · anchors, tools — **SPAWN ANCHORS ARE PLACED ONLY THROUGH THE TOOLS.** Chris.";
        assert_eq!(label_of("Decisions/DECISIONS.md", None, binding, Some(entry)), "decision D-410 · binding · 2026-10-06");
        let stamped = "D-379 · 2026-10-05 · anchors — The server never overwrites. [SUPERSEDED BY D-410]";
        assert_eq!(label_of("Decisions/DECISIONS.md", None, binding, Some(stamped)), "decision D-379 · superseded · 2026-10-05");
        assert_eq!(label_of("Decisions/DECISIONS.md", None, binding, Some("# Decisions\n\nThe permanent record.")), "decision · binding");
        assert_eq!(label_of("log/RULINGS.md", None, Some(("rule", W_BINDING)), Some(entry)), "decision D-410 · binding · 2026-10-06", "a log by its entries, the older tier name");
        let cancelled = page(Some("cancelled"), Some("2026-07-21"));
        assert_eq!(label_of("tickets/SR-121.md", Some(&cancelled), Some(("superseded", -W_SUPERSEDED)), None), "ticket · superseded · 2026-07-21");
        let superseded = Some(("superseded", -W_SUPERSEDED));
        let retired = PageMeta { superseded_by: vec!["Design/new.md".into()], ..page(Some("superseded"), Some("2026-07-21")) };
        assert_eq!(label_of("Design/old.md", Some(&retired), superseded, None), "doc · superseded · 2026-07-21 · superseded by Design/new.md");
        let older = crate::pagemeta::parse("---\nstatus: superseded\nreplaced_by: \"[[Design/new]]\"\nupdated: 2026-07-21\n---\n").unwrap();
        assert_eq!(label_of("Design/old.md", Some(&older), superseded, None), "doc · superseded · 2026-07-21 · superseded by Design/new", "the older key");
        let current = page(Some("current"), Some("2026-10-07"));
        assert_eq!(label_of("Current/Project.md", Some(&current), Some(("current", W_CURRENT)), None), "doc · current · 2026-10-07");
        assert_eq!(label_of("Rules/Lifecycle.md", None, binding, Some("Docs lanes push main.")), "doc · binding");
        assert_eq!(label_of("Research/Ingested/Standup - 2026-09-12.md", None, Some(("record", -W_RECORD)), None), "note · record · 2026-09-12");
        assert_eq!(label_of("Decisions/Archive-2025.md", None, binding, None), "decision · binding", "the kind is read from the path");
        let checked = PageMeta { checked: crate::pagemeta::Checked::parse("2026-09-20 f00218c"), ..Default::default() };
        assert_eq!(label_of("Design/Why.md", Some(&checked), None, None), "doc · reference · 2026-09-20");
    }

    #[test]
    fn a_code_label_says_its_role_else_its_tier() {
        // 2026-10-01 12:00 UTC.
        let at = 1_790_856_000;
        assert_eq!(file_label("src/pay/PayService.java", Some("service"), None, at), "code · service · 2026-10-01");
        assert_eq!(file_label("src/main.rs", None, None, at), "code · reference · 2026-10-01");
        assert_eq!(file_label("src/legacy/old.rs", None, Some(("record", -W_RECORD)), at), "code · record · 2026-10-01");
        assert_eq!(file_label("config/app.yaml", None, None, -1), "config · reference", "no stored time, no date");
    }

    #[test]
    fn hit_label_reads_the_index_for_pages_and_code() {
        let mut db = Db::open_in_memory().unwrap();
        for (p, kind) in [("tickets/SR-1.md", "md"), ("src/services/pay.rs", "code")] {
            db.upsert_file(p, kind, 1, 1_790_856_000, "indexed", None, "x").unwrap();
        }
        db.set_page_meta("tickets/SR-1.md", Some(&PageMeta { status: Some("done".into()), ..Default::default() })).unwrap();
        refresh_with(&db, &Roles::default(), None).unwrap();
        crate::coderole::refresh_with(&db, &crate::coderole::RoleRules::default()).unwrap();
        assert_eq!(hit_label(&db, "tickets/SR-1.md", None).unwrap().as_deref(), Some("ticket · record"));
        assert_eq!(hit_label(&db, "src/services/pay.rs", None).unwrap().as_deref(), Some("code · service · 2026-10-01"));
        assert_eq!(hit_label(&db, "src/gone.rs", None).unwrap(), None);
    }

    #[test]
    fn refresh_stores_what_is_not_neutral_and_follows_the_index() {
        let mut db = Db::open_in_memory().unwrap();
        for (p, kind) in [("tickets/SR-1.md", "md"), ("Decisions/DECISIONS.md", "md"), ("Design/Notes.md", "md"), ("src/main.rs", "code")] {
            db.upsert_file(p, kind, 1, 1, "indexed", None, "x").unwrap();
        }
        db.set_page_meta("tickets/SR-1.md", Some(&meta("cancelled"))).unwrap();
        refresh_with(&db, &Roles::default(), None).unwrap();
        let w = db.authority_weights(&["tickets/SR-1.md".into(), "Decisions/DECISIONS.md".into(), "Design/Notes.md".into(), "src/main.rs".into()]).unwrap().unwrap();
        assert_eq!(w.get("tickets/SR-1.md"), Some(&-W_SUPERSEDED));
        assert_eq!(w.get("Decisions/DECISIONS.md"), Some(&W_BINDING));
        assert_eq!(w.get("Design/Notes.md"), None, "neutral is not stored");
        assert_eq!(w.get("src/main.rs"), None);
        // The ticket reopens: refreshing that one path moves it.
        db.set_page_meta("tickets/SR-1.md", Some(&meta("todo"))).unwrap();
        refresh_with(&db, &Roles::default(), Some(&["tickets/SR-1.md".to_string()])).unwrap();
        let w = db.authority_weights(&["tickets/SR-1.md".into(), "Decisions/DECISIONS.md".into()]).unwrap().unwrap();
        assert_eq!(w.get("tickets/SR-1.md"), Some(&-W_RECORD));
        assert_eq!(w.get("Decisions/DECISIONS.md"), Some(&W_BINDING), "the others stay");
    }
}

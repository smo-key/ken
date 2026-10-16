//! A change to one page that waits for a person: the page as Ken read it,
//! the page as Ken would write it, and what to append when the change is a
//! decisions-log entry or a RAID register entry. Ingest files one when it
//! holds an edit for a person, when a ruling waits as a ready entry, when it
//! drafts a ticket from an action, and when a risk or issue waits for the
//! register; Apply writes it only while the page still reads as Ken read it.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::db::Db;
use crate::{Error, Result};

/// Review kind of a proposed change to one page.
pub const PROPOSAL_KIND: &str = "page-proposal";

/// `Proposal::from` of a ruling a chat proposed through the MCP
/// `propose_decision` (knowledge-core decision 13): no note holds it.
pub const FROM_CHAT: &str = "chat";

/// A change to one page, held until a person applies it. `base` is the page
/// as Ken read it; the change applies only while the page still reads so.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Proposal {
    pub page: String,
    pub base: String,
    pub proposed: String,
    /// The repo the page is in, when not the one whose Review holds the
    /// card. Ingest no longer sets it: everything it proposes is in the
    /// knowledge base. Older proposals for a team repo still apply there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    /// A decisions-log entry to append, worked out against the log as it is
    /// when applied: rulings from one note then apply in any order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub append: Option<RulingEntry>,
    /// A risk or issue to add to the RAID register, numbered, like a ruling,
    /// against the register as it is when applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raid: Option<crate::raid::Draft>,
    /// The ingested note this came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

/// A ruling to append to a decisions log, as a ready entry in the one entry
/// format (knowledge-core decision 13). Its number is taken when it is added.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RulingEntry {
    /// The ruling, in one sentence.
    pub ruling: String,
    /// Who made it, as free text, when the note says. Nobody's confirm is
    /// waited for: one click adds the entry (there are no deciders, decision
    /// 35). An older proposal stored it as `decider`.
    #[serde(default, alias = "decider")]
    pub who: Option<String>,
    pub date: String,
    /// The note it was said in, by its file stem.
    pub note: String,
    /// A few words for what it is about.
    #[serde(default)]
    pub topic: String,
    #[serde(default)]
    pub why: String,
    /// The words it was made in.
    #[serde(default)]
    pub quote: String,
    /// Other words anyone might search for it by.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// The rulings it replaces (`D-003`).
    #[serde(default)]
    pub supersedes: Vec<String>,
}

impl Proposal {
    pub fn new(page: impl Into<String>, base: impl Into<String>, proposed: impl Into<String>) -> Proposal {
        Proposal { page: page.into(), base: base.into(), proposed: proposed.into(), ..Default::default() }
    }
}

/// Store one proposed page change, to apply or discard on its ingest card.
pub fn file_page_proposal(db: &mut Db, p: &Proposal, title: &str, body: &str, now: i64) -> Result<()> {
    let payload = serde_json::to_string(p).map_err(|e| Error::Other(e.to_string()))?;
    db.insert_review_item(PROPOSAL_KIND, title, body, &p.page, Some(&payload), now)?;
    Ok(())
}

/// What a proposal is, as its card names it: `ruling` (a ready entry for the
/// decisions log), `risk` or `issue` (an entry for the RAID register),
/// `ticket` (a drafted ticket), `new page` or `page` (an edit held for a
/// person).
pub fn kind_of(p: &Proposal) -> &'static str {
    if let Some(d) = &p.raid {
        d.kind.as_str()
    } else if p.append.is_some() {
        "ruling"
    } else if p.base.is_empty() && (p.page.starts_with("tickets/") || p.root.is_some()) {
        "ticket"
    } else if p.base.is_empty() {
        "new page"
    } else {
        "page"
    }
}

/// Why a proposal could not be applied.
#[derive(Debug, PartialEq)]
pub enum ApplyError {
    /// The page changed after Ken proposed; the proposal is out of date.
    Changed,
    Io(String),
}

/// Write a proposal, only while the page still reads as `base`: a change
/// made against an older page would undo whatever a person wrote since. A
/// ruling to append goes onto the log as it is now, whatever was added since,
/// and a risk or issue onto the register the same way.
/// `root` is the repo whose Review holds the card; `p.root` wins when set.
pub fn apply(root: &Path, p: &Proposal) -> std::result::Result<(), ApplyError> {
    let root = p.root.as_deref().map(Path::new).unwrap_or(root);
    let path = root.join(&p.page);
    let now = fs::read_to_string(&path).unwrap_or_default();
    if let Some(r) = &p.append {
        let log = if now.trim().is_empty() { "# Decisions\n".to_string() } else { now };
        let text = crate::ingest::decisions_entry(&log, r);
        return write_page(&path, &text).map_err(|e| ApplyError::Io(e.to_string()));
    }
    if let Some(d) = &p.raid {
        let (text, _) = crate::raid::append(&now, d);
        return write_page(&path, &text).map_err(|e| ApplyError::Io(e.to_string()));
    }
    if now.replace("\r\n", "\n") != p.base.replace("\r\n", "\n") {
        return Err(ApplyError::Changed);
    }
    write_page(&path, &p.proposed).map_err(|e| ApplyError::Io(e.to_string()))
}

fn write_page(path: &Path, text: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
    }
    fs::write(path, text).map_err(|e| Error::io(path, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decision 35: no deciders. Who made a ruling is free text, and a
    /// proposal stored with the older `decider` key still reads.
    #[test]
    fn who_made_a_ruling_reads_from_the_older_key() {
        let old: RulingEntry = serde_json::from_str(r#"{"ruling":"We ship saves.","decider":"Ana","date":"2026-09-24","note":"standup"}"#).unwrap();
        assert_eq!(old.who.as_deref(), Some("Ana"));
        let json = serde_json::to_string(&old).unwrap();
        assert!(json.contains(r#""who":"Ana""#) && !json.contains("decider"), "{json}");
    }

    #[test]
    fn apply_refuses_a_page_that_moved_on() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("P.md"), "now").unwrap();
        let p = Proposal::new("P.md", "then", "proposed");
        assert_eq!(apply(dir.path(), &p), Err(ApplyError::Changed));
        let p = Proposal::new("P.md", "now", "proposed");
        apply(dir.path(), &p).unwrap();
        assert_eq!(fs::read_to_string(dir.path().join("P.md")).unwrap(), "proposed");
    }

    #[test]
    fn a_proposal_says_what_it_is() {
        let ruling = Proposal { append: Some(RulingEntry::default()), ..Proposal::new("Decisions/DECISIONS.md", "# Decisions\n", "x") };
        assert_eq!(kind_of(&ruling), "ruling");
        assert_eq!(kind_of(&Proposal::new("tickets/SR-5.md", "", "x")), "ticket");
        assert_eq!(kind_of(&Proposal { root: Some("team".into()), ..Proposal::new("SR-5.md", "", "x") }), "ticket", "an older team-repo ticket");
        assert_eq!(kind_of(&Proposal::new("Rules/New.md", "", "x")), "new page");
        assert_eq!(kind_of(&Proposal::new("Current/Project.md", "before", "after")), "page");
        let issue = crate::raid::Draft { kind: crate::raid::Kind::Issue, ..Default::default() };
        assert_eq!(kind_of(&Proposal { raid: Some(issue), ..Proposal::new(crate::raid::RAID_PAGE, "", "x") }), "issue");
    }

    /// A risk waits like a ruling: numbered when it is added, against the
    /// register as it is then, whatever was added since.
    #[test]
    fn a_risk_takes_the_next_number_when_it_is_added() {
        let dir = tempfile::tempdir().unwrap();
        let d = crate::raid::Draft { title: "Saves may corrupt".into(), date: "2026-10-14".into(), ..Default::default() };
        let p = Proposal { raid: Some(d), ..Proposal::new(crate::raid::RAID_PAGE, "", "shown, not written") };
        apply(dir.path(), &p).unwrap();
        apply(dir.path(), &p).unwrap();
        let page = fs::read_to_string(dir.path().join(crate::raid::RAID_PAGE)).unwrap();
        let ids: Vec<String> = crate::raid::parse(&page).into_iter().map(|e| e.id).collect();
        assert_eq!(ids, vec!["R-001", "R-002"]);
    }
}

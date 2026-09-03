//! Workspace-level digest composition (ken-home-workspace 1.1).
//!
//! Home used to read one member's digest and present it as an overview.
//! This module rolls every member's *already-stored* digest for a given
//! local day together with the pipeline board summary into one structure.
//!
//! It **composes and never generates** (design D4). Per-project digest
//! generation — the ≥07:00 local gate, the in-flight guard, the quiet-day
//! fallback, the one-row-per-local-day contract — stays entirely owned by
//! `digest.rs` and its scheduler in `src-tauri`. Nothing here calls a
//! model, spawns a thread, reads a clock, or touches a database: the
//! caller supplies the rows and the date, which keeps the whole thing
//! fixture-testable.
//!
//! Member bodies are parsed with [`digest::parse_digest`], the same
//! tolerant splitter the per-project card uses, so a stored digest with a
//! mangled or missing `SOURCES:` line degrades to all-body/no-sources
//! here exactly as it does there.

use uuid::Uuid;

use crate::digest::{parse_digest, ParsedDigest};
use crate::pipeline::Digest as BoardDigest;

/// One member's stored digest for the requested day, as read by the
/// caller. `content` is the raw `digests.content` column — `None` when
/// that member has no row for the day.
#[derive(Debug, Clone, PartialEq)]
pub struct MemberDigestInput {
    pub project_id: Uuid,
    pub name: String,
    pub content: Option<String>,
}

/// A member that had a digest stored, parsed into body + sources.
#[derive(Debug, Clone, PartialEq)]
pub struct MemberDigestEntry {
    pub project_id: Uuid,
    pub name: String,
    pub body: String,
    /// Project-relative source paths, as parsed. May be empty.
    pub sources: Vec<String>,
}

/// A member with nothing stored for the day. Named rather than dropped —
/// silently omitting it is how Home came to lie about six of seven
/// projects in the first place.
#[derive(Debug, Clone, PartialEq)]
pub struct MemberAwaitingDigest {
    pub project_id: Uuid,
    pub name: String,
}

/// The composed workspace digest.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceDigest {
    /// The local calendar day these entries are for, `yyyy-mm-dd`.
    pub date: String,
    /// Members with a stored digest, in the order the caller supplied
    /// them — i.e. manifest order, which is stable and user-meaningful.
    /// Deliberately not re-sorted here.
    pub members: Vec<MemberDigestEntry>,
    /// Members with no digest stored for `date`, same ordering rule.
    pub awaiting: Vec<MemberAwaitingDigest>,
    /// The pipeline board summary, passed through unchanged.
    pub board: BoardDigest,
}

impl WorkspaceDigest {
    /// Whether there is anything worth rendering: any member digest, or
    /// any board group with entries. `awaiting` alone does not count —
    /// a workspace where nobody has written a digest yet has nothing to
    /// say, only something to explain.
    pub fn has_content(&self) -> bool {
        !self.members.is_empty() || board_has_content(&self.board)
    }
}

/// Whether a pipeline digest carries any entry in any of its groups.
fn board_has_content(board: &BoardDigest) -> bool {
    !board.awaiting_review.is_empty()
        || !board.newly_unblocked.is_empty()
        || !board.blocked.is_empty()
        || !board.moved_today.is_empty()
        || !board.new_ideas.is_empty()
        || !board.stale_runs.is_empty()
}

/// Compose the workspace digest for `date` from per-member stored rows
/// and the board summary.
///
/// Pure: no I/O, no clock, no AI call. A member whose stored content is
/// blank (or whitespace) is treated as *awaiting* rather than as an empty
/// digest — an empty body would render as a member that had nothing to
/// say, which is a different and wrong claim.
pub fn compose_workspace_digest(
    date: &str,
    members: &[MemberDigestInput],
    board: BoardDigest,
) -> WorkspaceDigest {
    let mut entries: Vec<MemberDigestEntry> = Vec::new();
    let mut awaiting: Vec<MemberAwaitingDigest> = Vec::new();

    for member in members {
        match member.content.as_deref().map(str::trim) {
            Some(raw) if !raw.is_empty() => {
                let ParsedDigest { body, sources } = parse_digest(raw);
                // `parse_digest` can still yield an empty body if the row
                // held nothing but a SOURCES line — that is not a digest.
                if body.trim().is_empty() {
                    awaiting.push(MemberAwaitingDigest {
                        project_id: member.project_id,
                        name: member.name.clone(),
                    });
                } else {
                    entries.push(MemberDigestEntry {
                        project_id: member.project_id,
                        name: member.name.clone(),
                        body,
                        sources,
                    });
                }
            }
            _ => awaiting.push(MemberAwaitingDigest {
                project_id: member.project_id,
                name: member.name.clone(),
            }),
        }
    }

    WorkspaceDigest {
        date: date.to_string(),
        members: entries,
        awaiting,
        board,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_board() -> BoardDigest {
        BoardDigest {
            awaiting_review: Vec::new(),
            newly_unblocked: Vec::new(),
            blocked: Vec::new(),
            moved_today: Vec::new(),
            new_ideas: Vec::new(),
            stale_runs: Vec::new(),
        }
    }

    fn member(name: &str, content: Option<&str>) -> MemberDigestInput {
        MemberDigestInput {
            project_id: Uuid::new_v4(),
            name: name.to_string(),
            content: content.map(str::to_string),
        }
    }

    #[test]
    fn mixed_workspace_splits_written_from_awaiting() {
        let members = vec![
            member("ken", Some("Busy day on the sync engine.\nSOURCES: src/sync.rs")),
            member("ShatteredRealms", None),
            member("ItemSearch", Some("Quiet — one config tweak.")),
        ];
        let out = compose_workspace_digest("2026-08-05", &members, empty_board());

        assert_eq!(out.date, "2026-08-05");
        assert_eq!(
            out.members.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            ["ken", "ItemSearch"],
            "written members keep caller (manifest) order"
        );
        assert_eq!(out.members[0].sources, ["src/sync.rs"]);
        assert!(out.members[1].sources.is_empty(), "no SOURCES line is fine");
        assert_eq!(
            out.awaiting.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            ["ShatteredRealms"]
        );
        assert!(out.has_content());
    }

    /// The whole point of `awaiting`: a member with nothing stored is
    /// named, not dropped.
    #[test]
    fn every_member_missing_is_all_awaiting_and_empty() {
        let members = vec![member("a", None), member("b", None)];
        let out = compose_workspace_digest("2026-08-05", &members, empty_board());

        assert!(out.members.is_empty());
        assert_eq!(out.awaiting.len(), 2);
        assert!(
            !out.has_content(),
            "nothing written and an empty board has nothing to render"
        );
    }

    #[test]
    fn empty_workspace_composes_to_nothing() {
        let out = compose_workspace_digest("2026-08-05", &[], empty_board());
        assert!(out.members.is_empty());
        assert!(out.awaiting.is_empty());
        assert!(!out.has_content());
    }

    /// A stored row that is blank, whitespace, or nothing but a SOURCES
    /// line is not a digest — rendering it would claim the member had
    /// nothing to report, which is a different statement from "not
    /// written yet".
    #[test]
    fn contentless_rows_count_as_awaiting_not_as_empty_digests() {
        let members = vec![
            member("blank", Some("")),
            member("spaces", Some("   \n  ")),
            member("sources-only", Some("SOURCES: a.rs, b.rs")),
        ];
        let out = compose_workspace_digest("2026-08-05", &members, empty_board());

        assert!(out.members.is_empty(), "none of these are digests");
        assert_eq!(out.awaiting.len(), 3);
    }

    /// A board with entries makes the digest worth rendering even when
    /// no member has written one.
    #[test]
    fn board_alone_is_content() {
        let mut board = empty_board();
        board.new_ideas.push(crate::pipeline::IdeaEntry {
            ticket_id: "T-1".into(),
            title: "Cache the route plan".into(),
            spawned_by: None,
        });
        let out = compose_workspace_digest("2026-08-05", &[member("a", None)], board);
        assert!(out.members.is_empty());
        assert!(out.has_content(), "the board carries the day");
    }

    /// The board is passed through untouched — this module summarizes
    /// nothing about it.
    #[test]
    fn board_passes_through_unchanged() {
        let mut board = empty_board();
        board.awaiting_review.push(crate::pipeline::AwaitingReviewEntry {
            ticket_id: "T-9".into(),
            title: "Sign off the migration".into(),
            updated: "2026-08-05".into(),
            run_count: 2,
        });
        let expected = board.clone();
        let out = compose_workspace_digest("2026-08-05", &[], board);
        assert_eq!(out.board, expected);
    }
}

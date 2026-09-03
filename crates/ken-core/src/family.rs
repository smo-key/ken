//! Family repos (`openspec/changes/ken-families`): the `family.json`
//! manifest, the strict write lanes that make merge conflicts impossible by
//! construction (D3), typed inbox items and the acceptance gate (D4), the
//! built-in tier rules for a family clone (D6), and the repo template (D2).
//!
//! Everything here is pure: path arithmetic, frontmatter parsing, text
//! composition, and one manifest read/write pair. No git, no `Db`, no
//! engine — `family_sync.rs` owns the transport and the sync state machine,
//! and `src-tauri` owns the clone directory, the poll timer, and the flag
//! gate.
//!
//! ## Reuse, don't fork
//!
//! Inbox items and accepted board tasks are *ken-tasks files*: they parse
//! and serialize through [`crate::tasks`]'s frontmatter patch core
//! ([`tasks::patch_text`], [`tasks::scalar_lines`], [`tasks::seq_lines`],
//! [`tasks::compose_log_entry`], [`tasks::slugify`]) rather than growing a
//! second frontmatter writer. That inherits S6's byte-fidelity contract for
//! free: patching an item's `status` rewrites the `status` and `updated`
//! lines and nothing else, so a teammate's Ken (or a future Ken with extra
//! keys) never loses data to our rewrite.
//!
//! ## The trust posture, in code
//!
//! Two things in this module are load-bearing for safety rather than
//! convenience:
//!
//! - [`lane_check`] is the enforcement of D3. Lanes protect against *bugs*,
//!   not adversaries (a malicious member already has push access), so a
//!   violation is reported as a bug — the commit is refused, never
//!   "fixed up".
//! - Nothing here accepts anything automatically. [`accept_task`] is a pure
//!   function a human action calls; there is no code path from "an item
//!   arrived" to "a task is on my board" (D4, locked: no auto-accept in
//!   v1).

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::kenignore::{Rule, Tier};
use crate::tasks;
use crate::{Error, Result};

/// The manifest file at the family repo root.
pub const MANIFEST_FILE: &str = "family.json";
/// Root folder holding one subfolder per member.
pub const MEMBERS_DIR: &str = "members";
/// Root folder holding the team's shared knowledge (full tier, D6).
pub const SHARED_DIR: &str = "shared";
/// Per-member folder: typed items addressed to that member.
pub const INBOX_SUBDIR: &str = "inbox";
/// Per-member folder: that member's tasks, visible to the team.
pub const BOARD_SUBDIR: &str = "board";
/// Per-member folder: that member's AI working area.
pub const WORKSPACE_SUBDIR: &str = "workspace";
/// The Ken behavior contract scaffolded into `shared/` (D3).
pub const CONVENTIONS_FILE: &str = "conventions.md";
/// Root README — the "no-human repo" sign on the door.
pub const README_FILE: &str = "README.md";
/// Placeholder that lets git track an otherwise empty member folder.
pub const GITKEEP_FILE: &str = ".gitkeep";

/// The newest repo-template schema version this build understands (D2:
/// "the template shape is entirely ours to define, so it evolves freely
/// behind this one number").
pub const SUPPORTED_TEMPLATE: u32 = 1;

// ---------------------------------------------------------------------
// 1.1 Manifest
// ---------------------------------------------------------------------

/// A manifest declaring a `template` version this build doesn't understand.
///
/// A typed error rather than an [`Error::Other`] string because the caller
/// has to *act* on it specifically: the connection goes unavailable ("needs
/// a newer Ken"), and — unlike every other manifest problem — no sync, no
/// ingest, and no write may run against the clone (spec: "newer template
/// halts, doesn't guess"). Squashing it into a message would leave that
/// decision to string matching.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnsupportedTemplate {
    /// What the manifest declared.
    pub found: u32,
    /// The newest version this build supports ([`SUPPORTED_TEMPLATE`]).
    pub supported: u32,
}

impl std::fmt::Display for UnsupportedTemplate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "this family repo uses template version {} and needs a newer Ken \
             (this one supports up to {})",
            self.found, self.supported
        )
    }
}

impl std::error::Error for UnsupportedTemplate {}

impl From<UnsupportedTemplate> for Error {
    fn from(e: UnsupportedTemplate) -> Self {
        Error::Other(e.to_string())
    }
}

fn default_template() -> u32 {
    1
}

/// One member of a family. `id` is a short stable slug (`"owner"`,
/// `"sarah"`), never an email (D2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FamilyMember {
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// Keys written by newer Kens or other capabilities round-trip
    /// untouched (same idiom as `WorkspaceConfig::extra`).
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl FamilyMember {
    /// A member with no extra keys. `id` is used verbatim — run it through
    /// [`normalize_member_id`] first if it came from a human.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        FamilyMember {
            id: id.into(),
            name: name.into(),
            extra: serde_json::Map::new(),
        }
    }
}

/// `family.json` (D2). Every field is `#[serde(default)]` so a manifest
/// written by a partially-different Ken still loads, and `extra` flattens
/// whatever we don't model so a rewrite never drops it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FamilyManifest {
    #[serde(default)]
    pub id: Uuid,
    #[serde(default)]
    pub name: String,
    /// Repo-template schema version, starting at 1. A manifest with no
    /// `template` key predates the field and is read as 1 rather than 0 —
    /// tolerance on the read side, never a downgrade of the version check.
    #[serde(default = "default_template")]
    pub template: u32,
    #[serde(default)]
    pub members: Vec<FamilyMember>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Default for FamilyManifest {
    fn default() -> Self {
        FamilyManifest {
            id: Uuid::nil(),
            name: String::new(),
            template: default_template(),
            members: Vec::new(),
            extra: serde_json::Map::new(),
        }
    }
}

impl FamilyManifest {
    /// Parse manifest JSON. Unknown keys survive in [`FamilyManifest::extra`];
    /// only genuinely unparsable JSON is an error.
    ///
    /// Deliberately does **not** run the version check — reading a manifest
    /// is how you *discover* it needs a newer Ken, so the check is a
    /// separate step ([`FamilyManifest::check_supported`]) the caller runs
    /// before it does anything with the clone.
    pub fn parse(raw: &str) -> Result<FamilyManifest> {
        serde_json::from_str(raw).map_err(|e| Error::Other(format!("invalid {MANIFEST_FILE}: {e}")))
    }

    /// Pretty JSON with a trailing newline — the exact bytes
    /// [`FamilyManifest::save`] writes, exposed separately so the scaffold
    /// (1.7) can produce a manifest without touching a filesystem.
    pub fn to_json(&self) -> Result<String> {
        let json = serde_json::to_string_pretty(self).map_err(|e| Error::Other(e.to_string()))?;
        Ok(json + "\n")
    }

    /// Read `<clone-root>/family.json`.
    pub fn load(clone_root: &Path) -> Result<FamilyManifest> {
        let path = manifest_path(clone_root);
        let raw = fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
        FamilyManifest::parse(&raw)
    }

    /// Write `<clone-root>/family.json` atomically (temp file + rename, the
    /// pattern `workspace::Workspace::save` established).
    pub fn save(&self, clone_root: &Path) -> Result<()> {
        let path = manifest_path(clone_root);
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        }
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, self.to_json()?).map_err(|e| Error::io(&tmp, e))?;
        fs::rename(&tmp, &path).map_err(|e| Error::io(&path, e))
    }

    /// `Ok(())` when this build can safely operate on the repo.
    pub fn check_supported(&self) -> std::result::Result<(), UnsupportedTemplate> {
        if self.template > SUPPORTED_TEMPLATE {
            return Err(UnsupportedTemplate {
                found: self.template,
                supported: SUPPORTED_TEMPLATE,
            });
        }
        Ok(())
    }

    pub fn member(&self, id: &str) -> Option<&FamilyMember> {
        self.members.iter().find(|m| m.id == id)
    }

    pub fn has_member(&self, id: &str) -> bool {
        self.member(id).is_some()
    }

    pub fn member_ids(&self) -> Vec<&str> {
        self.members.iter().map(|m| m.id.as_str()).collect()
    }

    /// The family's designated owner — the first member in the manifest
    /// (D3's default). The owner is the one member whose Ken writes
    /// `shared/`; everyone else proposes changes as inbox items.
    pub fn owner_id(&self) -> Option<&str> {
        self.members.first().map(|m| m.id.as_str())
    }

    /// Append one member (the join flow's single allowed out-of-lane
    /// write, D3 rule 3). Append-only by construction: existing entries are
    /// never touched and a duplicate id is refused.
    pub fn add_member(&mut self, member: FamilyMember) -> Result<()> {
        let id = normalize_member_id(&member.id)?;
        if id != member.id {
            return Err(Error::Other(format!(
                "member id '{}' is not a valid slug (try '{id}')",
                member.id
            )));
        }
        if self.has_member(&id) {
            return Err(Error::Other(format!("member '{id}' is already in this family")));
        }
        self.members.push(member);
        Ok(())
    }
}

/// `<clone-root>/family.json`.
pub fn manifest_path(clone_root: &Path) -> PathBuf {
    clone_root.join(MANIFEST_FILE)
}

/// `<clone-root>/shared/`.
pub fn shared_dir(clone_root: &Path) -> PathBuf {
    clone_root.join(SHARED_DIR)
}

/// `members/<id>` — repo-relative, forward slashes, the form
/// [`lane_check`] and git both speak.
pub fn member_rel(member_id: &str) -> String {
    format!("{MEMBERS_DIR}/{member_id}")
}

/// `members/<id>/inbox`.
pub fn inbox_rel(member_id: &str) -> String {
    format!("{MEMBERS_DIR}/{member_id}/{INBOX_SUBDIR}")
}

/// `members/<id>/board`.
pub fn board_rel(member_id: &str) -> String {
    format!("{MEMBERS_DIR}/{member_id}/{BOARD_SUBDIR}")
}

/// `members/<id>/workspace`.
pub fn workspace_rel(member_id: &str) -> String {
    format!("{MEMBERS_DIR}/{member_id}/{WORKSPACE_SUBDIR}")
}

pub fn member_dir(clone_root: &Path, member_id: &str) -> PathBuf {
    clone_root.join(MEMBERS_DIR).join(member_id)
}

pub fn inbox_dir(clone_root: &Path, member_id: &str) -> PathBuf {
    member_dir(clone_root, member_id).join(INBOX_SUBDIR)
}

/// `<clone-root>/members/<id>/board` — the third task home (ken-tasks
/// MODIFIED requirement); `archive/YYYY-MM/` goes underneath it, exactly
/// as `tasks::archive_dir` computes for the other two homes.
pub fn board_dir(clone_root: &Path, member_id: &str) -> PathBuf {
    member_dir(clone_root, member_id).join(BOARD_SUBDIR)
}

pub fn member_workspace_dir(clone_root: &Path, member_id: &str) -> PathBuf {
    member_dir(clone_root, member_id).join(WORKSPACE_SUBDIR)
}

/// Whether `id` is usable as a member id: a short lowercase slug of ASCII
/// alphanumerics and single inner hyphens.
///
/// This is a *path safety* check as much as a style one — member ids are
/// interpolated straight into repo paths, so anything that could contain a
/// separator, a drive letter, or a `..` must never get this far.
pub fn is_valid_member_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 40
        && !id.starts_with('-')
        && !id.ends_with('-')
        && !id.contains("--")
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Slugify a human name into a member id, or fail.
///
/// Deliberately not `tasks::slugify`: that one falls back to the literal
/// string `"task"` for an input with no alphanumerics, which would quietly
/// name a person "task" and — worse — let two unnamed members collide on
/// one id. A member id that can't be derived is an error the join flow
/// shows the user, not a default.
pub fn normalize_member_id(input: &str) -> Result<String> {
    let mut out = String::new();
    for c in input.chars() {
        if c.is_ascii_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let mut id = out.trim_matches('-').to_string();
    if id.len() > 40 {
        let cut = id[..40].rfind('-').unwrap_or(40);
        id.truncate(cut);
        id = id.trim_matches('-').to_string();
    }
    if !is_valid_member_id(&id) {
        return Err(Error::Other(format!(
            "'{input}' does not make a usable member id — pick a short name like 'sarah'"
        )));
    }
    Ok(id)
}

// ---------------------------------------------------------------------
// 1.1 Write lanes (D3, locked)
// ---------------------------------------------------------------------

/// Why a path may not be written. Every variant is a *bug report*: lanes
/// are enforced against Ken's own code paths, so a violation means some
/// caller computed a path it had no business computing (D3: "that's a bug,
/// not a user error").
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum LaneViolation {
    /// Absolute, empty, or containing `.`/`..` — never even considered
    /// against the rules, because a traversal component makes "which
    /// member's folder is this" unanswerable.
    UnsafePath { path: String },
    /// A modification (or deletion) of a file in another member's inbox.
    /// Rule 2 is create-only: once delivered, only the recipient's Ken
    /// touches an item.
    ForeignEdit { path: String, owner: String },
    /// Any write to another member's folder outside their `inbox/`.
    ForeignArea { path: String, owner: String },
    /// `shared/` is written by the family's owner member only; everyone
    /// else proposes changes as an inbox item (D3, v1).
    SharedNotOwner { path: String },
    /// A path in no lane at all: repo-root files other than the manifest,
    /// unknown top-level folders, a bare `members/` entry.
    OutsideLanes { path: String },
    /// `family.json` changed by something other than a member append.
    ManifestNotAppend { detail: String },
}

impl LaneViolation {
    pub fn path(&self) -> &str {
        match self {
            LaneViolation::UnsafePath { path }
            | LaneViolation::ForeignEdit { path, .. }
            | LaneViolation::ForeignArea { path, .. }
            | LaneViolation::SharedNotOwner { path }
            | LaneViolation::OutsideLanes { path } => path,
            LaneViolation::ManifestNotAppend { .. } => MANIFEST_FILE,
        }
    }
}

impl std::fmt::Display for LaneViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LaneViolation::UnsafePath { path } => {
                write!(f, "'{path}' is not a safe repo-relative path")
            }
            LaneViolation::ForeignEdit { path, owner } => write!(
                f,
                "'{path}' belongs to member '{owner}' — inbox delivery is create-only, \
                 never an edit"
            ),
            LaneViolation::ForeignArea { path, owner } => write!(
                f,
                "'{path}' is inside member '{owner}''s folder — only their inbox/ is writable"
            ),
            LaneViolation::SharedNotOwner { path } => write!(
                f,
                "'{path}' is shared knowledge — send a proposal to the family owner instead"
            ),
            LaneViolation::OutsideLanes { path } => {
                write!(f, "'{path}' is outside every write lane")
            }
            LaneViolation::ManifestNotAppend { detail } => {
                write!(f, "{MANIFEST_FILE} may only gain one member entry: {detail}")
            }
        }
    }
}

impl std::error::Error for LaneViolation {}

impl From<LaneViolation> for Error {
    fn from(v: LaneViolation) -> Self {
        Error::Other(format!("write lane violation (this is a bug): {v}"))
    }
}

/// Split a repo-relative path into components, rejecting anything that
/// could escape the repo or confuse ownership. Accepts both separators
/// defensively — git speaks `/`, but a path that reached us from a
/// `PathBuf` on Windows may not.
fn safe_components(path: &str) -> Option<Vec<&str>> {
    if path.is_empty() || path.starts_with('/') || path.starts_with('\\') {
        return None;
    }
    // A Windows drive-qualified path ("C:/x") is absolute, not relative.
    if path.len() >= 2 && path.as_bytes()[1] == b':' {
        return None;
    }
    let parts: Vec<&str> = path.split(['/', '\\']).collect();
    if parts
        .iter()
        .any(|p| p.is_empty() || *p == "." || *p == ".." || p.trim() != *p)
    {
        return None;
    }
    Some(parts)
}

/// D3 rules 1–3, exactly: may `member_id`'s Ken commit `path`?
///
/// - Rule 1: anything under `members/<member_id>/`.
/// - Rule 2: **new files only** under another member's `inbox/`.
/// - Rule 3: `family.json` (the join-flow member append; the *content*
///   half of rule 3 is [`manifest_append_only`], which a path alone can't
///   express).
///
/// Everything else — including `shared/` — is refused. `shared/` is a lane
/// too (D3), just not this member's: use [`Lane::owner`] for the family
/// owner's Ken, which is the single writer of shared knowledge in v1.
///
/// `is_new_file` must be ground truth from the working tree, not caller
/// intent; [`crate::family_sync::GitTransport::commit_paths`] derives it
/// from the transport for exactly that reason.
pub fn lane_check(
    member_id: &str,
    path: &str,
    is_new_file: bool,
) -> std::result::Result<(), LaneViolation> {
    let Some(parts) = safe_components(path) else {
        return Err(LaneViolation::UnsafePath { path: path.to_string() });
    };

    // Rule 3 (path half): the manifest.
    if parts.len() == 1 && parts[0] == MANIFEST_FILE {
        return Ok(());
    }

    if parts[0] == MEMBERS_DIR {
        // `members/` or `members/<id>` alone names a folder, not a file.
        if parts.len() < 3 {
            return Err(LaneViolation::OutsideLanes { path: path.to_string() });
        }
        let owner = parts[1];
        // Rule 1: my own folder, no restrictions.
        if owner == member_id {
            return Ok(());
        }
        // Rule 2: create-only delivery into a teammate's inbox.
        if parts[2] == INBOX_SUBDIR {
            if parts.len() < 4 {
                return Err(LaneViolation::OutsideLanes { path: path.to_string() });
            }
            return if is_new_file {
                Ok(())
            } else {
                Err(LaneViolation::ForeignEdit {
                    path: path.to_string(),
                    owner: owner.to_string(),
                })
            };
        }
        return Err(LaneViolation::ForeignArea {
            path: path.to_string(),
            owner: owner.to_string(),
        });
    }

    if parts[0] == SHARED_DIR {
        return Err(LaneViolation::SharedNotOwner { path: path.to_string() });
    }

    Err(LaneViolation::OutsideLanes { path: path.to_string() })
}

/// Which lane a write is being made in.
///
/// [`lane_check`] is the pure statement of D3 rules 1–3 and is what
/// `Lane::member` delegates to. Two situations D3 describes can't be
/// expressed by `(member_id, path, is_new)` alone, so they are separate
/// constructors rather than extra parameters on the rule function:
///
/// - **owner** — D3 makes `shared/` lane-governed, with the family's
///   designated owner (first manifest member) as its single writer;
///   everyone else's edits travel as proposal inbox items. Whether *I* am
///   the owner is a manifest fact, not a path fact.
/// - **bootstrap** — "Create family" commits the whole template (1.7) into
///   a freshly initialized, empty repo. There is no other member's data to
///   protect yet, and no file to overwrite; the lane therefore permits any
///   safe path but only as a *creation*, so a bootstrap lane can never be
///   reused to edit an existing repo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lane<'a> {
    member_id: &'a str,
    shared_writer: bool,
    bootstrap: bool,
}

impl<'a> Lane<'a> {
    /// The ordinary lane: D3 rules 1–3 for this member.
    pub fn member(member_id: &'a str) -> Self {
        Lane { member_id, shared_writer: false, bootstrap: false }
    }

    /// This member plus `shared/**` — use only when `member_id ==
    /// manifest.owner_id()`.
    pub fn owner(member_id: &'a str) -> Self {
        Lane { member_id, shared_writer: true, bootstrap: false }
    }

    /// The one-shot lane for the initial template commit into an empty
    /// repo. New files only.
    pub fn bootstrap() -> Self {
        Lane { member_id: "", shared_writer: false, bootstrap: true }
    }

    /// Convenience: `owner` when this member owns the family, `member`
    /// otherwise.
    pub fn for_manifest(member_id: &'a str, manifest: &FamilyManifest) -> Self {
        if manifest.owner_id() == Some(member_id) {
            Lane::owner(member_id)
        } else {
            Lane::member(member_id)
        }
    }

    pub fn member_id(&self) -> &str {
        self.member_id
    }

    pub fn check(&self, path: &str, is_new_file: bool) -> std::result::Result<(), LaneViolation> {
        if self.bootstrap {
            let Some(_) = safe_components(path) else {
                return Err(LaneViolation::UnsafePath { path: path.to_string() });
            };
            return if is_new_file {
                Ok(())
            } else {
                Err(LaneViolation::OutsideLanes { path: path.to_string() })
            };
        }
        match lane_check(self.member_id, path, is_new_file) {
            Err(LaneViolation::SharedNotOwner { path: p }) if self.shared_writer => {
                // `shared/` alone is a folder, not a file.
                match safe_components(&p) {
                    Some(parts) if parts.len() >= 2 => Ok(()),
                    _ => Err(LaneViolation::OutsideLanes { path: p }),
                }
            }
            other => other,
        }
    }
}

/// The content half of D3 rule 3: `family.json` may only ever *gain*
/// member entries. `id`, `name`, `template`, `extra`, and every existing
/// member entry must be byte-identical across the edit.
///
/// A path-level lane check can't see this — `family.json` is one path
/// whether you appended a member or rewrote the whole team — so the join
/// flow runs this before it commits the manifest.
pub fn manifest_append_only(
    before: &FamilyManifest,
    after: &FamilyManifest,
) -> std::result::Result<(), LaneViolation> {
    if before.id != after.id
        || before.name != after.name
        || before.template != after.template
        || before.extra != after.extra
    {
        return Err(LaneViolation::ManifestNotAppend {
            detail: "family id, name, template, or extra keys changed".into(),
        });
    }
    if after.members.len() < before.members.len() {
        return Err(LaneViolation::ManifestNotAppend {
            detail: format!(
                "{} member(s) removed",
                before.members.len() - after.members.len()
            ),
        });
    }
    if after.members.len() > before.members.len() + 1 {
        return Err(LaneViolation::ManifestNotAppend {
            detail: format!(
                "{} members appended at once (one join, one entry)",
                after.members.len() - before.members.len()
            ),
        });
    }
    for (i, old) in before.members.iter().enumerate() {
        if after.members[i] != *old {
            return Err(LaneViolation::ManifestNotAppend {
                detail: format!("existing member '{}' was rewritten", old.id),
            });
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------
// 1.2 Typed inbox items (D4, locked)
// ---------------------------------------------------------------------

/// What an inbox item *is*. `None` on a parsed item means the file said
/// something outside the vocabulary — shown as-is, never guessed at, and
/// never acceptable to [`accept_task`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InboxKind {
    Task,
    Message,
    Notification,
}

impl InboxKind {
    pub fn as_str(self) -> &'static str {
        match self {
            InboxKind::Task => "task",
            InboxKind::Message => "message",
            InboxKind::Notification => "notification",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "task" => Some(InboxKind::Task),
            "message" => Some(InboxKind::Message),
            "notification" => Some(InboxKind::Notification),
            _ => None,
        }
    }

    pub const ALL: [InboxKind; 3] =
        [InboxKind::Task, InboxKind::Message, InboxKind::Notification];
}

/// Where an item sits in the lifecycle (D4). The sender writes `unread`;
/// every later transition is the recipient's Ken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InboxStatus {
    Unread,
    Seen,
    Accepted,
    Archived,
}

impl InboxStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            InboxStatus::Unread => "unread",
            InboxStatus::Seen => "seen",
            InboxStatus::Accepted => "accepted",
            InboxStatus::Archived => "archived",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "unread" => Some(InboxStatus::Unread),
            "seen" => Some(InboxStatus::Seen),
            "accepted" => Some(InboxStatus::Accepted),
            "archived" => Some(InboxStatus::Archived),
            _ => None,
        }
    }

    pub const ALL: [InboxStatus; 4] = [
        InboxStatus::Unread,
        InboxStatus::Seen,
        InboxStatus::Accepted,
        InboxStatus::Archived,
    ];
}

/// The `task:` payload carried by a `kind: task` item — the fields
/// [`accept_task`] copies onto a real board task. Every field is a
/// `String`/`Vec<String>` so a teammate's odd value degrades to "shown as
/// written" instead of failing the parse.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InboxTaskPayload {
    pub title: String,
    pub project: String,
    pub tags: Vec<String>,
    pub due: String,
    /// `human` | `ai` — the ken-tasks vocabulary.
    pub kind: String,
}

const INBOX_KNOWN_KEYS: &[&str] =
    &["id", "kind", "from", "status", "created", "updated", "title", "task"];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct InboxFrontmatter {
    #[serde(default)]
    id: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    from: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    created: String,
    #[serde(default)]
    updated: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    task: Option<InboxTaskPayload>,
    #[serde(flatten)]
    extra: serde_yaml::Mapping,
}

/// A parsed inbox item.
///
/// Reads are tolerant by contract (D1 risks: "bad frontmatter ⇒ item shown
/// raw, never crash"): [`parse_inbox_item`] never returns an error.
/// `malformed` marks a file whose frontmatter block was missing or
/// unparsable — the tray shows its text verbatim and Ken never rewrites it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxItem {
    pub id: String,
    pub kind: Option<InboxKind>,
    /// Exactly what the file said, so an unknown kind is displayable.
    pub kind_raw: String,
    pub from: String,
    pub status: Option<InboxStatus>,
    pub status_raw: String,
    pub created: String,
    pub updated: String,
    pub title: String,
    pub task: Option<InboxTaskPayload>,
    pub body: String,
    pub malformed: bool,
    pub file_name: String,
    /// Unknown frontmatter keys, preserved for display only — writes go
    /// through [`tasks::patch_text`], which leaves their lines untouched.
    #[serde(skip)]
    extra: serde_yaml::Mapping,
}

impl InboxItem {
    pub fn extra(&self) -> &serde_yaml::Mapping {
        &self.extra
    }

    /// Whether this item is a task that hasn't been accepted yet — the
    /// only thing [`accept_task`] will act on.
    pub fn is_pending_task(&self) -> bool {
        self.kind == Some(InboxKind::Task)
            && matches!(self.status, Some(InboxStatus::Unread) | Some(InboxStatus::Seen))
    }
}

/// `<ulid>-<slug>.md`, matching `tasks::task_file_name`'s shape so a
/// family clone reads like every other Ken folder. An item with no usable
/// title falls back to its kind rather than `tasks::slugify`'s `"task"`
/// default, which would mislabel a message.
pub fn inbox_item_file_name(id: &str, kind: InboxKind, title: &str) -> String {
    let slug = if title.trim().chars().any(|c| c.is_ascii_alphanumeric()) {
        tasks::slugify(title)
    } else {
        kind.as_str().to_string()
    };
    format!("{id}-{slug}.md")
}

/// The fields a sender supplies. `id` is caller-supplied-or-generated so
/// tests stay deterministic (the convention `memory.rs` set for dates and
/// `tasks::NewTask` for ids).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NewInboxItem {
    pub id: Option<String>,
    pub kind: Option<InboxKind>,
    /// The sending member's id.
    pub from: String,
    pub title: String,
    pub body: String,
    pub task: Option<InboxTaskPayload>,
}

/// Indent a rendered key's lines by one nesting level, so a nested
/// mapping is built from the very same renderers (and therefore the very
/// same quoting rules) as a top-level key.
fn indent(lines: Vec<String>) -> Vec<String> {
    lines.into_iter().map(|l| format!("  {l}")).collect()
}

fn task_payload_lines(task: &InboxTaskPayload) -> Vec<String> {
    let mut out = vec!["task:".to_string()];
    out.extend(indent(tasks::scalar_lines("title", task.title.trim())));
    if !task.project.trim().is_empty() {
        out.extend(indent(tasks::scalar_lines("project", task.project.trim())));
    }
    if !task.tags.is_empty() {
        out.extend(indent(tasks::seq_lines("tags", &task.tags)));
    }
    if !task.due.trim().is_empty() {
        out.extend(indent(tasks::scalar_lines("due", task.due.trim())));
    }
    let kind = tasks::TaskKind::parse(&task.kind).unwrap_or(tasks::TaskKind::Human);
    out.extend(indent(tasks::scalar_lines("kind", kind.as_str())));
    out
}

/// Serialize a new inbox item.
///
/// Goes through [`tasks::patch_text`] with an empty input rather than
/// `format!`-ing YAML by hand: the patch core owns frontmatter assembly,
/// scalar quoting, and body spacing for every Ken file format, and an item
/// built any other way would drift from what a later `status` patch
/// expects to find.
pub fn render_inbox_item(new: &NewInboxItem, id: &str, created: &str) -> String {
    let kind = new.kind.unwrap_or(InboxKind::Message);
    let mut edits: Vec<(&str, Vec<String>)> = vec![
        ("id", tasks::scalar_lines("id", id)),
        ("kind", tasks::scalar_lines("kind", kind.as_str())),
        ("from", tasks::scalar_lines("from", &new.from)),
        // Every item starts unread; the recipient's Ken owns every later
        // transition (D4).
        ("status", tasks::scalar_lines("status", InboxStatus::Unread.as_str())),
        ("created", tasks::scalar_lines("created", created)),
        ("updated", tasks::scalar_lines("updated", created)),
        ("title", tasks::scalar_lines("title", new.title.trim())),
    ];
    if kind == InboxKind::Task {
        let payload = new.task.clone().unwrap_or_else(|| InboxTaskPayload {
            title: new.title.trim().to_string(),
            ..InboxTaskPayload::default()
        });
        edits.push(("task", task_payload_lines(&payload)));
    }
    let body = new.body.trim();
    tasks::patch_text("", &edits, if body.is_empty() { None } else { Some(body) })
}

fn fm_str(m: &serde_yaml::Mapping, key: &str) -> String {
    match m.get(serde_yaml::Value::String(key.to_string())) {
        Some(serde_yaml::Value::String(s)) => s.clone(),
        Some(serde_yaml::Value::Number(n)) => n.to_string(),
        Some(serde_yaml::Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

/// Split `---\n…\n---\n` off the front. Local rather than borrowed from
/// `tasks.rs` because that module's splitter is private (it retains the
/// delimiter bytes its patch core needs); this read-side use only wants
/// the two halves, exactly like `memory::split_frontmatter`.
fn split_frontmatter(raw: &str) -> Option<(&str, &str)> {
    let open = if let Some(r) = raw.strip_prefix("---\r\n") {
        r
    } else {
        raw.strip_prefix("---\n")?
    };
    let mut off = 0usize;
    loop {
        let end = open[off..].find('\n').map(|i| off + i + 1).unwrap_or(open.len());
        let line = &open[off..end];
        if line.trim_end_matches('\n').trim_end_matches('\r') == "---" {
            return Some((&open[..off], &open[end..]));
        }
        if end >= open.len() {
            return None;
        }
        off = end;
    }
}

/// Parse an inbox item. Never fails: a file with no frontmatter, broken
/// YAML, an unknown `kind`, or keys we've never heard of all come back as
/// an [`InboxItem`] the tray can show (spec: "Malformed items SHALL
/// surface raw in the tray, never crash, never be rewritten").
///
/// `file_name` supplies the id fallback — an item whose frontmatter lost
/// its `id` is still addressable by the ULID in its name.
pub fn parse_inbox_item(file_name: &str, raw: &str) -> InboxItem {
    let id_fallback = file_name
        .strip_suffix(".md")
        .unwrap_or(file_name)
        .split('-')
        .next()
        .unwrap_or("")
        .to_string();

    let malformed_item = |body: &str| InboxItem {
        id: id_fallback.clone(),
        kind: None,
        kind_raw: String::new(),
        from: String::new(),
        status: None,
        status_raw: String::new(),
        created: String::new(),
        updated: String::new(),
        title: String::new(),
        task: None,
        body: body.to_string(),
        malformed: true,
        file_name: file_name.to_string(),
        extra: serde_yaml::Mapping::new(),
    };

    let Some((fm, body)) = split_frontmatter(raw) else {
        return malformed_item(raw);
    };

    let parsed = match serde_yaml::from_str::<InboxFrontmatter>(fm) {
        Ok(p) => p,
        Err(_) => {
            // A typed failure (e.g. `tags:` hand-written as a comma
            // string) still deserves a tolerant mapping read before we
            // give up and call the whole file raw — same ladder as
            // `tasks::parse_frontmatter`.
            let Ok(map) = serde_yaml::from_str::<serde_yaml::Mapping>(fm) else {
                return malformed_item(raw);
            };
            let mut extra = map.clone();
            for k in INBOX_KNOWN_KEYS {
                extra.remove(serde_yaml::Value::String((*k).to_string()));
            }
            InboxFrontmatter {
                id: fm_str(&map, "id"),
                kind: fm_str(&map, "kind"),
                from: fm_str(&map, "from"),
                status: fm_str(&map, "status"),
                created: fm_str(&map, "created"),
                updated: fm_str(&map, "updated"),
                title: fm_str(&map, "title"),
                task: map
                    .get(serde_yaml::Value::String("task".into()))
                    .and_then(|v| serde_yaml::from_value(v.clone()).ok()),
                extra,
            }
        }
    };

    let id = if parsed.id.trim().is_empty() {
        id_fallback
    } else {
        parsed.id.trim().to_string()
    };
    InboxItem {
        id,
        kind: InboxKind::parse(&parsed.kind),
        kind_raw: parsed.kind.clone(),
        from: parsed.from.clone(),
        status: InboxStatus::parse(&parsed.status),
        status_raw: parsed.status.clone(),
        created: parsed.created.clone(),
        updated: parsed.updated.clone(),
        title: parsed.title.clone(),
        task: parsed.task.clone(),
        body: body.to_string(),
        malformed: false,
        file_name: file_name.to_string(),
        extra: parsed.extra,
    }
}

/// The item's text with `status` and `updated` rewritten and every other
/// byte — comments, key order, unknown keys, the body, CRLF terminators —
/// left exactly as it was. Pure; [`apply_inbox_status`] is the on-disk
/// version.
pub fn set_status_text(raw: &str, status: InboxStatus, updated: &str) -> String {
    tasks::patch_text(
        raw,
        &[
            ("status", tasks::scalar_lines("status", status.as_str())),
            ("updated", tasks::scalar_lines("updated", updated)),
        ],
        None,
    )
}

/// Patch an item's status on disk through `tasks::apply_edits`, inheriting
/// its optimistic-concurrency retry (S6) — a poll that rewrites the clone
/// underneath us is exactly the race that guard exists for.
pub fn apply_inbox_status(path: &Path, status: InboxStatus, updated: &str) -> Result<()> {
    tasks::apply_edits(
        path,
        &[
            ("status", tasks::scalar_lines("status", status.as_str())),
            ("updated", tasks::scalar_lines("updated", updated)),
        ],
        None,
    )
}

// ---------------------------------------------------------------------
// 1.5 The acceptance gate (D4, locked)
// ---------------------------------------------------------------------

/// The two file writes an accept produces — both inside the accepting
/// member's own lane (rule 1), so accepting can never fail a lane check.
///
/// Returned as content rather than written, so the caller can put both
/// into one commit (and so this stays testable without a repo).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptedTask {
    /// The inbox item that was accepted.
    pub item_id: String,
    /// The freshly minted board task's id — a *new* ULID: accept copies,
    /// never moves (spec: "a new board task exists with a fresh id citing
    /// the sender").
    pub task_id: String,
    /// `members/<me>/board/<ulid>-<slug>.md`.
    pub board_rel_path: String,
    pub board_content: String,
    /// The same inbox item with `status: accepted`.
    pub inbox_content: String,
}

/// Accept an inbox task item into `member_id`'s family board.
///
/// Pure: takes the item's text, returns the two new file contents. The
/// caller supplies `task_id` (fresh ULID — `tasks::new_ulid()` in
/// production) and the clock, keeping this deterministic under test.
///
/// Refuses anything that isn't a live task item. This is the trust
/// boundary: nothing from a family repo reaches a board, a daily board, or
/// an agent's claimable queue except through this function, and this
/// function is only ever called by an explicit user action (D4, locked: no
/// auto-accept in v1).
pub fn accept_task(
    raw_item: &str,
    member_id: &str,
    task_id: &str,
    today: &str,
    time_hhmm: &str,
) -> Result<AcceptedTask> {
    if !is_valid_member_id(member_id) {
        return Err(Error::Other(format!("'{member_id}' is not a valid member id")));
    }
    let item = parse_inbox_item("", raw_item);
    if item.malformed {
        return Err(Error::Other(
            "this inbox item's frontmatter could not be read — it is shown as-is and \
             cannot be accepted"
                .into(),
        ));
    }
    if item.kind != Some(InboxKind::Task) {
        return Err(Error::Other(format!(
            "only task items can be accepted (this one is '{}')",
            if item.kind_raw.is_empty() { "untyped" } else { &item.kind_raw }
        )));
    }
    if item.status == Some(InboxStatus::Accepted) {
        return Err(Error::Other(format!(
            "inbox item '{}' was already accepted",
            item.id
        )));
    }

    let payload = item.task.clone().unwrap_or_default();
    let title = if payload.title.trim().is_empty() {
        item.title.trim()
    } else {
        payload.title.trim()
    };
    if title.is_empty() {
        return Err(Error::Other("this task item has no title".into()));
    }
    let kind = tasks::TaskKind::parse(&payload.kind).unwrap_or(tasks::TaskKind::Human);

    let mut edits: Vec<(&str, Vec<String>)> = vec![
        ("id", tasks::scalar_lines("id", task_id)),
        ("title", tasks::scalar_lines("title", title)),
        // Accepted work lands in the intake column, not in progress —
        // acceptance is "this is mine now", not "I started it".
        ("status", tasks::scalar_lines("status", tasks::TaskStatus::Backlog.as_str())),
        ("kind", tasks::scalar_lines("kind", kind.as_str())),
        // D5: family board tasks carry manifest member ids in `assignee`.
        ("assignee", tasks::scalar_lines("assignee", member_id)),
        ("project", tasks::scalar_lines("project", payload.project.trim())),
        ("tags", tasks::seq_lines("tags", &payload.tags)),
    ];
    if !payload.due.trim().is_empty() {
        edits.push(("due", tasks::scalar_lines("due", payload.due.trim())));
    }
    edits.push(("board", tasks::scalar_lines("board", tasks::BoardKind::Main.as_str())));
    edits.push(("created", tasks::scalar_lines("created", today)));
    edits.push(("updated", tasks::scalar_lines("updated", today)));

    let brief = item.body.trim();
    let provenance = format!(
        "Accepted from family inbox item `{}` sent by `{}`.",
        item.id,
        if item.from.trim().is_empty() { "unknown" } else { item.from.trim() }
    );
    let log = tasks::compose_log_entry(brief, &provenance, today, time_hhmm);
    let body = if brief.is_empty() {
        log
    } else {
        format!("{brief}\n\n{log}")
    };

    let file_name = tasks::task_file_name(task_id, title);
    Ok(AcceptedTask {
        item_id: item.id.clone(),
        task_id: task_id.to_string(),
        board_rel_path: format!("{}/{file_name}", board_rel(member_id)),
        board_content: tasks::patch_text("", &edits, Some(&body)),
        inbox_content: set_status_text(raw_item, InboxStatus::Accepted, today),
    })
}

/// The push-back a secretary sends instead of editing the sender's files
/// (D4): an ordinary message item created in the *sender's* inbox, which
/// is lane rule 2 and nothing more.
pub fn push_back_item(item: &InboxItem, me: &str, note: &str) -> NewInboxItem {
    NewInboxItem {
        id: None,
        kind: Some(InboxKind::Message),
        from: me.to_string(),
        title: format!("Re: {}", item.title.trim()),
        body: format!("{}\n\n> {}", note.trim(), item.title.trim()),
        task: None,
    }
}

// ---------------------------------------------------------------------
// 1.6 Built-in tier rules (D6)
// ---------------------------------------------------------------------

/// Built-in tier rules for a family clone ingested as a `kind: family`
/// member (D6): `shared/**` **full** — curated team knowledge whose
/// entities federate into the workspace KG — and `members/**` plus
/// `family.json` **search-only**, so inboxes, boards, and working folders
/// are findable but never mint entities and never feed the profiler.
///
/// Deliberately **not** folded into `kenignore::built_in_rule_sets()`,
/// for the same reason `memory::workspace_builtin_rules()` isn't: that
/// function takes no arguments and `scan.rs` folds it into *every*
/// project's classify call, so these patterns would leak onto ordinary
/// member projects — demoting any repo's own `members/` folder to
/// search-only and force-promoting any repo's `shared/` folder to full,
/// overriding the user's `.kenignore` intent. The rules here are scoped to
/// one clone of one family, which is exactly what a parameterless
/// every-project hook cannot express. The seam is this function: whoever
/// ingests the family clone (src-tauri task 2.5) folds it into that
/// member's `rule_sets`, the same way `Project::kenignore_rules()` supplies
/// the user tier for an ordinary project.
pub fn family_builtin_rules() -> Vec<Rule> {
    vec![
        Rule { tier: Tier::Full, pattern: format!("/{SHARED_DIR}/") },
        Rule { tier: Tier::SearchOnly, pattern: format!("/{MEMBERS_DIR}/") },
        Rule { tier: Tier::SearchOnly, pattern: format!("/{MANIFEST_FILE}") },
    ]
}

// ---------------------------------------------------------------------
// 1.7 Template scaffold (D2)
// ---------------------------------------------------------------------

/// One file of the initial template, ready to write and commit.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScaffoldFile {
    /// Repo-relative, forward slashes.
    pub rel_path: String,
    pub content: String,
}

fn readme_text(name: &str) -> String {
    format!(
        "# {name} — a Ken family repo\n\
         \n\
         This repository is a collaboration bus, not a project. **Nobody edits\n\
         it by hand.** Each member's Ken is their secretary inside it: it\n\
         delivers items to teammates' inboxes, surfaces what arrives, and keeps\n\
         that member's board visible to the team.\n\
         \n\
         ```\n\
         {MANIFEST_FILE}                  who is in this family\n\
         {MEMBERS_DIR}/<member-id>/\n\
           {INBOX_SUBDIR}/                     items addressed to that member\n\
           {BOARD_SUBDIR}/                     that member's tasks\n\
           {WORKSPACE_SUBDIR}/                 that member's AI working area\n\
         {SHARED_DIR}/                    team knowledge (start with {CONVENTIONS_FILE})\n\
         ```\n\
         \n\
         Every file has exactly one writer, so merges are trivially clean. If\n\
         you hand-edit this repo you can break that, and your teammates' Kens\n\
         will stop syncing until someone resolves it manually. Read\n\
         `{SHARED_DIR}/{CONVENTIONS_FILE}` before you touch anything.\n"
    )
}

fn conventions_text(name: &str, members: &[FamilyMember]) -> String {
    let owner = members.first().map(|m| m.id.as_str()).unwrap_or("the first member");
    let roster = members
        .iter()
        .map(|m| {
            let who = if m.name.trim().is_empty() { m.id.clone() } else { m.name.clone() };
            format!("- `{}` — {who}\n", m.id)
        })
        .collect::<String>();
    format!(
        "---\n\
         title: Ken conventions for {name}\n\
         ---\n\
         \n\
         # Ken conventions for {name}\n\
         \n\
         This file is the behavior contract every member's Ken loads and\n\
         adheres to. It is the human-readable half of rules that are also\n\
         enforced in code — if the code and this file ever disagree, the code\n\
         wins and this file is the bug.\n\
         \n\
         ## Who is here\n\
         \n\
         {roster}\n\
         `{owner}` is the family owner: the one member whose Ken writes\n\
         `{SHARED_DIR}/`.\n\
         \n\
         ## Write lanes\n\
         \n\
         Your Ken may write exactly three things:\n\
         \n\
         1. Anything under `{MEMBERS_DIR}/<you>/` — your inbox, your board,\n\
            your working area.\n\
         2. **New files only** under another member's\n\
            `{MEMBERS_DIR}/<them>/{INBOX_SUBDIR}/`. You create an item once,\n\
            with a ULID filename; from that moment only *they* may change it.\n\
            Never edit, never delete, never rename someone else's file.\n\
         3. One appended entry in `{MANIFEST_FILE}`'s `members` array, when\n\
            you join.\n\
         \n\
         That is the whole rule set, and it is why this repo never conflicts:\n\
         every file has exactly one writer at any moment. Ken refuses commits\n\
         that break it — a refusal is a bug report, not a permission problem.\n\
         \n\
         ## Inbox etiquette\n\
         \n\
         - Delivery is not assignment. Putting a task in someone's inbox asks;\n\
           it does not schedule. Nothing you send reaches their board, their\n\
           daily plan, or their agents until they explicitly accept it.\n\
         - One item, one subject. Write a real `title` — it is what the\n\
           recipient's tray shows.\n\
         - Say why, not just what. The body is the brief: context, links,\n\
           what \"done\" looks like.\n\
         - Don't chase. Items are polled on an interval, not delivered\n\
           instantly. Re-sending the same request produces two items, not a\n\
           faster answer.\n\
         - The recipient's Ken is a secretary: it may summarize, group, and\n\
           propose accept / push-back / archive. Expect triage, not obedience.\n\
         \n\
         ## What belongs in `{SHARED_DIR}/`\n\
         \n\
         Team knowledge with a shelf life longer than a task: decisions and\n\
         why they were made, architecture and interfaces, glossary and naming,\n\
         onboarding, conventions like this one. It is indexed at full tier and\n\
         its entities join the workspace knowledge graph, so treat it as\n\
         documentation the whole team's Ken will quote back.\n\
         \n\
         Not here: anything addressed to one person (that's an inbox item),\n\
         work-in-progress (that's your board or your working area), secrets of\n\
         any kind, or generated files.\n\
         \n\
         To change `{SHARED_DIR}/`, send a proposal item to `{owner}` — a\n\
         message item describing the edit. `{owner}`'s Ken makes the write.\n\
         \n\
         ## How push-back works\n\
         \n\
         You never negotiate by editing the sender's files. To decline,\n\
         re-scope, or ask a question, your Ken creates a normal message item\n\
         in the *sender's* inbox (lane rule 2) quoting the original. The\n\
         original item stays yours to mark `seen`, `accepted`, or `archived`.\n\
         Every disagreement is therefore just two more one-writer files, and\n\
         the repo stays conflict-free.\n"
    )
}

/// The initial file set for "Create family" (D2) — pure, so the caller
/// writes and commits it (through [`Lane::bootstrap`]) and tests can read
/// it without a repo.
///
/// `id` is caller-supplied for the same reason task ids are: it lets a
/// test assert on exact bytes, and it lets the connection store know the
/// family id before the repo exists.
pub fn scaffold_family(
    name: &str,
    id: Uuid,
    members: &[FamilyMember],
) -> Result<Vec<ScaffoldFile>> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::Other("a family needs a name".into()));
    }
    if members.is_empty() {
        return Err(Error::Other("a family needs at least one member".into()));
    }
    for (i, m) in members.iter().enumerate() {
        if !is_valid_member_id(&m.id) {
            return Err(Error::Other(format!(
                "'{}' is not a valid member id — use a short slug like 'sarah'",
                m.id
            )));
        }
        if members[..i].iter().any(|prev| prev.id == m.id) {
            return Err(Error::Other(format!("duplicate member id '{}'", m.id)));
        }
    }

    let manifest = FamilyManifest {
        id,
        name: name.to_string(),
        template: SUPPORTED_TEMPLATE,
        members: members.to_vec(),
        extra: serde_json::Map::new(),
    };

    let mut files = vec![
        ScaffoldFile { rel_path: MANIFEST_FILE.to_string(), content: manifest.to_json()? },
        ScaffoldFile { rel_path: README_FILE.to_string(), content: readme_text(name) },
        ScaffoldFile {
            rel_path: format!("{SHARED_DIR}/{CONVENTIONS_FILE}"),
            content: conventions_text(name, members),
        },
    ];
    // git tracks files, not folders, so each member area needs a marker or
    // it simply won't exist in a fresh clone.
    for m in members {
        for sub in [INBOX_SUBDIR, BOARD_SUBDIR, WORKSPACE_SUBDIR] {
            files.push(ScaffoldFile {
                rel_path: format!("{MEMBERS_DIR}/{}/{sub}/{GITKEEP_FILE}", m.id),
                content: String::new(),
            });
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn members() -> Vec<FamilyMember> {
        vec![
            FamilyMember::new("owner", "Ada"),
            FamilyMember::new("sarah", "Sarah"),
        ]
    }

    // ---- 1.1 manifest ----

    #[test]
    fn manifest_round_trips_unknown_keys() {
        let raw = r#"{
  "id": "6b8c0c9e-7a2f-4d61-9c37-1c9a4e6f0a11",
  "name": "Team",
  "template": 1,
  "members": [{"id": "owner", "name": "Ada", "avatar": "cat.png"}],
  "policy": {"autoAccept": false}
}"#;
        let m = FamilyManifest::parse(raw).unwrap();
        assert_eq!(m.name, "Team");
        assert_eq!(m.template, 1);
        assert_eq!(m.members[0].extra["avatar"], serde_json::json!("cat.png"));
        assert!(m.extra.contains_key("policy"));

        let round = FamilyManifest::parse(&m.to_json().unwrap()).unwrap();
        assert_eq!(round, m);
    }

    #[test]
    fn manifest_missing_template_reads_as_version_one() {
        let m = FamilyManifest::parse(r#"{"name":"T","members":[]}"#).unwrap();
        assert_eq!(m.template, 1);
        assert!(m.check_supported().is_ok());
    }

    #[test]
    fn newer_template_is_refused_with_a_typed_error() {
        let raw = r#"{"name":"T","template":9,"members":[{"id":"owner","name":"Ada"}]}"#;
        let m = FamilyManifest::parse(raw).unwrap();
        let err = m.check_supported().unwrap_err();
        assert_eq!(err, UnsupportedTemplate { found: 9, supported: SUPPORTED_TEMPLATE });
        assert!(err.to_string().contains("needs a newer Ken"));
        // Parsing still worked — the refusal is a separate, explicit step
        // so callers can't accidentally sync a repo they can't read.
        assert_eq!(m.members.len(), 1);
    }

    #[test]
    fn manifest_saves_and_loads_from_a_clone_root() {
        let dir = tempdir().unwrap();
        let m = FamilyManifest {
            id: Uuid::nil(),
            name: "Team".into(),
            template: 1,
            members: members(),
            extra: serde_json::Map::new(),
        };
        m.save(dir.path()).unwrap();
        assert_eq!(FamilyManifest::load(dir.path()).unwrap(), m);
        assert_eq!(m.owner_id(), Some("owner"));
        assert!(m.has_member("sarah"));
        assert!(!m.has_member("nobody"));
    }

    #[test]
    fn member_ids_normalize_or_fail_loudly() {
        assert_eq!(normalize_member_id("Sarah Connor").unwrap(), "sarah-connor");
        assert_eq!(normalize_member_id("  Ada  ").unwrap(), "ada");
        assert!(normalize_member_id("...").is_err());
        assert!(normalize_member_id("").is_err());
        assert!(is_valid_member_id("owner-2"));
        assert!(!is_valid_member_id("Owner"));
        assert!(!is_valid_member_id("a/b"));
        assert!(!is_valid_member_id("-x"));
    }

    // ---- 1.1 lanes (D3 rules 1-3) ----

    #[test]
    fn lane_allows_own_member_folder() {
        for path in [
            "members/owner/board/01H-x.md",
            "members/owner/inbox/01H-y.md",
            "members/owner/workspace/notes.md",
            "members/owner/board/archive/2026-08/01H-z.md",
        ] {
            assert!(lane_check("owner", path, true).is_ok(), "new: {path}");
            assert!(lane_check("owner", path, false).is_ok(), "edit: {path}");
        }
    }

    #[test]
    fn lane_allows_new_file_in_foreign_inbox() {
        assert!(lane_check("owner", "members/sarah/inbox/01HZZ-review.md", true).is_ok());
    }

    #[test]
    fn lane_denies_edit_of_foreign_inbox_file() {
        let err = lane_check("owner", "members/sarah/inbox/01HZZ-review.md", false).unwrap_err();
        assert_eq!(
            err,
            LaneViolation::ForeignEdit {
                path: "members/sarah/inbox/01HZZ-review.md".into(),
                owner: "sarah".into(),
            }
        );
    }

    #[test]
    fn lane_denies_foreign_board() {
        for path in [
            "members/sarah/board/01HZZ-x.md",
            "members/sarah/workspace/scratch.md",
            "members/sarah/notes.md",
        ] {
            let err = lane_check("owner", path, true).unwrap_err();
            assert!(
                matches!(err, LaneViolation::ForeignArea { ref owner, .. } if owner == "sarah"),
                "{path} => {err:?}"
            );
        }
    }

    #[test]
    fn lane_allows_manifest_member_append() {
        // Path half of rule 3.
        assert!(lane_check("sarah", MANIFEST_FILE, false).is_ok());

        // Content half: only an append passes.
        let before = FamilyManifest {
            id: Uuid::nil(),
            name: "T".into(),
            template: 1,
            members: vec![FamilyMember::new("owner", "Ada")],
            extra: serde_json::Map::new(),
        };
        let mut after = before.clone();
        after.add_member(FamilyMember::new("sarah", "Sarah")).unwrap();
        assert!(manifest_append_only(&before, &after).is_ok());

        let mut renamed = before.clone();
        renamed.members[0].name = "Someone else".into();
        assert!(matches!(
            manifest_append_only(&before, &renamed),
            Err(LaneViolation::ManifestNotAppend { .. })
        ));

        let mut emptied = before.clone();
        emptied.members.clear();
        assert!(manifest_append_only(&before, &emptied).is_err());

        let mut retitled = after.clone();
        retitled.name = "Other".into();
        assert!(manifest_append_only(&before, &retitled).is_err());

        assert!(before.clone().add_member(FamilyMember::new("owner", "x")).is_err());
        assert!(before.clone().add_member(FamilyMember::new("Bad Id", "x")).is_err());
    }

    #[test]
    fn lane_denies_unsafe_paths() {
        for path in [
            "",
            "/members/owner/x.md",
            "../secrets.md",
            "members/../../etc/passwd",
            "members/owner/./x.md",
            "C:/windows/system32",
            "\\\\server\\share\\x.md",
        ] {
            assert!(
                matches!(lane_check("owner", path, true), Err(LaneViolation::UnsafePath { .. })),
                "{path:?} should be unsafe"
            );
        }
    }

    #[test]
    fn lane_denies_everything_outside_the_three_rules() {
        for path in ["README.md", "shared/architecture.md", "members", "members/owner", "x/y.md"] {
            assert!(lane_check("owner", path, true).is_err(), "{path}");
        }
    }

    #[test]
    fn shared_is_owner_only() {
        let manifest = FamilyManifest {
            id: Uuid::nil(),
            name: "T".into(),
            template: 1,
            members: members(),
            extra: serde_json::Map::new(),
        };
        let owner = Lane::for_manifest("owner", &manifest);
        let sarah = Lane::for_manifest("sarah", &manifest);

        assert!(owner.check("shared/architecture.md", true).is_ok());
        assert!(owner.check("shared/architecture.md", false).is_ok());
        assert!(matches!(
            sarah.check("shared/architecture.md", true),
            Err(LaneViolation::SharedNotOwner { .. })
        ));
        // The owner's extra reach stops at shared/ — every other rule holds.
        assert!(owner.check("members/sarah/board/x.md", true).is_err());
        assert!(owner.check("README.md", true).is_err());
    }

    #[test]
    fn bootstrap_lane_creates_but_never_edits() {
        let lane = Lane::bootstrap();
        for f in scaffold_family("Team", Uuid::nil(), &members()).unwrap() {
            assert!(lane.check(&f.rel_path, true).is_ok(), "{}", f.rel_path);
            assert!(lane.check(&f.rel_path, false).is_err(), "{}", f.rel_path);
        }
        assert!(lane.check("../escape.md", true).is_err());
    }

    // ---- 1.2 inbox items ----

    fn sample_task_item() -> NewInboxItem {
        NewInboxItem {
            id: None,
            kind: Some(InboxKind::Task),
            from: "sarah".into(),
            title: "Review the sync loop".into(),
            body: "Focus on the non-fast-forward retry.".into(),
            task: Some(InboxTaskPayload {
                title: "Review the sync loop".into(),
                project: "ken".into(),
                tags: vec!["review".into(), "sync".into()],
                due: "2026-08-10".into(),
                kind: "human".into(),
            }),
        }
    }

    #[test]
    fn inbox_item_round_trips_every_kind_and_status() {
        // Hand-rolled property sweep (no proptest in this workspace's
        // dependency tree): every kind x status x a spread of awkward
        // titles/bodies, serialized then parsed then re-serialized.
        let titles = [
            "Plain title",
            "Title: with a colon",
            "It's got an apostrophe",
            "  padded  ",
            "#hash-leading",
            "",
        ];
        let bodies = ["", "one line", "two\n\nparagraphs", "---\nlooks like frontmatter"];
        let mut checked = 0;
        for kind in InboxKind::ALL {
            for (ti, title) in titles.iter().enumerate() {
                for (bi, body) in bodies.iter().enumerate() {
                    let new = NewInboxItem {
                        id: None,
                        kind: Some(kind),
                        from: "sarah".into(),
                        title: (*title).into(),
                        body: (*body).into(),
                        task: (kind == InboxKind::Task).then(|| InboxTaskPayload {
                            title: (*title).into(),
                            project: "ken".into(),
                            tags: vec!["a".into(), "b: c".into()],
                            due: "2026-08-10".into(),
                            kind: if ti % 2 == 0 { "human".into() } else { "ai".into() },
                        }),
                    };
                    let id = tasks::ulid_from_parts(1_700_000_000_000 + bi as u64, [7; 10]);
                    let text = render_inbox_item(&new, &id, "2026-08-03");
                    let file = inbox_item_file_name(&id, kind, title);
                    let item = parse_inbox_item(&file, &text);

                    assert!(!item.malformed, "{text}");
                    assert_eq!(item.id, id);
                    assert_eq!(item.kind, Some(kind));
                    assert_eq!(item.from, "sarah");
                    assert_eq!(item.status, Some(InboxStatus::Unread));
                    assert_eq!(item.title, title.trim());
                    assert_eq!(item.body.trim(), body.trim());
                    assert_eq!(item.task.is_some(), kind == InboxKind::Task);
                    if let Some(payload) = &item.task {
                        assert_eq!(payload.title, title.trim());
                        assert_eq!(payload.tags, vec!["a".to_string(), "b: c".to_string()]);
                        assert_eq!(payload.due, "2026-08-10");
                    }
                    assert!(file.ends_with(".md") && file.starts_with(&id));

                    // Status transitions rewrite two lines and nothing else.
                    for status in InboxStatus::ALL {
                        let patched = set_status_text(&text, status, "2026-08-04");
                        let back = parse_inbox_item(&file, &patched);
                        assert_eq!(back.status, Some(status));
                        assert_eq!(back.updated, "2026-08-04");
                        assert_eq!(back.id, item.id);
                        assert_eq!(back.title, item.title);
                        assert_eq!(back.body, item.body);
                        assert_eq!(back.task, item.task);
                    }
                    checked += 1;
                }
            }
        }
        assert_eq!(checked, 3 * titles.len() * bodies.len());
    }

    #[test]
    fn inbox_status_patch_preserves_unknown_keys_and_comments_byte_for_byte() {
        // `concat!`, not a `\`-continued literal: Rust strips the leading
        // whitespace after a line continuation, which would silently
        // un-indent the nested `task:` block.
        let raw = concat!(
            "---\n",
            "# a comment a teammate's Ken wrote\n",
            "id: 01HZZ\n",
            "kind: task\n",
            "from: sarah\n",
            "status: unread\n",
            "priority: high\n",
            "created: 2026-08-01\n",
            "updated: 2026-08-01\n",
            "title: Ship it\n",
            "task:\n",
            "  title: Ship it\n",
            "  kind: human\n",
            "---\n",
            "\n",
            "Body stays.\n",
        );
        let patched = set_status_text(raw, InboxStatus::Seen, "2026-08-03");
        // Two lines replaced, every other byte identical. (`tasks`'
        // renderer quotes a date-shaped scalar — that's its rule, not a
        // rewrite of anything we didn't name.)
        assert_eq!(
            patched,
            raw.replace("status: unread", "status: seen")
                .replace("updated: 2026-08-01", "updated: '2026-08-03'")
        );
        let item = parse_inbox_item("01HZZ-ship-it.md", &patched);
        assert_eq!(item.extra()["priority"], serde_yaml::Value::String("high".into()));
        assert_eq!(item.task.unwrap().title, "Ship it");
    }

    #[test]
    fn malformed_items_surface_raw_and_are_never_accepted() {
        let cases = [
            "no frontmatter at all, just prose",
            "---\nthis: [is not: valid yaml\n---\n\nbody",
            "---\nunterminated frontmatter\n",
            "",
        ];
        for raw in cases {
            let item = parse_inbox_item("01HZZ-x.md", raw);
            assert!(item.malformed, "{raw:?}");
            assert_eq!(item.id, "01HZZ");
            assert_eq!(item.body, raw);
            assert_eq!(item.kind, None);
            assert!(accept_task(raw, "owner", "01NEW", "2026-08-03", "09:00").is_err());
        }
    }

    #[test]
    fn unknown_kind_and_status_degrade_instead_of_failing() {
        let raw = "---\nid: 01HZZ\nkind: invoice\nfrom: sarah\nstatus: pending\n---\n\nhi\n";
        let item = parse_inbox_item("01HZZ-x.md", raw);
        assert!(!item.malformed);
        assert_eq!(item.kind, None);
        assert_eq!(item.kind_raw, "invoice");
        assert_eq!(item.status, None);
        assert_eq!(item.status_raw, "pending");
        assert!(!item.is_pending_task());
    }

    #[test]
    fn item_id_falls_back_to_the_filename_ulid() {
        let raw = "---\nkind: message\nfrom: sarah\nstatus: unread\n---\n\nhi\n";
        let item = parse_inbox_item("01HZZABCDEF-hello-there.md", raw);
        assert_eq!(item.id, "01HZZABCDEF");
    }

    // ---- 1.5 accept ----

    #[test]
    fn accept_mints_a_board_task_with_provenance_and_marks_the_item() {
        let text = render_inbox_item(&sample_task_item(), "01ITEM", "2026-08-01");
        let out = accept_task(&text, "owner", "01TASK", "2026-08-03", "09:30").unwrap();

        assert_eq!(out.item_id, "01ITEM");
        assert_eq!(out.task_id, "01TASK");
        assert_eq!(out.board_rel_path, "members/owner/board/01TASK-review-the-sync-loop.md");
        // The new task is entirely inside the accepting member's own lane.
        assert!(lane_check("owner", &out.board_rel_path, true).is_ok());

        let task = tasks::parse_task(
            Path::new(&out.board_rel_path),
            tasks::HomeKind::Project,
            "ken",
            &out.board_content,
        );
        assert_eq!(task.id, "01TASK");
        assert_eq!(task.title, "Review the sync loop");
        assert_eq!(task.status, Some(tasks::TaskStatus::Backlog));
        assert_eq!(task.assignee, "owner");
        assert_eq!(task.project, "ken");
        assert_eq!(task.tags, vec!["review".to_string(), "sync".to_string()]);
        assert_eq!(task.due.as_deref(), Some("2026-08-10"));
        assert_eq!(task.created, "2026-08-03");
        assert!(task.body.contains(tasks::LOG_HEADING));
        assert!(task.body.contains("01ITEM"));
        assert!(task.body.contains("sarah"));
        assert!(task.body.contains("Focus on the non-fast-forward retry."));

        let item = parse_inbox_item("01ITEM-review-the-sync-loop.md", &out.inbox_content);
        assert_eq!(item.status, Some(InboxStatus::Accepted));
        assert_eq!(item.updated, "2026-08-03");
        // Accept copies, never moves: the payload is untouched.
        assert_eq!(item.task, sample_task_item().task);
    }

    #[test]
    fn accept_refuses_non_tasks_already_accepted_and_bad_members() {
        let msg = NewInboxItem { kind: Some(InboxKind::Message), ..sample_task_item() };
        let text = render_inbox_item(&msg, "01ITEM", "2026-08-01");
        assert!(accept_task(&text, "owner", "01TASK", "2026-08-03", "09:30").is_err());

        let task_text = render_inbox_item(&sample_task_item(), "01ITEM", "2026-08-01");
        let accepted = set_status_text(&task_text, InboxStatus::Accepted, "2026-08-02");
        assert!(accept_task(&accepted, "owner", "01TASK", "2026-08-03", "09:30").is_err());

        assert!(accept_task(&task_text, "../evil", "01TASK", "2026-08-03", "09:30").is_err());
    }

    #[test]
    fn push_back_is_a_new_message_into_the_senders_inbox() {
        let text = render_inbox_item(&sample_task_item(), "01ITEM", "2026-08-01");
        let item = parse_inbox_item("01ITEM-x.md", &text);
        let reply = push_back_item(&item, "owner", "Too big — split it?");
        assert_eq!(reply.kind, Some(InboxKind::Message));
        assert_eq!(reply.from, "owner");
        assert!(reply.title.starts_with("Re: "));

        let rendered = render_inbox_item(&reply, "01REPLY", "2026-08-03");
        let path = format!("{}/01REPLY-re-review-the-sync-loop.md", inbox_rel(&item.from));
        // A push-back is a create in the sender's inbox — lane rule 2 —
        // and would be refused as an edit.
        assert!(lane_check("owner", &path, true).is_ok());
        assert!(lane_check("owner", &path, false).is_err());
        assert!(rendered.contains("status: unread"));
    }

    // ---- 1.6 tier rules ----

    #[test]
    fn family_builtin_rules_classify_as_designed() {
        let rules = family_builtin_rules();
        let rs: &[&[Rule]] = &[&rules];
        assert_eq!(crate::kenignore::classify("shared/architecture.md", false, rs), Tier::Full);
        assert_eq!(
            crate::kenignore::classify("members/owner/inbox/01H-x.md", false, rs),
            Tier::SearchOnly
        );
        assert_eq!(
            crate::kenignore::classify("members/owner/board/01H-y.md", false, rs),
            Tier::SearchOnly
        );
        assert_eq!(crate::kenignore::classify("family.json", false, rs), Tier::SearchOnly);
        // Anything else in the clone keeps the default.
        assert_eq!(crate::kenignore::classify("README.md", false, rs), Tier::Full);
    }

    // ---- 1.7 scaffold ----

    #[test]
    fn scaffold_produces_the_template_and_a_readable_manifest() {
        let files = scaffold_family("Ken Team", Uuid::nil(), &members()).unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.rel_path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "family.json",
                "README.md",
                "shared/conventions.md",
                "members/owner/inbox/.gitkeep",
                "members/owner/board/.gitkeep",
                "members/owner/workspace/.gitkeep",
                "members/sarah/inbox/.gitkeep",
                "members/sarah/board/.gitkeep",
                "members/sarah/workspace/.gitkeep",
            ]
        );

        let manifest = FamilyManifest::parse(&files[0].content).unwrap();
        assert_eq!(manifest.template, 1);
        assert_eq!(manifest.name, "Ken Team");
        assert_eq!(manifest.member_ids(), vec!["owner", "sarah"]);
        assert!(manifest.check_supported().is_ok());

        // conventions.md is the behavior contract: it must actually state
        // the four things D3 says it states.
        let conventions = &files[2].content;
        assert!(conventions.contains("New files only"));
        assert!(conventions.contains("Inbox etiquette"));
        assert!(conventions.contains("What belongs in `shared/`"));
        assert!(conventions.contains("push-back"));
        assert!(conventions.contains("`owner`"));

        assert!(files[1].content.contains("Nobody edits"));
    }

    #[test]
    fn scaffold_refuses_a_family_it_could_not_address() {
        assert!(scaffold_family("", Uuid::nil(), &members()).is_err());
        assert!(scaffold_family("T", Uuid::nil(), &[]).is_err());
        assert!(scaffold_family(
            "T",
            Uuid::nil(),
            &[FamilyMember::new("Bad Id", "x")]
        )
        .is_err());
        assert!(scaffold_family(
            "T",
            Uuid::nil(),
            &[FamilyMember::new("a", "x"), FamilyMember::new("a", "y")]
        )
        .is_err());
    }
}

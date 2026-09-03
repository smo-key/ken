//! Workspace lifecycle: `<parent>/.ken-workspace/workspace.json` is the
//! shared, text-only source of truth for a parent folder whose child
//! projects are opened together (`openspec/changes/workspace`). Same
//! philosophy as `project.rs`'s `.ken/project.json` — unknown fields survive
//! a rewrite, and a manifest already on disk is adopted rather than
//! clobbered.
//!
//! Deviation from `project.rs::Project::save` (which does a plain
//! `fs::write`): `proposal.md`/`spec.md` both call out the workspace
//! manifest as written *atomically*, so [`Workspace::save`] uses the
//! temp-file + rename pattern `profiler::ProjectProfile::save` already
//! established in this crate, rather than copying `Project::save`'s
//! non-atomic write verbatim.
//!
//! `workspace.rs` only resolves members into `Project`s (via the same
//! adopt-or-create discipline as `Project::create`) and reports per-member
//! status; it holds no DB handle, engine, or watcher of its own — those are
//! `src-tauri`'s `ProjectHandle` concern (design D2/D3).

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::project::{self, Project};
use crate::{Error, Result};

pub const CONFIG_DIR: &str = ".ken-workspace";
pub const CONFIG_FILE: &str = "workspace.json";

pub fn config_path(parent: &Path) -> PathBuf {
    parent.join(CONFIG_DIR).join(CONFIG_FILE)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    pub name: String,
    pub id: Uuid,
    /// Parent-relative paths, one or two segments (design D1: relative so
    /// the manifest survives the parent being moved or synced to a
    /// teammate; D6: a two-segment member like `SR/ShatteredRealms` lives
    /// inside a *group folder*, and the leading segment IS its group).
    /// Always forward slashes, even on Windows — [`validate_member_name`]
    /// normalizes.
    pub members: Vec<String>,
    /// Named sets of members that belong together conceptually even though
    /// they are separate repos (e.g. a game and its tools).
    ///
    /// Unlike [`WorkspaceConfig::links`] — which is read out of `extra` on
    /// demand precisely because nothing in-tree writes it — groups ARE
    /// written by Ken (the user manages them in Settings), so they get a
    /// typed field. `skip_serializing_if` keeps a manifest that has no
    /// groups byte-identical to one written before this field existed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<ProjectGroup>,
    /// Fields written by newer versions or other capabilities survive a
    /// round-trip through this one (same idiom as `ProjectConfig::extra`).
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// One named group of members. `members` holds parent-relative folder
/// names — the same vocabulary as [`WorkspaceConfig::members`] — so a
/// group survives the parent folder moving, exactly like membership does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectGroup {
    pub name: String,
    pub members: Vec<String>,
}

/// The member's own folder name — the last path segment. This is the
/// display name; the full string stays the manifest key.
pub fn member_leaf(member: &str) -> &str {
    member.rsplit('/').next().unwrap_or(member)
}

/// The group folder a nested member lives in (`SR/ShatteredRealms` → `SR`),
/// or `None` for a direct child of the workspace root.
pub fn member_group(member: &str) -> Option<&str> {
    member.rsplit_once('/').map(|(group, _)| group)
}

/// Validate and normalize a member name to its canonical manifest form:
/// forward slashes, no surrounding separators, at most TWO segments (a
/// member either sits directly in the workspace root or one level down
/// inside a group folder — never deeper), and no segment that is empty,
/// dot-prefixed, or a `..` traversal. Returns the normalized string.
///
/// This is the only gate between user input and `parent.join(name)`, so
/// it is deliberately strict: everything it lets through joins to a path
/// that stays inside the workspace.
pub fn validate_member_name(name: &str) -> Result<String> {
    let name = name.trim().replace('\\', "/");
    let name = name.trim_matches('/');
    if name.is_empty() {
        return Err(Error::Other("member name cannot be empty".into()));
    }
    // A Windows drive ("C:") or UNC remnant is absolute intent, not a name.
    if name.contains(':') {
        return Err(Error::Other(format!(
            "member name {name:?} must be a relative path inside the workspace"
        )));
    }
    let segments: Vec<&str> = name.split('/').collect();
    if segments.len() > 2 {
        return Err(Error::Other(format!(
            "member name {name:?} nests too deep — a member is either a \
             folder in the workspace or inside ONE group folder"
        )));
    }
    for segment in &segments {
        if segment.is_empty() || *segment == ".." || segment.starts_with('.') {
            return Err(Error::Other(format!(
                "member name {name:?} contains an invalid path segment"
            )));
        }
    }
    Ok(name.to_string())
}

/// One entry of `workspace.json`'s `links` array (design D12,
/// `ken-pipeline`): an explicit cross-project link, e.g. a tool repo linked
/// to the project it supports. Not a typed `WorkspaceConfig` field — see
/// [`WorkspaceConfig::links`] for why — this struct exists only to give
/// callers a parsed shape for the entries already living in `extra`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectLink {
    pub from: String,
    pub to: String,
    pub relation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl WorkspaceConfig {
    /// The group named `name`, matched case-insensitively (users type
    /// group names; "Shattered Realms" and "shattered realms" are the
    /// same group).
    pub fn group(&self, name: &str) -> Option<&ProjectGroup> {
        self.groups
            .iter()
            .find(|g| g.name.trim().eq_ignore_ascii_case(name.trim()))
    }

    /// A group's members, filtered to folder names that are ACTUALLY
    /// members of this workspace and de-duplicated, preserving the
    /// group's own ordering.
    ///
    /// A group can name a member that was later removed from the
    /// workspace; resolving it here rather than validating on write means
    /// a hand-edited manifest, or a member removed behind Ken's back,
    /// degrades to a smaller group instead of a broken one.
    pub fn group_members(&self, name: &str) -> Vec<String> {
        let Some(group) = self.group(name) else {
            return Vec::new();
        };
        let mut out: Vec<String> = Vec::new();
        for candidate in &group.members {
            let is_member = self.members.iter().any(|m| m == candidate);
            let already = out.iter().any(|o| o == candidate);
            if is_member && !already {
                out.push(candidate.clone());
            }
        }
        out
    }

    /// Every group `member` (a parent-relative folder name) belongs to.
    /// A member may be in more than one group — nothing here enforces a
    /// partition, because "tools" could reasonably sit in both a product
    /// group and a tooling group.
    pub fn groups_for_member(&self, member: &str) -> Vec<&str> {
        self.groups
            .iter()
            .filter(|g| g.members.iter().any(|m| m == member))
            .map(|g| g.name.as_str())
            .collect()
    }

    /// Groups implied by the directory layout: every group folder (the
    /// leading segment of a nested member) becomes a group holding the
    /// members inside it. `SR/ShatteredRealms` + `SR/sr-docs` yields group
    /// "SR" with those two members — no Settings bookkeeping involved.
    /// Member entries hold the FULL manifest strings, the same vocabulary
    /// as `groups`, so both kinds resolve identically downstream.
    pub fn derived_groups(&self) -> Vec<ProjectGroup> {
        let mut out: Vec<ProjectGroup> = Vec::new();
        for member in &self.members {
            let Some(folder) = member_group(member) else {
                continue;
            };
            match out
                .iter_mut()
                .find(|g| g.name.eq_ignore_ascii_case(folder))
            {
                Some(group) => group.members.push(member.clone()),
                None => out.push(ProjectGroup {
                    name: folder.to_string(),
                    members: vec![member.clone()],
                }),
            }
        }
        out
    }

    /// Directory-derived groups first, then manifest groups whose names
    /// don't collide (case-insensitive) with a derived one. The folder is
    /// the stronger claim: it's visible in the filesystem, and `set_group`
    /// refuses to create the collision in the first place — this filter
    /// only matters for a hand-edited manifest.
    pub fn effective_groups(&self) -> Vec<ProjectGroup> {
        let mut out = self.derived_groups();
        for group in &self.groups {
            if !out.iter().any(|d| d.name.eq_ignore_ascii_case(&group.name)) {
                out.push(group.clone());
            }
        }
        out
    }

    /// [`Self::group_members`] over the effective view: resolves a derived
    /// (folder) group or a manifest group by one name, with the same
    /// members-only filtering and de-duplication.
    pub fn effective_group_members(&self, name: &str) -> Vec<String> {
        let name = name.trim();
        let Some(group) = self
            .effective_groups()
            .into_iter()
            .find(|g| g.name.trim().eq_ignore_ascii_case(name))
        else {
            return Vec::new();
        };
        let mut out: Vec<String> = Vec::new();
        for candidate in &group.members {
            let is_member = self.members.iter().any(|m| m == candidate);
            if is_member && !out.iter().any(|o| o == candidate) {
                out.push(candidate.clone());
            }
        }
        out
    }

    /// Create or replace a group. Returns an error for a blank name or an
    /// empty member list — a group with nothing in it is a scope that can
    /// never match anything, which reads as a bug at the point of use.
    /// Members not in the workspace are dropped here rather than stored.
    pub fn set_group(&mut self, name: &str, members: &[String]) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::Other("group name cannot be empty".into()));
        }
        // A group folder already owns this name (D6). Refusing here beats
        // storing a manifest group that `effective_groups` would shadow
        // forever — the user would see their edit silently not exist.
        if self
            .derived_groups()
            .iter()
            .any(|g| g.name.trim().eq_ignore_ascii_case(name))
        {
            return Err(Error::Other(format!(
                "\"{name}\" is already a group folder in the workspace — \
                 its members are the folders inside it"
            )));
        }
        let mut kept: Vec<String> = Vec::new();
        for candidate in members {
            if self.members.iter().any(|m| m == candidate)
                && !kept.iter().any(|k| k == candidate)
            {
                kept.push(candidate.clone());
            }
        }
        if kept.is_empty() {
            return Err(Error::Other(format!(
                "group \"{name}\" would contain no workspace members"
            )));
        }
        match self
            .groups
            .iter_mut()
            .find(|g| g.name.trim().eq_ignore_ascii_case(name))
        {
            Some(existing) => {
                existing.name = name.to_string();
                existing.members = kept;
            }
            None => self.groups.push(ProjectGroup {
                name: name.to_string(),
                members: kept,
            }),
        }
        Ok(())
    }

    /// Remove a group. Returns whether one was actually removed, so a
    /// caller can tell "deleted" from "already gone" without a prior read.
    pub fn remove_group(&mut self, name: &str) -> bool {
        let before = self.groups.len();
        self.groups
            .retain(|g| !g.name.trim().eq_ignore_ascii_case(name.trim()));
        self.groups.len() != before
    }

    /// Parsed `links` array (design D12). Deliberately **not** a typed
    /// `WorkspaceConfig` field: the manifest already round-trips unknown
    /// keys through `extra` (see `unknown_fields_survive_roundtrip`), and a
    /// typed field would only be needed if something here wrote `links`
    /// itself — nothing in this phase does, links are hand-authored or
    /// written by a future lane. Reading them out of `extra` on demand keeps
    /// that round-trip guarantee exactly as-is. Entries that don't parse as
    /// `{from, to, relation, note?}` are skipped rather than failing the
    /// whole read (same tolerant-load philosophy as the rest of this
    /// module).
    pub fn links(&self) -> Vec<ProjectLink> {
        self.extra
            .get("links")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|entry| serde_json::from_value(entry.clone()).ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Project names linked to `name`, in either direction. `links` entries
    /// are stored directional (`from` -> `to`, carrying a `relation` that
    /// reads naturally one way, e.g. "ShatteredRealmsTools tools_for
    /// ShatteredRealms"), but every consumer named in design D12 — idea
    /// dedupe scope, the board's "include linked projects" filter, and
    /// cross-project `blocked_by` suggestions — cares about connectivity,
    /// not which side is `from`. So a link recorded as `{from: A, to: B}`
    /// makes each project visible from the other's scope; callers that need
    /// the raw direction/relation should use [`WorkspaceConfig::links`]
    /// instead.
    pub fn linked_projects(&self, name: &str) -> Vec<&str> {
        let mut out = Vec::new();
        let Some(arr) = self.extra.get("links").and_then(|v| v.as_array()) else {
            return out;
        };
        for entry in arr {
            let from = entry.get("from").and_then(|v| v.as_str());
            let to = entry.get("to").and_then(|v| v.as_str());
            match (from, to) {
                (Some(f), Some(t)) if f == name => out.push(t),
                (Some(f), Some(t)) if t == name => out.push(f),
                _ => {}
            }
        }
        out
    }
}

/// Per-member resolution result within an opened [`Workspace`].
#[derive(Debug, Clone)]
pub enum MemberStatus {
    /// The member folder exists and its `.ken/project.json` was opened (or
    /// adopted/created if the folder had none yet — same adopt-or-create
    /// discipline as [`Project::create`]).
    Ok(Project),
    /// The parent-relative folder name in the manifest doesn't exist on
    /// disk. Not fatal to the workspace open (spec: "Missing member folders
    /// SHALL be reported as `missing` status, never fail the open").
    Missing,
    /// The folder exists but its `.ken/project.json` failed to parse. Not
    /// one of the spec's two named states, but the same tolerant-load
    /// philosophy that runs through the rest of ken-core (`project.rs`,
    /// `settings.rs`) applies here too: one corrupt member must never fail
    /// opening the other N-1.
    Invalid(String),
}

#[derive(Debug, Clone)]
pub struct Member {
    /// Parent-relative folder name, exactly as stored in the manifest.
    pub name: String,
    pub status: MemberStatus,
}

#[derive(Debug, Clone)]
pub struct Workspace {
    /// The parent folder — i.e. the folder containing `.ken-workspace/`.
    pub root: PathBuf,
    pub config: WorkspaceConfig,
    pub members: Vec<Member>,
}

impl Workspace {
    /// Create a new workspace in `parent`, resolving each of `member_names`
    /// via `Project::create` (mkdir `.ken-workspace`, write the manifest).
    /// If a manifest already exists at `parent` (e.g. a teammate's clone),
    /// it is adopted unchanged, mirroring `Project::create`'s
    /// adopt-if-exists discipline — `member_names` is ignored in that case,
    /// same as `Project::create` ignoring `name` when adopting.
    ///
    /// Every `member_names` entry must already exist as a subfolder of
    /// `parent` (candidates come from `discover_candidates`, which only
    /// lists real folders) — unlike `Workspace::open`, a missing member
    /// here is a hard error, not a `Missing` status, since there is no
    /// prior manifest state to be tolerant of yet.
    pub fn create(parent: &Path, name: &str, member_names: &[String]) -> Result<Workspace> {
        if !parent.is_dir() {
            return Err(Error::ProjectMissing(parent.to_path_buf()));
        }
        if config_path(parent).exists() {
            return Workspace::open(parent);
        }
        let name = project::normalize_name(name)?;
        let member_names: Vec<String> = member_names
            .iter()
            .map(|n| validate_member_name(n))
            .collect::<Result<_>>()?;
        let mut members = Vec::with_capacity(member_names.len());
        for member_name in &member_names {
            let member_root = parent.join(member_name);
            // The leaf is the display name; the full relative path stays
            // the manifest key. A project inside a group folder is still
            // just "ShatteredRealms", not "SR/ShatteredRealms".
            let project = Project::create(&member_root, member_leaf(member_name))?;
            members.push(Member {
                name: member_name.clone(),
                status: MemberStatus::Ok(project),
            });
        }
        let config = WorkspaceConfig {
            name,
            id: Uuid::new_v4(),
            members: member_names.clone(),
            groups: Vec::new(),
            extra: serde_json::Map::new(),
        };
        let workspace = Workspace {
            root: parent.to_path_buf(),
            config,
            members,
        };
        workspace.save()?;
        Ok(workspace)
    }

    /// Open a parent folder that already contains
    /// `.ken-workspace/workspace.json`. Members are resolved relative to
    /// `parent` at open time, so a renamed/moved parent still resolves them
    /// (spec: "moved parent folder still opens"). A member folder missing
    /// from disk yields `MemberStatus::Missing`; a member folder present
    /// but with an unparsable `.ken/project.json` yields
    /// `MemberStatus::Invalid` — neither fails the open.
    pub fn open(parent: &Path) -> Result<Workspace> {
        let path = config_path(parent);
        let raw = fs::read_to_string(&path).map_err(|e| {
            if !parent.is_dir() {
                Error::ProjectMissing(parent.to_path_buf())
            } else {
                Error::io(&path, e)
            }
        })?;
        let config: WorkspaceConfig =
            serde_json::from_str(&raw).map_err(|e| Error::InvalidProject {
                path: path.clone(),
                reason: e.to_string(),
            })?;
        let members = config
            .members
            .iter()
            .map(|name| Self::resolve_member_tolerant(parent, name))
            .collect();
        Ok(Workspace {
            root: parent.to_path_buf(),
            config,
            members,
        })
    }

    fn resolve_member_tolerant(parent: &Path, name: &str) -> Member {
        let member_root = parent.join(name);
        if !member_root.is_dir() {
            return Member {
                name: name.to_string(),
                status: MemberStatus::Missing,
            };
        }
        match Project::create(&member_root, member_leaf(name)) {
            Ok(project) => Member {
                name: name.to_string(),
                status: MemberStatus::Ok(project),
            },
            Err(e) => Member {
                name: name.to_string(),
                status: MemberStatus::Invalid(e.to_string()),
            },
        }
    }

    /// Write the manifest atomically (temp file + rename — see module docs
    /// for why this diverges from `Project::save`'s plain write).
    pub fn save(&self) -> Result<()> {
        let dir = self.root.join(CONFIG_DIR);
        fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
        let path = config_path(&self.root);
        let json = serde_json::to_string_pretty(&self.config)
            .map_err(|e| Error::Other(e.to_string()))?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, json + "\n").map_err(|e| Error::io(&tmp, e))?;
        fs::rename(&tmp, &path).map_err(|e| Error::io(&path, e))
    }
}

/// One immediate subfolder of a prospective workspace parent, as surfaced
/// to the folder-select UI (design D5: "shallow and dumb on purpose" — one
/// level deep, no recursion, no LLM).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    /// Parent-relative path — becomes the manifest's member name if
    /// selected. One segment for a direct child; `Group/Child` for a repo
    /// living inside a group folder (D6).
    pub name: String,
    /// Already has `.ken/project.json` — pre-checked in the selection UI.
    pub existing: bool,
    /// File count directly inside the folder. Not recursive: matches the
    /// "one level deep" discovery philosophy and keeps discovery cheap
    /// across many sibling folders — enough for a "N files" caption without
    /// a full tree walk per candidate.
    pub file_count: usize,
    /// Repo marker file names found directly inside the folder (see
    /// `crate::profiler::REPO_MARKERS`), plus `.git`/`*.sln` when present.
    pub markers: Vec<String>,
}

/// List `parent`'s immediate subfolders as workspace-member candidates.
/// Hidden folders (dot-prefixed — this also excludes `.ken-workspace`
/// itself) and junk build dirs (`node_modules`, `target`, ... —
/// `crate::scan::is_junk_dir_name`, the same table the ingest walk and the
/// profiler use) are never candidates. One level deep only: neither this
/// listing nor a candidate's `file_count`/`markers` recurse into
/// subfolders (D5).
/// The workspace-root `.kenignore` — `<parent>/.kenignore`.
///
/// Same file name, same syntax, same matcher as a project's own
/// `.kenignore`; the only difference is what it governs. A project's file
/// covers paths inside that project; this one covers the parent folder,
/// so it is what excludes SIBLING folders (world data, vendored source
/// drops) from ever being offered as projects. Exactly the relationship a
/// repo-root `.gitignore` has to the tree beneath it.
///
/// Distinct from `.ken-workspace/.kenignore`, which Ken regenerates for
/// the memory pseudo-member and warns against hand-editing. This one is
/// yours.
pub fn ignore_path(parent: &Path) -> PathBuf {
    parent.join(".kenignore")
}

/// Parsed rules from the workspace-root `.kenignore`. Missing file reads
/// as no rules — same tolerance as `Project::kenignore_rules`.
pub fn ignore_rules(parent: &Path) -> Vec<crate::kenignore::Rule> {
    fs::read_to_string(ignore_path(parent))
        .map(|text| crate::kenignore::parse(&text))
        .unwrap_or_default()
}

/// Whether a sibling folder is excluded by the workspace-root
/// `.kenignore`. Only the `Ignore` tier hides a folder outright: a
/// `~search-only` rule is about how much of a file's content gets
/// indexed, which has no meaning for "is this a project".
pub fn is_ignored_folder(rules: &[crate::kenignore::Rule], name: &str) -> bool {
    crate::kenignore::classify(name, true, &[rules]) == crate::kenignore::Tier::Ignore
}

/// Append a folder rule to the workspace-root `.kenignore`, creating the
/// file with a short header if absent. Idempotent — a folder already
/// excluded by ANY existing rule (a glob, not just its own literal line)
/// is left alone rather than adding a redundant duplicate.
pub fn ignore_folder(parent: &Path, folder: &str) -> Result<bool> {
    let folder = folder.trim().trim_end_matches('/');
    // Same shape rule as membership (D6): a bare folder or one inside a
    // single group folder. `validate_member_name` also rejects `..`/drive
    // prefixes, which matters more here — this string is written to disk.
    let folder = &validate_member_name(folder).map_err(|_| {
        Error::Other(
            "only a folder in the workspace (or inside one of its group \
             folders) can be ignored"
                .into(),
        )
    })?;
    if is_ignored_folder(&ignore_rules(parent), folder) {
        return Ok(false);
    }
    let path = ignore_path(parent);
    let mut text = fs::read_to_string(&path).unwrap_or_default();
    if text.is_empty() {
        text.push_str(
            "# Ken ignores these, using .gitignore syntax.\n\
             # `~pattern` indexes a path for search only; `!pattern` forces it back in.\n",
        );
    }
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&format!("{folder}/\n"));
    fs::write(&path, text).map_err(|e| Error::io(&path, e))?;
    Ok(true)
}

/// Remove the plain `folder/` line this module writes. Returns false when
/// no such literal line exists — a folder excluded by a hand-written glob
/// (`sr-universe-*/`) is deliberately NOT rewritten here, because editing
/// someone's glob to carve out one folder is a guess about intent.
pub fn unignore_folder(parent: &Path, folder: &str) -> Result<bool> {
    let folder = folder.trim().trim_end_matches('/');
    let path = ignore_path(parent);
    let Ok(text) = fs::read_to_string(&path) else {
        return Ok(false);
    };
    let target = format!("{folder}/");
    let kept: Vec<&str> = text
        .lines()
        .filter(|line| line.trim() != target && line.trim() != folder)
        .collect();
    if kept.len() == text.lines().count() {
        return Ok(false);
    }
    let mut out = kept.join("\n");
    out.push('\n');
    fs::write(&path, out).map_err(|e| Error::io(&path, e))?;
    Ok(true)
}

pub fn discover_candidates(parent: &Path) -> Result<Vec<Candidate>> {
    let entries = fs::read_dir(parent).map_err(|e| Error::io(parent, e))?;
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();

    let rules = ignore_rules(parent);
    let mut out = Vec::with_capacity(dirs.len());
    for dir in dirs {
        let Some(name) = dir.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.starts_with('.') || crate::scan::is_junk_dir_name(name) {
            continue;
        }
        // The workspace-root `.kenignore` decides what is even a candidate.
        if is_ignored_folder(&rules, name) {
            continue;
        }
        let candidate = scan_candidate(&dir, name)?;
        // D6: a folder that is not repo-ish itself but holds repos is a
        // GROUP folder — offer what's inside it (as `Group/Child` member
        // names) rather than the container. Still bounded: one extra
        // level, only when the evidence says "this wraps projects", so a
        // plain folder of loose notes keeps its old depth-1 candidacy.
        if !candidate.existing && candidate.markers.is_empty() {
            let children = group_folder_children(&dir, name, &rules)?;
            if children.iter().any(|c| c.existing || !c.markers.is_empty()) {
                out.extend(children);
                continue;
            }
        }
        out.push(candidate);
    }
    Ok(out)
}

/// The immediate subfolders of a prospective group folder, as `Group/Child`
/// candidates — same hidden/junk/`.kenignore` filters as the top level, with
/// the ignore check running against the nested relative path so an `SR/`
/// rule hides everything inside `SR` via parent matching.
fn group_folder_children(
    dir: &Path,
    folder: &str,
    rules: &[crate::kenignore::Rule],
) -> Result<Vec<Candidate>> {
    let entries = fs::read_dir(dir).map_err(|e| Error::io(dir, e))?;
    let mut child_dirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    child_dirs.sort();
    let mut out = Vec::new();
    for child in child_dirs {
        let Some(child_name) = child.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if child_name.starts_with('.') || crate::scan::is_junk_dir_name(child_name) {
            continue;
        }
        let rel = format!("{folder}/{child_name}");
        if is_ignored_folder(rules, &rel) {
            continue;
        }
        out.push(scan_candidate(&child, &rel)?);
    }
    Ok(out)
}

/// One level deep into a single candidate: does it have `.ken/project.json`
/// already, how many files sit directly in it, and which repo markers
/// (`crate::profiler::REPO_MARKERS`, `.git`, `*.sln`) does it carry.
fn scan_candidate(dir: &Path, name: &str) -> Result<Candidate> {
    let existing = project::config_path(dir).exists();
    let mut file_count = 0usize;
    let mut markers: Vec<String> = Vec::new();

    // `.git` is itself a hidden dir, so it's checked directly rather than
    // encountered in the (non-hidden) listing below — same reasoning as
    // `profiler::scan_stats`.
    if dir.join(".git").is_dir() {
        markers.push(".git".to_string());
    }

    let entries = fs::read_dir(dir).map_err(|e| Error::io(dir, e))?;
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            continue;
        }
        file_count += 1;
        let Some(fname) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if crate::profiler::REPO_MARKERS.contains(&fname) && !markers.iter().any(|m| m == fname) {
            markers.push(fname.to_string());
        } else if fname.to_ascii_lowercase().ends_with(".sln") && !markers.iter().any(|m| m == "*.sln") {
            markers.push("*.sln".to_string());
        }
    }

    Ok(Candidate {
        name: name.to_string(),
        existing,
        file_count,
        markers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn create_then_open_round_trip() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("alpha")).unwrap();
        fs::create_dir_all(dir.path().join("beta")).unwrap();
        let created =
            Workspace::create(dir.path(), "My Workspace", &["alpha".into(), "beta".into()])
                .unwrap();
        assert!(config_path(dir.path()).exists());
        assert_eq!(created.members.len(), 2);
        assert!(created
            .members
            .iter()
            .all(|m| matches!(m.status, MemberStatus::Ok(_))));

        let reopened = Workspace::open(dir.path()).unwrap();
        assert_eq!(reopened.config, created.config);
        assert_eq!(reopened.members.len(), 2);
        for m in &reopened.members {
            assert!(
                matches!(m.status, MemberStatus::Ok(_)),
                "member {} not ok",
                m.name
            );
        }
    }

    /// A workspace with no groups must serialize exactly as it did before
    /// the field existed — otherwise every existing manifest churns on the
    /// next save.
    #[test]
    fn no_groups_writes_no_groups_key() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("alpha")).unwrap();
        let ws = Workspace::create(dir.path(), "WS", &["alpha".into()]).unwrap();
        ws.save().unwrap();
        let raw = fs::read_to_string(config_path(dir.path())).unwrap();
        assert!(!raw.contains("groups"), "empty groups must not be written: {raw}");
    }

    #[test]
    fn groups_round_trip_and_resolve() {
        let dir = tempdir().unwrap();
        for m in ["realms", "realms-tools", "unrelated"] {
            fs::create_dir_all(dir.path().join(m)).unwrap();
        }
        let mut ws = Workspace::create(
            dir.path(),
            "WS",
            &["realms".into(), "realms-tools".into(), "unrelated".into()],
        )
        .unwrap();

        ws.config
            .set_group("Shattered Realms", &["realms".into(), "realms-tools".into()])
            .unwrap();
        ws.save().unwrap();

        let reopened = Workspace::open(dir.path()).unwrap();
        assert_eq!(
            reopened.config.group_members("Shattered Realms"),
            ["realms", "realms-tools"]
        );
        // Group names are typed by humans — matching is case-insensitive.
        assert_eq!(
            reopened.config.group_members("shattered realms").len(),
            2,
            "group lookup must not be case-sensitive"
        );
        assert_eq!(
            reopened.config.groups_for_member("realms-tools"),
            ["Shattered Realms"]
        );
        assert!(reopened.config.groups_for_member("unrelated").is_empty());
    }

    /// A group naming a folder that is no longer a workspace member
    /// degrades to the members that remain, rather than resolving to a
    /// broken target list.
    #[test]
    fn group_drops_members_that_left_the_workspace() {
        let dir = tempdir().unwrap();
        for m in ["realms", "realms-tools"] {
            fs::create_dir_all(dir.path().join(m)).unwrap();
        }
        let mut ws =
            Workspace::create(dir.path(), "WS", &["realms".into(), "realms-tools".into()]).unwrap();
        ws.config
            .set_group("SR", &["realms".into(), "realms-tools".into()])
            .unwrap();

        // Someone removes a member from the workspace but not from the group.
        ws.config.members.retain(|m| m != "realms-tools");
        assert_eq!(ws.config.group_members("SR"), ["realms"]);
    }

    #[test]
    fn set_group_rejects_empty_name_and_empty_membership() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("alpha")).unwrap();
        let mut ws = Workspace::create(dir.path(), "WS", &["alpha".into()]).unwrap();

        assert!(ws.config.set_group("  ", &["alpha".into()]).is_err());
        assert!(
            ws.config.set_group("Ghosts", &["not-a-member".into()]).is_err(),
            "a group of non-members can never match anything"
        );
        assert!(ws.config.groups.is_empty());
    }

    #[test]
    fn set_group_replaces_and_dedupes() {
        let dir = tempdir().unwrap();
        for m in ["a", "b"] {
            fs::create_dir_all(dir.path().join(m)).unwrap();
        }
        let mut ws = Workspace::create(dir.path(), "WS", &["a".into(), "b".into()]).unwrap();

        ws.config.set_group("G", &["a".into(), "a".into()]).unwrap();
        assert_eq!(ws.config.groups.len(), 1);
        assert_eq!(ws.config.group_members("G"), ["a"], "duplicates collapse");

        // Same name, different case → replaces rather than adding a second.
        ws.config.set_group("g", &["a".into(), "b".into()]).unwrap();
        assert_eq!(ws.config.groups.len(), 1);
        assert_eq!(ws.config.group_members("G"), ["a", "b"]);

        assert!(ws.config.remove_group("G"));
        assert!(!ws.config.remove_group("G"), "second remove is a no-op");
    }

    #[test]
    fn workspace_kenignore_hides_candidates() {
        let dir = tempdir().unwrap();
        for m in ["ken", "worlds", "shared-source"] {
            fs::create_dir_all(dir.path().join(m)).unwrap();
        }

        let before = discover_candidates(dir.path()).unwrap();
        assert_eq!(before.len(), 3);

        assert!(ignore_folder(dir.path(), "worlds").unwrap());
        assert!(!ignore_folder(dir.path(), "worlds").unwrap(), "idempotent");

        let after = discover_candidates(dir.path()).unwrap();
        let names: Vec<&str> = after.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["ken", "shared-source"]);

        assert!(unignore_folder(dir.path(), "worlds").unwrap());
        assert_eq!(discover_candidates(dir.path()).unwrap().len(), 3);
        assert!(!unignore_folder(dir.path(), "worlds").unwrap(), "no-op twice");
    }

    /// The whole point of using `.kenignore` rather than a list: a
    /// hand-written glob works, exactly like `.gitignore`.
    #[test]
    fn hand_written_globs_work_like_gitignore() {
        let dir = tempdir().unwrap();
        for m in ["ken", "sr-universe-current", "sr-universe-2026-08-09"] {
            fs::create_dir_all(dir.path().join(m)).unwrap();
        }
        fs::write(ignore_path(dir.path()), "sr-universe-*/\n").unwrap();

        let names: Vec<String> = discover_candidates(dir.path())
            .unwrap()
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, ["ken"], "a glob must hide every match");

        // And a glob is left alone rather than rewritten to carve one out.
        assert!(
            !unignore_folder(dir.path(), "sr-universe-current").unwrap(),
            "un-ignoring must not edit someone's glob"
        );
    }

    /// A search-only rule says how much of a file to index; it has no
    /// meaning for "is this folder a project", so it must not hide one.
    #[test]
    fn search_only_rule_does_not_hide_a_candidate() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("notes")).unwrap();
        fs::write(ignore_path(dir.path()), "~notes/\n").unwrap();
        assert_eq!(discover_candidates(dir.path()).unwrap().len(), 1);
    }

    /// D6 relaxed the old flat-only rule: ONE level of nesting (a folder
    /// inside a group folder) is now a valid ignore target, the same shape
    /// membership allows. Deeper paths and traversal stay rejected.
    #[test]
    fn ignore_folder_rejects_paths() {
        let dir = tempdir().unwrap();
        assert!(ignore_folder(dir.path(), "a/b").is_ok());
        assert!(ignore_folder(dir.path(), "a/b/c").is_err());
        assert!(ignore_folder(dir.path(), "../escape").is_err());
        assert!(ignore_folder(dir.path(), "  ").is_err());
    }

    #[test]
    fn unknown_fields_survive_roundtrip() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("alpha")).unwrap();
        Workspace::create(dir.path(), "WS", &["alpha".into()]).unwrap();

        // Simulate a newer version adding a field.
        let path = config_path(dir.path());
        let mut v: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        v["ingestRunner"] = "hidden-tui".into();
        fs::write(&path, serde_json::to_string(&v).unwrap()).unwrap();

        let reopened = Workspace::open(dir.path()).unwrap();
        assert_eq!(
            reopened.config.extra.get("ingestRunner").and_then(|x| x.as_str()),
            Some("hidden-tui")
        );
        reopened.save().unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("ingestRunner"), "extra field lost: {raw}");
    }

    #[test]
    fn links_round_trip_through_unknown_keys() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("alpha")).unwrap();
        Workspace::create(dir.path(), "WS", &["alpha".into()]).unwrap();

        // Simulate a newer/other-tool write that adds `links` alongside an
        // unrelated unknown key, same as `unknown_fields_survive_roundtrip`.
        let path = config_path(dir.path());
        let mut v: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        v["links"] = serde_json::json!([
            { "from": "ShatteredRealms", "to": "ShatteredRealmsTools", "relation": "tools_for" },
            { "from": "ShatteredRealms", "to": "Docs", "relation": "documents", "note": "wiki" },
        ]);
        v["ingestRunner"] = "hidden-tui".into();
        fs::write(&path, serde_json::to_string(&v).unwrap()).unwrap();

        let reopened = Workspace::open(dir.path()).unwrap();
        let links = reopened.config.links();
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].from, "ShatteredRealms");
        assert_eq!(links[0].to, "ShatteredRealmsTools");
        assert_eq!(links[0].relation, "tools_for");
        assert_eq!(links[0].note, None);
        assert_eq!(links[1].note.as_deref(), Some("wiki"));

        // Write path preserves both the unrelated unknown key and `links`
        // itself (a manifest saved by this build must not drop either).
        reopened.save().unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("ingestRunner"), "extra field lost: {raw}");
        assert!(raw.contains("ShatteredRealmsTools"), "links lost: {raw}");
    }

    #[test]
    fn linked_projects_resolves_both_directions() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("alpha")).unwrap();
        let mut ws = Workspace::create(dir.path(), "WS", &["alpha".into()]).unwrap();
        ws.config.extra.insert(
            "links".into(),
            serde_json::json!([
                { "from": "ShatteredRealms", "to": "ShatteredRealmsTools", "relation": "tools_for" },
            ]),
        );

        assert_eq!(
            ws.config.linked_projects("ShatteredRealms"),
            vec!["ShatteredRealmsTools"]
        );
        assert_eq!(
            ws.config.linked_projects("ShatteredRealmsTools"),
            vec!["ShatteredRealms"]
        );
        assert!(ws.config.linked_projects("Unrelated").is_empty());
    }

    #[test]
    fn links_absent_when_no_links_key() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("alpha")).unwrap();
        let ws = Workspace::create(dir.path(), "WS", &["alpha".into()]).unwrap();
        assert!(ws.config.links().is_empty());
        assert!(ws.config.linked_projects("alpha").is_empty());
    }

    #[test]
    fn adopt_existing_manifest() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("alpha")).unwrap();
        let first = Workspace::create(dir.path(), "Original", &["alpha".into()]).unwrap();
        // A second create (e.g. teammate opening a cloned folder) adopts.
        let second = Workspace::create(dir.path(), "Renamed", &["alpha".into()]).unwrap();
        assert_eq!(second.config.id, first.config.id);
        assert_eq!(second.config.name, "Original");
    }

    #[test]
    fn missing_member_reported() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("alpha")).unwrap();
        fs::create_dir_all(dir.path().join("beta")).unwrap();
        Workspace::create(dir.path(), "WS", &["alpha".into(), "beta".into()]).unwrap();
        fs::remove_dir_all(dir.path().join("beta")).unwrap();

        let reopened = Workspace::open(dir.path()).unwrap();
        let beta = reopened.members.iter().find(|m| m.name == "beta").unwrap();
        assert!(matches!(beta.status, MemberStatus::Missing));
        let alpha = reopened.members.iter().find(|m| m.name == "alpha").unwrap();
        assert!(matches!(alpha.status, MemberStatus::Ok(_)));
    }

    #[test]
    fn relative_member_resolution_after_parent_rename() {
        let base = tempdir().unwrap();
        let parent = base.path().join("parent");
        fs::create_dir_all(parent.join("alpha")).unwrap();
        let created = Workspace::create(&parent, "WS", &["alpha".into()]).unwrap();
        let original_id = match &created.members[0].status {
            MemberStatus::Ok(p) => p.config.id,
            other => panic!("expected ok member, got {other:?}"),
        };

        let moved = base.path().join("parent-renamed");
        fs::rename(&parent, &moved).unwrap();

        let reopened = Workspace::open(&moved).unwrap();
        match &reopened.members[0].status {
            MemberStatus::Ok(p) => {
                assert_eq!(p.config.id, original_id);
                assert_eq!(p.root, moved.join("alpha"));
            }
            other => panic!("expected ok member, got {other:?}"),
        }
    }

    #[test]
    fn discovery_excludes_junk_and_tags_existing_vs_new() {
        let dir = tempdir().unwrap();
        // An already-adopted Ken project. (`Project::create` adopts an
        // existing folder — it never mkdirs — so the folder comes first.)
        fs::create_dir_all(dir.path().join("existing")).unwrap();
        Project::create(&dir.path().join("existing"), "Existing").unwrap();
        // A plain repo, not yet a Ken project.
        fs::create_dir_all(dir.path().join("repo")).unwrap();
        fs::write(
            dir.path().join("repo").join("Cargo.toml"),
            "[package]\nname = \"x\"\n",
        )
        .unwrap();
        fs::write(dir.path().join("repo").join("main.rs"), "fn main() {}").unwrap();
        // Junk / excluded.
        fs::create_dir_all(dir.path().join("node_modules")).unwrap();
        fs::create_dir_all(dir.path().join(".hidden")).unwrap();
        fs::create_dir_all(dir.path().join(".ken-workspace")).unwrap();

        let candidates = discover_candidates(dir.path()).unwrap();
        let names: Vec<&str> = candidates.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"existing"));
        assert!(names.contains(&"repo"));
        assert!(!names.contains(&"node_modules"));
        assert!(!names.contains(&".hidden"));
        assert!(!names.contains(&".ken-workspace"));

        let existing = candidates.iter().find(|c| c.name == "existing").unwrap();
        assert!(existing.existing);

        let repo = candidates.iter().find(|c| c.name == "repo").unwrap();
        assert!(!repo.existing);
        assert!(repo.markers.contains(&"Cargo.toml".to_string()));
        assert_eq!(repo.file_count, 2);
    }

    #[test]
    fn member_names_validate_to_at_most_two_clean_segments() {
        assert_eq!(validate_member_name("ken").unwrap(), "ken");
        assert_eq!(
            validate_member_name("SR/ShatteredRealms").unwrap(),
            "SR/ShatteredRealms"
        );
        // Windows separators and stray slashes normalize instead of failing.
        assert_eq!(
            validate_member_name("SR\\ShatteredRealms").unwrap(),
            "SR/ShatteredRealms"
        );
        assert_eq!(validate_member_name("/ken/").unwrap(), "ken");
        for bad in ["", "a/b/c", "../escape", "SR/..", "SR/.git", ".hidden", "C:/x"] {
            assert!(validate_member_name(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn group_folders_derive_groups_and_shadow_manifest_names() {
        let mut config = WorkspaceConfig {
            name: "ws".into(),
            id: Uuid::new_v4(),
            members: vec![
                "ken".into(),
                "SR/ShatteredRealms".into(),
                "SR/sr-docs".into(),
            ],
            groups: Vec::new(),
            extra: serde_json::Map::new(),
        };
        let derived = config.derived_groups();
        assert_eq!(derived.len(), 1);
        assert_eq!(derived[0].name, "SR");
        assert_eq!(
            derived[0].members,
            vec!["SR/ShatteredRealms".to_string(), "SR/sr-docs".to_string()]
        );
        assert_eq!(
            config.effective_group_members("sr"),
            vec!["SR/ShatteredRealms".to_string(), "SR/sr-docs".to_string()]
        );
        // The folder owns the name — a manifest group can't squat on it...
        assert!(config.set_group("SR", &["ken".into()]).is_err());
        // ...but a manifest group under a fresh name coexists and resolves
        // through the same effective view.
        config
            .set_group("Everything", &["ken".into(), "SR/sr-docs".into()])
            .unwrap();
        assert_eq!(config.effective_groups().len(), 2);
        assert_eq!(
            config.effective_group_members("everything"),
            vec!["ken".to_string(), "SR/sr-docs".to_string()]
        );
    }

    #[test]
    fn create_gives_nested_members_leaf_display_names() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("SR/Realms")).unwrap();
        fs::create_dir_all(dir.path().join("ken")).unwrap();
        let ws = Workspace::create(
            dir.path(),
            "ws",
            &["SR/Realms".to_string(), "ken".to_string()],
        )
        .unwrap();
        let MemberStatus::Ok(project) = &ws.members[0].status else {
            panic!("nested member did not resolve");
        };
        assert_eq!(project.config.name, "Realms");
        assert_eq!(ws.members[0].name, "SR/Realms");
        // Re-open resolves the same shape from the saved manifest.
        let reopened = Workspace::open(dir.path()).unwrap();
        let MemberStatus::Ok(project) = &reopened.members[0].status else {
            panic!("nested member did not re-resolve");
        };
        assert_eq!(project.config.name, "Realms");
    }

    #[test]
    fn discovery_surfaces_group_folder_children_not_the_container() {
        let dir = tempdir().unwrap();
        // SR wraps two repos (one marked by .git, one by Cargo.toml) plus a
        // junk dir that must not surface.
        fs::create_dir_all(dir.path().join("SR/Realms/.git")).unwrap();
        fs::create_dir_all(dir.path().join("SR/tools")).unwrap();
        fs::write(dir.path().join("SR/tools/Cargo.toml"), "[package]\n").unwrap();
        fs::create_dir_all(dir.path().join("SR/node_modules")).unwrap();
        // A plain folder of loose subfolders stays a depth-1 candidate.
        fs::create_dir_all(dir.path().join("notes/drafts")).unwrap();
        let names: Vec<String> = discover_candidates(dir.path())
            .unwrap()
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert!(names.contains(&"SR/Realms".to_string()));
        assert!(names.contains(&"SR/tools".to_string()));
        assert!(names.contains(&"notes".to_string()));
        assert!(!names.contains(&"SR".to_string()));
        assert!(!names.contains(&"SR/node_modules".to_string()));
    }

    #[test]
    fn ignoring_the_group_folder_hides_its_members_from_discovery() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("SR/Realms/.git")).unwrap();
        fs::write(dir.path().join(".kenignore"), "SR/\n").unwrap();
        let names: Vec<String> = discover_candidates(dir.path())
            .unwrap()
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert!(names.is_empty(), "SR/ rule should hide the whole group: {names:?}");
        // And a nested entry can itself be ignored/unignored.
        let dir2 = tempdir().unwrap();
        fs::create_dir_all(dir2.path().join("SR/scratch")).unwrap();
        assert!(ignore_folder(dir2.path(), "SR/scratch").unwrap());
        assert!(is_ignored_folder(&ignore_rules(dir2.path()), "SR/scratch"));
        assert!(unignore_folder(dir2.path(), "SR/scratch").unwrap());
        assert!(!is_ignored_folder(&ignore_rules(dir2.path()), "SR/scratch"));
    }
}

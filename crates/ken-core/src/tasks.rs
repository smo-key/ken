//! Ken's task board (`openspec/changes/ken-tasks`): task files
//! (`.ken-workspace/tasks/`, `<project>/.ken/tasks/`), the overarching
//! goals that group them (`.ken-workspace/tasks/goals/`), and the pure
//! transitions the board, the chat tools, and `ken-mcp` all share.
//!
//! Everything here is path arithmetic, frontmatter parsing, byte-faithful
//! file patching, and text composition — no `Db`/`IngestEngine`/watcher
//! access (design D3: "UI mutations are commands that call the D1 rewrite
//! core, then let the watcher event round-trip confirm"). Callers own the
//! watcher, the board model, the flag gate, and the clock.
//!
//! ## Frontmatter fidelity — S6 is binding here (unlike `memory.rs`)
//!
//! `features/multi-project/spikes/S6-frontmatter-roundtrip.md` benchmarked
//! two frontmatter patch cores and concluded: use the **raw line splitter**
//! (candidate A, 20/20 byte-exact round trips) for writes, and keep
//! serde_yaml for read-side parsing only. `memory.rs` deliberately did not
//! follow that recommendation (see its module doc) because memory files are
//! whole-file rewrites of a four-key frontmatter. Task files are the
//! opposite case and the spike's own headline consumer: they are hand
//! edited, carry unknown keys, and are patched key-by-key on every
//! drag-drop, so byte fidelity of every untouched line is the contract
//! (spec: "Unknown frontmatter keys and the body SHALL survive every
//! programmatic rewrite byte-for-byte"). So:
//!
//! - **Writes** ([`patch_text`]) go through the raw line splitter. Only the
//!   physical lines of the named keys are replaced; comments, key order,
//!   indentation, quoting style, CRLF terminators, and the body pass
//!   through untouched. Files with no frontmatter block get one prepended
//!   (the case serde_yaml had no fallback for).
//! - **Reads** ([`parse_task`]) use serde_yaml, exactly as S6 permits.
//! - **Concurrency**: S6's unplanned finding was that a naive
//!   read-modify-write loses ~17% of a concurrent external writer's edits.
//!   [`apply_edits`] therefore re-fingerprints the file (len + mtime +
//!   content hash) immediately before writing and retries the whole
//!   read-patch-write cycle on a mismatch.
//! - **Multi-line values**: the S6 prototype left "multiline_value on a
//!   *patched* key" as a TODO (its corpus only patched single-line keys).
//!   [`patch_text`] closes it: replacing a key consumes the key's whole
//!   physical extent — every following indented line, `-` sequence item at
//!   column 0, and interior blank line — so block scalars and block
//!   sequences are replaced as a unit instead of leaving orphaned
//!   continuation lines behind.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

const TASKS_SUBDIR: &str = "tasks";
const GOALS_SUBDIR: &str = "goals";
const ARCHIVE_SUBDIR: &str = "archive";

/// The `## Log` heading `task_complete` reports are appended under (D4).
pub const LOG_HEADING: &str = "## Log";

/// How many read-patch-write cycles [`apply_edits`] will run before giving
/// up on a file another writer keeps changing underneath it (S6's
/// optimistic-concurrency guard: "retry on mismatch").
pub const PATCH_MAX_ATTEMPTS: usize = 4;

// ---------------------------------------------------------------------
// 1.1 / 1.3 Homes and paths
// ---------------------------------------------------------------------

/// `.ken-workspace/tasks/`, relative to the workspace parent folder
/// (`workspace::Workspace::root` — "the folder containing
/// `.ken-workspace/`", not `.ken-workspace/` itself), mirroring
/// `memory::workspace_memory_dir`.
pub fn workspace_tasks_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join(crate::workspace::CONFIG_DIR).join(TASKS_SUBDIR)
}

/// `<project>/.ken/tasks/` — the opt-in per-repo home (D2: "just create
/// the folder — its existence is the opt-in").
pub fn project_tasks_dir(project_root: &Path) -> PathBuf {
    project_root.join(crate::project::CONFIG_DIR).join(TASKS_SUBDIR)
}

/// `.ken-workspace/tasks/goals/` — goals live in the workspace home only
/// (D7), so per-repo tasks reference them by plain id.
pub fn goals_dir(workspace_root: &Path) -> PathBuf {
    workspace_tasks_dir(workspace_root).join(GOALS_SUBDIR)
}

/// `<tasks-home>/archive/YYYY-MM/` — archiving stays inside the task's own
/// home so per-repo history ships with the repo (D2).
pub fn archive_dir(home_dir: &Path, year_month: &str) -> PathBuf {
    home_dir.join(ARCHIVE_SUBDIR).join(year_month)
}

/// Which home a task file lives in. Home is "invisible plumbing" for the
/// board (D2) but load-bearing for two things: archive pathing and the
/// `project` default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HomeKind {
    Workspace,
    Project,
    /// A family board (`<family-clone>/members/<member-id>/board/`) —
    /// ken-families' third task home (design D2 follow-up: "the board scans
    /// both [homes] and treats home as invisible plumbing" extended to a
    /// third). Deliberately a fieldless unit variant like its siblings so
    /// `Task::home` keeps serializing as a plain lowercase string
    /// (`"family"`) rather than changing shape on the wire — the extra data
    /// a family task needs (which board dir, which family's display name)
    /// lives on `TaskHome::Family` at scan time, not here.
    Family,
}

/// A task home to scan. Borrowed like `memory::MemoryScope` so callers
/// never have to know the `.ken`/`.ken-workspace` folder-naming details.
#[derive(Debug, Clone, Copy)]
pub enum TaskHome<'a> {
    Workspace {
        workspace_root: &'a Path,
    },
    /// `project` is the display name used to default the `project`
    /// frontmatter key of tasks that omit it (D2: "a per-repo task needs no
    /// `project` key — defaulted from its home").
    Project {
        project_root: &'a Path,
        project: &'a str,
    },
    /// A family board (ken-families task 2.4's "third home"). Unlike
    /// `Project`, whose `tasks_dir()` derives `<project_root>/.ken/tasks`
    /// from a root, a family board has no such fixed suffix to append — the
    /// caller already resolved `<clone>/members/<member-id>/board` (see
    /// `family::board_dir`) before it has enough context (the family clone
    /// root lives in app data, keyed by a `family_id` this module doesn't
    /// know about) to hand it to `TaskHome`, so this variant takes the
    /// board dir directly rather than re-deriving it.
    Family {
        board_dir: &'a Path,
        /// The family's display name — fills the same `default_project`
        /// role a real project's name would (mirrors `Project::project`).
        family_name: &'a str,
    },
}

impl<'a> TaskHome<'a> {
    pub fn tasks_dir(&self) -> PathBuf {
        match self {
            TaskHome::Workspace { workspace_root } => workspace_tasks_dir(workspace_root),
            TaskHome::Project { project_root, .. } => project_tasks_dir(project_root),
            // The board dir itself IS the listing dir — a family board has
            // no `.ken/tasks` (or similar) subfolder to append, files live
            // directly under `members/<id>/board/` (mirrors `family::
            // board_dir`'s own doc comment: "archive/YYYY-MM/ goes
            // underneath it, exactly as tasks::archive_dir computes for the
            // other two homes").
            TaskHome::Family { board_dir, .. } => board_dir.to_path_buf(),
        }
    }

    pub fn kind(&self) -> HomeKind {
        match self {
            TaskHome::Workspace { .. } => HomeKind::Workspace,
            TaskHome::Project { .. } => HomeKind::Project,
            TaskHome::Family { .. } => HomeKind::Family,
        }
    }

    /// The `project` value a task in this home inherits when its own
    /// frontmatter leaves the key empty.
    pub fn default_project(&self) -> &str {
        match self {
            TaskHome::Workspace { .. } => "",
            TaskHome::Project { project, .. } => project,
            TaskHome::Family { family_name, .. } => family_name,
        }
    }
}

// ---------------------------------------------------------------------
// 1.1 Enums
// ---------------------------------------------------------------------

/// Board columns, left to right. `Backlog` is the intake column (D7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Backlog,
    Todo,
    Doing,
    Review,
    Done,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Backlog => "backlog",
            TaskStatus::Todo => "todo",
            TaskStatus::Doing => "doing",
            TaskStatus::Review => "review",
            TaskStatus::Done => "done",
        }
    }

    /// Case-insensitive parse. `None` for anything not in the vocabulary —
    /// the caller decides whether that means "needs attention" (a parsed
    /// file) or "reject" (a tool argument).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "backlog" => Some(TaskStatus::Backlog),
            "todo" => Some(TaskStatus::Todo),
            "doing" => Some(TaskStatus::Doing),
            "review" => Some(TaskStatus::Review),
            "done" => Some(TaskStatus::Done),
            _ => None,
        }
    }

    pub const ALL: [TaskStatus; 5] = [
        TaskStatus::Backlog,
        TaskStatus::Todo,
        TaskStatus::Doing,
        TaskStatus::Review,
        TaskStatus::Done,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskKind {
    Human,
    Ai,
}

impl TaskKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskKind::Human => "human",
            TaskKind::Ai => "ai",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "human" => Some(TaskKind::Human),
            "ai" => Some(TaskKind::Ai),
            _ => None,
        }
    }
}

/// Which board a task renders on (D5: the daily board is "a filter plus
/// two rituals", not a separate format).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BoardKind {
    Main,
    Daily,
}

impl BoardKind {
    pub fn as_str(self) -> &'static str {
        match self {
            BoardKind::Main => "main",
            BoardKind::Daily => "daily",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "main" => Some(BoardKind::Main),
            "daily" => Some(BoardKind::Daily),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GoalStatus {
    Active,
    Done,
    Dropped,
}

impl GoalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            GoalStatus::Active => "active",
            GoalStatus::Done => "done",
            GoalStatus::Dropped => "dropped",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "active" => Some(GoalStatus::Active),
            "done" => Some(GoalStatus::Done),
            "dropped" => Some(GoalStatus::Dropped),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------
// 1.1 Frontmatter read model
// ---------------------------------------------------------------------

/// The modeled frontmatter keys, in canonical emit order. Used to decide
/// what counts as an "unknown key" in [`Task::extra`] and to order keys
/// appended to a file that didn't have them.
const KNOWN_KEYS: &[&str] = &[
    "id", "title", "status", "kind", "assignee", "project", "tags", "due", "goal", "board",
    "created", "updated",
];

/// Tolerant read-side frontmatter (S6: serde_yaml is fine for reads).
/// Enum-valued keys are modeled as `String` on purpose — a hand-edited
/// `status: blocked` must degrade to a needs-attention entry, not fail the
/// whole file's parse.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct TaskFrontmatter {
    #[serde(default)]
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    assignee: String,
    #[serde(default)]
    project: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    due: String,
    #[serde(default)]
    goal: String,
    #[serde(default)]
    board: String,
    #[serde(default)]
    created: String,
    #[serde(default)]
    updated: String,
    #[serde(flatten)]
    extra: serde_yaml::Mapping,
}

/// A parsed task file. `id` is authoritative over the filename (D1), so
/// everything downstream keys on it and renaming a file changes nothing.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: String,
    pub title: String,
    /// `None` when the file's `status` is present but outside the
    /// vocabulary — the needs-attention case. An absent/empty `status`
    /// reads as `Backlog`, the intake column (D7).
    pub status: Option<TaskStatus>,
    /// Exactly what the file said, so the tray can show it and so nothing
    /// silently normalizes it.
    pub status_raw: String,
    /// The resolved pipeline lane id (ken-pipeline D2), or `None` for a
    /// ticket with no `pipeline:` key — the classic path.
    ///
    /// [`parse_task`] always leaves this `None`, because a lane can only be
    /// resolved against a loaded pipeline definition. `pipeline::
    /// resolve_task_lane` (and its board-wide sibling) is the *single* home
    /// of the board-scoped vocabulary check; it fills this in and rewrites
    /// [`Task::status`] from the matched lane's `maps_to`. Nothing else
    /// parses a lane.
    pub lane: Option<String>,
    pub kind: TaskKind,
    pub kind_raw: String,
    pub assignee: String,
    pub project: String,
    pub tags: Vec<String>,
    pub due: Option<String>,
    pub goal: Option<String>,
    pub board: BoardKind,
    pub board_raw: String,
    pub created: String,
    pub updated: String,
    pub body: String,
    pub path: PathBuf,
    pub home: HomeKind,
    /// The `tasks/` directory this task lives in — where its
    /// `archive/YYYY-MM/` goes.
    pub home_dir: PathBuf,
    /// Unknown frontmatter keys, preserved for display. Writes never
    /// rebuild frontmatter from this — [`patch_text`] leaves the original
    /// lines untouched — so this is read-side only.
    #[serde(skip)]
    extra: serde_yaml::Mapping,
}

impl Task {
    pub fn extra(&self) -> &serde_yaml::Mapping {
        &self.extra
    }

    /// The file name (`<ulid>-<slug>.md`).
    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// Path relative to the member root that owns this task — the tail of
    /// its `ken://` address.
    pub fn address_rel_path(&self) -> String {
        match self.home {
            HomeKind::Workspace => format!("{TASKS_SUBDIR}/{}", self.file_name()),
            HomeKind::Project => format!(
                "{}/{TASKS_SUBDIR}/{}",
                crate::project::CONFIG_DIR,
                self.file_name()
            ),
            // `home_dir` for a family task is always exactly the board dir
            // `family::board_dir(clone_root, member_id)` built
            // (`TaskHome::Family::tasks_dir()` returns it unmodified, and
            // `parse_task` sets `home_dir` from the file's own parent), i.e.
            // `<clone-root>/members/<member-id>/board`. So the member id is
            // recoverable as `home_dir`'s grandparent-relative directory
            // name — the same "matched by directory, not by any field the
            // task carries" posture `ken-mcp`'s own `family_origin`/`host_
            // for` use, just without that caller's live connection list to
            // cross-check against. `family::board_rel` then composes the
            // exact same `members/<id>/board` shape ken-mcp's own override
            // uses, so a family task's address ends up byte-identical
            // whichever caller renders it.
            HomeKind::Family => {
                let member_id = self
                    .home_dir
                    .parent()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                format!("{}/{}", crate::family::board_rel(&member_id), self.file_name())
            }
        }
    }

    /// `ken://<host>/<rel-path>` (`routing::ken_address`'s fixed scheme).
    /// `host` is `memory::WORKSPACE_ADDRESS_ID` for workspace-home tasks
    /// and the owning project's id for per-repo tasks — this module has no
    /// `Project` handle, so the caller supplies it.
    pub fn address(&self, host: &str) -> String {
        format!("ken://{host}/{}", self.address_rel_path())
    }

    /// True when the file's `status` was present but unrecognized. Such a
    /// file is shown in the needs-attention tray and never rewritten by a
    /// patch that doesn't explicitly resolve the status (spec: "the file is
    /// not rewritten").
    pub fn has_invalid_status(&self) -> bool {
        self.status.is_none()
    }
}

/// Split `---\n ... \n---\n` off the front of a file's raw text, keeping
/// every byte addressable. Unlike `memory::split_frontmatter` (which only
/// needs the two halves) this retains the opening/closing delimiter lines
/// verbatim so the patch core can reassemble the file without touching
/// them.
struct Split<'a> {
    /// `"---\n"` or `"---\r\n"`.
    open: &'a str,
    /// The frontmatter lines, each with its own terminator.
    fm: &'a str,
    /// The closing `---` line, with its terminator (may be missing at EOF).
    close: &'a str,
    /// Everything after the closing delimiter, byte-for-byte.
    body: &'a str,
}

fn split_raw(raw: &str) -> Option<Split<'_>> {
    let open_len = if raw.starts_with("---\r\n") {
        5
    } else if raw.starts_with("---\n") {
        4
    } else {
        return None;
    };
    let rest = &raw[open_len..];
    let mut off = 0usize;
    loop {
        let line_end = rest[off..]
            .find('\n')
            .map(|i| off + i + 1)
            .unwrap_or(rest.len());
        let line = &rest[off..line_end];
        if line.trim_end_matches('\n').trim_end_matches('\r') == "---" {
            return Some(Split {
                open: &raw[..open_len],
                fm: &rest[..off],
                close: line,
                body: &rest[line_end..],
            });
        }
        if line_end >= rest.len() {
            return None; // unterminated frontmatter — treat as no frontmatter
        }
        off = line_end;
    }
}

/// The first non-empty line of `body` with leading `#`s trimmed — the
/// title fallback for a hand-written file with no `title:` key (same
/// posture as `memory::first_line`).
fn first_line(body: &str) -> String {
    body.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.trim_start_matches('#').trim().to_string())
        .unwrap_or_default()
}

/// `(frontmatter, body)` for sibling modules that parse the same file
/// shape through this module's splitter (pipeline definitions in
/// `pipeline.rs`). Deliberately narrower than [`split_raw`]: read-side
/// callers never need the delimiter lines, and only [`patch_text`] may
/// reassemble a file.
pub(crate) fn split_fm_body(raw: &str) -> (Option<&str>, &str) {
    match split_raw(raw) {
        Some(s) => (Some(s.fm), s.body),
        None => (None, raw),
    }
}

pub(crate) fn map_str(m: &serde_yaml::Mapping, key: &str) -> String {
    match m.get(serde_yaml::Value::String(key.to_string())) {
        Some(serde_yaml::Value::String(s)) => s.clone(),
        Some(serde_yaml::Value::Number(n)) => n.to_string(),
        Some(serde_yaml::Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

pub(crate) fn map_list(m: &serde_yaml::Mapping, key: &str) -> Vec<String> {
    match m.get(serde_yaml::Value::String(key.to_string())) {
        Some(serde_yaml::Value::Sequence(items)) => items
            .iter()
            .filter_map(|v| match v {
                serde_yaml::Value::String(s) => Some(s.trim().to_string()),
                serde_yaml::Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .filter(|s| !s.is_empty())
            .collect(),
        // A hand edit like `tags: alpha, beta` is a scalar, not a list.
        Some(serde_yaml::Value::String(s)) => s
            .split(',')
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

/// Parse the frontmatter block. The typed `#[serde(default)]` struct
/// (tasks.md 1.1) is the happy path; when a hand edit makes the *typed*
/// shape fail (e.g. `tags:` written as a comma string) we fall back to a
/// tolerant `Mapping` read instead of `unwrap_or_default()`-ing the whole
/// file into blanks. Either way this is read-side only.
fn parse_frontmatter(fm: &str) -> TaskFrontmatter {
    if let Ok(parsed) = serde_yaml::from_str::<TaskFrontmatter>(fm) {
        return parsed;
    }
    let Ok(map) = serde_yaml::from_str::<serde_yaml::Mapping>(fm) else {
        return TaskFrontmatter::default();
    };
    let mut extra = map.clone();
    for k in KNOWN_KEYS {
        extra.remove(serde_yaml::Value::String((*k).to_string()));
    }
    TaskFrontmatter {
        id: map_str(&map, "id"),
        title: map_str(&map, "title"),
        status: map_str(&map, "status"),
        kind: map_str(&map, "kind"),
        assignee: map_str(&map, "assignee"),
        project: map_str(&map, "project"),
        tags: map_list(&map, "tags"),
        due: map_str(&map, "due"),
        goal: map_str(&map, "goal"),
        board: map_str(&map, "board"),
        created: map_str(&map, "created"),
        updated: map_str(&map, "updated"),
        extra,
    }
}

/// Parse a task file's raw text. Infallible and tolerant: no frontmatter,
/// malformed YAML, or an out-of-vocabulary `status`/`kind`/`board` all
/// degrade to a renderable task rather than an error (spec: "invalid hand
/// edit is surfaced, not destroyed").
///
/// `default_project` is the owning home's project name (empty for the
/// workspace home) and only applies when the file leaves `project` empty.
pub fn parse_task(path: &Path, home: HomeKind, default_project: &str, raw: &str) -> Task {
    let (fm, body) = match split_raw(raw) {
        Some(s) => (parse_frontmatter(s.fm), s.body.to_string()),
        None => (TaskFrontmatter::default(), raw.to_string()),
    };
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    // `id` is authoritative over the filename; a file with no `id` yet
    // (hand-created) falls back to its stem so it still shows on the board
    // with a stable identity.
    let id = if fm.id.trim().is_empty() {
        stem.clone()
    } else {
        fm.id.trim().to_string()
    };
    let body_trimmed = body.trim().to_string();
    let title = if fm.title.trim().is_empty() {
        first_line(&body_trimmed)
    } else {
        fm.title.trim().to_string()
    };

    // Absent ⇒ the documented default; present-but-unrecognized ⇒ flagged.
    let status = if fm.status.trim().is_empty() {
        Some(TaskStatus::Backlog)
    } else {
        TaskStatus::parse(&fm.status)
    };
    let kind_parsed = TaskKind::parse(&fm.kind);
    let board_parsed = BoardKind::parse(&fm.board);

    let home_dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let project = if fm.project.trim().is_empty() {
        default_project.to_string()
    } else {
        fm.project.trim().to_string()
    };

    Task {
        id,
        title,
        status,
        status_raw: fm.status.trim().to_string(),
        // Classic path only. Lane resolution needs the pipeline
        // definitions, which this function has no access to on purpose —
        // see `pipeline::resolve_task_lane`.
        lane: None,
        kind: kind_parsed.unwrap_or(TaskKind::Human),
        kind_raw: fm.kind.trim().to_string(),
        assignee: fm.assignee.trim().to_string(),
        project,
        tags: fm
            .tags
            .iter()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect(),
        due: non_empty(&fm.due),
        goal: non_empty(&fm.goal),
        board: board_parsed.unwrap_or(BoardKind::Main),
        board_raw: fm.board.trim().to_string(),
        created: fm.created.trim().to_string(),
        updated: fm.updated.trim().to_string(),
        body: body_trimmed,
        path: path.to_path_buf(),
        home,
        home_dir,
        extra: fm.extra,
    }
}

fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

// ---------------------------------------------------------------------
// 1.2 Raw line-splitter patch core (S6 candidate A)
// ---------------------------------------------------------------------

/// YAML plain scalars that would resolve to something other than a string.
const YAML_AMBIGUOUS: &[&str] = &[
    "true", "false", "yes", "no", "on", "off", "null", "nil", "~", "y", "n",
];

/// Conservative: a value is emitted bare only when it is unambiguously a
/// plain string (starts with an ASCII letter or `_`, contains only
/// alphanumerics/`_-./ `, no trailing space, not a YAML keyword). Anything
/// else — dates, numbers, ids with `:`, empty strings — is single-quoted,
/// which is always safe.
fn scalar_needs_quoting(s: &str) -> bool {
    if s.is_empty() {
        return true;
    }
    if YAML_AMBIGUOUS.contains(&s.to_ascii_lowercase().as_str()) {
        return true;
    }
    let first = s.chars().next().unwrap_or(' ');
    if !(first.is_ascii_alphabetic() || first == '_') {
        return true;
    }
    if s.ends_with(' ') {
        return true;
    }
    !s.chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ' '))
}

/// Render a value as a single-line YAML scalar. Embedded newlines are
/// folded to spaces: the patch core is line-based, so a value carrying a
/// raw newline would desynchronize the block. Task titles/assignees are
/// single-line by construction; this is a guard, not a feature.
fn render_scalar(s: &str) -> String {
    let flat = s.replace("\r\n", " ").replace(['\n', '\r'], " ");
    if scalar_needs_quoting(&flat) {
        format!("'{}'", flat.replace('\'', "''"))
    } else {
        flat
    }
}

/// `key: value` as one physical line. Public because [`patch_text`] and
/// [`apply_edits`] take *rendered* lines — a caller patching a key this
/// module doesn't model (a future frontmatter key, an unknown key a tool
/// wants to set) needs the same quoting rules.
pub fn scalar_lines(key: &str, value: &str) -> Vec<String> {
    vec![format!("{key}: {}", render_scalar(value))]
}

/// A block sequence (`key:` + `  - item` lines), or `key: []` when empty.
/// Multi-line by design — this is the case the S6 prototype's TODO was
/// about on the *write* side.
pub fn seq_lines(key: &str, items: &[String]) -> Vec<String> {
    if items.is_empty() {
        return vec![format!("{key}: []")];
    }
    let mut out = vec![format!("{key}:")];
    for item in items {
        out.push(format!("  - {}", render_scalar(item)));
    }
    out
}

/// Split text into `(content, terminator)` pairs. The terminator is `""`
/// only for a final line with no newline, so joining the pairs back
/// reproduces the input byte-for-byte.
fn split_lines(s: &str) -> Vec<(&str, &str)> {
    let mut out = Vec::new();
    let mut off = 0usize;
    while off < s.len() {
        let end = s[off..].find('\n').map(|i| off + i + 1).unwrap_or(s.len());
        let line = &s[off..end];
        let pair = if let Some(stripped) = line.strip_suffix("\r\n") {
            (stripped, &line[line.len() - 2..])
        } else if let Some(stripped) = line.strip_suffix('\n') {
            (stripped, &line[line.len() - 1..])
        } else {
            (line, "")
        };
        out.push(pair);
        off = end;
    }
    out
}

/// The key a top-level frontmatter line declares, if any. Indented lines,
/// comments, blank lines, and sequence items are all `None` — only column-0
/// `key:` / `key: value` lines are patch targets, so nested mappings are
/// never mistaken for the keys we own.
fn top_level_key(content: &str) -> Option<&str> {
    if content.starts_with(' ') || content.starts_with('\t') {
        return None;
    }
    let trimmed = content.trim_end();
    if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('-') {
        return None;
    }
    let colon = trimmed.find(':')?;
    let key = &trimmed[..colon];
    if key.is_empty()
        || !key
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        return None;
    }
    let after = &trimmed[colon + 1..];
    if after.is_empty() || after.starts_with(' ') || after.starts_with('\t') {
        Some(key)
    } else {
        None
    }
}

/// The dominant line terminator of a file — CRLF files stay CRLF (S6:
/// candidate A "never rewrites terminators of untouched lines"; newly
/// *added* lines have to pick one, and matching the file is the only
/// answer that keeps a diff clean).
fn detect_eol(raw: &str) -> &'static str {
    match raw.find('\n') {
        Some(i) if i > 0 && raw.as_bytes()[i - 1] == b'\r' => "\r\n",
        _ => "\n",
    }
}

/// The byte-faithful patch core (S6 candidate A). Replaces the physical
/// lines of each named key in `edits`, appends keys the file doesn't have
/// yet (in `edits` order), and optionally appends text to the end of the
/// body. Everything else — comments, key order, indentation, quoting
/// style, unknown keys, terminators, and the body — passes through
/// untouched.
///
/// A file with no frontmatter block gets one prepended (the case
/// serde_yaml had no fallback for in S6); its whole content becomes the
/// body, unchanged.
pub fn patch_text(raw: &str, edits: &[(&str, Vec<String>)], append_body: Option<&str>) -> String {
    let eol = detect_eol(raw);

    let (open, fm, close, body) = match split_raw(raw) {
        Some(s) => (
            s.open.to_string(),
            s.fm.to_string(),
            s.close.to_string(),
            s.body.to_string(),
        ),
        None => (
            format!("---{eol}"),
            String::new(),
            format!("---{eol}{eol}"),
            raw.to_string(),
        ),
    };

    let lines = split_lines(&fm);
    let mut written = vec![false; edits.len()];
    let mut out = String::with_capacity(raw.len() + 128);
    let mut i = 0usize;

    while i < lines.len() {
        let (content, term) = lines[i];
        let hit = top_level_key(content).and_then(|k| edits.iter().position(|(ek, _)| *ek == k));
        match hit {
            Some(idx) if !written[idx] => {
                let t = if term.is_empty() { eol } else { term };
                for line in &edits[idx].1 {
                    out.push_str(line);
                    out.push_str(t);
                }
                written[idx] = true;
                // Consume this key's whole physical extent: following
                // indented lines, column-0 sequence items, and interior
                // blank lines (S6's multi-line-value TODO). Trailing blank
                // lines before the next key stay where they are.
                i += 1;
                let mut j = i;
                let mut last = i;
                while j < lines.len() {
                    let c = lines[j].0;
                    if c.trim().is_empty() {
                        j += 1;
                        continue;
                    }
                    let continuation =
                        c.starts_with(' ') || c.starts_with('\t') || c.starts_with('-');
                    if continuation {
                        j += 1;
                        last = j;
                    } else {
                        break;
                    }
                }
                i = last;
            }
            _ => {
                out.push_str(content);
                out.push_str(term);
                i += 1;
            }
        }
    }

    for (idx, (_, value_lines)) in edits.iter().enumerate() {
        if written[idx] {
            continue;
        }
        for line in value_lines {
            out.push_str(line);
            out.push_str(eol);
        }
    }

    let mut result = String::with_capacity(raw.len() + 256);
    result.push_str(&open);
    result.push_str(&out);
    result.push_str(&close);
    result.push_str(&body);

    if let Some(extra) = append_body {
        if !result.ends_with('\n') {
            result.push_str(eol);
        }
        if !result.ends_with(&format!("{eol}{eol}")) {
            result.push_str(eol);
        }
        result.push_str(&extra.replace("\r\n", "\n").replace('\n', eol));
        if !result.ends_with(eol) {
            result.push_str(eol);
        }
    }
    result
}

/// len + mtime + content hash — the optimistic-concurrency precondition
/// S6 requires. mtime alone is too coarse (Windows FAT/NTFS granularity
/// and same-millisecond writes), so the content hash is the real check and
/// the metadata is the cheap early-out.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Fingerprint {
    len: u64,
    mtime_ms: Option<u128>,
    hash: u64,
}

fn fingerprint(path: &Path) -> Result<(String, Fingerprint)> {
    let raw = fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
    let meta = fs::metadata(path).map_err(|e| Error::io(path, e))?;
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis());
    let fp = Fingerprint {
        len: meta.len(),
        mtime_ms,
        hash: twox_hash::XxHash64::oneshot(0x4B454E5F5441534B, raw.as_bytes()), // "KEN_TASK"
    };
    Ok((raw, fp))
}

/// A body-append computed from the *current* body, so a retry recomputes
/// it against whatever the other writer left behind (see
/// [`complete_task`], which only adds a `## Log` heading if the body
/// doesn't already have one).
pub type BodyAppend<'a> = &'a dyn Fn(&str) -> String;

/// Read → patch → verify-unchanged → write, retrying up to
/// [`PATCH_MAX_ATTEMPTS`] times when the file changed underneath us (S6:
/// "read-then-write alone drops ~1 in 6 concurrent external edits").
///
/// A patch that produces byte-identical text writes nothing at all, so a
/// no-op update never wakes the watcher.
pub fn apply_edits(
    path: &Path,
    edits: &[(&str, Vec<String>)],
    append_body: Option<BodyAppend>,
) -> Result<()> {
    for _ in 0..PATCH_MAX_ATTEMPTS {
        let (raw, before) = fingerprint(path)?;
        let appended = append_body.map(|f| {
            let body = split_raw(&raw).map(|s| s.body.to_string()).unwrap_or_else(|| raw.clone());
            f(&body)
        });
        let next = patch_text(&raw, edits, appended.as_deref());
        if next == raw {
            return Ok(());
        }
        let (_, now) = fingerprint(path)?;
        if now != before {
            continue; // someone else wrote between our read and our write
        }
        fs::write(path, &next).map_err(|e| Error::io(path, e))?;
        return Ok(());
    }
    Err(Error::Other(format!(
        "task file changed concurrently {PATCH_MAX_ATTEMPTS} times, giving up: {}",
        path.display()
    )))
}

// ---------------------------------------------------------------------
// 1.2 Typed patches
// ---------------------------------------------------------------------

/// The keys a `task_update` may name. `None` means "leave alone" — the
/// whole point of the patch core is that unnamed keys are never touched.
/// There is deliberately no "remove key" variant: clearing a value means
/// setting it empty (`assignee: ''`), which keeps the line — and therefore
/// the file's key order — stable.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TaskPatch {
    pub title: Option<String>,
    pub status: Option<TaskStatus>,
    pub kind: Option<TaskKind>,
    pub assignee: Option<String>,
    pub project: Option<String>,
    pub tags: Option<Vec<String>>,
    pub due: Option<String>,
    pub goal: Option<String>,
    pub board: Option<BoardKind>,

    // -- ken-pipeline (tasks.md 1.5): the *writable* ticket keys. All ride
    // the existing `extra` flatten on the read side, so today's Ken
    // round-trips them untouched, and all are rendered through
    // `scalar_lines`/`seq_lines` like every other key.
    //
    // Read-only-by-design and therefore absent here: nothing. `pipeline`
    // is writable so a ticket can be opted in; `bounces`/`return_lane`/
    // `blocked_at` are writable only because `pipeline::advance` and
    // `pipeline::block` compose them — no UI or tool should set them
    // directly.
    /// The lane id to write into the `status` key (D2: one `status` key).
    /// Mutually exclusive with [`TaskPatch::status`]; see [`TaskPatch::
    /// edits`].
    pub lane: Option<String>,
    pub pipeline: Option<String>,
    pub model: Option<String>,
    pub agent: Option<String>,
    pub scope: Option<Vec<String>>,
    pub verify: Option<String>,
    pub bounces: Option<u32>,
    pub return_lane: Option<String>,
    pub blocked_by: Option<Vec<String>>,
    pub block_reason: Option<String>,
    pub blocked_at: Option<String>,
    pub parent: Option<String>,
    pub spawned_by: Option<String>,
    pub origin: Option<String>,
    pub projects: Option<Vec<String>>,
    pub target: Option<String>,
}

impl TaskPatch {
    pub fn is_empty(&self) -> bool {
        *self == TaskPatch::default()
    }

    /// True when this patch resolves the `status` key one way or the other
    /// — the escape hatch [`apply_patch`] allows for a ticket whose
    /// on-disk status it doesn't understand.
    pub fn sets_status(&self) -> bool {
        self.status.is_some() || self.lane.is_some()
    }

    /// Rendered edit lines in canonical key order. `updated` is appended by
    /// [`apply_patch`], not here, so this stays a pure view of the caller's
    /// intent.
    fn edits(&self) -> Vec<(&'static str, Vec<String>)> {
        let mut out: Vec<(&'static str, Vec<String>)> = Vec::new();
        if let Some(v) = &self.title {
            out.push(("title", scalar_lines("title", v)));
        }
        // One physical `status` key (D2), two vocabularies. `lane` wins
        // when both are set, because the board-scoped vocabulary is the
        // more specific statement of intent; setting both is a caller bug.
        if let Some(v) = &self.lane {
            out.push(("status", scalar_lines("status", v)));
        } else if let Some(v) = self.status {
            out.push(("status", scalar_lines("status", v.as_str())));
        }
        if let Some(v) = self.kind {
            out.push(("kind", scalar_lines("kind", v.as_str())));
        }
        if let Some(v) = &self.assignee {
            out.push(("assignee", scalar_lines("assignee", v)));
        }
        if let Some(v) = &self.project {
            out.push(("project", scalar_lines("project", v)));
        }
        if let Some(v) = &self.tags {
            out.push(("tags", seq_lines("tags", v)));
        }
        if let Some(v) = &self.due {
            out.push(("due", scalar_lines("due", v)));
        }
        if let Some(v) = &self.goal {
            out.push(("goal", scalar_lines("goal", v)));
        }
        if let Some(v) = self.board {
            out.push(("board", scalar_lines("board", v.as_str())));
        }
        // ken-pipeline keys, appended after the classic ones so a file
        // that doesn't have them yet grows them in a stable order.
        if let Some(v) = &self.pipeline {
            out.push(("pipeline", scalar_lines("pipeline", v)));
        }
        if let Some(v) = &self.model {
            out.push(("model", scalar_lines("model", v)));
        }
        if let Some(v) = &self.agent {
            out.push(("agent", scalar_lines("agent", v)));
        }
        if let Some(v) = &self.scope {
            out.push(("scope", seq_lines("scope", v)));
        }
        if let Some(v) = &self.verify {
            out.push(("verify", scalar_lines("verify", v)));
        }
        if let Some(v) = self.bounces {
            out.push(("bounces", scalar_lines("bounces", &v.to_string())));
        }
        if let Some(v) = &self.return_lane {
            out.push(("return_lane", scalar_lines("return_lane", v)));
        }
        if let Some(v) = &self.blocked_by {
            out.push(("blocked_by", seq_lines("blocked_by", v)));
        }
        if let Some(v) = &self.block_reason {
            out.push(("block_reason", scalar_lines("block_reason", v)));
        }
        if let Some(v) = &self.blocked_at {
            out.push(("blocked_at", scalar_lines("blocked_at", v)));
        }
        if let Some(v) = &self.parent {
            out.push(("parent", scalar_lines("parent", v)));
        }
        if let Some(v) = &self.spawned_by {
            out.push(("spawned_by", scalar_lines("spawned_by", v)));
        }
        if let Some(v) = &self.origin {
            out.push(("origin", scalar_lines("origin", v)));
        }
        if let Some(v) = &self.projects {
            out.push(("projects", seq_lines("projects", v)));
        }
        if let Some(v) = &self.target {
            out.push(("target", scalar_lines("target", v)));
        }
        out
    }
}

/// Rewrite only the patch's named keys plus `updated` (spec: "patches SHALL
/// rewrite only the named keys plus `updated`"; drag-drop = `status` +
/// `updated`).
///
/// Refuses to touch a file whose on-disk `status` is out of vocabulary
/// unless the patch itself sets `status` — that is what "the file is not
/// rewritten" means for a needs-attention task: Ken never edits around a
/// hand edit it doesn't understand, but an explicit fix is always allowed.
pub fn apply_patch(path: &Path, patch: &TaskPatch, updated: &str) -> Result<()> {
    apply_patch_with_pipelines(path, patch, updated, &[])
}

/// [`apply_patch`] with the loaded pipeline definitions, so a pipeline
/// ticket's lane id is recognised as a valid `status` (D2) and an
/// *unknown* lane earns the same refusal an invalid classic status does
/// (tasks.md 1.4).
///
/// Note the asymmetry this creates on purpose: called with no definitions
/// (which is what [`apply_patch`] does, and therefore what every
/// pre-ken-pipeline caller does), a ticket sitting in a pipeline lane
/// looks exactly like a ticket with an unrecognized status and is refused.
/// That is the conservative direction — a caller that doesn't know about
/// pipelines has no business editing around a lane it can't validate.
pub fn apply_patch_with_pipelines(
    path: &Path,
    patch: &TaskPatch,
    updated: &str,
    pipelines: &[crate::pipeline::Pipeline],
) -> Result<()> {
    let raw = fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
    let mut current = parse_task(path, HomeKind::Workspace, "", &raw);
    crate::pipeline::resolve_task_lane(&mut current, pipelines);
    if current.has_invalid_status() && !patch.sets_status() {
        let what = match crate::pipeline::ticket_pipeline(&current) {
            Some(p) => format!("status '{}' in pipeline '{p}'", current.status_raw),
            None => format!("status '{}'", current.status_raw),
        };
        return Err(Error::Other(format!(
            "task '{}' has an unrecognized {what} — resolve it before patching other keys",
            current.id
        )));
    }
    let mut edits = patch.edits();
    edits.push(("updated", scalar_lines("updated", updated)));
    apply_edits(path, &edits, None)
}

// ---------------------------------------------------------------------
// 1.1 / 1.6 Creation
// ---------------------------------------------------------------------

/// Crockford base32 (no I, L, O, U) — the ULID alphabet.
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Encode a ULID from its parts: 48-bit millisecond timestamp + 80 bits of
/// entropy, 26 Crockford-base32 characters, most significant first.
pub fn ulid_from_parts(unix_ms: u64, entropy: [u8; 10]) -> String {
    let mut s = String::with_capacity(26);
    let ts = unix_ms & 0xFFFF_FFFF_FFFF;
    for i in 0..10 {
        let shift = 45 - 5 * i;
        s.push(CROCKFORD[((ts >> shift) & 0x1F) as usize] as char);
    }
    let mut rand: u128 = 0;
    for b in entropy {
        rand = (rand << 8) | b as u128;
    }
    for i in 0..16 {
        let shift = 75 - 5 * i;
        s.push(CROCKFORD[((rand >> shift) & 0x1F) as usize] as char);
    }
    s
}

/// A fresh ULID from the wall clock plus 80 bits of `Uuid::new_v4`
/// randomness. No `ulid` crate is in this workspace's dependency tree and
/// adding one for 20 lines of base32 isn't worth it; `uuid` (already a
/// dependency, `v4` feature) is the entropy source.
///
/// Tests never call this — [`NewTask::id`] lets the caller supply the id,
/// the same caller-supplies-nondeterminism convention `memory.rs` uses for
/// dates.
pub fn new_ulid() -> String {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let bytes = *uuid::Uuid::new_v4().as_bytes();
    let mut entropy = [0u8; 10];
    entropy.copy_from_slice(&bytes[..10]);
    ulid_from_parts(ms, entropy)
}

const SLUG_MAX: usize = 40;

/// Kebab-case filename tail for a task/goal title. Kept local rather than
/// reusing `research::slugify`: that one caps at 50 and falls back to the
/// literal string `"research"`, which would be a confusing task filename.
pub fn slugify(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let mut slug = out.trim_matches('-').to_string();
    if slug.len() > SLUG_MAX {
        let end = SLUG_MAX;
        let cut = slug[..end].rfind('-').unwrap_or(end);
        slug.truncate(cut);
    }
    if slug.is_empty() {
        "task".into()
    } else {
        slug
    }
}

/// `<ulid>-<slug>.md` (D1: "the `id` in frontmatter is authoritative; the
/// filename is for humans").
pub fn task_file_name(id: &str, title: &str) -> String {
    format!("{id}-{}.md", slugify(title))
}

/// A task to create. `id` is caller-supplied-or-generated so tests can be
/// deterministic (mirrors `memory.rs`'s caller-supplied `today`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NewTask {
    pub id: Option<String>,
    pub title: String,
    pub body: String,
    pub fields: TaskPatch,
}

/// Create a task file in `home`. Status defaults to `backlog` — the intake
/// column (spec: "Task creation without an explicit status SHALL default
/// to `backlog`") — kind to `human`, board to `main`, and
/// `created`/`updated` to the caller-supplied `today`. `due`/`goal` keys
/// are emitted only when set.
pub fn create_task(home: TaskHome, new: &NewTask, today: &str) -> Result<Task> {
    let title = new.title.trim();
    if title.is_empty() {
        return Err(Error::Other("a task needs a title".into()));
    }
    let id = match &new.id {
        Some(i) if !i.trim().is_empty() => i.trim().to_string(),
        _ => new_ulid(),
    };
    let dir = home.tasks_dir();
    let path = dir.join(task_file_name(&id, title));
    if path.exists() {
        return Err(Error::Other(format!(
            "a task file already exists at {}",
            path.display()
        )));
    }

    let f = &new.fields;
    let mut fm = String::from("---\n");
    fm.push_str(&format!("id: {}\n", render_scalar(&id)));
    fm.push_str(&format!("title: {}\n", render_scalar(title)));
    fm.push_str(&format!(
        "status: {}\n",
        f.status.unwrap_or(TaskStatus::Backlog).as_str()
    ));
    fm.push_str(&format!(
        "kind: {}\n",
        f.kind.unwrap_or(TaskKind::Human).as_str()
    ));
    fm.push_str(&format!(
        "assignee: {}\n",
        render_scalar(f.assignee.as_deref().unwrap_or(""))
    ));
    fm.push_str(&format!(
        "project: {}\n",
        render_scalar(
            f.project
                .as_deref()
                .unwrap_or_else(|| home.default_project())
        )
    ));
    for line in seq_lines("tags", f.tags.as_deref().unwrap_or(&[])) {
        fm.push_str(&line);
        fm.push('\n');
    }
    if let Some(due) = f.due.as_deref().filter(|d| !d.trim().is_empty()) {
        fm.push_str(&format!("due: {}\n", render_scalar(due)));
    }
    if let Some(goal) = f.goal.as_deref().filter(|g| !g.trim().is_empty()) {
        fm.push_str(&format!("goal: {}\n", render_scalar(goal)));
    }
    fm.push_str(&format!(
        "board: {}\n",
        f.board.unwrap_or(BoardKind::Main).as_str()
    ));
    fm.push_str(&format!("created: {}\n", render_scalar(today)));
    fm.push_str(&format!("updated: {}\n", render_scalar(today)));
    // ken-pipeline fields. These live in `extra` on a parsed task, but a file
    // being *created* has no extra to flatten from — so they are written here
    // or they are lost. Without this, a sign-off child would land with no
    // `parent` and a generated idea with no `spawned_by` citation, silently
    // breaking exactly the links those two flows exist to record.
    // `blocked_by` is deliberately absent: a task cannot be born blocked —
    // `pipeline::block` captures the return lane from a lane the ticket is
    // already in, so there is no valid blocked state at creation.
    for (key, value) in [
        ("lane", f.lane.as_deref()),
        ("pipeline", f.pipeline.as_deref()),
        ("model", f.model.as_deref()),
        ("agent", f.agent.as_deref()),
        ("verify", f.verify.as_deref()),
        ("parent", f.parent.as_deref()),
        ("spawned_by", f.spawned_by.as_deref()),
        ("origin", f.origin.as_deref()),
        ("target", f.target.as_deref()),
    ] {
        if let Some(v) = value.filter(|v| !v.trim().is_empty()) {
            fm.push_str(&format!("{key}: {}\n", render_scalar(v)));
        }
    }
    for (key, seq) in [("scope", f.scope.as_deref()), ("projects", f.projects.as_deref())] {
        if let Some(items) = seq.filter(|s| !s.is_empty()) {
            for line in seq_lines(key, items) {
                fm.push_str(&line);
                fm.push('\n');
            }
        }
    }
    fm.push_str("---\n\n");
    fm.push_str(new.body.trim());
    if !new.body.trim().is_empty() {
        fm.push('\n');
    }

    fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
    fs::write(&path, &fm).map_err(|e| Error::io(&path, e))?;
    Ok(parse_task(&path, home.kind(), home.default_project(), &fm))
}

// ---------------------------------------------------------------------
// 1.3 Scanning + aggregation
// ---------------------------------------------------------------------

/// Parse every `.md` file directly in `home`'s tasks dir, sorted by
/// filename for determinism. Not recursive — `archive/` and `goals/` are
/// subfolders and are therefore skipped for free. A missing folder reads as
/// no tasks (homes are created lazily on first write, and a per-repo home
/// that doesn't exist is simply not opted in).
pub fn list_tasks(home: TaskHome) -> Result<Vec<Task>> {
    let dir = home.tasks_dir();
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut paths: Vec<PathBuf> = fs::read_dir(&dir)
        .map_err(|e| Error::io(&dir, e))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "md"))
        .collect();
    paths.sort();
    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        let raw = fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
        out.push(parse_task(&path, home.kind(), home.default_project(), &raw));
    }
    Ok(out)
}

/// Scan every home into one board list (D2: "the board scans both and
/// treats home as invisible plumbing"). Homes are scanned in the order
/// given; duplicate `id`s (e.g. a task file copied between homes) collapse
/// to the first occurrence, because `id` — not the path — is a task's
/// identity.
pub fn scan_tasks(homes: &[TaskHome]) -> Result<Vec<Task>> {
    let mut out: Vec<Task> = Vec::new();
    for home in homes {
        for task in list_tasks(*home)? {
            if !out.iter().any(|t| t.id == task.id) {
                out.push(task);
            }
        }
    }
    Ok(out)
}

/// Find a task by its authoritative `id` (never by filename).
pub fn find_by_id<'a>(tasks: &'a [Task], id: &str) -> Option<&'a Task> {
    tasks.iter().find(|t| t.id == id)
}

// ---------------------------------------------------------------------
// 1.3 / 1.6 Filter matching
// ---------------------------------------------------------------------

/// Assignee filtering needs a third state beyond "any"/"this person": D4's
/// worked example is an agent asking for *unclaimed* `ai` tasks, which is
/// `assignee` empty. A bare `Option<String>` couldn't express that without
/// reserving a magic name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AssigneeFilter {
    /// `assignee` empty — the claimable pool.
    Unassigned,
    Named(String),
}

impl AssigneeFilter {
    /// Tool-argument convenience: `none`/`unassigned`/empty ⇒
    /// [`AssigneeFilter::Unassigned`].
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "none" | "unassigned" => AssigneeFilter::Unassigned,
            _ => AssigneeFilter::Named(s.trim().to_string()),
        }
    }
}

/// The one filter shared by the board UI and `task_list` (tasks.md 1.3).
/// Every field is AND-ed; `None` means "don't filter on this".
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TaskFilter {
    pub status: Option<TaskStatus>,
    pub project: Option<String>,
    pub tag: Option<String>,
    pub assignee: Option<AssigneeFilter>,
    pub kind: Option<TaskKind>,
    pub goal: Option<String>,
    /// Not in tasks.md 1.3's list, but the daily view (D5) is "just a
    /// filter" over `board: daily`, so it belongs in the same struct rather
    /// than a parallel one.
    pub board: Option<BoardKind>,
    /// ken-pipeline D2: the resolved lane id. Only ever matches a ticket
    /// that has been through `pipeline::resolve_task_lane` — a pipeline-less
    /// ticket has no lane and therefore matches no lane filter, exactly as
    /// an out-of-vocabulary `status` matches no status filter.
    pub lane: Option<String>,
    /// ken-pipeline D2: the ticket's `pipeline:` frontmatter key.
    pub pipeline: Option<String>,
    /// ken-pipeline D5: "what is stuck and why" as a filter.
    pub blocked: Option<BlockedFilter>,
}

/// Block-state filter (ken-pipeline D5 / tasks.md 1.3).
///
/// Deliberately evaluated from the ticket's own frontmatter alone — no
/// pipeline definition, no board — so [`matches`] stays pure over one
/// [`Task`]. The consequence, stated plainly: "blocked" here means *the
/// ticket carries block evidence* (`blocked_by` and/or `block_reason`),
/// not "the ticket's status is the blocked lane's id". Those agree for
/// every ticket that went through `pipeline::block`, and the disagreement
/// cases (a hand edit that sets `status: blocked` with no reason and no
/// dependency) are tray entries by construction, not board rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BlockedFilter {
    /// Match regardless of block state — a no-op, present so a tool
    /// argument can say "don't care" explicitly.
    Any,
    Blocked,
    NotBlocked,
    /// Blocked by a specific ticket id (case-insensitive).
    By(String),
    /// Was blocked, no longer is, and has not yet been moved back to its
    /// `return_lane` — the digest's "unblocked overnight" group.
    NewlyUnblocked,
}

fn eq_ci(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// Does one task match one filter? Text comparisons are case-insensitive
/// (a hand-typed `project: itemsearch` should match the board's
/// `ItemSearch` chip); `tag` matches if any of the task's tags match.
/// A task with an unrecognized `status` matches no status filter — it is
/// in the needs-attention tray, not in a column.
pub fn matches(task: &Task, filter: &TaskFilter) -> bool {
    if let Some(s) = filter.status {
        if task.status != Some(s) {
            return false;
        }
    }
    if let Some(p) = &filter.project {
        if !eq_ci(&task.project, p) {
            return false;
        }
    }
    if let Some(t) = &filter.tag {
        if !task.tags.iter().any(|x| eq_ci(x, t)) {
            return false;
        }
    }
    match &filter.assignee {
        Some(AssigneeFilter::Unassigned) => {
            if !task.assignee.is_empty() {
                return false;
            }
        }
        Some(AssigneeFilter::Named(name)) => {
            if !eq_ci(&task.assignee, name) {
                return false;
            }
        }
        None => {}
    }
    if let Some(k) = filter.kind {
        if task.kind != k {
            return false;
        }
    }
    if let Some(g) = &filter.goal {
        match &task.goal {
            Some(task_goal) if eq_ci(task_goal, g) => {}
            _ => return false,
        }
    }
    if let Some(b) = filter.board {
        if task.board != b {
            return false;
        }
    }
    if let Some(l) = &filter.lane {
        match &task.lane {
            Some(task_lane) if eq_ci(task_lane, l) => {}
            _ => return false,
        }
    }
    if let Some(p) = &filter.pipeline {
        match crate::pipeline::ticket_pipeline(task) {
            Some(task_pipeline) if eq_ci(&task_pipeline, p) => {}
            _ => return false,
        }
    }
    if let Some(b) = &filter.blocked {
        if !crate::pipeline::matches_block_filter(task, b) {
            return false;
        }
    }
    true
}

pub fn filter_tasks<'a>(tasks: &'a [Task], filter: &TaskFilter) -> Vec<&'a Task> {
    tasks.iter().filter(|t| matches(t, filter)).collect()
}

// ---------------------------------------------------------------------
// 1.2 / 1.6 Needs-attention tray
// ---------------------------------------------------------------------

/// Why a task can't be placed on the board as-is. Both cases are hand-edit
/// or agent-error artifacts that must never crash and must never be
/// silently rewritten (spec).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "reason", content = "value")]
pub enum AttentionReason {
    /// `status` present but outside `backlog|todo|doing|review|done`.
    InvalidStatus(String),
    /// `kind` present but outside `human|ai`.
    InvalidKind(String),
    /// `board` present but outside `main|daily`.
    InvalidBoard(String),
    /// `goal` references an id with no goal file (D7).
    UnknownGoal(String),
    /// ken-pipeline D2: `pipeline:` names a definition that isn't loaded.
    UnknownPipeline(String),
    /// ken-pipeline D2: the ticket's pipeline has no lane matching its
    /// `status`. Carries the raw status, like [`AttentionReason::
    /// InvalidStatus`] does for the classic path.
    UnknownLane(String),
    /// ken-pipeline D5: a `blocked_by` id matching no ticket on the board.
    UnknownBlocker(String),
    /// ken-pipeline D5: the ticket is blocked but its `return_lane` is
    /// missing, or names a lane the pipeline doesn't define (the
    /// lane-rename orphan case). Empty string means "missing".
    UnknownReturnLane(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NeedsAttention {
    pub id: String,
    pub title: String,
    pub path: PathBuf,
    pub reasons: Vec<AttentionReason>,
}

/// Everything the tray shows. Pure over an already-scanned board, so the
/// caller can recompute it on every watcher event for free.
///
/// This is the classic, pipeline-unaware entry point and it keeps its
/// exact pre-ken-pipeline behaviour, so every existing caller is
/// unchanged. A board that has pipeline definitions loaded calls
/// [`needs_attention_with_pipelines`] instead.
pub fn needs_attention(tasks: &[Task], goals: &[Goal]) -> Vec<NeedsAttention> {
    needs_attention_with_pipelines(tasks, goals, &[])
}

/// [`needs_attention`] plus the board-scoped lane checks (ken-pipeline
/// D2/D5): unknown pipeline, unknown lane, unknown blocker, and a missing
/// or orphaned `return_lane`.
///
/// Every one of these is *surfaced*, never repaired — the same rule
/// [`apply_patch`] already enforces for an out-of-vocabulary status.
pub fn needs_attention_with_pipelines(
    tasks: &[Task],
    goals: &[Goal],
    pipelines: &[crate::pipeline::Pipeline],
) -> Vec<NeedsAttention> {
    let mut out = Vec::new();
    for task in tasks {
        let mut reasons = Vec::new();
        let pipeline_reasons = crate::pipeline::attention_reasons(task, tasks, pipelines);
        // A pipeline ticket's `status` holds a lane id, so the classic
        // five-value check is meaningless for it: `UnknownLane` /
        // `UnknownPipeline` replace `InvalidStatus` rather than doubling
        // up on it.
        let pipeline_owned_status = crate::pipeline::ticket_pipeline(task).is_some();
        if task.status.is_none() && !pipeline_owned_status {
            reasons.push(AttentionReason::InvalidStatus(task.status_raw.clone()));
        }
        reasons.extend(pipeline_reasons);
        if !task.kind_raw.is_empty() && TaskKind::parse(&task.kind_raw).is_none() {
            reasons.push(AttentionReason::InvalidKind(task.kind_raw.clone()));
        }
        if !task.board_raw.is_empty() && BoardKind::parse(&task.board_raw).is_none() {
            reasons.push(AttentionReason::InvalidBoard(task.board_raw.clone()));
        }
        if let Some(goal_id) = &task.goal {
            if !goals.iter().any(|g| eq_ci(&g.id, goal_id)) {
                reasons.push(AttentionReason::UnknownGoal(goal_id.clone()));
            }
        }
        if !reasons.is_empty() {
            out.push(NeedsAttention {
                id: task.id.clone(),
                title: task.title.clone(),
                path: task.path.clone(),
                reasons,
            });
        }
    }
    out
}

// ---------------------------------------------------------------------
// 1.4 Archive, log append, journal line
// ---------------------------------------------------------------------

/// True for a strict `YYYY-MM-DD` string. Date arithmetic isn't needed
/// anywhere in this module — ISO dates compare correctly as plain strings,
/// so `memory.rs`'s `days_from_civil` machinery stays where it is.
fn is_iso_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[8..].iter().all(u8::is_ascii_digit)
}

/// `YYYY-MM` from a `YYYY-MM-DD` date — the archive month bucket.
pub fn archive_month(date: &str) -> Result<String> {
    if !is_iso_date(date) {
        return Err(Error::Other(format!(
            "invalid date '{date}' — expected YYYY-MM-DD"
        )));
    }
    Ok(date[..7].to_string())
}

/// Where a task archives to: `<its own home>/archive/YYYY-MM/<same
/// filename>` (D2 — per-repo history ships with the repo).
pub fn archive_target(task: &Task, today: &str) -> Result<PathBuf> {
    let month = archive_month(today)?;
    Ok(archive_dir(&task.home_dir, &month).join(task.file_name()))
}

/// Move a task file into its home's `archive/YYYY-MM/`. A name collision in
/// the target month (same task archived, restored, and archived again)
/// gets a `-2`, `-3`, … suffix rather than clobbering history.
pub fn archive_task(task: &Task, today: &str) -> Result<PathBuf> {
    let mut target = archive_target(task, today)?;
    let dir = target
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| archive_dir(&task.home_dir, "unknown"));
    fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
    if target.exists() {
        let stem = task
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| task.id.clone());
        for n in 2..1000 {
            let candidate = dir.join(format!("{stem}-{n}.md"));
            if !candidate.exists() {
                target = candidate;
                break;
            }
        }
    }
    fs::rename(&task.path, &target).map_err(|e| Error::io(&task.path, e))?;
    Ok(target)
}

/// The text `task_complete` appends to a task body: a `## Log` heading if
/// the body doesn't already have one, then a `### <date> <time>` entry with
/// the report verbatim (D4).
pub fn compose_log_entry(body: &str, report: &str, today: &str, time_hhmm: &str) -> String {
    let has_heading = body
        .lines()
        .any(|l| l.trim_end().eq_ignore_ascii_case(LOG_HEADING));
    let mut out = String::new();
    if !has_heading {
        out.push_str(LOG_HEADING);
        out.push_str("\n\n");
    }
    out.push_str(&format!("### {today} {time_hhmm}\n\n"));
    out.push_str(report.trim());
    out.push('\n');
    out
}

/// `task_complete`'s file half (D4): set `status: done`, bump `updated`,
/// and append the report under `## Log` — in a single guarded write, so an
/// agent completing a task can't lose the log to a concurrent claim.
///
/// The journal half is the caller's: compose it with
/// [`journal_summary_line`] and hand it to `memory::append_journal` when
/// `kenMemory` is on.
pub fn complete_task(task: &Task, report: &str, today: &str, time_hhmm: &str) -> Result<()> {
    let edits = vec![
        ("status", scalar_lines("status", TaskStatus::Done.as_str())),
        ("updated", scalar_lines("updated", today)),
    ];
    let today = today.to_string();
    let time = time_hhmm.to_string();
    let report = report.to_string();
    let append = move |body: &str| compose_log_entry(body, &report, &today, &time);
    apply_edits(&task.path, &edits, Some(&append))
}

/// Longest report excerpt carried into the journal — the journal line is a
/// pointer, not a copy (the full report lives in the task's `## Log`).
const JOURNAL_EXCERPT_CHARS: usize = 160;

/// The one-line journal summary `task_complete` writes when `kenMemory` is
/// on (D4: "a one-line journal summary linking the task's `ken://`
/// address"). Pure text composition — the caller passes it to
/// `memory::append_journal`.
pub fn journal_summary_line(task: &Task, host: &str, report: &str) -> String {
    let excerpt = report
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let excerpt: String = if excerpt.chars().count() > JOURNAL_EXCERPT_CHARS {
        let cut: String = excerpt.chars().take(JOURNAL_EXCERPT_CHARS).collect();
        format!("{}…", cut.trim_end())
    } else {
        excerpt.to_string()
    };
    let address = task.address(host);
    if excerpt.is_empty() {
        format!("Completed task \"{}\" ({address})", task.title)
    } else {
        format!("Completed task \"{}\" — {excerpt} ({address})", task.title)
    }
}

// ---------------------------------------------------------------------
// 1.5 Daily rollover
// ---------------------------------------------------------------------

/// Unfinished daily tasks from before `today` (D5: `board: daily`,
/// status ≠ done, `updated` before today). ISO dates compare as strings.
///
/// Two tolerant calls: a task whose `updated` isn't a valid ISO date is a
/// candidate (unknown staleness ⇒ ask the user, which is the whole posture
/// of the rollover prompt), and a task with an unrecognized `status` is
/// *not* (it belongs to the needs-attention tray, and rolling it would
/// mean writing to a file we don't understand).
pub fn rollover_candidates<'a>(tasks: &'a [Task], today: &str) -> Vec<&'a Task> {
    tasks
        .iter()
        .filter(|t| t.board == BoardKind::Daily)
        .filter(|t| matches!(t.status, Some(s) if s != TaskStatus::Done))
        .filter(|t| !is_iso_date(&t.updated) || t.updated.as_str() < today)
        .collect()
}

/// The three per-task resolutions the rollover prompt offers (D5). Never
/// auto-applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Rollover {
    /// Keep it on today's daily board — bump `updated` only.
    Roll,
    /// It wasn't a daily-sized item after all — `board: main`.
    Promote,
    /// Let it go — move to `archive/YYYY-MM/`.
    Archive,
}

/// What a resolution *means*, computed without touching the filesystem —
/// so the caller can preview, batch, or test the whole prompt before
/// anything is written.
#[derive(Debug, Clone, PartialEq)]
pub enum RolloverAction {
    /// Apply this patch (plus the usual `updated` bump).
    Patch(TaskPatch),
    /// Move the file to this path.
    Archive(PathBuf),
}

/// The pure transition. `Roll` is an empty patch on purpose: `updated` is
/// always rewritten by [`apply_patch`], and bumping it is the entire
/// meaning of rolling forward.
pub fn resolve_rollover(task: &Task, choice: Rollover, today: &str) -> Result<RolloverAction> {
    match choice {
        Rollover::Roll => Ok(RolloverAction::Patch(TaskPatch::default())),
        Rollover::Promote => Ok(RolloverAction::Patch(TaskPatch {
            board: Some(BoardKind::Main),
            ..TaskPatch::default()
        })),
        Rollover::Archive => Ok(RolloverAction::Archive(archive_target(task, today)?)),
    }
}

/// Execute one rollover resolution. Returns the task's path afterwards
/// (unchanged for roll/promote, the archive path for archive).
pub fn apply_rollover(task: &Task, choice: Rollover, today: &str) -> Result<PathBuf> {
    match resolve_rollover(task, choice, today)? {
        RolloverAction::Patch(patch) => {
            apply_patch(&task.path, &patch, today)?;
            Ok(task.path.clone())
        }
        RolloverAction::Archive(_) => archive_task(task, today),
    }
}

// ---------------------------------------------------------------------
// 1.6 Goals
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct GoalFrontmatter {
    #[serde(default)]
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    created: String,
    #[serde(default)]
    updated: String,
    #[serde(flatten)]
    extra: serde_yaml::Mapping,
}

/// A goal file from `tasks/goals/` (D7). Same tolerant parse and same
/// patch core as tasks; no assignee, no claim lifecycle.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Goal {
    pub id: String,
    pub title: String,
    /// `None` when `status` is present but outside `active|done|dropped`.
    /// Absent reads as `Active`.
    pub status: Option<GoalStatus>,
    pub status_raw: String,
    pub created: String,
    pub updated: String,
    pub body: String,
    pub path: PathBuf,
}

pub fn parse_goal(path: &Path, raw: &str) -> Goal {
    let (fm, body) = match split_raw(raw) {
        Some(s) => (
            serde_yaml::from_str::<GoalFrontmatter>(s.fm).unwrap_or_default(),
            s.body.to_string(),
        ),
        None => (GoalFrontmatter::default(), raw.to_string()),
    };
    let _ = &fm.extra; // unknown keys survive on disk via the patch core
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let body = body.trim().to_string();
    let status = if fm.status.trim().is_empty() {
        Some(GoalStatus::Active)
    } else {
        GoalStatus::parse(&fm.status)
    };
    Goal {
        id: if fm.id.trim().is_empty() {
            stem
        } else {
            fm.id.trim().to_string()
        },
        title: if fm.title.trim().is_empty() {
            first_line(&body)
        } else {
            fm.title.trim().to_string()
        },
        status,
        status_raw: fm.status.trim().to_string(),
        created: fm.created.trim().to_string(),
        updated: fm.updated.trim().to_string(),
        body,
        path: path.to_path_buf(),
    }
}

/// Every goal file in the workspace home's `tasks/goals/`, sorted by
/// filename. Missing folder ⇒ no goals, not an error.
pub fn list_goals(workspace_root: &Path) -> Result<Vec<Goal>> {
    let dir = goals_dir(workspace_root);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut paths: Vec<PathBuf> = fs::read_dir(&dir)
        .map_err(|e| Error::io(&dir, e))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "md"))
        .collect();
    paths.sort();
    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        let raw = fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
        out.push(parse_goal(&path, &raw));
    }
    Ok(out)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NewGoal {
    pub id: Option<String>,
    pub title: String,
    pub body: String,
    pub status: Option<GoalStatus>,
}

/// Create a goal file in `.ken-workspace/tasks/goals/` (workspace home
/// only, D7).
pub fn create_goal(workspace_root: &Path, new: &NewGoal, today: &str) -> Result<Goal> {
    let title = new.title.trim();
    if title.is_empty() {
        return Err(Error::Other("a goal needs a title".into()));
    }
    let id = match &new.id {
        Some(i) if !i.trim().is_empty() => i.trim().to_string(),
        _ => new_ulid(),
    };
    let dir = goals_dir(workspace_root);
    let path = dir.join(task_file_name(&id, title));
    if path.exists() {
        return Err(Error::Other(format!(
            "a goal file already exists at {}",
            path.display()
        )));
    }
    let mut text = String::from("---\n");
    text.push_str(&format!("id: {}\n", render_scalar(&id)));
    text.push_str(&format!("title: {}\n", render_scalar(title)));
    text.push_str(&format!(
        "status: {}\n",
        new.status.unwrap_or(GoalStatus::Active).as_str()
    ));
    text.push_str(&format!("created: {}\n", render_scalar(today)));
    text.push_str(&format!("updated: {}\n", render_scalar(today)));
    text.push_str("---\n\n");
    text.push_str(new.body.trim());
    if !new.body.trim().is_empty() {
        text.push('\n');
    }
    fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
    fs::write(&path, &text).map_err(|e| Error::io(&path, e))?;
    Ok(parse_goal(&path, &text))
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GoalPatch {
    pub title: Option<String>,
    pub status: Option<GoalStatus>,
}

/// Same rewrite core as tasks (D7: "same patch core, same tolerant
/// parse"), same never-rewrite-what-we-don't-understand guard.
pub fn apply_goal_patch(path: &Path, patch: &GoalPatch, updated: &str) -> Result<()> {
    let raw = fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
    let current = parse_goal(path, &raw);
    if current.status.is_none() && patch.status.is_none() {
        return Err(Error::Other(format!(
            "goal '{}' has an unrecognized status '{}' — resolve it before patching other keys",
            current.id, current.status_raw
        )));
    }
    let mut edits: Vec<(&str, Vec<String>)> = Vec::new();
    if let Some(t) = &patch.title {
        edits.push(("title", scalar_lines("title", t)));
    }
    if let Some(s) = patch.status {
        edits.push(("status", scalar_lines("status", s.as_str())));
    }
    edits.push(("updated", scalar_lines("updated", updated)));
    apply_edits(path, &edits, None)
}

/// Derived goal progress — never stored (spec: "no progress value exists
/// in any file").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub done: usize,
    pub total: usize,
}

/// done/total among the tasks tagging `goal_id`. Tasks with an
/// unrecognized `status` count toward `total` but never toward `done` —
/// they're real work, just not placeable.
pub fn goal_progress(tasks: &[Task], goal_id: &str) -> Progress {
    let mut p = Progress { done: 0, total: 0 };
    for t in tasks {
        if t.goal.as_deref().is_some_and(|g| eq_ci(g, goal_id)) {
            p.total += 1;
            if t.status == Some(TaskStatus::Done) {
                p.done += 1;
            }
        }
    }
    p
}

/// Progress for every known goal, keyed by goal id — what the board's
/// group-by-goal mode renders. Tasks tagging an unknown goal id are absent
/// here by construction; they surface via [`needs_attention`] instead.
pub fn goal_progress_all(tasks: &[Task], goals: &[Goal]) -> BTreeMap<String, Progress> {
    goals
        .iter()
        .map(|g| (g.id.clone(), goal_progress(tasks, &g.id)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::{tempdir, TempDir};

    fn ws(dir: &TempDir) -> TaskHome<'_> {
        TaskHome::Workspace {
            workspace_root: dir.path(),
        }
    }

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap()
    }

    fn task_at(path: &Path) -> Task {
        parse_task(path, HomeKind::Workspace, "", &read(path))
    }

    /// The adversarial corpus in one file: a comment, every modeled key,
    /// a block sequence, unknown scalar and *nested* unknown keys, and a
    /// body with load-bearing whitespace. Built with `concat!` rather than
    /// a raw string so the literal stays LF regardless of how the source
    /// file is checked out (the CRLF case is exercised explicitly below).
    const RICH: &str = concat!(
        "---\n",
        "# a hand-written comment\n",
        "id: 01J0AAAAAAAAAAAAAAAAAAAAAA\n",
        "title: Decompile the mob spawner\n",
        "status: todo\n",
        "kind: ai\n",
        "assignee: ''\n",
        "project: ShatteredRealms\n",
        "tags:\n",
        "  - hytale\n",
        "  - decomp\n",
        "due: '2026-08-30'\n",
        "board: main\n",
        "created: '2026-08-01'\n",
        "updated: '2026-08-01'\n",
        "severity: high\n",
        "links:\n",
        "  upstream: https://example.invalid/x\n",
        "---\n",
        "\n",
        "# Decompile the mob spawner\n",
        "\n",
        "Some   *hand-written*   body   with   odd  spacing.\n",
        "\n",
        "- [ ] find the entry point\n",
    );

    // ---- 1.1 parse / round-trip ----

    #[test]
    fn parses_full_frontmatter() {
        let t = parse_task(Path::new("/w/tasks/01J0-x.md"), HomeKind::Workspace, "", RICH);
        assert_eq!(t.id, "01J0AAAAAAAAAAAAAAAAAAAAAA");
        assert_eq!(t.title, "Decompile the mob spawner");
        assert_eq!(t.status, Some(TaskStatus::Todo));
        assert_eq!(t.kind, TaskKind::Ai);
        assert_eq!(t.assignee, "");
        assert_eq!(t.project, "ShatteredRealms");
        assert_eq!(t.tags, vec!["hytale".to_string(), "decomp".to_string()]);
        assert_eq!(t.due.as_deref(), Some("2026-08-30"));
        assert_eq!(t.board, BoardKind::Main);
        assert!(t.body.contains("odd  spacing"));
        assert!(t.extra().contains_key(serde_yaml::Value::String("severity".into())));
    }

    #[test]
    fn no_frontmatter_file_still_parses() {
        let t = parse_task(
            Path::new("/w/tasks/hand-made.md"),
            HomeKind::Workspace,
            "",
            "# Look at the loot tables\n\nnotes\n",
        );
        assert_eq!(t.id, "hand-made", "id falls back to the file stem");
        assert_eq!(t.title, "Look at the loot tables");
        assert_eq!(t.status, Some(TaskStatus::Backlog), "absent status = intake");
    }

    #[test]
    fn id_is_authoritative_over_filename() {
        let a = parse_task(
            Path::new("/w/tasks/01J0AAAAAAAAAAAAAAAAAAAAAA-typoed-slgu.md"),
            HomeKind::Workspace,
            "",
            RICH,
        );
        let b = parse_task(
            Path::new("/w/tasks/01J0AAAAAAAAAAAAAAAAAAAAAA-fixed-slug.md"),
            HomeKind::Workspace,
            "",
            RICH,
        );
        assert_eq!(a.id, b.id);
        // Aggregation keys on id, so a rename is a no-op for the board.
        assert_eq!(find_by_id(&[a, b], "01J0AAAAAAAAAAAAAAAAAAAAAA").is_some(), true);
    }

    #[test]
    fn tolerates_scalar_tags_hand_edit() {
        let raw = "---\nid: t1\ntitle: T\nstatus: todo\ntags: alpha, beta\n---\n\nbody\n";
        let t = parse_task(Path::new("/w/tasks/t1.md"), HomeKind::Workspace, "", raw);
        assert_eq!(t.title, "T", "a bad tags shape must not blank the file");
        assert_eq!(t.tags, vec!["alpha".to_string(), "beta".to_string()]);
    }

    // ---- 1.2 patch core: byte fidelity ----

    #[test]
    fn patch_touches_only_named_keys() {
        let next = patch_text(
            RICH,
            &[
                ("status", scalar_lines("status", "doing")),
                ("updated", scalar_lines("updated", "2026-08-03")),
            ],
            None,
        );
        let before: Vec<&str> = RICH.lines().collect();
        let after: Vec<&str> = next.lines().collect();
        assert_eq!(before.len(), after.len(), "no lines added or removed");
        let changed: Vec<(usize, &str, &str)> = before
            .iter()
            .zip(after.iter())
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(i, (a, b))| (i, *a, *b))
            .collect();
        assert_eq!(changed.len(), 2, "changed: {changed:?}");
        assert_eq!(changed[0].2, "status: doing");
        assert_eq!(changed[1].2, "updated: '2026-08-03'");
    }

    #[test]
    fn patch_preserves_comments_unknown_keys_and_body() {
        let next = patch_text(RICH, &[("status", scalar_lines("status", "review"))], None);
        assert!(next.contains("# a hand-written comment"));
        assert!(next.contains("severity: high"));
        assert!(next.contains("  upstream: https://example.invalid/x"));
        assert!(next.contains("Some   *hand-written*   body   with   odd  spacing."));
        assert!(next.contains("- [ ] find the entry point"));
    }

    #[test]
    fn patch_preserves_crlf() {
        let crlf = RICH.replace('\n', "\r\n");
        let next = patch_text(&crlf, &[("status", scalar_lines("status", "done"))], None);
        assert!(next.contains("status: done\r\n"));
        assert!(!next.contains("status: done\n\r"), "no mangled terminators");
        assert_eq!(next.matches('\n').count(), next.matches("\r\n").count());
    }

    #[test]
    fn patch_replaces_multi_line_block_sequence_as_a_unit() {
        // S6's open TODO: patching a key whose value spans several lines.
        let next = patch_text(
            RICH,
            &[("tags", seq_lines("tags", &["one".into(), "two".into(), "three".into()]))],
            None,
        );
        assert!(next.contains("tags:\n  - one\n  - two\n  - three\n"));
        assert!(!next.contains("- hytale"), "old items must not linger: {next}");
        assert!(next.contains("due: '2026-08-30'"), "next key survives");
    }

    #[test]
    fn patch_replaces_multi_line_block_scalar_as_a_unit() {
        let raw = "---\nid: t1\nnotes: |\n  line one\n  line two\n\nstatus: todo\n---\n\nbody\n";
        let next = patch_text(raw, &[("notes", scalar_lines("notes", "flat"))], None);
        assert!(next.contains("notes: flat\n"));
        assert!(!next.contains("line one"));
        assert!(next.contains("status: todo"));
    }

    #[test]
    fn patch_adds_a_missing_key_at_the_end_of_frontmatter() {
        let raw = "---\nid: t1\nstatus: todo\n---\n\nbody\n";
        let next = patch_text(raw, &[("goal", scalar_lines("goal", "g1"))], None);
        assert_eq!(next, "---\nid: t1\nstatus: todo\ngoal: g1\n---\n\nbody\n");
    }

    #[test]
    fn patch_creates_frontmatter_when_absent() {
        let raw = "# Just a note\n\nno frontmatter here\n";
        let next = patch_text(raw, &[("status", scalar_lines("status", "todo"))], None);
        assert_eq!(next, "---\nstatus: todo\n---\n\n# Just a note\n\nno frontmatter here\n");
    }

    #[test]
    fn patch_ignores_nested_keys_with_the_same_name() {
        let raw = "---\nid: t1\nstatus: todo\nmeta:\n  status: nested\n---\n\nbody\n";
        let next = patch_text(raw, &[("status", scalar_lines("status", "done"))], None);
        assert!(next.contains("status: done\n"));
        assert!(next.contains("  status: nested"), "nested key untouched: {next}");
    }

    #[test]
    fn patch_is_idempotent() {
        let once = patch_text(RICH, &[("status", scalar_lines("status", "doing"))], None);
        let twice = patch_text(&once, &[("status", scalar_lines("status", "doing"))], None);
        assert_eq!(once, twice);
    }

    #[test]
    fn quoting_is_conservative_but_round_trips() {
        assert_eq!(render_scalar("todo"), "todo");
        assert_eq!(render_scalar(""), "''");
        assert_eq!(render_scalar("2026-08-01"), "'2026-08-01'");
        assert_eq!(render_scalar("no"), "'no'");
        assert_eq!(render_scalar("it's"), "'it''s'");
        assert_eq!(render_scalar("ken://a/b"), "'ken://a/b'");
        // Newlines fold to spaces first, so the result is a plain scalar.
        assert_eq!(render_scalar("multi\nline"), "multi line");
        assert_eq!(render_scalar("multi\r\nline: x"), "'multi line: x'");
        // Everything above must survive a serde_yaml read.
        for v in ["", "2026-08-01", "no", "it's", "ken://a/b", "plain value"] {
            let raw = format!("---\nid: x\ntitle: {}\n---\n\nb\n", render_scalar(v));
            let t = parse_task(Path::new("/w/tasks/x.md"), HomeKind::Workspace, "", &raw);
            let expected = if v.is_empty() { "b" } else { v };
            assert_eq!(t.title, expected, "round trip of {v:?} via {raw}");
        }
    }

    // ---- 1.2 apply_patch on disk ----

    #[test]
    fn apply_patch_writes_only_status_and_updated() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tasks/01J0-x.md");
        write(&path, RICH);
        apply_patch(
            &path,
            &TaskPatch {
                status: Some(TaskStatus::Doing),
                ..TaskPatch::default()
            },
            "2026-08-03",
        )
        .unwrap();
        let after = read(&path);
        let diff = RICH
            .lines()
            .zip(after.lines())
            .filter(|(a, b)| a != b)
            .count();
        assert_eq!(diff, 2, "{after}");
        assert_eq!(task_at(&path).status, Some(TaskStatus::Doing));
    }

    #[test]
    fn dragging_twice_still_diffs_only_two_lines() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tasks/01J0-x.md");
        write(&path, RICH);
        for (status, day) in [(TaskStatus::Doing, "2026-08-03"), (TaskStatus::Review, "2026-08-04")]
        {
            let before = read(&path);
            apply_patch(
                &path,
                &TaskPatch {
                    status: Some(status),
                    ..TaskPatch::default()
                },
                day,
            )
            .unwrap();
            let after = read(&path);
            let changed = before.lines().zip(after.lines()).filter(|(a, b)| a != b).count();
            assert_eq!(changed, 2, "{after}");
            assert_eq!(before.lines().count(), after.lines().count());
        }
        assert!(read(&path).contains("severity: high"));
    }

    #[test]
    fn no_op_patch_does_not_rewrite_the_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tasks/01J0-x.md");
        write(&path, RICH);
        apply_patch(
            &path,
            &TaskPatch {
                status: Some(TaskStatus::Todo),
                ..TaskPatch::default()
            },
            "2026-08-01",
        )
        .unwrap();
        assert_eq!(read(&path), RICH, "byte-identical, no watcher churn");
    }

    #[test]
    fn invalid_status_is_surfaced_and_the_file_is_not_rewritten() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tasks/01J0-x.md");
        let raw = RICH.replace("status: todo", "status: blocked");
        write(&path, &raw);

        let t = task_at(&path);
        assert!(t.has_invalid_status());
        assert_eq!(t.status_raw, "blocked");

        let tray = needs_attention(&[t.clone()], &[]);
        assert_eq!(tray.len(), 1);
        assert_eq!(
            tray[0].reasons,
            vec![AttentionReason::InvalidStatus("blocked".into())]
        );

        // A patch that doesn't resolve the status is refused outright.
        let err = apply_patch(
            &path,
            &TaskPatch {
                assignee: Some("agent-desktop".into()),
                ..TaskPatch::default()
            },
            "2026-08-03",
        )
        .unwrap_err();
        assert!(err.to_string().contains("unrecognized status"), "{err}");
        assert_eq!(read(&path), raw, "file untouched");

        // Explicitly fixing it is allowed.
        apply_patch(
            &path,
            &TaskPatch {
                status: Some(TaskStatus::Todo),
                ..TaskPatch::default()
            },
            "2026-08-03",
        )
        .unwrap();
        assert_eq!(task_at(&path).status, Some(TaskStatus::Todo));
    }

    #[test]
    fn invalid_kind_and_board_also_reach_the_tray() {
        let raw = RICH.replace("kind: ai", "kind: robot").replace("board: main", "board: weekly");
        let t = parse_task(Path::new("/w/tasks/x.md"), HomeKind::Workspace, "", &raw);
        let tray = needs_attention(&[t], &[]);
        assert_eq!(
            tray[0].reasons,
            vec![
                AttentionReason::InvalidKind("robot".into()),
                AttentionReason::InvalidBoard("weekly".into()),
            ]
        );
    }

    #[test]
    fn concurrent_change_between_read_and_write_retries_and_wins() {
        // Direct check of the S6 precondition: a file that changes between
        // the fingerprint and the write is re-read, not clobbered.
        let dir = tempdir().unwrap();
        let path = dir.path().join("tasks/01J0-x.md");
        write(&path, RICH);
        // An "external writer" adds a key; our patch must keep it.
        let external = read(&path).replace("severity: high", "severity: high\nexternal: kept");
        write(&path, &external);
        apply_patch(
            &path,
            &TaskPatch {
                status: Some(TaskStatus::Done),
                ..TaskPatch::default()
            },
            "2026-08-05",
        )
        .unwrap();
        let after = read(&path);
        assert!(after.contains("external: kept"));
        assert!(after.contains("status: done"));
    }

    // ---- 1.1 / 1.6 creation ----

    #[test]
    fn create_defaults_to_backlog_and_round_trips() {
        let dir = tempdir().unwrap();
        let t = create_task(
            ws(&dir),
            &NewTask {
                id: Some("01J0TESTTESTTESTTESTTESTTE".into()),
                title: "Groom the backlog".into(),
                body: "Check the intake column.".into(),
                fields: TaskPatch::default(),
            },
            "2026-08-03",
        )
        .unwrap();
        assert_eq!(t.status, Some(TaskStatus::Backlog));
        assert_eq!(t.kind, TaskKind::Human);
        assert_eq!(t.board, BoardKind::Main);
        assert_eq!(t.created, "2026-08-03");
        assert_eq!(
            t.path.file_name().unwrap().to_string_lossy(),
            "01J0TESTTESTTESTTESTTESTTE-groom-the-backlog.md"
        );
        assert_eq!(task_at(&t.path), task_at(&t.path));
        assert_eq!(task_at(&t.path).title, "Groom the backlog");
        assert!(read(&t.path).contains("Check the intake column."));
    }

    #[test]
    fn create_omits_optional_keys_and_defaults_project_from_home() {
        let dir = tempdir().unwrap();
        let home = TaskHome::Project {
            project_root: dir.path(),
            project: "ShatteredRealms",
        };
        let t = create_task(
            home,
            &NewTask {
                id: Some("01J0PROJPROJPROJPROJPROJPR".into()),
                title: "Ship it".into(),
                ..NewTask::default()
            },
            "2026-08-03",
        )
        .unwrap();
        let raw = read(&t.path);
        assert!(!raw.contains("due:"));
        assert!(!raw.contains("goal:"));
        assert!(raw.contains("tags: []"));
        assert_eq!(t.project, "ShatteredRealms", "defaulted from the home");
        assert!(t.path.ends_with(".ken/tasks/01J0PROJPROJPROJPROJPROJPR-ship-it.md"));
    }

    #[test]
    fn create_writes_pipeline_fields_so_provenance_survives() {
        // A sign-off child and a generated idea are both born carrying links
        // (`parent`, `spawned_by`) that only exist at creation time. If
        // `create_task` drops them the ticket still looks fine — it just
        // silently loses the connection it was created to record.
        let dir = tempdir().unwrap();
        let t = create_task(
            TaskHome::Workspace {
                workspace_root: dir.path(),
            },
            &NewTask {
                id: Some("01J0CHILDCHILDCHILDCHILDCH".into()),
                title: "Address review comment".into(),
                fields: TaskPatch {
                    lane: Some("todo".into()),
                    pipeline: Some("default".into()),
                    parent: Some("01J0PARENTPARENTPARENTPARE".into()),
                    spawned_by: Some("01J0SOURCESOURCESOURCESOUR".into()),
                    origin: Some("generated".into()),
                    projects: Some(vec!["ShatteredRealms".into(), "ShatteredRealmsTools".into()]),
                    ..TaskPatch::default()
                },
                ..NewTask::default()
            },
            "2026-08-03",
        )
        .unwrap();

        let raw = read(&t.path);
        for expected in [
            "lane:",
            "pipeline:",
            "parent:",
            "spawned_by:",
            "origin:",
            "projects:",
        ] {
            assert!(raw.contains(expected), "{expected} missing from:\n{raw}");
        }
        assert!(raw.contains("ShatteredRealmsTools"), "sequence lost:\n{raw}");
        // Unset pipeline fields stay absent — no empty keys on a plain task.
        assert!(!raw.contains("scope:"), "unset key emitted:\n{raw}");
        assert!(!raw.contains("blocked_by:"), "a task cannot be born blocked");

        // And they survive the round-trip back through the parser.
        let reparsed = parse_task(&t.path, HomeKind::Workspace, "workspace", &raw);
        assert_eq!(
            reparsed.extra().get("parent").and_then(|v| v.as_str()),
            Some("01J0PARENTPARENTPARENTPARE")
        );
        assert_eq!(
            reparsed.extra().get("spawned_by").and_then(|v| v.as_str()),
            Some("01J0SOURCESOURCESOURCESOUR")
        );
    }

    #[test]
    fn ulid_encoding_is_26_crockford_chars() {
        let id = ulid_from_parts(0x0192_3456_789A, [0xFF; 10]);
        assert_eq!(id.len(), 26);
        assert!(id.chars().all(|c| CROCKFORD.contains(&(c as u8))));
        assert_eq!(&id[10..], "ZZZZZZZZZZZZZZZZ");
        assert_eq!(ulid_from_parts(0, [0; 10]), "0".repeat(26));
        assert_ne!(new_ulid(), new_ulid());
    }

    // ---- 1.3 scanning + aggregation ----

    fn seed_board(dir: &TempDir) -> (Vec<Task>, PathBuf) {
        let wsroot = dir.path().join("ws");
        let proj = dir.path().join("ws/ShatteredRealms");
        let mk = |home: TaskHome, id: &str, title: &str, fields: TaskPatch| {
            create_task(
                home,
                &NewTask {
                    id: Some(id.into()),
                    title: title.into(),
                    body: String::new(),
                    fields,
                },
                "2026-08-01",
            )
            .unwrap();
        };
        let w = TaskHome::Workspace {
            workspace_root: &wsroot,
        };
        let p = TaskHome::Project {
            project_root: &proj,
            project: "ShatteredRealms",
        };
        mk(
            w,
            "01J0W1",
            "Workspace todo",
            TaskPatch {
                status: Some(TaskStatus::Todo),
                kind: Some(TaskKind::Ai),
                project: Some("ItemSearch".into()),
                tags: Some(vec!["alpha".into()]),
                goal: Some("01J0GOAL".into()),
                ..TaskPatch::default()
            },
        );
        mk(
            w,
            "01J0W2",
            "Workspace done",
            TaskPatch {
                status: Some(TaskStatus::Done),
                assignee: Some("agent-desktop".into()),
                goal: Some("01J0GOAL".into()),
                ..TaskPatch::default()
            },
        );
        mk(
            p,
            "01J0P1",
            "Repo task",
            TaskPatch {
                status: Some(TaskStatus::Doing),
                tags: Some(vec!["Alpha".into(), "beta".into()]),
                goal: Some("01J0GOAL".into()),
                ..TaskPatch::default()
            },
        );
        let tasks = scan_tasks(&[w, p]).unwrap();
        (tasks, wsroot)
    }

    #[test]
    fn scan_aggregates_both_homes_and_defaults_project() {
        let dir = tempdir().unwrap();
        let (tasks, _) = seed_board(&dir);
        assert_eq!(tasks.len(), 3);
        let repo = find_by_id(&tasks, "01J0P1").unwrap();
        assert_eq!(repo.project, "ShatteredRealms", "defaulted from its home");
        assert_eq!(repo.home, HomeKind::Project);
        let wtask = find_by_id(&tasks, "01J0W1").unwrap();
        assert_eq!(wtask.project, "ItemSearch", "explicit key wins");
        assert_eq!(wtask.home, HomeKind::Workspace);
    }

    #[test]
    fn missing_home_folder_is_not_an_error() {
        let dir = tempdir().unwrap();
        assert!(list_tasks(ws(&dir)).unwrap().is_empty());
        assert!(list_goals(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn archive_and_goals_subfolders_are_not_scanned_as_tasks() {
        let dir = tempdir().unwrap();
        let home = workspace_tasks_dir(dir.path());
        write(&home.join("01J0A-live.md"), "---\nid: live\nstatus: todo\n---\n\nx\n");
        write(&home.join("archive/2026-07/01J0B-old.md"), "---\nid: old\n---\n\nx\n");
        write(&home.join("goals/01J0G-goal.md"), "---\nid: g\n---\n\nx\n");
        let tasks = list_tasks(ws(&dir)).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "live");
    }

    // ---- 1.3 / 1.6 filter table ----

    #[test]
    fn filter_table() {
        let dir = tempdir().unwrap();
        let (tasks, _) = seed_board(&dir);
        let ids = |f: TaskFilter| {
            let mut v: Vec<String> = filter_tasks(&tasks, &f).iter().map(|t| t.id.clone()).collect();
            v.sort();
            v
        };
        let cases: Vec<(TaskFilter, Vec<&str>)> = vec![
            (TaskFilter::default(), vec!["01J0P1", "01J0W1", "01J0W2"]),
            (
                TaskFilter {
                    status: Some(TaskStatus::Todo),
                    ..Default::default()
                },
                vec!["01J0W1"],
            ),
            (
                TaskFilter {
                    project: Some("shatteredrealms".into()),
                    ..Default::default()
                },
                vec!["01J0P1"],
            ),
            (
                TaskFilter {
                    tag: Some("alpha".into()),
                    ..Default::default()
                },
                vec!["01J0P1", "01J0W1"],
            ),
            (
                TaskFilter {
                    assignee: Some(AssigneeFilter::Unassigned),
                    ..Default::default()
                },
                vec!["01J0P1", "01J0W1"],
            ),
            (
                TaskFilter {
                    assignee: Some(AssigneeFilter::Named("agent-desktop".into())),
                    ..Default::default()
                },
                vec!["01J0W2"],
            ),
            (
                TaskFilter {
                    kind: Some(TaskKind::Ai),
                    ..Default::default()
                },
                vec!["01J0W1"],
            ),
            (
                TaskFilter {
                    goal: Some("01J0GOAL".into()),
                    ..Default::default()
                },
                vec!["01J0P1", "01J0W1", "01J0W2"],
            ),
            (
                TaskFilter {
                    goal: Some("nope".into()),
                    ..Default::default()
                },
                vec![],
            ),
            (
                TaskFilter {
                    board: Some(BoardKind::Daily),
                    ..Default::default()
                },
                vec![],
            ),
            (
                // D4's worked example: unclaimed ai tasks for a project.
                TaskFilter {
                    kind: Some(TaskKind::Ai),
                    assignee: Some(AssigneeFilter::Unassigned),
                    status: Some(TaskStatus::Todo),
                    project: Some("ItemSearch".into()),
                    ..Default::default()
                },
                vec!["01J0W1"],
            ),
        ];
        for (i, (f, expect)) in cases.into_iter().enumerate() {
            assert_eq!(ids(f), expect, "case {i}");
        }
    }

    #[test]
    fn assignee_filter_parses_the_unassigned_sentinels() {
        assert_eq!(AssigneeFilter::parse("none"), AssigneeFilter::Unassigned);
        assert_eq!(AssigneeFilter::parse("  "), AssigneeFilter::Unassigned);
        assert_eq!(
            AssigneeFilter::parse("Ken"),
            AssigneeFilter::Named("Ken".into())
        );
    }

    #[test]
    fn invalid_status_matches_no_column() {
        let t = parse_task(
            Path::new("/w/tasks/x.md"),
            HomeKind::Workspace,
            "",
            &RICH.replace("status: todo", "status: blocked"),
        );
        for s in TaskStatus::ALL {
            assert!(!matches(
                &t,
                &TaskFilter {
                    status: Some(s),
                    ..Default::default()
                }
            ));
        }
    }

    // ---- 1.4 archive / log / journal ----

    #[test]
    fn archive_month_cases() {
        assert_eq!(archive_month("2026-07-31").unwrap(), "2026-07");
        assert_eq!(archive_month("2026-01-01").unwrap(), "2026-01");
        assert!(archive_month("2026-7-1").is_err());
        assert!(archive_month("").is_err());
        assert!(archive_month("not-a-date").is_err());
    }

    #[test]
    fn archive_stays_in_the_tasks_own_home() {
        let dir = tempdir().unwrap();
        let (tasks, _) = seed_board(&dir);
        let repo = find_by_id(&tasks, "01J0P1").unwrap();
        let moved = archive_task(repo, "2026-07-15").unwrap();
        assert!(
            moved.ends_with("ShatteredRealms/.ken/tasks/archive/2026-07/01J0P1-repo-task.md"),
            "{}",
            moved.display()
        );
        assert!(!repo.path.exists());

        let wtask = find_by_id(&tasks, "01J0W1").unwrap();
        let moved = archive_task(wtask, "2026-08-02").unwrap();
        assert!(
            moved.ends_with(".ken-workspace/tasks/archive/2026-08/01J0W1-workspace-todo.md"),
            "{}",
            moved.display()
        );
    }

    #[test]
    fn archiving_the_same_name_twice_does_not_clobber() {
        let dir = tempdir().unwrap();
        let (tasks, _) = seed_board(&dir);
        let t = find_by_id(&tasks, "01J0W2").unwrap();
        let first = archive_task(t, "2026-08-02").unwrap();
        write(&t.path, &read(&first));
        let second = archive_task(t, "2026-08-02").unwrap();
        assert_ne!(first, second);
        assert!(second.to_string_lossy().ends_with("-2.md"), "{}", second.display());
        assert!(first.exists() && second.exists());
    }

    #[test]
    fn complete_sets_done_appends_log_and_composes_a_journal_line() {
        let dir = tempdir().unwrap();
        let (tasks, _) = seed_board(&dir);
        let t = find_by_id(&tasks, "01J0W1").unwrap();
        complete_task(t, "Found the spawner in Mob.class.\nDetails follow.", "2026-08-03", "14:05")
            .unwrap();
        let after = read(&t.path);
        assert!(after.contains("status: done"));
        assert!(after.contains("updated: '2026-08-03'"));
        assert!(after.contains("## Log"));
        assert!(after.contains("### 2026-08-03 14:05"));
        assert!(after.contains("Found the spawner in Mob.class."));

        // A second completion reuses the existing heading.
        let t2 = task_at(&t.path);
        complete_task(&t2, "Second pass.", "2026-08-04", "09:00").unwrap();
        let after = read(&t.path);
        assert_eq!(after.matches("## Log").count(), 1, "{after}");
        assert_eq!(after.matches("### ").count(), 2);

        let line = journal_summary_line(&t2, crate::memory::WORKSPACE_ADDRESS_ID, "Found the spawner in Mob.class.\nmore");
        assert_eq!(
            line,
            "Completed task \"Workspace todo\" — Found the spawner in Mob.class. (ken://workspace/tasks/01J0W1-workspace-todo.md)"
        );
    }

    #[test]
    fn journal_line_addresses_a_per_repo_task_under_its_project() {
        let dir = tempdir().unwrap();
        let (tasks, _) = seed_board(&dir);
        let t = find_by_id(&tasks, "01J0P1").unwrap();
        let line = journal_summary_line(t, "proj-uuid", "");
        assert_eq!(
            line,
            "Completed task \"Repo task\" (ken://proj-uuid/.ken/tasks/01J0P1-repo-task.md)"
        );
    }

    #[test]
    fn log_entry_is_pure_and_only_adds_the_heading_once() {
        let first = compose_log_entry("Body.", "2026-08-03", "10:00", "hi");
        assert!(first.starts_with("## Log\n\n"));
        let second = compose_log_entry("Body.\n\n## Log\n\n### x\n", "2026-08-03", "10:00", "hi");
        assert!(!second.contains("## Log"));
    }

    // ---- 1.5 rollover ----

    fn daily(dir: &TempDir, id: &str, status: TaskStatus, updated: &str) -> Task {
        let home = ws(dir);
        let t = create_task(
            home,
            &NewTask {
                id: Some(id.into()),
                title: format!("Daily {id}"),
                body: String::new(),
                fields: TaskPatch {
                    status: Some(status),
                    board: Some(BoardKind::Daily),
                    ..TaskPatch::default()
                },
            },
            "2026-08-01",
        )
        .unwrap();
        apply_patch(&t.path, &TaskPatch::default(), updated).unwrap();
        task_at(&t.path)
    }

    #[test]
    fn rollover_candidates_are_stale_unfinished_daily_tasks() {
        let dir = tempdir().unwrap();
        let stale = daily(&dir, "01J0D1", TaskStatus::Todo, "2026-08-02");
        let today_already = daily(&dir, "01J0D2", TaskStatus::Doing, "2026-08-03");
        let finished = daily(&dir, "01J0D3", TaskStatus::Done, "2026-08-02");
        let main_board = create_task(
            ws(&dir),
            &NewTask {
                id: Some("01J0M1".into()),
                title: "Main board".into(),
                body: String::new(),
                fields: TaskPatch {
                    status: Some(TaskStatus::Todo),
                    ..TaskPatch::default()
                },
            },
            "2026-08-01",
        )
        .unwrap();
        let broken = parse_task(
            Path::new("/w/tasks/b.md"),
            HomeKind::Workspace,
            "",
            "---\nid: b\nstatus: blocked\nboard: daily\nupdated: '2026-08-01'\n---\n\nx\n",
        );

        let all = vec![stale, today_already, finished, main_board, broken];
        let ids: Vec<&str> = rollover_candidates(&all, "2026-08-03")
            .iter()
            .map(|t| t.id.as_str())
            .collect();
        assert_eq!(ids, vec!["01J0D1"]);
    }

    #[test]
    fn rollover_resolutions_are_pure_transitions() {
        let dir = tempdir().unwrap();
        let t = daily(&dir, "01J0D1", TaskStatus::Todo, "2026-08-02");
        assert_eq!(
            resolve_rollover(&t, Rollover::Roll, "2026-08-03").unwrap(),
            RolloverAction::Patch(TaskPatch::default())
        );
        assert_eq!(
            resolve_rollover(&t, Rollover::Promote, "2026-08-03").unwrap(),
            RolloverAction::Patch(TaskPatch {
                board: Some(BoardKind::Main),
                ..TaskPatch::default()
            })
        );
        let RolloverAction::Archive(p) = resolve_rollover(&t, Rollover::Archive, "2026-08-03").unwrap()
        else {
            panic!("expected an archive action");
        };
        assert!(p.ends_with("tasks/archive/2026-08/01J0D1-daily-01j0d1.md"));
        // Pure: nothing moved.
        assert!(t.path.exists());
    }

    #[test]
    fn repeated_rollover_keeps_rolling_day_after_day() {
        let dir = tempdir().unwrap();
        let t = daily(&dir, "01J0D1", TaskStatus::Todo, "2026-08-02");
        for day in ["2026-08-03", "2026-08-04", "2026-08-05"] {
            let current = task_at(&t.path);
            assert_eq!(
                rollover_candidates(std::slice::from_ref(&current), day).len(),
                1,
                "stale again on {day}"
            );
            apply_rollover(&current, Rollover::Roll, day).unwrap();
            let rolled = task_at(&t.path);
            assert_eq!(rolled.updated, day);
            assert_eq!(rolled.board, BoardKind::Daily, "roll never changes the board");
            assert_eq!(rolled.status, Some(TaskStatus::Todo));
            assert!(
                rollover_candidates(std::slice::from_ref(&rolled), day).is_empty(),
                "no longer stale the same day"
            );
        }
    }

    #[test]
    fn promote_and_archive_resolutions_apply() {
        let dir = tempdir().unwrap();
        let promoted = daily(&dir, "01J0D2", TaskStatus::Todo, "2026-08-02");
        apply_rollover(&promoted, Rollover::Promote, "2026-08-03").unwrap();
        let after = task_at(&promoted.path);
        assert_eq!(after.board, BoardKind::Main);
        assert_eq!(after.updated, "2026-08-03");
        assert!(rollover_candidates(&[after], "2026-08-04").is_empty());

        let dropped = daily(&dir, "01J0D3", TaskStatus::Todo, "2026-08-02");
        let moved = apply_rollover(&dropped, Rollover::Archive, "2026-08-03").unwrap();
        assert!(!dropped.path.exists());
        assert!(moved.ends_with("tasks/archive/2026-08/01J0D3-daily-01j0d3.md"));
    }

    // ---- 1.6 goals ----

    fn seed_goal(root: &Path, id: &str, title: &str) -> Goal {
        create_goal(
            root,
            &NewGoal {
                id: Some(id.into()),
                title: title.into(),
                body: "Why this matters.".into(),
                status: None,
            },
            "2026-08-01",
        )
        .unwrap()
    }

    #[test]
    fn goal_files_live_in_the_workspace_home_and_default_to_active() {
        let dir = tempdir().unwrap();
        let g = seed_goal(dir.path(), "01J0GOAL", "Ship multi-project Ken");
        assert!(g
            .path
            .ends_with(".ken-workspace/tasks/goals/01J0GOAL-ship-multi-project-ken.md"));
        assert_eq!(g.status, Some(GoalStatus::Active));
        assert_eq!(g.title, "Ship multi-project Ken");
        assert_eq!(list_goals(dir.path()).unwrap(), vec![g]);
    }

    #[test]
    fn goal_patch_uses_the_same_core_and_preserves_unknown_keys() {
        let dir = tempdir().unwrap();
        let g = seed_goal(dir.path(), "01J0GOAL", "Ship it");
        let hand_edited = read(&g.path).replace("status: active", "status: active\nowner: ken");
        write(&g.path, &hand_edited);

        apply_goal_patch(
            &g.path,
            &GoalPatch {
                status: Some(GoalStatus::Done),
                ..GoalPatch::default()
            },
            "2026-08-09",
        )
        .unwrap();
        let after = read(&g.path);
        assert!(after.contains("owner: ken"));
        assert!(after.contains("Why this matters."));
        let changed = hand_edited.lines().zip(after.lines()).filter(|(a, b)| a != b).count();
        assert_eq!(changed, 2, "{after}");

        let bad = read(&g.path).replace("status: done", "status: paused");
        write(&g.path, &bad);
        let g2 = parse_goal(&g.path, &read(&g.path));
        assert!(g2.status.is_none());
        let err = apply_goal_patch(
            &g.path,
            &GoalPatch {
                title: Some("Renamed".into()),
                ..GoalPatch::default()
            },
            "2026-08-10",
        )
        .unwrap_err();
        assert!(err.to_string().contains("unrecognized status"), "{err}");
        assert_eq!(read(&g.path), bad, "not rewritten");
    }

    #[test]
    fn goal_progress_is_derived_and_never_stored() {
        let dir = tempdir().unwrap();
        let (mut tasks, wsroot) = seed_board(&dir);
        let g = seed_goal(&wsroot, "01J0GOAL", "Ship multi-project Ken");
        // 3 tasks tag the goal, 1 is done.
        assert_eq!(goal_progress(&tasks, "01J0GOAL"), Progress { done: 1, total: 3 });

        let t = find_by_id(&tasks, "01J0P1").unwrap().clone();
        complete_task(&t, "done", "2026-08-03", "10:00").unwrap();
        tasks = scan_tasks(&[
            TaskHome::Workspace {
                workspace_root: &wsroot,
            },
            TaskHome::Project {
                project_root: &wsroot.join("ShatteredRealms"),
                project: "ShatteredRealms",
            },
        ])
        .unwrap();
        assert_eq!(goal_progress(&tasks, "01J0GOAL"), Progress { done: 2, total: 3 });

        let all = goal_progress_all(&tasks, &[g.clone()]);
        assert_eq!(all.get("01J0GOAL"), Some(&Progress { done: 2, total: 3 }));
        // Nothing about progress is on disk.
        assert!(!read(&g.path).contains("progress"));
        assert!(!read(&tasks[0].path).contains("progress"));
        // Unknown goal id ⇒ no progress bucket, and no panic.
        assert_eq!(goal_progress(&tasks, "01J0NOPE"), Progress { done: 0, total: 0 });
    }

    #[test]
    fn unknown_goal_id_reaches_the_tray_and_the_file_is_not_rewritten() {
        let dir = tempdir().unwrap();
        let (tasks, wsroot) = seed_board(&dir);
        let known = seed_goal(&wsroot, "01J0OTHER", "Some other goal");
        let before: Vec<String> = tasks.iter().map(|t| read(&t.path)).collect();

        let tray = needs_attention(&tasks, &[known]);
        let ids: Vec<&str> = tray.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, vec!["01J0W1", "01J0W2", "01J0P1"]);
        for entry in &tray {
            assert_eq!(
                entry.reasons,
                vec![AttentionReason::UnknownGoal("01J0GOAL".into())]
            );
        }
        for (t, raw) in tasks.iter().zip(before) {
            assert_eq!(read(&t.path), raw, "needs-attention never rewrites");
        }
    }

    #[test]
    fn a_per_repo_task_can_tag_a_workspace_goal() {
        let dir = tempdir().unwrap();
        let (tasks, wsroot) = seed_board(&dir);
        let g = seed_goal(&wsroot, "01J0GOAL", "Ship multi-project Ken");
        let grouped = filter_tasks(
            &tasks,
            &TaskFilter {
                goal: Some(g.id.clone()),
                ..Default::default()
            },
        );
        assert!(grouped.iter().any(|t| t.home == HomeKind::Project));
        assert!(needs_attention(&tasks, &[g]).is_empty());
    }

    // ---- ken-families follow-up: TaskHome::Family (folds src-tauri's old
    // `list_family_board_tasks` duplication back into the shared scanner) ----

    #[test]
    fn family_home_lists_a_board_and_defaults_project() {
        let dir = tempdir().unwrap();
        let board_dir = dir.path().join("families/FAM1/members/mem-1/board");
        let home = TaskHome::Family {
            board_dir: &board_dir,
            family_name: "The Smiths",
        };
        create_task(
            home,
            &NewTask {
                id: Some("01J0FAM1".into()),
                title: "Pick up groceries".into(),
                ..NewTask::default()
            },
            "2026-08-03",
        )
        .unwrap();
        let listed = list_tasks(home).unwrap();
        assert_eq!(listed.len(), 1);
        let t = &listed[0];
        assert_eq!(t.home, HomeKind::Family);
        assert_eq!(t.project, "The Smiths", "defaulted from the family's display name");
        assert!(
            t.path.ends_with("families/FAM1/members/mem-1/board/01J0FAM1-pick-up-groceries.md"),
            "{}",
            t.path.display()
        );
        assert_eq!(
            t.address_rel_path(),
            "members/mem-1/board/01J0FAM1-pick-up-groceries.md",
            "member id is recovered from home_dir, matching family::board_rel's shape"
        );
    }

    #[test]
    fn family_archive_stays_inside_the_family_clone() {
        let dir = tempdir().unwrap();
        let board_dir = dir.path().join("families/FAM1/members/mem-1/board");
        let home = TaskHome::Family {
            board_dir: &board_dir,
            family_name: "The Smiths",
        };
        let t = create_task(
            home,
            &NewTask {
                id: Some("01J0FAM2".into()),
                title: "Book the vet".into(),
                ..NewTask::default()
            },
            "2026-08-03",
        )
        .unwrap();
        let moved = archive_task(&t, "2026-08-15").unwrap();
        assert!(
            moved.ends_with("families/FAM1/members/mem-1/board/archive/2026-08/01J0FAM2-book-the-vet.md"),
            "{}",
            moved.display()
        );
        assert!(!t.path.exists());
    }

    #[test]
    fn family_home_dedupes_by_id_against_another_home() {
        let dir = tempdir().unwrap();
        let wsroot = dir.path().join("ws");
        let board_dir = dir.path().join("families/FAM1/members/mem-1/board");
        let w = TaskHome::Workspace {
            workspace_root: &wsroot,
        };
        let f = TaskHome::Family {
            board_dir: &board_dir,
            family_name: "The Smiths",
        };
        let mk = |home: TaskHome, title: &str| {
            create_task(
                home,
                &NewTask {
                    id: Some("01J0DUP".into()),
                    title: title.into(),
                    ..NewTask::default()
                },
                "2026-08-03",
            )
            .unwrap()
        };
        mk(w, "Workspace version");
        mk(f, "Family version");

        // Same `id` in two homes collapses to the first-home-wins task
        // (scan_tasks: "duplicate ids ... collapse to the first occurrence"),
        // exactly like a workspace/project collision already does.
        let scanned = scan_tasks(&[w, f]).unwrap();
        assert_eq!(scanned.len(), 1);
        assert_eq!(scanned[0].home, HomeKind::Workspace);
        assert_eq!(scanned[0].title, "Workspace version");

        let scanned2 = scan_tasks(&[f, w]).unwrap();
        assert_eq!(scanned2.len(), 1);
        assert_eq!(scanned2[0].home, HomeKind::Family);
        assert_eq!(scanned2[0].title, "Family version");
    }

    #[test]
    fn family_task_matches_filters_like_any_other_home() {
        let dir = tempdir().unwrap();
        let board_dir = dir.path().join("families/FAM1/members/mem-1/board");
        let home = TaskHome::Family {
            board_dir: &board_dir,
            family_name: "The Smiths",
        };
        create_task(
            home,
            &NewTask {
                id: Some("01J0FAM3".into()),
                title: "Renew passports".into(),
                fields: TaskPatch {
                    status: Some(TaskStatus::Doing),
                    tags: Some(vec!["admin".into()]),
                    ..TaskPatch::default()
                },
                ..NewTask::default()
            },
            "2026-08-03",
        )
        .unwrap();
        let tasks = list_tasks(home).unwrap();

        let hit = filter_tasks(
            &tasks,
            &TaskFilter {
                project: Some("the smiths".into()),
                ..Default::default()
            },
        );
        assert_eq!(hit.len(), 1, "project filter is case-insensitive, same as any other home");

        let hit = filter_tasks(
            &tasks,
            &TaskFilter {
                status: Some(TaskStatus::Doing),
                tag: Some("admin".into()),
                ..Default::default()
            },
        );
        assert_eq!(hit.len(), 1);

        let miss = filter_tasks(
            &tasks,
            &TaskFilter {
                status: Some(TaskStatus::Done),
                ..Default::default()
            },
        );
        assert!(miss.is_empty());
    }
}

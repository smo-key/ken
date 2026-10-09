//! Task files (`.ken-workspace/tasks/` and my board in each family): the
//! byte-faithful frontmatter core that Your day (`day.rs`), the team inbox
//! (`family.rs`) and `ken-mcp` share. The old task board (columns, goals,
//! rollover) is gone; what is left here is the file format's plumbing and
//! the patch core it was built on. Reading a task is `day::parse_task`.
//!
//! Everything here is path arithmetic, byte-faithful file patching, and
//! text composition — no `Db`/`IngestEngine`/watcher access. Callers own
//! the watcher and the clock.
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
//! - **Reads** (`day::parse_task`, `map_str`, `map_list`) use
//!   serde_yaml, exactly as S6 permits.
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

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

const TASKS_SUBDIR: &str = "tasks";
const ARCHIVE_SUBDIR: &str = "archive";

/// The `## Log` heading an accepted inbox task records its provenance
/// under ([`compose_log_entry`]).
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

/// `<tasks-home>/archive/YYYY-MM/`: archiving stays inside the task's own
/// home.
pub fn archive_dir(home_dir: &Path, year_month: &str) -> PathBuf {
    home_dir.join(ARCHIVE_SUBDIR).join(year_month)
}

/// Which home a task file lives in: the workspace home, or my board in a
/// family (`<family-clone>/members/<member-id>/board/`). Load-bearing for
/// archive pathing and the `ken://` address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HomeKind {
    Workspace,
    Family,
}

/// A task home to scan. Borrowed like `memory::MemoryScope` so callers
/// never have to know the `.ken-workspace` folder-naming details.
#[derive(Debug, Clone, Copy)]
pub enum TaskHome<'a> {
    Workspace {
        workspace_root: &'a Path,
    },
    /// A family board. The caller resolves `<clone>/members/<member-id>/
    /// board` (`family::board_dir`) since the clone root lives in app data,
    /// keyed by a family id this module does not know about.
    Family {
        board_dir: &'a Path,
    },
}

impl<'a> TaskHome<'a> {
    pub fn tasks_dir(&self) -> PathBuf {
        match self {
            TaskHome::Workspace { workspace_root } => workspace_tasks_dir(workspace_root),
            // Files live directly under `members/<id>/board/`.
            TaskHome::Family { board_dir } => board_dir.to_path_buf(),
        }
    }

    pub fn kind(&self) -> HomeKind {
        match self {
            TaskHome::Workspace { .. } => HomeKind::Workspace,
            TaskHome::Family { .. } => HomeKind::Family,
        }
    }
}

// ---------------------------------------------------------------------
// Task kind (an inbox task's payload carries one)
// ---------------------------------------------------------------------

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

/// Path of a task file relative to the member root that owns its home —
/// the tail of its `ken://` address. A family board's `home_dir` is always
/// `<clone-root>/members/<member-id>/board`, so the member id is the name
/// of its parent directory, and `family::board_rel` composes the same
/// `members/<id>/board` shape every caller uses.
pub fn home_rel_path(home: HomeKind, home_dir: &Path, file_name: &str) -> String {
    match home {
        HomeKind::Workspace => format!("{TASKS_SUBDIR}/{file_name}"),
        HomeKind::Family => {
            let member_id = home_dir
                .parent()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            format!("{}/{file_name}", crate::family::board_rel(&member_id))
        }
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

/// Like [`map_list`], but a scalar is one value, never split on commas:
/// for keys whose values may contain a comma (`assignee: Staud, Chris`).
/// Only a YAML sequence is a list.
pub(crate) fn map_values(m: &serde_yaml::Mapping, key: &str) -> Vec<String> {
    match m.get(serde_yaml::Value::String(key.to_string())) {
        Some(serde_yaml::Value::Sequence(_)) => map_list(m, key),
        Some(_) => Some(map_str(m, key).trim().to_string()).filter(|s| !s.is_empty()).into_iter().collect(),
        None => Vec::new(),
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
/// [`compose_log_entry`], which only adds a `## Log` heading if the body
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
    apply_text_change(path, &|raw: &str| {
        let appended = append_body.map(|f| {
            let body = split_raw(raw).map(|s| s.body.to_string()).unwrap_or_else(|| raw.to_string());
            f(&body)
        });
        patch_text(raw, edits, appended.as_deref())
    })
}

/// The frontmatter block and the body of a task file, as `(frontmatter,
/// body)` — `None` when the file has no (terminated) frontmatter block.
/// Read-side only; writes go through [`patch_text`] / [`replace_body`].
pub fn split_frontmatter(raw: &str) -> Option<(&str, &str)> {
    split_raw(raw).map(|s| (s.fm, s.body))
}

/// `raw` with its body replaced by `body` and every frontmatter byte kept.
/// The new body follows the closing `---` after one blank line (the shape
/// new files are written in) and ends with one terminator; an empty `body`
/// leaves just that blank line. Line endings follow the file's own.
pub fn replace_body(raw: &str, body: &str) -> String {
    let eol = detect_eol(raw);
    let text = body.trim().replace("\r\n", "\n").replace('\n', eol);
    let tail = if text.is_empty() { eol.to_string() } else { format!("{eol}{text}{eol}") };
    match split_raw(raw) {
        Some(s) => {
            let close = if s.close.ends_with('\n') { s.close.to_string() } else { format!("{}{eol}", s.close) };
            format!("{}{}{close}{tail}", s.open, s.fm)
        }
        None => format!("---{eol}---{eol}{tail}"),
    }
}

/// The optimistic-concurrency write loop behind [`apply_edits`], for any
/// pure text change: read, compute `change(raw)`, check the file did not
/// move underneath, write. A change that produces the same text writes
/// nothing.
pub fn apply_text_change(path: &Path, change: &dyn Fn(&str) -> String) -> Result<()> {
    for _ in 0..PATCH_MAX_ATTEMPTS {
        let (raw, before) = fingerprint(path)?;
        let next = change(&raw);
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
/// Tests pass their own ids instead (`day::create_task`'s `id`), the same
/// caller-supplies-nondeterminism convention `memory.rs` uses for dates.
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

/// Kebab-case filename tail for a task title. Kept local rather than
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

// ---------------------------------------------------------------------
// 1.4 Archive and log append
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

/// Archive a task file: `path` moves into
/// `<home_dir>/archive/YYYY-MM/`, keeping its name (or `-2`, `-3`, … when
/// that name is taken).
pub fn archive_file(path: &Path, home_dir: &Path, today: &str) -> Result<PathBuf> {
    let month = archive_month(today)?;
    let dir = archive_dir(home_dir, &month);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "task.md".into());
    let mut target = dir.join(&name);
    fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
    if target.exists() {
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "task".into());
        for n in 2..1000 {
            let candidate = dir.join(format!("{stem}-{n}.md"));
            if !candidate.exists() {
                target = candidate;
                break;
            }
        }
    }
    fs::rename(path, &target).map_err(|e| Error::io(path, e))?;
    Ok(target)
}

/// Text to append to a task body (an accepted inbox task's provenance): a
/// `## Log` heading if the body doesn't already have one, then a
/// `### <date> <time>` entry with the report verbatim.
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



#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap()
    }

    fn fm_map(raw: &str) -> serde_yaml::Mapping {
        let (fm, _) = split_frontmatter(raw).unwrap();
        serde_yaml::from_str(fm).unwrap()
    }

    /// The adversarial corpus in one file: a comment, a block sequence,
    /// unknown scalar and *nested* unknown keys, and a body with
    /// load-bearing whitespace. Built with `concat!` rather than a raw
    /// string so the literal stays LF regardless of how the source file is
    /// checked out (the CRLF case is exercised explicitly below).
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

    fn status_edits(status: &str, updated: &str) -> Vec<(&'static str, Vec<String>)> {
        vec![("status", scalar_lines("status", status)), ("updated", scalar_lines("updated", updated))]
    }

    // ---- patch core: byte fidelity ----

    #[test]
    fn patch_touches_only_named_keys() {
        let next = patch_text(RICH, &status_edits("doing", "2026-08-03"), None);
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
        let next = patch_text(raw, &[("target", scalar_lines("target", "2026-10-01"))], None);
        assert_eq!(next, "---\nid: t1\nstatus: todo\ntarget: '2026-10-01'\n---\n\nbody\n");
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
            assert_eq!(map_str(&fm_map(&raw), "title"), v, "round trip of {v:?} via {raw}");
        }
    }

    #[test]
    fn map_values_keeps_a_scalar_whole() {
        let m = fm_map("---\nassignee: Staud, Chris\ntags: alpha, beta\nlist: [a, b]\n---\n");
        assert_eq!(map_values(&m, "assignee"), vec!["Staud, Chris"]);
        assert_eq!(map_list(&m, "tags"), vec!["alpha", "beta"]);
        assert_eq!(map_values(&m, "list"), vec!["a", "b"]);
        assert!(map_values(&m, "missing").is_empty());
    }

    // ---- apply_edits on disk ----

    #[test]
    fn apply_edits_writes_only_the_named_keys() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tasks/01J0-x.md");
        write(&path, RICH);
        for (status, day) in [("doing", "2026-08-03"), ("review", "2026-08-04")] {
            let before = read(&path);
            apply_edits(&path, &status_edits(status, day), None).unwrap();
            let after = read(&path);
            let changed = before.lines().zip(after.lines()).filter(|(a, b)| a != b).count();
            assert_eq!(changed, 2, "{after}");
            assert_eq!(before.lines().count(), after.lines().count());
        }
        assert!(read(&path).contains("severity: high"));
        assert_eq!(map_str(&fm_map(&read(&path)), "status"), "review");
    }

    #[test]
    fn no_op_edit_does_not_rewrite_the_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tasks/01J0-x.md");
        write(&path, RICH);
        apply_edits(&path, &status_edits("todo", "2026-08-01"), None).unwrap();
        assert_eq!(read(&path), RICH, "byte-identical, no watcher churn");
    }

    #[test]
    fn an_external_edit_before_the_write_is_kept() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tasks/01J0-x.md");
        write(&path, RICH);
        let external = read(&path).replace("severity: high", "severity: high\nexternal: kept");
        write(&path, &external);
        apply_edits(&path, &status_edits("done", "2026-08-05"), None).unwrap();
        let after = read(&path);
        assert!(after.contains("external: kept"));
        assert!(after.contains("status: done"));
    }

    #[test]
    fn replace_body_keeps_the_frontmatter() {
        let next = replace_body(RICH, "New body.\nTwo lines.");
        assert!(next.starts_with("---\n# a hand-written comment\n"));
        assert!(next.ends_with("---\n\nNew body.\nTwo lines.\n"), "{next:?}");
    }

    // ---- ids, file names ----

    #[test]
    fn ulid_encoding_is_26_crockford_chars() {
        let id = ulid_from_parts(0x0192_3456_789A, [0xFF; 10]);
        assert_eq!(id.len(), 26);
        assert!(id.chars().all(|c| CROCKFORD.contains(&(c as u8))));
        assert_eq!(&id[10..], "ZZZZZZZZZZZZZZZZ");
        assert_eq!(ulid_from_parts(0, [0; 10]), "0".repeat(26));
        assert_ne!(new_ulid(), new_ulid());
    }

    #[test]
    fn file_names_are_id_then_slug() {
        assert_eq!(task_file_name("01J0", "Groom the backlog!"), "01J0-groom-the-backlog.md");
        assert_eq!(task_file_name("01J0", "!!!"), "01J0-task.md");
    }

    // ---- archive / log ----

    #[test]
    fn archive_month_cases() {
        assert_eq!(archive_month("2026-07-31").unwrap(), "2026-07");
        assert_eq!(archive_month("2026-01-01").unwrap(), "2026-01");
        assert!(archive_month("2026-7-1").is_err());
        assert!(archive_month("").is_err());
        assert!(archive_month("not-a-date").is_err());
    }

    #[test]
    fn archive_stays_in_the_home_and_never_clobbers() {
        let dir = tempdir().unwrap();
        let home = workspace_tasks_dir(dir.path());
        let path = home.join("01J0W2-done.md");
        write(&path, "---\nid: 01J0W2\n---\n");
        let first = archive_file(&path, &home, "2026-08-02").unwrap();
        assert!(first.ends_with(".ken-workspace/tasks/archive/2026-08/01J0W2-done.md"), "{}", first.display());
        assert!(!path.exists());
        write(&path, &read(&first));
        let second = archive_file(&path, &home, "2026-08-02").unwrap();
        assert_ne!(first, second);
        assert!(second.to_string_lossy().ends_with("-2.md"), "{}", second.display());
        assert!(first.exists() && second.exists());
    }

    #[test]
    fn family_board_paths_carry_the_member_id() {
        let board = Path::new("/data/families/FAM1/members/mem-1/board");
        let home = TaskHome::Family { board_dir: board };
        assert_eq!(home.tasks_dir(), board);
        assert_eq!(home.kind(), HomeKind::Family);
        assert_eq!(home_rel_path(HomeKind::Family, board, "01J0-x.md"), "members/mem-1/board/01J0-x.md");
        assert_eq!(home_rel_path(HomeKind::Workspace, Path::new("/w/.ken-workspace/tasks"), "a.md"), "tasks/a.md");
    }

    #[test]
    fn log_entry_is_pure_and_only_adds_the_heading_once() {
        let first = compose_log_entry("Body.", "2026-08-03", "10:00", "hi");
        assert!(first.starts_with("## Log\n\n"));
        let second = compose_log_entry("Body.\n\n## Log\n\n### x\n", "2026-08-03", "10:00", "hi");
        assert!(!second.contains("## Log"));
    }
}

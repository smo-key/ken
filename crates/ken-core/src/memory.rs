//! Ken's own working memory (`openspec/changes/ken-memory`): long-term
//! memory files (`.ken-workspace/memory/`, `<project>/.ken/memory/`) and
//! the short-term journal (`.ken-workspace/journal/YYYY-MM-DD.md`).
//!
//! Everything here is pure path arithmetic, frontmatter parsing, and text
//! composition — no `Db`/`IngestEngine`/watcher access (design.md: "Reuses
//! `engine.rs` unchanged"; tasks.md 1.2: "path resolution only, no engine
//! calls"). Callers (src-tauri commands, `ken-mcp` tools) own the actual
//! `Project`/`Workspace` handles, the file watcher that reindexes a write,
//! and the model call that produces distillation candidates — this module
//! only computes what those layers need.
//!
//! ## Frontmatter fidelity — a recorded conflict with spikes/S6
//!
//! `features/multi-project/spikes/S6-frontmatter-roundtrip.md` benchmarked
//! two frontmatter patch cores and concluded serde_yaml-round-trip loses
//! comments and has no no-frontmatter fallback, recommending a raw
//! line-splitter for all frontmatter *writes* (informing this very
//! change's D1/D5). This module does not follow that recommendation: task
//! 1.1 explicitly specifies `#[serde(default)]` + "flattened extras
//! preserved on rewrite" — the serde_yaml `Mapping`-flatten idiom already
//! shipped for `recipe.rs`/`automation.rs` in this crate, not a
//! line-splitter. Docs-authoritative-over-spike, and matching existing
//! precedent, wins here; the S6 finding (comments dropped, key order not
//! guaranteed) is a real, accepted trade-off for hand-edited memory files,
//! recorded rather than silently resolved either way.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::kenignore::{Rule, Tier};
use crate::{Error, Result};

const MEMORY_SUBDIR: &str = "memory";
const JOURNAL_SUBDIR: &str = "journal";
const ARCHIVE_SUBDIR: &str = "archive";

/// `.ken-workspace/memory/`, relative to the workspace parent folder
/// (`workspace::Workspace::root` — "the folder containing
/// `.ken-workspace/`", not `.ken-workspace/` itself).
pub fn workspace_memory_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join(crate::workspace::CONFIG_DIR).join(MEMORY_SUBDIR)
}

/// `<project>/.ken/memory/`.
pub fn project_memory_dir(project_root: &Path) -> PathBuf {
    project_root.join(crate::project::CONFIG_DIR).join(MEMORY_SUBDIR)
}

/// `.ken-workspace/journal/`.
pub fn journal_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join(crate::workspace::CONFIG_DIR).join(JOURNAL_SUBDIR)
}

/// `.ken-workspace/journal/archive/`.
pub fn journal_archive_dir(workspace_root: &Path) -> PathBuf {
    journal_dir(workspace_root).join(ARCHIVE_SUBDIR)
}

/// Where a memory lives — each variant carries what it needs to resolve
/// its own directory, so callers never have to know the `.ken`/
/// `.ken-workspace` folder-naming details themselves (proposal.md: long-term
/// memory is either workspace-wide or project-scoped, nothing else).
#[derive(Debug, Clone, Copy)]
pub enum MemoryScope<'a> {
    Workspace { workspace_root: &'a Path },
    Project { project_root: &'a Path },
}

impl<'a> MemoryScope<'a> {
    pub fn memory_dir(&self) -> PathBuf {
        match self {
            MemoryScope::Workspace { workspace_root } => workspace_memory_dir(workspace_root),
            MemoryScope::Project { project_root } => project_memory_dir(project_root),
        }
    }
}

// ---------------------------------------------------------------------
// 1.1 Frontmatter model
// ---------------------------------------------------------------------

/// Tolerant frontmatter (D1): only these four keys are modeled, everything
/// else round-trips through `extra` untouched.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Frontmatter {
    #[serde(default)]
    description: String,
    #[serde(default)]
    projects: Vec<String>,
    #[serde(default)]
    created: String,
    #[serde(default)]
    updated: String,
    #[serde(flatten)]
    extra: serde_yaml::Mapping,
}

/// A parsed memory file, whether or not it had frontmatter to begin with.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Memory {
    /// File stem (no `.md`), also the `write_memory` identity.
    pub slug: String,
    /// Explicit frontmatter `description`, or the body's first non-empty
    /// line (leading `#`s trimmed) when absent (D1 / spec "hand-created
    /// file is a memory").
    pub description: String,
    pub projects: Vec<String>,
    pub created: String,
    pub updated: String,
    pub body: String,
    #[serde(skip)]
    extra: serde_yaml::Mapping,
}

/// Split `---\n...\n---\n` frontmatter off the front of a file's raw text.
/// `None` means no frontmatter block (CRLF and LF both accepted) — kept
/// local rather than shared with `recipe.rs`/`automation.rs`'s identical
/// helper, same "avoid a cross-module dep" call those two already made.
fn split_frontmatter(raw: &str) -> Option<(&str, &str)> {
    let rest = raw.strip_prefix("---")?;
    let rest = rest.strip_prefix('\n').or_else(|| rest.strip_prefix("\r\n"))?;
    let end = rest.find("\n---")?;
    let fm = &rest[..end + 1];
    let after = &rest[end + 4..];
    let body = after.strip_prefix('\n').unwrap_or(after);
    Some((fm, body))
}

/// The first non-empty line of `body`, with a leading markdown heading
/// marker trimmed — the no-frontmatter description fallback.
fn first_line(body: &str) -> String {
    body.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.trim_start_matches('#').trim().to_string())
        .unwrap_or_default()
}

/// Parse a memory file's raw text. Infallible (D1: "a file with no
/// frontmatter SHALL still be a valid memory") — malformed YAML in a
/// frontmatter block degrades to empty fields rather than an error, same
/// tolerant posture as every other parser in this crate.
pub fn parse_memory(slug: &str, raw: &str) -> Memory {
    match split_frontmatter(raw) {
        Some((fm_str, body)) => {
            let fm: Frontmatter = serde_yaml::from_str(fm_str).unwrap_or_default();
            let body = body.trim().to_string();
            let description = if fm.description.trim().is_empty() {
                first_line(&body)
            } else {
                fm.description.clone()
            };
            Memory {
                slug: slug.to_string(),
                description,
                projects: fm.projects,
                created: fm.created,
                updated: fm.updated,
                body,
                extra: fm.extra,
            }
        }
        None => {
            let body = raw.trim().to_string();
            Memory {
                slug: slug.to_string(),
                description: first_line(&body),
                projects: Vec::new(),
                created: String::new(),
                updated: String::new(),
                body,
                extra: serde_yaml::Mapping::new(),
            }
        }
    }
}

/// Serialize a memory back to file text: frontmatter block (unknown keys
/// from `extra` preserved) + a blank line + the trimmed body.
fn render_memory(m: &Memory) -> Result<String> {
    let fm = Frontmatter {
        description: m.description.clone(),
        projects: m.projects.clone(),
        created: m.created.clone(),
        updated: m.updated.clone(),
        extra: m.extra.clone(),
    };
    let yaml = serde_yaml::to_string(&fm).map_err(|e| Error::Other(e.to_string()))?;
    Ok(format!("---\n{yaml}---\n\n{}\n", m.body.trim()))
}

/// A memory's identity must be a plain, file-name-safe slug — same shape
/// as `recipe.rs`/`automation.rs` slugs.
pub fn validate_slug(slug: &str) -> Result<()> {
    let trimmed = slug.trim();
    if trimmed.is_empty()
        || trimmed != slug
        || slug.contains('/')
        || slug.contains('\\')
        || slug.starts_with('.')
    {
        return Err(Error::Other(format!(
            "'{slug}' is not a valid memory slug — use a simple file-name-safe slug"
        )));
    }
    Ok(())
}

fn memory_file_path(dir: &Path, slug: &str) -> PathBuf {
    dir.join(format!("{slug}.md"))
}

/// Load and parse every `.md` file directly in `dir` (not recursive — a
/// memory folder is flat), sorted by slug for determinism. A missing
/// folder reads as no memories, not an error (folders are created lazily
/// on first write per src-tauri task 2.1, so "not created yet" is the
/// common case).
pub fn list_memories(dir: &Path) -> Result<Vec<Memory>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut slugs: Vec<String> = fs::read_dir(dir)
        .map_err(|e| Error::io(dir, e))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "md"))
        .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .collect();
    slugs.sort();
    let mut out = Vec::with_capacity(slugs.len());
    for slug in slugs {
        let path = memory_file_path(dir, &slug);
        let raw = fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
        out.push(parse_memory(&slug, &raw));
    }
    Ok(out)
}

// ---------------------------------------------------------------------
// 1.2 write_memory / append_journal
// ---------------------------------------------------------------------

/// `memory_write`'s two intents (spec: "called in create mode with an
/// existing slug" ⇒ error; `memory_write` also replaces an existing
/// memory's body — design D5's "create or replace body"). Kept as an
/// explicit mode rather than an implicit upsert so the collision-error
/// requirement and the body-swap-on-replace requirement can't contradict
/// each other on the same call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteMode {
    /// Slug must not already exist; creates `created == updated == today`.
    Create,
    /// Slug must already exist; swaps the body and bumps `updated`. Every
    /// other frontmatter field (description, projects, created, unknown
    /// extras) carries over unchanged from the file on disk — "replace ⇒
    /// body swap + `updated` bump" (tasks.md 1.2), nothing more.
    Replace,
}

/// Create or replace a memory's body at `scope`/`slug` (D5). Path
/// resolution + frontmatter read/write only — no engine/DB/watcher call;
/// the caller's file watcher is what picks up the write and reindexes.
pub fn write_memory(
    scope: MemoryScope,
    slug: &str,
    content: &str,
    mode: WriteMode,
    today: &str,
) -> Result<PathBuf> {
    validate_slug(slug)?;
    let dir = scope.memory_dir();
    let path = memory_file_path(&dir, slug);
    match mode {
        WriteMode::Create => {
            if path.exists() {
                return Err(Error::Other(format!(
                    "a memory named '{slug}' already exists"
                )));
            }
            let body = content.trim().to_string();
            let m = Memory {
                slug: slug.to_string(),
                description: first_line(&body),
                projects: Vec::new(),
                created: today.to_string(),
                updated: today.to_string(),
                body,
                extra: serde_yaml::Mapping::new(),
            };
            fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
            fs::write(&path, render_memory(&m)?).map_err(|e| Error::io(&path, e))?;
        }
        WriteMode::Replace => {
            if !path.exists() {
                return Err(Error::Other(format!(
                    "no memory named '{slug}' exists to replace"
                )));
            }
            let raw = fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
            let mut m = parse_memory(slug, &raw);
            m.body = content.trim().to_string();
            m.updated = today.to_string();
            if m.created.trim().is_empty() {
                // A hand-created file with no frontmatter yet — this is the
                // first Ken-managed write, so it establishes `created`.
                m.created = today.to_string();
            }
            fs::write(&path, render_memory(&m)?).map_err(|e| Error::io(&path, e))?;
        }
    }
    Ok(path)
}

/// Append a `## HH:MM` entry to today's journal file, creating it (and the
/// `journal/` folder) if absent. `today`/`time_hhmm` are caller-supplied
/// (mirrors `engine::now_epoch`'s caller-passes-time convention, and
/// `knowledge_model.rs`'s `today: &str` parameter already used throughout
/// this crate) — this function never reads the wall clock, so tests can
/// assert exact output.
pub fn append_journal(
    workspace_root: &Path,
    text: &str,
    project: Option<&str>,
    tags: &[String],
    today: &str,
    time_hhmm: &str,
) -> Result<PathBuf> {
    let dir = journal_dir(workspace_root);
    fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
    let path = dir.join(format!("{today}.md"));

    let mut entry = format!("## {time_hhmm}\n");
    if let Some(p) = project {
        if !p.trim().is_empty() {
            entry.push_str(&format!("Project: {}\n", p.trim()));
        }
    }
    if !tags.is_empty() {
        entry.push_str(&format!("Tags: {}\n", tags.join(", ")));
    }
    entry.push_str(text.trim());
    entry.push('\n');

    let mut existing = fs::read_to_string(&path).unwrap_or_default();
    if !existing.is_empty() {
        if !existing.ends_with('\n') {
            existing.push('\n');
        }
        existing.push('\n'); // blank line between entries
    }
    existing.push_str(&entry);
    fs::write(&path, existing).map_err(|e| Error::io(&path, e))?;
    Ok(path)
}

// ---------------------------------------------------------------------
// 1.3 Injection builder
// ---------------------------------------------------------------------

/// Whole-file injection budget in characters (D4). A constant, not
/// config, until real use argues otherwise (mirrors `ARCHIVE_AFTER_DAYS`
/// below and `recipe.rs`'s `DEFAULT_RULES` posture).
pub const INJECTION_BUDGET_CHARS: usize = 4_000;

fn render_whole(m: &Memory) -> String {
    format!("### {}\n{}\n\n", m.slug, m.body.trim())
}

fn render_description_only(m: &Memory) -> String {
    format!("### {}\n{}\n\n", m.slug, m.description)
}

/// Build the `## Memories` chat-context block from an already-loaded list
/// of memories (workspace-scope + the focused project's, per D4 — the
/// caller decides which `Memory`s to pass in; this function is pure over
/// that list). Ordered by `updated` descending; each memory is injected
/// whole if it fits in what's left of the 4,000-char budget, otherwise it
/// contributes only its `### slug` heading + description line and
/// injection continues to the next memory (spec: "over-budget memory
/// degrades to its description"). Empty input yields an empty string —
/// no empty `## Memories` heading is injected for nothing.
pub fn build_injection(memories: &[Memory]) -> String {
    if memories.is_empty() {
        return String::new();
    }
    let mut ordered: Vec<&Memory> = memories.iter().collect();
    ordered.sort_by(|a, b| b.updated.cmp(&a.updated));

    let mut out = String::from("## Memories\n\n");
    let mut used = 0usize;
    for m in ordered {
        let whole = render_whole(m);
        if used + whole.len() <= INJECTION_BUDGET_CHARS {
            used += whole.len();
            out.push_str(&whole);
        } else {
            let fallback = render_description_only(m);
            used += fallback.len();
            out.push_str(&fallback);
        }
    }
    out
}

// ---------------------------------------------------------------------
// 1.4 Archive roll
// ---------------------------------------------------------------------

/// Journal files older than this many days roll to `journal/archive/`
/// (D2). A constant, not config, until real use argues otherwise.
pub const ARCHIVE_AFTER_DAYS: i64 = 30;

/// Days since 1970-01-01 for a proleptic-Gregorian `(y, m, d)` date —
/// Howard Hinnant's public-domain `days_from_civil` algorithm
/// (https://howardhinnant.github.io/date_algorithms.html). Used instead
/// of pulling in a date/time crate (none is a `ken-core` dependency)
/// purely to diff two `YYYY-MM-DD` journal-adjacent dates.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (m + 9) % 12; // [0, 11]: Mar=0 .. Feb=11
    let doy = (153 * mp + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// Parse a strict `YYYY-MM-DD` string into a day count. `None` for
/// anything else (including archive-adjacent files that aren't dated
/// journal entries) — the archive roll skips those tolerantly.
fn parse_iso_date(s: &str) -> Option<i64> {
    let mut parts = s.splitn(3, '-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next()?.parse().ok()?;
    let d: i64 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, m, d))
}

/// Move every `journal/YYYY-MM-DD.md` file more than `ARCHIVE_AFTER_DAYS`
/// old (relative to caller-supplied `today`) into `journal/archive/`,
/// same filename. Idempotent: a file already under `archive/` is never
/// re-listed (it's no longer in `journal/` itself), so a second run
/// against the same `today` moves nothing. Returns the moved filenames,
/// sorted. A missing `journal/` folder yields no moves, not an error.
pub fn roll_archive(workspace_root: &Path, today: &str) -> Result<Vec<String>> {
    let dir = journal_dir(workspace_root);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let today_days = parse_iso_date(today)
        .ok_or_else(|| Error::Other(format!("invalid date '{today}'")))?;
    let archive_dir = journal_archive_dir(workspace_root);

    let mut names: Vec<String> = fs::read_dir(&dir)
        .map_err(|e| Error::io(&dir, e))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    names.sort();

    let mut moved = Vec::new();
    for name in names {
        let Some(stem) = name.strip_suffix(".md") else {
            continue;
        };
        let Some(file_days) = parse_iso_date(stem) else {
            continue;
        };
        if today_days - file_days > ARCHIVE_AFTER_DAYS {
            fs::create_dir_all(&archive_dir).map_err(|e| Error::io(&archive_dir, e))?;
            let from = dir.join(&name);
            let to = archive_dir.join(&name);
            fs::rename(&from, &to).map_err(|e| Error::io(&from, e))?;
            moved.push(name);
        }
    }
    Ok(moved)
}

// ---------------------------------------------------------------------
// 1.5 Distillation prompt + tolerant parse
// ---------------------------------------------------------------------

/// Candidates per distillation run, capped (D6/spec).
pub const MAX_DISTILL_CANDIDATES: usize = 5;

/// One proposed long-term memory, awaiting approval (D6). Never written
/// to `memory/` directly — the caller renders this as an approval card;
/// approval calls `write_memory` with `WriteMode::Create`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct DistillCandidate {
    pub slug: String,
    pub description: String,
    pub body: String,
    pub sources: Vec<String>,
}

/// The distillation prompt: journal window + existing memory descriptions
/// as a dedupe guard (D6), same compose-prompt shape as
/// `digest.rs::compose_digest_prompt` / `knowledge_model.rs`'s prompts —
/// prose contract followed by a JSON-object output shape.
pub fn compose_distill_prompt(journal_window: &str, existing: &[(String, String)]) -> String {
    let mut p = String::from(
        "You are Ken, distilling recent journal entries into candidate \
long-term memories — small, curated notes about ways of working, \
conventions, or standing decisions that keep recurring. Read the journal \
window below and propose AT MOST 5 candidates, only for themes that \
clearly recur or matter going forward; when in doubt, propose nothing.\n\n\
Do not repeat anything already captured — these memories already exist:\n",
    );
    if existing.is_empty() {
        p.push_str("- none yet\n");
    }
    for (slug, description) in existing {
        p.push_str(&format!("- {slug}: {description}\n"));
    }
    p.push_str(
        "\nOutput ONLY a JSON object — no prose before or after, no code \
fences — shaped exactly like this:\n\
{\n  \"candidates\": [\n    {\n      \"slug\": \"kebab-case-file-name\",\n\
      \"description\": \"one-line hook\",\n\
      \"body\": \"the memory's full markdown body\",\n\
      \"sources\": [\"journal/2026-07-24.md\"]\n    }\n  ]\n}\n\
An empty \"candidates\" array is a completely valid answer.\n\n\
Journal window:\n",
    );
    p.push_str(journal_window);
    p
}

fn non_empty_str(v: &serde_json::Value) -> Option<String> {
    v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from)
}

fn string_list(v: &serde_json::Value) -> Vec<String> {
    v.as_array()
        .map(|arr| arr.iter().filter_map(non_empty_str).collect())
        .unwrap_or_default()
}

/// Parse the distillation model's answer. Infallible and tolerant (spec:
/// "garbage model output proposes nothing"): no JSON object, invalid
/// JSON, an invalid slug, or a missing `slug`/`body` drops that candidate
/// (or the whole answer) rather than erroring. Capped at
/// `MAX_DISTILL_CANDIDATES`.
pub fn parse_distill_candidates(raw: &str) -> Vec<DistillCandidate> {
    let Some(start) = raw.find('{') else {
        return Vec::new();
    };
    let Some(end) = raw.rfind('}').filter(|e| *e > start) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw[start..=end]) else {
        return Vec::new();
    };

    let empty = Vec::new();
    let mut out = Vec::new();
    for item in value["candidates"].as_array().unwrap_or(&empty) {
        if out.len() >= MAX_DISTILL_CANDIDATES {
            break;
        }
        let Some(slug) = non_empty_str(&item["slug"]) else {
            continue;
        };
        if validate_slug(&slug).is_err() {
            continue;
        }
        let Some(body) = non_empty_str(&item["body"]) else {
            continue;
        };
        let description =
            non_empty_str(&item["description"]).unwrap_or_else(|| first_line(&body));
        out.push(DistillCandidate {
            slug,
            description,
            body,
            sources: string_list(&item["sources"]),
        });
    }
    out
}

// ---------------------------------------------------------------------
// 1.6 Workspace pseudo-member
// ---------------------------------------------------------------------

/// The literal `ken://` host for every workspace-pseudo-member address
/// (spec: `ken://workspace/memory/<file>`) — independent of the reserved
/// UUID below, which only names the on-disk derived DB
/// (`db::db_path(base, <reserved-uuid>)`, D3), never the address itself.
pub const WORKSPACE_ADDRESS_ID: &str = "workspace";

/// Derive the workspace pseudo-member's reserved project id from the
/// workspace's own id (D3: "a fixed namespace-uuid of the workspace id").
///
/// `uuid`'s Cargo feature set in this workspace is `["v4", "serde"]` only
/// (root `Cargo.toml`) — no `v5`, so this cannot use `Uuid::new_v5`
/// without adding a dependency feature, which is out of scope here.
/// Instead: hash `workspace_id`'s bytes with two independently-seeded
/// `XxHash64` passes (the same primitive `chunker.rs`'s `content_hash` and
/// `embedder.rs`'s deterministic fallback vectors already use in this
/// crate) into 16 bytes, then stamp them as an RFC 9562 version-8
/// ("custom") / RFC-4122-variant UUID. Deterministic and, because every
/// `Uuid::new_v4()`-generated project/workspace id in this codebase always
/// carries version nibble `0100`, this can never collide with one
/// (Risks/Trade-offs: "Reserved id collisions").
pub fn workspace_pseudo_member_id(workspace_id: Uuid) -> Uuid {
    let bytes = workspace_id.as_bytes();
    let h1 = twox_hash::XxHash64::oneshot(0x4B454E5F4D454D31, bytes); // "KEN_MEM1"
    let h2 = twox_hash::XxHash64::oneshot(0x4B454E5F4D454D32, bytes); // "KEN_MEM2"
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(&h1.to_be_bytes());
    out[8..].copy_from_slice(&h2.to_be_bytes());
    out[6] = (out[6] & 0x0F) | 0x80; // version 8 (custom)
    out[8] = (out[8] & 0x3F) | 0x80; // RFC 4122 variant
    Uuid::from_bytes(out)
}

/// Built-in tier rules for classifying the workspace pseudo-member's own
/// files (D3): `memory/` full, `journal/` (current + `archive/`, covered
/// by one directory-prefix rule) and `tasks/` search-only, `workspace.json`
/// and `kg.sqlite` ignored.
///
/// Deliberately **not** wired into `kenignore::built_in_rule_sets()`:
/// that function is parameterless and folded into *every* project's
/// classify call (`scan.rs` lines ~121/292, unconditionally, for every
/// member), so filling it with these patterns would apply workspace-only
/// semantics (e.g. ignoring any regular project's own root-level
/// `workspace.json`/`kg.sqlite`, or demoting any project's own `journal/`
/// or `tasks/` folder to search-only) to every ordinary member project —
/// contradicting D3, which scopes these rules to the pseudo-member only.
/// This function is the seam instead: whichever call classifies the
/// pseudo-member's own files (src-tauri task 2.1, spinning up its engine
/// instance) folds `workspace_builtin_rules()` into that classify call's
/// `rule_sets` specifically, the same way `Project::kenignore_rules()`
/// supplies the user tier for an ordinary project.
pub fn workspace_builtin_rules() -> Vec<Rule> {
    vec![
        Rule { tier: Tier::Full, pattern: "/memory/".to_string() },
        Rule { tier: Tier::SearchOnly, pattern: "/journal/".to_string() },
        Rule { tier: Tier::SearchOnly, pattern: "/tasks/".to_string() },
        Rule { tier: Tier::Ignore, pattern: "/workspace.json".to_string() },
        Rule { tier: Tier::Ignore, pattern: "/kg.sqlite".to_string() },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // ---- 1.1 frontmatter round-trip ----

    #[test]
    fn frontmatter_round_trip_preserves_unknown_keys() {
        let raw = "---\ndescription: How we name things\nprojects:\n  - Atlas\ncreated: '2026-07-01'\nupdated: '2026-07-01'\nfutureField: keep-me\n---\n\nBody text.\n";
        let m = parse_memory("naming", raw);
        assert_eq!(m.description, "How we name things");
        assert_eq!(m.projects, vec!["Atlas".to_string()]);
        assert_eq!(m.created, "2026-07-01");
        assert_eq!(m.body, "Body text.");

        let rendered = render_memory(&m).unwrap();
        assert!(rendered.contains("futureField: keep-me"), "{rendered}");
        assert!(rendered.contains("Body text."));

        let reparsed = parse_memory("naming", &rendered);
        assert_eq!(reparsed, m);
    }

    #[test]
    fn no_frontmatter_file_is_still_a_valid_memory() {
        let m = parse_memory("adhoc", "# Ways of working\n\nSome details here.\n");
        assert_eq!(m.description, "Ways of working");
        assert_eq!(m.body, "# Ways of working\n\nSome details here.");
        assert!(m.created.is_empty());
    }

    // ---- 1.2 write_memory / append_journal ----

    #[test]
    fn write_memory_create_then_collision_errors() {
        let dir = tempdir().unwrap();
        let scope = MemoryScope::Workspace { workspace_root: dir.path() };
        let path = write_memory(scope, "style", "Prefer tabs.", WriteMode::Create, "2026-07-01")
            .unwrap();
        assert!(path.exists());
        let m = parse_memory("style", &fs::read_to_string(&path).unwrap());
        assert_eq!(m.created, "2026-07-01");
        assert_eq!(m.updated, "2026-07-01");

        let err = write_memory(scope, "style", "Prefer spaces.", WriteMode::Create, "2026-07-02")
            .unwrap_err();
        assert!(err.to_string().contains("already exists"));
        // Untouched.
        let still = parse_memory("style", &fs::read_to_string(&path).unwrap());
        assert_eq!(still.body, "Prefer tabs.");
    }

    #[test]
    fn write_memory_replace_swaps_body_and_bumps_updated() {
        let dir = tempdir().unwrap();
        let scope = MemoryScope::Workspace { workspace_root: dir.path() };
        write_memory(scope, "style", "Prefer tabs.", WriteMode::Create, "2026-07-01").unwrap();

        let path =
            write_memory(scope, "style", "Prefer spaces.", WriteMode::Replace, "2026-07-15")
                .unwrap();
        let m = parse_memory("style", &fs::read_to_string(&path).unwrap());
        assert_eq!(m.body, "Prefer spaces.");
        assert_eq!(m.created, "2026-07-01", "created is untouched by replace");
        assert_eq!(m.updated, "2026-07-15");
    }

    #[test]
    fn write_memory_replace_missing_slug_errors() {
        let dir = tempdir().unwrap();
        let scope = MemoryScope::Workspace { workspace_root: dir.path() };
        let err = write_memory(scope, "ghost", "x", WriteMode::Replace, "2026-07-01")
            .unwrap_err();
        assert!(err.to_string().contains("no memory named"));
    }

    #[test]
    fn append_journal_formats_hhmm_heading_and_creates_today() {
        let dir = tempdir().unwrap();
        let path =
            append_journal(dir.path(), "Shipped the search fix.", None, &[], "2026-07-01", "09:15")
                .unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        assert_eq!(raw, "## 09:15\nShipped the search fix.\n");
        assert_eq!(path, journal_dir(dir.path()).join("2026-07-01.md"));
    }

    #[test]
    fn append_journal_includes_project_and_tags() {
        let dir = tempdir().unwrap();
        let path = append_journal(
            dir.path(),
            "Agent finished the report.",
            Some("Atlas"),
            &["automation".to_string(), "agent-desktop".to_string()],
            "2026-07-01",
            "10:00",
        )
        .unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("Project: Atlas\n"));
        assert!(raw.contains("Tags: automation, agent-desktop\n"));
    }

    #[test]
    fn append_journal_accumulates_same_day_separate_files_across_midnight() {
        let dir = tempdir().unwrap();
        append_journal(dir.path(), "Morning note.", None, &[], "2026-07-01", "08:00").unwrap();
        append_journal(dir.path(), "Evening note.", None, &[], "2026-07-01", "20:00").unwrap();
        let same_day = fs::read_to_string(journal_dir(dir.path()).join("2026-07-01.md")).unwrap();
        assert_eq!(same_day, "## 08:00\nMorning note.\n\n## 20:00\nEvening note.\n");

        // Crossing midnight starts a new file; yesterday's is untouched.
        append_journal(dir.path(), "Next day.", None, &[], "2026-07-02", "00:05").unwrap();
        assert!(journal_dir(dir.path()).join("2026-07-02.md").exists());
        let still = fs::read_to_string(journal_dir(dir.path()).join("2026-07-01.md")).unwrap();
        assert_eq!(still, same_day);
    }

    // ---- 1.3 injection builder ----

    fn mem(slug: &str, updated: &str, body: &str) -> Memory {
        Memory {
            slug: slug.to_string(),
            description: format!("{slug} description"),
            projects: Vec::new(),
            created: updated.to_string(),
            updated: updated.to_string(),
            body: body.to_string(),
            extra: serde_yaml::Mapping::new(),
        }
    }

    #[test]
    fn injection_empty_list_is_empty_string() {
        assert_eq!(build_injection(&[]), "");
    }

    #[test]
    fn injection_orders_by_updated_desc() {
        let memories = vec![
            mem("old", "2026-01-01", "old body"),
            mem("new", "2026-07-01", "new body"),
            mem("mid", "2026-04-01", "mid body"),
        ];
        let out = build_injection(&memories);
        let pos_new = out.find("### new").unwrap();
        let pos_mid = out.find("### mid").unwrap();
        let pos_old = out.find("### old").unwrap();
        assert!(pos_new < pos_mid && pos_mid < pos_old);
    }

    #[test]
    fn injection_over_budget_memory_degrades_to_description() {
        let big = "x".repeat(3_970);
        let memories = vec![
            mem("first", "2026-07-02", &big),
            mem("second", "2026-07-01", "small body that would still fit alone"),
        ];
        let out = build_injection(&memories);
        assert!(out.contains(&big), "first memory injected whole");
        assert!(
            !out.contains("small body that would still fit alone"),
            "second memory's body must NOT appear once budget is exceeded"
        );
        assert!(
            out.contains("second description"),
            "second memory falls back to its description line"
        );
    }

    // ---- 1.4 archive roll ----

    #[test]
    fn archive_roll_moves_only_files_older_than_30_days() {
        let dir = tempdir().unwrap();
        let jdir = journal_dir(dir.path());
        fs::create_dir_all(&jdir).unwrap();
        fs::write(jdir.join("2026-07-31.md"), "recent\n").unwrap(); // 1 day old
        fs::write(jdir.join("2026-07-02.md"), "boundary\n").unwrap(); // exactly 30 days
        fs::write(jdir.join("2026-07-01.md"), "old\n").unwrap(); // 31 days old

        let moved = roll_archive(dir.path(), "2026-08-01").unwrap();
        assert_eq!(moved, vec!["2026-07-01.md".to_string()]);
        assert!(journal_archive_dir(dir.path()).join("2026-07-01.md").exists());
        assert!(jdir.join("2026-07-31.md").exists());
        assert!(jdir.join("2026-07-02.md").exists(), "exactly 30 days stays put");
        assert!(!jdir.join("2026-07-01.md").exists());
    }

    #[test]
    fn archive_roll_is_idempotent() {
        let dir = tempdir().unwrap();
        let jdir = journal_dir(dir.path());
        fs::create_dir_all(&jdir).unwrap();
        fs::write(jdir.join("2026-01-01.md"), "ancient\n").unwrap();

        let first = roll_archive(dir.path(), "2026-08-01").unwrap();
        assert_eq!(first, vec!["2026-01-01.md".to_string()]);
        let second = roll_archive(dir.path(), "2026-08-01").unwrap();
        assert!(second.is_empty(), "second run moves nothing");
    }

    #[test]
    fn archive_roll_missing_journal_dir_is_a_noop() {
        let dir = tempdir().unwrap();
        assert_eq!(roll_archive(dir.path(), "2026-08-01").unwrap(), Vec::<String>::new());
    }

    // ---- 1.5 distillation parse ----

    #[test]
    fn parse_distill_valid_candidates() {
        let raw = r#"{"candidates": [
            {"slug": "review-cadence", "description": "How we review PRs", "body": "Full body.", "sources": ["journal/2026-07-01.md"]},
            {"slug": "naming", "description": "", "body": "Prefix everything with feat/fix.", "sources": []}
        ]}"#;
        let out = parse_distill_candidates(raw);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].slug, "review-cadence");
        assert_eq!(out[0].sources, vec!["journal/2026-07-01.md".to_string()]);
        // Empty description falls back to the body's first line.
        assert_eq!(out[1].description, "Prefix everything with feat/fix.");
    }

    #[test]
    fn parse_distill_partial_drops_bad_records_keeps_good_ones() {
        let raw = r#"{"candidates": [
            {"slug": "", "description": "no slug", "body": "x"},
            {"slug": "bad/slug", "description": "invalid", "body": "x"},
            {"slug": "no-body", "description": "missing body"},
            {"slug": "ok", "description": "fine", "body": "Kept."}
        ]}"#;
        let out = parse_distill_candidates(raw);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].slug, "ok");
    }

    #[test]
    fn parse_distill_caps_at_five() {
        let items: Vec<String> = (0..8)
            .map(|i| format!(r#"{{"slug": "s{i}", "description": "d", "body": "b"}}"#))
            .collect();
        let raw = format!(r#"{{"candidates": [{}]}}"#, items.join(","));
        assert_eq!(parse_distill_candidates(&raw).len(), MAX_DISTILL_CANDIDATES);
    }

    #[test]
    fn parse_distill_garbage_is_empty() {
        assert!(parse_distill_candidates("not json at all").is_empty());
        assert!(parse_distill_candidates("").is_empty());
        assert!(parse_distill_candidates("{ this is not valid json").is_empty());
        assert!(parse_distill_candidates(r#"{"candidates": "not an array"}"#).is_empty());
    }

    #[test]
    fn compose_distill_prompt_carries_dedupe_guard_and_window() {
        let existing = vec![("style".to_string(), "How we name things".to_string())];
        let prompt = compose_distill_prompt("## 09:00\nShipped X.\n", &existing);
        assert!(prompt.contains("style: How we name things"));
        assert!(prompt.contains("Shipped X."));
        assert!(prompt.contains("\"candidates\""));
    }

    // ---- 1.6 workspace pseudo-member ----

    #[test]
    fn pseudo_member_id_is_deterministic_and_never_collides_with_v4() {
        let ws = Uuid::new_v4();
        let a = workspace_pseudo_member_id(ws);
        let b = workspace_pseudo_member_id(ws);
        assert_eq!(a, b, "deterministic for the same workspace id");
        assert_ne!(a, ws);
        // Version nibble 8 (custom) can never come out of Uuid::new_v4()
        // (version nibble 4), so this reserved id can never collide with a
        // real project/workspace id, by construction.
        assert_eq!(a.get_version_num(), 8);

        let other_ws = Uuid::new_v4();
        assert_ne!(workspace_pseudo_member_id(other_ws), a);
    }

    #[test]
    fn workspace_builtin_rules_classify_as_designed() {
        let rules = workspace_builtin_rules();
        let rs: &[&[Rule]] = &[&rules];
        assert_eq!(crate::kenignore::classify("memory/style.md", false, rs), Tier::Full);
        assert_eq!(crate::kenignore::classify("journal/2026-07-01.md", false, rs), Tier::SearchOnly);
        assert_eq!(
            crate::kenignore::classify("journal/archive/2026-01-01.md", false, rs),
            Tier::SearchOnly
        );
        assert_eq!(crate::kenignore::classify("tasks/todo.md", false, rs), Tier::SearchOnly);
        assert_eq!(crate::kenignore::classify("workspace.json", false, rs), Tier::Ignore);
        assert_eq!(crate::kenignore::classify("kg.sqlite", false, rs), Tier::Ignore);
        // Unrelated paths default to Full, unaffected.
        assert_eq!(crate::kenignore::classify("tasks_notes.md", false, rs), Tier::Full);
    }
}

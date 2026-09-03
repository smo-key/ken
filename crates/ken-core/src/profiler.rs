//! Project profiling (project-profiler): a deterministic, LLM-free scan of a
//! project's shape (`scan_stats` + `deterministic_profile`), an optional
//! Background-priority LLM refinement pass over that scan
//! (`compose_profile_prompt` + `parse_profile_refinement`), and the
//! human-editable `.ken/index-profile.json` the result lives in
//! (`ProjectProfile`). Downstream consumers (chunking, exclusions, the
//! knowledge-model prompt) read the saved profile through narrow seams —
//! see `engine::rebuild_semantic_index_with_profile`,
//! `Project::effective_excluded`, and `profile_prompt_addendum`.
//!
//! Design D1 (two-stage: deterministic floor, LLM ceiling): every project
//! gets a usable profile with zero LLM involvement; the LLM only refines,
//! additively, and never on its own can remove a deterministic exclude or
//! invent a nonexistent one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::chunker::{ChunkMode, IndexProfile};
use crate::{Error, Result};

/// Hard cap on files walked by [`scan_stats`]. Guards against pathological
/// (huge, deeply-nested, or symlink-cyclic) trees turning a profiling pass
/// into an unbounded I/O sweep — see design.md's "Scan cost on huge trees"
/// risk. The scan does no hashing or content reads either way, so this is a
/// belt-and-suspenders cap, not the primary cost control.
pub const MAX_SCAN_FILES: usize = 50_000;

/// How many of the largest directories (by aggregate file bytes) `scan_stats`
/// keeps. Only the biggest few matter for kind/exclude heuristics and the
/// refinement tree sample; keeping all of them would make `ScanStats` grow
/// with tree size for no benefit.
const MAX_LARGEST_DIRS: usize = 20;

/// File names (and, for `*.sln`, an extension) that mark a folder as a known
/// project type. `.git` is checked separately in `scan_stats` since the walk
/// itself never descends into hidden directories (see its doc comment).
pub const REPO_MARKERS: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "Gemfile",
    "requirements.txt",
    "CMakeLists.txt",
];

/// Marker → build-output directories to exclude, keyed by the exact string
/// `scan_stats` records in `ScanStats::markers`. Deliberately narrow and
/// well-known; anything the marker table doesn't anticipate is exactly what
/// the LLM refinement pass (task 1.4) exists to catch, additively.
const MARKER_EXCLUDES: &[(&str, &[&str])] = &[
    ("Cargo.toml", &["target"]),
    ("package.json", &["node_modules", "dist", "build"]),
    ("pyproject.toml", &["__pycache__", "venv", ".venv", "dist", "build"]),
    ("go.mod", &["vendor"]),
    ("pom.xml", &["target"]),
    ("build.gradle", &["build", ".gradle"]),
    ("*.sln", &["bin", "obj"]),
];

/// Extension → display language, for `deterministic_profile`'s language
/// guess. Deliberately excludes prose extensions (`chunker::PROSE_EXTS`) —
/// "language" here means a programming language, not a document format.
const LANGUAGE_EXTS: &[(&str, &str)] = &[
    ("rs", "Rust"),
    ("ts", "TypeScript"),
    ("tsx", "TypeScript"),
    ("js", "JavaScript"),
    ("jsx", "JavaScript"),
    ("mjs", "JavaScript"),
    ("cjs", "JavaScript"),
    ("py", "Python"),
    ("go", "Go"),
    ("java", "Java"),
    ("c", "C"),
    ("h", "C"),
    ("cc", "C++"),
    ("cpp", "C++"),
    ("hpp", "C++"),
    ("cs", "C#"),
    ("rb", "Ruby"),
    ("php", "PHP"),
    ("swift", "Swift"),
    ("kt", "Kotlin"),
    ("kts", "Kotlin"),
    ("sql", "SQL"),
    ("sh", "Shell"),
    ("bash", "Shell"),
    ("ps1", "PowerShell"),
    ("svelte", "Svelte"),
    ("vue", "Vue"),
];

/// Extensions whose bytes count toward `ScanStats::media_bytes` for the
/// `media` kind heuristic.
const MEDIA_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "svg", "mp4", "mov", "avi", "mkv", "webm", "mp3",
    "wav", "flac", "m4a", "ogg",
];

/// Per-extension count + total bytes, as recorded by [`scan_stats`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ExtStat {
    pub count: usize,
    pub bytes: u64,
}

/// Pure structural facts about a project tree — no interpretation, no LLM.
/// `deterministic_profile` turns this into a [`ProjectProfile`]; the
/// refinement prompt (`compose_profile_prompt`) summarizes it for the LLM.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ScanStats {
    pub total_files: usize,
    pub total_bytes: u64,
    /// Extension (no dot, lowercase; empty string for extensionless files) →
    /// count + bytes.
    pub extensions: HashMap<String, ExtStat>,
    /// Names of directories directly under the root, sorted.
    pub top_level_dirs: Vec<String>,
    /// Repo marker file names found anywhere in the tree (see
    /// [`REPO_MARKERS`]), plus `".git"` and/or `"*.sln"` when present.
    pub markers: Vec<String>,
    /// Total bytes of files with a `chunker::PROSE_EXTS` extension.
    pub doc_bytes: u64,
    /// Total bytes of files with a `chunker::CODE_EXTS` extension.
    pub code_bytes: u64,
    /// Total bytes of files with a [`MEDIA_EXTS`] extension.
    pub media_bytes: u64,
    /// The [`MAX_LARGEST_DIRS`] directories (project-relative,
    /// forward-slash) with the most aggregate file bytes under them,
    /// largest first.
    pub largest_dirs: Vec<(String, u64)>,
    /// `true` if the walk hit [`MAX_SCAN_FILES`] and stopped early — the
    /// stats above are a partial (but still usable) picture.
    pub truncated: bool,
}

impl ScanStats {
    /// Fraction (0.0-1.0) of (doc + code) bytes that are doc bytes. `0.0`
    /// when there are no doc or code bytes at all (e.g. an all-media or
    /// all-binary tree) rather than dividing by zero.
    pub fn doc_ratio(&self) -> f64 {
        let denom = self.doc_bytes + self.code_bytes;
        if denom == 0 {
            0.0
        } else {
            self.doc_bytes as f64 / denom as f64
        }
    }

    /// Fraction (0.0-1.0) of all scanned bytes that are media bytes.
    pub fn media_ratio(&self) -> f64 {
        if self.total_bytes == 0 {
            0.0
        } else {
            self.media_bytes as f64 / self.total_bytes as f64
        }
    }
}

/// Is `rel` (project-relative, forward-slash, no leading `/`) inside one of
/// `excluded`'s folders? Mirrors `Project::is_excluded` exactly — duplicated
/// rather than taking a `&Project` because `scan_stats` needs to run over a
/// bare `excluded` list (matching its task-1.1 signature) without requiring
/// a full `Project` (loose fixture trees in tests, or a candidate folder
/// during workspace creation that isn't a `Project` yet).
fn is_excluded(excluded: &[String], rel: &str) -> bool {
    let rel = rel.trim_start_matches('/');
    excluded.iter().any(|ex| {
        let ex = ex.trim_matches('/');
        !ex.is_empty() && (rel == ex || rel.starts_with(&format!("{ex}/")))
    })
}

/// The same walk configuration `scan::scan` uses (D1: "reusing the ingest
/// walk rules") — hidden files/dirs skipped, `.ken` and Office lock files
/// skipped, junk build-output dirs (`node_modules`, `target`, ...) never
/// descended into. `.kenignore` tiers are NOT applied here: the profiler
/// scans the raw tree shape (including search-only-tier paths) to decide
/// excludes/chunking in the first place, so it must see more than the
/// ingest walk's final indexable set.
fn build_walker(root: &Path) -> ignore::Walk {
    ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            name != crate::project::CONFIG_DIR
                && !crate::scan::is_office_lock_name(&name)
                && !(e.path().is_dir() && crate::scan::is_junk_dir_name(&name))
        })
        .build()
}

/// Walk `root` (skipping `excluded` folders, hidden files, `.ken`, and the
/// same junk build dirs `scan::scan` skips) and collect structural stats:
/// extension histogram, top-level dir names, repo markers, doc/code/media
/// byte totals, and the largest directories by aggregate bytes. Capped at
/// [`MAX_SCAN_FILES`] files (`truncated: true` beyond); no file content is
/// ever read, only names and metadata.
pub fn scan_stats(root: &Path, excluded: &[String]) -> Result<ScanStats> {
    scan_stats_capped(root, excluded, MAX_SCAN_FILES)
}

/// `scan_stats` with an injectable cap — split out so the truncation branch
/// is exercisable in a unit test without actually creating [`MAX_SCAN_FILES`]
/// files on disk.
fn scan_stats_capped(root: &Path, excluded: &[String], cap: usize) -> Result<ScanStats> {
    let mut stats = ScanStats::default();
    let mut ext_map: HashMap<String, ExtStat> = HashMap::new();
    let mut dir_bytes: HashMap<String, u64> = HashMap::new();
    let mut markers: Vec<String> = Vec::new();
    let mut top_level_dirs: Vec<String> = Vec::new();
    let mut file_count = 0usize;

    // The walk (like scan::scan's) never descends into hidden dirs, so
    // `.git` — the one marker that's itself a hidden directory — has to be
    // checked directly rather than encountered mid-walk.
    if root.join(".git").is_dir() {
        markers.push(".git".to_string());
    }

    for entry in build_walker(root).flatten() {
        let path = entry.path();
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        if rel.as_os_str().is_empty() {
            continue; // the root entry itself
        }
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if is_excluded(excluded, &rel_str) {
            continue;
        }

        if path.is_dir() {
            if rel.components().count() == 1 {
                top_level_dirs.push(rel_str);
            }
            continue;
        }
        if !path.is_file() {
            continue;
        }

        if file_count >= cap {
            stats.truncated = true;
            break;
        }
        file_count += 1;

        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if REPO_MARKERS.contains(&name) && !markers.iter().any(|m| m == name) {
                markers.push(name.to_string());
            } else if name.to_ascii_lowercase().ends_with(".sln")
                && !markers.iter().any(|m| m == "*.sln")
            {
                markers.push("*.sln".to_string());
            }
        }

        let Ok(meta) = path.metadata() else {
            continue;
        };
        let bytes = meta.len();
        stats.total_bytes += bytes;

        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let entry = ext_map.entry(ext.clone()).or_default();
        entry.count += 1;
        entry.bytes += bytes;

        if crate::chunker::PROSE_EXTS.contains(&ext.as_str()) {
            stats.doc_bytes += bytes;
        } else if crate::chunker::CODE_EXTS.contains(&ext.as_str()) {
            stats.code_bytes += bytes;
        }
        if MEDIA_EXTS.contains(&ext.as_str()) {
            stats.media_bytes += bytes;
        }

        if let Some(parent) = rel.parent() {
            let p = parent.to_string_lossy().replace('\\', "/");
            if !p.is_empty() {
                *dir_bytes.entry(p).or_insert(0) += bytes;
            }
        }
    }

    stats.total_files = file_count;
    stats.extensions = ext_map;
    stats.markers = markers;
    top_level_dirs.sort();
    stats.top_level_dirs = top_level_dirs;

    let mut largest: Vec<(String, u64)> = dir_bytes.into_iter().collect();
    largest.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    largest.truncate(MAX_LARGEST_DIRS);
    stats.largest_dirs = largest;

    Ok(stats)
}

/// `ProjectProfile::kind` — one of the four closed categories the profiler
/// picks between. `Mixed` is the safe default when the evidence is thin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectKind {
    Code,
    Docs,
    Mixed,
    Media,
}

impl Default for ProjectKind {
    fn default() -> Self {
        ProjectKind::Mixed
    }
}

impl ProjectKind {
    fn parse(s: &str) -> Option<ProjectKind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "code" => Some(ProjectKind::Code),
            "docs" => Some(ProjectKind::Docs),
            "mixed" => Some(ProjectKind::Mixed),
            "media" => Some(ProjectKind::Media),
            _ => None,
        }
    }
}

/// One extension-pattern → chunking-strategy entry, e.g. `*.rs` → code mode.
/// `ProjectProfile::chunking_for` looks these up by the target path's
/// extension.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PatternProfile {
    /// `"*.<ext>"` — the only pattern shape `chunking_for` currently
    /// matches. Stored as a string (not just the bare extension) so a
    /// hand-edited profile reads naturally and a future richer glob is a
    /// non-breaking extension of this same field.
    pub pattern: String,
    pub profile: IndexProfile,
}

/// A project's profile: how the profiler understands the tree's shape, and
/// what downstream consumers (chunking, excludes, the knowledge-model
/// prompt) should do differently because of it. Saved to
/// `.ken/index-profile.json` (design D2) — text the user owns and can edit;
/// `generated_hash` is how Ken tells its own last write apart from a hand
/// edit (see [`ProjectProfile::is_hand_edited`]).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectProfile {
    pub kind: ProjectKind,
    pub summary: String,
    pub languages: Vec<String>,
    /// Additive-only exclusions (project-relative, trailing `/`), unioned
    /// with `ProjectConfig.excluded` by `Project::effective_excluded` — this
    /// list never removes a user entry and is never removed from except by
    /// an explicit user edit.
    pub excludes: Vec<String>,
    pub chunking: Vec<PatternProfile>,
    pub focus_hints: Vec<String>,
    /// Hash of this profile's own content (everything except this field) as
    /// of Ken's last write. See [`ProjectProfile::is_hand_edited`].
    pub generated_hash: String,
    /// Forward-compat passthrough, same idiom as `ProjectConfig::extra`.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

pub const PROFILE_FILE: &str = "index-profile.json";

pub fn profile_path(root: &Path) -> PathBuf {
    root.join(crate::project::CONFIG_DIR).join(PROFILE_FILE)
}

/// Canonical JSON of `profile` with `generated_hash` blanked out — the input
/// to the hash that goes *into* `generated_hash`. Blanking it (rather than
/// hashing the struct as-loaded) avoids the hash depending on its own
/// previous value.
fn canonical_for_hash(profile: &ProjectProfile) -> String {
    let mut clone = profile.clone();
    clone.generated_hash = String::new();
    serde_json::to_string(&clone).unwrap_or_default()
}

impl ProjectProfile {
    /// Look up the chunking entry for `rel_path` by extension (`"*.<ext>"`
    /// patterns only, matched case-insensitively). `None` means this
    /// profile has no opinion for that extension — callers (engine ingest)
    /// fall back to `IndexProfile::default_for` per design D3.
    pub fn chunking_for(&self, rel_path: &str) -> Option<IndexProfile> {
        let ext = rel_path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        if ext.is_empty() {
            return None;
        }
        self.chunking
            .iter()
            .find(|p| {
                p.pattern
                    .strip_prefix("*.")
                    .is_some_and(|e| e.eq_ignore_ascii_case(&ext))
            })
            .map(|p| p.profile)
    }

    /// Load `.ken/index-profile.json` from `root`. Tolerant like
    /// `AppSettings::load`/`ProjectConfig` (design idiom): a missing or
    /// corrupt file loads as `ProjectProfile::default()` — an empty profile
    /// contributes nothing to chunking or excludes, which is exactly
    /// "no profile" behavior for every consumer. Never errors.
    pub fn load(root: &Path) -> ProjectProfile {
        match std::fs::read_to_string(profile_path(root)) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            Err(_) => ProjectProfile::default(),
        }
    }

    /// Write this profile atomically (temp file + rename, mirroring
    /// `AppSettings::save`) to `.ken/index-profile.json`, stamping
    /// `generated_hash` to match the content being written. Always call
    /// this only with content Ken itself generated/refined — a caller that
    /// found `is_hand_edited()` true on the previously-loaded profile must
    /// get explicit user confirmation before calling `save` again (that
    /// confirm flow is a src-tauri/UI concern, task 2.x/3.x; this method
    /// has no way to know it wasn't confirmed).
    pub fn save(&self, root: &Path) -> Result<()> {
        let mut to_write = self.clone();
        to_write.generated_hash = crate::knowledge_model::content_hash(&canonical_for_hash(&to_write));

        let dir = root.join(crate::project::CONFIG_DIR);
        std::fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
        let path = profile_path(root);
        let json = serde_json::to_string_pretty(&to_write).map_err(|e| Error::Other(e.to_string()))?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json + "\n").map_err(|e| Error::io(&tmp, e))?;
        std::fs::rename(&tmp, &path).map_err(|e| Error::io(&path, e))
    }

    /// Has this profile's content diverged from the hash Ken stamped the
    /// last time it wrote the file (design D2)? `false` for a profile that
    /// was never saved/stamped (`generated_hash` empty) — there's nothing
    /// to have diverged from yet.
    pub fn is_hand_edited(&self) -> bool {
        if self.generated_hash.is_empty() {
            return false;
        }
        crate::knowledge_model::content_hash(&canonical_for_hash(self)) != self.generated_hash
    }
}

fn marker_excludes(stats: &ScanStats) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for marker in &stats.markers {
        if let Some((_, dirs)) = MARKER_EXCLUDES.iter().find(|(m, _)| *m == marker.as_str()) {
            for d in *dirs {
                let entry = format!("{d}/");
                if !out.contains(&entry) {
                    out.push(entry);
                }
            }
        }
    }
    out
}

fn guess_languages(stats: &ScanStats) -> Vec<String> {
    let mut by_lang: HashMap<&str, u64> = HashMap::new();
    for (ext, stat) in &stats.extensions {
        if let Some((_, lang)) = LANGUAGE_EXTS.iter().find(|(e, _)| *e == ext.as_str()) {
            *by_lang.entry(lang).or_insert(0) += stat.bytes;
        }
    }
    let mut ranked: Vec<(&str, u64)> = by_lang.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    ranked.into_iter().take(6).map(|(lang, _)| lang.to_string()).collect()
}

/// kind rules (design D1/spec "Deterministic profile without any LLM"):
/// repo markers strongly imply `code` (unless docs dominate the bytes
/// anyway, e.g. a Rust project that's mostly a `docs/` folder → `mixed`);
/// otherwise media/doc byte ratios decide; a thin/ambiguous tree defaults to
/// `mixed`, never guesses `code` or `docs` without evidence.
fn classify_kind(stats: &ScanStats) -> ProjectKind {
    if stats.total_bytes == 0 {
        return ProjectKind::Mixed;
    }
    if !stats.markers.is_empty() {
        // A repo marker is strong code-project evidence even alongside a
        // sizeable docs/ folder — but not if docs dominate outright.
        return if stats.doc_ratio() > 0.6 {
            ProjectKind::Mixed
        } else {
            ProjectKind::Code
        };
    }
    if stats.media_ratio() > 0.6 {
        return ProjectKind::Media;
    }
    if stats.doc_bytes + stats.code_bytes == 0 {
        // No doc or code signal at all (all media/binary, below the media
        // threshold, or an extensionless tree) — nothing to classify by.
        return ProjectKind::Mixed;
    }
    let doc_ratio = stats.doc_ratio();
    if doc_ratio > 0.8 {
        ProjectKind::Docs
    } else if doc_ratio < 0.2 {
        ProjectKind::Code
    } else {
        ProjectKind::Mixed
    }
}

/// extension→chunk-mode table: reuses `chunker::PROSE_EXTS`/`CODE_EXTS` so
/// the profiler's default chunking table can never drift from the chunker's
/// own `IndexProfile::default_for` taxonomy — it's a data-driven,
/// overridable copy of exactly those rules, not a second source of truth.
fn default_chunking_table() -> Vec<PatternProfile> {
    let mut out = Vec::with_capacity(crate::chunker::PROSE_EXTS.len() + crate::chunker::CODE_EXTS.len());
    for ext in crate::chunker::PROSE_EXTS {
        out.push(PatternProfile {
            pattern: format!("*.{ext}"),
            profile: IndexProfile { mode: ChunkMode::Prose, target_tokens: 350, overlap_pct: 0.15 },
        });
    }
    for ext in crate::chunker::CODE_EXTS {
        out.push(PatternProfile {
            pattern: format!("*.{ext}"),
            profile: IndexProfile { mode: ChunkMode::Code, target_tokens: 500, overlap_pct: 0.0 },
        });
    }
    out
}

/// Build a usable [`ProjectProfile`] from `stats` alone — no model, no I/O
/// beyond what `scan_stats` already did. Always succeeds (design D1's
/// "every project gets a usable profile with zero LLM involvement").
/// `summary`/`focus_hints` are left empty; those are LLM-only fields filled
/// in (additively) by `apply_refinement`.
pub fn deterministic_profile(stats: &ScanStats) -> ProjectProfile {
    ProjectProfile {
        kind: classify_kind(stats),
        summary: String::new(),
        languages: guess_languages(stats),
        excludes: marker_excludes(stats),
        chunking: default_chunking_table(),
        focus_hints: Vec::new(),
        generated_hash: String::new(),
        extra: Map::new(),
    }
}

/// How many lines of tree sample `compose_profile_prompt` will include —
/// design D4's "at most 150 lines of depth-2 tree listing".
pub const MAX_TREE_SAMPLE_LINES: usize = 150;

/// Build the refinement prompt's tree sample from already-collected `stats`
/// (no second filesystem walk — `ScanStats::largest_dirs`/`top_level_dirs`
/// already capture "dirs first, largest by bytes" at the granularity the
/// prompt needs). Dirs with recorded aggregate bytes come first, largest
/// first; any top-level dir with no recorded bytes (e.g. empty) follows;
/// capped at [`MAX_TREE_SAMPLE_LINES`].
pub fn tree_sample(stats: &ScanStats) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for (dir, bytes) in &stats.largest_dirs {
        lines.push(format!("{dir}/  (~{bytes} bytes)"));
        if lines.len() >= MAX_TREE_SAMPLE_LINES {
            return lines;
        }
    }
    for name in &stats.top_level_dirs {
        if stats.largest_dirs.iter().any(|(d, _)| d == name) {
            continue;
        }
        lines.push(format!("{name}/"));
        if lines.len() >= MAX_TREE_SAMPLE_LINES {
            return lines;
        }
    }
    lines
}

/// The Background-priority refinement prompt (design D4): stats summary +
/// capped tree sample, no file contents. Asks for a single JSON object the
/// same tolerant-parsing idiom as `knowledge_model.rs` expects back.
pub fn compose_profile_prompt(stats: &ScanStats, tree_sample: &[String]) -> String {
    let mut p = String::new();
    p.push_str(
        "You are Ken, refining a project profile that tunes search chunking \
and knowledge extraction for this project. You will not modify anything — \
just describe what you see.\n\n",
    );
    p.push_str(&format!(
        "Files scanned: {}{}\n",
        stats.total_files,
        if stats.truncated { " (truncated at the scan cap)" } else { "" }
    ));
    p.push_str(&format!(
        "Repo markers: {}\n",
        if stats.markers.is_empty() { "none".to_string() } else { stats.markers.join(", ") }
    ));
    p.push_str(&format!("Doc-vs-code byte ratio: {:.0}% doc\n\n", stats.doc_ratio() * 100.0));
    p.push_str("Top-level tree (directories first, largest by bytes):\n");
    if tree_sample.is_empty() {
        p.push_str("- (empty)\n");
    }
    for line in tree_sample.iter().take(MAX_TREE_SAMPLE_LINES) {
        p.push_str(&format!("- {line}\n"));
    }
    p.push_str(
        "\nOutput ONLY a JSON object — no prose before or after, no code fences — \
shaped exactly like this:\n\
{\n  \"kind\": \"code|docs|mixed|media\",\n  \"summary\": \"one paragraph describing what this project is\",\n  \"excludes\": [\"generated-or-vendored-dir\"],\n  \"focus_hints\": [\"topic the knowledge extraction should prioritize\"]\n}\n\n\
Rules:\n\
- excludes are ADDITIVE ONLY — suggest a directory only if it is visible in \
the tree above and is not already an obvious build-output dir (target, \
node_modules, dist, build, vendor, bin, obj).\n\
- Every field is optional; omit any you have no opinion on.\n\
- Keep summary to one paragraph and focus_hints to a few short topics.\n",
    );
    p
}

/// A refinement answer, loosely parsed — structurally valid but not yet
/// checked against the real tree. [`apply_refinement`] does that validation
/// and the actual (additive-only) merge into a [`ProjectProfile`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProfileRefinement {
    pub kind: Option<ProjectKind>,
    pub excludes: Vec<String>,
    pub summary: Option<String>,
    pub focus_hints: Vec<String>,
}

/// Cap on how many focus hints a single refinement can contribute — bounds
/// the junk one bad generation could inject, same spirit as
/// `knowledge_model`'s per-file caps.
const MAX_FOCUS_HINTS: usize = 20;

/// Parse a refinement answer tolerantly (design D4/spec "LLM refinement is
/// additive and validated"): find the JSON object (fences/prose stripped),
/// ignore unknown fields, drop anything the wrong shape. Deliberately
/// infallible — unlike `knowledge_model::parse_extraction`, unparseable
/// input here must NOT be an error (spec: "model failure falls back... \
/// profile-state reaches ready, not error"), so this returns an empty
/// (no-op) [`ProfileRefinement`] instead of `Result`.
pub fn parse_profile_refinement(raw: &str) -> ProfileRefinement {
    let mut out = ProfileRefinement::default();
    let Some(start) = raw.find('{') else {
        return out;
    };
    let Some(end) = raw.rfind('}').filter(|e| *e > start) else {
        return out;
    };
    let Ok(value) = serde_json::from_str::<Value>(&raw[start..=end]) else {
        return out;
    };

    out.kind = value["kind"].as_str().and_then(ProjectKind::parse);

    out.excludes = value["excludes"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();

    out.summary = value["summary"]
        .as_str()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    out.focus_hints = value["focus_hints"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .take(MAX_FOCUS_HINTS)
                .collect()
        })
        .unwrap_or_default();

    out
}

/// Does `dir` (an absolute path) directly contain a repo marker or a
/// `*.sln` file? Used by `apply_refinement` to reject an exclude suggestion
/// that would wholesale-hide a nested project (design's "must not contain a
/// repo marker" validation).
fn dir_contains_repo_marker(dir: &Path) -> bool {
    if REPO_MARKERS.iter().any(|m| dir.join(m).is_file()) {
        return true;
    }
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| {
            e.path()
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("sln"))
        })
}

/// Validate and merge a parsed refinement into `profile`, in place (design
/// D1/D4, spec "LLM refinement is additive and validated"): `kind`/`summary`
/// may be overwritten (the LLM's whole job is to refine those), but
/// `excludes` are additive-only — each candidate must resolve to an
/// existing directory under `root` that does not itself contain a repo
/// marker (never exclude a nested project wholesale), and duplicates of an
/// already-present exclude are silently skipped rather than re-added.
/// `focus_hints` accumulate, deduplicated.
pub fn apply_refinement(profile: &mut ProjectProfile, refinement: &ProfileRefinement, root: &Path) {
    if let Some(kind) = refinement.kind {
        profile.kind = kind;
    }
    if let Some(summary) = &refinement.summary {
        profile.summary = summary.clone();
    }
    for hint in &refinement.focus_hints {
        if !profile.focus_hints.iter().any(|h| h == hint) {
            profile.focus_hints.push(hint.clone());
        }
    }
    for candidate in &refinement.excludes {
        let normalized = candidate.trim_matches('/');
        if normalized.is_empty() {
            continue;
        }
        if profile.excludes.iter().any(|e| e.trim_matches('/') == normalized) {
            continue; // already present — additive, not duplicated
        }
        let dir = root.join(normalized);
        if !dir.is_dir() {
            continue; // doesn't exist — dropped per D1
        }
        if dir_contains_repo_marker(&dir) {
            continue; // would hide a nested project — dropped per D1
        }
        profile.excludes.push(format!("{normalized}/"));
    }
}

/// Cap on the knowledge-model prompt addendum this profile contributes
/// (design D3: "capped at 500 chars ... inside EXTRACT_CHAR_BUDGET").
pub const PROFILE_PROMPT_ADDENDUM_CHAR_CAP: usize = 500;

/// Format `profile`'s summary/focus hints for the knowledge-model
/// extraction prompt: `"Project summary: {summary}\nFocus areas: {hints}"`,
/// truncated to [`PROFILE_PROMPT_ADDENDUM_CHAR_CAP`] characters. Empty when
/// the profile has neither a summary nor hints (pure-deterministic profile,
/// no refinement yet) — callers should skip appending it entirely in that
/// case rather than inserting a blank block.
pub fn profile_prompt_addendum(profile: &ProjectProfile) -> String {
    if profile.summary.trim().is_empty() && profile.focus_hints.is_empty() {
        return String::new();
    }
    let mut s = String::new();
    if !profile.summary.trim().is_empty() {
        s.push_str("Project summary: ");
        s.push_str(profile.summary.trim());
        s.push('\n');
    }
    if !profile.focus_hints.is_empty() {
        s.push_str("Focus areas: ");
        s.push_str(&profile.focus_hints.join(", "));
        s.push('\n');
    }
    s.chars().take(PROFILE_PROMPT_ADDENDUM_CHAR_CAP).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    /// A Rust workspace fixture: Cargo.toml + src/*.rs + a target/ dir with
    /// junk in it (never walked — matches the ingest walker's own
    /// `is_junk_dir_name` filter).
    fn rust_fixture() -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() {}\n".repeat(20)).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "pub fn x() {}\n".repeat(20)).unwrap();
        fs::create_dir_all(dir.path().join("target/debug")).unwrap();
        fs::write(dir.path().join("target/debug/x"), "binary junk").unwrap();
        dir
    }

    fn node_fixture() -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("package.json"), "{\"name\":\"x\"}").unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/index.ts"), "export const x = 1;\n".repeat(20)).unwrap();
        fs::create_dir_all(dir.path().join("node_modules/dep")).unwrap();
        fs::write(dir.path().join("node_modules/dep/index.js"), "junk").unwrap();
        dir
    }

    fn docs_fixture() -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("guides")).unwrap();
        fs::write(dir.path().join("guides/intro.md"), "# Intro\n".to_string() + &"prose text here. ".repeat(200)).unwrap();
        fs::write(dir.path().join("guides/setup.md"), "# Setup\n".to_string() + &"more prose. ".repeat(200)).unwrap();
        fs::write(dir.path().join("README.txt"), "readme text. ".repeat(50)).unwrap();
        dir
    }

    fn mixed_fixture() -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/app.py"), "x = 1\n".repeat(30)).unwrap();
        fs::create_dir_all(dir.path().join("docs")).unwrap();
        fs::write(dir.path().join("docs/notes.md"), "notes. ".repeat(30)).unwrap();
        dir
    }

    // ---- scan_stats ----

    #[test]
    fn scan_stats_over_rust_fixture() {
        let dir = rust_fixture();
        let stats = scan_stats(dir.path(), &[]).unwrap();

        // Cargo.toml + main.rs + lib.rs = 3 files; target/ is a junk dir,
        // never walked at all (same ignore semantics as scan::scan).
        assert_eq!(stats.total_files, 3, "{stats:?}");
        assert!(stats.markers.contains(&"Cargo.toml".to_string()));
        assert!(!stats.top_level_dirs.contains(&"target".to_string()));
        assert!(!stats.truncated);
    }

    #[test]
    fn scan_stats_excludes_are_honored() {
        let dir = rust_fixture();
        let stats = scan_stats(dir.path(), &["src".to_string()]).unwrap();
        assert_eq!(stats.total_files, 1, "only Cargo.toml should remain: {stats:?}");
    }

    #[test]
    fn scan_stats_under_cap_is_not_truncated() {
        let dir = tempdir().unwrap();
        for i in 0..10 {
            fs::write(dir.path().join(format!("f{i}.txt")), "x").unwrap();
        }
        let stats = scan_stats(dir.path(), &[]).unwrap();
        assert_eq!(stats.total_files, 10);
        assert!(!stats.truncated);
        assert_eq!(MAX_SCAN_FILES, 50_000);
    }

    #[test]
    fn scan_stats_over_cap_stops_early_and_marks_truncated() {
        // Exercises the real cap-and-stop branch without creating
        // MAX_SCAN_FILES files on disk: scan_stats_capped takes the cap as
        // a parameter, and scan_stats itself is just that with
        // MAX_SCAN_FILES hard-coded (see its body).
        let dir = tempdir().unwrap();
        for i in 0..10 {
            fs::write(dir.path().join(format!("f{i}.txt")), "x").unwrap();
        }
        let stats = scan_stats_capped(dir.path(), &[], 5).unwrap();
        assert_eq!(stats.total_files, 5);
        assert!(stats.truncated);
    }

    #[test]
    fn scan_stats_detects_dot_git_without_walking_into_it() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(dir.path().join("README.md"), "hello").unwrap();
        let stats = scan_stats(dir.path(), &[]).unwrap();
        assert!(stats.markers.contains(&".git".to_string()));
        // .git's contents are never walked (hidden dirs are skipped).
        assert_eq!(stats.total_files, 1);
    }

    // ---- deterministic_profile: kind ----

    #[test]
    fn rust_project_profiled_offline_is_code_with_rust_language_and_target_excluded() {
        let dir = rust_fixture();
        let stats = scan_stats(dir.path(), &[]).unwrap();
        let profile = deterministic_profile(&stats);

        assert_eq!(profile.kind, ProjectKind::Code);
        assert!(profile.languages.contains(&"Rust".to_string()));
        assert!(profile.excludes.contains(&"target/".to_string()));
        let rs = profile.chunking_for("src/main.rs").unwrap();
        assert_eq!(rs.mode, ChunkMode::Code);
    }

    #[test]
    fn node_project_excludes_node_modules_and_dist() {
        let dir = node_fixture();
        let stats = scan_stats(dir.path(), &[]).unwrap();
        let profile = deterministic_profile(&stats);

        assert_eq!(profile.kind, ProjectKind::Code);
        assert!(profile.excludes.contains(&"node_modules/".to_string()));
        assert!(profile.excludes.contains(&"dist/".to_string()));
    }

    #[test]
    fn docs_folder_detected() {
        let dir = docs_fixture();
        let stats = scan_stats(dir.path(), &[]).unwrap();
        assert!(stats.doc_ratio() > 0.8, "doc_ratio: {}", stats.doc_ratio());
        let profile = deterministic_profile(&stats);

        assert_eq!(profile.kind, ProjectKind::Docs);
        let md = profile.chunking_for("guides/intro.md").unwrap();
        assert_eq!(md.mode, ChunkMode::Prose);
    }

    #[test]
    fn mixed_tree_without_markers_is_mixed() {
        let dir = mixed_fixture();
        let stats = scan_stats(dir.path(), &[]).unwrap();
        let profile = deterministic_profile(&stats);
        assert_eq!(profile.kind, ProjectKind::Mixed);
    }

    #[test]
    fn empty_tree_defaults_to_mixed_with_no_excludes() {
        let dir = tempdir().unwrap();
        let stats = scan_stats(dir.path(), &[]).unwrap();
        let profile = deterministic_profile(&stats);
        assert_eq!(profile.kind, ProjectKind::Mixed);
        assert!(profile.excludes.is_empty());
    }

    // ---- ProjectProfile persistence: save/load, hand-edit, round-trip ----

    #[test]
    fn save_then_load_roundtrips_and_is_not_hand_edited() {
        let dir = tempdir().unwrap();
        let mut profile = ProjectProfile { kind: ProjectKind::Code, ..Default::default() };
        profile.languages.push("Rust".into());
        profile.save(dir.path()).unwrap();

        let loaded = ProjectProfile::load(dir.path());
        assert_eq!(loaded.kind, ProjectKind::Code);
        assert_eq!(loaded.languages, vec!["Rust".to_string()]);
        assert!(!loaded.generated_hash.is_empty());
        assert!(!loaded.is_hand_edited());
    }

    #[test]
    fn missing_file_loads_as_default() {
        let dir = tempdir().unwrap();
        let loaded = ProjectProfile::load(dir.path());
        assert_eq!(loaded, ProjectProfile::default());
        assert!(!loaded.is_hand_edited());
    }

    #[test]
    fn corrupt_file_loads_as_default() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".ken")).unwrap();
        fs::write(profile_path(dir.path()), "{ not json").unwrap();
        let loaded = ProjectProfile::load(dir.path());
        assert_eq!(loaded, ProjectProfile::default());
    }

    #[test]
    fn hand_edited_profile_is_detected_and_preserved() {
        let dir = tempdir().unwrap();
        let profile = ProjectProfile { summary: "Original".into(), ..Default::default() };
        profile.save(dir.path()).unwrap();

        // The user hand-edits the summary without touching generated_hash.
        let path = profile_path(dir.path());
        let mut v: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        v["summary"] = "Hand-edited by a human".into();
        fs::write(&path, serde_json::to_string_pretty(&v).unwrap()).unwrap();

        let loaded = ProjectProfile::load(dir.path());
        assert_eq!(loaded.summary, "Hand-edited by a human");
        assert!(loaded.is_hand_edited());

        // A caller that respects hand_edited never calls save() again here;
        // simulate that by re-reading and confirming the file is untouched.
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("Hand-edited by a human"));
    }

    #[test]
    fn unknown_keys_survive_roundtrip() {
        let dir = tempdir().unwrap();
        let profile = ProjectProfile::default();
        profile.save(dir.path()).unwrap();

        let path = profile_path(dir.path());
        let mut v: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        v["futureField"] = serde_json::json!({"nested": 42});
        fs::write(&path, serde_json::to_string_pretty(&v).unwrap()).unwrap();

        let loaded = ProjectProfile::load(dir.path());
        assert!(loaded.extra.contains_key("futureField"));
        loaded.save(dir.path()).unwrap();
        let reloaded = ProjectProfile::load(dir.path());
        assert!(reloaded.extra.contains_key("futureField"), "unknown field lost on re-save");
    }

    #[test]
    fn no_temp_file_left_after_save() {
        let dir = tempdir().unwrap();
        ProjectProfile::default().save(dir.path()).unwrap();
        assert!(!profile_path(dir.path()).with_extension("json.tmp").exists());
        assert!(profile_path(dir.path()).exists());
    }

    // ---- chunking_for ----

    #[test]
    fn chunking_for_skip_pattern_yields_skip_mode() {
        let mut profile = ProjectProfile::default();
        profile.chunking.push(PatternProfile {
            pattern: "*.log".into(),
            profile: IndexProfile { mode: ChunkMode::Skip, target_tokens: 1, overlap_pct: 0.0 },
        });
        assert_eq!(profile.chunking_for("run.log").unwrap().mode, ChunkMode::Skip);
        assert!(profile.chunking_for("other.rs").is_none());
    }

    // ---- refinement parsing ----

    #[test]
    fn parse_refinement_strips_fences_and_prose() {
        let raw = "Sure, here you go:\n```json\n{\"kind\": \"docs\", \"summary\": \"A docs site.\", \"focus_hints\": [\"onboarding\"]}\n```\nHope that helps!";
        let r = parse_profile_refinement(raw);
        assert_eq!(r.kind, Some(ProjectKind::Docs));
        assert_eq!(r.summary.as_deref(), Some("A docs site."));
        assert_eq!(r.focus_hints, vec!["onboarding".to_string()]);
    }

    #[test]
    fn parse_refinement_ignores_unknown_fields_and_bad_kind() {
        let raw = r#"{"kind": "spreadsheet", "madeUpField": 1, "excludes": ["vendor"]}"#;
        let r = parse_profile_refinement(raw);
        assert_eq!(r.kind, None, "invalid kind is dropped, not defaulted");
        assert_eq!(r.excludes, vec!["vendor".to_string()]);
    }

    #[test]
    fn parse_refinement_total_garbage_is_a_no_op() {
        let r = parse_profile_refinement("not json at all, sorry, can't help");
        assert_eq!(r, ProfileRefinement::default());
    }

    // ---- apply_refinement: additive validation ----

    #[test]
    fn junk_suggestion_dropped_rest_kept() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("vendor")).unwrap();
        // "imaginary/" is never created.
        let mut profile = ProjectProfile::default();
        let refinement = ProfileRefinement {
            excludes: vec!["vendor".to_string(), "imaginary".to_string()],
            ..Default::default()
        };
        apply_refinement(&mut profile, &refinement, dir.path());
        assert_eq!(profile.excludes, vec!["vendor/".to_string()]);
    }

    #[test]
    fn exclude_suggestion_containing_a_repo_marker_is_dropped() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("packages/sibling")).unwrap();
        fs::write(dir.path().join("packages/sibling/package.json"), "{}").unwrap();
        let mut profile = ProjectProfile::default();
        let refinement = ProfileRefinement {
            excludes: vec!["packages/sibling".to_string()],
            ..Default::default()
        };
        apply_refinement(&mut profile, &refinement, dir.path());
        assert!(profile.excludes.is_empty(), "must not exclude a nested project: {:?}", profile.excludes);
    }

    #[test]
    fn apply_refinement_never_removes_deterministic_excludes() {
        let dir = tempdir().unwrap();
        let mut profile = ProjectProfile { excludes: vec!["target/".to_string()], ..Default::default() };
        // A refinement that says nothing about excludes at all.
        apply_refinement(&mut profile, &ProfileRefinement::default(), dir.path());
        assert_eq!(profile.excludes, vec!["target/".to_string()]);
    }

    #[test]
    fn apply_refinement_deduplicates_against_existing_excludes() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("target")).unwrap();
        let mut profile = ProjectProfile { excludes: vec!["target/".to_string()], ..Default::default() };
        let refinement = ProfileRefinement { excludes: vec!["target".to_string()], ..Default::default() };
        apply_refinement(&mut profile, &refinement, dir.path());
        assert_eq!(profile.excludes, vec!["target/".to_string()], "must not duplicate");
    }

    // ---- prompt addendum ----

    #[test]
    fn profile_prompt_addendum_empty_when_no_summary_or_hints() {
        assert_eq!(profile_prompt_addendum(&ProjectProfile::default()), "");
    }

    #[test]
    fn profile_prompt_addendum_contains_summary_and_hints_within_cap() {
        let profile = ProjectProfile {
            summary: "A tool for managing widgets.".into(),
            focus_hints: vec!["widgets".into(), "billing".into()],
            ..Default::default()
        };
        let addendum = profile_prompt_addendum(&profile);
        assert!(addendum.contains("Project summary: A tool for managing widgets."));
        assert!(addendum.contains("Focus areas: widgets, billing"));
        assert!(addendum.chars().count() <= PROFILE_PROMPT_ADDENDUM_CHAR_CAP);
    }

    #[test]
    fn profile_prompt_addendum_is_capped_at_500_chars() {
        let profile = ProjectProfile {
            summary: "x".repeat(2000),
            ..Default::default()
        };
        let addendum = profile_prompt_addendum(&profile);
        assert_eq!(addendum.chars().count(), PROFILE_PROMPT_ADDENDUM_CHAR_CAP);
    }

    // ---- compose_profile_prompt / tree_sample ----

    #[test]
    fn tree_sample_is_capped_and_dirs_first() {
        let mut stats = ScanStats::default();
        for i in 0..(MAX_TREE_SAMPLE_LINES + 20) {
            stats.largest_dirs.push((format!("dir{i}"), (MAX_TREE_SAMPLE_LINES + 20 - i) as u64));
        }
        let sample = tree_sample(&stats);
        assert_eq!(sample.len(), MAX_TREE_SAMPLE_LINES);
        assert!(sample[0].starts_with("dir0/"));
    }

    #[test]
    fn compose_profile_prompt_includes_stats_and_asks_for_json() {
        let dir = rust_fixture();
        let stats = scan_stats(dir.path(), &[]).unwrap();
        let sample = tree_sample(&stats);
        let prompt = compose_profile_prompt(&stats, &sample);
        assert!(prompt.contains("Cargo.toml"));
        assert!(prompt.contains("\"kind\""));
        assert!(prompt.contains("\"excludes\""));
    }
}

//! Scanner: reconcile the index with the folder. Used for the initial scan,
//! watcher batches, exclusion changes, and full reindex — one code path.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::Serialize;

use crate::cloud;
use crate::db::Db;
use crate::extract::{self, FileKind};
use crate::project::Project;
use crate::Result;

#[derive(Debug, Default, Clone, Serialize)]
pub struct ScanStats {
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    pub failed: usize,
    pub unchanged: usize,
    /// Paths that were added, updated, or removed this scan — consumed by
    /// the ingest engine to decide which recipes to queue.
    #[serde(skip)]
    pub changed_paths: Vec<String>,
    /// Local videos indexed this scan that still have no transcript — the app
    /// layer may enqueue on-device transcription for these. Empty unless a
    /// video was (re)indexed and no adjacent/generated transcript was found.
    #[serde(skip)]
    pub videos_needing_transcript: Vec<String>,
}

/// Images larger than this are not queued for OCR. Vision rasterization +
/// recognition on a very large image is expensive and rarely worth it; the file
/// stays name-searchable and its bytes are never read for OCR. Independent of
/// [`extract::MAX_EXTRACT_BYTES`] because OCR is a heavier, opt-in pass.
pub const MAX_OCR_IMAGE_BYTES: i64 = 25 * 1024 * 1024;

/// A scanned/image PDF whose extractable text layer holds fewer than this many
/// non-whitespace characters is treated as "low text" and queued for OCR. A
/// genuine text PDF clears this easily (hundreds+ of characters per page), so
/// text PDFs are never needlessly OCR'd; a scanned one yields ~nothing and gets
/// picked up. Deliberately a small absolute floor — cheap to compute and safely
/// below any real document's body text.
pub const PDF_OCR_LOW_TEXT_CHARS: usize = 64;

/// Does a PDF's extracted text look too sparse to be a real text layer (i.e. a
/// scanned/image PDF worth OCR'ing)? Counts non-whitespace characters only, so
/// page breaks and stray whitespace don't inflate the estimate.
pub fn pdf_text_is_sparse(text: &str) -> bool {
    text.chars().filter(|c| !c.is_whitespace()).count() < PDF_OCR_LOW_TEXT_CHARS
}

/// SVG (and any other vector image) can't be rasterized by ImageIO, so the OCR
/// bridge errors on it — never enqueue it. The only vector kind among the image
/// extensions is `.svg`.
pub fn is_vector_image(rel: &str) -> bool {
    rel.rsplit('.').next().is_some_and(|e| e.eq_ignore_ascii_case("svg"))
}

/// Machine-generated folders no knowledge project wants indexed. (Hidden
/// folders are already skipped.)
pub const JUNK_DIRS: &[&str] = &["node_modules", "target", "dist", "build", "__pycache__", "venv"];

pub fn is_junk_dir_name(name: &str) -> bool {
    JUNK_DIRS.contains(&name)
}

/// A folder whose `.git` is a file, not a directory, is a linked worktree or
/// a submodule: another checkout of some repo, indexed (if at all) as its
/// own project. Walking it would index the same files twice; nine worktrees
/// held more files than the four real repos they came from. A plain nested
/// clone (a `.git` directory) is still walked, so a folder of repos indexed
/// as one project keeps its contents.
pub fn is_linked_checkout(dir: &Path) -> bool {
    dir.join(".git").is_file()
}

/// Files above this are not byte-hashed: reading a large video in full on
/// every touch would cost more than the parse it saves. They fall back to
/// size and modified time.
const MAX_BYTE_HASH_BYTES: i64 = 64 * 1024 * 1024;

/// A hash of the file's bytes, or None when it is too big or unreadable.
fn byte_hash(abs: &Path, size: i64) -> Option<String> {
    if size > MAX_BYTE_HASH_BYTES {
        return None;
    }
    let bytes = std::fs::read(abs).ok()?;
    Some(format!("{:016x}", twox_hash::XxHash64::oneshot(0x4B454E_4259_5445, &bytes)))
}

/// A file whose size is unchanged and whose bytes hash as last time only had
/// its modified time moved (a sync client, a checkout, a copy). Record the
/// new time and skip the parse and the search-index write. Never called for
/// a cloud placeholder, whose bytes are not local.
fn only_mtime_moved(db: &mut Db, rel: &str, abs: &Path, size: i64, mtime: i64) -> Result<bool> {
    let Some(row) = db.get_file(rel)? else {
        return Ok(false);
    };
    if row.size != size || !(row.status == STATUS_INDEXED || row.status == STATUS_METADATA_ONLY) {
        return Ok(false);
    }
    let (Some(stored), Some(now)) = (db.file_byte_hash(rel)?, byte_hash(abs, size)) else {
        return Ok(false);
    };
    if stored != now {
        return Ok(false);
    }
    db.set_file_byte_hash(rel, &now, mtime)?;
    Ok(true)
}

/// Whether `rel` sits under a linked checkout below the project root.
fn inside_linked_checkout(root: &Path, rel: &str) -> bool {
    let mut dir = root.to_path_buf();
    let mut parts: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
    parts.pop(); // the entry itself: a file, or the folder the event is about
    parts.into_iter().any(|seg| {
        dir.push(seg);
        is_linked_checkout(&dir)
    })
}

/// Microsoft Office writes transient lock files named `~$<document>` beside
/// any open document. They carry no content and churn constantly — ignore
/// them everywhere (initial scan, refresh, watcher) so they never enter the
/// index or trigger rescans.
pub fn is_office_lock_name(name: &str) -> bool {
    name.starts_with("~$")
}

/// Ken-owned directories directly under `.ken/` that stay indexable and
/// watchable despite the rest of `.ken/` being hidden — ken-memory's
/// project-scope memories (`.ken/memory/`) and ken-tasks' per-repo task home
/// (`.ken/tasks/`). `.ken/project.json`, `.ken/index-profile.json`, and any
/// other current or future `.ken/` metadata deliberately stay off this list.
/// A feature that wants the same treatment adds its subdir name here, not a
/// one-off literal elsewhere.
const KEN_ALLOWLISTED_SUBDIRS: &[&str] = &["memory", "tasks"];

/// Is `rel` (project-root-relative, forward-slash separated, no leading
/// slash) a Ken-owned indexable subpath living under the otherwise fully
/// hidden `.ken/` directory? True only for `.ken/memory/**` and
/// `.ken/tasks/**`; `.ken` itself is not "under" `.ken`, so it returns
/// `false` too — scan.rs's walker and watch.rs's `relevant_path` each still
/// need to let the bare `.ken` directory be entered/observed one level deep
/// to reach these allowlisted subpaths, which is their job, not this
/// predicate's. Shared by both so a file one indexes is always a file the
/// other watches, and vice versa.
pub fn is_ken_allowlisted_path(rel: &str) -> bool {
    let Some(sub) = rel
        .strip_prefix(crate::project::CONFIG_DIR)
        .and_then(|s| s.strip_prefix('/'))
    else {
        return false;
    };
    KEN_ALLOWLISTED_SUBDIRS
        .iter()
        .any(|dir| sub == *dir || sub.starts_with(&format!("{dir}/")))
}

/// Walk one allowlisted Ken-owned subdirectory (`.ken/memory` or
/// `.ken/tasks`) like an ordinary project folder: office-lock and junk-dir
/// filtering still applies, and a nested dot-directory (e.g. a hypothetical
/// `.ken/tasks/.trash/`) still stays hidden — the allowlist covers
/// `memory/**` and `tasks/**`, not "every path anywhere under `.ken`". A
/// missing directory (most projects have no memories/tasks yet) yields no
/// entries rather than an error.
fn ken_owned_subwalk(dir: &std::path::Path) -> impl Iterator<Item = PathBuf> {
    ignore::WalkBuilder::new(dir)
        .hidden(true)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !is_office_lock_name(&name) && !(e.path().is_dir() && is_junk_dir_name(&name))
        })
        .build()
        .flatten()
        .map(|e| e.into_path())
}

/// Timestamped backup files (`report.xlsx.bak`, `report.xlsx.bak-2026-07-16T…Z`)
/// written by external tools beside live documents. Pure noise in a knowledge
/// index — and in a cloud folder they pile up as cloud-only rows that inflate
/// the offline count — so they are ignored everywhere, like Office lock files.
pub fn is_backup_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".bak") || lower.contains(".bak-")
}

/// File status values stored in the index.
pub const STATUS_INDEXED: &str = "indexed";
pub const STATUS_METADATA_ONLY: &str = "metadata_only";
pub const STATUS_FAILED: &str = "failed";
/// Present in the cloud (OneDrive/iCloud/Dropbox) but not downloaded yet, so
/// its bytes were deliberately never read. Name-searchable; content arrives
/// when the user opens the file and the next scan picks it up.
pub const STATUS_CLOUD_ONLY: &str = "cloud_only";

/// Should a rescan re-index this file even though size and mtime are
/// unchanged? Downloading a cloud placeholder alters neither, so without this
/// a file we skipped (or that timed out mid-download) would stay contentless
/// forever. Deliberately narrow: a file still in the cloud has nothing new to
/// read, and a genuinely corrupt file must not be retried on every scan.
fn needs_retry(status: &str, error: Option<&str>, dataless_now: bool) -> bool {
    match status {
        // We skipped its bytes; they've since arrived (the user opened it, or
        // the provider synced it down). Extract the content now.
        STATUS_CLOUD_ONLY => !dataless_now,
        // A timed-out read of a file that is still in the cloud: re-index it
        // as cloud_only. Costs nothing (we no longer read it) and converges —
        // next scan sees cloud_only + dataless and leaves it alone. A file
        // that failed for any other reason is left alone, so a corrupt one
        // isn't re-extracted on every scan.
        STATUS_FAILED => dataless_now && error.is_some_and(is_transient_error),
        _ => false,
    }
}

/// Extraction failures that are worth another attempt: the cloud provider
/// stalled or timed out (`ETIMEDOUT` = "os error 60") rather than the file
/// being malformed.
fn is_transient_error(error: &str) -> bool {
    let e = error.to_ascii_lowercase();
    e.contains("timed out") || e.contains("i/o error") || e.contains("io error")
}

/// Walk the project and bring the index to match the folder exactly.
pub fn scan(project: &Project, db: &mut Db) -> Result<ScanStats> {
    let mut stats = ScanStats::default();
    // A repo whose kind nobody has set is not read at all: set-up says what
    // each repo is before any indexing happens. What is indexed stays.
    if crate::registry::awaits_kind(&project.root) {
        return Ok(stats);
    }

    // kenignore (D2): built-ins first, user `.kenignore` appended last so a
    // `!` line can override them. Loaded once per scan, not per file —
    // `classify` is pure and re-parsing per path would be wasted work. The
    // repo's kind sits between them: it sets the whole repo's state.
    let built_in_rules = crate::kenignore::built_in_rule_sets();
    let kind_rules = crate::kenignore::kind_rules_for(&project.root);
    let user_rules = project.kenignore_rules();
    let rule_sets: [&[crate::kenignore::Rule]; 3] = [&built_in_rules, &kind_rules, &user_rules];

    // What's on disk (rel_path -> size, mtime, cloud placeholder?, tier)
    let mut on_disk: HashMap<String, (i64, i64, bool, crate::kenignore::Tier)> = HashMap::new();
    // A git repo's own `.gitignore` is the best statement anyone has of
    // "this is generated, not authored" — build output, caches, server
    // world data. Honouring it in a repo is why a Gradle project indexes
    // its `src/` and not its 1.7GB `build/`, without the user writing a
    // `.kenignore` that restates what git already knows.
    //
    // Gated on the project actually BEING a repo, because the original
    // reasoning still holds everywhere else: a notes folder is not a code
    // repo, and a stray `.gitignore` inherited from a parent directory
    // must not silently hide someone's documents. `parents(false)` keeps
    // this to the project's own rules for the same reason — a workspace
    // parent's `.gitignore` has no authority over a member's contents.
    //
    // LIMITATION, verified by test rather than assumed: `.kenignore` is
    // applied to paths this walk YIELDS, so in a repo a `!` line cannot
    // pull back something `.gitignore` already excluded — the walker
    // never hands it over to be reclassified. To index a gitignored path
    // deliberately, un-ignore it in `.gitignore` (a `!` line there), or
    // move it out from under the pattern. `.kenignore` remains able to
    // exclude further, and to demote to search-only, which is what it is
    // overwhelmingly used for.
    let is_repo = project.root.join(".git").exists();
    let walker = ignore::WalkBuilder::new(&project.root)
        .hidden(true) // skip dotfiles: .git, .ken, .DS_Store…
        .git_ignore(is_repo)
        .git_exclude(is_repo)
        .parents(false)
        .require_git(true)
        .git_global(false)
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            // The entry's own file type, from the directory listing: a status
            // call per entry is what made a walk of tens of thousands of
            // files cost seconds on Windows.
            let is_dir = e.file_type().is_some_and(|t| t.is_dir());
            name != crate::project::CONFIG_DIR
                && !is_office_lock_name(&name)
                && !is_backup_name(&name)
                && !(is_dir && (is_junk_dir_name(&name) || (e.depth() > 0 && is_linked_checkout(e.path()))))
        })
        .build()
        .flatten()
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        // Metadata from the listing where the platform gives it (Windows
        // does), so a file costs no extra status call.
        .map(|e| {
            let meta = e.metadata().ok();
            (e.into_path(), meta)
        });
    // `.ken/` is hidden wholesale above (D2 hard-ignore); walk its
    // allowlisted subpaths (`.ken/memory/`, `.ken/tasks/`) separately so
    // ken-memory and ken-tasks documents reach the index like any other file
    // — see `is_ken_allowlisted_path` for exactly what qualifies.
    let allowlisted = KEN_ALLOWLISTED_SUBDIRS
        .iter()
        .flat_map(|sub| ken_owned_subwalk(&project.root.join(crate::project::CONFIG_DIR).join(sub)))
        .filter(|p| p.is_file())
        .map(|p| (p, None));
    for (path, listed) in walker.chain(allowlisted) {
        let Ok(rel) = path.strip_prefix(&project.root) else {
            continue;
        };
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if project.is_excluded(&rel_str) {
            continue;
        }
        // kenignore D3: "Ignore-tier paths simply never produce rows —
        // identical to today's exclusion path." Skip exactly like `excluded`
        // above so an ignored file leaves no file/FTS row either.
        let tier = crate::kenignore::classify(&rel_str, false, &rule_sets);
        if tier == crate::kenignore::Tier::Ignore {
            continue;
        }
        if let Some(meta) = listed.or_else(|| path.metadata().ok()) {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            on_disk.insert(rel_str, (meta.len() as i64, mtime, cloud::is_dataless(&meta), tier));
        }
    }

    // What's in the index
    let indexed: HashMap<String, (i64, i64, String, Option<String>)> = db
        .list_files()?
        .into_iter()
        .map(|f| (f.rel_path, (f.size, f.mtime, f.status, f.error)))
        .collect();

    // Writes go in batches of BATCH_FILES, one commit each, not several
    // commits per file; an error rolls the open batch back.
    db.begin_batch()?;
    let result = write_changes(project, db, &indexed, &on_disk, &mut stats);
    match result {
        Ok(()) => db.commit_batch()?,
        Err(e) => {
            db.rollback_batch();
            return Err(e);
        }
    }

    // Re-tier. An unchanged file skipped `index_one` above, so a change of
    // the repo's kind or a new `~` line reaches it only here; files that are
    // now search-only also leave the extraction queue.
    let tiers: Vec<(String, crate::kenignore::Tier)> =
        on_disk.iter().map(|(rel, (_, _, _, tier))| (rel.clone(), *tier)).collect();
    db.set_file_tiers(&tiers)?;
    // Each file's authority in search, from the index's frontmatter and the
    // team file, for every file: a team file's new override reaches them all.
    crate::authority::refresh(db, &project.root, None)?;

    if let Some(result) = control_query(db)? {
        db.set_index_control(&result)?;
    }
    // The team's words for search, rebuilt when a page may have changed.
    if !stats.changed_paths.is_empty() || db.stored_vocabulary()?.is_none() {
        crate::vocab::Vocabulary::rebuild(db)?;
    }

    // The link report is read live, on the Team screen (`links::findings`).

    Ok(stats)
}

/// Files written per transaction during a scan.
const BATCH_FILES: usize = 500;

type OnDisk = HashMap<String, (i64, i64, bool, crate::kenignore::Tier)>;
type Indexed = HashMap<String, (i64, i64, String, Option<String>)>;

/// A scan's removals, adds and updates, committed every [`BATCH_FILES`]
/// files inside the batch the caller opened.
fn write_changes(project: &Project, db: &mut Db, indexed: &Indexed, on_disk: &OnDisk, stats: &mut ScanStats) -> Result<()> {
    let mut since = 0usize;
    let mut tick = |db: &mut Db| -> Result<()> {
        since += 1;
        if since >= BATCH_FILES {
            db.commit_batch()?;
            db.begin_batch()?;
            since = 0;
        }
        Ok(())
    };

    // Removals: indexed but gone from disk (or newly excluded)
    for rel in indexed.keys() {
        if !on_disk.contains_key(rel) {
            db.remove_file(rel)?;
            stats.removed += 1;
            stats.changed_paths.push(rel.clone());
            tick(db)?;
        }
    }

    // Adds/updates
    for (rel, (size, mtime, dataless, tier)) in on_disk {
        match indexed.get(rel) {
            Some((s, m, status, error)) if s == size
                && m == mtime
                && !needs_retry(status, error.as_deref(), *dataless) =>
            {
                stats.unchanged += 1;
                continue;
            }
            Some((s, _, status, error))
                if s == size
                    && !*dataless
                    && !needs_retry(status, error.as_deref(), *dataless)
                    && only_mtime_moved(db, rel, &project.root.join(rel), *size, *mtime)? =>
            {
                stats.unchanged += 1;
                continue;
            }
            Some(_) => stats.updated += 1,
            None => stats.added += 1,
        }
        let status = index_one(project, db, rel, *size, *mtime, *dataless, *tier)?;
        if status == STATUS_FAILED {
            stats.failed += 1;
        }
        // A local video with no transcript is metadata-only until one is made.
        if status == STATUS_METADATA_ONLY
            && FileKind::from_path(&project.root.join(rel)) == FileKind::Video
        {
            stats.videos_needing_transcript.push(rel.clone());
        }
        stats.changed_paths.push(rel.clone());
        tick(db)?;
    }
    Ok(())
}

/// Pages tried, in order, as the known page a control query must rank first.
const CONTROL_PAGES: &[&str] = &["START-HERE.md", "Current/Index.md", "README.md", "readme.md"];

/// Search the first control page present by its title and check it comes
/// back first (docs-system: "Every rebuild runs one control query that must
/// rank one known page first"). None when the project has none of them.
pub fn control_query(db: &Db) -> Result<Option<crate::db::ControlResult>> {
    let Some(page) = CONTROL_PAGES
        .iter()
        .find(|p| matches!(db.get_file(p), Ok(Some(f)) if f.status == STATUS_INDEXED))
    else {
        return Ok(None);
    };
    let stem = page.rsplit('/').next().unwrap_or(page).trim_end_matches(".md");
    let query = db
        .page_meta(page)?
        .and_then(|m| m.title)
        .unwrap_or_else(|| stem.replace(['-', '_'], " "));
    let top = db.search(&query, 5)?.into_iter().next().map(|h| h.rel_path);
    let ok = top.as_deref() == Some(*page);
    Ok(Some(crate::db::ControlResult {
        page: page.to_string(),
        query,
        top: (!ok).then_some(top).flatten(),
        ok,
        at: std::time::SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0),
    }))
}

/// Index a single file (already known to exist and be included). Returns the
/// status stored.
fn index_one(
    project: &Project,
    db: &mut Db,
    rel: &str,
    size: i64,
    mtime: i64,
    dataless: bool,
    tier: crate::kenignore::Tier,
) -> Result<&'static str> {
    let abs = project.root.join(rel);
    let kind = FileKind::from_path(&abs);
    // Its bytes are still in the cloud. Reading them here would force a
    // blocking download (and usually time out), so record the file by name
    // and leave the content to `cloud::hydrate` when the user opens it.
    if dataless {
        db.upsert_file(rel, kind.as_str(), size, mtime, STATUS_CLOUD_ONLY, None, "")?;
        return Ok(STATUS_CLOUD_ONLY);
    }
    let (status, error, text) = match extract::extract(&abs) {
        // A video whose transcript hasn't been resolved yet is searchable by
        // name only — metadata-only, not an empty-content "indexed" row.
        Ok(out) if kind == FileKind::Video && out.text.trim().is_empty() => {
            (STATUS_METADATA_ONLY, None, out.text)
        }
        // A drawio file whose labels couldn't be decoded is searchable by name
        // only — metadata-only, not an empty-content "indexed" row.
        Ok(out) if kind == FileKind::Drawio && out.text.trim().is_empty() => {
            (STATUS_METADATA_ONLY, None, out.text)
        }
        Ok(out) if kind.has_content() => (STATUS_INDEXED, None, out.text),
        Ok(out) => (STATUS_METADATA_ONLY, None, out.text),
        Err(e) => (STATUS_FAILED, Some(e.to_string()), String::new()),
    };
    db.upsert_file(rel, kind.as_str(), size, mtime, status, error.as_deref(), &text)?;
    if status != STATUS_FAILED {
        if let Some(hash) = byte_hash(&abs, size) {
            db.set_file_byte_hash(rel, &hash, mtime)?;
        }
    }
    // Chunks, for keyword search over sections and `repo:path:line`
    // citations. They need no model, so every indexed file has them; the
    // semantic rebuild only adds vectors. A file with no readable text has
    // none.
    if status == STATUS_INDEXED {
        let chunks = crate::chunker::chunk_file(rel, &text, &crate::chunker::IndexProfile::default_for(rel));
        db.upsert_chunks(rel, &chunks, tier)?;
    } else {
        db.delete_chunks(rel)?;
    }
    // What a page says about itself (title, aliases, verified, retired,
    // generated), read from its frontmatter for search to rank and show.
    if kind == FileKind::Md {
        let meta = (status == STATUS_INDEXED).then(|| crate::pagemeta::parse(&text)).flatten();
        db.set_page_meta(rel, meta.as_ref())?;
        let links = if status == STATUS_INDEXED { crate::links::extract(rel, &text) } else { Vec::new() };
        db.set_page_links(rel, &links)?;
    }
    // The code map: definitions, references and imports, for "find usages"
    // and "go to definition". Every tier: a code repo is search-only and
    // this is exactly what it is searched for.
    if crate::codemap::Lang::of(rel).is_some() {
        let map = if status == STATUS_INDEXED { crate::codemap::map_file(rel, &text).unwrap_or_default() } else { Default::default() };
        db.set_code_map(rel, &map)?;
    }
    // Incremental Map: an indexed file whose content changed is queued for
    // local-LLM extraction. The hash is over the extracted text, so mtime/size
    // churn without a content change never re-runs extraction. kenignore D3/
    // task 1.5: search-only files are searchable but never enter the
    // knowledge model, so extraction is gated to full-tier only. FTS (the
    // upsert_file call above) and OCR (the enqueue below) both stay
    // tier-blind (both tiers are searchable) — only this extraction enqueue
    // is gated.
    if status == STATUS_INDEXED && tier == crate::kenignore::Tier::Full {
        let hash = crate::knowledge_model::content_hash(&text);
        db.enqueue_extraction_if_changed(rel, &hash)?;
    }
    // OCR enqueue (Phase 2): images and low-text (scanned) PDFs get queued for
    // a background OCR pass so their pixels become searchable text. Keep this
    // OUT of the knowledge-model extraction queue/coverage — it's a separate
    // pipeline. Never run OCR inline here: it reads bytes and hits Vision, which
    // would stall the scan. Keyed by a cheap per-version hash so an unchanged
    // rescan (which never re-runs `index_one`) never re-OCRs, and a real change
    // (new size/mtime → new hash) re-queues. Only reached when the file was read
    // (not a cloud placeholder, which returned early above). Nothing is queued
    // where the OS has no OCR engine: marking such files done unread would keep
    // them unread once one exists.
    if status != STATUS_FAILED && crate::ocr::available() {
        let ocr_hash = crate::knowledge_model::content_hash(&format!("{size}:{mtime}"));
        match kind {
            FileKind::Image if !is_vector_image(rel) && size <= MAX_OCR_IMAGE_BYTES => {
                db.enqueue_ocr_if_changed(rel, &ocr_hash)?;
            }
            FileKind::Pdf if pdf_text_is_sparse(&text) => {
                db.enqueue_ocr_if_changed(rel, &ocr_hash)?;
            }
            _ => {}
        }
    }
    Ok(status)
}

/// Re-index one path in response to a watcher event: index if it exists and
/// is included, remove otherwise. Returns true if the index changed.
pub fn refresh_path(project: &Project, db: &mut Db, rel: &str) -> Result<bool> {
    if crate::registry::awaits_kind(&project.root) {
        return Ok(false);
    }
    let abs = project.root.join(rel);
    let excluded = project.is_excluded(rel)
        || is_hidden_rel(rel)
        || rel.rsplit('/').next().is_some_and(|n| is_office_lock_name(n) || is_backup_name(n))
        || inside_linked_checkout(&project.root, rel);
    // kenignore D3: an Ignore-tier path is treated exactly like `excluded` —
    // no row at all. This is a basic per-event check, not D4's fuller
    // old-tier -> new-tier transition diffing (deleting stale KM
    // contributions on Full->SearchOnly, etc.) — that belongs to task 1.6 and
    // the src-tauri watcher work, out of scope here.
    let built_in_rules = crate::kenignore::built_in_rule_sets();
    let kind_rules = crate::kenignore::kind_rules_for(&project.root);
    let user_rules = project.kenignore_rules();
    let rule_sets: [&[crate::kenignore::Rule]; 3] = [&built_in_rules, &kind_rules, &user_rules];
    let tier = crate::kenignore::classify(rel, false, &rule_sets);
    let excluded = excluded || tier == crate::kenignore::Tier::Ignore;
    if !excluded && abs.is_file() {
        let meta = abs.metadata().map_err(|e| crate::Error::io(&abs, e))?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let dataless = cloud::is_dataless(&meta);
        if !dataless && only_mtime_moved(db, rel, &abs, meta.len() as i64, mtime)? {
            db.set_file_tiers(&[(rel.to_string(), tier)])?;
            return Ok(false);
        }
        index_one(project, db, rel, meta.len() as i64, mtime, dataless, tier)?;
        db.set_file_tiers(&[(rel.to_string(), tier)])?;
        crate::authority::refresh(db, &project.root, Some(&[rel.to_string()]))?;
        // A page's aliases, or the Vocabulary page itself, may have changed.
        if rel.ends_with(".md") {
            crate::vocab::Vocabulary::rebuild(db)?;
        }
        Ok(true)
    } else if db.get_file(rel)?.is_some() {
        db.remove_file(rel)?;
        Ok(true)
    } else if abs.is_dir() || (!abs.exists() && !rel.contains('.')) {
        // A folder appeared/disappeared/renamed: reconcile the whole subtree.
        db.remove_folder(rel)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// A repo's `.gitignore` files, read as a walk reads them, for one path at
/// a time: the nearest file that says anything about a path decides.
struct GitIgnores {
    root: PathBuf,
    on: bool,
    dirs: HashMap<PathBuf, Option<ignore::gitignore::Gitignore>>,
    exclude: Option<ignore::gitignore::Gitignore>,
}

impl GitIgnores {
    fn of(project: &Project) -> GitIgnores {
        let root = project.root.clone();
        let on = root.join(".git").exists();
        let exclude = on.then(|| root.join(".git/info/exclude")).filter(|p| p.is_file()).map(|p| {
            let mut b = ignore::gitignore::GitignoreBuilder::new(&root);
            b.add(p);
            b.build().unwrap_or_else(|_| ignore::gitignore::Gitignore::empty())
        });
        GitIgnores { root, on, dirs: HashMap::new(), exclude }
    }

    /// Whether git would leave `rel` out, as the scan's walk does in a repo.
    fn ignored(&mut self, rel: &str) -> bool {
        use ignore::Match;
        if !self.on {
            return false;
        }
        let abs = self.root.join(rel);
        let mut dir = abs.parent().map(Path::to_path_buf);
        while let Some(d) = dir {
            if !d.starts_with(&self.root) {
                break;
            }
            let gi = self.dirs.entry(d.clone()).or_insert_with(|| {
                let f = d.join(".gitignore");
                f.is_file().then(|| ignore::gitignore::Gitignore::new(&f).0)
            });
            if let Some(gi) = gi {
                match gi.matched_path_or_any_parents(&abs, false) {
                    Match::Ignore(_) => return true,
                    Match::Whitelist(_) => return false,
                    Match::None => {}
                }
            }
            if d == self.root {
                break;
            }
            dir = d.parent().map(Path::to_path_buf);
        }
        self.exclude.as_ref().is_some_and(|x| x.matched_path_or_any_parents(&abs, false).is_ignore())
    }
}

/// What refreshing one path did.
enum Refreshed {
    Unchanged,
    Indexed { added: bool, status: &'static str },
    Removed,
}

/// One path against the index, with the rules already loaded: index it if
/// it exists and is included, remove it otherwise. A folder is not handled
/// here (the caller scans).
fn refresh_one(
    project: &Project,
    db: &mut Db,
    rel: &str,
    rule_sets: &[&[crate::kenignore::Rule]; 3],
    git: &mut GitIgnores,
) -> Result<Refreshed> {
    let abs = project.root.join(rel);
    let excluded = project.is_excluded(rel)
        || is_hidden_rel(rel)
        || rel.rsplit('/').next().is_some_and(|n| is_office_lock_name(n) || is_backup_name(n))
        || inside_linked_checkout(&project.root, rel)
        || git.ignored(rel);
    let tier = crate::kenignore::classify(rel, false, rule_sets);
    let excluded = excluded || tier == crate::kenignore::Tier::Ignore;
    if !excluded && abs.is_file() {
        let meta = abs.metadata().map_err(|e| crate::Error::io(&abs, e))?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let dataless = cloud::is_dataless(&meta);
        let before = db.get_file(rel)?;
        if let Some(f) = &before {
            if f.size == meta.len() as i64 && f.mtime == mtime && !needs_retry(&f.status, f.error.as_deref(), dataless) {
                db.set_file_tiers(&[(rel.to_string(), tier)])?;
                return Ok(Refreshed::Unchanged);
            }
        }
        if !dataless && only_mtime_moved(db, rel, &abs, meta.len() as i64, mtime)? {
            db.set_file_tiers(&[(rel.to_string(), tier)])?;
            return Ok(Refreshed::Unchanged);
        }
        let status = index_one(project, db, rel, meta.len() as i64, mtime, dataless, tier)?;
        db.set_file_tiers(&[(rel.to_string(), tier)])?;
        Ok(Refreshed::Indexed { added: before.is_none(), status })
    } else if db.get_file(rel)?.is_some() {
        db.remove_file(rel)?;
        Ok(Refreshed::Removed)
    } else {
        Ok(Refreshed::Unchanged)
    }
}

/// Most paths a scoped rescan takes; a bigger burst (a checkout, a sync
/// client catching up) is cheaper as one full scan.
pub const SCOPED_MAX: usize = 2_000;

/// Rescan only `rels`, the paths a watcher saw change, instead of the whole
/// repo: what a full scan would do to those paths, and the same follow-ups
/// (the control query, the vocabulary, the link report) when a page moved.
/// `None` when a full scan is the right tool: too many paths, or a folder
/// among them (a folder moved in brings files no event named).
pub fn scan_paths(project: &Project, db: &mut Db, rels: &[String]) -> Result<Option<ScanStats>> {
    if crate::registry::awaits_kind(&project.root) {
        return Ok(Some(ScanStats::default()));
    }
    if rels.len() > SCOPED_MAX || rels.iter().any(|r| project.root.join(r).is_dir()) {
        return Ok(None);
    }
    let built_in_rules = crate::kenignore::built_in_rule_sets();
    let kind_rules = crate::kenignore::kind_rules_for(&project.root);
    let user_rules = project.kenignore_rules();
    let rule_sets: [&[crate::kenignore::Rule]; 3] = [&built_in_rules, &kind_rules, &user_rules];
    let mut git = GitIgnores::of(project);
    let mut stats = ScanStats::default();

    db.begin_batch()?;
    let mut gone_folders: Vec<&String> = Vec::new();
    let result = (|| -> Result<()> {
        for rel in rels {
            let abs = project.root.join(rel);
            if !abs.exists() && db.get_file(rel)?.is_none() {
                // Nothing indexed under this name: a folder that went away,
                // or a file never indexed. Its subtree is reconciled below.
                gone_folders.push(rel);
                continue;
            }
            match refresh_one(project, db, rel, &rule_sets, &mut git)? {
                Refreshed::Unchanged => stats.unchanged += 1,
                Refreshed::Removed => {
                    stats.removed += 1;
                    stats.changed_paths.push(rel.clone());
                }
                Refreshed::Indexed { added, status } => {
                    if added {
                        stats.added += 1;
                    } else {
                        stats.updated += 1;
                    }
                    if status == STATUS_FAILED {
                        stats.failed += 1;
                    }
                    if status == STATUS_METADATA_ONLY && FileKind::from_path(&abs) == FileKind::Video {
                        stats.videos_needing_transcript.push(rel.clone());
                    }
                    stats.changed_paths.push(rel.clone());
                }
            }
        }
        Ok(())
    })();
    match result {
        Ok(()) => db.commit_batch()?,
        Err(e) => {
            db.rollback_batch();
            return Err(e);
        }
    }
    for rel in gone_folders {
        let removed = db.remove_folder(rel)?;
        if removed > 0 {
            stats.removed += removed;
            stats.changed_paths.push(rel.clone());
        }
    }

    let pages_moved = stats.changed_paths.iter().any(|p| p.ends_with(".md"));
    if !stats.changed_paths.is_empty() {
        crate::authority::refresh(db, &project.root, Some(&stats.changed_paths))?;
    }
    if !stats.changed_paths.is_empty() {
        if let Some(result) = control_query(db)? {
            db.set_index_control(&result)?;
        }
    }
    if pages_moved || db.stored_vocabulary()?.is_none() {
        crate::vocab::Vocabulary::rebuild(db)?;
    }
    Ok(Some(stats))
}

/// Hidden for single-path reindex purposes. Mirrors the walker's rule — any
/// dot-prefixed component hides the path — with the same `.ken/` allowlist
/// carve-out, so a memory or task file created through the UI (which reaches
/// the index via `refresh_path`, not the walker) is indexed exactly like one
/// found by a full scan. Without this the two entry points disagree, and the
/// disagreement is silent: the file simply never appears in search.
fn is_hidden_rel(rel: &str) -> bool {
    rel.split('/').any(|part| part.starts_with('.')) && !is_ken_allowlisted_path(rel)
}

/// Full rebuild: drop everything and rescan.
///
/// "Everything" includes the on-disk transcript cache (`.ken/transcripts`): a
/// reindex means "start from scratch", so regenerated transcripts must not
/// reuse stale VTTs keyed off paths that may no longer exist. The DB is cleared
/// (files, contents, extractions, entities, events, knowledge-model stamp) and
/// the cache dir removed before the fresh scan repopulates the index.
pub fn reindex(project: &Project, db: &mut Db) -> Result<ScanStats> {
    db.clear()?;
    // Remove the generated-transcript cache. Ignore a missing dir (nothing was
    // ever transcribed); surface real I/O errors.
    let cache = crate::transcript::cache_dir(&project.root);
    match std::fs::remove_dir_all(&cache) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(crate::Error::io(&cache, e)),
    }
    scan(project, db)
}

/// Convenience wrapper mirroring the design's `ken_core::search`.
pub fn search(db: &Db, query: &str, limit: usize) -> Result<Vec<crate::db::SearchHit>> {
    db.search(query, limit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn fixture_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/project")
    }

    /// Copy the fixture project into a temp dir so tests can mutate it.
    fn temp_project() -> (tempfile::TempDir, Project) {
        let dir = tempfile::tempdir().unwrap();
        copy_dir(&fixture_root(), dir.path());
        let project = Project::create(dir.path(), "Fixture").unwrap();
        (dir, project)
    }

    fn copy_dir(src: &Path, dst: &Path) {
        for entry in fs::read_dir(src).unwrap().flatten() {
            let to = dst.join(entry.file_name());
            if entry.path().is_dir() {
                fs::create_dir_all(&to).unwrap();
                copy_dir(&entry.path(), &to);
            } else {
                fs::copy(entry.path(), &to).unwrap();
            }
        }
    }

    /// A `.ui` file indexed when `.ui` read as binary: name only, its byte
    /// hash stored. Once the kind says text, the next scan reads it, though
    /// its bytes are the same.
    #[test]
    fn a_file_whose_kind_became_text_is_read_by_the_next_scan() {
        let (_dir, project) = temp_project();
        let mut db = Db::open_in_memory().unwrap();
        fs::write(project.root.join("PartyMemberChip.ui"), "Group #NameCell { Anchor: (Width: 168); }\n").unwrap();
        scan(&project, &mut db).unwrap();
        let row = db.get_file("PartyMemberChip.ui").unwrap().unwrap();
        // As an older Ken left it.
        db.upsert_file("PartyMemberChip.ui", "binary", row.size, row.mtime, STATUS_METADATA_ONLY, None, "").unwrap();
        db.delete_chunks("PartyMemberChip.ui").unwrap();
        assert!(db.file_byte_hash("PartyMemberChip.ui").unwrap().is_some());
        assert!(db.search_chunks_fts("NameCell", 5).unwrap().is_empty());

        assert_eq!(db.refresh_stored_kinds().unwrap(), 1);
        let stats = scan(&project, &mut db).unwrap();
        assert_eq!(stats.updated, 1, "{stats:?}");
        let row = db.get_file("PartyMemberChip.ui").unwrap().unwrap();
        assert_eq!((row.kind.as_str(), row.status.as_str()), ("code", STATUS_INDEXED));
        assert_eq!(db.search_chunks_fts("NameCell", 5).unwrap().first().map(|h| h.path.as_str()), Some("PartyMemberChip.ui"));
    }

    #[test]
    fn indexing_enqueues_changed_files_for_extraction() {
        let (dir, project) = temp_project();
        let mut db = Db::open(&PathBuf::from(dir.path()).join("idx"), project.config.id)
            .unwrap_or_else(|_| Db::open_in_memory().unwrap());
        // Index a real indexed file directly through index_one.
        fs::write(project.root.join("note.md"), "Priya leads billing.").unwrap();
        let meta = project.root.join("note.md").metadata().unwrap();
        let status = index_one(
            &project, &mut db, "note.md",
            meta.len() as i64, 0, false, crate::kenignore::Tier::Full,
        ).unwrap();
        assert_eq!(status, STATUS_INDEXED);
        // The file is now queued with the hash of its extracted text.
        assert_eq!(
            db.next_pending_extraction().unwrap(),
            Some(("note.md".into(), crate::knowledge_model::content_hash("Priya leads billing.").into()))
        );

        // Re-indexing identical content does NOT re-queue once it's done.
        db.mark_extraction_done("note.md", &crate::knowledge_model::content_hash("Priya leads billing."), 1).unwrap();
        index_one(&project, &mut db, "note.md", meta.len() as i64, 0, false, crate::kenignore::Tier::Full).unwrap();
        assert!(db.next_pending_extraction().unwrap().is_none());
    }

    #[test]
    fn images_and_sparse_pdfs_enqueue_ocr_but_text_pdfs_and_svg_do_not() {
        let (dir, project) = temp_project();
        let mut db = Db::open_in_memory().unwrap();

        // An image (no EXIF text): enqueued for OCR.
        fs::write(project.root.join("photo.png"), b"not really a png").unwrap();
        let meta = project.root.join("photo.png").metadata().unwrap();
        index_one(&project, &mut db, "photo.png", meta.len() as i64, 5, false, crate::kenignore::Tier::Full).unwrap();
        assert_eq!(
            db.next_pending_ocr().unwrap().map(|(r, _)| r),
            Some("photo.png".to_string()),
            "an image should be queued for OCR"
        );
        // Drain it so the next assertions start clean.
        let (rel, hash) = db.next_pending_ocr().unwrap().unwrap();
        db.mark_ocr_done(&rel, &hash, &[]).unwrap();

        // An SVG is vector-only — the OCR bridge can't rasterize it — so skip.
        fs::write(project.root.join("logo.svg"), b"<svg></svg>").unwrap();
        let m = project.root.join("logo.svg").metadata().unwrap();
        index_one(&project, &mut db, "logo.svg", m.len() as i64, 5, false, crate::kenignore::Tier::Full).unwrap();
        assert!(db.next_pending_ocr().unwrap().is_none(), "SVG must not enqueue OCR");

        // The fixture PDF has only a one-line text layer (sparse) — exactly the
        // scanned/image case the heuristic catches — so it IS queued for OCR.
        // (A genuine multi-paragraph text PDF clears the threshold; that path is
        // covered directly by `pdf_low_text_heuristic`.)
        let pdf = project.root.join("vendor/contract.pdf");
        let pm = pdf.metadata().unwrap();
        index_one(&project, &mut db, "vendor/contract.pdf", pm.len() as i64, 5, false, crate::kenignore::Tier::Full).unwrap();
        assert_eq!(
            db.next_pending_ocr().unwrap().map(|(r, _)| r),
            Some("vendor/contract.pdf".to_string()),
            "a sparse (scanned-style) PDF should be queued for OCR"
        );
        drop(dir);
    }

    #[test]
    fn nothing_is_queued_for_ocr_without_an_engine() {
        crate::ocr::tests::force(false);
        let (dir, project) = temp_project();
        let mut db = Db::open_in_memory().unwrap();
        fs::write(project.root.join("photo.png"), b"not really a png").unwrap();
        let meta = project.root.join("photo.png").metadata().unwrap();
        index_one(&project, &mut db, "photo.png", meta.len() as i64, 5, false, crate::kenignore::Tier::Full).unwrap();
        assert!(db.next_pending_ocr().unwrap().is_none());
        drop(dir);
    }

    #[test]
    fn ocr_enqueue_is_idempotent_across_unchanged_rescans() {
        let (dir, project) = temp_project();
        let mut db = Db::open_in_memory().unwrap();
        fs::write(project.root.join("photo.png"), b"bytes").unwrap();
        let meta = project.root.join("photo.png").metadata().unwrap();
        let (size, mtime) = (meta.len() as i64, 5);

        index_one(&project, &mut db, "photo.png", size, mtime, false, crate::kenignore::Tier::Full).unwrap();
        let (rel, hash) = db.next_pending_ocr().unwrap().unwrap();
        db.mark_ocr_done(&rel, &hash, &[]).unwrap();

        // Same (size, mtime): re-indexing does NOT re-OCR.
        index_one(&project, &mut db, "photo.png", size, mtime, false, crate::kenignore::Tier::Full).unwrap();
        assert!(db.next_pending_ocr().unwrap().is_none(), "unchanged image must not re-OCR");

        // A changed mtime (new version) re-queues it.
        index_one(&project, &mut db, "photo.png", size, mtime + 1, false, crate::kenignore::Tier::Full).unwrap();
        assert_eq!(
            db.next_pending_ocr().unwrap().map(|(r, _)| r),
            Some("photo.png".to_string())
        );
        drop(dir);
    }

    #[test]
    fn pdf_low_text_heuristic() {
        assert!(pdf_text_is_sparse(""));
        assert!(pdf_text_is_sparse("  \n \t 12 \n"));
        assert!(!pdf_text_is_sparse(&"word ".repeat(50)));
    }

    #[test]
    fn initial_scan_indexes_all_formats() {
        let (_dir, project) = temp_project();
        let mut db = Db::open_in_memory().unwrap();
        let stats = scan(&project, &mut db).unwrap();

        // 11 fixture files, one of which (corrupt.pdf) fails extraction.
        assert_eq!(stats.added, 11, "stats: {stats:?}");
        assert_eq!(stats.failed, 1);

        // Content from every supported format is searchable.
        for (q, path_frag) in [
            ("billing cutover", "meeting.md"),
            ("rollback rehearsal", "plain.txt"),
            ("vendor pricing", "example.rs"),
            ("quarterly budget", "sample.docx"),
            ("kickoff deck", "deck.pptx"),
            ("LangdonSoft", "quotes.xlsx"),
            ("contract renewal terms", "contract.pdf"),
        ] {
            let hits = db.search(q, 10).unwrap();
            assert!(
                hits.iter().any(|h| h.rel_path.contains(path_frag)),
                "query {q:?} should hit {path_frag}: {hits:?}"
            );
        }

        // Failed + binary files findable by name.
        assert!(db.search("corrupt", 5).unwrap().iter().any(|h| h.status == "failed"));
        assert!(!db.search("team photo", 5).unwrap().is_empty());
    }

    /// A OneDrive/iCloud placeholder must never be read during indexing —
    /// reading it forces a multi-second on-demand download that stalls the
    /// scan and often dies with ETIMEDOUT. Index the name, skip the bytes.
    #[test]
    fn cloud_only_file_is_indexed_without_reading_its_bytes() {
        let (_dir, project) = temp_project();
        let mut db = Db::open_in_memory().unwrap();

        let status = index_one(&project, &mut db, "notes/meeting.md", 120, 7, true, crate::kenignore::Tier::Full).unwrap();

        assert_eq!(status, STATUS_CLOUD_ONLY);
        let row = db.get_file("notes/meeting.md").unwrap().unwrap();
        assert_eq!(row.status, STATUS_CLOUD_ONLY);
        assert_eq!(row.error, None);
        // Findable by name…
        assert!(db
            .search("meeting", 5)
            .unwrap()
            .iter()
            .any(|h| h.rel_path == "notes/meeting.md"));
        // …but its content was deliberately not extracted.
        assert!(db.search("billing cutover", 5).unwrap().is_empty());
    }

    /// Once the bytes arrive (the user opened the file, or OneDrive synced it
    /// down), the next scan must pick the content up. Hydration changes
    /// neither size nor mtime, so status — not the (size, mtime) key — has to
    /// drive the retry.
    #[test]
    fn rescan_extracts_a_cloud_only_file_whose_bytes_have_arrived() {
        let (_dir, project) = temp_project();
        let mut db = Db::open_in_memory().unwrap();
        scan(&project, &mut db).unwrap();

        // The file was skipped as cloud-only on an earlier scan. Its bytes are
        // on disk now (fixture files are ordinary local files).
        let row = db.get_file("notes/meeting.md").unwrap().unwrap();
        db.upsert_file(
            &row.rel_path, &row.kind, row.size, row.mtime, STATUS_CLOUD_ONLY, None, "",
        )
        .unwrap();

        let stats = scan(&project, &mut db).unwrap();

        assert_eq!(stats.updated, 1, "the cloud-only row should be retried: {stats:?}");
        let after = db.get_file("notes/meeting.md").unwrap().unwrap();
        assert_eq!(after.status, STATUS_INDEXED);
        assert_eq!(after.error, None);
        assert!(db
            .search("billing cutover", 5)
            .unwrap()
            .iter()
            .any(|h| h.rel_path.contains("meeting.md")));
    }

    /// The retry rule has to converge: re-index exactly the rows whose content
    /// could now be read, and leave every other row alone. Anything else means
    /// either content that never appears, or a rescan that re-extracts files
    /// forever (and re-triggers the ingest recipes watching them).
    #[test]
    fn retry_rule_covers_only_rows_whose_content_could_have_changed() {
        let timeout = Some("extraction failed: Operation timed out (os error 60)");

        // Still in the cloud: nothing new to read, so don't touch it.
        assert!(!needs_retry(STATUS_CLOUD_ONLY, None, true));
        // Bytes have landed: extract the content.
        assert!(needs_retry(STATUS_CLOUD_ONLY, None, false));

        // A timed-out read of a file that's still a placeholder — a row left
        // by the old behaviour. Re-index it (cheaply) as cloud_only.
        assert!(needs_retry(STATUS_FAILED, timeout, true));
        // …and once it is cloud_only, the scan above leaves it alone: converges.

        // A genuinely corrupt local file must not be re-extracted every scan.
        assert!(!needs_retry(STATUS_FAILED, Some("PDF error: invalid xref"), false));

        // Healthy rows are never redundant work.
        assert!(!needs_retry(STATUS_INDEXED, None, false));
        assert!(!needs_retry(STATUS_METADATA_ONLY, None, false));
    }

    #[test]
    fn rescan_is_incremental() {
        let (dir, project) = temp_project();
        let mut db = Db::open_in_memory().unwrap();
        scan(&project, &mut db).unwrap();

        // Touch nothing: everything unchanged.
        let stats = scan(&project, &mut db).unwrap();
        assert_eq!(stats.added + stats.updated + stats.removed, 0, "{stats:?}");

        // Modify, add, remove.
        fs::write(dir.path().join("notes/plain.txt"), "entirely new fact\n").unwrap();
        fs::write(dir.path().join("notes/new.md"), "# New\nfresh doc\n").unwrap();
        fs::remove_file(dir.path().join("src/example.rs")).unwrap();
        // mtime granularity is 1s; force a different mtime.
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(10);
        let f = fs::File::options()
            .write(true)
            .open(dir.path().join("notes/plain.txt"))
            .unwrap();
        f.set_modified(past).unwrap();
        drop(f);

        let stats = scan(&project, &mut db).unwrap();
        assert_eq!(stats.added, 1, "{stats:?}");
        assert_eq!(stats.updated, 1, "{stats:?}");
        assert_eq!(stats.removed, 1, "{stats:?}");

        assert!(!db.search("entirely new fact", 5).unwrap().is_empty());
        assert!(!db.search("fresh doc", 5).unwrap().is_empty());
        assert!(db.search("vendor pricing", 5).unwrap().is_empty());
    }

    #[test]
    fn exclusions_apply_on_scan_and_rescan() {
        let (_dir, mut project) = temp_project();
        let mut db = Db::open_in_memory().unwrap();
        scan(&project, &mut db).unwrap();
        assert!(!db.search("obsolete cutover plan", 5).unwrap().is_empty());

        project.set_excluded(vec!["archive".into()]).unwrap();
        let stats = scan(&project, &mut db).unwrap();
        assert_eq!(stats.removed, 1, "{stats:?}");
        assert!(db.search("obsolete cutover plan", 5).unwrap().is_empty());

        project.set_excluded(vec![]).unwrap();
        let stats = scan(&project, &mut db).unwrap();
        assert_eq!(stats.added, 1, "{stats:?}");
        assert!(!db.search("obsolete cutover plan", 5).unwrap().is_empty());
    }

    #[test]
    fn hidden_and_config_files_not_indexed() {
        let (dir, project) = temp_project();
        fs::write(dir.path().join(".DS_Store"), "junk").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        scan(&project, &mut db).unwrap();
        assert!(db.get_file(".DS_Store").unwrap().is_none());
        assert!(db.get_file(".ken/project.json").unwrap().is_none());
        drop(dir);
    }

    /// In a git repo, `.gitignore` is honoured — otherwise a Gradle
    /// project's 1.7GB `build/` and a server's `run/universe` world data
    /// land in the index.
    #[test]
    fn a_kenignored_folder_leaves_the_index_at_every_depth() {
        let (dir, project) = temp_project();
        let root = dir.path();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".gitignore"), "brands/*/runs/\n").unwrap();
        let files = [
            "brands/att/theme.yaml",
            "brands/att/backend/data/mocks/a.json",
            "brands/att/backend/components/01-system-context/context/norms.md",
            "brands/att/backend/components/12-skill/market-analysis/SKILL.md",
            "brands/att/backend/components/12-skill/market-analysis/deep/x/y.md",
            "src/keep.md",
        ];
        for f in files {
            fs::create_dir_all(root.join(f).parent().unwrap()).unwrap();
            fs::write(root.join(f), format!("text of {f}")).unwrap();
        }
        let mut db = Db::open_in_memory().unwrap();
        scan(&project, &mut db).unwrap();
        for f in files {
            assert!(db.get_file(f).unwrap().is_some(), "{f} indexed first");
        }
        fs::write(root.join(".kenignore"), "brands/\n").unwrap();
        let stats = scan(&project, &mut db).unwrap();
        let left: Vec<&str> = files.iter().copied().filter(|f| f.starts_with("brands/") && db.get_file(f).unwrap().is_some()).collect();
        assert!(left.is_empty(), "still indexed: {left:?} ({stats:?})");
        assert!(db.get_file("src/keep.md").unwrap().is_some());
    }

    /// The workspace's own `.kenignore` (the one setup writes) reaches a
    /// member's scan for the lines that name that member.
    #[test]
    fn a_workspace_kenignore_line_takes_a_member_folder_out() {
        let ws = tempfile::tempdir().unwrap();
        fs::create_dir_all(ws.path().join(crate::workspace::CONFIG_DIR)).unwrap();
        let root = ws.path().join("app");
        fs::create_dir_all(root.join("frontend/src")).unwrap();
        fs::write(root.join("frontend/src/a.ts"), "export const a = 1;").unwrap();
        fs::write(root.join("readme.md"), "# App").unwrap();
        fs::write(ws.path().join(".kenignore"), "app/frontend/\nother/x/\n").unwrap();
        let project = Project::create(&root, "app").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        scan(&project, &mut db).unwrap();
        assert!(db.get_file("frontend/src/a.ts").unwrap().is_none(), "the workspace line applies inside the member");
        assert!(db.get_file("readme.md").unwrap().is_some());
        // The member's own file still has the last word.
        fs::write(root.join(".kenignore"), "!frontend/\n").unwrap();
        scan(&project, &mut db).unwrap();
        assert!(db.get_file("frontend/src/a.ts").unwrap().is_some());
    }

    #[test]
    fn gitignore_is_honoured_inside_a_repo() {
        let (dir, project) = temp_project();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(".gitignore"), "build/\nrun/\n").unwrap();
        fs::create_dir_all(dir.path().join("build")).unwrap();
        fs::create_dir_all(dir.path().join("run/universe")).unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("build/out.txt"), "generated artifact").unwrap();
        fs::write(dir.path().join("run/universe/region.txt"), "world data").unwrap();
        fs::write(dir.path().join("src/main.txt"), "authored source").unwrap();

        let mut db = Db::open_in_memory().unwrap();
        scan(&project, &mut db).unwrap();

        assert!(db.get_file("src/main.txt").unwrap().is_some(), "source must index");
        assert!(db.get_file("build/out.txt").unwrap().is_none(), "build output must not");
        assert!(
            db.get_file("run/universe/region.txt").unwrap().is_none(),
            "world data must not"
        );
        drop(dir);
    }

    /// …and NOT outside one. A notes folder that happens to carry a
    /// `.gitignore` (copied in, inherited) must not have its documents
    /// silently hidden.
    #[test]
    fn gitignore_is_ignored_outside_a_repo() {
        let (dir, project) = temp_project();
        fs::write(dir.path().join(".gitignore"), "notes/\n").unwrap();
        fs::create_dir_all(dir.path().join("notes")).unwrap();
        fs::write(dir.path().join("notes/plan.md"), "a real document").unwrap();

        let mut db = Db::open_in_memory().unwrap();
        scan(&project, &mut db).unwrap();
        assert!(
            db.get_file("notes/plan.md").unwrap().is_some(),
            "no .git means .gitignore has no authority here"
        );
        drop(dir);
    }

    /// Pins the limitation documented at the walker: `.kenignore` filters
    /// what the walk YIELDS, so in a repo a `!` line cannot resurrect a
    /// path `.gitignore` already excluded. Asserted so the behavior can't
    /// drift silently in either direction — if someone later makes `!`
    /// win, this test should fail and be updated deliberately.
    #[test]
    fn kenignore_bang_cannot_resurrect_a_gitignored_path() {
        let (dir, project) = temp_project();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(".gitignore"), "dist/\n").unwrap();
        fs::write(dir.path().join(".kenignore"), "!dist/\n").unwrap();
        fs::create_dir_all(dir.path().join("dist")).unwrap();
        fs::write(dir.path().join("dist/notes.md"), "wanted, but gitignored").unwrap();

        let mut db = Db::open_in_memory().unwrap();
        scan(&project, &mut db).unwrap();
        assert!(
            db.get_file("dist/notes.md").unwrap().is_none(),
            "the walker never yields it, so kenignore never sees it"
        );
        drop(dir);
    }

    #[test]
    fn ken_memory_and_tasks_are_indexed_but_other_dot_ken_paths_are_not() {
        let (dir, project) = temp_project();
        fs::create_dir_all(dir.path().join(".ken/memory")).unwrap();
        fs::create_dir_all(dir.path().join(".ken/tasks/archive/2026-07")).unwrap();
        fs::write(dir.path().join(".ken/memory/foo.md"), "a project memory").unwrap();
        fs::write(dir.path().join(".ken/tasks/bar.md"), "a task").unwrap();
        fs::write(
            dir.path().join(".ken/tasks/archive/2026-07/old.md"),
            "an archived task",
        )
        .unwrap();
        fs::write(dir.path().join(".ken/index-profile.json"), "{}").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        scan(&project, &mut db).unwrap();

        // Allowlisted: walked and indexed, including a nested archive folder.
        assert!(db.get_file(".ken/memory/foo.md").unwrap().is_some());
        assert!(db.get_file(".ken/tasks/bar.md").unwrap().is_some());
        assert!(db.get_file(".ken/tasks/archive/2026-07/old.md").unwrap().is_some());

        // Everything else under `.ken/` stays excluded, same as today.
        assert!(db.get_file(".ken/project.json").unwrap().is_none());
        assert!(db.get_file(".ken/index-profile.json").unwrap().is_none());
        drop(dir);
    }

    #[test]
    fn other_dot_directories_still_excluded_alongside_ken_allowlist() {
        let (dir, project) = temp_project();
        fs::create_dir_all(dir.path().join(".ken/memory")).unwrap();
        fs::write(dir.path().join(".ken/memory/foo.md"), "a project memory").unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(".git/HEAD"), "ref: refs/heads/main").unwrap();
        fs::create_dir_all(dir.path().join(".vscode")).unwrap();
        fs::write(dir.path().join(".vscode/settings.json"), "{}").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        scan(&project, &mut db).unwrap();

        assert!(db.get_file(".ken/memory/foo.md").unwrap().is_some());
        assert!(db.get_file(".git/HEAD").unwrap().is_none());
        assert!(db.get_file(".vscode/settings.json").unwrap().is_none());
        drop(dir);
    }

    #[test]
    fn is_ken_allowlisted_path_matches_the_allowlist() {
        // `.ken/memory/**` and `.ken/tasks/**` qualify; `.ken` itself, other
        // `.ken/` metadata, and non-`.ken` paths never do — this predicate
        // only answers "is this specifically a Ken-owned carve-out", nothing
        // broader. `scan::scan` and `watch::relevant_path` both delegate to
        // it for that one narrow question; see
        // `watch::tests::scan_and_watch_agree_on_ken_paths` for the fuller
        // walked-vs-watched agreement check.
        let cases: &[(&str, bool)] = &[
            (".ken/memory/foo.md", true),
            (".ken/memory/sub/bar.md", true),
            (".ken/tasks/bar.md", true),
            (".ken/tasks/archive/2026-07/old.md", true),
            (".ken", false),
            (".ken/project.json", false),
            (".ken/index-profile.json", false),
            (".git/HEAD", false),
            (".vscode/settings.json", false),
            ("notes/meeting.md", false),
        ];
        for (rel, want_ken_allowlisted) in cases {
            assert_eq!(
                is_ken_allowlisted_path(rel),
                *want_ken_allowlisted,
                "is_ken_allowlisted_path({rel:?})"
            );
        }
    }

    /// Making a whole repo search-only (a code or reference kind writes the
    /// same `~*` rule) must reach files already indexed, though none of them
    /// changed: they leave the extraction queue and the coverage count, and
    /// a project reopen's backfill does not put them back.
    #[test]
    fn a_repo_made_search_only_leaves_the_extraction_queue_without_a_file_change() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("plan.md"), "# Plan\nShip it.\n").unwrap();
        fs::write(dir.path().join("notes.md"), "Some notes.\n").unwrap();
        let project = Project::create(dir.path(), "Retier").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        scan(&project, &mut db).unwrap();
        assert!(db.next_pending_extraction().unwrap().is_some(), "full tier: queued");
        assert_eq!(db.extraction_coverage().unwrap().1, 2);
        // What the graph read out of them: one entity from plan.md alone, one
        // also backed by a file that stays full, and an event from notes.md.
        let entity = |name: &str, sources: &[&str]| crate::db::EntityInput {
            kind: "topic".into(),
            name: name.into(),
            summary: String::new(),
            sources: sources.iter().map(|s| s.to_string()).collect(),
            connections: Vec::new(),
        };
        db.replace_knowledge_model(
            &[entity("Ship", &["plan.md"]), entity("Notes", &["notes.md", "elsewhere.md"])],
            &[crate::db::EventInput { date: "2026-09-25".into(), category: "note".into(), text: "x".into(), source: "notes.md".into() }],
            1,
        )
        .unwrap();

        fs::write(dir.path().join(".kenignore"), "~*\n").unwrap();
        let stats = scan(&project, &mut db).unwrap();
        assert_eq!(stats.updated + stats.added, 0, "no file changed: {stats:?}");
        assert!(db.next_pending_extraction().unwrap().is_none(), "search-only: off the queue");
        assert_eq!(db.extraction_coverage().unwrap().1, 0, "not counted as extractable");
        assert_eq!(db.backfill_extractions().unwrap(), 0, "backfill leaves search-only alone");
        assert_eq!(db.unqueued_extractable_count().unwrap(), 0);
        // Their entities left the graph with them; one another file backs stays.
        let (entities, _) = db.list_entities_with_edges().unwrap();
        let names: Vec<&str> = entities.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["Notes"], "{names:?}");
        assert_eq!(entities[0].sources, vec!["elsewhere.md".to_string()]);

        // Back to full: the files are extractable again and backfill queues them.
        fs::remove_file(dir.path().join(".kenignore")).unwrap();
        scan(&project, &mut db).unwrap();
        assert_eq!(db.extraction_coverage().unwrap().1, 2);
        assert_eq!(db.backfill_extractions().unwrap(), 2);
    }

    /// A folder a `.kenignore` line drops leaves the code map with its files,
    /// so find-usages never points into it; unignored, it is mapped again.
    #[test]
    fn an_ignored_folder_leaves_the_code_map() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("app")).unwrap();
        fs::create_dir_all(dir.path().join("web")).unwrap();
        fs::write(dir.path().join("app/auth.py"), "def current_user():\n    return 1\n").unwrap();
        fs::write(dir.path().join("web/page.py"), "from app.auth import current_user\n\ndef show():\n    return current_user()\n").unwrap();
        let project = Project::create(dir.path(), "T").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        scan(&project, &mut db).unwrap();
        let paths = |db: &Db| {
            let mut p: Vec<String> = db.code_symbols(None, None, None, 100).unwrap().into_iter().map(|s| s.path).collect();
            p.dedup();
            p
        };
        assert_eq!(paths(&db), vec!["app/auth.py", "web/page.py"]);

        fs::write(dir.path().join(".kenignore"), "web/\n").unwrap();
        scan(&project, &mut db).unwrap();
        assert_eq!(paths(&db), vec!["app/auth.py"], "the ignored folder's symbols went with its files");

        fs::remove_file(dir.path().join(".kenignore")).unwrap();
        scan(&project, &mut db).unwrap();
        assert_eq!(paths(&db), vec!["app/auth.py", "web/page.py"]);
    }

    /// A linked worktree (a folder whose `.git` is a file) is another
    /// checkout: not walked, and a watcher event inside it is a no-op. A
    /// nested clone and plain folders are still walked. Secrets and archives
    /// never get a row.
    #[test]
    fn worktrees_secrets_and_archives_stay_out_of_the_index() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("plan.md"), "# Plan\n").unwrap();
        fs::create_dir_all(root.join("wt-u7/docs")).unwrap();
        fs::write(root.join("wt-u7/.git"), "gitdir: ../main/.git/worktrees/u7\n").unwrap();
        fs::write(root.join("wt-u7/docs/plan.md"), "# Plan, again\n").unwrap();
        fs::create_dir_all(root.join("vendor-clone/.git")).unwrap();
        fs::write(root.join("vendor-clone/readme.md"), "kept\n").unwrap();
        fs::create_dir_all(root.join("config")).unwrap();
        fs::write(root.join("config/credentials.json"), "{\"token\":\"x\"}").unwrap();
        fs::write(root.join("config/secrets.yaml"), "token: x\n").unwrap();
        fs::write(root.join("backup.zip"), "PK").unwrap();
        let project = Project::create(root, "Skips").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        scan(&project, &mut db).unwrap();

        assert!(db.get_file("plan.md").unwrap().is_some());
        assert!(db.get_file("vendor-clone/readme.md").unwrap().is_some(), "a nested clone is walked");
        for gone in ["wt-u7/docs/plan.md", "config/credentials.json", "config/secrets.yaml", "backup.zip"] {
            assert!(db.get_file(gone).unwrap().is_none(), "{gone}");
        }
        assert!(!refresh_path(&project, &mut db, "wt-u7/docs/plan.md").unwrap());
        assert!(db.get_file("wt-u7/docs/plan.md").unwrap().is_none());
        assert!(!refresh_path(&project, &mut db, "config/credentials.json").unwrap());
    }

    /// A new modified time on the same bytes (a sync client touching the
    /// file) is not a change: no re-parse, no update counted, and the stored
    /// time moves on. A real edit of the same size still re-indexes.
    #[test]
    fn a_touched_file_with_the_same_bytes_is_not_read_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plan.md");
        fs::write(&path, "# Plan\nalpha\n").unwrap();
        let project = Project::create(dir.path(), "Touch").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        scan(&project, &mut db).unwrap();
        let before = db.get_file("plan.md").unwrap().unwrap().mtime;

        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(120);
        fs::File::options().write(true).open(&path).unwrap().set_modified(later).unwrap();
        let stats = scan(&project, &mut db).unwrap();
        assert_eq!((stats.updated, stats.unchanged), (0, 1), "{stats:?}");
        let after = db.get_file("plan.md").unwrap().unwrap().mtime;
        assert!(after > before, "the stored time follows the file");
        assert!(!refresh_path(&project, &mut db, "plan.md").unwrap(), "a watcher touch is a no-op");

        fs::write(&path, "# Plan\nbravo\n").unwrap(); // same size, new bytes
        let stats = scan(&project, &mut db).unwrap();
        assert_eq!(stats.updated, 1, "{stats:?}");
        assert!(db.get_file("plan.md").unwrap().is_some());
    }

    #[test]
    fn a_pages_frontmatter_is_stored_at_index_time_and_backfilled_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let page = dir.path().join("Rules.md");
        fs::write(&page, "---\ntitle: Rules\naliases: [\"binding rules\"]\nverified: 2026-09-20\n---\n# Rules\n").unwrap();
        let project = Project::create(dir.path(), "Meta").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        scan(&project, &mut db).unwrap();
        let meta = db.page_meta("Rules.md").unwrap().unwrap();
        assert_eq!(meta.aliases, vec!["binding rules"]);
        assert_eq!(meta.verified.as_deref(), Some("2026-09-20"));

        // A page indexed before frontmatter was read gets it on open.
        db.set_page_meta("Rules.md", None).unwrap();
        assert_eq!(db.backfill_page_meta().unwrap(), 1);
        assert!(db.page_meta("Rules.md").unwrap().is_some());

        fs::write(&page, "# Rules, no frontmatter now, and longer\n").unwrap();
        scan(&project, &mut db).unwrap();
        assert_eq!(db.page_meta("Rules.md").unwrap(), None);
    }

    #[test]
    fn each_scan_runs_the_control_query_and_health_counts_the_queue() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("START-HERE.md"), "---\ntitle: Start here\n---\n# Start here\nRead this first.\n").unwrap();
        fs::write(dir.path().join("notes.md"), "Notes that mention start once.\n").unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "fn main() {}\n").unwrap();
        fs::write(dir.path().join(".kenignore"), "~src/\n").unwrap();
        let project = Project::create(dir.path(), "Health").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        scan(&project, &mut db).unwrap();

        let h = db.index_health().unwrap();
        let control = h.control.clone().unwrap();
        assert!(control.ok, "{control:?}");
        assert_eq!((control.page.as_str(), control.query.as_str()), ("START-HERE.md", "Start here"));
        assert_eq!(h.pending, 2, "both pages queued");
        assert_eq!(h.skipped, 1, "src/ is searchable only");
        assert_eq!((h.failed, h.retrying), (0, 0));

        let empty = tempfile::tempdir().unwrap();
        fs::write(empty.path().join("a.md"), "x\n").unwrap();
        let p2 = Project::create(empty.path(), "None").unwrap();
        let mut db2 = Db::open_in_memory().unwrap();
        scan(&p2, &mut db2).unwrap();
        assert_eq!(db2.index_health().unwrap().control, None, "no control page, no control");
    }

    #[test]
    fn office_lock_files_not_indexed() {
        let (dir, project) = temp_project();
        // Word/Excel drop these beside an open document.
        fs::write(dir.path().join("notes/~$meeting.docx"), "office lock junk").unwrap();
        fs::write(dir.path().join("~$budget.xlsx"), "office lock junk").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        let stats = scan(&project, &mut db).unwrap();

        // Same fixture count as a clean scan: the lock files add nothing.
        assert_eq!(stats.added, 11, "stats: {stats:?}");
        assert!(db.get_file("notes/~$meeting.docx").unwrap().is_none());
        assert!(db.get_file("~$budget.xlsx").unwrap().is_none());

        // A watcher event for a lock file must be a no-op through refresh_path.
        assert!(!refresh_path(&project, &mut db, "notes/~$meeting.docx").unwrap());
        assert!(db.get_file("notes/~$meeting.docx").unwrap().is_none());
        drop(dir);
    }

    #[test]
    fn backup_names_are_junk() {
        // Timestamped backups written by external tools next to live documents.
        assert!(is_backup_name("ATT Op Model Dev V2.xlsx.bak-2026-07-16T11-55-35-773Z"));
        assert!(is_backup_name("notes.md.bak"));
        assert!(is_backup_name("NOTES.MD.BAK")); // case-insensitive
        // Words that merely contain "bak" are not backups.
        assert!(!is_backup_name("bakery-roadmap.md"));
        assert!(!is_backup_name("a.baker"));
        assert!(!is_backup_name("bak")); // no dot — not a suffix segment
    }

    #[test]
    fn scan_skips_backup_files() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::create(dir.path(), "Fixture").unwrap();
        std::fs::write(dir.path().join("live.md"), "# keep me").unwrap();
        std::fs::write(dir.path().join("live.md.bak-2026-07-16T11-55-35-773Z"), "old").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        scan(&project, &mut db).unwrap();
        let rels: Vec<String> = db.list_files().unwrap().into_iter().map(|f| f.rel_path).collect();
        assert_eq!(rels, vec!["live.md"]);
    }

    #[test]
    fn refresh_path_handles_add_change_remove() {
        let (dir, project) = temp_project();
        let mut db = Db::open_in_memory().unwrap();
        scan(&project, &mut db).unwrap();

        fs::write(dir.path().join("notes/hot.md"), "hot new note\n").unwrap();
        assert!(refresh_path(&project, &mut db, "notes/hot.md").unwrap());
        assert!(!db.search("hot new note", 5).unwrap().is_empty());

        fs::remove_file(dir.path().join("notes/hot.md")).unwrap();
        assert!(refresh_path(&project, &mut db, "notes/hot.md").unwrap());
        assert!(db.search("hot new note", 5).unwrap().is_empty());
    }

    #[test]
    fn refresh_path_indexes_ken_allowlisted_files_but_not_metadata() {
        // The third entry point into the index: files created through the UI
        // reach it via `refresh_path`, not the walker. It must apply the same
        // `.ken/` allowlist, or a memory written in-app is silently unsearchable
        // while an identical file found by a full scan is not.
        let (dir, project) = temp_project();
        let mut db = Db::open_in_memory().unwrap();
        scan(&project, &mut db).unwrap();

        let memory_dir = dir.path().join(".ken/memory");
        fs::create_dir_all(&memory_dir).unwrap();
        fs::write(memory_dir.join("ways-of-working.md"), "prefers small diffs\n").unwrap();
        assert!(refresh_path(&project, &mut db, ".ken/memory/ways-of-working.md").unwrap());
        assert!(!db.search("prefers small diffs", 5).unwrap().is_empty());

        // Ken's own metadata stays out, exactly as before.
        fs::write(dir.path().join(".ken/index-profile.json"), "{\"kind\":\"code\"}\n").unwrap();
        assert!(!refresh_path(&project, &mut db, ".ken/index-profile.json").unwrap());
        assert!(db.get_file(".ken/index-profile.json").unwrap().is_none());

        // And no other dot-directory becomes reachable.
        let git_dir = dir.path().join(".git");
        fs::create_dir_all(&git_dir).unwrap();
        fs::write(git_dir.join("COMMIT_EDITMSG"), "unique gitmessage token\n").unwrap();
        assert!(!refresh_path(&project, &mut db, ".git/COMMIT_EDITMSG").unwrap());
        assert!(db.search("unique gitmessage token", 5).unwrap().is_empty());
        drop(dir);
    }

    #[test]
    fn video_transcript_indexing_and_candidates() {
        let (dir, project) = temp_project();
        let mut db = Db::open_in_memory().unwrap();

        // A video with an adjacent .vtt: searchable by its transcript, no
        // generation needed.
        fs::create_dir_all(dir.path().join("clips")).unwrap();
        fs::write(dir.path().join("clips/kickoff.mp4"), b"fake mp4 bytes").unwrap();
        fs::write(
            dir.path().join("clips/kickoff.vtt"),
            "WEBVTT\n\n00:00:00.000 --> 00:00:03.000\nmigration kickoff agenda\n",
        )
        .unwrap();
        // A bare video: metadata-only, and flagged for transcription.
        fs::write(dir.path().join("clips/standup.mp4"), b"fake mp4 bytes").unwrap();

        let stats = scan(&project, &mut db).unwrap();

        // The transcript text is searchable and attributed to the video.
        let hits = db.search("migration kickoff agenda", 5).unwrap();
        assert!(
            hits.iter().any(|h| h.rel_path == "clips/kickoff.mp4"),
            "video searchable by transcript: {hits:?}"
        );
        // The transcribed video is indexed; the bare one is metadata-only.
        assert_eq!(db.get_file("clips/kickoff.mp4").unwrap().unwrap().status, STATUS_INDEXED);
        assert_eq!(
            db.get_file("clips/standup.mp4").unwrap().unwrap().status,
            STATUS_METADATA_ONLY
        );
        // Only the transcript-less video is a generation candidate.
        assert!(stats.videos_needing_transcript.contains(&"clips/standup.mp4".to_string()));
        assert!(!stats.videos_needing_transcript.contains(&"clips/kickoff.mp4".to_string()));

        // Convergence: a second scan touches nothing, so the candidate does not
        // reappear on every scan.
        let again = scan(&project, &mut db).unwrap();
        assert!(again.videos_needing_transcript.is_empty(), "{again:?}");
        drop(dir);
    }

    #[test]
    fn reindex_rebuilds_from_scratch() {
        let (_dir, project) = temp_project();
        let mut db = Db::open_in_memory().unwrap();
        scan(&project, &mut db).unwrap();
        // Poison the index, then reindex.
        db.upsert_file("ghost.md", "md", 1, 1, "indexed", None, "ghost content")
            .unwrap();
        // Seed a stale generated-transcript cache; reindex must purge it so
        // regenerated transcripts never reuse it.
        let cache = crate::transcript::cache_dir(&project.root);
        fs::create_dir_all(&cache).unwrap();
        fs::write(cache.join("stale.vtt"), "WEBVTT\n").unwrap();
        assert!(cache.exists());

        let stats = reindex(&project, &mut db).unwrap();
        assert_eq!(stats.added, 11);
        assert!(db.search("ghost content", 5).unwrap().is_empty());
        // From scratch: the on-disk transcript cache is gone.
        assert!(!cache.exists(), "transcript cache dir should be removed");
    }

    #[test]
    fn refresh_path_reflects_a_move() {
        // Mirrors the `move_file` command: rename on disk, then refresh both
        // endpoints so the index follows the file to its new path.
        let (dir, project) = temp_project();
        let mut db = Db::open_in_memory().unwrap();
        scan(&project, &mut db).unwrap();
        assert!(db.get_file("notes/plain.txt").unwrap().is_some());

        fs::rename(
            dir.path().join("notes/plain.txt"),
            dir.path().join("src/plain.txt"),
        )
        .unwrap();
        assert!(refresh_path(&project, &mut db, "notes/plain.txt").unwrap());
        assert!(refresh_path(&project, &mut db, "src/plain.txt").unwrap());

        assert!(db.get_file("notes/plain.txt").unwrap().is_none());
        assert!(db.get_file("src/plain.txt").unwrap().is_some());

        let hits = db.search("rollback rehearsal", 5).unwrap();
        assert!(
            hits.iter().any(|h| h.rel_path == "src/plain.txt"),
            "moved file searchable at new path: {hits:?}",
        );
        assert!(
            !hits.iter().any(|h| h.rel_path == "notes/plain.txt"),
            "old path gone from index: {hits:?}",
        );
    }
    fn indexed(db: &Db) -> Vec<(String, i64)> {
        let mut v: Vec<(String, i64)> = db.list_files().unwrap().into_iter().map(|f| (f.rel_path, f.size)).collect();
        v.sort();
        v
    }

    /// A scoped rescan of the paths a watcher named leaves the index as a
    /// full scan would, and hands a folder back to the full scan.
    #[test]
    fn a_scoped_rescan_does_what_a_full_scan_does_to_those_paths() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".gitignore"), "build/\n").unwrap();
        fs::create_dir_all(root.join("notes")).unwrap();
        fs::write(root.join("a.md"), "# A\n").unwrap();
        fs::write(root.join("b.md"), "# B\n").unwrap();
        fs::write(root.join("notes/one.txt"), "one").unwrap();
        fs::write(root.join("notes/two.txt"), "two").unwrap();
        let project = Project::create(root, "Scoped").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        scan(&project, &mut db).unwrap();

        // An edit, a new file, a deletion, a gitignored build file, a gone folder.
        fs::write(root.join("a.md"), "# A\n\nMore about A.\n").unwrap();
        fs::write(root.join("c.md"), "# C\n").unwrap();
        fs::remove_file(root.join("b.md")).unwrap();
        fs::create_dir_all(root.join("build")).unwrap();
        fs::write(root.join("build/out.txt"), "generated").unwrap();
        fs::remove_dir_all(root.join("notes")).unwrap();
        let rels: Vec<String> = ["a.md", "b.md", "c.md", "build/out.txt", "notes"].iter().map(|s| s.to_string()).collect();
        let stats = scan_paths(&project, &mut db, &rels).unwrap().expect("no folder on disk among them");
        assert_eq!((stats.added, stats.updated, stats.removed), (1, 1, 3), "{stats:?}");
        let scoped = indexed(&db);

        let mut full = Db::open_in_memory().unwrap();
        scan(&project, &mut full).unwrap();
        assert_eq!(scoped, indexed(&full), "the same index as a full scan");
        assert!(!scoped.iter().any(|(p, _)| p.starts_with("build/")), "gitignored, as the walk leaves it");
        assert!(db.search("More about", 5).unwrap().iter().any(|h| h.rel_path == "a.md"));

        // A folder among the paths: a full scan's job.
        fs::create_dir_all(root.join("moved-in")).unwrap();
        fs::write(root.join("moved-in/x.md"), "# X\n").unwrap();
        assert!(scan_paths(&project, &mut db, &["moved-in".to_string()]).unwrap().is_none());
    }
}

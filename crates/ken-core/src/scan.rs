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
///
/// A file that failed for any other reason is tried again by the next scans
/// until it has failed [`crate::db::MAX_READ_ATTEMPTS`] times, and is then
/// counted as failed and left alone (knowledge-core decision 34).
fn needs_retry(status: &str, error: Option<&str>, dataless_now: bool, attempts: i64) -> bool {
    match status {
        // We skipped its bytes; they've since arrived (the user opened it, or
        // the provider synced it down). Extract the content now.
        STATUS_CLOUD_ONLY => !dataless_now,
        // A timed-out read of a file that is still in the cloud: re-index it
        // as cloud_only. Costs nothing (we no longer read it) and converges —
        // next scan sees cloud_only + dataless and leaves it alone. A file
        // that failed for any other reason is left alone, so a corrupt one
        // isn't re-extracted on every scan.
        STATUS_FAILED => (dataless_now && error.is_some_and(is_transient_error)) || attempts < crate::db::MAX_READ_ATTEMPTS,
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

    // On first sight, the older ignore files move into the knowledge base's
    // `.ken/knowledge.json` (one commit there) and are not read after.
    if let Err(e) = crate::knowledge_settings::move_legacy_ignores(project) {
        eprintln!("ignore: could not move the older ignore rules of {}: {e}", project.root.display());
    }

    // The index rules (knowledge-core decision 34), loaded once per scan:
    // the fixed built-ins, the repo switched off, the older `.kenignore`
    // files until they move, then `.ken/knowledge.json`. The built-ins leave
    // tests out (decision 17), so a file indexed before and ignored now is
    // removed below like any other.
    let rules = project.index_rules();
    let excluded = project.legacy_excluded();

    // What's on disk (rel_path -> size, mtime, cloud placeholder?)
    let mut on_disk: HashMap<String, (i64, i64, bool)> = HashMap::new();
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
    // move it out from under the pattern. The index rules remain able to
    // exclude further, which is what they are overwhelmingly used for.
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
        if crate::project::in_folders(&excluded, &rel_str) {
            continue;
        }
        // A path that is not indexed never produces a row, exactly like
        // `excluded` above.
        if crate::kenignore::classify(&rel_str, false, &[&rules]) == crate::kenignore::Tier::Ignored {
            continue;
        }
        if let Some(meta) = listed.or_else(|| path.metadata().ok()) {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            on_disk.insert(rel_str, (meta.len() as i64, mtime, cloud::is_dataless(&meta)));
        }
    }

    // What's in the index, with how often each failed file was tried.
    let attempts = db.read_attempts()?;
    let indexed: Indexed = db
        .list_files()?
        .into_iter()
        .map(|f| {
            let tries = attempts.get(&f.rel_path).copied().unwrap_or(0);
            (f.rel_path, (f.size, f.mtime, f.status, f.error, tries))
        })
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

    // Each file's authority in search, from the index's frontmatter and the
    // knowledge base's `.ken/knowledge.json`, for every file: a new tier
    // override there reaches them all.
    crate::authority::refresh(db, &project.root, None)?;
    // Each code file's role, from the repo's role rules and the built-in
    // core, for every file: a new rule reaches them all (decision 17).
    crate::coderole::refresh(db, &project.root)?;

    // The team's words for search, rebuilt when a page may have changed.
    if !stats.changed_paths.is_empty() || db.stored_vocabulary()?.is_none() {
        crate::vocab::Vocabulary::rebuild(db)?;
    }

    // The controls, by keyword, while this source has no meaning index:
    // that is how it is searched until one is built, and the rebuild runs
    // them with meaning after (decision 19). They run again when the files
    // or the controls changed, or none ran yet.
    if db.semantic_built_at()?.is_none() {
        let controls = crate::controls::for_source(&project.root);
        let due = !stats.changed_paths.is_empty()
            || db.control_results()?.is_none()
            || !crate::controls::same_set(&crate::controls::stored(db), &controls);
        if due {
            if let Err(e) = crate::controls::run_and_store(db, &controls, &mut |_| None) {
                eprintln!("controls: {e}");
            }
        }
    }

    after_scan(project, db)?;
    Ok(stats)
}

/// What every scan ends with: the supersessions the decisions log marks
/// itself, stored for search (knowledge-core decision 25), and in a
/// knowledge base the lint that needs no model (decision 29), its findings
/// filed and the fixed ones resolved with a `log.md` line when it did
/// either (decision 2), and the project graph's edges that
/// the knowledge base's own index holds (decision 21).
fn after_scan(project: &Project, db: &mut Db) -> Result<()> {
    crate::supersede::store_marked(db, &crate::authority::Roles::of_repo(&project.root))?;
    if crate::registry::is_library(&project.root) {
        // The lint's `log.md` line carries the UTC date: a scan has no local
        // clock to read (the app's runs pass their local date).
        let now = crate::engine::now_epoch();
        if let Err(e) = crate::lint::on_scan(db, &project.root, now, &crate::day::date_of_epoch(now)) {
            eprintln!("lint: {e}");
        }
        if let Err(e) = crate::graph::on_scan(db, &project.root) {
            eprintln!("graph: {e}");
        }
    }
    Ok(())
}

/// Files written per transaction during a scan.
const BATCH_FILES: usize = 500;

type OnDisk = HashMap<String, (i64, i64, bool)>;
/// rel_path -> size, mtime, status, error, failed tries.
type Indexed = HashMap<String, (i64, i64, String, Option<String>, i64)>;

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
    for (rel, (size, mtime, dataless)) in on_disk {
        match indexed.get(rel) {
            Some((s, m, status, error, tries)) if s == size
                && m == mtime
                && !needs_retry(status, error.as_deref(), *dataless, *tries) =>
            {
                stats.unchanged += 1;
                continue;
            }
            Some((s, _, status, error, tries))
                if s == size
                    && !*dataless
                    && !needs_retry(status, error.as_deref(), *dataless, *tries)
                    && only_mtime_moved(db, rel, &project.root.join(rel), *size, *mtime)? =>
            {
                stats.unchanged += 1;
                continue;
            }
            Some(_) => stats.updated += 1,
            None => stats.added += 1,
        }
        let status = index_one(project, db, rel, *size, *mtime, *dataless)?;
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

/// Whether `text` is minified: a line over 2,000 characters with almost no
/// spaces in it. Only code and data are checked; a page's long paragraph is
/// prose, however long its lines.
pub fn minified(text: &str) -> bool {
    text.lines().any(|l| {
        let n = l.chars().count();
        n > 2000 && (l.chars().filter(|c| c.is_whitespace()).count() as f64) < 0.03 * n as f64
    })
}

/// The chunks of a file with no readable text: one, holding its path.
fn name_only(rel: &str) -> crate::chunker::Chunked {
    crate::chunker::Chunked { chunks: vec![crate::chunker::name_chunk(rel)], capped: false }
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
) -> Result<&'static str> {
    let abs = project.root.join(rel);
    let kind = FileKind::from_path(&abs);
    // Its bytes are still in the cloud. Reading them here would force a
    // blocking download (and usually time out), so record the file by name
    // and leave the content to `cloud::hydrate` when the user opens it.
    if dataless {
        db.upsert_file(rel, kind.as_str(), size, mtime, STATUS_CLOUD_ONLY, None, "")?;
        db.store_chunked(rel, &name_only(rel))?;
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
        // Minified code or data (a bundle, a one-line dump) matches many
        // searches a little and answers none: searchable by name only.
        Ok(out) if kind == FileKind::Code && minified(&out.text) => (STATUS_METADATA_ONLY, None, out.text),
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
    // The code index: definitions, references and imports, for "find usages"
    // and "go to definition", for every indexed file: it is what a code
    // repo is searched for. Made before the chunks, which
    // cut code at its definitions.
    let map = crate::codeindex::Lang::of(rel).map(|_| {
        if status == STATUS_INDEXED { crate::codeindex::map_file(rel, &text).unwrap_or_default() } else { Default::default() }
    });
    // Chunks, for keyword search over sections and `repo:path:line`
    // citations. They need no model, so every indexed file has them; the
    // semantic rebuild only adds vectors. A file with no readable text has
    // one chunk holding its path, so it is still found by name.
    let chunked = if status == STATUS_INDEXED && !text.trim().is_empty() {
        let defs = map.as_ref().map_or(&[][..], |m| &m.symbols[..]);
        crate::chunker::chunk_with(rel, &text, &crate::chunker::IndexProfile::default_for(rel), defs)
    } else {
        name_only(rel)
    };
    db.store_chunked(rel, &chunked)?;
    // What a page says about itself (title, aliases, status, checked),
    // read from its frontmatter for search to rank and show.
    if kind == FileKind::Md {
        let meta = (status == STATUS_INDEXED).then(|| crate::pagemeta::parse(&text)).flatten();
        db.set_page_meta(rel, meta.as_ref())?;
        let links = if status == STATUS_INDEXED { crate::links::extract(rel, &text) } else { Vec::new() };
        db.set_page_links(rel, &links)?;
    }
    if let Some(map) = &map {
        db.set_code_index(rel, map)?;
    }
    // OCR enqueue (Phase 2): images and low-text (scanned) PDFs get queued for
    // a background OCR pass so their pixels become searchable text. Never run
    // OCR inline here: it reads bytes and hits Vision, which
    // would stall the scan. Keyed by a cheap per-version hash so an unchanged
    // rescan (which never re-runs `index_one`) never re-OCRs, and a real change
    // (new size/mtime → new hash) re-queues. Only reached when the file was read
    // (not a cloud placeholder, which returned early above). Nothing is queued
    // where the OS has no OCR engine: marking such files done unread would keep
    // them unread once one exists.
    if status != STATUS_FAILED && crate::ocr::available() {
        let ocr_hash = crate::db::content_hash(&format!("{size}:{mtime}"));
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
    let excluded = crate::project::in_folders(&project.legacy_excluded(), rel)
        || is_hidden_rel(rel)
        || rel.rsplit('/').next().is_some_and(|n| is_office_lock_name(n) || is_backup_name(n))
        || inside_linked_checkout(&project.root, rel);
    // A path that is not indexed is treated exactly like `excluded`: no row.
    let rules = project.index_rules();
    let excluded = excluded || crate::kenignore::classify(rel, false, &[&rules]) == crate::kenignore::Tier::Ignored;
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
            return Ok(false);
        }
        index_one(project, db, rel, meta.len() as i64, mtime, dataless)?;
        crate::authority::refresh(db, &project.root, Some(&[rel.to_string()]))?;
        let role = crate::coderole::RoleRules::for_repo(&project.root).role_of(rel);
        db.set_file_roles(&[(rel.to_string(), role)])?;
        // The Vocabulary page or the decisions log may have changed.
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
    rules: &[crate::kenignore::Rule],
    legacy_excluded: &[String],
    git: &mut GitIgnores,
) -> Result<Refreshed> {
    let abs = project.root.join(rel);
    let excluded = crate::project::in_folders(legacy_excluded, rel)
        || is_hidden_rel(rel)
        || rel.rsplit('/').next().is_some_and(|n| is_office_lock_name(n) || is_backup_name(n))
        || inside_linked_checkout(&project.root, rel)
        || git.ignored(rel);
    let excluded = excluded || crate::kenignore::classify(rel, false, &[rules]) == crate::kenignore::Tier::Ignored;
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
            // A watcher event on an unchanged file is no reason to try a
            // failed read again; the next full scan counts the tries.
            if f.size == meta.len() as i64
                && f.mtime == mtime
                && !needs_retry(&f.status, f.error.as_deref(), dataless, crate::db::MAX_READ_ATTEMPTS)
            {
                return Ok(Refreshed::Unchanged);
            }
        }
        if !dataless && only_mtime_moved(db, rel, &abs, meta.len() as i64, mtime)? {
            return Ok(Refreshed::Unchanged);
        }
        let status = index_one(project, db, rel, meta.len() as i64, mtime, dataless)?;
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
/// (the vocabulary, the link report) when a page moved.
/// `None` when a full scan is the right tool: too many paths, or a folder
/// among them (a folder moved in brings files no event named).
pub fn scan_paths(project: &Project, db: &mut Db, rels: &[String]) -> Result<Option<ScanStats>> {
    if crate::registry::awaits_kind(&project.root) {
        return Ok(Some(ScanStats::default()));
    }
    if rels.len() > SCOPED_MAX || rels.iter().any(|r| project.root.join(r).is_dir()) {
        return Ok(None);
    }
    let rules = project.index_rules();
    let legacy_excluded = project.legacy_excluded();
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
            match refresh_one(project, db, rel, &rules, &legacy_excluded, &mut git)? {
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
        let rules = crate::coderole::RoleRules::for_repo(&project.root);
        let roles: Vec<(String, Option<String>)> = stats.changed_paths.iter().map(|p| (p.clone(), rules.role_of(p))).collect();
        db.set_file_roles(&roles)?;
    }
    if pages_moved || db.stored_vocabulary()?.is_none() {
        crate::vocab::Vocabulary::rebuild(db)?;
    }
    if pages_moved {
        after_scan(project, db)?;
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
/// (files, contents, OCR) and the cache dir removed before the fresh scan repopulates the index.
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
    let stats = scan(project, db)?;
    // `clear` keeps the chunks, so the vectors of unchanged files survive a
    // reindex; the chunks of files that are gone go now.
    db.drop_orphan_chunks()?;
    Ok(stats)
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
    /// Whether a keyword search for `q` finds `path` in the chunk index.
    fn finds(db: &Db, q: &str, path: &str) -> bool {
        db.search_chunks_fts(q, 10).unwrap().iter().any(|h| h.path == path)
    }

    fn temp_project() -> (tempfile::TempDir, Project) {
        let dir = tempfile::tempdir().unwrap();
        copy_dir(&fixture_root(), dir.path());
        let project = Project::create(dir.path(), "Fixture").unwrap();
        (dir, project)
    }

    /// The fixture's corrupt PDF at its last try, as three scans leave it
    /// (decision 34), so a rescan passes it by like any unchanged file.
    fn corrupt_pdf_tried_out(db: &mut Db) {
        let rel = "suppliers/corrupt.pdf";
        let row = db.get_file(rel).unwrap().unwrap();
        while db.read_attempts().unwrap().get(rel).copied().unwrap_or(0) < crate::db::MAX_READ_ATTEMPTS {
            db.upsert_file(rel, &row.kind, row.size, row.mtime, STATUS_FAILED, row.error.as_deref(), "").unwrap();
        }
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
        corrupt_pdf_tried_out(&mut db);
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
    fn images_and_sparse_pdfs_enqueue_ocr_but_text_pdfs_and_svg_do_not() {
        let (dir, project) = temp_project();
        let mut db = Db::open_in_memory().unwrap();

        // An image (no EXIF text): enqueued for OCR.
        fs::write(project.root.join("photo.png"), b"not really a png").unwrap();
        let meta = project.root.join("photo.png").metadata().unwrap();
        index_one(&project, &mut db, "photo.png", meta.len() as i64, 5, false).unwrap();
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
        index_one(&project, &mut db, "logo.svg", m.len() as i64, 5, false).unwrap();
        assert!(db.next_pending_ocr().unwrap().is_none(), "SVG must not enqueue OCR");

        // The fixture PDF has only a one-line text layer (sparse) — exactly the
        // scanned/image case the heuristic catches — so it IS queued for OCR.
        // (A genuine multi-paragraph text PDF clears the threshold; that path is
        // covered directly by `pdf_low_text_heuristic`.)
        let pdf = project.root.join("suppliers/contract.pdf");
        let pm = pdf.metadata().unwrap();
        index_one(&project, &mut db, "suppliers/contract.pdf", pm.len() as i64, 5, false).unwrap();
        assert_eq!(
            db.next_pending_ocr().unwrap().map(|(r, _)| r),
            Some("suppliers/contract.pdf".to_string()),
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
        index_one(&project, &mut db, "photo.png", meta.len() as i64, 5, false).unwrap();
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

        index_one(&project, &mut db, "photo.png", size, mtime, false).unwrap();
        let (rel, hash) = db.next_pending_ocr().unwrap().unwrap();
        db.mark_ocr_done(&rel, &hash, &[]).unwrap();

        // Same (size, mtime): re-indexing does NOT re-OCR.
        index_one(&project, &mut db, "photo.png", size, mtime, false).unwrap();
        assert!(db.next_pending_ocr().unwrap().is_none(), "unchanged image must not re-OCR");

        // A changed mtime (new version) re-queues it.
        index_one(&project, &mut db, "photo.png", size, mtime + 1, false).unwrap();
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
            let hits = db.search_chunks_fts(q, 10).unwrap();
            assert!(
                hits.iter().any(|h| h.path.contains(path_frag)),
                "query {q:?} should hit {path_frag}: {hits:?}"
            );
        }

        // Failed and binary files are listed, with no text to search.
        assert_eq!(db.get_file("suppliers/corrupt.pdf").unwrap().unwrap().status, STATUS_FAILED);
        assert!(db.get_file("images/team-photo.png").unwrap().is_some());
    }

    /// A OneDrive/iCloud placeholder must never be read during indexing —
    /// reading it forces a multi-second on-demand download that stalls the
    /// scan and often dies with ETIMEDOUT. Index the name, skip the bytes.
    #[test]
    fn cloud_only_file_is_indexed_without_reading_its_bytes() {
        let (_dir, project) = temp_project();
        let mut db = Db::open_in_memory().unwrap();

        let status = index_one(&project, &mut db, "notes/meeting.md", 120, 7, true).unwrap();

        assert_eq!(status, STATUS_CLOUD_ONLY);
        let row = db.get_file("notes/meeting.md").unwrap().unwrap();
        assert_eq!(row.status, STATUS_CLOUD_ONLY);
        assert_eq!(row.error, None);
        // Listed, but its content was deliberately not extracted.
        assert_eq!(db.get_text("notes/meeting.md").unwrap().as_deref(), Some(""));
        assert!(!finds(&db, "billing cutover", "notes/meeting.md"));
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
        corrupt_pdf_tried_out(&mut db);

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
        assert!(finds(&db, "billing cutover", "notes/meeting.md"));
    }

    /// The retry rule has to converge: re-index exactly the rows whose content
    /// could now be read, and leave every other row alone. Anything else means
    /// either content that never appears, or a rescan that re-extracts files
    /// forever (and re-triggers the ingest recipes watching them).
    #[test]
    fn retry_rule_covers_only_rows_whose_content_could_have_changed() {
        let timeout = Some("extraction failed: Operation timed out (os error 60)");

        let tried_out = crate::db::MAX_READ_ATTEMPTS;
        // Still in the cloud: nothing new to read, so don't touch it.
        assert!(!needs_retry(STATUS_CLOUD_ONLY, None, true, 0));
        // Bytes have landed: extract the content.
        assert!(needs_retry(STATUS_CLOUD_ONLY, None, false, 0));

        // A timed-out read of a file that's still a placeholder — a row left
        // by the old behaviour. Re-index it (cheaply) as cloud_only.
        assert!(needs_retry(STATUS_FAILED, timeout, true, tried_out));
        // …and once it is cloud_only, the scan above leaves it alone: converges.

        // A corrupt local file is tried again until its third failure
        // (decision 34), then left alone and counted as failed.
        let corrupt = Some("PDF error: invalid xref");
        assert!(needs_retry(STATUS_FAILED, corrupt, false, 1));
        assert!(needs_retry(STATUS_FAILED, corrupt, false, tried_out - 1));
        assert!(!needs_retry(STATUS_FAILED, corrupt, false, tried_out));

        // Healthy rows are never redundant work.
        assert!(!needs_retry(STATUS_INDEXED, None, false, 0));
        assert!(!needs_retry(STATUS_METADATA_ONLY, None, false, 0));
    }

    #[test]
    fn rescan_is_incremental() {
        let (dir, project) = temp_project();
        let mut db = Db::open_in_memory().unwrap();
        scan(&project, &mut db).unwrap();
        corrupt_pdf_tried_out(&mut db);

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

        assert!(finds(&db, "entirely new fact", "notes/plain.txt"));
        assert!(finds(&db, "fresh doc", "notes/new.md"));
        assert!(!finds(&db, "vendor pricing", "src/example.rs"));
    }

    #[test]
    fn exclusions_apply_on_scan_and_rescan() {
        let (_dir, mut project) = temp_project();
        let mut db = Db::open_in_memory().unwrap();
        scan(&project, &mut db).unwrap();
        assert!(finds(&db, "obsolete cutover plan", "archive/old-notes.md"));

        project.set_excluded(vec!["archive".into()]).unwrap();
        let stats = scan(&project, &mut db).unwrap();
        assert_eq!(stats.removed, 1, "{stats:?}");
        assert!(!finds(&db, "obsolete cutover plan", "archive/old-notes.md"));

        project.set_excluded(vec![]).unwrap();
        let stats = scan(&project, &mut db).unwrap();
        assert_eq!(stats.added, 1, "{stats:?}");
        assert!(finds(&db, "obsolete cutover plan", "archive/old-notes.md"));
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

    /// A folder a `.kenignore` line drops leaves the code index with its files,
    /// so find-usages never points into it; unignored, it is mapped again.
    #[test]
    fn an_ignored_folder_leaves_the_code_index() {
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
        fs::write(&page, "---\ntitle: Rules\naliases: [\"binding rules\"]\nchecked: 2026-09-20 f00218c\nreviewed_by: Chris\n---\n# Rules\n").unwrap();
        let project = Project::create(dir.path(), "Meta").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        scan(&project, &mut db).unwrap();
        let meta = db.page_meta("Rules.md").unwrap().unwrap();
        assert_eq!(meta.aliases, vec!["binding rules"]);
        assert_eq!(meta.checked.as_ref().map(|c| c.to_string()).as_deref(), Some("2026-09-20 f00218c"));
        assert_eq!(meta.reviewed_by.as_deref(), Some("Chris"));

        // A page indexed before frontmatter was read gets it on open.
        db.set_page_meta("Rules.md", None).unwrap();
        assert_eq!(db.backfill_page_meta().unwrap(), 1);
        assert!(db.page_meta("Rules.md").unwrap().is_some());

        fs::write(&page, "# Rules, no frontmatter now, and longer\n").unwrap();
        scan(&project, &mut db).unwrap();
        assert_eq!(db.page_meta("Rules.md").unwrap(), None);
    }

    /// A file with no readable text (here minified, then empty) is found by
    /// its name through one chunk holding its path; the chunk goes when the
    /// file gains text, and with the file.
    #[test]
    fn a_file_with_no_text_is_found_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = format!("var a=1;{}\n", "x=y+1;".repeat(600));
        fs::write(dir.path().join("bundle.js"), &bundle).unwrap();
        fs::write(dir.path().join("empty.txt"), "").unwrap();
        let project = Project::create(dir.path(), "Names").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        scan(&project, &mut db).unwrap();
        let texts = |db: &Db, p: &str| db.chunks_of(p).unwrap().into_iter().map(|(_, _, t)| t).collect::<Vec<_>>();
        assert_eq!(texts(&db, "bundle.js"), vec!["bundle.js"]);
        assert_eq!(texts(&db, "empty.txt"), vec!["empty.txt"]);
        let hits = crate::routing::search_member(&db, "bundle", None, 10).unwrap();
        assert!(hits.iter().any(|h| h.path == "bundle.js"), "found by name: {hits:?}");

        fs::write(dir.path().join("empty.txt"), "Now it says something about regions.\n").unwrap();
        scan(&project, &mut db).unwrap();
        let now = texts(&db, "empty.txt");
        assert!(!now.is_empty() && now.iter().all(|t| t != "empty.txt"), "{now:?}");

        fs::remove_file(dir.path().join("bundle.js")).unwrap();
        scan(&project, &mut db).unwrap();
        assert!(texts(&db, "bundle.js").is_empty());
    }

    #[test]
    fn index_health_counts_what_the_scan_read_and_runs_the_controls() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("START-HERE.md"), "---\ntitle: Start here\n---\n# Start here\nRead this first.\n").unwrap();
        fs::write(dir.path().join("notes.md"), "Notes that mention start once.\n").unwrap();
        fs::create_dir_all(dir.path().join("src/services")).unwrap();
        fs::write(dir.path().join("src/services/pay.rs"), "fn pay() {}\n").unwrap();
        fs::create_dir_all(dir.path().join("tests")).unwrap();
        fs::write(dir.path().join("tests/pay_test.rs"), "fn it_pays() {}\n").unwrap();
        fs::create_dir_all(dir.path().join(".ken")).unwrap();
        fs::write(
            dir.path().join(".ken/knowledge.json"),
            r#"{ "controls": [ { "question": "read this first", "expect": "START-HERE.md" } ] }"#,
        )
        .unwrap();
        let project = Project::create(dir.path(), "Health").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        scan(&project, &mut db).unwrap();

        let h = db.index_health().unwrap();
        assert_eq!((h.files, h.indexed, h.failed), (3, 3, 0), "tests are not indexed");
        assert!(db.get_file("tests/pay_test.rs").unwrap().is_none());
        assert_eq!(db.file_role("src/services/pay.rs").unwrap().as_deref(), Some("service"));
        assert_eq!((h.controls_passed, h.controls_total), (1, 1), "run by keyword while there is no meaning index");
    }

    /// A file cut at the per-file chunk cap is counted in index health, and
    /// code with a grammar is chunked by definition as it is indexed.
    #[test]
    fn the_chunk_cap_is_counted_and_code_is_cut_by_definition() {
        let dir = tempfile::tempdir().unwrap();
        // No grammar: line blocks of about 2,000 bytes, far past 200 of them.
        let big: String = (0..9000).map(|i| format!("key_{i}: a value long enough to pad the line out to sixty\n")).collect();
        fs::write(dir.path().join("big.yaml"), big).unwrap();
        fs::write(dir.path().join("small.yaml"), "a: 1\n").unwrap();
        fs::write(dir.path().join("store.py"), "def open_store():\n    return 1\n\n\nclass Ledger:\n    def reconcile(self):\n        return 2\n").unwrap();
        let project = Project::create(dir.path(), "Cap").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        scan(&project, &mut db).unwrap();
        assert_eq!(db.index_health().unwrap().capped, 1);
        // What is embedded starts with the path, the symbols and the role.
        // Two small neighbours share a chunk, which names them both.
        let (_, embed) = db.chunks_missing_vectors("store.py").unwrap().remove(0);
        assert!(embed.starts_with("store.py › open_store, Ledger (store)"), "the chunk names what it holds: {embed}");
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
        assert!(finds(&db, "hot new note", "notes/hot.md"));

        fs::remove_file(dir.path().join("notes/hot.md")).unwrap();
        assert!(refresh_path(&project, &mut db, "notes/hot.md").unwrap());
        assert!(!finds(&db, "hot new note", "notes/hot.md"));
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
        assert!(finds(&db, "prefers small diffs", ".ken/memory/ways-of-working.md"));

        // Ken's own metadata stays out, exactly as before.
        fs::write(dir.path().join(".ken/index-profile.json"), "{\"kind\":\"code\"}\n").unwrap();
        assert!(!refresh_path(&project, &mut db, ".ken/index-profile.json").unwrap());
        assert!(db.get_file(".ken/index-profile.json").unwrap().is_none());

        // And no other dot-directory becomes reachable.
        let git_dir = dir.path().join(".git");
        fs::create_dir_all(&git_dir).unwrap();
        fs::write(git_dir.join("COMMIT_EDITMSG"), "unique gitmessage token\n").unwrap();
        assert!(!refresh_path(&project, &mut db, ".git/COMMIT_EDITMSG").unwrap());
        assert!(!finds(&db, "unique gitmessage token", ".git/COMMIT_EDITMSG"));
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
        assert!(finds(&db, "migration kickoff agenda", "clips/kickoff.mp4"), "video searchable by transcript");
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
        let chunks = crate::chunker::chunk_file("ghost.md", "ghost content", &crate::chunker::IndexProfile::default_for("ghost.md"));
        db.upsert_chunks("ghost.md", &chunks).unwrap();
        // Seed a stale generated-transcript cache; reindex must purge it so
        // regenerated transcripts never reuse it.
        let cache = crate::transcript::cache_dir(&project.root);
        fs::create_dir_all(&cache).unwrap();
        fs::write(cache.join("stale.vtt"), "WEBVTT\n").unwrap();
        assert!(cache.exists());

        let stats = reindex(&project, &mut db).unwrap();
        assert_eq!(stats.added, 11);
        assert!(db.get_file("ghost.md").unwrap().is_none());
        assert!(!finds(&db, "ghost content", "ghost.md"), "its chunks went too");
        assert!(finds(&db, "billing cutover", "notes/meeting.md"), "the rest are searchable again");
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

        assert!(finds(&db, "rollback rehearsal", "src/plain.txt"), "moved file searchable at new path");
        assert!(!finds(&db, "rollback rehearsal", "notes/plain.txt"), "old path gone from index");
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
        assert!(finds(&db, "More about", "a.md"));

        // A folder among the paths: a full scan's job.
        fs::create_dir_all(root.join("moved-in")).unwrap();
        fs::write(root.join("moved-in/x.md"), "# X\n").unwrap();
        assert!(scan_paths(&project, &mut db, &["moved-in".to_string()]).unwrap().is_none());
    }

    #[test]
    fn minified_code_is_found_and_prose_is_not() {
        // A minifier drops even the space after `var` (`var a=1;` is one
        // whitespace in eight, over the 3%).
        let bundle = format!("!function(){{{}}}();", "a=1;b=2;".repeat(400));
        assert!(minified(&bundle));
        assert!(minified(&format!("[{}]", "{\"id\":1,\"name\":\"x\"},".repeat(200))));
        assert!(!minified("fn save() {\n    write();\n}\n"));
        assert!(!minified(&"A person wrote this sentence in one long paragraph. ".repeat(60)), "a long line of prose has spaces");
    }
}

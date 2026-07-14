//! Scanner: reconcile the index with the folder. Used for the initial scan,
//! watcher batches, exclusion changes, and full reindex — one code path.

use std::collections::HashMap;
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

/// Machine-generated folders no knowledge project wants indexed. (Hidden
/// folders are already skipped.)
pub const JUNK_DIRS: &[&str] = &["node_modules", "target", "dist", "build", "__pycache__", "venv"];

pub fn is_junk_dir_name(name: &str) -> bool {
    JUNK_DIRS.contains(&name)
}

/// Microsoft Office writes transient lock files named `~$<document>` beside
/// any open document. They carry no content and churn constantly — ignore
/// them everywhere (initial scan, refresh, watcher) so they never enter the
/// index or trigger rescans.
pub fn is_office_lock_name(name: &str) -> bool {
    name.starts_with("~$")
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

    // What's on disk (rel_path -> size, mtime, cloud placeholder?)
    let mut on_disk: HashMap<String, (i64, i64, bool)> = HashMap::new();
    let walker = ignore::WalkBuilder::new(&project.root)
        .hidden(true) // skip dotfiles: .git, .ken, .DS_Store…
        .git_ignore(false) // knowledge folders aren't code repos
        .git_global(false)
        .git_exclude(false)
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            name != ".ken"
                && !is_office_lock_name(&name)
                && !(e.path().is_dir() && is_junk_dir_name(&name))
        })
        .build();
    for entry in walker.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Ok(rel) = path.strip_prefix(&project.root) else {
            continue;
        };
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if project.is_excluded(&rel_str) {
            continue;
        }
        if let Ok(meta) = path.metadata() {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            on_disk.insert(rel_str, (meta.len() as i64, mtime, cloud::is_dataless(&meta)));
        }
    }

    // What's in the index
    let indexed: HashMap<String, (i64, i64, String, Option<String>)> = db
        .list_files()?
        .into_iter()
        .map(|f| (f.rel_path, (f.size, f.mtime, f.status, f.error)))
        .collect();

    // Removals: indexed but gone from disk (or newly excluded)
    for rel in indexed.keys() {
        if !on_disk.contains_key(rel) {
            db.remove_file(rel)?;
            stats.removed += 1;
            stats.changed_paths.push(rel.clone());
        }
    }

    // Adds/updates
    for (rel, (size, mtime, dataless)) in &on_disk {
        match indexed.get(rel) {
            Some((s, m, status, error)) if s == size
                && m == mtime
                && !needs_retry(status, error.as_deref(), *dataless) =>
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
    }

    Ok(stats)
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
        return Ok(STATUS_CLOUD_ONLY);
    }
    let (status, error, text) = match extract::extract(&abs) {
        // A video whose transcript hasn't been resolved yet is searchable by
        // name only — metadata-only, not an empty-content "indexed" row.
        Ok(out) if kind == FileKind::Video && out.text.trim().is_empty() => {
            (STATUS_METADATA_ONLY, None, out.text)
        }
        Ok(out) if kind.has_content() => (STATUS_INDEXED, None, out.text),
        Ok(out) => (STATUS_METADATA_ONLY, None, out.text),
        Err(e) => (STATUS_FAILED, Some(e.to_string()), String::new()),
    };
    db.upsert_file(rel, kind.as_str(), size, mtime, status, error.as_deref(), &text)?;
    Ok(status)
}

/// Re-index one path in response to a watcher event: index if it exists and
/// is included, remove otherwise. Returns true if the index changed.
pub fn refresh_path(project: &Project, db: &mut Db, rel: &str) -> Result<bool> {
    let abs = project.root.join(rel);
    let excluded = project.is_excluded(rel)
        || is_hidden_rel(rel)
        || rel.rsplit('/').next().is_some_and(is_office_lock_name);
    if !excluded && abs.is_file() {
        let meta = abs.metadata().map_err(|e| crate::Error::io(&abs, e))?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let dataless = cloud::is_dataless(&meta);
        index_one(project, db, rel, meta.len() as i64, mtime, dataless)?;
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

fn is_hidden_rel(rel: &str) -> bool {
    rel.split('/').any(|part| part.starts_with('.'))
}

/// Full rebuild: drop everything and rescan.
pub fn reindex(project: &Project, db: &mut Db) -> Result<ScanStats> {
    db.clear()?;
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

        let status = index_one(&project, &mut db, "notes/meeting.md", 120, 7, true).unwrap();

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
        let stats = reindex(&project, &mut db).unwrap();
        assert_eq!(stats.added, 11);
        assert!(db.search("ghost content", 5).unwrap().is_empty());
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
}

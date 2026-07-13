//! Scanner: reconcile the index with the folder. Used for the initial scan,
//! watcher batches, exclusion changes, and full reindex — one code path.

use std::collections::HashMap;
use std::time::UNIX_EPOCH;

use serde::Serialize;

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

/// Walk the project and bring the index to match the folder exactly.
pub fn scan(project: &Project, db: &mut Db) -> Result<ScanStats> {
    let mut stats = ScanStats::default();

    // What's on disk (rel_path -> size, mtime)
    let mut on_disk: HashMap<String, (i64, i64)> = HashMap::new();
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
            on_disk.insert(rel_str, (meta.len() as i64, mtime));
        }
    }

    // What's in the index
    let indexed: HashMap<String, (i64, i64)> = db
        .list_files()?
        .into_iter()
        .map(|f| (f.rel_path, (f.size, f.mtime)))
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
    for (rel, (size, mtime)) in &on_disk {
        match indexed.get(rel) {
            Some(&(s, m)) if s == *size && m == *mtime => {
                stats.unchanged += 1;
                continue;
            }
            Some(_) => stats.updated += 1,
            None => stats.added += 1,
        }
        if index_one(project, db, rel, *size, *mtime)? == STATUS_FAILED {
            stats.failed += 1;
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
) -> Result<&'static str> {
    let abs = project.root.join(rel);
    let kind = FileKind::from_path(&abs);
    let (status, error, text) = match extract::extract(&abs) {
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
        index_one(project, db, rel, meta.len() as i64, mtime)?;
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
}

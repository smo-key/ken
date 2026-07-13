//! File watcher: debounced events funnel into the incremental scanner, so
//! renames, folder moves, and event storms all resolve through one
//! reconcile path. The handle owns the OS watcher; drop it to stop.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use notify::{RecursiveMode, Watcher as _};

use crate::db::Db;
use crate::project::Project;
use crate::scan::{self, ScanStats};
use crate::{Error, Result};

pub struct WatchHandle {
    watcher: Option<notify::RecommendedWatcher>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for WatchHandle {
    fn drop(&mut self) {
        // The watcher must drop FIRST: it owns the channel sender, and the
        // worker thread exits on channel disconnect. Joining before dropping
        // it would deadlock.
        drop(self.watcher.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Watch a project folder and keep its index fresh. `on_update` fires after
/// each processed batch with the scan stats. The watcher maintains its own
/// DB connection (SQLite WAL allows concurrent readers/writers).
pub fn start(
    project: Project,
    db_path: PathBuf,
    debounce: Duration,
    on_update: impl Fn(&ScanStats) + Send + 'static,
) -> Result<WatchHandle> {
    let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher = notify::recommended_watcher(tx)
        .map_err(|e| Error::Watch(e.to_string()))?;
    watcher
        .watch(&project.root, RecursiveMode::Recursive)
        .map_err(|e| Error::Watch(e.to_string()))?;
    // macOS FSEvents reports canonical paths (/private/var/…) while the
    // project may be registered via a symlinked path (/var/…); accept both.
    let mut roots = vec![project.root.clone()];
    if let Ok(canonical) = project.root.canonicalize() {
        if !roots.contains(&canonical) {
            roots.push(canonical);
        }
    }

    let thread = std::thread::spawn(move || {
        let mut db = match Db::open_at(&db_path) {
            Ok(db) => db,
            Err(_) => return,
        };
        let mut pending: HashSet<PathBuf> = HashSet::new();
        let mut first_event: Option<Instant> = None;
        // Never let a continuous event stream postpone flushing forever.
        let max_hold = debounce * 4;

        loop {
            let timeout = if pending.is_empty() {
                Duration::from_secs(3600)
            } else {
                debounce
            };
            match rx.recv_timeout(timeout) {
                Ok(Ok(event)) => {
                    if is_relevant(&event) {
                        for p in event.paths {
                            if relevant_path(&roots, &p) {
                                pending.insert(p);
                            }
                        }
                        if !pending.is_empty() && first_event.is_none() {
                            first_event = Some(Instant::now());
                        }
                    }
                    let held_too_long =
                        first_event.is_some_and(|t| t.elapsed() >= max_hold);
                    if held_too_long {
                        flush(&project, &mut db, &mut pending, &mut first_event, &on_update);
                    }
                }
                Ok(Err(_)) => { /* backend hiccup; the next scan reconciles */ }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if !pending.is_empty() {
                        flush(&project, &mut db, &mut pending, &mut first_event, &on_update);
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });

    Ok(WatchHandle {
        watcher: Some(watcher),
        thread: Some(thread),
    })
}

fn flush(
    project: &Project,
    db: &mut Db,
    pending: &mut HashSet<PathBuf>,
    first_event: &mut Option<Instant>,
    on_update: &impl Fn(&ScanStats),
) {
    pending.clear();
    *first_event = None;
    if let Ok(stats) = scan::scan(project, db) {
        let changed =
            stats.added + stats.updated + stats.removed + stats.failed > 0;
        if changed {
            on_update(&stats);
        }
    }
}

fn is_relevant(event: &notify::Event) -> bool {
    use notify::EventKind::*;
    matches!(event.kind, Create(_) | Modify(_) | Remove(_) | Any)
}

/// Ignore events under hidden folders (.git, .ken) — they never affect the
/// index and .git churn would otherwise trigger constant rescans.
fn relevant_path(roots: &[PathBuf], abs: &std::path::Path) -> bool {
    roots.iter().any(|root| match abs.strip_prefix(root) {
        Ok(rel) => !rel.components().any(|c| {
            c.as_os_str()
                .to_str()
                .is_some_and(|s| s.starts_with('.'))
        }),
        Err(_) => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn wait_until(mut cond: impl FnMut() -> bool, secs: u64) -> bool {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            if cond() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    fn setup() -> (tempfile::TempDir, tempfile::TempDir, Project, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        // The DB must live OUTSIDE the watched folder (as in production —
        // app-data), or its own writes would feed the watcher forever.
        let app_dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("notes")).unwrap();
        fs::write(dir.path().join("notes/seed.md"), "# Seed\ninitial fact\n").unwrap();
        let project = Project::create(dir.path(), "Watched").unwrap();
        let db_path = app_dir.path().join("index-test.db");
        // Initial scan through a separate connection.
        let mut db = Db::open_at(&db_path).unwrap();
        scan::scan(&project, &mut db).unwrap();
        (dir, app_dir, project, db_path)
    }

    #[test]
    fn watcher_indexes_new_and_removes_deleted() {
        let (dir, _app_dir, project, db_path) = setup();
        let updates = Arc::new(AtomicUsize::new(0));
        let u = updates.clone();
        let handle = start(project.clone(), db_path.clone(), Duration::from_millis(200), move |_| {
            u.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();

        fs::write(dir.path().join("notes/fresh.md"), "brand new insight\n").unwrap();
        let db = Db::open_at(&db_path).unwrap();
        assert!(
            wait_until(|| !db.search("brand new insight", 5).unwrap().is_empty(), 15),
            "new file should become searchable"
        );

        fs::remove_file(dir.path().join("notes/fresh.md")).unwrap();
        assert!(
            wait_until(|| db.search("brand new insight", 5).unwrap().is_empty(), 15),
            "deleted file should leave the index"
        );
        assert!(updates.load(Ordering::SeqCst) >= 2);
        drop(handle);
    }

    #[test]
    fn watcher_survives_event_burst() {
        let (dir, _app_dir, project, db_path) = setup();
        let handle = start(project.clone(), db_path.clone(), Duration::from_millis(200), |_| {})
            .unwrap();

        fs::create_dir_all(dir.path().join("bulk")).unwrap();
        for i in 0..600 {
            fs::write(
                dir.path().join(format!("bulk/file-{i}.md")),
                format!("bulk doc {i} contents\n"),
            )
            .unwrap();
        }
        let db = Db::open_at(&db_path).unwrap();
        assert!(
            wait_until(|| db.file_count().unwrap() >= 601, 30),
            "all burst files should be indexed, got {}",
            db.file_count().unwrap()
        );
        drop(handle);
    }
}

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use ken_core::assistant::{self, OneshotOutcome};
use ken_core::chat::{self, ChatEngine, ChatPty, ChatUpdate};
use ken_core::cloud;
use ken_core::db::{
    db_path, ChatField, ChatFlag, ChatMessage, ChatRow, Db, DigestRow, EdgeRow, EntityRow,
    EventRow, FileRow, RunRow, SearchHit,
};
use ken_core::digest;
use ken_core::knowledge_model::{self, AutoBuildTracker};
use ken_core::model;
use ken_core::engine::{self, EngineConfig, IngestEngine, IngestEvent};
use ken_core::research;
use ken_core::runner::{CancelToken, RunOutcome};
use ken_core::hooks::HookListener;
use ken_core::project::Project;
use ken_core::recipe::{self, Mode, Recipe, RecipeEntry, Refresh, ResolvedRules, RulesOverride};
use ken_core::registry::{Registry, RegistryEntryStatus};
use ken_core::pty_registry;
use ken_core::scan::{self, ScanStats};
use ken_core::sync::{self, SyncConfig, SyncEngine, SyncNotice};
use ken_core::transcript;
use ken_core::user_state::{self, UserState};
use ken_core::watch::{self, WatchHandle};

enum TerminalHandle {
    /// Our own PTY (user chat opened in terminal mode).
    Own(ChatPty),
    /// Attached to a live runner session via the registry.
    Attached,
}

struct ActiveProject {
    project: Project,
    db: Db,
    /// Dedicated read-only connection for `search`, on its OWN mutex so a
    /// keystroke's query never contends on the global `AppState` lock (which
    /// every other command and the background workers hold). WAL lets this
    /// reader run concurrently with the writers and still see their commits.
    search_db: Arc<Mutex<Db>>,
    _watch: WatchHandle,
    engine: Arc<IngestEngine>,
    chat_engine: Option<Arc<ChatEngine>>,
    /// Shared connection for chat persistence from event threads.
    chat_db: Arc<Mutex<Db>>,
    sync: Arc<SyncEngine>,
    terminals: Arc<Mutex<std::collections::HashMap<String, TerminalHandle>>>,
    /// True while a digest generation thread is running for this project.
    digest_running: Arc<AtomicBool>,
    /// True while a knowledge-model build thread is running.
    knowledge_running: Arc<AtomicBool>,
    /// Change/scan/build bookkeeping behind automatic Map & Timeline builds.
    auto_knowledge: Arc<AutoBuildTracker>,
    /// Stops the per-project extraction worker when this project closes.
    _extraction_worker: StopOnDrop,
    /// Stops the background cloud-hydration worker when this project closes.
    _bg_hydrate: StopOnDrop,
    /// Live research runs: chat/session id → cancel token.
    research: Arc<Mutex<std::collections::HashMap<String, CancelToken>>>,
    /// Video transcription bookkeeping shared by the manual command and the
    /// automatic ingest enqueue.
    transcripts: Arc<Mutex<TranscriptJobs>>,
}

/// Which videos are being transcribed now, and which have already been tried
/// this session. `attempted` stops an automatic transcription that failed
/// (bad audio, a Whisper error) from being retried on every scan — the same
/// converge-don't-thrash discipline the cloud retry rule follows.
#[derive(Default)]
struct TranscriptJobs {
    in_flight: std::collections::HashSet<String>,
    attempted: std::collections::HashSet<String>,
}

/// A long-lived worker thread's stop signal, flipped when the project it
/// belongs to is dropped (project switch or shutdown).
struct StopOnDrop(Arc<AtomicBool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

struct AppState {
    base_dir: PathBuf,
    hooks: Option<Arc<HookListener>>,
    active: Option<ActiveProject>,
    /// Model downloads are app-global (they live under the app-data dir, not a
    /// project), so their bookkeeping hangs off the top-level state: the set of
    /// model ids downloading right now (guards a second concurrent download of
    /// the same id).
    model_downloads: Arc<Mutex<std::collections::HashSet<String>>>,
    /// Monotonic id for ⌘K quick answers; a newer query bumps it so the
    /// in-flight generation's token callback sees a mismatch and cancels.
    qa_gen: Arc<AtomicU64>,
}

type SharedState = Arc<Mutex<AppState>>;

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ProjectInfo {
    id: String,
    name: String,
    root: String,
    excluded: Vec<String>,
    ingest_runner: String,
}

impl ProjectInfo {
    fn of(project: &Project) -> ProjectInfo {
        ProjectInfo {
            id: project.config.id.to_string(),
            name: project.config.name.clone(),
            root: project.root.to_string_lossy().into_owned(),
            excluded: project.config.excluded.clone(),
            ingest_runner: project
                .config
                .extra
                .get("ingestRunner")
                .and_then(|v| v.as_str())
                .unwrap_or("headless")
                .to_string(),
        }
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct SyncStateEvent {
    /// `off` | `synced` | `syncing` | `attention`
    state: String,
    detail: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TreeData {
    files: Vec<FileRow>,
    folders: Vec<FolderInfo>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FolderInfo {
    rel_path: String,
    excluded: bool,
}

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Activate a project: register it, open its DB, start the watcher, and
/// kick off a background scan that reports through events.
fn activate(app: &AppHandle, state: &SharedState, project: Project) -> CmdResult<ProjectInfo> {
    // The asset protocol streams this project's videos to the webview with
    // range support and no JS memory copy. Grant its root at runtime — project
    // roots are chosen by the user, so the static config scope can't name them.
    {
        use tauri::Manager as _;
        let _ = app.asset_protocol_scope().allow_directory(&project.root, true);
    }

    let mut guard = state.lock().unwrap();

    let mut registry = Registry::load(&guard.base_dir).map_err(err)?;
    registry.add(&project);
    registry.last_project = Some(project.config.id);
    registry.save(&guard.base_dir).map_err(err)?;

    let db = Db::open(&guard.base_dir, project.config.id).map_err(err)?;
    let watch_db_path = db_path(&guard.base_dir, project.config.id);

    // One-time unread baseline: snapshot the already-indexed files (this DB
    // persists across sessions, so an existing project has them here) as "seen"
    // so opening a project for the first time under this feature doesn't flag
    // every file. Only files that change or are added AFTER this point count as
    // unread. No-op once the project has been baselined before.
    {
        let mut us = UserState::load(&guard.base_dir, project.config.id);
        if !us.baselined {
            if let Ok(files) = db.list_files() {
                us.baseline(&index_versions(&files));
                let _ = us.save(&guard.base_dir, project.config.id);
            }
        }
    }

    let chat_db = Arc::new(Mutex::new(Db::open(&guard.base_dir, project.config.id).map_err(err)?));

    // Opened after `Db::open` above created and migrated the file, so the
    // read-only handle is guaranteed something to attach to.
    let search_db =
        Arc::new(Mutex::new(Db::open_read_only(&guard.base_dir, project.config.id).map_err(err)?));

    let hooks = match &guard.hooks {
        Some(h) => h.clone(),
        None => {
            let h = Arc::new(HookListener::start().map_err(err)?);
            guard.hooks = Some(h.clone());
            h
        }
    };
    let engine_app = app.clone();
    let ingest_chat_db = chat_db.clone();
    let engine = Arc::new(
        IngestEngine::start(
            project.root.clone(),
            watch_db_path.clone(),
            hooks.clone(),
            EngineConfig::default(),
            move |ev: IngestEvent| {
                // Ingest runs surface as system sessions in the chat drawer.
                if let Some(sid) = &ev.session_id {
                    let status = match ev.status.as_str() {
                        "running" => "working",
                        "blocked" => "needs_input",
                        "failed" => "error",
                        _ => "done",
                    };
                    let now = engine::now_epoch();
                    let mut db = ingest_chat_db.lock().unwrap();
                    let row = db.get_chat(sid).ok().flatten().unwrap_or(ChatRow {
                        id: sid.clone(),
                        title: format!("Ingest — {}", ev.slug),
                        kind: "ingest".into(),
                        pinned: false,
                        status: status.into(),
                        created_at: now,
                        last_active_at: now,
                        archived: false,
                        model: None,
                    });
                    let _ = db.upsert_chat(&ChatRow {
                        status: status.into(),
                        last_active_at: now,
                        ..row
                    });
                    if let Ok(Some(updated)) = db.get_chat(sid) {
                        let _ = engine_app.emit("chat-updated", updated);
                    }
                }
                let _ = engine_app.emit("ingest-run-changed", ev);
            },
        )
        .map_err(err)?,
    );

    // Conversation engine (chat drawer). Missing CLI → chats explain how to
    // install; everything else works.
    let chat_engine = ken_core::runner::discover_claude().map(|binary| {
        let chat_app = app.clone();
        let update_db = chat_db.clone();
        Arc::new(ChatEngine::new(
            binary,
            project.root.clone(),
            move |update: ChatUpdate| {
                let now = engine::now_epoch();
                let mut db = update_db.lock().unwrap();
                match update {
                    ChatUpdate::Message { chat_id, role, content } => {
                        let id = db.append_chat_message(&chat_id, &role, &content, now).unwrap_or(0);
                        let _ = db.touch_chat(&chat_id, now);
                        let _ = chat_app.emit("chat-message", ChatMessage {
                            id,
                            chat_id,
                            role,
                            content,
                            created_at: now,
                        });
                    }
                    ChatUpdate::Status { chat_id, status, detail } => {
                        let _ = db.set_chat_field(&chat_id, ChatField::Status, &status);
                        if let Some(d) = detail {
                            let _ = db.append_chat_message(&chat_id, "activity", &d, now);
                            let _ = chat_app.emit("chat-message", ChatMessage {
                                id: 0,
                                chat_id: chat_id.clone(),
                                role: "activity".into(),
                                content: d,
                                created_at: now,
                            });
                        }
                        if let Ok(Some(row)) = db.get_chat(&chat_id) {
                            let _ = chat_app.emit("chat-updated", row);
                        }
                    }
                }
            },
        ))
    });

    // Sync engine: active git sync when the folder is a repo with a
    // remote; passive conflicted-copy detection otherwise. Notices become
    // the title-bar dot (`sync-state`) and inbox refreshes (`review-changed`).
    let sync_app = app.clone();
    let sync_engine = Arc::new(
        SyncEngine::start(
            project.root.clone(),
            watch_db_path.clone(),
            SyncConfig::default(),
            move |notice| match notice {
                SyncNotice::State { state, detail } => {
                    let _ = sync_app.emit("sync-state", SyncStateEvent {
                        state: state.as_str().into(),
                        detail,
                    });
                }
                SyncNotice::ReviewChanged => {
                    let _ = sync_app.emit("review-changed", ());
                }
            },
        )
        .map_err(err)?,
    );

    let auto_knowledge = Arc::new(AutoBuildTracker::new());

    let emit_app = app.clone();
    let watch_engine = engine.clone();
    let watch_sync = sync_engine.clone();
    let watch_knowledge = auto_knowledge.clone();
    let watch_state = state.clone();
    let watch = watch::start(
        project.clone(),
        watch_db_path,
        Duration::from_secs(2),
        move |stats: &ScanStats| {
            if !stats.changed_paths.is_empty() {
                watch_engine.sources_changed(stats.changed_paths.clone());
                watch_sync.changed(stats.changed_paths.clone());
                // Same signal, slower consumer: the knowledge model is
                // rebuilt only once the changes stop coming.
                watch_knowledge.changed();
            }
            enqueue_transcriptions(&emit_app, &watch_state, &stats.videos_needing_transcript);
            let _ = emit_app.emit("index-updated", stats.clone());
        },
    )
    .map_err(err)?;

    let stop = Arc::new(AtomicBool::new(false));
    let bg_stop = Arc::new(AtomicBool::new(false));
    let info = ProjectInfo::of(&project);
    guard.active = Some(ActiveProject {
        project: project.clone(),
        db,
        search_db,
        _watch: watch,
        engine,
        chat_engine,
        chat_db,
        sync: sync_engine.clone(),
        terminals: Arc::new(Mutex::new(std::collections::HashMap::new())),
        digest_running: Arc::new(AtomicBool::new(false)),
        knowledge_running: Arc::new(AtomicBool::new(false)),
        auto_knowledge: auto_knowledge.clone(),
        _extraction_worker: StopOnDrop(stop.clone()),
        _bg_hydrate: StopOnDrop(bg_stop.clone()),
        research: Arc::new(Mutex::new(std::collections::HashMap::new())),
        transcripts: Arc::new(Mutex::new(TranscriptJobs::default())),
    });
    drop(guard);

    // Fetch teammates' updates right after opening.
    sync_engine.pull_now();

    // Initial scan in the background so opening stays instant.
    let scan_app = app.clone();
    let scan_sync = sync_engine.clone();
    let scan_knowledge = auto_knowledge.clone();
    let scan_state = state.clone();
    let base = { state.lock().unwrap().base_dir.clone() };
    let scan_project = project.clone();
    std::thread::spawn(move || {
        if let Ok(mut db) = Db::open(&base, scan_project.config.id) {
            let _ = scan_app.emit("scan-started", ());
            match scan::scan(&scan_project, &mut db) {
                Ok(stats) => {
                    if !stats.changed_paths.is_empty() {
                        scan_sync.changed(stats.changed_paths.clone());
                        scan_knowledge.changed();
                    }
                    enqueue_transcriptions(&scan_app, &scan_state, &stats.videos_needing_transcript);
                    let _ = scan_app.emit("index-updated", stats);
                }
                Err(e) => {
                    let _ = scan_app.emit("scan-error", e.to_string());
                }
            }
        }
        // Whatever happened, the folder is no longer being walked — a
        // knowledge build may now read a complete file list.
        scan_knowledge.scan_finished();
    });

    // One background extraction worker per open project: it drains the
    // `extractions` queue at the local model's background priority, one file
    // per generation, merging each delta into the Map/Timeline model and
    // emitting a throttled `knowledge-updated` after each merged file. When the
    // local model isn't ready it idles quietly (the Map shows a plain notice).
    let worker_app = app.clone();
    let worker_state = state.clone();
    let worker_id = project.config.id;
    let worker_stop = stop.clone();
    std::thread::spawn(move || {
        extraction_worker(worker_app, worker_state, worker_id, worker_stop);
    });

    // A low-priority worker that downloads cloud-offline DOCUMENTS in the
    // background so they become searchable without the user opening each one.
    // One thread per project, stopped by `_bg_hydrate` on close, gated on the
    // persisted `backgroundIndex` setting, and paced so it never fights the
    // transcript/knowledge workers or the user's own downloads.
    let bg_app = app.clone();
    let bg_state = state.clone();
    let bg_id = project.config.id;
    std::thread::spawn(move || {
        background_hydrate_worker(bg_app, bg_state, bg_id, bg_stop);
    });

    // The morning digest may be due the moment a project opens.
    let _ = maybe_generate_digest(app, state, false);

    Ok(info)
}

/// How often the background hydration worker wakes to look for cloud-only
/// documents to pull down. Long on purpose: this is opportunistic work that
/// must stay out of the way.
const BG_HYDRATE_TICK: Duration = Duration::from_secs(20);

/// Per-file download budget for one background attempt. Far shorter than the
/// on-open `cloud::DEFAULT_DEADLINE` (300s) — nobody is waiting on this, so a
/// slow file is abandoned quickly (its provider keeps downloading) and retried
/// on a later tick instead of pinning the single worker to one file.
const BG_HYDRATE_DEADLINE: Duration = Duration::from_secs(45);

/// Quiet gap between two background downloads so a large backlog trickles in
/// rather than saturating disk and bandwidth in a burst.
const BG_HYDRATE_PACING: Duration = Duration::from_secs(3);

/// Minimum wait before retrying a file that failed to materialize. A provider
/// that's offline (or a file that keeps timing out) must not be hammered every
/// tick — the backoff makes a persistent failure quiet instead of a spin.
const BG_HYDRATE_BACKOFF: Duration = Duration::from_secs(300);

/// The worker loop. Sequential and single-threaded, so there's no in-flight
/// dedup to do beyond the per-file backoff map; all file I/O runs OFF the
/// global lock against a private `Db` handle (the same pattern `hydrate_file`
/// and the background scan use), so it never freezes the UI.
fn background_hydrate_worker(
    app: AppHandle,
    state: SharedState,
    project_id: uuid::Uuid,
    stop: Arc<AtomicBool>,
) {
    // rel_path -> earliest instant we may retry after a failed hydrate.
    let mut backoff: std::collections::HashMap<String, Instant> = Default::default();

    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(BG_HYDRATE_TICK);
        if stop.load(Ordering::SeqCst) {
            return;
        }

        // Snapshot everything the tick needs, then drop the lock immediately —
        // the download and re-index below must not hold it. A different project
        // being open means this tick is stale (the drop guard will stop us).
        let (base, project, engine, sync, auto_knowledge) = {
            let guard = state.lock().unwrap();
            let Some(active) = guard.active.as_ref() else {
                continue;
            };
            if active.project.config.id != project_id {
                return;
            }
            if !ken_core::bg_hydrate::background_index_enabled(&active.project) {
                continue; // feature off → idle, but keep the thread alive
            }
            (
                guard.base_dir.clone(),
                active.project.clone(),
                active.engine.clone(),
                active.sync.clone(),
                active.auto_knowledge.clone(),
            )
        };

        // Read the cloud-only document backlog off a private handle.
        let Ok(db) = Db::open(&base, project_id) else {
            continue;
        };
        let Ok(files) = db.list_files() else {
            continue;
        };
        drop(db);
        let pending = ken_core::bg_hydrate::pending_documents(&files, &project);

        for rel in pending {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            // Respect the backoff for a file that recently refused to arrive.
            if backoff.get(&rel).is_some_and(|&until| Instant::now() < until) {
                continue;
            }
            let Ok(abs) = project.resolve(&rel) else {
                continue;
            };

            match cloud::hydrate_with_deadline(&abs, BG_HYDRATE_DEADLINE) {
                Ok(()) => {
                    backoff.remove(&rel);
                    // Re-index off the global lock, on a private handle: a
                    // committed write is visible to the app's own reads.
                    let changed = Db::open(&base, project_id)
                        .ok()
                        .and_then(|mut db| scan::refresh_path(&project, &mut db, &rel).ok())
                        .unwrap_or(false);
                    if changed {
                        // Same downstream notifications `hydrate_file` fires, so
                        // a just-downloaded doc reaches ingests, Map/Timeline and
                        // sync, and the footer's cloud_only count drops.
                        let paths = vec![rel.clone()];
                        engine.sources_changed(paths.clone());
                        sync.changed(paths.clone());
                        auto_knowledge.changed();
                        let _ = app.emit(
                            "index-updated",
                            ScanStats {
                                changed_paths: paths,
                                ..Default::default()
                            },
                        );
                    }
                    // Pace the backlog so it trickles rather than bursts.
                    std::thread::sleep(BG_HYDRATE_PACING);
                }
                // Provider offline, or the file is still downloading past the
                // short deadline: leave the row cloud_only and try later. Never
                // an error — nobody asked for this work.
                Err(_) => {
                    backoff.insert(rel, Instant::now() + BG_HYDRATE_BACKOFF);
                }
            }
        }
    }
}

#[tauri::command]
fn list_projects(state: State<SharedState>) -> CmdResult<Vec<RegistryEntryStatus>> {
    let guard = state.lock().unwrap();
    Ok(Registry::load(&guard.base_dir).map_err(err)?.statuses())
}

#[tauri::command]
fn create_project(
    app: AppHandle,
    state: State<SharedState>,
    path: String,
    name: String,
) -> CmdResult<ProjectInfo> {
    let project = Project::create(std::path::Path::new(&path), &name).map_err(err)?;
    activate(&app, &state, project)
}

#[tauri::command]
fn open_project(app: AppHandle, state: State<SharedState>, path: String) -> CmdResult<ProjectInfo> {
    let project = Project::open(std::path::Path::new(&path)).map_err(err)?;
    activate(&app, &state, project)
}

#[tauri::command]
fn forget_project(state: State<SharedState>, id: String) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let uuid: uuid::Uuid = id.parse().map_err(err)?;
    let mut registry = Registry::load(&guard.base_dir).map_err(err)?;
    registry.remove(uuid);
    if registry.last_project == Some(uuid) {
        registry.last_project = None;
    }
    registry.save(&guard.base_dir).map_err(err)
}

/// Rename a project. The name lives in two stores that must stay in step: the
/// project's own `.ken/project.json` (source of truth, travels with the folder)
/// and the user-level registry (drives the switcher/recents). When the renamed
/// project is the open one, the in-memory copy is updated too so the title bar
/// and switcher reflect it without a reopen.
#[tauri::command]
fn rename_project(
    state: State<SharedState>,
    id: String,
    name: String,
) -> CmdResult<ProjectInfo> {
    let uuid: uuid::Uuid = id.parse().map_err(err)?;
    let mut guard = state.lock().unwrap();
    let base_dir = guard.base_dir.clone();

    let mut registry = Registry::load(&base_dir).map_err(err)?;
    let entry_path = registry
        .projects
        .iter()
        .find(|e| e.id == uuid)
        .map(|e| e.path.clone())
        .ok_or("unknown project")?;

    // Rewrite `.ken/project.json`, validating first — an invalid name aborts
    // here before either store is touched.
    let info = if let Some(active) =
        guard.active.as_mut().filter(|a| a.project.config.id == uuid)
    {
        active.project.set_name(&name).map_err(err)?;
        ProjectInfo::of(&active.project)
    } else {
        let mut project = Project::open(&entry_path).map_err(err)?;
        project.set_name(&name).map_err(err)?;
        ProjectInfo::of(&project)
    };

    if let Some(entry) = registry.projects.iter_mut().find(|e| e.id == uuid) {
        entry.name = info.name.clone();
    }
    registry.save(&base_dir).map_err(err)?;
    Ok(info)
}

/// The id of the most recently opened project, for launch-to-last-project.
#[tauri::command]
fn last_project_id(state: State<SharedState>) -> CmdResult<Option<String>> {
    let guard = state.lock().unwrap();
    let registry = Registry::load(&guard.base_dir).map_err(err)?;
    Ok(registry.last_project.map(|id| id.to_string()))
}

#[tauri::command]
fn current_project(state: State<SharedState>) -> CmdResult<Option<ProjectInfo>> {
    let guard = state.lock().unwrap();
    Ok(guard.active.as_ref().map(|a| ProjectInfo::of(&a.project)))
}

#[tauri::command]
fn set_folder_selection(
    app: AppHandle,
    state: State<SharedState>,
    excluded: Vec<String>,
) -> CmdResult<ProjectInfo> {
    let mut guard = state.lock().unwrap();
    let active = guard.active.as_mut().ok_or("no project open")?;
    active.project.set_excluded(excluded).map_err(err)?;
    let stats = scan::scan(&active.project, &mut active.db).map_err(err)?;
    let info = ProjectInfo::of(&active.project);
    let videos = stats.videos_needing_transcript.clone();
    drop(guard);
    enqueue_transcriptions(&app, state.inner(), &videos);
    let _ = app.emit("index-updated", stats);
    Ok(info)
}

#[tauri::command]
fn get_tree(state: State<SharedState>) -> CmdResult<TreeData> {
    let mut guard = state.lock().unwrap();
    let active = guard.active.as_mut().ok_or("no project open")?;
    let files = active.db.list_files().map_err(err)?;

    // Folders straight from disk so excluded/empty ones still show.
    let mut folders = Vec::new();
    let walker = ignore::WalkBuilder::new(&active.project.root)
        .hidden(true)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            name != ".ken" && !(e.path().is_dir() && ken_core::scan::is_junk_dir_name(&name))
        })
        .build();
    for entry in walker.flatten() {
        if entry.path().is_dir() && entry.path() != active.project.root {
            if let Ok(rel) = entry.path().strip_prefix(&active.project.root) {
                let rel = rel.to_string_lossy().replace('\\', "/");
                folders.push(FolderInfo {
                    excluded: active.project.is_excluded(&rel),
                    rel_path: rel,
                });
            }
        }
    }
    folders.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(TreeData { files, folders })
}

#[tauri::command]
fn search(state: State<SharedState>, query: String, limit: Option<usize>) -> CmdResult<Vec<SearchHit>> {
    // Hold the global lock only long enough to clone the Arc (microseconds),
    // then release it so the FTS query runs on the dedicated read-only handle
    // without serializing against every other command and background worker.
    let search_db = {
        let guard = state.lock().unwrap();
        guard.active.as_ref().ok_or("no project open")?.search_db.clone()
    };
    let db = search_db.lock().unwrap();
    db.search(&query, limit.unwrap_or(30)).map_err(err)
}

/// Error code the frontend matches on to offer a download instead of a
/// failure: the file's bytes are still in the cloud.
const CLOUD_ONLY_ERR: &str = "CLOUD_ONLY";

/// Resolve a project-relative path without holding the state lock across the
/// file I/O that follows. Reads can block for a long time (a cloud
/// placeholder downloads on first read), and the lock serializes every other
/// command in the app.
fn resolve_path(state: &State<SharedState>, rel_path: &str) -> CmdResult<PathBuf> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    active.project.resolve(rel_path).map_err(err)
}

#[tauri::command]
fn read_file(state: State<SharedState>, rel_path: String) -> CmdResult<String> {
    let abs = resolve_path(&state, &rel_path)?;
    if cloud::is_placeholder(&abs) {
        return Err(CLOUD_ONLY_ERR.into());
    }
    std::fs::read_to_string(&abs).map_err(err)
}

#[tauri::command]
fn read_file_bytes(state: State<SharedState>, rel_path: String) -> CmdResult<tauri::ipc::Response> {
    let abs = resolve_path(&state, &rel_path)?;
    if cloud::is_placeholder(&abs) {
        return Err(CLOUD_ONLY_ERR.into());
    }
    let bytes = std::fs::read(&abs).map_err(err)?;
    Ok(tauri::ipc::Response::new(bytes))
}

/// Is this file a cloud placeholder (OneDrive/iCloud "online-only")?
#[tauri::command]
fn is_cloud_only(state: State<SharedState>, rel_path: String) -> CmdResult<bool> {
    Ok(cloud::is_placeholder(&resolve_path(&state, &rel_path)?))
}

/// Download a cloud placeholder's bytes, then index its content. The download
/// blocks for as long as the provider takes (minutes, for a big file — see
/// `cloud::hydrate`), so it runs on a blocking thread with no lock held; the
/// rest of the app stays responsive meanwhile.
#[tauri::command]
async fn hydrate_file(
    app: AppHandle,
    state: State<'_, SharedState>,
    rel_path: String,
) -> CmdResult<()> {
    let abs = resolve_path(&state, &rel_path)?;
    let path = abs.clone();
    tauri::async_runtime::spawn_blocking(move || ken_core::cloud::hydrate(&path))
        .await
        .map_err(err)?
        .map_err(err)?;

    // Snapshot everything the re-index and its notifications need, then drop
    // the global lock immediately.
    let (base, project, engine, sync, auto_knowledge) = {
        let guard = state.lock().unwrap();
        let active = guard.active.as_ref().ok_or("no project open")?;
        (
            guard.base_dir.clone(),
            active.project.clone(),
            active.engine.clone(),
            active.sync.clone(),
            active.auto_knowledge.clone(),
        )
    };
    let project_id = project.config.id;

    // The bytes are here now. Re-index so the content becomes searchable;
    // hydration changes neither size nor mtime, so nothing else would notice.
    // Parsing a large just-downloaded file (extract::extract) can take
    // seconds, so run it OFF the global lock — like the download above —
    // against a private Db handle to the same sqlite file (the pattern the
    // background scan uses; a committed write is visible to the global
    // handle's reads). Holding the lock across the parse would freeze every
    // other IPC command right after "Downloading…" clears.
    let rel = rel_path.clone();
    let changed = tauri::async_runtime::spawn_blocking(move || -> CmdResult<bool> {
        let mut db = Db::open(&base, project_id).map_err(err)?;
        scan::refresh_path(&project, &mut db, &rel).map_err(err)
    })
    .await
    .map_err(err)??;

    // Drive the SAME downstream notifications the watcher fires for a changed
    // file, so a just-downloaded doc reaches ingests, Map/Timeline, and sync
    // immediately instead of waiting for an edit or a manual reindex.
    if changed {
        let paths = vec![rel_path.clone()];
        engine.sources_changed(paths.clone());
        sync.changed(paths.clone());
        auto_knowledge.changed();
        let _ = app.emit(
            "index-updated",
            ScanStats {
                changed_paths: paths,
                ..Default::default()
            },
        );
    }
    Ok(())
}

#[tauri::command]
fn save_file(
    app: AppHandle,
    state: State<SharedState>,
    rel_path: String,
    content: String,
) -> CmdResult<i64> {
    let mut guard = state.lock().unwrap();
    let active = guard.active.as_mut().ok_or("no project open")?;
    let abs = active.project.resolve(&rel_path).map_err(err)?;
    std::fs::write(&abs, &content).map_err(err)?;
    // Index immediately — no need to wait for the watcher debounce.
    scan::refresh_path(&active.project, &mut active.db, &rel_path).map_err(err)?;
    let mtime = abs
        .metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // Record the just-written version as seen so the user's OWN edit never
    // counts as unread — the whole point of unread being "changed by someone
    // else". Read the post-refresh row so size/mtime match what the index (and
    // the unread check) now hold. Capture from `active` first, then touch
    // `guard.base_dir` (its mutable borrow through `active` must end first).
    let seen_version = active
        .db
        .get_file(&rel_path)
        .map_err(err)?
        .map(|r| (r.size, r.mtime));
    let project_id = active.project.config.id;
    if let Some(version) = seen_version {
        let base = guard.base_dir.clone();
        let mut us = UserState::load(&base, project_id);
        if us.mark_seen(&rel_path, version) {
            let _ = us.save(&base, project_id);
        }
    }
    let _ = app.emit("file-saved", &rel_path);
    Ok(mtime)
}

#[tauri::command]
fn file_meta(state: State<SharedState>, rel_path: String) -> CmdResult<Option<FileRow>> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    active.db.get_file(&rel_path).map_err(err)
}

#[tauri::command]
fn extracted_text(state: State<SharedState>, rel_path: String) -> CmdResult<String> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    let abs = active.project.resolve(&rel_path).map_err(err)?;
    Ok(ken_core::extract::extract(&abs)
        .map(|e| e.text)
        .unwrap_or_default())
}

#[tauri::command]
fn reindex(app: AppHandle, state: State<SharedState>) -> CmdResult<ScanStats> {
    let mut guard = state.lock().unwrap();
    let active = guard.active.as_mut().ok_or("no project open")?;
    let stats = scan::reindex(&active.project, &mut active.db).map_err(err)?;
    let videos = stats.videos_needing_transcript.clone();
    drop(guard);
    enqueue_transcriptions(&app, state.inner(), &videos);
    let _ = app.emit("index-updated", stats.clone());
    Ok(stats)
}

#[tauri::command]
fn open_external(state: State<SharedState>, app: AppHandle, rel_path: String) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    let abs = active.project.resolve(&rel_path).map_err(err)?;
    tauri_plugin_opener::OpenerExt::opener(&app)
        .open_path(abs.to_string_lossy(), None::<&str>)
        .map_err(err)
}

/// `errno` for a cross-device rename on Unix; `fs::rename` fails with this when
/// source and destination live on different filesystems.
#[cfg(unix)]
const EXDEV: i32 = 18;

/// Move a file OR folder within the project. Both paths are validated to stay
/// inside the project root (`resolve` rejects `..`/absolute escapes); overwriting
/// an existing destination is refused. Folder moves (same-parent rename or a
/// full move) rename the directory, then reconcile child index rows through the
/// standard rescan — the same reconciliation the watcher does, but synchronous
/// so the caller's tree refresh already sees it.
#[tauri::command]
fn move_file(
    app: AppHandle,
    state: State<SharedState>,
    from_rel: String,
    to_rel: String,
) -> CmdResult<()> {
    let (from_abs, to_abs) = {
        let guard = state.lock().unwrap();
        let active = guard.active.as_ref().ok_or("no project open")?;
        let from_abs = active.project.resolve(&from_rel).map_err(err)?;
        let to_abs = active.project.resolve(&to_rel).map_err(err)?;
        (from_abs, to_abs)
    };

    if from_abs == to_abs {
        return Ok(());
    }
    if ken_core::fsops::is_into_own_subtree(&from_rel, &to_rel) {
        return Err("A folder can't be moved into itself.".to_string());
    }
    let from_is_dir = from_abs.is_dir();
    if !from_abs.is_file() && !from_is_dir {
        return Err("That file or folder no longer exists.".to_string());
    }
    if to_abs.exists() {
        let name = to_abs
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| to_rel.clone());
        return Err(format!("\u{201c}{name}\u{201d} already exists in that folder."));
    }
    if let Some(parent) = to_abs.parent() {
        std::fs::create_dir_all(parent).map_err(err)?;
    }

    match std::fs::rename(&from_abs, &to_abs) {
        Ok(()) => {}
        #[cfg(unix)]
        Err(e) if e.raw_os_error() == Some(EXDEV) => {
            if from_is_dir {
                return Err(
                    "That folder can't be moved across drives from here — move it in Finder instead."
                        .to_string(),
                );
            }
            std::fs::copy(&from_abs, &to_abs).map_err(err)?;
            std::fs::remove_file(&from_abs).map_err(err)?;
        }
        Err(e) => return Err(err(e)),
    }

    let mut guard = state.lock().unwrap();
    let active = guard.active.as_mut().ok_or("no project open")?;
    if from_is_dir {
        // Drop the old subtree's rows, then rescan so every child re-indexes at
        // its new path (unchanged files elsewhere are skipped by the scanner).
        active.db.remove_folder(&from_rel).map_err(err)?;
        let stats = scan::reindex(&active.project, &mut active.db).map_err(err)?;
        let videos = stats.videos_needing_transcript.clone();
        drop(guard);
        enqueue_transcriptions(&app, state.inner(), &videos);
        let _ = app.emit("index-updated", stats);
    } else {
        scan::refresh_path(&active.project, &mut active.db, &from_rel).map_err(err)?;
        scan::refresh_path(&active.project, &mut active.db, &to_rel).map_err(err)?;
    }
    Ok(())
}

/// Create a folder. Fails when something with that name already exists (the UI
/// validates sibling names first; this is the race-safety backstop). Folders
/// aren't index rows — the tree walks them off disk — so no refresh is needed;
/// the caller's tree refresh picks it up.
#[tauri::command]
fn create_folder(state: State<SharedState>, rel_path: String) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    let abs = active.project.resolve(&rel_path).map_err(err)?;
    if abs.exists() {
        let name = rel_path.rsplit('/').next().unwrap_or(&rel_path);
        return Err(format!("\u{201c}{name}\u{201d} already exists here."));
    }
    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent).map_err(err)?;
    }
    std::fs::create_dir(&abs).map_err(err)?;
    Ok(())
}

/// Create an empty markdown document. `rel_path` names the desired location
/// (e.g. "Meetings/Untitled.md"); a collision dedupes with a counter
/// ("Untitled 2.md", …) rather than failing, and the FINAL project-relative
/// path is returned so the UI opens the tab it actually created. The new file
/// is indexed immediately so search and the tree stay correct before the
/// watcher fires.
#[tauri::command]
fn create_document(state: State<SharedState>, rel_path: String) -> CmdResult<String> {
    let mut guard = state.lock().unwrap();
    let active = guard.active.as_mut().ok_or("no project open")?;
    let desired_abs = active.project.resolve(&rel_path).map_err(err)?;
    let dir = desired_abs
        .parent()
        .ok_or_else(|| "invalid document path".to_string())?
        .to_path_buf();
    let name = desired_abs
        .file_name()
        .ok_or_else(|| "invalid document name".to_string())?
        .to_string_lossy()
        .into_owned();
    std::fs::create_dir_all(&dir).map_err(err)?;
    let final_name = ken_core::fsops::numbered_name(&name, |c| dir.join(c).exists());
    let abs = dir.join(&final_name);
    // create_new: never clobber a file that appeared between the dedupe and now.
    std::fs::File::options()
        .write(true)
        .create_new(true)
        .open(&abs)
        .map_err(err)?;
    let folder = match rel_path.rfind('/') {
        Some(i) => &rel_path[..i],
        None => "",
    };
    let final_rel = if folder.is_empty() {
        final_name
    } else {
        format!("{folder}/{final_name}")
    };
    scan::refresh_path(&active.project, &mut active.db, &final_rel).map_err(err)?;
    Ok(final_rel)
}

// ---------- file import ----------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ImportDto {
    import_id: String,
    file_name: String,
    /// Project-relative path of the STAGED copy — feed it straight to the same
    /// preview commands a normal file uses (`.ken/imports/...` reads fine
    /// through `project.resolve`; it's not a cloud placeholder, so it isn't
    /// blocked). The staged file is not indexed, hence `kind`/`size` here.
    preview_rel: String,
    kind: String,
    size: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PlacementDto {
    /// Project-relative destination folder; empty means the project root.
    folder: String,
    is_new: bool,
    rationale: Option<String>,
}

/// The single staged file for an import: (absolute path, file name). Its dir
/// holds exactly the one file `import_begin` copied in, so classify/commit read
/// the name back rather than threading it through every call.
fn staged_file(root: &std::path::Path, import_id: &str) -> CmdResult<(PathBuf, String)> {
    let dir = ken_core::import::staging_dir(root, import_id);
    let entry = std::fs::read_dir(&dir)
        .map_err(|_| "This import is no longer available.".to_string())?
        .flatten()
        .find(|e| e.path().is_file())
        .ok_or("This import is no longer available.")?;
    let name = entry.file_name().to_string_lossy().into_owned();
    Ok((entry.path(), name))
}

/// The project's real folders (project-relative), the same set `get_tree`
/// surfaces — the grounding the classifier chooses among. Walks off any lock.
fn project_folders(root: &std::path::Path) -> Vec<String> {
    let mut folders = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            name != ".ken" && !(e.path().is_dir() && ken_core::scan::is_junk_dir_name(&name))
        })
        .build();
    for entry in walker.flatten() {
        if entry.path().is_dir() && entry.path() != root {
            if let Ok(rel) = entry.path().strip_prefix(root) {
                folders.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    folders.sort();
    folders
}

/// Copy an external file into a private staging area inside the project so it's
/// previewable before it's placed, without indexing it. The original is only
/// read; the copy runs OFF the lock (a large file must not freeze other IPC).
#[tauri::command]
fn import_begin(state: State<SharedState>, src_path: String) -> CmdResult<ImportDto> {
    let root = {
        let guard = state.lock().unwrap();
        guard.active.as_ref().ok_or("no project open")?.project.root.clone()
    };
    let src = PathBuf::from(&src_path);
    if !src.is_file() {
        return Err("Choose a file to import.".into());
    }
    let file_name = src
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or("That file has no name.")?;
    let import_id = ken_core::import::new_import_id();
    let dir = ken_core::import::staging_dir(&root, &import_id);
    std::fs::create_dir_all(&dir).map_err(err)?;
    let dest = dir.join(&file_name);
    std::fs::copy(&src, &dest).map_err(err)?;
    let size = dest.metadata().map(|m| m.len()).unwrap_or(0);
    let kind = ken_core::extract::FileKind::from_path(&dest).as_str().to_string();
    Ok(ImportDto {
        preview_rel: ken_core::import::staging_rel(&import_id, &file_name),
        import_id,
        file_name,
        kind,
        size,
    })
}

/// Ask Claude Code where the staged file should live, grounded in the project's
/// real folders. Awaitable (the CLI takes seconds). Never hard-fails: a missing
/// CLI, a lost staging file, or an unusable reply all degrade to the project
/// root so the import flow keeps working.
#[tauri::command]
async fn import_classify(
    state: State<'_, SharedState>,
    import_id: String,
) -> CmdResult<PlacementDto> {
    let root = {
        let guard = state.lock().unwrap();
        guard.active.as_ref().ok_or("no project open")?.project.root.clone()
    };
    let default = || PlacementDto { folder: String::new(), is_new: false, rationale: None };
    let Some(binary) = ken_core::runner::discover_claude() else {
        return Ok(default());
    };
    let Ok((abs, file_name)) = staged_file(&root, &import_id) else {
        return Ok(default());
    };
    let kind = ken_core::extract::FileKind::from_path(&abs).as_str().to_string();
    let folders = project_folders(&root);

    // Parsing a large file can take seconds — do it off any lock, like the
    // oneshot below.
    let excerpt = tauri::async_runtime::spawn_blocking({
        let abs = abs.clone();
        move || {
            ken_core::extract::extract(&abs)
                .map(|e| e.text.chars().take(4000).collect::<String>())
                .unwrap_or_default()
        }
    })
    .await
    .map_err(err)?;

    let prompt = ken_core::import::compose_classify_prompt(&file_name, &kind, &excerpt, &folders);
    let folders_for_parse = folders.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        assistant::oneshot(&binary, &root, &prompt, Duration::from_secs(60), &CancelToken::new())
    })
    .await
    .map_err(err)?;

    let placement = match outcome {
        Ok(OneshotOutcome::Completed(text)) => {
            ken_core::import::parse_placement(&text, &folders_for_parse)
        }
        _ => ken_core::import::Placement::root(),
    };
    Ok(PlacementDto {
        folder: placement.folder,
        is_new: placement.is_new,
        rationale: placement.rationale,
    })
}

/// Place the staged file into the chosen folder and index it. Works even if
/// classification never ran (save-before-processed): the frontend passes
/// whatever folder is selected. Validates the folder stays inside the project,
/// creates it when asked, disambiguates the name so nothing is overwritten,
/// then fires the SAME downstream notifications `hydrate_file` does.
#[tauri::command]
fn import_commit(
    app: AppHandle,
    state: State<SharedState>,
    import_id: String,
    dest_folder_rel: String,
    create_folder: bool,
) -> CmdResult<String> {
    let (root, project, engine, sync, auto_knowledge) = {
        let guard = state.lock().unwrap();
        let active = guard.active.as_ref().ok_or("no project open")?;
        (
            active.project.root.clone(),
            active.project.clone(),
            active.engine.clone(),
            active.sync.clone(),
            active.auto_knowledge.clone(),
        )
    };

    let (staged_abs, file_name) = staged_file(&root, &import_id)?;

    // `resolve` rejects `..`/absolute escapes; an empty folder is the root.
    let dest_dir = project.resolve(&dest_folder_rel).map_err(err)?;
    if !dest_dir.exists() {
        if create_folder {
            std::fs::create_dir_all(&dest_dir).map_err(err)?;
        } else {
            return Err("That folder doesn't exist.".into());
        }
    }

    let chosen = ken_core::import::disambiguate_name(&file_name, |c| dest_dir.join(c).exists());
    let final_abs = dest_dir.join(&chosen);
    let final_rel = ken_core::import::final_rel(&dest_folder_rel, &chosen);

    // staging → final is a move; the external original was already copied.
    match std::fs::rename(&staged_abs, &final_abs) {
        Ok(()) => {}
        #[cfg(unix)]
        Err(e) if e.raw_os_error() == Some(EXDEV) => {
            std::fs::copy(&staged_abs, &final_abs).map_err(err)?;
            std::fs::remove_file(&staged_abs).map_err(err)?;
        }
        Err(e) => return Err(err(e)),
    }
    let _ = std::fs::remove_dir_all(ken_core::import::staging_dir(&root, &import_id));

    let changed = {
        let mut guard = state.lock().unwrap();
        let active = guard.active.as_mut().ok_or("no project open")?;
        scan::refresh_path(&active.project, &mut active.db, &final_rel).map_err(err)?
    };
    if changed {
        let paths = vec![final_rel.clone()];
        engine.sources_changed(paths.clone());
        sync.changed(paths.clone());
        auto_knowledge.changed();
        let _ = app.emit(
            "index-updated",
            ScanStats { changed_paths: paths, ..Default::default() },
        );
    }
    Ok(final_rel)
}

/// Discard a staged import (dialog cancelled): delete its staging dir.
#[tauri::command]
fn import_cancel(state: State<SharedState>, import_id: String) -> CmdResult<()> {
    let root = {
        let guard = state.lock().unwrap();
        guard.active.as_ref().ok_or("no project open")?.project.root.clone()
    };
    let _ = std::fs::remove_dir_all(ken_core::import::staging_dir(&root, &import_id));
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct McpInfo {
    binary_path: Option<String>,
    project_root: String,
    add_command: String,
    json_config: String,
    llm_instruction: String,
}

/// Where is the `ken-mcp` binary? Installer layout first (sibling of the
/// app executable, then `~/.local/bin`), then PATH, then a dev build.
fn find_ken_mcp() -> Option<PathBuf> {
    let name = format!("ken-mcp{}", std::env::consts::EXE_SUFFIX);
    if let Ok(exe) = std::env::current_exe() {
        if let Some(sibling) = exe.parent().map(|d| d.join(&name)) {
            if sibling.is_file() {
                return Some(sibling);
            }
        }
    }
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        let local = PathBuf::from(home).join(".local/bin").join(&name);
        if local.is_file() {
            return Some(local);
        }
    }
    if let Ok(out) = std::process::Command::new("which").arg("ken-mcp").output() {
        if out.status.success() {
            let path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
            if path.is_file() {
                return Some(path);
            }
        }
    }
    let dev = PathBuf::from("target/debug").join(&name);
    if dev.is_file() {
        return Some(dev.canonicalize().unwrap_or(dev));
    }
    None
}

/// Quote a word for pasting into a shell only when it needs it.
fn shell_word(s: &str) -> String {
    if s.chars().any(|c| c.is_whitespace() || c == '"' || c == '\'') {
        format!("\"{}\"", s.replace('"', "\\\""))
    } else {
        s.to_string()
    }
}

#[tauri::command]
fn mcp_info(state: State<SharedState>) -> CmdResult<McpInfo> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    let root = active.project.root.to_string_lossy().into_owned();
    let binary_path = find_ken_mcp().map(|p| p.to_string_lossy().into_owned());
    // When the binary isn't found the strings still render with the bare
    // name so copied configs work once it lands on PATH.
    let command = binary_path.clone().unwrap_or_else(|| "ken-mcp".into());
    let add_command = format!(
        "claude mcp add ken -- {} --project {}",
        shell_word(&command),
        shell_word(&root)
    );
    let json_config = serde_json::to_string_pretty(&serde_json::json!({
        "mcpServers": {
            "ken": { "command": command, "args": ["--project", root] }
        }
    }))
    .map_err(err)?;
    let llm_instruction = format!(
        "Set up the Ken MCP server so you can search this team's knowledge base.\n\n\
Ken is a local knowledge app that indexes the project folder at {root}. Its MCP \
server binary, ken-mcp, exposes read-only tools over that index: search_knowledge \
(full-text search), read_document, list_documents, and list_projects. It runs on \
demand over stdio and never modifies any files.\n\n\
If you are Claude Code, register it by running:\n\n  {add_command}\n\n\
Otherwise, add this to your MCP configuration:\n\n{json_config}\n\n\
Once connected, use search_knowledge to find relevant documents and read_document \
to read them."
    );
    Ok(McpInfo {
        binary_path,
        project_root: root,
        add_command,
        json_config,
        llm_instruction,
    })
}

#[tauri::command]
fn file_mtime(state: State<SharedState>, rel_path: String) -> CmdResult<i64> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    let abs = active.project.resolve(&rel_path).map_err(err)?;
    Ok(abs
        .metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0))
}


// ---------- video: streaming + transcripts ----------

/// Build the webview-loadable URL Tauri's asset protocol serves for a local
/// file, matching `@tauri-apps/api/core`'s `convertFileSrc`. The asset
/// protocol streams with HTTP range support, so the `<video>` element can seek
/// without the bytes ever passing through JS. The project root is added to the
/// protocol's runtime scope when the project opens (see `activate`).
fn to_asset_url(abs: &std::path::Path) -> String {
    let encoded = encode_uri_component(&abs.to_string_lossy());
    if cfg!(windows) {
        format!("http://asset.localhost/{encoded}")
    } else {
        format!("asset://localhost/{encoded}")
    }
}

/// `encodeURIComponent`, so the path survives inside a URL exactly as the JS
/// `convertFileSrc` would encode it (the asset handler decodes it the same way).
fn encode_uri_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        let c = b as char;
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '!' | '~' | '*' | '\'' | '(' | ')') {
            out.push(c);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Resolve a project-relative video to a streamable asset URL. The file is
/// already local by the time media plays (EditorPane hydrates first), so this
/// only validates the path and confirms the bytes are here.
#[tauri::command]
fn media_src(state: State<SharedState>, rel_path: String) -> CmdResult<String> {
    let abs = resolve_path(&state, &rel_path)?;
    if !abs.is_file() {
        return Err("That media file isn't available on disk.".into());
    }
    Ok(to_asset_url(&abs))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TranscriptDto {
    vtt: Option<String>,
    source_rel: Option<String>,
    /// `ready` | `generating` | `none`
    status: String,
}

/// A video's transcript, resolved in the contract's order: adjacent `.vtt`,
/// then a fuzzy-matched adjacent `.docx`, then a previously generated file;
/// failing all three, `generating` if a Whisper job is in flight, else `none`.
#[tauri::command]
fn video_transcript(state: State<SharedState>, rel_path: String) -> CmdResult<TranscriptDto> {
    let (abs, root, generating) = {
        let guard = state.lock().unwrap();
        let active = guard.active.as_ref().ok_or("no project open")?;
        let abs = active.project.resolve(&rel_path).map_err(err)?;
        let generating = active.transcripts.lock().unwrap().in_flight.contains(&rel_path);
        (abs, active.project.root.clone(), generating)
    };
    // Resolution reads small adjacent files; do it off the state lock.
    match transcript::resolve_transcript(&abs, &root) {
        Some(t) => Ok(TranscriptDto {
            vtt: Some(t.vtt),
            source_rel: t.source_rel,
            status: "ready".into(),
        }),
        None => Ok(TranscriptDto {
            vtt: None,
            source_rel: None,
            status: if generating { "generating" } else { "none" }.into(),
        }),
    }
}

/// Kick off on-device transcription for one video as a background job. Manual
/// invocation surfaces a clear error when ffmpeg or the model is missing; the
/// finished `.vtt` re-indexes the video so `index-updated` fires and the
/// frontend re-fetches the transcript.
#[tauri::command]
fn generate_transcript(app: AppHandle, state: State<SharedState>, rel_path: String) -> CmdResult<()> {
    let (root, base, project_id, jobs) = {
        let guard = state.lock().unwrap();
        let active = guard.active.as_ref().ok_or("no project open")?;
        active.project.resolve(&rel_path).map_err(err)?; // reject path escape
        (
            active.project.root.clone(),
            guard.base_dir.clone(),
            active.project.config.id,
            active.transcripts.clone(),
        )
    };
    let ffmpeg = transcript::discover_ffmpeg();
    let model = model::selected_model_path(&base, model::ModelCategory::Transcription)
        .unwrap_or_else(|| transcript::model_path(&base));
    if let Some(blocker) = transcript::transcription_blocker(ffmpeg.is_some(), model.is_file(), &model) {
        return Err(blocker);
    }
    let job = TranscriptionJob {
        state: state.inner().clone(),
        root,
        project_id,
        jobs,
        ffmpeg: ffmpeg.unwrap(),
        model,
        rel_path,
        quiet: false,
    };
    spawn_transcription(&app, job);
    Ok(())
}

struct TranscriptionJob {
    state: SharedState,
    root: PathBuf,
    project_id: uuid::Uuid,
    jobs: Arc<Mutex<TranscriptJobs>>,
    ffmpeg: PathBuf,
    model: PathBuf,
    rel_path: String,
    /// Automatic (ingest) jobs stay silent on failure; manual ones don't.
    quiet: bool,
}

/// The ingest-side enqueue: transcribe newly-seen videos automatically, but go
/// completely quiet when ffmpeg or the model is missing — exactly like the
/// knowledge auto-build going silent without the Claude CLI. Nobody asked for
/// this work, so a missing prerequisite is a no-op, never an error.
fn enqueue_transcriptions(app: &AppHandle, state: &SharedState, rels: &[String]) {
    if rels.is_empty() {
        return;
    }
    let (root, base, project_id, jobs) = {
        let guard = state.lock().unwrap();
        let Some(active) = guard.active.as_ref() else {
            return;
        };
        (
            active.project.root.clone(),
            guard.base_dir.clone(),
            active.project.config.id,
            active.transcripts.clone(),
        )
    };
    let Some(ffmpeg) = transcript::discover_ffmpeg() else {
        return; // no ffmpeg → quiet no-op
    };
    let model = model::selected_model_path(&base, model::ModelCategory::Transcription)
        .unwrap_or_else(|| transcript::model_path(&base));
    if !model.is_file() {
        return; // no model → quiet no-op
    }
    for rel in rels {
        spawn_transcription(app, TranscriptionJob {
            state: state.clone(),
            root: root.clone(),
            project_id,
            jobs: jobs.clone(),
            ffmpeg: ffmpeg.clone(),
            model: model.clone(),
            rel_path: rel.clone(),
            quiet: true,
        });
    }
}

/// Run one transcription off every lock (ffmpeg + Whisper are slow, like a
/// cloud hydrate), then re-index the video under the lock so its transcript
/// becomes searchable. Deduplicated by `in_flight`; a failed automatic job is
/// remembered in `attempted` so it isn't retried on every scan.
fn spawn_transcription(app: &AppHandle, job: TranscriptionJob) {
    {
        let mut j = job.jobs.lock().unwrap();
        if j.in_flight.contains(&job.rel_path) {
            return; // already transcribing this video
        }
        if job.quiet && j.attempted.contains(&job.rel_path) {
            return; // an earlier automatic attempt failed; don't thrash
        }
        j.in_flight.insert(job.rel_path.clone());
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let result =
            transcript::generate_and_cache(&job.ffmpeg, &job.model, &job.root, &job.rel_path);
        match result {
            Ok(_) => {
                // Re-index the video so the fresh transcript is searchable.
                let mut guard = job.state.lock().unwrap();
                if let Some(active) = guard.active.as_mut() {
                    if active.project.config.id == job.project_id {
                        let _ = scan::refresh_path(&active.project, &mut active.db, &job.rel_path);
                    }
                }
                drop(guard);
                let _ = app.emit("index-updated", ScanStats::default());
            }
            Err(e) => {
                job.jobs.lock().unwrap().attempted.insert(job.rel_path.clone());
                if !job.quiet {
                    let _ = app.emit("transcript-error", e.to_string());
                }
            }
        }
        job.jobs.lock().unwrap().in_flight.remove(&job.rel_path);
    });
}

// ---------- offline model download ----------

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ModelStatusDto {
    id: String,
    name: String,
    installed: bool,
    size_bytes: Option<u64>,
    expected_bytes: u64,
    /// The recommended default, pre-selected in the UI.
    recommended: bool,
    /// "transcription" | "language"
    category: String,
    /// "recommended" | "advanced"
    tier: String,
    blurb: String,
    /// Whether this is the selected model for its category.
    selected: bool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ModelProgress {
    id: String,
    downloaded: u64,
    total: u64,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ModelError {
    id: String,
    message: String,
}

fn status_dto(
    base_dir: &std::path::Path,
    entry: &model::CatalogEntry,
    selected_id: &str,
) -> ModelStatusDto {
    let size = model::installed_size(base_dir, &entry.spec);
    ModelStatusDto {
        id: entry.spec.id.clone(),
        name: entry.spec.name.clone(),
        installed: size.is_some(),
        size_bytes: size,
        expected_bytes: entry.spec.expected_bytes,
        recommended: entry.spec.recommended,
        category: match entry.category {
            model::ModelCategory::Transcription => "transcription".into(),
            model::ModelCategory::Language => "language".into(),
        },
        tier: match entry.tier {
            model::ModelTier::Recommended => "recommended".into(),
            model::ModelTier::Advanced => "advanced".into(),
        },
        blurb: entry.blurb.to_string(),
        selected: entry.spec.id == selected_id,
    }
}

/// Status of the recommended model only — cheap and offline (just a file
/// check), so the transcript feature can gate on it without a network round
/// trip.
#[tauri::command]
fn model_status(state: State<SharedState>) -> CmdResult<ModelStatusDto> {
    let base = { state.lock().unwrap().base_dir.clone() };
    let rec = model::catalog()
        .into_iter()
        .find(|e| e.category == model::ModelCategory::Transcription && e.spec.recommended)
        .expect("recommended transcription model");
    let selected_id = model::selected(&base, model::ModelCategory::Transcription).id;
    Ok(status_dto(&base, &rec, &selected_id))
}

/// All curated models available to download, in catalog order. Fully offline
/// (just file checks): each entry carries its category, tier, blurb, and whether
/// it is the selected model for its category.
#[tauri::command]
fn list_models(state: State<SharedState>) -> CmdResult<Vec<ModelStatusDto>> {
    let base = { state.lock().unwrap().base_dir.clone() };
    let sel_trans = model::selected(&base, model::ModelCategory::Transcription).id;
    // The language selection is resolved lazily, per language entry: while the
    // language catalog is empty `model::selected(_, Language)` has no recommended
    // fallback and would panic, so we never call it until a language entry exists.
    Ok(model::catalog()
        .iter()
        .map(|e| {
            let selected_id = match e.category {
                model::ModelCategory::Transcription => sel_trans.clone(),
                model::ModelCategory::Language => {
                    model::selected(&base, model::ModelCategory::Language).id
                }
            };
            status_dto(&base, e, &selected_id)
        })
        .collect())
}

/// Start downloading a model. Returns immediately; progress and completion flow
/// through the `model-download-progress` event (completion = a final 100%
/// sample), failures through `model-download-error`. A second concurrent
/// download of the same id is refused. The download itself streams to a temp
/// file, verifies, and atomically installs — all off the global lock, on its
/// own thread (like `spawn_transcription`).
#[tauri::command]
fn download_model(app: AppHandle, state: State<SharedState>, id: String) -> CmdResult<()> {
    let (base, downloads) = {
        let guard = state.lock().unwrap();
        (guard.base_dir.clone(), guard.model_downloads.clone())
    };
    {
        let mut in_flight = downloads.lock().unwrap();
        if !in_flight.insert(id.clone()) {
            return Err("This model is already downloading.".into());
        }
    }

    let app = app.clone();
    std::thread::spawn(move || {
        // The curated catalog resolves every downloadable model offline.
        let Some(spec) = model::find_spec(&id) else {
            let _ = app.emit(
                "model-download-error",
                ModelError { id: id.clone(), message: "Unknown model.".into() },
            );
            downloads.lock().unwrap().remove(&id);
            return;
        };

        let mut throttle = model::ProgressThrottle::new();
        let start = Instant::now();
        let progress_app = app.clone();
        let progress_id = id.clone();
        let on_progress = |downloaded: u64, total: u64| {
            let now_ms = start.elapsed().as_millis() as u64;
            // Always emit the terminal 100% (the completion signal); throttle
            // the rest so a fast download doesn't flood the UI.
            let done = total > 0 && downloaded >= total;
            if done || throttle.should_emit(downloaded, total, now_ms) {
                let _ = progress_app.emit(
                    "model-download-progress",
                    ModelProgress { id: progress_id.clone(), downloaded, total },
                );
            }
        };

        // Installed size is read from disk on the next status/list call, so a
        // successful install needs no cache mutation here.
        match model::download_to(&model::HttpSource, &spec, &base, on_progress) {
            Ok(()) => {
                // A newly installed Language model clears any cached LLM load
                // error and re-arms the engine build (cheap; no-op before the
                // service spawns). Transcription installs don't touch the LLM.
                let is_language = model::category_specs(model::ModelCategory::Language)
                    .iter()
                    .any(|s| s.id == spec.id);
                if is_language {
                    ken_core::local_llm::notify_model_installed();
                }
            }
            Err(e) => {
                let _ = app.emit(
                    "model-download-error",
                    ModelError { id: id.clone(), message: e.to_string() },
                );
            }
        }
        downloads.lock().unwrap().remove(&id);
    });
    Ok(())
}

/// Delete an installed model file. Missing is a no-op.
#[tauri::command]
fn remove_model(state: State<SharedState>, id: String) -> CmdResult<()> {
    let base = { state.lock().unwrap().base_dir.clone() };
    // Prefer the catalog spec; fall back to a minimal one so an unknown id (e.g.
    // a stale install) can still be removed by file name.
    let spec = model::find_spec(&id).unwrap_or_else(|| model::ModelSpec {
        id: id.clone(),
        name: id.clone(),
        file: id.clone(),
        url: String::new(),
        expected_bytes: 0,
        recommended: false,
    });
    model::remove(&base, &spec).map_err(err)
}

/// Persist the user's chosen model for a category ("transcription" | "language").
#[tauri::command]
fn set_model_selection(state: State<SharedState>, category: String, id: String) -> CmdResult<()> {
    let base = { state.lock().unwrap().base_dir.clone() };
    let cat = match category.as_str() {
        "transcription" => model::ModelCategory::Transcription,
        "language" => model::ModelCategory::Language,
        other => return Err(format!("unknown model category: {other}")),
    };
    model::set_selected(&base, cat, &id).map_err(err)?;
    if cat == model::ModelCategory::Language {
        // Switching 4B↔8B: rebuild the engine with the newly selected file on
        // the next job (cheap flag flip; no-op before the service spawns).
        ken_core::local_llm::notify_model_installed();
    }
    Ok(())
}

// ---------- ingest commands ----------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IngestSummary {
    entry: RecipeEntry,
    last_run: Option<RunRow>,
    resolved_rules: Option<ResolvedRules>,
    stale: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IngestDetail {
    recipe: Recipe,
    runs: Vec<RunRow>,
    resolved_rules: ResolvedRules,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct IngestForm {
    slug: Option<String>,
    name: String,
    #[serde(default)]
    description: String,
    instruction: String,
    #[serde(default)]
    sources: Vec<String>,
    output: String,
    mode: Mode,
    refresh: Refresh,
    #[serde(default)]
    rules: Option<RulesOverride>,
}

fn kebab(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() { "ingest".into() } else { trimmed }
}

#[tauri::command]
fn list_ingests(state: State<SharedState>) -> CmdResult<Vec<IngestSummary>> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    let now = engine::now_epoch();
    let mut out = Vec::new();
    for entry in recipe::list(&active.project).map_err(err)? {
        let (last_run, rules, stale) = match &entry {
            RecipeEntry::Ok { recipe: r } => {
                let last = active.db.list_runs(&r.slug, 1).map_err(err)?.into_iter().next();
                let rules = recipe::resolve_rules(r, &active.project);
                let stale = last
                    .as_ref()
                    .filter(|l| l.status == "fresh")
                    .map(|l| now - l.started_at > rules.stale_days as i64 * 86_400)
                    .unwrap_or(false);
                (last, Some(rules), stale)
            }
            RecipeEntry::Broken { .. } => (None, None, false),
        };
        out.push(IngestSummary { entry, last_run, resolved_rules: rules, stale });
    }
    Ok(out)
}

#[tauri::command]
fn get_ingest(state: State<SharedState>, slug: String) -> CmdResult<IngestDetail> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    let recipe = recipe::load_slug(&active.project, &slug).map_err(err)?;
    let runs = active.db.list_runs(&slug, 20).map_err(err)?;
    let resolved_rules = recipe::resolve_rules(&recipe, &active.project);
    Ok(IngestDetail { recipe, runs, resolved_rules })
}

#[tauri::command]
fn save_ingest(state: State<SharedState>, form: IngestForm) -> CmdResult<Recipe> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    let slug = match form.slug {
        Some(s) => s,
        None => {
            // New ingest: derive a unique slug from the name.
            let base = kebab(&form.name);
            let mut slug = base.clone();
            let mut n = 2;
            while recipe::recipe_path(&active.project.root, &slug).exists() {
                slug = format!("{base}-{n}");
                n += 1;
            }
            slug
        }
    };
    // Editing goes through the loaded recipe so unknown frontmatter fields
    // survive; a fresh slug builds from scratch.
    let mut recipe = recipe::load_slug(&active.project, &slug).unwrap_or_else(|_| {
        Recipe::build(
            slug.clone(),
            String::new(),
            String::new(),
            Vec::new(),
            String::from("out.md"),
            form.mode,
            form.refresh,
            None,
            String::from("-"),
        )
    });
    recipe.update_from_form(
        form.name,
        form.description,
        form.sources,
        form.output,
        form.mode,
        form.refresh,
        form.rules,
        form.instruction,
    );
    recipe::save(&active.project, &recipe).map_err(err)?;
    let recipe = recipe::load_slug(&active.project, &recipe.slug).map_err(err)?;
    Ok(recipe)
}

#[tauri::command]
fn delete_ingest(state: State<SharedState>, slug: String) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    recipe::delete(&active.project, &slug).map_err(err)
}

#[tauri::command]
fn run_ingest(state: State<SharedState>, slug: String, full: Option<bool>) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    active.engine.trigger(&slug, full.unwrap_or(true));
    Ok(())
}

#[tauri::command]
fn cancel_run(state: State<SharedState>, slug: String) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    active.engine.cancel(&slug);
    Ok(())
}

/// Tell listeners (ingests + review stores) a run changed outside the
/// engine — approvals and discards resolve runs from a command, not a run
/// thread, so the engine never emits for them.
fn emit_run_changed(app: &AppHandle, db: &Db, run_id: i64) {
    if let Ok(Some(run)) = db.get_run(run_id) {
        let _ = app.emit(
            "ingest-run-changed",
            IngestEvent::at(&run.kind, &run.slug, run_id, run.session_id, &run.status, run.summary),
        );
    }
}

#[tauri::command]
fn approve_run(app: AppHandle, state: State<SharedState>, run_id: i64) -> CmdResult<()> {
    let mut guard = state.lock().unwrap();
    let active = guard.active.as_mut().ok_or("no project open")?;
    engine::approve_run(&active.project, &mut active.db, run_id).map_err(err)?;
    // Applied files land on disk; index them promptly.
    let _ = scan::scan(&active.project, &mut active.db);
    emit_run_changed(&app, &active.db, run_id);
    Ok(())
}

#[tauri::command]
fn discard_run(app: AppHandle, state: State<SharedState>, run_id: i64) -> CmdResult<()> {
    let mut guard = state.lock().unwrap();
    let active = guard.active.as_mut().ok_or("no project open")?;
    engine::discard_run(&active.project, &mut active.db, run_id).map_err(err)?;
    emit_run_changed(&app, &active.db, run_id);
    Ok(())
}

#[tauri::command]
fn pending_approvals(state: State<SharedState>) -> CmdResult<Vec<RunRow>> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    active.db.runs_with_status("pending_approval").map_err(err)
}

// ---------- review inbox ----------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InboxItem {
    /// Kind-prefixed and stable across refreshes: "run-12", "stale-people",
    /// "file-notes/x.pdf", "broken-people", "item-3".
    id: String,
    /// `approval` | `stale` | `failed-file` | `broken-recipe` | `stored`
    /// | `conflict` | `conflict-copy`
    kind: String,
    title: String,
    body: String,
    when: i64,
    source_ref: String,
    /// Kind-specific JSON for stored items (conflict versions, copy paths).
    payload: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReviewInbox {
    items: Vec<InboxItem>,
    done: Vec<InboxItem>,
}

/// Sort key mirroring the prototype's top-down severity order.
fn inbox_rank(kind: &str) -> u8 {
    match kind {
        "approval" => 0,
        "conflict" | "conflict-copy" => 1,
        "stored" => 2,
        "broken-recipe" => 3,
        "failed-file" => 4,
        _ => 5, // stale
    }
}

/// Stored items carry their real kind when the frontend knows it
/// (conflicts render their own detail); anything else stays generic.
fn stored_kind(kind: &str) -> String {
    match kind {
        "conflict" | "conflict-copy" => kind.to_string(),
        _ => "stored".to_string(),
    }
}

/// The unified Review inbox, assembled at read time: pending approvals,
/// stale ingests, failed files, and broken recipes stay derived from their
/// own sources of truth; stored review items are merged in. `done` is the
/// last 7 days of resolved items.
#[tauri::command]
fn review_inbox(state: State<SharedState>) -> CmdResult<ReviewInbox> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    let now = engine::now_epoch();
    let mut items: Vec<InboxItem> = Vec::new();

    // Per-user, per-project ignores (app-data, never synced): a file the user
    // silenced drops out of the inbox entirely, so the list AND the live badge
    // count reflect the choice. The file stays indexed — only its nag is hidden.
    let ignored = UserState::load(&guard.base_dir, active.project.config.id).ignored;

    // One walk over the recipes yields stale + broken items and the
    // name/threshold lookups the run-based items need.
    let mut names: std::collections::HashMap<String, String> = Default::default();
    let mut thresholds: std::collections::HashMap<String, f64> = Default::default();
    for entry in recipe::list(&active.project).map_err(err)? {
        match entry {
            RecipeEntry::Ok { recipe: r } => {
                let rules = recipe::resolve_rules(&r, &active.project);
                names.insert(r.slug.clone(), r.name.clone());
                thresholds.insert(r.slug.clone(), rules.review_threshold_pct as f64 / 100.0);
                // Same staleness derivation as list_ingests.
                let last = active.db.list_runs(&r.slug, 1).map_err(err)?.into_iter().next();
                if let Some(last) = last.filter(|l| l.status == "fresh") {
                    if now - last.started_at > rules.stale_days as i64 * 86_400 {
                        items.push(InboxItem {
                            id: format!("stale-{}", r.slug),
                            kind: "stale".into(),
                            title: format!("{} may be out of date", r.name),
                            body: format!(
                                "{} hasn't been refreshed in over {} days. Run it to check for drift, or leave it if nothing has changed.",
                                r.output, rules.stale_days
                            ),
                            when: last.started_at,
                            source_ref: r.slug.clone(),
                            payload: None,
                        });
                    }
                }
            }
            RecipeEntry::Broken { error } => {
                let when = recipe::recipe_path(&active.project.root, &error.slug)
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(now);
                items.push(InboxItem {
                    id: format!("broken-{}", error.slug),
                    kind: "broken-recipe".into(),
                    title: format!("The {} recipe has a problem", error.slug),
                    body: format!(
                        "Ken can't read this ingest recipe, so it won't run — {}. Open it in Ingests to fix or recreate it.",
                        error.reason
                    ),
                    when,
                    source_ref: error.slug,
                    payload: None,
                });
            }
        }
    }

    for run in active.db.runs_with_status("pending_approval").map_err(err)? {
        let name = names.get(&run.slug).cloned().unwrap_or_else(|| run.slug.clone());
        let held = run
            .summary
            .clone()
            .unwrap_or_else(|| "A large update is staged.".into());
        items.push(InboxItem {
            id: format!("run-{}", run.id),
            kind: "approval".into(),
            title: format!("Large refresh — {name}"),
            body: format!(
                "{held}\n\nApprove to write the update, or discard to keep the document as it is."
            ),
            when: run.finished_at.unwrap_or(run.started_at),
            source_ref: run.slug,
            payload: None,
        });
    }

    for f in active.db.list_files().map_err(err)? {
        if f.status != "failed" {
            continue;
        }
        if ignored.contains(&f.rel_path) {
            continue;
        }
        let file_name = f.rel_path.rsplit('/').next().unwrap_or(&f.rel_path).to_string();
        items.push(InboxItem {
            id: format!("file-{}", f.rel_path),
            kind: "failed-file".into(),
            title: format!("{file_name} couldn't be read"),
            body: format!(
                "Ken couldn't get text out of this file, so its contents aren't searchable — {}. It's still findable by name; open it in Files for the details.",
                f.error.as_deref().unwrap_or("the reason is unknown")
            ),
            when: f.mtime,
            source_ref: f.rel_path,
            payload: None,
        });
    }

    for it in active.db.list_open_review_items().map_err(err)? {
        let kind = stored_kind(&it.kind);
        // Conflicts/stored items reference a file in `source_ref`; skip the ones
        // whose file the user ignored. Slug-based kinds carry no file ref.
        if user_state::inbox_item_ignored(&kind, &it.source_ref, &ignored) {
            continue;
        }
        items.push(InboxItem {
            id: format!("item-{}", it.id),
            kind,
            title: it.title,
            body: it.body,
            when: it.created_at,
            source_ref: it.source_ref,
            payload: it.payload,
        });
    }

    items.sort_by(|a, b| {
        inbox_rank(&a.kind)
            .cmp(&inbox_rank(&b.kind))
            .then(b.when.cmp(&a.when))
    });

    // Done: the simplest honest cut — discarded runs (only reachable from
    // pending_approval), fresh runs whose ratio exceeded their threshold
    // (i.e. held-then-approved; a first full build can also land here),
    // and resolved stored items.
    let since = now - 7 * 86_400;
    let default_threshold = recipe::DEFAULT_RULES.review_threshold_pct as f64 / 100.0;
    let mut done: Vec<InboxItem> = Vec::new();
    for run in active.db.runs_finished_since(since).map_err(err)? {
        let threshold = thresholds.get(&run.slug).copied().unwrap_or(default_threshold);
        let what = match run.status.as_str() {
            "discarded" => "Refresh discarded",
            "fresh" if run.change_ratio.is_some_and(|r| r > threshold) => "Large refresh applied",
            _ => continue,
        };
        let name = names.get(&run.slug).cloned().unwrap_or_else(|| run.slug.clone());
        done.push(InboxItem {
            id: format!("run-{}", run.id),
            kind: "approval".into(),
            title: format!("{what} — {name}"),
            body: run.summary.clone().unwrap_or_default(),
            when: run.finished_at.unwrap_or(run.started_at),
            source_ref: run.slug,
            payload: None,
        });
    }
    for it in active.db.list_recent_resolved_review_items(since).map_err(err)? {
        done.push(InboxItem {
            id: format!("item-{}", it.id),
            kind: stored_kind(&it.kind),
            title: it.title,
            body: it.body,
            when: it.resolved_at.unwrap_or(it.created_at),
            source_ref: it.source_ref,
            payload: None,
        });
    }
    done.sort_by(|a, b| b.when.cmp(&a.when));

    Ok(ReviewInbox { items, done })
}

#[tauri::command]
fn resolve_review_item(state: State<SharedState>, id: i64) -> CmdResult<()> {
    let mut guard = state.lock().unwrap();
    let active = guard.active.as_mut().ok_or("no project open")?;
    active
        .db
        .resolve_review_item(id, engine::now_epoch())
        .map_err(err)
}

// ---------- ignored files (user-level, never synced) ----------

/// Load the current project's private user-state from app-data. Helper for the
/// ignore commands so each one reads/writes the same non-synced file.
fn load_user_state(guard: &AppState) -> CmdResult<(std::path::PathBuf, uuid::Uuid, UserState)> {
    let active = guard.active.as_ref().ok_or("no project open")?;
    let base = guard.base_dir.clone();
    let id = active.project.config.id;
    let us = UserState::load(&base, id);
    Ok((base, id, us))
}

/// Silence a file's review issues for THIS user only (stored in app-data, never
/// written to the synced `.ken/` config). The file stays indexed and findable.
#[tauri::command]
fn ignore_file(state: State<SharedState>, rel_path: String) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let (base, id, mut us) = load_user_state(&guard)?;
    if us.ignore(rel_path) {
        us.save(&base, id).map_err(err)?;
    }
    Ok(())
}

/// Reverse an ignore, so the file's issues can surface again.
#[tauri::command]
fn unignore_file(state: State<SharedState>, rel_path: String) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let (base, id, mut us) = load_user_state(&guard)?;
    if us.unignore(&rel_path) {
        us.save(&base, id).map_err(err)?;
    }
    Ok(())
}

/// The current project's ignored files, for the Settings undo list and the
/// home "Needs a look" filter.
#[tauri::command]
fn list_ignored(state: State<SharedState>) -> CmdResult<Vec<String>> {
    let guard = state.lock().unwrap();
    let (_, _, us) = load_user_state(&guard)?;
    Ok(us.ignored.into_iter().collect())
}

/// The index's files as `(rel_path, (size, mtime))` version pairs — the shape
/// the unread computation consumes.
fn index_versions(files: &[FileRow]) -> Vec<(String, (i64, i64))> {
    files
        .iter()
        .map(|f| (f.rel_path.clone(), (f.size, f.mtime)))
        .collect()
}

/// Files changed by someone/something ELSE since the user last looked (the nav
/// dot + the Files "unread" filter). Self-saves and opens keep files seen, so
/// what remains is external edits, syncs, and cloud hydrates.
#[tauri::command]
fn unread_files(state: State<SharedState>) -> CmdResult<Vec<String>> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    let files = active.db.list_files().map_err(err)?;
    let (base, id, mut us) = load_user_state(&guard)?;
    let index = index_versions(&files);
    // Defensive baseline: activate() normally does this, but guarantee it so a
    // never-baselined project reports empty rather than its entire tree.
    if us.baseline(&index) {
        us.save(&base, id).map_err(err)?;
    }
    Ok(us.unread(&index))
}

/// Record a file as seen at its current version (the frontend calls this on
/// open, and it backs the "Mark as viewed" context-menu item).
#[tauri::command]
fn mark_seen(state: State<SharedState>, rel_path: String) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    let Some(row) = active.db.get_file(&rel_path).map_err(err)? else {
        return Ok(()); // not indexed (yet) — nothing to mark
    };
    let (base, id, mut us) = load_user_state(&guard)?;
    if us.mark_seen(rel_path, (row.size, row.mtime)) {
        us.save(&base, id).map_err(err)?;
    }
    Ok(())
}

/// Mark every currently-unread file seen ("Mark all as viewed").
#[tauri::command]
fn mark_all_seen(state: State<SharedState>) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    let files = active.db.list_files().map_err(err)?;
    let (base, id, mut us) = load_user_state(&guard)?;
    if us.mark_all_seen(&index_versions(&files)) {
        us.save(&base, id).map_err(err)?;
    }
    Ok(())
}

// ---------- sync ----------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SyncStatus {
    /// `git` | `drive`
    mode: String,
    auto: bool,
    /// Whether automatic pull/push is actually running (git + remote + auto).
    active: bool,
    remote: Option<String>,
    branch: Option<String>,
}

fn sync_status_of(project: &Project) -> SyncStatus {
    let is_git = sync::is_git_repo(&project.root);
    let (remote, branch) = if is_git {
        sync::remote_and_branch(&project.root)
    } else {
        (None, None)
    };
    let auto = sync::sync_auto(project);
    SyncStatus {
        mode: if is_git { "git" } else { "drive" }.into(),
        auto,
        active: is_git && auto && remote.is_some(),
        remote,
        branch,
    }
}

#[tauri::command]
fn sync_status(state: State<SharedState>) -> CmdResult<SyncStatus> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    Ok(sync_status_of(&active.project))
}

#[tauri::command]
fn set_sync_auto(app: AppHandle, state: State<SharedState>, auto: bool) -> CmdResult<SyncStatus> {
    let mut guard = state.lock().unwrap();
    let active = guard.active.as_mut().ok_or("no project open")?;
    let mut obj = active
        .project
        .config
        .extra
        .get("sync")
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    obj.insert("auto".into(), serde_json::Value::Bool(auto));
    active
        .project
        .config
        .extra
        .insert("sync".into(), serde_json::Value::Object(obj));
    active.project.save().map_err(err)?;

    let status = sync_status_of(&active.project);
    // Reflect the toggle in the dot immediately; a fresh pull confirms.
    let _ = app.emit("sync-state", SyncStateEvent {
        state: if status.active { "synced" } else { "off" }.into(),
        detail: None,
    });
    if status.active {
        active.sync.pull_now();
    }
    Ok(status)
}

#[tauri::command]
fn sync_now(state: State<SharedState>) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    active.sync.sync_now();
    Ok(())
}

/// Payload of a `conflict` review item, parsed for command use.
fn conflict_payload(item: &ken_core::db::ReviewItemRow) -> CmdResult<serde_json::Value> {
    item.payload
        .as_deref()
        .and_then(|p| serde_json::from_str(p).ok())
        .ok_or_else(|| "this item has no conflict details".to_string())
}

/// Resolve a merge-conflict review item: write the chosen content to the
/// project file, resolve the item, reindex — the normal sync path pushes
/// it out. Returns the project-relative path that was written.
#[tauri::command]
fn resolve_conflict(
    app: AppHandle,
    state: State<SharedState>,
    item_id: i64,
    resolution: String,
    content: Option<String>,
) -> CmdResult<String> {
    let mut guard = state.lock().unwrap();
    let active = guard.active.as_mut().ok_or("no project open")?;
    let item = active
        .db
        .get_review_item(item_id)
        .map_err(err)?
        .ok_or("item not found")?;
    if item.kind != "conflict" || item.status != "open" {
        return Err("this item isn't an open conflict".into());
    }
    let payload = conflict_payload(&item)?;
    let path = payload["path"]
        .as_str()
        .ok_or("conflict details are missing the file path")?
        .to_string();
    let ours = payload["ours"].as_str().unwrap_or_default();
    let theirs = payload["theirs"].as_str().unwrap_or_default();
    let draft = payload["draft"].as_str();
    let chosen: String = match resolution.as_str() {
        // Accept Ken's merge; fall back to the user's version when no
        // draft exists (also the "edit manually" starting point).
        "accept-draft" => draft.unwrap_or(ours).to_string(),
        "keep-mine" => ours.to_string(),
        "take-theirs" => theirs.to_string(),
        "manual" => content.ok_or("manual resolution needs content")?,
        _ => return Err("unknown resolution".into()),
    };

    let abs = active.project.resolve(&path).map_err(err)?;
    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent).map_err(err)?;
    }
    std::fs::write(&abs, &chosen).map_err(err)?;
    scan::refresh_path(&active.project, &mut active.db, &path).map_err(err)?;
    active
        .db
        .resolve_review_item(item_id, engine::now_epoch())
        .map_err(err)?;
    active.sync.changed(vec![path.clone()]);
    let _ = app.emit("file-saved", &path);
    let _ = app.emit("review-changed", ());
    Ok(path)
}

/// Resolve a shared-drive conflicted-copy item: `keep-copy` promotes the
/// copy's content to the original name, `keep-original` deletes the copy.
/// Returns the path of the surviving file.
#[tauri::command]
fn resolve_conflict_copy(
    app: AppHandle,
    state: State<SharedState>,
    item_id: i64,
    resolution: String,
) -> CmdResult<String> {
    let mut guard = state.lock().unwrap();
    let active = guard.active.as_mut().ok_or("no project open")?;
    let item = active
        .db
        .get_review_item(item_id)
        .map_err(err)?
        .ok_or("item not found")?;
    if item.kind != "conflict-copy" || item.status != "open" {
        return Err("this item isn't an open conflicting copy".into());
    }
    let payload = conflict_payload(&item)?;
    let copy_rel = payload["copyPath"]
        .as_str()
        .ok_or("details are missing the copy's path")?
        .to_string();
    let orig_rel = payload["originalPath"].as_str().map(String::from);
    let copy_abs = active.project.resolve(&copy_rel).map_err(err)?;

    let survivor = match resolution.as_str() {
        "keep-copy" => {
            // Promote the copy to the original name (derive it when the
            // original no longer exists).
            let target_rel = match orig_rel {
                Some(o) => o,
                None => {
                    let file_name = copy_rel.rsplit('/').next().unwrap_or(&copy_rel);
                    let original = sync::conflicted_copy_original(file_name)
                        .ok_or("couldn't work out the original name")?;
                    match copy_rel.rsplit_once('/') {
                        Some((dir, _)) => format!("{dir}/{original}"),
                        None => original,
                    }
                }
            };
            let target_abs = active.project.resolve(&target_rel).map_err(err)?;
            std::fs::rename(&copy_abs, &target_abs).or_else(|_| {
                std::fs::copy(&copy_abs, &target_abs)
                    .and_then(|_| std::fs::remove_file(&copy_abs))
            })
            .map_err(err)?;
            scan::refresh_path(&active.project, &mut active.db, &target_rel).map_err(err)?;
            target_rel
        }
        "keep-original" => {
            if copy_abs.is_file() {
                std::fs::remove_file(&copy_abs).map_err(err)?;
            }
            orig_rel.unwrap_or_else(|| copy_rel.clone())
        }
        _ => return Err("unknown resolution".into()),
    };
    scan::refresh_path(&active.project, &mut active.db, &copy_rel).map_err(err)?;
    active
        .db
        .resolve_review_item(item_id, engine::now_epoch())
        .map_err(err)?;
    active.sync.changed(vec![survivor.clone(), copy_rel]);
    let _ = app.emit("review-changed", ());
    Ok(survivor)
}

#[tauri::command]
fn set_ingest_runner_mode(state: State<SharedState>, mode: String) -> CmdResult<()> {
    let mut guard = state.lock().unwrap();
    let active = guard.active.as_mut().ok_or("no project open")?;
    if mode != "hidden-tui" && mode != "headless" {
        return Err("unknown runner mode".into());
    }
    active
        .project
        .config
        .extra
        .insert("ingestRunner".into(), serde_json::Value::String(mode));
    active.project.save().map_err(err)
}

/// Is background download-and-index of cloud-offline documents enabled?
#[tauri::command]
fn get_background_index(state: State<SharedState>) -> CmdResult<bool> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    Ok(ken_core::bg_hydrate::background_index_enabled(&active.project))
}

/// Turn background cloud indexing on or off. Persisted in `project.json`; the
/// always-running worker reads this each tick, so it idles when off and picks
/// the backlog straight back up when on — no thread to restart.
#[tauri::command]
fn set_background_index(state: State<SharedState>, enabled: bool) -> CmdResult<()> {
    let mut guard = state.lock().unwrap();
    let active = guard.active.as_mut().ok_or("no project open")?;
    active
        .project
        .config
        .extra
        .insert("backgroundIndex".into(), serde_json::Value::Bool(enabled));
    active.project.save().map_err(err)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeDoctor {
    found: bool,
    path: Option<String>,
    version: Option<String>,
    help: String,
}

#[tauri::command]
fn claude_doctor() -> CmdResult<ClaudeDoctor> {
    match ken_core::runner::discover_claude() {
        Some(path) => {
            let version = std::process::Command::new(&path)
                .arg("--version")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
            Ok(ClaudeDoctor {
                found: true,
                path: Some(path.to_string_lossy().into_owned()),
                version,
                help: String::new(),
            })
        }
        None => Ok(ClaudeDoctor {
            found: false,
            path: None,
            version: None,
            help: ken_core::runner::MISSING_CLAUDE_HELP.to_string(),
        }),
    }
}


// ---------- daily digest + quick answer ----------

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct DigestDto {
    /// Local calendar day, `yyyy-mm-dd`.
    date: String,
    body: String,
    sources: Vec<String>,
    generated_at: i64,
}

fn digest_dto(row: &DigestRow) -> DigestDto {
    let parsed = digest::parse_digest(&row.content);
    DigestDto {
        date: row.date.clone(),
        body: parsed.body,
        sources: parsed.sources,
        generated_at: row.created_at,
    }
}

fn local_date_today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

fn local_hour() -> u32 {
    use chrono::Timelike;
    chrono::Local::now().hour()
}

/// The cheap digest check, run on activate, window focus, and (with
/// `force`) from the refresh command: today already digested → nothing;
/// otherwise past 07:00 local with Claude installed and nothing in
/// flight → generate in the background, store, and emit
/// `digest-updated`. A day with nothing to report stores the quiet
/// fallback without calling Claude.
fn maybe_generate_digest(app: &AppHandle, state: &SharedState, force: bool) -> CmdResult<()> {
    let mut guard = state.lock().unwrap();
    let Some(active) = guard.active.as_mut() else {
        return Ok(());
    };
    let today = local_date_today();
    if !force {
        if active.db.get_digest(&today).map_err(err)?.is_some() {
            return Ok(()); // already written today
        }
        if local_hour() < 7 {
            return Ok(()); // the digest is a morning thing
        }
    }
    let running = active.digest_running.clone();
    if running.swap(true, Ordering::SeqCst) {
        return if force {
            Err("Today's digest is already being written.".into())
        } else {
            Ok(())
        };
    }
    // Everything below must clear the flag on early return.
    let done = |r: &Arc<AtomicBool>| r.store(false, Ordering::SeqCst);

    let since = engine::now_epoch() - 86_400;
    let sources = match digest::gather(&active.db, since) {
        Ok(s) => s,
        Err(e) => {
            done(&running);
            return Err(err(e));
        }
    };
    if sources.is_quiet() {
        // Nothing happened — say so honestly, no AI call.
        let now = engine::now_epoch();
        active
            .db
            .upsert_digest(&today, digest::QUIET_DIGEST, now)
            .map_err(err)?;
        done(&running);
        if let Ok(Some(row)) = active.db.get_digest(&today) {
            let _ = app.emit("digest-updated", digest_dto(&row));
        }
        return Ok(());
    }
    let Some(binary) = ken_core::runner::discover_claude() else {
        done(&running);
        return if force {
            Err(ken_core::runner::MISSING_CLAUDE_HELP.into())
        } else {
            Ok(())
        };
    };

    let prompt = digest::compose_digest_prompt(&active.project.config.name, &sources);
    let root = active.project.root.clone();
    let project_id = active.project.config.id;
    let base = guard.base_dir.clone();
    drop(guard);

    let thread_app = app.clone();
    let thread_state = state.clone();
    let _ = app.emit("digest-generating", ());
    std::thread::spawn(move || {
        let outcome = assistant::oneshot(
            &binary,
            &root,
            &prompt,
            Duration::from_secs(180),
            &CancelToken::new(),
        );
        // If the user switched projects mid-write, store quietly but
        // don't repaint the new project's Home with the old digest.
        let still_active = || {
            let guard = thread_state.lock().unwrap();
            guard
                .active
                .as_ref()
                .is_some_and(|a| a.project.config.id == project_id)
        };
        match outcome {
            Ok(OneshotOutcome::Completed(text)) => {
                if let Ok(mut db) = Db::open(&base, project_id) {
                    let _ = db.upsert_digest(&today, &text, engine::now_epoch());
                    if let Ok(Some(row)) = db.get_digest(&today) {
                        if still_active() {
                            let _ = thread_app.emit("digest-updated", digest_dto(&row));
                        }
                    }
                }
            }
            Ok(OneshotOutcome::TimedOut) if still_active() => {
                let _ = thread_app.emit(
                    "digest-error",
                    "Writing the digest took too long and was stopped — it'll try again later.",
                );
            }
            Ok(OneshotOutcome::Failed(detail)) if still_active() => {
                let _ = thread_app.emit("digest-error", detail);
            }
            Err(e) => {
                let _ = thread_app.emit("digest-error", e.to_string());
            }
            _ => {}
        }
        running.store(false, Ordering::SeqCst);
    });
    Ok(())
}

/// Today's digest, parsed for the Home card. None until it's written.
#[tauri::command]
fn current_digest(state: State<SharedState>) -> CmdResult<Option<DigestDto>> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    Ok(active
        .db
        .get_digest(&local_date_today())
        .map_err(err)?
        .map(|row| digest_dto(&row)))
}

/// Force-regenerate today's digest ("Write it now" / re-run).
#[tauri::command]
fn refresh_digest(app: AppHandle, state: State<SharedState>) -> CmdResult<()> {
    maybe_generate_digest(&app, state.inner(), true)
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct QuickAnswerEvent {
    query: String,
    body: String,
    sources: Vec<String>,
}

/// One streamed chunk of a local quick answer, tied to its query so the
/// overlay can drop deltas from a superseded query.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct QuickAnswerDelta {
    query: String,
    delta: String,
}

/// Build the local-model quick-answer prompt: the same FTS grounding as the
/// Claude path, wrapped in Qwen3 ChatML, ending with the `SOURCES:` convention
/// `digest::parse_digest` already understands.
fn local_quick_answer_prompt(query: &str, hits: &[SearchHit]) -> String {
    let mut material = String::new();
    for hit in hits {
        let snippet = hit.snippet.replace("<mark>", "").replace("</mark>", "");
        material.push_str(&format!("- {}: {}\n", hit.rel_path, snippet));
    }
    let system = "You answer questions using only the provided project material. \
Answer in one or two sentences. If the material doesn't answer it, say you don't \
know. End with a final line `SOURCES: path1, path2` listing the project-relative \
paths you used (omit the line if none).";
    let user = format!("Question: {query}\n\nMaterial:\n\n{material}");
    ken_core::local_llm::chatml_prompt(system, &user)
}

fn quick_answer_prompt(query: &str, hits: &[SearchHit]) -> String {
    let mut p = format!(
        "Question: {query}\n\nMaterial from the project's search index:\n\n"
    );
    for hit in hits {
        let snippet = hit.snippet.replace("<mark>", "").replace("</mark>", "");
        p.push_str(&format!("- {}: {}\n", hit.rel_path, snippet));
    }
    p.push_str(
        "\nAnswer the question in one or two sentences using ONLY this \
material; name the source paths you used; if the material doesn't answer \
it, say you don't know. End with a final line `SOURCES: path1, path2` \
listing the project-relative paths you used (omit the line if none).\n",
    );
    p
}

/// Kick off a background quick answer for a ⌘K query. Prefers the on-device
/// model — streaming `quick-answer-delta` chunks and a final `quick-answer` —
/// and falls back silently to the Claude oneshot when the local model isn't
/// ready or hits a runtime error. Returns false only when neither the local
/// model nor Claude is available (the overlay then stops asking); the answer
/// arrives later as events and never blocks the match list. A newer query
/// bumps `qa_gen`, cancelling any in-flight generation so a stale answer never
/// lands in the card.
#[tauri::command]
fn quick_answer(app: AppHandle, state: State<SharedState>, query: String) -> CmdResult<bool> {
    let (hits, root, qa_gen, claude) = {
        let guard = state.lock().unwrap();
        let active = guard.active.as_ref().ok_or("no project open")?;
        let hits = active.db.search(&query, 8).map_err(err)?;
        (
            hits,
            active.project.root.clone(),
            guard.qa_gen.clone(),
            ken_core::runner::discover_claude(),
        )
    };
    if hits.is_empty() {
        return Ok(true); // nothing to ground on — no card, but AI is "available"
    }

    let my_gen = qa_gen.fetch_add(1, Ordering::SeqCst) + 1;
    let local_ready = matches!(
        ken_core::local_llm::llm_status(),
        ken_core::local_llm::LlmStatus::Ready
    );

    if local_ready {
        let prompt = local_quick_answer_prompt(&query, &hits);
        std::thread::spawn(move || {
            let mut on_token = |piece: &str| -> bool {
                if qa_gen.load(Ordering::SeqCst) != my_gen {
                    return false; // superseded by a newer query
                }
                let _ = app.emit(
                    "quick-answer-delta",
                    QuickAnswerDelta { query: query.clone(), delta: piece.to_string() },
                );
                true
            };
            match ken_core::local_llm::generate_stream(
                &prompt,
                ken_core::local_llm::Priority::Interactive,
                &mut on_token,
            ) {
                Ok(text) if qa_gen.load(Ordering::SeqCst) == my_gen => {
                    let parsed = digest::parse_digest(&text);
                    let _ = app.emit(
                        "quick-answer",
                        QuickAnswerEvent { query, body: parsed.body, sources: parsed.sources },
                    );
                }
                Ok(_) => {} // superseded — a newer generation owns the card
                Err(_) => {
                    // Runtime load/inference failure → fall back to Claude,
                    // still honouring the generation id.
                    run_claude_quick_answer(app, root, claude, query, hits, qa_gen, my_gen);
                }
            }
        });
        return Ok(true);
    }

    // Local model not ready → the existing Claude oneshot path (unchanged output).
    let Some(binary) = claude else {
        return Ok(false); // neither local nor Claude — stop asking
    };
    std::thread::spawn(move || {
        run_claude_quick_answer(app, root, Some(binary), query, hits, qa_gen, my_gen);
    });
    Ok(true)
}

/// Shared Claude-oneshot quick-answer path (the fallback), honouring the
/// supersede generation id so a stale answer never lands in the card.
fn run_claude_quick_answer(
    app: AppHandle,
    root: std::path::PathBuf,
    claude: Option<std::path::PathBuf>,
    query: String,
    hits: Vec<SearchHit>,
    qa_gen: Arc<AtomicU64>,
    my_gen: u64,
) {
    let Some(binary) = claude else { return };
    let prompt = quick_answer_prompt(&query, &hits);
    if let Ok(OneshotOutcome::Completed(text)) = assistant::oneshot(
        &binary,
        &root,
        &prompt,
        Duration::from_secs(60),
        &CancelToken::new(),
    ) {
        if qa_gen.load(Ordering::SeqCst) != my_gen {
            return; // superseded
        }
        let parsed = digest::parse_digest(&text);
        let _ = app.emit(
            "quick-answer",
            QuickAnswerEvent { query, body: parsed.body, sources: parsed.sources },
        );
    }
}

/// The on-device language model's state, for the ⌘K "not installed" hint.
#[tauri::command]
fn llm_status() -> &'static str {
    match ken_core::local_llm::llm_status() {
        ken_core::local_llm::LlmStatus::Ready => "ready",
        ken_core::local_llm::LlmStatus::NotInstalled => "notInstalled",
        ken_core::local_llm::LlmStatus::Error(_) => "error",
    }
}

// ---------- knowledge model (Map & Timeline) ----------

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct KnowledgeModelDto {
    entities: Vec<EntityRow>,
    edges: Vec<EdgeRow>,
    events: Vec<EventRow>,
    /// Epoch seconds of the last build; null before the first one.
    built_at: Option<i64>,
    /// A manual Deep rebuild is running right now — the Map/Timeline screens
    /// may open mid-build, so the state has to come back with the model, not
    /// only as an event.
    building: bool,
    /// Incremental coverage: files extracted / indexed files.
    analyzed: i64,
    total: i64,
    /// `ready` | `notInstalled` | `error` — drives the Map's paused notice.
    llm_status: String,
    /// The model's error message when `llm_status == "error"`, else null.
    llm_error: Option<String>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct KnowledgeModelState {
    /// `building` | `ready` | `error` | `idle`
    ///
    /// `idle` ends an automatic build that didn't produce a model (a CLI
    /// that isn't logged in, say): the screens stop saying "building" and
    /// go back to offering a manual refresh, which reports the real error.
    /// Nobody asked for the build, so nobody gets an error banner for it.
    state: String,
    detail: Option<String>,
}

/// The whole stored knowledge model in one call — it's small by
/// construction (extraction caps), so no pagination.
#[tauri::command]
fn knowledge_model(state: State<SharedState>) -> CmdResult<KnowledgeModelDto> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    let (entities, edges) = active.db.list_entities_with_edges().map_err(err)?;
    let (analyzed, total) = active.db.extraction_coverage().map_err(err)?;
    let (llm_status, llm_error) = match ken_core::local_llm::llm_status() {
        ken_core::local_llm::LlmStatus::Ready => ("ready".to_string(), None),
        ken_core::local_llm::LlmStatus::NotInstalled => ("notInstalled".to_string(), None),
        ken_core::local_llm::LlmStatus::Error(e) => ("error".to_string(), Some(e)),
    };
    Ok(KnowledgeModelDto {
        entities,
        edges,
        events: active.db.list_events().map_err(err)?,
        built_at: active.db.knowledge_model_built_at().map_err(err)?,
        building: active.knowledge_running.load(Ordering::SeqCst),
        analyzed,
        total,
        llm_status,
        llm_error,
    })
}

/// Rebuild the knowledge model now, by hand. Progress arrives as
/// `knowledge-model-state` events: building → ready | error {detail}.
/// Unlike the automatic build this ignores every threshold — "rebuild it
/// now" is exactly what it says.
#[tauri::command]
fn refresh_knowledge_model(app: AppHandle, state: State<SharedState>) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    let Some(binary) = ken_core::runner::discover_claude() else {
        return Err(ken_core::runner::MISSING_CLAUDE_HELP.into());
    };
    let job = KnowledgeBuild {
        base: guard.base_dir.clone(),
        project: active.project.clone(),
        binary,
        running: active.knowledge_running.clone(),
        tracker: active.auto_knowledge.clone(),
        quiet_failure: false,
    };
    drop(guard);

    if !start_knowledge_build(&app, job) {
        return Err("Ken is already mapping this project — give it a moment.".into());
    }
    Ok(())
}

struct KnowledgeBuild {
    base: PathBuf,
    project: Project,
    binary: PathBuf,
    running: Arc<AtomicBool>,
    tracker: Arc<AutoBuildTracker>,
    /// Automatic builds report failure as `idle`, not `error`.
    quiet_failure: bool,
}

/// Start one build thread, or report that one is already in flight. The
/// `knowledge_running` flag is the single guard both entry points share,
/// so a manual refresh and the tick can never run two Claude sessions
/// over the same corpus.
fn start_knowledge_build(app: &AppHandle, job: KnowledgeBuild) -> bool {
    if job.running.swap(true, Ordering::SeqCst) {
        return false;
    }
    // The changes indexed so far are this build's input; anything landing
    // while it reads must survive into the next rebuild.
    job.tracker.build_started(Instant::now());

    let _ = app.emit("knowledge-model-state", KnowledgeModelState {
        state: "building".into(),
        detail: None,
    });
    let project_id = job.project.config.id;
    let thread_app = app.clone();
    std::thread::spawn(move || {
        let today = local_date_today();
        let result = Db::open(&job.base, project_id)
            .map_err(|e| e.to_string())
            .and_then(|mut db| {
                knowledge_model::build_knowledge_model(
                    &job.binary,
                    &job.project,
                    &mut db,
                    &today,
                    &CancelToken::new(),
                )
                .map_err(|e| e.to_string())
            });
        let event = match result {
            Ok(counts) => KnowledgeModelState {
                state: "ready".into(),
                detail: Some(format!(
                    "{} entities, {} events",
                    counts.entities, counts.events
                )),
            },
            Err(_) if job.quiet_failure => KnowledgeModelState {
                state: "idle".into(),
                detail: None,
            },
            Err(detail) => KnowledgeModelState {
                state: "error".into(),
                detail: Some(detail),
            },
        };
        let _ = thread_app.emit("knowledge-model-state", event);
        // Stamp the attempt BEFORE clearing the guard: a later manual rebuild
        // must never see "not running" together with a stale attempt clock, or
        // a failing build could restart immediately.
        job.tracker.build_finished(Instant::now());
        job.running.store(false, Ordering::SeqCst);
    });
    true
}

/// The incremental-Map worker: one per open project. Loops draining the
/// extraction queue while the local model is ready, emitting a throttled
/// `knowledge-updated` after each merged file. Every wait is short so a newly
/// indexed file is picked up promptly; the model's own queue (background
/// priority) yields to interactive quick answers upstream, and the worker also
/// steps aside between files while a quick answer is in flight. When the local
/// model isn't ready — no model installed, a load error — the worker idles
/// quietly (polling with a sleep), never erroring rows and never spinning CPU.
fn extraction_worker(
    app: AppHandle,
    state: SharedState,
    project_id: uuid::Uuid,
    stop: Arc<AtomicBool>,
) {
    let mut last_emit = Instant::now() - Duration::from_secs(1);
    let mut pending_emit = false;
    // The worker's own connection, opened once and reused for its whole life
    // (opening runs pragmas + a migration probe — needless work per file).
    // Dropped with the function when the project switches/closes.
    let mut db: Option<Db> = None;
    while !stop.load(Ordering::SeqCst) {
        // Pause quietly unless the local model is ready.
        if !matches!(
            ken_core::local_llm::llm_status(),
            ken_core::local_llm::LlmStatus::Ready
        ) {
            std::thread::sleep(Duration::from_secs(2));
            continue;
        }
        // Yield to interactive quick answers: while one is queued or running,
        // step aside briefly rather than occupying the single inference worker
        // with a background generation.
        if ken_core::local_llm::interactive_pending() {
            std::thread::sleep(Duration::from_millis(200));
            continue;
        }
        // Resolve base + project id under the lock, then drop it before the
        // (slow) generation so IPC stays responsive.
        let base = {
            let guard = state.lock().unwrap();
            match guard.active.as_ref() {
                Some(active) if active.project.config.id == project_id => {
                    guard.base_dir.clone()
                }
                _ => return, // project closed or switched — this worker is done
            }
        };
        if db.is_none() {
            match Db::open(&base, project_id) {
                Ok(d) => db = Some(d),
                Err(_) => {
                    std::thread::sleep(Duration::from_secs(2));
                    continue;
                }
            }
        }
        let db = db.as_mut().expect("opened above");
        let today = local_date_today();
        let at = engine::now_epoch();
        let generate = |prompt: &str| {
            ken_core::local_llm::generate_json(prompt, ken_core::local_llm::Priority::Background)
        };
        match knowledge_model::process_next_pending(db, &today, at, &generate) {
            Ok(Some(_)) => {
                pending_emit = true;
                // Throttle: coalesce a burst into at most one event / 750ms.
                if last_emit.elapsed() >= Duration::from_millis(750) {
                    let _ = app.emit("knowledge-updated", ());
                    last_emit = Instant::now();
                    pending_emit = false;
                }
                // Immediately loop for the next pending file.
            }
            Ok(None) => {
                // Queue empty: flush any trailing throttled emit, then idle.
                if pending_emit {
                    let _ = app.emit("knowledge-updated", ());
                    last_emit = Instant::now();
                    pending_emit = false;
                }
                std::thread::sleep(Duration::from_secs(2));
            }
            Err(_) => {
                // A generation failed (already recorded on the row, which is now
                // `error` and won't be re-popped). Continue the loop past it —
                // a brief backoff so a bad model state doesn't spin.
                std::thread::sleep(Duration::from_secs(2));
            }
        }
    }
}

// ---------- chat commands ----------

#[tauri::command]
fn list_chats(state: State<SharedState>) -> CmdResult<Vec<ChatRow>> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    let result = active.chat_db.lock().unwrap().list_chats().map_err(err);
    result
}

#[tauri::command]
fn chat_transcript(state: State<SharedState>, chat_id: String) -> CmdResult<Vec<ChatMessage>> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    let result = active.chat_db.lock().unwrap().chat_messages(&chat_id).map_err(err);
    result
}

#[tauri::command]
fn create_chat(app: AppHandle, state: State<SharedState>) -> CmdResult<ChatRow> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    let now = engine::now_epoch();
    let row = ChatRow {
        id: uuid::Uuid::new_v4().to_string(),
        title: "New chat".into(),
        kind: "user".into(),
        pinned: false,
        status: "done".into(),
        created_at: now,
        last_active_at: now,
        archived: false,
        model: None,
    };
    active.chat_db.lock().unwrap().upsert_chat(&row).map_err(err)?;
    let _ = app.emit("chat-updated", row.clone());
    Ok(row)
}

#[tauri::command]
fn send_chat_message(
    app: AppHandle,
    state: State<SharedState>,
    chat_id: String,
    text: String,
    open_files: Option<Vec<String>>,
    focused_file: Option<String>,
) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    let engine_arc = active
        .chat_engine
        .as_ref()
        .ok_or(ken_core::runner::MISSING_CLAUDE_HELP)?
        .clone();

    let now = engine::now_epoch();
    let (resume, row) = {
        let mut db = active.chat_db.lock().unwrap();
        let row = db
            .get_chat(&chat_id)
            .map_err(err)?
            .ok_or("chat not found")?;
        if row.kind == "ingest" || row.kind == "research" {
            return Err(format!(
                "This is {} session — open it in the terminal to interact.",
                if row.kind == "research" { "a research" } else { "an ingest" }
            ));
        }
        let had_messages = !db.chat_messages(&chat_id).map_err(err)?.is_empty();
        let id = db.append_chat_message(&chat_id, "user", &text, now).map_err(err)?;
        let _ = db.touch_chat(&chat_id, now);
        let _ = app.emit("chat-message", ChatMessage {
            id,
            chat_id: chat_id.clone(),
            role: "user".into(),
            content: text.clone(),
            created_at: now,
        });
        if row.title == "New chat" {
            let title: String = text.chars().take(40).collect();
            let _ = db.set_chat_field(&chat_id, ChatField::Title, title.trim());
            if let Ok(Some(updated)) = db.get_chat(&chat_id) {
                let _ = app.emit("chat-updated", updated);
            }
        }
        (had_messages, row)
    };
    drop(guard);

    // The stored transcript keeps the user's raw text; the CLI additionally
    // gets a weak-hint preamble naming the files open on screen (when any),
    // clearly caveated as "not necessarily relevant".
    let open = open_files.unwrap_or_default();
    let prompt = match chat::build_context_preamble(focused_file.as_deref(), &open) {
        Some(preamble) => format!("{preamble}\n\n{text}"),
        None => text.clone(),
    };
    engine_arc
        .send(&chat_id, &prompt, resume, row.model.as_deref())
        .map_err(err)
}

#[tauri::command]
fn rename_chat(app: AppHandle, state: State<SharedState>, chat_id: String, title: String) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    let mut db = active.chat_db.lock().unwrap();
    db.set_chat_field(&chat_id, ChatField::Title, title.trim()).map_err(err)?;
    if let Ok(Some(row)) = db.get_chat(&chat_id) {
        let _ = app.emit("chat-updated", row);
    }
    Ok(())
}

#[tauri::command]
fn set_chat_pinned(app: AppHandle, state: State<SharedState>, chat_id: String, pinned: bool) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    let mut db = active.chat_db.lock().unwrap();
    db.set_chat_flag(&chat_id, ChatFlag::Pinned, pinned).map_err(err)?;
    if let Ok(Some(row)) = db.get_chat(&chat_id) {
        let _ = app.emit("chat-updated", row);
    }
    Ok(())
}

/// Set a chat's model to a stable tier alias (or clear to the CLI default when
/// `model` is None/empty/unrecognized). Applies to the next message/session;
/// any live process keeps its current model until it respawns.
#[tauri::command]
fn set_chat_model(app: AppHandle, state: State<SharedState>, chat_id: String, model: Option<String>) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    // Store only a validated alias; anything else clears to the default so we
    // never persist a string we'd refuse to pass to the CLI.
    let alias = model.as_deref().and_then(chat::valid_model_alias);
    let mut db = active.chat_db.lock().unwrap();
    db.set_chat_model(&chat_id, alias).map_err(err)?;
    if let Ok(Some(row)) = db.get_chat(&chat_id) {
        let _ = app.emit("chat-updated", row);
    }
    Ok(())
}

#[tauri::command]
fn archive_chat(app: AppHandle, state: State<SharedState>, chat_id: String) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    if let Some(engine) = &active.chat_engine {
        engine.stop(&chat_id);
    }
    close_terminal(active, &chat_id);
    let mut db = active.chat_db.lock().unwrap();
    db.set_chat_flag(&chat_id, ChatFlag::Archived, true).map_err(err)?;
    if let Ok(Some(row)) = db.get_chat(&chat_id) {
        let _ = app.emit("chat-updated", row);
    }
    Ok(())
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct PtyChunk {
    chat_id: String,
    data: String, // base64
}

fn close_terminal(active: &ActiveProject, chat_id: &str) {
    match active.terminals.lock().unwrap().remove(chat_id) {
        Some(TerminalHandle::Own(mut pty)) => pty.kill(),
        Some(TerminalHandle::Attached) => pty_registry::detach(chat_id),
        None => {}
    }
}

#[tauri::command]
fn enter_terminal_mode(app: AppHandle, state: State<SharedState>, chat_id: String) -> CmdResult<()> {
    use base64::Engine as _;
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;

    // One process per session: stop conversation mode first.
    if let Some(engine) = &active.chat_engine {
        engine.stop(&chat_id);
    }
    close_terminal(active, &chat_id);

    let emit_app = app.clone();
    let id_for_data = chat_id.clone();
    let on_data = move |bytes: &[u8]| {
        let _ = emit_app.emit("chat-pty-data", PtyChunk {
            chat_id: id_for_data.clone(),
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
        });
    };

    if pty_registry::attach(&chat_id, Box::new(on_data)) {
        // A live runner session (ingest run): tap it, don't spawn.
        active.terminals.lock().unwrap().insert(chat_id.clone(), TerminalHandle::Attached);
        return Ok(());
    }

    let binary = ken_core::runner::discover_claude()
        .ok_or(ken_core::runner::MISSING_CLAUDE_HELP)?;
    let (resume, row) = {
        let mut db = active.chat_db.lock().unwrap();
        let row = db.get_chat(&chat_id).map_err(err)?.ok_or("chat not found")?;
        let had = !db.chat_messages(&chat_id).map_err(err)?.is_empty();
        let now = engine::now_epoch();
        let _ = db.append_chat_message(&chat_id, "divider", "continued in the terminal", now);
        (had || row.kind == "ingest" || row.kind == "research", row)
    };

    let pty = chat::attach_terminal(&binary, &active.project.root, &chat_id, resume, row.model.as_deref(), on_data_dup(app.clone(), chat_id.clone()))
        .map_err(err)?;
    active.terminals.lock().unwrap().insert(chat_id.clone(), TerminalHandle::Own(pty));

    // Status for own terminals comes from hooks (Stop/Notification). The
    // subscription thread ends when the session is unsubscribed or replaced.
    if let Some(hooks) = &guard.hooks {
        let rx = hooks.subscribe(&chat_id);
        let status_app = app.clone();
        let status_db = active.chat_db.clone();
        let sid = chat_id.clone();
        std::thread::spawn(move || {
            while let Ok(ev) = rx.recv() {
                let status = match ev.event.as_str() {
                    "Stop" => "done",
                    "Notification" => "needs_input",
                    _ => continue,
                };
                let mut db = status_db.lock().unwrap();
                let _ = db.set_chat_field(&sid, ChatField::Status, status);
                let _ = db.touch_chat(&sid, engine::now_epoch());
                if let Ok(Some(row)) = db.get_chat(&sid) {
                    let _ = status_app.emit("chat-updated", row);
                }
            }
        });
    }
    Ok(())
}

/// Second copy of the data emitter for the spawn path (the first was moved
/// into the registry-attach attempt).
fn on_data_dup(app: AppHandle, chat_id: String) -> impl Fn(&[u8]) + Send + 'static {
    use base64::Engine as _;
    move |bytes: &[u8]| {
        let _ = app.emit("chat-pty-data", PtyChunk {
            chat_id: chat_id.clone(),
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
        });
    }
}

#[tauri::command]
fn leave_terminal_mode(state: State<SharedState>, chat_id: String) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    close_terminal(active, &chat_id);
    if let Some(hooks) = &guard.hooks {
        hooks.unsubscribe(&chat_id);
    }
    Ok(())
}

#[tauri::command]
fn chat_pty_input(state: State<SharedState>, chat_id: String, data: String) -> CmdResult<()> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&data)
        .map_err(err)?;
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    let mut terminals = active.terminals.lock().unwrap();
    match terminals.get_mut(&chat_id) {
        Some(TerminalHandle::Own(pty)) => pty.input(&bytes).map_err(err),
        Some(TerminalHandle::Attached) => {
            if pty_registry::input(&chat_id, &bytes) {
                Ok(())
            } else {
                Err("that session has ended".into())
            }
        }
        None => Err("no terminal open for this chat".into()),
    }
}

#[tauri::command]
fn chat_pty_resize(state: State<SharedState>, chat_id: String, rows: u16, cols: u16) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    let mut terminals = active.terminals.lock().unwrap();
    if let Some(TerminalHandle::Own(pty)) = terminals.get_mut(&chat_id) {
        pty.resize(rows, cols).map_err(err)?;
    }
    Ok(())
}

// ---------- deep research ----------

/// Kick off a research run: a hidden interactive session that searches the
/// web and writes a cited report into the project. Returns the chat id the
/// drawer can watch (it doubles as the runner session id). The finished
/// report lands in the project folder, so the existing watcher indexes it —
/// no extra wiring.
#[tauri::command]
fn start_research(
    app: AppHandle,
    state: State<SharedState>,
    question: String,
    output_dir: String,
) -> CmdResult<String> {
    let question = question.trim().to_string();
    if question.is_empty() {
        return Err("Type a question first.".into());
    }
    let binary = ken_core::runner::discover_claude()
        .ok_or(ken_core::runner::MISSING_CLAUDE_HELP)?;

    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    let hooks = guard.hooks.clone().ok_or("hooks not running")?;
    let report_rel = research::plan_report(&active.project, &output_dir, &question).map_err(err)?;

    let chat_id = uuid::Uuid::new_v4().to_string();
    let now = engine::now_epoch();
    let short: String = question.chars().take(40).collect();
    let row = ChatRow {
        id: chat_id.clone(),
        title: format!("Research — {}", short.trim()),
        kind: "research".into(),
        pinned: false,
        status: "working".into(),
        created_at: now,
        last_active_at: now,
        archived: false,
        model: None,
    };
    {
        let mut db = active.chat_db.lock().unwrap();
        db.upsert_chat(&row).map_err(err)?;
        let note = format!("Researching — the report will land at {report_rel}.");
        let id = db.append_chat_message(&chat_id, "activity", &note, now).unwrap_or(0);
        let _ = app.emit("chat-updated", row.clone());
        let _ = app.emit("chat-message", ChatMessage {
            id,
            chat_id: chat_id.clone(),
            role: "activity".into(),
            content: note,
            created_at: now,
        });
    }

    let token = CancelToken::new();
    active.research.lock().unwrap().insert(chat_id.clone(), token.clone());

    let project = active.project.clone();
    let chat_db = active.chat_db.clone();
    let research_map = active.research.clone();
    drop(guard);

    let worker_app = app.clone();
    let sid = chat_id.clone();
    std::thread::spawn(move || {
        let set_status = |status: &str, note: Option<String>| {
            let now = engine::now_epoch();
            let mut db = chat_db.lock().unwrap();
            let _ = db.set_chat_field(&sid, ChatField::Status, status);
            let _ = db.touch_chat(&sid, now);
            if let Some(note) = note {
                let id = db.append_chat_message(&sid, "activity", &note, now).unwrap_or(0);
                let _ = worker_app.emit("chat-message", ChatMessage {
                    id,
                    chat_id: sid.clone(),
                    role: "activity".into(),
                    content: note,
                    created_at: now,
                });
            }
            if let Ok(Some(row)) = db.get_chat(&sid) {
                let _ = worker_app.emit("chat-updated", row);
            }
        };

        let outcome = research::run_research(
            &project,
            &binary,
            &sid,
            &question,
            &report_rel,
            &hooks,
            research::DEFAULT_TIMEOUT,
            &token,
            || {
                set_status(
                    "needs_input",
                    Some("The research session is waiting on something — open it here to answer, or cancel the run.".into()),
                );
            },
        );
        match outcome {
            Ok(RunOutcome::Completed) => set_status(
                "done",
                Some(format!("Done — the report is at {report_rel}, ready to open in Files.")),
            ),
            Ok(RunOutcome::Cancelled) => {
                set_status("done", Some("Cancelled — no report was written.".into()))
            }
            Ok(RunOutcome::TimedOut(_)) => set_status(
                "error",
                Some(format!(
                    "The research didn't finish within {} minutes and was stopped.",
                    research::DEFAULT_TIMEOUT.as_secs() / 60
                )),
            ),
            Ok(RunOutcome::Failed(detail)) => set_status("error", Some(detail)),
            Err(e) => set_status("error", Some(e.to_string())),
        }
        research_map.lock().unwrap().remove(&sid);
    });
    Ok(chat_id)
}

#[tauri::command]
fn cancel_research(state: State<SharedState>, chat_id: String) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    if let Some(token) = active.research.lock().unwrap().get(&chat_id) {
        token.cancel();
    }
    Ok(())
}

/// Where can a report go? `research` first — always, even before the
/// folder exists — then the project's existing top-level folders.
#[tauri::command]
fn research_output_options(state: State<SharedState>) -> CmdResult<Vec<String>> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    let mut options = vec!["research".to_string()];
    if let Ok(entries) = std::fs::read_dir(&active.project.root) {
        let mut dirs: Vec<String> = entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|name| {
                !name.starts_with('.')
                    && !ken_core::scan::is_junk_dir_name(name)
                    && name != "research"
                    && !active.project.is_excluded(name)
            })
            .collect();
        dirs.sort();
        options.extend(dirs);
    }
    Ok(options)
}

pub fn run() {
    let base_dir = ken_core::registry::default_base_dir()
        .expect("no OS data directory available");
    // Hand the app-data dir to the on-device LLM so it can resolve/load the
    // installed model; this is the only wiring that activates the local path.
    ken_core::local_llm::init(base_dir.clone());
    let state: SharedState = Arc::new(Mutex::new(AppState {
        base_dir,
        hooks: None,
        active: None,
        model_downloads: Arc::new(Mutex::new(std::collections::HashSet::new())),
        qa_gen: Arc::new(AtomicU64::new(0)),
    }));

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(state)
        // Focus = "the user is back" — the moment to fetch teammates'
        // work and to check whether today's digest is due.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Focused(true) = event {
                use tauri::Manager;
                let state = window.state::<SharedState>();
                let sync = {
                    let guard = state.lock().unwrap();
                    guard.active.as_ref().map(|a| a.sync.clone())
                };
                if let Some(sync) = sync {
                    sync.pull_now();
                }
                let _ = maybe_generate_digest(window.app_handle(), state.inner(), false);
            }
        })
        .invoke_handler(tauri::generate_handler![
            list_projects,
            create_project,
            open_project,
            forget_project,
            rename_project,
            last_project_id,
            current_project,
            set_folder_selection,
            get_tree,
            search,
            read_file,
            read_file_bytes,
            is_cloud_only,
            hydrate_file,
            save_file,
            file_meta,
            extracted_text,
            reindex,
            move_file,
            create_folder,
            create_document,
            import_begin,
            import_classify,
            import_commit,
            import_cancel,
            open_external,
            file_mtime,
            media_src,
            video_transcript,
            generate_transcript,
            model_status,
            list_models,
            download_model,
            remove_model,
            set_model_selection,
            list_ingests,
            get_ingest,
            save_ingest,
            delete_ingest,
            run_ingest,
            cancel_run,
            approve_run,
            discard_run,
            pending_approvals,
            review_inbox,
            resolve_review_item,
            ignore_file,
            unignore_file,
            list_ignored,
            unread_files,
            mark_seen,
            mark_all_seen,
            sync_status,
            set_sync_auto,
            sync_now,
            resolve_conflict,
            resolve_conflict_copy,
            set_ingest_runner_mode,
            get_background_index,
            set_background_index,
            claude_doctor,
            mcp_info,
            current_digest,
            refresh_digest,
            quick_answer,
            knowledge_model,
            refresh_knowledge_model,
            list_chats,
            chat_transcript,
            create_chat,
            send_chat_message,
            rename_chat,
            set_chat_pinned,
            set_chat_model,
            archive_chat,
            enter_terminal_mode,
            leave_terminal_mode,
            chat_pty_input,
            chat_pty_resize,
            start_research,
            cancel_research,
            research_output_options,
            llm_status,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Ken");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_quick_answer_prompt_grounds_and_uses_chatml() {
        let hits = vec![SearchHit {
            rel_path: "People.md".into(),
            kind: "note".into(),
            status: String::new(),
            snippet: "Priya owns <mark>billing</mark>".into(),
            rank: 0.0,
        }];
        let p = local_quick_answer_prompt("who owns billing?", &hits);
        assert!(p.contains("<|im_start|>system"));
        assert!(p.contains("who owns billing?"));
        assert!(p.contains("People.md: Priya owns billing")); // marks stripped
        assert!(p.contains("<|im_start|>assistant"));
    }
}

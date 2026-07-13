use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use ken_core::db::{db_path, Db, FileRow, SearchHit};
use ken_core::project::Project;
use ken_core::registry::{Registry, RegistryEntryStatus};
use ken_core::scan::{self, ScanStats};
use ken_core::watch::{self, WatchHandle};

struct ActiveProject {
    project: Project,
    db: Db,
    _watch: WatchHandle,
}

struct AppState {
    base_dir: PathBuf,
    active: Option<ActiveProject>,
}

type SharedState = Arc<Mutex<AppState>>;

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ProjectInfo {
    id: String,
    name: String,
    root: String,
    excluded: Vec<String>,
}

impl ProjectInfo {
    fn of(project: &Project) -> ProjectInfo {
        ProjectInfo {
            id: project.config.id.to_string(),
            name: project.config.name.clone(),
            root: project.root.to_string_lossy().into_owned(),
            excluded: project.config.excluded.clone(),
        }
    }
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
    let mut guard = state.lock().unwrap();

    let mut registry = Registry::load(&guard.base_dir).map_err(err)?;
    registry.add(&project);
    registry.save(&guard.base_dir).map_err(err)?;

    let db = Db::open(&guard.base_dir, project.config.id).map_err(err)?;
    let watch_db_path = db_path(&guard.base_dir, project.config.id);

    let emit_app = app.clone();
    let watch = watch::start(
        project.clone(),
        watch_db_path,
        Duration::from_secs(2),
        move |stats: &ScanStats| {
            let _ = emit_app.emit("index-updated", stats.clone());
        },
    )
    .map_err(err)?;

    let info = ProjectInfo::of(&project);
    guard.active = Some(ActiveProject {
        project: project.clone(),
        db,
        _watch: watch,
    });
    drop(guard);

    // Initial scan in the background so opening stays instant.
    let scan_app = app.clone();
    let base = { state.lock().unwrap().base_dir.clone() };
    std::thread::spawn(move || {
        if let Ok(mut db) = Db::open(&base, project.config.id) {
            let _ = scan_app.emit("scan-started", ());
            match scan::scan(&project, &mut db) {
                Ok(stats) => {
                    let _ = scan_app.emit("index-updated", stats);
                }
                Err(e) => {
                    let _ = scan_app.emit("scan-error", e.to_string());
                }
            }
        }
    });

    Ok(info)
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
    let uuid = id.parse().map_err(err)?;
    let mut registry = Registry::load(&guard.base_dir).map_err(err)?;
    registry.remove(uuid);
    registry.save(&guard.base_dir).map_err(err)
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
        .filter_entry(|e| e.file_name() != ".ken")
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
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    active.db.search(&query, limit.unwrap_or(30)).map_err(err)
}

#[tauri::command]
fn read_file(state: State<SharedState>, rel_path: String) -> CmdResult<String> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    let abs = active.project.resolve(&rel_path).map_err(err)?;
    std::fs::read_to_string(&abs).map_err(err)
}

#[tauri::command]
fn read_file_bytes(state: State<SharedState>, rel_path: String) -> CmdResult<tauri::ipc::Response> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    let abs = active.project.resolve(&rel_path).map_err(err)?;
    let bytes = std::fs::read(&abs).map_err(err)?;
    Ok(tauri::ipc::Response::new(bytes))
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

pub fn run() {
    let base_dir = ken_core::registry::default_base_dir()
        .expect("no OS data directory available");
    let state: SharedState = Arc::new(Mutex::new(AppState {
        base_dir,
        active: None,
    }));

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            list_projects,
            create_project,
            open_project,
            forget_project,
            current_project,
            set_folder_selection,
            get_tree,
            search,
            read_file,
            read_file_bytes,
            save_file,
            file_meta,
            extracted_text,
            reindex,
            open_external,
            file_mtime,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Ken");
}

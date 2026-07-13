use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use ken_core::db::{db_path, Db, FileRow, RunRow, SearchHit};
use ken_core::engine::{self, EngineConfig, IngestEngine, IngestEvent};
use ken_core::hooks::HookListener;
use ken_core::project::Project;
use ken_core::recipe::{self, Mode, Recipe, RecipeEntry, Refresh, ResolvedRules, RulesOverride};
use ken_core::registry::{Registry, RegistryEntryStatus};
use ken_core::scan::{self, ScanStats};
use ken_core::watch::{self, WatchHandle};

struct ActiveProject {
    project: Project,
    db: Db,
    _watch: WatchHandle,
    engine: Arc<IngestEngine>,
}

struct AppState {
    base_dir: PathBuf,
    hooks: Option<Arc<HookListener>>,
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
                .unwrap_or("hidden-tui")
                .to_string(),
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

    let hooks = match &guard.hooks {
        Some(h) => h.clone(),
        None => {
            let h = Arc::new(HookListener::start().map_err(err)?);
            guard.hooks = Some(h.clone());
            h
        }
    };
    let engine_app = app.clone();
    let engine = Arc::new(
        IngestEngine::start(
            project.root.clone(),
            watch_db_path.clone(),
            hooks,
            EngineConfig::default(),
            move |ev: IngestEvent| {
                let _ = engine_app.emit("ingest-run-changed", ev);
            },
        )
        .map_err(err)?,
    );

    let emit_app = app.clone();
    let watch_engine = engine.clone();
    let watch = watch::start(
        project.clone(),
        watch_db_path,
        Duration::from_secs(2),
        move |stats: &ScanStats| {
            if !stats.changed_paths.is_empty() {
                watch_engine.sources_changed(stats.changed_paths.clone());
            }
            let _ = emit_app.emit("index-updated", stats.clone());
        },
    )
    .map_err(err)?;

    let info = ProjectInfo::of(&project);
    guard.active = Some(ActiveProject {
        project: project.clone(),
        db,
        _watch: watch,
        engine,
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

#[tauri::command]
fn approve_run(state: State<SharedState>, run_id: i64) -> CmdResult<()> {
    let mut guard = state.lock().unwrap();
    let active = guard.active.as_mut().ok_or("no project open")?;
    engine::approve_run(&active.project, &mut active.db, run_id).map_err(err)?;
    // Applied files land on disk; index them promptly.
    let _ = scan::scan(&active.project, &mut active.db);
    Ok(())
}

#[tauri::command]
fn discard_run(state: State<SharedState>, run_id: i64) -> CmdResult<()> {
    let mut guard = state.lock().unwrap();
    let active = guard.active.as_mut().ok_or("no project open")?;
    engine::discard_run(&active.project, &mut active.db, run_id).map_err(err)
}

#[tauri::command]
fn pending_approvals(state: State<SharedState>) -> CmdResult<Vec<RunRow>> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    active.db.runs_with_status("pending_approval").map_err(err)
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

pub fn run() {
    let base_dir = ken_core::registry::default_base_dir()
        .expect("no OS data directory available");
    let state: SharedState = Arc::new(Mutex::new(AppState {
        base_dir,
        hooks: None,
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
            list_ingests,
            get_ingest,
            save_ingest,
            delete_ingest,
            run_ingest,
            cancel_run,
            approve_run,
            discard_run,
            pending_approvals,
            set_ingest_runner_mode,
            claude_doctor,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Ken");
}

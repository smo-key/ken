use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use ken_core::chat::{self, ChatEngine, ChatPty, ChatUpdate};
use ken_core::db::{
    db_path, ChatField, ChatFlag, ChatMessage, ChatRow, Db, FileRow, RunRow, SearchHit,
};
use ken_core::engine::{self, EngineConfig, IngestEngine, IngestEvent};
use ken_core::hooks::HookListener;
use ken_core::project::Project;
use ken_core::recipe::{self, Mode, Recipe, RecipeEntry, Refresh, ResolvedRules, RulesOverride};
use ken_core::registry::{Registry, RegistryEntryStatus};
use ken_core::pty_registry;
use ken_core::scan::{self, ScanStats};
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
    _watch: WatchHandle,
    engine: Arc<IngestEngine>,
    chat_engine: Option<Arc<ChatEngine>>,
    /// Shared connection for chat persistence from event threads.
    chat_db: Arc<Mutex<Db>>,
    terminals: Arc<Mutex<std::collections::HashMap<String, TerminalHandle>>>,
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
                .unwrap_or("headless")
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

    let chat_db = Arc::new(Mutex::new(Db::open(&guard.base_dir, project.config.id).map_err(err)?));

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
        chat_engine,
        chat_db,
        terminals: Arc::new(Mutex::new(std::collections::HashMap::new())),
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
        if row.kind == "ingest" {
            return Err("This is an ingest session — open it in the terminal to interact.".into());
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
    let _ = row;
    engine_arc.send(&chat_id, &text, resume).map_err(err)
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
        (had || row.kind == "ingest", row)
    };
    let _ = row;

    let pty = chat::attach_terminal(&binary, &active.project.root, &chat_id, resume, on_data_dup(app.clone(), chat_id.clone()))
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
            list_chats,
            chat_transcript,
            create_chat,
            send_chat_message,
            rename_chat,
            set_chat_pinned,
            archive_chat,
            enter_terminal_mode,
            leave_terminal_mode,
            chat_pty_input,
            chat_pty_resize,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Ken");
}

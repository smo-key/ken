use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use ken_core::assistant::{self, OneshotOutcome};
use ken_core::chat::{self, ChatEngine, ChatPty, ChatUpdate};
use ken_core::cloud;
use ken_core::db::{db_path, ChatField, ChatFlag, ChatMessage, ChatRow, Db, FileRow};
use ken_core::digest;
use ken_core::embedder::Embedder;
use ken_core::model;
use ken_core::engine;
use ken_core::kenignore;
use ken_core::memory;
use ken_core::research;
use ken_core::tasks;
use ken_core::runner::{CancelToken, RunOutcome};
use ken_core::hooks::HookListener;
use ken_core::project::Project;
use ken_core::registry::{Registry, RegistryEntryStatus};
use ken_core::pty_registry;
use ken_core::routing;
use ken_core::scan::{self, ScanStats};
use ken_core::sync::{self, SyncEngine, SyncNotice};
use ken_core::record::{self, CaptureSource, LinearResampler, RecorderState, Source};
use ken_core::transcript;
use ken_core::user_state::{self, UserState};
use ken_core::watch::{self, WatchHandle};

/// Starting without a Vulkan driver on Windows (delay-loaded vulkan-1.dll).
#[cfg(all(windows, target_env = "msvc"))]
pub mod vulkan;

enum TerminalHandle {
    /// Our own PTY (user chat opened in terminal mode).
    Own(ChatPty),
    /// Attached to a live runner session via the registry.
    Attached,
}

struct MemberRuntime {
    project: Project,
    db: Db,
    /// Dedicated read-only connection for search, on its OWN mutex so a
    /// keystroke's query never contends on the global `AppState` lock (which
    /// every other command and the background workers hold). WAL lets this
    /// reader run concurrently with the writers and still see their commits.
    search_db: Arc<Mutex<Db>>,
    _watch: WatchHandle,
    /// Live on/off handle for the semantic-index build step: `Some` while
    /// `semanticIndex` is on and the model loaded, so
    /// `set_project_feature("semanticIndex", ..)` and `route_search` can
    /// read/toggle it.
    semantic_embedder: Arc<Mutex<Option<Box<dyn Embedder + Send>>>>,
    /// Cancel token for an in-flight `rebuild_semantic_index` spawned by
    /// `apply_semantic_index_flag`. `None` when no build is running. Set when
    /// the build thread is spawned, cleared when it finishes;
    /// `set_project_feature("semanticIndex", false)` cancels it if present.
    semantic_build_cancel: Arc<Mutex<Option<CancelToken>>>,
    /// One-time latch for the ~50k `chunks` row-count guardrail (semantic-index
    /// task 2.4): flips to `true` the first time the threshold is crossed so
    /// the "large project" warning event fires once per activation, not on
    /// every subsequent build/incremental update.
    semantic_index_warned_large: Arc<AtomicBool>,
    chat_engine: Option<Arc<ChatEngine>>,
    /// Shared connection for chat persistence from event threads.
    chat_db: Arc<Mutex<Db>>,
    sync: Arc<SyncEngine>,
    terminals: Arc<Mutex<std::collections::HashMap<String, TerminalHandle>>>,
    /// True while a background reindex (clear + full rescan) is running, so a
    /// second `reindex` command can't stack another rebuild on top.
    reindex_running: Arc<AtomicBool>,
    /// Stops the per-project background worker (one-line summaries and the
    /// knowledge checks) when this project closes.
    _background_worker: StopOnDrop,
    /// Stops the per-project OCR worker when this project closes.
    _ocr_worker: StopOnDrop,
    /// Stops the background cloud-hydration worker when this project closes.
    _bg_hydrate: StopOnDrop,
    /// Stops the `.kenignore` change poller (kenignore task 2.2) when this
    /// project closes.
    _kenignore_watch: StopOnDrop,
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
    members: std::collections::HashMap<uuid::Uuid, MemberRuntime>,
    focused: Option<uuid::Uuid>,
    /// Global feature-flag defaults, loaded from `settings.json` at startup and
    /// held behind the same lock as the rest of app state.
    app_settings: ken_core::settings::AppSettings,
    /// Model downloads are app-global (they live under the app-data dir, not a
    /// project), so their bookkeeping hangs off the top-level state: the set of
    /// model ids downloading right now (guards a second concurrent download of
    /// the same id).
    model_downloads: Arc<Mutex<std::collections::HashSet<String>>>,
    /// Monotonic id for ⌘K quick answers; a newer query bumps it so the
    /// in-flight generation's token callback sees a mismatch and cancels.
    qa_gen: Arc<AtomicU64>,
    /// The single in-progress recording, if any (app-global: one recorder at a
    /// time). Holds its own state/writers behind its mutex.
    record: Arc<Mutex<Option<RecordSession>>>,
    /// The open workspace's bookkeeping (`workspace` change, tasks 3.1/3.2):
    /// the resolved manifest plus the resident-runtime LRU. `None` in
    /// single-project mode — the enum-free equivalent of design.md D2's
    /// `AppMode::Workspace` arm, layered onto the committed `members` map (S9
    /// refactor) rather than replacing it with the D2 enum. Dormant members
    /// live only here (project id + root); they gain a `MemberRuntime` lazily
    /// on first focus.
    workspace: Option<WorkspaceState>,
    /// Caps concurrent member initial-scans at `WORKSPACE_INGEST_CONCURRENCY`
    /// (task 3.1 / design D4: "ingest concurrency is capped at 2 members at a
    /// time"). Shared by every `activate` scan thread — see [`IngestGate`].
    ingest_gate: Arc<IngestGate>,
    /// ken-memory task 2.2: true while a `distill_journal` run is in flight,
    /// one `AtomicBool` guard at the workspace-journal scope.
    memory_distill_running: Arc<AtomicBool>,
    /// The most recent `distill_journal` run's proposed candidates, kept
    /// server-side so `resolve_distill_candidate(slug, approve)` — whose own
    /// contract (tasks.md 2.2) carries no body/description — can look up
    /// what it's approving/dismissing by slug. Replaced wholesale by each
    /// new `distill_journal` run; never persisted to disk (candidates are
    /// ephemeral until approved, per D6 — approval is what makes one durable).
    memory_distill_candidates: Arc<Mutex<Vec<memory::DistillCandidate>>>,
    /// Self-write dedupe registry for Your day's watcher. A mutating command
    /// (`with_task_writes`) notes each path as `Writing` before it writes,
    /// then as `Present(hash)` or `Absent` after, and emits `day-changed`
    /// itself. The poller's next tick consults this entry-by-entry: a match
    /// means "already announced" (consumed, no second emit); anything else
    /// is a change nobody has announced (an agent's write, a hand edit, a
    /// git pull) and gets exactly one emit for the whole tick. Notes expire
    /// after `TASK_WRITE_NOTE_TTL`, so the map stays small.
    task_recent_writes: Arc<Mutex<std::collections::HashMap<PathBuf, TaskWriteNote>>>,
    /// True while a digest thread is running (one at a time).
    digest_running: Arc<AtomicBool>,
}

type SharedState = Arc<Mutex<AppState>>;

/// Resident `MemberRuntime` cap for an open workspace (design D4 /
/// task 3.1): fully activate up to this many members; the rest stay dormant
/// (tracked, not resident) and open lazily on first focus, evicting the
/// least-recently-focused resident when the cap would be exceeded.
const WORKSPACE_RESIDENT_CAP: usize = 12;

/// Max member initial-scans allowed to run at once (design D4 / task 3.1).
const WORKSPACE_INGEST_CONCURRENCY: usize = 2;

/// A counting semaphore capping how many member initial-scans run
/// concurrently (task 3.1 / design D4). `std` has no semaphore, so this is
/// the classic `Mutex<permits>` + `Condvar` pair — the same hand-rolled
/// coordination discipline the rest of this file already leans on (atomics +
/// generation counters like `qa_gen`). One
/// instance lives on `AppState::ingest_gate`; each `activate` initial-scan
/// thread `acquire`s a permit around its `scan::scan` call and releases it
/// when the returned [`IngestPermit`] drops. With the cap at 2, a workspace
/// opening N members runs at most two initial scans at once and the rest
/// queue; single-project open (one scan, two permits free) never waits, so
/// its behavior is unchanged.
struct IngestGate {
    permits: Mutex<usize>,
    ready: std::sync::Condvar,
}

impl IngestGate {
    fn new(max: usize) -> Self {
        IngestGate {
            permits: Mutex::new(max),
            ready: std::sync::Condvar::new(),
        }
    }

    /// Block until a permit is free, take it, and return a guard that
    /// releases the permit on drop.
    fn acquire(self: &Arc<Self>) -> IngestPermit {
        let mut permits = self.permits.lock().unwrap();
        while *permits == 0 {
            permits = self.ready.wait(permits).unwrap();
        }
        *permits -= 1;
        IngestPermit { gate: self.clone() }
    }
}

/// Releases its `IngestGate` permit when dropped (RAII), so a scan that
/// panics or returns early still frees its slot.
struct IngestPermit {
    gate: Arc<IngestGate>,
}

impl Drop for IngestPermit {
    fn drop(&mut self) {
        let mut permits = self.gate.permits.lock().unwrap();
        *permits += 1;
        self.gate.ready.notify_one();
    }
}

/// Bookkeeping for one open workspace (tasks 3.1/3.2). Holds the whole
/// resolved `ken_core::workspace::Workspace` — manifest id/name/root plus each
/// member's `Ok(Project)`/`Missing`/`Invalid` resolution — so the close path
/// can hand it straight to `Registry::add_workspace` and the overview/focus
/// paths can enumerate members (including dormant ones) without re-reading
/// disk. `lru` tracks only the *resident* members (those with a live
/// `MemberRuntime` in `AppState::members`), most-recently-focused last;
/// eviction pops the front.
struct WorkspaceState {
    ws: ken_core::workspace::Workspace,
    lru: Vec<uuid::Uuid>,
    /// Your day's watcher (`spawn_task_board_watch`), set once the workspace
    /// is installed. Dropping `WorkspaceState` (workspace close or
    /// switch) drops this too, which stops the poller thread via
    /// `StopOnDrop` — no separate teardown call needed.
    task_watch: Option<StopOnDrop>,
}

impl WorkspaceState {
    /// Root folder of a resolvable member by project id — used to reopen a
    /// dormant member on first focus.
    fn member_root(&self, id: uuid::Uuid) -> Option<PathBuf> {
        self.ws.members.iter().find_map(|m| match &m.status {
            ken_core::workspace::MemberStatus::Ok(p) if p.config.id == id => Some(p.root.clone()),
            _ => None,
        })
    }

    /// Parent-relative member name (label) of a resolvable member by id.
    fn member_name(&self, id: uuid::Uuid) -> Option<String> {
        self.ws.members.iter().find_map(|m| match &m.status {
            ken_core::workspace::MemberStatus::Ok(p) if p.config.id == id => Some(m.name.clone()),
            _ => None,
        })
    }
}

/// Look up the runtime for a member project. `None` means "the sole open
/// project" - today's single-project semantics. Callers migrate to passing
/// real ids as multi-project lands (S9 step list / workspace design).
/// The open member that holds chat `chat_id`: the focused one when it
/// does, else whichever does. Opening a citation in another repo moves the
/// focus while the chat in the first one goes on.
fn chat_owner(state: &AppState, chat_id: &str) -> Option<uuid::Uuid> {
    let has = |rt: &MemberRuntime| lock_tolerant(&rt.chat_db).get_chat(chat_id).ok().flatten().is_some();
    if let Some(f) = state.focused {
        if state.members.get(&f).is_some_and(has) {
            return Some(f);
        }
    }
    state.members.iter().find(|(_, rt)| has(rt)).map(|(id, _)| *id)
}

fn member<'a>(state: &'a AppState, id: Option<uuid::Uuid>) -> Result<&'a MemberRuntime, &'static str> {
    match id.or(state.focused) {
        None => Err("no project open"),
        Some(resolved) => state
            .members
            .get(&resolved)
            .ok_or(if id.is_some() { "project not open" } else { "no project open" }),
    }
}

/// Mutable twin of [`member`].
fn member_mut<'a>(state: &'a mut AppState, id: Option<uuid::Uuid>) -> Result<&'a mut MemberRuntime, &'static str> {
    match id.or(state.focused) {
        None => Err("no project open"),
        Some(resolved) => state
            .members
            .get_mut(&resolved)
            .ok_or(if id.is_some() { "project not open" } else { "no project open" }),
    }
}

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
            excluded: project.excluded_folders(),
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

/// kenignore task 2.2: emitted when the `.kenignore` poller notices a
/// change and finds lines `ken_core::kenignore::parse` silently skips (a
/// bare `~` or `!` with no pattern after it). 1-based line numbers.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct KenignoreWarningEvent {
    malformed_lines: Vec<usize>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TreeData {
    files: Vec<FileRowDto>,
    folders: Vec<FolderInfo>,
}

/// A `FileRow` plus the one thing the DB can't know on its own: whether the
/// background hydration worker would pull this file down. The selection rule
/// lives solely in `ken_core::bg_hydrate::wants_background_index`; we evaluate
/// it here, at the serialization boundary, because that's where the live
/// `Project` (needed for exclusions) is in hand. Flattened so the frontend sees
/// the same shape as before with one added `backgroundEligible` field.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FileRowDto {
    #[serde(flatten)]
    row: FileRow,
    background_eligible: bool,
}

impl FileRowDto {
    /// `excluded`: the source's excluded folders (`Project::excluded_folders`),
    /// read once by the caller.
    fn new(row: FileRow, excluded: &[String]) -> FileRowDto {
        let excluded = ken_core::project::in_folders(excluded, &row.rel_path);
        let background_eligible =
            ken_core::bg_hydrate::wants_background_index(&row, excluded);
        FileRowDto { row, background_eligible }
    }
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

/// Wraps a member-scoped event's payload with `project_id`, flattened in
/// alongside the payload's own fields so a payload of `{"foo":1}` becomes
/// `{"project_id":"...","foo":1}` — every existing field stays top-level and
/// listeners written against the old flat shape keep working unchanged.
/// `Clone` is required because `AppHandle::emit`'s own bound
/// (`Emitter::emit<S: Serialize + Clone>`) demands it of whatever it's
/// given — every payload passed through here already satisfies it, since
/// the same bound applied when these were emitted directly.
#[derive(Clone, Serialize)]
struct MemberEventEnvelope<T: Serialize> {
    project_id: uuid::Uuid,
    #[serde(flatten)]
    payload: T,
}

/// Emit an event scoped to one project/member (S9 step 5, event envelope).
/// Fires the SAME event name as a plain `app.emit`, but wraps the payload so
/// a multi-project-aware frontend can tell which project it came from
/// without every emit site threading routing logic itself. Event names are
/// unchanged — only per-project (member-context) events should go through
/// this; genuinely app-global events (model download progress, recording,
/// app lifecycle) should keep using `app.emit` directly.
///
/// Only call this with a payload that serializes to a JSON object (a struct
/// or map). A scalar, array, or unit (`()`) payload isn't an object to
/// flatten `project_id` into — `serde_json` would error trying — so those
/// events are deliberately left as plain `app.emit` at their call sites.
fn emit_member(app: &AppHandle, project_id: uuid::Uuid, event: &str, payload: impl Serialize + Clone) {
    let _ = app.emit(event, MemberEventEnvelope { project_id, payload });
}

/// Payload for the `semantic-index-state` event (semantic-index task 2.3).
/// Serializes with a `state` discriminant field so the frontend can switch
/// on `payload.state`: `{"state":"building","done":1,"total":10}`,
/// `{"state":"ready"}`, `{"state":"unavailable","reason":"..."}`,
/// `{"state":"warning","reason":"..."}`.
#[derive(Clone, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
enum SemanticIndexStateEvent {
    Building { done: usize, total: usize },
    Ready,
    Unavailable { reason: String },
    /// One-time guardrail warning (semantic-index task 2.4 / spike
    /// `S4-knn-latency.md`): distinct from `Unavailable` so the frontend
    /// doesn't mistake "search may be slower" for "search is off".
    Warning { reason: String },
}

/// Effective `semanticIndex` flag for a project — a thin call to the shared
/// resolver, which layers project override > (reserved workspace slot) >
/// global default > registry default, honoring the legacy top-level
/// `extra["semanticIndex"]` key on the way.
fn semantic_index_enabled(app_settings: &ken_core::settings::AppSettings, project: &Project) -> bool {
    ken_core::features::effective_flag(app_settings, project, "semanticIndex")
}

/// Build the embedder used for semantic indexing, or `None` if no embedding
/// model is available — either this build lacks the `local-llm` feature
/// (e.g. the FTS-only `ken-mcp` sidecar), or the user hasn't downloaded the
/// embedding model yet (Settings → Models), or the model file failed to
/// load. `FakeEmbedder` is deliberately NOT substituted here: it would
/// silently return meaningless hash-based vectors for real search results
/// instead of failing loudly. Callers must treat `None` as "semantic index
/// unavailable" and emit `semantic-index-state` `unavailable {reason}`
/// rather than degrading silently.
fn build_semantic_embedder() -> Option<Box<dyn Embedder + Send>> {
    ken_core::embedder::installed_embedding_model()
}

type EmbedderSlot = Arc<Mutex<Option<Box<dyn Embedder + Send>>>>;

/// One embedding model for the whole app: every repo's slot is this one, so
/// a workspace of five repos holds the model once, not five times.
fn shared_embedder() -> EmbedderSlot {
    static SLOT: std::sync::OnceLock<EmbedderSlot> = std::sync::OnceLock::new();
    SLOT.get_or_init(|| Arc::new(Mutex::new(None))).clone()
}

/// The selected embedding model, loaded into the shared slot unless it is
/// already there. `(model id, width)`, or `None` with no model installed.
fn ensure_shared_embedder() -> Option<(String, usize)> {
    let slot = shared_embedder();
    let wanted = ken_core::embedder::selected_profile()?;
    let mut guard = slot.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(e) = guard.as_ref() {
        if e.model_id() == wanted.model_id {
            return Some((e.model_id(), e.dim()));
        }
    }
    // Drop the old model before loading the new one: two in memory at once
    // is what a switch on a small laptop cannot afford.
    guard.take();
    let e = build_semantic_embedder()?;
    let out = (e.model_id(), e.dim());
    *guard = Some(e);
    Some(out)
}

/// The shared embedder, locked per batch rather than for a whole build, so a
/// search between two batches is not held up by a re-read of every file.
struct SlotEmbedder {
    slot: EmbedderSlot,
    model_id: String,
    dim: usize,
}

impl Embedder for SlotEmbedder {
    fn embed(&mut self, texts: &[String]) -> ken_core::Result<Vec<Vec<f32>>> {
        let mut guard = self.slot.lock().unwrap_or_else(|p| p.into_inner());
        match guard.as_deref_mut() {
            Some(e) if e.model_id() == self.model_id => e.embed(texts),
            _ => Err(ken_core::Error::Other("the embedding model changed during the build".into())),
        }
    }

    fn dim(&self) -> usize {
        self.dim
    }

    fn model_id(&self) -> String {
        self.model_id.clone()
    }

    fn embed_query(&mut self, text: &str) -> ken_core::Result<Vec<f32>> {
        let mut guard = self.slot.lock().unwrap_or_else(|p| p.into_inner());
        match guard.as_deref_mut() {
            Some(e) => e.embed_query(text),
            None => Err(ken_core::Error::Other("no embedding model".into())),
        }
    }
}

/// Builds running now, by repo: files done and total.
fn semantic_builds() -> &'static Mutex<std::collections::HashMap<uuid::Uuid, (usize, usize)>> {
    static BUILDS: std::sync::OnceLock<Mutex<std::collections::HashMap<uuid::Uuid, (usize, usize)>>> = std::sync::OnceLock::new();
    BUILDS.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// The search-by-meaning index, for Settings: which model, how far, and
/// whether a re-read is running. Also the `semantic-progress` payload.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct EmbeddingStateDto {
    model: Option<String>,
    dims: Option<usize>,
    embedded: usize,
    total: usize,
    rebuilding: bool,
    device: String,
}

/// While builds run: files read for meaning over files to read, across repos.
fn emit_semantic_progress(app: &AppHandle) {
    let (embedded, total, rebuilding) = {
        let builds = semantic_builds().lock().unwrap_or_else(|p| p.into_inner());
        let (d, t) = builds.values().fold((0, 0), |(d, t), (bd, bt)| (d + bd, t + bt));
        (d, t, !builds.is_empty())
    };
    let (model, dims) = {
        let slot = shared_embedder();
        let guard = slot.lock().unwrap_or_else(|p| p.into_inner());
        guard.as_ref().map(|e| (Some(e.model_id()), Some(e.dim()))).unwrap_or((None, None))
    };
    let _ = app.emit(
        "semantic-progress",
        EmbeddingStateDto { model, dims, embedded, total, rebuilding, device: device_label() },
    );
}

/// "NVIDIA RTX PRO 2000 (Vulkan)" or "CPU".
fn device_label() -> String {
    let d = ken_core::compute::device();
    match d.name {
        Some(n) => format!("{n} ({})", if d.backend == "metal" { "Metal" } else { "Vulkan" }),
        None => "CPU".into(),
    }
}

/// Re-apply search by meaning to every open repo that has it on: after a
/// model is installed or switched, or the graphics card setting changes.
fn reapply_semantic_index_everywhere(app: &AppHandle, state: &SharedState) {
    let targets: Vec<uuid::Uuid> = {
        let guard = state.lock().unwrap();
        guard
            .members
            .iter()
            .filter(|(_, m)| semantic_index_enabled(&guard.app_settings, &m.project))
            .map(|(id, _)| *id)
            .collect()
    };
    for id in targets {
        let _ = apply_semantic_index_flag(app, state, Some(id), true);
    }
}

/// semantic-index task 2.4 / spike `S4-knn-latency.md`: once a project's
/// `chunks` table crosses ~50k rows, KNN latency starts to degrade. This
/// never blocks or disables search — it's a one-time, latched warning so
/// the UI can tell the user why things feel slower. Call after each build
/// or incremental update completes, not per search.
const LARGE_SEMANTIC_INDEX_CHUNK_THRESHOLD: i64 = 50_000;

fn maybe_warn_large_index(
    app: &AppHandle,
    project_id: uuid::Uuid,
    db: &Db,
    warned: &Arc<AtomicBool>,
) {
    if warned.load(Ordering::SeqCst) {
        return;
    }
    let Ok(count) = db.chunk_count() else { return };
    if count >= LARGE_SEMANTIC_INDEX_CHUNK_THRESHOLD && !warned.swap(true, Ordering::SeqCst) {
        emit_member(
            app,
            project_id,
            "semantic-index-state",
            SemanticIndexStateEvent::Warning {
                reason: format!(
                    "large project ({count} chunks) — semantic search may be slower"
                ),
            },
        );
    }
}

/// Turn the semantic index on or off for one project (semantic-index task
/// 2.1). Mirrors `reindex`'s pattern: lock only long enough to snapshot the
/// `Arc` handles needed, release the lock, then do the real work
/// (background rebuild) on a detached thread with its own `Db::open` handle
/// so the IPC thread and the global state mutex are never held for the
/// duration of an embedding pass. Called from `set_project_feature`,
/// `activate()` on project open, the model-download-completion handler
/// (all three pass `target: None` — the pre-workspace single-project
/// semantics `member(&guard, None)` resolves, unchanged from before this
/// parameter existed), and from `reapply_semantic_index_everywhere` (which
/// passes each open member's id).
fn apply_semantic_index_flag(
    app: &AppHandle,
    state: &SharedState,
    target: Option<uuid::Uuid>,
    enabled: bool,
) -> CmdResult<()> {
    let (project, base, embedder_slot, cancel_slot, warned_large) = {
        let guard = state.lock().unwrap();
        let active = member(&guard, target)?;
        (
            active.project.clone(),
            guard.base_dir.clone(),
            active.semantic_embedder.clone(),
            active.semantic_build_cancel.clone(),
            active.semantic_index_warned_large.clone(),
        )
    };

    if !enabled {
        // Turning off for this repo: cancel its rebuild. The embedder is
        // shared with the other repos, so it stays; this repo's searches stop
        // using it because its flag is off. Existing vectors are kept, so
        // turning it back on resumes rather than starting over.
        let _ = embedder_slot;
        if let Some(token) = cancel_slot.lock().unwrap().take() {
            token.cancel();
        }
        return Ok(());
    }

    start_semantic_build(app, project, base, embedder_slot, cancel_slot, warned_large, MeaningBuild::Full);
    Ok(())
}

/// What a meaning build does: a full rebuild chunks every file again, a
/// catch-up embeds only what a scan left without a vector and runs the
/// controls (`engine::catch_up_semantic_index`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum MeaningBuild {
    Full,
    CatchUp,
}

/// Repos whose meaning index should catch up once the build running for them
/// ends: a change that lands mid-build is not lost, and changes in a burst
/// cost one more build, not one each. Locked only under `semantic_builds`.
fn meaning_wanted() -> &'static Mutex<std::collections::HashSet<uuid::Uuid>> {
    static WANTED: std::sync::OnceLock<Mutex<std::collections::HashSet<uuid::Uuid>>> = std::sync::OnceLock::new();
    WANTED.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// Tell the UI a repo's index changed, and have its meaning index catch up.
/// Every path that writes to a repo's index (a scan, the watcher, a file
/// refreshed after a write) ends here, so a changed chunk always gets its
/// vector and the controls run again (knowledge-core decision 19).
fn emit_index_updated(app: &AppHandle, project_id: uuid::Uuid, stats: ScanStats) {
    emit_member(app, project_id, "index-updated", stats);
    queue_meaning_catch_up(app, project_id);
}

/// Queue a catch-up of a repo's meaning index, when the repo has search by
/// meaning on: at once when no build runs for it, else once the running one
/// ends. Its own thread, so a caller holding the state lock is never held
/// up by it. The embedding goes through the throttle's local-work gate: it
/// waits while paused and rests by the pace (`engine::embed_pending`).
fn queue_meaning_catch_up(app: &AppHandle, project_id: uuid::Uuid) {
    use tauri::Manager as _;
    let app = app.clone();
    std::thread::spawn(move || {
        let state: SharedState = app.state::<SharedState>().inner().clone();
        let snapshot = {
            let guard = lock_tolerant(&state);
            guard
                .members
                .get(&project_id)
                .filter(|m| semantic_index_enabled(&guard.app_settings, &m.project))
                .map(|m| {
                    (
                        m.project.clone(),
                        guard.base_dir.clone(),
                        m.semantic_embedder.clone(),
                        m.semantic_build_cancel.clone(),
                        m.semantic_index_warned_large.clone(),
                    )
                })
        };
        let Some((project, base, embedder_slot, cancel_slot, warned_large)) = snapshot else { return };
        {
            let builds = semantic_builds().lock().unwrap_or_else(|p| p.into_inner());
            if builds.contains_key(&project_id) {
                meaning_wanted().lock().unwrap_or_else(|p| p.into_inner()).insert(project_id);
                return;
            }
        }
        start_semantic_build(&app, project, base, embedder_slot, cancel_slot, warned_large, MeaningBuild::CatchUp);
    });
}

/// Start a meaning build for one repo on its own thread, replacing one
/// running for it. Shared by the flag (`apply_semantic_index_flag`, a full
/// rebuild) and the catch-up after a change.
#[allow(clippy::too_many_arguments)]
fn start_semantic_build(
    app: &AppHandle,
    project: Project,
    base: PathBuf,
    embedder_slot: EmbedderSlot,
    cancel_slot: Arc<Mutex<Option<CancelToken>>>,
    warned_large: Arc<AtomicBool>,
    mode: MeaningBuild,
) {
    let project_id = project.config.id;

    let Some((model_id, dim)) = ensure_shared_embedder() else {
        emit_member(
            app,
            project_id,
            "semantic-index-state",
            SemanticIndexStateEvent::Unavailable {
                reason: "No search-by-meaning model is installed. Download one in Settings › AI.".into(),
            },
        );
        return;
    };

    let token = CancelToken::new();
    if let Some(old) = cancel_slot.lock().unwrap().replace(token.clone()) {
        old.cancel();
    }
    // Marked running before the thread starts, so a change that lands now
    // waits for this build rather than starting another.
    semantic_builds().lock().unwrap_or_else(|p| p.into_inner()).insert(project_id, (0, 0));

    let bg_app = app.clone();
    let bg_embedder_slot = embedder_slot.clone();
    let bg_cancel_slot = cancel_slot.clone();
    std::thread::spawn(move || {
        let mut db = match Db::open(&base, project.config.id) {
            Ok(db) => db,
            Err(e) => {
                semantic_builds().lock().unwrap_or_else(|p| p.into_inner()).remove(&project_id);
                emit_member(
                    &bg_app,
                    project_id,
                    "semantic-index-state",
                    SemanticIndexStateEvent::Unavailable { reason: e.to_string() },
                );
                return;
            }
        };
        let mut embedder = SlotEmbedder { slot: bg_embedder_slot, model_id, dim };
        let progress_app = bg_app.clone();
        let mut last_emit = std::time::Instant::now() - std::time::Duration::from_secs(1);
        let progress = |done, total| {
            emit_member(
                &progress_app,
                project_id,
                "semantic-index-state",
                SemanticIndexStateEvent::Building { done, total },
            );
            semantic_builds().lock().unwrap_or_else(|p| p.into_inner()).insert(project_id, (done, total));
            if done == total || last_emit.elapsed() >= std::time::Duration::from_millis(500) {
                last_emit = std::time::Instant::now();
                emit_semantic_progress(&progress_app);
            }
        };
        let result = match mode {
            MeaningBuild::Full => engine::rebuild_semantic_index(&project, &mut db, &mut embedder, &token, progress),
            MeaningBuild::CatchUp => engine::catch_up_semantic_index(&project, &mut db, &mut embedder, &token, progress),
        };
        // A build another one replaced leaves the mark to it; one turned off,
        // or one that ran to its end, takes it down, and a change that landed
        // while it ran starts the catch-up now.
        let again = {
            let mut builds = semantic_builds().lock().unwrap_or_else(|p| p.into_inner());
            let replaced = token.is_cancelled() && bg_cancel_slot.lock().unwrap_or_else(|p| p.into_inner()).is_some();
            if replaced {
                false
            } else {
                builds.remove(&project_id);
                meaning_wanted().lock().unwrap_or_else(|p| p.into_inner()).remove(&project_id) && !token.is_cancelled()
            }
        };
        if again {
            queue_meaning_catch_up(&bg_app, project_id);
        }
        emit_semantic_progress(&bg_app);
        match result {
            Ok(true) => {
                emit_member(&bg_app, project_id, "semantic-index-state", SemanticIndexStateEvent::Ready);
                maybe_warn_large_index(&bg_app, project_id, &db, &warned_large);
            }
            Ok(false) => {
                // Cancelled (flag toggled off mid-build) — the disable path
                // already told the frontend, so no event here.
            }
            Err(e) => {
                emit_member(
                    &bg_app,
                    project_id,
                    "semantic-index-state",
                    SemanticIndexStateEvent::Unavailable { reason: e.to_string() },
                );
            }
        }
    });
}

/// Activate a project: register it, open its DB, start the watcher, and
/// kick off a background scan that reports through events.
///
/// `clear_others` controls whether pre-existing entries in `state.members`
/// are dropped first: `true` (single-project mode — `create_project` /
/// `open_project`) keeps the map at <=1 entry, matching the pre-workspace
/// behavior exactly; `false` (a workspace's members) leaves other open
/// members running so the map can hold more than one.
fn activate(app: &AppHandle, state: &SharedState, project: Project, clear_others: bool) -> CmdResult<ProjectInfo> {
    // Captured up front (Uuid is Copy) so every per-project event closure
    // below can move its own copy in without borrowing `project` itself.
    let project_id = project.config.id;
    // The asset protocol streams this project's videos to the webview with
    // range support and no JS memory copy. Grant its root at runtime — project
    // roots are chosen by the user, so the static config scope can't name them.
    {
        use tauri::Manager as _;
        let _ = app.asset_protocol_scope().allow_directory(&project.root, true);
    }

    // The registry, this repo's index and its one-time backfills read and
    // write only this repo's files: done before the lock, which every other
    // command needs, while a workspace opens its repos one after another.
    let base_dir = lock_tolerant(state).base_dir.clone();

    // A workspace's own `.ken-workspace` (its memory) is not a repo: it is
    // never registered, and an entry an earlier version wrote goes.
    let mut registry = Registry::load(&base_dir).map_err(err)?;
    let forgot = registry.forget_workspace_folders();
    if !ken_core::registry::is_workspace_folder(&project.root) {
        registry.add(&project);
        registry.last_project = Some(project.config.id);
        registry.save(&base_dir).map_err(err)?;
    } else if forgot {
        registry.save(&base_dir).map_err(err)?;
    }

    let mut db = Db::open(&base_dir, project.config.id).map_err(err)?;
    let watch_db_path = db_path(&base_dir, project.config.id);

    // One-time kind refresh: `files.kind` is STORED at index time, and the
    // scanner never re-classifies an unchanged file — so a file whose kind
    // definition changed after it was indexed (e.g. `.vtt` moving from "binary"
    // to editable "txt") keeps its stale stored kind, wrong glyph, and wrong
    // search-hit kind. Recompute every row's kind from its path and correct the
    // drifted ones. Rows transitioning from a non-content to a content kind also
    // get their stored mtime reset to a sentinel, so the initial background scan
    // below re-runs `index_one` and makes their content searchable + extractable.
    // Cheap (no byte reads), idempotent, and safe on every open.
    if let Err(e) = db.refresh_stored_kinds() {
        eprintln!("warning: stored-kind refresh failed: {e}");
    }

    // Pages indexed before frontmatter was read get their title, aliases,
    // status and checked stamp from the stored text.
    if let Err(e) = db.backfill_page_meta() {
        eprintln!("warning: page frontmatter backfill failed: {e}");
    }
    if let Err(e) = db.backfill_page_links() {
        eprintln!("warning: page links backfill failed: {e}");
    }
    // The code index for an index made before it: definitions, usages and
    // imports parsed from the stored text, no rescan.
    if let Err(e) = db.backfill_code_index() {
        eprintln!("warning: code index backfill failed: {e}");
    }

    let mut guard = state.lock().unwrap();

    // One-time unread baseline: snapshot the already-indexed files (this DB
    // persists across sessions, so an existing project has them here) as "seen"
    // so opening a project for the first time under this feature doesn't flag
    // every file. Only files that change or are added AFTER this point count as
    // unread. No-op once the project has been baselined before.
    {
        let _us = user_state_lock();
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
            // Ken's MCP server asks the app to open a file (only when the
            // person asked Claude to): the frontend focuses and opens it. It
            // also signals what it filed (`app_signal`).
            let ui_app = app.clone();
            h.set_ui_handler(move |v| {
                if v.get("signal").is_some() {
                    app_signal(&ui_app, v);
                } else {
                    let _ = ui_app.emit("ken-open", v);
                }
            });
            // Any Claude session's ken-mcp finds the listener here; the file
            // goes when the app quits.
            if let Err(e) = ken_core::hooks::write_listener_file(&guard.base_dir, &h.ui_url(), h.token()) {
                eprintln!("warning: could not write the listener file: {e}");
            }
            guard.hooks = Some(h.clone());
            h
        }
    };
    // Live on/off handle for the semantic-index build step (see
    // `MemberRuntime::semantic_embedder`).
    let semantic_embedder: EmbedderSlot = shared_embedder();
    // One-time latch for the ~50k chunk guardrail (semantic-index task 2.4).
    let semantic_index_warned_large = Arc::new(AtomicBool::new(false));

    // Conversation engine (chat drawer). Missing CLI → chats explain how to
    // install; everything else works.
    let chat_engine = ken_core::runner::discover_claude().map(|binary| {
        let chat_app = app.clone();
        let update_db = chat_db.clone();
        // Each tool card's message id by its tool_use_id, so its result
        // updates the card in place.
        let tool_cards: Mutex<std::collections::HashMap<String, i64>> = Mutex::new(std::collections::HashMap::new());
        Arc::new(ChatEngine::new(
            binary,
            project.root.clone(),
            move |update: ChatUpdate| {
                let now = engine::now_epoch();
                // Streamed text is shown, not kept: no database needed.
                if let ChatUpdate::Delta { chat_id, text } = &update {
                    emit_member(&chat_app, project_id, "chat-delta", serde_json::json!({
                        "chatId": chat_id,
                        "text": text,
                    }));
                    return;
                }
                let mut db = update_db.lock().unwrap();
                match update {
                    ChatUpdate::Delta { .. } => {}
                    ChatUpdate::Tool { chat_id, payload } => {
                        let id = db.append_chat_message(&chat_id, "tool", &payload, now).unwrap_or(0);
                        let _ = db.touch_chat(&chat_id, now);
                        if let Some(tool_use_id) = serde_json::from_str::<serde_json::Value>(&payload)
                            .ok()
                            .and_then(|v| v.get("toolUseId").and_then(|t| t.as_str()).map(str::to_string))
                            .filter(|t| !t.is_empty())
                        {
                            tool_cards.lock().unwrap().insert(tool_use_id, id);
                        }
                        emit_member(&chat_app, project_id, "chat-message", ChatMessage {
                            id,
                            chat_id,
                            role: "tool".into(),
                            content: payload,
                            created_at: now,
                        });
                    }
                    ChatUpdate::ToolResult { chat_id, tool_use_id, is_error, preview } => {
                        let Some(id) = tool_cards.lock().unwrap().remove(&tool_use_id) else { return };
                        let Ok(Some(old)) = db.chat_message_content(id) else { return };
                        let Ok(mut card) = serde_json::from_str::<serde_json::Value>(&old) else { return };
                        card["status"] = serde_json::json!(if is_error { "error" } else { "done" });
                        card["result"] = serde_json::json!(preview);
                        let content = card.to_string();
                        let _ = db.update_chat_message_content(id, &content);
                        emit_member(&chat_app, project_id, "chat-message", ChatMessage {
                            id,
                            chat_id,
                            role: "tool".into(),
                            content,
                            created_at: now,
                        });
                    }
                    ChatUpdate::Message { chat_id, role, content } => {
                        let id = db.append_chat_message(&chat_id, &role, &content, now).unwrap_or(0);
                        let _ = db.touch_chat(&chat_id, now);
                        emit_member(&chat_app, project_id, "chat-message", ChatMessage {
                            id,
                            chat_id,
                            role,
                            content,
                            created_at: now,
                        });
                    }
                    ChatUpdate::Question { chat_id, payload } => {
                        let id = db
                            .append_chat_message(&chat_id, "question", &payload, now)
                            .unwrap_or(0);
                        let _ = db.touch_chat(&chat_id, now);
                        emit_member(&chat_app, project_id, "chat-message", ChatMessage {
                            id,
                            chat_id,
                            role: "question".into(),
                            content: payload,
                            created_at: now,
                        });
                    }
                    ChatUpdate::EditProposal { chat_id, payload } => {
                        let id = db.append_chat_message(&chat_id, "edit", &payload, now).unwrap_or(0);
                        let _ = db.touch_chat(&chat_id, now);
                        emit_member(&chat_app, project_id, "chat-message", ChatMessage {
                            id,
                            chat_id,
                            role: "edit".into(),
                            content: payload,
                            created_at: now,
                        });
                    }
                    ChatUpdate::Status { chat_id, status, detail } => {
                        let _ = db.set_chat_field(&chat_id, ChatField::Status, &status);
                        if let Some(d) = detail {
                            let _ = db.append_chat_message(&chat_id, "activity", &d, now);
                            emit_member(&chat_app, project_id, "chat-message", ChatMessage {
                                id: 0,
                                chat_id: chat_id.clone(),
                                role: "activity".into(),
                                content: d,
                                created_at: now,
                            });
                        }
                        if let Ok(Some(row)) = db.get_chat(&chat_id) {
                            emit_member(&chat_app, project_id, "chat-updated", row);
                        }
                    }
                }
            },
        ))
    });
    // Ken's own MCP servers in every chat session of this project: the
    // knowledge server, and the personal set beside it (`chat_mcp_config`).
    if let (Some(engine), Some(mcp)) = (&chat_engine, find_ken_mcp()) {
        let cfg = chat_mcp_config(&mcp, &project.root, &hooks.ui_url(), hooks.token());
        let dir = guard.base_dir.join("chat-mcp");
        let path = dir.join(format!("{}.json", project.config.id));
        if std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, cfg.to_string())).is_ok() {
            engine.set_mcp_config(Some(path));
        }
    }
    // The workspace's other repos, readable in chat: when Ken's index misses
    // something, Claude looks for it by hand rather than saying it isn't there.
    if let (Some(engine), Some(ws)) = (&chat_engine, guard.workspace.as_ref()) {
        let roots: Vec<PathBuf> = ws
            .ws
            .members
            .iter()
            .filter_map(|m| match &m.status {
                ken_core::workspace::MemberStatus::Ok(p) => Some(p.root.clone()),
                _ => None,
            })
            .collect();
        engine.set_read_dirs(roots);
    }

    // Sync engine: active git sync when the folder is a repo with a
    // remote; passive conflicted-copy detection otherwise. Notices become
    // the title-bar dot (`sync-state`) and inbox refreshes (`review-changed`).
    let sync_app = app.clone();
    let sync_engine = Arc::new(
        SyncEngine::start(
            project.root.clone(),
            watch_db_path.clone(),
            // Per-project timers from the `sync` block, not the bare
            // defaults — see `sync::sync_config_for`.
            sync::sync_config_for(&project),
            move |notice| match notice {
                SyncNotice::State { state, detail } => {
                    emit_member(&sync_app, project_id, "sync-state", SyncStateEvent {
                        state: state.as_str().into(),
                        detail,
                    });
                }
                SyncNotice::ReviewChanged => {
                    // Unit payload — nothing to flatten `project_id` into, so
                    // this stays a plain, unscoped emit (S9 step 5 skip list).
                    let _ = sync_app.emit("review-changed", ());
                }
            },
        )
        .map_err(err)?,
    );

    // kenignore task 2.2 needs its own `Db` handle later (see the poller
    // below), so grab a clone before `watch::start` moves `watch_db_path`.
    let kenignore_db_path = watch_db_path.clone();

    let emit_app = app.clone();
    let watch_sync = sync_engine.clone();
    let watch_state = state.clone();
    let watch_project = project.clone();
    let watch_base = guard.base_dir.clone();
    let watch = watch::start(
        project.clone(),
        watch_db_path,
        Duration::from_secs(2),
        move |stats: &ScanStats| {
            if !stats.changed_paths.is_empty() {
                // A source landed in (or left) the inbox: read it, once the
                // changes settle.
                if stats.changed_paths.iter().any(|p| p.starts_with(&format!("{}/", ken_core::ingest::RAW))) {
                    schedule_ingest_pass(&watch_state, &watch_project, &watch_base);
                }
                watch_sync.changed(stats.changed_paths.clone());
            }
            enqueue_transcriptions(&emit_app, &watch_state, project_id, &stats.videos_needing_transcript);
            emit_index_updated(&emit_app, project_id, stats.clone());
        },
    )
    .map_err(err)?;

    // kenignore task 2.2: `watch::start` above deliberately never fires for
    // `.kenignore` itself — `relevant_path` in ken-core's `watch.rs` filters
    // out any path component starting with `.` (the same rule that skips
    // `.git`, editor swap files, etc.), and narrowing that shared filter
    // just for this one file risks un-filtering something else. So this is
    // a second, much narrower poller: every 2s, check whether `.kenignore`
    // changed (mtime+len), and if so re-scan.
    //
    // Tier-transition diffing for pre-existing, untouched files (ken-core
    // task 1.6) isn't implemented yet, so this covers the feasible subset
    // only: `scan::scan` re-reads `.kenignore` fresh (via
    // `Project::kenignore_rules`, which always reads from disk) and applies
    // the new rules to every file it walks from this point on. Files that
    // already have a DB row and aren't otherwise touched keep their old
    // tier until they're next modified or a manual `reindex` runs.
    let kenignore_stop = Arc::new(AtomicBool::new(false));
    {
        let kenignore_path = project.root.join(".kenignore");
        let db_path = kenignore_db_path;
        let poll_project = project.clone();
        let emit_app = app.clone();
        let stop = kenignore_stop.clone();
        std::thread::spawn(move || {
            fn snapshot(path: &std::path::Path) -> Option<(std::time::SystemTime, u64)> {
                let meta = std::fs::metadata(path).ok()?;
                Some((meta.modified().ok()?, meta.len()))
            }
            let mut last = snapshot(&kenignore_path);
            while !stop.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_secs(2));
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                let current = snapshot(&kenignore_path);
                if current == last {
                    continue;
                }
                last = current;

                let text = std::fs::read_to_string(&kenignore_path).unwrap_or_default();
                let malformed = kenignore::malformed_lines(&text);
                if !malformed.is_empty() {
                    emit_member(
                        &emit_app,
                        project_id,
                        "kenignore-warning",
                        KenignoreWarningEvent {
                            malformed_lines: malformed,
                        },
                    );
                }

                if let Ok(mut db) = Db::open_at(&db_path) {
                    if let Ok(stats) = scan::scan(&poll_project, &mut db) {
                        emit_index_updated(&emit_app, project_id, stats);
                    }
                }
            }
        });
    }

    let stop = Arc::new(AtomicBool::new(false));
    let ocr_stop = Arc::new(AtomicBool::new(false));
    let bg_stop = Arc::new(AtomicBool::new(false));
    let info = ProjectInfo::of(&project);
    let runtime = MemberRuntime {
        project: project.clone(),
        db,
        search_db,
        _watch: watch,
        semantic_embedder,
        semantic_build_cancel: Arc::new(Mutex::new(None)),
        semantic_index_warned_large,
        chat_engine,
        chat_db,
        sync: sync_engine.clone(),
        terminals: Arc::new(Mutex::new(std::collections::HashMap::new())),
        reindex_running: Arc::new(AtomicBool::new(false)),
        _background_worker: StopOnDrop(stop.clone()),
        _ocr_worker: StopOnDrop(ocr_stop.clone()),
        _bg_hydrate: StopOnDrop(bg_stop.clone()),
        _kenignore_watch: StopOnDrop(kenignore_stop.clone()),
        research: Arc::new(Mutex::new(std::collections::HashMap::new())),
        transcripts: Arc::new(Mutex::new(TranscriptJobs::default())),
    };
    // clear-before-insert keeps the map at <=1 entry (single-project mode) and
    // drops the old runtime only after the new one is fully built, matching the
    // previous single-assignment drop timing. Skipped when `clear_others` is
    // false so a workspace member joins the others already open.
    let focus_id = runtime.project.config.id;
    let replaced = if clear_others { std::mem::take(&mut guard.members) } else { Default::default() };
    guard.members.insert(focus_id, runtime);
    guard.focused = Some(focus_id);
    drop(guard);
    // Dropped with the lock released: dropping a runtime waits for its
    // watcher and sync threads, and those can be waiting for the lock.
    drop(replaced);

    // semantic-index task 2.1: resume the semantic index on open if the
    // project already had `semanticIndex` turned on in a previous session
    // (the flag persists in the project's `features` map, but the live
    // embedder slot in `MemberRuntime` always starts empty). Best-effort — a
    // project opening should never fail because of this.
    let resume_semantic = {
        let guard = state.lock().unwrap();
        semantic_index_enabled(&guard.app_settings, &project)
    };
    if resume_semantic {
        let _ = apply_semantic_index_flag(app, state, None, true);
    }

    // Fetch teammates' updates right after opening.
    sync_engine.pull_now();

    // Initial scan in the background so opening stays instant.
    let scan_app = app.clone();
    let scan_sync = sync_engine.clone();
    let scan_state = state.clone();
    let (base, scan_gate) = {
        let guard = state.lock().unwrap();
        (guard.base_dir.clone(), guard.ingest_gate.clone())
    };
    let scan_project = project.clone();
    std::thread::spawn(move || {
        // Cap concurrent member initial-scans (task 3.1 / design D4). Single-
        // project mode always has two free permits, so this never waits and
        // open timing is unchanged; a workspace opening N members staggers
        // them at most two at a time. `_permit` releases when this thread
        // finishes its scan.
        let _permit = scan_gate.acquire();
        if let Ok(mut db) = Db::open(&base, scan_project.config.id) {
            // Unit and scalar-string payloads — nothing for `emit_member` to
            // flatten `project_id` into, so these stay plain (S9 step 5 skip
            // list: "scan-started" is `()`, "scan-error" is a bare string).
            let _ = scan_app.emit("scan-started", ());
            match scan::scan(&scan_project, &mut db) {
                Ok(stats) => {
                    if !stats.changed_paths.is_empty() {
                        scan_sync.changed(stats.changed_paths.clone());
                    }
                    enqueue_transcriptions(&scan_app, &scan_state, project_id, &stats.videos_needing_transcript);
                    emit_index_updated(&scan_app, project_id, stats);
                    // The library inbox: anything waiting in Raw/. Drift and
                    // the lint run in the background worker (`knowledge_checks`).
                    start_ingest_pass(&scan_project, &base, code_sources_of(&scan_state, &scan_project.root), false);
                }
                Err(e) => {
                    let _ = scan_app.emit("scan-error", e.to_string());
                }
            }
        }
    });

    // One background worker per open project: one-line summaries for search,
    // a knowledge base's checks and its build queue, through Claude, on the
    // one background throttle.
    let worker_app = app.clone();
    let worker_state = state.clone();
    let worker_id = project.config.id;
    let worker_stop = stop.clone();
    std::thread::spawn(move || {
        background_worker(worker_app, worker_state, worker_id, worker_stop);
    });

    // One background OCR worker per open project: drains `ocr_pending`, runs
    // Vision on images/scanned PDFs, and merges the recognized text into the
    // index so it becomes searchable (and stores per-region boxes for the
    // Cmd+F overlay). Independent of the background worker above.
    let ocr_app = app.clone();
    let ocr_worker_state = state.clone();
    let ocr_id = project.config.id;
    let ocr_worker_stop = ocr_stop.clone();
    std::thread::spawn(move || {
        ocr_worker(ocr_app, ocr_worker_state, ocr_id, ocr_worker_stop);
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

    // The digest may be due the moment a repo opens (it waits for
    // the workspace to be installed; see `open_workspace_inner`).
    maybe_generate_digest(app, state);

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
        let (base, project, sync) = {
            let guard = state.lock().unwrap();
            // Own project by id. The old
            // `values().next()` plus id-mismatch check meant that with several
            // members open, every hydrate worker but one returned on its first
            // tick and their projects' cloud-only files were never pulled down.
            let Some(active) = guard.members.get(&project_id) else {
                return; // project closed — this worker is done
            };
            if !ken_core::bg_hydrate::background_index_enabled(&active.project) {
                continue; // feature off → idle, but keep the thread alive
            }
            (
                guard.base_dir.clone(),
                active.project.clone(),
                active.sync.clone(),
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
            // Each download is a step of the one background throttle
            // (knowledge-core decision 34): it waits while paused and paces
            // the backlog so it trickles rather than bursts.
            let halt = || stop.load(Ordering::SeqCst);
            if !ken_core::throttle::step_turn(&halt) {
                return;
            }
            let _running = ken_core::throttle::begin("cloud", &rel);

            match hydrate_emitting(&app, project_id, &rel, &abs, BG_HYDRATE_DEADLINE) {
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
                        // a just-downloaded doc reaches ingests and sync, and
                        // the footer's cloud_only count drops.
                        let paths = vec![rel.clone()];
                        sync.changed(paths.clone());
                        emit_index_updated(&app, project_id, ScanStats { changed_paths: paths, ..Default::default() });
                    }
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

#[tauri::command(async)]
fn create_project(
    app: AppHandle,
    state: State<SharedState>,
    path: String,
    name: String,
) -> CmdResult<ProjectInfo> {
    let project = Project::create(std::path::Path::new(&path), &name).map_err(err)?;
    activate(&app, &state, project, true)
}

#[tauri::command(async)]
fn open_project(app: AppHandle, state: State<SharedState>, path: String) -> CmdResult<ProjectInfo> {
    let project = Project::open(std::path::Path::new(&path)).map_err(err)?;
    activate(&app, &state, project, true)
}

/// Display label given to the workspace pseudo-member's `Project::name`
/// (ken-memory task 2.1) — shows up wherever a member's name flows through
/// unchanged (route_search's `MemberInfo::name`, say).
const WORKSPACE_MEMORY_LABEL: &str = "Workspace Memory";

/// Namespace prefix for a dismissed distillation candidate's slug inside the
/// pseudo-member's `UserState::ignored` set (see `resolve_distill_candidate`'s
/// doc comment for why `ignored` — not a dedicated field — is the honest home
/// available here).
const DISMISSED_MEMORY_PREFIX: &str = "distill-dismissed:";

/// The workspace pseudo-member's reserved project id, if a workspace is
/// currently open (ken-memory task 2.1) — `None` when none is. Pure function
/// of the open workspace's own id, independent of whether the pseudo-member
/// is actually resident in `AppState::members`; callers
/// that need residency check `guard.members.contains_key(..)`/`.get(..)`
/// themselves. Centralizes the `memory::workspace_pseudo_member_id` call so
/// every exclusion site (the search fan-out) derives
/// the same id the same way.
fn memory_pseudo_member_id(state: &AppState) -> Option<uuid::Uuid> {
    state
        .workspace
        .as_ref()
        .map(|ws| memory::workspace_pseudo_member_id(ws.ws.config.id))
}

// ── Workspace mode ─────────────────────────────────────────────────────────
//
// Built on the committed S9 shape (`AppState::members` map + `focused`), NOT
// design.md D2's `AppMode { Single | Workspace }` enum — see tasks.md 2.1's
// superseded note. `WorkspaceState` carries the enum's `Workspace` arm data
// (config + resolved members) alongside the existing map instead of replacing
// it, so single-project mode stays literally the old path.

/// Payload for the app-global `workspace-state` event (task 3.1). Tag-shaped
/// like `WorkspaceKgStateEvent`/`RoutedSearchStateEvent`, and app-global (a
/// plain `app.emit`, not `emit_member`) because a workspace lifecycle spans
/// every member at once and has no single owning project. Per-member
/// transitions ride the separate `member-status` event.
#[derive(Clone, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
enum WorkspaceStateEvent {
    /// Open/create started (members not yet activated).
    Opening { name: String },
    /// Open/create finished; the workspace is live.
    Open { id: String, name: String },
    /// Focus moved to a member (`focus_project` or open-time restore).
    Focus { project_id: String },
    /// The workspace was torn down (`close_workspace`).
    Closed,
}

/// Payload for the `member-status` event (task 3.1). Emitted through
/// `emit_member`, so the wire shape gains a flattened `project_id` on top of
/// these fields (spec/brief: "member-status carries project_id"). Only the
/// two *runtime* transitions a resolvable member goes through are events —
/// `active` (gained a live `MemberRuntime`) and `dormant` (evicted / opened
/// lazy-deferred). Missing/invalid members are reported through
/// `workspace_overview`, which has no `project_id` to scope an event to.
#[derive(Clone, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
enum MemberStatusEvent {
    Active { name: String },
    Dormant { name: String },
}

/// One member's row in `workspace_overview` (task 3.2).
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceMemberDto {
    /// Parent-relative folder name (the manifest member label).
    name: String,
    /// `Some` for resolvable (`Ok`) members, `None` for `missing`/`invalid`.
    project_id: Option<String>,
    /// `"active"` | `"dormant"` | `"missing"` | `"invalid"`.
    status: &'static str,
    /// The parse error for an `invalid` member; `None` otherwise.
    reason: Option<String>,
    /// Indexed file count, read off the member's DB. `Some` only for active
    /// members (whose read handle is already open); dormant/missing/invalid
    /// report `None` rather than paying to open a DB just for a count.
    file_count: Option<usize>,
}

/// `workspace_overview` return shape (task 3.2): the manifest header plus the
/// full member roster with per-member status and counts.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceOverviewDto {
    id: String,
    name: String,
    root: String,
    /// Focused member's project id, if any.
    focused: Option<String>,
    members: Vec<WorkspaceMemberDto>,
}

/// Build the workspace overview (task 3.2). Snapshots each member's facts and
/// the active members' read handles under one short lock, then releases the
/// global guard before doing any DB `file_count` IO — lock-audit discipline:
/// never hold the guard across a member loop doing IO.
fn build_workspace_overview(state: &SharedState) -> CmdResult<WorkspaceOverviewDto> {
    struct Snap {
        name: String,
        project_id: Option<uuid::Uuid>,
        status: &'static str,
        reason: Option<String>,
        search_db: Option<Arc<Mutex<Db>>>,
    }
    let (id, name, root, focused, snaps) = {
        let guard = state.lock().unwrap();
        let ws = guard.workspace.as_ref().ok_or("no workspace open")?;
        let snaps: Vec<Snap> = ws
            .ws
            .members
            .iter()
            .map(|m| match &m.status {
                ken_core::workspace::MemberStatus::Ok(p) => {
                    let pid = p.config.id;
                    let active = guard.members.contains_key(&pid);
                    Snap {
                        name: m.name.clone(),
                        project_id: Some(pid),
                        status: if active { "active" } else { "dormant" },
                        reason: None,
                        search_db: if active {
                            guard.members.get(&pid).map(|rt| rt.search_db.clone())
                        } else {
                            None
                        },
                    }
                }
                ken_core::workspace::MemberStatus::Missing => Snap {
                    name: m.name.clone(),
                    project_id: None,
                    status: "missing",
                    reason: None,
                    search_db: None,
                },
                ken_core::workspace::MemberStatus::Invalid(e) => Snap {
                    name: m.name.clone(),
                    project_id: None,
                    status: "invalid",
                    reason: Some(e.clone()),
                    search_db: None,
                },
            })
            .collect();
        (
            ws.ws.config.id.to_string(),
            ws.ws.config.name.clone(),
            ws.ws.root.to_string_lossy().into_owned(),
            guard.focused.map(|f| f.to_string()),
            snaps,
        )
    };
    let members = snaps
        .into_iter()
        .map(|s| {
            let file_count = s
                .search_db
                .as_ref()
                .and_then(|db| db.lock().ok().and_then(|db| db.file_count().ok()))
                .map(|c| c as usize);
            WorkspaceMemberDto {
                name: s.name,
                project_id: s.project_id.map(|p| p.to_string()),
                status: s.status,
                reason: s.reason,
                file_count,
            }
        })
        .collect();
    Ok(WorkspaceOverviewDto { id, name, root, focused, members })
}

/// Persist the open workspace into recents (best-effort): `add_workspace`
/// with the current focus + now, and `last_workspace` for reopen-on-launch
/// (task 3.4). Called on open/create and close so recents always reflect the
/// most recent focus (spec: "reopen restores focus").
fn record_workspace_recent(state: &SharedState) {
    let (base, ws_core, focused) = {
        let guard = state.lock().unwrap();
        match guard.workspace.as_ref() {
            Some(ws) => (guard.base_dir.clone(), ws.ws.clone(), guard.focused),
            None => return,
        }
    };
    if let Ok(mut reg) = Registry::load(&base) {
        reg.add_workspace(&ws_core, focused, engine::now_epoch());
        reg.last_workspace = Some(ws_core.config.id);
        let _ = reg.save(&base);
    }
}

/// Focus a workspace member, activating it if dormant and evicting the LRU
/// front when that pushes residents past `WORKSPACE_RESIDENT_CAP` (task 3.2).
/// Eviction is a plain `members.remove`, which drops the `MemberRuntime`:
/// its `StopOnDrop`/`Drop` fields stop the watcher and background workers. Shared by `focus_project`
/// and the open/reopen focus-restore path. Never holds the global guard while
/// calling `activate` (which locks internally).
fn focus_member_inner(app: &AppHandle, state: &SharedState, id: uuid::Uuid) -> CmdResult<()> {
    let resident = { state.lock().unwrap().members.contains_key(&id) };
    if !resident {
        // Dormant → activate now (lazy open, task 3.1). Root + label come from
        // the resolved manifest held in `WorkspaceState`.
        let (root, name) = {
            let guard = state.lock().unwrap();
            let ws = guard.workspace.as_ref().ok_or("no workspace open")?;
            let root = ws.member_root(id).ok_or("unknown workspace member")?;
            (root, ws.member_name(id).unwrap_or_default())
        };
        let project = Project::open(&root).map_err(err)?;
        activate(app, state, project, false)?;
        emit_member(app, id, "member-status", MemberStatusEvent::Active { name });
    }
    // Touch the LRU, set focus, and evict past the cap — one short lock.
    let evicted = {
        let mut guard = state.lock().unwrap();
        guard.focused = Some(id);
        match guard.workspace.as_mut() {
            Some(ws) => {
                ws.lru.retain(|x| *x != id);
                ws.lru.push(id);
                if ws.lru.len() > WORKSPACE_RESIDENT_CAP {
                    // Front is least-recently-focused and never the id we just
                    // pushed to the back, so this can't evict the new focus.
                    Some(ws.lru.remove(0))
                } else {
                    None
                }
            }
            None => None,
        }
    };
    if let Some(ev) = evicted {
        let name = {
            let mut guard = state.lock().unwrap();
            guard.members.remove(&ev); // drop runtime = close semantics
            guard
                .workspace
                .as_ref()
                .and_then(|ws| ws.member_name(ev))
                .unwrap_or_default()
        };
        emit_member(app, ev, "member-status", MemberStatusEvent::Dormant { name });
    }
    let _ = app.emit(
        "workspace-state",
        WorkspaceStateEvent::Focus { project_id: id.to_string() },
    );
    Ok(())
}

/// Reverse of `kenignore::parse` (ken-memory task 2.1): render `Rule`s back
/// to `.kenignore`-syntax lines (`Tier::Indexed` -> `!pattern`,
/// `Tier::Ignored` -> bare `pattern`). Lets
/// `activate_memory_pseudo_member` hand `memory::workspace_builtin_rules()`
/// to the existing `Project::kenignore_rules()`/`scan::scan` disk-read path
/// completely unchanged — see that function's doc comment for why this file-
/// based channel, not a new `rule_sets` parameter, is how the built-in rules
/// reach `classify` here.
fn render_builtin_kenignore(rules: &[kenignore::Rule]) -> String {
    let mut out = String::from(
        "# Auto-generated by Ken for the workspace-memory pseudo-member.\n\
         # Regenerated every workspace open — do not hand-edit, it will be\n\
         # overwritten. See ken_core::memory::workspace_builtin_rules().\n",
    );
    for r in rules {
        let prefix = match r.tier {
            kenignore::Tier::Indexed => "!",
            kenignore::Tier::Ignored => "",
        };
        out.push_str(prefix);
        out.push_str(&r.pattern);
        out.push('\n');
    }
    out
}

/// Spin up the workspace pseudo-member's own `MemberRuntime` (ken-memory
/// task 2.1 / design D3): one more `activate()` call, rooted at
/// `.ken-workspace/` itself (`ws_root.join(workspace::CONFIG_DIR)`), under
/// the deterministic reserved id `memory::workspace_pseudo_member_id(ws_id)`
/// so reopening the same workspace always resolves to the same derived DB.
///
/// `.ken-workspace/` is guaranteed to already exist (a workspace can't be
/// open without it), so the only folder created eagerly here is
/// `.ken-workspace/.ken/` — this pseudo-project's own metadata folder, via
/// `Project::save`, exactly like every other project has one.
/// `memory/`/`journal/`/`tasks/` stay absent until the first write (task
/// 2.1: "folders created lazily on first write, not on open").
///
/// Judgment call, recorded here rather than silently: this reuses `activate()`
/// unchanged instead of a bespoke slim spin-up path. `activate()` is the one
/// function that already builds a complete, correctly-wired `MemberRuntime`
/// (engine + watcher + `search_db` + every background worker), so duplicating
/// a parallel path risks subtle drift from it. Accepted side effects: the
/// pseudo-member also gets a chat drawer (harmless — nothing opens a chat
/// against it) and a passive sync engine (harmless — `.ken-workspace/` is
/// never itself a git repo), and — like every `activate()` call — is added
/// to the global project `Registry` and briefly becomes `AppState::focused`
/// (the caller's later `focus_member_inner` call corrects focus back; the
/// `Registry` entry means it *could* technically surface in a "recent
/// projects" list outside the current workspace's own member list — not
/// addressed here, out of this task's named exclusion list).
fn activate_memory_pseudo_member(
    app: &AppHandle,
    state: &SharedState,
    ws_root: &Path,
    ws_id: uuid::Uuid,
    _app_settings: &ken_core::settings::AppSettings,
) -> CmdResult<()> {
    let pseudo_id = memory::workspace_pseudo_member_id(ws_id);
    let pseudo_root = ws_root.join(ken_core::workspace::CONFIG_DIR);

    let project = if ken_core::project::config_path(&pseudo_root).exists() {
        Project::open(&pseudo_root).map_err(err)?
    } else {
        let config = ken_core::project::ProjectConfig {
            name: WORKSPACE_MEMORY_LABEL.to_string(),
            id: pseudo_id,
            excluded: Vec::new(),
            features: serde_json::Map::new(),
            extra: serde_json::Map::new(),
        };
        let project = Project { root: pseudo_root.clone(), config };
        project.save().map_err(err)?;
        project
    };

    // The pseudo-member's own index rules (memory/, journal/ and tasks/
    // indexed, workspace.json and kg.sqlite not), regenerated on every
    // activation. They reach `scan::scan` through the `.kenignore` that
    // `Project::index_rules` reads as legacy, written here by
    // `render_builtin_kenignore`. Best-effort: a write failure leaves every
    // file of it indexed rather than blocking the open.
    let kenignore_path = pseudo_root.join(".kenignore");
    let rules = memory::workspace_builtin_rules();
    let text = render_builtin_kenignore(&rules);
    if let Err(e) = std::fs::write(&kenignore_path, text) {
        eprintln!("warning: failed to write workspace-memory pseudo-member .kenignore: {e}");
    }

    activate(app, state, project, false)?;
    Ok(())
}

/// Shared open/create path (task 3.1): tear down whatever is currently open,
/// activate resolvable members up to the resident cap (the rest stay dormant),
/// restore focus, and record recents. Returns the initial overview so the
/// caller gets the workspace's state in one round-trip. Each `activate` runs
/// off the global guard (it locks internally) — the loop never holds the
/// guard, honoring the lock-audit rule.
fn open_workspace_inner(
    app: &AppHandle,
    state: &SharedState,
    ws: ken_core::workspace::Workspace,
) -> CmdResult<WorkspaceOverviewDto> {
    // Restore-focus target from recents (spec: "reopen restores focus"), read
    // before we mutate any state.
    let restore = {
        let guard = state.lock().unwrap();
        Registry::load(&guard.base_dir).ok().and_then(|r| {
            r.workspaces
                .iter()
                .find(|w| w.id == ws.config.id)
                .and_then(|w| w.last_focused)
        })
    };
    // Tear down any currently-open project or workspace. The runtimes drop
    // with the lock released (see `activate`).
    let closed = {
        let mut guard = state.lock().unwrap();
        guard.focused = None;
        guard.workspace = None;
        std::mem::take(&mut guard.members)
    };
    drop(closed);
    let _ = app.emit(
        "workspace-state",
        WorkspaceStateEvent::Opening { name: ws.config.name.clone() },
    );

    // Resolvable (`Ok`) members, in manifest order.
    let ok_members: Vec<(String, PathBuf, uuid::Uuid)> = ws
        .members
        .iter()
        .filter_map(|m| match &m.status {
            ken_core::workspace::MemberStatus::Ok(p) => {
                Some((m.name.clone(), p.root.clone(), p.config.id))
            }
            _ => None,
        })
        .collect();

    // ken-memory tasks 2.1/2.3: spin up the workspace pseudo-member and roll
    // the journal archive when the flag resolves on. Run before the real
    // members' activation loop below so `Registry::last_project` (which
    // every `activate()` call overwrites — see `activate_memory_pseudo_
    // member`'s doc comment) ends up on a real project rather than the
    // pseudo-member's reserved id once the loop re-overwrites it; order here
    // is purely about that side effect, not about `AppState::focused`
    // correctness (the explicit `focus_member_inner` call at the end of this
    // function fixes that regardless of order). Both steps are best-effort —
    // a failure here must never fail `open_workspace`/`create_workspace`.
    let settings_now = { state.lock().unwrap().app_settings.clone() };
    {
        if let Err(e) = activate_memory_pseudo_member(app, state, &ws.root, ws.config.id, &settings_now) {
            eprintln!("warning: workspace-memory pseudo-member failed to activate: {e}");
        }
        let roll_root = ws.root.clone();
        std::thread::spawn(move || {
            let today = local_date_today();
            if let Err(e) = memory::roll_archive(&roll_root, &today) {
                eprintln!("warning: journal archive roll failed: {e}");
            }
            // D6: an archive roll is also meant to *offer* distillation.
            // Deferred — no frontend/event contract for "nudge the user to
            // distill" exists yet in this codebase (task 2.3 asks only for
            // the roll itself to run on open); `distill_journal` is still
            // reachable on demand via its own command.
        });
    }


    // Activate up to the cap; the rest stay dormant (their project id + root
    // remain tracked in `WorkspaceState.ws` for lazy focus-time activation).
    let mut resident_lru: Vec<uuid::Uuid> = Vec::new();
    for (name, root, id) in ok_members.iter().take(WORKSPACE_RESIDENT_CAP) {
        match Project::open(root) {
            Ok(project) => {
                activate(app, state, project, false)?;
                resident_lru.push(*id);
                emit_member(
                    app,
                    *id,
                    "member-status",
                    MemberStatusEvent::Active { name: name.clone() },
                );
            }
            Err(e) => {
                // Resolved `Ok` at manifest time but failed to reopen now
                // (e.g. deleted between resolve and activate) — skipped, not
                // fatal, matching `Workspace::open`'s per-member tolerance.
                eprintln!("warning: workspace member {name} failed to open: {e}");
            }
        }
    }
    for (name, _root, id) in ok_members.iter().skip(WORKSPACE_RESIDENT_CAP) {
        emit_member(
            app,
            *id,
            "member-status",
            MemberStatusEvent::Dormant { name: name.clone() },
        );
    }

    // A repo with the inbox and no kind said is the knowledge base: say so.
    {
        let guard = state.lock().unwrap();
        if let Ok(mut reg) = Registry::load(&guard.base_dir) {
            let mut changed = false;
            for (_, root, _) in &ok_members {
                changed |= reg.infer_library(root);
            }
            if changed {
                if let Err(e) = reg.save(&guard.base_dir) {
                    eprintln!("warning: could not record the wiki's kind: {e}");
                }
            }
        }
    }

    // Install bookkeeping (holds the resolved manifest + resident LRU).
    {
        let mut guard = state.lock().unwrap();
        guard.workspace = Some(WorkspaceState {
            ws,
            lru: resident_lru.clone(),
            task_watch: None,
        });
        guard.focused = resident_lru.last().copied();
        // The members were activated before the workspace was installed, so
        // their chats could not be given the other repos then: every repo
        // of the workspace is readable from any of its chats.
        let roots: Vec<PathBuf> = guard
            .workspace
            .as_ref()
            .map(|w| {
                w.ws.members
                    .iter()
                    .filter_map(|m| match &m.status {
                        ken_core::workspace::MemberStatus::Ok(p) => Some(p.root.clone()),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        for rt in guard.members.values() {
            if let Some(engine) = rt.chat_engine.as_ref() {
                engine.set_read_dirs(roots.clone());
            }
        }
    }

    // Your day's watcher: task files and ticket files, now
    // that `guard.workspace` is installed (every tick reads it). It emits
    // `day-changed`; nothing here creates a folder.
    {
        let handle = spawn_task_board_watch(app.clone(), state.clone());
        let mut guard = state.lock().unwrap();
        if let Some(ws_state) = guard.workspace.as_mut() {
            ws_state.task_watch = Some(handle);
        }
    }
    // The morning digest, now that the workspace is installed.
    maybe_generate_digest(app, state);

    // Restore focus, or default to the first resident member. Routed through
    // the shared focus path so a dormant restore target activates + evicts
    // consistently.
    let focus_target = restore
        .filter(|rid| ok_members.iter().any(|(_, _, mid)| mid == rid))
        .or_else(|| ok_members.first().map(|(_, _, id)| *id));
    if let Some(t) = focus_target {
        focus_member_inner(app, state, t)?;
    }

    record_workspace_recent(state);
    let overview = build_workspace_overview(state)?;
    let _ = app.emit(
        "workspace-state",
        WorkspaceStateEvent::Open {
            id: overview.id.clone(),
            name: overview.name.clone(),
        },
    );
    Ok(overview)
}

/// Open an existing workspace (`<parent>/.ken-workspace/workspace.json`) and
/// bring its members online (task 3.1).
#[tauri::command(async)]
fn open_workspace(
    app: AppHandle,
    state: State<SharedState>,
    parent: String,
) -> CmdResult<WorkspaceOverviewDto> {
    let ws = ken_core::workspace::Workspace::open(Path::new(&parent)).map_err(err)?;
    open_workspace_inner(&app, state.inner(), ws)
}

/// Create a new workspace over `parent` from the selected member folder names,
/// then open it (task 3.1).
#[tauri::command(async)]
fn create_workspace(
    app: AppHandle,
    state: State<SharedState>,
    parent: String,
    name: String,
    members: Vec<String>,
) -> CmdResult<WorkspaceOverviewDto> {
    let ws = ken_core::workspace::Workspace::create(Path::new(&parent), &name, &members)
        .map_err(err)?;
    open_workspace_inner(&app, state.inner(), ws)
}

/// Members + per-member status + counts for the open workspace (task 3.2).
#[tauri::command(async)]
fn workspace_overview(state: State<SharedState>) -> CmdResult<WorkspaceOverviewDto> {
    build_workspace_overview(state.inner())
}

/// Focus a member, activating a dormant one and LRU-evicting past the cap
/// (task 3.2).
#[tauri::command(async)]
fn focus_project(app: AppHandle, state: State<SharedState>, id: String) -> CmdResult<()> {
    let uuid: uuid::Uuid = id.parse().map_err(err)?;
    focus_member_inner(&app, state.inner(), uuid)
}

/// Set-up from repos picked one by one, wherever they are: a row per repo
/// (a group folder stands for the repos in it). Reads only.
#[tauri::command]
async fn setup_propose_repos(state: State<'_, SharedState>, paths: Vec<String>) -> CmdResult<ken_core::setup::Proposal> {
    // Names already in the open workspace, so an added repo gets its own.
    let taken: Vec<String> = {
        let guard = state.lock().unwrap();
        guard.workspace.as_ref().map(|w| w.ws.config.members.clone()).unwrap_or_default()
    };
    let paths: Vec<std::path::PathBuf> = paths.into_iter().map(std::path::PathBuf::from).collect();
    tauri::async_runtime::spawn_blocking(move || ken_core::setup::propose_repos(&paths, &taken).map_err(err))
        .await
        .map_err(|e| e.to_string())?
}

/// Confirm picked repos. With `add` false, a new workspace in Ken's app
/// data (`workspaces/<id>`); with `add` true, into the open workspace. Then opens it so indexing starts.
#[tauri::command(async)]
fn setup_confirm_repos(
    app: AppHandle,
    state: State<SharedState>,
    name: String,
    rows: Vec<ken_core::setup::RepoRow>,
    add: bool,
) -> CmdResult<WorkspaceOverviewDto> {
    let (base, root) = {
        let guard = state.lock().unwrap();
        let root = if add {
            guard.workspace.as_ref().ok_or("no workspace open to add to")?.ws.root.clone()
        } else {
            ken_core::setup::workspaces_dir(&guard.base_dir).join(uuid::Uuid::new_v4().to_string())
        };
        (guard.base_dir.clone(), root)
    };
    let ws = ken_core::setup::confirm_repos(&base, &root, &name, &rows).map_err(err)?;
    let overview = open_workspace_inner(&app, state.inner(), ws)?;
    analyze_new_sources(&app, state.inner());
    Ok(overview)
}

/// How long the repo analysis of one source may take.
const ANALYSIS_TIMEOUT: Duration = Duration::from_secs(300);

/// The repo analysis (knowledge-core decisions 17 and 19), when sources are
/// added: for each source of the open workspace whose kind is said, whose
/// knowledge base lists it, which has no rules or controls there yet and was
/// not proposed before, one Claude call over its file list proposes its role
/// rules and controls. They wait as one Inbox card in the knowledge base's
/// index for a person to accept or dismiss (Chris, 2026-10-14); nothing is
/// written into `.ken/knowledge.json` until then ([`accept_analysis`]).
/// Without Claude nothing is asked and the built-in core roles apply. In the
/// background.
fn analyze_new_sources(app: &AppHandle, state: &SharedState) {
    let (base, sources): (PathBuf, Vec<Project>) = {
        let guard = lock_tolerant(state);
        let sources = match guard.workspace.as_ref() {
            Some(ws) => ws
                .ws
                .members
                .iter()
                .filter_map(|m| match &m.status {
                    ken_core::workspace::MemberStatus::Ok(p) => Some(p.clone()),
                    _ => None,
                })
                .collect(),
            None => Vec::new(),
        };
        (guard.base_dir.clone(), sources)
    };
    let app = app.clone();
    std::thread::spawn(move || {
        let Some(binary) = ken_core::runner::discover_claude() else {
            return;
        };
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        for project in &sources {
            let kinds = ken_core::registry::kind_of(&project.root);
            if kinds.is_empty() {
                continue;
            }
            let Some(settings) = ken_core::repo_analysis::due(&project.root) else {
                continue;
            };
            // The card waits in the knowledge base's own index.
            let Some(kb) = sources.iter().find(|p| canon(&p.root) == canon(&settings.kb_root)) else {
                continue;
            };
            let Ok(mut kb_db) = Db::open(&base, kb.config.id) else {
                continue;
            };
            let root = project.root.clone();
            // Background work: a turn of the one throttle (decision 34).
            let mut generate = |prompt: &str| -> ken_core::Result<String> {
                let _turn = ken_core::throttle::claude_turn(&|| false);
                let _running = ken_core::throttle::begin("analysis", &project.config.name);
                match ken_core::assistant::oneshot_answer(&binary, &root, prompt, None, ANALYSIS_TIMEOUT, &CancelToken::new())? {
                    ken_core::assistant::OneshotOutcome::Completed(text) => Ok(text),
                    other => Err(ken_core::Error::Other(format!("claude: {other:?}"))),
                }
            };
            let filed = ken_core::repo_analysis::analyze_and_propose(
                &project.root,
                Some(settings),
                &kinds,
                &mut kb_db,
                &mut generate,
                engine::now_epoch(),
            );
            match filed {
                Ok(Some(_)) => {
                    let _ = app.emit("review-changed", ());
                }
                Ok(None) => {}
                Err(e) => eprintln!("repo analysis of {}: {e}", project.config.name),
            }
        }
    });
}

/// Accept the repo analysis waiting as Inbox item `item_id` in the
/// workspace's knowledge base (Chris, 2026-10-14): its role rules and
/// controls go into `.ken/knowledge.json` as one commit naming the file
/// (`repo_analysis::accept`), and the source it is for is scanned again so
/// its files take their roles. Dismiss is `resolve_review_item`. Returns the
/// commit, when one was made.
#[tauri::command(async)]
fn accept_analysis(app: AppHandle, state: State<SharedState>, item_id: i64, project_id: Option<String>) -> CmdResult<Option<String>> {
    let pid = project_id.as_deref().map(|p| p.parse::<uuid::Uuid>()).transpose().map_err(err)?;
    let (source, accepted, base, open) = {
        let mut guard = state.lock().unwrap();
        let base = guard.base_dir.clone();
        let open: Vec<Project> = guard.members.values().map(|m| m.project.clone()).collect();
        let active = member_mut(&mut guard, pid)?;
        let root = active.project.root.clone();
        let (source, accepted) = ken_core::repo_analysis::accept(&root, &mut active.db, item_id, engine::now_epoch()).map_err(err)?;
        (source, accepted, base, open)
    };
    if accepted.written {
        for p in &open {
            let named = ken_core::knowledge_settings::for_repo(&p.root).is_some_and(|s| s.name.eq_ignore_ascii_case(&source));
            if named {
                rescan_member(&app, &base, p);
            }
        }
    }
    let _ = app.emit("review-changed", ());
    Ok(accepted.commit)
}

/// The repo analyses waiting in the workspace's knowledge base, for the
/// Inbox: each with its role rules and controls.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AnalysisDto {
    item_id: i64,
    project_id: String,
    title: String,
    created_at: i64,
    #[serde(flatten)]
    proposal: ken_core::repo_analysis::Proposal,
}

#[tauri::command(async)]
fn analysis_proposals(state: State<SharedState>) -> CmdResult<Vec<AnalysisDto>> {
    let found = {
        let guard = lock_tolerant(&state);
        inbox_project(&guard).and_then(|p| guard.members.get(&p.config.id).map(|m| (p.config.id, m.search_db.clone())))
    };
    let Some((id, db)) = found else { return Ok(Vec::new()) };
    let open = ken_core::repo_analysis::open(&lock_tolerant(&db)).map_err(err)?;
    Ok(open
        .into_iter()
        .map(|(it, proposal)| AnalysisDto { item_id: it.id, project_id: id.to_string(), title: it.title, created_at: it.created_at, proposal })
        .collect())
}

/// Sources whose analysis a person asked for again, while it is queued for
/// the throttle's turn or running; Settings › Workspace shows them busy.
static ANALYSING: std::sync::LazyLock<Mutex<std::collections::HashSet<uuid::Uuid>>> =
    std::sync::LazyLock::new(|| Mutex::new(std::collections::HashSet::new()));

/// A source's place in [`ANALYSING`], given up however its analysis ends.
struct Analysing(uuid::Uuid);

impl Drop for Analysing {
    fn drop(&mut self) {
        ANALYSING.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.0);
    }
}

/// "Analyse again" in Settings › Workspace (Chris, 2026-10-14): the mark
/// that keeps the source `project_id` from being analysed again is cleared
/// now (`repo_analysis::forget`), and one Claude call is queued in the
/// background, a turn of the one throttle counted against the hour like any
/// analysis. Its proposal arrives as a new Inbox card in the knowledge
/// base's index, replacing one still waiting for the source
/// (`repo_analysis::analyze_again`); Accept and Dismiss are the usual ones.
/// Refused for a code repo that keeps its own settings file, where Ken
/// writes nothing (decision 36). Asking while it is queued or running does
/// nothing more.
#[tauri::command(async)]
fn analyze_source_again(app: AppHandle, state: State<SharedState>, project_id: String) -> CmdResult<()> {
    let id: uuid::Uuid = project_id.parse().map_err(err)?;
    let (base, sources): (PathBuf, Vec<Project>) = {
        let guard = lock_tolerant(&state);
        let ws = guard.workspace.as_ref().ok_or("no workspace open")?;
        let sources = ws
            .ws
            .members
            .iter()
            .filter_map(|m| match &m.status {
                ken_core::workspace::MemberStatus::Ok(p) => Some(p.clone()),
                _ => None,
            })
            .collect();
        (guard.base_dir.clone(), sources)
    };
    let project = sources.iter().find(|p| p.config.id == id).cloned().ok_or("that source is not in the workspace")?;
    let name = project.config.name.clone();
    let kinds = ken_core::registry::kind_of(&project.root);
    if kinds.is_empty() {
        return Err(format!("Nobody has said what {name} is: pick its kind first."));
    }
    let settings = ken_core::knowledge_settings::for_repo(&project.root).ok_or("no knowledge base lists this source")?;
    if !ken_core::repo_analysis::writable(&settings) {
        return Err(format!("{name} keeps its own settings file, and Ken writes nothing in a code repo."));
    }
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let kb = sources.iter().find(|p| canon(&p.root) == canon(&settings.kb_root)).ok_or("its knowledge base is not open")?;
    let kb_id = kb.config.id;
    let binary = ken_core::runner::discover_claude().ok_or("Claude Code is not there to ask")?;
    if !ANALYSING.lock().unwrap_or_else(|e| e.into_inner()).insert(id) {
        return Ok(());
    }
    let place = Analysing(id);
    let mut kb_db = Db::open(&base, kb_id).map_err(err)?;
    ken_core::repo_analysis::forget(&mut kb_db, &settings.name).map_err(err)?;
    std::thread::spawn(move || {
        let _place = place;
        let root = project.root.clone();
        // Background work: a turn of the one throttle (decision 34).
        let mut generate = |prompt: &str| -> ken_core::Result<String> {
            let _turn = ken_core::throttle::claude_turn(&|| false);
            let _running = ken_core::throttle::begin("analysis", &name);
            match ken_core::assistant::oneshot_answer(&binary, &root, prompt, None, ANALYSIS_TIMEOUT, &CancelToken::new())? {
                ken_core::assistant::OneshotOutcome::Completed(text) => Ok(text),
                other => Err(ken_core::Error::Other(format!("claude: {other:?}"))),
            }
        };
        let filed = ken_core::repo_analysis::analyze_again(
            &project.root,
            Some(settings),
            &kinds,
            &mut kb_db,
            &mut generate,
            engine::now_epoch(),
        );
        match filed {
            Ok(_) => ken_core::throttle::clear_failure("analysis", &name),
            Err(e) => {
                eprintln!("repo analysis of {name}, asked again: {e}");
                ken_core::throttle::fail("analysis", &name, &e.to_string());
            }
        }
        drop(_place);
        let _ = app.emit("review-changed", ());
    });
    Ok(())
}

/// Say what a repo is for, in a person's words; the first-wiki draft reads it.
#[tauri::command]
fn set_project_description(state: State<SharedState>, id: String, description: String) -> CmdResult<Vec<RegistryEntryStatus>> {
    let guard = state.lock().unwrap();
    let uuid: uuid::Uuid = id.parse().map_err(err)?;
    let mut registry = Registry::load(&guard.base_dir).map_err(err)?;
    if !registry.set_description(uuid, &description) {
        return Err(format!("no project with id {id}"));
    }
    registry.save(&guard.base_dir).map_err(err)?;
    Ok(registry.statuses())
}

/// Scan again: what moved since set-up (new folders, gone members, new
/// worktrees). Reports; never changes the set-up.
#[tauri::command]
async fn setup_rescan(parent: String) -> CmdResult<Vec<ken_core::setup::Moved>> {
    tauri::async_runtime::spawn_blocking(move || ken_core::setup::rescan(Path::new(&parent)).map_err(err))
        .await
        .map_err(|e| e.to_string())?
}

/// Close the open workspace, tearing down every member handle (task 3.2).
/// `members.clear()` drops all runtimes; recents get the last focus + now.
#[tauri::command(async)]
fn close_workspace(app: AppHandle, state: State<SharedState>) -> CmdResult<()> {
    let (base, ws_core, focused, closed) = {
        let mut guard = state.lock().unwrap();
        let ws = guard.workspace.take().ok_or("no workspace open")?;
        let focused = guard.focused.take();
        // Every runtime goes (full teardown); they drop below, with the lock
        // released (see `activate`).
        let closed = std::mem::take(&mut guard.members);
        (guard.base_dir.clone(), ws.ws, focused, closed)
    };
    drop(closed);
    if let Ok(mut reg) = Registry::load(&base) {
        reg.add_workspace(&ws_core, focused, engine::now_epoch());
        reg.last_workspace = Some(ws_core.config.id);
        let _ = reg.save(&base);
    }
    let _ = app.emit("workspace-state", WorkspaceStateEvent::Closed);
    Ok(())
}

/// Say what a repo is for: its kinds. Kept in the local registry, never in
/// the repo. Only the knowledge base syncs (decision 36), and the dot is
/// refreshed at once for an open member.
#[tauri::command]
fn set_project_kind(
    app: AppHandle,
    state: State<SharedState>,
    id: String,
    kind: Vec<ken_core::registry::RepoKind>,
) -> CmdResult<Vec<RegistryEntryStatus>> {
    let guard = state.lock().unwrap();
    let uuid: uuid::Uuid = id.parse().map_err(err)?;
    let mut registry = Registry::load(&guard.base_dir).map_err(err)?;
    if !registry.set_kind(uuid, kind) {
        return Err(format!("no project with id {id}"));
    }
    registry.save(&guard.base_dir).map_err(err)?;
    if let Some(member) = guard.members.get(&uuid) {
        let status = sync_status_of(&member.project, &member.sync.pending());
        emit_member(&app, uuid, "sync-state", SyncStateEvent {
            state: if status.active { "synced" } else { "off" }.into(),
            detail: None,
        });
        rescan_member(&app, &guard.base_dir, &member.project);
    }
    drop(guard);
    // A repo given its kind is a source added: analyse it once.
    analyze_new_sources(&app, state.inner());
    Ok(registry.statuses())
}

/// Scan an open repo again in the background, after what it is changed: a
/// repo given its kind is read for the first time, and one switched off or
/// on loses or gains its files.
fn rescan_member(app: &AppHandle, base: &Path, project: &Project) {
    let (app, base, project) = (app.clone(), base.to_path_buf(), project.clone());
    std::thread::spawn(move || {
        if let Ok(mut db) = Db::open(&base, project.config.id) {
            if let Ok(stats) = scan::scan(&project, &mut db) {
                emit_index_updated(&app, project.config.id, stats);
            }
        }
    });
}

/// Switch a repo's index off, or back on with None or `indexed` (the two
/// states of knowledge-core decision 34). The next scan drops or reads its
/// files.
#[tauri::command]
fn set_project_index(
    app: AppHandle,
    state: State<SharedState>,
    id: String,
    index: Option<ken_core::registry::IndexState>,
) -> CmdResult<Vec<RegistryEntryStatus>> {
    let guard = state.lock().unwrap();
    let uuid: uuid::Uuid = id.parse().map_err(err)?;
    let mut registry = Registry::load(&guard.base_dir).map_err(err)?;
    if !registry.set_index(uuid, index) {
        return Err(format!("no project with id {id}"));
    }
    registry.save(&guard.base_dir).map_err(err)?;
    if let Some(member) = guard.members.get(&uuid) {
        rescan_member(&app, &guard.base_dir, &member.project);
    }
    Ok(registry.statuses())
}

/// The workspaces opened recently, newest first, each with whether its folder
/// is still there: the start screen's "Recent workspaces".
#[tauri::command(async)]
fn list_recent_workspaces(state: State<SharedState>) -> CmdResult<Vec<ken_core::registry::RecentWorkspaceStatus>> {
    let guard = state.lock().unwrap();
    let mut recent = Registry::load(&guard.base_dir).map_err(err)?.workspace_statuses();
    recent.sort_by(|a, b| b.entry.opened_at.cmp(&a.entry.opened_at));
    Ok(recent)
}

/// Take a workspace off the recent list. Its folders and files stay.
#[tauri::command(async)]
fn forget_workspace(state: State<SharedState>, id: String) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let uuid: uuid::Uuid = id.parse().map_err(err)?;
    let mut registry = Registry::load(&guard.base_dir).map_err(err)?;
    registry.remove_workspace(uuid);
    if registry.last_workspace == Some(uuid) {
        registry.last_workspace = None;
    }
    registry.save(&guard.base_dir).map_err(err)
}

#[tauri::command]
fn current_project(state: State<SharedState>) -> CmdResult<Option<ProjectInfo>> {
    let guard = state.lock().unwrap();
    // `member(.., None)` resolves through `focused`. Taking an arbitrary
    // `members.values().next()` here reported a random open project as the
    // current one once a workspace held more than one member.
    Ok(member(&guard, None).ok().map(|a| ProjectInfo::of(&a.project)))
}

#[tauri::command(async)]
fn set_folder_selection(
    app: AppHandle,
    state: State<SharedState>,
    excluded: Vec<String>,
) -> CmdResult<ProjectInfo> {
    let mut guard = state.lock().unwrap();
    let active = member_mut(&mut guard, None)?;
    // Once the older ignore files have moved into `.ken/knowledge.json`, the
    // folders are lines there; until then, `project.json`'s `excluded`.
    if !ken_core::knowledge_settings::set_excluded_folders(&active.project.root, &excluded).map_err(err)? {
        active.project.set_excluded(excluded).map_err(err)?;
    }
    let stats = scan::scan(&active.project, &mut active.db).map_err(err)?;
    let info = ProjectInfo::of(&active.project);
    let project_id = active.project.config.id;
    let videos = stats.videos_needing_transcript.clone();
    drop(guard);
    enqueue_transcriptions(&app, state.inner(), project_id, &videos);
    emit_index_updated(&app, project_id, stats);
    Ok(info)
}

#[tauri::command(async)]
fn get_tree(state: State<SharedState>) -> CmdResult<TreeData> {
    // What the read needs, cloned under the lock; the listing and the walk
    // happen after it is released. Indexing asks for the tree again and
    // again, and every other command waits on this lock.
    let (project, search_db) = {
        let mut guard = lock_tolerant(&state);
        let active = member_mut(&mut guard, None)?;
        (active.project.clone(), active.search_db.clone())
    };
    let files = lock_tolerant(&search_db).list_files().map_err(err)?;
    let excluded = project.excluded_folders();

    // Folders straight from disk so excluded/empty ones still show.
    let mut folders = Vec::new();
    let walker = ignore::WalkBuilder::new(&project.root)
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
        if entry.path().is_dir() && entry.path() != project.root {
            if let Ok(rel) = entry.path().strip_prefix(&project.root) {
                let rel = rel.to_string_lossy().replace('\\', "/");
                folders.push(FolderInfo {
                    excluded: ken_core::project::in_folders(&excluded, &rel),
                    rel_path: rel,
                });
            }
        }
    }
    folders.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    let files = files
        .into_iter()
        .map(|row| FileRowDto::new(row, &excluded))
        .collect();
    Ok(TreeData { files, folders })
}

/// Flip a per-project boolean feature flag. The flag must be registered and
/// project-scoped (validated against the registry so a frontend typo surfaces
/// immediately instead of quietly doing nothing). The value is written to the
/// project's typed `features` map; for `semanticIndex` the legacy top-level
/// `extra` key is removed in the same save, so first write migrates old files.
/// Per-flag side effects run after persisting (semanticIndex resumes/stops the
/// index build).
#[tauri::command(async)]
fn set_project_feature(
    app: AppHandle,
    state: State<SharedState>,
    flag: String,
    value: bool,
) -> CmdResult<()> {
    match ken_core::features::flag(&flag) {
        Some(def) if def.scope == ken_core::features::FlagScope::Project => {}
        Some(_) => return Err(format!("feature flag is not project-scoped: {flag}")),
        None => return Err(format!("unknown feature flag: {flag}")),
    }

    let mut project = {
        let guard = state.lock().unwrap();
        member(&guard, None)?.project.clone()
    };
    project
        .config
        .features
        .insert(flag.clone(), serde_json::Value::Bool(value));
    // Lazy migration: the value now lives in the typed `features` map, so drop
    // the legacy top-level key the old code wrote to `extra`.
    if flag == "semanticIndex" {
        project.config.extra.remove("semanticIndex");
    }
    project.save().map_err(err)?;

    // Keep the in-memory copy (read by `semantic_index_enabled` elsewhere,
    // e.g. a future re-open) in sync with what was just persisted.
    {
        let mut guard = state.lock().unwrap();
        // Write back to the member this project actually IS. The read above
        // resolves through `focused`, but the write used to land on an
        // arbitrary `values_mut().next()` — so in a workspace, flipping a flag
        // on the focused project could overwrite a *different* member's
        // in-memory project with the focused one's root, name and id.
        if let Some(active) = guard.members.get_mut(&project.config.id) {
            active.project = project.clone();
        }
    }

    // Per-flag side effects.
    if flag == "semanticIndex" {
        apply_semantic_index_flag(&app, state.inner(), None, value)?;
    }
    Ok(())
}

/// Set a global (per-user) feature-flag default in `settings.json`. Accepts any
/// registered flag regardless of scope: global-scoped flags live only here, and
/// project-scoped flags use the global layer as their default when no project
/// override is set. The durable write happens first; the in-memory
/// `AppSettings` in `AppState` is updated only after it succeeds, so a failed
/// save leaves state and disk consistent.
#[tauri::command(async)]
fn set_global_feature(_app: AppHandle, state: State<SharedState>, flag: String, value: bool) -> CmdResult<()> {
    if ken_core::features::flag(&flag).is_none() {
        return Err(format!("unknown feature flag: {flag}"));
    }
    let (base_dir, mut settings) = {
        let guard = state.lock().unwrap();
        (guard.base_dir.clone(), guard.app_settings.clone())
    };
    settings
        .features
        .insert(flag.clone(), serde_json::Value::Bool(value));
    settings.save(&base_dir).map_err(err)?;
    state.lock().unwrap().app_settings = settings;
    Ok(())
}

/// One registered flag's resolved state, for the Settings and onboarding UIs.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct FeatureInfo {
    name: String,
    /// `global` | `project` | `workspace`.
    scope: String,
    description: String,
    /// The plain name shown in Settings, and where the flag applies.
    label: String,
    applies: String,
    /// The global-layer value: `settings.json` if set, else the registry default.
    global: bool,
    /// The project's own override — `Some` only when a project-scoped flag is
    /// explicitly set by the requested project (includes the legacy location).
    project_override: Option<bool>,
    /// Resolved value after applying the full precedence chain.
    effective: bool,
}

fn scope_str(scope: ken_core::features::FlagScope) -> &'static str {
    match scope {
        ken_core::features::FlagScope::Global => "global",
        ken_core::features::FlagScope::Project => "project",
        ken_core::features::FlagScope::Workspace => "workspace",
    }
}

/// The raw value the project layer contributes for `name`, or `None` if the
/// project sets no override. Mirrors the project tiers of `effective_flag`: the
/// typed `features` map, then the legacy `extra["semanticIndex"]` location.
fn project_layer_value(project: &Project, name: &str) -> Option<bool> {
    if let Some(v) = project
        .config
        .features
        .get(name)
        .and_then(serde_json::Value::as_bool)
    {
        return Some(v);
    }
    if name == "semanticIndex" {
        if let Some(v) = project
            .config
            .extra
            .get(name)
            .and_then(serde_json::Value::as_bool)
        {
            return Some(v);
        }
    }
    None
}

/// List every registered feature flag with its resolved state — one command
/// feeds both the Settings section and the onboarding disclosure. With no
/// `project_id`, only the global/registry layers are reflected (project
/// overrides absent). With one, project-scoped flags also report that project's
/// override and full effective value; the project is read from the active copy
/// when it's open, otherwise loaded from disk via the registry.
#[tauri::command(async)]
fn list_features(
    state: State<SharedState>,
    project_id: Option<String>,
) -> CmdResult<Vec<FeatureInfo>> {
    // Snapshot global settings and (if requested and open) the active project,
    // without holding the lock across any file I/O below.
    let (base_dir, app_settings, active_project) = {
        let guard = state.lock().unwrap();
        let active = match &project_id {
            Some(id) => guard
                .members
                .values()
                .next()
                .filter(|a| a.project.config.id.to_string() == *id)
                .map(|a| a.project.clone()),
            None => None,
        };
        (guard.base_dir.clone(), guard.app_settings.clone(), active)
    };

    // A requested project that isn't the open one is loaded from disk via the
    // registry, so overrides read true even when it isn't active.
    let project = match (&project_id, active_project) {
        (Some(_), Some(p)) => Some(p),
        (Some(id), None) => {
            let uuid = uuid::Uuid::parse_str(id).map_err(|_| "invalid project id".to_string())?;
            let registry = Registry::load(&base_dir).map_err(err)?;
            let entry = registry
                .projects
                .iter()
                .find(|e| e.id == uuid)
                .ok_or("unknown project")?;
            Some(Project::open(&entry.path).map_err(err)?)
        }
        (None, _) => None,
    };

    let features = ken_core::features::FLAGS
        .iter()
        .map(|def| {
            let global = app_settings
                .features
                .get(def.name)
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(def.default);
            let (project_override, effective) = match &project {
                Some(p) if def.scope == ken_core::features::FlagScope::Project => (
                    project_layer_value(p, def.name),
                    ken_core::features::effective_flag(&app_settings, p, def.name),
                ),
                Some(p) => (
                    None,
                    ken_core::features::effective_flag(&app_settings, p, def.name),
                ),
                None => (None, global),
            };
            FeatureInfo {
                name: def.name.to_string(),
                scope: scope_str(def.scope).to_string(),
                description: def.description.to_string(),
                label: def.label.to_string(),
                applies: def.applies.to_string(),
                global,
                project_override,
                effective,
            }
        })
        .collect();
    Ok(features)
}

/// Is the semantic (meaning-based) index enabled for the active project?
/// Mirrors `get_background_index`/`get_transcribe_on_index`: resolves the
/// `semanticIndex` flag through `semantic_index_enabled` — the same helper
/// `activate()` uses to decide whether to resume the build on open. Lets the
/// frontend initialize the toggle from the effective value instead of always
/// starting it at `false` on project activation.
#[tauri::command]
fn get_semantic_index(state: State<SharedState>) -> CmdResult<bool> {
    let guard = state.lock().unwrap();
    let active = member(&guard, None)?;
    Ok(semantic_index_enabled(&guard.app_settings, &active.project))
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
    let active = member(&guard, None)?;
    active.project.resolve(rel_path).map_err(err)
}

#[tauri::command(async)]
fn read_file(state: State<SharedState>, rel_path: String) -> CmdResult<String> {
    let abs = resolve_path(&state, &rel_path)?;
    if cloud::is_placeholder(&abs) {
        return Err(CLOUD_ONLY_ERR.into());
    }
    std::fs::read_to_string(&abs).map_err(err)
}

#[tauri::command(async)]
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

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct HydrationProgress {
    rel_path: String,
    downloaded: u64,
    total: u64,
}

/// Hydrate a placeholder while streaming `hydration-progress` events, throttled
/// exactly like model downloads. The terminal `(total, total)` sample always
/// goes out so the UI can treat 100% as "the bytes are here".
fn hydrate_emitting(
    app: &AppHandle,
    project_id: uuid::Uuid,
    rel_path: &str,
    abs: &Path,
    deadline: Duration,
) -> ken_core::Result<()> {
    let mut throttle = model::ProgressThrottle::new();
    let start = Instant::now();
    let app = app.clone();
    let rel = rel_path.to_string();
    cloud::hydrate_with_progress(abs, deadline, move |downloaded, total| {
        let now_ms = start.elapsed().as_millis() as u64;
        let done = total > 0 && downloaded >= total;
        if done || throttle.should_emit(downloaded, total, now_ms) {
            emit_member(
                &app,
                project_id,
                "hydration-progress",
                HydrationProgress { rel_path: rel.clone(), downloaded, total },
            );
        }
    })
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

    // Snapshot everything the download, re-index, and notifications need, then
    // drop the global lock immediately — none of this may hold it.
    let (base, project, sync) = {
        let guard = state.lock().unwrap();
        let active = member(&guard, None)?;
        (guard.base_dir.clone(), active.project.clone(), active.sync.clone())
    };
    let project_id = project.config.id;

    let path = abs.clone();
    let progress_app = app.clone();
    let progress_rel = rel_path.clone();
    tauri::async_runtime::spawn_blocking(move || {
        hydrate_emitting(&progress_app, project_id, &progress_rel, &path, cloud::DEFAULT_DEADLINE)
    })
    .await
    .map_err(err)?
    .map_err(err)?;

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
    // file, so a just-downloaded doc reaches ingests and sync immediately
    // instead of waiting for an edit or a manual reindex.
    if changed {
        let paths = vec![rel_path.clone()];
        sync.changed(paths.clone());
        emit_index_updated(&app, project_id, ScanStats { changed_paths: paths, ..Default::default() });
    }
    Ok(())
}

/// Post-write bookkeeping shared by `save_file` and `save_file_bytes`: reindex
/// the file, mark the version as seen, tell sync Ken wrote it (decision 36:
/// in the knowledge base, sync commits only what Ken wrote), notify the
/// frontend, return the mtime. Assumes the bytes are already on disk at
/// `rel_path`.
fn finish_save(app: &AppHandle, project: &Project, base: &Path, rel_path: &str, sync: &SyncEngine) -> CmdResult<i64> {
    let abs = project.resolve(rel_path).map_err(err)?;
    sync.wrote(vec![rel_path.to_string()]);
    let project_id = project.config.id;
    // Index it now, on its own connection: no wait for the watcher, and no
    // other command waits on the lock meanwhile. The file is written; an
    // index error is the watcher's to retry, not a failed save.
    let seen_version = match Db::open(base, project_id) {
        Ok(mut db) => {
            if let Err(e) = scan::refresh_path(project, &mut db, rel_path) {
                eprintln!("warning: indexing {rel_path} after a save failed: {e}");
            }
            db.get_file(rel_path).ok().flatten().map(|r| (r.size, r.mtime))
        }
        Err(e) => {
            eprintln!("warning: could not open the index after saving {rel_path}: {e}");
            None
        }
    };
    let mtime = abs
        .metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // Record the just-written version as seen so the user's OWN edit never
    // counts as unread — the whole point of unread being "changed by someone
    // else".
    if let Some(version) = seen_version {
        let _us = user_state_lock();
        let mut us = UserState::load(base, project_id);
        if us.mark_seen(rel_path, version) {
            let _ = us.save(base, project_id);
        }
    }
    let _ = app.emit("file-saved", rel_path);
    Ok(mtime)
}

/// The focused member's project, the app data dir and its sync engine,
/// cloned under the lock for a save that writes and indexes after releasing
/// it.
fn save_target(state: &SharedState) -> CmdResult<(Project, PathBuf, Arc<SyncEngine>)> {
    let guard = lock_tolerant(state);
    let active = member(&guard, None)?;
    Ok((active.project.clone(), guard.base_dir.clone(), active.sync.clone()))
}

// Saves stay on the main thread, so they run in the order they were made.
#[tauri::command]
fn save_file(
    app: AppHandle,
    state: State<SharedState>,
    rel_path: String,
    content: String,
) -> CmdResult<i64> {
    let (project, base, sync) = save_target(state.inner())?;
    let abs = project.resolve(&rel_path).map_err(err)?;
    std::fs::write(&abs, &content).map_err(err)?;
    finish_save(&app, &project, &base, &rel_path, &sync)
}

/// Overwrite a file with raw bytes. Used by the PDF form filler: `bytes` is the
/// WHOLE file, not a patch — the caller hands over a fully re-serialized
/// document. Otherwise identical to `save_file`.
#[tauri::command]
fn save_file_bytes(
    app: AppHandle,
    state: State<SharedState>,
    rel_path: String,
    bytes: Vec<u8>,
) -> CmdResult<i64> {
    let (project, base, sync) = save_target(state.inner())?;
    let abs = project.resolve(&rel_path).map_err(err)?;
    std::fs::write(&abs, &bytes).map_err(err)?;
    finish_save(&app, &project, &base, &rel_path, &sync)
}

/// Create a NEW folder `name` inside `dir_rel` ("" = root) for a folder dropped
/// in from the OS; returns its project-relative path. A taken name is deduped
/// (`Photos` → `Photos-1`) so a drop never merges into an existing folder.
#[tauri::command]
fn create_unique_folder(state: State<SharedState>, dir_rel: String, name: String) -> CmdResult<String> {
    let (project, _, _) = save_target(state.inner())?;
    let dir = project.resolve(&dir_rel).map_err(err)?;
    let created = ken_core::fsops::create_unique_dir(&dir, &name).map_err(err)?;
    Ok(if dir_rel.is_empty() {
        created
    } else {
        format!("{dir_rel}/{created}")
    })
}

/// Save a file pasted or dropped into a Markdown document or the file tree as a
/// NEW file in the folder `x-dir` (project-relative, "" = root), named after
/// `x-name`; returns
/// the project-relative path it landed at. The name is sanitized and deduped
/// (`shot.png` → `shot-1.png`), never overwriting. The bytes come as the raw
/// IPC body rather than a JSON number array (~4x smaller for a screenshot);
/// the two strings ride in percent-encoded headers since header values are
/// ASCII-only.
#[tauri::command]
fn write_new_file(
    app: AppHandle,
    state: State<SharedState>,
    request: tauri::ipc::Request<'_>,
) -> CmdResult<String> {
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err("write_new_file expects the file bytes as a raw body".into());
    };
    let header = |name: &str| -> CmdResult<String> {
        let value = request
            .headers()
            .get(name)
            .ok_or_else(|| format!("missing {name} header"))?;
        let value = value.to_str().map_err(err)?;
        Ok(ken_core::drawio::percent_decode(value))
    };
    let dir_rel = header("x-dir")?;
    let file_name = header("x-name")?;

    let (project, base, sync) = save_target(state.inner())?;
    // `resolve` keeps the folder inside the project; the name is reduced to a
    // single sanitized component, so the file cannot land anywhere else.
    let dir = project.resolve(&dir_rel).map_err(err)?;
    let (mut file, name) = ken_core::fsops::create_unique_file(&dir, &file_name).map_err(err)?;
    {
        use std::io::Write;
        file.write_all(bytes).map_err(err)?;
    }
    drop(file);
    let rel_path = if dir_rel.is_empty() {
        name
    } else {
        format!("{dir_rel}/{name}")
    };
    finish_save(&app, &project, &base, &rel_path, &sync)?;
    Ok(rel_path)
}

#[tauri::command(async)]
fn file_meta(state: State<SharedState>, rel_path: String) -> CmdResult<Option<FileRowDto>> {
    let guard = state.lock().unwrap();
    let active = member(&guard, None)?;
    let mut row = active.db.get_file(&rel_path).map_err(err)?;
    // Read-time authority over `kind`: the column is STORED at index time, so a
    // file whose classification changed after it was indexed (e.g. `.vtt` moving
    // from "binary" to editable "txt") keeps a stale kind until a reindex. The
    // editor decides read-only/editable from this value, so recompute it from the
    // path here — one cheap `from_path` — and override the stored kind. Falls back
    // to the stored kind if the path can't be resolved to an absolute path.
    if let Some(r) = row.as_mut() {
        if let Ok(abs) = active.project.resolve(&rel_path) {
            r.kind = ken_core::extract::FileKind::from_path(&abs).as_str().to_string();
        }
    }
    Ok(row.map(|r| FileRowDto::new(r, &active.project.excluded_folders())))
}

#[tauri::command(async)]
fn extracted_text(state: State<SharedState>, rel_path: String) -> CmdResult<String> {
    let guard = state.lock().unwrap();
    let active = member(&guard, None)?;
    let abs = active.project.resolve(&rel_path).map_err(err)?;
    Ok(ken_core::extract::extract(&abs)
        .map(|e| e.text)
        .unwrap_or_default())
}

/// One OCR text region for the Phase 3 Cmd+F highlight overlay. `bbox` is
/// `[x, y, w, h]`, normalized to the unit square with a **top-left** origin
/// (x grows right, y grows down), matching `crate::ocr::OcrRegion`. Keep this
/// shape stable — the frontend overlay depends on it.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct OcrRegionDto {
    /// 0-based page index (always 0 for a single image).
    page: i64,
    text: String,
    /// `[x, y, w, h]`, normalized, top-left origin.
    bbox: [f32; 4],
}

/// Stored OCR regions for a file, in reading order — the input to the Cmd+F
/// highlight overlay. Empty when the file was never OCR'd (or held no text);
/// the OCR pass is asynchronous, so an image just added may return `[]` until
/// the background worker finishes it.
#[tauri::command(async)]
fn get_ocr_regions(state: State<SharedState>, rel_path: String) -> CmdResult<Vec<OcrRegionDto>> {
    let guard = state.lock().unwrap();
    let active = member(&guard, None)?;
    let rows = active.db.get_ocr_regions(&rel_path).map_err(err)?;
    Ok(rows
        .into_iter()
        .map(|r| OcrRegionDto { page: r.page, text: r.text, bbox: r.bbox })
        .collect())
}

#[tauri::command(async)]
fn reindex(app: AppHandle, state: State<SharedState>) -> CmdResult<ScanStats> {
    // Reindex is a full `db.clear()` + recursive rescan — far too heavy to run
    // inline on the IPC thread while holding the global state mutex (that froze
    // the whole app and every worker that needs the lock). Instead we mirror the
    // initial activation scan: lock only long enough to read what the rebuild
    // needs (and to claim a guard so two rebuilds can't stack), then run the
    // clear+rescan on a detached thread with its OWN `Db::open` handle. That
    // handle writes to the same SQLite file the main `active.db` reads, so once
    // it commits, the main handle and the read-only search handle see the
    // rebuilt index. Progress reaches the UI via `scan-started`/`index-updated`
    // /`scan-error`, and the command returns immediately so the button never
    // hangs.
    let (project, base, sync, running) = {
        let guard = state.lock().unwrap();
        let active = member(&guard, None)?;
        (active.project.clone(), guard.base_dir.clone(), active.sync.clone(), active.reindex_running.clone())
    };
    // Already rebuilding? Don't stack a second clear+rescan.
    if running.swap(true, Ordering::SeqCst) {
        return Ok(ScanStats::default());
    }

    let project_id = project.config.id;
    let bg_app = app.clone();
    let bg_state = state.inner().clone();
    std::thread::spawn(move || {
        match Db::open(&base, project.config.id) {
            Ok(mut db) => {
                let _ = bg_app.emit("scan-started", ());
                match scan::reindex(&project, &mut db) {
                    Ok(stats) => {
                        // A from-scratch rebuild touches everything: push the
                        // changed set to sync and re-enqueue any videos that
                        // still need transcripts.
                        if !stats.changed_paths.is_empty() {
                            sync.changed(stats.changed_paths.clone());
                        }
                        enqueue_transcriptions(
                            &bg_app,
                            &bg_state,
                            project_id,
                            &stats.videos_needing_transcript,
                        );
                        emit_index_updated(&bg_app, project_id, stats);
                    }
                    Err(e) => {
                        let _ = bg_app.emit("scan-error", e.to_string());
                    }
                }
            }
            Err(e) => {
                let _ = bg_app.emit("scan-error", e.to_string());
            }
        }
        running.store(false, Ordering::SeqCst);
    });

    // Return immediately; the real stats arrive on the `index-updated` event.
    Ok(ScanStats::default())
}

#[tauri::command]
fn open_external(state: State<SharedState>, app: AppHandle, rel_path: String) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let active = member(&guard, None)?;
    let abs = active.project.resolve(&rel_path).map_err(err)?;
    tauri_plugin_opener::OpenerExt::opener(&app)
        .open_path(abs.to_string_lossy(), None::<&str>)
        .map_err(err)
}

/// Show a file in Finder/Explorer (selected, not opened). Same shape as
/// `open_external`: the path is resolved against the project root here so the
/// frontend never handles an absolute path.
#[tauri::command]
fn reveal_in_folder(state: State<SharedState>, app: AppHandle, rel_path: String, project_id: Option<String>) -> CmdResult<()> {
    let pid = project_id.as_deref().map(|p| p.parse::<uuid::Uuid>()).transpose().map_err(err)?;
    let guard = state.lock().unwrap();
    let active = member(&guard, pid)?;
    let abs = active.project.resolve(&rel_path).map_err(err)?;
    tauri_plugin_opener::OpenerExt::opener(&app)
        .reveal_item_in_dir(abs)
        .map_err(err)
}

/// Open a web link in the system browser (e.g. the target of a `.url` file).
/// Routed through Rust like `record_open_settings`, so validate the scheme
/// server-side: only http(s) may open — never `file:` or an app scheme that
/// would launch something local from indexed content.
#[tauri::command]
fn open_web_url(app: AppHandle, url: String) -> CmdResult<()> {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("refused to open a non-web URL".into());
    }
    tauri_plugin_opener::OpenerExt::opener(&app)
        .open_url(url, None::<&str>)
        .map_err(err)
}

// ===========================================================================
// Record: on-device meeting recorder session, commands, and throttled events.
// The pure logic (state machine, downmix, resampler, WAV wrappers, merge) lives
// in `ken_core::record`; this owns the live session and wires backends →
// downmix/resample → WAV writers → transcription → project write.
// ===========================================================================

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct AudioDeviceDto {
    id: String,
    name: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct RecordPermissionsDto {
    mic: record::PermissionStatus,
    screen: record::PermissionStatus,
    mic_settings_url: String,
    screen_settings_url: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct RecordLevel {
    source: Source,
    rms: f32,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct RecordStateDto {
    phase: record::Phase,
    elapsed_ms: u64,
    mic: bool,
    system: bool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct RecordSaved {
    /// The transcript's path in the library's Raw/; for an audio-only
    /// recording, the folder in app data its audio was kept in.
    rel_path: String,
    /// The library the transcript is in; none for an audio-only recording.
    project_id: Option<String>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct RecordErrorDto {
    message: String,
    can_retry: bool,
}

/// One live recording. Owns the phase/timing state, the per-source WAV writers
/// and resamplers, the meter throttles, and the capture backends. All mutation
/// happens under `AppState.record`'s mutex except the audio callbacks, which
/// write through `Arc<Mutex<..>>` channel state created at start.
struct RecordSession {
    state: RecorderState,
    started: Instant,
    /// The take's WAVs while it runs, in app data (`record/<id>/`).
    tmp_dir: PathBuf,
    /// The library whose Raw/ the transcript goes into.
    project_id: uuid::Uuid,
    // Per-source capture backends (stopped on finish).
    mic: Option<Box<dyn CaptureSource>>,
    system: Option<Box<dyn CaptureSource>>,
    // Shared writer/meter state each callback appends to. Retained through
    // stop so `record_stop` can read each channel's WAV path after finalizing.
    mic_chan: Option<Arc<Mutex<ChannelWriter>>>,
    system_chan: Option<Arc<Mutex<ChannelWriter>>>,
}

/// A single source's WAV writer + resampler + meter throttle. The audio
/// callback locks this, downmixes/resamples the device block, writes 16 kHz
/// samples, and (throttled) emits a level event. The writer is an `Option` so
/// the stop path can flush the resampler tail and `finalize()` it under the
/// same mutex — and so a late callback arriving after finalize simply no-ops.
struct ChannelWriter {
    writer: Option<hound::WavWriter<std::io::BufWriter<std::fs::File>>>,
    resampler: LinearResampler,
    /// The resampler is created lazily on the first callback, when the device's
    /// true rate is known.
    resampler_ready: bool,
    path: PathBuf,
    paused: bool,
    last_emit: Instant,
    app: AppHandle,
    source: Source,
    /// Write the silence a source does not deliver (WASAPI loopback sends
    /// nothing while nothing plays), so this channel keeps the recording's
    /// clock and Me and Them line up.
    pad_to_clock: bool,
    /// 16 kHz samples written so far.
    written: u64,
    /// The recording clock: when this channel started, the time spent
    /// paused, and when the current pause began.
    started: Instant,
    paused_total: Duration,
    paused_at: Option<Instant>,
}

impl ChannelWriter {
    fn new(
        writer: hound::WavWriter<std::io::BufWriter<std::fs::File>>,
        path: PathBuf,
        app: AppHandle,
        source: Source,
        pad_to_clock: bool,
    ) -> Self {
        ChannelWriter {
            writer: Some(writer),
            resampler: LinearResampler::new(record::TARGET_RATE, record::TARGET_RATE), // replaced on first feed
            resampler_ready: false,
            path,
            paused: false,
            last_emit: Instant::now(),
            app,
            source,
            pad_to_clock,
            written: 0,
            started: Instant::now(),
            paused_total: Duration::ZERO,
            paused_at: None,
        }
    }

    /// Recording time on this channel: wall time less the pauses.
    fn clock_ms(&self) -> u64 {
        let paused_now = self.paused_at.map(|p| p.elapsed()).unwrap_or_default();
        self.started.elapsed().saturating_sub(self.paused_total + paused_now).as_millis() as u64
    }

    fn set_paused(&mut self, paused: bool) {
        match (self.paused, paused) {
            (false, true) => self.paused_at = Some(Instant::now()),
            (true, false) => {
                if let Some(at) = self.paused_at.take() {
                    self.paused_total += at.elapsed();
                }
            }
            _ => {}
        }
        self.paused = paused;
    }

    fn write(&mut self, samples: &[f32]) {
        if let Some(w) = self.writer.as_mut() {
            for s in samples {
                let _ = w.write_sample(record::f32_to_i16(*s));
            }
        }
        self.written += samples.len() as u64;
    }

    fn feed(&mut self, device: &[f32], rate: u32, channels: u16) {
        if self.paused {
            return;
        }
        // Create the resampler on the first callback, when the true device rate
        // is known.
        if !self.resampler_ready {
            self.resampler = LinearResampler::new(rate, record::TARGET_RATE);
            self.resampler_ready = true;
        }
        let mono = record::downmix_to_mono(device, channels);
        if self.pad_to_clock && rate > 0 {
            let block = (mono.len() as u64 * record::TARGET_RATE as u64 / rate as u64) as usize;
            let gap = record::silence_gap(self.written, self.clock_ms(), block);
            if gap > 0 {
                self.write(&vec![0.0; gap]);
            }
        }
        let samples = self.resampler.process(&mono);
        let level = record::rms(&samples);
        self.write(&samples);
        // ~10 Hz meter, throttled per source.
        if self.last_emit.elapsed().as_millis() >= 100 {
            self.last_emit = Instant::now();
            let _ = self
                .app
                .emit("record-level", RecordLevel { source: self.source, rms: level });
        }
    }

    /// Flush the resampler's trailing sample (handoff: `ingest_frames` never
    /// flushed, so the tail of a recording was lost) and finalize the WAV.
    fn finalize_flush(&mut self) {
        if self.resampler_ready {
            let tail = self.resampler.finish();
            self.write(&tail);
        }
        if let Some(w) = self.writer.take() {
            let _ = w.finalize();
        }
    }
}

/// A single input device's `(id, name)`.
#[tauri::command]
fn record_input_devices() -> CmdResult<Vec<AudioDeviceDto>> {
    Ok(record::cpal::list_input_devices()
        .into_iter()
        .map(|(id, name)| AudioDeviceDto { id, name })
        .collect())
}

/// What this machine can record: a microphone, system audio (Windows:
/// WASAPI loopback of the default output; macOS: ScreenCaptureKit), and why
/// not when one is missing.
#[tauri::command]
fn record_support() -> CmdResult<record::Support> {
    Ok(record::support())
}

#[tauri::command]
fn record_permissions() -> CmdResult<RecordPermissionsDto> {
    #[cfg(target_os = "macos")]
    let (mic, screen) = (record::mac::mic_permission(), record::mac::screen_permission());
    // System audio on Windows needs no permission; the microphone has its
    // privacy switch.
    #[cfg(windows)]
    let (mic, screen) = (record::win::mic_permission(), record::PermissionStatus::Granted);
    #[cfg(not(any(target_os = "macos", windows)))]
    let (mic, screen) = (record::PermissionStatus::Granted, record::PermissionStatus::Unsupported);
    Ok(RecordPermissionsDto {
        mic,
        screen,
        mic_settings_url: record::mic_settings_url().into(),
        screen_settings_url: record::screen_settings_url().into(),
    })
}

#[tauri::command]
fn record_request_permission(kind: String) -> CmdResult<()> {
    // Only macOS can ask; Windows reads its privacy switch, which a person
    // turns on in Settings (`record_open_settings`).
    #[cfg(target_os = "macos")]
    match kind.as_str() {
        "mic" => record::mac::request_mic(),
        "screen" => {
            let _ = record::mac::request_screen();
        }
        _ => return Err("unknown permission".into()),
    }
    let _ = kind;
    Ok(())
}

/// Open this OS's privacy settings page for recording. Routed through Rust
/// (rather than the JS opener) because the frontend `opener:default`
/// capability scope does not permit the `x-apple.systempreferences:` or
/// `ms-settings:` schemes; a server-side `open_url` bypasses that scope.
/// `open_external` can't be reused — it resolves its argument against the
/// project root as a file path.
#[tauri::command]
fn record_open_settings(app: AppHandle, url: String) -> CmdResult<()> {
    // Routing through Rust bypasses the frontend `opener:default` scope for ANY
    // scheme, so validate server-side: only this OS's own privacy pages may
    // open — anything else (file:, arbitrary app schemes) is refused.
    if !record::is_settings_url(&url) {
        return Err("refused to open an unrecognized settings URL".into());
    }
    tauri_plugin_opener::OpenerExt::opener(&app)
        .open_url(url, None::<&str>)
        .map_err(err)
}

/// Start a take: the microphone and/or system audio, into WAVs in app data.
/// The transcript goes into the knowledge base's Raw/ when the take stops. Refused when no requested source could start.
#[tauri::command]
fn record_start(
    app: AppHandle,
    state: State<SharedState>,
    mic: bool,
    system: bool,
    device_id: Option<String>,
) -> CmdResult<()> {
    if !mic && !system {
        return Err("Pick at least one source to record.".into());
    }
    let support = record::support();
    if system && !support.system {
        return Err(support.reason.unwrap_or_else(|| "System audio can't be recorded on this computer.".into()));
    }
    if mic && !support.mic {
        return Err(support.reason.unwrap_or_else(|| "No microphone is connected.".into()));
    }
    // Windows records silence from a microphone its privacy switch blocks.
    #[cfg(windows)]
    if mic && record::win::mic_permission() == record::PermissionStatus::Denied {
        return Err("Microphone access is off in Windows Settings › Privacy & security › Microphone. Turn it on for desktop apps, then try again.".into());
    }
    // Where the transcript will go, found now so a take is never made with
    // nowhere to put it.
    let project_id = inbox_member(&app, &state)?;
    let base = { state.lock().unwrap().base_dir.clone() };
    let slot = { state.lock().unwrap().record.clone() };
    // Guard against a second recording.
    {
        let existing = slot.lock().unwrap();
        if existing.is_some() {
            return Err("A recording is already in progress.".into());
        }
    }
    let id = uuid::Uuid::new_v4();
    let tmp_dir = base.join("record").join(id.to_string());
    std::fs::create_dir_all(&tmp_dir).map_err(err)?;

    let now = Instant::now();
    let mut sess = RecordSession {
        state: RecorderState::new(),
        started: now,
        tmp_dir: tmp_dir.clone(),
        project_id,
        mic: None,
        system: None,
        mic_chan: None,
        system_chan: None,
    };
    sess.state.start(0, mic, system);

    // Build each requested channel's writer + backend. If a backend fails to
    // start, tear down anything already started and clean the temp dir.
    let mut build = || -> CmdResult<()> {
        if mic {
            let path = tmp_dir.join("me.wav");
            let writer = record::create_wav(&path).map_err(err)?;
            let chan = Arc::new(Mutex::new(ChannelWriter::new(writer, path, app.clone(), Source::Mic, false)));
            let chan2 = chan.clone();
            let mut src = Box::new(record::cpal::MicSource::new(device_id.clone()));
            src.start(Box::new(move |data, rate, ch| chan2.lock().unwrap().feed(data, rate, ch)))
                .map_err(err)?;
            sess.mic = Some(src);
            sess.mic_chan = Some(chan);
        }
        if system {
            let path = tmp_dir.join("them.wav");
            let writer = record::create_wav(&path).map_err(err)?;
            let chan = Arc::new(Mutex::new(ChannelWriter::new(writer, path, app.clone(), Source::System, cfg!(windows))));
            let chan2 = chan.clone();
            let mut src = system_source()?;
            src.start(Box::new(move |data, rate, ch| chan2.lock().unwrap().feed(data, rate, ch)))
                .map_err(err)?;
            sess.system = Some(src);
            sess.system_chan = Some(chan);
        }
        Ok(())
    };
    if let Err(e) = build() {
        stop_backends(&mut sess);
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err(e);
    }
    if sess.mic.is_none() && sess.system.is_none() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err("No recording source could start on this computer.".into());
    }

    let dto = RecordStateDto { phase: sess.state.phase, elapsed_ms: 0, mic, system };
    *slot.lock().unwrap() = Some(sess);
    let _ = app.emit("record-state", dto);
    Ok(())
}

/// This OS's system-audio capture backend.
fn system_source() -> CmdResult<Box<dyn CaptureSource>> {
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(record::mac::SystemAudioSource::new()))
    }
    #[cfg(windows)]
    {
        Ok(Box::new(record::win::SystemAudioSource::new()))
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        Err("System audio is recorded on Windows and macOS only.".into())
    }
}

#[tauri::command]
fn record_pause(app: AppHandle, state: State<SharedState>) -> CmdResult<()> {
    with_session(&state, |sess| {
        let now = sess.started.elapsed().as_millis() as u64;
        sess.state.pause(now);
        set_paused(sess, true);
        Ok(state_dto(sess))
    })
    .map(|dto| {
        let _ = app.emit("record-state", dto);
    })
}

#[tauri::command]
fn record_resume(app: AppHandle, state: State<SharedState>) -> CmdResult<()> {
    with_session(&state, |sess| {
        let now = sess.started.elapsed().as_millis() as u64;
        sess.state.resume(now);
        set_paused(sess, false);
        Ok(state_dto(sess))
    })
    .map(|dto| {
        let _ = app.emit("record-state", dto);
    })
}

#[tauri::command]
fn record_cancel(app: AppHandle, state: State<SharedState>) -> CmdResult<()> {
    let slot = { state.lock().unwrap().record.clone() };
    let sess = slot.lock().unwrap().take();
    if let Some(mut sess) = sess {
        stop_backends(&mut sess);
        let _ = std::fs::remove_dir_all(&sess.tmp_dir);
    }
    let _ = app.emit(
        "record-state",
        RecordStateDto { phase: record::Phase::Idle, elapsed_ms: 0, mic: false, system: false },
    );
    Ok(())
}

/// Stop the take. `storage`: `transcript` (the default: the audio goes once
/// the transcript is written), `both` (the audio is kept too), or `audio`
/// (no transcript). Kept audio lives in app data (`recordings/`), never in a
/// repo. The transcript goes into the library's Raw/ and is read at once.
#[tauri::command]
fn record_stop(app: AppHandle, state: State<SharedState>, storage: String) -> CmdResult<()> {
    let slot = { state.lock().unwrap().record.clone() };
    let mut sess = slot.lock().unwrap().take().ok_or("No recording to stop.")?;
    let now = sess.started.elapsed().as_millis() as u64;
    sess.state.stop(now);
    let duration = Duration::from_millis(sess.state.elapsed_ms(now));
    stop_backends(&mut sess); // stops capture, flushes resampler tails, finalizes WAVs

    let _ = app.emit("record-transcribing", ());
    // Do the slow transcription + write off the command thread.
    let base = { state.lock().unwrap().base_dir.clone() };
    let take = Take {
        project_id: sess.project_id,
        tmp_dir: sess.tmp_dir.clone(),
        mic_wav: sess.mic_chan.as_ref().map(|c| c.lock().unwrap().path.clone()),
        system_wav: sess.system_chan.as_ref().map(|c| c.lock().unwrap().path.clone()),
        duration,
    };
    let inner = state.inner().clone();

    std::thread::spawn(move || {
        let outcome = finish_recording(&app, &inner, &base, &storage, &take);
        if let Err(e) = outcome {
            // Unexpected failure (e.g. couldn't write the transcript). The
            // temp WAVs stay in app data so nothing is lost.
            let _ = app.emit("record-error", RecordErrorDto { message: e, can_retry: false });
        }
        let _ = app.emit(
            "record-state",
            RecordStateDto { phase: record::Phase::Idle, elapsed_ms: 0, mic: false, system: false },
        );
    });
    Ok(())
}

fn with_session<T>(
    state: &State<SharedState>,
    f: impl FnOnce(&mut RecordSession) -> CmdResult<T>,
) -> CmdResult<T> {
    let slot = { state.lock().unwrap().record.clone() };
    let mut guard = slot.lock().unwrap();
    let sess = guard.as_mut().ok_or("No recording in progress.")?;
    f(sess)
}

fn state_dto(sess: &RecordSession) -> RecordStateDto {
    let now = sess.started.elapsed().as_millis() as u64;
    RecordStateDto {
        phase: sess.state.phase,
        elapsed_ms: sess.state.elapsed_ms(now),
        mic: sess.state.mic,
        system: sess.state.system,
    }
}

fn set_paused(sess: &mut RecordSession, paused: bool) {
    if let Some(c) = &sess.mic_chan {
        c.lock().unwrap().set_paused(paused);
    }
    if let Some(c) = &sess.system_chan {
        c.lock().unwrap().set_paused(paused);
    }
}

/// Stop both backends, then flush + finalize each channel's WAV. The channel
/// handles are kept on the session so callers can read the WAV paths afterward.
fn stop_backends(sess: &mut RecordSession) {
    if let Some(mut s) = sess.mic.take() {
        s.stop();
    }
    if let Some(mut s) = sess.system.take() {
        s.stop();
    }
    if let Some(c) = &sess.mic_chan {
        c.lock().unwrap().finalize_flush();
    }
    if let Some(c) = &sess.system_chan {
        c.lock().unwrap().finalize_flush();
    }
}

/// A stopped take, ready to finish.
struct Take {
    project_id: uuid::Uuid,
    tmp_dir: PathBuf,
    mic_wav: Option<PathBuf>,
    system_wav: Option<PathBuf>,
    duration: Duration,
}

/// Move a file, copying when it crosses drives.
fn move_across(from: &Path, to: &Path) -> std::io::Result<()> {
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(e) if is_cross_device(&e) => {
            std::fs::copy(from, to)?;
            let _ = std::fs::remove_file(from);
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Off-thread finish: transcribe the take, write the transcript into the
/// library's Raw/ (never an empty one) and start the read; keep the audio in
/// app data when asked, or when there is no transcript to show for it.
fn finish_recording(app: &AppHandle, state: &SharedState, base: &Path, storage: &str, take: &Take) -> CmdResult<()> {
    use chrono::{Datelike, Local, Timelike};
    let now = Local::now();
    let (y, mo, d, h, mi) = (now.year(), now.month(), now.day(), now.hour(), now.minute());
    let stem = record::recording_stem(y, mo, d, h, mi);
    let (mic_on, system_on) = (take.mic_wav.is_some(), take.system_wav.is_some());
    let single = mic_on ^ system_on;

    // Kept audio goes to app data, `recordings/`, beside nothing in a repo.
    let audio_dir = base.join("recordings");
    let keep_audio = || -> CmdResult<Vec<PathBuf>> {
        std::fs::create_dir_all(&audio_dir).map_err(err)?;
        let mut kept = Vec::new();
        for (src, suffix) in [(&take.mic_wav, "Me"), (&take.system_wav, "Them")] {
            let Some(src) = src.as_ref().filter(|p| p.is_file()) else { continue };
            let wav_stem = if single { stem.clone() } else { format!("{stem} - {suffix}") };
            let dest = audio_dir.join(record::unique_name(&audio_dir, &wav_stem, "wav"));
            move_across(src, &dest).map_err(err)?;
            kept.push(dest);
        }
        let _ = std::fs::remove_dir_all(&take.tmp_dir);
        Ok(kept)
    };
    let fail = |message: String| {
        let _ = app.emit("record-error", RecordErrorDto { message, can_retry: false });
        Ok(())
    };

    if storage == "audio" {
        keep_audio()?;
        let _ = app.emit(
            "record-saved",
            RecordSaved { rel_path: audio_dir.to_string_lossy().into_owned(), project_id: None },
        );
        return Ok(());
    }

    // The library and where in its Raw/ the transcript goes.
    let project = {
        let guard = state.lock().unwrap();
        guard.members.get(&take.project_id).map(|m| m.project.clone())
    };
    let Some(project) = project else {
        let kept = keep_audio()?;
        return fail(format!(
            "The knowledge base closed before the transcript was written. The audio is kept in {}.",
            kept.first().map(|p| p.display().to_string()).unwrap_or_else(|| audio_dir.display().to_string())
        ));
    };
    let raw_dir = project.root.join(ken_core::ingest::RAW);
    std::fs::create_dir_all(&raw_dir).map_err(err)?;
    let md_name = record::unique_name(&raw_dir, &stem, "md");
    let md_rel = format!("{}/{md_name}", ken_core::ingest::RAW);

    let Some(model) = transcription_model(base) else {
        let kept = keep_audio()?;
        return fail(format!(
            "Install a transcription model in Settings to make transcripts. The audio is kept in {}.",
            kept.first().map(|p| p.display().to_string()).unwrap_or_else(|| audio_dir.display().to_string())
        ));
    };
    let channel_count = mic_on as usize + system_on as usize;
    // One continuous bar across sequential channels: Me fills the first
    // half, Them the second (or the whole bar when only one exists).
    let scaled_sink = |idx: usize| -> transcript::ProgressFn {
        let app = app.clone();
        let rel = md_rel.clone();
        let pid = take.project_id;
        let last = std::sync::atomic::AtomicI32::new(-1);
        std::sync::Arc::new(move |phase| {
            if let transcript::TranscriptPhase::Transcribing(p) = phase {
                let overall = transcript::scale_channel_pct(idx, channel_count, p);
                if last.swap(overall as i32, Ordering::Relaxed) == overall as i32 {
                    return;
                }
                emit_transcript_phase(&app, Some(pid), &rel, transcript::TranscriptPhase::Transcribing(overall));
            }
        })
    };
    let transcribe_all = || -> CmdResult<String> {
        let mut me_cues = Vec::new();
        let mut them_cues = Vec::new();
        let mut idx = 0usize;
        if let Some(p) = &take.mic_wav {
            let samples = record::read_wav_f32(p).map_err(err)?;
            me_cues = transcript::transcribe_with_progress(&model, &samples, scaled_sink(idx)).map_err(err)?;
            idx += 1;
        }
        if let Some(p) = &take.system_wav {
            let samples = record::read_wav_f32(p).map_err(err)?;
            them_cues = transcript::transcribe_with_progress(&model, &samples, scaled_sink(idx)).map_err(err)?;
        }
        let channels: Vec<record::LabeledChannel> = if single {
            vec![record::LabeledChannel { label: None, cues: if mic_on { &me_cues } else { &them_cues } }]
        } else {
            vec![
                record::LabeledChannel { label: Some("Me"), cues: &me_cues },
                record::LabeledChannel { label: Some("Them"), cues: &them_cues },
            ]
        };
        Ok(record::merge_transcript(&channels))
    };
    let body = match transcribe_all() {
        Ok(body) => body,
        Err(e) => {
            let kept = keep_audio()?;
            return fail(format!(
                "Transcription failed: {e}. The audio is kept in {}.",
                kept.first().map(|p| p.display().to_string()).unwrap_or_else(|| audio_dir.display().to_string())
            ));
        }
    };
    if body.trim().is_empty() {
        // Never an empty transcript in Raw/.
        let kept = keep_audio()?;
        return fail(format!(
            "Nothing was heard in the recording, so no transcript was written. The audio is kept in {}.",
            kept.first().map(|p| p.display().to_string()).unwrap_or_else(|| audio_dir.display().to_string())
        ));
    }

    let kept: Vec<String> = if storage == "both" {
        keep_audio()?.into_iter().map(|p| p.display().to_string()).collect()
    } else {
        let _ = std::fs::remove_dir_all(&take.tmp_dir);
        Vec::new()
    };
    let me = cached_git_me();
    let when = format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}");
    let header = record::metadata_header(y, mo, d, h, mi, take.duration, mic_on, system_on);
    let doc = format!(
        "{}{}",
        record::recording_frontmatter(&when, me.name.as_deref().or(me.email.as_deref()), &kept),
        record::build_document(&header, &body)
    );
    std::fs::write(raw_dir.join(&md_name), &doc).map_err(err)?;

    // Index it, then read it.
    let (base_dir, others) = {
        let mut guard = state.lock().unwrap();
        if let Some(m) = guard.members.get_mut(&take.project_id) {
            let _ = scan::refresh_path(&m.project, &mut m.db, &md_rel);
        }
        (guard.base_dir.clone(), code_sources(&guard, &project.root))
    };
    emit_index_updated(app, take.project_id, ScanStats::default());
    start_ingest_pass(&project, &base_dir, others, true);
    let _ = app.emit("record-saved", RecordSaved { rel_path: md_rel, project_id: Some(take.project_id.to_string()) });
    Ok(())
}

/// Whether a rename failed because the two paths are on different drives:
/// `EXDEV` (18) on Unix, `ERROR_NOT_SAME_DEVICE` (17) on Windows. The caller
/// copies and removes instead.
fn is_cross_device(e: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        e.raw_os_error() == Some(18)
    }
    #[cfg(windows)]
    {
        e.raw_os_error() == Some(17)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = e;
        false
    }
}

/// What a person calls the OS's file manager, for "move it there instead".
fn file_manager() -> &'static str {
    if cfg!(target_os = "macos") {
        "Finder"
    } else if cfg!(windows) {
        "File Explorer"
    } else {
        "your file manager"
    }
}

/// Reconcile the index after a folder (or file) has moved on disk: rewrite every
/// row keyed by the old path prefix to the new one. A move changes no file's
/// bytes, size or mtime, so there is nothing to re-read — `Db::rename_prefix`
/// does the whole job in one SQL transaction (milliseconds, no file I/O),
/// preserving extracted content, OCR regions and queue state instead of
/// re-deriving them. Factored out of `move_file` so it's unit-testable without
/// Tauri.
fn reindex_moved(db: &mut Db, from_rel: &str, to_rel: &str) -> ken_core::Result<()> {
    db.rename_prefix(from_rel, to_rel)?;
    Ok(())
}

/// Move a file OR folder within the project. Both paths are validated to stay
/// inside the project root (`resolve` rejects `..`/absolute escapes); overwriting
/// an existing destination is refused.
///
/// A folder move (same-parent rename or a full move) renames the directory and
/// then reconciles the index with `reindex_moved` — a prefix rewrite of the
/// already-indexed rows. This used to be `remove_folder` + `scan::reindex`,
/// which is a full `db.clear()` + re-walk + re-extract of the ENTIRE project:
/// it froze the whole app (heavy work on the IPC thread while holding the global
/// state mutex) and discarded every extraction, OCR result and transcript in the
/// project just to rename a directory. The rewrite is pure SQL, so it is safe to
/// keep synchronous; `index-updated` still fires so the UI refreshes.
#[tauri::command(async)]
fn move_file(
    app: AppHandle,
    state: State<SharedState>,
    from_rel: String,
    to_rel: String,
) -> CmdResult<()> {
    let (from_abs, to_abs) = {
        let guard = state.lock().unwrap();
        let active = member(&guard, None)?;
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
        Err(e) if is_cross_device(&e) => {
            if from_is_dir {
                return Err(format!(
                    "That folder can't be moved across drives from here — move it in {} instead.",
                    file_manager()
                ));
            }
            std::fs::copy(&from_abs, &to_abs).map_err(err)?;
            std::fs::remove_file(&from_abs).map_err(err)?;
        }
        Err(e) => return Err(err(e)),
    }

    let mut guard = state.lock().unwrap();
    let active = member_mut(&mut guard, None)?;
    active.sync.wrote(vec![from_rel.clone(), to_rel.clone()]);
    if from_is_dir {
        // Pure index bookkeeping: every child row moves to the new prefix with
        // its content and queue state intact. No rescan, no re-extraction.
        reindex_moved(&mut active.db, &from_rel, &to_rel).map_err(err)?;
        let project_id = active.project.config.id;
        drop(guard);
        // Nothing was re-read, so there are no fresh stats and no new
        // transcription work — just tell the UI the index moved.
        emit_index_updated(&app, project_id, ScanStats::default());
    } else {
        scan::refresh_path(&active.project, &mut active.db, &from_rel).map_err(err)?;
        scan::refresh_path(&active.project, &mut active.db, &to_rel).map_err(err)?;
    }
    Ok(())
}

/// Reconcile the index after a file or folder has left the project on disk
/// (deleted or moved to the trash). A file's row is dropped through the standard
/// `refresh_path` (which sees the path is gone and removes it); a folder's whole
/// subtree is dropped with `remove_folder` — the same reconciliation the watcher
/// performs, but synchronous so the caller's tree refresh already reflects it.
/// Factored out of `delete_file` so it's unit-testable without the OS trash.
fn deindex_removed(project: &Project, db: &mut Db, rel: &str, is_dir: bool) -> ken_core::Result<()> {
    if is_dir {
        db.remove_folder(rel)?;
    } else {
        scan::refresh_path(project, db, rel)?;
    }
    Ok(())
}

/// Move one path to the OS trash. The single place the app talks to the OS
/// trash, so every caller gets the same delete method and the same phrasing.
///
/// On macOS the `trash` crate defaults to `DeleteMethod::Finder`, which drives
/// Finder over `osascript`. That has two problems here: it blocks on Finder
/// (seconds, and forever if Finder is busy or not running), and Finder REFUSES
/// to trash a folder holding undownloaded iCloud placeholders — it insists the
/// item "needs to be downloaded" first. `DeleteMethod::NsFileManager` calls
/// `-[NSFileManager trashItemAtURL:…]` instead: no Finder dependency, no
/// AppleScript round trip, and dataless items move as-is without being
/// hydrated first (which is the whole point — you shouldn't have to download a
/// 50 GB folder to throw it away). The trade-off is that Finder's "Put Back"
/// may not be offered for these items on some macOS versions; the files are
/// still in the Trash and can be dragged out.
///
/// Every other platform keeps the crate's default behaviour.
fn trash_path(abs: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        let mut ctx = trash::TrashContext::new();
        ctx.set_delete_method(DeleteMethod::NsFileManager);
        ctx.delete(abs).map_err(trash_err)
    }
    #[cfg(not(target_os = "macos"))]
    {
        trash::delete(abs).map_err(trash_err)
    }
}

/// Readable phrasing for a failed trash. The crate's `Error::Unknown` carries
/// the raw OS text (an `NSError` debug string on macOS), which is noise in a
/// toast, so name the action and keep the cause on the end for support.
fn trash_err(e: trash::Error) -> String {
    match e {
        trash::Error::CouldNotAccess { target } => {
            format!("Couldn't move \u{201c}{target}\u{201d} to the trash — it may have already been removed.")
        }
        other => format!("Couldn't move that to the trash. {other}"),
    }
}

/// Move a file OR folder to the OS trash (recoverable — it lands in Finder's
/// Trash / the Recycle Bin, not an unlink). The path is validated to stay inside
/// the project root (`resolve` rejects `..`/absolute escapes). After the trash
/// succeeds the index is reconciled via `deindex_removed` and the tree refreshes.
///
/// The trash call itself runs on a blocking thread with no lock held: even the
/// fast path touches the filesystem for every item in a folder tree, and on a
/// cloud-backed folder that is far from instant. Doing it inline froze the
/// whole UI (every other IPC command queues behind it) for as long as the OS
/// took. The state lock is taken twice — briefly, to read the path, and again
/// after the await to reconcile the index — because a `State` borrow can't be
/// held across an await point in a Tauri command.
#[tauri::command]
async fn delete_file(
    app: AppHandle,
    state: State<'_, SharedState>,
    rel_path: String,
) -> CmdResult<()> {
    let abs = resolve_path(&state, &rel_path)?;

    let is_dir = abs.is_dir();
    if !abs.is_file() && !is_dir {
        return Err("That file or folder no longer exists.".to_string());
    }

    // Recoverable delete: hand the path to the OS trash rather than unlinking it.
    tauri::async_runtime::spawn_blocking(move || trash_path(&abs)).await.map_err(err)??;

    let mut guard = state.lock().unwrap();
    let active = member_mut(&mut guard, None)?;
    active.sync.wrote(vec![rel_path.clone()]);
    deindex_removed(&active.project, &mut active.db, &rel_path, is_dir).map_err(err)?;
    let project_id = active.project.config.id;
    drop(guard);
    emit_index_updated(&app, project_id, ScanStats::default());
    Ok(())
}

/// Create a folder. Fails when something with that name already exists (the UI
/// validates sibling names first; this is the race-safety backstop). Folders
/// aren't index rows — the tree walks them off disk — so no refresh is needed;
/// the caller's tree refresh picks it up.
#[tauri::command(async)]
fn create_folder(state: State<SharedState>, rel_path: String) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let active = member(&guard, None)?;
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
#[tauri::command(async)]
fn create_document(state: State<SharedState>, rel_path: String) -> CmdResult<String> {
    let mut guard = state.lock().unwrap();
    let active = member_mut(&mut guard, None)?;
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
#[tauri::command(async)]
fn import_begin(state: State<SharedState>, src_path: String) -> CmdResult<ImportDto> {
    let root = {
        let guard = state.lock().unwrap();
        member(&guard, None)?.project.root.clone()
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
        member(&guard, None)?.project.root.clone()
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
#[tauri::command(async)]
fn import_commit(
    app: AppHandle,
    state: State<SharedState>,
    import_id: String,
    dest_folder_rel: String,
    create_folder: bool,
) -> CmdResult<String> {
    let (root, project, sync) = {
        let guard = state.lock().unwrap();
        let active = member(&guard, None)?;
        (active.project.root.clone(), active.project.clone(), active.sync.clone())
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
        Err(e) if is_cross_device(&e) => {
            std::fs::copy(&staged_abs, &final_abs).map_err(err)?;
            std::fs::remove_file(&staged_abs).map_err(err)?;
        }
        Err(e) => return Err(err(e)),
    }
    let _ = std::fs::remove_dir_all(ken_core::import::staging_dir(&root, &import_id));

    let changed = {
        let mut guard = state.lock().unwrap();
        let active = member_mut(&mut guard, None)?;
        scan::refresh_path(&active.project, &mut active.db, &final_rel).map_err(err)?
    };
    if changed {
        let paths = vec![final_rel.clone()];
        sync.wrote(paths.clone());
        emit_index_updated(&app, project.config.id, ScanStats { changed_paths: paths, ..Default::default() });
    }
    Ok(final_rel)
}

/// Discard a staged import (dialog cancelled): delete its staging dir.
#[tauri::command]
fn import_cancel(state: State<SharedState>, import_id: String) -> CmdResult<()> {
    let root = {
        let guard = state.lock().unwrap();
        member(&guard, None)?.project.root.clone()
    };
    let _ = std::fs::remove_dir_all(ken_core::import::staging_dir(&root, &import_id));
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct McpInfo {
    binary_path: Option<String>,
    workspace_root: String,
    add_command: String,
    json_config: String,
    llm_instruction: String,
}

/// The `--mcp-config` Ken's own chat runs with (knowledge-core decision 22):
/// the knowledge server (`ken`, locked to the chat's project for paths), and
/// beside it the personal set (`ken-personal`, `ken-mcp --personal`) with the
/// app's listener, so the chat keeps the person's tasks and memory and can
/// open a file when asked. Claude Code's own registration (`mcp_info`) gets
/// the knowledge server only.
fn chat_mcp_config(mcp: &Path, root: &Path, app_url: &str, app_token: &str) -> serde_json::Value {
    let mut servers = serde_json::Map::new();
    servers.insert(ken_core::chat::KEN_SERVER.into(), serde_json::json!({ "command": mcp, "args": ["--project", root] }));
    servers.insert(
        ken_core::chat::PERSONAL_SERVER.into(),
        serde_json::json!({
            "command": mcp,
            "args": ["--personal"],
            "env": { "KEN_APP_URL": app_url, "KEN_APP_TOKEN": app_token }
        }),
    );
    serde_json::json!({ "mcpServers": servers })
}

/// Where is the `ken-mcp` binary? Installer layout first (sibling of the
/// app executable; `%LOCALAPPDATA%\Programs\ken-mcp`, where `install.ps1`
/// puts it on Windows; `~/.local/bin` from `install.sh`), then PATH, then a
/// dev build.
fn find_ken_mcp() -> Option<PathBuf> {
    let name = format!("ken-mcp{}", std::env::consts::EXE_SUFFIX);
    if let Ok(exe) = std::env::current_exe() {
        if let Some(sibling) = exe.parent().map(|d| d.join(&name)) {
            if sibling.is_file() {
                return Some(sibling);
            }
        }
    }
    if cfg!(windows) {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            let installed = PathBuf::from(local).join("Programs").join("ken-mcp").join(&name);
            if installed.is_file() {
                return Some(installed);
            }
        }
    }
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        let local = PathBuf::from(home).join(".local/bin").join(&name);
        if local.is_file() {
            return Some(local);
        }
    }
    // PATH: `where` on Windows (there is no `which`), `which` elsewhere.
    let finder = if cfg!(windows) { "where" } else { "which" };
    let mut cmd = std::process::Command::new(finder);
    if let Ok(out) = ken_core::proc::quiet(&mut cmd).arg("ken-mcp").output() {
        if out.status.success() {
            let first = String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or("").trim().to_string();
            let path = PathBuf::from(first);
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

/// How an agent outside Ken connects to it (Settings › Agents): the
/// knowledge server for the whole open workspace, its knowledge base and
/// every source (`ken-mcp --workspace`), not the focused repo (Chris,
/// 2026-10-15). The server stays on this workspace whatever Ken opens later.
#[tauri::command(async)]
fn mcp_info(state: State<SharedState>) -> CmdResult<McpInfo> {
    let guard = state.lock().unwrap();
    let ws = guard.workspace.as_ref().ok_or("no workspace open")?;
    let root = ws.ws.root.to_string_lossy().into_owned();
    let name = ws.ws.config.name.clone();
    let binary_path = find_ken_mcp().map(|p| p.to_string_lossy().into_owned());
    // When the binary isn't found the strings still render with the bare
    // name so copied configs work once it lands on PATH.
    let command = binary_path.clone().unwrap_or_else(|| "ken-mcp".into());
    let add_command = format!(
        "claude mcp add ken -- {} --workspace {}",
        shell_word(&command),
        shell_word(&root)
    );
    let json_config = serde_json::to_string_pretty(&serde_json::json!({
        "mcpServers": {
            "ken": { "command": command, "args": ["--workspace", root] }
        }
    }))
    .map_err(err)?;
    // The knowledge server only (decision 22): the person's tasks and memory
    // are the personal set, which only Ken's own chat registers.
    let llm_instruction = format!(
        "Set up the Ken MCP server so you can search the workspace \"{name}\": its knowledge base \
and every source.\n\n\
Ken is a local knowledge app. The workspace's folder is {root}; Ken indexes its knowledge \
base and each of its sources, and the server covers them all. Its MCP server binary, \
ken-mcp, runs on demand over stdio and tells you how to use it in its instructions. Its \
tools: search (every source of the workspace), read (a hit, a file or a folder; a bare \
path is read in the knowledge base), related (what a page, ruling, ticket or file is \
tied to), the code index (code_definition, code_usages, code_outline, code_imports), \
history, tickets and repos (the workspace's sources). It never edits the person's \
files. Two tools only queue for a person: propose_decision files a ruling in Ken's \
Inbox for one click into the decisions log, and add_source puts a source in the \
knowledge base's inbox for the next ingest.\n\n\
If you are Claude Code, register it by running:\n\n  {add_command}\n\n\
Otherwise, add this to your MCP configuration:\n\n{json_config}\n\n\
Once connected, start with search to find what answers a question, and read to \
read it."
    );
    Ok(McpInfo {
        binary_path,
        workspace_root: root,
        add_command,
        json_config,
        llm_instruction,
    })
}

#[tauri::command]
fn file_mtime(state: State<SharedState>, rel_path: String) -> CmdResult<i64> {
    let guard = state.lock().unwrap();
    let active = member(&guard, None)?;
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
#[tauri::command(async)]
fn media_src(state: State<SharedState>, rel_path: String) -> CmdResult<String> {
    let abs = resolve_path(&state, &rel_path)?;
    if !abs.is_file() {
        return Err("That media file isn't available on disk.".into());
    }
    Ok(to_asset_url(&abs))
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct TranscriptProgress {
    rel_path: String,
    /// "extracting" | "transcribing"
    phase: String,
    /// 0–100; present only while transcribing.
    pct: Option<u8>,
}

/// `project_id` is `None` only for the recording-finish path, where the
/// active project is looked up once, best-effort, before transcription
/// starts (see `finish_recording`) — everywhere else it's always known.
fn emit_transcript_phase(
    app: &AppHandle,
    project_id: Option<uuid::Uuid>,
    rel: &str,
    phase: transcript::TranscriptPhase,
) {
    let (phase, pct) = match phase {
        transcript::TranscriptPhase::Extracting => ("extracting", None),
        transcript::TranscriptPhase::Transcribing(p) => ("transcribing", Some(p)),
    };
    let payload = TranscriptProgress { rel_path: rel.to_string(), phase: phase.into(), pct };
    match project_id {
        Some(id) => emit_member(app, id, "transcript-progress", payload),
        None => {
            let _ = app.emit("transcript-progress", payload);
        }
    }
}

/// A `ProgressFn` that forwards to `transcript-progress`, dropping repeated
/// percents (Whisper's callback re-fires the same value between segments).
fn transcript_progress_sink(
    app: AppHandle,
    project_id: uuid::Uuid,
    rel: String,
) -> transcript::ProgressFn {
    let last = std::sync::atomic::AtomicI32::new(-1);
    std::sync::Arc::new(move |phase| {
        if let transcript::TranscriptPhase::Transcribing(p) = phase {
            if last.swap(p as i32, Ordering::Relaxed) == p as i32 {
                return;
            }
        }
        emit_transcript_phase(&app, Some(project_id), &rel, phase);
    })
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
#[tauri::command(async)]
fn video_transcript(state: State<SharedState>, rel_path: String) -> CmdResult<TranscriptDto> {
    let (abs, root, generating) = {
        let guard = state.lock().unwrap();
        let active = member(&guard, None)?;
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
#[tauri::command(async)]
fn generate_transcript(app: AppHandle, state: State<SharedState>, rel_path: String) -> CmdResult<()> {
    let (root, base, project_id, jobs) = {
        let guard = state.lock().unwrap();
        let active = member(&guard, None)?;
        active.project.resolve(&rel_path).map_err(err)?; // reject path escape
        (
            active.project.root.clone(),
            guard.base_dir.clone(),
            active.project.config.id,
            active.transcripts.clone(),
        )
    };
    let ffmpeg = transcript::discover_ffmpeg();
    let model = transcription_model(&base);
    if let Some(blocker) = transcript::transcription_blocker(Path::new(&rel_path), ffmpeg.is_some(), model.is_some()) {
        return Err(blocker);
    }
    let job = TranscriptionJob {
        state: state.inner().clone(),
        root,
        project_id,
        jobs,
        ffmpeg,
        model: model.unwrap_or_default(),
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
    /// Only for what Ken cannot decode itself (`transcript::decodes_in_process`).
    ffmpeg: Option<PathBuf>,
    model: PathBuf,
    rel_path: String,
    /// Automatic (ingest) jobs stay silent on failure; manual ones don't.
    quiet: bool,
}

/// The ingest-side enqueue: transcribe newly-seen videos automatically, but go
/// completely quiet when ffmpeg or the model is missing — exactly like the
/// knowledge auto-build going silent without the Claude CLI. Nobody asked for
/// this work, so a missing prerequisite is a no-op, never an error.
/// `project_id` is the project the videos were found in. It used to be
/// resolved as `members.values().next()` — an arbitrary open member — so in a
/// workspace a video indexed under one project could be queued against a
/// different project's root, DB and opt-in setting. Every caller already knew
/// the right id; now it passes it.
fn enqueue_transcriptions(
    app: &AppHandle,
    state: &SharedState,
    project_id: uuid::Uuid,
    rels: &[String],
) {
    if rels.is_empty() {
        return;
    }
    let (root, base, jobs) = {
        let guard = state.lock().unwrap();
        let Some(active) = guard.members.get(&project_id) else {
            return;
        };
        // Auto-transcription during indexing is opt-in (off by default). The
        // manual `generate_transcript` command bypasses this and always runs.
        // Read it from THIS project: it is a per-project setting, and the old
        // arbitrary-member lookup could consult a different project's answer.
        if !transcript::transcribe_on_index_enabled(&active.project) {
            return;
        }
        (
            active.project.root.clone(),
            guard.base_dir.clone(),
            active.transcripts.clone(),
        )
    };
    let ffmpeg = transcript::discover_ffmpeg();
    let Some(model) = transcription_model(&base) else {
        return; // no model → quiet no-op
    };
    for rel in rels {
        // What only ffmpeg can read waits quietly when it is missing.
        if transcript::transcription_blocker(Path::new(rel), ffmpeg.is_some(), true).is_some() {
            continue;
        }
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
        let on_progress =
            transcript_progress_sink(app.clone(), job.project_id, job.rel_path.clone());
        let result = transcript::generate_and_cache_with_progress(
            job.ffmpeg.as_deref(), &job.model, &job.root, &job.rel_path, on_progress,
        );
        match result {
            Ok(_) => {
                // Re-index the video so the fresh transcript is searchable.
                // The job carries the project it belongs to, so ask for it.
                // Taking an arbitrary member and then checking the id matched
                // meant that in a workspace the re-index silently never ran,
                // and a finished transcript stayed unsearchable. The re-index
                // runs on its own connection, with the lock released: it
                // extracts and hashes the file.
                let target = {
                    let guard = lock_tolerant(&job.state);
                    guard.members.get(&job.project_id).map(|a| (a.project.clone(), guard.base_dir.clone()))
                };
                if let Some((project, base)) = target {
                    if let Ok(mut db) = Db::open(&base, job.project_id) {
                        let _ = scan::refresh_path(&project, &mut db, &job.rel_path);
                    }
                }
                emit_index_updated(&app, job.project_id, ScanStats::default());
            }
            Err(e) => {
                job.jobs.lock().unwrap().attempted.insert(job.rel_path.clone());
                if !job.quiet {
                    // Scalar string payload — nothing to flatten `project_id`
                    // into (S9 step 5 skip list), so this stays plain.
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
    /// "transcription" | "embedding"
    category: String,
    /// "light" | "recommended" | "advanced" | "best"
    tier: String,
    blurb: String,
    /// The languages it handles, as a person says them.
    languages: String,
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
            model::ModelCategory::Embedding => "embedding".into(),
        },
        tier: entry.tier.as_str().into(),
        blurb: entry.blurb.to_string(),
        languages: entry.languages.to_string(),
        selected: entry.spec.id == selected_id,
    }
}

/// Status of the recommended model only — cheap and offline (just a file
/// check), so the transcript feature can gate on it without a network round
/// trip.
#[tauri::command(async)]
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
#[tauri::command(async)]
fn list_models(state: State<SharedState>) -> CmdResult<Vec<ModelStatusDto>> {
    let base = { state.lock().unwrap().base_dir.clone() };
    let sel_trans = model::selected(&base, model::ModelCategory::Transcription).id;
    let sel_embed = model::selected(&base, model::ModelCategory::Embedding).id;
    // Settings offers transcription and search by meaning only; the unused
    // language models are listed only when a file is still on disk, so it can
    // be removed.
    let unused = model::language_catalog().into_iter().filter(|e| model::installed_size(&base, &e.spec).is_some());
    Ok(model::catalog()
        .into_iter()
        .chain(unused)
        .map(|e| {
            let selected_id = match e.category {
                model::ModelCategory::Transcription => sel_trans.clone(),
                model::ModelCategory::Embedding => sel_embed.clone(),
                model::ModelCategory::Language => String::new(),
            };
            status_dto(&base, &e, &selected_id)
        })
        .collect())
}

/// Start downloading a model. Returns immediately; progress and completion flow
/// through the `model-download-progress` event (completion = a final 100%
/// sample), failures through `model-download-error`. A second concurrent
/// download of the same id is refused. The download itself streams to a temp
/// file, verifies, and atomically installs — all off the global lock, on its
/// own thread (like `spawn_transcription`).
#[tauri::command(async)]
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
                // A transcription model: recordings waiting in an inbox can
                // be read now.
                if model::category_specs(model::ModelCategory::Transcription).iter().any(|s| s.id == spec.id) {
                    use tauri::Manager;
                    start_ingest_passes(app.state::<SharedState>().inner());
                }
                // A newly installed Embedding model: if the active project has
                // semanticIndex on, the earlier toggle/open bailed `unavailable`
                // because the model file was missing — resume now, same as
                // `activate()` does on open. The LLM service doesn't use this
                // model, so no rearm.
                let is_embedding = model::category_specs(model::ModelCategory::Embedding)
                    .iter()
                    .any(|s| s.id == spec.id);
                if is_embedding {
                    use tauri::Manager;
                    let state = app.state::<SharedState>();
                    reapply_semantic_index_everywhere(&app, state.inner());
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
#[tauri::command(async)]
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

/// Persist the user's chosen model for a category
/// ("transcription" | "language" | "embedding").
#[tauri::command(async)]
fn set_model_selection(app: AppHandle, state: State<SharedState>, category: String, id: String) -> CmdResult<()> {
    let base = { state.lock().unwrap().base_dir.clone() };
    let cat = match category.as_str() {
        "transcription" => model::ModelCategory::Transcription,
        "embedding" => model::ModelCategory::Embedding,
        other => return Err(format!("unknown model category: {other}")),
    };
    model::set_selected(&base, cat, &id).map_err(err)?;
    if cat == model::ModelCategory::Embedding {
        // Another embedding model: load it, and re-read every repo for
        // meaning with it (the build drops the old vectors when the model
        // differs). Keyword search keeps working meanwhile.
        let app = app.clone();
        let state = state.inner().clone();
        std::thread::spawn(move || reapply_semantic_index_everywhere(&app, &state));
    }
    Ok(())
}

/// The search-by-meaning index now (Settings › AI).
#[tauri::command(async)]
fn embedding_state(state: State<SharedState>) -> CmdResult<EmbeddingStateDto> {
    let dbs: Vec<Arc<Mutex<Db>>> = {
        let guard = state.lock().unwrap();
        guard.members.values().map(|m| m.search_db.clone()).collect()
    };
    let (mut embedded, mut total) = (0usize, 0usize);
    for db in dbs {
        let db = db.lock().unwrap_or_else(|p| p.into_inner());
        embedded += db.vector_count().unwrap_or(0).max(0) as usize;
        total += db.chunk_count().unwrap_or(0).max(0) as usize;
    }
    let rebuilding = !semantic_builds().lock().unwrap_or_else(|p| p.into_inner()).is_empty();
    let (model, dims) = {
        let slot = shared_embedder();
        let guard = slot.lock().unwrap_or_else(|p| p.into_inner());
        match guard.as_ref() {
            Some(e) => (Some(e.model_id()), Some(e.dim())),
            None => (ken_core::embedder::selected_profile().map(|p| p.model_id.to_string()), None),
        }
    };
    Ok(EmbeddingStateDto { model, dims, embedded, total, rebuilding, device: device_label() })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GpuInfoDto {
    device: Option<String>,
    /// "vulkan" | "metal" | "cpu"
    backend: String,
    use_gpu: bool,
}

/// The device the on-device models run on, and the graphics card setting.
#[tauri::command(async)]
fn gpu_info(state: State<SharedState>) -> CmdResult<GpuInfoDto> {
    let use_gpu = use_gpu_setting(&state.lock().unwrap().app_settings);
    let d = ken_core::compute::device();
    let backend = match d.backend.as_str() {
        b if b.contains("metal") => "metal",
        b if b.contains("vulkan") => "vulkan",
        _ => "cpu",
    };
    Ok(GpuInfoDto { device: d.name, backend: backend.into(), use_gpu })
}

/// Turn the graphics card on or off for the on-device models. The models
/// are loaded again where they now run.
#[tauri::command(async)]
fn set_use_gpu(app: AppHandle, state: State<SharedState>, on: bool) -> CmdResult<()> {
    {
        let mut guard = state.lock().unwrap();
        guard.app_settings.extra.insert("useGpu".into(), serde_json::Value::Bool(on));
        guard.app_settings.save(&guard.base_dir).map_err(err)?;
    }
    ken_core::compute::set_use_gpu(on);
    // The loaded model sits where it was loaded; drop it so the next build
    // and search load it on the new device.
    shared_embedder().lock().unwrap_or_else(|p| p.into_inner()).take();
    let app = app.clone();
    let state = state.inner().clone();
    std::thread::spawn(move || reapply_semantic_index_everywhere(&app, &state));
    Ok(())
}

/// The `useGpu` setting, on unless the person turned it off.
fn use_gpu_setting(app_settings: &ken_core::settings::AppSettings) -> bool {
    app_settings.extra.get("useGpu").and_then(serde_json::Value::as_bool).unwrap_or(true)
}

// ---------- the Review store ----------

#[tauri::command(async)]
fn resolve_review_item(state: State<SharedState>, id: i64, project_id: Option<String>) -> CmdResult<()> {
    let pid = project_id.as_deref().map(|p| p.parse::<uuid::Uuid>()).transpose().map_err(err)?;
    let mut guard = state.lock().unwrap();
    let active = member_mut(&mut guard, pid)?;
    settle_review_item(&active.project.root, &mut active.db, id)
}

/// Mark a stored item done (a discarded proposal, a finding marked done or
/// dismissed, an applied proposal), then close the ingest card it came from
/// when nothing else waits from that note (`ingest::close_if_settled`).
fn settle_review_item(root: &Path, db: &mut Db, id: i64) -> CmdResult<()> {
    let note = db.list_open_review_items().map_err(err)?.iter().find(|it| it.id == id).and_then(ken_core::ingest::note_of);
    let now = engine::now_epoch();
    db.resolve_review_item(id, now).map_err(err)?;
    if let Some(note) = note {
        if let Err(e) = ken_core::ingest::close_if_settled(root, db, &note, now) {
            eprintln!("warning: could not close the card of {note}: {e}");
        }
    }
    Ok(())
}

/// Confirm a supersession finding (knowledge-core decision 25): Ken writes
/// `supersedes:` on the later ruling and `[SUPERSEDED BY …]` on the earlier
/// into the decisions log, as one commit in the knowledge base, and marks the
/// finding done. Returns the commit, when one was made.
#[tauri::command(async)]
fn confirm_finding(state: State<SharedState>, id: i64, project_id: Option<String>) -> CmdResult<Option<String>> {
    let pid = project_id.as_deref().map(|p| p.parse::<uuid::Uuid>()).transpose().map_err(err)?;
    let mut guard = state.lock().unwrap();
    let active = member_mut(&mut guard, pid)?;
    let (item, finding) = ken_core::findings::open(&active.db)
        .map_err(err)?
        .into_iter()
        .find(|(it, _)| it.id == id)
        .ok_or("no open finding with that id")?;
    let commit = ken_core::supersede::confirm(&active.project.root, &finding)?;
    settle_review_item(&active.project.root, &mut active.db, item.id)?;
    Ok(commit)
}

/// Raise an open question an ingest left unsettled as an issue in the RAID
/// register (Chris, 2026-10-14): the entry takes the next `I-nnn` and is one
/// commit in the knowledge base, and the finding is marked done, since the
/// register tracks it now. Returns the issue's id.
#[tauri::command(async)]
fn raise_issue(state: State<SharedState>, id: i64, project_id: Option<String>) -> CmdResult<String> {
    let pid = project_id.as_deref().map(|p| p.parse::<uuid::Uuid>()).transpose().map_err(err)?;
    let mut guard = state.lock().unwrap();
    let active = member_mut(&mut guard, pid)?;
    let (item, finding) = ken_core::findings::open(&active.db)
        .map_err(err)?
        .into_iter()
        .find(|(it, _)| it.id == id)
        .ok_or("no open finding with that id")?;
    let (issue, _) = ken_core::raid::raise_question(&active.project.root, &finding, &local_date_today())?;
    settle_review_item(&active.project.root, &mut active.db, item.id)?;
    Ok(issue)
}

// ---------- ignored files (user-level, never synced) ----------

/// Load the current project's private user-state from app-data. Helper for the
/// ignore commands so each one reads/writes the same non-synced file.
fn load_user_state(guard: &AppState) -> CmdResult<(std::path::PathBuf, uuid::Uuid, UserState)> {
    let active = member(guard, None)?;
    let base = guard.base_dir.clone();
    let id = active.project.config.id;
    let us = UserState::load(&base, id);
    Ok((base, id, us))
}

/// Held from loading the user's state file to saving it, so two commands
/// changing it at once do not lose one change. Taken after the global lock
/// when both are held, never before it.
fn user_state_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    lock_tolerant(LOCK.get_or_init(|| Mutex::new(())))
}

/// The focused member's app data dir, id and read connection, for the unread
/// commands: they read the index and the user's state off the global lock,
/// because the tree and the unread set are read again on every index update.
fn unread_snapshot(state: &SharedState) -> CmdResult<(std::path::PathBuf, uuid::Uuid, Arc<Mutex<Db>>)> {
    let guard = lock_tolerant(state);
    let active = member(&guard, None)?;
    Ok((guard.base_dir.clone(), active.project.config.id, active.search_db.clone()))
}

/// Silence a file's review issues for THIS user only (stored in app-data, never
/// written to the synced `.ken/` config). The file stays indexed and findable.
#[tauri::command(async)]
fn ignore_file(state: State<SharedState>, rel_path: String) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let _us = user_state_lock();
    let (base, id, mut us) = load_user_state(&guard)?;
    if us.ignore(rel_path) {
        us.save(&base, id).map_err(err)?;
    }
    Ok(())
}

/// Reverse an ignore, so the file's issues can surface again.
#[tauri::command(async)]
fn unignore_file(state: State<SharedState>, rel_path: String) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let _us = user_state_lock();
    let (base, id, mut us) = load_user_state(&guard)?;
    if us.unignore(&rel_path) {
        us.save(&base, id).map_err(err)?;
    }
    Ok(())
}

/// The current project's ignored files, for the Settings undo list and the
/// home "Needs a look" filter.
#[tauri::command(async)]
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
#[tauri::command(async)]
fn unread_files(state: State<SharedState>) -> CmdResult<Vec<String>> {
    let (base, id, search_db) = unread_snapshot(state.inner())?;
    let files = lock_tolerant(&search_db).list_files().map_err(err)?;
    let _us = user_state_lock();
    let mut us = UserState::load(&base, id);
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
#[tauri::command(async)]
fn mark_seen(state: State<SharedState>, rel_path: String) -> CmdResult<()> {
    let (base, id, search_db) = unread_snapshot(state.inner())?;
    let Some(row) = lock_tolerant(&search_db).get_file(&rel_path).map_err(err)? else {
        return Ok(()); // not indexed (yet) — nothing to mark
    };
    let _us = user_state_lock();
    let mut us = UserState::load(&base, id);
    if us.mark_seen(rel_path, (row.size, row.mtime)) {
        us.save(&base, id).map_err(err)?;
    }
    Ok(())
}

/// Mark every indexed file under one folder seen ("Mark folder as viewed").
/// `rel_path` is the folder; files directly at that path or beneath it count.
#[tauri::command(async)]
fn mark_seen_under(state: State<SharedState>, rel_path: String) -> CmdResult<()> {
    let (base, id, search_db) = unread_snapshot(state.inner())?;
    let files = lock_tolerant(&search_db).list_files().map_err(err)?;
    let _us = user_state_lock();
    let mut us = UserState::load(&base, id);
    if us.mark_seen_under(&rel_path, &index_versions(&files)) {
        us.save(&base, id).map_err(err)?;
    }
    Ok(())
}

/// Mark every currently-unread file seen ("Mark all as viewed").
#[tauri::command(async)]
fn mark_all_seen(state: State<SharedState>) -> CmdResult<()> {
    let (base, id, search_db) = unread_snapshot(state.inner())?;
    let files = lock_tolerant(&search_db).list_files().map_err(err)?;
    let _us = user_state_lock();
    let mut us = UserState::load(&base, id);
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
    /// When it last pulled and last pushed, Unix seconds (`sync::last_pull_push`).
    last_pull: Option<i64>,
    last_push: Option<i64>,
    /// The knowledge base's changes not written through Ken
    /// (`sync::other_changes`): a cycle never commits them, and "commit my
    /// other changes" commits them only when a person asks. Empty in a code
    /// or reference repo, where Ken never commits.
    others: Vec<String>,
}

/// The sync card's state for `project`; `pending` is what Ken wrote there
/// that its next cycle commits.
fn sync_status_of(project: &Project, pending: &[String]) -> SyncStatus {
    let is_git = sync::is_git_repo(&project.root);
    let (remote, branch) = if is_git {
        sync::remote_and_branch(&project.root)
    } else {
        (None, None)
    };
    let (last_pull, last_push) = if remote.is_some() {
        sync::last_pull_push(&project.root)
    } else {
        (None, None)
    };
    let auto = sync::sync_auto(project, &ken_core::registry::kind_of(&project.root));
    let others = if is_git && ken_core::registry::is_library(&project.root) {
        sync::other_changes(&project.root, pending)
    } else {
        Vec::new()
    };
    SyncStatus {
        mode: if is_git { "git" } else { "drive" }.into(),
        auto,
        active: is_git && auto && remote.is_some(),
        remote,
        branch,
        last_pull,
        last_push,
        others,
    }
}

/// `project_id` names the member (Workspace shows the knowledge base's
/// beside its row); none is the focused one.
#[tauri::command(async)]
fn sync_status(state: State<SharedState>, project_id: Option<String>) -> CmdResult<SyncStatus> {
    let id = project_id.as_deref().map(|p| p.parse::<uuid::Uuid>()).transpose().map_err(err)?;
    // git runs a few times: with the lock released.
    let (project, pending) = {
        let guard = lock_tolerant(&state);
        let active = member(&guard, id)?;
        (active.project.clone(), active.sync.pending())
    };
    Ok(sync_status_of(&project, &pending))
}

/// "Commit my other changes" (Chris, 2026-10-14): commit exactly the files
/// a person was shown as changed outside Ken in the knowledge base
/// (`sync::commit_others`), with a message naming each, then sync so the
/// commit is pushed with the rest. Offered with or without a remote; with
/// none, the commit stays on this computer. Refused in a code or reference
/// repo (decision 36). Returns the commit, when one was made.
#[tauri::command(async)]
fn sync_commit_others(
    state: State<SharedState>,
    paths: Vec<String>,
    project_id: Option<String>,
) -> CmdResult<Option<String>> {
    let id = project_id.as_deref().map(|p| p.parse::<uuid::Uuid>()).transpose().map_err(err)?;
    let root = {
        let guard = lock_tolerant(&state);
        member(&guard, id)?.project.root.clone()
    };
    let commit = sync::commit_others(&root, &paths).map_err(err)?;
    if commit.is_some() {
        let guard = lock_tolerant(&state);
        if let Ok(active) = member(&guard, id) {
            if active.project.root == root {
                active.sync.sync_now();
            }
        }
    }
    Ok(commit)
}

#[tauri::command]
fn set_sync_auto(app: AppHandle, state: State<SharedState>, auto: bool) -> CmdResult<SyncStatus> {
    let mut guard = state.lock().unwrap();
    let active = member_mut(&mut guard, None)?;
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

    let status = sync_status_of(&active.project, &active.sync.pending());
    // Reflect the toggle in the dot immediately; a fresh pull confirms.
    emit_member(&app, active.project.config.id, "sync-state", SyncStateEvent {
        state: if status.active { "synced" } else { "off" }.into(),
        detail: None,
    });
    if status.active {
        active.sync.pull_now();
    }
    Ok(status)
}

#[tauri::command(async)]
fn sync_now(state: State<SharedState>) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let active = member(&guard, None)?;
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
#[tauri::command(async)]
fn resolve_conflict(
    app: AppHandle,
    state: State<SharedState>,
    item_id: i64,
    resolution: String,
    content: Option<String>,
    project_id: Option<String>,
) -> CmdResult<String> {
    let pid = project_id.as_deref().map(|p| p.parse::<uuid::Uuid>()).transpose().map_err(err)?;
    let mut guard = state.lock().unwrap();
    let active = member_mut(&mut guard, pid)?;
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
    active.sync.wrote(vec![path.clone()]);
    let _ = app.emit("file-saved", &path);
    let _ = app.emit("review-changed", ());
    Ok(path)
}

/// Resolve a shared-drive conflicted-copy item: `keep-copy` promotes the
/// copy's content to the original name, `keep-original` deletes the copy.
/// Returns the path of the surviving file.
#[tauri::command(async)]
fn resolve_conflict_copy(
    app: AppHandle,
    state: State<SharedState>,
    item_id: i64,
    resolution: String,
    project_id: Option<String>,
) -> CmdResult<String> {
    let pid = project_id.as_deref().map(|p| p.parse::<uuid::Uuid>()).transpose().map_err(err)?;
    let mut guard = state.lock().unwrap();
    let active = member_mut(&mut guard, pid)?;
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
    active.sync.wrote(vec![survivor.clone(), copy_rel]);
    let _ = app.emit("review-changed", ());
    Ok(survivor)
}

/// A sync conflict or a conflicted copy, for the banner at the top of Files.
/// Shaped like the Review item the conflict view reads (`id` is
/// `item-<n>`), plus the repo it is in: resolve it with `resolve_conflict` /
/// `resolve_conflict_copy` and this `project_id`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConflictItemDto {
    id: String,
    item_id: i64,
    /// `conflict` | `conflict-copy`
    kind: String,
    title: String,
    body: String,
    when: i64,
    source_ref: String,
    /// The versions (conflict) or the copy's paths (conflict-copy), as JSON.
    payload: Option<String>,
    project_id: String,
    repo: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FilesBannerDto {
    /// Every open conflict and conflicted copy in the workspace's open
    /// repos, newest first.
    conflicts: Vec<ConflictItemDto>,
    /// How many of them are conflicted copies (a shared drive's).
    conflicted_copies: usize,
}

/// The banner at the top of Files: the sync conflicts and conflicted copies
/// waiting in the workspace's open repos, less those on files I ignore.
#[tauri::command(async)]
fn files_banner(state: State<SharedState>) -> CmdResult<FilesBannerDto> {
    // Cloned under the lock, read after it is released: this is read again
    // on every index update.
    let (base_dir, repos) = {
        let guard = lock_tolerant(&state);
        let ids: Vec<(uuid::Uuid, String)> = match guard.workspace.as_ref() {
            Some(ws) => ws_repos(&ws.ws).into_iter().map(|r| (r.id, r.name)).collect(),
            None => guard.members.values().map(|m| (m.project.config.id, m.project.config.name.clone())).collect(),
        };
        let repos: Vec<(uuid::Uuid, String, Arc<Mutex<Db>>)> = ids
            .into_iter()
            .filter_map(|(id, name)| guard.members.get(&id).map(|m| (id, name, m.search_db.clone())))
            .collect();
        (guard.base_dir.clone(), repos)
    };
    let mut conflicts = Vec::new();
    for (id, name, search_db) in repos {
        let ignored = UserState::load(&base_dir, id).ignored;
        let items = lock_tolerant(&search_db).list_open_review_items().map_err(err)?;
        for it in items {
            if it.kind != "conflict" && it.kind != "conflict-copy" {
                continue;
            }
            if user_state::inbox_item_ignored(&it.kind, &it.source_ref, &ignored) {
                continue;
            }
            conflicts.push(ConflictItemDto {
                // Row ids restart in each repo's index: the repo keeps two
                // repos' conflicts apart in the list.
                id: format!("item-{}@{id}", it.id),
                item_id: it.id,
                kind: it.kind,
                title: it.title,
                body: it.body,
                when: it.created_at,
                source_ref: it.source_ref,
                payload: it.payload,
                project_id: id.to_string(),
                repo: name.clone(),
            });
        }
    }
    conflicts.sort_by(|a, b| b.when.cmp(&a.when));
    let conflicted_copies = conflicts.iter().filter(|c| c.kind == "conflict-copy").count();
    Ok(FilesBannerDto { conflicts, conflicted_copies })
}

/// Is background download-and-index of cloud-offline documents enabled?
#[tauri::command]
fn get_background_index(state: State<SharedState>) -> CmdResult<bool> {
    let guard = state.lock().unwrap();
    let active = member(&guard, None)?;
    Ok(ken_core::bg_hydrate::background_index_enabled(&active.project))
}

/// Turn background cloud indexing on or off. Persisted in `project.json`; the
/// always-running worker reads this each tick, so it idles when off and picks
/// the backlog straight back up when on — no thread to restart.
#[tauri::command]
fn set_background_index(state: State<SharedState>, enabled: bool) -> CmdResult<()> {
    let mut guard = state.lock().unwrap();
    let active = member_mut(&mut guard, None)?;
    active
        .project
        .config
        .extra
        .insert("backgroundIndex".into(), serde_json::Value::Bool(enabled));
    active.project.save().map_err(err)
}

/// Is automatic on-device transcription of videos during indexing enabled?
#[tauri::command]
fn get_transcribe_on_index(state: State<SharedState>) -> CmdResult<bool> {
    let guard = state.lock().unwrap();
    let active = member(&guard, None)?;
    Ok(transcript::transcribe_on_index_enabled(&active.project))
}

/// Turn automatic video transcription during indexing on or off. Persisted in
/// `project.json`; off by default. Every automatic scan reads this before
/// enqueueing Whisper jobs, so the manual `generate_transcript` command is
/// unaffected either way.
#[tauri::command]
fn set_transcribe_on_index(state: State<SharedState>, enabled: bool) -> CmdResult<()> {
    let mut guard = state.lock().unwrap();
    let active = member_mut(&mut guard, None)?;
    active
        .project
        .config
        .extra
        .insert("transcribeVideosOnIndex".into(), serde_json::Value::Bool(enabled));
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

/// Where Claude Code is and its version. `help` is the per-OS install help
/// when it is not found, plus a line for each half-updated npm install that
/// was skipped; it can be non-empty when Claude Code is found.
#[tauri::command(async)]
fn claude_doctor() -> CmdResult<ClaudeDoctor> {
    match ken_core::runner::discover_claude() {
        Some(path) => {
            let mut cmd = std::process::Command::new(&path);
            let version = ken_core::proc::quiet(&mut cmd)
                .arg("--version")
                .stdin(std::process::Stdio::null())
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
            Ok(ClaudeDoctor {
                found: true,
                path: Some(path.to_string_lossy().into_owned()),
                version,
                help: ken_core::runner::claude_help(true),
            })
        }
        None => Ok(ClaudeDoctor {
            found: false,
            path: None,
            version: None,
            help: ken_core::runner::claude_help(false),
        }),
    }
}


// ---------- daily digest + quick answer ----------

fn local_date_today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

fn local_hour() -> u32 {
    use chrono::Timelike;
    chrono::Local::now().hour()
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

/// One hit a quick answer is grounded on: how the answer cites it
/// (`source:path`), where it opens (`ken://<project-id>/<path>`), its label
/// (`kind · tier or role · date`) and a paragraph of its text.
#[derive(Clone, Debug)]
struct QaSource {
    cite: String,
    address: String,
    label: Option<String>,
    text: String,
}

/// The material lines of a quick answer: `- cite (label): text`.
fn qa_material(sources: &[QaSource]) -> String {
    let mut material = String::new();
    for s in sources {
        match &s.label {
            Some(label) => material.push_str(&format!("- {} ({label}): {}\n", s.cite, s.text)),
            None => material.push_str(&format!("- {}: {}\n", s.cite, s.text)),
        }
    }
    material
}

/// How a quick answer weighs what it was given: the chat guide's rule
/// (`chat.rs`, rule 4), so the quick answer and the chat agree. A binding
/// ruling wins; where the code differs from one, both are named.
const QA_WEIGHING: &str = "Each item carries its label (kind · tier or role · date). Weigh them by it: a binding \
ruling or rule over a current page, a current page over a record (a ticket, note or draft), a later ruling over an \
earlier one. A binding ruling is settled, so state what it says as how things are decided. Where the code or a page \
differs from a ruling, say so and cite both.";

/// How a quick answer ends: a `SOURCES:` line `digest::parse_digest` reads,
/// each source cited as the material names it.
const QA_SOURCES: &str = "End with a final line `SOURCES: a, b` listing the items you used, each written exactly as the \
material names it before its label (such as `Game:src/world.py`); omit the line if none.";

/// Build the local-model quick-answer prompt: the same grounding as the Claude
/// path, wrapped in Qwen3 ChatML.
fn local_quick_answer_prompt(query: &str, sources: &[QaSource]) -> String {
    let system = format!(
        "You answer questions using only the provided project material. Answer in one or two sentences. {QA_WEIGHING} \
If the material doesn't answer it, say you don't know. {QA_SOURCES}"
    );
    let user = format!("Question: {query}\n\nMaterial:\n\n{}", qa_material(sources));
    ken_core::local_llm::chatml_prompt(&system, &user)
}

fn quick_answer_prompt(query: &str, sources: &[QaSource]) -> String {
    format!(
        "Question: {query}\n\nMaterial from the workspace's search index, each item its source and path, its label and \
a paragraph of it:\n\n{}\n\
Answer the question in one or two sentences using ONLY this material. {QA_WEIGHING} If the material doesn't answer \
it, say you don't know. {QA_SOURCES}\n",
        qa_material(sources)
    )
}

/// The `SOURCES:` a quick answer gave, as addresses that open: each cited
/// item's `ken://` address (by its `source:path`, or its path alone), and
/// what matches no item as it was written.
fn qa_cited(cited: Vec<String>, sources: &[QaSource]) -> Vec<String> {
    cited
        .into_iter()
        .map(|c| {
            let c = c.trim().trim_matches('`').to_string();
            sources
                .iter()
                .find(|s| s.cite.eq_ignore_ascii_case(&c))
                .or_else(|| sources.iter().find(|s| s.cite.split_once(':').is_some_and(|(_, path)| path.eq_ignore_ascii_case(&c))))
                .map(|s| s.address.clone())
                .unwrap_or(c)
        })
        .collect()
}

/// What a look found: the answer and the `ken://` sources it cites.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LookAnswer {
    body: String,
    sources: Vec<String>,
}

/// "Ask Ken to look": when search found nothing, or nothing that answers, a
/// read-only Claude session searches the workspace's folders itself (Grep,
/// Glob, Read; nothing that writes or runs) and answers with citations. The
/// fallback that keeps Ken from saying something isn't there when only its
/// index missed it.
#[tauri::command]
async fn look_for(state: State<'_, SharedState>, query: String, leads: Option<Vec<String>>) -> CmdResult<LookAnswer> {
    let (root, folders) = {
        let guard = state.lock().unwrap();
        let active = member(&guard, None)?;
        let mut folders: Vec<(String, uuid::Uuid, PathBuf)> = match guard.workspace.as_ref() {
            Some(ws) => ws
                .ws
                .members
                .iter()
                .filter_map(|m| match &m.status {
                    ken_core::workspace::MemberStatus::Ok(p) => Some((m.name.clone(), p.config.id, p.root.clone())),
                    _ => None,
                })
                .collect(),
            None => Vec::new(),
        };
        if !folders.iter().any(|(_, id, _)| *id == active.project.config.id) {
            folders.insert(0, (active.project.config.name.clone(), active.project.config.id, active.project.root.clone()));
        }
        (active.project.root.clone(), folders)
    };
    let binary = ken_core::runner::discover_claude().ok_or_else(|| ken_core::runner::MISSING_CLAUDE_HELP.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let dirs: Vec<PathBuf> = folders.iter().map(|(_, _, r)| r.clone()).collect();
        let prompt = assistant::look_prompt(&query, &folders, &leads.unwrap_or_default());
        match assistant::look(&binary, &root, &dirs, &prompt, Duration::from_secs(180), &CancelToken::new()).map_err(err)? {
            OneshotOutcome::Completed(text) => {
                let parsed = digest::parse_digest(&text);
                Ok(LookAnswer { body: parsed.body, sources: parsed.sources })
            }
            OneshotOutcome::TimedOut => Err("Looking took too long; try a narrower question.".to_string()),
            OneshotOutcome::Cancelled => Err("cancelled".to_string()),
            OneshotOutcome::Failed(detail) => Err(detail),
        }
    })
    .await
    .map_err(|e| e.to_string())?
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
async fn quick_answer(
    app: AppHandle,
    state: State<'_, SharedState>,
    query: String,
) -> CmdResult<bool> {
    // Hold the global lock only long enough to clone the read-only handle + the
    // supersede counter (microseconds), then release it so the search and the
    // per-source excerpt reads run on the dedicated read-only handle off the lock,
    // never freezing the UI event loop.
    let (members, base_dir, root, project_id, qa_gen, claude) = {
        let guard = state.lock().unwrap();
        let active = member(&guard, None)?;
        // Every source of the workspace, as the overlay's matches are (decision
        // 20): a ruling in the knowledge base answers a question asked with a
        // code repo focused. Single-project mode searches the open projects.
        let members: Vec<(uuid::Uuid, String, Option<Arc<Mutex<Db>>>)> = match guard.workspace.as_ref() {
            Some(ws) => ws
                .ws
                .members
                .iter()
                .filter_map(|m| match &m.status {
                    ken_core::workspace::MemberStatus::Ok(p) => Some(p),
                    _ => None,
                })
                .map(|p| (p.config.id, p.config.name.clone(), guard.members.get(&p.config.id).map(|m| m.search_db.clone())))
                .collect(),
            None => guard.members.values().map(|m| (m.project.config.id, m.project.config.name.clone(), Some(m.search_db.clone()))).collect(),
        };
        (
            members,
            guard.base_dir.clone(),
            active.project.root.clone(),
            active.project.config.id,
            guard.qa_gen.clone(),
            ken_core::runner::discover_claude(),
        )
    };

    // The same meaningful-term tokenization the keyword search uses
    // (alphanumeric split minus English stopwords, with as-you-type guards), so
    // each excerpt centers on a term the search actually ranked on ("operating",
    // "model") rather than a stopword ("what", "the").
    let tokens = ken_core::db::significant_tokens(&query);

    // Search through the one pipeline (knowledge-core decision 15), by
    // keyword, in every source, best first, and build a ~350-char paragraph
    // excerpt per hit on the blocking pool (richer grounding than the hit's
    // chunk alone); fall back to the chunk's text when a source has no stored
    // content to excerpt. Each hit keeps its label, so the answer can weigh
    // a binding ruling over the code it rules on.
    let query_for_search = query.clone();
    let sources: Vec<QaSource> = tauri::async_runtime::spawn_blocking(move || -> CmdResult<Vec<QaSource>> {
        let mut scored: Vec<(f64, QaSource)> = Vec::new();
        for (id, name, handle) in members {
            let handle = match handle {
                Some(h) => h,
                None => match Db::open_read_only(&base_dir, id) {
                    Ok(db) => Arc::new(Mutex::new(db)),
                    Err(_) => continue,
                },
            };
            let db = lock_tolerant(&handle);
            let Ok(hits) = routing::search_member(&db, &query_for_search, None, 8) else { continue };
            for hit in hits {
                let text = db.excerpt(&hit.path, &tokens, 350).unwrap_or(hit.snippet);
                let source = QaSource {
                    cite: format!("{name}:{}", hit.path),
                    address: format!("ken://{id}/{}", hit.path),
                    label: hit.label,
                    text,
                };
                scored.push((hit.score, source));
            }
        }
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        Ok(scored.into_iter().take(8).map(|(_, s)| s).collect())
    })
    .await
    .map_err(|e| e.to_string())??;

    if sources.is_empty() {
        return Ok(true); // nothing to ground on — no card, but AI is "available"
    }

    let my_gen = qa_gen.fetch_add(1, Ordering::SeqCst) + 1;
    let local_ready = LOCAL_GENERATION
        && matches!(ken_core::local_llm::llm_status(), ken_core::local_llm::LlmStatus::Ready);

    if local_ready {
        let prompt = local_quick_answer_prompt(&query, &sources);
        std::thread::spawn(move || {
            let mut on_token = |piece: &str| -> bool {
                if qa_gen.load(Ordering::SeqCst) != my_gen {
                    return false; // superseded by a newer query
                }
                emit_member(
                    &app,
                    project_id,
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
                    emit_member(
                        &app,
                        project_id,
                        "quick-answer",
                        QuickAnswerEvent { query, body: parsed.body, sources: qa_cited(parsed.sources, &sources) },
                    );
                }
                Ok(_) => {} // superseded — a newer generation owns the card
                Err(_) => {
                    // Runtime load/inference failure → fall back to Claude,
                    // still honouring the generation id.
                    run_claude_quick_answer(app, project_id, root, claude, query, sources, qa_gen, my_gen);
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
        run_claude_quick_answer(app, project_id, root, Some(binary), query, sources, qa_gen, my_gen);
    });
    Ok(true)
}

/// Shared Claude-oneshot quick-answer path (the fallback), honouring the
/// supersede generation id so a stale answer never lands in the card.
fn run_claude_quick_answer(
    app: AppHandle,
    project_id: uuid::Uuid,
    root: std::path::PathBuf,
    claude: Option<std::path::PathBuf>,
    query: String,
    sources: Vec<QaSource>,
    qa_gen: Arc<AtomicU64>,
    my_gen: u64,
) {
    let Some(binary) = claude else { return };
    let prompt = quick_answer_prompt(&query, &sources);
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
        emit_member(
            &app,
            project_id,
            "quick-answer",
            QuickAnswerEvent { query, body: parsed.body, sources: qa_cited(parsed.sources, &sources) },
        );
    }
}

/// The on-device language model's state, for the ⌘K "not installed" hint.
#[tauri::command(async)]
fn llm_status() -> &'static str {
    generator_status()
}

/// What answers and judges in Ken (see [`LOCAL_GENERATION`]):
/// `ready` when it can run, `notInstalled` when Claude Code is missing.
/// The on-device model's own state matters only to meaning search now.
fn generator_status() -> &'static str {
    if LOCAL_GENERATION {
        return match ken_core::local_llm::llm_status() {
            ken_core::local_llm::LlmStatus::Ready => "ready",
            ken_core::local_llm::LlmStatus::NotInstalled => "notInstalled",
            ken_core::local_llm::LlmStatus::Error(_) => "error",
        };
    }
    if ken_core::runner::discover_claude().is_some() {
        "ready"
    } else {
        "notInstalled"
    }
}

/// Fire-and-forget: pay the (slow) on-device model load up front — on ⌘K open —
/// so the first quick answer streams without waiting on it. Returns immediately.
/// Only meaningful when the local model is the active backend, so skip the
/// warm-up entirely when no usable model is installed (nothing to load, and the
/// Claude path never warms).
#[tauri::command(async)]
fn warm_llm() {
    if LOCAL_GENERATION && matches!(
        ken_core::local_llm::llm_status(),
        ken_core::local_llm::LlmStatus::Ready
    ) {
        ken_core::local_llm::warm();
    }
}

/// Longest one background check's Claude call may take (a drift re-read,
/// a lint comparison, a supersession cluster).
const CHECK_TIMEOUT: Duration = Duration::from_secs(300);

/// A background Claude call for the knowledge checks (knowledge-core
/// decisions 25, 28 and 29): through the Claude CLI with no tools, never the
/// local model. Each call is a turn of the one background throttle
/// (`throttle::claude_turn`, decision 34): it waits while paused, for the
/// pace's gap and for one of the hour's calls. A call asked to stop while it
/// waits is an error, which leaves its work waiting. None when Claude Code
/// is not installed.
fn background_claude(
    _state: &SharedState,
    _project_id: uuid::Uuid,
    root: &Path,
    stop: &Arc<AtomicBool>,
) -> Option<impl FnMut(&str) -> ken_core::Result<String>> {
    let binary = ken_core::runner::discover_claude()?;
    let (root, stop) = (root.to_path_buf(), stop.clone());
    Some(move |prompt: &str| -> ken_core::Result<String> {
        let halt = || stop.load(Ordering::SeqCst);
        let Some(_turn) = ken_core::throttle::claude_turn(&halt) else {
            return Err(ken_core::Error::Other("stopped".into()));
        };
        let out = ken_core::assistant::oneshot_answer(&binary, &root, prompt, None, CHECK_TIMEOUT, &CancelToken::new());
        match out? {
            ken_core::assistant::OneshotOutcome::Completed(text) => Ok(text),
            other => Err(ken_core::Error::Other(format!("claude: {other:?}"))),
        }
    })
}

/// One drift run for the knowledge base `project` (knowledge-core decision
/// 28), when one is due (`drift::due`: its sources' branch moved, the age
/// rule's week, or pages waiting for a re-read), or now when `force`: the
/// sweep, what it found acted on with re-reads through
/// [`background_claude`], and the run's commit. Then the code citations of
/// the workspace's other repos are read against the decisions log (decision
/// 30): the ones the log lacks are findings, the rest graph edges (decision
/// 21). It spawns git per cited file, so callers run it off the UI thread.
/// Returns the run.
fn run_drift(state: &SharedState, project: &Project, db: &mut Db, force: bool, stop: &Arc<AtomicBool>) -> Option<ken_core::drift::DriftRun> {
    if !ken_core::registry::is_library(&project.root) && !force {
        return None;
    }
    let now = engine::now_epoch();
    if !force && !ken_core::drift::due(db, now).unwrap_or(false) {
        return None;
    }
    let mut claude = background_claude(state, project.config.id, &project.root, stop);
    let reread = claude.as_mut().map(|c| c as &mut ken_core::drift::Reread<'_>);
    let run = match ken_core::drift::run(project, db, now, &local_date_today(), reread) {
        Ok(run) => run,
        Err(e) => {
            eprintln!("warning: drift run failed: {e}");
            return None;
        }
    };
    // Rulings cited in code (decision 30): the other repos' indexes, read only.
    let base = lock_tolerant(state).base_dir.clone();
    let code: Vec<ken_core::ingest::CodeSource> = code_sources_of(state, &project.root)
        .into_iter()
        .filter_map(|(name, id)| Db::open_read_only(&base, id).ok().map(|db| ken_core::ingest::CodeSource { name, db }))
        .collect();
    // Every code repo of the workspace is watched from now on, cited yet or
    // not, so its next commit makes a run due (decision 28).
    let watched: Vec<(String, PathBuf)> = {
        let guard = lock_tolerant(state);
        workspace_repos(&guard).into_iter().filter(|(_, id, _)| *id != project.config.id).map(|(name, _, root)| (name, root)).collect()
    };
    if let Err(e) = ken_core::drift::watch_repos(db, &project.root, &watched) {
        eprintln!("warning: could not watch the workspace's repos: {e}");
    }
    match ken_core::lint::rulings_in_code(db, &project.root, &code) {
        Ok(found) => {
            if let Err(e) = ken_core::findings::sync(db, &[ken_core::lint::UNKNOWN_RULING], &ken_core::lint::unknown_of(&found), now) {
                eprintln!("warning: could not file the code citations: {e}");
            }
            if let Err(e) = ken_core::graph::store_code_citations(db, &found) {
                eprintln!("warning: could not store the code citations: {e}");
            }
        }
        Err(e) => eprintln!("warning: could not read the code citations: {e}"),
    }
    Some(run)
}

/// How long the checks are left alone after a Claude call failed. How often
/// they are looked at is the throttle's pace (`Limits::checks_every`).
const CHECKS_BACKOFF: Duration = Duration::from_secs(600);

/// One turn of a knowledge base's checks (knowledge-core decisions 21, 25,
/// 28 to 30), for a knowledge base only: a drift run when one is due
/// ([`run_drift`]), the graph's commit edges from each repo whose branch
/// moved, then at most one background Claude call, the first of what
/// waits: the lint after an edit, the weekly lint, the supersession pass
/// over new decisions-log entries. Topic tags need no call of their own:
/// the ingest plan and the drift re-read give them (decision 21). The
/// lint that needs no model, the supersession markers and the graph's own
/// edges are made with every scan (`scan::after_scan`).
fn knowledge_checks(state: &SharedState, project_id: uuid::Uuid, stop: &Arc<AtomicBool>) -> SummaryTick {
    let (base, project) = {
        let guard = lock_tolerant(state);
        let Some(active) = guard.members.get(&project_id) else {
            return SummaryTick::Stopped;
        };
        (guard.base_dir.clone(), active.project.clone())
    };
    if !ken_core::registry::is_library(&project.root) {
        return SummaryTick::Idle;
    }
    let Ok(mut db) = Db::open(&base, project_id) else {
        return SummaryTick::Idle;
    };
    run_drift(state, &project, &mut db, false, stop);
    if stop.load(Ordering::SeqCst) {
        return SummaryTick::Stopped;
    }
    // Commits citing a ticket or a ruling (decision 21): each repo read from
    // the head read last time, one git call for a repo that has not moved.
    let repos: Vec<(String, PathBuf)> = {
        let guard = lock_tolerant(state);
        workspace_repos(&guard).into_iter().map(|(name, _, root)| (name, root)).collect()
    };
    if let Err(e) = ken_core::graph::read_commits(&mut db, &project.root, &repos) {
        eprintln!("graph: {e}");
    }
    let Some(mut claude) = background_claude(state, project_id, &project.root, stop) else {
        return SummaryTick::Idle;
    };
    let root = project.root.as_path();
    let now = engine::now_epoch();
    let label = project.config.name.clone();
    let _running = ken_core::throttle::begin("checks", &label);
    let done = |r: ken_core::Result<usize>| match r {
        Ok(_) => {
            ken_core::throttle::clear_failure("checks", &label);
            SummaryTick::Ran
        }
        Err(e) => {
            eprintln!("checks: {e}");
            ken_core::throttle::fail("checks", &label, &e.to_string());
            SummaryTick::Failed
        }
    };
    if let Ok(Some(p)) = ken_core::lint::after_edit_next(&db, root) {
        return done(claude(&p.prompt).and_then(|reply| ken_core::lint::after_edit_done(&mut db, root, &p, &reply, now, &local_date_today())));
    }
    if ken_core::lint::weekly_due(&db, now).unwrap_or(false) {
        if let Ok(Some(p)) = ken_core::lint::weekly_next(&db, root, now) {
            return done(claude(&p.prompt).and_then(|reply| ken_core::lint::weekly_done(&mut db, root, &reply, now, &local_date_today())));
        }
    }
    let roles = ken_core::authority::Roles::of_repo(root);
    if let Ok(Some(p)) = ken_core::supersede::next_cluster(&db, &roles, &ken_core::supersede::Tuning::default()) {
        return done(claude(&p.prompt).and_then(|reply| ken_core::supersede::judged(&mut db, &p, &reply, now)));
    }
    SummaryTick::Idle
}

/// Projects with an ingest pass in flight, so a scan that lands mid-pass
/// does not start a second one over the same inbox.
static INGEST_RUNNING: std::sync::LazyLock<Mutex<std::collections::HashSet<uuid::Uuid>>> =
    std::sync::LazyLock::new(|| Mutex::new(std::collections::HashSet::new()));

/// Libraries asked for another pass while one ran: it runs again when it
/// ends, so a source dropped mid-pass is not left queued.
static INGEST_AGAIN: std::sync::LazyLock<Mutex<std::collections::HashSet<uuid::Uuid>>> =
    std::sync::LazyLock::new(|| Mutex::new(std::collections::HashSet::new()));

/// The source each library's pass is on now, and what it is doing with it.
type PassAt = (String, ken_core::ingest::PassPhase);
static INGEST_READING: std::sync::LazyLock<Mutex<std::collections::HashMap<uuid::Uuid, PassAt>>> =
    std::sync::LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

/// The libraries whose ingest waits for its turn of the throttle now
/// (paused, or the gap): the status panel lists the source as waiting, not
/// running.
static INGEST_WAITING: std::sync::LazyLock<Mutex<std::collections::HashSet<uuid::Uuid>>> =
    std::sync::LazyLock::new(|| Mutex::new(std::collections::HashSet::new()));

/// Why a library's last pass stopped before the end (Claude Code itself
/// failed), until the next pass starts.
static INGEST_STOPPED: std::sync::LazyLock<Mutex<std::collections::HashMap<uuid::Uuid, String>>> =
    std::sync::LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

/// The latest inbox change per library, so the watcher's pass waits until
/// a burst of changes settles.
static INGEST_SETTLE_GEN: std::sync::LazyLock<Mutex<std::collections::HashMap<uuid::Uuid, u64>>> =
    std::sync::LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

/// How long the inbox must stay unchanged before the watcher starts a pass.
const INGEST_SETTLE: Duration = Duration::from_secs(5);

/// The repos an ingest in the library at `root` checks a source against
/// (knowledge-core decision 10): every other repo of the open workspace, by
/// name and id. Their indexes are opened for reading on the pass's thread.
fn code_sources(guard: &AppState, root: &Path) -> Vec<(String, uuid::Uuid)> {
    guard
        .workspace
        .as_ref()
        .map(|w| {
            w.ws.members
                .iter()
                .filter_map(|m| match &m.status {
                    ken_core::workspace::MemberStatus::Ok(p) if !same_dir(&p.root, root) => Some((p.config.name.clone(), p.config.id)),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Every repo of the open workspace as (name, id, checkout), or the open
/// members when no workspace is open. The memory pseudo-member is not one.
fn workspace_repos(guard: &AppState) -> Vec<(String, uuid::Uuid, PathBuf)> {
    let pseudo = memory_pseudo_member_id(guard);
    let all: Vec<(String, uuid::Uuid, PathBuf)> = match guard.workspace.as_ref() {
        Some(w) => w
            .ws
            .members
            .iter()
            .filter_map(|m| match &m.status {
                ken_core::workspace::MemberStatus::Ok(p) => Some((p.config.name.clone(), p.config.id, p.root.clone())),
                _ => None,
            })
            .collect(),
        None => guard.members.values().map(|m| (m.project.config.name.clone(), m.project.config.id, m.project.root.clone())).collect(),
    };
    all.into_iter().filter(|(_, id, _)| Some(*id) != pseudo).collect()
}

/// The workspace's knowledge base: the repo `prefer` names when it is one,
/// else the first repo that is (`registry::is_library`).
fn knowledge_base_of(repos: &[(String, uuid::Uuid, PathBuf)], prefer: Option<uuid::Uuid>) -> Option<(String, uuid::Uuid, PathBuf)> {
    let is_kb = |r: &&(String, uuid::Uuid, PathBuf)| ken_core::registry::is_library(&r.2);
    repos
        .iter()
        .filter(is_kb)
        .find(|r| Some(r.1) == prefer)
        .or_else(|| repos.iter().find(is_kb))
        .cloned()
}

/// [`code_sources`], taking the lock.
fn code_sources_of(state: &SharedState, root: &Path) -> Vec<(String, uuid::Uuid)> {
    code_sources(&lock_tolerant(state), root)
}

/// The Whisper model a pass transcribes with: the selected one when it is
/// installed, else any installed one.
fn transcription_model(base: &Path) -> Option<PathBuf> {
    model::selected_model_path(base, model::ModelCategory::Transcription)
        .or_else(|| Some(transcript::model_path(base)).filter(|p| p.is_file()))
}

/// What Ken's MCP server signals a running app once it filed something
/// (`hooks::signal_app`; Chris, 2026-10-14): `proposed` (a ruling from chat
/// waits in the knowledge base's Inbox) refreshes the Inbox; `source-added`
/// (a source put in Raw) starts the ingest of the knowledge base it names,
/// when that is open, and refreshes the Inbox. The listener serves one
/// request at a time, so the work is done off its thread.
fn app_signal(app: &AppHandle, v: serde_json::Value) {
    let app = app.clone();
    std::thread::spawn(move || {
        if v.get("signal").and_then(|s| s.as_str()) == Some("source-added") {
            let state = tauri::Manager::state::<SharedState>(&app);
            let id = v.get("projectId").and_then(|s| s.as_str()).and_then(|s| s.parse::<uuid::Uuid>().ok());
            let found = {
                let guard = lock_tolerant(state.inner());
                id.and_then(|id| guard.members.get(&id)).map(|m| (m.project.clone(), guard.base_dir.clone()))
            };
            if let Some((project, base)) = found {
                schedule_ingest_pass(state.inner(), &project, &base);
            }
        }
        let _ = app.emit("review-changed", ());
    });
}

/// A pass over `project`'s inbox once its changes settle ([`INGEST_SETTLE`]):
/// the watcher's way in, so a file copied into Raw/ by hand is read too.
fn schedule_ingest_pass(state: &SharedState, project: &Project, base: &Path) {
    let id = project.config.id;
    let gen = {
        let mut gens = INGEST_SETTLE_GEN.lock().unwrap();
        let g = gens.entry(id).or_insert(0);
        *g += 1;
        *g
    };
    let (state, project, base) = (state.clone(), project.clone(), base.to_path_buf());
    std::thread::spawn(move || {
        std::thread::sleep(INGEST_SETTLE);
        if INGEST_SETTLE_GEN.lock().unwrap().get(&id) != Some(&gen) {
            return; // a later change restarted the wait
        }
        start_ingest_pass(&project, &base, code_sources_of(&state, &project.root), false);
    });
}

/// Read every new source waiting in a library's `Research/Ingestion/Raw/`,
/// one at a time on a background thread, through the Claude CLI, never the
/// local model (`ingest::run_pass`): a recording is transcribed first with
/// the selected Whisper model; each source becomes its dated note, is
/// claimed, is checked against the knowledge base and the code of `others`
/// (the workspace's other repos, by name and id), and what follows from it
/// is written and committed in the library. A source that fails stays in Raw
/// with its failure recorded and the pass moves on; the pass stops only when
/// the CLI itself fails. A library is a repo with the inbox, whatever its kind
/// (`registry::is_library`); `force` reads any repo with an inbox. Returns
/// whether a pass started or was queued behind the one running.
fn start_ingest_pass(project: &Project, base: &Path, others: Vec<(String, uuid::Uuid)>, force: bool) -> bool {
    let root = project.root.clone();
    if !ken_core::ingest::has_inbox(&root) || ken_core::ingest::waiting(&root).is_empty() {
        return false;
    }
    if !force && !ken_core::registry::is_library(&root) {
        return false;
    }
    let id = project.config.id;
    let project_name = format!("{}: reading the inbox", project.config.name);
    let Some(binary) = ken_core::runner::discover_claude() else {
        INGEST_STOPPED.lock().unwrap().insert(id, ken_core::runner::MISSING_CLAUDE_HELP.to_string());
        return false;
    };
    {
        let mut running = INGEST_RUNNING.lock().unwrap();
        if !running.insert(id) {
            INGEST_AGAIN.lock().unwrap().insert(id);
            return true;
        }
    }
    let base = base.to_path_buf();
    std::thread::spawn(move || {
        loop {
            INGEST_STOPPED.lock().unwrap().remove(&id);
            match Db::open(&base, id) {
                Ok(mut db) => {
                    // The other repos' indexes, read only: what each source is
                    // checked against. One that will not open is left out.
                    let code: Vec<ken_core::ingest::CodeSource> = others
                        .iter()
                        .filter_map(|(name, other)| {
                            Db::open_read_only(&base, *other).ok().map(|db| ken_core::ingest::CodeSource { name: name.clone(), db })
                        })
                        .collect();
                    let model = transcription_model(&base);
                    let ffmpeg = transcript::discover_ffmpeg();
                    let transcriber = ken_core::ingest::Transcriber { model: model.as_deref(), ffmpeg: ffmpeg.as_deref() };
                    let on_phase: Arc<dyn Fn(&str, ken_core::ingest::PassPhase) + Send + Sync> = Arc::new(move |raw, phase| {
                        INGEST_READING.lock().unwrap().insert(id, (raw.to_string(), phase));
                    });
                    // Each ingest call is a turn of the one throttle
                    // (knowledge-core decision 34): paused with everything else
                    // and paced. A source the build queue placed is background
                    // work, counted against the hour's calls; a source a person
                    // dropped or added is their own ingest, which the hour does
                    // not count or hold (Chris, 2026-10-14).
                    let call = |prompt: &str| {
                        let reading = INGEST_READING.lock().unwrap().get(&id).map(|(raw, _)| raw.clone());
                        INGEST_WAITING.lock().unwrap().insert(id);
                        let _turn = if reading.is_none_or(|raw| placed_by_build(&base, id, &raw)) {
                            ken_core::throttle::claude_turn(&|| false)
                        } else {
                            ken_core::throttle::own_turn(&|| false)
                        };
                        INGEST_WAITING.lock().unwrap().remove(&id);
                        ken_core::ingest::call_result(ken_core::assistant::oneshot(
                            &binary,
                            &root,
                            prompt,
                            Duration::from_secs(600),
                            &CancelToken::new(),
                        ))
                    };
                    let _running = ken_core::throttle::begin("ingest", &project_name);
                    let today = local_date_today();
                    match ken_core::ingest::run_pass(&root, &mut db, &code, &today, engine::now_epoch, transcriber, call, &on_phase) {
                        Ok(report) => {
                            if let Some(why) = report.stopped {
                                INGEST_STOPPED.lock().unwrap().insert(id, why);
                            }
                        }
                        Err(e) => eprintln!("warning: ingest pass failed: {e}"),
                    }
                }
                Err(e) => eprintln!("warning: ingest pass could not open the index: {e}"),
            }
            INGEST_READING.lock().unwrap().remove(&id);
            // Asked for again while this one ran: go round once more.
            let mut running = INGEST_RUNNING.lock().unwrap();
            if !INGEST_AGAIN.lock().unwrap().remove(&id) {
                running.remove(&id);
                break;
            }
        }
    });
    true
}

/// Whether the source at `raw` in the knowledge base `id`'s Raw was placed
/// there by its build queue (`buildqueue::placed`), so its ingest is
/// background work. When the queue cannot be read, it is counted as
/// background work too.
fn placed_by_build(base: &Path, id: uuid::Uuid, raw: &str) -> bool {
    match Db::open_read_only(base, id).and_then(|db| ken_core::buildqueue::load(&db)) {
        Ok(Some(queue)) => ken_core::buildqueue::placed(&queue, raw),
        Ok(None) => false,
        Err(_) => true,
    }
}

/// A pass over every open library, as after a transcription model is
/// installed: recordings that waited for one are read.
fn start_ingest_passes(state: &SharedState) {
    let (libraries, base) = {
        let guard = lock_tolerant(state);
        let libs: Vec<(Project, Vec<(String, uuid::Uuid)>)> = guard
            .members
            .values()
            .map(|m| m.project.clone())
            .filter(|p| ken_core::ingest::has_inbox(&p.root))
            .map(|p| {
                let others = code_sources(&guard, &p.root);
                (p, others)
            })
            .collect();
        (libs, guard.base_dir.clone())
    };
    for (p, others) in libraries {
        start_ingest_pass(&p, &base, others, false);
    }
}

/// Ken's generation policy: the on-device model embeds (meaning search) and
/// nothing else. Everything that writes, extracts, judges or answers runs
/// through Claude, in-agent Ken; the local 4B model, measured on a real
/// knowledge base, left half its extractions unparseable at ~42 s each and
/// merged near-miss names it should not have.
const LOCAL_GENERATION: bool = false;

/// Create a knowledge base from the bundled template at `dir` (a new or
/// empty folder) and return its set-up row. It holds the tickets and the
/// decisions log too. Local only: a remote and a push are the person's step.
#[tauri::command]
async fn setup_create_wiki(
    dir: String,
    name: String,
    repos: Vec<ken_core::wikinew::Covered>,
    taken: Vec<String>,
) -> CmdResult<ken_core::setup::RepoRow> {
    tauri::async_runtime::spawn_blocking(move || {
        ken_core::setup::create_wiki(Path::new(&dir), &name, &repos, &taken, &local_date_today()).map_err(err)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// What overlaying an existing wiki did: its set-up row and the template
/// files laid down.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OverlayDto {
    row: ken_core::setup::RepoRow,
    added: Vec<String>,
}

/// Set-up's second step, the other choice (knowledge-core decision 32): lay
/// Ken's structure over an existing wiki at `dir`. Only files it lacks are
/// written; nothing it has is changed or deleted. Its pages are mapped into
/// the sections after a dry run ([`wiki_map_plan`]).
#[tauri::command]
async fn setup_overlay_wiki(
    dir: String,
    name: String,
    repos: Vec<ken_core::wikinew::Covered>,
    taken: Vec<String>,
) -> CmdResult<OverlayDto> {
    tauri::async_runtime::spawn_blocking(move || {
        let (row, added) = ken_core::setup::overlay_wiki(Path::new(&dir), &name, &repos, &taken, &local_date_today()).map_err(err)?;
        Ok(OverlayDto { row, added })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The dry run of mapping an existing wiki at `dir` into the sections
/// (knowledge-core decision 33): every move with why, and what stays where
/// it is. Moves nothing.
#[tauri::command(async)]
fn wiki_map_plan(dir: String) -> CmdResult<ken_core::wikimap::Plan> {
    let dir = PathBuf::from(dir);
    if !dir.is_dir() {
        return Err(format!("{} is not a folder", dir.display()));
    }
    Ok(ken_core::wikimap::plan(&dir))
}

/// Apply the moves a person confirmed from [`wiki_map_plan`]: pages moved,
/// links rewritten where certain and the rest reported, a `log.md` line and
/// one commit. Nothing is deleted.
#[tauri::command(async)]
fn wiki_map_apply(dir: String, moves: Vec<ken_core::wikimap::Move>) -> CmdResult<ken_core::wikimap::Applied> {
    ken_core::wikimap::apply(Path::new(&dir), &moves, &local_date_today()).map_err(err)
}

/// Apply a proposed change to a page a person keeps, only while the page
/// still reads as it did when Ken proposed it: a held edit, a drafted ticket,
/// or a ready ruling, which takes the next number in the decisions log
/// (anyone may add it; knowledge-core decision 13). In the knowledge base the
/// change is then its own commit of that page alone (decision 36).
#[tauri::command(async)]
fn apply_page_proposal(state: State<SharedState>, item_id: i64, project_id: Option<String>) -> CmdResult<String> {
    let id = project_id.as_deref().map(|p| p.parse::<uuid::Uuid>()).transpose().map_err(err)?;
    let mut guard = state.lock().unwrap();
    let active = member_mut(&mut guard, id)?;
    let item = active
        .db
        .list_open_review_items()
        .map_err(err)?
        .into_iter()
        .find(|it| it.id == item_id && it.kind == ken_core::proposal::PROPOSAL_KIND)
        .ok_or("no open proposal with that id")?;
    let proposal: ken_core::proposal::Proposal =
        serde_json::from_str(item.payload.as_deref().unwrap_or("")).map_err(|_| "the card has no proposed change")?;
    match ken_core::proposal::apply(&active.project.root, &proposal) {
        Ok(()) => {}
        Err(ken_core::proposal::ApplyError::Changed) => {
            return Err(format!(
                "{} changed after Ken proposed this, so applying it would undo that edit. Discard it; the next source added proposes again.",
                proposal.page
            ))
        }
        Err(ken_core::proposal::ApplyError::Io(e)) => return Err(e),
    }
    if let Err(e) = ken_core::ingest::accepted(&active.project.root, &proposal) {
        eprintln!("warning: could not commit {}: {e}", proposal.page);
    }
    settle_review_item(&active.project.root, &mut active.db, item_id)?;
    Ok(proposal.page)
}

/// The knowledge base the Ingest screen works on: the workspace's repo with
/// an inbox (`Research/Ingestion/Raw/`), a wiki before any other, then
/// whichever repo is focused. With no workspace, the focused repo. A
/// workspace has one knowledge base (decision 35: no team repo).
fn inbox_project(guard: &AppState) -> Option<Project> {
    let reg = Registry::load(&guard.base_dir).ok();
    let mut best: Option<(i32, Project)> = None;
    let mut consider = |p: &Project| {
        if !ken_core::ingest::has_inbox(&p.root) {
            return;
        }
        let entry = reg.as_ref().and_then(|r| r.entry_at(&p.root));
        let mut score = 0;
        if entry.is_some_and(|e| e.kind.contains(&ken_core::registry::RepoKind::Wiki)) {
            score += 2;
        }
        if guard.focused == Some(p.config.id) {
            score += 1;
        }
        if best.as_ref().is_none_or(|(s, _)| score > *s) {
            best = Some((score, p.clone()));
        }
    };
    match guard.workspace.as_ref() {
        Some(ws) => {
            for m in &ws.ws.members {
                if let ken_core::workspace::MemberStatus::Ok(p) = &m.status {
                    consider(p);
                }
            }
        }
        None => {
            if let Ok(active) = member(guard, None) {
                consider(&active.project);
            }
        }
    }
    best.map(|(_, p)| p)
}

/// The workspace's knowledge base: the repo with the inbox, the one Files
/// opens on under Wiki. None when no repo has one.
#[tauri::command(async)]
fn knowledge_base_id(state: State<SharedState>) -> CmdResult<Option<String>> {
    let guard = state.lock().unwrap();
    Ok(inbox_project(&guard).map(|p| p.config.id.to_string()))
}

/// [`inbox_project`], opened if it was dormant, focus left where it was.
fn inbox_member(app: &AppHandle, state: &SharedState) -> CmdResult<uuid::Uuid> {
    let (project, open, focused) = {
        let guard = state.lock().unwrap();
        let project = inbox_project(&guard).ok_or("No source of this workspace has an inbox (Research/Ingestion/Raw/). A knowledge base made by set-up has one.")?;
        let open = guard.members.contains_key(&project.config.id);
        (project, open, guard.focused)
    };
    let id = project.config.id;
    if !open {
        activate(app, state, project, false)?;
        state.lock().unwrap().focused = focused;
    }
    Ok(id)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RawSourceDto {
    path: String,
    name: String,
    /// `queued` | `transcribing` | `reading` | `read` | `failed` | `waiting`
    /// (a recording waiting for what Ken cannot make here: a transcription
    /// model, ffmpeg)
    state: String,
    /// Why it failed or waits; how far transcribing is (`42%`).
    detail: Option<String>,
    /// meeting | recording | document: from its note once read, else a
    /// guess from the file (empty when there is none).
    kind: String,
    /// `48 min`, `14 pages` from its note; the file's size before that.
    length: String,
    /// Who was present, from its note.
    present: Vec<String>,
    /// Its card, once read (a source read before sources were claimed as
    /// their note was written waits in Raw/ with its card).
    card_id: Option<i64>,
    /// How many pages were written from it.
    pages: usize,
    /// What waits from it.
    waiting: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IngestedDto {
    /// The ingest card's id.
    id: i64,
    note: String,
    title: String,
    kind: String,
    length: String,
    present: Vec<String>,
    /// When the note was written (Unix seconds).
    at: i64,
    /// Not yet marked seen.
    open: bool,
    /// How many pages were written from it.
    pages: usize,
    /// What waits from it: rulings, tickets, held edits.
    waiting: usize,
    /// Its note is written and Claude is still writing the pages.
    writing: bool,
}

/// What a source in Raw is before its note says: a guess from its name.
fn raw_kind(name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "vtt" | "srt" | "mp4" | "mov" | "m4v" | "avi" | "m4a" | "aac" | "mp3" | "wav" | "flac" | "ogg" | "oga" | "opus"
        | "webm" | "mkv" => "recording",
        "pdf" | "docx" | "doc" | "pptx" | "xlsx" | "odt" | "rtf" | "html" => "document",
        _ => "",
    }
}

/// A file's size as a person reads it.
fn file_size(path: &Path) -> String {
    match std::fs::metadata(path).map(|m| m.len()) {
        Ok(n) if n >= 1024 * 1024 => format!("{:.1} MB", n as f64 / (1024.0 * 1024.0)),
        Ok(n) if n >= 1024 => format!("{} KB", n / 1024),
        Ok(n) => format!("{n} bytes"),
        Err(_) => String::new(),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IngestOverviewDto {
    /// The library's project id and name.
    project_id: String,
    library: String,
    raw: Vec<RawSourceDto>,
    ingested: Vec<IngestedDto>,
    running: bool,
    claude_found: bool,
    /// Sources with something waiting on the person, for the Ingest badge:
    /// in Raw, one that failed, waits for a transcript it cannot get, or has
    /// rulings, tickets or held edits waiting; filed, one with those still
    /// waiting.
    waiting: usize,
    /// Why the last read stopped before the end (Claude Code itself failed:
    /// not installed, not logged in, timed out), until the next one starts.
    stopped: Option<String>,
}

/// The Ingest screen: the knowledge base's inbox, every source
/// in Raw with where it is in the read, and what has been ingested, newest
/// first.
#[tauri::command(async)]
fn ingest_overview(app: AppHandle, state: State<SharedState>) -> CmdResult<IngestOverviewDto> {
    let id = inbox_member(&app, &state)?;
    // Cloned under the lock, read after it is released: Home, Inbox and the
    // sidebar ask for this on every index update.
    let (base, root, library, search_db) = {
        let guard = lock_tolerant(&state);
        let active = member(&guard, Some(id))?;
        (guard.base_dir.clone(), active.project.root.clone(), active.project.config.name.clone(), active.search_db.clone())
    };
    let root = &root;
    let db_guard = lock_tolerant(&search_db);
    let db: &Db = &db_guard;
    let reading = INGEST_READING.lock().unwrap().get(&id).cloned();
    let open = db.list_open_review_items().map_err(err)?;
    // The open cards of sources still in Raw: read before a source was
    // claimed as its note was written, not yet seen.
    let cards: Vec<(i64, ken_core::ingest::Card)> = open
        .iter()
        .filter(|it| it.kind == ken_core::ingest::REVIEW_KIND)
        .filter_map(|it| ken_core::ingest::card_of(it.payload.as_deref()).map(|c| (it.id, c)))
        .filter(|(_, c)| root.join(&c.placement.raw).is_file())
        .collect();
    let counts = ken_core::ingest::proposal_counts(&open);
    let waits = |note: &str| counts.get(note).copied().unwrap_or(0);
    // What a recording with no transcript waits for, if anything.
    let has_model = transcription_model(&base).is_some();
    let has_ffmpeg = std::cell::OnceCell::new();
    let raw: Vec<RawSourceDto> = ken_core::ingest::waiting(root)
        .into_iter()
        .map(|path| {
            let name = path.rsplit('/').next().unwrap_or(&path).to_string();
            let failed = open.iter().find(|it| it.kind == ken_core::ingest::FAILED_KIND && it.source_ref == path);
            let card = cards.iter().find(|(_, c)| c.placement.raw == path);
            let on_it = reading.as_ref().filter(|(p, _)| *p == path).map(|(_, phase)| *phase);
            let waits_for = || {
                if !ken_core::ingest::needs_transcript(root, &path) {
                    return None;
                }
                let ffmpeg = *has_ffmpeg.get_or_init(|| transcript::discover_ffmpeg().is_some());
                transcript::waiting_reason(&root.join(&path), ffmpeg, has_model)
            };
            let (state, detail) = match (on_it, card, failed) {
                (Some(ken_core::ingest::PassPhase::Transcribing(pct)), _, _) => ("transcribing", Some(format!("{pct}%"))),
                (Some(ken_core::ingest::PassPhase::Reading | ken_core::ingest::PassPhase::Writing), _, _) => ("reading", None),
                // Read since it failed: the card is what counts.
                (None, Some(_), _) => ("read", None),
                (None, None, Some(f)) => ("failed", Some(f.body.clone())),
                (None, None, None) => match waits_for() {
                    Some(why) => ("waiting", Some(why)),
                    None => ("queued", None),
                },
            };
            let (kind, length, present) = match card {
                Some((_, c)) => {
                    let text = std::fs::read_to_string(root.join(&c.placement.note)).unwrap_or_default();
                    let t = ken_core::ingest::takeaways(&c.placement.note, &text);
                    (t.kind, t.length, t.present)
                }
                None => (raw_kind(&name).to_string(), file_size(&root.join(&path)), Vec::new()),
            };
            RawSourceDto {
                state: state.to_string(),
                detail,
                kind,
                length,
                present,
                card_id: card.map(|(id, _)| *id),
                pages: card.map(|(_, c)| c.pages.len()).unwrap_or(0),
                waiting: card.map(|(_, c)| waits(&c.placement.note)).unwrap_or(0),
                name,
                path,
            }
        })
        .collect();
    let mut filed_waiting = 0;
    // The source whose note is written and whose pages are being written.
    let writing_raw = reading.as_ref().filter(|(_, phase)| *phase == ken_core::ingest::PassPhase::Writing).map(|(raw, _)| raw.clone());
    let ingested = db
        .review_items_of_kind(ken_core::ingest::REVIEW_KIND, 200)
        .map_err(err)?
        .into_iter()
        .map(|it| {
            let note = it.source_ref.clone();
            let text = std::fs::read_to_string(root.join(&note)).unwrap_or_default();
            let t = ken_core::ingest::takeaways(&note, &text);
            let card = ken_core::ingest::card_of(it.payload.as_deref());
            let waiting = waits(&note);
            let in_raw = card.as_ref().is_some_and(|c| root.join(&c.placement.raw).is_file());
            if waiting > 0 && !in_raw {
                filed_waiting += 1;
            }
            IngestedDto {
                id: it.id,
                title: if t.title.is_empty() { it.title.trim_start_matches("Ingested: ").to_string() } else { t.title },
                kind: t.kind,
                length: t.length,
                present: t.present,
                at: it.created_at,
                open: it.status == "open",
                pages: card.as_ref().map_or(0, |c| c.pages.len()),
                waiting,
                writing: card.as_ref().is_some_and(|c| writing_raw.as_deref() == Some(c.placement.raw.as_str())),
                note,
            }
        })
        .collect();
    drop(db_guard);
    let waiting = raw.iter().filter(|r| r.state == "failed" || r.state == "waiting" || r.waiting > 0).count() + filed_waiting;
    Ok(IngestOverviewDto {
        project_id: id.to_string(),
        library,
        raw,
        ingested,
        running: INGEST_RUNNING.lock().unwrap().contains(&id),
        claude_found: ken_core::runner::discover_claude().is_some(),
        waiting,
        stopped: INGEST_STOPPED.lock().unwrap().get(&id).cloned(),
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IngestProposalDto {
    id: i64,
    /// What waits: `ruling` (a ready entry for the decisions log), `risk` or
    /// `issue` (an entry for the RAID register), `ticket` (a drafted ticket),
    /// or an edit held for a person, `page` (a change to one) or `new page`.
    kind: String,
    title: String,
    /// The page (or ticket file) it writes.
    page: String,
    body: String,
    /// The proposal itself (the page as it is and with the change), for the diff.
    payload: Option<String>,
    /// Who made a ruling, or who watches a risk or issue, as the note says;
    /// free text, never a check.
    who: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IngestPageDto {
    /// The page, in the library.
    path: String,
    /// The change in a line, or a new page's purpose.
    label: String,
    /// A new page, not an edit.
    new: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IngestCardDto {
    id: i64,
    open: bool,
    note: String,
    source: String,
    takeaways: ken_core::ingest::Takeaways,
    /// The pages written from it, in order.
    pages: Vec<IngestPageDto>,
    /// What waits from it.
    proposals: Vec<IngestProposalDto>,
    /// The knowledge base's commit holding the ingest (short sha), when
    /// there is one: git is the undo.
    commit: Option<String>,
    /// Why the commit failed, when it did.
    commit_error: Option<String>,
    /// Its note is written and Claude is still writing the pages.
    writing: bool,
}

/// Two folders the same, whatever the case and the slashes.
fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().replace('\\', "/").trim_end_matches('/').to_lowercase();
    norm(a) == norm(b)
}

/// One waiting proposal as the Ingest screen and the Inbox show it.
fn proposal_dto(it: ken_core::db::ReviewItemRow) -> IngestProposalDto {
    let p: Option<ken_core::proposal::Proposal> = it.payload.as_deref().and_then(|p| serde_json::from_str(p).ok());
    IngestProposalDto {
        id: it.id,
        kind: p.as_ref().map_or("page", ken_core::proposal::kind_of).into(),
        page: p.as_ref().map(|p| p.page.clone()).unwrap_or_default(),
        who: p
            .as_ref()
            .and_then(|p| p.append.as_ref().and_then(|r| r.who.clone()).or_else(|| p.raid.as_ref().and_then(|d| d.owner.clone()))),
        title: it.title,
        body: it.body,
        payload: it.payload,
    }
}

/// A proposal that waits from no ingest card: a ruling proposed in chat
/// (MCP `propose_decision`) or an edit a drift run held.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LooseProposalDto {
    #[serde(flatten)]
    proposal: IngestProposalDto,
    /// `chat` or `drift`.
    from: String,
    project_id: String,
}

/// The proposals waiting in the knowledge base that no ingest card holds,
/// for the Inbox: a ruling proposed in chat, an edit a drift run held.
/// The ones from a note are read through their card.
#[tauri::command(async)]
fn loose_proposals(state: State<SharedState>) -> CmdResult<Vec<LooseProposalDto>> {
    let found = {
        let guard = lock_tolerant(&state);
        inbox_project(&guard).and_then(|p| guard.members.get(&p.config.id).map(|m| (p.config.id, m.search_db.clone())))
    };
    let Some((id, db)) = found else { return Ok(Vec::new()) };
    let open = lock_tolerant(&db).list_open_review_items().map_err(err)?;
    Ok(open
        .into_iter()
        .filter(|it| it.kind == ken_core::proposal::PROPOSAL_KIND)
        .filter_map(|it| {
            let from = it.payload.as_deref().and_then(|p| serde_json::from_str::<ken_core::proposal::Proposal>(p).ok())?.from?;
            (from == ken_core::proposal::FROM_CHAT || from == ken_core::drift::HELD_FROM)
                .then(|| LooseProposalDto { proposal: proposal_dto(it), from, project_id: id.to_string() })
        })
        .collect())
}

/// One ingested source for the Ingest screen: its takeaways, the pages
/// written from it, what waits (ready rulings, drafted tickets, held edits)
/// and the commit that holds it.
#[tauri::command(async)]
fn ingest_card(app: AppHandle, state: State<SharedState>, item_id: i64) -> CmdResult<IngestCardDto> {
    let id = inbox_member(&app, &state)?;
    // Cloned under the lock, read after it is released (the Inbox reads
    // every card that has something waiting).
    let (root, search_db) = {
        let guard = lock_tolerant(&state);
        let active = member(&guard, Some(id))?;
        (active.project.root.clone(), active.search_db.clone())
    };
    let db = lock_tolerant(&search_db);
    let item = db.get_review_item(item_id).map_err(err)?.ok_or("no ingest card with that id")?;
    let card = ken_core::ingest::card_of(item.payload.as_deref()).ok_or("the card has no record of where things went")?;
    let text = std::fs::read_to_string(root.join(&card.placement.note)).unwrap_or_default();
    let proposals = ken_core::ingest::proposals_from(&db, &card.placement.note).map_err(err)?.into_iter().map(proposal_dto).collect();
    drop(db);
    let writing = INGEST_READING
        .lock()
        .unwrap()
        .get(&id)
        .is_some_and(|(raw, phase)| *phase == ken_core::ingest::PassPhase::Writing && *raw == card.placement.raw);
    // A source read before sources were claimed waits in Raw/ until Seen.
    let source = if root.join(&card.placement.raw).is_file() { card.placement.raw.clone() } else { card.placement.source.clone() };
    Ok(IngestCardDto {
        id: item.id,
        open: item.status == "open",
        source,
        takeaways: ken_core::ingest::takeaways(&card.placement.note, &text),
        pages: card.pages.iter().map(|p| IngestPageDto { path: p.path.clone(), label: p.label.clone(), new: p.new }).collect(),
        proposals,
        commit: card.commit.clone(),
        commit_error: card.commit_error.clone(),
        writing,
        note: card.placement.note.clone(),
    })
}

/// Files dropped on the Ingest screen: each is copied into the library's
/// `Research/Ingestion/Raw/` (a name already there gets a number) and the
/// read starts. Returns the paths in Raw.
#[tauri::command(async)]
fn ingest_add(app: AppHandle, state: State<SharedState>, paths: Vec<String>) -> CmdResult<Vec<String>> {
    let id = inbox_member(&app, &state)?;
    let (project, base) = {
        let guard = state.lock().unwrap();
        (member(&guard, Some(id))?.project.clone(), guard.base_dir.clone())
    };
    let others = code_sources_of(&state, &project.root);
    let mut added = Vec::new();
    for path in &paths {
        let from = std::path::Path::new(path);
        if !from.is_file() {
            return Err(format!("{path} is not a file"));
        }
        let name = from.file_name().map(|n| n.to_string_lossy().to_string()).ok_or("a file with no name")?;
        let target = raw_target(&project.root, &name)?;
        std::fs::copy(from, &target).map_err(|e| format!("could not copy {name}: {e}"))?;
        added.push(format!("{}/{}", ken_core::ingest::RAW, target.file_name().unwrap().to_string_lossy()));
    }
    if ken_core::runner::discover_claude().is_none() {
        return Err(ken_core::runner::MISSING_CLAUDE_HELP.into());
    }
    // The watcher indexes the new files; the read starts now, not at the
    // next scan.
    start_ingest_pass(&project, &base, others, true);
    Ok(added)
}

/// Where a file named `name` goes in a library's Raw/: its own name, or with
/// a number when that is taken (`ingest::raw_target`, which the MCP
/// `add_source` shares).
fn raw_target(root: &std::path::Path, name: &str) -> CmdResult<std::path::PathBuf> {
    ken_core::ingest::raw_target(root, name).map_err(err)
}

/// `%XX` decoded (what the frontend's encodeURIComponent wrote), as UTF-8.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Some(v) = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A file dropped on the Ingest screen, as its bytes (a drop in the window
/// carries no path): the request body is the file, the `x-name` header its
/// name, URI-encoded. Written into the knowledge base's Raw/ and the read
/// starts. Returns its path in Raw.
#[tauri::command(async)]
fn ingest_add_bytes(app: AppHandle, state: State<SharedState>, request: tauri::ipc::Request<'_>) -> CmdResult<String> {
    let header = |key: &str| -> Option<String> {
        request
            .headers()
            .get(key)
            .and_then(|v| v.to_str().ok())
            .map(percent_decode)
            .filter(|v| !v.is_empty())
    };
    let name = header("x-name").ok_or("the dropped file has no name")?;
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err("the dropped file came without its bytes".into());
    };
    let id = inbox_member(&app, &state)?;
    let (project, base) = {
        let guard = state.lock().unwrap();
        (member(&guard, Some(id))?.project.clone(), guard.base_dir.clone())
    };
    let target = raw_target(&project.root, &name)?;
    std::fs::write(&target, bytes).map_err(|e| format!("could not write {name}: {e}"))?;
    if ken_core::runner::discover_claude().is_none() {
        return Err(ken_core::runner::MISSING_CLAUDE_HELP.into());
    }
    start_ingest_pass(&project, &base, code_sources_of(&state, &project.root), true);
    Ok(format!("{}/{}", ken_core::ingest::RAW, target.file_name().unwrap().to_string_lossy()))
}

/// `YYYY-MM-DD HH.MM`, local: how a source Ken writes into an inbox is named.
fn local_source_stamp() -> String {
    chrono::Local::now().format("%Y-%m-%d %H.%M").to_string()
}

/// Write a source Ken made (a note, a chat) into the library's Raw/ and
/// start the read. Returns its path in Raw.
fn add_source(app: &AppHandle, state: &SharedState, name: &str, text: &str) -> CmdResult<String> {
    let id = inbox_member(app, state)?;
    let (project, base) = {
        let guard = state.lock().unwrap();
        (member(&guard, Some(id))?.project.clone(), guard.base_dir.clone())
    };
    let target = raw_target(&project.root, name)?;
    std::fs::write(&target, text).map_err(|e| format!("could not write {name}: {e}"))?;
    if ken_core::runner::discover_claude().is_none() {
        return Err(ken_core::runner::MISSING_CLAUDE_HELP.into());
    }
    start_ingest_pass(&project, &base, code_sources_of(state, &project.root), true);
    Ok(format!("{}/{}", ken_core::ingest::RAW, target.file_name().unwrap().to_string_lossy()))
}

/// Write a note on the Ingest screen: it lands in the library's Raw/ as
/// `YYYY-MM-DD HH.MM Note - <title>.md` (`kind: note`, by me) and is read
/// at once. Returns its path in Raw.
#[tauri::command(async)]
fn ingest_add_text(app: AppHandle, state: State<SharedState>, title: Option<String>, text: String) -> CmdResult<String> {
    if text.trim().is_empty() {
        return Err("Write the note first.".into());
    }
    let me = cached_git_me();
    let (name, doc) = ken_core::ingest::note_source(&local_source_stamp(), title.as_deref(), &text, me.name.as_deref().or(me.email.as_deref()));
    add_source(&app, state.inner(), &name, &doc)
}

/// Send a chat to Ingest, whole or the messages named: the person's and
/// Ken's turns only, as **Me** and **Ken**, into the library's Raw/ as
/// `YYYY-MM-DD HH.MM Chat - <chat title>.md` (`kind: session`), read at
/// once. `project_id` is the repo the chat is kept in. Returns its path in
/// Raw.
#[tauri::command(async)]
fn ingest_add_chat(
    app: AppHandle,
    state: State<SharedState>,
    project_id: String,
    chat_id: String,
    message_ids: Option<Vec<i64>>,
) -> CmdResult<String> {
    let pid: uuid::Uuid = project_id.parse().map_err(err)?;
    let (chat, messages) = {
        let guard = state.lock().unwrap();
        match guard.members.get(&pid) {
            Some(m) => {
                let db = m.chat_db.lock().unwrap();
                (db.get_chat(&chat_id).map_err(err)?, db.chat_messages(&chat_id).map_err(err)?)
            }
            None => {
                let db = Db::open_read_only(&guard.base_dir, pid).map_err(err)?;
                (db.get_chat(&chat_id).map_err(err)?, db.chat_messages(&chat_id).map_err(err)?)
            }
        }
    };
    let chat = chat.ok_or("That chat is not there any more.")?;
    let turns: Vec<ken_core::ingest::ChatTurn> = messages
        .into_iter()
        .filter(|m| message_ids.as_ref().is_none_or(|ids| ids.contains(&m.id)))
        .map(|m| ken_core::ingest::ChatTurn { role: m.role, content: m.content })
        .collect();
    let me = cached_git_me();
    let (name, doc) = ken_core::ingest::chat_source(&local_source_stamp(), &chat.title, &chat.id, me.name.as_deref().or(me.email.as_deref()), &turns)
        .ok_or("Nothing to send: pick a message from you or from Ken.")?;
    add_source(&app, state.inner(), &name, &doc)
}

/// The library's source at `path` (in Raw/), checked to be one.
fn raw_source(root: &Path, path: &str) -> CmdResult<String> {
    let rel = path.replace('\\', "/");
    if !ken_core::ingest::waiting(root).contains(&rel) {
        return Err(format!("{rel} is not in Raw/ any more."));
    }
    Ok(rel)
}

/// Try a source that could not be read again: its failure is cleared and a
/// read starts. Returns whether one started.
#[tauri::command(async)]
fn ingest_retry(app: AppHandle, state: State<SharedState>, path: String) -> CmdResult<bool> {
    let id = inbox_member(&app, &state)?;
    let (project, base) = {
        let mut guard = state.lock().unwrap();
        let base = guard.base_dir.clone();
        let active = member_mut(&mut guard, Some(id))?;
        let rel = raw_source(&active.project.root, &path)?;
        ken_core::ingest::resolve_failure(&mut active.db, &rel, engine::now_epoch()).map_err(err)?;
        (active.project.clone(), base)
    };
    if ken_core::runner::discover_claude().is_none() {
        return Err(ken_core::runner::MISSING_CLAUDE_HELP.into());
    }
    let others = code_sources_of(&state, &project.root);
    Ok(start_ingest_pass(&project, &base, others, true))
}

/// Take a source out of Raw/: the file goes to the trash (never a hard
/// delete) and its failure is cleared. A source an older card still holds
/// is filed with Seen instead.
#[tauri::command]
async fn ingest_remove(app: AppHandle, state: State<'_, SharedState>, path: String) -> CmdResult<()> {
    let id = inbox_member(&app, &state)?;
    let (abs, rel) = {
        let guard = state.lock().unwrap();
        let active = member(&guard, Some(id))?;
        let rel = raw_source(&active.project.root, &path)?;
        if ken_core::ingest::in_review(&active.project.root, &active.db).map_err(err)?.contains(&rel) {
            return Err("This source has been read. Mark it Seen to file it beside its note.".into());
        }
        (active.project.resolve(&rel).map_err(err)?, rel)
    };
    let to_trash = abs.clone();
    tauri::async_runtime::spawn_blocking(move || trash_path(&to_trash)).await.map_err(|e| e.to_string())??;
    let mut guard = state.lock().unwrap();
    let active = member_mut(&mut guard, Some(id))?;
    let now = engine::now_epoch();
    ken_core::ingest::resolve_failure(&mut active.db, &rel, now).map_err(err)?;
    let _ = deindex_removed(&active.project, &mut active.db, &rel, false);
    Ok(())
}

/// Read what waits in Raw/ now, whatever the repo's kind.
#[tauri::command(async)]
fn ingest_now(app: AppHandle, state: State<SharedState>) -> CmdResult<bool> {
    let id = inbox_member(&app, &state)?;
    let guard = state.lock().unwrap();
    let active = member(&guard, Some(id))?;
    if ken_core::runner::discover_claude().is_none() {
        return Err(ken_core::runner::MISSING_CLAUDE_HELP.into());
    }
    let others = code_sources(&guard, &active.project.root);
    Ok(start_ingest_pass(&active.project, &guard.base_dir, others, true))
}

/// Done with an ingest card: it is marked seen. A source an older card
/// still holds in Raw/ moves beside its note first; a source read now was
/// claimed as its note was written. Returns where the source is.
#[tauri::command(async)]
fn ingest_seen(app: AppHandle, state: State<SharedState>, item_id: i64) -> CmdResult<String> {
    let id = inbox_member(&app, &state)?;
    let mut guard = state.lock().unwrap();
    let active = member_mut(&mut guard, Some(id))?;
    let item = active
        .db
        .list_open_review_items()
        .map_err(err)?
        .into_iter()
        .find(|it| it.id == item_id && it.kind == ken_core::ingest::REVIEW_KIND)
        .ok_or("no open ingest card with that id")?;
    let card = ken_core::ingest::card_of(item.payload.as_deref()).ok_or("the card has no record of where things went")?;
    if active.project.root.join(&card.placement.raw).is_file() {
        ken_core::ingest::claim(&active.project.root, &card.placement).map_err(err)?;
    }
    active.db.resolve_review_item(item_id, engine::now_epoch()).map_err(err)?;
    Ok(card.placement.source)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceDto {
    id: String,
    name: String,
    path: String,
    kind: Vec<ken_core::registry::RepoKind>,
    /// `off` when a person switched the whole repo off, else None.
    index: Option<ken_core::registry::IndexState>,
    /// Indexed or off (knowledge-core decision 34).
    effective_index: ken_core::registry::IndexState,
    description: String,
    available: bool,
    /// The branch checked out, when it is a git repo.
    branch: Option<String>,
    /// Its commit, short.
    head: Option<String>,
    /// Commits its upstream has that it does not, as of the last fetch.
    behind: Option<u32>,
    files: i64,
    /// The one health line (decision 19), None when it has no index yet.
    health: Option<String>,
    /// What `.ken/knowledge.json` calls it: its `repos` key, a control's
    /// `source`.
    settings_name: String,
    /// Its role rules (decision 17) as `pattern = role` lines.
    roles: Vec<String>,
    /// Whether "Analyse again" is offered (Chris, 2026-10-14): its kind is
    /// said, a knowledge base lists it, and its settings are not a code
    /// repo's own file, where Ken writes nothing (decision 36).
    can_analyze_again: bool,
    /// Its analysis, asked for again, is queued or running.
    analyzing: bool,
    /// A repo analysis for it waits in the Inbox.
    analysis_waiting: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GapDto {
    /// The repo it is about, when it is about one.
    repo: Option<String>,
    text: String,
    /// A path to open to see or fix it.
    open: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PageDto {
    path: String,
    title: String,
}

/// One control (decision 19) with how it did on its source's last run.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlRowDto {
    question: String,
    expect: String,
    /// The source it belongs to, by its settings name; None is the
    /// knowledge base itself.
    source: Option<String>,
    /// Whether its source has run it yet.
    ran: bool,
    /// Where its file came, from 1, on the last run.
    rank: Option<usize>,
    passed: bool,
}

/// The workspace settings screen (knowledge-core decision 37): what used to
/// be the Team screen, for the one workspace and its knowledge base.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceSettingsDto {
    workspace: String,
    /// The folder the workspace lives in (what Scan again reads).
    root: String,
    repos: Vec<SourceDto>,
    /// The knowledge base, when the workspace has one.
    wiki: Option<SourceDto>,
    gaps: Vec<GapDto>,
    /// The index rules every source reads, from the knowledge base's
    /// `.ken/knowledge.json` (knowledge-core decision 34).
    ignores: Vec<String>,
    /// The older workspace `.kenignore` lines, while a source still reads
    /// them (until they move into `.ken/knowledge.json`); never written.
    legacy_ignores: Vec<String>,
    /// The tier overrides (`search.weights`, decision 23) as
    /// `pattern = tier` lines.
    weights: Vec<String>,
    /// Every control in `.ken/knowledge.json`, with its last result.
    controls: Vec<ControlRowDto>,
    /// Running set-up again: the template files the knowledge base lacks,
    /// and the ones Ken owns that differ from its template, as a diff.
    template_gaps: Vec<ken_core::wikinew::TemplateGap>,
    /// The knowledge base's last drift run.
    sweep: Option<ken_core::drift::DriftRun>,
    /// What waits on a person in the knowledge base: the one findings list
    /// (drift, lint, ingest, supersession).
    findings: Vec<FindingDto>,
    rules: Vec<PageDto>,
    templates: Vec<PageDto>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FindingDto {
    /// The finding's kind (`ken_core::findings::Finding::kind`):
    /// `contradiction` · `question` · `drift` · `broken-link` ·
    /// `missing-page` · `ambiguous-name` · `orphan` · `suggested-page` ·
    /// `unknown-ruling` · `supersession`.
    kind: String,
    title: String,
    detail: String,
    /// The page to open, in the repo `project_id` names; empty when the
    /// finding is about the sweep itself.
    path: String,
    project_id: String,
    /// The stored finding's id, for `resolve_review_item` (Mark as done)
    /// and `confirm_finding`.
    item_id: Option<i64>,
}

/// The knowledge base's one findings list (`ken_core::findings`), newest
/// first, each marked done once seen to. Drift, the lint and the
/// supersession pass file theirs as they run; nothing is worked out here.
/// The title and the detail are read from the finding, not the stored
/// row, so one filed before titles were cut at a word reads the same.
fn kb_findings(wiki_id: &str, db: &Db) -> Vec<FindingDto> {
    ken_core::findings::open(db)
        .unwrap_or_default()
        .into_iter()
        .map(|(it, f)| FindingDto {
            path: f.page(),
            title: f.title(),
            detail: f.body(),
            kind: f.kind,
            project_id: wiki_id.to_string(),
            item_id: Some(it.id),
        })
        .collect()
}

/// The findings for the Inbox and the sidebar's count: the knowledge base's
/// findings list, without the rest of the settings screen.
#[tauri::command(async)]
fn knowledge_findings(state: State<SharedState>) -> CmdResult<Vec<FindingDto>> {
    let found = {
        let guard = lock_tolerant(&state);
        inbox_project(&guard).and_then(|p| guard.members.get(&p.config.id).map(|m| (p.config.id, m.search_db.clone())))
    };
    Ok(match found {
        Some((id, db)) => kb_findings(&id.to_string(), &lock_tolerant(&db)),
        None => Vec::new(),
    })
}

fn git_line(root: &std::path::Path, args: &[&str]) -> Option<String> {
    let mut cmd = std::process::Command::new("git");
    let out = ken_core::proc::quiet(&mut cmd).args(args).current_dir(root).env("GIT_TERMINAL_PROMPT", "0").output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|s| !s.is_empty())
}

/// A wiki page's title: its frontmatter `title`, else its first heading,
/// else its file name. A title or heading that is still a placeholder (a
/// blank in `Templates/`, `# {{Title}}`) is not one, so a blank shows by its
/// file name.
fn page_title(root: &std::path::Path, rel: &str) -> String {
    let text = std::fs::read_to_string(root.join(rel)).unwrap_or_default();
    text.lines()
        .find_map(|l| l.trim_start().strip_prefix("title:").map(|t| t.trim().trim_matches('"').to_string()))
        .filter(|t| !t.contains("{{") && !t.is_empty())
        .or_else(|| text.lines().find_map(|l| l.strip_prefix("# ").map(|t| t.trim().to_string())).filter(|t| !t.contains("{{") && !t.is_empty()))
        .unwrap_or_else(|| rel.rsplit('/').next().unwrap_or(rel).trim_end_matches(".md").to_string())
}

/// The `.md` pages directly in `dir` of `root`, titled, the copy template a
/// wiki laid down before the 2026-10 template kept among its rules
/// (`RULE.md`) left out.
fn pages_in(root: &std::path::Path, dir: &str) -> Vec<PageDto> {
    let mut out: Vec<PageDto> = std::fs::read_dir(root.join(dir))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            (name.ends_with(".md") && name != "RULE.md" && name != "Index.md").then(|| format!("{dir}/{name}"))
        })
        .map(|path| PageDto { title: page_title(root, &path), path })
        .collect();
    out.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
    out
}

/// Run `f` on a repo's index: a resident repo through its read connection
/// (held for `f` only), a dormant one through a read-only open. None when it
/// has no index yet.
fn with_index<T>(
    dbs: &std::collections::HashMap<uuid::Uuid, Arc<Mutex<Db>>>,
    base_dir: &Path,
    id: uuid::Uuid,
    f: impl Fn(&Db) -> T,
) -> Option<T> {
    match dbs.get(&id) {
        Some(d) => Some(f(&lock_tolerant(d))),
        None if db_path(base_dir, id).exists() => Db::open_read_only(base_dir, id).ok().map(|db| f(&db)),
        None => None,
    }
}

/// Each control with its last result: a control's source is the source
/// whose settings name it gives (the knowledge base when it gives none),
/// and its result is that source's stored run of the same question and file.
fn control_rows(
    controls: Vec<ken_core::controls::Control>,
    sources: &[(String, Vec<ken_core::controls::ControlResult>)],
    kb_name: Option<&str>,
) -> Vec<ControlRowDto> {
    controls
        .into_iter()
        .map(|c| {
            let owner = c.source.as_deref().or(kb_name);
            let result = owner
                .and_then(|o| sources.iter().find(|(name, _)| name.eq_ignore_ascii_case(o)))
                .and_then(|(_, results)| results.iter().find(|r| r.question == c.question && r.expect == c.expect));
            ControlRowDto {
                ran: result.is_some(),
                rank: result.and_then(|r| r.rank),
                passed: result.is_some_and(|r| r.passed),
                question: c.question,
                expect: c.expect,
                source: c.source,
            }
        })
        .collect()
}

/// The workspace settings screen (knowledge-core decision 37): every source
/// as set-up wrote it (kind, indexed or off, branch, commit, how far behind
/// its upstream, its health line and role rules), what each lacks for its
/// kind, the index rules, the tiers and the controls in the knowledge base's
/// `.ken/knowledge.json`, the knowledge base's last drift run and findings,
/// and its rules and templates.
#[tauri::command(async)]
fn workspace_settings(state: State<SharedState>) -> CmdResult<WorkspaceSettingsDto> {
    // Cloned under the lock; the registry, the indexes and git are read
    // after it is released.
    let (ws, base_dir, dbs) = {
        let guard = lock_tolerant(&state);
        let ws = guard.workspace.as_ref().ok_or("no workspace open")?.ws.clone();
        let dbs: std::collections::HashMap<uuid::Uuid, Arc<Mutex<Db>>> =
            guard.members.iter().map(|(id, m)| (*id, m.search_db.clone())).collect();
        (ws, guard.base_dir.clone(), dbs)
    };
    let registry = Registry::load(&base_dir).map_err(err)?;
    let model_missing = ken_core::embedder::selected_profile().is_none();
    let mut repos: Vec<SourceDto> = Vec::new();
    // Each source's stored control results, by its settings name.
    let mut results: Vec<(String, Vec<ken_core::controls::ControlResult>)> = Vec::new();
    for m in &ws.members {
        let ken_core::workspace::MemberStatus::Ok(p) = &m.status else { continue };
        let entry = registry.entry_at(&p.root);
        let kind = entry.map(|e| e.kind.clone()).unwrap_or_default();
        let index = entry.and_then(|e| e.index);
        let git = p.root.join(".git").exists();
        let (files, health, stored) =
            with_index(&dbs, &base_dir, p.config.id, |db| (db.file_count().unwrap_or(-1), db.index_health().ok(), ken_core::controls::stored(db)))
                .unwrap_or((-1, None, Vec::new()));
        let settings = ken_core::knowledge_settings::for_repo(&p.root);
        let settings_name =
            settings.as_ref().map(|s| s.name.clone()).unwrap_or_else(|| ken_core::knowledge_settings::repo_name(&p.root));
        let can_analyze_again =
            !kind.is_empty() && p.root.is_dir() && settings.as_ref().is_some_and(ken_core::repo_analysis::writable);
        results.push((settings_name.clone(), stored));
        repos.push(SourceDto {
            id: p.config.id.to_string(),
            name: m.name.clone(),
            path: p.root.display().to_string(),
            effective_index: index.unwrap_or(ken_core::registry::IndexState::Indexed),
            kind,
            index,
            description: entry.and_then(|e| e.description.clone()).unwrap_or_default(),
            available: p.root.is_dir(),
            // Filled in below, the repos side by side: three git calls per
            // repo.
            branch: git.then(String::new),
            head: None,
            behind: None,
            files,
            health: health.filter(|h| h.files > 0).map(|mut h| {
                h.model_missing = model_missing;
                h.line()
            }),
            settings_name,
            roles: Vec::new(),
            can_analyze_again,
            analyzing: ANALYSING.lock().unwrap_or_else(|e| e.into_inner()).contains(&p.config.id),
            analysis_waiting: false,
        });
    }
    let is_wiki = |r: &SourceDto| r.kind.contains(&ken_core::registry::RepoKind::Wiki);
    let wiki_index = repos.iter().position(is_wiki);

    let mut gaps: Vec<GapDto> = Vec::new();
    if wiki_index.is_none() {
        gaps.push(GapDto {
            repo: None,
            text: "This workspace has no knowledge base. Set-up makes one from Ken's template, or lays it over an existing wiki.".into(),
            open: None,
        });
    }
    for r in &repos {
        let root = std::path::Path::new(&r.path);
        if !r.available {
            gaps.push(GapDto { repo: Some(r.name.clone()), text: format!("{}'s folder is not there any more.", r.name), open: None });
            continue;
        }
        if r.kind.contains(&ken_core::registry::RepoKind::Code) && !["CLAUDE.md", "AGENTS.md"].iter().any(|f| root.join(f).exists()) {
            gaps.push(GapDto {
                repo: Some(r.name.clone()),
                text: format!("{} has no CLAUDE.md or AGENTS.md, so an agent starts in it without its conventions.", r.name),
                open: None,
            });
        }
        if r.kind.is_empty() {
            gaps.push(GapDto { repo: Some(r.name.clone()), text: format!("Nobody has said what {} is: pick its kind.", r.name), open: None });
        }
    }
    let mut wiki = wiki_index.map(|i| repos.remove(i));
    let mut sweep = None;
    let mut findings = Vec::new();
    // The sources a repo analysis waits for in the knowledge base's Inbox.
    let mut waiting_analyses: Vec<String> = Vec::new();
    let (mut rules, mut templates) = (Vec::new(), Vec::new());
    let (mut ignores, mut weights, mut controls, mut template_gaps) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    if let Some(w) = &wiki {
        let root = std::path::Path::new(&w.path);
        if let Ok(id) = w.id.parse::<uuid::Uuid>() {
            if let Some(d) = dbs.get(&id) {
                let db = lock_tolerant(d);
                sweep = db.last_drift_run().ok().flatten();
                findings = kb_findings(&w.id, &db);
                waiting_analyses = ken_core::repo_analysis::open(&db).map(|o| o.into_iter().map(|(_, p)| p.source).collect()).unwrap_or_default();
            }
        }
        // The older layout kept the rules in Ways-of-Working/Rules.
        rules = pages_in(root, "Rules");
        if rules.is_empty() {
            rules = pages_in(root, "Ways-of-Working/Rules");
        }
        templates = pages_in(root, "Templates");
        ignores = ken_core::knowledge_settings::ignore_lines(root);
        weights = ken_core::knowledge_settings::weight_lines(root);
        controls = control_rows(ken_core::knowledge_settings::all_controls(root), &results, Some(&w.settings_name));
        template_gaps = ken_core::wikinew::template_gaps(root, &ws.config.name, &local_date_today());
    }
    if let Some(root) = wiki.as_ref().map(|w| PathBuf::from(&w.path)) {
        for r in repos.iter_mut().chain(wiki.iter_mut()) {
            r.roles = ken_core::knowledge_settings::role_lines(&root, &r.settings_name);
            r.analysis_waiting = waiting_analyses.iter().any(|s| s.eq_ignore_ascii_case(&r.settings_name));
        }
    }
    // The older workspace `.kenignore`, while a source still reads it: each
    // source's lines move into `.ken/knowledge.json` at its first scan.
    let still_read = repos
        .iter()
        .chain(wiki.iter())
        .any(|r| !ken_core::knowledge_settings::for_repo(std::path::Path::new(&r.path)).is_some_and(|s| s.ignores_moved()));
    let legacy_ignores = std::fs::read_to_string(ws.root.join(".kenignore"))
        .ok()
        .filter(|_| still_read)
        .map(|t| t.lines().map(str::to_string).filter(|l| !l.trim().is_empty() && !l.starts_with('#')).collect())
        .unwrap_or_default();
    let workspace = ws.config.name.clone();
    let ws_root = ws.root.display().to_string();

    // Each repo's branch, commit and how far behind its upstream, the repos
    // side by side.
    std::thread::scope(|s| {
        for r in repos.iter_mut().chain(wiki.iter_mut()) {
            if r.branch.is_none() {
                continue;
            }
            s.spawn(move || {
                let root = std::path::PathBuf::from(&r.path);
                r.branch = git_line(&root, &["rev-parse", "--abbrev-ref", "HEAD"]);
                r.head = git_line(&root, &["rev-parse", "--short=8", "HEAD"]);
                r.behind = git_line(&root, &["rev-list", "--count", "HEAD..@{u}"]).and_then(|n| n.parse().ok());
            });
        }
    });
    for r in repos.iter().chain(wiki.iter()) {
        if let Some(n) = r.behind.filter(|n| *n > 0) {
            gaps.push(GapDto {
                repo: Some(r.name.clone()),
                text: format!("{} is {n} commit{} behind its upstream: pull it so Ken reads what is merged.", r.name, if n == 1 { "" } else { "s" }),
                open: None,
            });
        }
    }
    Ok(WorkspaceSettingsDto {
        workspace,
        root: ws_root,
        repos,
        wiki,
        gaps,
        ignores,
        legacy_ignores,
        weights,
        controls,
        template_gaps,
        sweep,
        findings,
        rules,
        templates,
    })
}

/// The workspace's knowledge base and the open repos, for a settings save.
fn kb_and_open(state: &SharedState) -> CmdResult<((String, uuid::Uuid, PathBuf), PathBuf, Vec<Project>)> {
    let guard = state.lock().unwrap();
    let kb = knowledge_base_of(&workspace_repos(&guard), None).ok_or("this workspace has no knowledge base to keep the settings in")?;
    let open: Vec<Project> = guard.members.values().map(|m| m.project.clone()).collect();
    Ok((kb, guard.base_dir.clone(), open))
}

/// Write the index rules every source reads: the top-level `ignore` lines of
/// the knowledge base's `.ken/knowledge.json`, the one place they live
/// (knowledge-core decision 34). Every open repo is scanned again, so a path
/// newly left out leaves the index and one brought back is read.
#[tauri::command(async)]
fn workspace_save_ignores(app: AppHandle, state: State<SharedState>, lines: Vec<String>) -> CmdResult<()> {
    let (kb, base, open) = kb_and_open(&state)?;
    ken_core::knowledge_settings::set_ignore_lines(&kb.2, &lines).map_err(err)?;
    for p in &open {
        rescan_member(&app, &base, p);
    }
    Ok(())
}

/// Write the tier overrides (`search.weights`, decision 23) from
/// `pattern = tier` lines. The knowledge base is scanned again so its pages
/// take their new tiers.
#[tauri::command(async)]
fn workspace_save_weights(app: AppHandle, state: State<SharedState>, lines: Vec<String>) -> CmdResult<()> {
    let (kb, base, open) = kb_and_open(&state)?;
    ken_core::knowledge_settings::set_weight_lines(&kb.2, &lines).map_err(err)?;
    if let Some(p) = open.iter().find(|p| p.config.id == kb.1) {
        rescan_member(&app, &base, p);
    }
    Ok(())
}

/// Write one source's role rules (decision 17) from `pattern = role` lines,
/// under its `repos` entry in the knowledge base. The source is scanned again
/// so its files take their roles.
#[tauri::command(async)]
fn workspace_save_roles(app: AppHandle, state: State<SharedState>, project_id: String, name: String, lines: Vec<String>) -> CmdResult<()> {
    let (kb, base, open) = kb_and_open(&state)?;
    ken_core::knowledge_settings::set_role_lines(&kb.2, &name, &lines).map_err(err)?;
    let id: uuid::Uuid = project_id.parse().map_err(err)?;
    if let Some(p) = open.iter().find(|p| p.config.id == id) {
        rescan_member(&app, &base, p);
    }
    Ok(())
}

/// Write the knowledge base's controls (decision 19). Each open source's
/// meaning index catches up, which runs its controls again; a source with no
/// meaning index runs them at its next scan.
#[tauri::command(async)]
fn workspace_save_controls(app: AppHandle, state: State<SharedState>, controls: Vec<ken_core::controls::Control>) -> CmdResult<()> {
    let (kb, _, open) = kb_and_open(&state)?;
    ken_core::knowledge_settings::set_controls(&kb.2, &controls).map_err(err)?;
    for p in &open {
        queue_meaning_catch_up(&app, p.config.id);
    }
    Ok(())
}

/// Lay down the template files the knowledge base lacks that a person
/// chose (running set-up again, decision 4): never over a file it has, and
/// committed by name. Returns the files written.
#[tauri::command(async)]
fn workspace_add_template_files(state: State<SharedState>, paths: Vec<String>) -> CmdResult<Vec<String>> {
    let (kb, name) = {
        let guard = state.lock().unwrap();
        let kb = knowledge_base_of(&workspace_repos(&guard), None).ok_or("this workspace has no knowledge base")?;
        let name = guard.workspace.as_ref().map(|w| w.ws.config.name.clone()).unwrap_or_else(|| kb.0.clone());
        (kb, name)
    };
    ken_core::wikinew::add_missing(&kb.2, &name, &paths, &local_date_today()).map_err(err)
}

/// A new rule in the knowledge base's Rules/, from its own
/// rule template, named as the rule. Returns its path, to open and write.
#[tauri::command(async)]
fn workspace_add_rule(state: State<SharedState>, wiki_id: String, rule: String) -> CmdResult<String> {
    let guard = state.lock().unwrap();
    let id: uuid::Uuid = wiki_id.parse().map_err(err)?;
    let root = guard.members.get(&id).map(|m| m.project.root.clone()).ok_or("the knowledge base is not open")?;
    let rule = rule.trim();
    if rule.is_empty() {
        return Err("say the rule as a sentence".into());
    }
    let slug: String = rule
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let rel = format!("Rules/{slug}.md");
    let path = root.join(&rel);
    if path.exists() {
        return Err(format!("{rel} is already there"));
    }
    // `Templates/Rule.md`; a wiki laid down before the 2026-10 template kept
    // its blank beside the rules, as `RULE.md`.
    let template = std::fs::read_to_string(root.join("Templates/Rule.md"))
        .or_else(|_| std::fs::read_to_string(root.join("Ways-of-Working/Rules/RULE.md")))
        .unwrap_or_else(|_| "---\ntitle: \"{{the-rule-as-a-sentence}}\"\nstatus: current\n---\n\n# {{The rule, as a sentence}}\n".into());
    let text = template
        .replace("{{the-rule-as-a-sentence}}", rule)
        .replace("{{The Rule as a Sentence}}", rule)
        .replace("{{The rule, as a sentence}}", rule)
        .replace("{{date}}", &local_date_today());
    std::fs::create_dir_all(path.parent().unwrap()).map_err(err)?;
    std::fs::write(&path, text).map_err(err)?;
    Ok(rel)
}

/// Run drift now, whether or not a run is due or the repo is a knowledge
/// base: the sweep, what it found acted on, and the code citations checked.
#[tauri::command]
async fn run_drift_now(state: State<'_, SharedState>, project_id: Option<String>) -> CmdResult<Option<ken_core::drift::DriftRun>> {
    let id = project_id.as_deref().map(|p| p.parse::<uuid::Uuid>()).transpose().map_err(err)?;
    let (base, project) = {
        let guard = state.lock().unwrap();
        let active = member(&guard, id)?;
        (guard.base_dir.clone(), active.project.clone())
    };
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut db = Db::open(&base, project.config.id).map_err(err)?;
        Ok(run_drift(&state, &project, &mut db, true, &Arc::new(AtomicBool::new(false))))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The page a `[[link]]` in `from` names, by file name or by an alias in a
/// page's frontmatter; the one in `from`'s folder when two pages claim it.
#[tauri::command(async)]
fn resolve_page_link(state: State<SharedState>, from: String, target: String) -> CmdResult<Option<String>> {
    let search_db = {
        let guard = lock_tolerant(&state);
        member(&guard, None)?.search_db.clone()
    };
    let db = lock_tolerant(&search_db);
    let resolver = ken_core::links::Resolver::from_db(&db).map_err(err)?;
    let name = target.split('|').next().unwrap_or(&target).split('#').next().unwrap_or("").trim().to_string();
    let link = ken_core::links::Link {
        kind: if name.contains('/') { ken_core::links::LinkKind::Path } else { ken_core::links::LinkKind::Name },
        target: name,
    };
    let found = resolver.resolve(&link);
    let dir = from.rsplit_once('/').map(|(d, _)| d.to_lowercase()).unwrap_or_default();
    let near = found.iter().find(|p| p.rsplit_once('/').map(|(d, _)| d.to_lowercase()).unwrap_or_default() == dir);
    Ok(near.or(found.first()).cloned())
}

#[tauri::command(async)]
fn page_links(state: State<SharedState>, path: String) -> CmdResult<ken_core::links::PageLinks> {
    let guard = state.lock().unwrap();
    let active = member(&guard, None)?;
    ken_core::links::page_links(&active.db, &path).map_err(err)
}

/// One neighbour in the project graph, for Files (knowledge-core decision
/// 21): what it is, how it joins, and where it opens.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RelatedDto {
    /// `kind:id`.
    key: String,
    kind: String,
    /// What to show: a file's name, a ruling's id, a topic, a commit.
    label: String,
    edge: String,
    outgoing: bool,
    detail: String,
    hops: u8,
    via: Option<String>,
    /// The member that holds it and its path there, when it opens as a file.
    project_id: Option<String>,
    path: Option<String>,
}

/// Most neighbours `graph_related` returns.
const RELATED_LIMIT: usize = 40;

/// A file's neighbours in the project graph (knowledge-core decision 21):
/// `path` in the member `project_id` (default the focused one). A file of
/// the knowledge base is a page, note or ticket; a file of another repo is
/// a code file. The workspace's indexes are opened for reading, off the
/// lock.
#[tauri::command(async)]
fn graph_related(state: State<SharedState>, path: String, project_id: Option<String>) -> CmdResult<Vec<RelatedDto>> {
    let (base, repos, asked) = {
        let guard = state.lock().unwrap();
        let asked = match project_id.as_deref() {
            Some(id) => uuid::Uuid::parse_str(id).map_err(err)?,
            None => member(&guard, None)?.project.config.id,
        };
        (guard.base_dir.clone(), workspace_repos(&guard), asked)
    };
    let path = path.replace('\\', "/");
    let kb = knowledge_base_of(&repos, Some(asked));
    let node = match (&kb, repos.iter().find(|r| r.1 == asked)) {
        (Some(k), _) if k.1 == asked => ken_core::graph::Node::of_path(&path),
        (_, Some((name, _, _))) => ken_core::graph::Node::code(name, &path),
        _ => return Ok(Vec::new()),
    };
    let kb_db = kb.as_ref().and_then(|k| Db::open_read_only(&base, k.1).ok());
    let code: Vec<ken_core::ingest::CodeSource> = repos
        .iter()
        .filter(|r| kb.as_ref().map(|k| k.1) != Some(r.1))
        .filter_map(|(name, id, _)| Db::open_read_only(&base, *id).ok().map(|db| ken_core::ingest::CodeSource { name: name.clone(), db }))
        .collect();
    let found = ken_core::graph::related(kb_db.as_ref(), &code, &node, RELATED_LIMIT).map_err(err)?;
    let file_name = |p: &str| p.rsplit('/').next().unwrap_or(p).trim_end_matches(".md").to_string();
    Ok(found
        .into_iter()
        .map(|n| {
            let (project_id, path, label) = if let Some(p) = n.node.kb_path() {
                (kb.as_ref().map(|k| k.1.to_string()), Some(p.to_string()), file_name(p))
            } else if let Some((repo, p, symbol)) = n.node.code_place() {
                let id = repos.iter().find(|r| r.0 == repo).map(|r| r.1.to_string());
                (id, Some(p.to_string()), symbol.map(str::to_string).unwrap_or_else(|| file_name(p)))
            } else {
                (None, None, n.node.id.clone())
            };
            RelatedDto {
                key: n.node.key(),
                kind: n.node.kind.clone(),
                label,
                edge: n.edge,
                outgoing: n.outgoing,
                detail: n.detail,
                hops: n.hops,
                via: n.via.map(|v| v.key()),
                project_id,
                path,
            }
        })
        .collect())
}

/// The Log: the project's RAID register and milestones (knowledge-core
/// decision 37), read from the workspace's knowledge base, else the focused
/// repo.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RaidRegisterDto {
    /// The repo it was read from, so an item's file opens there.
    project_id: Option<String>,
    name: Option<String>,
    register: ken_core::raidlog::Register,
}

/// The Log (knowledge-core decision 37): the risks, actions, issues and
/// decisions, and the milestones, each with its status, owner, date and
/// source. Reads git for releases, so it runs off the lock.
#[tauri::command(async)]
fn raid_register(state: State<SharedState>) -> CmdResult<RaidRegisterDto> {
    let (base, repos, focused) = {
        let guard = state.lock().unwrap();
        let focused = member(&guard, None).ok().map(|m| (m.project.config.name.clone(), m.project.config.id, m.project.root.clone()));
        (guard.base_dir.clone(), workspace_repos(&guard), focused)
    };
    let Some((name, id, root)) = knowledge_base_of(&repos, focused.as_ref().map(|f| f.1)).or(focused) else {
        return Ok(RaidRegisterDto { project_id: None, name: None, register: Default::default() });
    };
    let db = Db::open_read_only(&base, id).map_err(err)?;
    let others: Vec<(String, PathBuf)> = repos.into_iter().filter(|r| r.1 != id).map(|(n, _, r)| (n, r)).collect();
    let offset = i64::from(chrono::Local::now().offset().local_minus_utc());
    let register = ken_core::raidlog::register(&db, &root, &others, offset).map_err(err)?;
    Ok(RaidRegisterDto { project_id: Some(id.to_string()), name: Some(name), register })
}

/// A code file's structure for the Files panel: its definitions in order
/// (with nesting depth) and the files it imports and is imported by.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CodeFileDto {
    outline: Vec<CodeOutlineItem>,
    related: ken_core::codeindex::Related,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CodeOutlineItem {
    depth: usize,
    #[serde(flatten)]
    symbol: ken_core::codeindex::Symbol,
}

#[tauri::command(async)]
fn code_file(state: State<SharedState>, path: String) -> CmdResult<CodeFileDto> {
    let guard = state.lock().unwrap();
    let active = member(&guard, None)?;
    let outline = ken_core::codeindex::outline(&active.db, &path)
        .map_err(err)?
        .into_iter()
        .map(|(depth, symbol)| CodeOutlineItem { depth, symbol })
        .collect();
    let related = ken_core::codeindex::related(&active.db, &path).map_err(err)?;
    Ok(CodeFileDto { outline, related })
}

/// One use of a symbol, for the Files panel: where, what kind, in what.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CodeUseDto {
    project_id: String,
    member_name: String,
    path: String,
    line: i64,
    /// `call` (the grammar's kind) or `mention` (named some other way).
    kind: String,
    within: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CodeUsagesDto {
    definitions: Vec<CodeUseDto>,
    uses: Vec<CodeUseDto>,
}

/// "Go to definition" and "find usages" for a symbol, across every project
/// of the open workspace (the focused one alone without a workspace).
#[tauri::command]
async fn code_usages(state: State<'_, SharedState>, name: String) -> CmdResult<CodeUsagesDto> {
    let (base, targets) = {
        let guard = state.lock().unwrap();
        let active = member(&guard, None)?;
        let mut targets: Vec<(uuid::Uuid, String)> = match guard.workspace.as_ref() {
            Some(ws) => ws
                .ws
                .members
                .iter()
                .filter_map(|m| match &m.status {
                    ken_core::workspace::MemberStatus::Ok(p) => Some((p.config.id, m.name.clone())),
                    _ => None,
                })
                .collect(),
            None => Vec::new(),
        };
        if !targets.iter().any(|(id, _)| *id == active.project.config.id) {
            targets.insert(0, (active.project.config.id, active.project.config.name.clone()));
        }
        (guard.base_dir.clone(), targets)
    };
    tauri::async_runtime::spawn_blocking(move || {
        let mut out = CodeUsagesDto { definitions: Vec::new(), uses: Vec::new() };
        for (id, member_name) in targets {
            let Ok(db) = Db::open_read_only(&base, id) else { continue };
            let dto = |path: String, line: i64, kind: String, within: Option<String>| CodeUseDto {
                project_id: id.to_string(),
                member_name: member_name.clone(),
                path,
                line,
                kind,
                within,
            };
            for d in ken_core::codeindex::definitions(&db, &name).map_err(err)? {
                out.definitions.push(dto(d.path, d.line, d.kind, None));
            }
            let calls = ken_core::codeindex::usages(&db, &name, 200).map_err(err)?;
            let known: Vec<(String, i64)> = calls.iter().map(|u| (u.path.clone(), u.line)).collect();
            for u in calls {
                out.uses.push(dto(u.path, u.line, u.kind, u.within));
            }
            for m in ken_core::codeindex::mentions(&db, &name, &known, 200).map_err(err)? {
                out.uses.push(dto(m.path, m.line, m.kind, m.within));
            }
        }
        Ok(out)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RoutePlanDto {
    targets: Vec<String>,
    /// `"named"` (the question named them, or the person chose them) or
    /// `"every"`.
    reason: &'static str,
    /// Reference repos held back: searched only when nothing else answered,
    /// and then listed in `memberStatus` too.
    reserve: Vec<String>,
}

impl From<&routing::RoutePlan> for RoutePlanDto {
    fn from(p: &routing::RoutePlan) -> Self {
        RoutePlanDto {
            targets: p.targets.iter().map(|id| id.to_string()).collect(),
            reason: match p.reason {
                routing::RouteReason::Named => "named",
                routing::RouteReason::Every => "every",
            },
            reserve: p.reserve.iter().map(|id| id.to_string()).collect(),
        }
    }
}

/// One merged, cited hit. Mirrors `routing::RoutedHit`, with `source` as a
/// word.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RoutedHitDto {
    path: String,
    chunk_id: i64,
    snippet: String,
    /// `"keyword"`, `"semantic"` or `"both"`.
    source: &'static str,
    project_id: String,
    member_name: String,
    /// `ken://<project-id>/<rel-path>`.
    address: String,
    /// The line the hit's chunk starts on, when known.
    line: Option<i64>,
    /// `repo:path:line`, `repo@sha:…`, led by `[[Note]]` in a wiki repo.
    locator: String,
    /// For a Markdown page: section, checked or evidence date, and whether
    /// it is superseded.
    page: Option<ken_core::pagemeta::HitPage>,
    /// What the file is for (`contenttype::of`).
    content_type: &'static str,
    /// One label per hit: `kind · tier or role · date`
    /// (`authority::hit_label`).
    label: Option<String>,
    /// For a code hit, the definition it holds (`Store.open`).
    symbol: Option<String>,
}

impl From<routing::RoutedHit> for RoutedHitDto {
    fn from(h: routing::RoutedHit) -> Self {
        let source = match h.source {
            ken_core::search::Source::Keyword => "keyword",
            ken_core::search::Source::Semantic => "semantic",
            ken_core::search::Source::Both => "both",
        };
        RoutedHitDto {
            content_type: ken_core::contenttype::of(&h.path).as_str(),
            path: h.path,
            chunk_id: h.chunk_id,
            snippet: h.snippet,
            source,
            project_id: h.project_id.to_string(),
            member_name: h.member_name,
            address: h.address,
            line: h.line,
            locator: h.locator,
            page: h.page,
            label: h.label,
            symbol: h.symbol,
        }
    }
}

/// How one source's search went in a `route_search` call.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct MemberStatusEntryDto {
    project_id: String,
    member_name: String,
    /// `"searched"`, `"building"` (searched by keyword while its meaning
    /// index is built) or `"unavailable"` (its index would not open).
    status: &'static str,
    /// While building: the passages with a meaning vector so far, of
    /// `chunks`.
    embedded: Option<i64>,
    chunks: Option<i64>,
    /// How long its search took; `slow` from `routing::SLOW_MILLIS`.
    millis: u64,
    slow: bool,
}

impl From<&routing::MemberStatusEntry> for MemberStatusEntryDto {
    fn from(s: &routing::MemberStatusEntry) -> Self {
        let (status, embedded, chunks) = match s.status {
            routing::MemberStatus::Searched => ("searched", None, None),
            routing::MemberStatus::Building { embedded, chunks } => ("building", Some(embedded), Some(chunks)),
            routing::MemberStatus::Unavailable => ("unavailable", None, None),
        };
        MemberStatusEntryDto {
            project_id: s.project_id.to_string(),
            member_name: s.member_name.clone(),
            status,
            embedded,
            chunks,
            millis: s.millis,
            slow: s.slow(),
        }
    }
}

/// `route_search`'s return shape, mirroring `routing::ExecutionReport`.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RouteSearchDto {
    plan: RoutePlanDto,
    results: Vec<RoutedHitDto>,
    member_status: Vec<MemberStatusEntryDto>,
}

impl From<routing::ExecutionReport> for RouteSearchDto {
    fn from(r: routing::ExecutionReport) -> Self {
        RouteSearchDto {
            plan: (&r.plan).into(),
            results: r.results.into_iter().map(Into::into).collect(),
            member_status: r.member_status.iter().map(Into::into).collect(),
        }
    }
}

/// Payload for the `routed-search-state` event, app-global rather than
/// `emit_member`: a routed search spans every source at once, so it has no
/// single owning project. `{"state":"planning"}`,
/// `{"state":"searching","done":1,"total":3}`, `{"state":"done"}`.
#[derive(Clone, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
enum RoutedSearchStateEvent {
    Planning,
    Searching { done: usize, total: usize },
    Done,
}

/// One workspace member's handles needed to plan and run a routed search,
/// snapshotted under the state lock and used after it is dropped, so
/// planning, embedding and searching never hold the global lock.
struct RouteMemberSnapshot {
    project_id: uuid::Uuid,
    name: String,
    /// The member's live handle when it is **resident**; `None` when it is a
    /// manifest member whose runtime is dormant (ken-home-workspace 2.1).
    /// A dormant member is still searchable — its index lives at
    /// `<base>/index/<project-id>.db` and opens by id — so this being
    /// `None` means "open one for the query", not "skip it". Residency
    /// governs runtimes (watcher, engine, extraction worker), not whether
    /// the database can be read.
    search_db: Option<Arc<Mutex<Db>>>,
    /// Only resident members carry a loaded embedder.
    embedder_slot: Option<Arc<Mutex<Option<Box<dyn Embedder + Send>>>>>,
}

/// What every source's search in one `route_search` shares: the open
/// indexes, the names, the query and its vector, the filters and the
/// vocabulary.
struct RouteFanOut {
    app: AppHandle,
    dbs: std::collections::HashMap<uuid::Uuid, Arc<Mutex<Db>>>,
    names: std::collections::HashMap<uuid::Uuid, String>,
    query: String,
    query_vec: Option<Vec<f32>>,
    /// Hits kept from each source.
    limit: usize,
    wanted: Vec<ken_core::contenttype::ContentType>,
    vocab: Arc<ken_core::vocab::Vocabulary>,
    done: Arc<AtomicUsize>,
}

impl RouteFanOut {
    /// Search `ids` at once, each on its own blocking task
    /// (`routing::search_target`), and emit `searching done/total` as each
    /// finishes. A source whose index did not open is reported unavailable.
    async fn run(&self, ids: &[uuid::Uuid], total: usize) -> CmdResult<Vec<routing::MemberHits>> {
        let mut handles = Vec::with_capacity(ids.len());
        for &id in ids {
            let name = self.names.get(&id).cloned().unwrap_or_default();
            let db = self.dbs.get(&id).cloned();
            let (query, query_vec, wanted) = (self.query.clone(), self.query_vec.clone(), self.wanted.clone());
            let (vocab, done, app) = (self.vocab.clone(), self.done.clone(), self.app.clone());
            let limit = self.limit;
            handles.push(tauri::async_runtime::spawn_blocking(move || {
                let guard = db.as_ref().map(|db| db.lock().unwrap());
                let found =
                    routing::search_target(id, &name, guard.as_deref(), &query, query_vec.as_deref(), limit, &wanted, Some(&vocab));
                drop(guard);
                let done = done.fetch_add(1, Ordering::SeqCst) + 1;
                let _ = app.emit("routed-search-state", RoutedSearchStateEvent::Searching { done, total });
                found
            }));
        }
        let mut out = Vec::with_capacity(handles.len());
        for h in handles {
            out.push(h.await.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }
}

/// Search the workspace's sources and return the merged, cited results
/// (knowledge-core decisions 15 and 20). `routing::plan_route` picks the
/// sources: every one, less the reference repos it holds back, or only those
/// the question names. Each is searched at once on the blocking pool, the
/// held-back reference repos only when nothing answered, and
/// `routing::merge_routed` merges the hits once. A source still building its
/// meaning index is searched by keyword and reported; a slow one is waited
/// for and reported. Emits `routed-search-state` (`planning`, `searching
/// m/n`, `done`).
///
/// Sources come from the **workspace manifest** (`Workspace::members`,
/// `MemberStatus::Ok` only). A resident member is searched through its live
/// handle; a dormant one through an index opened for the query and dropped
/// after, which activates nothing. With no workspace open the open projects
/// are the sources.
///
/// `scope` pins the search to one member and `group` to a named group's
/// members: a plan the person chose, with nothing held back. The app's
/// "This repo" search is this with `scope` set to the focused member.
///
/// The query is embedded once, with the focused member's embedder (any
/// resident member's otherwise: every slot loads the same local model), and
/// the vector is used for every source. With every member dormant there is
/// no embedder and the search is keyword only.
#[tauri::command]
async fn route_search(
    app: AppHandle,
    state: State<'_, SharedState>,
    query: String,
    limit: Option<usize>,
    scope: Option<uuid::Uuid>,
    group: Option<String>,
    types: Option<String>,
) -> CmdResult<RouteSearchDto> {
    let limit = limit.unwrap_or(30);
    let wanted = ken_core::contenttype::parse_filter(types.as_deref());

    let group_targets: Option<Vec<uuid::Uuid>>;
    let (base_dir, embedder_slot, snapshots) = {
        let guard = state.lock().unwrap();
        // Manifest members when a workspace is open, so a member that has
        // never been focused this session is still searchable; the open
        // projects otherwise (single-project mode).
        let snapshots: Vec<RouteMemberSnapshot> = match guard.workspace.as_ref() {
            Some(ws) => ws
                .ws
                .members
                .iter()
                .filter_map(|m| match &m.status {
                    ken_core::workspace::MemberStatus::Ok(p) => Some(p),
                    _ => None,
                })
                .map(|p| {
                    let resident = guard.members.get(&p.config.id);
                    RouteMemberSnapshot {
                        project_id: p.config.id,
                        name: p.config.name.clone(),
                        search_db: resident.map(|m| m.search_db.clone()),
                        embedder_slot: resident.map(|m| m.semantic_embedder.clone()),
                    }
                })
                .collect(),
            None => guard
                .members
                .values()
                .map(|m| RouteMemberSnapshot {
                    project_id: m.project.config.id,
                    name: m.project.config.name.clone(),
                    search_db: Some(m.search_db.clone()),
                    embedder_slot: Some(m.semantic_embedder.clone()),
                })
                .collect(),
        };
        if snapshots.is_empty() {
            return Err("no project open".into());
        }
        if let Some(id) = scope {
            if !snapshots.iter().any(|s| s.project_id == id) {
                return Err("that project is not a member of this workspace".into());
            }
        }
        // A group scope resolves to its members' ids, which become the
        // plan's targets directly.
        group_targets = match (&group, guard.workspace.as_ref()) {
            (Some(name), Some(ws)) => {
                let ids: Vec<uuid::Uuid> = ws
                    .ws
                    .config
                    .effective_group_members(name)
                    .iter()
                    .filter_map(|folder| {
                        ws.ws.members.iter().find(|m| &m.name == folder).and_then(|m| {
                            match &m.status {
                                ken_core::workspace::MemberStatus::Ok(p) => Some(p.config.id),
                                _ => None,
                            }
                        })
                    })
                    .collect();
                if ids.is_empty() {
                    return Err(format!("group \"{name}\" has no resolvable members"));
                }
                Some(ids)
            }
            (Some(_), None) => return Err("no workspace open".into()),
            (None, _) => None,
        };
        let embedder_slot = guard
            .focused
            .and_then(|id| snapshots.iter().find(|s| s.project_id == id))
            .and_then(|s| s.embedder_slot.clone())
            .or_else(|| snapshots.iter().find_map(|s| s.embedder_slot.clone()));
        (guard.base_dir.clone(), embedder_slot, snapshots)
    };

    let _ = app.emit("routed-search-state", RoutedSearchStateEvent::Planning);

    // Plan and embed on the blocking pool: opening a dormant member's index
    // and embedding the query both touch disk or the local model.
    let query_for_plan = query.clone();
    let (plan, names, dbs, query_vec) = tauri::async_runtime::spawn_blocking(move || {
        // Dormant members are opened once here and reused by the fan-out.
        let registry = Registry::load(&base_dir).ok();
        let mut dbs: std::collections::HashMap<uuid::Uuid, Arc<Mutex<Db>>> = std::collections::HashMap::new();
        let mut members: Vec<routing::MemberInfo> = Vec::with_capacity(snapshots.len());
        for s in &snapshots {
            let handle = match &s.search_db {
                Some(db) => Some(db.clone()),
                None => Db::open(&base_dir, s.project_id).ok().map(|db| Arc::new(Mutex::new(db))),
            };
            if let Some(db) = handle {
                dbs.insert(s.project_id, db);
            }
            let reference = registry
                .as_ref()
                .and_then(|r| r.projects.iter().find(|e| e.id == s.project_id))
                .is_some_and(|e| routing::is_reference(&e.kind));
            members.push(routing::MemberInfo { project_id: s.project_id, name: s.name.clone(), reference });
        }
        let plan = match (scope, group_targets) {
            (Some(id), _) => routing::RoutePlan::chosen(vec![id]),
            (None, Some(ids)) => routing::RoutePlan::chosen(ids),
            (None, None) => routing::plan_route(&query_for_plan, &members),
        };
        let query_vec = embedder_slot.as_ref().and_then(|slot| {
            let mut guard = slot.lock().unwrap();
            guard.as_deref_mut().and_then(|e| e.embed_query(&query_for_plan).ok())
        });
        let names = members.into_iter().map(|m| (m.project_id, m.name)).collect();
        (plan, names, dbs, query_vec)
    })
    .await
    .map_err(|e| e.to_string())?;

    // One vocabulary for the whole search: the knowledge base's Vocabulary page
    // and decisions aliases widen the search in every repo, not only the wiki.
    let vocab = {
        let vocabs: Vec<ken_core::vocab::Vocabulary> = dbs
            .values()
            .filter_map(|db| db.lock().ok().and_then(|g| ken_core::vocab::Vocabulary::cached(&g).ok()))
            .collect();
        Arc::new(ken_core::vocab::Vocabulary::merged(vocabs.iter()))
    };
    let fan_out = RouteFanOut {
        app: app.clone(),
        dbs,
        names,
        query,
        query_vec,
        limit,
        wanted,
        vocab,
        done: Arc::new(AtomicUsize::new(0)),
    };

    let total = plan.targets.len();
    let _ = app.emit("routed-search-state", RoutedSearchStateEvent::Searching { done: 0, total });
    let mut member_hits = fan_out.run(&plan.targets, total).await?;
    if routing::needs_reserve(&plan, &member_hits) {
        member_hits.extend(fan_out.run(&plan.reserve, total + plan.reserve.len()).await?);
    }

    let report = routing::merge_routed(&plan, &member_hits, limit);
    let _ = app.emit("routed-search-state", RoutedSearchStateEvent::Done);
    Ok(report.into())
}

// ---------------------------------------------------------------------
// ken-memory task 2.2: memory_write / journal_append / read_journal /
// distill_journal / resolve_distill_candidate.
// ---------------------------------------------------------------------

/// Payload for the `memory-state` event (ken-memory task 2.2), mirroring
/// `WorkspaceKgStateEvent`/`RoutedSearchStateEvent`'s tag shape
/// (`{"state":"planning"}`, `{"state":"distilling"}`,
/// `{"state":"ready","candidates":[...]}`, `{"state":"error","reason":"..."}`)
/// and their "app-global, not `emit_member`" choice — a distillation run
/// reads the whole workspace journal, with no single owning project.
#[derive(Clone, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
enum MemoryStateEvent {
    Planning,
    Distilling,
    Ready { candidates: Vec<memory::DistillCandidate> },
    Error { reason: String },
}

/// Concatenate every current (non-archived) journal file, oldest first, each
/// under a `## YYYY-MM-DD` heading — the "rolling window" `distill_journal`
/// feeds `memory::compose_distill_prompt` (D6). Bounded to <= 30 days by
/// construction: the archive roll (task 2.3) already moves anything older
/// out of `journal/` before this ever runs, so no separate cutoff is needed
/// here.
fn journal_window_text(ws_root: &Path) -> String {
    let dir = memory::journal_dir(ws_root);
    let mut names: Vec<String> = match std::fs::read_dir(&dir) {
        Ok(entries) => entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "md"))
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect(),
        Err(_) => return String::new(),
    };
    names.sort();
    let mut out = String::new();
    for name in names {
        if let Ok(text) = std::fs::read_to_string(dir.join(&name)) {
            out.push_str(&format!("## {name}\n{}\n\n", text.trim()));
        }
    }
    out
}

/// How long one distillation call may take.
const DISTILL_TIMEOUT: Duration = Duration::from_secs(300);

/// Run a distillation pass over the current journal window (ken-memory task
/// 2.2 / design D6) through `memory::distill`: the journal window, with the
/// existing workspace memories and the previously dismissed slugs as the
/// dedupe guard, in one Claude call with no tools. The call is a turn of the
/// background throttle (`throttle::claude_turn`, decision 34), as the
/// summaries' are: it waits while paused and for the pace, counts against
/// the hour, and shows in the status panel ("memory"), with a failure
/// listed there too (Chris, 2026-10-15: distillation runs on Claude, not
/// the local model). It emits `memory-state` (`planning` → `distilling` →
/// `ready`/`error`). Candidates are cached server-side
/// (`AppState::memory_distill_candidates`) so
/// `resolve_distill_candidate(slug, approve)` can look one up by slug alone,
/// matching that command's own two-argument contract.
#[tauri::command(async)]
fn distill_journal(app: AppHandle, state: State<SharedState>) -> CmdResult<()> {
    let (base_dir, ws_root, pseudo_id, running, candidates_slot) = {
        let guard = state.lock().unwrap();
        let ws = guard.workspace.as_ref().ok_or("no workspace open")?;
        (
            guard.base_dir.clone(),
            ws.ws.root.clone(),
            memory::workspace_pseudo_member_id(ws.ws.config.id),
            guard.memory_distill_running.clone(),
            guard.memory_distill_candidates.clone(),
        )
    };
    if running.swap(true, Ordering::SeqCst) {
        return Err("a distillation run is already in progress".into());
    }

    let _ = app.emit("memory-state", MemoryStateEvent::Planning);
    let bg_app = app.clone();
    std::thread::spawn(move || {
        let outcome = (|| -> Result<Vec<memory::DistillCandidate>, String> {
            let binary = ken_core::runner::discover_claude().ok_or_else(|| ken_core::runner::MISSING_CLAUDE_HELP.to_string())?;
            let window = journal_window_text(&ws_root);
            let dismissed: Vec<String> = UserState::load(&base_dir, pseudo_id)
                .ignored
                .iter()
                .filter_map(|tagged| tagged.strip_prefix(DISMISSED_MEMORY_PREFIX).map(str::to_string))
                .collect();
            let existing: Vec<(String, String)> = memory::list_memories(&memory::workspace_memory_dir(&ws_root))
                .unwrap_or_default()
                .into_iter()
                .map(|m| (m.slug, m.description))
                .collect();

            let _ = bg_app.emit("memory-state", MemoryStateEvent::Distilling);
            // Background work: a turn of the one throttle (decision 34).
            let mut generate = |prompt: &str| -> ken_core::Result<String> {
                let _turn = ken_core::throttle::claude_turn(&|| false);
                let _running = ken_core::throttle::begin("memory", "journal");
                match ken_core::assistant::oneshot_answer(&binary, &ws_root, prompt, None, DISTILL_TIMEOUT, &CancelToken::new())? {
                    ken_core::assistant::OneshotOutcome::Completed(text) => Ok(text),
                    other => Err(ken_core::Error::Other(format!("claude: {other:?}"))),
                }
            };
            match memory::distill(&window, &existing, &dismissed, &mut generate) {
                Ok(candidates) => {
                    ken_core::throttle::clear_failure("memory", "journal");
                    Ok(candidates)
                }
                Err(e) => {
                    ken_core::throttle::fail("memory", "journal", &e.to_string());
                    Err(e.to_string())
                }
            }
        })();

        match outcome {
            Ok(candidates) => {
                *candidates_slot.lock().unwrap() = candidates.clone();
                let _ = bg_app.emit("memory-state", MemoryStateEvent::Ready { candidates });
            }
            Err(reason) => {
                let _ = bg_app.emit("memory-state", MemoryStateEvent::Error { reason });
            }
        }
        running.store(false, Ordering::SeqCst);
    });
    Ok(())
}

/// Approve or dismiss a distillation candidate by slug (ken-memory task 2.2
/// / design D6). Approve writes it via `memory::write_memory` in `Create`
/// mode at workspace scope (distillation candidates are always workspace-
/// scope — the journal itself has no per-project home, design's Non-Goals:
/// "per-project journals"); dismiss records the slug so `distill_journal`
/// won't re-propose it. The command's own contract (tasks.md 2.2) is
/// `(slug, approve)` only, so the candidate's body/description is looked up
/// from the last `distill_journal` run's cache, not resent by the caller.
///
/// Judgment call on persistence: `UserState` (`ken_core::user_state`) is
/// per-project, keyed by project id — there's no dedicated "workspace user
/// state" store in this codebase. The honest workspace-level home available
/// without touching `user_state.rs` (out of this task's touch-boundary) is
/// the pseudo-member's OWN `UserState` (it already has a real project id,
/// `memory::workspace_pseudo_member_id`), reusing its existing `ignored: BTreeSet<String>`
/// field with a `distill-dismissed:` prefix so a dismissed candidate slug
/// can never collide with an ignored file path (the pseudo-member has no
/// review-issue concept of its own to ignore in the first place).
#[tauri::command(async)]
fn resolve_distill_candidate(state: State<SharedState>, slug: String, approve: bool) -> CmdResult<()> {
    let (base_dir, ws_root, pseudo_id, candidate) = {
        let guard = state.lock().unwrap();
        let ws = guard.workspace.as_ref().ok_or("no workspace open")?;
        let candidate = guard
            .memory_distill_candidates
            .lock()
            .unwrap()
            .iter()
            .find(|c| c.slug == slug)
            .cloned();
        (
            guard.base_dir.clone(),
            ws.ws.root.clone(),
            memory::workspace_pseudo_member_id(ws.ws.config.id),
            candidate,
        )
    };

    if approve {
        let candidate = candidate.ok_or_else(|| {
            format!("no pending distillation candidate named '{slug}' — run distill_journal again")
        })?;
        let mscope = memory::MemoryScope::Workspace { workspace_root: &ws_root };
        memory::write_memory(mscope, &slug, &candidate.body, memory::WriteMode::Create, &local_date_today())
            .map_err(err)?;
    } else {
        let _us = user_state_lock();
        let mut us = UserState::load(&base_dir, pseudo_id);
        us.ignore(format!("{DISMISSED_MEMORY_PREFIX}{slug}"));
        us.save(&base_dir, pseudo_id).map_err(err)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Your day: my tasks, the workspace's ticket files, the digest, index
// health, and the watcher behind `day-changed`. The file logic lives in
// `ken_core::day`, shared with ken-mcp; this layer finds the homes, the
// workspace's repos and the inboxes, and shapes the wire DTOs.
// ---------------------------------------------------------------------

/// One repo of the workspace: a resolvable member.
#[derive(Clone)]
struct WsRepo {
    id: uuid::Uuid,
    name: String,
    root: PathBuf,
}

/// Every resolvable repo of the workspace. Groups only sort repos; a
/// workspace is not split into teams (decision 35).
fn ws_repos(ws: &ken_core::workspace::Workspace) -> Vec<WsRepo> {
    ws.members
        .iter()
        .filter_map(|m| match &m.status {
            ken_core::workspace::MemberStatus::Ok(p) => Some(WsRepo { id: p.config.id, name: m.name.clone(), root: p.root.clone() }),
            _ => None,
        })
        .collect()
}

/// The task home Your day reads: the workspace home, where every task lives.
/// A repo's own `.ken/tasks/` is not read: tasks live in your own folder, not
/// inside a repo (Ways of Working).
struct DayHomes {
    ws_root: PathBuf,
}

impl DayHomes {
    fn new(ws: &ken_core::workspace::Workspace) -> DayHomes {
        DayHomes { ws_root: ws.root.clone() }
    }

    fn homes(&self) -> Vec<tasks::TaskHome<'_>> {
        vec![tasks::TaskHome::Workspace { workspace_root: &self.ws_root }]
    }

    fn scan(&self, today: &str) -> Vec<ken_core::day::DayTask> {
        ken_core::day::scan(&self.homes(), today)
    }
}

/// `DayTask` on the wire (frame Y1's Other list).
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct DayTaskDto {
    id: String,
    title: String,
    state: ken_core::day::DayTaskState,
    target: Option<String>,
    repeat: Option<String>,
    links: Vec<String>,
    from: Option<String>,
    description: String,
    updated_by: Option<String>,
    updated: Option<String>,
    created: Option<String>,
    /// Relative to the workspace root (forward slashes).
    rel_path: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct DayTicketDto {
    id: String,
    title: String,
    state: String,
    target: Option<String>,
    project_id: String,
    repo: String,
    rel_path: String,
    linked_tasks: usize,
    linked_done: usize,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct DayMeDto {
    name: Option<String>,
    email: Option<String>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct DayStateDto {
    tickets: Vec<DayTicketDto>,
    tasks: Vec<DayTaskDto>,
    me: DayMeDto,
    has_tickets: bool,
}

fn ws_rel_path(ws_root: &Path, path: &Path) -> String {
    match path.strip_prefix(ws_root) {
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => path.to_string_lossy().replace('\\', "/"),
    }
}

fn day_task_dto(ws_root: &Path, t: &ken_core::day::DayTask) -> DayTaskDto {
    DayTaskDto {
        id: t.id.clone(),
        title: t.title.clone(),
        state: t.state,
        target: t.target.clone(),
        repeat: t.repeat.clone(),
        links: t.links.clone(),
        from: t.from.clone(),
        description: t.description.clone(),
        updated_by: t.updated_by.clone(),
        updated: t.updated.clone(),
        created: t.created.clone(),
        rel_path: ws_rel_path(ws_root, &t.path),
    }
}

/// git's global identity, read at most once a minute (every `day-changed`
/// recomputes Your day, and the identity rarely moves).
fn cached_git_me() -> ken_core::day::Me {
    // Who I am changes rarely, and asking git spawns two processes: the
    // first call waits for them; after that the cached answer comes back at
    // once, and one older than ten minutes is read again in the background.
    static CACHE: OnceLock<Mutex<Option<(Instant, ken_core::day::Me)>>> = OnceLock::new();
    static REFRESHING: AtomicBool = AtomicBool::new(false);
    let slot = CACHE.get_or_init(|| Mutex::new(None));
    let cached = lock_tolerant(slot).clone();
    match cached {
        Some((at, me)) => {
            if at.elapsed() >= Duration::from_secs(600) && !REFRESHING.swap(true, Ordering::SeqCst) {
                std::thread::spawn(move || {
                    let fresh = ken_core::day::git_me();
                    *lock_tolerant(slot) = Some((Instant::now(), fresh));
                    REFRESHING.store(false, Ordering::SeqCst);
                });
            }
            me
        }
        None => {
            let me = ken_core::day::git_me();
            *lock_tolerant(slot) = Some((Instant::now(), me.clone()));
            me
        }
    }
}

/// The folder every one of `roots` sits directly in, when they share one.
fn shared_parent(roots: &[PathBuf]) -> Option<PathBuf> {
    let first = roots.first()?.parent()?.to_path_buf();
    roots.iter().all(|r| r.parent() == Some(first.as_path())).then_some(first)
}

/// Lock a mutex even when a thread that held it panicked. Your day's
/// state (the workspace handle, a search index connection) stays usable
/// after a panic elsewhere, and a background thread must not die on it.
fn lock_tolerant<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The open workspace, the app data dir and the settings, cloned so no
/// filesystem work happens under the lock.
fn day_snapshot(state: &SharedState) -> CmdResult<(ken_core::workspace::Workspace, PathBuf, ken_core::settings::AppSettings)> {
    let guard = lock_tolerant(state);
    let ws = guard.workspace.as_ref().ok_or("no workspace open")?.ws.clone();
    Ok((ws, guard.base_dir.clone(), guard.app_settings.clone()))
}

/// My open tickets in the workspace, with how many of my tasks link to
/// each. The second value is whether any repo has a ticket file at all.
fn my_tickets(
    repos: &[WsRepo],
    me: &ken_core::day::Me,
    all_tasks: &[ken_core::day::DayTask],
) -> (Vec<DayTicketDto>, bool) {
    let mut has_tickets = false;
    let mut found: Vec<(ken_core::day::Ticket, &WsRepo)> = Vec::new();
    for repo in repos {
        let tickets = ken_core::day::scan_tickets(&repo.root);
        has_tickets |= !tickets.is_empty();
        if !me.is_known() {
            continue;
        }
        found.extend(tickets.into_iter().filter(|t| t.open && me.assigned(t)).map(|t| (t, repo)));
    }
    found.sort_by(|a, b| ken_core::day::ticket_order(&a.0, &b.0));
    let out = found
        .into_iter()
        .map(|(t, repo)| {
            // `<repo>/<ID>` links count for this repo's ticket only; a bare
            // `<ID>` link counts for any repo's ticket with that id.
            let (linked_tasks, linked_done) = ken_core::day::linked_counts(all_tasks, Some(&repo.name), &t.id);
            DayTicketDto {
                id: t.id,
                title: t.title,
                state: t.state,
                target: t.target,
                project_id: repo.id.to_string(),
                repo: repo.name.clone(),
                rel_path: t.rel_path,
                linked_tasks,
                linked_done,
            }
        })
        .collect();
    (out, has_tickets)
}

/// Your day (frame Y1): my open tickets in the workspace, and my tasks
/// for today.
#[tauri::command(async)]
fn day_state(state: State<SharedState>) -> CmdResult<DayStateDto> {
    let (ws, _base_dir, _settings) = day_snapshot(state.inner())?;
    let today = local_date_today();
    let all = DayHomes::new(&ws).scan(&today);

    let mut listed = ken_core::day::today_list(all.clone(), &today);
    ken_core::day::sort_for_day(&mut listed);
    let tasks_out = listed.iter().map(|t| day_task_dto(&ws.root, t)).collect();

    let me = cached_git_me();
    let repos = ws_repos(&ws);
    let (tickets, has_tickets) = my_tickets(&repos, &me, &all);
    Ok(DayStateDto {
        tickets,
        tasks: tasks_out,
        me: DayMeDto { name: me.name, email: me.email },
        has_tickets,
    })
}

fn local_stamp_now() -> String {
    chrono::Local::now().format("%Y-%m-%dT%H:%M").to_string()
}

/// Find one of my tasks by id.
fn find_day_task(ws: &ken_core::workspace::Workspace, id: &str, today: &str) -> CmdResult<ken_core::day::DayTask> {
    let all = DayHomes::new(ws).scan(today);
    ken_core::day::find(&all, id).cloned().ok_or_else(|| format!("no task with id '{id}'"))
}

fn reread_day_task(t: &ken_core::day::DayTask, today: &str) -> CmdResult<ken_core::day::DayTask> {
    let raw = std::fs::read_to_string(&t.path).map_err(err)?;
    Ok(ken_core::day::parse_task(&t.path, t.home, &raw, today))
}

/// The folder of the workspace member `project_id`.
fn member_root(state: &SharedState, project_id: &str) -> CmdResult<PathBuf> {
    let id: uuid::Uuid = project_id.parse().map_err(err)?;
    let guard = lock_tolerant(state);
    guard
        .workspace
        .as_ref()
        .and_then(|w| {
            w.ws.members.iter().find_map(|m| match &m.status {
                ken_core::workspace::MemberStatus::Ok(p) if p.config.id == id => Some(p.root.clone()),
                _ => None,
            })
        })
        .ok_or_else(|| "that source is not open".to_string())
}

/// Move one of my tickets to `status` (todo, in-progress, blocked,
/// in-review, testing, done, cancelled) in its file. In the knowledge base,
/// sync commits it as Ken's own change (decision 36); done and cancelled
/// leave Your day.
#[tauri::command(async)]
fn ticket_set_status(app: AppHandle, state: State<SharedState>, project_id: String, rel_path: String, status: String) -> CmdResult<()> {
    let status = status.trim().to_lowercase();
    if !ken_core::day::TICKET_STATUSES.contains(&status.as_str()) {
        return Err(format!("\"{status}\" is not a ticket status"));
    }
    let rel = rel_path.replace('\\', "/");
    if !rel.starts_with("tickets/") || rel.split('/').any(|p| p == "..") || !rel.ends_with(".md") {
        return Err(format!("{rel_path} is not a ticket"));
    }
    let path = member_root(state.inner(), &project_id)?.join(&rel);
    let raw = std::fs::read_to_string(&path).map_err(err)?;
    std::fs::write(&path, ken_core::day::set_frontmatter(&raw, "status", &status)).map_err(err)?;
    if let Ok(id) = project_id.parse::<uuid::Uuid>() {
        if let Some(m) = lock_tolerant(&state).members.get(&id) {
            m.sync.wrote(vec![rel.clone()]);
        }
    }
    let _ = app.emit("day-changed", ());
    Ok(())
}

/// Add a task to the workspace home (`updated_by: you`).
#[tauri::command(async)]
fn day_task_create(app: AppHandle, state: State<SharedState>, input: ken_core::day::DayTaskInput) -> CmdResult<DayTaskDto> {
    let (ws, _, _) = day_snapshot(state.inner())?;
    let (today, now) = (local_date_today(), local_stamp_now());
    let stamp = ken_core::day::Stamp { today: &today, now: &now, by: ken_core::day::BY_YOU };
    // The id, and so the path, is known before the write: the watcher is
    // told about the file before it appears.
    let id = tasks::new_ulid();
    let path = tasks::workspace_tasks_dir(&ws.root).join(tasks::task_file_name(&id, input.title.trim()));
    let task = with_task_writes(&app, state.inner(), &[path], || {
        ken_core::day::create_task(&ws.root, &input, &stamp, Some(&id)).map_err(err)
    })?;
    Ok(day_task_dto(&ws.root, &task))
}

/// Change a task (`updated_by: you`): only the fields given are written.
#[tauri::command(async)]
fn day_task_update(
    app: AppHandle,
    state: State<SharedState>,
    id: String,
    patch: ken_core::day::DayTaskPatch,
) -> CmdResult<DayTaskDto> {
    let (ws, _, _) = day_snapshot(state.inner())?;
    let (today, now) = (local_date_today(), local_stamp_now());
    let task = find_day_task(&ws, &id, &today)?;
    let stamp = ken_core::day::Stamp { today: &today, now: &now, by: ken_core::day::BY_YOU };
    with_task_writes(&app, state.inner(), &[task.path.clone()], || {
        ken_core::day::update_task(&task, &patch, &stamp).map_err(err)
    })?;
    let updated = reread_day_task(&task, &today)?;
    Ok(day_task_dto(&ws.root, &updated))
}

/// Delete a task: the file moves to its home's `archive/YYYY-MM/`.
#[tauri::command(async)]
fn day_task_delete(app: AppHandle, state: State<SharedState>, id: String) -> CmdResult<()> {
    let (ws, _, _) = day_snapshot(state.inner())?;
    let today = local_date_today();
    let task = find_day_task(&ws, &id, &today)?;
    with_task_writes(&app, state.inner(), &[task.path.clone()], || {
        ken_core::day::archive_task(&task, &today).map(|_| ()).map_err(err)
    })
}

/// My tasks linked to a ticket (frame Y1c), done ones included: those
/// linked by `<repo>/<ID>` (`repo` = the member `project_id` names) and
/// those linked by the bare `<ID>` (older links, any repo).
#[tauri::command(async)]
fn ticket_tasks(state: State<SharedState>, project_id: String, ticket_id: String) -> CmdResult<Vec<DayTaskDto>> {
    let (ws, _, _) = day_snapshot(state.inner())?;
    let repo = ws_repos(&ws)
        .into_iter()
        .find(|r| r.id.to_string().eq_ignore_ascii_case(project_id.trim()))
        .map(|r| r.name);
    let today = local_date_today();
    let all = DayHomes::new(&ws).scan(&today);
    let id = ticket_id.trim();
    let linked = match &repo {
        Some(name) => format!("{name}/{id}"),
        None => id.to_string(),
    };
    let q = ken_core::day::TaskQuery { linked: Some(linked), ..Default::default() };
    Ok(ken_core::day::query(&all, &q).into_iter().map(|t| day_task_dto(&ws.root, t)).collect())
}

// ---------- the digest ----------

#[derive(Serialize, serde::Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct DigestDto {
    generated_at: i64,
    body: String,
    sources: Vec<String>,
}

/// What is stored: the DTO plus the local day it was written for.
#[derive(Serialize, serde::Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct StoredDigest {
    date: String,
    #[serde(flatten)]
    digest: DigestDto,
}

/// `<app data>/team-digests/<workspace id>/_all.json`: the folder keeps its
/// older name so a digest written before decision 35 is still found. App
/// data, not the workspace folder: the digest is this machine's reading of
/// the day, not something to commit.
fn digest_path(base_dir: &Path, workspace_id: uuid::Uuid) -> PathBuf {
    base_dir.join("team-digests").join(workspace_id.to_string()).join("_all.json")
}

fn load_digest(base_dir: &Path, workspace_id: uuid::Uuid) -> Option<StoredDigest> {
    let text = std::fs::read_to_string(digest_path(base_dir, workspace_id)).ok()?;
    serde_json::from_str(&text).ok()
}

fn save_digest(base_dir: &Path, workspace_id: uuid::Uuid, stored: &StoredDigest) -> CmdResult<()> {
    let path = digest_path(base_dir, workspace_id);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(err)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(stored).map_err(err)?).map_err(err)
}

/// The workspace's newest digest, whatever day it was written.
#[tauri::command(async)]
fn day_digest(state: State<SharedState>) -> CmdResult<Option<DigestDto>> {
    let (ws, base_dir, _) = day_snapshot(state.inner())?;
    Ok(load_digest(&base_dir, ws.config.id).map(|s| s.digest))
}

/// Write the digest now ("Write it now").
#[tauri::command(async)]
fn refresh_day_digest(app: AppHandle, state: State<SharedState>) -> CmdResult<()> {
    day_snapshot(state.inner())?;
    generate_digest(&app, state.inner(), true)
}

/// Whether the digest is being written: Your day shows "Writing…" for one
/// it did not start itself.
#[tauri::command(async)]
fn day_digest_writing(state: State<SharedState>) -> CmdResult<bool> {
    let ws = lock_tolerant(&state).workspace.as_ref().map(|w| w.ws.config.id);
    Ok(ws.is_some_and(digest_writing))
}

/// The morning schedule's memory, per workspace, in app data
/// (`team-digests/<workspace id>/schedule.json`): the last attempt, so a
/// failure is not retried on every window focus.
#[derive(Serialize, serde::Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
struct DigestSchedule {
    last: Option<DigestAttempt>,
}

#[derive(Serialize, serde::Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct DigestAttempt {
    /// Epoch seconds.
    at: i64,
    /// The local day it was for.
    date: String,
    ok: bool,
}

/// After a failed attempt, the schedule waits this long before trying again.
const DIGEST_RETRY_AFTER_SECS: i64 = 30 * 60;

fn digest_schedule_path(base_dir: &Path, workspace_id: uuid::Uuid) -> PathBuf {
    base_dir.join("team-digests").join(workspace_id.to_string()).join("schedule.json")
}

fn load_digest_schedule(base_dir: &Path, workspace_id: uuid::Uuid) -> DigestSchedule {
    std::fs::read_to_string(digest_schedule_path(base_dir, workspace_id))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Read, change and write the schedule (best effort: a failed write only
/// means the schedule forgets). One writer at a time.
fn update_digest_schedule(base_dir: &Path, workspace_id: uuid::Uuid, change: impl FnOnce(&mut DigestSchedule)) {
    static WRITE: Mutex<()> = Mutex::new(());
    let _one = lock_tolerant(&WRITE);
    let mut s = load_digest_schedule(base_dir, workspace_id);
    change(&mut s);
    let path = digest_schedule_path(base_dir, workspace_id);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string_pretty(&s) {
        let _ = std::fs::write(&path, text);
    }
}

/// Should the schedule try now? Not when today's digest is stored, not when
/// an attempt already succeeded today, and not within
/// `DIGEST_RETRY_AFTER_SECS` of a failed one.
fn digest_due(schedule: &DigestSchedule, stored_today: bool, today: &str, now: i64) -> bool {
    if stored_today {
        return false;
    }
    match &schedule.last {
        Some(a) if a.ok => a.date != today,
        Some(a) => now - a.at >= DIGEST_RETRY_AFTER_SECS,
        None => true,
    }
}

/// The morning schedule (workspace open, window focus): past 07:00, write
/// today's digest for the workspace; see `digest_due` for when it is
/// skipped.
fn maybe_generate_digest(app: &AppHandle, state: &SharedState) {
    if local_hour() < 7 {
        return;
    }
    let (ws_id, base_dir) = {
        let guard = lock_tolerant(state);
        let Some(ws) = guard.workspace.as_ref() else { return };
        (ws.ws.config.id, guard.base_dir.clone())
    };
    let schedule = load_digest_schedule(&base_dir, ws_id);
    let today = local_date_today();
    let stored_today = load_digest(&base_dir, ws_id).is_some_and(|s| s.date == today);
    if digest_due(&schedule, stored_today, &today, engine::now_epoch()) {
        let _ = generate_digest(app, state, false);
    }
}

/// The digest's inputs, read off the lock: each repo's last day (resident
/// repos through their read connection, dormant ones through a read-only
/// open of their index). What waits on the person is not among them: Home
/// shows it live, and the digest is written once a day (Chris, 2026-10-15).
struct DigestJob {
    repos: Vec<(String, ken_core::digest::DigestSources)>,
    /// The repos' folders, for Claude to read.
    roots: Vec<PathBuf>,
}

fn digest_job(state: &SharedState) -> CmdResult<DigestJob> {
    let (ws, base_dir, _) = day_snapshot(state)?;
    let repos = ws_repos(&ws);
    let live: std::collections::HashMap<uuid::Uuid, Arc<Mutex<Db>>> = {
        let guard = lock_tolerant(state);
        repos
            .iter()
            .filter_map(|r| guard.members.get(&r.id).map(|m| (r.id, m.search_db.clone())))
            .collect()
    };
    let since = engine::now_epoch() - 86_400;
    let mut out = Vec::new();
    for r in &repos {
        let sources = with_index(&live, &base_dir, r.id, |db| ken_core::digest::gather(db, since).ok()).flatten();
        out.push((r.name.clone(), sources.unwrap_or_default()));
    }
    let roots = repos.iter().map(|r| r.root.clone()).collect();
    Ok(DigestJob { repos: out, roots })
}

/// The workspaces whose digest is being written or waits its turn. The
/// writer takes them in order; asking while it runs adds to the queue, and
/// Your day reads it to show "Writing…".
fn digest_queue() -> &'static Mutex<std::collections::VecDeque<uuid::Uuid>> {
    static Q: OnceLock<Mutex<std::collections::VecDeque<uuid::Uuid>>> = OnceLock::new();
    Q.get_or_init(|| Mutex::new(std::collections::VecDeque::new()))
}

/// Is workspace `ws`'s digest being written, or waiting to be?
fn digest_writing(ws: uuid::Uuid) -> bool {
    lock_tolerant(digest_queue()).contains(&ws)
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct DigestEvent {
    message: Option<String>,
}

/// Write the open workspace's digest on one background thread: Claude Code
/// runs from the folder that holds the repos so it can read across them. A
/// quiet day stores the quiet line without calling it. Emits
/// `day-digest-generating`, then `day-digest-updated` (the digest) or
/// `day-digest-error` (a string).
fn generate_digest(app: &AppHandle, state: &SharedState, force: bool) -> CmdResult<()> {
    let (running, ws_root, ws_id, ws_name, base_dir) = {
        let guard = lock_tolerant(state);
        let ws = guard.workspace.as_ref().ok_or("no workspace open")?;
        (guard.digest_running.clone(), ws.ws.root.clone(), ws.ws.config.id, ws.ws.config.name.clone(), guard.base_dir.clone())
    };
    let binary = ken_core::runner::discover_claude();
    if binary.is_none() && force {
        return Err(ken_core::runner::MISSING_CLAUDE_HELP.into());
    }
    {
        let mut q = lock_tolerant(digest_queue());
        if !q.contains(&ws_id) {
            q.push_back(ws_id);
            let _ = app.emit("day-digest-generating", DigestEvent { message: None });
        }
    }
    if running.swap(true, Ordering::SeqCst) {
        // The writer that is running takes it too.
        return Ok(());
    }
    // Cleared however the thread ends: done, an early return, or a panic.
    struct ClearOnDrop(Arc<AtomicBool>);
    impl Drop for ClearOnDrop {
        fn drop(&mut self) {
            self.0.store(false, Ordering::SeqCst);
        }
    }
    let running = ClearOnDrop(running);
    let thread_app = app.clone();
    let thread_state = state.clone();
    std::thread::spawn(move || {
        let next = || lock_tolerant(digest_queue()).front().copied();
        let done = |ws: uuid::Uuid| {
            let mut q = lock_tolerant(digest_queue());
            if let Some(i) = q.iter().position(|w| *w == ws) {
                q.remove(i);
            }
        };
        loop {
            while let Some(ws) = next() {
                // Only the open workspace is written; another left in the
                // queue (closed meanwhile) is dropped.
                if ws == ws_id {
                    write_digest(&thread_app, &thread_state, &base_dir, &ws_root, ws_id, &ws_name, binary.as_ref());
                }
                done(ws);
            }
            running.0.store(false, Ordering::SeqCst);
            // Asked for between the last look and the flag falling.
            if next().is_none() || running.0.swap(true, Ordering::SeqCst) {
                break;
            }
        }
    });
    Ok(())
}

/// The workspace's digest: stored, or an error event saying why not.
fn write_digest(
    thread_app: &AppHandle,
    thread_state: &SharedState,
    base_dir: &Path,
    ws_root: &Path,
    ws_id: uuid::Uuid,
    ws_name: &str,
    binary: Option<&PathBuf>,
) {
    let fail = |message: String| {
        let _ = thread_app.emit("day-digest-error", DigestEvent { message: Some(message) });
    };
    let today = local_date_today();
    // Recorded as failed until it is stored, so a failure (or a crash) backs
    // the schedule off instead of retrying each focus.
    let attempt = |ok: bool| {
        let date = today.clone();
        update_digest_schedule(base_dir, ws_id, move |s| {
            s.last = Some(DigestAttempt { at: engine::now_epoch(), date, ok });
        });
    };
    attempt(false);
    let job = match digest_job(thread_state) {
        Ok(j) => j,
        Err(e) => {
            fail(e);
            return;
        }
    };
    let store = |body: String, sources: Vec<String>| {
        let stored = StoredDigest { date: today.clone(), digest: DigestDto { generated_at: engine::now_epoch(), body, sources } };
        match save_digest(base_dir, ws_id, &stored) {
            Ok(()) => {
                attempt(true);
                let _ = thread_app.emit("day-digest-updated", stored.digest);
            }
            Err(e) => fail(e),
        }
    };
    if ken_core::digest::workspace_is_quiet(&job.repos) {
        store(ken_core::digest::QUIET_WORKSPACE_DIGEST.to_string(), Vec::new());
        return;
    }
    let Some(binary) = binary else {
        return; // the schedule without Claude Code: nothing to write with
    };
    let prompt = ken_core::digest::compose_workspace_digest_prompt(ws_name, &job.repos);
    // From the folder that holds the repos, when they share one, so a repo's
    // name is its folder; each repo is readable either way.
    let cwd = shared_parent(&job.roots).unwrap_or_else(|| ws_root.to_path_buf());
    match assistant::oneshot_in(binary, &cwd, &job.roots, &prompt, Duration::from_secs(240), &CancelToken::new()) {
        Ok(OneshotOutcome::Completed(text)) => {
            let parsed = digest::parse_digest(&text);
            store(parsed.body, parsed.sources);
        }
        Ok(OneshotOutcome::TimedOut) => fail("Writing the digest took too long and was stopped. It will try again later.".into()),
        Ok(OneshotOutcome::Failed(detail)) => fail(detail),
        Err(e) => fail(e.to_string()),
        Ok(OneshotOutcome::Cancelled) => {}
    }
}

// ---------- index health for the workspace ----------

/// The one health line for Your day (knowledge-core decision 19): how many
/// of the workspace's repos have an index with files in it, and their health
/// summed (`Db::index_health`: counting queries only): files indexed,
/// failed or skipped, meaning built, controls passing, whether the
/// embedding model is missing, files cut at the chunk cap.
#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct IndexHealthDto {
    repos_indexed: usize,
    repos: usize,
    #[serde(flatten)]
    health: ken_core::db::IndexHealth,
}

#[tauri::command(async)]
fn index_health(state: State<SharedState>) -> CmdResult<IndexHealthDto> {
    let (ws, base_dir, _) = day_snapshot(state.inner())?;
    let repos = ws_repos(&ws);
    let live: std::collections::HashMap<uuid::Uuid, Arc<Mutex<Db>>> = {
        let guard = lock_tolerant(state.inner());
        repos
            .iter()
            .filter_map(|r| guard.members.get(&r.id).map(|m| (r.id, m.search_db.clone())))
            .collect()
    };
    let mut out = IndexHealthDto { repos: repos.len(), ..Default::default() };
    for r in &repos {
        let health = match live.get(&r.id) {
            // The index lock is held for the counting queries only.
            Some(db) => lock_tolerant(db).index_health().ok(),
            None if db_path(&base_dir, r.id).exists() => Db::open_read_only(&base_dir, r.id).ok().and_then(|db| db.index_health().ok()),
            None => None,
        };
        if let Some(h) = health {
            if h.files > 0 {
                out.repos_indexed += 1;
            }
            out.health.add(&h);
        }
    }
    out.health.model_missing = ken_core::embedder::selected_profile().is_none();
    Ok(out)
}

// ---------- the watcher behind `day-changed` ----------

/// Cheap non-cryptographic content hash (std `DefaultHasher`/SipHash), for
/// the watcher's tick-to-tick snapshot and `AppState::task_recent_writes`.
/// Runtime-only, never persisted (except in a digest file's name).
fn content_hash(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

fn hash_file(path: &Path) -> Option<u64> {
    std::fs::read(path).ok().map(|b| content_hash(&b))
}

/// One file in the watcher's snapshot. The content hash decides whether
/// it changed; size and mtime only decide whether it has to be read again.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct WatchedFile {
    len: u64,
    mtime: Option<std::time::SystemTime>,
    hash: u64,
}

type DaySnapshot = std::collections::BTreeMap<PathBuf, WatchedFile>;

/// What a command said about a path it writes (`AppState::task_recent_writes`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TaskWriteExpect {
    /// The command is writing it now and will emit `day-changed` itself.
    Writing,
    /// Written, with this content.
    Present(u64),
    /// Removed (archived, moved).
    Absent,
}

#[derive(Clone, Copy, Debug)]
struct TaskWriteNote {
    expect: TaskWriteExpect,
    at: Instant,
}

/// A note the watcher has not consumed is dropped after this long, so the
/// registry cannot grow (a write the watcher never saw change, a command
/// that died mid-write).
const TASK_WRITE_NOTE_TTL: Duration = Duration::from_secs(30);

/// The folders Your day reads: the workspace home and each member's ticket
/// files (`tickets/` and one folder deeper). Only paths are taken under the
/// lock; the folder listing happens after. `None` when no workspace is open.
fn day_watch_dirs(state: &SharedState) -> Option<Vec<PathBuf>> {
    let (mut dirs, repo_roots) = {
        let guard = lock_tolerant(state);
        let w = guard.workspace.as_ref()?;
        let dirs = vec![tasks::workspace_tasks_dir(&w.ws.root)];
        let roots: Vec<PathBuf> = ws_repos(&w.ws).into_iter().map(|r| r.root).collect();
        (dirs, roots)
    };
    for root in repo_roots {
        let tickets = root.join(ken_core::day::TICKETS_DIR);
        if let Ok(entries) = std::fs::read_dir(&tickets) {
            for e in entries.flatten() {
                if e.file_type().is_ok_and(|t| t.is_dir()) {
                    dirs.push(e.path());
                }
            }
        }
        dirs.push(tickets);
    }
    Some(dirs)
}

/// A snapshot of every `.md` file directly in `dirs` (archives are
/// subfolders, so archiving reads as a removal). A file whose size and
/// mtime match `prev` keeps its hash without being read; only a file whose
/// metadata moved is read and hashed. A folder that does not exist
/// contributes nothing.
fn task_board_file_snapshot(dirs: &[PathBuf], prev: &DaySnapshot) -> DaySnapshot {
    let mut out = DaySnapshot::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.extension().is_some_and(|x| x.eq_ignore_ascii_case("md")) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            let (len, mtime) = (meta.len(), meta.modified().ok());
            let hash = match prev.get(&path) {
                Some(p) if p.len == len && p.mtime == mtime && mtime.is_some() => p.hash,
                _ => match hash_file(&path) {
                    Some(h) => h,
                    None => continue,
                },
            };
            out.insert(path, WatchedFile { len, mtime, hash });
        }
    }
    out
}

/// Did anything change between two snapshots that no command has
/// announced? Consumes the notes a change matches, keeps `Writing` notes
/// (their command emits), and drops notes older than the TTL.
fn day_change_unexplained(
    last: &DaySnapshot,
    current: &DaySnapshot,
    writes: &mut std::collections::HashMap<PathBuf, TaskWriteNote>,
) -> bool {
    writes.retain(|_, n| n.at.elapsed() < TASK_WRITE_NOTE_TTL);
    let mut unexplained = false;
    for (path, f) in current {
        if last.get(path).map(|l| l.hash) == Some(f.hash) {
            continue;
        }
        match writes.get(path).map(|n| n.expect) {
            Some(TaskWriteExpect::Writing) => {}
            Some(TaskWriteExpect::Present(h)) if h == f.hash => {
                writes.remove(path);
            }
            _ => unexplained = true,
        }
    }
    for path in last.keys().filter(|p| !current.contains_key(*p)) {
        match writes.get(path).map(|n| n.expect) {
            Some(TaskWriteExpect::Writing) => {}
            Some(TaskWriteExpect::Absent) => {
                writes.remove(path);
            }
            _ => unexplained = true,
        }
    }
    unexplained
}

/// Tell the frontend Your day changed (no payload: it asks `day_state`
/// again). App-global, like the inbox: it spans every repo.
fn emit_day_changed(app: &AppHandle, state: &SharedState) {
    if lock_tolerant(state).workspace.is_none() {
        return;
    }
    let _ = app.emit("day-changed", ());
}

/// Run `op`, which writes or removes `paths`, so Your day hears about it
/// exactly once: each path is noted as being written *before* `op` runs
/// (a watcher tick in the middle stays quiet), then noted with what is on
/// disk afterwards (so the next tick consumes it), and `day-changed` is
/// emitted once. The after-note and the emit happen whether `op`
/// succeeded or not: a failed write may still have changed a file.
fn with_task_writes<T>(app: &AppHandle, state: &SharedState, paths: &[PathBuf], op: impl FnOnce() -> CmdResult<T>) -> CmdResult<T> {
    let registry = { lock_tolerant(state).task_recent_writes.clone() };
    {
        let mut writes = lock_tolerant(&registry);
        for p in paths {
            writes.insert(p.clone(), TaskWriteNote { expect: TaskWriteExpect::Writing, at: Instant::now() });
        }
    }
    let out = op();
    {
        let after: Vec<(PathBuf, TaskWriteExpect)> = paths
            .iter()
            .map(|p| {
                let expect = match hash_file(p) {
                    Some(h) => TaskWriteExpect::Present(h),
                    None => TaskWriteExpect::Absent,
                };
                (p.clone(), expect)
            })
            .collect();
        let mut writes = lock_tolerant(&registry);
        for (p, expect) in after {
            writes.insert(p, TaskWriteNote { expect, at: Instant::now() });
        }
    }
    emit_day_changed(app, state);
    out
}

/// How often the watcher re-snapshots (1.2 s: an agent's write, a hand
/// edit or a pull feels live; a tick lists a few folders and reads only
/// the files whose size or mtime moved).
const TASK_BOARD_POLL_INTERVAL: Duration = Duration::from_millis(1200);

/// Your day's file watcher: a sleep loop over a metadata-plus-hash
/// snapshot (`task_board_file_snapshot`), the same polling discipline the
/// `.kenignore` watcher uses (`notify` is a ken-core-only dependency).
/// Runs for the open workspace's lifetime; `WorkspaceState::task_watch`
/// stops it on close or switch.
///
/// Self-write dedupe: a command that writes a task goes through
/// `with_task_writes`, which notes each path in
/// `AppState::task_recent_writes` before and after the write and emits
/// `day-changed` itself. A tick that sees only noted changes stays quiet;
/// any other change (an agent, a hand edit, a pull, a teammate's inbox
/// item) emits one `day-changed` for the whole tick.
fn spawn_task_board_watch(app: AppHandle, state: SharedState) -> StopOnDrop {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    std::thread::spawn(move || {
        let mut last = day_watch_dirs(&state)
            .map(|dirs| task_board_file_snapshot(&dirs, &DaySnapshot::new()))
            .unwrap_or_default();
        while !stop_thread.load(Ordering::SeqCst) {
            std::thread::sleep(TASK_BOARD_POLL_INTERVAL);
            if stop_thread.load(Ordering::SeqCst) {
                break;
            }
            let Some(dirs) = day_watch_dirs(&state) else {
                continue; // no workspace open this tick (closing/switching)
            };
            let current = task_board_file_snapshot(&dirs, &last);
            let registry = { lock_tolerant(&state).task_recent_writes.clone() };
            let unexplained = {
                let mut writes = lock_tolerant(&registry);
                if current == last {
                    writes.retain(|_, n| n.at.elapsed() < TASK_WRITE_NOTE_TTL);
                    false
                } else {
                    day_change_unexplained(&last, &current, &mut writes)
                }
            };
            last = current;
            if unexplained {
                emit_day_changed(&app, &state);
            }
        }
    });
    StopOnDrop(stop)
}

// ---------------------------------------------------------------------
// The workspace's members: candidates to add, ignored folders, groups, and
// the members overview the add/remove commands return.
// ---------------------------------------------------------------------

/// Join an existing sibling folder to the open workspace.
///
/// Until now the only way to add a member was the creation picker, which
/// means closing the workspace — so a repo cloned after setup could not be
/// added at all. `Project::create` adopts an existing `.ken/project.json`
/// rather than clobbering it, so re-adding a folder Ken already knows is
/// safe.
///
/// The new member is registered but deliberately NOT activated: it lands
/// dormant and opens on first focus, exactly like a member past the
/// resident cap. Joining a repo should not evict a resident one.
#[tauri::command(async)]
fn workspace_add_member(state: State<SharedState>, folder: String) -> CmdResult<Vec<MemberOverviewDto>> {
    // D6: a member is a folder in the workspace or one inside a single
    // group folder — `validate_member_name` normalizes to forward slashes
    // and rejects traversal (`..`, absolutes), so the join below can't
    // escape the workspace root.
    let folder = ken_core::workspace::validate_member_name(&folder).map_err(err)?;
    let mut guard = state.lock().unwrap();
    let ws = guard.workspace.as_mut().ok_or("no workspace open")?;
    if ws.ws.config.members.iter().any(|m| m == &folder) {
        return Err(format!("\"{folder}\" is already in this workspace"));
    }
    let member_root = ws.ws.root.join(&folder);
    if !member_root.is_dir() {
        return Err(format!("no folder named \"{folder}\" in this workspace"));
    }
    let project = ken_core::project::Project::create(
        &member_root,
        ken_core::workspace::member_leaf(&folder),
    )
    .map_err(err)?;
    ws.ws.config.members.push(folder.clone());
    ws.ws.members.push(ken_core::workspace::Member {
        name: folder,
        status: ken_core::workspace::MemberStatus::Ok(project),
    });
    ws.ws.save().map_err(err)?;
    drop(guard);
    workspace_members_overview(state)
}

/// What removing a member did: the fresh roster. Pages that still cite the
/// repo are lint's to report.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RemovedMemberDto {
    members: Vec<MemberOverviewDto>,
}

/// Take a repo out of the open workspace: out of the manifest and its
/// groups, its runtime closed. The folder, its files and its index stay; the
/// repo stays in Ken's list.
#[tauri::command(async)]
fn workspace_remove_member(state: State<SharedState>, name: String) -> CmdResult<RemovedMemberDto> {
    {
        let mut guard = state.lock().unwrap();
        let ws = guard.workspace.as_mut().ok_or("no workspace open")?;
        let id = ws.ws.members.iter().find(|m| m.name == name).and_then(|m| match &m.status {
            ken_core::workspace::MemberStatus::Ok(p) => Some(p.config.id),
            _ => None,
        });
        if !ws.ws.config.remove_member(&name) {
            return Err(format!("\"{name}\" is not in this workspace"));
        }
        ws.ws.members.retain(|m| m.name != name);
        ws.ws.save().map_err(err)?;
        if let Some(id) = id {
            guard.members.remove(&id);
            if guard.focused == Some(id) {
                guard.focused = guard.members.keys().next().copied();
            }
        }
    }
    Ok(RemovedMemberDto { members: workspace_members_overview(state)? })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectGroupDto {
    name: String,
    /// Parent-relative folder names, filtered to current members.
    members: Vec<String>,
    /// Resolved project ids for the members that are currently resolvable —
    /// what a scoped search actually targets.
    project_ids: Vec<uuid::Uuid>,
}

/// Named groups of members (e.g. a game and its tools). Stored in the
/// workspace manifest so the grouping travels with the folder.
#[tauri::command(async)]
fn workspace_groups(state: State<SharedState>) -> CmdResult<Vec<ProjectGroupDto>> {
    let guard = state.lock().unwrap();
    let ws = guard.workspace.as_ref().ok_or("no workspace open")?;
    Ok(group_dtos(&ws.ws))
}

/// Resolve every group against the live manifest. Kept separate so the
/// create/delete commands can return the fresh list without re-locking.
fn group_dtos(ws: &ken_core::workspace::Workspace) -> Vec<ProjectGroupDto> {
    // The effective view: groups derived from group FOLDERS (D6 — a
    // nested member's leading segment) first, then manifest groups from
    // Settings. One list, so every surface (Home's scope picker, routed
    // search, chat scope) sees the same groups without knowing which kind
    // each one is.
    ws.config
        .effective_groups()
        .iter()
        .map(|g| {
            let members = ws.config.effective_group_members(&g.name);
            let project_ids = members
                .iter()
                .filter_map(|folder| {
                    ws.members.iter().find(|m| &m.name == folder).and_then(|m| {
                        match &m.status {
                            ken_core::workspace::MemberStatus::Ok(p) => Some(p.config.id),
                            _ => None,
                        }
                    })
                })
                .collect();
            ProjectGroupDto {
                name: g.name.clone(),
                members,
                project_ids,
            }
        })
        .collect()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MemberOverviewDto {
    /// Parent-relative folder name, exactly as the manifest stores it.
    folder: String,
    /// `ok` | `missing` | `invalid`.
    status: String,
    /// Parse error, for `invalid` members only.
    detail: Option<String>,
    project_id: Option<uuid::Uuid>,
    name: Option<String>,
    /// Whether this member has a live runtime this session.
    resident: bool,
    /// Semantic index built and searchable.
    index_ready: bool,
    file_count: usize,
    failed_files: usize,
    unread: usize,
}

/// Per-member state, what `workspace_add_member` and
/// `workspace_remove_member` return. It covers EVERY manifest member,
/// including `Missing` and `Invalid` ones.
///
/// Read-only in the strict sense: a member whose user-state has never been
/// baselined reports `unread: 0` rather than its whole tree, and no
/// baseline is written — computing an overview must not mark a project's
/// files as seen.
fn workspace_members_overview(state: State<SharedState>) -> CmdResult<Vec<MemberOverviewDto>> {
    enum Entry {
        Ok {
            folder: String,
            project_id: uuid::Uuid,
            name: String,
            live: Option<Arc<Mutex<Db>>>,
        },
        Missing(String),
        Invalid(String, String),
    }

    let (base_dir, entries) = {
        let guard = state.lock().unwrap();
        let ws = guard.workspace.as_ref().ok_or("no workspace open")?;
        let entries: Vec<Entry> = ws
            .ws
            .members
            .iter()
            .map(|m| match &m.status {
                ken_core::workspace::MemberStatus::Ok(p) => Entry::Ok {
                    folder: m.name.clone(),
                    project_id: p.config.id,
                    name: p.config.name.clone(),
                    live: guard.members.get(&p.config.id).map(|r| r.search_db.clone()),
                },
                ken_core::workspace::MemberStatus::Missing => Entry::Missing(m.name.clone()),
                ken_core::workspace::MemberStatus::Invalid(e) => {
                    Entry::Invalid(m.name.clone(), e.clone())
                }
            })
            .collect();
        (guard.base_dir.clone(), entries)
    };

    Ok(entries
        .into_iter()
        .map(|entry| match entry {
            Entry::Missing(folder) => MemberOverviewDto {
                folder,
                status: "missing".into(),
                detail: None,
                project_id: None,
                name: None,
                resident: false,
                index_ready: false,
                file_count: 0,
                failed_files: 0,
                unread: 0,
            },
            Entry::Invalid(folder, detail) => MemberOverviewDto {
                folder,
                status: "invalid".into(),
                detail: Some(detail),
                project_id: None,
                name: None,
                resident: false,
                index_ready: false,
                file_count: 0,
                failed_files: 0,
                unread: 0,
            },
            Entry::Ok {
                folder,
                project_id,
                name,
                live,
            } => {
                let resident = live.is_some();
                let opened = match live {
                    Some(db) => Some(db),
                    None => Db::open(&base_dir, project_id)
                        .ok()
                        .map(|db| Arc::new(Mutex::new(db))),
                };
                let (index_ready, files) = match &opened {
                    Some(db) => {
                        let db = db.lock().unwrap();
                        (db.vec_available(), db.list_files().unwrap_or_default())
                    }
                    None => (false, Vec::new()),
                };
                let failed_files = files.iter().filter(|f| f.status == "failed").count();
                let unread = {
                    let mut us = UserState::load(&base_dir, project_id);
                    let index = index_versions(&files);
                    // `baseline` returning true means this project has never
                    // been baselined — reporting its entire tree as unread
                    // would be nonsense. Nothing is saved either way.
                    if us.baseline(&index) {
                        0
                    } else {
                        us.unread(&index).len()
                    }
                };
                MemberOverviewDto {
                    folder,
                    status: "ok".into(),
                    detail: None,
                    project_id: Some(project_id),
                    name: Some(name),
                    resident,
                    index_ready,
                    file_count: files.len(),
                    failed_files,
                    unread,
                }
            }
        })
        .collect())
}

/// How often the background worker asks whether chunks wait for a summary.
const SUMMARY_CHECK: Duration = Duration::from_secs(30);
/// How long the worker leaves summaries alone after a Claude call failed.
const SUMMARY_BACKOFF: Duration = Duration::from_secs(600);
/// Longest one batch of summaries may take.
const SUMMARY_TIMEOUT: Duration = Duration::from_secs(300);

/// What one turn of a background Claude job did ([`summarize_by_claude`],
/// [`knowledge_checks`]).
#[derive(PartialEq, Eq)]
enum SummaryTick {
    /// Nothing to do, or not allowed to.
    Idle,
    /// A call was made (a batch summarized, a check answered).
    Ran,
    /// The Claude call failed.
    Failed,
    /// The project closed or the worker was asked to stop.
    Stopped,
}

/// Summarize one batch of this project's knowledge-base chunks with Claude
/// (`summary`, knowledge-core decision 16). Only for a knowledge-base repo
/// (kind wiki), each call a turn of the one background throttle
/// (decision 34). Claude reads the prompt and answers, with no tools. The
/// local model never writes them.
fn summarize_by_claude(state: &SharedState, project_id: uuid::Uuid, stop: &Arc<AtomicBool>) -> SummaryTick {
    let (base, root, name) = {
        let guard = lock_tolerant(state);
        let Some(active) = guard.members.get(&project_id) else {
            return SummaryTick::Stopped;
        };
        (guard.base_dir.clone(), active.project.root.clone(), active.project.config.name.clone())
    };
    let (kinds, _) = ken_core::registry::index_of(&root);
    if !kinds.contains(&ken_core::registry::RepoKind::Wiki) {
        return SummaryTick::Idle;
    }
    let Ok(mut db) = Db::open(&base, project_id) else {
        return SummaryTick::Idle;
    };
    // Nothing waiting: no Claude, and the pace is left for others.
    if !ken_core::summary::next_batch(&db).is_ok_and(|b| !b.is_empty()) {
        return SummaryTick::Idle;
    }
    let Some(binary) = ken_core::runner::discover_claude() else {
        return SummaryTick::Idle;
    };
    let halt = || stop.load(Ordering::SeqCst);
    let Some(turn) = ken_core::throttle::claude_turn(&halt) else {
        return SummaryTick::Stopped;
    };
    let _running = ken_core::throttle::begin("summaries", &name);
    let mut generate = |prompt: &str| -> ken_core::Result<String> {
        match ken_core::assistant::oneshot_answer(&binary, &root, prompt, None, SUMMARY_TIMEOUT, &CancelToken::new())? {
            ken_core::assistant::OneshotOutcome::Completed(text) => Ok(text),
            other => Err(ken_core::Error::Other(format!("claude: {other:?}"))),
        }
    };
    let result = ken_core::summary::summarize_next(&mut db, &mut generate, engine::now_epoch());
    drop(turn);
    match result {
        Ok(Some(_)) => {
            ken_core::throttle::clear_failure("summaries", &name);
            SummaryTick::Ran
        }
        Ok(None) => SummaryTick::Idle,
        Err(e) => {
            eprintln!("summaries: {e}");
            ken_core::throttle::fail("summaries", &name, &e.to_string());
            SummaryTick::Failed
        }
    }
}

/// The background worker: one per open project. It takes the project's
/// background turns in order: one batch of one-line summaries for search
/// (knowledge-core decision 16), a knowledge base's checks (drift, lint, the
/// supersession pass, topic tags and the graph's commits, decisions 21 and
/// 25 to 30), and a knowledge base's build queue (decision 33). Every Claude
/// call and every step is a turn of the one background throttle (decision
/// 34), whose pace also sets how often the checks are looked at. The local
/// model is never used: its background reader is gone (decision 21).
fn background_worker(app: AppHandle, state: SharedState, project_id: uuid::Uuid, stop: Arc<AtomicBool>) {
    let mut last_summary_check = Instant::now() - SUMMARY_CHECK;
    let mut summaries_after = Instant::now();
    // The first look at the checks a minute after the project opens, once
    // its first scan has had time to land.
    let soon = |wait: Duration| {
        let every = ken_core::throttle::limits().checks_every;
        Instant::now().checked_sub(every.saturating_sub(wait)).unwrap_or_else(Instant::now)
    };
    let mut last_checks = soon(Duration::from_secs(60));
    let mut checks_after = Instant::now();
    let mut last_build = Instant::now();
    let mut last_move_look = Instant::now();
    let mut moves_seen: Vec<(String, String)> = Vec::new();
    while !stop.load(Ordering::SeqCst) {
        if ken_core::throttle::paused() {
            std::thread::sleep(Duration::from_secs(2));
            continue;
        }
        // One-line summaries of the knowledge base's pages, for search
        // (knowledge-core decision 16): one batch per turn, while any wait.
        if last_summary_check.elapsed() >= SUMMARY_CHECK && Instant::now() >= summaries_after {
            last_summary_check = Instant::now();
            match summarize_by_claude(&state, project_id, &stop) {
                // More may wait: look again on the next turn.
                SummaryTick::Ran => last_summary_check = Instant::now() - SUMMARY_CHECK,
                SummaryTick::Failed => summaries_after = Instant::now() + SUMMARY_BACKOFF,
                SummaryTick::Idle => {}
                SummaryTick::Stopped => return,
            }
        }
        // A commit in a source moves the checks forward (decision 28): the
        // branches are looked at every minute, cheaply, rather than at the
        // pace's next look (15 minutes when Gentle). A move that brought the
        // checks forward once does not again until the heads move further.
        if last_move_look.elapsed() >= MOVE_LOOK && Instant::now() >= checks_after {
            last_move_look = Instant::now();
            let moves = moved_since_drift(&state, project_id);
            if !moves.is_empty() && moves != moves_seen {
                last_checks = Instant::now() - ken_core::throttle::limits().checks_every;
            }
            moves_seen = moves;
        }
        // A knowledge base's drift, lint and graph (knowledge-core decisions
        // 21, 25, 28 to 30): a drift run when one is due, then at most one
        // background Claude call per turn while any check waits.
        if last_checks.elapsed() >= ken_core::throttle::limits().checks_every && Instant::now() >= checks_after {
            last_checks = Instant::now();
            match knowledge_checks(&state, project_id, &stop) {
                SummaryTick::Ran => last_checks = soon(SUMMARY_CHECK),
                SummaryTick::Failed => checks_after = Instant::now() + CHECKS_BACKOFF,
                SummaryTick::Idle => {}
                SummaryTick::Stopped => return,
            }
        }
        // The build queue (decision 33): its next item into the inbox when
        // there is room, every few seconds.
        if last_build.elapsed() >= BUILD_EVERY {
            last_build = Instant::now();
            if build_turn(&app, &state, project_id, &stop) == SummaryTick::Stopped {
                return;
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// How often the background worker looks at the build queue.
const BUILD_EVERY: Duration = Duration::from_secs(10);

/// How often the background worker looks for a source's branch moving.
const MOVE_LOOK: Duration = Duration::from_secs(60);

/// The code repos whose branch moved since the knowledge base's last drift
/// run (`drift::moved_heads`), with their heads now. Empty for a repo that is
/// not a knowledge base, or whose index will not open.
fn moved_since_drift(state: &SharedState, project_id: uuid::Uuid) -> Vec<(String, String)> {
    let (base, root) = {
        let guard = lock_tolerant(state);
        let Some(m) = guard.members.get(&project_id) else { return Vec::new() };
        (guard.base_dir.clone(), m.project.root.clone())
    };
    if !ken_core::registry::is_library(&root) {
        return Vec::new();
    }
    Db::open_read_only(&base, project_id).map(|db| ken_core::drift::moved_heads(&db)).unwrap_or_default()
}

/// One turn of a knowledge base's build queue (knowledge-core decision 33):
/// the queue is brought up to date with the inbox (an item read is done, one
/// that failed carries ingest's reason), then the next items go into Raw
/// while there is room, each a step of the one background throttle
/// (decision 34), and an ingest pass is started for them. The overview of a
/// source is made from its own index, read only; a file is copied as it is.
fn build_turn(_app: &AppHandle, state: &SharedState, project_id: uuid::Uuid, stop: &Arc<AtomicBool>) -> SummaryTick {
    let (base, project, sources) = {
        let guard = lock_tolerant(state);
        let Some(active) = guard.members.get(&project_id) else {
            return SummaryTick::Stopped;
        };
        let sources: Vec<(String, uuid::Uuid, PathBuf)> =
            workspace_repos(&guard).into_iter().filter(|(_, id, _)| *id != project_id).collect();
        (guard.base_dir.clone(), active.project.clone(), sources)
    };
    if !ken_core::registry::is_library(&project.root) {
        return SummaryTick::Idle;
    }
    let Ok(db) = Db::open(&base, project_id) else {
        return SummaryTick::Idle;
    };
    let Ok(Some(mut queue)) = ken_core::buildqueue::load(&db) else {
        return SummaryTick::Idle;
    };
    let failures = ken_core::ingest::failures(&db).unwrap_or_default();
    let mut changed = ken_core::buildqueue::refresh(&mut queue, &project.root, &failures);
    let mut placed = false;
    let halt = || stop.load(Ordering::SeqCst);
    while let Some(i) = ken_core::buildqueue::next(&queue) {
        if !ken_core::throttle::step_turn(&halt) {
            break;
        }
        let item = queue.items[i].clone();
        let _running = ken_core::throttle::begin("build", &item.label());
        let root = project.root.clone();
        let place = || -> Result<PathBuf, String> {
            let (_, source_id, source_root) =
                sources.iter().find(|(n, _, _)| *n == item.source).ok_or("the source is not in the workspace any more")?;
            match item.pass {
                ken_core::buildqueue::Pass::Overview => {
                    let sdb = Db::open_read_only(&base, *source_id).map_err(err)?;
                    let (name, text) =
                        ken_core::buildqueue::overview_of(&item.source, source_root, &sdb, &local_source_stamp()).map_err(err)?;
                    let target = ken_core::ingest::raw_target(&root, &name).map_err(err)?;
                    std::fs::write(&target, text).map_err(err)?;
                    Ok(target)
                }
                ken_core::buildqueue::Pass::File => {
                    let path = item.path.as_deref().ok_or("no file named")?;
                    let target = ken_core::ingest::raw_target(&root, &ken_core::buildqueue::raw_name(&item.source, path)).map_err(err)?;
                    std::fs::copy(source_root.join(path), &target).map_err(|e| format!("could not copy {path}: {e}"))?;
                    Ok(target)
                }
            }
        };
        let it = &mut queue.items[i];
        match place() {
            Ok(target) => {
                let rel = target.strip_prefix(&project.root).map(|r| r.to_string_lossy().replace('\\', "/")).unwrap_or_default();
                it.state = ken_core::buildqueue::ItemState::InRaw;
                it.raw = Some(rel);
                placed = true;
            }
            Err(e) => {
                it.state = ken_core::buildqueue::ItemState::Failed;
                it.reason = Some(e);
            }
        }
        changed = true;
    }
    if changed {
        if let Err(e) = ken_core::buildqueue::save(&db, &queue) {
            eprintln!("build queue: {e}");
        }
    }
    if placed {
        start_ingest_pass(&project, &base, code_sources_of(state, &project.root), false);
        SummaryTick::Ran
    } else if halt() {
        SummaryTick::Stopped
    } else {
        SummaryTick::Idle
    }
}

/// The workspace's knowledge base and the other repos, its sources.
fn kb_and_sources(state: &SharedState) -> CmdResult<((String, uuid::Uuid, PathBuf), Vec<(String, uuid::Uuid, PathBuf)>, PathBuf)> {
    let guard = state.lock().unwrap();
    let repos = workspace_repos(&guard);
    let kb = knowledge_base_of(&repos, None).ok_or("this workspace has no knowledge base to build")?;
    let sources = repos.into_iter().filter(|r| r.1 != kb.1).collect();
    Ok((kb, sources, guard.base_dir.clone()))
}

/// Start building the library (knowledge-core decision 33): a queue of the
/// workspace's sources, the heavy hitters first, an overview of each, then
/// their files broad to fine, kept in the knowledge base's index. The
/// background worker then works through it under the one throttle; Pause
/// stops it, and starting again plans the queue anew.
#[tauri::command(async)]
fn build_start(state: State<SharedState>, at_once: Option<usize>) -> CmdResult<ken_core::buildqueue::Progress> {
    let (kb, sources, base) = kb_and_sources(&state)?;
    let mut planned = Vec::new();
    for (name, id, _) in &sources {
        if let Ok(db) = Db::open_read_only(&base, *id) {
            planned.push(ken_core::buildqueue::source_of(name, &db).map_err(err)?);
        }
    }
    if planned.is_empty() {
        return Err("no source of the workspace has an index yet: wait for its first scan".into());
    }
    let db = Db::open(&base, kb.1).map_err(err)?;
    let at_once = at_once.or_else(|| ken_core::buildqueue::load(&db).ok().flatten().map(|q| q.at_once)).unwrap_or(1);
    let queue = ken_core::buildqueue::plan(&planned, at_once);
    ken_core::buildqueue::save(&db, &queue).map_err(err)?;
    Ok(ken_core::buildqueue::progress(&queue))
}

/// Stop building: what has not gone into the inbox is dropped; what waits
/// there is read like any source.
#[tauri::command(async)]
fn build_stop(state: State<SharedState>) -> CmdResult<()> {
    let (kb, _, base) = kb_and_sources(&state)?;
    let db = Db::open(&base, kb.1).map_err(err)?;
    if let Some(mut queue) = ken_core::buildqueue::load(&db).map_err(err)? {
        ken_core::buildqueue::stop(&mut queue);
        ken_core::buildqueue::save(&db, &queue).map_err(err)?;
    }
    Ok(())
}

/// How many build items may wait in the inbox at once: one to start, a few
/// once the results are trusted (1 to 5).
#[tauri::command(async)]
fn build_set_at_once(state: State<SharedState>, at_once: usize) -> CmdResult<()> {
    let (kb, _, base) = kb_and_sources(&state)?;
    let db = Db::open(&base, kb.1).map_err(err)?;
    if let Some(mut queue) = ken_core::buildqueue::load(&db).map_err(err)? {
        queue.at_once = at_once.clamp(1, 5);
        ken_core::buildqueue::save(&db, &queue).map_err(err)?;
    }
    Ok(())
}

/// One line of the status panel: something queued, with how many.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct QueuedDto {
    kind: String,
    label: String,
    count: usize,
}

/// The one status panel (knowledge-core decision 34): the throttle (pace,
/// pause, the hour's calls), what is running with its progress, what is
/// queued, and what failed with its reason.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BackgroundStatusDto {
    throttle: ken_core::throttle::Snapshot,
    running: Vec<ken_core::throttle::Job>,
    queued: Vec<QueuedDto>,
    failed: Vec<ken_core::throttle::Failure>,
    /// The library's build queue, when one was started.
    build: Option<ken_core::buildqueue::Progress>,
}

#[tauri::command(async)]
fn background_status(state: State<SharedState>) -> CmdResult<BackgroundStatusDto> {
    let snapshot = ken_core::throttle::snapshot();
    let mut running = snapshot.running.clone();
    let mut failed = snapshot.failed.clone();
    let (members, base, kb) = {
        let guard = lock_tolerant(&state);
        let members: Vec<(uuid::Uuid, String, PathBuf)> =
            guard.members.values().map(|m| (m.project.config.id, m.project.config.name.clone(), m.project.root.clone())).collect();
        let kb = knowledge_base_of(&workspace_repos(&guard), None);
        (members, guard.base_dir.clone(), kb)
    };
    // Meaning search being built, per repo.
    {
        let builds = semantic_builds().lock().unwrap_or_else(|p| p.into_inner());
        for (id, (done, total)) in builds.iter() {
            let name = members.iter().find(|m| m.0 == *id).map(|m| m.1.clone()).unwrap_or_default();
            // Embedding waits while paused, between batches.
            let waiting = snapshot.setting.paused;
            running.push(ken_core::throttle::Job { id: 0, kind: "meaning".into(), label: name, done: *done as u64, total: *total as u64, waiting });
        }
    }
    // The source each inbox is reading now.
    for (id, (raw, phase)) in INGEST_READING.lock().unwrap().iter() {
        let name = members.iter().find(|m| m.0 == *id).map(|m| m.1.clone()).unwrap_or_default();
        let what = match phase {
            ken_core::ingest::PassPhase::Transcribing(pct) => format!("transcribing {pct}%"),
            ken_core::ingest::PassPhase::Reading => "reading".into(),
            ken_core::ingest::PassPhase::Writing => "writing pages".into(),
        };
        let file = raw.rsplit('/').next().unwrap_or(raw);
        let waiting = INGEST_WAITING.lock().unwrap().contains(id);
        running.push(ken_core::throttle::Job { id: 0, kind: "ingest".into(), label: format!("{name}: {file}, {what}"), done: 0, total: 0, waiting });
    }
    let mut queued = Vec::new();
    for (id, name, root) in &members {
        let Ok(db) = Db::open_read_only(&base, *id) else { continue };
        if let Ok(h) = db.index_health() {
            if h.queued > 0 {
                queued.push(QueuedDto { kind: "index".into(), label: format!("{name}: files to read"), count: h.queued as usize });
            }
            if h.chunks > h.embedded && !h.model_missing && semantic_builds().lock().unwrap_or_else(|p| p.into_inner()).contains_key(id) {
                queued.push(QueuedDto { kind: "meaning".into(), label: format!("{name}: passages to read for meaning"), count: (h.chunks - h.embedded) as usize });
            }
        }
        if ken_core::ingest::has_inbox(root) {
            let waiting = ken_core::ingest::waiting_new(root, &db).map(|w| w.len()).unwrap_or(0);
            if waiting > 0 {
                queued.push(QueuedDto { kind: "ingest".into(), label: format!("{name}: sources in the inbox"), count: waiting });
            }
            for (raw, why) in ken_core::ingest::failures(&db).unwrap_or_default() {
                failed.push(ken_core::throttle::Failure {
                    kind: "ingest".into(),
                    label: raw.rsplit('/').next().unwrap_or(&raw).to_string(),
                    reason: why,
                    at: 0,
                });
            }
        }
    }
    let build = kb.and_then(|kb| Db::open_read_only(&base, kb.1).ok()).and_then(|db| ken_core::buildqueue::load(&db).ok().flatten());
    let build = build.map(|q| ken_core::buildqueue::progress(&q));
    if let Some(b) = &build {
        if b.queued > 0 {
            queued.push(QueuedDto { kind: "build".into(), label: "building the library".into(), count: b.queued });
        }
        for (label, why) in &b.failed {
            failed.push(ken_core::throttle::Failure { kind: "build".into(), label: label.clone(), reason: why.clone(), at: 0 });
        }
    }
    Ok(BackgroundStatusDto { throttle: snapshot, running, queued, failed, build })
}

/// Set the one background throttle (knowledge-core decision 34): the pace,
/// Gentle, Normal or Fast, and Pause. Saved in `settings.json` and in force
/// at once for every background job.
#[tauri::command(async)]
fn background_set(state: State<SharedState>, pace: ken_core::throttle::Pace, paused: bool) -> CmdResult<()> {
    let setting = ken_core::throttle::Setting { pace, paused };
    let (base_dir, mut settings) = {
        let guard = state.lock().unwrap();
        (guard.base_dir.clone(), guard.app_settings.clone())
    };
    settings.background = Some(setting);
    settings.save(&base_dir).map_err(err)?;
    state.lock().unwrap().app_settings = settings;
    ken_core::throttle::set(setting);
    Ok(())
}

/// Cap on how many pages of a scanned PDF are OCR'd. Rasterizing + recognizing
/// every page is expensive; the first pages carry the searchable gist, and the
/// cap bounds the cost of a pathological 500-page scan.
const MAX_OCR_PAGES: usize = 50;

/// One background OCR worker per open project, beside the background worker
/// and independent of it. It drains `ocr_pending`: reads each queued file's bytes, runs the OS's OCR
/// (`ken_core::ocr`: Vision on macOS, Windows.Media.Ocr on Windows), stores
/// the recognized regions, merges the text into
/// `contents` (→ FTS, so search picks it up), and emits a coalesced
/// `index-updated`. Errors are recorded with bounded retries; the queue is
/// self-healing (errored-with-budget rows return to `pending` when it goes
/// idle). Never runs inline in a scan — OCR is slow and must stay off that path.
fn ocr_worker(app: AppHandle, state: SharedState, project_id: uuid::Uuid, stop: Arc<AtomicBool>) {
    // No OCR engine on this machine: nothing is queued (scan checks the same),
    // and rows queued earlier wait for one rather than being marked done.
    if !ken_core::ocr::available() {
        return;
    }
    ken_core::ocr::init_worker_thread();
    let mut last_emit = Instant::now() - Duration::from_secs(1);
    let mut pending_emit = false;
    // Own connection, opened once and reused; dropped when the project closes.
    let mut db: Option<Db> = None;
    while !stop.load(Ordering::SeqCst) {
        // Resolve base + root under the lock, then drop it before the (slow)
        // Vision pass so IPC stays responsive.
        // Own project by id, not an arbitrary map entry: picking
        // `values().next()` and bailing unless it matched meant only one
        // project's OCR ever ran.
        let (base, root) = {
            let guard = state.lock().unwrap();
            let Some(active) = guard.members.get(&project_id) else {
                return; // project closed — this worker is done
            };
            (guard.base_dir.clone(), active.project.root.clone())
        };
        if db.is_none() {
            match Db::open(&base, project_id) {
                Ok(mut d) => {
                    // Files marked done before this OS had an engine (Windows
                    // until now) are read once, now that it has one.
                    let _ = d.requeue_ocr_for_engine(ken_core::ocr::ENGINE, ken_core::ocr::ENGINE_BEFORE_STAMP);
                    db = Some(d)
                }
                Err(_) => {
                    std::thread::sleep(Duration::from_secs(2));
                    continue;
                }
            }
        }
        let db = db.as_mut().expect("opened above");
        let Ok(next) = db.next_pending_ocr() else {
            std::thread::sleep(Duration::from_secs(2));
            continue;
        };
        let Some((rel, hash)) = next else {
            // Queue empty: flush a trailing throttled emit, self-heal errored
            // rows that still have retries, then idle.
            if pending_emit {
                emit_index_updated(&app, project_id, ScanStats::default());
                last_emit = Instant::now();
                pending_emit = false;
            }
            let _ = db.requeue_errored_ocr();
            std::thread::sleep(Duration::from_secs(2));
            continue;
        };

        let abs = root.join(&rel);
        // A file that vanished or turned back into a cloud placeholder can't be
        // OCR'd right now: record it as an error (bounded retries) and move on.
        if cloud::is_placeholder(&abs) {
            let _ = db.mark_ocr_error(&rel, &hash, "cloud placeholder — bytes not local");
            continue;
        }
        let bytes = match std::fs::read(&abs) {
            Ok(b) => b,
            Err(e) => {
                let _ = db.mark_ocr_error(&rel, &hash, &format!("read failed: {e}"));
                continue;
            }
        };
        if bytes.len() as i64 > scan::MAX_OCR_IMAGE_BYTES && !is_pdf(&rel) {
            let _ = db.mark_ocr_error(&rel, &hash, "image too large for OCR");
            continue;
        }
        // OCR runs on this computer, so it obeys the one background throttle
        // (knowledge-core decision 34): it waits while paused and rests by
        // the pace after each file.
        let halt = || stop.load(Ordering::SeqCst);
        if !ken_core::throttle::wait_unpaused(&halt) {
            return;
        }
        let started = Instant::now();
        let result = {
            let _running = ken_core::throttle::begin("ocr", &rel);
            if is_pdf(&rel) {
                ken_core::ocr::ocr_pdf_bytes(&bytes, MAX_OCR_PAGES)
            } else {
                ken_core::ocr::ocr_image_bytes(&bytes)
            }
        };
        ken_core::throttle::rest_after(started.elapsed(), &halt);
        match result {
            Ok(regions) => {
                let rows: Vec<ken_core::db::OcrRegionRow> = regions
                    .into_iter()
                    .map(|r| ken_core::db::OcrRegionRow {
                        page: r.page as i64,
                        text: r.text,
                        bbox: r.bbox,
                    })
                    .collect();
                if db.mark_ocr_done(&rel, &hash, &rows).is_ok() {
                    pending_emit = true;
                    if last_emit.elapsed() >= Duration::from_millis(750) {
                        emit_index_updated(&app, project_id, ScanStats::default());
                        last_emit = Instant::now();
                        pending_emit = false;
                    }
                }
                // Loop straight to the next queued file.
            }
            Err(e) => {
                let _ = db.mark_ocr_error(&rel, &hash, &e.to_string());
                // Brief backoff so a bad run doesn't spin.
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

/// True for a `.pdf` rel path (OCR routes PDFs through the page rasterizer,
/// everything else through the single-image path).
fn is_pdf(rel: &str) -> bool {
    rel.rsplit('.').next().is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

// ---------- chat commands ----------

#[tauri::command(async)]
fn list_chats(state: State<SharedState>) -> CmdResult<Vec<ChatRow>> {
    let guard = state.lock().unwrap();
    let active = member(&guard, None)?;
    let result = active.chat_db.lock().unwrap().list_chats().map_err(err);
    result
}

#[tauri::command(async)]
fn chat_transcript(state: State<SharedState>, chat_id: String) -> CmdResult<Vec<ChatMessage>> {
    let guard = state.lock().unwrap();
    let active = member(&guard, None)?;
    let result = active.chat_db.lock().unwrap().chat_messages(&chat_id).map_err(err);
    result
}

#[tauri::command(async)]
fn create_chat(app: AppHandle, state: State<SharedState>) -> CmdResult<ChatRow> {
    let guard = state.lock().unwrap();
    let active = member(&guard, None)?;
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
        // Bound on the first message from the scope the UI is showing —
        // see `send_chat_message`. Creating a chat is not yet a commitment
        // to a scope, so an abandoned empty chat never records one.
        scope: None,
    };
    active.chat_db.lock().unwrap().upsert_chat(&row).map_err(err)?;
    emit_member(&app, active.project.config.id, "chat-updated", row.clone());
    Ok(row)
}

/// Send a chat message.
///
/// `scope` widens what the Claude Code session may READ: `None`/absent is
/// this project only (unchanged behavior), `"all"` is every resolvable
/// workspace member, and any other value names a group. Writes stay
/// pinned to the focused project either way — see
/// `chat::build_scope_preamble` for why the asymmetry, and note that the
/// session's cwd is unchanged, so the focused project remains the natural
/// write target rather than merely the instructed one.
///
/// Scope is bound to the CHAT (schema v13's `chats.scope`), not to the
/// send: the row's value wins, and the `scope` argument only seeds a chat
/// that has none yet (its first message). Changing the Home picker later
/// therefore cannot silently re-scope a conversation that is already
/// under way, and an "all projects" chat is still one after a restart.
#[tauri::command(async)]
fn send_chat_message(
    app: AppHandle,
    state: State<SharedState>,
    chat_id: String,
    text: String,
    open_files: Option<Vec<String>>,
    focused_file: Option<String>,
    scope: Option<String>,
) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let owner = chat_owner(&guard, &chat_id);
    let active = member(&guard, owner)?;
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
        emit_member(&app, active.project.config.id, "chat-message", ChatMessage {
            id,
            chat_id: chat_id.clone(),
            role: "user".into(),
            content: text.clone(),
            created_at: now,
        });
        // Bind scope on the first message, so a chat opened while Home said
        // "all projects" stays an all-projects chat for its whole life.
        if row.scope.is_none() {
            if let Some(s) = scope.as_deref().filter(|s| !s.trim().is_empty()) {
                let _ = db.set_chat_field(&chat_id, ChatField::Scope, s);
            }
        }
        if row.title == "New chat" {
            let title: String = text.chars().take(40).collect();
            let _ = db.set_chat_field(&chat_id, ChatField::Title, title.trim());
            if let Ok(Some(updated)) = db.get_chat(&chat_id) {
                emit_member(&app, active.project.config.id, "chat-updated", updated);
            }
        }
        (had_messages, row)
    };
    // ken-memory task 2.3: snapshot what's needed for the `## Memories`
    // injection (project root, workspace root if open) while the guard is
    // still held — `active`/`guard` don't outlive this block — then do the
    // actual file reads after dropping it (lock-audit discipline: never do
    // filesystem IO across the global lock).
    let kenmem_ctx = Some((
        active.project.root.clone(),
        guard.workspace.as_ref().map(|w| w.ws.root.clone()),
    ));

    // Cross-project scope: collect the sibling roots this session may read,
    // under the same lock, and trust them after dropping it (trusting
    // touches `~/.claude.json`, so it is IO and must not span the lock).
    let focused_id = active.project.config.id;
    let focused_name = active.project.config.name.clone();
    let focused_root = active.project.root.display().to_string();
    // The chat's own stored scope wins; the argument seeds a chat that has
    // none yet, so scope is fixed by the first message and stable after.
    let scope = row.scope.clone().or(scope);
    let scope_siblings: Vec<(String, String)> = match (scope.as_deref(), guard.workspace.as_ref()) {
        (None, _) | (_, None) => Vec::new(),
        (Some(kind), Some(ws)) => {
            // "all" spans every resolvable member; anything else names a
            // group (manifest or group-folder), whose member list filters
            // the same set.
            let allowed: Option<Vec<String>> =
                (kind != "all").then(|| ws.ws.config.effective_group_members(kind));
            // A named scope that resolves to nothing is an error, not a
            // quiet fallback: silently sending the chat with single-project
            // scope while the UI says "asking about <group>" hands the
            // model the wrong world without telling anyone.
            if allowed.as_ref().is_some_and(|list| list.is_empty()) {
                return Err(format!("scope \"{kind}\" matches no workspace members"));
            }
            ws.ws
                .members
                .iter()
                .filter(|m| allowed.as_ref().is_none_or(|list| list.contains(&m.name)))
                .filter_map(|m| match &m.status {
                    ken_core::workspace::MemberStatus::Ok(p) if p.config.id != focused_id => {
                        Some((p.config.name.clone(), p.root.display().to_string()))
                    }
                    _ => None,
                })
                .collect()
        }
    };
    let group_label = scope.as_deref().filter(|s| *s != "all").map(str::to_string);
    // Every repo of the workspace, for linking a file read in any of them.
    let cite_members: Vec<(String, uuid::Uuid)> = guard
        .workspace
        .as_ref()
        .map(|ws| {
            ws.ws
                .members
                .iter()
                .filter_map(|m| match &m.status {
                    ken_core::workspace::MemberStatus::Ok(p) => Some((m.name.clone(), p.config.id)),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    drop(guard);

    // No folder trust here: the chat runs in print mode, which never shows
    // Claude Code's trust dialog (see `chat::ensure_folder_trusted`).

    // Design D4: workspace-scope memories + the focused project's own,
    // ordered/budgeted by `memory::build_injection`. Composed here rather
    // than inside `chat::build_context_preamble` itself — that function
    // lives in `ken-core/chat.rs`, outside this task's touch-boundary — so
    // this is "extend build_context_preamble's USAGE with build_injection
    // output", the closest available lever without editing ken-core.
    let memories_block = kenmem_ctx.and_then(|(project_root, ws_root)| {
        let mut mems = memory::list_memories(&memory::project_memory_dir(&project_root)).unwrap_or_default();
        if let Some(ws_root) = ws_root {
            mems.extend(memory::list_memories(&memory::workspace_memory_dir(&ws_root)).unwrap_or_default());
        }
        let block = memory::build_injection(&mems);
        (!block.is_empty()).then_some(block)
    });

    // The stored transcript keeps the user's raw text; the CLI additionally
    // gets a weak-hint preamble naming the files open on screen (when any),
    // clearly caveated as "not necessarily relevant".
    let open = open_files.unwrap_or_default();
    let file_preamble = chat::build_context_preamble(focused_file.as_deref(), &open);
    let mut prompt = text.clone();
    if let Some(preamble) = file_preamble {
        prompt = format!("{preamble}\n\n{prompt}");
    }
    // Scope goes ABOVE the open-files hint: which projects are in play
    // frames everything below it, and the files hint is explicitly the
    // weakest signal in the prompt.
    if let Some(scope_preamble) = chat::build_scope_preamble(
        &focused_name,
        &focused_root,
        &scope_siblings,
        group_label.as_deref(),
    ) {
        prompt = format!("{scope_preamble}\n\n{prompt}");
    }
    if let Some(cite) = chat::build_cite_preamble(&cite_members) {
        prompt = format!("{cite}\n\n{prompt}");
    }
    if let Some(mems) = memories_block {
        prompt = format!("{mems}\n\n{prompt}");
    }
    engine_arc
        .send(&chat_id, &prompt, resume, row.model.as_deref())
        .map_err(err)
}

/// Answer a pending AskUserQuestion card. `answers` is keyed by the exact
/// question text; a multi-select answer is a comma-separated list of labels and
/// a free-text "Other" answer is just the typed string.
/// Answer an edit Claude proposed in chat, after the person read its diff.
/// `decision` is `accepted` (the CLI makes the edit as asked), `declined`
/// (nothing is written), or `partial`: Ken writes `merged` (the file with only
/// the accepted changes) itself, and Claude is told which changes, listed in
/// `declined_changes`, were left out. A partial write refuses a file that
/// changed since the proposal.
#[tauri::command(async)]
fn answer_edit_proposal(
    app: AppHandle,
    state: State<SharedState>,
    chat_id: String,
    message_id: i64,
    decision: String,
    merged: Option<String>,
    declined_changes: Vec<String>,
) -> CmdResult<()> {
    let mut guard = state.lock().unwrap();
    let owner = chat_owner(&guard, &chat_id);
    let active = member_mut(&mut guard, owner)?;
    let engine_arc = active.chat_engine.as_ref().ok_or(ken_core::runner::MISSING_CLAUDE_HELP)?.clone();
    let msg = active
        .chat_db
        .lock()
        .unwrap()
        .chat_messages(&chat_id)
        .map_err(err)?
        .into_iter()
        .find(|m| m.id == message_id)
        .ok_or("edit not found")?;
    if msg.role != "edit" {
        return Err("that message is not an edit".into());
    }
    let mut proposal: ken_core::chat::EditProposal =
        serde_json::from_str(&msg.content).map_err(|e| format!("bad edit payload: {e}"))?;
    if proposal.decision.is_some() {
        return Err("this edit has already been answered".into());
    }
    let file = proposal.rel_path.clone().unwrap_or_else(|| proposal.path.clone());
    let list = |v: &[String]| v.iter().map(|c| format!("- {c}")).collect::<Vec<_>>().join("\n");
    let (allow, note) = match decision.as_str() {
        "accepted" => (true, format!("The person accepted every change to {file}.")),
        "declined" => (
            false,
            format!("The person reviewed your edit to {file} and declined it; nothing was written. Do not retry it unless they ask."),
        ),
        "partial" => {
            let merged = merged.ok_or("a partial accept needs the merged text")?;
            let on_disk = std::fs::read_to_string(&proposal.path).unwrap_or_default();
            if on_disk.replace("\r\n", "\n") != proposal.base.replace("\r\n", "\n") {
                return Err(format!("{file} changed after Claude proposed this edit; decline it and ask again."));
            }
            std::fs::write(&proposal.path, &merged).map_err(err)?;
            if let Some(rel) = &proposal.rel_path {
                let _ = scan::refresh_path(&active.project, &mut active.db, rel);
            }
            (
                false,
                format!(
                    "The person reviewed your edit to {file} change by change. Ken wrote the changes they accepted and left out these:\n{}\nThe file now holds the accepted version: read it again before editing it further, and do not retry the declined changes unless they ask.",
                    list(&declined_changes)
                ),
            )
        }
        other => return Err(format!("unknown decision {other}")),
    };
    engine_arc.answer_edit(&chat_id, &proposal, allow, &note).map_err(err)?;

    proposal.decision = Some(decision);
    proposal.note = Some(note);
    let content = serde_json::to_string(&proposal).map_err(err)?;
    let project_id = active.project.config.id;
    let now = engine::now_epoch();
    let mut db = active.chat_db.lock().unwrap();
    db.update_chat_message_content(message_id, &content).map_err(err)?;
    emit_member(&app, project_id, "chat-message", ChatMessage {
        id: message_id,
        chat_id: chat_id.clone(),
        role: "edit".into(),
        content,
        created_at: msg.created_at,
    });
    // The engine said "working" before it wrote the answer; setting it again
    // here could land after the turn's "done" and leave it stuck.
    let _ = db.touch_chat(&chat_id, now);
    if let Ok(Some(row)) = db.get_chat(&chat_id) {
        let _ = app.emit("chat-updated", row);
    }
    if let Some(rel) = proposal.rel_path {
        emit_index_updated(&app, project_id, ScanStats { changed_paths: vec![rel], ..Default::default() });
    }
    Ok(())
}

#[tauri::command(async)]
fn answer_chat_question(
    app: AppHandle,
    state: State<SharedState>,
    chat_id: String,
    message_id: i64,
    answers: std::collections::HashMap<String, String>,
) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let owner = chat_owner(&guard, &chat_id);
    let active = member(&guard, owner)?;
    let engine_arc = active
        .chat_engine
        .as_ref()
        .ok_or(ken_core::runner::MISSING_CLAUDE_HELP)?
        .clone();

    let msg = active
        .chat_db
        .lock()
        .unwrap()
        .chat_messages(&chat_id)
        .map_err(err)?
        .into_iter()
        .find(|m| m.id == message_id)
        .ok_or("question not found")?;
    if msg.role != "question" {
        return Err("that message is not a question".into());
    }
    let mut payload: serde_json::Value =
        serde_json::from_str(&msg.content).map_err(|e| format!("bad question payload: {e}"))?;
    // A second answer would be written to a request the CLI already consumed.
    if payload.get("answers").is_some_and(|a| !a.is_null()) {
        return Err("this question has already been answered".into());
    }
    let request_id = payload["requestId"].as_str().unwrap_or_default().to_string();
    let tool_use_id = payload["toolUseId"].as_str().unwrap_or_default().to_string();
    let questions = payload["questions"].clone();
    let answers = serde_json::to_value(&answers).map_err(err)?;

    engine_arc
        .answer_question(&chat_id, &request_id, &tool_use_id, questions, answers.clone())
        .map_err(err)?;

    let now = engine::now_epoch();
    payload["answers"] = answers;
    let content = payload.to_string();
    let mut db = active.chat_db.lock().unwrap();
    db.update_chat_message_content(message_id, &content).map_err(err)?;
    let _ = app.emit("chat-message", ChatMessage {
        id: message_id,
        chat_id: chat_id.clone(),
        role: "question".into(),
        content,
        created_at: msg.created_at,
    });
    // The engine said "working" before it wrote the answer; setting it again
    // here could land after the turn's "done" and leave it stuck.
    let _ = db.touch_chat(&chat_id, now);
    if let Ok(Some(row)) = db.get_chat(&chat_id) {
        let _ = app.emit("chat-updated", row);
    }
    Ok(())
}

#[tauri::command]
fn rename_chat(app: AppHandle, state: State<SharedState>, chat_id: String, title: String) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let active = member(&guard, None)?;
    let mut db = active.chat_db.lock().unwrap();
    db.set_chat_field(&chat_id, ChatField::Title, title.trim()).map_err(err)?;
    if let Ok(Some(row)) = db.get_chat(&chat_id) {
        emit_member(&app, active.project.config.id, "chat-updated", row);
    }
    Ok(())
}

#[tauri::command]
fn set_chat_pinned(app: AppHandle, state: State<SharedState>, chat_id: String, pinned: bool) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let active = member(&guard, None)?;
    let mut db = active.chat_db.lock().unwrap();
    db.set_chat_flag(&chat_id, ChatFlag::Pinned, pinned).map_err(err)?;
    if let Ok(Some(row)) = db.get_chat(&chat_id) {
        emit_member(&app, active.project.config.id, "chat-updated", row);
    }
    Ok(())
}

/// Set a chat's model to a stable tier alias (or clear to the CLI default when
/// `model` is None/empty/unrecognized). Applies to the next message/session;
/// any live process keeps its current model until it respawns.
#[tauri::command]
fn set_chat_model(app: AppHandle, state: State<SharedState>, chat_id: String, model: Option<String>) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let active = member(&guard, None)?;
    // Store only a validated alias; anything else clears to the default so we
    // never persist a string we'd refuse to pass to the CLI.
    let alias = model.as_deref().and_then(chat::valid_model_alias);
    let mut db = active.chat_db.lock().unwrap();
    db.set_chat_model(&chat_id, alias).map_err(err)?;
    if let Ok(Some(row)) = db.get_chat(&chat_id) {
        emit_member(&app, active.project.config.id, "chat-updated", row);
    }
    Ok(())
}

#[tauri::command]
fn archive_chat(app: AppHandle, state: State<SharedState>, chat_id: String) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let active = member(&guard, None)?;
    if let Some(engine) = &active.chat_engine {
        engine.stop(&chat_id);
    }
    close_terminal(active, &chat_id);
    let mut db = active.chat_db.lock().unwrap();
    db.set_chat_flag(&chat_id, ChatFlag::Archived, true).map_err(err)?;
    if let Ok(Some(row)) = db.get_chat(&chat_id) {
        emit_member(&app, active.project.config.id, "chat-updated", row);
    }
    Ok(())
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct PtyChunk {
    chat_id: String,
    data: String, // base64
}

fn close_terminal(active: &MemberRuntime, chat_id: &str) {
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
    let active = member(&guard, None)?;

    // One process per session: stop conversation mode first.
    if let Some(engine) = &active.chat_engine {
        engine.stop(&chat_id);
    }
    close_terminal(active, &chat_id);

    let emit_app = app.clone();
    let id_for_data = chat_id.clone();
    let project_id = active.project.config.id;
    let on_data = move |bytes: &[u8]| {
        emit_member(&emit_app, project_id, "chat-pty-data", PtyChunk {
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

    let pty = chat::attach_terminal(&binary, &active.project.root, &chat_id, resume, row.model.as_deref(), on_data_dup(app.clone(), project_id, chat_id.clone()))
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
                    emit_member(&status_app, project_id, "chat-updated", row);
                }
            }
        });
    }
    Ok(())
}

/// Second copy of the data emitter for the spawn path (the first was moved
/// into the registry-attach attempt).
fn on_data_dup(app: AppHandle, project_id: uuid::Uuid, chat_id: String) -> impl Fn(&[u8]) + Send + 'static {
    use base64::Engine as _;
    move |bytes: &[u8]| {
        emit_member(&app, project_id, "chat-pty-data", PtyChunk {
            chat_id: chat_id.clone(),
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
        });
    }
}

#[tauri::command]
fn leave_terminal_mode(state: State<SharedState>, chat_id: String) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let active = member(&guard, None)?;
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
    let active = member(&guard, None)?;
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
    let active = member(&guard, None)?;
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
#[tauri::command(async)]
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
    let active = member(&guard, None)?;
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
        // Research runs in one project's own tree.
        scope: None,
    };
    {
        let mut db = active.chat_db.lock().unwrap();
        db.upsert_chat(&row).map_err(err)?;
        let note = format!("Researching — the report will land at {report_rel}.");
        let id = db.append_chat_message(&chat_id, "activity", &note, now).unwrap_or(0);
        emit_member(&app, active.project.config.id, "chat-updated", row.clone());
        emit_member(&app, active.project.config.id, "chat-message", ChatMessage {
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
    let project_id = active.project.config.id;
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
                emit_member(&worker_app, project_id, "chat-message", ChatMessage {
                    id,
                    chat_id: sid.clone(),
                    role: "activity".into(),
                    content: note,
                    created_at: now,
                });
            }
            if let Ok(Some(row)) = db.get_chat(&sid) {
                emit_member(&worker_app, project_id, "chat-updated", row);
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
    let active = member(&guard, None)?;
    if let Some(token) = active.research.lock().unwrap().get(&chat_id) {
        token.cancel();
    }
    Ok(())
}

/// Where can a report go? `research` first — always, even before the
/// folder exists — then the project's existing top-level folders.
#[tauri::command(async)]
fn research_output_options(state: State<SharedState>) -> CmdResult<Vec<String>> {
    let guard = state.lock().unwrap();
    let active = member(&guard, None)?;
    let mut options = vec!["research".to_string()];
    let excluded = active.project.excluded_folders();
    if let Ok(entries) = std::fs::read_dir(&active.project.root) {
        let mut dirs: Vec<String> = entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|name| {
                !name.starts_with('.')
                    && !ken_core::scan::is_junk_dir_name(name)
                    && name != "research"
                    && !ken_core::project::in_folders(&excluded, name)
            })
            .collect();
        dirs.sort();
        options.extend(dirs);
    }
    Ok(options)
}

/// Copy the bundled ken-mcp sidecar over `dest` when the bytes differ, so the
/// copy the install script put on PATH (`~/.local/bin/ken-mcp` from
/// install.sh, `%LOCALAPPDATA%\Programs\ken-mcp\ken-mcp.exe` from
/// install.ps1) stays in sync across auto-updates. Returns whether a copy
/// happened.
fn refresh_ken_mcp_from(bundled: &std::path::Path, dest: &std::path::Path) -> std::io::Result<bool> {
    let new_bytes = std::fs::read(bundled)?;
    if let Ok(old_bytes) = std::fs::read(dest) {
        if old_bytes == new_bytes {
            return Ok(false);
        }
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Write to a temp name then rename, so a running ken-mcp keeps its
    // old inode and the swap is atomic.
    let tmp = dest.with_extension("tmp");
    std::fs::write(&tmp, &new_bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    if let Err(e) = std::fs::rename(&tmp, dest) {
        // Windows will not replace an exe that is running (an agent has it
        // open), but it will rename it: move it aside, then put the new one
        // in place. The old file goes once nothing runs it.
        #[cfg(windows)]
        {
            let old = dest.with_extension("old");
            let _ = std::fs::remove_file(&old);
            if std::fs::rename(dest, &old).is_ok() && std::fs::rename(&tmp, dest).is_ok() {
                let _ = std::fs::remove_file(&old);
                return Ok(true);
            }
        }
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(true)
}

/// Where the install script put ken-mcp on PATH, and the bundled sidecar
/// next to the app to copy over it. On Windows only a copy install.ps1 made
/// is kept current; Ken's own chat uses the sidecar directly.
fn ken_mcp_copy_paths() -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    #[cfg(windows)]
    {
        let local = std::env::var_os("LOCALAPPDATA")?;
        let dest = std::path::PathBuf::from(local).join("Programs").join("ken-mcp").join("ken-mcp.exe");
        dest.exists().then(|| (exe_dir.join("ken-mcp.exe"), dest))
    }
    #[cfg(not(windows))]
    {
        let home = std::env::var_os("HOME")?;
        Some((exe_dir.join("ken-mcp"), std::path::PathBuf::from(home).join(".local/bin/ken-mcp")))
    }
}

/// Locate the bundled sidecar (next to the app executable) and refresh the
/// user's copy. Release builds only; failures are logged, never fatal.
fn refresh_ken_mcp() {
    if cfg!(debug_assertions) {
        return;
    }
    let Some((bundled, dest)) = ken_mcp_copy_paths() else { return };
    if !bundled.exists() {
        return;
    }
    match refresh_ken_mcp_from(&bundled, &dest) {
        Ok(true) => eprintln!("ken-mcp refreshed at {}", dest.display()),
        Ok(false) => {}
        Err(err) => eprintln!("warning: could not refresh ken-mcp: {err}"),
    }
}

/// Bring the main window back from the tray: unhide it, restore it if the
/// user minimized it, and focus it. Shared by the tray's left click and its
/// "Open Ken" item so both do exactly the same thing.
fn reveal_main_window(app: &tauri::AppHandle) {
    use tauri::Manager;
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

pub fn run() {
    // Before any model loads (see `vulkan`): no Vulkan driver, CPU models.
    #[cfg(all(windows, target_env = "msvc"))]
    vulkan::prefer_cpu_without_loader();
    let base_dir = ken_core::registry::default_base_dir()
        .expect("no OS data directory available");
    // Hand the app-data dir to the on-device LLM so it can resolve/load the
    // installed model; this is the only wiring that activates the local path.
    ken_core::local_llm::init(base_dir.clone());
    let app_settings = ken_core::settings::AppSettings::load(&base_dir);
    // The one background throttle, before any background work starts.
    ken_core::throttle::set(app_settings.throttle());
    // The graphics card setting, before any model loads.
    ken_core::compute::set_use_gpu(use_gpu_setting(&app_settings));
    let state: SharedState = Arc::new(Mutex::new(AppState {
        base_dir,
        hooks: None,
        members: std::collections::HashMap::new(),
        focused: None,
        app_settings,
        model_downloads: Arc::new(Mutex::new(std::collections::HashSet::new())),
        qa_gen: Arc::new(AtomicU64::new(0)),
        record: Arc::new(Mutex::new(None)),
        workspace: None,
        ingest_gate: Arc::new(IngestGate::new(WORKSPACE_INGEST_CONCURRENCY)),
        memory_distill_running: Arc::new(AtomicBool::new(false)),
        memory_distill_candidates: Arc::new(Mutex::new(Vec::new())),
        task_recent_writes: Arc::new(Mutex::new(std::collections::HashMap::new())),
        digest_running: Arc::new(AtomicBool::new(false)),
    }));

    tauri::Builder::default()
        // First, so a second launch hands over to the running Ken (its window
        // comes back from the tray) before it sets up anything of its own.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| reveal_main_window(app)))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(state)
        .setup(|app| {
            std::thread::spawn(refresh_ken_mcp);
            #[allow(unused_imports)]
            use tauri::Manager;

            // Closing the window hides Ken to the tray rather than quitting
            // it (see `on_window_event` below). The file watchers that keep
            // each resident member's index current live in this process, so
            // quitting means nothing is indexed until Ken is opened again —
            // which is exactly the "it waits for you to open it" problem.
            // The tray is therefore the way back to the window, and its
            // "Quit Ken" item the only real exit.
            {
                use tauri::menu::{Menu, MenuItem};
                use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

                let show = MenuItem::with_id(app, "ken-show", "Open Ken", true, None::<&str>)?;
                let quit = MenuItem::with_id(app, "ken-quit", "Quit Ken", true, None::<&str>)?;
                let menu = Menu::with_items(app, &[&show, &quit])?;

                let mut tray = TrayIconBuilder::with_id("ken-tray")
                    .tooltip("Ken — watching your workspaces")
                    .menu(&menu)
                    .show_menu_on_left_click(false)
                    .on_menu_event(|app, event| match event.id.as_ref() {
                        "ken-show" => reveal_main_window(app),
                        "ken-quit" => app.exit(0),
                        _ => {}
                    })
                    .on_tray_icon_event(|tray, event| {
                        if let TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        } = event
                        {
                            reveal_main_window(tray.app_handle());
                        }
                    });
                if let Some(icon) = app.default_window_icon() {
                    tray = tray.icon(icon.clone());
                }
                tray.build(app)?;
            }
            Ok(())
        })
        .on_window_event(|window, event| match event {
            // The close button hides Ken instead of quitting, so the
            // watchers keep indexing while the window is away. Quit lives
            // in the tray menu.
            tauri::WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                let _ = window.hide();
            }
            // Focus = "the user is back" — the moment to fetch teammates'
            // work and to check whether today's digest is due.
            tauri::WindowEvent::Focused(true) => {
                use tauri::Manager;
                let state = window.state::<SharedState>();
                // Every open member, not an arbitrary one: "the user is back"
                // is true for the whole workspace, and pulling only whichever
                // member the map happened to yield left the others stale.
                let syncs: Vec<_> = {
                    let guard = state.lock().unwrap();
                    guard.members.values().map(|a| a.sync.clone()).collect()
                };
                for sync in syncs {
                    sync.pull_now();
                }
                maybe_generate_digest(window.app_handle(), state.inner());
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            list_projects,
            create_project,
            open_project,
            open_workspace,
            create_workspace,
            workspace_overview,
            focus_project,
            close_workspace,
            list_recent_workspaces,
            forget_workspace,
            current_project,
            set_folder_selection,
            get_tree,
            set_project_feature,
            set_global_feature,
            list_features,
            get_semantic_index,
            read_file,
            read_file_bytes,
            is_cloud_only,
            hydrate_file,
            save_file,
            save_file_bytes,
            write_new_file,
            create_unique_folder,
            file_meta,
            extracted_text,
            get_ocr_regions,
            reindex,
            move_file,
            delete_file,
            create_folder,
            create_document,
            import_begin,
            import_classify,
            import_commit,
            import_cancel,
            open_external,
            reveal_in_folder,
            open_web_url,
            file_mtime,
            media_src,
            video_transcript,
            generate_transcript,
            model_status,
            list_models,
            download_model,
            remove_model,
            set_model_selection,
            resolve_review_item,
            confirm_finding,
            raise_issue,
            accept_analysis,
            loose_proposals,
            analysis_proposals,
            analyze_source_again,
            ignore_file,
            unignore_file,
            list_ignored,
            unread_files,
            mark_seen,
            mark_seen_under,
            mark_all_seen,
            sync_status,
            set_sync_auto,
            set_project_kind,
            set_project_index,
            index_health,
            page_links,
            graph_related,
            raid_register,
            code_file,
            code_usages,
            run_drift_now,
            setup_rescan,
            setup_propose_repos,
            setup_confirm_repos,
            set_project_description,
            ingest_overview,
            knowledge_base_id,
            embedding_state,
            gpu_info,
            set_use_gpu,
            ingest_card,
            ingest_add,
            ingest_add_bytes,
            ingest_add_text,
            ingest_add_chat,
            ingest_retry,
            ingest_remove,
            files_banner,
            workspace_settings,
            knowledge_findings,
            workspace_save_ignores,
            workspace_save_weights,
            workspace_save_roles,
            workspace_save_controls,
            workspace_add_template_files,
            build_start,
            build_stop,
            build_set_at_once,
            background_status,
            background_set,
            setup_overlay_wiki,
            wiki_map_plan,
            wiki_map_apply,
            workspace_add_rule,
            ingest_now,
            ingest_seen,
            setup_create_wiki,
            apply_page_proposal,
            sync_now,
            sync_commit_others,
            resolve_conflict,
            resolve_conflict_copy,
            get_background_index,
            set_background_index,
            get_transcribe_on_index,
            set_transcribe_on_index,
            claude_doctor,
            mcp_info,
            quick_answer,
            look_for,
            warm_llm,
            route_search,
            list_chats,
            chat_transcript,
            create_chat,
            send_chat_message,
            answer_chat_question,
            answer_edit_proposal,
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
            record_input_devices,
            record_permissions,
            record_request_permission,
            record_open_settings,
            record_support,
            record_start,
            record_pause,
            record_resume,
            record_stop,
            record_cancel,
            distill_journal,
            resolve_distill_candidate,
            day_state,
            day_task_create,
            day_task_update,
            day_task_delete,
            ticket_tasks,
            day_digest,
            refresh_day_digest,
            day_digest_writing,
            resolve_page_link,
            ticket_set_status,
            workspace_groups,
            workspace_add_member,
            workspace_remove_member,
        ])
        .build(tauri::generate_context!())
        .expect("error while running Ken")
        .run(|_app, event| {
            // Clicking Ken in the Dock while its window is hidden in the tray.
            #[cfg(target_os = "macos")]
            {
                if let tauri::RunEvent::Reopen { .. } = event {
                    reveal_main_window(_app);
                }
            }
            if let tauri::RunEvent::Exit = event {
                // The listener goes with the app: ken-mcp then finds no file
                // and signals nothing.
                if let Some(state) = tauri::Manager::try_state::<SharedState>(_app) {
                    let guard = lock_tolerant(state.inner());
                    if let Some(h) = guard.hooks.as_ref() {
                        ken_core::hooks::remove_listener_file(&guard.base_dir, h.token());
                    }
                }
                // ggml (statically linked via llama-cpp-2 / whisper-rs) frees
                // its Metal device in a C++ static destructor, which
                // GGML_ASSERTs that no residency sets remain. Our LLM engine
                // lives in a process-lifetime static and is never dropped, so
                // letting exit() run those destructors aborts with SIGABRT on
                // every quit. Tauri has finished its own cleanup by this
                // event; skip atexit handlers entirely.
                #[cfg(unix)]
                {
                    use std::io::Write;
                    let _ = std::io::stdout().flush();
                    let _ = std::io::stderr().flush();
                    unsafe { libc::_exit(0) };
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_quick_answer_prompt_grounds_and_uses_chatml() {
        // The caller hands each source in with marks already stripped (the
        // excerpt/snippet fallback does that upstream), so the fixture
        // supplies clean text directly.
        let sources = vec![QaSource {
            cite: "Wiki:People.md".into(),
            address: "ken://x/People.md".into(),
            label: None,
            text: "Priya owns billing".into(),
        }];
        let p = local_quick_answer_prompt("who owns billing?", &sources);
        assert!(p.contains("<|im_start|>system"));
        assert!(p.contains("who owns billing?"));
        assert!(p.contains("Wiki:People.md: Priya owns billing"));
        assert!(p.contains("<|im_start|>assistant"));
    }

    /// A blank in Templates/ shows by its file name, not its placeholder
    /// title (end-to-end finding 7); a page keeps its own title.
    #[test]
    fn a_blank_shows_by_its_file_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("Templates")).unwrap();
        std::fs::write(dir.path().join("Templates/Design-Note.md"), "---
title: \"{{Design Question}}\"
---

# {{Design Question}}
").unwrap();
        std::fs::write(dir.path().join("Templates/Rule.md"), "# {{The rule}}
").unwrap();
        std::fs::write(dir.path().join("Saving.md"), "---
title: \"How worlds save\"
---
# Saving
").unwrap();
        assert_eq!(page_title(dir.path(), "Templates/Design-Note.md"), "Design-Note");
        assert_eq!(page_title(dir.path(), "Templates/Rule.md"), "Rule");
        assert_eq!(page_title(dir.path(), "Saving.md"), "How worlds save");
    }

    /// A quick answer is told each hit's label and that a binding ruling
    /// wins over the code it rules on (end-to-end finding 15), and its
    /// sources open in the repo they came from.
    #[test]
    fn a_quick_answer_weighs_each_hit_by_its_label() {
        let sources = vec![
            QaSource {
                cite: "tiny-game:src/world.py".into(),
                address: "ken://g/src/world.py".into(),
                label: Some("code · reference · 2026-10-14".into()),
                text: "def save_region(region): writes one file per region".into(),
            },
            QaSource {
                cite: "Wiki:Decisions/DECISIONS.md".into(),
                address: "ken://w/Decisions/DECISIONS.md".into(),
                label: Some("decision D-001 · binding · 2026-10-14".into()),
                text: "The whole world saves to a single file.".into(),
            },
        ];
        for p in [quick_answer_prompt("how is the world saved", &sources), local_quick_answer_prompt("how is the world saved", &sources)] {
            assert!(p.contains("- Wiki:Decisions/DECISIONS.md (decision D-001 · binding · 2026-10-14): The whole world"), "{p}");
            assert!(p.contains("- tiny-game:src/world.py (code · reference · 2026-10-14):"), "{p}");
            assert!(p.contains("A binding ruling is settled") && p.contains("say so and cite both"), "{p}");
        }
        let cited = qa_cited(vec!["Wiki:Decisions/DECISIONS.md".into(), "src/world.py".into(), "elsewhere.md".into()], &sources);
        assert_eq!(cited, vec!["ken://w/Decisions/DECISIONS.md", "ken://g/src/world.py", "elsewhere.md"]);
    }

    /// The trash call plus `deindex_removed` (the two halves of `delete_file`)
    /// must leave a file gone from BOTH disk and the index. Trashing a real path
    /// is exercised here so the recoverable-delete contract is covered end to end.
    #[test]
    fn delete_file_trashes_and_deindexes_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::create(dir.path(), "Fixture").unwrap();
        let mut db = Db::open(&dir.path().join("idx"), project.config.id).unwrap();

        let rel = "note.md";
        let abs = project.root.join(rel);
        std::fs::write(&abs, "Priya owns billing.").unwrap();
        // Index it the way the watcher/scan would, so it lands in the DB.
        scan::refresh_path(&project, &mut db, rel).unwrap();
        assert!(db.get_file(rel).unwrap().is_some(), "file should be indexed");

        // Same two steps delete_file runs, minus the Tauri State plumbing.
        trash_path(&abs).unwrap();
        deindex_removed(&project, &mut db, rel, false).unwrap();

        assert!(!abs.exists(), "file should be gone from disk (in the trash)");
        assert!(db.get_file(rel).unwrap().is_none(), "row should be gone from the index");
    }

    /// Deleting a folder drops its whole subtree from the index.
    #[test]
    fn delete_file_deindexes_a_folder_subtree() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::create(dir.path(), "Fixture").unwrap();
        let mut db = Db::open(&dir.path().join("idx"), project.config.id).unwrap();

        std::fs::create_dir(project.root.join("Meetings")).unwrap();
        let child = "Meetings/kickoff.md";
        std::fs::write(project.root.join(child), "Kickoff notes.").unwrap();
        scan::refresh_path(&project, &mut db, child).unwrap();
        assert!(db.get_file(child).unwrap().is_some());

        trash_path(&project.root.join("Meetings")).unwrap();
        deindex_removed(&project, &mut db, "Meetings", true).unwrap();

        assert!(!project.root.join("Meetings").exists());
        assert!(db.get_file(child).unwrap().is_none(), "child row should be dropped with the folder");
    }

    /// Moving a folder must reconcile the index in place: rows appear at the new
    /// paths with their content intact, nothing is left behind at the old paths,
    /// and search still finds the text. The old implementation cleared the whole
    /// DB and rescanned the project, which froze the app and threw away every
    /// extraction/OCR/transcript in the project just to rename a directory.
    #[test]
    fn move_file_reindexes_a_folder_subtree_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::create(dir.path(), "Fixture").unwrap();
        let mut db = Db::open(&dir.path().join("idx"), project.config.id).unwrap();

        std::fs::create_dir_all(project.root.join("Meetings/Q3")).unwrap();
        std::fs::write(project.root.join("Meetings/kickoff.md"), "Kickoff notes zebra.").unwrap();
        std::fs::write(project.root.join("Meetings/Q3/review.md"), "Review notes zebra.").unwrap();
        // A sibling folder sharing the name as a string prefix must not move.
        std::fs::create_dir_all(project.root.join("Meetings-old")).unwrap();
        std::fs::write(project.root.join("Meetings-old/stale.md"), "Stale zebra.").unwrap();
        for rel in ["Meetings/kickoff.md", "Meetings/Q3/review.md", "Meetings-old/stale.md"] {
            scan::refresh_path(&project, &mut db, rel).unwrap();
        }
        let before = db.file_count().unwrap();

        // Same two steps move_file runs for a folder, minus the Tauri plumbing.
        std::fs::create_dir_all(project.root.join("Archive")).unwrap();
        std::fs::rename(project.root.join("Meetings"), project.root.join("Archive/Meetings"))
            .unwrap();
        reindex_moved(&mut db, "Meetings", "Archive/Meetings").unwrap();

        assert_eq!(db.file_count().unwrap(), before, "no rows dropped or re-created");
        assert!(db.get_file("Meetings/kickoff.md").unwrap().is_none());
        assert!(db.get_file("Meetings/Q3/review.md").unwrap().is_none());
        let moved = db.get_file("Archive/Meetings/kickoff.md").unwrap().expect("moved row");
        assert_eq!(moved.status, "indexed");
        assert_eq!(
            db.get_text("Archive/Meetings/kickoff.md").unwrap().as_deref(),
            Some("Kickoff notes zebra.")
        );
        assert!(db.get_file("Archive/Meetings/Q3/review.md").unwrap().is_some());
        assert!(db.get_file("Meetings-old/stale.md").unwrap().is_some(), "sibling untouched");

        let paths: Vec<String> = db
            .search_chunks_fts("zebra", 10)
            .unwrap()
            .into_iter()
            .map(|h| h.path)
            .collect();
        assert!(paths.contains(&"Archive/Meetings/kickoff.md".to_string()), "got {paths:?}");
        assert!(paths.contains(&"Archive/Meetings/Q3/review.md".to_string()), "got {paths:?}");
        assert!(paths.contains(&"Meetings-old/stale.md".to_string()), "got {paths:?}");
    }
}

#[cfg(test)]
mod chat_mcp_config_tests {
    use super::chat_mcp_config;
    use std::path::Path;

    /// Ken's chat registers the knowledge server and the personal set beside
    /// it; only the personal set gets the app's listener (decision 22).
    #[test]
    fn the_chat_registers_the_knowledge_server_and_the_personal_set() {
        let cfg = chat_mcp_config(Path::new("/bin/ken-mcp"), Path::new("/work/wiki"), "http://127.0.0.1:9/ui", "tok");
        let servers = cfg["mcpServers"].as_object().unwrap();
        assert_eq!(servers.len(), 2);
        let ken = &servers["ken"];
        assert_eq!(ken["args"][0], "--project");
        assert!(ken.get("env").is_none(), "{ken}");
        let personal = &servers["ken-personal"];
        assert_eq!(personal["args"], serde_json::json!(["--personal"]));
        assert_eq!(personal["env"]["KEN_APP_TOKEN"], "tok");
        assert_eq!(personal["command"], ken["command"]);
    }
}

#[cfg(test)]
mod ken_mcp_refresh_tests {
    use super::refresh_ken_mcp_from;

    #[test]
    fn copies_when_dest_missing_or_stale_and_skips_when_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let bundled = dir.path().join("bundled-ken-mcp");
        let dest = dir.path().join("bin").join("ken-mcp");
        std::fs::write(&bundled, b"v2 binary").unwrap();

        // Missing dest -> copied (and parent dir created).
        assert!(refresh_ken_mcp_from(&bundled, &dest).unwrap());
        assert_eq!(std::fs::read(&dest).unwrap(), b"v2 binary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dest).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "must be executable");
        }

        // Same bytes -> no copy.
        assert!(!refresh_ken_mcp_from(&bundled, &dest).unwrap());

        // Stale dest -> copied.
        std::fs::write(&dest, b"v1 binary").unwrap();
        assert!(refresh_ken_mcp_from(&bundled, &dest).unwrap());
        assert_eq!(std::fs::read(&dest).unwrap(), b"v2 binary");
    }
}

#[cfg(test)]
mod your_day_tests {
    use super::*;

    fn attempt(at: i64, date: &str, ok: bool) -> DigestAttempt {
        DigestAttempt { at, date: date.into(), ok }
    }

    #[test]
    fn the_schedule_backs_off_after_a_failure_and_runs_once_a_day_after_success() {
        let mut s = DigestSchedule::default();
        let today = "2026-10-01";
        assert!(digest_due(&s, false, today, 10_000), "never tried");
        assert!(!digest_due(&s, true, today, 10_000), "stored today");

        s.last = Some(attempt(10_000, today, false));
        assert!(!digest_due(&s, false, today, 10_000 + 60), "a minute after a failure");
        assert!(digest_due(&s, false, today, 10_000 + DIGEST_RETRY_AFTER_SECS), "30 minutes on");

        s.last = Some(attempt(10_000, today, true));
        assert!(!digest_due(&s, false, today, 90_000), "succeeded today");
        assert!(digest_due(&s, false, "2026-10-02", 90_000), "a new day");
    }

    #[test]
    fn the_schedule_file_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let ws = uuid::Uuid::new_v4();
        assert!(load_digest_schedule(dir.path(), ws).last.is_none());
        update_digest_schedule(dir.path(), ws, |s| s.last = Some(attempt(5, "2026-10-01", true)));
        let s = load_digest_schedule(dir.path(), ws);
        assert!(s.last.is_some_and(|a| a.ok && a.date == "2026-10-01"));
        // An older schedule (per team, with the last team) reads as never
        // tried, so the workspace's digest is written once more.
        std::fs::write(digest_schedule_path(dir.path(), ws), r#"{"lastTeam":"Ops","attempts":{"":{"at":5,"date":"2026-10-01","ok":true}}}"#).unwrap();
        assert!(load_digest_schedule(dir.path(), ws).last.is_none());
    }

    #[test]
    fn the_snapshot_rereads_only_files_whose_metadata_moved() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.md");
        std::fs::write(&a, "one").unwrap();
        std::fs::write(dir.path().join("note.txt"), "x").unwrap();
        let dirs = vec![dir.path().to_path_buf()];
        let first = task_board_file_snapshot(&dirs, &DaySnapshot::new());
        assert_eq!(first.len(), 1, "only .md files");
        assert_eq!(first[&a].hash, content_hash(b"one"));

        // A recorded hash is trusted while size and mtime hold.
        let mut seeded = first.clone();
        seeded.get_mut(&a).unwrap().hash = 42;
        assert_eq!(task_board_file_snapshot(&dirs, &seeded)[&a].hash, 42);

        // A changed size is read again.
        std::fs::write(&a, "one two").unwrap();
        assert_eq!(task_board_file_snapshot(&dirs, &seeded)[&a].hash, content_hash(b"one two"));
    }

    #[test]
    fn noted_writes_are_quiet_and_others_are_not() {
        let file = |hash: u64| WatchedFile { len: 1, mtime: None, hash };
        let note = |expect| TaskWriteNote { expect, at: Instant::now() };
        let (a, b) = (PathBuf::from("/t/a.md"), PathBuf::from("/t/b.md"));
        let last: DaySnapshot = [(a.clone(), file(1))].into_iter().collect();
        let current: DaySnapshot = [(a.clone(), file(2)), (b.clone(), file(3))].into_iter().collect();

        let mut writes = std::collections::HashMap::new();
        writes.insert(a.clone(), note(TaskWriteExpect::Present(2)));
        writes.insert(b.clone(), note(TaskWriteExpect::Writing));
        assert!(!day_change_unexplained(&last, &current, &mut writes));
        assert!(!writes.contains_key(&a), "a matched note is consumed");
        assert!(writes.contains_key(&b), "a write in progress stays noted");

        let mut writes = std::collections::HashMap::new();
        assert!(day_change_unexplained(&last, &current, &mut writes), "nobody announced these");

        // A removal: explained only by an `Absent` note.
        let mut writes = std::collections::HashMap::new();
        writes.insert(a.clone(), note(TaskWriteExpect::Absent));
        assert!(!day_change_unexplained(&last, &DaySnapshot::new(), &mut writes));
        assert!(writes.is_empty());

        // Old notes expire.
        let mut writes = std::collections::HashMap::new();
        writes.insert(
            b.clone(),
            TaskWriteNote { expect: TaskWriteExpect::Writing, at: Instant::now() - TASK_WRITE_NOTE_TTL - Duration::from_secs(1) },
        );
        assert!(day_change_unexplained(&last, &current, &mut writes));
        assert!(writes.is_empty());
    }
}

#[cfg(test)]
mod ingest_record_tests {
    use super::*;

    #[test]
    fn a_move_across_drives_is_recognised_on_each_os() {
        let code = if cfg!(windows) { 17 } else { 18 };
        assert!(is_cross_device(&std::io::Error::from_raw_os_error(code)));
        assert!(!is_cross_device(&std::io::Error::from_raw_os_error(2)));
        let d = tempfile::tempdir().unwrap();
        let (from, to) = (d.path().join("a.wav"), d.path().join("b.wav"));
        std::fs::write(&from, b"x").unwrap();
        move_across(&from, &to).unwrap();
        assert!(!from.exists() && to.is_file());
    }

    /// Your day reads my open tickets from every repo of the workspace; an
    /// `escalations/` folder (cut with decision 14) is not read at all.
    #[test]
    fn your_day_lists_my_tickets_from_every_repo_and_no_escalations() {
        let d = tempfile::tempdir().unwrap();
        let wiki = d.path().join("Wiki");
        let game = d.path().join("Game");
        for (root, file, text) in [
            (&wiki, "tickets/SR-001.md", "---\nid: SR-001\nstatus: todo\nassignee: Chris Staud\n---\n# Save the world\n"),
            (&wiki, "tickets/SR-002.md", "---\nid: SR-002\nstatus: done\nassignee: Chris Staud\n---\n# Done\n"),
            (&game, "tickets/G-1.md", "---\nid: G-1\nstatus: in-progress\nassignee: chris\n---\n# Load it\n"),
            (&game, "tickets/G-2.md", "---\nid: G-2\nstatus: todo\nassignee: Kate\n---\n# Not mine\n"),
            (&wiki, "escalations/E-001.md", "---\nid: E-001\nto: Chris Staud\nstatus: open\n---\n# Which save format?\n"),
        ] {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        let repos = vec![
            WsRepo { id: uuid::Uuid::new_v4(), name: "Wiki".into(), root: wiki },
            WsRepo { id: uuid::Uuid::new_v4(), name: "Game".into(), root: game },
        ];
        let me = ken_core::day::Me { name: Some("Chris Staud".into()), email: Some("chris@example.com".into()) };
        let (tickets, has_tickets) = my_tickets(&repos, &me, &[]);
        assert!(has_tickets);
        let mut ids: Vec<&str> = tickets.iter().map(|t| t.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, vec!["G-1", "SR-001"], "open and mine, from both repos");
        assert!(my_tickets(&repos, &ken_core::day::Me::default(), &[]).0.is_empty(), "no identity, no list");
        let json = serde_json::to_value(DayStateDto {
            tickets,
            tasks: Vec::new(),
            me: DayMeDto { name: None, email: None },
            has_tickets,
        })
        .unwrap();
        assert!(json.get("escalations").is_none(), "{json}");
    }

    /// The settings screen pairs each control with its source's last run:
    /// by the source the control names, else the knowledge base.
    #[test]
    fn controls_carry_their_last_result() {
        use ken_core::controls::{Control, ControlResult};
        let c = |q: &str, e: &str, s: Option<&str>| Control { question: q.into(), expect: e.into(), source: s.map(str::to_string) };
        let r = |q: &str, e: &str, rank: Option<usize>| ControlResult { question: q.into(), expect: e.into(), rank, passed: rank.is_some() };
        let sources = vec![
            ("Wiki".to_string(), vec![r("where do rules live", "Rules/Index.md", Some(2))]),
            ("Game".to_string(), vec![r("how are worlds saved", "src/Save.java", None)]),
        ];
        let rows = control_rows(
            vec![
                c("where do rules live", "Rules/Index.md", None),
                c("how are worlds saved", "src/Save.java", Some("game")),
                c("what is new", "Current/Project.md", Some("Tools")),
            ],
            &sources,
            Some("Wiki"),
        );
        assert!(rows[0].ran && rows[0].passed && rows[0].rank == Some(2), "no source: the knowledge base's run");
        assert!(rows[1].ran && !rows[1].passed && rows[1].rank.is_none(), "a source by name, any case");
        assert!(!rows[2].ran && !rows[2].passed, "a source that has not run it");
    }

    #[test]
    fn the_settings_screen_reads_the_findings_list() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        std::fs::create_dir_all(root.join("Current")).unwrap();
        // A knowledge base (it has the inbox): its scan runs the lint.
        std::fs::create_dir_all(root.join("Research/Ingestion/Raw")).unwrap();
        std::fs::write(root.join("Current/Team.md"), "---\nstatus: draft\n---\n# Team\nSee [[Ghost]].\n").unwrap();
        let project = Project::create(root, "Wiki").unwrap();
        let data = tempfile::tempdir().unwrap();
        let mut db = Db::open_at(&data.path().join("w.db")).unwrap();
        scan::scan(&project, &mut db).unwrap();
        let found = kb_findings("w1", &db);
        assert!(found.iter().any(|f| f.kind == "missing-page" && f.path == "Current/Team.md" && f.project_id == "w1"), "{:?}", found.iter().map(|f| &f.title).collect::<Vec<_>>());
    }
}

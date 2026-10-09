use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use ken_core::assistant::{self, OneshotOutcome};
use ken_core::chat::{self, ChatEngine, ChatPty, ChatUpdate};
use ken_core::cloud;
use ken_core::db::{
    db_path, ChatField, ChatFlag, ChatMessage, ChatRow, Db, EdgeRow, EntityRow, EventRow,
    FileRow, SearchHit,
};
use ken_core::digest;
use ken_core::embedder::Embedder;
use ken_core::knowledge_model::{self, AutoBuildTracker};
use ken_core::model;
use ken_core::engine;
use ken_core::family::{self, FamilyManifest, FamilyMember, Lane};
use ken_core::family_sync::{self, GitTransport, PendingWrite, SystemGit};
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
use ken_core::search as hybrid_search_mod;
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
    /// Dedicated read-only connection for `search`, on its OWN mutex so a
    /// keystroke's query never contends on the global `AppState` lock (which
    /// every other command and the background workers hold). WAL lets this
    /// reader run concurrently with the writers and still see their commits.
    search_db: Arc<Mutex<Db>>,
    _watch: WatchHandle,
    /// Live on/off handle for the semantic-index build step: `Some` while
    /// `semanticIndex` is on and the model loaded, so
    /// `set_project_feature("semanticIndex", ..)` and `hybrid_search` can
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
    /// True while a knowledge-model build thread is running.
    knowledge_running: Arc<AtomicBool>,
    /// True while a background reindex (clear + full rescan) is running, so a
    /// second `reindex` command can't stack another rebuild on top.
    reindex_running: Arc<AtomicBool>,
    /// True while `profile_project` (project-profiler task 2.1) is scanning
    /// or refining this project, so a second call can't stack another pass
    /// on top. `profile_candidates` (task 2.2) has no equivalent guard: its
    /// targets aren't open members and share no state to race on.
    profiling_running: Arc<AtomicBool>,
    /// Change/scan/build bookkeeping behind automatic Map & Timeline builds.
    auto_knowledge: Arc<AutoBuildTracker>,
    /// Stops the per-project extraction worker when this project closes.
    _extraction_worker: StopOnDrop,
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
    /// federated-kg task 2.1: true while a `build_workspace_kg` run is in
    /// flight. App-global (not per-member) because one build spans every
    /// open member at once — mirrors `MemberRuntime::reindex_running`'s
    /// single-guard discipline, just at the workspace scope instead of the
    /// project scope.
    workspace_kg_running: Arc<AtomicBool>,
    /// Cancel token for the in-flight workspace-KG build, if any — mirrors
    /// `MemberRuntime::semantic_build_cancel`'s replace-and-cancel slot.
    workspace_kg_cancel: Arc<Mutex<Option<CancelToken>>>,
    /// Debounce generation counter for the 30s workspace-KG auto-trigger
    /// (mirrors `qa_gen`: "a newer request bumps it so the older one sees a
    /// mismatch and stands down"). See `schedule_workspace_kg_debounce`.
    workspace_kg_debounce_gen: Arc<AtomicU64>,
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
    /// ken-memory task 2.2: true while a `distill_journal` run is in flight —
    /// mirrors `workspace_kg_running`'s single-`AtomicBool`-guard discipline,
    /// at the workspace-journal scope instead.
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
    /// The team Your day last asked about (`day_state`, `team_digest`, …):
    /// the morning schedule writes that team's digest. Also saved in app
    /// data (`TeamDigestSchedule`), so it survives a restart.
    last_team: Option<String>,
    /// True while a team-digest thread is running (one at a time).
    team_digest_running: Arc<AtomicBool>,
    /// ken-families task 2.3: one `family_sync::SyncEngine` per connection,
    /// keyed by family id, living for the process's lifetime rather than
    /// tied to any open workspace (families are a global-flag feature,
    /// D6: "unattached connections still sync and notify"). Populated
    /// lazily on first touch (`family_engine_handle`) so the same instance
    /// backs both the poll loop and every on-demand command (`family_sync_
    /// now`, accept/dismiss commits, `family_resolve_conflict`) — a fresh
    /// engine per call would lose `ConnectionState::Conflict`/`Unavailable`
    /// between ticks, defeating D1's "polling stops here until a human
    /// resolves it".
    family_engines: Arc<Mutex<std::collections::HashMap<uuid::Uuid, Arc<Mutex<family_sync::SyncEngine>>>>>,
    /// ken-families task 2.3: the poll-loop stop signal per connection with
    /// `liveSync` on, keyed by family id. `reconcile_family_pollers` is the
    /// only writer — it recomputes the wanted set from each saved
    /// connection's `liveSync` after every mutation that could
    /// change either, and drops (`StopOnDrop`) whatever's no longer wanted
    /// — mirroring `WorkspaceState::task_watch`'s drop-stops-the-thread
    /// discipline, fanned out over N connections instead of one workspace.
    family_pollers: Arc<Mutex<std::collections::HashMap<uuid::Uuid, StopOnDrop>>>,
}

type SharedState = Arc<Mutex<AppState>>;

/// Resident `MemberRuntime` cap for an open workspace (design D4 /
/// task 3.1): fully activate up to this many members; the rest stay dormant
/// (tracked, not resident) and open lazily on first focus, evicting the
/// least-recently-focused resident when the cap would be exceeded.
const WORKSPACE_RESIDENT_CAP: usize = 12;

/// Max member initial-scans allowed to run at once (design D4 / task 3.1).
const WORKSPACE_INGEST_CONCURRENCY: usize = 2;

/// Friendly error returned by every workspace command when the global
/// `workspace` flag is off (task 3.4). Mirrors `open_member`/`close_member`'s
/// flag gate, just with launcher-ready wording.
const WORKSPACE_DISABLED_MSG: &str =
    "The workspace feature is off — turn it on in Settings → Features to open a workspace.";

/// A counting semaphore capping how many member initial-scans run
/// concurrently (task 3.1 / design D4). `std` has no semaphore, so this is
/// the classic `Mutex<permits>` + `Condvar` pair — the same hand-rolled
/// coordination discipline the rest of this file already leans on (atomics +
/// generation counters like `qa_gen`/`workspace_kg_debounce_gen`). One
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
    fn new(row: FileRow, project: &Project) -> FileRowDto {
        let excluded = project.is_excluded(&row.rel_path);
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
/// parameter existed), and from `profile_project`/task 2.3's re-ingest
/// trigger (which passes a real target id — profiling can run against any
/// open member, not just the focused one).
///
/// project-profiler task 2.3: when the `profiler` flag is on for this
/// project, the background rebuild loads `.ken/index-profile.json` and
/// passes it to `rebuild_semantic_index_with_profile` so the profile's
/// chunking actually takes effect; flag off (or no profile) passes `None`,
/// which reproduces the old extension-only chunking exactly.
fn apply_semantic_index_flag(
    app: &AppHandle,
    state: &SharedState,
    target: Option<uuid::Uuid>,
    enabled: bool,
) -> CmdResult<()> {
    let (project, base, embedder_slot, cancel_slot, warned_large, app_settings) = {
        let guard = state.lock().unwrap();
        let active = member(&guard, target)?;
        (
            active.project.clone(),
            guard.base_dir.clone(),
            active.semantic_embedder.clone(),
            active.semantic_build_cancel.clone(),
            active.semantic_index_warned_large.clone(),
            guard.app_settings.clone(),
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
        return Ok(());
    };

    let token = CancelToken::new();
    if let Some(old) = cancel_slot.lock().unwrap().replace(token.clone()) {
        old.cancel();
    }

    let bg_app = app.clone();
    let bg_embedder_slot = embedder_slot.clone();
    std::thread::spawn(move || {
        let mut db = match Db::open(&base, project.config.id) {
            Ok(db) => db,
            Err(e) => {
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
        semantic_builds().lock().unwrap_or_else(|p| p.into_inner()).insert(project_id, (0, 0));
        // project-profiler task 2.3: resolve the stored profile only when the
        // `profiler` flag is on for this project — `None` otherwise, which
        // makes `rebuild_semantic_index_with_profile` byte-identical to the
        // old profile-less `rebuild_semantic_index` (ken-core task 1.5's
        // flag-off inertness guarantee).
        let profiler_enabled = ken_core::features::effective_flag(&app_settings, &project, "profiler");
        let profile = profiler_enabled.then(|| ken_core::profiler::ProjectProfile::load(&project.root));
        let mut last_emit = std::time::Instant::now() - std::time::Duration::from_secs(1);
        let result = engine::rebuild_semantic_index_with_profile(
            &project,
            &mut db,
            &mut embedder,
            &token,
            |done, total| {
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
            },
            profile.as_ref(),
        );
        semantic_builds().lock().unwrap_or_else(|p| p.into_inner()).remove(&project_id);
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

    Ok(())
}

/// Activate a project: register it, open its DB, start the watcher, and
/// kick off a background scan that reports through events.
///
/// `clear_others` controls whether pre-existing entries in `state.members`
/// are dropped first: `true` (single-project mode — `create_project` /
/// `open_project`) keeps the map at <=1 entry, matching the pre-workspace
/// behavior exactly; `false` (`open_member`, workspace flag on) leaves other
/// open members running so the map can hold more than one.
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
    // verified date and retired/generated state from the stored text.
    if let Err(e) = db.backfill_page_meta() {
        eprintln!("warning: page frontmatter backfill failed: {e}");
    }
    if let Err(e) = db.backfill_page_links() {
        eprintln!("warning: page links backfill failed: {e}");
    }
    // The code map for an index made before it: definitions, usages and
    // imports parsed from the stored text, no rescan.
    if let Err(e) = db.backfill_code_map() {
        eprintln!("warning: code map backfill failed: {e}");
    }

    // One-time extraction backfill: a project indexed before the incremental
    // Map landed has no `extractions` rows, and the scanner never re-runs
    // index_one for unchanged files — so those files would show "0 of N
    // analyzed" forever. Enqueue every indexed file that has no row yet,
    // hashing its already-stored text. Cheap, idempotent, and re-index-safe.
    match db.backfill_extractions() {
        Ok(_) => {
            // Sanity check: after backfill, every extractable (indexed + has
            // content) file should have an extractions row. If any still don't,
            // the coverage denominator and the enqueuable population disagree —
            // the Map would stick below 100% with no queued work to close the
            // gap (the "0 of N" wedge). Surface it instead of failing silently.
            if let Ok(missing) = db.unqueued_extractable_count() {
                if missing > 0 {
                    eprintln!(
                        "warning: extraction backfill left {missing} extractable file(s) \
                         unqueued; Map coverage may not reach 100%"
                    );
                }
            }
        }
        Err(e) => eprintln!("warning: extraction backfill failed: {e}"),
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
            // person asked Claude to): the frontend focuses and opens it.
            let ui_app = app.clone();
            h.set_ui_handler(move |v| {
                let _ = ui_app.emit("ken-open", v);
            });
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
    // Ken's own MCP server in every chat session of this project: Ken's
    // search, with ken:// citations, and opening a file when asked.
    if let (Some(engine), Some(mcp)) = (&chat_engine, find_ken_mcp()) {
        let cfg = serde_json::json!({
            "mcpServers": {
                "ken": {
                    "command": mcp,
                    "args": ["--project", project.root],
                    "env": {
                        "KEN_APP_URL": hooks.ui_url(),
                        "KEN_APP_TOKEN": hooks.token(),
                        "KEN_PROJECT_ID": project.config.id.to_string(),
                    }
                }
            }
        });
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

    let auto_knowledge = Arc::new(AutoBuildTracker::new());

    // kenignore task 2.2 needs its own `Db` handle later (see the poller
    // below), so grab a clone before `watch::start` moves `watch_db_path`.
    let kenignore_db_path = watch_db_path.clone();

    let emit_app = app.clone();
    let watch_sync = sync_engine.clone();
    let watch_knowledge = auto_knowledge.clone();
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
                // Same signal, slower consumer: the knowledge model is
                // rebuilt only once the changes stop coming.
                watch_knowledge.changed();
            }
            enqueue_transcriptions(&emit_app, &watch_state, project_id, &stats.videos_needing_transcript);
            emit_member(&emit_app, project_id, "index-updated", stats.clone());
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
                        emit_member(&emit_app, project_id, "index-updated", stats);
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
        knowledge_running: Arc::new(AtomicBool::new(false)),
        reindex_running: Arc::new(AtomicBool::new(false)),
        profiling_running: Arc::new(AtomicBool::new(false)),
        auto_knowledge: auto_knowledge.clone(),
        _extraction_worker: StopOnDrop(stop.clone()),
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
    let scan_knowledge = auto_knowledge.clone();
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
                        scan_knowledge.changed();
                    }
                    enqueue_transcriptions(&scan_app, &scan_state, project_id, &stats.videos_needing_transcript);
                    emit_member(&scan_app, project_id, "index-updated", stats);
                    // The standing sweep, when due: team and wiki repos only,
                    // on the index this scan just brought up to date.
                    run_drift_if_due(&scan_project, &mut db, false);
                    // And the library inbox: anything waiting in Raw/.
                    start_ingest_pass(&scan_project, &base, workspace_root_of(&scan_state), false);
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
    // Paused behind `KNOWLEDGE_EXTRACTION_ENABLED` (see its doc comment): the
    // `stop` plumbing above stays wired either way, so nothing else changes.
    if KNOWLEDGE_EXTRACTION_ENABLED {
        let worker_app = app.clone();
        let worker_state = state.clone();
        let worker_id = project.config.id;
        let worker_stop = stop.clone();
        std::thread::spawn(move || {
            extraction_worker(worker_app, worker_state, worker_id, worker_stop);
        });
    }

    // One background OCR worker per open project: drains `ocr_pending`, runs
    // Vision on images/scanned PDFs, and merges the recognized text into the
    // index so it becomes searchable (and stores per-region boxes for the
    // Cmd+F overlay). Independent of the extraction worker above.
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

    // The team digest may be due the moment a repo opens (it waits for
    // the workspace to be installed; see `open_workspace_inner`).
    maybe_generate_team_digest(app, state);

    Ok(info)
}

/// Master switch for the per-project knowledge extraction worker — the
/// background pass that runs the local LLM over every indexed file and feeds
/// the Map and Timeline screens.
///
/// On: the knowledge graph behind hybrid search is built from it. It was
/// paused upstream while testing (it drove llama.cpp on the GPU once per file
/// across a whole project); on this branch it reads only files a repo's kind
/// and index state mark for entities (team and wiki repos by default), so a
/// code repo costs it nothing. Switching it off leaves queue rows `pending`,
/// so switching back on resumes where it left off; the `map` and `timeline`
/// entries in `src/shell/NavRail.svelte` go with it.
const KNOWLEDGE_EXTRACTION_ENABLED: bool = true;

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
        let (base, project, sync, auto_knowledge) = {
            let guard = state.lock().unwrap();
            // Own project by id — see `extraction_worker`. The old
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
                        // a just-downloaded doc reaches ingests, Map/Timeline and
                        // sync, and the footer's cloud_only count drops.
                        let paths = vec![rel.clone()];
                        sync.changed(paths.clone());
                        auto_knowledge.changed();
                        emit_member(
                            &app,
                            project_id,
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

/// Workspaces, the workspace knowledge graph and search routed across the
/// workspace are how Ken works, not switches (`features::BUILT_IN`): always
/// on, whatever an older `settings.json` says. The helpers stay so their
/// callers read as before.
fn workspace_enabled(_app_settings: &ken_core::settings::AppSettings) -> bool {
    true
}

fn federated_kg_enabled(_app_settings: &ken_core::settings::AppSettings) -> bool {
    true
}

fn kg_routing_enabled(_app_settings: &ken_core::settings::AppSettings) -> bool {
    true
}

/// Display label given to the workspace pseudo-member's `Project::name`
/// (ken-memory task 2.1) — shows up wherever a member's name flows through
/// unchanged (route_search's `MemberInfo::name`, `search_all_projects`'
/// hand-added roster entry below).
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
/// every exclusion site (search fan-out, federation loops, profiler) derives
/// the same id the same way.
fn memory_pseudo_member_id(state: &AppState) -> Option<uuid::Uuid> {
    state
        .workspace
        .as_ref()
        .map(|ws| memory::workspace_pseudo_member_id(ws.ws.config.id))
}

/// Open an additional project alongside whatever is already open, without
/// disturbing it (S9 step 6 / `workspace` change). Gated on the global
/// `workspace` flag — off, this is a no-op error so a stray call can't grow
/// `state.members` past one entry outside workspace mode. The asset-protocol
/// scope grant for the member's root happens inside `activate`, same as
/// `open_project` — nothing to duplicate here.
#[tauri::command(async)]
fn open_member(app: AppHandle, state: State<SharedState>, path: String) -> CmdResult<ProjectInfo> {
    {
        let guard = state.lock().unwrap();
        if !workspace_enabled(&guard.app_settings) {
            return Err("workspace disabled".into());
        }
    }
    let project = Project::open(std::path::Path::new(&path)).map_err(err)?;
    activate(&app, &state, project, false)
}

/// Close one member of an open workspace, leaving the others running. Gated
/// on the global `workspace` flag like `open_member`. If the closed member
/// was focused, focus falls to an arbitrary remaining member (or `None` if
/// it was the last one) — `MemberRuntime`'s `Drop`/`StopOnDrop` fields stop
/// its watcher and background workers.
#[tauri::command(async)]
fn close_member(state: State<SharedState>, project_id: String) -> CmdResult<()> {
    let mut guard = state.lock().unwrap();
    if !workspace_enabled(&guard.app_settings) {
        return Err("workspace disabled".into());
    }
    let uuid: uuid::Uuid = project_id.parse().map_err(err)?;
    if guard.members.remove(&uuid).is_none() {
        return Err("project not open".into());
    }
    if guard.focused == Some(uuid) {
        guard.focused = guard.members.keys().next().copied();
    }
    Ok(())
}

// ── Workspace mode (openspec/changes/workspace, tasks 3.1–3.4) ─────────────
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

/// One hit in `search_all_projects` (task 3.3): a member's `SearchHit`
/// labeled with its owning project id + name so the ⌘K "All projects" list
/// can badge and route it.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AllProjectsHitDto {
    project_id: String,
    member_name: String,
    /// What the file is for: code, test, spec, doc, config, data, design,
    /// meeting or ticket (`contenttype::of`).
    content_type: &'static str,
    #[serde(flatten)]
    hit: SearchHit,
}

/// One member's outcome in `search_all_projects` (task 3.3) — the honest
/// per-member status list, mirroring `route_search`'s `member_status`.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AllProjectsMemberStatusDto {
    /// `Some` for resolvable members; `None` for missing/invalid.
    project_id: Option<String>,
    member_name: String,
    /// `"searched"` (active, FTS ran) | `"dormant"` (skipped, not resident) |
    /// `"missing"` | `"invalid"`.
    status: &'static str,
}

/// `search_all_projects` return shape (task 3.3).
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchAllProjectsDto {
    results: Vec<AllProjectsHitDto>,
    member_status: Vec<AllProjectsMemberStatusDto>,
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
/// Eviction is a plain `members.remove`, which drops the `MemberRuntime` —
/// the exact same teardown `close_member` performs (its `StopOnDrop`/`Drop`
/// fields stop the watcher and background workers). Shared by `focus_project`
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
/// to `.kenignore`-syntax lines (`Tier::Full` -> `!pattern`,
/// `Tier::SearchOnly` -> `~pattern`, `Tier::Ignore` -> bare `pattern`). Lets
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
            kenignore::Tier::Full => "!",
            kenignore::Tier::SearchOnly => "~",
            kenignore::Tier::Ignore => "",
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

    // D3's built-in tier rules (memory/ full, journal/+tasks/ search-only,
    // workspace.json+kg.sqlite ignored), regenerated on every activation.
    // `scan::scan`'s `rule_sets` is hardcoded to
    // `[built_in_rule_sets(), user_rules]` with no third, per-member slot,
    // and both `scan.rs`/`kenignore.rs` are outside this task's touch-
    // boundary — so this writes the built-ins through the same
    // `Project::kenignore_rules()` disk-read channel `scan::scan` already
    // calls unmodified, via `render_builtin_kenignore` above. Best-effort:
    // a write failure degrades this activation to "everything Full tier"
    // (today's plain no-`.kenignore` default) rather than blocking the open.
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

    // ken-families task 2.5: activate every connection attached to this
    // workspace as its own `kind: family` pseudo-member (D6), the same
    // shape as the workspace-memory block just above — see `activate_family_
    // pseudo_member`'s doc comment for why. No ordering dependency on the
    // `Registry::last_project` concern that block's comment describes
    // (family pseudo-members key off the manifest's own family id, not a
    // reserved id derived from `ws.config.id`), so this can run in any
    // order relative to it; kept adjacent for readability. Best-effort per
    // connection, matching the memory block's own failure handling.
    {
        for conn in family_connections(&settings_now)
            .into_iter()
            .filter(|c| c.attached_workspace_id == Some(ws.config.id))
        {
            if let Err(e) = activate_family_pseudo_member(app, state, &conn) {
                eprintln!("warning: family pseudo-member '{}' failed to activate: {e}", conn.name);
            }
        }
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

    // A repo with the inbox and no kind said is the team's wiki: say so,
    // with the team of the group it is in.
    {
        let guard = state.lock().unwrap();
        if let Ok(mut reg) = Registry::load(&guard.base_dir) {
            let groups = ws.config.effective_groups();
            let mut changed = false;
            for (name, root, _) in &ok_members {
                let team = groups.iter().find(|g| g.members.contains(name)).map(|g| g.name.as_str());
                changed |= reg.infer_library(root, team);
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

    // Your day's watcher: task files, ticket files and family inboxes, now
    // that `guard.workspace` is installed (every tick reads it). It emits
    // `day-changed`; nothing here creates a folder.
    {
        let handle = spawn_task_board_watch(app.clone(), state.clone());
        let mut guard = state.lock().unwrap();
        if let Some(ws_state) = guard.workspace.as_mut() {
            ws_state.task_watch = Some(handle);
        }
    }
    // The morning team digest, now that the workspace is installed.
    maybe_generate_team_digest(app, state);

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
/// bring its members online (task 3.1). Flag-gated (task 3.4).
#[tauri::command(async)]
fn open_workspace(
    app: AppHandle,
    state: State<SharedState>,
    parent: String,
) -> CmdResult<WorkspaceOverviewDto> {
    {
        let guard = state.lock().unwrap();
        if !workspace_enabled(&guard.app_settings) {
            return Err(WORKSPACE_DISABLED_MSG.into());
        }
    }
    let ws = ken_core::workspace::Workspace::open(Path::new(&parent)).map_err(err)?;
    open_workspace_inner(&app, state.inner(), ws)
}

/// Create a new workspace over `parent` from the selected member folder names,
/// then open it (task 3.1). Flag-gated (task 3.4).
#[tauri::command(async)]
fn create_workspace(
    app: AppHandle,
    state: State<SharedState>,
    parent: String,
    name: String,
    members: Vec<String>,
) -> CmdResult<WorkspaceOverviewDto> {
    {
        let guard = state.lock().unwrap();
        if !workspace_enabled(&guard.app_settings) {
            return Err(WORKSPACE_DISABLED_MSG.into());
        }
    }
    let ws = ken_core::workspace::Workspace::create(Path::new(&parent), &name, &members)
        .map_err(err)?;
    open_workspace_inner(&app, state.inner(), ws)
}

/// Members + per-member status + counts for the open workspace (task 3.2).
/// Flag-gated (task 3.4).
#[tauri::command(async)]
fn workspace_overview(state: State<SharedState>) -> CmdResult<WorkspaceOverviewDto> {
    {
        let guard = state.lock().unwrap();
        if !workspace_enabled(&guard.app_settings) {
            return Err(WORKSPACE_DISABLED_MSG.into());
        }
    }
    build_workspace_overview(state.inner())
}

/// Focus a member, activating a dormant one and LRU-evicting past the cap
/// (task 3.2). Flag-gated (task 3.4).
#[tauri::command(async)]
fn focus_project(app: AppHandle, state: State<SharedState>, id: String) -> CmdResult<()> {
    {
        let guard = state.lock().unwrap();
        if !workspace_enabled(&guard.app_settings) {
            return Err(WORKSPACE_DISABLED_MSG.into());
        }
    }
    let uuid: uuid::Uuid = id.parse().map_err(err)?;
    focus_member_inner(&app, state.inner(), uuid)
}

/// Set-up, step one to three: scan a code folder and propose a row per repo
/// (kind, team, index state, evidence) and the ignore entries. Reads only.
#[tauri::command]
async fn setup_propose(parent: String) -> CmdResult<ken_core::setup::Proposal> {
    // Not flag-gated: set-up is how a fresh install opts into several repos
    // (Confirm turns the workspace flag on). `git worktree list` per repo:
    // off the UI thread.
    tauri::async_runtime::spawn_blocking(move || ken_core::setup::propose(Path::new(&parent)).map_err(err))
        .await
        .map_err(|e| e.to_string())?
}

/// Set-up, Confirm: write the manifest, the registry's kinds, teams and
/// index states, and the ticked ignore lines, then open the workspace so
/// indexing starts. The only set-up step that writes.
#[tauri::command(async)]
fn setup_confirm(
    app: AppHandle,
    state: State<SharedState>,
    parent: String,
    name: String,
    rows: Vec<ken_core::setup::RepoRow>,
    ignores: Vec<ken_core::setup::IgnoreRow>,
) -> CmdResult<WorkspaceOverviewDto> {
    let base = state.lock().unwrap().base_dir.clone();
    let ws = ken_core::setup::confirm(&base, Path::new(&parent), &name, &rows, &ignores, &local_date_today())
        .map_err(err)?;
    open_workspace_inner(&app, state.inner(), ws)
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
/// data (`workspaces/<id>`), switching the workspace feature on; with `add`
/// true, into the open workspace. Then opens it so indexing starts.
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
    open_workspace_inner(&app, state.inner(), ws)
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

/// Immediate subfolder candidates for the workspace-creation UI (task 3.2) —
/// a thin flag-gated wrapper over `ken_core::workspace::discover_candidates`.
#[tauri::command(async)]
fn discover_workspace_candidates(
    state: State<SharedState>,
    parent: String,
) -> CmdResult<Vec<ken_core::workspace::Candidate>> {
    {
        let guard = state.lock().unwrap();
        if !workspace_enabled(&guard.app_settings) {
            return Err(WORKSPACE_DISABLED_MSG.into());
        }
    }
    ken_core::workspace::discover_candidates(Path::new(&parent)).map_err(err)
}

/// Close the open workspace, tearing down every member handle (task 3.2).
/// `members.clear()` drops all runtimes; recents get the last focus + now.
/// Flag-gated (task 3.4).
#[tauri::command(async)]
fn close_workspace(app: AppHandle, state: State<SharedState>) -> CmdResult<()> {
    let (base, ws_core, focused, closed) = {
        let mut guard = state.lock().unwrap();
        if !workspace_enabled(&guard.app_settings) {
            return Err(WORKSPACE_DISABLED_MSG.into());
        }
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

/// Adapt `route_search`'s output into `search_all_projects`'s existing wire
/// shape (kg-routing task 2.2), so the flag-guarded delegation below is
/// invisible to the frontend's `SearchAllProjectsDto` contract.
/// `routing::RoutedHit` is chunk-level and carries no `kind`/`status`/BM25
/// `rank` — those are `SearchHit`'s FILE-level fields — so this fills them
/// in rather than leaving the adapted shape half-empty:
///   - `relPath` = the hit's `path` (already forward-slash normalized).
///   - `kind` = `ken_core::extract::FileKind::from_path(rel_path).as_str()`,
///     the same pure, DB-free extension classifier `scan.rs` uses to set
///     `files.kind` in the first place — the real value, not a guess, and no
///     extra per-hit DB round trip.
///   - `status` = `"indexed"` always: provably correct, not a placeholder —
///     a `RoutedHit` only exists because `search_chunks_fts`/
///     `semantic_search` matched a row in `chunks`, which is only ever
///     populated for indexed files.
///   - `rank` = the hit's 0-based position in the already-merged (RRF) list.
///     Not a real BM25 score (design forbids comparing raw scores across
///     members) but keeps the FTS branch's "lower = better" convention for
///     anything downstream that sorts by it.
///   - `snippet` = the hit's chunk text as-is. Unlike the FTS branch's
///     `<mark>`-highlighted snippet, this carries no highlight spans —
///     `routing::RoutedHit`/`HybridHit` don't carry them at this layer (the
///     same limitation `hybrid_search`'s own DTO already has).
///
/// `member_status`: `route_search`'s per-member outcomes map onto the
/// existing vocabulary (`"searched"` → `"searched"`; `"index-building"`/
/// `"unavailable"` → `"dormant"`, the closest existing meaning — "this
/// member contributed nothing to this search" — since the DTO has no room
/// to say *why*). Workspace members the routing layer never sees at all —
/// `missing`/`invalid` manifest entries, which never enter `AppState::
/// members` — are appended from the manifest roster (`manifest_extras`) so
/// the list stays complete for those two categories.
///
/// Known gap (see final report): an active/resident member that simply
/// wasn't in the route plan's targets (e.g. a `Named`/`KgEntities` plan that
/// picked 1-3 of N open members) does not appear in the adapted
/// `member_status` at all — neither `"searched"` nor `"dormant"` fits
/// ("dormant" means not-resident, which is false here), and the plan that
/// WOULD explain the omission (`RouteSearchDto::plan`) has no field to ride
/// in on `SearchAllProjectsDto`. Not fixed here to avoid widening a DTO the
/// frontend already depends on without sign-off.
fn adapt_route_search_to_all_projects(
    dto: RouteSearchDto,
    manifest_extras: Vec<AllProjectsMemberStatusDto>,
) -> SearchAllProjectsDto {
    let results = dto
        .results
        .into_iter()
        .enumerate()
        .map(|(i, hit)| {
            let kind = ken_core::extract::FileKind::from_path(std::path::Path::new(&hit.path))
                .as_str()
                .to_string();
            AllProjectsHitDto {
                content_type: ken_core::contenttype::of(&hit.path).as_str(),
                project_id: hit.project_id,
                member_name: hit.member_name,
                hit: SearchHit {
                    rel_path: hit.path,
                    kind,
                    status: "indexed".to_string(),
                    snippet: hit.snippet,
                    rank: i as f64,
                },
            }
        })
        .collect();

    let mut member_status: Vec<AllProjectsMemberStatusDto> = dto
        .member_status
        .into_iter()
        .map(|s| {
            let status = match s.status {
                "searched" => "searched",
                _ => "dormant",
            };
            AllProjectsMemberStatusDto {
                project_id: Some(s.project_id),
                member_name: s.member_name,
                status,
            }
        })
        .collect();
    member_status.extend(manifest_extras);

    SearchAllProjectsDto { results, member_status }
}

/// All-projects keyword search (task 3.3): fan `query` out over every ACTIVE
/// member's FTS index, interleave the results round-robin by rank position
/// (design D6: BM25 scores aren't comparable across corpora, so merge by
/// each member's rank *position*, not raw score), and label every hit with
/// its member. Dormant members are skipped and reported in `member_status`
/// (honest per-member list, mirroring `route_search`). Flag-gated (task 3.4).
///
/// Follows the `search`/`hybrid_search` lock-audit template: clone each
/// member's read-only `search_db` Arc under a brief lock, release the global
/// guard, then run the FTS on the blocking pool so a query never serializes
/// against other commands or the background workers.
///
/// kg-routing task 2.2 — the ONE flag-guarded call site: when `kgRouting`
/// also resolves true, this delegates to `route_search` (reusing its plan +
/// concurrent fan-out + RRF merge + `routed-search-state` events) and adapts
/// the result into this command's existing DTO shape via
/// `adapt_route_search_to_all_projects`, above. Everything from `let
/// (actives, roster) = {` onward is the original path, untouched — flag off
/// (or `kgRouting` off) runs it exactly as before, byte-identical.
#[tauri::command]
async fn search_all_projects(
    app: AppHandle,
    state: State<'_, SharedState>,
    query: String,
    limit: Option<usize>,
    audience: Option<String>,
    types: Option<String>,
) -> CmdResult<SearchAllProjectsDto> {
    let limit = limit.unwrap_or(30);
    let wanted = ken_core::contenttype::parse_filter(types.as_deref());

    let routed = {
        let guard = state.lock().unwrap();
        if !workspace_enabled(&guard.app_settings) {
            return Err(WORKSPACE_DISABLED_MSG.into());
        }
        kg_routing_enabled(&guard.app_settings)
    };
    if routed {
        // `route_search` only ever sees `AppState::members` (currently
        // open/active projects) — it has no concept of a workspace
        // manifest's `missing`/`invalid` entries, so those two categories
        // are read separately here and appended, keeping `member_status` as
        // complete as the FTS branch's for those cases (see the adapter
        // function's doc for the one case that's still lossy).
        let manifest_extras: Vec<AllProjectsMemberStatusDto> = {
            let guard = state.lock().unwrap();
            let ws = guard.workspace.as_ref().ok_or("no workspace open")?;
            ws.ws
                .members
                .iter()
                .filter_map(|m| match &m.status {
                    ken_core::workspace::MemberStatus::Missing => Some(AllProjectsMemberStatusDto {
                        project_id: None,
                        member_name: m.name.clone(),
                        status: "missing",
                    }),
                    ken_core::workspace::MemberStatus::Invalid(_) => Some(AllProjectsMemberStatusDto {
                        project_id: None,
                        member_name: m.name.clone(),
                        status: "invalid",
                    }),
                    ken_core::workspace::MemberStatus::Ok(_) => None,
                })
                .collect()
        };
        // No scope: this caller is the all-projects search by definition.
        let dto = route_search(app, state, query, Some(limit), None, None, audience, types).await?;
        return Ok(adapt_route_search_to_all_projects(dto, manifest_extras));
    }

    let (actives, roster) = {
        let guard = state.lock().unwrap();
        if !workspace_enabled(&guard.app_settings) {
            return Err(WORKSPACE_DISABLED_MSG.into());
        }
        let ws = guard.workspace.as_ref().ok_or("no workspace open")?;
        let mut actives: Vec<(uuid::Uuid, String, Arc<Mutex<Db>>)> = Vec::new();
        let mut roster: Vec<AllProjectsMemberStatusDto> = Vec::new();
        for m in &ws.ws.members {
            match &m.status {
                ken_core::workspace::MemberStatus::Ok(p) => {
                    let pid = p.config.id;
                    if let Some(rt) = guard.members.get(&pid) {
                        actives.push((pid, m.name.clone(), rt.search_db.clone()));
                        roster.push(AllProjectsMemberStatusDto {
                            project_id: Some(pid.to_string()),
                            member_name: m.name.clone(),
                            status: "searched",
                        });
                    } else {
                        roster.push(AllProjectsMemberStatusDto {
                            project_id: Some(pid.to_string()),
                            member_name: m.name.clone(),
                            status: "dormant",
                        });
                    }
                }
                ken_core::workspace::MemberStatus::Missing => roster.push(AllProjectsMemberStatusDto {
                    project_id: None,
                    member_name: m.name.clone(),
                    status: "missing",
                }),
                ken_core::workspace::MemberStatus::Invalid(_) => roster.push(AllProjectsMemberStatusDto {
                    project_id: None,
                    member_name: m.name.clone(),
                    status: "invalid",
                }),
            }
        }
        // ken-memory task 2.1: the pseudo-member is deliberately never in
        // `ws.ws.members` (kept out of the manifest/member list), so the
        // loop above can never find it. Fold it into the FTS fan-out
        // directly from `AppState::members` when resident — this is the one
        // fan-out path that doesn't already reach it for free the way
        // `route_search` does (it iterates `guard.members` directly with no
        // filtering, so needs no change here).
        {
            if let Some(pseudo_id) = memory_pseudo_member_id(&guard) {
                if let Some(rt) = guard.members.get(&pseudo_id) {
                    actives.push((pseudo_id, WORKSPACE_MEMORY_LABEL.to_string(), rt.search_db.clone()));
                    roster.push(AllProjectsMemberStatusDto {
                        project_id: Some(pseudo_id.to_string()),
                        member_name: WORKSPACE_MEMORY_LABEL.to_string(),
                        status: "searched",
                    });
                }
            }
        }
        (actives, roster)
    };

    // Each active member's FTS, off the global lock on the blocking pool,
    // all members at once: twenty repos cost the slowest one, not the sum.
    let q = query.clone();
    let per_member: Vec<(String, String, Vec<SearchHit>)> =
        tauri::async_runtime::spawn_blocking(move || {
            std::thread::scope(|s| {
                let handles: Vec<_> = actives
                    .into_iter()
                    .map(|(pid, name, db)| {
                        let q = &q;
                        let audience = audience.as_deref();
                        let wanted = &wanted;
                        s.spawn(move || {
                            let db = db.lock().unwrap();
                            let by_reader = audience.is_some_and(|a| !a.is_empty() && a != "any");
                            let fetch = if !wanted.is_empty() { limit * 5 } else if by_reader { limit * 3 } else { limit };
                            let mut hits = db.search(q, fetch).unwrap_or_default();
                            if by_reader || !wanted.is_empty() {
                                hits.retain(|h| {
                                    ken_core::contenttype::is_wanted(&h.rel_path, wanted)
                                        && ken_core::pagemeta::suits(audience, ken_core::pagemeta::audience_at(&db, &h.rel_path))
                                });
                                hits.truncate(limit);
                            }
                            (pid.to_string(), name, hits)
                        })
                    })
                    .collect();
                handles.into_iter().filter_map(|h| h.join().ok()).collect()
            })
        })
        .await
        .map_err(|e| e.to_string())?;

    // Round-robin rank interleave: position 0 of every member, then 1, … up
    // to `limit`. Each member's hits are already rank-ordered by `Db::search`.
    let mut results: Vec<AllProjectsHitDto> = Vec::new();
    let max_len = per_member.iter().map(|(_, _, h)| h.len()).max().unwrap_or(0);
    'outer: for i in 0..max_len {
        for (pid, name, hits) in &per_member {
            if let Some(hit) = hits.get(i) {
                results.push(AllProjectsHitDto {
                    content_type: ken_core::contenttype::of(&hit.rel_path).as_str(),
                    project_id: pid.clone(),
                    member_name: name.clone(),
                    hit: hit.clone(),
                });
                if results.len() >= limit {
                    break 'outer;
                }
            }
        }
    }

    Ok(SearchAllProjectsDto { results, member_status: roster })
}

/// Say what a repo is for: its kinds and team. Kept in the local registry,
/// never in the repo. Sync follows the kind unless `project.json` sets it,
/// and the dot is refreshed at once for an open member.
#[tauri::command]
fn set_project_kind(
    app: AppHandle,
    state: State<SharedState>,
    id: String,
    kind: Vec<ken_core::registry::RepoKind>,
    team: Option<String>,
) -> CmdResult<Vec<RegistryEntryStatus>> {
    let guard = state.lock().unwrap();
    let uuid: uuid::Uuid = id.parse().map_err(err)?;
    let mut registry = Registry::load(&guard.base_dir).map_err(err)?;
    if !registry.set_kind(uuid, kind, team) {
        return Err(format!("no project with id {id}"));
    }
    registry.save(&guard.base_dir).map_err(err)?;
    if let Some(member) = guard.members.get(&uuid) {
        let status = sync_status_of(&member.project);
        emit_member(&app, uuid, "sync-state", SyncStateEvent {
            state: if status.active { "synced" } else { "off" }.into(),
            detail: None,
        });
        rescan_member(&app, &guard.base_dir, &member.project);
    }
    Ok(registry.statuses())
}

/// Scan an open repo again in the background, after what it is changed: a
/// repo given its kind is read for the first time, and one whose index state
/// changed has its files re-tiered.
fn rescan_member(app: &AppHandle, base: &Path, project: &Project) {
    let (app, base, project) = (app.clone(), base.to_path_buf(), project.clone());
    std::thread::spawn(move || {
        if let Ok(mut db) = Db::open(&base, project.config.id) {
            if let Ok(stats) = scan::scan(&project, &mut db) {
                emit_member(&app, project.config.id, "index-updated", stats);
            }
        }
    });
}

/// Set how deep Ken reads a repo (off / search / entities), or clear the
/// choice so it follows the kind. The next scan re-tiers its files.
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

#[tauri::command(async)]
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
    let info = if let Some(active) = guard.members.get_mut(&uuid) {
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
    active.project.set_excluded(excluded).map_err(err)?;
    let stats = scan::scan(&active.project, &mut active.db).map_err(err)?;
    let info = ProjectInfo::of(&active.project);
    let project_id = active.project.config.id;
    let videos = stats.videos_needing_transcript.clone();
    drop(guard);
    enqueue_transcriptions(&app, state.inner(), project_id, &videos);
    emit_member(&app, project_id, "index-updated", stats);
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
                    excluded: project.is_excluded(&rel),
                    rel_path: rel,
                });
            }
        }
    }
    folders.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    let files = files
        .into_iter()
        .map(|row| FileRowDto::new(row, &project))
        .collect();
    Ok(TreeData { files, folders })
}

/// The whole workspace as ONE tree: every resolvable member's files and
/// folders, each path prefixed with the member's folder name so the top
/// level of the tree is the projects themselves.
///
/// Deliberately the same `TreeData` shape as [`get_tree`], so `FileTree`
/// renders it with no changes — a prefixed path is just a path with one
/// more leading segment. The prefix is also what makes a selection
/// resolvable: `app.openTab` splits it back off, focuses that member, and
/// opens the remainder (see its comment), which is why the prefix is the
/// MANIFEST folder name rather than the display name — folder names are
/// unique within a parent, display names need not be.
///
/// Dormant members are read the same way `route_search` reads them: their
/// index opens by project id without activating anything.
#[tauri::command(async)]
fn get_tree_all(state: State<SharedState>) -> CmdResult<TreeData> {
    let (base_dir, members) = {
        let guard = state.lock().unwrap();
        if !workspace_enabled(&guard.app_settings) {
            return Err(WORKSPACE_DISABLED_MSG.into());
        }
        let ws = guard.workspace.as_ref().ok_or("no workspace open")?;
        let members: Vec<(String, ken_core::project::Project, Option<Arc<Mutex<Db>>>)> = ws
            .ws
            .members
            .iter()
            .filter_map(|m| match &m.status {
                ken_core::workspace::MemberStatus::Ok(p) => Some((
                    m.name.clone(),
                    p.clone(),
                    guard.members.get(&p.config.id).map(|r| r.search_db.clone()),
                )),
                _ => None,
            })
            .collect();
        (guard.base_dir.clone(), members)
    };

    let mut files: Vec<FileRowDto> = Vec::new();
    let mut folders: Vec<FolderInfo> = Vec::new();

    for (folder_name, project, live) in members {
        // Each member is a folder in the merged tree — and a nested member
        // ("SR/ShatteredRealms") needs its group folder ("SR") emitted
        // too, or the tree has a node whose parent doesn't exist. Emitted
        // per member and de-duplicated by the sort+dedup below, so two SR
        // members yield one SR node.
        if let Some(group) = ken_core::workspace::member_group(&folder_name) {
            folders.push(FolderInfo {
                excluded: false,
                rel_path: group.to_string(),
            });
        }
        folders.push(FolderInfo {
            excluded: false,
            rel_path: folder_name.clone(),
        });

        let rows = match live {
            Some(db) => db.lock().unwrap().list_files().unwrap_or_default(),
            None => Db::open(&base_dir, project.config.id)
                .ok()
                .and_then(|db| db.list_files().ok())
                .unwrap_or_default(),
        };
        for mut row in rows {
            row.rel_path = format!("{folder_name}/{}", row.rel_path);
            files.push(FileRowDto::new(row, &project));
        }

        // Folders from disk, same as `get_tree`, so an excluded or empty
        // one still appears under its project.
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
                        excluded: project.is_excluded(&rel),
                        rel_path: format!("{folder_name}/{rel}"),
                    });
                }
            }
        }
    }

    folders.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    folders.dedup_by(|a, b| a.rel_path == b.rel_path);
    Ok(TreeData { files, folders })
}

#[tauri::command]
async fn search(
    state: State<'_, SharedState>,
    query: String,
    limit: Option<usize>,
) -> CmdResult<Vec<SearchHit>> {
    // Hold the global lock only long enough to clone the Arc (microseconds),
    // then release it so the FTS query runs on the dedicated read-only handle
    // without serializing against every other command and background worker.
    let search_db = {
        let guard = state.lock().unwrap();
        member(&guard, None)?.search_db.clone()
    };
    // Run the (potentially expensive — a short/common query can touch many
    // rows) DB work on the blocking pool. As a synchronous command this ran on
    // the main thread and could freeze the whole UI event loop; offloading
    // keeps IPC responsive.
    tauri::async_runtime::spawn_blocking(move || {
        let db = search_db.lock().unwrap();
        db.search(&query, limit.unwrap_or(30)).map_err(err)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// One path-grouped hit from `hybrid_search`. Mirrors `ken_core::search::HybridHit`
/// but adds `tier` (kenignore task 2.4, so the frontend can badge search-only
/// hits) and serializes `source` as a lowercase string instead of a Rust enum.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct HybridSearchHitDto {
    path: String,
    chunk_id: i64,
    snippet: String,
    source: &'static str,
    /// `0` = Full, `1` = SearchOnly (matches `chunks.tier` / `kenignore::Tier`
    /// minus `Ignore`, which never has a row to badge). `None` if the chunk
    /// row's tier couldn't be looked up (shouldn't normally happen for a hit
    /// that came from the chunks tables themselves).
    tier: Option<i64>,
    /// The line the chunk starts on, when known.
    line: Option<i64>,
    /// For a Markdown page: section, freshness, retired or generated.
    page: Option<ken_core::pagemeta::HitPage>,
    /// What the file is for (`contenttype::of`).
    content_type: &'static str,
}

/// Chunk-level hybrid (keyword + semantic) search (semantic-index task 2.2).
/// Always runs FTS. Also runs a KNN pass — merged in via
/// `ken_core::search::merge_and_rerank`: "B4 FTS-priority fill" (FTS hits keep
/// their order and snippet; KNN only fills in paths FTS missed) followed by a
/// deterministic precision rerank (filename/path/snippet token matches,
/// semantic proximity, agreement bonus) — the KNN pass only runs
/// when the project's `semanticIndex` flag is on, the DB actually has a
/// vector index (`vec_available`), and a live embedder is installed; in any
/// other case this transparently degrades to the exact plain-FTS results
/// `search` would give, per the proposal's "flag off ... → exact current FTS
/// path" requirement. Every hit is annotated with its stored `chunks.tier`
/// (kenignore task 2.4) so the frontend can badge search-only matches.
#[tauri::command]
async fn hybrid_search(
    state: State<'_, SharedState>,
    query: String,
    limit: Option<usize>,
    audience: Option<String>,
    types: Option<String>,
) -> CmdResult<Vec<HybridSearchHitDto>> {
    let limit = limit.unwrap_or(30);
    let wanted = ken_core::contenttype::parse_filter(types.as_deref());
    let (search_db, semantic_on, embedder_slot) = {
        let guard = state.lock().unwrap();
        let active = member(&guard, None)?;
        (
            active.search_db.clone(),
            semantic_index_enabled(&guard.app_settings, &active.project),
            active.semantic_embedder.clone(),
        )
    };

    // Embedding the query needs `&mut` on the embedder and can call into a
    // local model, so it's kept off the async executor same as the DB work
    // below — do both inside the one `spawn_blocking` closure.
    tauri::async_runtime::spawn_blocking(move || {
        let db = search_db.lock().unwrap();

        let query_vec = if semantic_on && db.vec_available() {
            let mut guard = embedder_slot.lock().unwrap();
            guard
                .as_deref_mut()
                .and_then(|embedder| embedder.embed_query(&query).ok())
        } else {
            None
        };
        // The one shared composition (keyword + semantic, vocabulary
        // alternatives, lines, page facts and bands), same as workspace
        // search and the MCP.
        // Item 2b: keep what suits the reader chosen (business, dev, any),
        // reading further down the ranking so a filter still fills the page.
        let filtered = audience.as_deref().is_some_and(|a| !a.is_empty() && a != "any");
        let fetch = if filtered { limit * 3 } else { limit };
        let mut merged = ken_core::routing::search_member_of(&db, &query, query_vec.as_deref(), fetch, &wanted).map_err(err)?;
        merged.retain(|h| ken_core::pagemeta::suits(audience.as_deref(), h.page.as_ref().and_then(|p| p.audience)));
        merged.truncate(limit);
        let chunk_ids: Vec<i64> = merged.iter().map(|h| h.chunk_id).collect();
        let tiers = db.chunk_tiers(&chunk_ids).map_err(err)?;

        Ok(merged
            .into_iter()
            .map(|h| {
                let tier = tiers.get(&h.chunk_id).copied();
                let content_type = ken_core::contenttype::of(&h.path).as_str();
                let source = match h.source {
                    hybrid_search_mod::Source::Keyword => "keyword",
                    hybrid_search_mod::Source::Semantic => "semantic",
                    hybrid_search_mod::Source::Both => "both",
                };
                HybridSearchHitDto {
                    path: h.path,
                    chunk_id: h.chunk_id,
                    snippet: h.snippet,
                    source,
                    tier,
                    line: h.line,
                    content_type,
                    page: h.page,
                }
            })
            .collect())
    })
    .await
    .map_err(|e| e.to_string())?
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
    // registry (mirrors `rename_project`), so overrides read true even when it
    // isn't active.
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
    let (base, project, sync, auto_knowledge) = {
        let guard = state.lock().unwrap();
        let active = member(&guard, None)?;
        (
            guard.base_dir.clone(),
            active.project.clone(),
            active.sync.clone(),
            active.auto_knowledge.clone(),
        )
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
    // file, so a just-downloaded doc reaches ingests, Map/Timeline, and sync
    // immediately instead of waiting for an edit or a manual reindex.
    if changed {
        let paths = vec![rel_path.clone()];
        sync.changed(paths.clone());
        auto_knowledge.changed();
        emit_member(
            &app,
            project_id,
            "index-updated",
            ScanStats {
                changed_paths: paths,
                ..Default::default()
            },
        );
    }
    Ok(())
}

/// Post-write bookkeeping shared by `save_file` and `save_file_bytes`: reindex
/// the file, mark the version as seen, notify the frontend, return the mtime.
/// Assumes the bytes are already on disk at `rel_path`.
fn finish_save(app: &AppHandle, project: &Project, base: &Path, rel_path: &str) -> CmdResult<i64> {
    let abs = project.resolve(rel_path).map_err(err)?;
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

/// The focused member's project and the app data dir, cloned under the lock
/// for a save that writes and indexes after releasing it.
fn save_target(state: &SharedState) -> CmdResult<(Project, PathBuf)> {
    let guard = lock_tolerant(state);
    let active = member(&guard, None)?;
    Ok((active.project.clone(), guard.base_dir.clone()))
}

// Saves stay on the main thread, so they run in the order they were made.
#[tauri::command]
fn save_file(
    app: AppHandle,
    state: State<SharedState>,
    rel_path: String,
    content: String,
) -> CmdResult<i64> {
    let (project, base) = save_target(state.inner())?;
    let abs = project.resolve(&rel_path).map_err(err)?;
    std::fs::write(&abs, &content).map_err(err)?;
    finish_save(&app, &project, &base, &rel_path)
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
    let (project, base) = save_target(state.inner())?;
    let abs = project.resolve(&rel_path).map_err(err)?;
    std::fs::write(&abs, &bytes).map_err(err)?;
    finish_save(&app, &project, &base, &rel_path)
}

/// Create a NEW folder `name` inside `dir_rel` ("" = root) for a folder dropped
/// in from the OS; returns its project-relative path. A taken name is deduped
/// (`Photos` → `Photos-1`) so a drop never merges into an existing folder.
#[tauri::command]
fn create_unique_folder(state: State<SharedState>, dir_rel: String, name: String) -> CmdResult<String> {
    let guard = state.lock().unwrap();
    let active = member(&guard, None)?;
    let dir = active.project.resolve(&dir_rel).map_err(err)?;
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

    // Same shape as `save_file`: clone the focused member's project under the
    // lock, then write and index without holding it.
    let (project, base) = save_target(state.inner())?;
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
    finish_save(&app, &project, &base, &rel_path)?;
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
    Ok(row.map(|r| FileRowDto::new(r, &active.project)))
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
    let (project, base, sync, knowledge, running) = {
        let guard = state.lock().unwrap();
        let active = member(&guard, None)?;
        (
            active.project.clone(),
            guard.base_dir.clone(),
            active.sync.clone(),
            active.auto_knowledge.clone(),
            active.reindex_running.clone(),
        )
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
                        // changed set to sync, trigger a knowledge rebuild, and
                        // re-enqueue any videos that still need transcripts.
                        if !stats.changed_paths.is_empty() {
                            sync.changed(stats.changed_paths.clone());
                        }
                        knowledge.changed();
                        enqueue_transcriptions(
                            &bg_app,
                            &bg_state,
                            project_id,
                            &stats.videos_needing_transcript,
                        );
                        emit_member(&bg_app, project_id, "index-updated", stats);
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
        // The folder is no longer being walked — a knowledge build may now read
        // a complete file list.
        knowledge.scan_finished();
        running.store(false, Ordering::SeqCst);
    });

    // Return immediately; the real stats arrive on the `index-updated` event.
    Ok(ScanStats::default())
}

// ===========================================================================
// project-profiler: deterministic scan + optional local-LLM refinement of a
// project's shape, written to `.ken/index-profile.json` (ken-core's
// `profiler.rs`, task 1.x). This section is the src-tauri wiring (task 2.x):
// the `profile_project` command for an open member, `profile_candidates` for
// workspace-creation folders that aren't projects yet, and the shared
// scan/refine/save pass both call into.
// ===========================================================================

/// Frontend-facing view of a `ken_core::profiler::ProjectProfile` (camelCase;
/// `chunking` and `generated_hash` are internal wiring the settings/
/// workspace-creation UI has no use for, so they're left off rather than
/// exposed and ignored).
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProfileDto {
    kind: ken_core::profiler::ProjectKind,
    summary: String,
    languages: Vec<String>,
    excludes: Vec<String>,
    focus_hints: Vec<String>,
    hand_edited: bool,
}

impl ProfileDto {
    fn of(profile: &ken_core::profiler::ProjectProfile) -> ProfileDto {
        ProfileDto {
            kind: profile.kind,
            summary: profile.summary.clone(),
            languages: profile.languages.clone(),
            excludes: profile.excludes.clone(),
            focus_hints: profile.focus_hints.clone(),
            hand_edited: profile.is_hand_edited(),
        }
    }
}

/// Payload for the `profile-state` event (project-profiler task 2.1),
/// mirroring `SemanticIndexStateEvent`'s state-tag shape:
/// `{"state":"scanning"}`, `{"state":"refining"}`,
/// `{"state":"ready","profile":{...}}`, `{"state":"error","reason":"..."}`.
/// Delivered via `emit_member` (keyed by `project_id`) for `profile_project`,
/// or a path-keyed envelope (see `CandidateProfileEnvelope`) for
/// `profile_candidates` — the frontend tells the two sources apart by which
/// key accompanies this same tag shape.
#[derive(Clone, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
enum ProfileStateEvent {
    Scanning,
    Refining,
    Ready { profile: ProfileDto },
    Error { reason: String },
}

/// `profile_candidates`' per-candidate envelope (task 2.2): candidate
/// folders being profiled during workspace creation aren't open projects —
/// no `Uuid`, no `MemberRuntime` — so events key off the candidate's
/// absolute path instead of `emit_member`'s `project_id`. Same event name
/// (`profile-state`) and same inner shape as the project-scoped version.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CandidateProfileEnvelope {
    path: String,
    #[serde(flatten)]
    state: ProfileStateEvent,
}

/// Result of one `scan_and_profile` pass: the profile that ended up on disk,
/// and whether it's worth kicking a re-ingest over (task 2.3).
struct ProfileOutcome {
    profile: ken_core::profiler::ProjectProfile,
    /// `true` when `excludes` or `chunking` differ from what was on disk
    /// before this pass. `summary`/`focus_hints`/`kind` changing alone
    /// doesn't warrant a re-ingest — nothing chunking- or exclusion-relevant
    /// changed.
    changed: bool,
}

/// The deterministic-scan → optional-refinement → save pass shared by
/// `profile_project` (one open member) and `profile_candidates` (workspace-
/// creation folders, task 2.2) — both just run this on a background thread
/// with a differently-scoped `emit` closure. Never called while holding the
/// global `AppState` lock: `scan_stats` walks the filesystem and the
/// refinement step may block on the local LLM, both of which would freeze
/// every other open member's commands if run under the guard (lock-audit.md
/// write template).
///
/// Hand-edit protection (design D2 / spec "hand-edited profile is
/// preserved"): if the on-disk profile has diverged from the hash Ken
/// stamped on its own last write, this refuses to overwrite it and emits
/// `error` instead of silently clobbering the user's edits.
/// `ProjectProfile::save`'s doc comment assigns the "get explicit user
/// confirmation before calling save again" responsibility to "task 2.x/3.x"
/// jointly; task 3.x (workspace-creation/settings UI, not built yet) is
/// where an actual confirm-then-force flow belongs. Until it lands, refusing
/// outright is the only safe behavior this command can offer on its own —
/// deviation noted in tasks.md.
///
/// LLM refinement runs only when the local model is `Ready` right now
/// (`local_llm::llm_status()`); the design's "skippable (setting)" toggle
/// has no backing setting anywhere in this codebase yet, so model
/// availability is the sole gate — noted as a deviation in tasks.md.
/// Refinement failure (timeout, unparseable output — `generate_stream`
/// returning `Err`) is not an error: the deterministic profile already
/// saved above stands, matching the spec's "model failure falls back...
/// reaches ready, not error".
fn scan_and_profile(
    root: &Path,
    excluded: &[String],
    emit: impl Fn(ProfileStateEvent),
) -> Option<ProfileOutcome> {
    emit(ProfileStateEvent::Scanning);

    let stats = match ken_core::profiler::scan_stats(root, excluded) {
        Ok(s) => s,
        Err(e) => {
            emit(ProfileStateEvent::Error { reason: e.to_string() });
            return None;
        }
    };

    let existing = ken_core::profiler::ProjectProfile::load(root);
    if existing.is_hand_edited() {
        emit(ProfileStateEvent::Error {
            reason: "index-profile.json has been hand-edited — re-analyze needs an \
                      explicit overwrite confirmation that the workspace-creation/ \
                      settings UI doesn't build yet; delete .ken/index-profile.json \
                      to force a fresh scan"
                .into(),
        });
        return None;
    }

    let mut profile = ken_core::profiler::deterministic_profile(&stats);
    if let Err(e) = profile.save(root) {
        emit(ProfileStateEvent::Error { reason: e.to_string() });
        return None;
    }

    if LOCAL_GENERATION && matches!(ken_core::local_llm::llm_status(), ken_core::local_llm::LlmStatus::Ready) {
        emit(ProfileStateEvent::Refining);
        let tree = ken_core::profiler::tree_sample(&stats);
        let prompt = ken_core::profiler::compose_profile_prompt(&stats, &tree);
        let mut sink = |_: &str| true;
        if let Ok(text) = ken_core::local_llm::generate_stream(
            &prompt,
            ken_core::local_llm::Priority::Background,
            &mut sink,
        ) {
            let refinement = ken_core::profiler::parse_profile_refinement(&text);
            ken_core::profiler::apply_refinement(&mut profile, &refinement, root);
            if let Err(e) = profile.save(root) {
                // The deterministic profile already landed above — a failed
                // refinement save degrades to "no refinement", not an error.
                eprintln!("warning: profile refinement save failed: {e}");
            }
        }
    }

    let changed = existing.excludes != profile.excludes || existing.chunking != profile.chunking;
    emit(ProfileStateEvent::Ready { profile: ProfileDto::of(&profile) });
    Some(ProfileOutcome { profile, changed })
}

/// project-profiler task 2.3: after a profile save changes `excludes` or
/// `chunking`, resume the semantic-index build with the fresh profile so
/// the new chunking actually takes effect on the `chunks` table — reusing
/// exactly the mechanism `set_project_feature("semanticIndex", true)`
/// already drives (`apply_semantic_index_flag`), which as of this change
/// resolves and passes the stored profile itself whenever the `profiler`
/// flag is on (see that function's doc comment). A no-op when semantic
/// indexing isn't already enabled for this project: there's nothing to
/// rebuild, and calling `apply_semantic_index_flag(.., true)`
/// unconditionally would incorrectly turn semantic indexing ON as a side
/// effect of profiling — the two flags must stay independent.
fn maybe_rebuild_semantic_index_for_profile(
    app: &AppHandle,
    state: &SharedState,
    target: Option<uuid::Uuid>,
) {
    let enabled = {
        let guard = state.lock().unwrap();
        match member(&guard, target) {
            Ok(active) => semantic_index_enabled(&guard.app_settings, &active.project),
            Err(_) => false, // member closed mid-profile — nothing to rebuild
        }
    };
    if enabled {
        let _ = apply_semantic_index_flag(app, state, target, true);
    }
}

/// Profile one open member: deterministic scan → save → optional
/// Background-priority LLM refinement → re-save (task 2.1). Gated on the
/// per-project `profiler` flag (spec: flag off touches nothing) — off
/// returns `Err` immediately rather than silently scanning. Returns as soon
/// as the background thread is spawned; progress reaches the frontend via
/// `profile-state` events scoped to `project_id` (`emit_member`). See
/// `scan_and_profile`'s doc comment for the hand-edit and refinement-gating
/// deviations.
#[tauri::command(async)]
fn profile_project(
    app: AppHandle,
    state: State<SharedState>,
    project_id: Option<String>,
) -> CmdResult<()> {
    let target: Option<uuid::Uuid> = project_id
        .as_deref()
        .map(|s| s.parse::<uuid::Uuid>())
        .transpose()
        .map_err(err)?;

    let (root, excluded, resolved_id, running) = {
        let guard = state.lock().unwrap();
        let active = member(&guard, target)?;
        // ken-memory task 2.1 (D3): the workspace pseudo-member is excluded
        // from profiler candidates. It's unreachable from the frontend today
        // (never listed in `workspace_overview`), but this is the actual
        // enforcement point, not just an absence-from-the-UI accident.
        if Some(active.project.config.id) == memory_pseudo_member_id(&guard) {
            return Err("the workspace-memory member can't be profiled".into());
        }
        if !ken_core::features::effective_flag(&guard.app_settings, &active.project, "profiler") {
            return Err("profiler flag is off for this project".into());
        }
        (
            active.project.root.clone(),
            active.project.config.excluded.clone(),
            active.project.config.id,
            active.profiling_running.clone(),
        )
    };

    if running.swap(true, Ordering::SeqCst) {
        return Err("this project is already being profiled".into());
    }

    let bg_app = app.clone();
    let bg_state = state.inner().clone();
    std::thread::spawn(move || {
        let emit_app = bg_app.clone();
        let outcome = scan_and_profile(&root, &excluded, |evt| {
            emit_member(&emit_app, resolved_id, "profile-state", evt);
        });
        if outcome.is_some_and(|o| o.changed) {
            maybe_rebuild_semantic_index_for_profile(&bg_app, &bg_state, Some(resolved_id));
        }
        running.store(false, Ordering::SeqCst);
    });

    Ok(())
}

/// Profile each selected workspace-creation candidate folder (task 2.2):
/// deterministic scan + save first (design D5 — the UI can show a kind
/// badge immediately), optional LLM refinement streamed in after,
/// concurrency 2 (same idea as the ingest semaphore) via two worker threads
/// sharing a path queue. Each candidate's `.ken/index-profile.json` is
/// written directly to its own folder (adopt-if-exists, design D2), so the
/// profile is already in place if/when the folder becomes a real project —
/// `create_project`/`open_member` need no changes to pick it up. A failed
/// candidate never blocks the others or the command's return (spec:
/// "per-candidate profiling failure SHALL never block creation").
///
/// Deviation (tasks.md 2.2): the workspace-creation UX itself (candidate
/// picker, "Skip analysis" control, confirm screen) doesn't exist yet —
/// Phase 2 built only `open_member`/`close_member`. This command is the
/// honestly-implementable slice: it profiles whatever paths a (future)
/// picker UI passes it. Wiring an actual creation flow around it, including
/// the skip control and kind-badge display, is frontend work deferred to
/// task 3.x.
///
/// Not gated on the `profiler` flag the way `profile_project` is: none of
/// these paths is a `Project` yet, so there's no per-project `features` map
/// to read an override from. The (future) workspace-creation UI is expected
/// to check the *global* `profiler` default (`list_features` with no
/// `project_id`) before calling this at all.
#[tauri::command(async)]
fn profile_candidates(app: AppHandle, paths: Vec<String>) -> CmdResult<()> {
    if paths.is_empty() {
        return Ok(());
    }
    let queue = Arc::new(Mutex::new(paths));
    const CANDIDATE_CONCURRENCY: usize = 2;
    for _ in 0..CANDIDATE_CONCURRENCY {
        let queue = queue.clone();
        let worker_app = app.clone();
        std::thread::spawn(move || loop {
            let path = { queue.lock().unwrap().pop() };
            let Some(path) = path else { break };
            let root = PathBuf::from(&path);
            let emit_app = worker_app.clone();
            let emit_path = path.clone();
            let _ = scan_and_profile(&root, &[], |evt| {
                let _ = emit_app.emit(
                    "profile-state",
                    CandidateProfileEnvelope { path: emit_path.clone(), state: evt },
                );
            });
        });
    }
    Ok(())
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
/// The transcript goes into the team wiki's Raw/ (`team`'s library) when
/// the take stops. Refused when no requested source could start.
#[tauri::command]
fn record_start(
    app: AppHandle,
    state: State<SharedState>,
    mic: bool,
    system: bool,
    device_id: Option<String>,
    team: Option<String>,
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
    let project_id = inbox_member(&app, &state, team.as_deref())?;
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
            "The team wiki closed before the transcript was written. The audio is kept in {}.",
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
    let (base_dir, ws_root) = {
        let mut guard = state.lock().unwrap();
        if let Some(m) = guard.members.get_mut(&take.project_id) {
            let _ = scan::refresh_path(&m.project, &mut m.db, &md_rel);
        }
        (guard.base_dir.clone(), guard.workspace.as_ref().map(|w| w.ws.root.clone()))
    };
    emit_member(app, take.project_id, "index-updated", ScanStats::default());
    start_ingest_pass(&project, &base_dir, ws_root, true);
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
    if from_is_dir {
        // Pure index bookkeeping: every child row moves to the new prefix with
        // its content and queue state intact. No rescan, no re-extraction.
        reindex_moved(&mut active.db, &from_rel, &to_rel).map_err(err)?;
        let project_id = active.project.config.id;
        drop(guard);
        // Nothing was re-read, so there are no fresh stats and no new
        // transcription work — just tell the UI the index moved.
        emit_member(&app, project_id, "index-updated", ScanStats::default());
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
    deindex_removed(&active.project, &mut active.db, &rel_path, is_dir).map_err(err)?;
    let project_id = active.project.config.id;
    drop(guard);
    emit_member(&app, project_id, "index-updated", ScanStats::default());
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
    let (root, project, sync, auto_knowledge) = {
        let guard = state.lock().unwrap();
        let active = member(&guard, None)?;
        (
            active.project.root.clone(),
            active.project.clone(),
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
        sync.changed(paths.clone());
        auto_knowledge.changed();
        emit_member(
            &app,
            project.config.id,
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
        member(&guard, None)?.project.root.clone()
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

#[tauri::command(async)]
fn mcp_info(state: State<SharedState>) -> CmdResult<McpInfo> {
    let guard = state.lock().unwrap();
    let active = member(&guard, None)?;
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
server binary, ken-mcp, runs on demand over stdio. Its tools: search (route_query \
across the workspace, semantic_search, search_knowledge, kg_search), reading \
(read_document, list_documents, list_projects), code navigation (find_definition, \
find_usages, file_outline, related_files), history, the person's Your day tasks and \
tickets (task_list, ticket_list), and the team inbox (family_list, family_inbox). It \
never edits the person's files. Five tools write: task_create and task_update (Your \
day tasks), memory_write and journal_append (Ken's memory and journal), and \
family_send, which commits and pushes an item to the team's inbox repo.\n\n\
If you are Claude Code, register it by running:\n\n  {add_command}\n\n\
Otherwise, add this to your MCP configuration:\n\n{json_config}\n\n\
Once connected, start with route_query to find what answers a question, and \
read_document to read it."
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
                emit_member(&app, job.project_id, "index-updated", ScanStats::default());
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
    active
        .db
        .resolve_review_item(id, engine::now_epoch())
        .map_err(err)
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
}

fn sync_status_of(project: &Project) -> SyncStatus {
    let is_git = sync::is_git_repo(&project.root);
    let (remote, branch) = if is_git {
        sync::remote_and_branch(&project.root)
    } else {
        (None, None)
    };
    let auto = sync::sync_auto(project, &ken_core::registry::kind_of(&project.root));
    SyncStatus {
        mode: if is_git { "git" } else { "drive" }.into(),
        auto,
        active: is_git && auto && remote.is_some(),
        remote,
        branch,
    }
}

#[tauri::command(async)]
fn sync_status(state: State<SharedState>) -> CmdResult<SyncStatus> {
    // git runs twice: with the lock released.
    let project = {
        let guard = lock_tolerant(&state);
        member(&guard, None)?.project.clone()
    };
    Ok(sync_status_of(&project))
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

    let status = sync_status_of(&active.project);
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
    active.sync.changed(vec![path.clone()]);
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
    active.sync.changed(vec![survivor.clone(), copy_rel]);
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
    /// Every open conflict and conflicted copy in the team's open repos,
    /// newest first.
    conflicts: Vec<ConflictItemDto>,
    /// How many of them are conflicted copies (a shared drive's).
    conflicted_copies: usize,
}

/// The banner at the top of Files: the sync conflicts and conflicted copies
/// waiting in the chosen team's open repos (every open repo with no team),
/// less those on files I ignore.
#[tauri::command(async)]
fn files_banner(state: State<SharedState>, team: Option<String>) -> CmdResult<FilesBannerDto> {
    // Cloned under the lock, read after it is released: this is read again
    // on every index update.
    let (base_dir, repos) = {
        let guard = lock_tolerant(&state);
        let ids: Vec<(uuid::Uuid, String)> = match guard.workspace.as_ref() {
            Some(ws) => team_repos(&ws.ws, team.as_deref()).into_iter().map(|r| (r.id, r.name)).collect(),
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

/// `HH:MM`, local time — ken-memory task 2.2's `journal_append` caller-
/// supplies-time param (`memory::append_journal`'s `time_hhmm`), same
/// caller-passes-a-formatted-string discipline as `local_date_today` above.
fn local_time_hhmm() -> String {
    chrono::Local::now().format("%H:%M").to_string()
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

/// Build the local-model quick-answer prompt: the same grounding as the Claude
/// path (paragraph excerpts, one `rel_path: text` line per source), wrapped in
/// Qwen3 ChatML, ending with the `SOURCES:` convention `digest::parse_digest`
/// already understands.
fn local_quick_answer_prompt(query: &str, sources: &[(String, String)]) -> String {
    let mut material = String::new();
    for (rel_path, text) in sources {
        material.push_str(&format!("- {rel_path}: {text}\n"));
    }
    let system = "You answer questions using only the provided project material. \
Answer in one or two sentences. If the material doesn't answer it, say you don't \
know. End with a final line `SOURCES: path1, path2` listing the project-relative \
paths you used (omit the line if none).";
    let user = format!("Question: {query}\n\nMaterial:\n\n{material}");
    ken_core::local_llm::chatml_prompt(system, &user)
}

fn quick_answer_prompt(query: &str, sources: &[(String, String)]) -> String {
    let mut p = format!(
        "Question: {query}\n\nMaterial from the project's search index:\n\n"
    );
    for (rel_path, text) in sources {
        p.push_str(&format!("- {rel_path}: {text}\n"));
    }
    p.push_str(
        "\nAnswer the question in one or two sentences using ONLY this \
material; name the source paths you used; if the material doesn't answer \
it, say you don't know. End with a final line `SOURCES: path1, path2` \
listing the project-relative paths you used (omit the line if none).\n",
    );
    p
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
    // supersede counter (microseconds), then release it so the FTS query and the
    // per-source excerpt reads run on the dedicated read-only handle off the lock
    // (mirrors the `search` command), never freezing the UI event loop.
    let (search_db, root, project_id, qa_gen, claude) = {
        let guard = state.lock().unwrap();
        let active = member(&guard, None)?;
        (
            active.search_db.clone(),
            active.project.root.clone(),
            active.project.config.id,
            guard.qa_gen.clone(),
            ken_core::runner::discover_claude(),
        )
    };

    // The same meaningful-term tokenization `Db::search` now uses internally
    // (alphanumeric split minus English stopwords, with as-you-type guards), so
    // each excerpt centers on a term the search actually ranked on ("operating",
    // "model") rather than a stopword ("what", "the").
    let tokens = ken_core::db::significant_tokens(&query);

    // Search + build a ~350-char paragraph excerpt per hit on the blocking pool
    // (richer grounding than the ~12-token FTS snippet); fall back to the FTS
    // snippet when a source has no stored content (binary / OCR-pending).
    let query_for_search = query.clone();
    let sources: Vec<(String, String)> = tauri::async_runtime::spawn_blocking(
        move || -> CmdResult<Vec<(String, String)>> {
            let db = search_db.lock().unwrap();
            let hits = db.search(&query_for_search, 8).map_err(err)?;
            Ok(hits
                .into_iter()
                .map(|hit| {
                    let text = db.excerpt(&hit.rel_path, &tokens, 350).unwrap_or_else(|| {
                        hit.snippet.replace("<mark>", "").replace("</mark>", "")
                    });
                    (hit.rel_path, text)
                })
                .collect())
        },
    )
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
                        QuickAnswerEvent { query, body: parsed.body, sources: parsed.sources },
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
    sources: Vec<(String, String)>,
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
            QuickAnswerEvent { query, body: parsed.body, sources: parsed.sources },
        );
    }
}

/// The on-device language model's state, for the ⌘K "not installed" hint.
#[tauri::command(async)]
fn llm_status() -> &'static str {
    generator_status()
}

/// What maps, answers and judges in Ken (see [`LOCAL_GENERATION`]):
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
    /// Incremental coverage: files extracted / extractable files.
    analyzed: i64,
    total: i64,
    /// Files whose extraction failed permanently (retry budget exhausted).
    /// Excluded from `analyzed`, so the Map can show "X analyzed, Y failed of N"
    /// instead of a silent gap that looks identical to "no progress".
    failed: i64,
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
#[tauri::command(async)]
fn knowledge_model(state: State<SharedState>) -> CmdResult<KnowledgeModelDto> {
    let guard = state.lock().unwrap();
    let active = member(&guard, None)?;
    let (entities, edges) = active.db.list_entities_with_edges().map_err(err)?;
    let (analyzed, total) = active.db.extraction_coverage().map_err(err)?;
    let failed = active.db.extraction_failed_count().map_err(err)?;
    let (llm_status, llm_error) = (generator_status().to_string(), None::<String>);
    Ok(KnowledgeModelDto {
        entities,
        edges,
        events: active.db.list_events().map_err(err)?,
        built_at: active.db.knowledge_model_built_at().map_err(err)?,
        building: active.knowledge_running.load(Ordering::SeqCst),
        analyzed,
        total,
        failed,
        llm_status,
        llm_error,
    })
}

/// Run the drift sweep for a team or wiki repo when it is due (weekly unless
/// `drift.intervalDays` says otherwise), or now when `force`. It spawns git
/// per cited file, so callers run it off the UI thread. Returns the run.
fn run_drift_if_due(project: &Project, db: &mut Db, force: bool) -> Option<ken_core::drift::DriftRun> {
    if !ken_core::registry::is_library(&project.root) && !force {
        return None;
    }
    let now = engine::now_epoch();
    if !force && !ken_core::drift::due(db, project, now).unwrap_or(false) {
        return None;
    }
    match ken_core::drift::sweep(project, db, now) {
        Ok(run) => {
            if let Err(e) = ken_core::drift::file_run(db, &run) {
                eprintln!("warning: could not record the drift sweep: {e}");
            }
            Some(run)
        }
        Err(e) => {
            eprintln!("warning: drift sweep failed: {e}");
            None
        }
    }
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

/// The open workspace's folder, where Your day's tasks live.
fn workspace_root_of(state: &SharedState) -> Option<std::path::PathBuf> {
    lock_tolerant(state).workspace.as_ref().map(|w| w.ws.root.clone())
}

/// The Whisper model a pass transcribes with: the selected one when it is
/// installed, else any installed one.
fn transcription_model(base: &Path) -> Option<PathBuf> {
    model::selected_model_path(base, model::ModelCategory::Transcription)
        .or_else(|| Some(transcript::model_path(base)).filter(|p| p.is_file()))
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
        start_ingest_pass(&project, &base, workspace_root_of(&state), false);
    });
}

/// Read every new source waiting in a library's `Research/Ingestion/Raw/`,
/// one at a time on a background thread, through the Claude CLI
/// (`ingest::run_pass`): a recording is transcribed first with the selected
/// Whisper model; each source becomes its dated note and its card, and what
/// follows from it is written at once (my next steps into `ws_root`'s Your
/// day). A source that fails stays in Raw with its failure recorded and the
/// pass moves on; the pass stops only when the CLI itself fails. A library is
/// a repo with the inbox, whatever its kind (`registry::is_library`); `force`
/// reads any repo with an inbox. Returns whether a pass started or was
/// queued behind the one running.
fn start_ingest_pass(project: &Project, base: &Path, ws_root: Option<std::path::PathBuf>, force: bool) -> bool {
    let root = project.root.clone();
    if !ken_core::ingest::has_inbox(&root) || ken_core::ingest::waiting(&root).is_empty() {
        return false;
    }
    if !force && !ken_core::registry::is_library(&root) {
        return false;
    }
    let id = project.config.id;
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
    // Where a note's actions become tickets: the team repo on this library's team.
    let team_repo: Option<std::path::PathBuf> = Registry::load(&base).ok().and_then(|reg| {
        let team = reg.entry_at(&root)?.team.clone()?;
        reg.projects
            .iter()
            .find(|e| {
                e.team.as_deref() == Some(team.as_str())
                    && e.kind.contains(&ken_core::registry::RepoKind::Team)
                    && e.path.is_dir()
            })
            .map(|e| e.path.clone())
    });
    std::thread::spawn(move || {
        loop {
            INGEST_STOPPED.lock().unwrap().remove(&id);
            match Db::open(&base, id) {
                Ok(mut db) => {
                    let me = cached_git_me();
                    let stamp = local_stamp_now();
                    let targets = ken_core::ingest::Targets {
                        team_repo: team_repo.as_deref(),
                        workspace: ws_root.as_deref(),
                        me: &me,
                        stamp: &stamp,
                    };
                    let model = transcription_model(&base);
                    let ffmpeg = transcript::discover_ffmpeg();
                    let transcriber = ken_core::ingest::Transcriber { model: model.as_deref(), ffmpeg: ffmpeg.as_deref() };
                    let on_phase: Arc<dyn Fn(&str, ken_core::ingest::PassPhase) + Send + Sync> = Arc::new(move |raw, phase| {
                        INGEST_READING.lock().unwrap().insert(id, (raw.to_string(), phase));
                    });
                    let call = |prompt: &str| {
                        ken_core::ingest::call_result(ken_core::assistant::oneshot(
                            &binary,
                            &root,
                            prompt,
                            Duration::from_secs(600),
                            &CancelToken::new(),
                        ))
                    };
                    let today = local_date_today();
                    match ken_core::ingest::run_pass(&root, &mut db, &targets, &today, engine::now_epoch, transcriber, call, &on_phase) {
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

/// A pass over every open library, as after a transcription model is
/// installed: recordings that waited for one are read.
fn start_ingest_passes(state: &SharedState) {
    let (libraries, base, ws_root) = {
        let guard = lock_tolerant(state);
        let libs: Vec<Project> = guard
            .members
            .values()
            .map(|m| m.project.clone())
            .filter(|p| ken_core::ingest::has_inbox(&p.root))
            .collect();
        (libs, guard.base_dir.clone(), guard.workspace.as_ref().map(|w| w.ws.root.clone()))
    };
    for p in libraries {
        start_ingest_pass(&p, &base, ws_root.clone(), false);
    }
}

/// Every workspace member as the wiki sees it: folder, kinds and team from
/// the registry.
fn wiki_team_members(guard: &AppState) -> CmdResult<Vec<ken_core::wikidraft::TeamMember>> {
    let ws = guard.workspace.as_ref().ok_or("open the workspace first")?;
    let reg = Registry::load(&guard.base_dir).map_err(err)?;
    Ok(ws
        .ws
        .members
        .iter()
        .filter_map(|m| match &m.status {
            ken_core::workspace::MemberStatus::Ok(p) => {
                let entry = reg.entry_at(&p.root);
                Some(ken_core::wikidraft::TeamMember {
                    name: m.name.clone(),
                    root: p.root.clone(),
                    kind: entry.map(|e| e.kind.clone()).unwrap_or_default(),
                    team: entry.and_then(|e| e.team.clone()),
                })
            }
            _ => None,
        })
        .collect())
}

/// The wiki member's folder and project id.
fn wiki_target(guard: &AppState, wiki: &str) -> CmdResult<(std::path::PathBuf, uuid::Uuid)> {
    let ws = guard.workspace.as_ref().ok_or("open the workspace first")?;
    ws.ws
        .members
        .iter()
        .find(|m| m.name == wiki)
        .and_then(|m| match &m.status {
            ken_core::workspace::MemberStatus::Ok(p) => Some((p.root.clone(), p.config.id)),
            _ => None,
        })
        .ok_or_else(|| format!("{wiki} is not a member of this workspace"))
}

/// One model call through the Claude CLI, run in the wiki's folder.
/// Ken's generation policy: the on-device model embeds (meaning search) and
/// nothing else. Everything that writes, extracts, judges or answers runs
/// through Claude, in-agent Ken; the local 4B model, measured on a real
/// team's vault, left half its extractions unparseable at ~42 s each and
/// merged near-miss names it should not have.
const LOCAL_GENERATION: bool = false;

/// One prompt through Claude, from `root`; the error is for a person.
fn claude_text(root: &Path, prompt: &str) -> Result<String, String> {
    let binary = ken_core::runner::discover_claude().ok_or_else(|| ken_core::runner::MISSING_CLAUDE_HELP.to_string())?;
    claude_generate(binary, root.to_path_buf(), Vec::new())(prompt).map_err(|e| e.to_string())
}

/// Claude from `root`, also reading `dirs` (the team's repos a draft reads).
fn claude_generate(
    binary: std::path::PathBuf,
    root: std::path::PathBuf,
    dirs: Vec<std::path::PathBuf>,
) -> impl FnMut(&str) -> ken_core::Result<String> {
    move |prompt: &str| match ken_core::assistant::oneshot_in(&binary, &root, &dirs, prompt, Duration::from_secs(600), &CancelToken::new())? {
        ken_core::assistant::OneshotOutcome::Completed(text) => Ok(text),
        ken_core::assistant::OneshotOutcome::Failed(d) => Err(ken_core::Error::Other(d)),
        ken_core::assistant::OneshotOutcome::TimedOut => Err(ken_core::Error::Other("timed out".into())),
        ken_core::assistant::OneshotOutcome::Cancelled => Err(ken_core::Error::Other("cancelled".into())),
    }
}

/// The wikis whose first pages are being drafted, by member name.
fn drafting() -> &'static Mutex<std::collections::HashSet<String>> {
    static D: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    D.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// Whether Claude is drafting `wiki`'s pages now (Team shows it).
#[tauri::command(async)]
fn wiki_drafting(wiki: String) -> bool {
    lock_tolerant(drafting()).contains(&wiki)
}

/// Draft the first wiki pages (item 4b) into the workspace member `wiki`,
/// from its team's repos plus an optional folder of documents: a Repo Map
/// page per repo, each from that repo alone, then the team pages from those
/// pages. Runs in the background through the Claude CLI; the result shows
/// on the Team screen (`team_overview`'s findings). Never touches a page a
/// person wrote.
#[tauri::command(async)]
fn draft_wiki(app: AppHandle, state: State<SharedState>, wiki: String, extra: Option<String>) -> CmdResult<()> {
    let (base, wiki_root, wiki_id, repos) = {
        let guard = state.lock().unwrap();
        let members = wiki_team_members(&guard)?;
        let (root, id) = wiki_target(&guard, &wiki)?;
        (guard.base_dir.clone(), root, id, ken_core::wikidraft::team_repos(&wiki, &members))
    };
    let Some(binary) = ken_core::runner::discover_claude() else {
        return Err(ken_core::runner::MISSING_CLAUDE_HELP.into());
    };
    let wiki_name = ken_core::workspace::member_leaf(&wiki).to_string();
    if !lock_tolerant(drafting()).insert(wiki.clone()) {
        return Err("Claude is already writing this wiki's pages.".into());
    }
    std::thread::spawn(move || {
        // Cleared however the draft ends, and the screens told.
        struct Done(AppHandle, String);
        impl Drop for Done {
            fn drop(&mut self) {
                lock_tolerant(drafting()).remove(&self.1);
                let _ = self.0.emit("review-changed", ());
                let _ = self.0.emit("wiki-drafted", self.1.clone());
            }
        }
        let _done = Done(app, wiki.clone());
        let Ok(mut db) = Db::open(&base, wiki_id) else { return };
        let dirs = repos.iter().map(|(_, root)| root.clone()).collect();
        let generate = claude_generate(binary, wiki_root.clone(), dirs);
        let extra = extra.as_deref().map(Path::new);
        if let Err(e) = ken_core::wikidraft::draft_team(
            &wiki_root,
            &wiki_name,
            &mut db,
            &repos,
            extra,
            &local_date_today(),
            engine::now_epoch(),
            generate,
        ) {
            eprintln!("warning: first-wiki draft failed: {e}");
        }
    });
    Ok(())
}

/// Repos joined the workspace: for each team wiki that covers any of them,
/// draft their Repo Map pages and propose changes to the pages its people
/// keep, in the background. Returns the wikis being updated.
#[tauri::command(async)]
fn wiki_add_repos(state: State<SharedState>, members: Vec<String>) -> CmdResult<Vec<String>> {
    let (base, all) = {
        let guard = state.lock().unwrap();
        (guard.base_dir.clone(), wiki_team_members(&guard)?)
    };
    let mut by_wiki: std::collections::BTreeMap<String, Vec<(String, std::path::PathBuf)>> = Default::default();
    for m in &members {
        let Some(w) = ken_core::wikidraft::wiki_for(m, &all) else { continue };
        if let Some(added) = all.iter().find(|x| &x.name == m) {
            by_wiki.entry(w.name.clone()).or_default().push((added.name.clone(), added.root.clone()));
        }
    }
    if by_wiki.is_empty() {
        return Ok(Vec::new());
    }
    let Some(binary) = ken_core::runner::discover_claude() else {
        return Err(ken_core::runner::MISSING_CLAUDE_HELP.into());
    };
    let wikis: Vec<String> = by_wiki.keys().cloned().collect();
    for (wiki, added) in by_wiki {
        let (root, id) = {
            let guard = state.lock().unwrap();
            wiki_target(&guard, &wiki)?
        };
        let team = ken_core::wikidraft::team_repos(&wiki, &all);
        let (base, binary) = (base.clone(), binary.clone());
        let wiki_name = ken_core::workspace::member_leaf(&wiki).to_string();
        std::thread::spawn(move || {
            let Ok(mut db) = Db::open(&base, id) else { return };
            let dirs = team.iter().map(|(_, root)| root.clone()).chain(added.iter().map(|(_, root)| root.clone())).collect();
            let generate = claude_generate(binary, root.clone(), dirs);
            if let Err(e) = ken_core::wikidraft::draft_added(
                &root,
                &wiki_name,
                &mut db,
                &added,
                &team,
                &local_date_today(),
                engine::now_epoch(),
                generate,
            ) {
                eprintln!("warning: wiki update for added repos failed: {e}");
            }
        });
    }
    Ok(wikis)
}

/// Create a team's wiki from the bundled Ways-of-Working template at `dir`
/// (a new or empty folder) and return its set-up row. Local only: a remote
/// and a push are the person's step.
#[tauri::command]
async fn setup_create_wiki(
    dir: String,
    team: String,
    repos: Vec<ken_core::wikinew::Covered>,
    taken: Vec<String>,
    holds_team: Option<bool>,
) -> CmdResult<ken_core::setup::RepoRow> {
    tauri::async_runtime::spawn_blocking(move || {
        ken_core::setup::create_wiki_with(Path::new(&dir), &team, &repos, &taken, &local_date_today(), holds_team.unwrap_or(false))
            .map_err(err)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Set-up's "Create a team repo": the method's team template at `dir`, its
/// manifest naming the team's `repos` with their kinds.
#[tauri::command]
async fn setup_create_team_repo(
    dir: String,
    team: String,
    wiki: Option<String>,
    taken: Vec<String>,
    repos: Option<Vec<ken_core::wikinew::Covered>>,
) -> CmdResult<ken_core::setup::RepoRow> {
    tauri::async_runtime::spawn_blocking(move || {
        let repos = repos.unwrap_or_default();
        ken_core::setup::create_team_repo(Path::new(&dir), &team, wiki.as_deref(), &repos, &taken).map_err(err)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Whether I may accept a ruling `decider` made: I am the decider, or this
/// machine does not say who I am.
fn ruling_is_mine(decider: &str) -> bool {
    let me = cached_git_me();
    !me.is_known() || ken_core::ingest::is_me(&me, decider)
}

/// Apply a proposed change to a page a person keeps, only while the page
/// still reads as it did when Ken proposed it.
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
        .find(|it| it.id == item_id && it.kind == ken_core::wikidraft::PROPOSAL_KIND)
        .ok_or("no open proposal with that id")?;
    let proposal: ken_core::wikidraft::Proposal =
        serde_json::from_str(item.payload.as_deref().unwrap_or("")).map_err(|_| "the card has no proposed change")?;
    // A ruling is written by its decider.
    if let Some(decider) = proposal.append.as_ref().and_then(|r| r.decider.as_deref()) {
        if !ruling_is_mine(decider) {
            return Err(format!("A ruling is written by its decider. Only {decider} can accept this one."));
        }
    }
    match ken_core::wikidraft::apply(&active.project.root, &proposal) {
        Ok(()) => {}
        Err(ken_core::wikidraft::ApplyError::Changed) => {
            return Err(format!(
                "{} changed after Ken proposed this, so applying it would undo that edit. Discard it; the next repo added proposes again.",
                proposal.page
            ))
        }
        Err(ken_core::wikidraft::ApplyError::Io(e)) => return Err(e),
    }
    active.db.resolve_review_item(item_id, engine::now_epoch()).map_err(err)?;
    Ok(proposal.page)
}

/// The library the Ingest screen works on: a repo of the workspace with an
/// inbox (`Research/Ingestion/Raw/`), the chosen team's first and its wiki
/// before its team repo, whichever repo is focused. With no workspace, the
/// focused repo.
fn inbox_project(guard: &AppState, team: Option<&str>) -> Option<Project> {
    let reg = Registry::load(&guard.base_dir).ok();
    let mut best: Option<(i32, Project)> = None;
    let mut consider = |p: &Project| {
        if !ken_core::ingest::has_inbox(&p.root) {
            return;
        }
        let entry = reg.as_ref().and_then(|r| r.entry_at(&p.root));
        let mut score = 0;
        if team.is_some() && entry.and_then(|e| e.team.as_deref()) == team {
            score += 4;
        }
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

/// The team's wiki: the repo with the inbox, the one Files opens on. None
/// when no repo of the team has one.
#[tauri::command(async)]
fn team_wiki(state: State<SharedState>, team: Option<String>) -> CmdResult<Option<String>> {
    let guard = state.lock().unwrap();
    Ok(inbox_project(&guard, team.as_deref()).map(|p| p.config.id.to_string()))
}

/// [`inbox_project`], opened if it was dormant, focus left where it was.
fn inbox_member(app: &AppHandle, state: &SharedState, team: Option<&str>) -> CmdResult<uuid::Uuid> {
    let (project, open, focused) = {
        let guard = state.lock().unwrap();
        let project = inbox_project(&guard, team).ok_or("No repo of this team has an inbox (Research/Ingestion/Raw/). A wiki made from the Ways of Working template has one.")?;
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
    /// Its card, once read.
    card_id: Option<i64>,
    /// What was written from it, one kind per write (undone ones left out).
    written: Vec<ken_core::ingest::WriteKind>,
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
    /// Not yet seen, or seen and filed.
    open: bool,
    /// What was written from it, one kind per write (undone ones left out).
    written: Vec<ken_core::ingest::WriteKind>,
    /// What waits from it: rulings, tickets, held edits.
    waiting: usize,
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

/// The kinds of a card's writes still in place.
fn live_writes(card: &ken_core::ingest::Card) -> Vec<ken_core::ingest::WriteKind> {
    card.writes.iter().filter(|w| !w.undone).map(|w| w.kind).collect()
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

/// The Ingest screen: the library's inbox for the chosen team, every source
/// in Raw with where it is in the read, and what has been ingested, newest
/// first.
#[tauri::command(async)]
fn ingest_overview(app: AppHandle, state: State<SharedState>, team: Option<String>) -> CmdResult<IngestOverviewDto> {
    let id = inbox_member(&app, &state, team.as_deref())?;
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
    // The open cards of sources still in Raw: read, not yet seen.
    let cards: Vec<(i64, ken_core::ingest::Card)> = open
        .iter()
        .filter(|it| it.kind == ken_core::ingest::REVIEW_KIND)
        .filter_map(|it| ken_core::ingest::card_of(it.payload.as_deref()).map(|c| (it.id, c)))
        .filter(|(_, c)| !c.filed)
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
                (Some(ken_core::ingest::PassPhase::Reading), _, _) => ("reading", None),
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
                    let t = ken_core::ingest::takeaways(&std::fs::read_to_string(root.join(&c.placement.note)).unwrap_or_default());
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
                written: card.map(|(_, c)| live_writes(c)).unwrap_or_default(),
                waiting: card.map(|(_, c)| waits(&c.placement.note)).unwrap_or(0),
                name,
                path,
            }
        })
        .collect();
    let mut filed_waiting = 0;
    let ingested = db
        .review_items_of_kind(ken_core::ingest::REVIEW_KIND, 200)
        .map_err(err)?
        .into_iter()
        .map(|it| {
            let note = it.source_ref.clone();
            let text = std::fs::read_to_string(root.join(&note)).unwrap_or_default();
            let t = ken_core::ingest::takeaways(&text);
            let card = ken_core::ingest::card_of(it.payload.as_deref());
            let waiting = waits(&note);
            if waiting > 0 && card.as_ref().is_some_and(|c| c.filed) {
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
                written: card.as_ref().map(live_writes).unwrap_or_default(),
                waiting,
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
    /// What waits: `ruling`, `ticket`, or an edit staging held, `page` (a
    /// change to one) or `new page`.
    kind: String,
    title: String,
    /// The page (or ticket file) it writes.
    page: String,
    body: String,
    /// The proposal itself (the page as it is and with the change), for the diff.
    payload: Option<String>,
    /// A ruling's decider, when the note names one.
    decider: Option<String>,
    /// Whether I may accept it: a ruling only its decider, when known.
    can_accept: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IngestWriteDto {
    /// Its place on the card, for Undo.
    index: usize,
    kind: ken_core::ingest::WriteKind,
    /// The file, inside the repo `project_id` names.
    path: String,
    label: String,
    to: Option<String>,
    undone: bool,
    /// The workspace member the file is in; none for a task on Your day,
    /// or a repo no longer open.
    project_id: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IngestCardDto {
    id: i64,
    open: bool,
    /// Undo all ran; the source waits in Raw/ until read again.
    undone: bool,
    note: String,
    source: String,
    takeaways: ken_core::ingest::Takeaways,
    /// What was written from it, in order.
    writes: Vec<IngestWriteDto>,
    /// What waits from it.
    proposals: Vec<IngestProposalDto>,
    /// What it calls for that stays on the card only.
    listed: Vec<ken_core::ingest::Listed>,
    /// My name, for "Accept as …".
    me: Option<String>,
}

/// Two folders the same, whatever the case and the slashes.
fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().replace('\\', "/").trim_end_matches('/').to_lowercase();
    norm(a) == norm(b)
}

/// One ingested source for the Ingest screen: its key takeaways, what was
/// written from it (each with Undo), what waits (rulings, tickets, held
/// edits) and what stays on the card only.
#[tauri::command(async)]
fn ingest_card(app: AppHandle, state: State<SharedState>, team: Option<String>, item_id: i64) -> CmdResult<IngestCardDto> {
    let id = inbox_member(&app, &state, team.as_deref())?;
    let me = cached_git_me();
    // Cloned under the lock, read after it is released (the Inbox reads
    // every card that has something waiting).
    let (root, search_db, members) = {
        let guard = lock_tolerant(&state);
        let active = member(&guard, Some(id))?;
        let members: Vec<(PathBuf, String)> = guard
            .workspace
            .as_ref()
            .map(|w| {
                w.ws.members
                    .iter()
                    .filter_map(|m| match &m.status {
                        ken_core::workspace::MemberStatus::Ok(p) => Some((p.root.clone(), p.config.id.to_string())),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        (active.project.root.clone(), active.search_db.clone(), members)
    };
    let db = lock_tolerant(&search_db);
    let item = db.get_review_item(item_id).map_err(err)?.ok_or("no ingest card with that id")?;
    let card = ken_core::ingest::card_of(item.payload.as_deref()).ok_or("the card has no record of where things went")?;
    let text = std::fs::read_to_string(root.join(&card.placement.note)).unwrap_or_default();
    let proposals = ken_core::ingest::proposals_from(&db, &card.placement.note)
        .map_err(err)?
        .into_iter()
        .map(|it| {
            let p: Option<ken_core::wikidraft::Proposal> = it.payload.as_deref().and_then(|p| serde_json::from_str(p).ok());
            let (kind, page) = match &p {
                Some(p) if p.append.is_some() => ("ruling", p.page.clone()),
                Some(p) if p.root.is_some() => ("ticket", p.page.clone()),
                Some(p) if p.base.is_empty() => ("new page", p.page.clone()),
                Some(p) => ("page", p.page.clone()),
                None => ("page", String::new()),
            };
            let decider = p.as_ref().and_then(|p| p.append.as_ref()).and_then(|r| r.decider.clone());
            let can_accept = decider.as_deref().is_none_or(|d| !me.is_known() || ken_core::ingest::is_me(&me, d));
            IngestProposalDto {
                id: it.id,
                kind: kind.into(),
                title: it.title,
                page,
                body: it.body,
                payload: it.payload,
                decider,
                can_accept,
            }
        })
        .collect();
    drop(db);
    let member_at = |root: &Path| members.iter().find(|(r, _)| same_dir(r, root)).map(|(_, id)| id.clone());
    let library = id.to_string();
    let writes = card
        .writes
        .iter()
        .enumerate()
        .map(|(index, w)| IngestWriteDto {
            index,
            kind: w.kind,
            path: w.path.clone(),
            label: w.label.clone(),
            to: w.to.clone(),
            undone: w.undone,
            project_id: match (&w.kind, &w.root) {
                (ken_core::ingest::WriteKind::Task, _) => None,
                (_, None) => Some(library.clone()),
                (_, Some(root)) => member_at(Path::new(root)),
            },
        })
        .collect();
    Ok(IngestCardDto {
        id: item.id,
        open: item.status == "open",
        undone: card.undone,
        source: if card.filed { card.placement.source.clone() } else { card.placement.raw.clone() },
        note: card.placement.note.clone(),
        takeaways: ken_core::ingest::takeaways(&text),
        writes,
        proposals,
        listed: card.listed.clone(),
        me: me.name.clone(),
    })
}

/// Files dropped on the Ingest screen: each is copied into the library's
/// `Research/Ingestion/Raw/` (a name already there gets a number) and the
/// read starts. Returns the paths in Raw.
#[tauri::command(async)]
fn ingest_add(app: AppHandle, state: State<SharedState>, team: Option<String>, paths: Vec<String>) -> CmdResult<Vec<String>> {
    let id = inbox_member(&app, &state, team.as_deref())?;
    let (project, base) = {
        let guard = state.lock().unwrap();
        (member(&guard, Some(id))?.project.clone(), guard.base_dir.clone())
    };
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
    start_ingest_pass(&project, &base, workspace_root_of(&state), true);
    Ok(added)
}

/// Where a file named `name` goes in a library's Raw/: its own name, or with
/// a number when that is taken. The folder is made if missing.
fn raw_target(root: &std::path::Path, name: &str) -> CmdResult<std::path::PathBuf> {
    let raw_dir = root.join(ken_core::ingest::RAW);
    std::fs::create_dir_all(&raw_dir).map_err(err)?;
    // A dropped name is only a name: no folders, nothing above Raw.
    let name = name.rsplit(['/', '\\']).next().unwrap_or(name).trim();
    if name.is_empty() || name == "." || name == ".." {
        return Err("a file with no name".into());
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    // Raw's own README and Index are not sources, so a source by that name
    // gets another.
    let (name, stem) = if name.eq_ignore_ascii_case("readme.md") || name.eq_ignore_ascii_case("index.md") {
        let s = format!("{stem} (source)");
        (format!("{s}{ext}"), s)
    } else {
        (name.to_string(), stem)
    };
    let mut target = raw_dir.join(&name);
    let mut n = 2;
    while target.exists() {
        target = raw_dir.join(format!("{stem} ({n}){ext}"));
        n += 1;
    }
    Ok(target)
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
/// name and `x-team` the team, both URI-encoded. Written into the library's
/// Raw/ and the read starts. Returns its path in Raw.
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
    let team = header("x-team");
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err("the dropped file came without its bytes".into());
    };
    let id = inbox_member(&app, &state, team.as_deref())?;
    let (project, base) = {
        let guard = state.lock().unwrap();
        (member(&guard, Some(id))?.project.clone(), guard.base_dir.clone())
    };
    let target = raw_target(&project.root, &name)?;
    std::fs::write(&target, bytes).map_err(|e| format!("could not write {name}: {e}"))?;
    if ken_core::runner::discover_claude().is_none() {
        return Err(ken_core::runner::MISSING_CLAUDE_HELP.into());
    }
    start_ingest_pass(&project, &base, workspace_root_of(&state), true);
    Ok(format!("{}/{}", ken_core::ingest::RAW, target.file_name().unwrap().to_string_lossy()))
}

/// `YYYY-MM-DD HH.MM`, local: how a source Ken writes into an inbox is named.
fn local_source_stamp() -> String {
    chrono::Local::now().format("%Y-%m-%d %H.%M").to_string()
}

/// Write a source Ken made (a note, a chat) into the library's Raw/ and
/// start the read. Returns its path in Raw.
fn add_source(app: &AppHandle, state: &SharedState, team: Option<&str>, name: &str, text: &str) -> CmdResult<String> {
    let id = inbox_member(app, state, team)?;
    let (project, base) = {
        let guard = state.lock().unwrap();
        (member(&guard, Some(id))?.project.clone(), guard.base_dir.clone())
    };
    let target = raw_target(&project.root, name)?;
    std::fs::write(&target, text).map_err(|e| format!("could not write {name}: {e}"))?;
    if ken_core::runner::discover_claude().is_none() {
        return Err(ken_core::runner::MISSING_CLAUDE_HELP.into());
    }
    start_ingest_pass(&project, &base, workspace_root_of(state), true);
    Ok(format!("{}/{}", ken_core::ingest::RAW, target.file_name().unwrap().to_string_lossy()))
}

/// Write a note on the Ingest screen: it lands in the library's Raw/ as
/// `YYYY-MM-DD HH.MM Note - <title>.md` (`kind: note`, by me) and is read
/// at once. Returns its path in Raw.
#[tauri::command(async)]
fn ingest_add_text(app: AppHandle, state: State<SharedState>, team: Option<String>, title: Option<String>, text: String) -> CmdResult<String> {
    if text.trim().is_empty() {
        return Err("Write the note first.".into());
    }
    let me = cached_git_me();
    let (name, doc) = ken_core::ingest::note_source(&local_source_stamp(), title.as_deref(), &text, me.name.as_deref().or(me.email.as_deref()));
    add_source(&app, state.inner(), team.as_deref(), &name, &doc)
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
    team: Option<String>,
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
    add_source(&app, state.inner(), team.as_deref(), &name, &doc)
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
fn ingest_retry(app: AppHandle, state: State<SharedState>, team: Option<String>, path: String) -> CmdResult<bool> {
    let id = inbox_member(&app, &state, team.as_deref())?;
    let (project, base, ws_root) = {
        let mut guard = state.lock().unwrap();
        let base = guard.base_dir.clone();
        let ws_root = guard.workspace.as_ref().map(|w| w.ws.root.clone());
        let active = member_mut(&mut guard, Some(id))?;
        let rel = raw_source(&active.project.root, &path)?;
        ken_core::ingest::resolve_failure(&mut active.db, &rel, engine::now_epoch()).map_err(err)?;
        (active.project.clone(), base, ws_root)
    };
    if ken_core::runner::discover_claude().is_none() {
        return Err(ken_core::runner::MISSING_CLAUDE_HELP.into());
    }
    Ok(start_ingest_pass(&project, &base, ws_root, true))
}

/// Take a source out of Raw/: the file goes to the trash (never a hard
/// delete) and its failure is cleared. A source already read is filed with
/// Seen, or its writes undone first.
#[tauri::command]
async fn ingest_remove(app: AppHandle, state: State<'_, SharedState>, team: Option<String>, path: String) -> CmdResult<()> {
    let id = inbox_member(&app, &state, team.as_deref())?;
    let (abs, rel, undone_card) = {
        let guard = state.lock().unwrap();
        let active = member(&guard, Some(id))?;
        let rel = raw_source(&active.project.root, &path)?;
        let card = active
            .db
            .list_open_review_items()
            .map_err(err)?
            .into_iter()
            .filter(|it| it.kind == ken_core::ingest::REVIEW_KIND)
            .find_map(|it| ken_core::ingest::card_of(it.payload.as_deref()).filter(|c| !c.filed && c.placement.raw == rel).map(|c| (it.id, c)));
        let undone_card = match card {
            Some((_, c)) if !c.undone => {
                return Err("This source has been read. Mark it Seen to file it, or Undo all to take back what it wrote, then remove it.".into())
            }
            Some((item, _)) => Some(item),
            None => None,
        };
        (active.project.resolve(&rel).map_err(err)?, rel, undone_card)
    };
    let to_trash = abs.clone();
    tauri::async_runtime::spawn_blocking(move || trash_path(&to_trash)).await.map_err(|e| e.to_string())??;
    let mut guard = state.lock().unwrap();
    let active = member_mut(&mut guard, Some(id))?;
    let now = engine::now_epoch();
    ken_core::ingest::resolve_failure(&mut active.db, &rel, now).map_err(err)?;
    if let Some(item) = undone_card {
        active.db.resolve_review_item(item, now).map_err(err)?;
    }
    let _ = deindex_removed(&active.project, &mut active.db, &rel, false);
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IngestStatusDto {
    has_inbox: bool,
    /// New sources in Raw, not yet processed.
    waiting: Vec<String>,
    /// Sources in Raw whose note is written and waiting to be filed.
    in_review: Vec<String>,
    running: bool,
    claude_found: bool,
}

/// What waits in the focused project's library inbox.
#[tauri::command(async)]
fn ingest_status(app: AppHandle, state: State<SharedState>, team: Option<String>) -> CmdResult<IngestStatusDto> {
    let id = inbox_member(&app, &state, team.as_deref())?;
    let guard = state.lock().unwrap();
    let active = member(&guard, Some(id))?;
    let root = &active.project.root;
    Ok(IngestStatusDto {
        has_inbox: ken_core::ingest::has_inbox(root),
        waiting: ken_core::ingest::waiting_new(root, &active.db).map_err(err)?,
        in_review: ken_core::ingest::in_review(&active.db).map_err(err)?,
        running: INGEST_RUNNING.lock().unwrap().contains(&active.project.config.id),
        claude_found: ken_core::runner::discover_claude().is_some(),
    })
}

/// Read what waits in Raw/ now, whatever the repo's kind.
#[tauri::command(async)]
fn ingest_now(app: AppHandle, state: State<SharedState>, team: Option<String>) -> CmdResult<bool> {
    let id = inbox_member(&app, &state, team.as_deref())?;
    let guard = state.lock().unwrap();
    let active = member(&guard, Some(id))?;
    if ken_core::runner::discover_claude().is_none() {
        return Err(ken_core::runner::MISSING_CLAUDE_HELP.into());
    }
    let ws_root = guard.workspace.as_ref().map(|w| w.ws.root.clone());
    Ok(start_ingest_pass(&active.project, &guard.base_dir, ws_root, true))
}

/// Undo all on an ingest card, open or seen: every write from the source,
/// last first (one whose file changed since is kept and named), then the
/// note unless a person edited it, and the source back to Raw/. What waits
/// is closed, and so is the card.
#[tauri::command(async)]
fn ingest_undo(app: AppHandle, state: State<SharedState>, team: Option<String>, item_id: i64) -> CmdResult<ken_core::ingest::UndoReport> {
    let id = inbox_member(&app, &state, team.as_deref())?;
    let report = {
        let mut guard = state.lock().unwrap();
        let active = member_mut(&mut guard, Some(id))?;
        let item = active
            .db
            .get_review_item(item_id)
            .map_err(err)?
            .filter(|it| it.kind == ken_core::ingest::REVIEW_KIND)
            .ok_or("no ingest card with that id")?;
        let mut card = ken_core::ingest::card_of(item.payload.as_deref()).ok_or("the card has no record of where things went")?;
        let report = ken_core::ingest::undo_all(&active.project.root, &mut card, &local_date_today()).map_err(err)?;
        let payload = serde_json::to_string(&card).map_err(err)?;
        active.db.set_review_item_payload(item_id, &payload).map_err(err)?;
        ken_core::ingest::withdraw(&mut active.db, &card.placement.note, engine::now_epoch()).map_err(err)?;
        // The card stays open and undone, so the next pass leaves the source
        // alone instead of writing it all again.
        active.db.reopen_review_item(item_id).map_err(err)?;
        report
    };
    emit_day_changed(&app, &state);
    Ok(report)
}

/// Read an undone source again: its card closes and the next pass, started
/// now, reads it from Raw/ as if it had just been dropped in.
#[tauri::command(async)]
fn ingest_read_again(app: AppHandle, state: State<SharedState>, team: Option<String>, item_id: i64) -> CmdResult<bool> {
    let id = inbox_member(&app, &state, team.as_deref())?;
    let mut guard = state.lock().unwrap();
    let base_dir = guard.base_dir.clone();
    let ws_root = guard.workspace.as_ref().map(|w| w.ws.root.clone());
    let active = member_mut(&mut guard, Some(id))?;
    let item = active
        .db
        .get_review_item(item_id)
        .map_err(err)?
        .filter(|it| it.kind == ken_core::ingest::REVIEW_KIND)
        .ok_or("no ingest card with that id")?;
    let card = ken_core::ingest::card_of(item.payload.as_deref()).ok_or("the card has no record of where things went")?;
    if !card.undone {
        return Err("only an undone source can be read again".into());
    }
    active.db.resolve_review_item(item_id, engine::now_epoch()).map_err(err)?;
    if ken_core::runner::discover_claude().is_none() {
        return Ok(false);
    }
    Ok(start_ingest_pass(&active.project, &base_dir, ws_root, true))
}

/// Undo one write on an ingest card (by its place on the card): an edit's
/// page back as it was, a created page, idea or escalation removed, a task
/// archived. Refused when the file changed since it was written.
#[tauri::command(async)]
fn ingest_undo_write(app: AppHandle, state: State<SharedState>, team: Option<String>, item_id: i64, index: usize) -> CmdResult<()> {
    let id = inbox_member(&app, &state, team.as_deref())?;
    let task = {
        let mut guard = state.lock().unwrap();
        let active = member_mut(&mut guard, Some(id))?;
        let item = active
            .db
            .get_review_item(item_id)
            .map_err(err)?
            .filter(|it| it.kind == ken_core::ingest::REVIEW_KIND)
            .ok_or("no ingest card with that id")?;
        let mut card = ken_core::ingest::card_of(item.payload.as_deref()).ok_or("the card has no record of where things went")?;
        ken_core::ingest::undo_write_at(&active.project.root, &mut card, index, &local_date_today()).map_err(err)?;
        let payload = serde_json::to_string(&card).map_err(err)?;
        active.db.set_review_item_payload(item_id, &payload).map_err(err)?;
        card.writes[index].kind == ken_core::ingest::WriteKind::Task
    };
    if task {
        emit_day_changed(&app, &state);
    }
    Ok(())
}

/// Done with an ingest: its source moves from Raw/ beside its note, in its
/// kind's folder and month, and the card is resolved.
#[tauri::command(async)]
fn ingest_file(app: AppHandle, state: State<SharedState>, team: Option<String>, item_id: i64) -> CmdResult<String> {
    let id = inbox_member(&app, &state, team.as_deref())?;
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
    let filed = ken_core::ingest::file(&active.project.root, &card).map_err(err)?;
    // The card records where the source went, so Open source and Undo all
    // find it there.
    let payload = serde_json::to_string(&filed).map_err(err)?;
    active.db.set_review_item_payload(item_id, &payload).map_err(err)?;
    active.db.resolve_review_item(item_id, engine::now_epoch()).map_err(err)?;
    Ok(filed.placement.source)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TeamRepoDto {
    id: String,
    name: String,
    path: String,
    kind: Vec<ken_core::registry::RepoKind>,
    /// Set only when a person chose other than the kind implies.
    index: Option<ken_core::registry::IndexState>,
    /// What Ken reads it as now: the choice, else the kind's default.
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
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TeamGapDto {
    /// The repo it is about, when it is about one.
    repo: Option<String>,
    text: String,
    /// A path to open to see or fix it.
    open: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TeamPageDto {
    path: String,
    title: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TeamOverviewDto {
    team: Option<String>,
    workspace: String,
    /// The folder the workspace lives in (what Scan again reads).
    root: String,
    repos: Vec<TeamRepoDto>,
    /// The team's wiki, when it has one.
    wiki: Option<TeamRepoDto>,
    gaps: Vec<TeamGapDto>,
    /// The workspace's own ignore lines (`.kenignore` beside the repos).
    ignores: Vec<String>,
    /// The wiki's last drift sweep.
    sweep: Option<ken_core::drift::DriftRun>,
    /// What waits on a person in the wiki: the last sweep's findings, the
    /// link report, and what the wiki's drafts left (pages to read, pages
    /// not drafted, proposals, repos that left the team).
    findings: Vec<TeamFindingDto>,
    rules: Vec<TeamPageDto>,
    templates: Vec<TeamPageDto>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TeamFindingDto {
    /// From the sweep: `void` · `gone` · `changed` · `aged`. From the link
    /// report: `missing-page` · `broken-link` · `ambiguous-name`. From the
    /// drafts: `draft` · `draft-failed` · `proposal` · `repo-removed`.
    kind: String,
    title: String,
    detail: String,
    /// The page to open, in the repo `project_id` names; empty when the
    /// finding is about the sweep itself.
    path: String,
    project_id: String,
    /// A proposal's id, for `apply_page_proposal` and `resolve_review_item`.
    item_id: Option<i64>,
}

/// The findings for the Team screen, from the wiki's index and its last
/// sweep, at most 20 of each link-report kind.
fn team_findings(wiki_id: &str, root: &Path, db: &Db, sweep: Option<&ken_core::drift::DriftRun>) -> Vec<TeamFindingDto> {
    let mut out: Vec<TeamFindingDto> = Vec::new();
    let mut push = |kind: String, title: String, detail: String, path: String, item_id: Option<i64>| {
        out.push(TeamFindingDto { kind, title, detail, path, project_id: wiki_id.to_string(), item_id });
    };
    if let Some(run) = sweep {
        for f in ken_core::drift::findings(run) {
            push(f.kind, f.title, f.detail, f.path, None);
        }
    }
    if let Ok(report) = ken_core::links::report(db) {
        for f in ken_core::links::findings(&report, 20) {
            push(f.kind, f.title, f.detail, f.path, None);
        }
    }
    match ken_core::wikidraft::findings(root, db) {
        Ok(found) => {
            for f in found {
                push(f.kind, f.title, f.detail, f.path, f.item_id);
            }
        }
        Err(e) => eprintln!("warning: could not read the wiki's draft findings: {e}"),
    }
    out
}

fn git_line(root: &std::path::Path, args: &[&str]) -> Option<String> {
    let mut cmd = std::process::Command::new("git");
    let out = ken_core::proc::quiet(&mut cmd).args(args).current_dir(root).env("GIT_TERMINAL_PROMPT", "0").output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|s| !s.is_empty())
}

/// A wiki page's title: its frontmatter `title`, else its first heading,
/// else its file name.
fn page_title(root: &std::path::Path, rel: &str) -> String {
    let text = std::fs::read_to_string(root.join(rel)).unwrap_or_default();
    text.lines()
        .find_map(|l| l.trim_start().strip_prefix("title:").map(|t| t.trim().trim_matches('"').to_string()))
        .filter(|t| !t.contains("{{"))
        .or_else(|| text.lines().find_map(|l| l.strip_prefix("# ").map(|t| t.trim().to_string())))
        .unwrap_or_else(|| rel.rsplit('/').next().unwrap_or(rel).trim_end_matches(".md").to_string())
}

/// The `.md` pages directly in `dir` of `root`, titled, the copy template a
/// wiki laid down before the 2026-10 template kept among its rules
/// (`RULE.md`) left out.
fn pages_in(root: &std::path::Path, dir: &str) -> Vec<TeamPageDto> {
    let mut out: Vec<TeamPageDto> = std::fs::read_dir(root.join(dir))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            (name.ends_with(".md") && name != "RULE.md" && name != "Index.md").then(|| format!("{dir}/{name}"))
        })
        .map(|path| TeamPageDto { title: page_title(root, &path), path })
        .collect();
    out.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
    out
}

/// The Team screen (the Wright white box, frame 9): the chosen team's repos
/// as set-up wrote them (kind, index, branch, commit, how far behind its
/// upstream), what each lacks for its kind, the workspace's ignore lines,
/// the wiki's last drift sweep, and its rules and templates.
#[tauri::command(async)]
fn team_overview(state: State<SharedState>, team: Option<String>) -> CmdResult<TeamOverviewDto> {
    // Cloned under the lock; the registry, the file counts, the wiki's
    // findings and git are read after it is released. Home, Inbox and the
    // sidebar ask for this while indexing runs.
    let (ws, base_dir, dbs) = {
        let guard = lock_tolerant(&state);
        let ws = guard.workspace.as_ref().ok_or("no workspace open")?.ws.clone();
        let dbs: std::collections::HashMap<uuid::Uuid, Arc<Mutex<Db>>> =
            guard.members.iter().map(|(id, m)| (*id, m.search_db.clone())).collect();
        (ws, guard.base_dir.clone(), dbs)
    };
    let registry = Registry::load(&base_dir).map_err(err)?;
    let in_team: Option<Vec<String>> = team.as_deref().map(|t| ws.config.effective_group_members(t));
    let mut repos: Vec<TeamRepoDto> = Vec::new();
    for m in &ws.members {
        if in_team.as_ref().is_some_and(|names| !names.contains(&m.name)) {
            continue;
        }
        let ken_core::workspace::MemberStatus::Ok(p) = &m.status else { continue };
        let entry = registry.entry_at(&p.root);
        let kind = entry.map(|e| e.kind.clone()).unwrap_or_default();
        let index = entry.and_then(|e| e.index);
        let git = p.root.join(".git").exists();
        let files = dbs.get(&p.config.id).and_then(|d| lock_tolerant(d).file_count().ok()).unwrap_or(-1);
        repos.push(TeamRepoDto {
            id: p.config.id.to_string(),
            name: m.name.clone(),
            path: p.root.display().to_string(),
            effective_index: index.unwrap_or_else(|| ken_core::registry::IndexState::for_kind(&kind)),
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
        });
    }
    let is_wiki = |r: &TeamRepoDto| r.kind.contains(&ken_core::registry::RepoKind::Wiki);
    let wiki_index = repos.iter().position(is_wiki);

    let mut gaps: Vec<TeamGapDto> = Vec::new();
    if wiki_index.is_none() {
        gaps.push(TeamGapDto { repo: None, text: "This team has no wiki. Set-up can make one from the Ways of Working template.".into(), open: None });
    }
    for r in &repos {
        let root = std::path::Path::new(&r.path);
        if !r.available {
            gaps.push(TeamGapDto { repo: Some(r.name.clone()), text: format!("{}'s folder is not there any more.", r.name), open: None });
            continue;
        }
        if r.kind.contains(&ken_core::registry::RepoKind::Code) && !["CLAUDE.md", "AGENTS.md"].iter().any(|f| root.join(f).exists()) {
            gaps.push(TeamGapDto {
                repo: Some(r.name.clone()),
                text: format!("{} has no CLAUDE.md or AGENTS.md, so an agent starts in it without its conventions.", r.name),
                open: None,
            });
        }
        if r.kind.is_empty() {
            gaps.push(TeamGapDto { repo: Some(r.name.clone()), text: format!("Nobody has said what {} is: pick its kind.", r.name), open: None });
        }
    }
    let wiki = wiki_index.map(|i| repos.remove(i));
    let mut sweep = None;
    let mut findings = Vec::new();
    let (mut rules, mut templates) = (Vec::new(), Vec::new());
    if let Some(w) = &wiki {
        let root = std::path::Path::new(&w.path);
        let left = ken_core::wikidraft::placeholders_left(root);
        if !left.is_empty() {
            gaps.push(TeamGapDto {
                repo: Some(w.name.clone()),
                text: format!("The wiki has {} placeholder{} still to fill, first {}.", left.len(), if left.len() == 1 { "" } else { "s" }, left[0]),
                open: left[0].split(':').next().map(str::to_string),
            });
        }
        for r in &repos {
            let page = ken_core::wikidraft::repo_page(&r.name);
            if !root.join(&page).exists() {
                gaps.push(TeamGapDto {
                    repo: Some(r.name.clone()),
                    text: format!("The wiki has no Repo Map page for {}.", r.name),
                    open: Some("Repo-Map/Index.md".into()),
                });
            }
        }
        if let Ok(id) = w.id.parse::<uuid::Uuid>() {
            if let Some(d) = dbs.get(&id) {
                let db = lock_tolerant(d);
                sweep = db.last_drift_run().ok().flatten();
                findings = team_findings(&w.id, root, &db, sweep.as_ref());
            }
        }
        rules = pages_in(root, "Ways-of-Working/Rules");
        templates = pages_in(root, "Templates");
    }
    let ignores = std::fs::read_to_string(ws.root.join(".kenignore"))
        .map(|t| t.lines().map(str::to_string).filter(|l| !l.trim().is_empty()).collect())
        .unwrap_or_default();
    let workspace = ws.config.name.clone();
    let ws_root = ws.root.display().to_string();

    // Each repo's branch, commit and how far behind its upstream, the repos
    // side by side.
    let mut repos = repos;
    let mut wiki = wiki;
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
            gaps.push(TeamGapDto {
                repo: Some(r.name.clone()),
                text: format!("{} is {n} commit{} behind its upstream: pull it so Ken reads what is merged.", r.name, if n == 1 { "" } else { "s" }),
                open: None,
            });
        }
    }
    Ok(TeamOverviewDto {
        team,
        workspace,
        root: ws_root,
        repos,
        wiki,
        gaps,
        ignores,
        sweep,
        findings,
        rules,
        templates,
    })
}

/// Write the workspace's ignore lines (`.kenignore` beside its repos). The
/// next scan of each repo reads them.
#[tauri::command(async)]
fn team_save_ignores(state: State<SharedState>, lines: Vec<String>) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let ws = guard.workspace.as_ref().ok_or("no workspace open")?;
    let text: String = lines.iter().map(|l| format!("{}\n", l.trim_end())).collect();
    std::fs::write(ws.ws.root.join(".kenignore"), text).map_err(err)
}

/// A new rule in the team wiki's Ways-of-Working/Rules/, from the wiki's own
/// rule template, named as the rule. Returns its path, to open and write.
#[tauri::command(async)]
fn team_add_rule(state: State<SharedState>, wiki_id: String, rule: String) -> CmdResult<String> {
    let guard = state.lock().unwrap();
    let id: uuid::Uuid = wiki_id.parse().map_err(err)?;
    let root = guard.members.get(&id).map(|m| m.project.root.clone()).ok_or("the wiki is not open")?;
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
    let rel = format!("Ways-of-Working/Rules/{slug}.md");
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

/// Run the drift sweep now, whatever the interval or the repo's kind.
#[tauri::command]
async fn run_drift_now(state: State<'_, SharedState>, project_id: Option<String>) -> CmdResult<Option<ken_core::drift::DriftRun>> {
    let id = project_id.as_deref().map(|p| p.parse::<uuid::Uuid>()).transpose().map_err(err)?;
    let (base, project) = {
        let guard = state.lock().unwrap();
        let active = member(&guard, id)?;
        (guard.base_dir.clone(), active.project.clone())
    };
    tauri::async_runtime::spawn_blocking(move || {
        let mut db = Db::open(&base, project.config.id).map_err(err)?;
        Ok(run_drift_if_due(&project, &mut db, true))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// A page's links both ways, resolved now: the pages it reaches and the
/// pages that reach it (the Map's page neighbours).
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

/// A code file's structure for the Files panel: its definitions in order
/// (with nesting depth) and the files it imports and is imported by.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CodeFileDto {
    outline: Vec<CodeOutlineItem>,
    related: ken_core::codemap::Related,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CodeOutlineItem {
    depth: usize,
    #[serde(flatten)]
    symbol: ken_core::codemap::Symbol,
}

#[tauri::command(async)]
fn code_file(state: State<SharedState>, path: String) -> CmdResult<CodeFileDto> {
    let guard = state.lock().unwrap();
    let active = member(&guard, None)?;
    let outline = ken_core::codemap::outline(&active.db, &path)
        .map_err(err)?
        .into_iter()
        .map(|(depth, symbol)| CodeOutlineItem { depth, symbol })
        .collect();
    let related = ken_core::codemap::related(&active.db, &path).map_err(err)?;
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
            for d in ken_core::codemap::definitions(&db, &name).map_err(err)? {
                out.definitions.push(dto(d.path, d.line, d.kind, None));
            }
            let calls = ken_core::codemap::usages(&db, &name, 200).map_err(err)?;
            let known: Vec<(String, i64)> = calls.iter().map(|u| (u.path.clone(), u.line)).collect();
            for u in calls {
                out.uses.push(dto(u.path, u.line, u.kind, u.within));
            }
            for m in ken_core::codemap::mentions(&db, &name, &known, 200).map_err(err)? {
                out.uses.push(dto(m.path, m.line, m.kind, m.within));
            }
        }
        Ok(out)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Rebuild the knowledge model now, by hand. Progress arrives as
/// `knowledge-model-state` events: building → ready | error {detail}.
/// Unlike the automatic build this ignores every threshold — "rebuild it
/// now" is exactly what it says.
#[tauri::command(async)]
fn refresh_knowledge_model(app: AppHandle, state: State<SharedState>) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let active = member(&guard, None)?;
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
        state: state.inner().clone(),
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
    /// federated-kg task 2.1: needed only to re-check `federatedKg`/
    /// `workspace` after this build finishes, to decide whether to schedule
    /// the debounced workspace-KG auto-trigger (checked fresh here rather
    /// than cached, since either flag could change while this build ran).
    state: SharedState,
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

    let project_id = job.project.config.id;
    emit_member(app, project_id, "knowledge-model-state", KnowledgeModelState {
        state: "building".into(),
        detail: None,
    });
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
        // federated-kg task 2.1 / spec "member knowledge-model completion in
        // workspace mode SHALL trigger a debounced (30s) build": snapshot
        // BEFORE `event` moves into `emit_member` below.
        let succeeded = event.state == "ready";
        emit_member(&thread_app, project_id, "knowledge-model-state", event);
        // Stamp the attempt BEFORE clearing the guard: a later manual rebuild
        // must never see "not running" together with a stale attempt clock, or
        // a failing build could restart immediately.
        job.tracker.build_finished(Instant::now());
        job.running.store(false, Ordering::SeqCst);
        if succeeded {
            let trigger = {
                let guard = job.state.lock().unwrap();
                federated_kg_enabled(&guard.app_settings)
            };
            if trigger {
                schedule_workspace_kg_debounce(&thread_app, &job.state);
            }
        }
    });
    true
}

// ===========================================================================
// federated-kg (openspec/changes/federated-kg) task 2.1-2.2: workspace-KG
// build orchestration and read commands. `federatedKg` gating (task 2.3)
// lives in `federated_kg_enabled` above; every entry point below checks it
// before touching `kg.sqlite`.
// ===========================================================================

/// Production `FederationLlm` (federated-kg task 2.1): wraps
/// `local_llm::generate_stream` at Background priority — the same call
/// `scan_and_profile`'s refinement pass and `extraction_worker`'s `generate`
/// closure make, just behind the trait `federation.rs` defines instead of a
/// closure (federation's merge/adjudication/linking passes need a `&self`
/// value they can hold across the whole build, not a one-shot closure). A
/// thin unit struct: `complete` carries no state of its own.
/// The workspace graph's model: summaries (one call per merged entity) on
/// the local model; the two batched judgements, which concepts are the same
/// and how they relate, on Claude when it is installed. The local 4B model
/// said "same" to most near-miss pairs on a real workspace ("DMA" and "DMA
/// Analysis", a tool and the agent that uses it).
struct AppFederationLlm {
    local: bool,
    claude: Option<PathBuf>,
    root: PathBuf,
}

impl ken_core::federation::FederationLlm for AppFederationLlm {
    fn complete(&self, prompt: &str) -> ken_core::Result<String> {
        if !self.local {
            // No local model: the build falls back to the longest summary.
            return Err(ken_core::Error::Other("no local model".into()));
        }
        let mut sink = |_: &str| true;
        ken_core::local_llm::generate_stream(prompt, ken_core::local_llm::Priority::Background, &mut sink)
    }

    fn judge(&self, prompt: &str) -> ken_core::Result<String> {
        let Some(binary) = &self.claude else {
            return self.complete(prompt);
        };
        match ken_core::assistant::oneshot(binary, &self.root, prompt, Duration::from_secs(300), &CancelToken::new())? {
            ken_core::assistant::OneshotOutcome::Completed(text) => Ok(text),
            other => Err(ken_core::Error::Other(format!("claude: {other:?}"))),
        }
    }
}

/// Payload for the `workspace-kg-state` event (federated-kg task 2.1),
/// mirroring `SemanticIndexStateEvent`'s tag shape:
/// `{"state":"building","done":0,"total":3}`, `{"state":"ready",...counts}`,
/// `{"state":"unavailable","reason":"..."}`. Emitted with plain `app.emit`
/// (not `emit_member`) since a workspace-KG build spans every open member
/// at once — it has no single owning `project_id` to scope the event to.
///
/// Deviation (see final report): `build_workspace_kg` (ken-core) runs its
/// per-member snapshot phase synchronously with no progress callback, so
/// this command layer can only observe "started" and "finished" — `Building`
/// is emitted once up front with `done: 0`, not ticked per member as the
/// build actually progresses. `total` is still real per-member information
/// (the member count), which is what task 2.1 asks this event to carry.
#[derive(Clone, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
enum WorkspaceKgStateEvent {
    Building { done: usize, total: usize },
    Ready {
        global_entities: usize,
        entity_links: usize,
        imported_edges: usize,
        cooccur_edges: usize,
        llm_edges: usize,
        llm_passes: bool,
    },
    Unavailable { reason: String },
}

/// How long a burst of member knowledge-model completions is allowed to
/// settle before the workspace-KG auto-trigger actually fires (spec:
/// "debounced (30 s)").
const WORKSPACE_KG_DEBOUNCE: Duration = Duration::from_secs(30);

/// Schedule the debounced workspace-KG auto-trigger (federated-kg task 2.1).
/// Bumps `AppState::workspace_kg_debounce_gen` and spawns a timer thread that
/// sleeps out the window before checking whether it's still the most recent
/// request — mirrors `AppState::qa_gen`'s "a newer request invalidates the
/// older one" pattern. A burst of member completions inside the window
/// therefore collapses into exactly one build, fired 30s after the LAST
/// completion (true debounce, not a fixed-interval throttle).
fn schedule_workspace_kg_debounce(app: &AppHandle, state: &SharedState) {
    let gen_counter = state.lock().unwrap().workspace_kg_debounce_gen.clone();
    let my_gen = gen_counter.fetch_add(1, Ordering::SeqCst) + 1;
    let bg_app = app.clone();
    let bg_state = state.clone();
    std::thread::spawn(move || {
        std::thread::sleep(WORKSPACE_KG_DEBOUNCE);
        if gen_counter.load(Ordering::SeqCst) != my_gen {
            return; // superseded by a later completion's own timer
        }
        start_workspace_kg_build(&bg_app, &bg_state);
    });
}

/// Start one workspace-KG build, or report that one is already in flight
/// (federated-kg task 2.1). Shared by `rebuild_workspace_kg` (manual) and
/// `schedule_workspace_kg_debounce`'s timer so the two entry points can
/// never stack two builds — mirrors `start_knowledge_build`'s single
/// `AtomicBool` guard, and `apply_semantic_index_flag`'s replace-and-cancel
/// `CancelToken` slot, at the workspace scope. Re-checks `federatedKg`
/// itself (not just at the call site) so a debounce timer that fires after
/// the flag was turned off mid-window is a no-op, per task 2.3's "no
/// auto-trigger" when off.
///
/// Deviation (see final report): no `workspace.rs`/manifest exists yet to
/// enumerate a workspace's members (proposal.md assumes one), so "members"
/// here is simply every project currently open in `AppState::members` — a
/// member that isn't open this session is invisible to the build until it
/// is. Each member is read through its own fresh `Db::open_read_only`
/// handle, the same way `ken-mcp` reads a project — never the live
/// `MemberRuntime::db`/`search_db` handles — so a slow build never contends
/// a member's own write connection or requires holding the global lock
/// across it.
/// Where the merged graph lives: beside the open workspace's manifest
/// (`<workspace>/.ken-workspace/kg.sqlite`, design D1), so each workspace has
/// its own graph instead of all of them rewriting one file; app data only
/// when no workspace is open.
fn workspace_kg_root(guard: &AppState) -> std::path::PathBuf {
    guard
        .workspace
        .as_ref()
        .map(|w| w.ws.root.clone())
        .unwrap_or_else(|| guard.base_dir.clone())
}

/// The project ids of a team (a manifest group, set at set-up or derived
/// from a group folder), for a team's view of the merged graph. None when
/// no team is asked for, or the name is not a group here.
fn team_project_ids(guard: &AppState, team: Option<&str>) -> Option<std::collections::HashSet<String>> {
    let team = team.filter(|t| !t.is_empty())?;
    let ws = guard.workspace.as_ref()?;
    let names = ws.ws.config.effective_group_members(team);
    Some(
        ws.ws
            .members
            .iter()
            .filter(|m| names.contains(&m.name))
            .filter_map(|m| match &m.status {
                ken_core::workspace::MemberStatus::Ok(p) => Some(p.config.id.to_string()),
                _ => None,
            })
            .collect(),
    )
}

/// The members the merged graph is built over: every member the workspace
/// manifest lists that has an index on disk and is read for entities (a
/// team or wiki repo, or one whose kind is not said yet), whether or not it
/// is open this session. With no workspace, the open members.
fn workspace_kg_members(guard: &AppState) -> Vec<uuid::Uuid> {
    let pseudo_id = memory_pseudo_member_id(guard);
    let listed: Vec<(uuid::Uuid, std::path::PathBuf)> = match guard.workspace.as_ref() {
        Some(w) => w
            .ws
            .members
            .iter()
            .filter_map(|m| match &m.status {
                ken_core::workspace::MemberStatus::Ok(p) => Some((p.config.id, p.root.clone())),
                _ => None,
            })
            .collect(),
        None => guard.members.values().map(|m| (m.project.config.id, m.project.root.clone())).collect(),
    };
    listed
        .into_iter()
        // ken-memory task 2.1 (D3): the pseudo-member's memories never join
        // the workspace-wide KG.
        .filter(|(id, _)| Some(*id) != pseudo_id)
        .filter(|(id, _)| db_path(&guard.base_dir, *id).exists())
        .filter(|(_, root)| {
            let (kind, index) = ken_core::registry::index_of(root);
            index.unwrap_or_else(|| ken_core::registry::IndexState::for_kind(&kind))
                == ken_core::registry::IndexState::Entities
        })
        .map(|(id, _)| id)
        .collect()
}

/// Every team's roster in the workspace: the `people/` files of each member
/// the manifest lists, or of the open members with no workspace. The graph
/// merges the names and addresses a roster gives one person into them.
fn workspace_roster(guard: &AppState) -> Vec<ken_core::people::Person> {
    let roots: Vec<std::path::PathBuf> = match guard.workspace.as_ref() {
        Some(w) => w
            .ws
            .members
            .iter()
            .filter_map(|m| match &m.status {
                ken_core::workspace::MemberStatus::Ok(p) => Some(p.root.clone()),
                _ => None,
            })
            .collect(),
        None => guard.members.values().map(|m| m.project.root.clone()).collect(),
    };
    ken_core::people::roster_of(roots.iter().map(|r| r.as_path()))
}

fn start_workspace_kg_build(app: &AppHandle, state: &SharedState) -> bool {
    let (base_dir, kg_root, running, cancel_slot, mut member_ids, enabled, roster) = {
        let guard = state.lock().unwrap();
        (
            guard.base_dir.clone(),
            workspace_kg_root(&guard),
            guard.workspace_kg_running.clone(),
            guard.workspace_kg_cancel.clone(),
            workspace_kg_members(&guard),
            federated_kg_enabled(&guard.app_settings),
            workspace_roster(&guard),
        )
    };
    if !enabled || running.swap(true, Ordering::SeqCst) {
        return false;
    }
    // Deterministic member order (design D3 / spec "delete and rebuild"
    // determinism) — `HashMap` iteration order isn't, so sort explicitly.
    member_ids.sort();

    let token = CancelToken::new();
    if let Some(old) = cancel_slot.lock().unwrap().replace(token.clone()) {
        old.cancel();
    }

    let _ = app.emit(
        "workspace-kg-state",
        WorkspaceKgStateEvent::Building { done: 0, total: member_ids.len() },
    );

    let thread_app = app.clone();
    std::thread::spawn(move || {
        let now = engine::now_epoch();
        let llm_ready = matches!(
            ken_core::local_llm::llm_status(),
            ken_core::local_llm::LlmStatus::Ready
        );
        let federation_llm = AppFederationLlm {
            local: LOCAL_GENERATION && llm_ready,
            claude: ken_core::runner::discover_claude(),
            root: kg_root.clone(),
        };
        let llm_ready = llm_ready || federation_llm.claude.is_some();
        let build = (|| -> ken_core::Result<ken_core::federation::BuildReport> {
            let mut kg = ken_core::workspace_kg_db::WorkspaceKgDb::open(&kg_root)?;
            let dbs = member_ids
                .iter()
                .map(|id| Db::open_read_only(&base_dir, *id).map(|db| (*id, db)))
                .collect::<ken_core::Result<Vec<(uuid::Uuid, Db)>>>()?;
            let members: Vec<ken_core::federation::Member> = dbs
                .iter()
                .map(|(id, db)| ken_core::federation::Member { project_id: *id, db })
                .collect();
            let llm: Option<&dyn ken_core::federation::FederationLlm> =
                if llm_ready { Some(&federation_llm) } else { None };
            ken_core::federation::build_workspace_kg(&mut kg, &members, &roster, llm, now, &token)
        })();

        let event = match build {
            Ok(report) if report.cancelled => WorkspaceKgStateEvent::Unavailable {
                reason: "workspace-KG build cancelled".into(),
            },
            Ok(report) => WorkspaceKgStateEvent::Ready {
                global_entities: report.global_entities,
                entity_links: report.entity_links,
                imported_edges: report.imported_edges,
                cooccur_edges: report.cooccur_edges,
                llm_edges: report.llm_edges,
                llm_passes: report.llm_passes,
            },
            Err(e) => WorkspaceKgStateEvent::Unavailable { reason: e.to_string() },
        };
        let _ = thread_app.emit("workspace-kg-state", event);
        cancel_slot.lock().unwrap().take();
        running.store(false, Ordering::SeqCst);
    });
    true
}

/// Manually rebuild the workspace knowledge graph now (federated-kg task
/// 2.1). Mirrors `refresh_knowledge_model`: returns as soon as the build
/// thread is spawned; progress and outcome arrive via `workspace-kg-state`
/// events (`building` → `ready` | `unavailable`).
#[tauri::command(async)]
fn rebuild_workspace_kg(app: AppHandle, state: State<SharedState>) -> CmdResult<()> {
    let enabled = {
        let guard = state.lock().unwrap();
        federated_kg_enabled(&guard.app_settings)
    };
    if !enabled {
        return Err("federatedKg flag is off".into());
    }
    if !start_workspace_kg_build(&app, state.inner()) {
        return Err("a workspace-KG build is already running — give it a moment.".into());
    }
    Ok(())
}

/// One member's staleness in `workspace_kg_overview` (federated-kg task 2.2).
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct WorkspaceKgMemberStatusDto {
    project_id: String,
    name: String,
    /// This member's current `knowledge_model_built_at` watermark (`null` =
    /// no knowledge model built yet).
    current_watermark: Option<i64>,
    /// True when the workspace-KG's cached snapshot for this member is
    /// missing or behind `current_watermark` — i.e. the next build will
    /// re-read it (spec: "unchanged members are skipped").
    stale: bool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct WorkspaceKgOverviewDto {
    /// `null` before the first build ever completes.
    built_at: Option<i64>,
    llm_passes: bool,
    global_entities: usize,
    entity_links: usize,
    edges: usize,
    /// Only currently-open members (task 2.1/2.2 deviation — no workspace
    /// manifest to enumerate members that aren't open; see final report).
    members: Vec<WorkspaceKgMemberStatusDto>,
}

/// Workspace-KG summary: counts + per-member staleness (federated-kg task
/// 2.2). Flag-gated (task 2.3) — off returns an error before `kg.sqlite` is
/// even opened, so a read alone can never create it.
/// With `team`, the team's graph: a view of the merged one (never a
/// second build) keeping entities linked into the team's repos, their links
/// into those repos, the edges between them, and the team's members.
#[tauri::command(async)]
fn workspace_kg_overview(state: State<SharedState>, team: Option<String>) -> CmdResult<WorkspaceKgOverviewDto> {
    let guard = state.lock().unwrap();
    if !federated_kg_enabled(&guard.app_settings) {
        return Err("federatedKg flag is off".into());
    }
    let kg = ken_core::workspace_kg_db::WorkspaceKgDb::open(&workspace_kg_root(&guard)).map_err(err)?;
    let built_at = kg
        .get_meta("workspace_kg_built_at")
        .map_err(err)?
        .and_then(|s| s.parse::<i64>().ok());
    let llm_passes = kg.get_meta("llm_passes").map_err(err)?.as_deref() == Some("true");
    let team_ids = team_project_ids(&guard, team.as_deref());
    let in_team = |p: &str| team_ids.as_ref().is_none_or(|ids| ids.contains(p));
    let mut kept: std::collections::HashSet<i64> = std::collections::HashSet::new();
    let mut entity_links = 0usize;
    for e in kg.list_global_entities().map_err(err)? {
        let links = kg.list_links_for_global(e.id).map_err(err)?;
        let team_links = links.iter().filter(|l| in_team(&l.project_id)).count();
        if team_links > 0 || (team_ids.is_none() && links.is_empty()) {
            kept.insert(e.id);
            entity_links += team_links;
        }
    }
    let edges = kg
        .list_all_edges()
        .map_err(err)?
        .iter()
        .filter(|g| kept.contains(&g.src_global_id) && kept.contains(&g.dst_global_id))
        .count();

    let pseudo_id = memory_pseudo_member_id(&guard);
    let mut members = Vec::with_capacity(guard.members.len());
    for m in guard.members.values() {
        let project_id = m.project.config.id;
        if Some(project_id) == pseudo_id {
            continue; // ken-memory task 2.1 (D3): never federated
        }
        if !in_team(&project_id.to_string()) {
            continue;
        }
        let current_watermark = m.db.knowledge_model_built_at().map_err(err)?;
        let cached = kg.get_watermark(project_id).map_err(err)?;
        let stale = cached != Some(current_watermark);
        members.push(WorkspaceKgMemberStatusDto {
            project_id: project_id.to_string(),
            name: m.project.config.name.clone(),
            current_watermark,
            stale,
        });
    }
    members.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(WorkspaceKgOverviewDto {
        built_at,
        llm_passes,
        global_entities: kept.len(),
        entity_links,
        edges,
        members,
    })
}

/// One edge in a `workspace_kg_entity` wiki page — either an out-link (this
/// entity is `src`) or a back-link (this entity is `dst`); `other_id`/
/// `other_name` always name the OTHER endpoint (design D4: back-links are
/// `global_edges` queried in the reverse direction, not a separate table).
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct WorkspaceKgEdgeDto {
    id: i64,
    other_id: i64,
    other_name: String,
    relation: String,
    weight: f64,
    /// `"imported"` | `"cooccur"` | `"llm"`.
    provenance: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct WorkspaceKgPointerDto {
    project_id: String,
    rel_path: String,
    snippet: String,
    /// `ken://<project-id>/<rel-path>` (design D4 addressing).
    uri: String,
    /// True if this pointer's file is confirmed missing on disk. Only
    /// checkable for a currently-open member (task 2.2 deviation — no
    /// workspace manifest to resolve a closed member's root); a pointer
    /// into a member that isn't open is never flagged stale by this field
    /// alone (spec: "or is marked stale if the file no longer exists —
    /// never a crash" — never crashing is satisfied unconditionally; the
    /// staleness *signal* is best-effort until member enumeration lands).
    stale: bool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct WorkspaceKgEntityDto {
    id: i64,
    kind: String,
    name: String,
    summary: String,
    updated_at: i64,
    /// `kg://<id>` (design D4 addressing).
    uri: String,
    out_links: Vec<WorkspaceKgEdgeDto>,
    back_links: Vec<WorkspaceKgEdgeDto>,
    pointers: Vec<WorkspaceKgPointerDto>,
}

/// The full wiki-page payload for one global entity in a single call
/// (federated-kg task 2.2 / spec "Every global entity is a wiki page" +
/// design D4: "the frontend never joins") — summary, both edge directions,
/// and per-project doc pointers. Flag-gated (task 2.3).
#[tauri::command(async)]
fn workspace_kg_entity(state: State<SharedState>, id: i64) -> CmdResult<WorkspaceKgEntityDto> {
    let guard = state.lock().unwrap();
    if !federated_kg_enabled(&guard.app_settings) {
        return Err("federatedKg flag is off".into());
    }
    let kg = ken_core::workspace_kg_db::WorkspaceKgDb::open(&workspace_kg_root(&guard)).map_err(err)?;
    let entity = kg
        .get_global_entity(id)
        .map_err(err)?
        .ok_or_else(|| format!("no global entity {id}"))?;

    let name_of = |kg: &ken_core::workspace_kg_db::WorkspaceKgDb, gid: i64| -> CmdResult<String> {
        Ok(kg.get_global_entity(gid).map_err(err)?.map(|e| e.name).unwrap_or_default())
    };
    let out_links = kg
        .list_edges_from(id)
        .map_err(err)?
        .into_iter()
        .map(|e| {
            Ok(WorkspaceKgEdgeDto {
                id: e.id,
                other_id: e.dst_global_id,
                other_name: name_of(&kg, e.dst_global_id)?,
                relation: e.relation,
                weight: e.weight,
                provenance: e.provenance,
            })
        })
        .collect::<CmdResult<Vec<_>>>()?;
    let back_links = kg
        .list_edges_to(id)
        .map_err(err)?
        .into_iter()
        .map(|e| {
            Ok(WorkspaceKgEdgeDto {
                id: e.id,
                other_id: e.src_global_id,
                other_name: name_of(&kg, e.src_global_id)?,
                relation: e.relation,
                weight: e.weight,
                provenance: e.provenance,
            })
        })
        .collect::<CmdResult<Vec<_>>>()?;

    let pointers = kg
        .list_pointers_for_global(id)
        .map_err(err)?
        .into_iter()
        .map(|p| {
            let stale = uuid::Uuid::parse_str(&p.project_id)
                .ok()
                .and_then(|pid| guard.members.get(&pid))
                .map(|m| !m.project.root.join(&p.rel_path).exists())
                .unwrap_or(false);
            WorkspaceKgPointerDto {
                uri: format!("ken://{}/{}", p.project_id, p.rel_path),
                project_id: p.project_id,
                rel_path: p.rel_path,
                snippet: p.snippet,
                stale,
            }
        })
        .collect();

    Ok(WorkspaceKgEntityDto {
        uri: format!("kg://{}", entity.id),
        id: entity.id,
        kind: entity.kind,
        name: entity.name,
        summary: entity.summary,
        updated_at: entity.updated_at,
        out_links,
        back_links,
        pointers,
    })
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct WorkspaceKgSearchHitDto {
    id: i64,
    kind: String,
    name: String,
    summary: String,
    uri: String,
}

/// Cap on returned hits. No pagination — design's own scale bound (≤~3k
/// entities even beyond ~15 members) makes a hard cap enough.
const WORKSPACE_KG_SEARCH_LIMIT: usize = 50;

/// Search global entity names + summaries (federated-kg task 2.2).
///
/// Deviation (see final report): `kg.sqlite`'s schema
/// (`workspace_kg_db.rs`, out of this task's touch scope) has no FTS table
/// — only plain `global_entities` rows — so this is a case-insensitive
/// substring match over `list_global_entities()` done in Rust, the honest
/// floor the proposal's "FTS over global entity names + summaries" degrades
/// to until a schema migration adds one. Fine at workspace scale. Flag-gated
/// (task 2.3).
#[tauri::command(async)]
fn workspace_kg_search(
    state: State<SharedState>,
    query: String,
    team: Option<String>,
) -> CmdResult<Vec<WorkspaceKgSearchHitDto>> {
    let guard = state.lock().unwrap();
    if !federated_kg_enabled(&guard.app_settings) {
        return Err("federatedKg flag is off".into());
    }
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let kg = ken_core::workspace_kg_db::WorkspaceKgDb::open(&workspace_kg_root(&guard)).map_err(err)?;
    // A team's view: only entities with a link into one of its repos.
    let team_ids = team_project_ids(&guard, team.as_deref());
    let in_team = |id: i64| -> bool {
        team_ids.as_ref().is_none_or(|ids| {
            kg.list_links_for_global(id).map(|ls| ls.iter().any(|l| ids.contains(&l.project_id))).unwrap_or(false)
        })
    };
    let mut hits: Vec<WorkspaceKgSearchHitDto> = kg
        .list_global_entities()
        .map_err(err)?
        .into_iter()
        .filter(|e| e.name.to_lowercase().contains(&q) || e.summary.to_lowercase().contains(&q))
        .filter(|e| in_team(e.id))
        .map(|e| WorkspaceKgSearchHitDto {
            uri: format!("kg://{}", e.id),
            id: e.id,
            kind: e.kind,
            name: e.name,
            summary: e.summary,
        })
        .collect();
    // Name matches rank above summary-only matches; stable by id within
    // each tier for a deterministic result order.
    hits.sort_by(|a, b| {
        let a_name = a.name.to_lowercase().contains(&q);
        let b_name = b.name.to_lowercase().contains(&q);
        b_name.cmp(&a_name).then(a.id.cmp(&b.id))
    });
    hits.truncate(WORKSPACE_KG_SEARCH_LIMIT);
    Ok(hits)
}

/// `RoutePlan` DTO for `route_search` (kg-routing task 2.1). `targets` are
/// stringified `Uuid`s (matches every other project-id DTO field in this
/// file, e.g. `WorkspaceKgOverviewDto::project_id`); `reason` self-tags so
/// the frontend can switch on `reason.type`.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RoutePlanDto {
    targets: Vec<String>,
    reason: RouteReasonDto,
}

#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum RouteReasonDto {
    Named,
    /// `entityIds`: the global entity ids (`kg.sqlite`) that selected these
    /// targets — same ids the merged hits' `kgBreadcrumbs` are built from.
    KgEntities { entity_ids: Vec<i64> },
    Broadcast,
}

impl From<&routing::RouteReason> for RouteReasonDto {
    fn from(r: &routing::RouteReason) -> Self {
        match r {
            routing::RouteReason::Named => RouteReasonDto::Named,
            routing::RouteReason::KgEntities(ids) => RouteReasonDto::KgEntities { entity_ids: ids.clone() },
            routing::RouteReason::Broadcast => RouteReasonDto::Broadcast,
        }
    }
}

impl From<&routing::RoutePlan> for RoutePlanDto {
    fn from(p: &routing::RoutePlan) -> Self {
        RoutePlanDto {
            targets: p.targets.iter().map(|id| id.to_string()).collect(),
            reason: (&p.reason).into(),
        }
    }
}

/// One merged, cited hit (kg-routing task 2.1). Mirrors `routing::RoutedHit`
/// with the same string-source/camelCase conventions `HybridSearchHitDto`
/// (semantic-index task 2.2) already uses for `source`.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RoutedHitDto {
    path: String,
    chunk_id: i64,
    snippet: String,
    source: &'static str,
    project_id: String,
    member_name: String,
    /// `ken://<project-id>/<rel-path>`.
    address: String,
    /// The line the hit's chunk starts on, when known.
    line: Option<i64>,
    /// `repo:path:line`, `repo@sha:…`, led by `[[Note]]` in a wiki repo.
    locator: String,
    /// For a Markdown page: section, verified or evidence date, retired or
    /// generated, and what replaces it.
    page: Option<ken_core::pagemeta::HitPage>,
    /// `kg://<entity-id>` per entity that selected this hit's plan; empty
    /// unless the plan's reason was `KgEntities` (routing.rs module doc: "KG
    /// breadcrumbs are plan-level, not per-hit").
    kg_breadcrumbs: Vec<String>,
    /// What the file is for (`contenttype::of`).
    content_type: &'static str,
}

impl From<routing::RoutedHit> for RoutedHitDto {
    fn from(h: routing::RoutedHit) -> Self {
        let source = match h.source {
            hybrid_search_mod::Source::Keyword => "keyword",
            hybrid_search_mod::Source::Semantic => "semantic",
            hybrid_search_mod::Source::Both => "both",
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
            kg_breadcrumbs: h.kg_breadcrumbs,
        }
    }
}

/// One member's outcome in a `route_search` call (kg-routing task 2.1).
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct MemberStatusEntryDto {
    project_id: String,
    member_name: String,
    /// `"searched"` | `"index-building"` | `"unavailable"` — spec's exact
    /// per-member status vocabulary ("Fan-out hybrid search with rank-only
    /// merge").
    status: &'static str,
}

impl From<&routing::MemberStatusEntry> for MemberStatusEntryDto {
    fn from(s: &routing::MemberStatusEntry) -> Self {
        let status = match s.status {
            routing::MemberStatus::Searched => "searched",
            routing::MemberStatus::IndexBuilding => "index-building",
            routing::MemberStatus::Unavailable => "unavailable",
        };
        MemberStatusEntryDto {
            project_id: s.project_id.to_string(),
            member_name: s.member_name.clone(),
            status,
        }
    }
}

/// `route_search`'s return shape (kg-routing task 2.1 / proposal: "returning
/// `{ plan, results, member_status }`"), mirroring `routing::ExecutionReport`
/// field-for-field.
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

/// Payload for the `routed-search-state` event (kg-routing task 2.1),
/// mirroring `WorkspaceKgStateEvent`'s tag shape AND its "app-global, not
/// `emit_member`" choice: a routed search spans every planned member at
/// once (like a workspace-KG build), so it has no single owning `project_id`
/// to scope the event to. `{"state":"planning"}`,
/// `{"state":"searching","done":1,"total":3}`, `{"state":"done"}`.
#[derive(Clone, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
enum RoutedSearchStateEvent {
    Planning,
    Searching { done: usize, total: usize },
    Done,
}

/// One workspace member's handles needed to plan and run a routed search
/// (kg-routing task 2.1) — snapshotted under the state lock, then used after
/// it's dropped so planning/embedding/searching never hold the global lock.
/// Mirrors `hybrid_search`'s own "clone the `Arc<Mutex<Db>>`, release the
/// guard, do the real work off it" discipline — the "search_db/spawn_blocking
/// pattern (lock-audit template)" this task's brief names — generalized to
/// every open member instead of just the focused one.
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

/// Route a query across every open workspace member and return the merged,
/// cited results (kg-routing task 2.1): plan with `routing::plan_route`
/// (KG-guided tier only when `federatedKg` is also on; Named/Broadcast
/// otherwise — design D1's "soft dependency"), fan out `routing::
/// search_member` per target concurrently on the blocking pool, merge with
/// `routing::merge_routed`. Emits `routed-search-state` (`planning` →
/// `searching m/n` → `done`) as an app-global event — see
/// `RoutedSearchStateEvent`'s doc for why not `emit_member`.
///
/// Targets come from the **workspace manifest** (`Workspace::members`,
/// `MemberStatus::Ok` only), not from whatever happens to be open this
/// session (ken-home-workspace 2.1 — this replaces kg-routing's original
/// `AppState::members` stand-in, which existed only because `workspace.rs`
/// hadn't shipped yet). A resident member is searched through its live
/// handle; a dormant one has a short-lived `Db::open(base, project_id)`
/// opened for the query and dropped after, which does not activate it,
/// start any watcher, or evict a resident. `Missing`/`Invalid` members are
/// never targets — they surface in the members overview instead. With no
/// workspace open (single-project mode) this falls back to `AppState::
/// members` exactly as before.
///
/// `scope` pins the search to one member: planning short-circuits to that
/// single target without consulting the KG, and the result shape is
/// identical to an unpinned search so the UI renders one thing.
///
/// The query is embedded exactly once (design: "the query is embedded once
/// and reused across all member KNN searches") via the focused member's
/// `semantic_embedder` slot when a member is focused, else the first open
/// member's — every open member's slot loads the same local embedding
/// model, so any one is representative; the resulting vector (or `None` if
/// embedding is unavailable) is cloned into every per-member search.
/// `MemberInfo::index_ready`/`last_activity` (kg-routing ken-core task 1.1's
/// doc comment) are read here from what `MemberRuntime` actually tracks:
/// `index_ready` = `Db::vec_available()` (same readiness signal
/// `hybrid_search` gates its KNN pass on); `last_activity` = the most recent
/// `finished_at` across that member's `fresh` ingest runs (`features/
/// multi-project/README.md`'s "recent activity ... most recent ingest
/// completion timestamp" contract), via the existing `Db::runs_with_status`
/// read — no new `Db` method needed.
#[tauri::command]
async fn route_search(
    app: AppHandle,
    state: State<'_, SharedState>,
    query: String,
    limit: Option<usize>,
    scope: Option<uuid::Uuid>,
    group: Option<String>,
    audience: Option<String>,
    types: Option<String>,
) -> CmdResult<RouteSearchDto> {
    let limit = limit.unwrap_or(30);
    let wanted = ken_core::contenttype::parse_filter(types.as_deref());
    // Item 2b: a reader chosen (business, dev) reads further down each
    // member's ranking so the filter still fills the page.
    let filtered = audience.as_deref().is_some_and(|a| !a.is_empty() && a != "any");
    let fetch = if filtered { limit * 3 } else { limit };

    let group_targets: Option<Vec<uuid::Uuid>>;
    let (base_dir, kg_root, kg_enabled, embedder_slot, snapshots) = {
        let guard = state.lock().unwrap();
        if !kg_routing_enabled(&guard.app_settings) {
            return Err("kgRouting flag is off".into());
        }
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
        // plan's targets directly — `RoutePlan.targets` is already a list,
        // so a group needs no special search path.
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
        // Every member's slot loads the same embedding model, so any
        // resident one is representative — but with every member dormant
        // there is none, and the search runs FTS-only rather than failing.
        let embedder_slot = guard
            .focused
            .and_then(|id| snapshots.iter().find(|s| s.project_id == id))
            .and_then(|s| s.embedder_slot.clone())
            .or_else(|| snapshots.iter().find_map(|s| s.embedder_slot.clone()));
        (
            guard.base_dir.clone(),
            workspace_kg_root(&guard),
            federated_kg_enabled(&guard.app_settings),
            embedder_slot,
            snapshots,
        )
    };

    let _ = app.emit("routed-search-state", RoutedSearchStateEvent::Planning);

    // Plan + embed on the blocking pool: `plan_route`'s KG read and
    // `embed_query` can both touch disk / the local model.
    let query_for_plan = query.clone();
    let plan_snapshots: Vec<(uuid::Uuid, String, Option<Arc<Mutex<Db>>>)> = snapshots
        .iter()
        .map(|s| (s.project_id, s.name.clone(), s.search_db.clone()))
        .collect();
    let plan_base = base_dir.clone();
    let (plan, members, query_vec, dormant_dbs) = tauri::async_runtime::spawn_blocking(move || {
        // Dormant members are opened ONCE here and reused by the fan-out
        // below. Opening again per search would double the per-member cost
        // the D5 latency budget is measured against.
        let mut dormant: std::collections::HashMap<uuid::Uuid, Arc<Mutex<Db>>> =
            std::collections::HashMap::new();
        let mut members: Vec<routing::MemberInfo> = Vec::with_capacity(plan_snapshots.len());
        let registry = ken_core::registry::Registry::load(&plan_base).ok();
        for (project_id, name, live) in &plan_snapshots {
            let handle: Option<Arc<Mutex<Db>>> = match live {
                Some(db) => Some(db.clone()),
                None => match Db::open(&plan_base, *project_id) {
                    Ok(db) => {
                        let arc = Arc::new(Mutex::new(db));
                        dormant.insert(*project_id, arc.clone());
                        Some(arc)
                    }
                    // An index that won't open is simply not ready: the
                    // Broadcast tier filters it out at plan time, and a
                    // KG-guided pick still reports `Unavailable` below
                    // rather than failing the whole search.
                    Err(_) => None,
                },
            };
            let (index_ready, last_activity) = match &handle {
                Some(db) => {
                    let db = db.lock().unwrap();
                    let last = db
                        .runs_with_status("fresh")
                        .ok()
                        .and_then(|rows| rows.iter().filter_map(|r| r.finished_at).max())
                        .unwrap_or(0);
                    (db.vec_available(), last)
                }
                None => (false, 0),
            };
            let knowledge_base = registry
                .as_ref()
                .and_then(|r| r.projects.iter().find(|e| e.id == *project_id))
                .is_some_and(|e| routing::is_knowledge_base(&e.kind));
            members.push(routing::MemberInfo {
                project_id: *project_id,
                name: name.clone(),
                index_ready,
                last_activity,
                knowledge_base,
            });
        }

        // A pinned scope never consults the KG (design D3) — there is
        // nothing to plan when the targets are already named. A group is
        // the same case with more than one target.
        let plan = match (scope, group_targets) {
            (Some(id), _) => routing::RoutePlan {
                targets: vec![id],
                reason: routing::RouteReason::Named,
                platform: routing::asks_about_platform(&query_for_plan),
                knowledge: Vec::new(),
            },
            (None, Some(ids)) => routing::RoutePlan {
                knowledge: members.iter().filter(|m| m.knowledge_base && ids.contains(&m.project_id)).map(|m| m.project_id).collect(),
                targets: ids,
                reason: routing::RouteReason::Named,
                platform: routing::asks_about_platform(&query_for_plan),
            },
            (None, None) => {
                let kg = if kg_enabled {
                    ken_core::workspace_kg_db::WorkspaceKgDb::open(&kg_root).ok()
                } else {
                    None
                };
                routing::plan_route(&query_for_plan, &members, kg.as_ref())
            }
        };

        let query_vec = embedder_slot.as_ref().and_then(|slot| {
            let mut guard = slot.lock().unwrap();
            guard.as_deref_mut().and_then(|e| e.embed_query(&query_for_plan).ok())
        });

        (plan, members, query_vec, dormant)
    })
    .await
    .map_err(|e| e.to_string())?;

    let total = plan.targets.len();
    let _ = app.emit(
        "routed-search-state",
        RoutedSearchStateEvent::Searching { done: 0, total },
    );

    let ready_by_id: std::collections::HashMap<uuid::Uuid, bool> =
        members.iter().map(|m| (m.project_id, m.index_ready)).collect();
    let name_by_id: std::collections::HashMap<uuid::Uuid, String> =
        members.iter().map(|m| (m.project_id, m.name.clone())).collect();
    // Resident handles plus the dormant ones opened during planning. A
    // member absent from this map (its index wouldn't open) falls through
    // to the `Unavailable` arm below.
    let mut db_by_id: std::collections::HashMap<uuid::Uuid, Arc<Mutex<Db>>> = snapshots
        .into_iter()
        .filter_map(|s| s.search_db.map(|db| (s.project_id, db)))
        .collect();
    db_by_id.extend(dormant_dbs);
    // One vocabulary for the whole search: the team wiki's Vocabulary page
    // and decisions aliases widen the search in every repo, not only the wiki.
    let shared_vocab = {
        let vocabs: Vec<ken_core::vocab::Vocabulary> = db_by_id
            .values()
            .filter_map(|db| db.lock().ok().and_then(|g| ken_core::vocab::Vocabulary::cached(&g).ok()))
            .collect();
        Arc::new(ken_core::vocab::Vocabulary::merged(vocabs.iter()))
    };

    // Concurrent fan-out (design D5: "per-member searches run concurrently"):
    // every target's search is its own `spawn_blocking` task, started before
    // any is awaited, so they run in parallel on the blocking pool rather
    // than one after another.
    let done_counter = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::with_capacity(total);
    for target_id in plan.targets.clone() {
        let name = name_by_id.get(&target_id).cloned().unwrap_or_default();
        let index_ready = ready_by_id.get(&target_id).copied().unwrap_or(false);
        let db = db_by_id.get(&target_id).cloned();
        let query = query.clone();
        let query_vec = query_vec.clone();
        let counter = done_counter.clone();
        let ev_app = app.clone();
        let audience = audience.clone();
        let wanted = wanted.clone();
        let shared_vocab = shared_vocab.clone();
        handles.push(tauri::async_runtime::spawn_blocking(
            move || -> routing::MemberHits {
                let result = if let Some(db) = db {
                    if !index_ready {
                        routing::MemberHits {
                            project_id: target_id,
                            member_name: name,
                            status: routing::MemberStatus::IndexBuilding,
                            hits: Vec::new(),
                        }
                    } else {
                        let db = db.lock().unwrap();
                        match routing::search_member_of_with(&db, &query, query_vec.as_deref(), fetch, &wanted, Some(&shared_vocab)) {
                            Ok(mut hits) => routing::MemberHits {
                                hits: {
                                    hits.retain(|h| {
                                        ken_core::pagemeta::suits(audience.as_deref(), h.page.as_ref().and_then(|p| p.audience))
                                    });
                                    hits.truncate(limit);
                                    hits
                                },
                                project_id: target_id,
                                member_name: name,
                                status: routing::MemberStatus::Searched,
                            },
                            Err(_) => routing::MemberHits {
                                project_id: target_id,
                                member_name: name,
                                status: routing::MemberStatus::Unavailable,
                                hits: Vec::new(),
                            },
                        }
                    }
                } else {
                    routing::MemberHits {
                        project_id: target_id,
                        member_name: name,
                        status: routing::MemberStatus::Unavailable,
                        hits: Vec::new(),
                    }
                };
                let done = counter.fetch_add(1, Ordering::SeqCst) + 1;
                let _ = ev_app.emit(
                    "routed-search-state",
                    RoutedSearchStateEvent::Searching { done, total },
                );
                result
            },
        ));
    }

    let mut member_hits = Vec::with_capacity(handles.len());
    for h in handles {
        member_hits.push(h.await.map_err(|e| e.to_string())?);
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

/// One day's journal content, as returned by `read_journal`.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct JournalDayDto {
    /// `YYYY-MM-DD`.
    date: String,
    content: String,
}

/// Create or replace a memory (ken-memory task 2.2 / design D5). `scope` is
/// `"workspace"` (`.ken-workspace/memory/`) or `"project"` (the currently
/// FOCUSED member's `.ken/memory/` — judgment call: the command takes no
/// explicit `project_id`, matching how D4's context injection already scopes
/// "project memories" to whichever member is focused, and keeping the tool
/// surface small; a future multi-target `memory_write` could add an explicit
/// id without breaking this contract, since "project" today unambiguously
/// means "the focused one"). `mode` is `"create"` (slug must not exist) or
/// `"replace"` (slug must exist) — `memory::WriteMode`'s two explicit
/// intents, spelled out as strings at the command boundary.
///
/// Known gap (project scope only): `<project>/.ken/memory/` lives inside
/// `.ken/`, which `scan.rs`'s `WalkBuilder` (`.hidden(true)`) and
/// `watch.rs`'s `relevant_path` both hard-exclude (any path component
/// starting with `.`) — outside this task's touch-boundary to fix. A
/// project-scoped memory is written and immediately reflected by
/// `list_memories`/chat injection (both read the filesystem directly, not
/// the index), but never reaches that project's own FTS/semantic index or
/// `ken://` search-resolved address. Only the workspace-scope tier is
/// actually indexed (via the pseudo-member's own engine, rooted at
/// `.ken-workspace` itself, which isn't nested under anything excluded).
#[tauri::command(async)]
fn memory_write(
    state: State<SharedState>,
    scope: String,
    slug: String,
    content: String,
    mode: String,
) -> CmdResult<memory::Memory> {
    let guard = state.lock().unwrap();
    let write_mode = match mode.as_str() {
        "create" => memory::WriteMode::Create,
        "replace" => memory::WriteMode::Replace,
        _ => return Err(format!("unknown memory write mode '{mode}' — use \"create\" or \"replace\"")),
    };
    let today = local_date_today();
    let path = match scope.as_str() {
        "workspace" => {
            let ws_root = guard.workspace.as_ref().ok_or("no workspace open")?.ws.root.clone();
            let mscope = memory::MemoryScope::Workspace { workspace_root: &ws_root };
            memory::write_memory(mscope, &slug, &content, write_mode, &today).map_err(err)?
        }
        "project" => {
            let active = member(&guard, None)?;
            let mscope = memory::MemoryScope::Project { project_root: &active.project.root };
            memory::write_memory(mscope, &slug, &content, write_mode, &today).map_err(err)?
        }
        _ => return Err(format!("unknown memory scope '{scope}' — use \"workspace\" or \"project\"")),
    };
    let raw = std::fs::read_to_string(&path).map_err(err)?;
    Ok(memory::parse_memory(&slug, &raw))
}

/// Append a timestamped entry to today's journal file, creating it if absent
/// (ken-memory task 2.2 / design D5) — the write MCP's `journal_append`
/// delegates to the same `memory::append_journal` core once ken-mcp task 3.1
/// lands (parallel session; not this file).
#[tauri::command(async)]
fn journal_append(
    state: State<SharedState>,
    text: String,
    project: Option<String>,
    tags: Option<Vec<String>>,
) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let ws_root = guard.workspace.as_ref().ok_or("no workspace open")?.ws.root.clone();
    memory::append_journal(
        &ws_root,
        &text,
        project.as_deref(),
        &tags.unwrap_or_default(),
        &local_date_today(),
        &local_time_hhmm(),
    )
    .map_err(err)?;
    Ok(())
}

/// Recent journal content on demand (ken-memory task 2.2 / design D4: "the
/// journal is NOT injected... a `read_journal(days_back?)` tool returns
/// recent days on demand"). Walks back `days_back` calendar days (default 1
/// = today only) from local today, checking `journal/` then
/// `journal/archive/` for each date (spec: "archived journal stays
/// findable") — a day with no file on either side is skipped, not an error.
/// Most-recent-first.
#[tauri::command(async)]
fn read_journal(state: State<SharedState>, days_back: Option<u32>) -> CmdResult<Vec<JournalDayDto>> {
    let ws_root = {
        let guard = state.lock().unwrap();
        guard.workspace.as_ref().ok_or("no workspace open")?.ws.root.clone()
    };

    let days_back = days_back.unwrap_or(1).max(1) as i64;
    let today = chrono::Local::now().date_naive();
    let dir = memory::journal_dir(&ws_root);
    let archive_dir = memory::journal_archive_dir(&ws_root);
    let mut out = Vec::new();
    for i in 0..days_back {
        let date = today - chrono::Duration::days(i);
        let name = date.format("%Y-%m-%d").to_string();
        let in_current = dir.join(format!("{name}.md"));
        let path = if in_current.is_file() { in_current } else { archive_dir.join(format!("{name}.md")) };
        if let Ok(content) = std::fs::read_to_string(&path) {
            out.push(JournalDayDto { date: name, content });
        }
    }
    Ok(out)
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

/// Run a distillation pass over the current journal window (ken-memory task
/// 2.2 / design D6): compose the prompt (journal window + existing workspace
/// memory descriptions + previously-dismissed slugs, all as the dedupe
/// guard), generate at Background priority on the local LLM — same call
/// shape as `AppFederationLlm` — parse tolerantly, cap 5, and emit
/// `memory-state` (`planning` → `distilling` → `ready`/`error`). Candidates
/// are cached server-side (`AppState::memory_distill_candidates`) so
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
            let window = journal_window_text(&ws_root);
            let dismissed = UserState::load(&base_dir, pseudo_id).ignored;

            let mut existing: Vec<(String, String)> =
                memory::list_memories(&memory::workspace_memory_dir(&ws_root))
                    .unwrap_or_default()
                    .into_iter()
                    .map(|m| (m.slug, m.description))
                    .collect();
            for tagged in &dismissed {
                if let Some(slug) = tagged.strip_prefix(DISMISSED_MEMORY_PREFIX) {
                    existing.push((slug.to_string(), "previously suggested and dismissed by the user".to_string()));
                }
            }

            let prompt = memory::compose_distill_prompt(&window, &existing);
            let _ = bg_app.emit("memory-state", MemoryStateEvent::Distilling);
            let raw = claude_text(&ws_root, &prompt)?;

            let mut candidates = memory::parse_distill_candidates(&raw);
            candidates.retain(|c| {
                !dismissed.contains(&format!("{DISMISSED_MEMORY_PREFIX}{}", c.slug))
            });
            Ok(candidates)
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
// Your day: my tasks, the team's ticket files, the team digest, index
// health, and the watcher behind `day-changed`. The file logic lives in
// `ken_core::day`, shared with ken-mcp; this layer finds the homes, the
// team's repos and the inboxes, and shapes the wire DTOs.
// ---------------------------------------------------------------------

/// One repo of a team: a resolvable workspace member.
#[derive(Clone)]
struct TeamRepo {
    id: uuid::Uuid,
    name: String,
    root: PathBuf,
}

/// The repos of `team` (a manifest group, by name), or every member when
/// no team is given. Same membership `team_overview` and `team_project_ids`
/// use.
fn team_repos(ws: &ken_core::workspace::Workspace, team: Option<&str>) -> Vec<TeamRepo> {
    let names: Option<Vec<String>> = team.filter(|t| !t.is_empty()).map(|t| ws.config.effective_group_members(t));
    ws.members
        .iter()
        .filter(|m| names.as_ref().is_none_or(|n| n.contains(&m.name)))
        .filter_map(|m| match &m.status {
            ken_core::workspace::MemberStatus::Ok(p) => {
                Some(TeamRepo { id: p.config.id, name: m.name.clone(), root: p.root.clone() })
            }
            _ => None,
        })
        .collect()
}

/// The family connections Your day reads: those attached to this
/// workspace, and those attached to none.
fn day_families(ws: &ken_core::workspace::Workspace, settings: &ken_core::settings::AppSettings) -> Vec<FamilyConnection> {
    family_connections(settings)
        .into_iter()
        .filter(|c| c.attached_workspace_id.is_none_or(|id| id == ws.config.id))
        .collect()
}

/// Every task home Your day reads, owned so the borrowed `TaskHome`s can
/// be built from it: the workspace home (where new tasks go) and my board
/// in each family. A repo's own `.ken/tasks/` is not read: tasks live in
/// your own folder, not inside a team repo (Ways of Working).
struct DayHomes {
    ws_root: PathBuf,
    boards: Vec<PathBuf>,
}

impl DayHomes {
    fn new(ws: &ken_core::workspace::Workspace, base_dir: &Path, settings: &ken_core::settings::AppSettings) -> DayHomes {
        DayHomes {
            ws_root: ws.root.clone(),
            boards: day_families(ws, settings)
                .into_iter()
                .map(|c| family::board_dir(&family_clone_root(base_dir, c.family_id), &c.member_id))
                .collect(),
        }
    }

    fn homes(&self) -> Vec<tasks::TaskHome<'_>> {
        let mut out = vec![tasks::TaskHome::Workspace { workspace_root: &self.ws_root }];
        for board_dir in &self.boards {
            out.push(tasks::TaskHome::Family { board_dir });
        }
        out
    }

    fn scan(&self, today: &str) -> Vec<ken_core::day::DayTask> {
        ken_core::day::scan(&self.homes(), today)
    }
}

/// A teammate's display name from a family manifest, keyed by member id.
fn family_member_names(base_dir: &Path, conns: &[FamilyConnection]) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for c in conns {
        if let Ok(m) = FamilyManifest::load(&family_clone_root(base_dir, c.family_id)) {
            for member in m.members {
                if !member.name.trim().is_empty() {
                    out.insert(member.id.clone(), member.name.trim().to_string());
                }
            }
        }
    }
    out
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct DayInboxRefDto {
    family_id: String,
    item_id: String,
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
    /// Relative to the workspace root (forward slashes). A file outside it
    /// (a family board or inbox, in app data) is its absolute path.
    rel_path: String,
    inbox: Option<DayInboxRefDto>,
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

/// An open escalation addressed to me (`escalations/*.md` in a team repo),
/// read-only: its row opens the file in Files.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct DayEscalationDto {
    id: String,
    title: String,
    raised_by: Option<String>,
    status: String,
    ticket: Option<String>,
    raised: Option<String>,
    blocks: Option<String>,
    project_id: String,
    repo: String,
    rel_path: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct DayStateDto {
    tickets: Vec<DayTicketDto>,
    tasks: Vec<DayTaskDto>,
    /// Open escalations addressed to me, from every repo of the team.
    escalations: Vec<DayEscalationDto>,
    me: DayMeDto,
    has_tickets: bool,
}

/// Open escalations addressed to me in `repos`, oldest raised first.
fn my_escalations(repos: &[TeamRepo], me: &ken_core::day::Me) -> Vec<DayEscalationDto> {
    if !me.is_known() {
        return Vec::new();
    }
    let mut out: Vec<DayEscalationDto> = repos
        .iter()
        .flat_map(|repo| {
            ken_core::day::scan_escalations(&repo.root)
                .into_iter()
                .filter(|e| e.open && me.addressed(e))
                .map(move |e| DayEscalationDto {
                    id: e.id,
                    title: e.title,
                    raised_by: e.raised_by,
                    status: e.status,
                    ticket: e.ticket,
                    raised: e.raised,
                    blocks: e.blocks,
                    project_id: repo.id.to_string(),
                    repo: repo.name.clone(),
                    rel_path: e.rel_path,
                })
        })
        .collect();
    out.sort_by(|a, b| (a.raised.is_none(), &a.raised, &a.id).cmp(&(b.raised.is_none(), &b.raised, &b.id)));
    out
}

fn ws_rel_path(ws_root: &Path, path: &Path) -> String {
    match path.strip_prefix(ws_root) {
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => path.to_string_lossy().replace('\\', "/"),
    }
}

fn day_task_dto(
    ws_root: &Path,
    t: &ken_core::day::DayTask,
    names: &std::collections::HashMap<String, String>,
    inbox: Option<DayInboxRefDto>,
) -> DayTaskDto {
    let named = |v: &Option<String>| v.as_ref().map(|s| names.get(s).cloned().unwrap_or_else(|| s.clone()));
    DayTaskDto {
        id: t.id.clone(),
        title: t.title.clone(),
        state: t.state,
        target: t.target.clone(),
        repeat: t.repeat.clone(),
        links: t.links.clone(),
        from: named(&t.from),
        description: t.description.clone(),
        updated_by: named(&t.updated_by),
        updated: t.updated.clone(),
        created: t.created.clone(),
        rel_path: ws_rel_path(ws_root, &t.path),
        inbox,
    }
}

/// Pending inbox tasks addressed to me (kind `task`, unread or seen), as
/// tasks, each with the inbox item it came from.
fn pending_inbox_tasks(base_dir: &Path, conns: &[FamilyConnection]) -> Vec<(ken_core::day::DayTask, DayInboxRefDto)> {
    let mut out = Vec::new();
    for c in conns {
        let clone_root = family_clone_root(base_dir, c.family_id);
        for (path, item) in list_inbox_items(&family::inbox_dir(&clone_root, &c.member_id)) {
            if !item.is_pending_task() {
                continue;
            }
            let payload = item.task.clone().unwrap_or_default();
            let title = if payload.title.trim().is_empty() { item.title.trim() } else { payload.title.trim() };
            let target = Some(payload.due.trim()).filter(|d| ken_core::day::is_date(d)).map(str::to_string);
            let from = Some(item.from.trim().to_string()).filter(|f| !f.is_empty());
            let task = ken_core::day::DayTask {
                id: item.id.clone(),
                title: title.to_string(),
                state: ken_core::day::DayTaskState::Open,
                target,
                repeat: None,
                links: Vec::new(),
                from: from.clone(),
                description: item.body.trim().to_string(),
                updated_by: from,
                updated: Some(item.updated.clone()).filter(|u| !u.is_empty()),
                created: Some(item.created.clone()).filter(|u| !u.is_empty()),
                done_on: None,
                status_raw: item.status_raw.clone(),
                has_due: false,
                home_dir: path.parent().map(Path::to_path_buf).unwrap_or_default(),
                path,
                home: tasks::HomeKind::Family,
            };
            out.push((task, DayInboxRefDto { family_id: c.family_id.to_string(), item_id: item.id.clone() }));
        }
    }
    out
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
/// filesystem work happens under the lock. Remembers `team` as the last
/// one asked for (the morning digest writes it), in memory and on disk.
fn day_snapshot(
    state: &SharedState,
    team: Option<&str>,
) -> CmdResult<(ken_core::workspace::Workspace, PathBuf, ken_core::settings::AppSettings)> {
    let (snapshot, remembered) = {
        let mut guard = lock_tolerant(state);
        let ws = guard.workspace.as_ref().ok_or("no workspace open")?.ws.clone();
        let mut remembered = false;
        if let Some(t) = team.map(str::trim).filter(|t| !t.is_empty()) {
            if guard.last_team.as_deref() != Some(t) {
                guard.last_team = Some(t.to_string());
                remembered = true;
            }
        }
        ((ws, guard.base_dir.clone(), guard.app_settings.clone()), remembered)
    };
    if remembered {
        let (ws, base_dir, _) = &snapshot;
        let last = team.map(|t| t.trim().to_string());
        update_digest_schedule(base_dir, ws.config.id, |s| s.last_team = last);
    }
    Ok(snapshot)
}

/// My open tickets in the team, with how many of my tasks link to each.
/// The second value is whether the team has any ticket file at all.
fn my_tickets(
    repos: &[TeamRepo],
    me: &ken_core::day::Me,
    all_tasks: &[ken_core::day::DayTask],
) -> (Vec<DayTicketDto>, bool) {
    let mut has_tickets = false;
    let mut found: Vec<(ken_core::day::Ticket, &TeamRepo)> = Vec::new();
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

/// Your day (frame Y1): my open tickets in the team, and my tasks for
/// today (pending inbox tasks included, with `inbox` set).
#[tauri::command(async)]
fn day_state(state: State<SharedState>, team: Option<String>) -> CmdResult<DayStateDto> {
    let (ws, base_dir, settings) = day_snapshot(state.inner(), team.as_deref())?;
    let today = local_date_today();
    let conns = day_families(&ws, &settings);
    let names = family_member_names(&base_dir, &conns);
    let all = DayHomes::new(&ws, &base_dir, &settings).scan(&today);

    let mut listed = ken_core::day::today_list(all.clone(), &today);
    let mut inbox_refs: std::collections::HashMap<PathBuf, DayInboxRefDto> = std::collections::HashMap::new();
    for (task, r) in pending_inbox_tasks(&base_dir, &conns) {
        inbox_refs.insert(task.path.clone(), r);
        listed.push(task);
    }
    ken_core::day::sort_for_day(&mut listed);
    let tasks_out = listed
        .iter()
        .map(|t| day_task_dto(&ws.root, t, &names, inbox_refs.remove(&t.path)))
        .collect();

    let me = cached_git_me();
    let repos = team_repos(&ws, team.as_deref());
    let (tickets, has_tickets) = my_tickets(&repos, &me, &all);
    let escalations = my_escalations(&repos, &me);
    Ok(DayStateDto {
        tickets,
        tasks: tasks_out,
        escalations,
        me: DayMeDto { name: me.name, email: me.email },
        has_tickets,
    })
}

fn local_stamp_now() -> String {
    chrono::Local::now().format("%Y-%m-%dT%H:%M").to_string()
}

/// Find one of my tasks by id (never a pending inbox item: those are
/// accepted first).
fn find_day_task(
    ws: &ken_core::workspace::Workspace,
    base_dir: &Path,
    settings: &ken_core::settings::AppSettings,
    id: &str,
    today: &str,
) -> CmdResult<ken_core::day::DayTask> {
    let all = DayHomes::new(ws, base_dir, settings).scan(today);
    if let Some(t) = ken_core::day::find(&all, id) {
        return Ok(t.clone());
    }
    if pending_inbox_tasks(base_dir, &day_families(ws, settings)).iter().any(|(t, _)| t.id == id) {
        return Err("This task is still in the inbox: accept it first.".into());
    }
    Err(format!("no task with id '{id}'"))
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
        .ok_or_else(|| "that repo is not open".to_string())
}

/// Move one of my tickets to `status` (todo, in-progress, blocked,
/// in-review, testing, done, cancelled) in its file. The team repo's sync
/// carries it to the team; done and cancelled leave Your day.
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
    let _ = app.emit("day-changed", ());
    Ok(())
}

/// Reply to an escalation raised to me, in its file's thread; `resolve`
/// also closes it (`status: resolved`). The team repo's sync carries it to
/// whoever raised it.
#[tauri::command(async)]
fn escalation_reply(
    app: AppHandle,
    state: State<SharedState>,
    project_id: String,
    rel_path: String,
    text: String,
    resolve: bool,
) -> CmdResult<()> {
    if text.trim().is_empty() {
        return Err("Write the reply first.".into());
    }
    let rel = rel_path.replace('\\', "/");
    if !rel.starts_with("escalations/") || rel.split('/').any(|p| p == "..") || !rel.ends_with(".md") {
        return Err(format!("{rel_path} is not an escalation"));
    }
    let id: uuid::Uuid = project_id.parse().map_err(err)?;
    let root = {
        let guard = lock_tolerant(&state);
        guard
            .workspace
            .as_ref()
            .and_then(|w| {
                w.ws.members.iter().find_map(|m| match &m.status {
                    ken_core::workspace::MemberStatus::Ok(p) if p.config.id == id => Some(p.root.clone()),
                    _ => None,
                })
            })
            .ok_or("that repo is not open")?
    };
    let path = root.join(&rel);
    let raw = std::fs::read_to_string(&path).map_err(err)?;
    let me = cached_git_me();
    let who = me.name.clone().or(me.email.clone()).unwrap_or_else(|| "me".into());
    let next = ken_core::day::escalation_reply(&raw, &who, &local_date_today(), &text, resolve);
    std::fs::write(&path, next).map_err(err)?;
    let _ = app.emit("day-changed", ());
    Ok(())
}

/// Add a task to the workspace home (`updated_by: you`).
#[tauri::command(async)]
fn day_task_create(app: AppHandle, state: State<SharedState>, input: ken_core::day::DayTaskInput) -> CmdResult<DayTaskDto> {
    let (ws, base_dir, settings) = day_snapshot(state.inner(), None)?;
    let (today, now) = (local_date_today(), local_stamp_now());
    let stamp = ken_core::day::Stamp { today: &today, now: &now, by: ken_core::day::BY_YOU };
    // The id, and so the path, is known before the write: the watcher is
    // told about the file before it appears.
    let id = tasks::new_ulid();
    let path = tasks::workspace_tasks_dir(&ws.root).join(tasks::task_file_name(&id, input.title.trim()));
    let task = with_task_writes(&app, state.inner(), &[path], || {
        ken_core::day::create_task(&ws.root, &input, &stamp, Some(&id)).map_err(err)
    })?;
    let names = family_member_names(&base_dir, &day_families(&ws, &settings));
    Ok(day_task_dto(&ws.root, &task, &names, None))
}

/// Change a task (`updated_by: you`): only the fields given are written.
#[tauri::command(async)]
fn day_task_update(
    app: AppHandle,
    state: State<SharedState>,
    id: String,
    patch: ken_core::day::DayTaskPatch,
) -> CmdResult<DayTaskDto> {
    let (ws, base_dir, settings) = day_snapshot(state.inner(), None)?;
    let (today, now) = (local_date_today(), local_stamp_now());
    let task = find_day_task(&ws, &base_dir, &settings, &id, &today)?;
    let stamp = ken_core::day::Stamp { today: &today, now: &now, by: ken_core::day::BY_YOU };
    with_task_writes(&app, state.inner(), &[task.path.clone()], || {
        ken_core::day::update_task(&task, &patch, &stamp).map_err(err)
    })?;
    let updated = reread_day_task(&task, &today)?;
    let names = family_member_names(&base_dir, &day_families(&ws, &settings));
    Ok(day_task_dto(&ws.root, &updated, &names, None))
}

/// Delete a task: the file moves to its home's `archive/YYYY-MM/`.
#[tauri::command(async)]
fn day_task_delete(app: AppHandle, state: State<SharedState>, id: String) -> CmdResult<()> {
    let (ws, base_dir, settings) = day_snapshot(state.inner(), None)?;
    let today = local_date_today();
    let task = find_day_task(&ws, &base_dir, &settings, &id, &today)?;
    with_task_writes(&app, state.inner(), &[task.path.clone()], || {
        ken_core::day::archive_task(&task, &today).map(|_| ()).map_err(err)
    })
}

/// My tasks linked to a ticket (frame Y1c), done ones included: those
/// linked by `<repo>/<ID>` (`repo` = the member `project_id` names) and
/// those linked by the bare `<ID>` (older links, any repo).
#[tauri::command(async)]
fn ticket_tasks(state: State<SharedState>, project_id: String, ticket_id: String) -> CmdResult<Vec<DayTaskDto>> {
    let (ws, base_dir, settings) = day_snapshot(state.inner(), None)?;
    let repo = team_repos(&ws, None)
        .into_iter()
        .find(|r| r.id.to_string().eq_ignore_ascii_case(project_id.trim()))
        .map(|r| r.name);
    let today = local_date_today();
    let all = DayHomes::new(&ws, &base_dir, &settings).scan(&today);
    let id = ticket_id.trim();
    let linked = match &repo {
        Some(name) => format!("{name}/{id}"),
        None => id.to_string(),
    };
    let q = ken_core::day::TaskQuery { linked: Some(linked), ..Default::default() };
    let names = family_member_names(&base_dir, &day_families(&ws, &settings));
    Ok(ken_core::day::query(&all, &q).into_iter().map(|t| day_task_dto(&ws.root, t, &names, None)).collect())
}

// ---------- the team digest ----------

#[derive(Serialize, serde::Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct TeamDigestDto {
    generated_at: i64,
    body: String,
    sources: Vec<String>,
}

/// What is stored per team: the DTO plus the local day it was written for.
#[derive(Serialize, serde::Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct StoredTeamDigest {
    date: String,
    #[serde(flatten)]
    digest: TeamDigestDto,
}

/// `<app data>/team-digests/<workspace id>/<team>.json` (`_all` for the
/// whole workspace). App data, not the workspace folder: the digest is
/// this machine's reading of the day, not something to commit.
fn team_digest_path(base_dir: &Path, workspace_id: uuid::Uuid, team: Option<&str>) -> PathBuf {
    let key = match team.filter(|t| !t.is_empty()) {
        Some(t) => {
            let slug: String = t
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c.to_ascii_lowercase() } else { '-' })
                .collect();
            format!("team-{slug}-{:08x}", content_hash(t.as_bytes()) as u32)
        }
        None => "_all".to_string(),
    };
    base_dir.join("team-digests").join(workspace_id.to_string()).join(format!("{key}.json"))
}

fn load_team_digest(base_dir: &Path, workspace_id: uuid::Uuid, team: Option<&str>) -> Option<StoredTeamDigest> {
    let text = std::fs::read_to_string(team_digest_path(base_dir, workspace_id, team)).ok()?;
    serde_json::from_str(&text).ok()
}

fn save_team_digest(base_dir: &Path, workspace_id: uuid::Uuid, team: Option<&str>, stored: &StoredTeamDigest) -> CmdResult<()> {
    let path = team_digest_path(base_dir, workspace_id, team);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(err)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(stored).map_err(err)?).map_err(err)
}

/// The team's newest digest, whatever day it was written.
#[tauri::command(async)]
fn team_digest(state: State<SharedState>, team: Option<String>) -> CmdResult<Option<TeamDigestDto>> {
    let (ws, base_dir, _) = day_snapshot(state.inner(), team.as_deref())?;
    Ok(load_team_digest(&base_dir, ws.config.id, team.as_deref()).map(|s| s.digest))
}

/// Write the team digest now ("Write it now").
#[tauri::command(async)]
fn refresh_team_digest(app: AppHandle, state: State<SharedState>, team: Option<String>) -> CmdResult<()> {
    day_snapshot(state.inner(), team.as_deref())?;
    generate_team_digests(&app, state.inner(), vec![team], true)
}

/// Whether `team`'s digest is being written, or waits its turn: Your day
/// shows "Writing…" for one it did not start itself.
#[tauri::command(async)]
fn team_digest_writing(state: State<SharedState>, team: Option<String>) -> CmdResult<bool> {
    let ws = lock_tolerant(&state).workspace.as_ref().map(|w| w.ws.config.id);
    Ok(ws.is_some_and(|id| digest_writing(id, &team)))
}

/// The morning schedule's memory, per workspace, in app data
/// (`team-digests/<workspace id>/schedule.json`): the team Your day last
/// asked about (so a fresh launch writes that team's digest, not every
/// group's), and each team's last attempt (so a failure is not retried on
/// every window focus).
#[derive(Serialize, serde::Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
struct TeamDigestSchedule {
    last_team: Option<String>,
    /// Keyed by team name; `""` is the whole workspace.
    attempts: std::collections::HashMap<String, TeamDigestAttempt>,
}

#[derive(Serialize, serde::Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct TeamDigestAttempt {
    /// Epoch seconds.
    at: i64,
    /// The local day it was for.
    date: String,
    ok: bool,
}

/// After a failed attempt, the schedule waits this long before trying again.
const TEAM_DIGEST_RETRY_AFTER_SECS: i64 = 30 * 60;

fn digest_schedule_key(team: Option<&str>) -> String {
    team.map(str::trim).unwrap_or("").to_string()
}

fn digest_schedule_path(base_dir: &Path, workspace_id: uuid::Uuid) -> PathBuf {
    base_dir.join("team-digests").join(workspace_id.to_string()).join("schedule.json")
}

fn load_digest_schedule(base_dir: &Path, workspace_id: uuid::Uuid) -> TeamDigestSchedule {
    std::fs::read_to_string(digest_schedule_path(base_dir, workspace_id))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Read, change and write the schedule (best effort: a failed write only
/// means the schedule forgets). One writer at a time.
fn update_digest_schedule(base_dir: &Path, workspace_id: uuid::Uuid, change: impl FnOnce(&mut TeamDigestSchedule)) {
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

/// Should the schedule try `team` now? Not when today's digest is stored,
/// not when an attempt already succeeded today, and not within
/// `TEAM_DIGEST_RETRY_AFTER_SECS` of a failed one.
fn digest_due(schedule: &TeamDigestSchedule, team: Option<&str>, stored_today: bool, today: &str, now: i64) -> bool {
    if stored_today {
        return false;
    }
    match schedule.attempts.get(&digest_schedule_key(team)) {
        Some(a) if a.ok => a.date != today,
        Some(a) => now - a.at >= TEAM_DIGEST_RETRY_AFTER_SECS,
        None => true,
    }
}

/// The morning schedule (workspace open, window focus): past 07:00, write
/// today's digest for the last team asked about (remembered across
/// launches), else the first team, else the whole workspace when it has no
/// teams. One team per run; see `digest_due` for when it is skipped.
fn maybe_generate_team_digest(app: &AppHandle, state: &SharedState) {
    if local_hour() < 7 {
        return;
    }
    let (last_team, first_team, ws_id, base_dir) = {
        let guard = lock_tolerant(state);
        let Some(ws) = guard.workspace.as_ref() else { return };
        let first = ws.ws.config.effective_groups().into_iter().next().map(|g| g.name);
        (guard.last_team.clone(), first, ws.ws.config.id, guard.base_dir.clone())
    };
    let schedule = load_digest_schedule(&base_dir, ws_id);
    let team = match last_team {
        Some(t) => Some(t),
        None => {
            let remembered = schedule.last_team.clone().filter(|t| !t.trim().is_empty());
            if let Some(t) = &remembered {
                let mut guard = lock_tolerant(state);
                if guard.last_team.is_none() {
                    guard.last_team = Some(t.clone());
                }
            }
            remembered.or(first_team)
        }
    };
    let today = local_date_today();
    let stored_today = load_team_digest(&base_dir, ws_id, team.as_deref()).is_some_and(|s| s.date == today);
    if digest_due(&schedule, team.as_deref(), stored_today, &today, engine::now_epoch()) {
        let _ = generate_team_digests(app, state, vec![team], false);
    }
}

/// One team's digest inputs, read off the lock: each repo's last day
/// (resident repos through their read connection, dormant ones through a
/// read-only open of their index), and my day's tickets and tasks.
struct TeamDigestJob {
    team: Option<String>,
    repos: Vec<(String, ken_core::digest::DigestSources)>,
    /// The repos' folders, for Claude to read.
    roots: Vec<PathBuf>,
    mine: Vec<String>,
}

fn team_digest_job(state: &SharedState, team: Option<String>) -> CmdResult<TeamDigestJob> {
    let (ws, base_dir, settings) = day_snapshot(state, None)?;
    let repos = team_repos(&ws, team.as_deref());
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
        let sources = match live.get(&r.id) {
            Some(db) => ken_core::digest::gather(&lock_tolerant(db), since).ok(),
            None if db_path(&base_dir, r.id).exists() => {
                Db::open_read_only(&base_dir, r.id).ok().and_then(|db| ken_core::digest::gather(&db, since).ok())
            }
            None => None,
        };
        out.push((r.name.clone(), sources.unwrap_or_default()));
    }

    let today = local_date_today();
    let all = DayHomes::new(&ws, &base_dir, &settings).scan(&today);
    let me = cached_git_me();
    let (tickets, _) = my_tickets(&repos, &me, &all);
    let mut mine: Vec<String> = Vec::new();
    for t in tickets.iter().take(8) {
        mine.push(format!(
            "ticket {} \"{}\" in {}{}{}",
            t.id,
            t.title,
            t.repo,
            if t.state.is_empty() { String::new() } else { format!(", {}", t.state) },
            t.target.as_deref().map(|d| format!(", target {d}")).unwrap_or_default()
        ));
    }
    for t in ken_core::day::today_list(all, &today)
        .iter()
        .filter(|t| t.state == ken_core::day::DayTaskState::Open)
        .take(8)
    {
        mine.push(format!(
            "task \"{}\"{}",
            t.title,
            t.target.as_deref().map(|d| format!(", target {d}")).unwrap_or_default()
        ));
    }
    for (t, _) in pending_inbox_tasks(&base_dir, &day_families(&ws, &settings)).iter().take(5) {
        mine.push(format!("{} sent a task: \"{}\"", t.from.as_deref().unwrap_or("a teammate"), t.title));
    }
    let roots = repos.iter().map(|r| r.root.clone()).collect();
    Ok(TeamDigestJob { team, repos: out, roots, mine })
}

/// Write the digest for each team, one after another, on one background
/// thread: Claude Code runs from the folder that holds the repos so it can
/// read across them. A quiet day stores the quiet line without calling it.
/// Emits `team-digest-generating`, then `team-digest-updated` (the digest)
/// or `team-digest-error` (a string) per team.
/// The teams whose digest is being written or waits its turn, by
/// workspace. The writer takes them in order; asking while it runs adds to
/// the queue, and Your day reads it to show "Writing…" for its team.
fn digest_queue() -> &'static Mutex<std::collections::HashMap<uuid::Uuid, std::collections::VecDeque<Option<String>>>> {
    static Q: OnceLock<Mutex<std::collections::HashMap<uuid::Uuid, std::collections::VecDeque<Option<String>>>>> = OnceLock::new();
    Q.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// Is `team`'s digest being written, or waiting to be, in workspace `ws`?
fn digest_writing(ws: uuid::Uuid, team: &Option<String>) -> bool {
    lock_tolerant(digest_queue()).get(&ws).is_some_and(|q| q.contains(team))
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct TeamDigestEvent {
    team: Option<String>,
    message: Option<String>,
}

fn generate_team_digests(app: &AppHandle, state: &SharedState, teams: Vec<Option<String>>, force: bool) -> CmdResult<()> {
    let (running, ws_root, ws_id, base_dir) = {
        let guard = lock_tolerant(state);
        let ws = guard.workspace.as_ref().ok_or("no workspace open")?;
        (guard.team_digest_running.clone(), ws.ws.root.clone(), ws.ws.config.id, guard.base_dir.clone())
    };
    let binary = ken_core::runner::discover_claude();
    if binary.is_none() && force {
        return Err(ken_core::runner::MISSING_CLAUDE_HELP.into());
    }
    {
        let mut q = lock_tolerant(digest_queue());
        let queue = q.entry(ws_id).or_default();
        for t in teams {
            if !queue.contains(&t) {
                queue.push_back(t.clone());
                let _ = app.emit("team-digest-generating", TeamDigestEvent { team: t, message: None });
            }
        }
    }
    if running.swap(true, Ordering::SeqCst) {
        // The writer that is running takes the new teams too.
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
        let next = || lock_tolerant(digest_queue()).get(&ws_id).and_then(|q| q.front().cloned());
        let done = |team: &Option<String>| {
            if let Some(q) = lock_tolerant(digest_queue()).get_mut(&ws_id) {
                if let Some(i) = q.iter().position(|t| t == team) {
                    q.remove(i);
                }
            }
        };
        loop {
            while let Some(team) = next() {
                write_team_digest(&thread_app, &thread_state, &base_dir, &ws_root, ws_id, binary.as_ref(), team.clone());
                done(&team);
            }
            running.0.store(false, Ordering::SeqCst);
            // A team asked for between the last look and the flag falling.
            if next().is_none() || running.0.swap(true, Ordering::SeqCst) {
                break;
            }
        }
    });
    Ok(())
}

/// One team's digest: stored, or an error event saying why not.
fn write_team_digest(
    thread_app: &AppHandle,
    thread_state: &SharedState,
    base_dir: &Path,
    ws_root: &Path,
    ws_id: uuid::Uuid,
    binary: Option<&PathBuf>,
    team: Option<String>,
) {
    let fail = |message: String| {
        let _ = thread_app.emit("team-digest-error", TeamDigestEvent { team: team.clone(), message: Some(message) });
    };
    {
        {
            let today = local_date_today();
            let key = digest_schedule_key(team.as_deref());
            // Recorded as failed until it is stored, so a failure (or a
            // crash) backs the schedule off instead of retrying each focus.
            let attempt = |ok: bool| {
                let (key, date) = (key.clone(), today.clone());
                update_digest_schedule(&base_dir, ws_id, move |s| {
                    s.attempts.insert(key, TeamDigestAttempt { at: engine::now_epoch(), date, ok });
                });
            };
            attempt(false);
            let job = match team_digest_job(thread_state, team.clone()) {
                Ok(j) => j,
                Err(e) => {
                    fail(e);
                    return;
                }
            };
            let store = |body: String, sources: Vec<String>| {
                let stored = StoredTeamDigest {
                    date: today.clone(),
                    digest: TeamDigestDto { generated_at: engine::now_epoch(), body, sources },
                };
                match save_team_digest(base_dir, ws_id, job.team.as_deref(), &stored) {
                    Ok(()) => {
                        attempt(true);
                        let _ = thread_app.emit("team-digest-updated", stored.digest);
                    }
                    Err(e) => fail(e),
                }
            };
            if ken_core::digest::team_is_quiet(&job.repos, &job.mine) {
                store(ken_core::digest::QUIET_TEAM_DIGEST.to_string(), Vec::new());
                return;
            }
            let Some(binary) = binary else {
                return; // the schedule without Claude Code: nothing to write with
            };
            let name = job.team.clone().unwrap_or_else(|| "this workspace".into());
            let prompt = ken_core::digest::compose_team_digest_prompt(&name, &job.repos, &job.mine);
            // From the folder that holds the repos, when they share one, so
            // a repo's name is its folder; each repo is readable either way.
            let cwd = shared_parent(&job.roots).unwrap_or_else(|| ws_root.to_path_buf());
            match assistant::oneshot_in(binary, &cwd, &job.roots, &prompt, Duration::from_secs(240), &CancelToken::new()) {
                Ok(OneshotOutcome::Completed(text)) => {
                    let parsed = digest::parse_digest(&text);
                    store(parsed.body, parsed.sources);
                }
                Ok(OneshotOutcome::TimedOut) => {
                    fail("Writing the digest took too long and was stopped. It will try again later.".into())
                }
                Ok(OneshotOutcome::Failed(detail)) => fail(detail),
                Err(e) => fail(e.to_string()),
                Ok(OneshotOutcome::Cancelled) => {}
            }
        }
    }
}

// ---------- index health for a team ----------

/// Index health for Your day: how many of the team's repos have an index
/// with files in it, how many files wait to be indexed, and how many files
/// failed to index (`Db::file_health_counts`: counting queries only). The
/// extraction backlog is not part of it.
#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct TeamIndexHealthDto {
    indexed: usize,
    total: usize,
    queued: i64,
    failed: usize,
}

#[tauri::command(async)]
fn index_health(state: State<SharedState>, team: Option<String>) -> CmdResult<TeamIndexHealthDto> {
    let (ws, base_dir, _) = day_snapshot(state.inner(), team.as_deref())?;
    let repos = team_repos(&ws, team.as_deref());
    let live: std::collections::HashMap<uuid::Uuid, Arc<Mutex<Db>>> = {
        let guard = lock_tolerant(state.inner());
        repos
            .iter()
            .filter_map(|r| guard.members.get(&r.id).map(|m| (r.id, m.search_db.clone())))
            .collect()
    };
    let mut out = TeamIndexHealthDto { total: repos.len(), ..Default::default() };
    for r in &repos {
        let counts = match live.get(&r.id) {
            // The index lock is held for the counting query only.
            Some(db) => lock_tolerant(db).file_health_counts().ok(),
            None if db_path(&base_dir, r.id).exists() => {
                Db::open_read_only(&base_dir, r.id).ok().and_then(|db| db.file_health_counts().ok())
            }
            None => None,
        };
        if let Some(c) = counts {
            if c.total > 0 {
                out.indexed += 1;
            }
            out.failed += c.failed.max(0) as usize;
            out.queued += c.queued;
        }
    }
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

/// The folders Your day reads: the workspace home, each member's ticket
/// files (`tickets/` and one folder deeper) and escalations, and my board
/// and inbox in each family. Only paths are taken under the lock; the folder listing
/// happens after. `None` when no workspace is open.
fn day_watch_dirs(state: &SharedState) -> Option<Vec<PathBuf>> {
    let (mut dirs, repo_roots) = {
        let guard = lock_tolerant(state);
        let w = guard.workspace.as_ref()?;
        let mut dirs = vec![tasks::workspace_tasks_dir(&w.ws.root)];
        for c in day_families(&w.ws, &guard.app_settings) {
            let clone_root = family_clone_root(&guard.base_dir, c.family_id);
            dirs.push(family::board_dir(&clone_root, &c.member_id));
            dirs.push(family::inbox_dir(&clone_root, &c.member_id));
        }
        let roots: Vec<PathBuf> = team_repos(&w.ws, None).into_iter().map(|r| r.root).collect();
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
        dirs.push(root.join(ken_core::day::ESCALATIONS_DIR));
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

/// Sibling folders under the workspace root that are NOT yet members —
/// the "add this repo too" list. Mirrors `discover_candidates`' filtering
/// (no dot-folders, no junk dirs) minus everything already joined.
#[tauri::command(async)]
fn workspace_candidates(state: State<SharedState>) -> CmdResult<Vec<CandidateDto>> {
    let guard = state.lock().unwrap();
    if !workspace_enabled(&guard.app_settings) {
        return Err(WORKSPACE_DISABLED_MSG.into());
    }
    let ws = guard.workspace.as_ref().ok_or("no workspace open")?;
    let existing: Vec<String> = ws.ws.config.members.clone();
    let root = ws.ws.root.clone();
    drop(guard);

    // `discover_candidates` already applies the workspace-root
    // `.kenignore`, so nothing extra is filtered here.
    let found = ken_core::workspace::discover_candidates(&root).map_err(err)?;
    Ok(found
        .into_iter()
        // Not already a member, and not a group folder that CONTAINS
        // members — adding "SR" itself while "SR/ShatteredRealms" is a
        // member would nest one project's tree inside another's.
        .filter(|c| {
            !existing
                .iter()
                .any(|m| m == &c.name || m.starts_with(&format!("{}/", c.name)))
        })
        .map(|c| CandidateDto {
            name: c.name,
            existing: c.existing,
            file_count: c.file_count,
            markers: c.markers,
        })
        .collect())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CandidateDto {
    name: String,
    /// Already has `.ken/project.json` — it will be adopted, not created.
    existing: bool,
    file_count: usize,
    markers: Vec<String>,
}

/// Folder names currently excluded by the workspace-root `.kenignore`,
/// for the UI's "not projects" list. Derived by testing each sibling
/// folder against the rules rather than by parsing lines back out, so a
/// hand-written glob (`sr-universe-*/`) reports every folder it hides.
#[tauri::command(async)]
fn workspace_ignored(state: State<SharedState>) -> CmdResult<Vec<String>> {
    let guard = state.lock().unwrap();
    if !workspace_enabled(&guard.app_settings) {
        return Err(WORKSPACE_DISABLED_MSG.into());
    }
    let root = guard.workspace.as_ref().ok_or("no workspace open")?.ws.root.clone();
    drop(guard);

    let rules = ken_core::workspace::ignore_rules(&root);
    if rules.is_empty() {
        return Ok(Vec::new());
    }
    // Two levels, matching membership's D6 shape: a rule can hide a direct
    // child ("worlds/") or something inside a group folder
    // ("SR/scratch/"). Rows are parent-relative paths; an ignored
    // top-level folder reports alone — descending into it would list every
    // child as redundantly ignored via parent matching.
    let mut out: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(&root).map_err(err)?.filter_map(|e| e.ok()) {
        if !entry.path().is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        if ken_core::workspace::is_ignored_folder(&rules, &name) {
            out.push(name);
            continue;
        }
        for child in std::fs::read_dir(entry.path())
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
        {
            if !child.path().is_dir() {
                continue;
            }
            let Some(child_name) = child.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if child_name.starts_with('.') {
                continue;
            }
            let rel = format!("{name}/{child_name}");
            if ken_core::workspace::is_ignored_folder(&rules, &rel) {
                out.push(rel);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Exclude a sibling folder by appending a rule to the workspace-root
/// `.kenignore` — the same file, syntax, and matcher a project's own
/// `.kenignore` uses, just governing the parent folder. Hand-editing it
/// works exactly as well as this command.
#[tauri::command]
fn workspace_ignore_candidate(state: State<SharedState>, folder: String) -> CmdResult<Vec<String>> {
    let root = {
        let guard = state.lock().unwrap();
        if !workspace_enabled(&guard.app_settings) {
            return Err(WORKSPACE_DISABLED_MSG.into());
        }
        guard.workspace.as_ref().ok_or("no workspace open")?.ws.root.clone()
    };
    ken_core::workspace::ignore_folder(&root, &folder).map_err(err)?;
    workspace_ignored(state)
}

#[tauri::command]
fn workspace_unignore_candidate(state: State<SharedState>, folder: String) -> CmdResult<Vec<String>> {
    let root = {
        let guard = state.lock().unwrap();
        if !workspace_enabled(&guard.app_settings) {
            return Err(WORKSPACE_DISABLED_MSG.into());
        }
        guard.workspace.as_ref().ok_or("no workspace open")?.ws.root.clone()
    };
    // False here means the folder is hidden by a hand-written glob rather
    // than a line we wrote — say so instead of silently doing nothing.
    if !ken_core::workspace::unignore_folder(&root, &folder).map_err(err)? {
        return Err(format!(
            "\"{folder}\" is excluded by a pattern in .kenignore — edit that file to change it"
        ));
    }
    workspace_ignored(state)
}

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
    if !workspace_enabled(&guard.app_settings) {
        return Err(WORKSPACE_DISABLED_MSG.into());
    }
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

/// What removing a member did: the fresh roster, and the team wiki whose
/// pages still cite it (listed on the Team screen).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RemovedMemberDto {
    members: Vec<MemberOverviewDto>,
    /// The wiki whose pages cite it, and how many.
    wiki: Option<String>,
    citing_pages: usize,
}

/// Take a repo out of the open workspace: out of the manifest and its
/// groups, its runtime closed. The folder, its files and its index stay; the
/// repo stays in Ken's list. When the team it was on has a wiki, the Team
/// screen lists the wiki's pages that still cite it.
#[tauri::command(async)]
fn workspace_remove_member(state: State<SharedState>, name: String) -> CmdResult<RemovedMemberDto> {
    let (wiki, citing_pages) = {
        let mut guard = state.lock().unwrap();
        if !workspace_enabled(&guard.app_settings) {
            return Err(WORKSPACE_DISABLED_MSG.into());
        }
        // The team's wiki, found before the member leaves the roster.
        let team = wiki_team_members(&guard)?;
        let wiki = ken_core::wikidraft::wiki_for(&name, &team).map(|w| w.name.clone());
        let wiki_target = wiki.as_deref().and_then(|w| wiki_target(&guard, w).ok());
        let base = guard.base_dir.clone();
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
        let mut pages = 0;
        if let Some((_, wiki_id)) = wiki_target {
            if let Ok(mut db) = Db::open(&base, wiki_id) {
                let repo = ken_core::workspace::member_leaf(&name);
                pages = ken_core::wikidraft::record_removed_repo(&mut db, repo).map_err(err)?.unwrap_or(0);
            }
        }
        (wiki.filter(|_| pages > 0), pages)
    };
    Ok(RemovedMemberDto { members: workspace_members_overview(state)?, wiki, citing_pages })
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
    if !workspace_enabled(&guard.app_settings) {
        return Err(WORKSPACE_DISABLED_MSG.into());
    }
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

/// Create or replace a group, then persist the manifest.
#[tauri::command(async)]
fn workspace_set_group(
    state: State<SharedState>,
    name: String,
    members: Vec<String>,
) -> CmdResult<Vec<ProjectGroupDto>> {
    let mut guard = state.lock().unwrap();
    if !workspace_enabled(&guard.app_settings) {
        return Err(WORKSPACE_DISABLED_MSG.into());
    }
    let ws = guard.workspace.as_mut().ok_or("no workspace open")?;
    ws.ws.config.set_group(&name, &members).map_err(err)?;
    ws.ws.save().map_err(err)?;
    Ok(group_dtos(&ws.ws))
}

#[tauri::command(async)]
fn workspace_remove_group(state: State<SharedState>, name: String) -> CmdResult<Vec<ProjectGroupDto>> {
    let mut guard = state.lock().unwrap();
    if !workspace_enabled(&guard.app_settings) {
        return Err(WORKSPACE_DISABLED_MSG.into());
    }
    let ws = guard.workspace.as_mut().ok_or("no workspace open")?;
    if ws.ws.config.remove_group(&name) {
        ws.ws.save().map_err(err)?;
    }
    Ok(group_dtos(&ws.ws))
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
        if !workspace_enabled(&guard.app_settings) {
            return Err(WORKSPACE_DISABLED_MSG.into());
        }
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

/// The incremental-Map worker: one per open project. Loops draining the
/// extraction queue while the local model is ready, emitting a throttled
/// `knowledge-updated` after each merged file. Every wait is short so a newly
/// indexed file is picked up promptly; the model's own queue (background
/// priority) yields to interactive quick answers upstream, and the worker also
/// steps aside between files while a quick answer is in flight. When the local
/// model isn't ready — no model installed, a load error — the worker idles
/// quietly (polling with a sleep), never erroring rows and never spinning CPU.
/// Minimum gap between two background extraction generations, counted ACROSS
/// EVERY OPEN PROJECT rather than per worker.
///
/// It has to be global, and that is the whole point. Local inference is a
/// single worker thread behind a shared two-priority queue (`local_llm`), so
/// N extraction workers never run N generations at once — they queue on the
/// one engine. A per-worker `sleep` therefore does not bound GPU duty cycle at
/// all: with ten projects open, ten independent gaps overlap and the queue is
/// never empty, which is exactly the state measured on 2026-09-03 when an
/// unpaced backlog held the A4000 at 94% / 114 W / 75 °C indefinitely. One
/// shared "not before" instant bounds the aggregate rate no matter how many
/// projects are open.
///
/// Two seconds against a background generation of roughly the same order
/// leaves the card idle about half the time. Raise it to be gentler; the
/// `backgroundExtraction` flag turns the work off outright.
const EXTRACT_PACING: Duration = Duration::from_secs(2);

/// The shared "not before" instant every extraction worker paces against.
fn extraction_gate() -> &'static Mutex<Instant> {
    static GATE: OnceLock<Mutex<Instant>> = OnceLock::new();
    GATE.get_or_init(|| Mutex::new(Instant::now()))
}

/// Block until the shared extraction slot opens. `false` means the worker was
/// asked to stop while waiting and should exit rather than generate.
fn wait_for_extraction_slot(stop: &Arc<AtomicBool>) -> bool {
    loop {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        let now = Instant::now();
        let until = *extraction_gate().lock().unwrap();
        if until <= now {
            return true;
        }
        // Sleep in short chunks so closing a project or quitting the app is
        // still responsive while a long pace is being served.
        std::thread::sleep((until - now).min(Duration::from_millis(250)));
    }
}

/// Close the shared slot for `EXTRACT_PACING`. Called only after a generation
/// actually happened: an idle worker finding an empty queue must not consume
/// the pace, or nine idle projects would starve the one with work to do.
fn stamp_extraction_slot() {
    *extraction_gate().lock().unwrap() = Instant::now() + EXTRACT_PACING;
}

/// How often the extraction worker asks whether a map build is due.
const MAP_CHECK: Duration = Duration::from_secs(15);

/// Build the project's map with Claude when the automatic-build policy says
/// it is due: Claude Code installed, background extraction on, something to
/// map, and either no map yet or changes since the last one, settled (see
/// `knowledge_model::should_auto_build`). True while a Claude build of this
/// project runs, started here or by hand, so the caller holds off.
fn map_by_claude(app: &AppHandle, state: &SharedState, project_id: uuid::Uuid) -> bool {
    let (base, project, running, tracker) = {
        let guard = lock_tolerant(state);
        let Some(active) = guard.members.get(&project_id) else {
            return false;
        };
        if active.knowledge_running.load(Ordering::SeqCst) {
            return true;
        }
        if !ken_core::features::effective_flag(&guard.app_settings, &active.project, "backgroundExtraction") {
            return false;
        }
        (guard.base_dir.clone(), active.project.clone(), active.knowledge_running.clone(), active.auto_knowledge.clone())
    };
    // Looking for Claude reads the disk: with the lock released, since this
    // runs every few seconds for every repo.
    let Some(binary) = ken_core::runner::discover_claude() else {
        return false;
    };
    let job = KnowledgeBuild { base, project, binary, running, tracker, quiet_failure: true, state: state.clone() };
    let Ok(db) = Db::open_read_only(&job.base, project_id) else {
        return false;
    };
    let never_built = db.knowledge_model_built_at().ok().flatten().is_none();
    let (_, total) = db.extraction_coverage().unwrap_or((0, 0));
    drop(db);
    let ctx = knowledge_model::AutoBuildContext {
        claude_available: true,
        in_flight: false,
        indexed_files: usize::try_from(total).unwrap_or(0),
        never_built,
    };
    if !job.tracker.should_build(ctx, Instant::now()) {
        return false;
    }
    start_knowledge_build(app, job)
}

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
    // Tracks the model's not-ready → ready edge so we retry files that errored
    // while the model was faulty (e.g. a bad load, or the batch-overflow bug)
    // exactly once per recovery — no content edit required.
    let mut was_ready = false;
    let mut requeue_on_next_open = false;
    let mut last_map_check = Instant::now() - MAP_CHECK;
    while !stop.load(Ordering::SeqCst) {
        // The map comes from Claude: one read of the whole project for the
        // first map, and again once changes have settled (the automatic-build
        // policy: a quiet window, at most two builds an hour). Minutes, where
        // the local model took hours file by file and failed half of them.
        if last_map_check.elapsed() >= MAP_CHECK {
            last_map_check = Instant::now();
            if map_by_claude(&app, &state, project_id) {
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
        }
        // The local, per-file path: off by policy (LOCAL_GENERATION), kept
        // for a build that turns it back on. Pause quietly unless it is on
        // and the local model is ready.
        let ready = LOCAL_GENERATION
            && matches!(ken_core::local_llm::llm_status(), ken_core::local_llm::LlmStatus::Ready);
        if ready && !was_ready {
            // Just became ready (model installed / selection changed / error
            // acknowledged): return any errored rows to pending on the next
            // time we hold the DB connection.
            requeue_on_next_open = true;
        }
        was_ready = ready;
        if !ready {
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
        // Resolve base + project root/profiler-flag under the lock, then drop
        // it before the (slow) generation so IPC stays responsive.
        // Look up THIS worker's own project by id.
        //
        // This used to read `members.values().next()` — an arbitrary entry of
        // a HashMap — and `return` unless that entry happened to be this
        // worker's project. With more than one member open it killed every
        // worker but the lucky one, and HashMap order is arbitrary (randomized
        // per process), so which project survived was chance rather than
        // design. Measured 2026-09-08: nine of ten projects had never
        // extracted a single file, 25,859 rows stuck at `pending`, while the
        // tenth held 77 done / 355 error. A worker that knows its own id must
        // ask for its own id.
        // The roster comes along so a teammate whose name reads like a bot's
        // is never dropped from the map.
        let (base, profiler_enabled, project_root, extraction_on, roster) = {
            let guard = state.lock().unwrap();
            let Some(active) = guard.members.get(&project_id) else {
                return; // project closed — this worker is done
            };
            (
                guard.base_dir.clone(),
                ken_core::features::effective_flag(&guard.app_settings, &active.project, "profiler"),
                active.project.root.clone(),
                ken_core::features::effective_flag(
                    &guard.app_settings,
                    &active.project,
                    "backgroundExtraction",
                ),
                workspace_roster(&guard),
            )
        };
        // Switched off: idle but stay alive, so turning it back on resumes
        // without having to reopen the project.
        if !extraction_on {
            std::thread::sleep(Duration::from_secs(2));
            continue;
        }
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
        // First tick after the model became ready: rescue errored rows so the
        // Map recovers files that failed under a transient model fault.
        if requeue_on_next_open {
            let _ = db.requeue_errored_extractions();
            requeue_on_next_open = false;
        }
        let today = local_date_today();
        let at = engine::now_epoch();
        let generate = |prompt: &str| {
            ken_core::local_llm::generate_json(prompt, ken_core::local_llm::Priority::Background)
        };
        // project-profiler task 2.3: append the stored profile's
        // summary/focus-hints addendum to the extraction prompt when the
        // `profiler` flag is on and a profile exists. Empty string when off
        // or absent, which reproduces `process_next_pending`'s plain prompt
        // exactly (ken-core task 1.5's flag-off inertness guarantee) — a
        // fresh, cheap read per file rather than caching, since a profile
        // can be regenerated ("Re-analyze") while this worker is running.
        let addendum = if profiler_enabled {
            ken_core::profiler::profile_prompt_addendum(&ken_core::profiler::ProjectProfile::load(&project_root))
        } else {
            String::new()
        };
        // Take the shared pace before spending a generation on the one engine.
        if !wait_for_extraction_slot(&stop) {
            return;
        }
        match knowledge_model::process_next_pending_for(db, &today, at, &generate, &addendum, &roster) {
            Ok(Some(_)) => {
                // A generation actually ran: close the slot behind us.
                stamp_extraction_slot();
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
                // Self-heal: return errored rows that still have retries left to
                // `pending` before sleeping, so a file that failed under a
                // transient fault is retried on the next tick without needing a
                // model restart. Rows that have exhausted the attempt cap stay
                // `error` and are NOT re-queued, so this can't spin forever.
                let _ = db.requeue_errored_extractions();
                std::thread::sleep(Duration::from_secs(2));
            }
            Err(_) => {
                // A generation failed (already recorded on the row, which is now
                // `error` and won't be re-popped). Continue the loop past it —
                // a brief backoff so a bad model state doesn't spin. A failed
                // generation still burned GPU, so it takes the shared pace too:
                // the per-worker sleep below only bounds ONE worker, and a
                // whole workspace erroring in lockstep is exactly the burst
                // the global gate exists to stop.
                stamp_extraction_slot();
                std::thread::sleep(Duration::from_secs(2));
            }
        }
    }
}

/// Cap on how many pages of a scanned PDF are OCR'd. Rasterizing + recognizing
/// every page is expensive; the first pages carry the searchable gist, and the
/// cap bounds the cost of a pathological 500-page scan.
const MAX_OCR_PAGES: usize = 50;

/// One background OCR worker per open project, mirroring `extraction_worker` but
/// independent of the local LLM and the knowledge-model coverage accounting. It
/// drains `ocr_pending`: reads each queued file's bytes, runs the OS's OCR
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
        // Own project by id, not an arbitrary map entry — same defect, and the
        // same fix, as `extraction_worker` above: picking `values().next()`
        // and bailing unless it matched meant only one project's OCR ever ran.
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
                emit_member(&app, project_id, "index-updated", ScanStats::default());
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
        let result = if is_pdf(&rel) {
            ken_core::ocr::ocr_pdf_bytes(&bytes, MAX_OCR_PAGES)
        } else {
            ken_core::ocr::ocr_image_bytes(&bytes)
        };
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
                        emit_member(&app, project_id, "index-updated", ScanStats::default());
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
        emit_member(&app, project_id, "index-updated", ScanStats { changed_paths: vec![rel], ..Default::default() });
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

// ---------------------------------------------------------------------
// ken-families tasks 2.1-2.6: connection store, clone/create/join, poll
// scheduler, accept/dismiss, attach-to-workspace. Always available; the
// inbox shows once a connection exists.
// ---------------------------------------------------------------------

/// One family connection (task 2.1): remote URL, which manifest member this
/// device is, the live-sync toggle, poll interval, and an optional attached
/// workspace.
///
/// **Storage judgment call**: `AppSettings` (`crates/ken-core/src/
/// settings.rs`) has exactly one structured field (`features`) plus a
/// flattened `extra` forward-compat map — there is no typed home for a list
/// of connections, and adding one means editing `settings.rs`, which is
/// outside this task's touch-boundary (owned by another session). So
/// connections live wholesale as one JSON array under `AppSettings::extra
/// ["familyConnections"]` (`family_connections`/`save_family_connections`
/// below) — riding the SAME channel `extra`'s own doc comment describes
/// ("forward-compat passthrough for keys this build doesn't know about"),
/// just repurposed as *this* build's actual storage for a key `settings.rs`
/// itself never models. This is honest, not a hack: `extra` round-trips
/// unknown keys byte-for-byte specifically so a capability like this one can
/// use it as a durable home without waiting on a `settings.rs` change.
/// Trade-off recorded: reads/writes are all-or-nothing (`serde_json::from_
/// value::<Vec<FamilyConnection>>` fails wholesale on any one malformed
/// entry, unlike a corrupt individual inbox item which only ever affects
/// itself) — accepted because this file is Ken-written only, never hand-
/// edited or shared with a teammate, so the corruption surface is far
/// smaller than the family repo's own per-file tolerance guarantees.
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct FamilyConnection {
    family_id: uuid::Uuid,
    /// The manifest's own `name`, cached here so the settings page and the
    /// inbox don't need a manifest read just to label a
    /// connection; `family_manifest_get` is still the source of truth for
    /// the member roster.
    name: String,
    remote_url: String,
    /// Which manifest member id THIS device is (D5).
    member_id: String,
    #[serde(default)]
    live_sync: bool,
    #[serde(default = "default_family_poll_interval_secs")]
    poll_interval_secs: u64,
    #[serde(default)]
    attached_workspace_id: Option<uuid::Uuid>,
}

/// Design.md D1: "every `poll_interval` (default 120s)".
fn default_family_poll_interval_secs() -> u64 {
    120
}

/// Design.md D1 open question: "Poll interval default: 120s (bounds
/// 30s–30min)".
const FAMILY_POLL_INTERVAL_MIN_SECS: u64 = 30;
const FAMILY_POLL_INTERVAL_MAX_SECS: u64 = 1800;

/// Read every saved connection from `AppSettings::extra["familyConnections"]`
/// — see `FamilyConnection`'s doc comment for why this key, not a typed
/// field, is the storage channel. Missing key or malformed JSON reads as no
/// connections (same "corrupt file loads as defaults" tolerance `AppSettings
/// ::load` itself uses), never a panic or an error the caller has to handle.
fn family_connections(app_settings: &ken_core::settings::AppSettings) -> Vec<FamilyConnection> {
    app_settings
        .extra
        .get("familyConnections")
        .and_then(|v| serde_json::from_value::<Vec<FamilyConnection>>(v.clone()).ok())
        .unwrap_or_default()
}

/// Persist the full connection list and refresh `AppState::app_settings` to
/// match — same "write succeeds, then update the in-memory copy" ordering
/// `set_global_feature` uses, so a failed save never leaves disk and memory
/// disagreeing.
fn save_family_connections(state: &SharedState, connections: Vec<FamilyConnection>) -> CmdResult<()> {
    let (base_dir, mut settings) = {
        let guard = state.lock().unwrap();
        (guard.base_dir.clone(), guard.app_settings.clone())
    };
    let value = serde_json::to_value(&connections).map_err(err)?;
    settings.extra.insert("familyConnections".to_string(), value);
    settings.save(&base_dir).map_err(err)?;
    state.lock().unwrap().app_settings = settings;
    Ok(())
}

fn find_family_connection(
    app_settings: &ken_core::settings::AppSettings,
    family_id: uuid::Uuid,
) -> CmdResult<FamilyConnection> {
    family_connections(app_settings)
        .into_iter()
        .find(|c| c.family_id == family_id)
        .ok_or_else(|| format!("no family connection '{family_id}'"))
}

/// `<app data>/ken/families/<family-id>/` (design D1) — `base_dir` here IS
/// `<app data>/ken` already (`ken_core::registry::default_base_dir` joins
/// `"ken"` onto the OS data dir), so this is just one more join.
fn family_clone_root(base_dir: &Path, family_id: uuid::Uuid) -> PathBuf {
    base_dir.join("families").join(family_id.to_string())
}

/// Run `git` directly for the one-time steps that happen *before* a
/// `family_sync::SystemGit` transport exists — `git init`/`git clone`/`git
/// remote add` (task 2.2). `family_sync.rs` deliberately has no seam for
/// these: D1 pins `SystemGit` to driving an *already-cloned* working tree,
/// so getting that tree onto disk in the first place is this task's own
/// small helper — using the same prompt-suppression env vars `family_sync::
/// SystemGit::run` uses (S8 Q4) so a spawned `git` never hangs on a
/// credential prompt here either.
///
/// Unlike `family_sync`'s own (private) `short_detail` — a settings-page
/// summary line, trimmed and capped at 400 chars — this surfaces stderr
/// **verbatim** (task 2.2: "surfacing git stderr verbatim on failure"): an
/// auth failure or a "repository not found" during create/join is exactly
/// the message the user needs to read in full, not a summary.
fn family_git(dir: Option<&Path>, args: &[&str]) -> Result<String, String> {
    let mut cmd = std::process::Command::new("git");
    ken_core::proc::quiet(&mut cmd);
    if let Some(d) = dir {
        cmd.current_dir(d);
    }
    let out = cmd
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.trim().is_empty() {
            Err(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(stderr.into_owned())
        }
    }
}

/// A fresh `SyncEngine`'s starting state for a clone already on disk (task
/// 2.3): `Unavailable` when `git` itself is missing, or when the manifest
/// declares a `template` newer than this Ken supports (spec: "needs a newer
/// Ken... no sync, ingest, or write runs against the clone"); `Idle`
/// otherwise. Called once per family id, the first time anything asks for
/// its engine (`family_engine_handle`) — including after a process restart,
/// since `AppState::family_engines` starts empty every run.
fn family_engine_initial_state(clone_root: &Path) -> family_sync::SyncEngine {
    if let Err(reason) = family_sync::git_available() {
        return family_sync::SyncEngine::unavailable(reason);
    }
    match FamilyManifest::load(clone_root) {
        Ok(m) => match m.check_supported() {
            Ok(()) => family_sync::SyncEngine::new(),
            Err(e) => family_sync::SyncEngine::unavailable(e.to_string()),
        },
        Err(e) => family_sync::SyncEngine::unavailable(err(e)),
    }
}

/// The one `SyncEngine` instance for a family id, created on first touch and
/// shared by the poll loop and every on-demand command thereafter — see
/// `AppState::family_engines`'s doc comment for why a fresh engine per call
/// would be wrong (it would lose `Conflict`/`Unavailable` between ticks).
fn family_engine_handle(
    state: &SharedState,
    family_id: uuid::Uuid,
    clone_root: &Path,
) -> Arc<Mutex<family_sync::SyncEngine>> {
    let engines = { state.lock().unwrap().family_engines.clone() };
    let mut map = engines.lock().unwrap();
    map.entry(family_id)
        .or_insert_with(|| Arc::new(Mutex::new(family_engine_initial_state(clone_root))))
        .clone()
}

/// Non-recursive `.md` listing of one inbox directory, sorted by filename,
/// each parsed via `family::parse_inbox_item` (which never fails — a
/// malformed file just comes back with `malformed: true`, per D4). Plain
/// `std::fs` reads, not `GitTransport::read`: listing/parsing is read-only
/// and never dirties the working tree, so it doesn't need a transport
/// instance (which also does a remote/branch probe on construction) at all
/// — only writes go through `GitTransport` in this file.
fn list_inbox_items(dir: &Path) -> Vec<(PathBuf, family::InboxItem)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "md"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .filter_map(|path| {
            let raw = std::fs::read_to_string(&path).ok()?;
            let file_name = path.file_name()?.to_string_lossy().to_string();
            Some((path.clone(), family::parse_inbox_item(&file_name, &raw)))
        })
        .collect()
}

/// `created`/`updated` timestamps for inbox items (D4: "iso datetime"),
/// unlike tasks' plain `YYYY-MM-DD` `local_date_today` — the first place in
/// this codebase that needs a full timestamp rather than a date.
fn iso_datetime_now() -> String {
    chrono::Local::now().to_rfc3339()
}

/// One connection's resolved state, as the (future) Families settings page
/// reads it: the saved settings half plus the live `SyncEngine` state.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct FamilyConnectionDto {
    connection: FamilyConnection,
    state: family_sync::ConnectionState,
}

/// The `family-sync` app event (task 2.3: "emits events for new inbox
/// items, board changes, sync errors"), emitted app-global after every poll
/// tick and every on-demand command that touches the transport — mirrors
/// `day-changed`'s own "app-global, no single owning project" choice.
/// `unread_inbox_count` is the mechanism for "new inbox items": rather than
/// a stateful tick-to-tick diff (out of scope for this task's poll-loop
/// shape), the frontend diffs successive counts itself — simple, and
/// correct for the common case (a) since the tray never removes items
/// except via this device's own accept/dismiss actions, which already know
/// what changed locally. "Board changes" for this device's OWN board are
/// covered by `emit_board_state` (called alongside this event whenever a
/// workspace has this connection attached), since board sync only ever
/// affects this device's own board file in the rare cross-device case (D1:
/// "one clone per device").
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct FamilySyncEvent {
    family_id: uuid::Uuid,
    report: family_sync::SyncReport,
    unread_inbox_count: usize,
}

fn emit_family_sync(
    app: &AppHandle,
    family_id: uuid::Uuid,
    report: &family_sync::SyncReport,
    clone_root: &Path,
    member_id: &str,
) {
    let unread = list_inbox_items(&family::inbox_dir(clone_root, member_id))
        .into_iter()
        .filter(|(_, item)| item.status == Some(family::InboxStatus::Unread))
        .count();
    let _ = app.emit(
        "family-sync",
        FamilySyncEvent { family_id, report: report.clone(), unread_inbox_count: unread },
    );
}

/// Per-connection poll timer (task 2.3), mirroring `spawn_task_board_watch`'s
/// one-thread-per-resource shape. Sleeps in short slices so a stop request
/// (dropping the `StopOnDrop` in `AppState::family_pollers`) lands within a
/// fraction of a second rather than after a full (up to 30-minute) interval.
fn spawn_family_poll(
    app: AppHandle,
    state: SharedState,
    family_id: uuid::Uuid,
    clone_root: PathBuf,
    engine: Arc<Mutex<family_sync::SyncEngine>>,
    interval: Duration,
) -> StopOnDrop {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    std::thread::spawn(move || {
        let slice = Duration::from_millis(500);
        'outer: loop {
            let mut waited = Duration::ZERO;
            while waited < interval {
                if stop_thread.load(Ordering::SeqCst) {
                    break 'outer;
                }
                std::thread::sleep(slice);
                waited += slice;
            }
            let member_id = {
                let guard = state.lock().unwrap();
                family_connections(&guard.app_settings)
                    .into_iter()
                    .find(|c| c.family_id == family_id)
                    .map(|c| c.member_id)
            };
            let Some(member_id) = member_id else {
                // The connection was removed since this poller was started;
                // `reconcile_family_pollers` will drop this thread's
                // `StopOnDrop` shortly, but there's no reason to keep
                // polling a clone nobody's tracking anymore.
                break;
            };
            let Ok(mut transport) = SystemGit::open(&clone_root) else { continue };
            let report = { engine.lock().unwrap().poll(&mut transport) };
            emit_family_sync(&app, family_id, &report, &clone_root, &member_id);
            emit_day_changed(&app, &state);
        }
    });
    StopOnDrop(stop)
}

/// Reconcile the running per-connection poll timers (task 2.3) against the
/// connections and each one's own `liveSync` toggle. Called after every
/// mutation that could change either — `family_create`/`family_join`, `family_remove`, `family_set_
/// live_sync`, `family_set_poll_interval` — rather than threading
/// incremental start/stop calls through each call site individually.
fn reconcile_family_pollers(app: &AppHandle, state: &SharedState) {
    let (base_dir, settings, pollers) = {
        let guard = state.lock().unwrap();
        (guard.base_dir.clone(), guard.app_settings.clone(), guard.family_pollers.clone())
    };
    let wanted: Vec<FamilyConnection> = family_connections(&settings).into_iter().filter(|c| c.live_sync).collect();
    let wanted_ids: std::collections::HashSet<uuid::Uuid> = wanted.iter().map(|c| c.family_id).collect();
    let mut map = pollers.lock().unwrap();
    // Dropping a `StopOnDrop` here stops its thread — no separate teardown
    // call needed, same as `WorkspaceState::task_watch`.
    map.retain(|id, _| wanted_ids.contains(id));
    for conn in wanted {
        if map.contains_key(&conn.family_id) {
            continue;
        }
        let clone_root = family_clone_root(&base_dir, conn.family_id);
        let engine = family_engine_handle(state, conn.family_id, &clone_root);
        let secs = conn.poll_interval_secs.clamp(FAMILY_POLL_INTERVAL_MIN_SECS, FAMILY_POLL_INTERVAL_MAX_SECS);
        let stop =
            spawn_family_poll(app.clone(), state.clone(), conn.family_id, clone_root, engine, Duration::from_secs(secs));
        map.insert(conn.family_id, stop);
    }
}

/// Ingest a family clone as a `kind: family` search member (task 2.5, D6):
/// a real `Project` rooted at the clone directory itself, project id = the
/// manifest id (so `ken://<family-id>/<rel-path>` addressing falls out of
/// the existing per-project addressing for free), with the family-scoped
/// tier rules (1.6) written out as a generated `.kenignore` through the SAME
/// disk-read channel `activate_memory_pseudo_member` established
/// (`render_builtin_kenignore` -> `Project::kenignore_rules()` ->
/// `scan::scan`). Required here because `family::family_builtin_rules()` is
/// deliberately NOT folded into `kenignore::built_in_rule_sets()` (1.6's own
/// note: that hook is parameterless and would leak `members/**`/
/// `shared/**` tiers onto every ordinary project). The generated file is
/// never staged by any `PendingWrite`, so it's never committed — `write_
/// and_commit` only ever `git add`s the exact paths it's given (never
/// `-A`), matching the "no-human repo" guarantee.
///
/// Judgment call, same one `activate_memory_pseudo_member`'s doc comment
/// records: this reuses `activate()` unchanged rather than a bespoke slim
/// spin-up path, for the same reason (it's the one function that wires a
/// complete, correctly-wired `MemberRuntime`). Same accepted side effects
/// too (a harmless chat drawer nobody opens; a `Registry` entry).
fn activate_family_pseudo_member(
    app: &AppHandle,
    state: &SharedState,
    conn: &FamilyConnection,
) -> CmdResult<()> {
    let base_dir = { state.lock().unwrap().base_dir.clone() };
    let clone_root = family_clone_root(&base_dir, conn.family_id);
    let manifest = FamilyManifest::load(&clone_root).map_err(err)?;

    let project = if ken_core::project::config_path(&clone_root).exists() {
        Project::open(&clone_root).map_err(err)?
    } else {
        let config = ken_core::project::ProjectConfig {
            name: format!("Family: {}", manifest.name),
            id: conn.family_id,
            excluded: Vec::new(),
            features: serde_json::Map::new(),
            extra: serde_json::Map::new(),
        };
        let project = Project { root: clone_root.clone(), config };
        project.save().map_err(err)?;
        project
    };

    let kenignore_path = clone_root.join(".kenignore");
    let text = render_builtin_kenignore(&family::family_builtin_rules());
    if let Err(e) = std::fs::write(&kenignore_path, text) {
        eprintln!("warning: failed to write family pseudo-member .kenignore: {e}");
    }

    activate(app, state, project, false)?;
    Ok(())
}

/// Create a new family repo (task 2.2): scaffold the template (1.7) and
/// commit + push it into a brand-new local repo whose remote is
/// `remote_url` — an empty repo the user already created on their git host
/// (this command only ever creates the local clone and its first commit/
/// push; design.md draws the hosting line at "whatever remote the team
/// already uses works"). The creating user becomes the family's first —
/// and therefore owner (D3: "the manifest's first member") — member.
#[tauri::command(async)]
fn family_create(
    state: State<SharedState>,
    name: String,
    member_name: String,
    remote_url: String,
) -> CmdResult<FamilyConnectionDto> {
    let (base_dir, settings) = {
        let guard = state.lock().unwrap();
        (guard.base_dir.clone(), guard.app_settings.clone())
    };
    if let Err(reason) = family_sync::git_available() {
        return Err(format!("git is unavailable: {reason}"));
    }
    let remote_url = remote_url.trim().to_string();
    if remote_url.is_empty() {
        return Err("a family needs a remote URL — create an empty repo on your git host first".into());
    }
    let member_id = family::normalize_member_id(&member_name).map_err(err)?;
    let owner = FamilyMember::new(member_id.clone(), member_name.trim());
    let family_id = uuid::Uuid::new_v4();
    let members = vec![owner];
    let files = family::scaffold_family(&name, family_id, &members).map_err(err)?;

    let clone_root = family_clone_root(&base_dir, family_id);
    std::fs::create_dir_all(&clone_root).map_err(err)?;
    let clone_root_str = clone_root.to_string_lossy().to_string();
    let cfg = family_sync::clone_config_args();
    let mut init_args: Vec<&str> = cfg.iter().map(String::as_str).collect();
    init_args.extend(["init", "--initial-branch=main", &clone_root_str]);
    family_git(None, &init_args)?;
    family_git(Some(&clone_root), &["remote", "add", "origin", &remote_url])?;

    let mut transport = SystemGit::new(&clone_root, "origin", "main");
    transport.configure_clone().map_err(err)?;
    let writes: Vec<PendingWrite> =
        files.into_iter().map(|f| PendingWrite::new(f.rel_path, f.content)).collect();
    transport.commit_paths(&Lane::bootstrap(), &writes, "Create family").map_err(err)?;
    match transport.push().map_err(err)? {
        family_sync::PushOutcome::Failed { detail } => return Err(detail),
        family_sync::PushOutcome::NonFastForward { detail } => {
            return Err(format!("push rejected: {detail}"))
        }
        _ => {}
    }

    let connection = FamilyConnection {
        family_id,
        name: name.trim().to_string(),
        remote_url,
        member_id,
        live_sync: false,
        poll_interval_secs: default_family_poll_interval_secs(),
        attached_workspace_id: None,
    };
    let mut connections = family_connections(&settings);
    connections.push(connection.clone());
    save_family_connections(&state, connections)?;
    let engine = family_engine_handle(&state, family_id, &clone_root);
    let engine_state = engine.lock().unwrap().state().clone();
    Ok(FamilyConnectionDto { connection, state: engine_state })
}

/// Clone an existing family and either select an already-listed member or
/// append a new one (task 2.2, D2 "join appends, never rewrites"). The clone
/// lands in a temp folder first — the target path is keyed by the manifest's
/// own family id (D1: one clone per device), which isn't known until after
/// the clone completes and `family.json` is read — then moves into place.
#[tauri::command(async)]
fn family_join(
    state: State<SharedState>,
    remote_url: String,
    existing_member_id: Option<String>,
    new_member_name: Option<String>,
) -> CmdResult<FamilyConnectionDto> {
    let (base_dir, settings) = {
        let guard = state.lock().unwrap();
        (guard.base_dir.clone(), guard.app_settings.clone())
    };
    if let Err(reason) = family_sync::git_available() {
        return Err(format!("git is unavailable: {reason}"));
    }
    let remote_url = remote_url.trim().to_string();
    if remote_url.is_empty() {
        return Err("a remote URL is required to join a family".into());
    }

    let families_dir = base_dir.join("families");
    std::fs::create_dir_all(&families_dir).map_err(err)?;
    let temp_dir = families_dir.join(format!("_joining-{}", uuid::Uuid::new_v4()));
    let temp_str = temp_dir.to_string_lossy().to_string();
    let cfg = family_sync::clone_config_args();
    let mut clone_args: Vec<&str> = cfg.iter().map(String::as_str).collect();
    clone_args.extend(["clone", "--", &remote_url, &temp_str]);
    if let Err(e) = family_git(None, &clone_args) {
        let _ = std::fs::remove_dir_all(&temp_dir);
        return Err(e);
    }

    let manifest = match FamilyManifest::load(&temp_dir) {
        Ok(m) => m,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&temp_dir);
            return Err(err(e));
        }
    };
    let family_id = manifest.id;
    let clone_root = family_clone_root(&base_dir, family_id);
    if clone_root.exists() {
        let _ = std::fs::remove_dir_all(&temp_dir);
        return Err("this family is already joined on this device".into());
    }
    std::fs::rename(&temp_dir, &clone_root).map_err(err)?;

    let mut transport = SystemGit::open(&clone_root).map_err(err)?;
    transport.configure_clone().map_err(err)?;

    // D2: "a manifest declaring a `template` version newer than this Ken
    // supports SHALL make the connection unavailable... no sync attempted"
    // — that includes the member-append write below, which this build has
    // no business making against a repo shape it can't fully understand.
    let supported = manifest.check_supported().is_ok();
    let member_id = if !supported {
        existing_member_id.unwrap_or_default()
    } else if let Some(id) = existing_member_id.as_deref() {
        if !manifest.has_member(id) {
            return Err(format!("'{id}' is not a member of this family"));
        }
        id.to_string()
    } else {
        let name = new_member_name
            .ok_or("either an existing member id or a new member name is required")?;
        let id = family::normalize_member_id(&name).map_err(err)?;
        if manifest.has_member(&id) {
            return Err(format!(
                "'{id}' is already a member of this family — join as that member instead"
            ));
        }
        let mut after = manifest.clone();
        after.add_member(FamilyMember::new(id.clone(), name.trim())).map_err(err)?;
        family::manifest_append_only(&manifest, &after).map_err(err)?;
        let write = PendingWrite::new(family::MANIFEST_FILE, after.to_json().map_err(err)?);
        transport
            .commit_paths(&Lane::member(&id), &[write], &format!("{id} joins the family"))
            .map_err(err)?;
        match transport.push().map_err(err)? {
            family_sync::PushOutcome::Failed { detail } => return Err(detail),
            family_sync::PushOutcome::NonFastForward { detail } => {
                return Err(format!("push rejected: {detail}"))
            }
            _ => {}
        }
        id
    };

    let connection = FamilyConnection {
        family_id,
        name: manifest.name.clone(),
        remote_url,
        member_id,
        live_sync: false,
        poll_interval_secs: default_family_poll_interval_secs(),
        attached_workspace_id: None,
    };
    let mut connections = family_connections(&settings);
    connections.push(connection.clone());
    save_family_connections(&state, connections)?;
    let engine = family_engine_handle(&state, family_id, &clone_root);
    let engine_state = engine.lock().unwrap().state().clone();
    Ok(FamilyConnectionDto { connection, state: engine_state })
}

/// Every saved connection with its live sync state (task 2.1/2.3). The
/// on-disk clone is never touched here — connection settings plus whatever
/// `family_engine_handle` already has cached.
#[tauri::command(async)]
fn family_list(state: State<SharedState>) -> CmdResult<Vec<FamilyConnectionDto>> {
    let (base_dir, settings) = {
        let guard = state.lock().unwrap();
        (guard.base_dir.clone(), guard.app_settings.clone())
    };
    Ok(family_connections(&settings)
        .into_iter()
        .map(|connection| {
            let clone_root = family_clone_root(&base_dir, connection.family_id);
            let engine = family_engine_handle(&state, connection.family_id, &clone_root);
            let engine_state = engine.lock().unwrap().state().clone();
            FamilyConnectionDto { connection, state: engine_state }
        })
        .collect())
}

/// The full manifest for one connection (member roster, owner, template
/// version) — a settings-page convenience read, not part of the connection
/// store itself.
#[tauri::command(async)]
fn family_manifest_get(state: State<SharedState>, family_id: String) -> CmdResult<FamilyManifest> {
    let id: uuid::Uuid = family_id.parse().map_err(err)?;
    let (base_dir, settings) = {
        let guard = state.lock().unwrap();
        (guard.base_dir.clone(), guard.app_settings.clone())
    };
    find_family_connection(&settings, id)?;
    FamilyManifest::load(&family_clone_root(&base_dir, id)).map_err(err)
}

/// Forget a connection (task 2.2) — mirrors `forget_project`'s "never
/// deletes files" discipline: the on-disk clone under app data is left in
/// place (re-joining later would find it, though today's `family_join`
/// treats an existing directory as "already joined" rather than adopting
/// it — a known rough edge, not addressed by this task). Stops the poller,
/// drops the cached engine, and detaches the pseudo-member if it's resident.
#[tauri::command(async)]
fn family_remove(app: AppHandle, state: State<SharedState>, family_id: String) -> CmdResult<()> {
    let id: uuid::Uuid = family_id.parse().map_err(err)?;
    let settings = { state.lock().unwrap().app_settings.clone() };
    let connections = family_connections(&settings);
    if !connections.iter().any(|c| c.family_id == id) {
        return Err(format!("no family connection '{family_id}'"));
    }
    let remaining: Vec<FamilyConnection> = connections.into_iter().filter(|c| c.family_id != id).collect();
    save_family_connections(&state, remaining)?;
    {
        let guard = state.lock().unwrap();
        guard.family_engines.lock().unwrap().remove(&id);
    }
    state.lock().unwrap().members.remove(&id);
    reconcile_family_pollers(&app, state.inner());
    Ok(())
}

/// Toggle live sync for one connection (task 2.1/2.3); starts or stops its
/// poller via `reconcile_family_pollers`.
#[tauri::command]
fn family_set_live_sync(app: AppHandle, state: State<SharedState>, family_id: String, live_sync: bool) -> CmdResult<()> {
    let id: uuid::Uuid = family_id.parse().map_err(err)?;
    let settings = { state.lock().unwrap().app_settings.clone() };
    let mut connections = family_connections(&settings);
    let conn = connections
        .iter_mut()
        .find(|c| c.family_id == id)
        .ok_or_else(|| format!("no family connection '{family_id}'"))?;
    conn.live_sync = live_sync;
    save_family_connections(&state, connections)?;
    reconcile_family_pollers(&app, state.inner());
    Ok(())
}

/// Change one connection's poll interval, clamped to design.md D1's 30s–30min
/// bounds; restarts its poller (if live) with the new interval.
#[tauri::command]
fn family_set_poll_interval(app: AppHandle, state: State<SharedState>, family_id: String, secs: u64) -> CmdResult<()> {
    let id: uuid::Uuid = family_id.parse().map_err(err)?;
    let settings = { state.lock().unwrap().app_settings.clone() };
    let mut connections = family_connections(&settings);
    let conn = connections
        .iter_mut()
        .find(|c| c.family_id == id)
        .ok_or_else(|| format!("no family connection '{family_id}'"))?;
    conn.poll_interval_secs = secs.clamp(FAMILY_POLL_INTERVAL_MIN_SECS, FAMILY_POLL_INTERVAL_MAX_SECS);
    save_family_connections(&state, connections)?;
    reconcile_family_pollers(&app, state.inner());
    Ok(())
}

/// "Sync now" (task 2.3): the same fetch -> rebase-integrate -> push cycle
/// the poller runs, on demand, against the SAME cached engine (so a manual
/// sync and the poller can't disagree about connection state).
#[tauri::command(async)]
fn family_sync_now(app: AppHandle, state: State<SharedState>, family_id: String) -> CmdResult<family_sync::SyncReport> {
    let id: uuid::Uuid = family_id.parse().map_err(err)?;
    let (base_dir, settings) = {
        let guard = state.lock().unwrap();
        (guard.base_dir.clone(), guard.app_settings.clone())
    };
    let conn = find_family_connection(&settings, id)?;
    let clone_root = family_clone_root(&base_dir, id);
    let engine = family_engine_handle(&state, id, &clone_root);
    let mut transport = SystemGit::open(&clone_root).map_err(err)?;
    let report = { engine.lock().unwrap().poll(&mut transport) };
    emit_family_sync(&app, id, &report, &clone_root, &conn.member_id);
    emit_day_changed(&app, state.inner());
    Ok(report)
}

/// Clear a conflict after the user has resolved the clone by hand (D1:
/// "never auto-resolve... require manual resolution"). Only ever clears
/// `ConnectionState::Conflict`; a no-op on any other state.
#[tauri::command(async)]
fn family_resolve_conflict(state: State<SharedState>, family_id: String) -> CmdResult<()> {
    let id: uuid::Uuid = family_id.parse().map_err(err)?;
    let base_dir = state.lock().unwrap().base_dir.clone();
    let clone_root = family_clone_root(&base_dir, id);
    let engine = family_engine_handle(&state, id, &clone_root);
    engine.lock().unwrap().resolved();
    Ok(())
}

/// Attach a connection to a workspace (task 2.5). Persists immediately; if
/// `workspace_id` is the CURRENTLY open workspace, also activates the
/// pseudo-member right away (mirrors `activate_memory_pseudo_member`'s call
/// site) so the UI reflects it without a reopen. Otherwise it takes effect
/// the next time that workspace opens (`open_workspace_inner`'s own
/// attached-connections loop).
#[tauri::command(async)]
fn family_attach_workspace(
    app: AppHandle,
    state: State<SharedState>,
    family_id: String,
    workspace_id: String,
) -> CmdResult<()> {
    let id: uuid::Uuid = family_id.parse().map_err(err)?;
    let ws_id: uuid::Uuid = workspace_id.parse().map_err(err)?;
    let settings = { state.lock().unwrap().app_settings.clone() };
    let mut connections = family_connections(&settings);
    let conn = connections
        .iter_mut()
        .find(|c| c.family_id == id)
        .ok_or_else(|| format!("no family connection '{family_id}'"))?;
    conn.attached_workspace_id = Some(ws_id);
    let conn = conn.clone();
    save_family_connections(&state, connections)?;

    let currently_open = { state.lock().unwrap().workspace.as_ref().map(|w| w.ws.config.id) };
    if currently_open == Some(ws_id) {
        if let Err(e) = activate_family_pseudo_member(&app, &state, &conn) {
            eprintln!("warning: family pseudo-member failed to activate: {e}");
        }
    }
    Ok(())
}

/// Detach a connection from its workspace (task 2.5). Persists immediately
/// and drops the pseudo-member's `MemberRuntime` if it's resident right
/// now — removing it from `AppState::members` runs every `Drop` impl the
/// runtime holds (extraction/OCR/kenignore workers, watcher), the same
/// teardown any other member close relies on.
#[tauri::command(async)]
fn family_detach_workspace(state: State<SharedState>, family_id: String) -> CmdResult<()> {
    let id: uuid::Uuid = family_id.parse().map_err(err)?;
    let settings = { state.lock().unwrap().app_settings.clone() };
    let mut connections = family_connections(&settings);
    let conn = connections
        .iter_mut()
        .find(|c| c.family_id == id)
        .ok_or_else(|| format!("no family connection '{family_id}'"))?;
    conn.attached_workspace_id = None;
    save_family_connections(&state, connections)?;
    state.lock().unwrap().members.remove(&id);
    Ok(())
}

/// This device's own inbox for one family (task 2.4) — `members/<me>/
/// inbox/`, never a teammate's (lanes make a teammate's inbox unreadable to
/// us in the git-write sense; nothing stops a local read, but there is no
/// reason for one, and this command doesn't offer it).
#[tauri::command(async)]
fn family_inbox_list(state: State<SharedState>, family_id: String) -> CmdResult<Vec<family::InboxItem>> {
    let id: uuid::Uuid = family_id.parse().map_err(err)?;
    let (base_dir, settings) = {
        let guard = state.lock().unwrap();
        (guard.base_dir.clone(), guard.app_settings.clone())
    };
    let conn = find_family_connection(&settings, id)?;
    let clone_root = family_clone_root(&base_dir, id);
    let dir = family::inbox_dir(&clone_root, &conn.member_id);
    Ok(list_inbox_items(&dir).into_iter().map(|(_, item)| item).collect())
}

/// Patch one inbox item's status — `seen`/`archived` (D4's dismiss path);
/// `accepted` is reserved for `family_accept_task`, which needs to write
/// the board task in the same commit, so this refuses that transition
/// rather than leaving an item `accepted` with no board task to match it.
///
/// Writes through `GitTransport::commit_paths`, never `family::apply_inbox_
/// status`'s direct-disk write: `commit_paths` is documented as "the only
/// sanctioned way to write to a family repo", and a direct write here would
/// leave the change uncommitted until some later commit happened to
/// re-stage the same path — this way every write is commit-then-push in one
/// step, consistent with every other mutation in this file.
#[tauri::command(async)]
fn family_set_item_status(
    app: AppHandle,
    state: State<SharedState>,
    family_id: String,
    item_id: String,
    status: String,
) -> CmdResult<family::InboxItem> {
    let id: uuid::Uuid = family_id.parse().map_err(err)?;
    let new_status = family::InboxStatus::parse(&status)
        .ok_or_else(|| format!("unknown inbox status '{status}'"))?;
    if new_status == family::InboxStatus::Accepted {
        return Err("use family_accept_task to accept a task item".into());
    }
    let (base_dir, settings) = {
        let guard = state.lock().unwrap();
        (guard.base_dir.clone(), guard.app_settings.clone())
    };
    let conn = find_family_connection(&settings, id)?;
    let clone_root = family_clone_root(&base_dir, id);
    let inbox_dir = family::inbox_dir(&clone_root, &conn.member_id);
    let (item_path, _item) = list_inbox_items(&inbox_dir)
        .into_iter()
        .find(|(_, it)| it.id == item_id)
        .ok_or_else(|| format!("no inbox item '{item_id}'"))?;
    let raw = std::fs::read_to_string(&item_path).map_err(err)?;
    let file_name = item_path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
    let updated = iso_datetime_now();
    let new_text = family::set_status_text(&raw, new_status, &updated);
    let rel_path = format!("{}/{file_name}", family::inbox_rel(&conn.member_id));

    let mut transport = SystemGit::open(&clone_root).map_err(err)?;
    let engine = family_engine_handle(&state, id, &clone_root);
    {
        let mut eng = engine.lock().unwrap();
        eng.commit(
            &mut transport,
            &Lane::member(&conn.member_id),
            &[PendingWrite::new(rel_path, new_text.clone())],
            &format!("Mark inbox item {item_id} {status}"),
        )
        .map_err(err)?;
    }
    let report = { engine.lock().unwrap().poll(&mut transport) };
    emit_family_sync(&app, id, &report, &clone_root, &conn.member_id);
    Ok(family::parse_inbox_item(&file_name, &new_text))
}

/// Accept a task inbox item (task 2.4, D4's acceptance gate): mints a new
/// board task in `members/<me>/board/` and marks the inbox item `accepted`,
/// both in one commit (`family::accept_task` returns both file contents for
/// exactly this reason) — so the repo never has a half-accepted state
/// where one file changed and the other didn't. Emits `day-changed` once
/// (the accepted task joins Your day's list) through `with_task_writes`,
/// like an ordinary task write. Returns the accepted task as Your day
/// shows it (the shape `day_task_create` returns).
#[tauri::command(async)]
fn family_accept_task(app: AppHandle, state: State<SharedState>, family_id: String, item_id: String) -> CmdResult<DayTaskDto> {
    let id: uuid::Uuid = family_id.parse().map_err(err)?;
    let (base_dir, settings, ws_root) = {
        let guard = state.lock().unwrap();
        (guard.base_dir.clone(), guard.app_settings.clone(), guard.workspace.as_ref().map(|w| w.ws.root.clone()))
    };
    let conn = find_family_connection(&settings, id)?;
    let clone_root = family_clone_root(&base_dir, id);
    let inbox_dir = family::inbox_dir(&clone_root, &conn.member_id);
    let (item_path, _item) = list_inbox_items(&inbox_dir)
        .into_iter()
        .find(|(_, it)| it.id == item_id)
        .ok_or_else(|| format!("no inbox item '{item_id}'"))?;
    let raw = std::fs::read_to_string(&item_path).map_err(err)?;
    let file_name = item_path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();

    let today = local_date_today();
    let time = local_time_hhmm();
    let task_id = tasks::new_ulid();
    let accepted = family::accept_task(&raw, &conn.member_id, &task_id, &today, &time).map_err(err)?;

    let inbox_rel_path = format!("{}/{file_name}", family::inbox_rel(&conn.member_id));
    let writes = vec![
        PendingWrite::new(accepted.board_rel_path.clone(), accepted.board_content.clone()),
        PendingWrite::new(inbox_rel_path, accepted.inbox_content),
    ];
    let board_path = clone_root.join(&accepted.board_rel_path);
    let mut transport = SystemGit::open(&clone_root).map_err(err)?;
    let engine = family_engine_handle(&state, id, &clone_root);
    with_task_writes(&app, state.inner(), &[board_path.clone(), item_path.clone()], || {
        let mut eng = engine.lock().unwrap();
        eng.commit(
            &mut transport,
            &Lane::member(&conn.member_id),
            &writes,
            &format!("Accept task from inbox item {}", accepted.item_id),
        )
        .map(|_| ())
        .map_err(err)
    })?;
    let report = { engine.lock().unwrap().poll(&mut transport) };
    emit_family_sync(&app, id, &report, &clone_root, &conn.member_id);

    // The file lives at `<clone>/members/<id>/board/…`: a family-home task,
    // as Your day's scan reads the same board a moment later.
    let task = ken_core::day::parse_task(&board_path, tasks::HomeKind::Family, &accepted.board_content, &today);
    let names = family_member_names(&base_dir, std::slice::from_ref(&conn));
    let root = ws_root.unwrap_or_default();
    Ok(day_task_dto(&root, &task, &names, None))
}

/// Push back on an inbox item (D4): creates a new message item in the
/// SENDER's inbox (lane rule 2 — never a modification of their files) and
/// leaves the original item exactly as it was. "The original item stays
/// yours to mark seen/accepted/archived" (D4) is deliberately a separate
/// action (`family_set_item_status`), not bundled into this one — push-back
/// is "reply", not "reply and also change my own status", so this command
/// does exactly the one lane-2 write it claims to.
#[tauri::command(async)]
fn family_push_back(app: AppHandle, state: State<SharedState>, family_id: String, item_id: String, note: String) -> CmdResult<()> {
    let id: uuid::Uuid = family_id.parse().map_err(err)?;
    let (base_dir, settings) = {
        let guard = state.lock().unwrap();
        (guard.base_dir.clone(), guard.app_settings.clone())
    };
    let conn = find_family_connection(&settings, id)?;
    let clone_root = family_clone_root(&base_dir, id);
    let inbox_dir = family::inbox_dir(&clone_root, &conn.member_id);
    let (_path, item) = list_inbox_items(&inbox_dir)
        .into_iter()
        .find(|(_, it)| it.id == item_id)
        .ok_or_else(|| format!("no inbox item '{item_id}'"))?;
    if item.from.trim().is_empty() {
        return Err("this item has no sender to push back to".into());
    }
    let new_item = family::push_back_item(&item, &conn.member_id, &note);
    let new_id = tasks::new_ulid();
    let created = iso_datetime_now();
    let content = family::render_inbox_item(&new_item, &new_id, &created);
    let file_name =
        family::inbox_item_file_name(&new_id, new_item.kind.unwrap_or(family::InboxKind::Message), &new_item.title);
    let rel_path = format!("{}/{file_name}", family::inbox_rel(item.from.trim()));

    let mut transport = SystemGit::open(&clone_root).map_err(err)?;
    let engine = family_engine_handle(&state, id, &clone_root);
    {
        let mut eng = engine.lock().unwrap();
        eng.commit(
            &mut transport,
            &Lane::member(&conn.member_id),
            &[PendingWrite::new(rel_path, content)],
            &format!("Push back on {item_id}"),
        )
        .map_err(err)?;
    }
    let report = { engine.lock().unwrap().poll(&mut transport) };
    emit_family_sync(&app, id, &report, &clone_root, &conn.member_id);
    Ok(())
}

/// Send a task, message or notification to a teammate's inbox from the app,
/// the same lane-2 write the MCP `family_send` tool makes. Delivery is not
/// assignment: the item waits in their inbox until they accept or read it.
#[tauri::command(async)]
fn family_send(
    app: AppHandle,
    state: State<SharedState>,
    family_id: String,
    to: String,
    kind: String,
    title: String,
    body: String,
) -> CmdResult<()> {
    let id: uuid::Uuid = family_id.parse().map_err(err)?;
    let (base_dir, settings) = {
        let guard = state.lock().unwrap();
        (guard.base_dir.clone(), guard.app_settings.clone())
    };
    let title = title.trim().to_string();
    if title.is_empty() {
        return Err("give the item a title".into());
    }
    let kind = family::InboxKind::parse(&kind).ok_or("kind must be task, message or notification")?;
    let conn = find_family_connection(&settings, id)?;
    let clone_root = family_clone_root(&base_dir, id);
    let manifest = FamilyManifest::load(&clone_root).map_err(err)?;
    let to = to.trim().to_string();
    if !manifest.has_member(&to) {
        return Err(format!("\"{to}\" is not a member of this team. Members: {}.", manifest.member_ids().join(", ")));
    }
    let new_item = family::NewInboxItem {
        id: None,
        kind: Some(kind),
        from: conn.member_id.clone(),
        title: title.clone(),
        body,
        task: None,
    };
    let new_id = tasks::new_ulid();
    let content = family::render_inbox_item(&new_item, &new_id, &iso_datetime_now());
    let rel_path = format!("{}/{}", family::inbox_rel(&to), family::inbox_item_file_name(&new_id, kind, &title));

    let mut transport = SystemGit::open(&clone_root).map_err(err)?;
    let engine = family_engine_handle(&state, id, &clone_root);
    {
        let mut eng = engine.lock().unwrap();
        eng.commit(
            &mut transport,
            &Lane::member(&conn.member_id),
            &[PendingWrite::new(rel_path, content)],
            &format!("Ken: deliver {} to {to}", kind.as_str()),
        )
        .map_err(err)?;
    }
    let report = { engine.lock().unwrap().poll(&mut transport) };
    emit_family_sync(&app, id, &report, &clone_root, &conn.member_id);
    Ok(())
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
        workspace_kg_running: Arc::new(AtomicBool::new(false)),
        workspace_kg_cancel: Arc::new(Mutex::new(None)),
        workspace_kg_debounce_gen: Arc::new(AtomicU64::new(0)),
        workspace: None,
        ingest_gate: Arc::new(IngestGate::new(WORKSPACE_INGEST_CONCURRENCY)),
        memory_distill_running: Arc::new(AtomicBool::new(false)),
        memory_distill_candidates: Arc::new(Mutex::new(Vec::new())),
        task_recent_writes: Arc::new(Mutex::new(std::collections::HashMap::new())),
        last_team: None,
        team_digest_running: Arc::new(AtomicBool::new(false)),
        family_engines: Arc::new(Mutex::new(std::collections::HashMap::new())),
        family_pollers: Arc::new(Mutex::new(std::collections::HashMap::new())),
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
            // ken-families task 2.3: resume every saved connection's poller
            // on startup (only those with `liveSync` on). Reuses the same
            // reconciliation every family_* mutator calls, so startup and every later change agree.
            use tauri::Manager;
            let app_handle = app.handle().clone();
            let shared_state = app_handle.state::<SharedState>().inner().clone();
            reconcile_family_pollers(&app_handle, &shared_state);

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
                maybe_generate_team_digest(window.app_handle(), state.inner());
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            list_projects,
            create_project,
            open_project,
            open_member,
            close_member,
            open_workspace,
            create_workspace,
            workspace_overview,
            focus_project,
            discover_workspace_candidates,
            close_workspace,
            search_all_projects,
            forget_project,
            list_recent_workspaces,
            forget_workspace,
            rename_project,
            last_project_id,
            current_project,
            set_folder_selection,
            get_tree,
            get_tree_all,
            search,
            hybrid_search,
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
            profile_project,
            profile_candidates,
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
            code_file,
            code_usages,
            run_drift_now,
            setup_propose,
            setup_confirm,
            setup_rescan,
            setup_propose_repos,
            setup_confirm_repos,
            set_project_description,
            ingest_status,
            ingest_overview,
            team_wiki,
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
            team_overview,
            team_save_ignores,
            team_add_rule,
            ingest_now,
            ingest_undo,
            ingest_undo_write,
            ingest_read_again,
            ingest_file,
            draft_wiki,
            wiki_add_repos,
            setup_create_wiki,
            setup_create_team_repo,
            apply_page_proposal,
            sync_now,
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
            knowledge_model,
            refresh_knowledge_model,
            rebuild_workspace_kg,
            workspace_kg_overview,
            workspace_kg_entity,
            workspace_kg_search,
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
            memory_write,
            journal_append,
            read_journal,
            distill_journal,
            resolve_distill_candidate,
            day_state,
            day_task_create,
            day_task_update,
            day_task_delete,
            ticket_tasks,
            team_digest,
            refresh_team_digest,
            team_digest_writing,
            resolve_page_link,
            wiki_drafting,
            escalation_reply,
            ticket_set_status,
            workspace_groups,
            workspace_set_group,
            workspace_remove_group,
            workspace_candidates,
            workspace_add_member,
            workspace_remove_member,
            workspace_ignored,
            workspace_ignore_candidate,
            workspace_unignore_candidate,
            family_create,
            family_join,
            family_list,
            family_manifest_get,
            family_remove,
            family_set_live_sync,
            family_set_poll_interval,
            family_sync_now,
            family_resolve_conflict,
            family_attach_workspace,
            family_detach_workspace,
            family_inbox_list,
            family_set_item_status,
            family_accept_task,
            family_push_back,
            family_send,
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
        // The caller now hands each source in as `(rel_path, excerpt_text)` with
        // marks already stripped (the excerpt/snippet fallback does that upstream),
        // so the fixture supplies clean text directly.
        let sources = vec![("People.md".to_string(), "Priya owns billing".to_string())];
        let p = local_quick_answer_prompt("who owns billing?", &sources);
        assert!(p.contains("<|im_start|>system"));
        assert!(p.contains("who owns billing?"));
        assert!(p.contains("People.md: Priya owns billing"));
        assert!(p.contains("<|im_start|>assistant"));
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
            .search("zebra", 10)
            .unwrap()
            .into_iter()
            .map(|h| h.rel_path)
            .collect();
        assert!(paths.contains(&"Archive/Meetings/kickoff.md".to_string()), "got {paths:?}");
        assert!(paths.contains(&"Archive/Meetings/Q3/review.md".to_string()), "got {paths:?}");
        assert!(paths.contains(&"Meetings-old/stale.md".to_string()), "got {paths:?}");
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

    fn attempt(at: i64, date: &str, ok: bool) -> TeamDigestAttempt {
        TeamDigestAttempt { at, date: date.into(), ok }
    }

    #[test]
    fn the_schedule_backs_off_after_a_failure_and_runs_once_a_day_after_success() {
        let mut s = TeamDigestSchedule::default();
        let today = "2026-10-01";
        assert!(digest_due(&s, Some("Ops"), false, today, 10_000), "never tried");
        assert!(!digest_due(&s, Some("Ops"), true, today, 10_000), "stored today");

        s.attempts.insert("Ops".into(), attempt(10_000, today, false));
        assert!(!digest_due(&s, Some("Ops"), false, today, 10_000 + 60), "a minute after a failure");
        assert!(digest_due(&s, Some("Ops"), false, today, 10_000 + TEAM_DIGEST_RETRY_AFTER_SECS), "30 minutes on");

        s.attempts.insert("Ops".into(), attempt(10_000, today, true));
        assert!(!digest_due(&s, Some("Ops"), false, today, 90_000), "succeeded today");
        assert!(digest_due(&s, Some("Ops"), false, "2026-10-02", 90_000), "a new day");
        assert!(digest_due(&s, None, false, today, 10_000), "the whole workspace is its own key");
    }

    #[test]
    fn the_schedule_file_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let ws = uuid::Uuid::new_v4();
        assert!(load_digest_schedule(dir.path(), ws).last_team.is_none());
        update_digest_schedule(dir.path(), ws, |s| s.last_team = Some("Ops".into()));
        update_digest_schedule(dir.path(), ws, |s| {
            s.attempts.insert(digest_schedule_key(None), attempt(5, "2026-10-01", true));
        });
        let s = load_digest_schedule(dir.path(), ws);
        assert_eq!(s.last_team.as_deref(), Some("Ops"));
        assert!(s.attempts[""].ok);
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

    #[test]
    fn your_day_lists_open_escalations_for_me_from_every_team_repo() {
        let d = tempfile::tempdir().unwrap();
        let team = d.path().join("Team");
        let wiki = d.path().join("Wiki");
        for (root, file, text) in [
            (&team, "E-001.md", "---\nid: E-001\nto: chris\nstatus: open\nraised: 2026-09-20\n---\n\n# Which save format?\n"),
            (&team, "E-002.md", "---\nid: E-002\nto: Kate\nstatus: open\n---\n\n# Not mine\n"),
            (&wiki, "E-003.md", "---\nid: E-003\nto: Chris Staud\nstatus: open\nraised: 2026-09-01\nblocks: SR-4\n---\n\n# Older one\n"),
            (&wiki, "E-004.md", "---\nid: E-004\nto: Chris Staud\nstatus: answered\n---\n\n# Done\n"),
        ] {
            std::fs::create_dir_all(root.join("escalations")).unwrap();
            std::fs::write(root.join("escalations").join(file), text).unwrap();
        }
        let repos = vec![
            TeamRepo { id: uuid::Uuid::new_v4(), name: "Team".into(), root: team },
            TeamRepo { id: uuid::Uuid::new_v4(), name: "Wiki".into(), root: wiki },
        ];
        let me = ken_core::day::Me { name: Some("Chris Staud".into()), email: None };
        let found = my_escalations(&repos, &me);
        assert_eq!(found.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), vec!["E-003", "E-001"], "oldest raised first");
        assert_eq!(found[0].repo, "Wiki");
        assert_eq!(found[0].rel_path, "escalations/E-003.md");
        assert_eq!(found[0].blocks.as_deref(), Some("SR-4"));
        assert!(my_escalations(&repos, &ken_core::day::Me::default()).is_empty(), "no identity, no list");
    }

    #[test]
    fn the_team_screen_reads_the_link_report_and_the_drafts_from_the_wiki() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        std::fs::create_dir_all(root.join("Current")).unwrap();
        std::fs::write(root.join("Current/Team.md"), "---\nstatus: draft\n---\n# Team\nSee [[Ghost]].\n").unwrap();
        let project = Project::create(root, "Wiki").unwrap();
        let data = tempfile::tempdir().unwrap();
        let mut db = Db::open_at(&data.path().join("w.db")).unwrap();
        scan::scan(&project, &mut db).unwrap();
        let found = team_findings("w1", root, &db, None);
        assert!(found.iter().any(|f| f.kind == "missing-page" && f.path == "Current/Team.md" && f.project_id == "w1"), "{:?}", found.iter().map(|f| &f.title).collect::<Vec<_>>());
        assert!(found.iter().all(|f| f.kind != "draft"), "nothing drafted by Ken yet");
    }
}

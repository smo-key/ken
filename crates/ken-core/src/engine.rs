//! Ingest engine: owns the run queue for one project. Watches for source
//! changes (fed by the app layer from scan results), debounces, and runs
//! one ingest at a time through the runner + refresh pipeline.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use uuid::Uuid;

use crate::automation;
use crate::db::Db;
use crate::hooks::{install_hooks, HookListener};
use crate::project::Project;
use crate::recipe::{self, Refresh};
use crate::refresh;
use crate::runner::{self, CancelToken, RunOutcome, RunnerConfig, RunnerMode};
use crate::{Error, Result};

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestEvent {
    /// `ingest` | `automation` — routes the event to the right UI surface.
    pub kind: String,
    pub slug: String,
    pub run_id: i64,
    pub session_id: Option<String>,
    /// Persisted: `running` | `blocked` | `fresh` | `pending_approval` |
    /// `failed` | `cancelled`. Transient (never stored): `queued` | `waiting`.
    pub status: String,
    pub detail: Option<String>,
    /// Latest human-readable activity line for a running run (transient).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity: Option<String>,
    /// Seconds the running run has been going (server snapshot; UI also ticks).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_secs: Option<u64>,
    /// For `queued`: whole seconds until the debounce deadline.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eta_secs: Option<u64>,
}

impl IngestEvent {
    /// The common case: a status transition with no live-activity payload.
    pub fn at(
        kind: &str,
        slug: &str,
        run_id: i64,
        session_id: Option<String>,
        status: &str,
        detail: Option<String>,
    ) -> IngestEvent {
        IngestEvent {
            kind: kind.to_string(),
            slug: slug.to_string(),
            run_id,
            session_id,
            status: status.to_string(),
            detail,
            activity: None,
            elapsed_secs: None,
            eta_secs: None,
        }
    }
}

#[derive(Clone)]
pub struct EngineConfig {
    /// Explicit CLI path (tests); None = discover at run time.
    pub binary: Option<PathBuf>,
    pub timeout: Duration,
    pub debounce: Duration,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            binary: None,
            timeout: Duration::from_secs(15 * 60),
            debounce: Duration::from_secs(10),
        }
    }
}

/// Which subsystem a queued job belongs to. Automations share the engine's
/// single-worker queue; the key carries the kind so an ingest recipe and an
/// automation that happen to share a slug never collide (in the queue or when
/// cancelling).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RunKind {
    Ingest,
    Automation,
}

impl RunKind {
    fn as_str(self) -> &'static str {
        match self {
            RunKind::Ingest => "ingest",
            RunKind::Automation => "automation",
        }
    }
}

/// Identity of a queued job: a run is unique per (subsystem, slug).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct QueueKey {
    kind: RunKind,
    slug: String,
}

/// A debounced unit of work waiting in the engine queue.
struct PendingJob {
    /// Earliest instant this job may start (debounce watermark).
    deadline: Instant,
    force_full: bool,
    /// Source files that matched an automation's globs across the debounce
    /// window (union). Empty for ingests.
    matched: Vec<String>,
    /// Phase-2 approved proposal text; `Some` only for an automation apply job.
    apply: Option<String>,
}

/// The single in-flight run, keyed by `(kind, slug)` so a cancel targets the
/// right subsystem. Shared between the loop (which claims/reaps it) and the run
/// thread (which clears it on exit).
type CurrentRun = Arc<Mutex<Option<(RunKind, String, CancelToken)>>>;

enum Msg {
    Trigger { kind: RunKind, slug: String, force_full: bool },
    /// Phase-2 automation run: carry the approved proposal text through to
    /// the worker as a due-now `PendingJob { apply: Some(..) }`.
    ApplyAutomation { slug: String, proposal: String },
    SourcesChanged(Vec<String>),
    Shutdown,
}

pub struct IngestEngine {
    tx: Sender<Msg>,
    current: CurrentRun,
    thread: Option<std::thread::JoinHandle<()>>,
}

pub fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl IngestEngine {
    pub fn start(
        project_root: PathBuf,
        db_path: PathBuf,
        hooks: Arc<HookListener>,
        cfg: EngineConfig,
        on_event: impl Fn(IngestEvent) + Send + Sync + 'static,
    ) -> Result<IngestEngine> {
        let (tx, rx) = channel::<Msg>();
        let current: CurrentRun = Arc::new(Mutex::new(None));
        let current_thread = current.clone();

        let thread = std::thread::spawn(move || {
            // Shared so the streaming activity callback (which needs a `'static`
            // sink) and this loop can both emit through the same on_event.
            let on_event: Arc<dyn Fn(IngestEvent) + Send + Sync> = Arc::new(on_event);
            // Validate the DB opens up front; each run opens its own WAL
            // connection on its worker thread (WAL allows the concurrent handles).
            if Db::open_at(&db_path).is_err() {
                return;
            }
            let mut pending: HashMap<QueueKey, PendingJob> = HashMap::new();
            let mut announced_queued: HashSet<QueueKey> = HashSet::new();
            let mut announced_waiting: HashSet<QueueKey> = HashSet::new();
            // The single in-flight run's thread (concurrency stays 1). Runs are
            // dispatched off-thread so the loop can keep servicing the queue —
            // that's what lets a blocked second job surface a `waiting` event.
            let mut run_handle: Option<std::thread::JoinHandle<()>> = None;

            loop {
                match rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(Msg::Trigger { kind, slug, force_full }) => {
                        // Run-now: due immediately, bypassing the debounce.
                        let key = QueueKey { kind, slug };
                        pending.insert(
                            key.clone(),
                            PendingJob {
                                deadline: Instant::now(),
                                force_full,
                                matched: vec![],
                                apply: None,
                            },
                        );
                        announced_queued.remove(&key);
                    }
                    Ok(Msg::ApplyAutomation { slug, proposal }) => {
                        // Approved phase-2 run: due immediately, carries the
                        // proposal text so the worker runs the apply prompt.
                        let key = QueueKey { kind: RunKind::Automation, slug };
                        pending.insert(
                            key.clone(),
                            PendingJob {
                                deadline: Instant::now(),
                                force_full: false,
                                matched: vec![],
                                apply: Some(proposal),
                            },
                        );
                        announced_queued.remove(&key);
                    }
                    Ok(Msg::SourcesChanged(paths)) => {
                        if let Ok(project) = Project::open(&project_root) {
                            // Recipes: on-change refreshes.
                            if let Ok(entries) = recipe::list(&project) {
                                for entry in entries {
                                    if let recipe::RecipeEntry::Ok { recipe: r } = entry {
                                        if r.refresh == Refresh::OnChange
                                            && refresh::triggers(&r, &paths)
                                        {
                                            let key = QueueKey {
                                                kind: RunKind::Ingest,
                                                slug: r.slug.clone(),
                                            };
                                            // Keep the earliest deadline so a
                                            // steady stream can't postpone the
                                            // run forever.
                                            pending.entry(key.clone()).or_insert_with(|| PendingJob {
                                                deadline: Instant::now() + cfg.debounce,
                                                force_full: false,
                                                matched: vec![],
                                                apply: None,
                                            });
                                            maybe_announce_queued(
                                                &mut announced_queued,
                                                &pending,
                                                &key,
                                                &on_event,
                                            );
                                        }
                                    }
                                }
                            }
                            // Automations: any enabled rule whose globs match a
                            // changed path is enqueued (same 10s debounce), the
                            // matched paths unioned across the window so the run
                            // sees every file that triggered it. auto_apply is
                            // resolved later, inside execute_automation.
                            if let Ok(autos) = automation::list_ok(&project) {
                                for a in autos {
                                    if !a.enabled {
                                        continue;
                                    }
                                    let hits = automation::triggers(&a, &paths);
                                    if hits.is_empty() {
                                        continue;
                                    }
                                    let key = QueueKey {
                                        kind: RunKind::Automation,
                                        slug: a.slug.clone(),
                                    };
                                    let job = pending.entry(key.clone()).or_insert_with(|| PendingJob {
                                        deadline: Instant::now() + cfg.debounce,
                                        force_full: false,
                                        matched: vec![],
                                        apply: None,
                                    });
                                    for h in hits {
                                        if !job.matched.contains(&h) {
                                            job.matched.push(h);
                                        }
                                    }
                                    maybe_announce_queued(
                                        &mut announced_queued,
                                        &pending,
                                        &key,
                                        &on_event,
                                    );
                                }
                            }
                        }
                    }
                    Ok(Msg::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                        break;
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                }

                let running = current_thread.lock().unwrap().is_some();
                // Earliest-due job across the unified queue.
                let due: Option<QueueKey> = pending
                    .iter()
                    .filter(|(_, j)| j.deadline <= Instant::now())
                    .min_by_key(|(_, j)| j.deadline)
                    .map(|(k, _)| k.clone());
                if let Some(key) = due {
                    if running {
                        // Something's due but the single worker is busy: surface
                        // it once so the UI can show "waiting for <name>".
                        if !announced_waiting.contains(&key) {
                            let current_name = current_thread
                                .lock()
                                .unwrap()
                                .as_ref()
                                .map(|(_, s, _)| s.clone())
                                .unwrap_or_default();
                            let mut ev = IngestEvent::at(
                                key.kind.as_str(),
                                &key.slug,
                                0,
                                None,
                                "waiting",
                                None,
                            );
                            ev.detail = Some(format!("waiting for {current_name}"));
                            on_event(ev);
                            announced_waiting.insert(key.clone());
                        }
                    } else {
                        // The previous run's thread has cleared `current`; reap it.
                        if let Some(h) = run_handle.take() {
                            let _ = h.join();
                        }
                        let job = pending.remove(&key).unwrap();
                        announced_queued.remove(&key);
                        announced_waiting.remove(&key);
                        match key.kind {
                            RunKind::Ingest => {
                                // Claim the worker synchronously (before the loop
                                // spins again) so the next iteration sees it busy.
                                let token = CancelToken::new();
                                *current_thread.lock().unwrap() =
                                    Some((RunKind::Ingest, key.slug.clone(), token.clone()));
                                let project_root = project_root.clone();
                                let db_path = db_path.clone();
                                let hooks = hooks.clone();
                                let cfg = cfg.clone();
                                let on_event = on_event.clone();
                                let current = current_thread.clone();
                                let slug = key.slug.clone();
                                let force_full = job.force_full;
                                run_handle = Some(std::thread::spawn(move || {
                                    if let Ok(mut db) = Db::open_at(&db_path) {
                                        execute_ingest(
                                            &project_root,
                                            &mut db,
                                            &hooks,
                                            &cfg,
                                            &slug,
                                            force_full,
                                            &on_event,
                                            &token,
                                        );
                                    }
                                    // Always release the worker, even on the
                                    // no-op early return inside execute_ingest.
                                    *current.lock().unwrap() = None;
                                }));
                            }
                            RunKind::Automation => {
                                // Claim the worker synchronously (before the loop
                                // spins again) so the next iteration sees it busy.
                                let token = CancelToken::new();
                                *current_thread.lock().unwrap() =
                                    Some((RunKind::Automation, key.slug.clone(), token.clone()));
                                let project_root = project_root.clone();
                                let db_path = db_path.clone();
                                let hooks = hooks.clone();
                                let cfg = cfg.clone();
                                let on_event = on_event.clone();
                                let current = current_thread.clone();
                                let slug = key.slug.clone();
                                run_handle = Some(std::thread::spawn(move || {
                                    if let Ok(mut db) = Db::open_at(&db_path) {
                                        execute_automation(
                                            &project_root,
                                            &mut db,
                                            &hooks,
                                            &cfg,
                                            &current,
                                            &slug,
                                            &job,
                                            &on_event,
                                        );
                                    }
                                    // Always release the worker (execute_automation
                                    // also clears it after the run — setting None
                                    // twice is harmless).
                                    *current.lock().unwrap() = None;
                                }));
                            }
                        }
                    }
                }
            }
            // Drain the in-flight run on shutdown — cancel first so a run
            // dispatched in the Shutdown race window (Drop saw `current == None`
            // and its cancel no-opped) still stops promptly instead of hanging
            // the join for the run's full duration.
            if let Some((_, _, token)) = current_thread.lock().unwrap().as_ref() {
                token.cancel();
            }
            if let Some(h) = run_handle.take() {
                let _ = h.join();
            }
        });

        Ok(IngestEngine {
            tx,
            current,
            thread: Some(thread),
        })
    }

    pub fn trigger(&self, slug: &str, force_full: bool) {
        let _ = self.tx.send(Msg::Trigger {
            kind: RunKind::Ingest,
            slug: slug.to_string(),
            force_full,
        });
    }

    /// Run-now for an automation: enqueue a due-now automation job (phase-1
    /// proposal, or a direct run when the automation is `auto_apply`).
    pub fn run_automation(&self, slug: &str) {
        let _ = self.tx.send(Msg::Trigger {
            kind: RunKind::Automation,
            slug: slug.to_string(),
            force_full: false,
        });
    }

    /// Queue the phase-2 "carry out the approved actions" run for an automation.
    pub fn apply_automation(&self, slug: &str, proposal: &str) {
        let _ = self.tx.send(Msg::ApplyAutomation {
            slug: slug.to_string(),
            proposal: proposal.to_string(),
        });
    }

    pub fn sources_changed(&self, paths: Vec<String>) {
        let _ = self.tx.send(Msg::SourcesChanged(paths));
    }

    /// Cancel the currently running job if it matches `(kind, slug)`. Keyed by
    /// kind so cancelling an ingest can't stop an automation that happens to
    /// share the slug (and vice versa).
    pub fn cancel(&self, kind: RunKind, slug: &str) {
        if let Some((running_kind, running_slug, token)) = self.current.lock().unwrap().as_ref() {
            if *running_kind == kind && running_slug == slug {
                token.cancel();
            }
        }
    }
}

impl Drop for IngestEngine {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Shutdown);
        if let Some((_, _, token)) = self.current.lock().unwrap().as_ref() {
            token.cancel();
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Emit a `queued` event with a whole-second ETA once per key, so the UI can
/// show a countdown to the debounce deadline.
fn maybe_announce_queued(
    announced: &mut HashSet<QueueKey>,
    pending: &HashMap<QueueKey, PendingJob>,
    key: &QueueKey,
    on_event: &Arc<dyn Fn(IngestEvent) + Send + Sync>,
) {
    if announced.contains(key) {
        return;
    }
    let Some(job) = pending.get(key) else { return };
    let dur = job.deadline.saturating_duration_since(Instant::now());
    // Round up so a sub-second debounce still reads as "~1s", not "0s".
    let eta = dur.as_secs() + u64::from(dur.subsec_millis() > 0);
    let mut ev = IngestEvent::at(key.kind.as_str(), &key.slug, 0, None, "queued", None);
    ev.eta_secs = Some(eta);
    on_event(ev);
    announced.insert(key.clone());
}

#[allow(clippy::too_many_arguments)]
fn execute_ingest(
    project_root: &PathBuf,
    db: &mut Db,
    hooks: &HookListener,
    cfg: &EngineConfig,
    slug: &str,
    force_full: bool,
    on_event: &Arc<dyn Fn(IngestEvent) + Send + Sync>,
    token: &CancelToken,
) {
    // Fresh state every run: settings and recipes may have changed on disk.
    let emit_fail = |run_id: i64, detail: String| {
        on_event(IngestEvent::at("ingest", slug, run_id, None, "failed", Some(detail)));
    };

    let project = match Project::open(project_root) {
        Ok(p) => p,
        Err(e) => {
            emit_fail(0, e.to_string());
            return;
        }
    };
    let recipe = match recipe::load_slug(&project, slug) {
        Ok(r) => r,
        Err(e) => {
            emit_fail(0, e.to_string());
            return;
        }
    };
    let rules = recipe::resolve_rules(&recipe, &project);

    let plan = match refresh::plan(&project, db, &recipe, &rules, force_full) {
        Ok(Some(p)) => p,
        Ok(None) => {
            // Not silent anymore: record a "checked, nothing to do" run so the
            // user sees the engine looked. Marking it `fresh` advances the
            // last-success watermark harmlessly — nothing changed, so no source
            // file is skipped by the next incremental plan.
            let run_id = match db.insert_run(slug, None, now_epoch()) {
                Ok(id) => id,
                Err(e) => { emit_fail(0, e.to_string()); return; }
            };
            let summary = "Checked — nothing to update.";
            let _ = db.update_run(run_id, "fresh", Some(now_epoch()), Some(summary), None, None);
            on_event(IngestEvent::at("ingest", slug, run_id, None, "fresh", Some(summary.into())));
            return;
        }
        Err(e) => {
            emit_fail(0, e.to_string());
            return;
        }
    };

    // Headless is the default: interactive TUI sessions can stall on
    // Claude's one-time startup prompts (trust, hooks approval) with no one
    // to answer them. hidden-tui is an explicit opt-in for watchable runs.
    let mode = project
        .config
        .extra
        .get("ingestRunner")
        .and_then(|v| v.as_str())
        .map(|s| {
            if s == "hidden-tui" {
                RunnerMode::HiddenTui
            } else {
                RunnerMode::Headless
            }
        })
        .unwrap_or(RunnerMode::Headless);

    let binary = cfg
        .binary
        .clone()
        .or_else(runner::discover_claude)
        .unwrap_or_else(|| PathBuf::from("claude-not-found"));

    let session_id = Uuid::new_v4().to_string();
    let run_id = match db.insert_run(slug, Some(&session_id), now_epoch()) {
        Ok(id) => id,
        Err(e) => {
            emit_fail(0, e.to_string());
            return;
        }
    };
    on_event(IngestEvent::at(
        "ingest",
        slug,
        run_id,
        Some(session_id.clone()),
        "running",
        None,
    ));

    if mode == RunnerMode::HiddenTui {
        if let Err(e) = install_hooks(&project.root, &hooks.hook_url()) {
            let _ = db.update_run(run_id, "failed", Some(now_epoch()), None, Some(&e.to_string()), None);
            emit_fail(run_id, e.to_string());
            return;
        }
    }

    let runner_cfg = RunnerConfig {
        binary,
        mode,
        timeout: cfg.timeout,
    };
    let blocked_event = {
        let slug = slug.to_string();
        let sid = session_id.clone();
        move || {
            on_event(IngestEvent::at(
                "ingest",
                &slug,
                run_id,
                Some(sid.clone()),
                "blocked",
                Some(
                    "The session is waiting on something — open it in Chats to answer (it may be a one-time setup prompt), or cancel the run.".into(),
                ),
            ));
        }
    };
    // Live activity: each parsed tool/text line re-emits a transient `running`
    // event carrying the newest activity string and elapsed seconds. Nothing is
    // persisted here — the frontend overwrites its per-slug live marker.
    let started = Instant::now();
    let outcome = {
        let act_emit = on_event.clone();
        let a_slug = slug.to_string();
        let a_sid = session_id.clone();
        runner::run_ingest_session(
            &runner_cfg,
            &project.root,
            &session_id,
            &plan.prompt,
            hooks,
            token,
            // Blocked detection is only live under hidden-TUI; the headless
            // streaming branch ignores on_blocked by design (no startup gates).
            blocked_event,
            move |line: &str| {
                act_emit(IngestEvent {
                    activity: Some(line.to_string()),
                    elapsed_secs: Some(started.elapsed().as_secs()),
                    ..IngestEvent::at("ingest", &a_slug, run_id, Some(a_sid.clone()), "running", None)
                });
            },
        )
    };

    let finish = |db: &mut Db, status: &str, summary: Option<&str>, error: Option<&str>, ratio: Option<f64>| {
        let _ = db.update_run(run_id, status, Some(now_epoch()), summary, error, ratio);
        on_event(IngestEvent::at(
            "ingest",
            slug,
            run_id,
            Some(session_id.clone()),
            status,
            summary.or(error).map(String::from),
        ));
    };

    match outcome {
        Ok(RunOutcome::Completed) => {
            match refresh::evaluate(&project, &recipe, &rules, &plan) {
                Ok(out) if out.applied => {
                    finish(db, "fresh", Some(&out.summary), None, Some(out.change_ratio));
                }
                Ok(out) => {
                    finish(db, "pending_approval", Some(&out.summary), None, Some(out.change_ratio));
                }
                Err(e) => finish(db, "failed", None, Some(&e.to_string()), None),
            }
        }
        Ok(RunOutcome::Cancelled) => {
            let _ = refresh::discard_staged(&project, slug);
            finish(db, "cancelled", Some("Cancelled."), None, None);
        }
        Ok(RunOutcome::TimedOut(tail)) => {
            let _ = refresh::discard_staged(&project, slug);
            let mut msg = format!(
                "The run didn't finish within {} minutes and was stopped.",
                cfg.timeout.as_secs() / 60
            );
            if !tail.is_empty() {
                msg.push_str(&format!(" The session's last output:\n{tail}"));
            }
            finish(db, "failed", None, Some(&msg), None);
        }
        Ok(RunOutcome::Failed(detail)) => {
            let _ = refresh::discard_staged(&project, slug);
            finish(db, "failed", None, Some(&detail), None);
        }
        Err(e) => finish(db, "failed", None, Some(&e.to_string()), None),
    }
}

enum Phase {
    Propose,
    Direct,
    Apply,
}

/// Run one automation through the streaming headless runner. Mirrors
/// `execute_ingest`'s wiring but branches on phase: phase-1 (`Propose`) writes a
/// proposal and stages an `automation-proposal` review item; `Direct`
/// (auto_apply) and `Apply` (approved) both act and record a `fresh` run.
#[allow(clippy::too_many_arguments)]
fn execute_automation(
    project_root: &PathBuf,
    db: &mut Db,
    hooks: &HookListener,
    cfg: &EngineConfig,
    current: &CurrentRun,
    slug: &str,
    job: &PendingJob,
    on_event: &Arc<dyn Fn(IngestEvent) + Send + Sync>,
) {
    let emit_fail = |run_id: i64, detail: String| {
        on_event(IngestEvent::at("automation", slug, run_id, None, "failed", Some(detail)));
    };
    let project = match Project::open(project_root) {
        Ok(p) => p,
        Err(e) => {
            emit_fail(0, e.to_string());
            return;
        }
    };
    let a = match automation::load_slug(&project, slug) {
        Ok(a) => a,
        Err(e) => {
            emit_fail(0, e.to_string());
            return;
        }
    };
    if !a.enabled && job.apply.is_none() {
        return;
    }

    // Matched files: from the queued change, else recompute from the index for
    // a run-now / apply.
    let matched: Vec<String> = if !job.matched.is_empty() {
        job.matched.clone()
    } else {
        db.list_files()
            .map(|files| {
                files
                    .into_iter()
                    .filter(|f| f.status == "indexed")
                    .map(|f| f.rel_path)
                    .filter(|p| automation::triggers(&a, &[p.clone()]).len() == 1)
                    .collect()
            })
            .unwrap_or_default()
    };

    // Fresh staging dir for a proposal file (reuse the refresh staging root).
    let staging = refresh::staging_dir(&project.root, &format!("auto-{slug}"));
    let _ = std::fs::remove_dir_all(&staging);
    if let Err(e) = std::fs::create_dir_all(&staging) {
        emit_fail(0, e.to_string());
        return;
    }

    let phase = if job.apply.is_some() {
        Phase::Apply
    } else if a.auto_apply {
        Phase::Direct
    } else {
        Phase::Propose
    };
    let prompt = match &phase {
        Phase::Apply => automation::apply_prompt(&a, job.apply.as_deref().unwrap_or("")),
        Phase::Direct => automation::direct_prompt(&a, &matched),
        Phase::Propose => automation::proposal_prompt(&a, &matched, &staging),
    };

    let binary = cfg
        .binary
        .clone()
        .or_else(runner::discover_claude)
        .unwrap_or_else(|| PathBuf::from("claude-not-found"));
    let session_id = Uuid::new_v4().to_string();
    let run_id = match db.insert_run_kind(slug, Some(&session_id), now_epoch(), "automation") {
        Ok(id) => id,
        Err(e) => {
            emit_fail(0, e.to_string());
            return;
        }
    };
    on_event(IngestEvent::at(
        "automation",
        slug,
        run_id,
        Some(session_id.clone()),
        "running",
        None,
    ));

    let token = CancelToken::new();
    *current.lock().unwrap() = Some((RunKind::Automation, slug.to_string(), token.clone()));
    let runner_cfg = RunnerConfig {
        binary,
        mode: RunnerMode::Headless,
        timeout: cfg.timeout,
    };
    let started = Instant::now();
    let outcome = {
        let act = on_event.clone();
        let a_slug = slug.to_string();
        let a_sid = session_id.clone();
        runner::run_ingest_session(
            &runner_cfg,
            &project.root,
            &session_id,
            &prompt,
            hooks,
            &token,
            || {}, // headless never blocks on an interactive gate
            move |line: &str| {
                act(IngestEvent {
                    activity: Some(line.to_string()),
                    elapsed_secs: Some(started.elapsed().as_secs()),
                    ..IngestEvent::at(
                        "automation",
                        &a_slug,
                        run_id,
                        Some(a_sid.clone()),
                        "running",
                        None,
                    )
                });
            },
        )
    };
    *current.lock().unwrap() = None;

    let finish = |db: &mut Db, status: &str, summary: Option<&str>, error: Option<&str>| {
        let _ = db.update_run(run_id, status, Some(now_epoch()), summary, error, None);
        on_event(IngestEvent::at(
            "automation",
            slug,
            run_id,
            Some(session_id.clone()),
            status,
            summary.or(error).map(String::from),
        ));
    };

    match outcome {
        Ok(RunOutcome::Completed) => match phase {
            Phase::Propose => {
                let proposal =
                    std::fs::read_to_string(staging.join("proposal.md")).unwrap_or_default();
                let _ = std::fs::remove_dir_all(&staging);
                if proposal.trim().is_empty() {
                    finish(db, "fresh", Some("Checked — nothing to propose."), None);
                    return;
                }
                let payload =
                    serde_json::json!({ "automationSlug": slug, "matched": matched }).to_string();
                let _ = db.insert_review_item(
                    "automation-proposal",
                    &format!("{} — proposal", a.name),
                    &proposal,
                    slug,
                    Some(&payload),
                    now_epoch(),
                );
                finish(db, "fresh", Some("Proposed actions — awaiting your approval."), None);
            }
            Phase::Direct => {
                let _ = std::fs::remove_dir_all(&staging);
                finish(db, "fresh", Some("Ran and applied."), None);
            }
            Phase::Apply => {
                let _ = std::fs::remove_dir_all(&staging);
                finish(db, "fresh", Some("Approved actions carried out."), None);
            }
        },
        Ok(RunOutcome::Cancelled) => {
            let _ = std::fs::remove_dir_all(&staging);
            finish(db, "cancelled", Some("Cancelled."), None);
        }
        Ok(RunOutcome::TimedOut(tail)) => {
            let _ = std::fs::remove_dir_all(&staging);
            finish(
                db,
                "failed",
                None,
                Some(&format!(
                    "The run didn't finish within {} minutes and was stopped.\n{tail}",
                    cfg.timeout.as_secs() / 60
                )),
            );
        }
        Ok(RunOutcome::Failed(d)) => {
            let _ = std::fs::remove_dir_all(&staging);
            finish(db, "failed", None, Some(&d));
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            finish(db, "failed", None, Some(&e.to_string()));
        }
    }
}

/// Apply a held run's staged output (explicit user approval).
pub fn approve_run(project: &Project, db: &mut Db, run_id: i64) -> Result<()> {
    let run = db
        .get_run(run_id)?
        .ok_or_else(|| Error::Other("run not found".into()))?;
    if run.status != "pending_approval" {
        return Err(Error::Other("this run isn't waiting for approval".into()));
    }
    let recipe = recipe::load_slug(project, &run.slug)?;
    let staging = refresh::staging_dir(&project.root, &run.slug);
    let out = refresh::apply_staged(project, &recipe, &staging)?;
    db.update_run(run_id, "fresh", Some(now_epoch()), Some(&out.summary), None, None)?;
    Ok(())
}

/// Throw away a held run's staged output.
pub fn discard_run(project: &Project, db: &mut Db, run_id: i64) -> Result<()> {
    let run = db
        .get_run(run_id)?
        .ok_or_else(|| Error::Other("run not found".into()))?;
    if run.status != "pending_approval" {
        return Err(Error::Other("this run isn't waiting for approval".into()));
    }
    refresh::discard_staged(project, &run.slug)?;
    db.update_run(
        run_id,
        "discarded",
        Some(now_epoch()),
        Some("Discarded without applying."),
        None,
        None,
    )?;
    Ok(())
}

/// Approve a staged automation proposal: resolve the review item and queue the
/// phase-2 "execute exactly these approved actions" run through the engine.
pub fn approve_automation_proposal(engine: &IngestEngine, db: &mut Db, item_id: i64) -> Result<()> {
    let item = db
        .get_review_item(item_id)?
        .ok_or_else(|| Error::Other("proposal not found".into()))?;
    if item.kind != "automation-proposal" || item.status != "open" {
        return Err(Error::Other("this isn't an open automation proposal".into()));
    }
    let slug = item.source_ref.clone();
    db.resolve_review_item(item_id, now_epoch())?;
    engine.apply_automation(&slug, &item.body);
    Ok(())
}

/// Discard a proposal: resolve it, run nothing.
pub fn discard_automation_proposal(db: &mut Db, item_id: i64) -> Result<()> {
    let item = db
        .get_review_item(item_id)?
        .ok_or_else(|| Error::Other("proposal not found".into()))?;
    if item.kind != "automation-proposal" || item.status != "open" {
        return Err(Error::Other("this isn't an open automation proposal".into()));
    }
    db.resolve_review_item(item_id, now_epoch())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipe::{Mode, Recipe, Refresh};
    use crate::runner::test_support::write_fake_claude;
    use crate::scan;
    use std::fs;
    use std::sync::mpsc::Receiver;

    struct Rig {
        _project_dir: tempfile::TempDir,
        _app_dir: tempfile::TempDir,
        project: Project,
        db_path: PathBuf,
        engine: IngestEngine,
        events: Receiver<IngestEvent>,
    }

    fn rig(behavior: &str) -> Rig {
        rig_debounce(behavior, 200)
    }

    fn rig_debounce(behavior: &str, debounce_ms: u64) -> Rig {
        let project_dir = tempfile::tempdir().unwrap();
        let app_dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(project_dir.path().join("notes")).unwrap();
        fs::write(project_dir.path().join("notes/a.md"), "# A\nPriya owns billing.\n").unwrap();
        let project = Project::create(project_dir.path(), "T").unwrap();

        let recipe = Recipe {
            slug: "people".into(),
            name: "People".into(),
            description: String::new(),
            sources: vec!["notes".into()],
            output: "knowledge/People.md".into(),
            mode: Mode::Single,
            refresh: Refresh::OnChange,
            rules: None,
            instruction: "Extract people.".into(),
            extra: Default::default(),
        };
        recipe::save(&project, &recipe).unwrap();

        // A second recipe so two ingests can contend for the single worker.
        let places = Recipe {
            slug: "places".into(),
            name: "Places".into(),
            description: String::new(),
            sources: vec!["notes".into()],
            output: "knowledge/Places.md".into(),
            mode: Mode::Single,
            refresh: Refresh::OnChange,
            rules: None,
            instruction: "Extract places.".into(),
            extra: Default::default(),
        };
        recipe::save(&project, &places).unwrap();

        let db_path = app_dir.path().join("test.db");
        let mut db = Db::open_at(&db_path).unwrap();
        scan::scan(&project, &mut db).unwrap();
        drop(db);

        let bin = write_fake_claude(app_dir.path(), behavior);
        let hooks = Arc::new(HookListener::start().unwrap());
        let (etx, events) = channel();
        let engine = IngestEngine::start(
            project.root.clone(),
            db_path.clone(),
            hooks,
            EngineConfig {
                binary: Some(bin),
                timeout: Duration::from_secs(30),
                debounce: Duration::from_millis(debounce_ms),
            },
            move |ev| {
                let _ = etx.send(ev);
            },
        )
        .unwrap();

        Rig {
            _project_dir: project_dir,
            _app_dir: app_dir,
            project,
            db_path,
            engine,
            events,
        }
    }

    fn rig_with_automation(behavior: &str, auto_apply: bool) -> Rig {
        rig_with_automation_globs(behavior, auto_apply, vec!["notes/*.md".into()])
    }

    /// An automation with explicit globs — used to watch a folder the rig's two
    /// recipes (which watch `notes/`) do NOT, so a `SourcesChanged` can trigger
    /// the automation in isolation.
    fn rig_with_automation_globs(behavior: &str, auto_apply: bool, globs: Vec<String>) -> Rig {
        let r = rig(behavior);
        let a = crate::automation::Automation {
            slug: "weekly".into(),
            name: "Weekly".into(),
            globs,
            prompt: "Summarize notes and propose follow-up tasks.".into(),
            auto_apply,
            enabled: true,
            extra: serde_yaml::Mapping::new(),
        };
        crate::automation::save(&r.project, &a).unwrap();
        r
    }

    fn wait_status(events: &Receiver<IngestEvent>, wanted: &str, secs: u64) -> IngestEvent {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            if let Ok(ev) = events.recv_timeout(Duration::from_millis(200)) {
                if ev.status == wanted {
                    return ev;
                }
            }
        }
        panic!("never saw status {wanted}");
    }

    /// Like `wait_status` but also requires the event's `kind` — the engine
    /// interleaves ingest and automation events, so tests that care which
    /// subsystem produced a status must match on both.
    fn wait_kind_status(
        events: &Receiver<IngestEvent>,
        kind: &str,
        wanted: &str,
        secs: u64,
    ) -> IngestEvent {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            if let Ok(ev) = events.recv_timeout(Duration::from_millis(200)) {
                if ev.kind == kind && ev.status == wanted {
                    return ev;
                }
            }
        }
        panic!("never saw {kind} status {wanted}");
    }

    #[test]
    fn default_debounce_is_ten_seconds() {
        assert_eq!(EngineConfig::default().debounce, Duration::from_secs(10));
    }

    #[test]
    fn source_change_emits_queued_with_eta() {
        let r = rig_debounce("complete", 800);
        r.engine.sources_changed(vec!["notes/a.md".into()]);
        let q = wait_status(&r.events, "queued", 5);
        assert_eq!(q.kind, "ingest");
        assert_eq!(q.slug, "people");
        assert!(q.eta_secs.unwrap_or(0) >= 1, "eta should be ~1s for an 800ms debounce");
        // And it still runs after the debounce.
        wait_status(&r.events, "fresh", 20);
    }

    #[test]
    fn second_due_job_reports_waiting() {
        // A slow first run so the second stays blocked long enough to observe.
        let r = rig("stream-hang"); // fake sleeps 300s in streamed headless
        r.engine.trigger("people", true);
        wait_status(&r.events, "running", 15);
        // A second run-now can't start while the single worker is busy: expect
        // a `waiting` event that names the running job.
        r.engine.trigger("places", true);
        let w = wait_status(&r.events, "waiting", 8);
        assert!(w.detail.unwrap_or_default().contains("people"));
        r.engine.cancel(RunKind::Ingest, "people");
    }

    #[test]
    fn ingest_event_constructor_defaults_are_none() {
        let ev = IngestEvent::at("ingest", "people", 5, Some("s".into()), "running", None);
        assert_eq!(ev.kind, "ingest");
        assert!(ev.activity.is_none() && ev.elapsed_secs.is_none() && ev.eta_secs.is_none());
    }

    #[test]
    fn noop_run_is_recorded_not_silent() {
        let r = rig("complete");
        {
            // A prior success in the far future so plan() finds nothing changed.
            let mut db = Db::open_at(&r.db_path).unwrap();
            let id = db.insert_run("people", None, now_epoch() + 10_000).unwrap();
            db.update_run(id, "fresh", Some(now_epoch()), None, None, None).unwrap();
        }
        // force_full = false: incremental, nothing newer than last success.
        r.engine.trigger("people", false);
        let done = wait_status(&r.events, "fresh", 20);
        assert_eq!(done.detail.as_deref(), Some("Checked — nothing to update."));
        let db = Db::open_at(&r.db_path).unwrap();
        // Two runs now exist: the seeded success and the recorded no-op.
        assert!(db.list_runs("people", 5).unwrap().len() >= 2);
    }

    #[test]
    fn trigger_runs_and_applies_first_run() {
        let r = rig("complete");
        r.engine.trigger("people", false);
        wait_status(&r.events, "running", 15);
        let done = wait_status(&r.events, "fresh", 20);
        assert!(done.detail.unwrap().contains("Updated 1 document"));
        assert!(r.project.root.join("knowledge/People.md").is_file());

        let db = Db::open_at(&r.db_path).unwrap();
        let runs = db.list_runs("people", 5).unwrap();
        assert_eq!(runs[0].status, "fresh");
        assert!(runs[0].change_ratio.is_some());
    }

    #[test]
    fn sources_changed_triggers_but_own_output_does_not() {
        let r = rig("complete");
        // Own output path: nothing should run.
        r.engine.sources_changed(vec!["knowledge/People.md".into()]);
        std::thread::sleep(Duration::from_millis(600));
        assert!(r.events.try_recv().is_err(), "own output must not trigger");

        // A real source change runs after the debounce.
        r.engine.sources_changed(vec!["notes/a.md".into()]);
        wait_status(&r.events, "fresh", 20);
    }

    #[test]
    fn over_threshold_holds_then_approve_applies() {
        let r = rig("complete");
        // Existing large output very different from what the fake writes,
        // and a prior success so it isn't a first run.
        fs::create_dir_all(r.project.root.join("knowledge")).unwrap();
        let existing: String = (0..40).map(|i| format!("existing line {i}\n")).collect();
        fs::write(r.project.root.join("knowledge/People.md"), existing).unwrap();
        {
            let mut db = Db::open_at(&r.db_path).unwrap();
            let id = db.insert_run("people", None, 1).unwrap();
            db.update_run(id, "fresh", None, None, None, None).unwrap();
            scan::scan(&r.project, &mut db).unwrap();
        }

        r.engine.trigger("people", true);
        let held = wait_status(&r.events, "pending_approval", 20);
        assert!(held.detail.unwrap().contains("Held for your review"));
        // Output untouched.
        assert!(fs::read_to_string(r.project.root.join("knowledge/People.md"))
            .unwrap()
            .contains("existing line 0"));

        let mut db = Db::open_at(&r.db_path).unwrap();
        approve_run(&r.project, &mut db, held.run_id).unwrap();
        assert!(fs::read_to_string(r.project.root.join("knowledge/People.md"))
            .unwrap()
            .contains("Priya Natarajan"));
        assert_eq!(db.get_run(held.run_id).unwrap().unwrap().status, "fresh");
    }

    /// Live test against the real Claude CLI — run explicitly with
    /// `cargo test -p ken-core real_claude -- --ignored --nocapture`.
    /// Uses the user's local auth; headless mode (no trust dialog).
    #[test]
    #[ignore]
    fn real_claude_end_to_end() {
        let Some(binary) = crate::runner::discover_claude() else {
            panic!("claude CLI not found");
        };
        let project_dir = tempfile::tempdir().unwrap();
        let app_dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(project_dir.path().join("notes")).unwrap();
        fs::write(
            project_dir.path().join("notes/standup.md"),
            "# Standup July 11\nPriya Natarajan confirmed vendor sign-off; she owns the billing cutover.\nMarcus Chen is her backup and runs the rollback rehearsal.\n",
        )
        .unwrap();
        let project = Project::create(project_dir.path(), "Live").unwrap();
        let mut headless = project.clone();
        headless
            .config
            .extra
            .insert("ingestRunner".into(), serde_json::json!("headless"));
        headless.save().unwrap();

        let recipe = Recipe::build(
            "people".into(),
            "People".into(),
            String::new(),
            vec!["notes".into()],
            "knowledge/People.md".into(),
            Mode::Single,
            Refresh::Manual,
            None,
            "Extract every person mentioned. For each: name, role, what they own.".into(),
        );
        recipe::save(&project, &recipe).unwrap();

        let db_path = app_dir.path().join("live.db");
        let mut db = Db::open_at(&db_path).unwrap();
        scan::scan(&project, &mut db).unwrap();
        drop(db);

        let hooks = Arc::new(HookListener::start().unwrap());
        let (etx, events) = channel();
        let engine = IngestEngine::start(
            project.root.clone(),
            db_path.clone(),
            hooks,
            EngineConfig {
                binary: Some(binary),
                timeout: Duration::from_secs(300),
                debounce: Duration::from_millis(100),
            },
            move |ev| {
                eprintln!("[event] {} -> {} {:?}", ev.slug, ev.status, ev.detail);
                let _ = etx.send(ev);
            },
        )
        .unwrap();

        engine.trigger("people", true);
        let done = wait_status(&events, "fresh", 300);
        eprintln!("first run: {:?}", done.detail);
        let output = fs::read_to_string(project.root.join("knowledge/People.md")).unwrap();
        eprintln!("--- People.md ---\n{output}\n-----------------");
        assert!(output.to_lowercase().contains("priya"), "{output}");
        assert!(output.to_lowercase().contains("marcus"), "{output}");

        // Incremental second run: new person appears in a new note.
        std::thread::sleep(Duration::from_secs(1));
        fs::write(
            project.root.join("notes/vendor.md"),
            "# Vendor call\nDana Whitfield from LangdonSoft handles the contract renewal.\n",
        )
        .unwrap();
        let future = std::time::SystemTime::now() + Duration::from_secs(60);
        fs::File::options()
            .write(true)
            .open(project.root.join("notes/vendor.md"))
            .unwrap()
            .set_modified(future)
            .unwrap();
        {
            let mut db = Db::open_at(&db_path).unwrap();
            scan::scan(&project, &mut db).unwrap();
        }
        engine.trigger("people", false);
        let done2 = wait_status(&events, "fresh", 300);
        eprintln!("second run: {:?}", done2.detail);
        let output2 = fs::read_to_string(project.root.join("knowledge/People.md")).unwrap();
        eprintln!("--- People.md v2 ---\n{output2}\n--------------------");
        assert!(output2.to_lowercase().contains("dana"), "{output2}");
        assert!(output2.to_lowercase().contains("priya"), "second run must preserve existing entries: {output2}");
    }

    #[test]
    fn running_event_carries_live_activity() {
        let r = rig("complete");
        r.engine.trigger("people", true);
        wait_status(&r.events, "running", 15);
        // At least one running event should carry an activity line before done.
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut saw_activity = false;
        while Instant::now() < deadline {
            if let Ok(ev) = r.events.recv_timeout(Duration::from_millis(200)) {
                if ev.status == "running" && ev.activity.as_deref().map(|a| a.contains("notes/a.md")).unwrap_or(false) {
                    saw_activity = true;
                }
                if ev.status == "fresh" { break; }
            }
        }
        assert!(saw_activity, "no running event carried an activity line");
    }

    #[test]
    fn failed_run_reports_detail() {
        // Ingests use the streaming headless path; `stream-fail` emits an
        // error terminal result and a non-zero exit, which must surface as a
        // failed run carrying diagnostic detail.
        let r = rig("stream-fail");
        r.engine.trigger("people", false);
        let failed = wait_status(&r.events, "failed", 20);
        assert!(failed.detail.unwrap().contains("exited"));
    }

    #[test]
    fn discard_leaves_output_untouched() {
        let r = rig("complete");
        fs::create_dir_all(r.project.root.join("knowledge")).unwrap();
        let existing: String = (0..40).map(|i| format!("keep line {i}\n")).collect();
        fs::write(r.project.root.join("knowledge/People.md"), &existing).unwrap();
        {
            let mut db = Db::open_at(&r.db_path).unwrap();
            let id = db.insert_run("people", None, 1).unwrap();
            db.update_run(id, "fresh", None, None, None, None).unwrap();
            scan::scan(&r.project, &mut db).unwrap();
        }

        r.engine.trigger("people", true);
        let held = wait_status(&r.events, "pending_approval", 20);

        let mut db = Db::open_at(&r.db_path).unwrap();
        discard_run(&r.project, &mut db, held.run_id).unwrap();
        assert_eq!(
            fs::read_to_string(r.project.root.join("knowledge/People.md")).unwrap(),
            existing
        );
        assert_eq!(db.get_run(held.run_id).unwrap().unwrap().status, "discarded");
        assert!(!refresh::staging_dir(&r.project.root, "people").exists());
    }

    #[test]
    fn automation_phase1_stages_a_proposal_review_item() {
        let r = rig_with_automation("complete", false);
        r.engine.run_automation("weekly");
        // Automation run logs as an automation-kind run and completes.
        let done = wait_status(&r.events, "fresh", 20);
        assert_eq!(done.kind, "automation");
        let db = Db::open_at(&r.db_path).unwrap();
        // A proposal review item was staged.
        let items = db.list_open_review_items().unwrap();
        let prop = items.iter().find(|i| i.kind == "automation-proposal").expect("proposal");
        assert!(prop.body.contains("Proposed actions"));
        assert_eq!(prop.source_ref, "weekly");
        // The automation run itself is recorded under kind=automation.
        assert!(!db.list_runs_of_kind("weekly", "automation", 5).unwrap().is_empty());
    }

    #[test]
    fn automation_auto_apply_runs_single_session_to_fresh() {
        let r = rig_with_automation("complete", true);
        r.engine.run_automation("weekly");
        let done = wait_status(&r.events, "fresh", 20);
        assert_eq!(done.kind, "automation");
        let db = Db::open_at(&r.db_path).unwrap();
        // No proposal item for an auto-apply automation.
        assert!(db.list_open_review_items().unwrap().iter().all(|i| i.kind != "automation-proposal"));
    }

    #[test]
    fn automation_apply_phase_runs_from_proposal() {
        let r = rig_with_automation("complete", false);
        r.engine.apply_automation("weekly", "## Proposed actions\n- do the thing");
        let done = wait_status(&r.events, "fresh", 20);
        assert_eq!(done.kind, "automation");
    }

    #[test]
    fn approve_proposal_resolves_item_and_queues_apply() {
        let r = rig_with_automation("complete", false);
        r.engine.run_automation("weekly");
        wait_status(&r.events, "fresh", 20);
        let item_id = {
            let db = Db::open_at(&r.db_path).unwrap();
            db.list_open_review_items().unwrap().iter()
                .find(|i| i.kind == "automation-proposal").unwrap().id
        };
        let mut db = Db::open_at(&r.db_path).unwrap();
        approve_automation_proposal(&r.engine, &mut db, item_id).unwrap();
        // Item resolved…
        assert_eq!(db.get_review_item(item_id).unwrap().unwrap().status, "resolved");
        // …and a phase-2 automation run happens.
        let done = wait_status(&r.events, "fresh", 20);
        assert_eq!(done.kind, "automation");
    }

    #[test]
    fn discard_proposal_just_resolves() {
        let r = rig_with_automation("complete", false);
        r.engine.run_automation("weekly");
        wait_status(&r.events, "fresh", 20);
        let mut db = Db::open_at(&r.db_path).unwrap();
        let id = db.list_open_review_items().unwrap().iter()
            .find(|i| i.kind == "automation-proposal").unwrap().id;
        discard_automation_proposal(&mut db, id).unwrap();
        assert_eq!(db.get_review_item(id).unwrap().unwrap().status, "resolved");
    }

    #[test]
    fn source_change_matching_an_automation_runs_it_and_stages_a_proposal() {
        // Watch a folder the recipes don't, so only the automation reacts.
        let r = rig_with_automation_globs("complete", false, vec!["Recordings/*.md".into()]);
        r.engine.sources_changed(vec!["Recordings/2026-07-13 14.02 Recording.md".into()]);
        // The automation runs (kind=automation) and stages a proposal.
        let done = wait_kind_status(&r.events, "automation", "fresh", 20);
        assert_eq!(done.slug, "weekly");
        let db = Db::open_at(&r.db_path).unwrap();
        let items = db.list_open_review_items().unwrap();
        let prop = items
            .iter()
            .find(|i| i.kind == "automation-proposal")
            .expect("a proposal was staged by the file-change trigger");
        assert_eq!(prop.source_ref, "weekly");
        // The matched path is carried on the proposal payload (the union).
        assert!(prop.payload.as_deref().unwrap_or("").contains("Recordings/2026-07-13 14.02 Recording.md"));
    }

    #[test]
    fn source_change_not_matching_an_automation_does_not_run_it() {
        // Automation watches Recordings/; a change under notes/ must not fire it
        // (the recipes will fire, but no automation-kind event may appear).
        let r = rig_with_automation_globs("complete", false, vec!["Recordings/*.md".into()]);
        r.engine.sources_changed(vec!["notes/a.md".into()]);
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Ok(ev) = r.events.recv_timeout(Duration::from_millis(200)) {
                assert_ne!(ev.kind, "automation", "a non-matching path must not trigger the automation");
            }
        }
    }

    #[test]
    fn cancel_is_kind_aware() {
        // A slow ingest named "people" is running; cancelling the *automation*
        // "people" must NOT stop it — only the ingest-kind cancel does.
        let r = rig("stream-hang");
        r.engine.trigger("people", true);
        wait_status(&r.events, "running", 15);
        r.engine.cancel(RunKind::Automation, "people"); // wrong kind — no-op
        // No cancellation should arrive in the next moment.
        let quiet = Instant::now() + Duration::from_millis(600);
        while Instant::now() < quiet {
            if let Ok(ev) = r.events.recv_timeout(Duration::from_millis(100)) {
                assert_ne!(ev.status, "cancelled", "wrong-kind cancel must not stop the ingest");
            }
        }
        // The matching-kind cancel does stop it.
        r.engine.cancel(RunKind::Ingest, "people");
        wait_status(&r.events, "cancelled", 10);
    }
}

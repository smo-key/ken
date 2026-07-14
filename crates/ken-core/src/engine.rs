//! Ingest engine: owns the run queue for one project. Watches for source
//! changes (fed by the app layer from scan results), debounces, and runs
//! one ingest at a time through the runner + refresh pipeline.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use uuid::Uuid;

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

enum Msg {
    Trigger { slug: String, force_full: bool },
    SourcesChanged(Vec<String>),
    Shutdown,
}

pub struct IngestEngine {
    tx: Sender<Msg>,
    current: Arc<Mutex<Option<(String, CancelToken)>>>,
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
        on_event: impl Fn(IngestEvent) + Send + 'static,
    ) -> Result<IngestEngine> {
        let (tx, rx) = channel::<Msg>();
        let current: Arc<Mutex<Option<(String, CancelToken)>>> =
            Arc::new(Mutex::new(None));
        let current_thread = current.clone();

        let thread = std::thread::spawn(move || {
            let Ok(mut db) = Db::open_at(&db_path) else { return };
            let mut pending: HashMap<String, Instant> = HashMap::new();
            let mut force: HashMap<String, bool> = HashMap::new();

            loop {
                match rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(Msg::Trigger { slug, force_full }) => {
                        pending.insert(slug.clone(), Instant::now());
                        force.insert(slug, force_full);
                    }
                    Ok(Msg::SourcesChanged(paths)) => {
                        if let Ok(project) = Project::open(&project_root) {
                            if let Ok(entries) = recipe::list(&project) {
                                for entry in entries {
                                    if let recipe::RecipeEntry::Ok { recipe: r } = entry {
                                        if r.refresh == Refresh::OnChange
                                            && refresh::triggers(&r, &paths)
                                        {
                                            // Keep the earliest deadline so a
                                            // steady stream can't postpone the
                                            // run forever.
                                            pending
                                                .entry(r.slug.clone())
                                                .or_insert_with(|| Instant::now() + cfg.debounce);
                                            force.entry(r.slug).or_insert(false);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Ok(Msg::Shutdown) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        break;
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                }

                let running = current_thread.lock().unwrap().is_some();
                if !running {
                    let due: Option<String> = pending
                        .iter()
                        .filter(|(_, at)| **at <= Instant::now())
                        .map(|(slug, _)| slug.clone())
                        .next();
                    if let Some(slug) = due {
                        pending.remove(&slug);
                        let force_full = force.remove(&slug).unwrap_or(false);
                        execute(
                            &project_root,
                            &mut db,
                            &hooks,
                            &cfg,
                            &current_thread,
                            &slug,
                            force_full,
                            &on_event,
                        );
                    }
                }
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
            slug: slug.to_string(),
            force_full,
        });
    }

    pub fn sources_changed(&self, paths: Vec<String>) {
        let _ = self.tx.send(Msg::SourcesChanged(paths));
    }

    /// Cancel the currently running ingest if it matches `slug`.
    pub fn cancel(&self, slug: &str) {
        if let Some((running_slug, token)) = self.current.lock().unwrap().as_ref() {
            if running_slug == slug {
                token.cancel();
            }
        }
    }
}

impl Drop for IngestEngine {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Shutdown);
        if let Some((_, token)) = self.current.lock().unwrap().as_ref() {
            token.cancel();
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn execute(
    project_root: &PathBuf,
    db: &mut Db,
    hooks: &HookListener,
    cfg: &EngineConfig,
    current: &Arc<Mutex<Option<(String, CancelToken)>>>,
    slug: &str,
    force_full: bool,
    on_event: &impl Fn(IngestEvent),
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

    let token = CancelToken::new();
    *current.lock().unwrap() = Some((slug.to_string(), token.clone()));

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
    let outcome = runner::run_session(
        &runner_cfg,
        &project.root,
        &session_id,
        &plan.prompt,
        hooks,
        &token,
        blocked_event,
    );
    *current.lock().unwrap() = None;

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
                debounce: Duration::from_millis(200),
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

    #[test]
    fn default_debounce_is_ten_seconds() {
        assert_eq!(EngineConfig::default().debounce, Duration::from_secs(10));
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
    fn failed_run_reports_detail() {
        let r = rig("fail");
        r.engine.trigger("people", false);
        let failed = wait_status(&r.events, "failed", 20);
        assert!(failed.detail.unwrap().contains("boom"));
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
}

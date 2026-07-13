//! Sync engine: keeps a shared project folder in step with the team.
//! Git-backed projects are driven actively — pull on focus, debounced
//! commit + push after saves — by shelling out to the user's `git` binary
//! (their config, their credentials; never a git library, never rebase,
//! never force). Non-git projects get passive shared-drive damage
//! detection: conflicted-copy filenames become Review items. Every
//! conflict either path produces is filed into the stored review-item
//! substrate with an AI-drafted merge, and surfaced through a
//! plain-language sync state — the user never sees git vocabulary.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;

use crate::db::Db;
use crate::engine::now_epoch;
use crate::project::Project;
use crate::{Error, Result};

pub const COMMIT_MESSAGE: &str = "Ken: update knowledge";

/// Ken's transient files, kept out of the user's repository via
/// `.git/info/exclude` (non-invasive: no tracked file is touched).
const EXCLUDE_ENTRIES: &[&str] = &[".ken/.staging/", ".claude/settings.local.json"];

// ---------- git primitives ----------

struct GitOut {
    ok: bool,
    stdout: String,
    stderr: String,
}

fn run_git(root: &Path, args: &[&str]) -> Result<GitOut> {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        // Never hang on a credential prompt — fail into the attention state.
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| Error::Other(format!("could not run git: {e}")))?;
    Ok(GitOut {
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// Short human-readable detail from a failed git command.
fn short_detail(out: &GitOut) -> String {
    let text = if out.stderr.trim().is_empty() {
        out.stdout.trim()
    } else {
        out.stderr.trim()
    };
    let mut s: String = text.lines().map(str::trim).filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    if s.len() > 240 {
        let mut end = 240;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
        s.push('…');
    }
    s
}

pub fn is_git_repo(root: &Path) -> bool {
    root.join(".git").exists()
}

/// First remote (if any) and current branch of the repository.
pub fn remote_and_branch(root: &Path) -> (Option<String>, Option<String>) {
    let remote = run_git(root, &["remote"])
        .ok()
        .filter(|o| o.ok)
        .and_then(|o| o.stdout.lines().next().map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty());
    let branch = run_git(root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .filter(|o| o.ok)
        .map(|o| o.stdout.trim().to_string())
        .filter(|s| !s.is_empty() && s != "HEAD");
    (remote, branch)
}

/// Project-level sync toggle: `project.json` extra `"sync": {"auto": bool}`.
/// Defaults to on.
pub fn sync_auto(project: &Project) -> bool {
    project
        .config
        .extra
        .get("sync")
        .and_then(|v| v.get("auto"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
}

/// Is active git sync in effect for this root? (git repo + remote + auto on)
pub fn sync_active(root: &Path) -> bool {
    if !is_git_repo(root) {
        return false;
    }
    let auto = Project::open(root).map(|p| sync_auto(&p)).unwrap_or(true);
    auto && remote_and_branch(root).0.is_some()
}

/// Idempotently keep Ken's transient files out of the repo via
/// `.git/info/exclude`. No-op for non-repos.
pub fn ensure_excludes(root: &Path) -> Result<()> {
    if !is_git_repo(root) {
        return Ok(());
    }
    let info = root.join(".git").join("info");
    std::fs::create_dir_all(&info).map_err(|e| Error::io(&info, e))?;
    let path = info.join("exclude");
    let current = std::fs::read_to_string(&path).unwrap_or_default();
    let mut out = current.clone();
    for entry in EXCLUDE_ENTRIES {
        if !current.lines().any(|l| l.trim() == *entry) {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(entry);
            out.push('\n');
        }
    }
    if out != current {
        std::fs::write(&path, out).map_err(|e| Error::io(&path, e))?;
    }
    Ok(())
}

/// Stage and commit everything. Returns true when a commit was made,
/// false when there was nothing to commit.
pub fn commit_all(root: &Path) -> Result<bool> {
    let add = run_git(root, &["add", "-A"])?;
    if !add.ok {
        return Err(Error::Other(short_detail(&add)));
    }
    // Exit 0 = nothing staged; exit 1 = staged changes exist.
    let staged = run_git(root, &["diff", "--cached", "--quiet"])?;
    if staged.ok {
        return Ok(false);
    }
    let commit = run_git(root, &["commit", "-m", COMMIT_MESSAGE])?;
    if !commit.ok {
        return Err(Error::Other(short_detail(&commit)));
    }
    Ok(true)
}

#[derive(Debug, Clone, PartialEq)]
pub enum PushOutcome {
    Pushed,
    NoRemote,
    Failed(String),
}

pub fn push(root: &Path) -> Result<PushOutcome> {
    if remote_and_branch(root).0.is_none() {
        return Ok(PushOutcome::NoRemote);
    }
    let out = run_git(root, &["push"])?;
    if out.ok {
        Ok(PushOutcome::Pushed)
    } else {
        Ok(PushOutcome::Failed(short_detail(&out)))
    }
}

/// One conflicted file from an attempted pull: both full versions,
/// captured before the merge was aborted.
#[derive(Debug, Clone, PartialEq)]
pub struct ConflictInfo {
    pub path: String,
    pub ours: String,
    pub theirs: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PullOutcome {
    Clean,
    /// The merge conflicted and was aborted — the working tree is exactly
    /// as it was before the pull. Carries the unresolved files' versions.
    Conflicts(Vec<ConflictInfo>),
    Failed(String),
}

fn show_stage(root: &Path, stage: u8, path: &str) -> String {
    run_git(root, &["show", &format!(":{stage}:{path}")])
        .ok()
        .filter(|o| o.ok)
        .map(|o| o.stdout)
        .unwrap_or_default()
}

/// Pull from the remote (merge, never rebase). On conflict, files for
/// which `keep_ours` returns true are auto-resolved to our version — used
/// after the user has already chosen a resolution in Review, so their
/// choice wins and the merge completes. If any conflicted file is
/// unresolved, the whole merge is aborted (working tree restored) and the
/// unresolved files' versions are returned.
pub fn pull(root: &Path, keep_ours: impl Fn(&str) -> bool) -> Result<PullOutcome> {
    let out = run_git(root, &["pull", "--no-rebase", "--no-edit"])?;
    if out.ok {
        return Ok(PullOutcome::Clean);
    }
    let unmerged = run_git(root, &["diff", "--name-only", "--diff-filter=U"])?;
    let paths: Vec<String> = unmerged
        .stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect();
    if paths.is_empty() {
        // Not a conflict — network, auth, identity…
        return Ok(PullOutcome::Failed(short_detail(&out)));
    }
    if paths.iter().all(|p| keep_ours(p)) {
        // Every conflicted file already carries the user's chosen content
        // on our side — keep it and finish the merge.
        let mut ok = true;
        for p in &paths {
            ok &= run_git(root, &["checkout", "--ours", "--", p])?.ok;
            ok &= run_git(root, &["add", "--", p])?.ok;
        }
        if ok {
            let commit = run_git(root, &["commit", "--no-edit"])?;
            if commit.ok {
                return Ok(PullOutcome::Clean);
            }
        }
        let _ = run_git(root, &["merge", "--abort"]);
        return Ok(PullOutcome::Failed(
            "couldn't finish combining the changes".into(),
        ));
    }
    let mut conflicts = Vec::new();
    for p in &paths {
        if keep_ours(p) {
            continue; // already resolved; waits for the rest
        }
        conflicts.push(ConflictInfo {
            path: p.clone(),
            ours: show_stage(root, 2, p),
            theirs: show_stage(root, 3, p),
        });
    }
    let _ = run_git(root, &["merge", "--abort"]);
    Ok(PullOutcome::Conflicts(conflicts))
}

// ---------- conflicted-copy detection (shared drives) ----------

/// If `file_name` looks like a sync service's conflicted copy — a
/// parenthesized marker segment containing "conflicted copy" (Dropbox,
/// incl. "(Bob's conflicted copy 2026-07-12)") or starting with "case
/// conflict" — return the original file name with the segment stripped.
/// Parentheses are required: bare lookalikes ("conflicted-copy-analysis")
/// never match.
pub fn conflicted_copy_original(file_name: &str) -> Option<String> {
    let mut start: Option<usize> = None;
    for (i, c) in file_name.char_indices() {
        if c == '(' {
            start = Some(i);
        } else if c == ')' {
            if let Some(s) = start.take() {
                let inner = file_name[s + 1..i].to_lowercase();
                if inner.contains("conflicted copy")
                    || inner.trim_start().starts_with("case conflict")
                {
                    let head = file_name[..s].trim_end();
                    let tail = &file_name[i + 1..];
                    let original = format!("{head}{tail}").trim().to_string();
                    if original.is_empty() {
                        return None;
                    }
                    return Some(original);
                }
            }
        }
    }
    None
}

// ---------- AI merge draft ----------

/// Draft a merged version of a conflicted file with the local Claude CLI
/// (headless `claude -p`, same shape as the ingest runner). The agent
/// writes the complete merged document into a staging path which is read
/// back and cleaned up. Returns None when no draft was produced.
pub fn draft_merge(
    binary: &Path,
    root: &Path,
    rel_path: &str,
    ours: &str,
    theirs: &str,
    timeout: Duration,
    cancel: &AtomicBool,
) -> Result<Option<String>> {
    let staging = root
        .join(".ken/.staging/sync-merge")
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&staging).map_err(|e| Error::io(&staging, e))?;
    let prompt = format!(
        r#"You are Ken's sync assistant. Two people edited the same document at the same time, and their versions need to be combined into one.

## Version A — this computer

{ours}

## Version B — teammate

{theirs}

## What to do

Produce ONE merged document that preserves both sides' intent. Keep every fact and edit from both versions unless they directly contradict; where they contradict, prefer the more recent or more specific statement. Output the COMPLETE merged document — no commentary, no diff markers.

## Where to write

STAGING_DIR={staging}
Write ONLY the merged file to `{staging}/{rel_path}`. Do not modify any other file.
"#,
        staging = staging.display(),
    );

    let session_id = uuid::Uuid::new_v4().to_string();
    let spawned = Command::new(binary)
        .args([
            "-p",
            &prompt,
            "--output-format",
            "json",
            "--permission-mode",
            "acceptEdits",
            "--session-id",
            &session_id,
        ])
        .current_dir(root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(Error::Other(format!("spawn {}: {e}", binary.display())));
        }
    };

    let deadline = Instant::now() + timeout;
    loop {
        if cancel.load(Ordering::Relaxed) || Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            break;
        }
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => std::thread::sleep(Duration::from_millis(150)),
            Err(_) => break,
        }
    }

    let content = std::fs::read_to_string(staging.join(rel_path))
        .ok()
        .filter(|c| !c.trim().is_empty());
    let _ = std::fs::remove_dir_all(&staging);
    Ok(content)
}

// ---------- sync engine ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncState {
    Off,
    Synced,
    Syncing,
    Attention,
}

impl SyncState {
    pub fn as_str(&self) -> &'static str {
        match self {
            SyncState::Off => "off",
            SyncState::Synced => "synced",
            SyncState::Syncing => "syncing",
            SyncState::Attention => "attention",
        }
    }
}

/// What the engine tells the app layer: a state change for the title-bar
/// dot, or "review items changed, refresh the inbox".
#[derive(Debug, Clone)]
pub enum SyncNotice {
    State {
        state: SyncState,
        detail: Option<String>,
    },
    ReviewChanged,
}

#[derive(Clone)]
pub struct SyncConfig {
    /// Explicit Claude CLI path for merge drafts (tests); None = discover.
    pub binary: Option<PathBuf>,
    /// At most one focus-triggered pull per this window.
    pub pull_throttle: Duration,
    /// Push this long after the last watcher-detected change batch
    /// (earliest deadline — a steady stream can't postpone forever).
    pub push_debounce: Duration,
    pub draft_timeout: Duration,
}

impl Default for SyncConfig {
    fn default() -> Self {
        SyncConfig {
            binary: None,
            pull_throttle: Duration::from_secs(60),
            push_debounce: Duration::from_secs(30),
            draft_timeout: Duration::from_secs(5 * 60),
        }
    }
}

enum Msg {
    /// Focus / activation: pull (throttled), then push anything local.
    PullNow,
    /// Explicit "Sync now": full cycle, throttle bypassed.
    SyncNow,
    /// Watcher-detected changed paths: conflicted-copy scan + push debounce.
    Changed(Vec<String>),
    Shutdown,
}

pub struct SyncEngine {
    tx: Sender<Msg>,
    cancel: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    draft_thread: Option<std::thread::JoinHandle<()>>,
}

type Notify = Arc<dyn Fn(SyncNotice) + Send + Sync>;

impl SyncEngine {
    pub fn start(
        project_root: PathBuf,
        db_path: PathBuf,
        cfg: SyncConfig,
        on_notice: impl Fn(SyncNotice) + Send + Sync + 'static,
    ) -> Result<SyncEngine> {
        let notify: Notify = Arc::new(on_notice);
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel::<Msg>();
        let (draft_tx, draft_rx) = channel::<i64>();

        let worker_root = project_root.clone();
        let worker_db = db_path.clone();
        let worker_cfg = cfg.clone();
        let worker_notify = notify.clone();
        let worker_cancel = cancel.clone();
        let draft_thread = std::thread::spawn(move || {
            draft_worker(draft_rx, worker_root, worker_db, worker_cfg, worker_cancel, worker_notify)
        });

        let thread = std::thread::spawn(move || {
            engine_loop(rx, draft_tx, project_root, db_path, cfg, notify)
        });

        Ok(SyncEngine {
            tx,
            cancel,
            thread: Some(thread),
            draft_thread: Some(draft_thread),
        })
    }

    pub fn pull_now(&self) {
        let _ = self.tx.send(Msg::PullNow);
    }

    pub fn sync_now(&self) {
        let _ = self.tx.send(Msg::SyncNow);
    }

    pub fn changed(&self, paths: Vec<String>) {
        let _ = self.tx.send(Msg::Changed(paths));
    }
}

impl Drop for SyncEngine {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        let _ = self.tx.send(Msg::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join(); // exiting drops draft_tx → worker unblocks
        }
        if let Some(t) = self.draft_thread.take() {
            let _ = t.join();
        }
    }
}

fn engine_loop(
    rx: Receiver<Msg>,
    draft_tx: Sender<i64>,
    root: PathBuf,
    db_path: PathBuf,
    cfg: SyncConfig,
    notify: Notify,
) {
    let Ok(mut db) = Db::open_at(&db_path) else {
        return;
    };
    emit_state(
        &notify,
        if sync_active(&root) { SyncState::Synced } else { SyncState::Off },
        None,
    );

    let mut last_pull: Option<Instant> = None;
    let mut next_push: Option<Instant> = None;

    loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(Msg::PullNow) => {
                let throttled =
                    last_pull.is_some_and(|t| t.elapsed() < cfg.pull_throttle);
                if sync_active(&root) && !throttled {
                    last_pull = Some(Instant::now());
                    cycle(&root, &mut db, &draft_tx, &cfg, &notify, true);
                }
            }
            Ok(Msg::SyncNow) => {
                if sync_active(&root) {
                    last_pull = Some(Instant::now());
                    next_push = None;
                    cycle(&root, &mut db, &draft_tx, &cfg, &notify, true);
                } else {
                    emit_state(&notify, SyncState::Off, None);
                }
            }
            Ok(Msg::Changed(paths)) => {
                detect_conflicted_copies(&root, &mut db, &paths, &notify);
                if sync_active(&root) {
                    // Earliest deadline wins.
                    next_push.get_or_insert(Instant::now() + cfg.push_debounce);
                }
            }
            Ok(Msg::Shutdown) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }

        if next_push.is_some_and(|t| t <= Instant::now()) {
            next_push = None;
            if sync_active(&root) {
                cycle(&root, &mut db, &draft_tx, &cfg, &notify, false);
            }
        }
    }
}

fn emit_state(notify: &Notify, state: SyncState, detail: Option<String>) {
    (notify)(SyncNotice::State { state, detail });
}

/// One sync cycle: commit local work, optionally pull (conflicts are
/// aborted and filed to Review), push (a rejected push pulls once and
/// retries), then settle the state dot.
fn cycle(
    root: &Path,
    db: &mut Db,
    draft_tx: &Sender<i64>,
    _cfg: &SyncConfig,
    notify: &Notify,
    do_pull: bool,
) {
    emit_state(notify, SyncState::Syncing, None);
    let _ = ensure_excludes(root);

    // Paths the user already resolved in Review recently: their chosen
    // content is on our side, so those conflicts auto-complete keeping ours.
    let resolved_paths: HashSet<String> = db
        .list_recent_resolved_review_items(now_epoch() - 24 * 3600)
        .unwrap_or_default()
        .into_iter()
        .filter(|i| i.kind == "conflict")
        .map(|i| i.source_ref)
        .collect();
    let keep_ours = |p: &str| resolved_paths.contains(p);

    let mut conflicts: Vec<ConflictInfo> = Vec::new();
    let mut failure: Option<String> = None;

    if let Err(e) = commit_all(root) {
        failure = Some(e.to_string());
    }

    if failure.is_none() && do_pull {
        match pull(root, keep_ours) {
            Ok(PullOutcome::Clean) => {}
            Ok(PullOutcome::Conflicts(c)) => conflicts = c,
            Ok(PullOutcome::Failed(e)) => failure = Some(e),
            Err(e) => failure = Some(e.to_string()),
        }
    }

    if failure.is_none() && conflicts.is_empty() {
        let first = match push(root) {
            Ok(PushOutcome::Failed(e)) => Some(e),
            Err(e) => Some(e.to_string()),
            _ => None,
        };
        if first.is_some() {
            // The remote may have moved under us — take its changes and
            // try once more. Never force.
            match pull(root, keep_ours) {
                Ok(PullOutcome::Clean) => match push(root) {
                    Ok(PushOutcome::Failed(e)) => failure = Some(e),
                    Err(e) => failure = Some(e.to_string()),
                    _ => {}
                },
                Ok(PullOutcome::Conflicts(c)) => conflicts = c,
                Ok(PullOutcome::Failed(e)) => failure = Some(e),
                Err(e) => failure = Some(e.to_string()),
            }
        }
    }

    if !conflicts.is_empty() {
        file_conflicts(db, draft_tx, notify, conflicts);
    }

    // Settle the dot: failures and open conflicts keep attention.
    if let Some(detail) = failure {
        emit_state(
            notify,
            SyncState::Attention,
            Some(format!(
                "Ken couldn't sync this project just now — it will keep trying. ({detail})"
            )),
        );
    } else if has_open_conflicts(db) {
        emit_state(
            notify,
            SyncState::Attention,
            Some("Some documents have two versions — open Review to choose what stays.".into()),
        );
    } else {
        emit_state(notify, SyncState::Synced, None);
    }
}

fn has_open_conflicts(db: &Db) -> bool {
    db.list_open_review_items()
        .map(|items| {
            items
                .iter()
                .any(|i| i.kind == "conflict" || i.kind == "conflict-copy")
        })
        .unwrap_or(false)
}

/// File one Review item per conflicted file (deduped on open items for
/// the same path) and queue AI merge drafts.
fn file_conflicts(
    db: &mut Db,
    draft_tx: &Sender<i64>,
    notify: &Notify,
    conflicts: Vec<ConflictInfo>,
) {
    let open = db.list_open_review_items().unwrap_or_default();
    let mut filed = false;
    for c in conflicts {
        let dup = open
            .iter()
            .any(|it| it.kind == "conflict" && it.source_ref == c.path);
        if dup {
            continue;
        }
        let name = c.path.rsplit('/').next().unwrap_or(&c.path);
        let payload = json!({
            "path": c.path,
            "ours": c.ours,
            "theirs": c.theirs,
            "draft": null,
            "draftStatus": "pending",
        })
        .to_string();
        let inserted = db.insert_review_item(
            "conflict",
            &format!("Two versions of {name}"),
            "You and a teammate edited this document at the same time. Your version is still in place — compare the two below and choose what stays.",
            &c.path,
            Some(&payload),
            now_epoch(),
        );
        if let Ok(id) = inserted {
            let _ = draft_tx.send(id);
            filed = true;
        }
    }
    if filed {
        (notify)(SyncNotice::ReviewChanged);
    }
}

/// Scan watcher-reported paths for shared-drive conflicted copies and
/// file `conflict-copy` Review items (deduped per copy path).
fn detect_conflicted_copies(
    root: &Path,
    db: &mut Db,
    paths: &[String],
    notify: &Notify,
) {
    let mut open: Option<Vec<crate::db::ReviewItemRow>> = None;
    let mut filed = false;
    for rel in paths {
        let file_name = rel.rsplit('/').next().unwrap_or(rel);
        let Some(orig_name) = conflicted_copy_original(file_name) else {
            continue;
        };
        if !root.join(rel).is_file() {
            continue; // deletion or rename-away — nothing to review
        }
        let open_items =
            open.get_or_insert_with(|| db.list_open_review_items().unwrap_or_default());
        if open_items
            .iter()
            .any(|it| it.kind == "conflict-copy" && it.source_ref == *rel)
        {
            continue;
        }
        let parent = rel
            .rsplit_once('/')
            .map(|(d, _)| format!("{d}/"))
            .unwrap_or_default();
        let orig_rel = format!("{parent}{orig_name}");
        let orig_exists = root.join(&orig_rel).is_file();
        let payload = json!({
            "copyPath": rel,
            "originalPath": if orig_exists { Some(orig_rel.clone()) } else { None },
        })
        .to_string();
        let (title, body) = if orig_exists {
            (
                format!("Two copies of {orig_name}"),
                "Your shared drive saved a conflicting copy next to the original instead of combining the edits. Choose which version to keep — the other file is removed.".to_string(),
            )
        } else {
            (
                format!("A conflicting copy appeared: {file_name}"),
                "Your shared drive saved this file under a conflict name and the original is gone. Keep it to restore the normal name, or remove it.".to_string(),
            )
        };
        if db
            .insert_review_item("conflict-copy", &title, &body, rel, Some(&payload), now_epoch())
            .is_ok()
        {
            open = None; // re-read next iteration so dedupe sees the insert
            filed = true;
        }
    }
    if filed {
        (notify)(SyncNotice::ReviewChanged);
        emit_state(
            notify,
            SyncState::Attention,
            Some("A conflicting copy needs review — open Review to choose.".into()),
        );
    }
}

/// Drafts AI merges one at a time so a burst of conflicts never fans out
/// into parallel agent runs.
fn draft_worker(
    rx: Receiver<i64>,
    root: PathBuf,
    db_path: PathBuf,
    cfg: SyncConfig,
    cancel: Arc<AtomicBool>,
    notify: Notify,
) {
    let Ok(mut db) = Db::open_at(&db_path) else {
        return;
    };
    while let Ok(id) = rx.recv() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let Ok(Some(item)) = db.get_review_item(id) else {
            continue;
        };
        if item.status != "open" {
            continue;
        }
        let Some(mut payload) = item
            .payload
            .as_deref()
            .and_then(|p| serde_json::from_str::<serde_json::Value>(p).ok())
        else {
            continue;
        };
        let path = payload["path"].as_str().unwrap_or_default().to_string();
        let ours = payload["ours"].as_str().unwrap_or_default().to_string();
        let theirs = payload["theirs"].as_str().unwrap_or_default().to_string();
        let draft = if path.is_empty() {
            None
        } else {
            cfg.binary
                .clone()
                .or_else(crate::runner::discover_claude)
                .and_then(|binary| {
                    draft_merge(&binary, &root, &path, &ours, &theirs, cfg.draft_timeout, &cancel)
                        .ok()
                        .flatten()
                })
        };
        match draft {
            Some(d) => {
                payload["draft"] = json!(d);
                payload["draftStatus"] = json!("ready");
            }
            None => {
                payload["draftStatus"] = json!("failed");
            }
        }
        let _ = db.set_review_item_payload(id, &payload.to_string());
        (notify)(SyncNotice::ReviewChanged);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::test_support::write_fake_claude;
    use std::fs;
    use std::sync::mpsc::channel as std_channel;

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} in {dir:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn set_identity(repo: &Path) {
        git(repo, &["config", "user.email", "ken@test.local"]);
        git(repo, &["config", "user.name", "Ken Test"]);
    }

    /// A bare origin with two clones, seeded with notes.md on main.
    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let bare = dir.path().join("origin.git");
        fs::create_dir(&bare).unwrap();
        git(&bare, &["init", "--bare", "--initial-branch=main"]);

        git(dir.path(), &["clone", "origin.git", "a"]);
        let a = dir.path().join("a");
        set_identity(&a);
        git(&a, &["checkout", "-B", "main"]);
        fs::write(a.join("notes.md"), "base\n").unwrap();
        git(&a, &["add", "-A"]);
        git(&a, &["commit", "-m", "seed"]);
        git(&a, &["push", "-u", "origin", "main"]);

        git(dir.path(), &["clone", "origin.git", "b"]);
        let b = dir.path().join("b");
        set_identity(&b);

        (dir, bare, a, b)
    }

    fn commit_push(repo: &Path, file: &str, content: &str) {
        fs::write(repo.join(file), content).unwrap();
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-m", "edit"]);
        git(repo, &["push"]);
    }

    #[test]
    fn clean_pull_propagates_a_file() {
        let (_d, _bare, a, b) = fixture();
        commit_push(&a, "fresh.md", "from teammate\n");
        assert_eq!(pull(&b, |_| false).unwrap(), PullOutcome::Clean);
        assert_eq!(fs::read_to_string(b.join("fresh.md")).unwrap(), "from teammate\n");
    }

    #[test]
    fn divergent_non_conflicting_changes_merge() {
        let (_d, _bare, a, b) = fixture();
        commit_push(&a, "theirs.md", "their new file\n");
        fs::write(b.join("mine.md"), "my new file\n").unwrap();
        git(&b, &["add", "-A"]);
        git(&b, &["commit", "-m", "mine"]);

        assert_eq!(pull(&b, |_| false).unwrap(), PullOutcome::Clean);
        assert!(b.join("theirs.md").is_file());
        assert!(b.join("mine.md").is_file());
        assert_eq!(push(&b).unwrap(), PushOutcome::Pushed);
    }

    #[test]
    fn conflicting_pull_aborts_cleanly_and_extracts_versions() {
        let (_d, _bare, a, b) = fixture();
        commit_push(&a, "notes.md", "teammate version\n");
        fs::write(b.join("notes.md"), "my version\n").unwrap();
        git(&b, &["add", "-A"]);
        git(&b, &["commit", "-m", "mine"]);

        let outcome = pull(&b, |_| false).unwrap();
        let PullOutcome::Conflicts(conflicts) = outcome else {
            panic!("expected conflicts, got {outcome:?}");
        };
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].path, "notes.md");
        assert_eq!(conflicts[0].ours, "my version\n");
        assert_eq!(conflicts[0].theirs, "teammate version\n");
        // Working tree restored — no markers, no merge in progress.
        assert_eq!(fs::read_to_string(b.join("notes.md")).unwrap(), "my version\n");
        assert!(!b.join(".git/MERGE_HEAD").exists());
        // Idempotent: the next pull reports the same conflict again.
        assert!(matches!(pull(&b, |_| false).unwrap(), PullOutcome::Conflicts(_)));
    }

    #[test]
    fn resolved_conflict_completes_merge_keeping_ours() {
        let (_d, _bare, a, b) = fixture();
        commit_push(&a, "notes.md", "teammate version\n");
        fs::write(b.join("notes.md"), "my version\n").unwrap();
        git(&b, &["add", "-A"]);
        git(&b, &["commit", "-m", "mine"]);
        assert!(matches!(pull(&b, |_| false).unwrap(), PullOutcome::Conflicts(_)));

        // The user resolved in Review (chose "take theirs" — content written
        // and committed on our side); the next pull keeps our side and
        // completes the merge.
        fs::write(b.join("notes.md"), "teammate version\n").unwrap();
        git(&b, &["add", "-A"]);
        git(&b, &["commit", "-m", "resolution"]);
        assert_eq!(pull(&b, |p| p == "notes.md").unwrap(), PullOutcome::Clean);
        assert_eq!(
            fs::read_to_string(b.join("notes.md")).unwrap(),
            "teammate version\n"
        );
        assert!(!b.join(".git/MERGE_HEAD").exists());
        assert_eq!(push(&b).unwrap(), PushOutcome::Pushed);
    }

    #[test]
    fn commit_all_and_push_edge_cases() {
        let (_d, _bare, _a, b) = fixture();
        assert!(!commit_all(&b).unwrap(), "clean tree → no commit");
        fs::write(b.join("new.md"), "x\n").unwrap();
        assert!(commit_all(&b).unwrap());
        assert_eq!(push(&b).unwrap(), PushOutcome::Pushed);

        // A repo without a remote skips the push.
        let lone = tempfile::tempdir().unwrap();
        git(lone.path(), &["init", "--initial-branch=main"]);
        set_identity(lone.path());
        assert_eq!(push(lone.path()).unwrap(), PushOutcome::NoRemote);
    }

    #[test]
    fn ensure_excludes_is_idempotent() {
        let (_d, _bare, a, _b) = fixture();
        ensure_excludes(&a).unwrap();
        ensure_excludes(&a).unwrap();
        let text = fs::read_to_string(a.join(".git/info/exclude")).unwrap();
        assert_eq!(text.matches(".ken/.staging/").count(), 1);
        assert_eq!(text.matches(".claude/settings.local.json").count(), 1);
        // Non-repo: quiet no-op.
        let plain = tempfile::tempdir().unwrap();
        ensure_excludes(plain.path()).unwrap();
        assert!(!plain.path().join(".git").exists());
    }

    #[test]
    fn conflicted_copy_patterns() {
        // Positives — Dropbox styles and case conflicts.
        assert_eq!(
            conflicted_copy_original("notes (conflicted copy 2026-07-12).md").as_deref(),
            Some("notes.md")
        );
        assert_eq!(
            conflicted_copy_original("Budget (Bob's conflicted copy).xlsx").as_deref(),
            Some("Budget.xlsx")
        );
        assert_eq!(
            conflicted_copy_original("readme (Case Conflict 1).txt").as_deref(),
            Some("readme.txt")
        );
        assert_eq!(
            conflicted_copy_original("plan (CONFLICTED COPY).md").as_deref(),
            Some("plan.md")
        );
        // Negatives — lookalikes must not match.
        assert_eq!(conflicted_copy_original("copy of notes.md"), None);
        assert_eq!(conflicted_copy_original("conflicted-copy-analysis.md"), None);
        assert_eq!(conflicted_copy_original("notes (copy).md"), None);
        assert_eq!(conflicted_copy_original("case conflict.md"), None);
    }

    #[test]
    fn draft_merge_uses_fake_claude() {
        let dir = tempfile::tempdir().unwrap();
        let bin = write_fake_claude(dir.path(), "complete");
        // The fake writes its default output (knowledge/People.md) into the
        // STAGING_DIR announced in the prompt — same contract as ingests.
        let draft = draft_merge(
            &bin,
            dir.path(),
            "knowledge/People.md",
            "ours\n",
            "theirs\n",
            Duration::from_secs(30),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(draft.unwrap().contains("Priya Natarajan"));
        // Staging cleaned up.
        assert!(!dir.path().join(".ken/.staging/sync-merge").join("x").exists());
    }

    #[test]
    fn sync_auto_reads_project_extra() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = Project::create(dir.path(), "X").unwrap();
        assert!(sync_auto(&p), "defaults on");
        p.config
            .extra
            .insert("sync".into(), json!({"auto": false}));
        assert!(!sync_auto(&p));
    }

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

    #[test]
    fn engine_pushes_local_changes() {
        let (_d, bare, _a, b) = fixture();
        Project::create(&b, "B").unwrap();
        let app = tempfile::tempdir().unwrap();
        let db_path = app.path().join("sync.db");
        drop(Db::open_at(&db_path).unwrap());

        let (ntx, notices) = std_channel::<SyncNotice>();
        let engine = SyncEngine::start(
            b.clone(),
            db_path,
            SyncConfig {
                binary: None,
                pull_throttle: Duration::from_millis(0),
                push_debounce: Duration::from_millis(200),
                draft_timeout: Duration::from_secs(5),
            },
            move |n| {
                let _ = ntx.send(n);
            },
        )
        .unwrap();

        fs::write(b.join("insight.md"), "a brand new insight\n").unwrap();
        engine.changed(vec!["insight.md".into()]);

        assert!(
            wait_until(
                || {
                    run_git(&bare, &["log", "--oneline"])
                        .map(|o| o.stdout.contains("Ken: update knowledge"))
                        .unwrap_or(false)
                },
                20
            ),
            "debounced commit+push should land on the remote"
        );
        // Ken's transient files were excluded non-invasively.
        let exclude = fs::read_to_string(b.join(".git/info/exclude")).unwrap();
        assert!(exclude.contains(".ken/.staging/"));
        // A synced notice arrived.
        let mut saw_synced = false;
        while let Ok(n) = notices.try_recv() {
            if let SyncNotice::State { state: SyncState::Synced, .. } = n {
                saw_synced = true;
            }
        }
        assert!(saw_synced);
        drop(engine);
    }

    #[test]
    fn engine_files_conflict_item_with_draft() {
        let (_d, _bare, a, b) = fixture();
        // Both sides get the seed doc at the path the fake claude writes.
        fs::create_dir_all(a.join("knowledge")).unwrap();
        commit_push(&a, "knowledge/People.md", "base\n");
        git(&b, &["pull", "--no-rebase", "--no-edit"]);
        Project::create(&b, "B").unwrap();

        // Divergence: teammate pushes, we edit locally (uncommitted).
        commit_push(&a, "knowledge/People.md", "teammate version\n");
        fs::write(b.join("knowledge/People.md"), "my version\n").unwrap();

        let app = tempfile::tempdir().unwrap();
        let db_path = app.path().join("sync.db");
        drop(Db::open_at(&db_path).unwrap());
        let fake = write_fake_claude(app.path(), "complete");

        let (ntx, notices) = std_channel::<SyncNotice>();
        let engine = SyncEngine::start(
            b.clone(),
            db_path.clone(),
            SyncConfig {
                binary: Some(fake),
                pull_throttle: Duration::from_millis(0),
                push_debounce: Duration::from_millis(200),
                draft_timeout: Duration::from_secs(20),
            },
            move |n| {
                let _ = ntx.send(n);
            },
        )
        .unwrap();
        engine.changed(vec!["knowledge/People.md".into()]);

        // The conflicted push→pull cycle files exactly one Review item…
        let db = Db::open_at(&db_path).unwrap();
        assert!(
            wait_until(
                || db
                    .list_open_review_items()
                    .map(|items| items.iter().any(|i| i.kind == "conflict"))
                    .unwrap_or(false),
                20
            ),
            "conflict item should be filed"
        );
        let items = db.list_open_review_items().unwrap();
        let item = items.iter().find(|i| i.kind == "conflict").unwrap();
        assert_eq!(item.source_ref, "knowledge/People.md");
        let payload: serde_json::Value =
            serde_json::from_str(item.payload.as_ref().unwrap()).unwrap();
        assert_eq!(payload["ours"], "my version\n");
        assert_eq!(payload["theirs"], "teammate version\n");

        // …the working tree keeps the user's version, no merge in progress…
        assert_eq!(
            fs::read_to_string(b.join("knowledge/People.md")).unwrap(),
            "my version\n"
        );
        assert!(!b.join(".git/MERGE_HEAD").exists());

        // …and the AI draft lands in the payload.
        let id = item.id;
        assert!(
            wait_until(
                || db
                    .get_review_item(id)
                    .ok()
                    .flatten()
                    .and_then(|i| i.payload)
                    .and_then(|p| serde_json::from_str::<serde_json::Value>(&p).ok())
                    .map(|p| p["draftStatus"] == "ready")
                    .unwrap_or(false),
                30
            ),
            "draft should be marked ready"
        );
        let payload: serde_json::Value = serde_json::from_str(
            &db.get_review_item(id).unwrap().unwrap().payload.unwrap(),
        )
        .unwrap();
        assert!(payload["draft"].as_str().unwrap().contains("Priya Natarajan"));

        // Attention was signalled.
        let mut saw_attention = false;
        while let Ok(n) = notices.try_recv() {
            if let SyncNotice::State { state: SyncState::Attention, .. } = n {
                saw_attention = true;
            }
        }
        assert!(saw_attention);

        // A second cycle does not double-file (dedupe on open items).
        engine.sync_now();
        std::thread::sleep(Duration::from_millis(800));
        let conflict_count = db
            .list_open_review_items()
            .unwrap()
            .iter()
            .filter(|i| i.kind == "conflict")
            .count();
        assert_eq!(conflict_count, 1);
        drop(engine);
    }

    #[test]
    fn engine_detects_conflicted_copies() {
        let dir = tempfile::tempdir().unwrap(); // not a git repo — shared drive
        Project::create(dir.path(), "Drive").unwrap();
        fs::write(dir.path().join("notes.md"), "original\n").unwrap();
        fs::write(
            dir.path().join("notes (conflicted copy 2026-07-12).md"),
            "copy\n",
        )
        .unwrap();
        let app = tempfile::tempdir().unwrap();
        let db_path = app.path().join("sync.db");
        drop(Db::open_at(&db_path).unwrap());

        let engine = SyncEngine::start(
            dir.path().to_path_buf(),
            db_path.clone(),
            SyncConfig::default(),
            |_| {},
        )
        .unwrap();
        engine.changed(vec![
            "notes.md".into(),
            "notes (conflicted copy 2026-07-12).md".into(),
        ]);

        let db = Db::open_at(&db_path).unwrap();
        assert!(
            wait_until(
                || db
                    .list_open_review_items()
                    .map(|items| items.iter().any(|i| i.kind == "conflict-copy"))
                    .unwrap_or(false),
                10
            ),
            "conflict-copy item should be filed"
        );
        let items = db.list_open_review_items().unwrap();
        let item = items.iter().find(|i| i.kind == "conflict-copy").unwrap();
        let payload: serde_json::Value =
            serde_json::from_str(item.payload.as_ref().unwrap()).unwrap();
        assert_eq!(payload["copyPath"], "notes (conflicted copy 2026-07-12).md");
        assert_eq!(payload["originalPath"], "notes.md");

        // Re-reporting the same path does not double-file.
        engine.changed(vec!["notes (conflicted copy 2026-07-12).md".into()]);
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(
            db.list_open_review_items()
                .unwrap()
                .iter()
                .filter(|i| i.kind == "conflict-copy")
                .count(),
            1
        );
        drop(engine);
    }
}

//! The family sync loop (`openspec/changes/ken-families` D1): a transport
//! seam over the system `git` CLI, and a pure state machine that drives it.
//!
//! ## Why a trait
//!
//! D1 pins the transport to the system `git` executable (spike S8 settled
//! it: "build ken-families on the system git CLI … no libgit2"), which
//! reuses the user's credential helpers instead of reimplementing auth.
//! That makes the *transport* untestable offline, so every decision the
//! loop makes lives above it: [`GitTransport`] is the seam,
//! [`SystemGit`] is the real thing, [`FakeTransport`] is an in-memory
//! remote, and [`SyncEngine`] never touches a process or a socket.
//!
//! ## S8 is binding
//!
//! `features/multi-project/spikes/S8-git-sync-windows.md` came back
//! GO-with-caveats, and its caveats are implemented here rather than
//! remembered:
//!
//! - **`git status` exits 0 mid-rebase** — so [`SystemGit::head_status`]
//!   parses porcelain output and the `.git/rebase-*` markers, and never
//!   infers state from an exit code.
//! - **Push rejection is normal under contention** (S8 saw 23–25 rejects
//!   in 25 iterations) — a non-fast-forward is an ordinary
//!   [`PushOutcome`], not an error, and the loop absorbs it with one
//!   re-integrate.
//! - **Prompt suppression** — `GIT_TERMINAL_PROMPT=0` and
//!   `GCM_INTERACTIVE=never` on every invocation, with an opt-in
//!   `-c credential.helper=` for unattended background sync (S8's one
//!   unverified item is Git Credential Manager's GUI on a live HTTP 401;
//!   design.md carries it as a pre-ship checklist item).
//! - **Per-clone config** — [`SystemGit::configure_clone`] sets
//!   `core.longpaths=true`, `core.autocrlf=false`, `pull.rebase=true`.
//! - **`index.lock` is never blind-deleted** — this module simply doesn't
//!   delete it; S8's rule is that removal requires a process-liveness
//!   check, so it belongs to a deliberate recovery command, not to the
//!   poll loop.
//! - **One clone per device, serialized ops** — S8: "never run concurrent
//!   git in one working copy". [`SyncEngine::poll`] and
//!   [`SyncEngine::commit`] both take `&mut self`, so a connection's git
//!   operations are serialized by the borrow checker.
//!
//! ## Conflicts halt, they never resolve
//!
//! Lanes (`family::lane_check`) make a conflict impossible in normal
//! operation. If one happens anyway — a hand-edited repo, a buggy Ken on
//! the other side — the connection goes to
//! [`ConnectionState::Conflict`] and polling stops until a human says
//! otherwise. Ken never auto-resolves and never force-pushes.
//!
//! One deliberate refinement of D1's wording: on conflict, [`SystemGit`]
//! runs `git rebase --abort` before reporting. That is *not* auto-resolving
//! — it is S8's settled recovery step ("recover via `rebase --abort`"), it
//! discards nothing (the local commits it was replaying are restored), and
//! it is what keeps the clone usable; a clone abandoned mid-rebase would
//! fail every subsequent git command, including the ones a user needs to
//! inspect it. The remote is untouched either way.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use crate::family::Lane;
use crate::{Error, Result};

/// One file the local Ken wants in the family repo. Content, not a diff:
/// every family file has exactly one writer, so "here is the new text" is
/// always the whole truth.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingWrite {
    /// Repo-relative, forward slashes — the form `family::lane_check`
    /// validates and git speaks.
    pub rel_path: String,
    pub content: String,
}

impl PendingWrite {
    pub fn new(rel_path: impl Into<String>, content: impl Into<String>) -> Self {
        PendingWrite { rel_path: rel_path.into(), content: content.into() }
    }
}

/// What `fetch` + rebase did.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "outcome")]
pub enum IntegrateOutcome {
    UpToDate,
    /// Nothing local to replay — the branch just moved forward.
    FastForward { commits: usize },
    /// Local commits were replayed on top of incoming ones.
    Rebased { commits: usize },
    /// A real conflict. Carries git's own output (D7/spec: "the connection
    /// shows the error with git's own output").
    Conflict { detail: String },
}

/// What `push` did. A non-fast-forward is an expected outcome, not an
/// error: S8 measured it on nearly every iteration under contention.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "outcome")]
pub enum PushOutcome {
    /// Nothing local to send.
    UpToDate,
    Pushed { commits: usize },
    /// The remote moved; re-integrate and try again.
    NonFastForward { detail: String },
    /// Anything else (auth, network, host) — git's own words.
    Failed { detail: String },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitOutcome {
    pub files: usize,
    /// False when the writes changed nothing on disk — a no-op write
    /// never becomes an empty commit.
    pub committed: bool,
}

/// Where the local branch sits relative to what the last fetch learned.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeadStatus {
    /// Local commits not yet pushed.
    pub ahead: usize,
    /// Fetched commits not yet integrated.
    pub behind: usize,
    /// Uncommitted changes in the working tree.
    pub dirty: bool,
    /// A rebase is in progress — S8: detected from `.git/rebase-merge` /
    /// `.git/rebase-apply`, never from an exit code.
    pub rebase_in_progress: bool,
}

/// A connection's sync state, as the Families settings page shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "state")]
pub enum ConnectionState {
    /// Synced and waiting for the next poll.
    Idle,
    /// A cycle is running. Never observed by a caller of [`SyncEngine`]
    /// itself (`poll` is synchronous); it exists for the src-tauri layer,
    /// which runs the cycle off-thread and publishes this meanwhile.
    Syncing,
    /// A rebase conflict. **Polling stops here** until a human resolves
    /// the clone and calls [`SyncEngine::resolved`].
    Conflict { detail: String },
    /// A transient failure (network, auth, a git command that failed for
    /// some other reason). The next poll retries.
    Error { detail: String },
    /// The feature can't run at all here — no `git` on PATH, or a manifest
    /// that needs a newer Ken. Inert, no retry, no dialogs.
    Unavailable { reason: String },
}

impl ConnectionState {
    /// True when polling must not run: a conflict needs a human, and an
    /// unavailable connection has nothing to poll with.
    pub fn is_halted(&self) -> bool {
        matches!(self, ConnectionState::Conflict { .. } | ConnectionState::Unavailable { .. })
    }
}

/// What one poll cycle did — the payload behind the tray badge and the
/// settings page's "last sync" line.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncReport {
    pub state: ConnectionState,
    /// False when the engine was halted and the cycle was skipped
    /// entirely — no git ran.
    pub ran: bool,
    pub integrated: Option<IntegrateOutcome>,
    pub pushed: Option<PushOutcome>,
    /// True when a non-fast-forward push was re-integrated and retried
    /// (the loop's one allowed retry).
    pub push_retried: bool,
}

// ---------------------------------------------------------------------
// 1.3 The transport seam
// ---------------------------------------------------------------------

/// Everything the sync loop needs from git.
///
/// Deliberately small and dumb: no policy lives here, so [`SystemGit`] and
/// [`FakeTransport`] can't disagree about behavior the loop depends on.
/// The one exception is [`GitTransport::commit_paths`], which is a
/// *provided* method precisely so no implementation can forget the lane
/// check.
pub trait GitTransport {
    /// Learn what the remote has (`git fetch`). Does not change the
    /// working tree.
    fn fetch(&mut self) -> Result<()>;

    /// Fetch and replay local commits on top of the remote
    /// (`git pull --rebase`). Returns [`IntegrateOutcome::Conflict`]
    /// rather than an error for a real conflict — that's a state, not a
    /// failure.
    fn integrate(&mut self) -> Result<IntegrateOutcome>;

    /// Send local commits (`git push`).
    fn push(&mut self) -> Result<PushOutcome>;

    fn head_status(&self) -> Result<HeadStatus>;

    /// Whether the path already exists in the working tree. Ground truth
    /// for lane rule 2 ("new files only") — see [`Self::commit_paths`].
    fn exists(&self, rel_path: &str) -> bool;

    /// Working-tree contents of a path, if any.
    fn read(&self, rel_path: &str) -> Option<String>;

    /// Write and commit, no questions asked. **Never call this directly**
    /// — it is the primitive [`Self::commit_paths`] delegates to after
    /// the lane check.
    fn write_and_commit(&mut self, writes: &[PendingWrite], message: &str)
        -> Result<CommitOutcome>;

    /// The only sanctioned way to write to a family repo.
    ///
    /// Validates every staged path against the caller's lane and refuses
    /// the whole commit on the first violation (D3: "enforcement is code,
    /// not convention"; spec: "`commit_paths` SHALL validate every staged
    /// path and refuse the commit on violation"). Nothing is written when
    /// any path is refused — a partial commit would be worse than none.
    ///
    /// `is_new` is taken from the transport's own working tree, never from
    /// the caller: "I believe this is a new file" is exactly the belief a
    /// buggy caller would get wrong, and it is the belief lane rule 2
    /// rests on.
    fn commit_paths(
        &mut self,
        lane: &Lane<'_>,
        writes: &[PendingWrite],
        message: &str,
    ) -> Result<CommitOutcome> {
        for w in writes {
            let is_new = !self.exists(&w.rel_path);
            lane.check(&w.rel_path, is_new)?;
        }
        self.write_and_commit(writes, message)
    }
}

// ---------------------------------------------------------------------
// 1.4 The sync loop (pure state machine)
// ---------------------------------------------------------------------

/// The poll cycle, as a state machine over [`GitTransport`].
///
/// Holds no clone, no timer, and no clock: src-tauri owns the interval
/// (default 120s, bounds 30s–30min) and calls [`SyncEngine::poll`]; this
/// type only decides what happens next and what state that leaves the
/// connection in.
#[derive(Debug, Clone, PartialEq)]
pub struct SyncEngine {
    state: ConnectionState,
}

impl Default for SyncEngine {
    fn default() -> Self {
        SyncEngine::new()
    }
}

impl SyncEngine {
    pub fn new() -> Self {
        SyncEngine { state: ConnectionState::Idle }
    }

    /// An engine that will never run: no `git` on PATH, or a manifest
    /// declaring a template this Ken can't read.
    pub fn unavailable(reason: impl Into<String>) -> Self {
        SyncEngine { state: ConnectionState::Unavailable { reason: reason.into() } }
    }

    pub fn state(&self) -> &ConnectionState {
        &self.state
    }

    pub fn is_halted(&self) -> bool {
        self.state.is_halted()
    }

    /// Clear a conflict after the user has resolved the clone by hand.
    /// Only ever called from an explicit user action — a conflict never
    /// clears itself.
    pub fn resolved(&mut self) {
        if matches!(self.state, ConnectionState::Conflict { .. }) {
            self.state = ConnectionState::Idle;
        }
    }

    /// One cycle: fetch → rebase-integrate → push pending.
    ///
    /// A non-fast-forward push is re-integrated and retried **once**
    /// (spec: "retrying once with a fresh integrate on non-fast-forward").
    /// Any conflict, at either integrate, halts the loop.
    pub fn poll<T: GitTransport + ?Sized>(&mut self, transport: &mut T) -> SyncReport {
        if self.state.is_halted() {
            return self.report(false, None, None, false);
        }
        self.state = ConnectionState::Syncing;

        if let Err(e) = transport.fetch() {
            self.state = ConnectionState::Error { detail: e.to_string() };
            return self.report(true, None, None, false);
        }

        let integrated = match transport.integrate() {
            Ok(IntegrateOutcome::Conflict { detail }) => {
                self.state = ConnectionState::Conflict { detail: detail.clone() };
                return self.report(true, Some(IntegrateOutcome::Conflict { detail }), None, false);
            }
            Ok(outcome) => outcome,
            Err(e) => {
                self.state = ConnectionState::Error { detail: e.to_string() };
                return self.report(true, None, None, false);
            }
        };

        let pushed = match transport.push() {
            Ok(PushOutcome::NonFastForward { .. }) => {
                // Someone pushed between our integrate and our push. S8
                // measured this as the common case under contention; one
                // fresh integrate is enough (the lanes guarantee the
                // rebase is trivial).
                match transport.integrate() {
                    Ok(IntegrateOutcome::Conflict { detail }) => {
                        self.state = ConnectionState::Conflict { detail: detail.clone() };
                        return self.report(
                            true,
                            Some(IntegrateOutcome::Conflict { detail }),
                            None,
                            true,
                        );
                    }
                    Ok(_) => {}
                    Err(e) => {
                        self.state = ConnectionState::Error { detail: e.to_string() };
                        return self.report(true, Some(integrated), None, true);
                    }
                }
                match transport.push() {
                    Ok(outcome) => {
                        self.finish_push(&outcome);
                        return self.report(true, Some(integrated), Some(outcome), true);
                    }
                    Err(e) => {
                        self.state = ConnectionState::Error { detail: e.to_string() };
                        return self.report(true, Some(integrated), None, true);
                    }
                }
            }
            Ok(outcome) => outcome,
            Err(e) => {
                self.state = ConnectionState::Error { detail: e.to_string() };
                return self.report(true, Some(integrated), None, false);
            }
        };

        self.finish_push(&pushed);
        self.report(true, Some(integrated), Some(pushed), false)
    }

    /// Write files into the clone and commit them, lane-checked.
    ///
    /// Refused while halted: a conflicted clone must not gain new commits
    /// on top of whatever the user is about to resolve, and an unavailable
    /// connection has no clone to write to.
    pub fn commit<T: GitTransport + ?Sized>(
        &mut self,
        transport: &mut T,
        lane: &Lane<'_>,
        writes: &[PendingWrite],
        message: &str,
    ) -> Result<CommitOutcome> {
        match &self.state {
            ConnectionState::Conflict { .. } => Err(Error::Other(
                "this family connection has an unresolved conflict — resolve it before writing"
                    .into(),
            )),
            ConnectionState::Unavailable { reason } => {
                Err(Error::Other(format!("this family connection is unavailable: {reason}")))
            }
            _ => transport.commit_paths(lane, writes, message),
        }
    }

    fn finish_push(&mut self, outcome: &PushOutcome) {
        self.state = match outcome {
            PushOutcome::UpToDate | PushOutcome::Pushed { .. } => ConnectionState::Idle,
            // A second rejection means the remote is moving faster than we
            // can follow, or something is wrong that another retry won't
            // fix. Surface it and let the next poll start clean.
            PushOutcome::NonFastForward { detail } => {
                ConnectionState::Error { detail: format!("push kept being rejected: {detail}") }
            }
            PushOutcome::Failed { detail } => {
                ConnectionState::Error { detail: detail.clone() }
            }
        };
    }

    fn report(
        &self,
        ran: bool,
        integrated: Option<IntegrateOutcome>,
        pushed: Option<PushOutcome>,
        push_retried: bool,
    ) -> SyncReport {
        SyncReport { state: self.state.clone(), ran, integrated, pushed, push_retried }
    }
}

// ---------------------------------------------------------------------
// 1.3 SystemGit
// ---------------------------------------------------------------------

static GIT_PROBE: OnceLock<std::result::Result<String, String>> = OnceLock::new();

fn probe_git() -> std::result::Result<String, String> {
    match crate::proc::quiet(&mut Command::new("git")).arg("--version").output() {
        Ok(out) if out.status.success() => {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        }
        Ok(out) => Err(format!(
            "git exited {}: {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Err(e) => Err(format!("git is not on PATH ({e})")),
    }
}

/// Is the system `git` usable? Probed once per process and cached (task
/// 1.3: "detect on PATH once, cache result") — the Families page asks this
/// on every render and a poll asks it on every tick.
///
/// `Err` carries the reason verbatim, which is what the "unavailable
/// {reason}" state shows. Missing git is never an error dialog; the
/// feature is simply inert (proposal: "same degradation style as
/// `vec_available`").
pub fn git_available() -> std::result::Result<&'static str, &'static str> {
    match GIT_PROBE.get_or_init(probe_git) {
        Ok(v) => Ok(v.as_str()),
        Err(e) => Err(e.as_str()),
    }
}

/// The git config every ken-families checkout carries (S8's "per-clone
/// config to bake into every ken-families checkout"): `core.longpaths`
/// because Windows fails outright past 260 characters without it,
/// `core.autocrlf=false` for byte-stable sync and quiet logs, and
/// `pull.rebase` because rebase is the only integration this feature does.
///
/// Applied **twice, on purpose**: as `-c` flags on the `clone`/`init`
/// command ([`clone_config_args`]) so the very first checkout already
/// obeys them, and then persisted into the clone
/// ([`SystemGit::configure_clone`]) so every later operation does too.
///
/// Doing only the second half is a trap this module walked into once:
/// setting `core.autocrlf=false` *after* a checkout made with the
/// machine's global `autocrlf=true` makes every text file look modified,
/// and `git pull --rebase` then refuses to run at all ("You have unstaged
/// changes"). S8 flagged autocrlf as "cosmetic"; on the config-flip path
/// it is not.
pub const CLONE_CONFIG: [(&str, &str); 3] = [
    ("core.longpaths", "true"),
    ("core.autocrlf", "false"),
    ("pull.rebase", "true"),
];

/// [`CLONE_CONFIG`] as `-c key=value` arguments for a `git clone` or
/// `git init` command line.
pub fn clone_config_args() -> Vec<String> {
    CLONE_CONFIG
        .iter()
        .flat_map(|(k, v)| ["-c".to_string(), format!("{k}={v}")])
        .collect()
}

/// Commit identity override. Left `None` in production: a family clone is
/// the user's own repo and should carry the user's own git identity (D1:
/// "their config, their credentials"). Tests set it so a machine with no
/// global `user.email` can still commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitIdentity {
    pub name: String,
    pub email: String,
}

struct GitOut {
    ok: bool,
    stdout: String,
    stderr: String,
}

/// git's own words, trimmed to something a settings page can show.
fn short_detail(out: &GitOut) -> String {
    let text = if out.stderr.trim().is_empty() { out.stdout.trim() } else { out.stderr.trim() };
    let mut s: String = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    if s.chars().count() > 400 {
        s = s.chars().take(400).collect::<String>() + "…";
    }
    s
}

/// The real transport: the system `git` binary, run against one clone.
pub struct SystemGit {
    root: PathBuf,
    remote: String,
    branch: String,
    /// S8's unattended-sync mitigation: run git with an empty credential
    /// helper so no interactive path (including Git Credential Manager's
    /// GUI) is reachable. Off by default — the user's helpers are the
    /// whole reason D1 shells out to git — and meant for background
    /// polling on a connection whose credentials are known to be
    /// non-interactive (SSH agent, stored PAT).
    pub suppress_credential_helper: bool,
    pub identity: Option<GitIdentity>,
}

impl SystemGit {
    /// A transport for an existing clone, using its own first remote and
    /// current branch.
    pub fn open(root: impl Into<PathBuf>) -> Result<SystemGit> {
        let root = root.into();
        let probe = SystemGit {
            root: root.clone(),
            remote: "origin".into(),
            branch: "main".into(),
            suppress_credential_helper: false,
            identity: None,
        };
        let remote = probe
            .run(&["remote"])
            .ok()
            .filter(|o| o.ok)
            .and_then(|o| o.stdout.lines().next().map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "origin".to_string());
        let branch = probe
            .run(&["rev-parse", "--abbrev-ref", "HEAD"])
            .ok()
            .filter(|o| o.ok)
            .map(|o| o.stdout.trim().to_string())
            .filter(|s| !s.is_empty() && s != "HEAD")
            .unwrap_or_else(|| "main".to_string());
        Ok(SystemGit { root, remote, branch, suppress_credential_helper: false, identity: None })
    }

    pub fn new(root: impl Into<PathBuf>, remote: impl Into<String>, branch: impl Into<String>) -> Self {
        SystemGit {
            root: root.into(),
            remote: remote.into(),
            branch: branch.into(),
            suppress_credential_helper: false,
            identity: None,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Persist [`CLONE_CONFIG`] into the clone, so every later git
    /// operation obeys it.
    ///
    /// Must be paired with [`clone_config_args`] on the `clone`/`init`
    /// command — see [`CLONE_CONFIG`] for why doing only this half leaves
    /// the working tree spuriously dirty.
    pub fn configure_clone(&self) -> Result<()> {
        for (key, value) in CLONE_CONFIG {
            let out = self.run(&["config", key, value])?;
            if !out.ok {
                return Err(Error::Other(format!(
                    "could not set {key} on the family clone: {}",
                    short_detail(&out)
                )));
            }
        }
        Ok(())
    }

    fn run(&self, args: &[&str]) -> Result<GitOut> {
        let mut cmd = Command::new("git");
        crate::proc::quiet(&mut cmd);
        if let Some(id) = &self.identity {
            cmd.args(["-c", &format!("user.name={}", id.name)]);
            cmd.args(["-c", &format!("user.email={}", id.email)]);
        }
        if self.suppress_credential_helper {
            cmd.args(["-c", "credential.helper="]);
        }
        let out = cmd
            .args(args)
            .current_dir(&self.root)
            // S8 Q4: fail fast, never hang on a prompt.
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GCM_INTERACTIVE", "never")
            .output()
            .map_err(|e| Error::Other(format!("could not run git: {e}")))?;
        Ok(GitOut {
            ok: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    fn head_sha(&self) -> Option<String> {
        self.run(&["rev-parse", "HEAD"])
            .ok()
            .filter(|o| o.ok)
            .map(|o| o.stdout.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// S8 Q2/Q3: a rebase in progress is a directory, not an exit code.
    fn rebase_in_progress(&self) -> bool {
        let git_dir = self.root.join(".git");
        git_dir.join("rebase-merge").exists() || git_dir.join("rebase-apply").exists()
    }

    fn conflicted_paths(&self) -> Vec<String> {
        self.run(&["ls-files", "-u", "--format=%(path)"])
            .ok()
            .filter(|o| o.ok)
            .map(|o| {
                let mut seen: BTreeSet<String> = BTreeSet::new();
                for line in o.stdout.lines() {
                    let p = line.trim();
                    if !p.is_empty() {
                        seen.insert(p.to_string());
                    }
                }
                seen.into_iter().collect()
            })
            .unwrap_or_default()
    }

    fn count_commits(&self, range: &str) -> usize {
        self.run(&["rev-list", "--count", range])
            .ok()
            .filter(|o| o.ok)
            .and_then(|o| o.stdout.trim().parse().ok())
            .unwrap_or(0)
    }

    fn ref_exists(&self, name: &str) -> bool {
        self.run(&["rev-parse", "--verify", "--quiet", name])
            .map(|o| o.ok)
            .unwrap_or(false)
    }

    /// How many local commits are unpushed when porcelain can't say.
    ///
    /// A branch with no configured upstream — which is exactly the state
    /// of a freshly `git init`ed repo right after the template's bootstrap
    /// commit — reports no `[ahead N]` at all, so trusting the porcelain
    /// line alone would make the first push a silent no-op. Fall back to
    /// the remote-tracking ref, and to "every commit there is" when even
    /// that doesn't exist yet.
    fn ahead_without_upstream(&self) -> usize {
        let tracking = format!("{}/{}", self.remote, self.branch);
        if self.ref_exists(&format!("refs/remotes/{tracking}")) {
            self.count_commits(&format!("{tracking}..HEAD"))
        } else {
            self.count_commits("HEAD")
        }
    }
}

/// `## main...origin/main [ahead 1, behind 2]` → `(1, 2)`.
fn parse_ahead_behind(branch_line: &str) -> (usize, usize) {
    let Some(start) = branch_line.find('[') else {
        return (0, 0);
    };
    let inner = &branch_line[start + 1..];
    let inner = inner.split(']').next().unwrap_or("");
    let mut ahead = 0;
    let mut behind = 0;
    for part in inner.split(',') {
        let part = part.trim();
        if let Some(n) = part.strip_prefix("ahead ") {
            ahead = n.trim().parse().unwrap_or(0);
        } else if let Some(n) = part.strip_prefix("behind ") {
            behind = n.trim().parse().unwrap_or(0);
        }
    }
    (ahead, behind)
}

impl GitTransport for SystemGit {
    fn fetch(&mut self) -> Result<()> {
        let out = self.run(&["fetch", &self.remote.clone()])?;
        if out.ok {
            Ok(())
        } else {
            Err(Error::Other(format!("git fetch failed: {}", short_detail(&out))))
        }
    }

    fn integrate(&mut self) -> Result<IntegrateOutcome> {
        if self.rebase_in_progress() {
            return Ok(IntegrateOutcome::Conflict {
                detail: "a rebase is already in progress in this clone".into(),
            });
        }
        let before = self.head_sha();
        let had_local = self.head_status()?.ahead > 0;
        let out = self.run(&["pull", "--rebase", &self.remote.clone(), &self.branch.clone()])?;
        if !out.ok {
            let conflicted = self.conflicted_paths();
            if self.rebase_in_progress() || !conflicted.is_empty() {
                let detail = if conflicted.is_empty() {
                    short_detail(&out)
                } else {
                    format!("{} · conflicted: {}", short_detail(&out), conflicted.join(", "))
                };
                // S8's settled recovery. Not auto-resolution: the local
                // commits come back untouched and nothing is pushed.
                let _ = self.run(&["rebase", "--abort"]);
                return Ok(IntegrateOutcome::Conflict { detail });
            }
            return Err(Error::Other(format!(
                "git pull --rebase failed: {}",
                short_detail(&out)
            )));
        }
        let after = self.head_sha();
        if before.is_some() && before == after {
            return Ok(IntegrateOutcome::UpToDate);
        }
        let commits = match &before {
            Some(b) => self.count_commits(&format!("{b}..HEAD")),
            None => 0,
        };
        Ok(if had_local {
            IntegrateOutcome::Rebased { commits }
        } else {
            IntegrateOutcome::FastForward { commits }
        })
    }

    fn push(&mut self) -> Result<PushOutcome> {
        let ahead = self.head_status()?.ahead;
        if ahead == 0 {
            return Ok(PushOutcome::UpToDate);
        }
        // First push of a repo we created ourselves has no upstream yet;
        // set it so every later `git status` can report ahead/behind
        // directly.
        let set_upstream = !self.ref_exists("@{u}");
        let (remote, branch) = (self.remote.clone(), self.branch.clone());
        let mut args = vec!["push"];
        if set_upstream {
            args.push("--set-upstream");
        }
        args.extend([remote.as_str(), branch.as_str()]);
        let out = self.run(&args)?;
        if out.ok {
            return Ok(PushOutcome::Pushed { commits: ahead });
        }
        let detail = short_detail(&out);
        let lower = detail.to_ascii_lowercase();
        // S8 Q1: rejection under contention is the normal case, and git
        // names it consistently across hosts.
        if lower.contains("non-fast-forward")
            || lower.contains("fetch first")
            || lower.contains("updates were rejected")
            || lower.contains("[rejected]")
        {
            return Ok(PushOutcome::NonFastForward { detail });
        }
        Ok(PushOutcome::Failed { detail })
    }

    fn head_status(&self) -> Result<HeadStatus> {
        let out = self.run(&["status", "--porcelain=v1", "-b", "--untracked-files=no"])?;
        // S8 Q2: `git status` exits 0 even mid-rebase, so its output — not
        // its exit code — is the source of truth. A nonzero exit here means
        // the command itself failed (not a repo, permissions), which is
        // worth reporting.
        if !out.ok {
            return Err(Error::Other(format!("git status failed: {}", short_detail(&out))));
        }
        let mut status = HeadStatus { rebase_in_progress: self.rebase_in_progress(), ..HeadStatus::default() };
        let mut saw_upstream = false;
        for (i, line) in out.stdout.lines().enumerate() {
            if i == 0 && line.starts_with("## ") {
                // `## <branch>...<upstream> [ahead N, behind M]` — the
                // `...` half is only there when an upstream is configured.
                saw_upstream = line.contains("...");
                let (ahead, behind) = parse_ahead_behind(line);
                status.ahead = ahead;
                status.behind = behind;
                if line.contains("(no branch)") {
                    status.rebase_in_progress = true;
                }
                continue;
            }
            if !line.trim().is_empty() {
                status.dirty = true;
            }
        }
        // `...origin/main [gone]` counts as "declared but not there": a
        // clone of an empty repo configures the tracking branch before the
        // ref it names exists, and porcelain then reports no ahead count
        // at all.
        if !saw_upstream || !self.ref_exists("@{u}") {
            status.ahead = self.ahead_without_upstream();
        }
        Ok(status)
    }

    fn exists(&self, rel_path: &str) -> bool {
        self.root.join(rel_path).exists()
    }

    fn read(&self, rel_path: &str) -> Option<String> {
        std::fs::read_to_string(self.root.join(rel_path)).ok()
    }

    fn write_and_commit(
        &mut self,
        writes: &[PendingWrite],
        message: &str,
    ) -> Result<CommitOutcome> {
        if writes.is_empty() {
            return Ok(CommitOutcome { files: 0, committed: false });
        }
        for w in writes {
            let path = self.root.join(&w.rel_path);
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
            }
            std::fs::write(&path, &w.content).map_err(|e| Error::io(&path, e))?;
        }
        let mut args: Vec<&str> = vec!["add", "--"];
        for w in writes {
            args.push(&w.rel_path);
        }
        let out = self.run(&args)?;
        if !out.ok {
            return Err(Error::Other(format!("git add failed: {}", short_detail(&out))));
        }
        // Nothing actually changed — don't manufacture an empty commit.
        if self.run(&["diff", "--cached", "--quiet"])?.ok {
            return Ok(CommitOutcome { files: writes.len(), committed: false });
        }
        let out = self.run(&["commit", "-m", message])?;
        if !out.ok {
            return Err(Error::Other(format!("git commit failed: {}", short_detail(&out))));
        }
        Ok(CommitOutcome { files: writes.len(), committed: true })
    }
}

// ---------------------------------------------------------------------
// 1.3 FakeTransport
// ---------------------------------------------------------------------

/// One commit in the fake remote's linear history.
#[derive(Debug, Clone, PartialEq)]
pub struct FakeCommit {
    pub author: String,
    pub message: String,
    /// Path → new content. Deletions aren't modeled: family writes never
    /// delete another member's file, and a member's own archive move is a
    /// rename within their lane, which the sync loop doesn't reason about.
    pub changes: BTreeMap<String, String>,
}

/// An in-memory "remote": a linear commit list several [`FakeTransport`]
/// clones share, so a test can play two (or three) Kens against each other
/// with no filesystem and no git.
#[derive(Debug, Default)]
pub struct FakeRemote {
    commits: Vec<FakeCommit>,
}

impl FakeRemote {
    pub fn new() -> Arc<Mutex<FakeRemote>> {
        Arc::new(Mutex::new(FakeRemote::default()))
    }

    /// A remote with one initial commit containing `files` — the template
    /// scaffold, in practice.
    pub fn seeded(files: &[PendingWrite]) -> Arc<Mutex<FakeRemote>> {
        let remote = FakeRemote::new();
        remote.lock().expect("fake remote").commits.push(FakeCommit {
            author: "scaffold".into(),
            message: "Create family".into(),
            changes: files.iter().map(|w| (w.rel_path.clone(), w.content.clone())).collect(),
        });
        remote
    }

    pub fn len(&self) -> usize {
        self.commits.len()
    }

    pub fn is_empty(&self) -> bool {
        self.commits.is_empty()
    }

    pub fn commits(&self) -> &[FakeCommit] {
        &self.commits
    }

    /// The working tree after the first `n` commits.
    pub fn tree_at(&self, n: usize) -> BTreeMap<String, String> {
        let mut tree = BTreeMap::new();
        for c in self.commits.iter().take(n) {
            for (p, content) in &c.changes {
                tree.insert(p.clone(), content.clone());
            }
        }
        tree
    }

    pub fn tree(&self) -> BTreeMap<String, String> {
        self.tree_at(self.commits.len())
    }
}

/// One member's clone of a [`FakeRemote`].
pub struct FakeTransport {
    remote: Arc<Mutex<FakeRemote>>,
    author: String,
    /// Remote commits already integrated locally.
    base: usize,
    /// Remote length the last fetch/integrate learned about.
    fetched: usize,
    /// Committed but unpushed.
    local: Vec<FakeCommit>,
    tree: BTreeMap<String, String>,
    /// How many times `fetch` ran — lets a test assert that a halted
    /// engine touched nothing at all.
    pub fetches: usize,
    /// Make the next fetch fail with this message.
    pub fail_fetch: Option<String>,
    /// Commits appended to the remote immediately *before* a push, one per
    /// push, simulating a teammate who pushed in the window between our
    /// integrate and our push (S8 Q1's measured common case).
    pub race_before_push: Vec<FakeCommit>,
}

impl FakeTransport {
    /// A fresh clone: everything currently on the remote, nothing local.
    pub fn clone_of(remote: &Arc<Mutex<FakeRemote>>, author: &str) -> FakeTransport {
        let (len, tree) = {
            let r = remote.lock().expect("fake remote");
            (r.len(), r.tree())
        };
        FakeTransport {
            remote: Arc::clone(remote),
            author: author.to_string(),
            base: len,
            fetched: len,
            local: Vec::new(),
            tree,
            fetches: 0,
            fail_fetch: None,
            race_before_push: Vec::new(),
        }
    }

    pub fn tree(&self) -> &BTreeMap<String, String> {
        &self.tree
    }

    pub fn local_commits(&self) -> &[FakeCommit] {
        &self.local
    }

    /// Commit straight into the local history with **no lane check** —
    /// the "another Ken (or a human) misbehaved" case D1's risks section
    /// names, and the only way to manufacture a conflict in a repo whose
    /// lanes are working.
    pub fn commit_unchecked(&mut self, writes: &[PendingWrite], message: &str) -> CommitOutcome {
        self.write_and_commit(writes, message).expect("fake commit")
    }

    fn rebuild_tree(&mut self) {
        let mut tree = {
            let r = self.remote.lock().expect("fake remote");
            r.tree_at(self.base)
        };
        for c in &self.local {
            for (p, content) in &c.changes {
                tree.insert(p.clone(), content.clone());
            }
        }
        self.tree = tree;
    }
}

impl GitTransport for FakeTransport {
    fn fetch(&mut self) -> Result<()> {
        self.fetches += 1;
        if let Some(reason) = self.fail_fetch.take() {
            return Err(Error::Other(reason));
        }
        self.fetched = self.remote.lock().expect("fake remote").len();
        Ok(())
    }

    fn integrate(&mut self) -> Result<IntegrateOutcome> {
        // `git pull --rebase` fetches first; so does this.
        self.fetched = self.remote.lock().expect("fake remote").len();
        let incoming: Vec<FakeCommit> = {
            let r = self.remote.lock().expect("fake remote");
            r.commits[self.base..self.fetched].to_vec()
        };
        if incoming.is_empty() {
            return Ok(IntegrateOutcome::UpToDate);
        }
        let incoming_paths: BTreeSet<&String> =
            incoming.iter().flat_map(|c| c.changes.keys()).collect();
        let clash: Vec<String> = self
            .local
            .iter()
            .flat_map(|c| c.changes.keys())
            .filter(|p| incoming_paths.contains(*p))
            .cloned()
            .collect();
        if !clash.is_empty() {
            // Exactly what lanes make impossible — and exactly what must
            // halt the loop when it happens anyway.
            return Ok(IntegrateOutcome::Conflict {
                detail: format!("CONFLICT (content): {}", clash.join(", ")),
            });
        }
        let commits = incoming.len();
        let had_local = !self.local.is_empty();
        self.base = self.fetched;
        self.rebuild_tree();
        Ok(if had_local {
            IntegrateOutcome::Rebased { commits }
        } else {
            IntegrateOutcome::FastForward { commits }
        })
    }

    fn push(&mut self) -> Result<PushOutcome> {
        if !self.race_before_push.is_empty() {
            let racer = self.race_before_push.remove(0);
            self.remote.lock().expect("fake remote").commits.push(racer);
        }
        let mut r = self.remote.lock().expect("fake remote");
        if r.len() != self.base {
            return Ok(PushOutcome::NonFastForward {
                detail: "! [rejected] main -> main (non-fast-forward)".into(),
            });
        }
        if self.local.is_empty() {
            return Ok(PushOutcome::UpToDate);
        }
        let commits = self.local.len();
        r.commits.extend(self.local.drain(..));
        self.base = r.len();
        self.fetched = self.base;
        Ok(PushOutcome::Pushed { commits })
    }

    fn head_status(&self) -> Result<HeadStatus> {
        Ok(HeadStatus {
            ahead: self.local.len(),
            behind: self.fetched.saturating_sub(self.base),
            dirty: false,
            rebase_in_progress: false,
        })
    }

    fn exists(&self, rel_path: &str) -> bool {
        self.tree.contains_key(rel_path)
    }

    fn read(&self, rel_path: &str) -> Option<String> {
        self.tree.get(rel_path).cloned()
    }

    fn write_and_commit(
        &mut self,
        writes: &[PendingWrite],
        message: &str,
    ) -> Result<CommitOutcome> {
        let mut changes = BTreeMap::new();
        for w in writes {
            if self.tree.get(&w.rel_path) != Some(&w.content) {
                changes.insert(w.rel_path.clone(), w.content.clone());
            }
        }
        if changes.is_empty() {
            return Ok(CommitOutcome { files: writes.len(), committed: false });
        }
        for (p, content) in &changes {
            self.tree.insert(p.clone(), content.clone());
        }
        self.local.push(FakeCommit {
            author: self.author.clone(),
            message: message.to_string(),
            changes,
        });
        Ok(CommitOutcome { files: writes.len(), committed: true })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::family::{
        self, FamilyMember, InboxKind, InboxStatus, InboxTaskPayload, Lane, NewInboxItem,
    };
    use uuid::Uuid;

    fn scaffold() -> Vec<PendingWrite> {
        family::scaffold_family(
            "Team",
            Uuid::nil(),
            &[FamilyMember::new("owner", "Ada"), FamilyMember::new("sarah", "Sarah")],
        )
        .unwrap()
        .into_iter()
        .map(|f| PendingWrite::new(f.rel_path, f.content))
        .collect()
    }

    fn task_item(from: &str, title: &str, id: &str) -> String {
        family::render_inbox_item(
            &NewInboxItem {
                id: None,
                kind: Some(InboxKind::Task),
                from: from.into(),
                title: title.into(),
                body: "Context for the work.".into(),
                task: Some(InboxTaskPayload {
                    title: title.into(),
                    project: "ken".into(),
                    tags: vec!["family".into()],
                    due: String::new(),
                    kind: "human".into(),
                }),
            },
            id,
            "2026-08-03",
        )
    }

    fn deliver(to: &str, id: &str, text: &str) -> PendingWrite {
        PendingWrite::new(
            format!("{}/{id}-item.md", family::inbox_rel(to)),
            text.to_string(),
        )
    }

    // ---- 1.4 the loop ----

    #[test]
    fn poll_fetches_integrates_and_pushes() {
        let remote = FakeRemote::seeded(&scaffold());
        let mut sarah = FakeTransport::clone_of(&remote, "sarah");
        let mut engine = SyncEngine::new();

        // Nothing to do.
        let report = engine.poll(&mut sarah);
        assert!(report.ran);
        assert_eq!(report.integrated, Some(IntegrateOutcome::UpToDate));
        assert_eq!(report.pushed, Some(PushOutcome::UpToDate));
        assert_eq!(*engine.state(), ConnectionState::Idle);

        // A delivery into the owner's inbox: lane rule 2, create-only.
        let item = task_item("sarah", "Review the sync loop", "01ITEM");
        engine
            .commit(
                &mut sarah,
                &Lane::member("sarah"),
                &[deliver("owner", "01ITEM", &item)],
                "Ken: deliver inbox item",
            )
            .unwrap();
        assert_eq!(sarah.head_status().unwrap().ahead, 1);

        let report = engine.poll(&mut sarah);
        assert_eq!(report.pushed, Some(PushOutcome::Pushed { commits: 1 }));
        assert!(!report.push_retried);
        assert_eq!(*engine.state(), ConnectionState::Idle);
        assert_eq!(remote.lock().unwrap().len(), 2);
    }

    #[test]
    fn non_fast_forward_push_retries_once_after_reintegrating() {
        let remote = FakeRemote::seeded(&scaffold());
        let mut sarah = FakeTransport::clone_of(&remote, "sarah");
        let mut engine = SyncEngine::new();

        engine
            .commit(
                &mut sarah,
                &Lane::member("sarah"),
                &[deliver("owner", "01ITEM", &task_item("sarah", "A", "01ITEM"))],
                "deliver",
            )
            .unwrap();

        // A teammate lands a commit in the window between our integrate
        // and our push.
        sarah.race_before_push.push(FakeCommit {
            author: "dave".into(),
            message: "deliver".into(),
            changes: [("members/owner/inbox/01OTHER-item.md".to_string(), "x".to_string())]
                .into_iter()
                .collect(),
        });

        let report = engine.poll(&mut sarah);
        assert!(report.push_retried);
        assert_eq!(report.pushed, Some(PushOutcome::Pushed { commits: 1 }));
        assert_eq!(*engine.state(), ConnectionState::Idle);

        let tree = remote.lock().unwrap().tree();
        assert!(tree.contains_key("members/owner/inbox/01ITEM-item.md"));
        assert!(tree.contains_key("members/owner/inbox/01OTHER-item.md"));
    }

    #[test]
    fn a_second_rejection_becomes_an_error_not_a_third_try() {
        let remote = FakeRemote::seeded(&scaffold());
        let mut sarah = FakeTransport::clone_of(&remote, "sarah");
        let mut engine = SyncEngine::new();
        engine
            .commit(
                &mut sarah,
                &Lane::member("sarah"),
                &[deliver("owner", "01ITEM", &task_item("sarah", "A", "01ITEM"))],
                "deliver",
            )
            .unwrap();

        for n in 0..2 {
            sarah.race_before_push.push(FakeCommit {
                author: "dave".into(),
                message: "deliver".into(),
                changes: [(format!("members/owner/inbox/0{n}RACE-item.md"), "x".to_string())]
                    .into_iter()
                    .collect(),
            });
        }

        let report = engine.poll(&mut sarah);
        assert!(report.push_retried);
        assert!(matches!(report.pushed, Some(PushOutcome::NonFastForward { .. })));
        assert!(matches!(engine.state(), ConnectionState::Error { .. }));
        // Error is transient, not a halt: the next poll may proceed.
        assert!(!engine.is_halted());
        let report = engine.poll(&mut sarah);
        assert!(report.ran);
        assert_eq!(report.pushed, Some(PushOutcome::Pushed { commits: 1 }));
    }

    #[test]
    fn concurrent_senders_both_land_in_one_inbox() {
        let remote = FakeRemote::seeded(&scaffold());
        let mut sarah = FakeTransport::clone_of(&remote, "sarah");
        let mut dave = FakeTransport::clone_of(&remote, "dave");
        let mut owner = FakeTransport::clone_of(&remote, "owner");
        let (mut es, mut ed, mut eo) = (SyncEngine::new(), SyncEngine::new(), SyncEngine::new());

        es.commit(
            &mut sarah,
            &Lane::member("sarah"),
            &[deliver("owner", "01FROMSARAH", &task_item("sarah", "From Sarah", "01FROMSARAH"))],
            "deliver",
        )
        .unwrap();
        ed.commit(
            &mut dave,
            &Lane::member("dave"),
            &[deliver("owner", "01FROMDAVE", &task_item("dave", "From Dave", "01FROMDAVE"))],
            "deliver",
        )
        .unwrap();

        // Both push between the owner's polls; the second one rebases.
        assert_eq!(es.poll(&mut sarah).pushed, Some(PushOutcome::Pushed { commits: 1 }));
        let dave_report = ed.poll(&mut dave);
        assert_eq!(dave_report.integrated, Some(IntegrateOutcome::Rebased { commits: 1 }));
        assert_eq!(dave_report.pushed, Some(PushOutcome::Pushed { commits: 1 }));

        let owner_report = eo.poll(&mut owner);
        assert_eq!(owner_report.integrated, Some(IntegrateOutcome::FastForward { commits: 2 }));
        assert_eq!(*eo.state(), ConnectionState::Idle);
        assert!(owner.exists("members/owner/inbox/01FROMSARAH-item.md"));
        assert!(owner.exists("members/owner/inbox/01FROMDAVE-item.md"));
    }

    #[test]
    fn a_conflict_halts_the_loop_and_freezes_writes() {
        let remote = FakeRemote::seeded(&scaffold());
        let mut owner = FakeTransport::clone_of(&remote, "owner");
        let mut stray = FakeTransport::clone_of(&remote, "hand-edit");
        let mut engine = SyncEngine::new();

        // The owner edits shared knowledge (their lane, D3)...
        engine
            .commit(
                &mut owner,
                &Lane::owner("owner"),
                &[PendingWrite::new("shared/conventions.md", "owner's version\n")],
                "shared update",
            )
            .unwrap();
        // ...while something outside the lane rules changed the same file
        // and got there first.
        stray.commit_unchecked(
            &[PendingWrite::new("shared/conventions.md", "someone else's version\n")],
            "hand edit",
        );
        assert_eq!(stray.push().unwrap(), PushOutcome::Pushed { commits: 1 });

        let report = engine.poll(&mut owner);
        assert!(report.ran);
        assert!(matches!(report.integrated, Some(IntegrateOutcome::Conflict { .. })));
        assert_eq!(report.pushed, None);
        assert!(engine.is_halted());
        match engine.state() {
            ConnectionState::Conflict { detail } => assert!(detail.contains("conventions.md")),
            other => panic!("expected Conflict, got {other:?}"),
        }

        // Halted means halted: no git runs, and no new writes pile up.
        let fetches = owner.fetches;
        let report = engine.poll(&mut owner);
        assert!(!report.ran);
        assert_eq!(owner.fetches, fetches);
        assert!(engine
            .commit(
                &mut owner,
                &Lane::owner("owner"),
                &[PendingWrite::new("shared/x.md", "no")],
                "nope"
            )
            .is_err());
        // The remote is untouched by the failed cycle.
        assert_eq!(remote.lock().unwrap().len(), 2);

        // Only a human clears it.
        engine.resolved();
        assert!(!engine.is_halted());
    }

    #[test]
    fn an_unavailable_connection_never_touches_git() {
        let remote = FakeRemote::seeded(&scaffold());
        let mut owner = FakeTransport::clone_of(&remote, "owner");
        let mut engine = SyncEngine::unavailable("this family repo needs a newer Ken");

        let report = engine.poll(&mut owner);
        assert!(!report.ran);
        assert_eq!(owner.fetches, 0);
        assert!(engine
            .commit(&mut owner, &Lane::member("owner"), &[], "x")
            .is_err());
        // Unavailable is not a conflict — `resolved` must not clear it.
        engine.resolved();
        assert!(engine.is_halted());
    }

    #[test]
    fn a_failed_fetch_is_transient_not_a_halt() {
        let remote = FakeRemote::seeded(&scaffold());
        let mut owner = FakeTransport::clone_of(&remote, "owner");
        let mut engine = SyncEngine::new();
        owner.fail_fetch = Some("Could not resolve host: example.invalid".into());

        let report = engine.poll(&mut owner);
        assert!(report.ran);
        assert_eq!(report.integrated, None);
        match engine.state() {
            ConnectionState::Error { detail } => assert!(detail.contains("Could not resolve host")),
            other => panic!("expected Error, got {other:?}"),
        }
        assert!(!engine.is_halted());
        assert_eq!(engine.poll(&mut owner).state, ConnectionState::Idle);
    }

    // ---- lane enforcement at the commit boundary ----

    #[test]
    fn commit_paths_refuses_a_cross_lane_write_and_writes_nothing() {
        let remote = FakeRemote::seeded(&scaffold());
        let mut sarah = FakeTransport::clone_of(&remote, "sarah");
        let mut engine = SyncEngine::new();

        let err = engine
            .commit(
                &mut sarah,
                &Lane::member("sarah"),
                &[
                    deliver("owner", "01OK", "fine"),
                    PendingWrite::new("members/owner/board/01BAD-x.md", "not mine"),
                ],
                "mixed batch",
            )
            .unwrap_err();
        assert!(err.to_string().contains("this is a bug"));
        // The whole batch is refused — including the path that was legal.
        assert!(!sarah.exists("members/owner/inbox/01OK-item.md"));
        assert!(sarah.local_commits().is_empty());
    }

    #[test]
    fn commit_paths_takes_is_new_from_the_working_tree_not_the_caller() {
        let remote = FakeRemote::seeded(&scaffold());
        let mut sarah = FakeTransport::clone_of(&remote, "sarah");
        let mut engine = SyncEngine::new();
        let write = deliver("owner", "01ITEM", &task_item("sarah", "A", "01ITEM"));

        // First delivery: a create, allowed by lane rule 2.
        engine.commit(&mut sarah, &Lane::member("sarah"), &[write.clone()], "deliver").unwrap();
        // Same path again: now it exists, so it is an edit of someone
        // else's file — refused no matter how the caller frames it.
        let err = engine
            .commit(
                &mut sarah,
                &Lane::member("sarah"),
                &[PendingWrite::new(write.rel_path.clone(), "rewritten")],
                "deliver again",
            )
            .unwrap_err();
        assert!(err.to_string().contains("create-only"));
        assert_eq!(sarah.read(&write.rel_path).as_deref(), Some(write.content.as_str()));
    }

    // ---- the two-Ken round trip (task 5.2, at the core layer) ----

    #[test]
    fn sarah_sends_owner_accepts_and_completes_sarah_sees_it() {
        let remote = FakeRemote::seeded(&scaffold());
        let mut sarah = FakeTransport::clone_of(&remote, "sarah");
        let mut owner = FakeTransport::clone_of(&remote, "owner");
        let (mut es, mut eo) = (SyncEngine::new(), SyncEngine::new());

        // 1. Sarah sends a task.
        let item = task_item("sarah", "Review the sync loop", "01ITEM");
        es.commit(
            &mut sarah,
            &Lane::member("sarah"),
            &[deliver("owner", "01ITEM", &item)],
            "Ken: deliver inbox item",
        )
        .unwrap();
        es.poll(&mut sarah);

        // 2. The owner polls and sees it unread.
        let report = eo.poll(&mut owner);
        assert_eq!(report.integrated, Some(IntegrateOutcome::FastForward { commits: 1 }));
        let item_path = "members/owner/inbox/01ITEM-item.md";
        let raw = owner.read(item_path).unwrap();
        let parsed = family::parse_inbox_item("01ITEM-item.md", &raw);
        assert_eq!(parsed.status, Some(InboxStatus::Unread));
        assert!(parsed.is_pending_task());

        // 3. The owner accepts — the gate, and the only way onto a board.
        let accepted = family::accept_task(&raw, "owner", "01TASK", "2026-08-04", "10:15").unwrap();
        eo.commit(
            &mut owner,
            &Lane::member("owner"),
            &[
                PendingWrite::new(&accepted.board_rel_path, &accepted.board_content),
                PendingWrite::new(item_path, &accepted.inbox_content),
            ],
            "Ken: accept inbox task",
        )
        .unwrap();
        let report = eo.poll(&mut owner);
        assert_eq!(report.pushed, Some(PushOutcome::Pushed { commits: 1 }));

        // 4. Sarah polls and sees the task on the owner's board, with the
        //    item she sent marked accepted — no conflicts, no lost writes.
        let report = es.poll(&mut sarah);
        assert!(matches!(
            report.integrated,
            Some(IntegrateOutcome::FastForward { .. }) | Some(IntegrateOutcome::Rebased { .. })
        ));
        let board = sarah.read(&accepted.board_rel_path).unwrap();
        assert!(board.contains("assignee: owner"));
        assert!(board.contains("01ITEM"));
        let echoed = family::parse_inbox_item("01ITEM-item.md", &sarah.read(item_path).unwrap());
        assert_eq!(echoed.status, Some(InboxStatus::Accepted));

        // Sarah may read the owner's board but never write it.
        assert!(es
            .commit(
                &mut sarah,
                &Lane::member("sarah"),
                &[PendingWrite::new(&accepted.board_rel_path, "mine now")],
                "nope"
            )
            .is_err());
        assert_eq!(*es.state(), ConnectionState::Idle);
    }

    // ---- SystemGit ----

    #[test]
    fn git_availability_is_probed_once_and_cached() {
        let first = git_available();
        let second = git_available();
        assert_eq!(first, second);
        // Whichever way it went, the pointers are the same cached values.
        match (first, second) {
            (Ok(a), Ok(b)) => assert!(std::ptr::eq(a, b)),
            (Err(a), Err(b)) => assert!(std::ptr::eq(a, b)),
            _ => unreachable!("cached probe changed its mind"),
        }
    }

    #[test]
    fn ahead_behind_is_parsed_from_porcelain_not_an_exit_code() {
        assert_eq!(parse_ahead_behind("## main...origin/main"), (0, 0));
        assert_eq!(parse_ahead_behind("## main...origin/main [ahead 3]"), (3, 0));
        assert_eq!(parse_ahead_behind("## main...origin/main [behind 2]"), (0, 2));
        assert_eq!(parse_ahead_behind("## main...origin/main [ahead 1, behind 2]"), (1, 2));
        assert_eq!(parse_ahead_behind("## HEAD (no branch)"), (0, 0));
    }

    /// End-to-end smoke test for [`SystemGit`] against a real local bare
    /// repo (`file://`, the shape S8 exercised). Skipped when `git` isn't
    /// available or the sandbox refuses to init a repo — this proves the
    /// shell-out plumbing, while every *decision* the loop makes is
    /// covered by the `FakeTransport` tests above.
    #[test]
    fn system_git_drives_a_real_local_clone() {
        if git_available().is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let bare = dir.path().join("remote.git");
        let work = dir.path().join("clone");
        let id = GitIdentity { name: "Ken Test".into(), email: "ken@example.invalid".into() };

        let init = Command::new("git")
            .args(["init", "--bare", "--initial-branch=main"])
            .arg(&bare)
            .output();
        match init {
            Ok(o) if o.status.success() => {}
            _ => return, // no usable git in this sandbox — skip, don't fail
        }
        let clone_into = |target: &Path| {
            Command::new("git")
                .args(clone_config_args())
                .args(["clone", "--"])
                .arg(&bare)
                .arg(target)
                .env("GIT_TERMINAL_PROMPT", "0")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        if !clone_into(&work) {
            return; // no usable git in this sandbox — skip, don't fail
        }

        let mut git = SystemGit::new(&work, "origin", "main");
        git.identity = Some(id.clone());
        git.configure_clone().unwrap();

        // The bootstrap commit: the whole template, into an empty repo.
        let files = scaffold();
        let out = git.commit_paths(&Lane::bootstrap(), &files, "Create family").unwrap();
        assert!(out.committed);
        assert_eq!(git.head_status().unwrap().ahead, 1);
        assert_eq!(git.push().unwrap(), PushOutcome::Pushed { commits: 1 });

        // A second clone plays the other member.
        let work2 = dir.path().join("clone2");
        assert!(clone_into(&work2), "second clone should succeed");
        let mut git2 = SystemGit::new(&work2, "origin", "main");
        git2.identity = Some(id);
        git2.configure_clone().unwrap();

        // Sarah delivers into the owner's inbox; the owner integrates it.
        let item = task_item("sarah", "Review the sync loop", "01ITEM");
        let mut engine2 = SyncEngine::new();
        engine2
            .commit(
                &mut git2,
                &Lane::member("sarah"),
                &[deliver("owner", "01ITEM", &item)],
                "Ken: deliver inbox item",
            )
            .unwrap();
        let report = engine2.poll(&mut git2);
        assert_eq!(report.pushed, Some(PushOutcome::Pushed { commits: 1 }), "{report:?}");

        let mut engine1 = SyncEngine::new();
        let report = engine1.poll(&mut git);
        assert!(
            matches!(report.integrated, Some(IntegrateOutcome::FastForward { commits: 1 })),
            "{report:?}"
        );
        assert_eq!(*engine1.state(), ConnectionState::Idle);
        let raw = git.read("members/owner/inbox/01ITEM-item.md").unwrap();
        assert_eq!(
            family::parse_inbox_item("01ITEM-item.md", &raw).status,
            Some(InboxStatus::Unread)
        );

        // And a cross-lane write is refused by the real transport too.
        assert!(git2
            .commit_paths(
                &Lane::member("sarah"),
                &[PendingWrite::new("members/owner/board/01BAD.md", "no")],
                "nope"
            )
            .is_err());
        assert!(!work2.join("members/owner/board/01BAD.md").exists());
    }
}

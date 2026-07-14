//! Ingest runner: drives the user's local Claude Code CLI. Two modes —
//! hidden-TUI (default; a real interactive session in a PTY, completion via
//! Stop hook) and headless (`claude -p`, completion via process exit).
//! All CLI interaction lives here so flag drift has one place to hurt.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

use crate::assistant::{self, ParsedOutput};
use crate::hooks::HookListener;
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunnerMode {
    HiddenTui,
    Headless,
}

#[derive(Debug, Clone)]
pub struct RunnerConfig {
    pub binary: PathBuf,
    pub mode: RunnerMode,
    pub timeout: Duration,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RunOutcome {
    Completed,
    Cancelled,
    /// Hit the run timeout; carries the tail of the session's output.
    TimedOut(String),
    /// Process died before completing; carries diagnostic detail.
    Failed(String),
}

#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> CancelToken {
        CancelToken::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Find the claude CLI: PATH first, then the usual install locations that
/// GUI apps' skinny PATH misses.
pub fn discover_claude() -> Option<PathBuf> {
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join("claude");
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    let home = dirs::home_dir()?;
    for candidate in [
        home.join(".local/bin/claude"),
        home.join(".claude/local/claude"),
        PathBuf::from("/opt/homebrew/bin/claude"),
        PathBuf::from("/usr/local/bin/claude"),
    ] {
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

pub(crate) fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.is_file()
            && path
                .metadata()
                .map(|m| m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Claude Code stores sessions under `~/.claude/projects/<path-with-slashes-
/// as-dashes>/<session-id>.jsonl`; existence means the session got past its
/// startup gates.
pub fn session_file_exists(project_root: &Path, session_id: &str) -> bool {
    let Some(home) = dirs::home_dir() else {
        return true; // can't check — assume fine rather than false-alarm
    };
    let canonical = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let encoded = canonical.to_string_lossy().replace(['/', '\\'], "-");
    home.join(".claude/projects")
        .join(encoded)
        .join(format!("{session_id}.jsonl"))
        .is_file()
}

pub const MISSING_CLAUDE_HELP: &str = "Claude Code isn't installed. Install it with:  npm install -g @anthropic-ai/claude-code  — then run `claude` once to log in. Everything else in Ken keeps working meanwhile.";

/// Run one agent session to completion. Synchronous — call from a worker
/// thread. `on_blocked` fires (once per event) when the agent signals it is
/// waiting on user input.
pub fn run_session(
    cfg: &RunnerConfig,
    project_root: &Path,
    session_id: &str,
    prompt: &str,
    hooks: &HookListener,
    cancel: &CancelToken,
    mut on_blocked: impl FnMut(),
) -> Result<RunOutcome> {
    if !is_executable(&cfg.binary) {
        return Ok(RunOutcome::Failed(MISSING_CLAUDE_HELP.to_string()));
    }
    match cfg.mode {
        RunnerMode::HiddenTui => {
            run_hidden_tui(cfg, project_root, session_id, prompt, hooks, cancel, &mut on_blocked)
        }
        RunnerMode::Headless => run_headless(cfg, project_root, session_id, prompt, cancel),
    }
}

fn run_hidden_tui(
    cfg: &RunnerConfig,
    project_root: &Path,
    session_id: &str,
    prompt: &str,
    hooks: &HookListener,
    cancel: &CancelToken,
    on_blocked: &mut impl FnMut(),
) -> Result<RunOutcome> {
    let rx = hooks.subscribe(session_id);
    let result = (|| {
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows: 50,
                cols: 200,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| Error::Other(format!("pty: {e}")))?;

        let mut cmd = CommandBuilder::new(&cfg.binary);
        cmd.args([
            "--session-id",
            session_id,
            "--permission-mode",
            "acceptEdits",
            prompt,
        ]);
        cmd.cwd(project_root);
        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| Error::Other(format!("spawn {}: {e}", cfg.binary.display())))?;
        drop(pair.slave);

        // Drain output into a capped ring buffer for diagnostics, and
        // broadcast it through the PTY registry so the chat drawer can
        // watch (and type into) this session live.
        let ring: Arc<Mutex<VecDeque<u8>>> = Arc::new(Mutex::new(VecDeque::new()));
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| Error::Other(format!("pty reader: {e}")))?;
        let writer: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(
            pair.master
                .take_writer()
                .map_err(|e| Error::Other(format!("pty writer: {e}")))?,
        ));
        let registration = Arc::new(crate::pty_registry::register(session_id, writer.clone()));
        let ring_w = ring.clone();
        let reg_reader = registration.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                reg_reader.broadcast(&buf[..n]);
                let mut r = ring_w.lock().unwrap();
                r.extend(&buf[..n]);
                while r.len() > 64 * 1024 {
                    r.pop_front();
                }
            }
        });

        let deadline = Instant::now() + cfg.timeout;
        // If the session hasn't materialized on disk shortly after spawn,
        // the TUI is almost certainly parked at an interactive startup gate
        // (trust dialog, hooks approval) that a hidden PTY can't answer.
        let gate_check_at = Instant::now() + Duration::from_secs(60);
        let mut gate_checked = false;
        loop {
            if cancel.is_cancelled() {
                let _ = child.kill();
                return Ok(RunOutcome::Cancelled);
            }
            if !gate_checked && Instant::now() > gate_check_at {
                gate_checked = true;
                if !session_file_exists(project_root, session_id) {
                    on_blocked();
                }
            }
            match rx.recv_timeout(Duration::from_millis(300)) {
                Ok(ev) if ev.event == "Stop" => {
                    // Ask the TUI to exit; force if it lingers.
                    {
                        let mut w = writer.lock().unwrap();
                        let _ = w.write_all(b"/exit\r");
                        let _ = w.flush();
                    }
                    let grace = Instant::now() + Duration::from_secs(5);
                    while Instant::now() < grace {
                        if child.try_wait().ok().flatten().is_some() {
                            return Ok(RunOutcome::Completed);
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    let _ = child.kill();
                    return Ok(RunOutcome::Completed);
                }
                Ok(ev) if ev.event == "Notification" => {
                    on_blocked();
                }
                Ok(_) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if let Ok(Some(status)) = child.try_wait() {
                        let tail = ring_tail(&ring);
                        return Ok(RunOutcome::Failed(format!(
                            "the agent exited before finishing (status {status:?}). Recent output:\n{tail}"
                        )));
                    }
                    if Instant::now() > deadline {
                        let _ = child.kill();
                        return Ok(RunOutcome::TimedOut(ring_tail(&ring)));
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    let _ = child.kill();
                    return Ok(RunOutcome::Failed("hook listener stopped".into()));
                }
            }
        }
    })();
    hooks.unsubscribe(session_id);
    result
}

fn ring_tail(ring: &Arc<Mutex<VecDeque<u8>>>) -> String {
    let r = ring.lock().unwrap();
    let bytes: Vec<u8> = r.iter().copied().collect();
    let text = String::from_utf8_lossy(&bytes);
    let cleaned = strip_ansi(&text);
    let tail: Vec<&str> = cleaned.lines().rev().take(15).collect();
    tail.into_iter().rev().collect::<Vec<_>>().join("\n")
}

/// Remove ANSI escape sequences from TUI output.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c2 in chars.by_ref() {
                    if c2.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn run_headless(
    cfg: &RunnerConfig,
    project_root: &Path,
    session_id: &str,
    prompt: &str,
    cancel: &CancelToken,
) -> Result<RunOutcome> {
    let child = std::process::Command::new(&cfg.binary)
        .args([
            "-p",
            prompt,
            "--output-format",
            "json",
            "--permission-mode",
            "acceptEdits",
            "--session-id",
            session_id,
        ])
        .current_dir(project_root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null())
        .spawn()
        .map_err(|e| Error::Other(format!("spawn {}: {e}", cfg.binary.display())))?;

    use assistant::DriveResult;
    let outcome = match assistant::drive_child(child, cfg.timeout, Duration::from_millis(200), cancel)
    {
        DriveResult::Exited(status, output, stderr) => match assistant::parse_oneshot_output(&output) {
            ParsedOutput::Success(_) if status.success() => RunOutcome::Completed,
            ParsedOutput::Success(_) => RunOutcome::Failed(assistant::with_stderr(
                format!("the run exited with status {status:?}"),
                &stderr,
            )),
            ParsedOutput::Error(msg) | ParsedOutput::Unusable(msg) => {
                RunOutcome::Failed(assistant::with_stderr(msg, &stderr))
            }
        },
        DriveResult::Cancelled => RunOutcome::Cancelled,
        DriveResult::TimedOut(tail) => RunOutcome::TimedOut(truncate(&tail, 2000).to_string()),
        DriveResult::WaitFailed(e) => RunOutcome::Failed(format!("wait failed: {e}")),
    };
    Ok(outcome)
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        let mut end = max;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}

#[cfg(test)]
pub mod test_support {
    //! A fake `claude` shell script for CI: honours the real CLI contract
    //! Ken relies on (args, hook POST, staging writes) with no network/AI.

    use std::path::{Path, PathBuf};

    /// Behaviours: complete | fail | hang | block | headless-fail |
    /// stream-die | stream-stall | stream-hang | stream-fail — selected via a
    /// `behavior` file next to the script (per-tempdir, so parallel tests
    /// never race).
    pub fn write_fake_claude(dir: &Path, behavior: &str) -> PathBuf {
        std::fs::write(dir.join("behavior"), behavior).unwrap();
        let path = dir.join("claude");
        let script = r#"#!/bin/bash
# Fake Claude Code CLI for ken-core tests.
BEHAVIOR=$(cat "$(dirname "$0")/behavior" 2>/dev/null || echo complete)

SESSION=""
PROMPT=""
HEADLESS=0
STREAM_INPUT=0
OUTFMT=""
args=("$@")
for ((i=0; i<${#args[@]}; i++)); do
  case "${args[$i]}" in
    --session-id|--resume) SESSION="${args[$((i+1))]}"; i=$((i+1));;
    -p) HEADLESS=1
        next="${args[$((i+1))]}"
        case "$next" in --*|"") ;; *) PROMPT="$next"; i=$((i+1));; esac;;
    --input-format)
        [ "${args[$((i+1))]}" = "stream-json" ] && STREAM_INPUT=1; i=$((i+1));;
    --permission-mode) i=$((i+1));;
    --output-format) OUTFMT="${args[$((i+1))]}"; i=$((i+1));;
    --verbose) ;;
    *) if [ -z "$PROMPT" ]; then PROMPT="${args[$i]}"; fi;;
  esac
done

# Conversation mode: JSONL in, events out. One assistant reply per user line.
if [ "$STREAM_INPUT" = "1" ]; then
  echo '{"type":"system","subtype":"init","session_id":"'"$SESSION"'"}'
  # Simulate a turn that stops draining stdin: never read, just stay alive so
  # the writer's pipe fills and its write blocks. Proves the engine doesn't
  # hold its map lock across a blocking send.
  if [ "$BEHAVIOR" = "stream-stall" ]; then
    sleep 300
    exit 0
  fi
  TURN=0
  while read -r line; do
    TURN=$((TURN+1))
    TEXT=$(printf '%s' "$line" | grep -o '"text":"[^"]*"' | head -1 | sed 's/^"text":"//;s/"$//')
    if printf '%s' "$TEXT" | grep -q usetool; then
      echo '{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"notes/meeting.md"}}]},"session_id":"'"$SESSION"'"}'
    fi
    echo '{"type":"assistant","message":{"content":[{"type":"text","text":"echo: '"$TEXT"'"}]},"session_id":"'"$SESSION"'"}'
    echo '{"type":"result","subtype":"success","is_error":false,"session_id":"'"$SESSION"'"}'
    if [ "$BEHAVIOR" = "stream-die" ] && [ "$TURN" -ge 1 ]; then
      exit 7
    fi
  done
  exit 0
fi

# Headless streamed output: `-p <prompt> --output-format stream-json --verbose`.
# Emits the same event shapes chat parses, writes staged outputs, then a
# terminal result. Behaviours reuse the BEHAVIOR file.
if [ "$HEADLESS" = "1" ] && [ "$OUTFMT" = "stream-json" ]; then
  STAGING=$(echo "$PROMPT" | grep -o 'STAGING_DIR=[^ ]*' | head -1 | cut -d= -f2)
  echo '{"type":"system","subtype":"init","session_id":"'"$SESSION"'"}'
  case "$BEHAVIOR" in
    stream-hang) sleep 300;;
    stream-fail)
      echo '{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"notes/a.md"}}]}}'
      echo '{"type":"result","subtype":"error_during_execution","is_error":true}'
      exit 4;;
    *)
      echo '{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"notes/a.md"}}]}}'
      echo '{"type":"assistant","message":{"content":[{"type":"text","text":"working on it"}]}}'
      # Stage the same People.md the object-mode path writes (for ingest apply).
      if [ -n "$STAGING" ]; then
        mkdir -p "$STAGING/knowledge" 2>/dev/null
        printf '%s' '# People

- Priya Natarajan — owns billing cutover
' > "$STAGING/knowledge/People.md"
      fi
      # Automation phase-1 announces a proposal file to write.
      PROPOSAL=$(echo "$PROMPT" | grep -o 'PROPOSAL_FILE=[^ ]*' | head -1 | cut -d= -f2-)
      if [ -n "$PROPOSAL" ]; then
        mkdir -p "$(dirname "$PROPOSAL")" 2>/dev/null
        printf '%s' '## Proposed actions

- Create Jira issue: follow up on billing cutover
' > "$PROPOSAL"
      fi
      echo '{"type":"result","subtype":"success","is_error":false,"result":"done"}'
      exit 0;;
  esac
fi

# Staging dir is announced in the prompt as: STAGING_DIR=<path>
STAGING=$(echo "$PROMPT" | grep -o 'STAGING_DIR=[^ ]*' | head -1 | cut -d= -f2)
# Research report path is announced as: OUTPUT_FILE=<path>
OUTFILE=$(echo "$PROMPT" | grep -o 'OUTPUT_FILE=[^ ]*' | head -1 | cut -d= -f2-)
HOOK_URL=""
if [ -f .claude/settings.local.json ]; then
  HOOK_URL=$(grep -o 'http://127.0.0.1:[0-9]*/ken-hook' .claude/settings.local.json | head -1)
fi

emit_hook() {
  local event="$1"
  if [ -n "$HOOK_URL" ]; then
    curl -s -X POST "$HOOK_URL" -H 'Content-Type: application/json' \
      -d "{\"hook_event_name\":\"$event\",\"session_id\":\"$SESSION\"}" >/dev/null 2>&1
  fi
}

write_outputs() {
  if [ -n "$STAGING" ]; then
    mkdir -p "$STAGING/$(dirname "${FAKE_CLAUDE_OUTPUT:-knowledge/People.md}")" 2>/dev/null
    printf '%s' "${FAKE_CLAUDE_CONTENT:-# People

- Priya Natarajan — owns billing cutover
}" > "$STAGING/${FAKE_CLAUDE_OUTPUT:-knowledge/People.md}"
  fi
  if [ -n "$OUTFILE" ]; then
    mkdir -p "$(dirname "$OUTFILE")" 2>/dev/null
    printf '%s' "Fake research report [1]

## Sources
1. https://example.com — supported the main claim
" > "$OUTFILE"
  fi
}

case "$BEHAVIOR" in
  fail) echo "boom: simulated crash"; exit 3;;
  hang) sleep 300;;
  block)
    emit_hook Notification
    sleep 300;;
  headless-fail)
    echo '{"is_error": true, "result": "simulated"}'; exit 0;;
  headless-array)
    # Real CLI shape since v2.x: an array of events, answer in the last one.
    echo '[{"type":"system","subtype":"init","session_id":"'"$SESSION"'"},{"type":"assistant","message":{"content":[{"type":"text","text":"OK"}]}},{"type":"rate_limit_event"},{"type":"result","subtype":"success","is_error":false,"result":"OK","duration_ms":12,"session_id":"'"$SESSION"'"}]'
    exit 0;;
  headless-array-error)
    echo '[{"type":"system","subtype":"init"},{"type":"result","subtype":"error_max_turns","is_error":true,"result":"hit the turn limit"}]'
    exit 0;;
  headless-array-noresult)
    # Clean exit, real work, but NO terminal result wrapper — the answer is
    # only in the last assistant message. Must be recovered as Completed.
    echo '[{"type":"system","subtype":"init"},{"type":"assistant","message":{"content":[{"type":"text","text":"recovered answer"}]}}]'
    exit 0;;
  headless-array-noresult-nonzero)
    # Same shape but a non-zero exit: the process failed, so recovery must NOT
    # mask it — still Failed.
    echo '[{"type":"system","subtype":"init"},{"type":"assistant","message":{"content":[{"type":"text","text":"partial"}]}}]'
    exit 5;;
  headless-stderr-fail)
    echo "API Error: credit balance too low" >&2
    echo '[{"type":"result","subtype":"error_during_execution","is_error":true}]'
    exit 0;;
  stderr-flood)
    # >64KB on stderr: fills the pipe buffer and blocks unless the parent drains it.
    for i in $(seq 1 4000); do echo "warn $i xxxxxxxxxxxxxxxxxxxxxxxxxxxxxx" >&2; done
    echo '[{"type":"result","subtype":"success","is_error":false,"result":"OK"}]'
    exit 0;;
  complete)
    sleep 0.2
    write_outputs
    if [ "$HEADLESS" = "1" ]; then
      # One-shot hook: a file named oneshot_result next to the script
      # becomes the result text (JSON-escaped), for assistant tests.
      ONESHOT="$(dirname "$0")/oneshot_result"
      if [ -f "$ONESHOT" ]; then
        ESCAPED=$(sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' "$ONESHOT" | awk '{printf "%s\\n", $0}')
        ESCAPED=${ESCAPED%\\n}
        echo '{"is_error": false, "result": "'"$ESCAPED"'"}'
        exit 0
      fi
      echo '{"is_error": false, "result": "done"}'
      exit 0
    fi
    emit_hook Stop
    # behave like a TUI: stay alive until /exit arrives on stdin
    while read -r line; do
      case "$line" in /exit*) exit 0;; esac
    done
    exit 0;;
esac
"#;
        std::fs::write(&path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::write_fake_claude;
    use super::*;
    use crate::hooks::{install_hooks, HookListener};

    fn setup(behavior: &str) -> (tempfile::TempDir, PathBuf, HookListener) {
        let dir = tempfile::tempdir().unwrap();
        let bin = write_fake_claude(dir.path(), behavior);
        let hooks = HookListener::start().unwrap();
        install_hooks(dir.path(), &hooks.hook_url()).unwrap();
        (dir, bin, hooks)
    }

    fn cfg(bin: &Path, mode: RunnerMode, timeout_secs: u64) -> RunnerConfig {
        RunnerConfig {
            binary: bin.to_path_buf(),
            mode,
            timeout: Duration::from_secs(timeout_secs),
        }
    }

    #[test]
    fn hidden_tui_completes_on_stop_hook() {
        let (dir, bin, hooks) = setup("complete");
        let staging = dir.path().join(".ken/.staging/people");
        let prompt = format!("Extract people. STAGING_DIR={}", staging.display());
        let outcome = run_session(
            &cfg(&bin, RunnerMode::HiddenTui, 30),
            dir.path(),
            "sess-tui-1",
            &prompt,
            &hooks,
            &CancelToken::new(),
            || {},
        )
        .unwrap();
        assert_eq!(outcome, RunOutcome::Completed);
        assert!(staging.join("knowledge/People.md").is_file());
    }

    #[test]
    fn process_death_is_failure_with_detail() {
        let (dir, bin, hooks) = setup("fail");
        let outcome = run_session(
            &cfg(&bin, RunnerMode::HiddenTui, 30),
            dir.path(),
            "sess-fail",
            "prompt",
            &hooks,
            &CancelToken::new(),
            || {},
        )
        .unwrap();
        match outcome {
            RunOutcome::Failed(detail) => assert!(detail.contains("boom"), "{detail}"),
            other => panic!("expected failure, got {other:?}"),
        }
    }

    #[test]
    fn timeout_kills_and_reports() {
        let (dir, bin, hooks) = setup("hang");
        let outcome = run_session(
            &cfg(&bin, RunnerMode::HiddenTui, 1),
            dir.path(),
            "sess-hang",
            "prompt",
            &hooks,
            &CancelToken::new(),
            || {},
        )
        .unwrap();
        assert!(matches!(outcome, RunOutcome::TimedOut(_)));
    }

    #[test]
    fn notification_reports_blocked_then_cancel_works() {
        let (dir, bin, hooks) = setup("block");
        let cancel = CancelToken::new();
        let cancel2 = cancel.clone();
        let blocked = Arc::new(AtomicBool::new(false));
        let blocked2 = blocked.clone();
        let outcome = run_session(
            &cfg(&bin, RunnerMode::HiddenTui, 30),
            dir.path(),
            "sess-block",
            "prompt",
            &hooks,
            &cancel,
            move || {
                blocked2.store(true, Ordering::SeqCst);
                cancel2.cancel(); // the test cancels as soon as it blocks
            },
        )
        .unwrap();
        assert!(blocked.load(Ordering::SeqCst), "blocked callback should fire");
        assert_eq!(outcome, RunOutcome::Cancelled);
    }

    #[test]
    fn headless_completes_and_fails() {
        let (dir, bin, hooks) = setup("complete");
        let staging = dir.path().join(".ken/.staging/people");
        let prompt = format!("Extract. STAGING_DIR={}", staging.display());
        let outcome = run_session(
            &cfg(&bin, RunnerMode::Headless, 30),
            dir.path(),
            "sess-headless",
            &prompt,
            &hooks,
            &CancelToken::new(),
            || {},
        )
        .unwrap();
        assert_eq!(outcome, RunOutcome::Completed);
        assert!(staging.join("knowledge/People.md").is_file());

        std::fs::write(dir.path().join("behavior"), "headless-fail").unwrap();
        let outcome = run_session(
            &cfg(&bin, RunnerMode::Headless, 30),
            dir.path(),
            "sess-headless-2",
            "prompt",
            &hooks,
            &CancelToken::new(),
            || {},
        )
        .unwrap();
        assert!(matches!(outcome, RunOutcome::Failed(_)));
    }

    /// The v2.x CLI exits 0 and reports failure inside the trailing result
    /// event of a JSON array; an ingest must not be reported as Completed.
    #[test]
    fn headless_honors_error_in_array_result_event() {
        let (dir, bin, hooks) = setup("headless-array-error");
        let outcome = run_session(
            &cfg(&bin, RunnerMode::Headless, 30),
            dir.path(),
            "sess-headless-arr",
            "prompt",
            &hooks,
            &CancelToken::new(),
            || {},
        )
        .unwrap();
        match outcome {
            RunOutcome::Failed(msg) => assert!(msg.contains("error_max_turns"), "{msg}"),
            other => panic!("expected failure, got {other:?}"),
        }
    }

    /// A clean exit whose stdout has no terminal result event but does carry
    /// a final assistant message did its work — report Completed (an ingest is
    /// applied only on Completed), not Failed.
    #[test]
    fn headless_recovers_when_no_result_event_but_exit_zero() {
        let (dir, bin, hooks) = setup("headless-array-noresult");
        let outcome = run_session(
            &cfg(&bin, RunnerMode::Headless, 30),
            dir.path(),
            "sess-noresult",
            "prompt",
            &hooks,
            &CancelToken::new(),
            || {},
        )
        .unwrap();
        assert_eq!(outcome, RunOutcome::Completed);
    }

    /// Same shape but a non-zero exit is a genuine failure — recovery must not
    /// mask it.
    #[test]
    fn headless_fails_when_no_result_event_and_nonzero_exit() {
        let (dir, bin, hooks) = setup("headless-array-noresult-nonzero");
        let outcome = run_session(
            &cfg(&bin, RunnerMode::Headless, 30),
            dir.path(),
            "sess-noresult-nz",
            "prompt",
            &hooks,
            &CancelToken::new(),
            || {},
        )
        .unwrap();
        assert!(matches!(outcome, RunOutcome::Failed(_)), "{outcome:?}");
    }

    #[test]
    fn missing_binary_gives_guided_error() {
        let (dir, _bin, hooks) = setup("complete");
        let outcome = run_session(
            &cfg(Path::new("/nonexistent/claude"), RunnerMode::HiddenTui, 5),
            dir.path(),
            "sess-x",
            "prompt",
            &hooks,
            &CancelToken::new(),
            || {},
        )
        .unwrap();
        match outcome {
            RunOutcome::Failed(msg) => assert!(msg.contains("isn't installed")),
            other => panic!("expected failure, got {other:?}"),
        }
    }
}

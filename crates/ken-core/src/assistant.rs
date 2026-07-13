//! One-shot assistant: run a single headless Claude Code session and
//! return the text it produced. The shared primitive behind the daily
//! digest and ⌘K quick answers — same CLI contract as the ingest
//! runner's headless mode, but the caller gets the `result` string back
//! instead of only pass/fail.

use std::io::Read;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::runner::{self, CancelToken};
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum OneshotOutcome {
    /// The session finished; carries the `result` text from the CLI's
    /// output JSON.
    Completed(String),
    Cancelled,
    TimedOut,
    /// Process failed or produced unusable output; carries detail.
    Failed(String),
}

/// Run one headless session (`claude -p … --output-format json`) to
/// completion and return its result text. Synchronous — call from a
/// worker thread. Cancel or timeout kills the process.
pub fn oneshot(
    binary: &Path,
    project_root: &Path,
    prompt: &str,
    timeout: Duration,
    cancel: &CancelToken,
) -> Result<OneshotOutcome> {
    if !runner::is_executable(binary) {
        return Ok(OneshotOutcome::Failed(
            runner::MISSING_CLAUDE_HELP.to_string(),
        ));
    }
    let session_id = uuid::Uuid::new_v4().to_string();
    let mut child = std::process::Command::new(binary)
        .args([
            "-p",
            prompt,
            "--output-format",
            "json",
            "--permission-mode",
            "acceptEdits",
            "--session-id",
            &session_id,
        ])
        .current_dir(project_root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null())
        .spawn()
        .map_err(|e| Error::Other(format!("spawn {}: {e}", binary.display())))?;

    let stdout = child.stdout.take();
    let out_buf = Arc::new(Mutex::new(String::new()));
    let out_clone = out_buf.clone();
    let reader_thread = std::thread::spawn(move || {
        if let Some(mut s) = stdout {
            let mut text = String::new();
            let _ = s.read_to_string(&mut text);
            *out_clone.lock().unwrap() = text;
        }
    });

    let deadline = Instant::now() + timeout;
    loop {
        if cancel.is_cancelled() {
            let _ = child.kill();
            let _ = reader_thread.join();
            return Ok(OneshotOutcome::Cancelled);
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let _ = reader_thread.join();
                let output = out_buf.lock().unwrap().clone();
                let json =
                    serde_json::from_str::<serde_json::Value>(output.trim()).ok();
                let is_error = json
                    .as_ref()
                    .and_then(|v| v.get("is_error").and_then(|b| b.as_bool()))
                    .unwrap_or(!status.success());
                if status.success() && !is_error {
                    let result = json
                        .as_ref()
                        .and_then(|v| v.get("result").and_then(|r| r.as_str()))
                        .map(String::from);
                    return Ok(match result {
                        Some(text) => OneshotOutcome::Completed(text),
                        None => OneshotOutcome::Failed(
                            "the session finished but produced no result text"
                                .into(),
                        ),
                    });
                }
                return Ok(OneshotOutcome::Failed(format!(
                    "one-shot session failed (status {status:?}): {}",
                    truncate(&output, 2000)
                )));
            }
            Ok(None) => {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = reader_thread.join();
                    return Ok(OneshotOutcome::TimedOut);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Ok(OneshotOutcome::Failed(format!("wait failed: {e}"))),
        }
    }
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
mod tests {
    use super::*;
    use crate::runner::test_support::write_fake_claude;
    use std::path::PathBuf;

    fn setup(behavior: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let bin = write_fake_claude(dir.path(), behavior);
        (dir, bin)
    }

    #[test]
    fn oneshot_returns_result_text() {
        let (dir, bin) = setup("complete");
        let text = "The **cutover** moved to Sept 12.\nSOURCES: notes/a.md, People.md";
        std::fs::write(dir.path().join("oneshot_result"), text).unwrap();
        let outcome = oneshot(
            &bin,
            dir.path(),
            "write the digest",
            Duration::from_secs(30),
            &CancelToken::new(),
        )
        .unwrap();
        assert_eq!(outcome, OneshotOutcome::Completed(text.to_string()));
    }

    #[test]
    fn oneshot_default_result_without_file() {
        let (dir, bin) = setup("complete");
        let outcome = oneshot(
            &bin,
            dir.path(),
            "prompt",
            Duration::from_secs(30),
            &CancelToken::new(),
        )
        .unwrap();
        assert_eq!(outcome, OneshotOutcome::Completed("done".into()));
    }

    #[test]
    fn oneshot_failure_reports_detail() {
        let (dir, bin) = setup("headless-fail");
        let outcome = oneshot(
            &bin,
            dir.path(),
            "prompt",
            Duration::from_secs(30),
            &CancelToken::new(),
        )
        .unwrap();
        match outcome {
            OneshotOutcome::Failed(detail) => {
                assert!(detail.contains("simulated"), "{detail}")
            }
            other => panic!("expected failure, got {other:?}"),
        }
    }

    #[test]
    fn oneshot_cancel_kills_session() {
        let (dir, bin) = setup("hang");
        let cancel = CancelToken::new();
        let canceller = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            canceller.cancel();
        });
        let outcome = oneshot(
            &bin,
            dir.path(),
            "prompt",
            Duration::from_secs(30),
            &cancel,
        )
        .unwrap();
        assert_eq!(outcome, OneshotOutcome::Cancelled);
    }

    #[test]
    fn oneshot_timeout_kills_session() {
        let (dir, bin) = setup("hang");
        let outcome = oneshot(
            &bin,
            dir.path(),
            "prompt",
            Duration::from_secs(1),
            &CancelToken::new(),
        )
        .unwrap();
        assert_eq!(outcome, OneshotOutcome::TimedOut);
    }

    #[test]
    fn oneshot_missing_binary_gives_guided_error() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = oneshot(
            Path::new("/nonexistent/claude"),
            dir.path(),
            "prompt",
            Duration::from_secs(5),
            &CancelToken::new(),
        )
        .unwrap();
        match outcome {
            OneshotOutcome::Failed(msg) => assert!(msg.contains("isn't installed")),
            other => panic!("expected failure, got {other:?}"),
        }
    }
}

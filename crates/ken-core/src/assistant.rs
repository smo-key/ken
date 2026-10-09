//! One-shot assistant: run a single headless Claude Code session and
//! return the text it produced. The shared primitive behind the daily
//! digest and ⌘K quick answers — same CLI contract as the ingest
//! runner's headless mode, but the caller gets the `result` string back
//! instead of only pass/fail.

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

/// What `--output-format json` stdout says about a finished session.
#[derive(Debug, Clone, PartialEq)]
pub enum ParsedOutput {
    /// Terminal result event, `is_error: false` — carries the result text.
    Success(String),
    /// Terminal result event, `is_error: true` — carries actionable detail.
    Error(String),
    /// Not JSON, or JSON with no terminal result event — carries a message
    /// that quotes what was actually received.
    Unusable(String),
}

/// Extract the terminal result from the CLI's `--output-format json` stdout.
///
/// Two shapes are in the wild and both must work: recent CLIs (v2.x, e.g.
/// 2.1.207) print an ARRAY of events (`system` init, `assistant`,
/// `rate_limit_event`, …) terminated by a `{"type":"result",…}` event, while
/// older ones print a single object carrying `result`/`is_error` at the top
/// level. We look for the last `type == "result"` element rather than the last
/// array element, so trailing telemetry events can be added without breaking us.
pub fn parse_oneshot_output(stdout: &str) -> ParsedOutput {
    let text = stdout.trim();
    if text.is_empty() {
        return ParsedOutput::Unusable(
            "the session produced no output on stdout (it may have crashed before starting)".into(),
        );
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return ParsedOutput::Unusable(format!(
            "the session's output wasn't JSON — received: {}",
            truncate(text, 2000)
        ));
    };
    parse_output_value(&value, text)
}

/// The events of `--output-format stream-json --verbose` stdout, one JSON
/// object a line, in order. A line that is not JSON (a warning the CLI
/// printed) is skipped.
pub fn parse_stream_events(stdout: &str) -> Vec<serde_json::Value> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('{'))
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// [`parse_oneshot_output`] for stream-json stdout: the same terminal result,
/// read from the events as the array shape is.
pub fn parse_stream_output(stdout: &str) -> ParsedOutput {
    let events = parse_stream_events(stdout);
    if events.is_empty() {
        return parse_oneshot_output(stdout);
    }
    parse_output_value(&serde_json::Value::Array(events), stdout.trim())
}

/// The terminal result in parsed output, either shape; `text` is what was
/// received, quoted when it is unusable.
fn parse_output_value(value: &serde_json::Value, text: &str) -> ParsedOutput {
    let event = match value {
        serde_json::Value::Array(events) => {
            match events
                .iter()
                .rev()
                .find(|e| e.get("type").and_then(|t| t.as_str()) == Some("result"))
            {
                Some(ev) => Some(ev),
                // No terminal result wrapper. The session can still have done
                // its work and printed its answer as the last assistant
                // message (a result event lost to a crash after printing, or
                // an older/edge CLI shape). Recover that text as the result;
                // callers gate on exit status, so a non-zero exit still fails.
                None => match last_assistant_text(events) {
                    Some(t) => return ParsedOutput::Success(t),
                    None => None,
                },
            }
        }
        serde_json::Value::Object(_) => Some(value),
        _ => None,
    };
    let Some(event) = event else {
        return ParsedOutput::Unusable(format!(
            "the session's output had no result event — received: {}",
            truncate(text, 2000)
        ));
    };

    let is_error = event
        .get("is_error")
        .and_then(|b| b.as_bool())
        .unwrap_or(false);
    let result = event.get("result").and_then(|r| r.as_str());
    if is_error {
        let subtype = event
            .get("subtype")
            .and_then(|s| s.as_str())
            .unwrap_or("error");
        let detail = result.unwrap_or("no detail from the CLI");
        return ParsedOutput::Error(format!(
            "Claude Code reported an error ({subtype}): {}",
            truncate(detail, 2000)
        ));
    }
    match result {
        Some(text) => ParsedOutput::Success(text.to_string()),
        None => ParsedOutput::Unusable(format!(
            "the session's result event had no result text — received: {}",
            truncate(text, 2000)
        )),
    }
}

/// Concatenated `text` blocks of the last `assistant` event, if any. The
/// fallback result when a run printed its answer but no terminal `result`
/// wrapper (see parse_oneshot_output). An assistant message carrying only a
/// tool call (no text) yields None, so such output stays Unusable.
fn last_assistant_text(events: &[serde_json::Value]) -> Option<String> {
    let content = events
        .iter()
        .rev()
        .find(|e| e.get("type").and_then(|t| t.as_str()) == Some("assistant"))?
        .get("message")?
        .get("content")?
        .as_array()?;
    let text: String = content
        .iter()
        .filter(|c| c.get("type").and_then(|t| t.as_str()) == Some("text"))
        .filter_map(|c| c.get("text").and_then(|t| t.as_str()))
        .collect();
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
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
    run_oneshot(binary, project_root, prompt, timeout, cancel, &["--permission-mode".into(), "acceptEdits".into()])
}

/// A session that only reads its prompt and answers it: no tools at all,
/// and the `model` named (`opus`), else the CLI's default. For a judgement
/// over text the prompt already holds (`supersede`).
pub fn oneshot_answer(
    binary: &Path,
    project_root: &Path,
    prompt: &str,
    model: Option<&str>,
    timeout: Duration,
    cancel: &CancelToken,
) -> Result<OneshotOutcome> {
    let mut args: Vec<String> = vec!["--tools".into(), String::new()];
    if let Some(m) = model.filter(|m| !m.is_empty()) {
        args.push("--model".into());
        args.push(m.into());
    }
    run_oneshot(binary, project_root, prompt, timeout, cancel, &args)
}

/// [`oneshot`] that may also read `dirs`. Claude Code reads nothing outside
/// the folder it runs from unless told, and a workspace's repos can sit
/// anywhere (set-up keeps the workspace itself in Ken's app data).
pub fn oneshot_in(
    binary: &Path,
    project_root: &Path,
    dirs: &[std::path::PathBuf],
    prompt: &str,
    timeout: Duration,
    cancel: &CancelToken,
) -> Result<OneshotOutcome> {
    let mut args: Vec<String> = vec!["--permission-mode".into(), "acceptEdits".into()];
    for dir in dirs.iter().filter(|d| d.as_path() != project_root) {
        args.push("--add-dir".into());
        args.push(dir.to_string_lossy().into_owned());
    }
    run_oneshot(binary, project_root, prompt, timeout, cancel, &args)
}

/// The tools a [`look`] may use: reading and searching, nothing that writes
/// or runs.
pub const LOOK_TOOLS: &str = "Read,Grep,Glob,LS";

/// The prompt for a [`look`]: the question, the folders to search as
/// (name, id, root), what Ken's search ranked (`folder/path`, best first),
/// and how to cite across them.
pub fn look_prompt(query: &str, folders: &[(String, uuid::Uuid, std::path::PathBuf)], leads: &[String]) -> String {
    let mut p = format!(
        "Question: {query}\n\nKen's search index found nothing strong for this, which does not mean it isn't there. \
Look for the answer yourself with Grep, Glob and Read in these folders:\n"
    );
    for (name, id, root) in folders {
        p.push_str(&format!("- {name}: {} (cite its files as ken://{id}/<path>#L<line>)\n", root.display()));
    }
    if !leads.is_empty() {
        p.push_str("\nKen's search ranked these files, best first. One may hold the answer or sit next to it: read them first.\n");
        for lead in leads.iter().take(8) {
            p.push_str(&format!("- {lead}\n"));
        }
    }
    p.push_str(
        "\nOn what the system does, the code wins: a ticket, plan or doc says what was intended, not what was built. \
When the question is about how something works, open the code that does it and cite that file, with the doc beside it \
if it helps. Code often sits deeper than a doc's path says (under a pack, `src/` or `backend/`): find a named \
file with Glob `**/<name>` across every folder before deciding it is elsewhere. Glob and Grep search only the \
folder you start in unless given a path, so pass each folder above as the path.\n\
\nAnswer in two to four sentences from what you read. Cite each file you used inline as its ken:// address. \
If after looking it genuinely is not there, say so plainly and say where you looked. End with a final line \
`SOURCES: ken://…, ken://…` listing the addresses you cited (omit the line if none).\n",
    );
    p
}

/// A read-only session that searches `dirs` itself: the fallback when Ken's
/// index found nothing, so "not there" is only said after looking. It may
/// read and search every folder given and write nothing.
pub fn look(
    binary: &Path,
    project_root: &Path,
    dirs: &[std::path::PathBuf],
    prompt: &str,
    timeout: Duration,
    cancel: &CancelToken,
) -> Result<OneshotOutcome> {
    let mut args: Vec<String> = vec![
        "--permission-mode".into(),
        "default".into(),
        "--allowedTools".into(),
        LOOK_TOOLS.into(),
        "--disallowedTools".into(),
        "Edit,MultiEdit,Write,NotebookEdit,Bash".into(),
    ];
    for dir in dirs.iter().filter(|d| d.as_path() != project_root) {
        args.push("--add-dir".into());
        args.push(dir.to_string_lossy().into_owned());
    }
    run_oneshot(binary, project_root, prompt, timeout, cancel, &args)
}

/// One chat turn run headless, as Ken's chat is set up: its guide, Ken's MCP
/// server (`mcp_config`) with the tools the chat may use without asking,
/// the read-only file tools over `dirs`, and nothing that writes or runs.
/// For measuring the chat's answers; the app's chat streams instead.
pub fn chat_oneshot(
    binary: &Path,
    project_root: &Path,
    dirs: &[std::path::PathBuf],
    mcp_config: &Path,
    prompt: &str,
    timeout: Duration,
    cancel: &CancelToken,
) -> Result<OneshotOutcome> {
    chat_oneshot_with(binary, project_root, dirs, mcp_config, prompt, timeout, cancel, true)
}

/// [`chat_oneshot`]; with `look` false, Ken's MCP tools only (no file
/// tools, no folders added), to measure what Ken's own search answers.
#[allow(clippy::too_many_arguments)]
pub fn chat_oneshot_with(
    binary: &Path,
    project_root: &Path,
    dirs: &[std::path::PathBuf],
    mcp_config: &Path,
    prompt: &str,
    timeout: Duration,
    cancel: &CancelToken,
    look: bool,
) -> Result<OneshotOutcome> {
    let args = chat_args(project_root, dirs, mcp_config, look);
    run_oneshot(binary, project_root, prompt, timeout, cancel, &args)
}

/// [`chat_oneshot_with`] over `--output-format stream-json --verbose`, so
/// every event comes back with the outcome: each tool call Claude made and
/// what it returned, in order, for measuring how an answer was found. On a
/// timeout the events are those printed before it.
#[allow(clippy::too_many_arguments)]
pub fn chat_oneshot_traced(
    binary: &Path,
    project_root: &Path,
    dirs: &[std::path::PathBuf],
    mcp_config: &Path,
    prompt: &str,
    timeout: Duration,
    cancel: &CancelToken,
    look: bool,
) -> Result<(OneshotOutcome, Vec<serde_json::Value>)> {
    let args = chat_args(project_root, dirs, mcp_config, look);
    run_oneshot_events(binary, project_root, prompt, timeout, cancel, &args, true)
}

/// The access a headless chat turn gets: its guide, Ken's MCP server and the
/// tools it may use without asking, and with `look` the read-only file tools
/// over `dirs`.
fn chat_args(project_root: &Path, dirs: &[std::path::PathBuf], mcp_config: &Path, look: bool) -> Vec<String> {
    let mut allowed: Vec<String> = crate::chat::KEN_MCP_ALLOWED
        .iter()
        .filter(|t| **t != "open_in_ken")
        .map(|t| format!("mcp__ken__{t}"))
        .collect();
    if look {
        allowed.push(LOOK_TOOLS.to_string());
    }
    let disallowed = if look { "Edit,MultiEdit,Write,NotebookEdit,Bash".to_string() } else { format!("Edit,MultiEdit,Write,NotebookEdit,Bash,{LOOK_TOOLS}") };
    let dirs: &[std::path::PathBuf] = if look { dirs } else { &[] };
    // Without `look` the file tools are refused, and the guide's rule 4 still
    // sends Claude to them: 45 refused calls over 32 questions, measured
    // 2026-10-07. Say so up front.
    let mut guide = crate::chat::KEN_GUIDE.replace('\n', " ");
    if !look {
        guide.push_str(" This turn has no file tools: Grep, Glob and Read are off. Search and read with Ken's tools only.");
    }
    let mut args: Vec<String> = vec![
        "--permission-mode".into(),
        "default".into(),
        "--append-system-prompt".into(),
        guide,
        "--mcp-config".into(),
        mcp_config.to_string_lossy().into_owned(),
        "--allowedTools".into(),
        allowed.join(","),
        "--disallowedTools".into(),
        disallowed,
    ];
    for dir in dirs.iter().filter(|d| d.as_path() != project_root) {
        args.push("--add-dir".into());
        args.push(dir.to_string_lossy().into_owned());
    }
    args
}

fn run_oneshot(
    binary: &Path,
    project_root: &Path,
    prompt: &str,
    timeout: Duration,
    cancel: &CancelToken,
    access: &[String],
) -> Result<OneshotOutcome> {
    run_oneshot_events(binary, project_root, prompt, timeout, cancel, access, false).map(|(outcome, _)| outcome)
}

/// [`run_oneshot`], with the session's events when `stream` (stream-json
/// output); without it the events are empty.
fn run_oneshot_events(
    binary: &Path,
    project_root: &Path,
    prompt: &str,
    timeout: Duration,
    cancel: &CancelToken,
    access: &[String],
    stream: bool,
) -> Result<(OneshotOutcome, Vec<serde_json::Value>)> {
    if !runner::is_executable(binary) {
        return Ok((
            OneshotOutcome::Failed(runner::MISSING_CLAUDE_HELP.to_string()),
            Vec::new(),
        ));
    }
    let session_id = uuid::Uuid::new_v4().to_string();
    // The prompt goes in on stdin: on Windows an argument with a line break
    // cannot reach the `.cmd` launcher at all (see `proc::spawn_with_input`).
    let mut cmd = std::process::Command::new(binary);
    if stream {
        // stream-json in print mode needs --verbose.
        cmd.args(["-p", "--output-format", "stream-json", "--verbose"]);
    } else {
        cmd.args(["-p", "--output-format", "json"]);
    }
    cmd.args(access)
        .args(["--session-id", &session_id])
        .current_dir(project_root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = crate::proc::spawn_with_input(&mut cmd, prompt)
        .map_err(|e| Error::Other(format!("spawn {}: {e}", binary.display())))?;

    let events = |output: &str| if stream { parse_stream_events(output) } else { Vec::new() };
    let outcome = match drive_child(child, timeout, Duration::from_millis(100), cancel) {
        DriveResult::Exited(status, output, stderr) => {
            let parsed = if stream { parse_stream_output(&output) } else { parse_oneshot_output(&output) };
            let outcome = match parsed {
                ParsedOutput::Success(text) if status.success() => OneshotOutcome::Completed(text),
                // A good result event but a non-zero exit can't happen in
                // practice; treat status as authoritative.
                ParsedOutput::Success(_) => OneshotOutcome::Failed(with_stderr(
                    format!("the session exited with status {status:?}"),
                    &stderr,
                )),
                ParsedOutput::Error(msg) | ParsedOutput::Unusable(msg) => {
                    OneshotOutcome::Failed(with_stderr(msg, &stderr))
                }
            };
            (outcome, events(&output))
        }
        DriveResult::Cancelled => (OneshotOutcome::Cancelled, Vec::new()),
        DriveResult::TimedOut(output) => (OneshotOutcome::TimedOut, events(&output)),
        DriveResult::WaitFailed(e) => (OneshotOutcome::Failed(format!("wait failed: {e}")), Vec::new()),
    };
    Ok(outcome)
}

/// The terminal state of a driven child. Callers map it onto their own
/// outcome type (the ingest runner keeps a timeout's output tail; the one-shot
/// assistant discards it).
pub(crate) enum DriveResult {
    /// Exited on its own: exit status, captured stdout, captured stderr.
    Exited(std::process::ExitStatus, String, String),
    Cancelled,
    /// Deadline hit and the child was killed; carries stdout captured so far.
    TimedOut(String),
    /// `try_wait` itself errored; carries the message.
    WaitFailed(String),
}

/// Drive a spawned child to completion, honouring cancellation and a deadline.
/// The single copy of the loop the headless ingest runner and the one-shot
/// assistant both need: drain BOTH pipes concurrently (either filling its
/// ~64KB buffer would deadlock the child), poll `try_wait`, and kill+join on
/// cancel or timeout. `poll` is the idle sleep between wait polls.
pub(crate) fn drive_child(
    mut child: std::process::Child,
    timeout: Duration,
    poll: Duration,
    cancel: &CancelToken,
) -> DriveResult {
    let (out_buf, out_thread) = drain_pipe(child.stdout.take());
    let (err_buf, err_thread) = drain_pipe(child.stderr.take());
    let join = move || {
        let _ = out_thread.join();
        let _ = err_thread.join();
    };

    let deadline = Instant::now() + timeout;
    loop {
        if cancel.is_cancelled() {
            crate::proc::kill_tree(&mut child);
            join();
            return DriveResult::Cancelled;
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                join();
                let stdout = out_buf.lock().unwrap().clone();
                let stderr = err_buf.lock().unwrap().clone();
                return DriveResult::Exited(status, stdout, stderr);
            }
            Ok(None) => {
                if Instant::now() > deadline {
                    crate::proc::kill_tree(&mut child);
                    join();
                    return DriveResult::TimedOut(out_buf.lock().unwrap().clone());
                }
                std::thread::sleep(poll);
            }
            Err(e) => return DriveResult::WaitFailed(e.to_string()),
        }
    }
}

/// Read a child pipe to EOF on its own thread. Both stdout and stderr must be
/// drained concurrently: a child that fills either pipe's ~64KB buffer blocks
/// forever if nobody is reading the other end.
pub(crate) fn drain_pipe<R: std::io::Read + Send + 'static>(
    pipe: Option<R>,
) -> (Arc<Mutex<String>>, std::thread::JoinHandle<()>) {
    let buf = Arc::new(Mutex::new(String::new()));
    let sink = buf.clone();
    let handle = std::thread::spawn(move || {
        if let Some(mut p) = pipe {
            let mut text = String::new();
            let _ = p.read_to_string(&mut text);
            *sink.lock().unwrap() = text;
        }
    });
    (buf, handle)
}

pub(crate) fn with_stderr(detail: String, stderr: &str) -> String {
    let stderr = stderr.trim();
    if stderr.is_empty() {
        detail
    } else {
        format!("{detail}\n\nstderr: {}", truncate(stderr, 2000))
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
    #[cfg_attr(windows, ignore = "runs the bash fake CLI; covered on macOS/Linux")]
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
    #[cfg_attr(windows, ignore = "runs the bash fake CLI; covered on macOS/Linux")]
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
    #[cfg_attr(windows, ignore = "runs the bash fake CLI; covered on macOS/Linux")]
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
    #[cfg_attr(windows, ignore = "runs the bash fake CLI; covered on macOS/Linux")]
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
    #[cfg_attr(windows, ignore = "runs the bash fake CLI; covered on macOS/Linux")]
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

    /// Real stdout captured from Claude Code v2.1.207 with the flags this
    /// module passes: an array of events, answer in the trailing result event.
    const CLI_ARRAY: &str = r#"[
      {"type":"system","subtype":"init","cwd":"/tmp/p","session_id":"abc","tools":["Read"],"model":"claude-opus-4-8"},
      {"type":"assistant","message":{"id":"msg_1","role":"assistant","content":[{"type":"text","text":"OK"}]},"session_id":"abc"},
      {"type":"rate_limit_event","rate_limit":{"status":"allowed"}},
      {"type":"result","subtype":"success","is_error":false,"result":"OK","duration_ms":2198,"session_id":"abc","total_cost_usd":0.1}
    ]"#;

    #[test]
    fn parses_event_array_result_event() {
        assert_eq!(
            parse_oneshot_output(CLI_ARRAY),
            ParsedOutput::Success("OK".into())
        );
    }

    #[test]
    fn parses_legacy_single_object() {
        assert_eq!(
            parse_oneshot_output(r#"{"is_error": false, "result": "done"}"#),
            ParsedOutput::Success("done".into())
        );
    }

    /// stream-json stdout: one event a line, tool calls and their results
    /// between the init and the result.
    const CLI_STREAM: &str = concat!(
        r#"{"type":"system","subtype":"init","session_id":"abc","tools":["Read"]}"#,
        "\n",
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"mcp__ken__route_query","input":{"query":"who decides"}}]}}"#,
        "\n",
        "a warning the CLI printed\n",
        r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":[{"type":"text","text":"1. doc people/README.md"}]}]}}"#,
        "\n",
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Chris decides."}]}}"#,
        "\n",
        r#"{"type":"result","subtype":"success","is_error":false,"result":"Chris decides.","duration_ms":900,"num_turns":2}"#,
        "\n"
    );

    #[test]
    fn stream_events_keep_every_json_line_in_order() {
        let events = parse_stream_events(CLI_STREAM);
        let types: Vec<&str> = events.iter().filter_map(|e| e.get("type").and_then(|t| t.as_str())).collect();
        assert_eq!(types, ["system", "assistant", "user", "assistant", "result"]);
        assert_eq!(events[1]["message"]["content"][0]["input"]["query"], "who decides");
    }

    #[test]
    fn stream_output_reads_the_result_event() {
        assert_eq!(parse_stream_output(CLI_STREAM), ParsedOutput::Success("Chris decides.".into()));
        // Cut off before the result: the last assistant text, as for the array.
        let cut: String = CLI_STREAM.lines().take(5).map(|l| format!("{l}\n")).collect();
        assert_eq!(parse_stream_output(&cut), ParsedOutput::Success("Chris decides.".into()));
        assert!(matches!(parse_stream_output("not json at all"), ParsedOutput::Unusable(_)));
    }

    #[test]
    fn error_result_event_in_array_maps_to_error() {
        let out = r#"[
          {"type":"system","subtype":"init","session_id":"abc"},
          {"type":"result","subtype":"error_max_turns","is_error":true,"result":"hit the turn limit","session_id":"abc"}
        ]"#;
        match parse_oneshot_output(out) {
            ParsedOutput::Error(msg) => {
                assert!(msg.contains("error_max_turns"), "{msg}");
                assert!(msg.contains("hit the turn limit"), "{msg}");
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn legacy_error_object_maps_to_error() {
        match parse_oneshot_output(r#"{"is_error": true, "result": "simulated"}"#) {
            ParsedOutput::Error(msg) => assert!(msg.contains("simulated"), "{msg}"),
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn last_result_event_wins() {
        let out = r#"[
          {"type":"result","subtype":"success","is_error":false,"result":"first"},
          {"type":"assistant","message":{"content":[]}},
          {"type":"result","subtype":"success","is_error":false,"result":"second"}
        ]"#;
        assert_eq!(
            parse_oneshot_output(out),
            ParsedOutput::Success("second".into())
        );
    }

    #[test]
    fn array_without_result_event_is_unusable_and_shows_output() {
        let out = r#"[{"type":"system","subtype":"init","session_id":"abc"}]"#;
        match parse_oneshot_output(out) {
            ParsedOutput::Unusable(msg) => {
                assert!(msg.contains("no result event"), "{msg}");
                assert!(msg.contains("\"type\":\"system\""), "{msg}");
            }
            other => panic!("expected unusable, got {other:?}"),
        }
    }

    #[test]
    fn array_without_result_recovers_last_assistant_text() {
        // No terminal result wrapper, but the CLI printed its answer as the
        // last assistant message: recover it rather than fail the run.
        let out = r#"[
          {"type":"system","subtype":"init","session_id":"abc"},
          {"type":"assistant","message":{"content":[{"type":"text","text":"recovered answer"}]}}
        ]"#;
        assert_eq!(
            parse_oneshot_output(out),
            ParsedOutput::Success("recovered answer".into())
        );
    }

    #[test]
    fn array_result_event_still_wins_over_assistant_text() {
        // With a real result event present, the assistant-text fallback must
        // not change the existing behavior.
        assert_eq!(
            parse_oneshot_output(CLI_ARRAY),
            ParsedOutput::Success("OK".into())
        );
    }

    #[test]
    fn array_with_only_tool_use_assistant_is_unusable() {
        // An assistant message with no text block (only a tool call) is not a
        // usable result — still Unusable, so the caller fails it.
        let out = r#"[
          {"type":"system","subtype":"init"},
          {"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{}}]}}
        ]"#;
        match parse_oneshot_output(out) {
            ParsedOutput::Unusable(msg) => assert!(msg.contains("no result event"), "{msg}"),
            other => panic!("expected unusable, got {other:?}"),
        }
    }

    #[test]
    #[cfg_attr(windows, ignore = "runs the bash fake CLI; covered on macOS/Linux")]
    fn oneshot_recovers_assistant_text_when_no_result_event() {
        // Exit 0 + assistant text, no result wrapper → Completed with the text.
        let (dir, bin) = setup("headless-array-noresult");
        let outcome = oneshot(
            &bin,
            dir.path(),
            "prompt",
            Duration::from_secs(30),
            &CancelToken::new(),
        )
        .unwrap();
        assert_eq!(
            outcome,
            OneshotOutcome::Completed("recovered answer".into())
        );
    }

    #[test]
    #[cfg_attr(windows, ignore = "runs the bash fake CLI; covered on macOS/Linux")]
    fn oneshot_fails_when_no_result_and_nonzero_exit() {
        // The recovery must not mask a real failure: non-zero exit → Failed.
        let (dir, bin) = setup("headless-array-noresult-nonzero");
        let outcome = oneshot(
            &bin,
            dir.path(),
            "prompt",
            Duration::from_secs(30),
            &CancelToken::new(),
        )
        .unwrap();
        assert!(matches!(outcome, OneshotOutcome::Failed(_)), "{outcome:?}");
    }

    #[test]
    fn non_json_garbage_is_unusable_and_shows_output() {
        match parse_oneshot_output("Error: ENOENT node_modules missing\n") {
            ParsedOutput::Unusable(msg) => {
                assert!(msg.contains("wasn't JSON"), "{msg}");
                assert!(msg.contains("ENOENT"), "{msg}");
            }
            other => panic!("expected unusable, got {other:?}"),
        }
    }

    #[test]
    fn empty_stdout_is_unusable() {
        match parse_oneshot_output("   \n") {
            ParsedOutput::Unusable(msg) => assert!(msg.contains("no output"), "{msg}"),
            other => panic!("expected unusable, got {other:?}"),
        }
    }

    #[test]
    fn unusable_snippet_is_truncated() {
        let junk = "x".repeat(5000);
        match parse_oneshot_output(&junk) {
            ParsedOutput::Unusable(msg) => assert!(msg.len() < 3000, "len {}", msg.len()),
            other => panic!("expected unusable, got {other:?}"),
        }
    }

    #[test]
    #[cfg_attr(windows, ignore = "runs the bash fake CLI; covered on macOS/Linux")]
    fn oneshot_reads_array_output_end_to_end() {
        let (dir, bin) = setup("headless-array");
        let outcome = oneshot(
            &bin,
            dir.path(),
            "prompt",
            Duration::from_secs(30),
            &CancelToken::new(),
        )
        .unwrap();
        assert_eq!(outcome, OneshotOutcome::Completed("OK".into()));
    }

    /// A child that floods stderr must not deadlock on the pipe buffer:
    /// without a stderr drain thread this test only ends at the timeout.
    #[test]
    #[cfg_attr(windows, ignore = "runs the bash fake CLI; covered on macOS/Linux")]
    fn oneshot_survives_stderr_flood() {
        let (dir, bin) = setup("stderr-flood");
        let outcome = oneshot(
            &bin,
            dir.path(),
            "prompt",
            Duration::from_secs(30),
            &CancelToken::new(),
        )
        .unwrap();
        assert_eq!(outcome, OneshotOutcome::Completed("OK".into()));
    }

    #[test]
    #[cfg_attr(windows, ignore = "runs the bash fake CLI; covered on macOS/Linux")]
    fn oneshot_failure_includes_stderr() {
        let (dir, bin) = setup("headless-stderr-fail");
        let outcome = oneshot(
            &bin,
            dir.path(),
            "prompt",
            Duration::from_secs(30),
            &CancelToken::new(),
        )
        .unwrap();
        match outcome {
            OneshotOutcome::Failed(msg) => assert!(msg.contains("credit balance too low"), "{msg}"),
            other => panic!("expected failure, got {other:?}"),
        }
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

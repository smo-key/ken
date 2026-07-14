//! Chat sessions: the conversation engine (stream-json over the CLI's print
//! mode) and the terminal attach (PTY running the real TUI). One session id
//! serves both modes; only one process per session runs at a time.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::Value;

use crate::{Error, Result};

/// Most conversation processes kept alive at once (LRU beyond this).
const MAX_LIVE_CONVERSATIONS: usize = 3;

#[derive(Debug, Clone, PartialEq)]
pub enum ChatUpdate {
    /// A transcript entry to persist/render. role: assistant | activity.
    Message { chat_id: String, role: String, content: String },
    /// working | done | error
    Status { chat_id: String, status: String, detail: Option<String> },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParsedEvent {
    Init,
    AssistantText(String),
    Activity(String),
    TurnResult { is_error: bool },
    Other,
}

/// Parse one stream-json stdout line. Tolerant: unknown shapes → Other.
pub fn parse_event(line: &str) -> ParsedEvent {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return ParsedEvent::Other;
    };
    match v.get("type").and_then(Value::as_str) {
        Some("system") => ParsedEvent::Init,
        Some("result") => ParsedEvent::TurnResult {
            is_error: v.get("is_error").and_then(Value::as_bool).unwrap_or(false),
        },
        Some("assistant") => {
            let blocks = v
                .pointer("/message/content")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            // One event usually carries one block; prefer text, else tool.
            for b in &blocks {
                match b.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(t) = b.get("text").and_then(Value::as_str) {
                            if !t.trim().is_empty() {
                                return ParsedEvent::AssistantText(t.to_string());
                            }
                        }
                    }
                    Some("tool_use") => {
                        return ParsedEvent::Activity(summarize_tool(b));
                    }
                    _ => {}
                }
            }
            ParsedEvent::Other
        }
        _ => ParsedEvent::Other,
    }
}

/// "Read notes/meeting.md" — a human-readable one-liner for a tool_use block.
fn summarize_tool(block: &Value) -> String {
    let name = block.get("name").and_then(Value::as_str).unwrap_or("Tool");
    let input = block.get("input");
    let arg = input.and_then(|i| {
        ["file_path", "path", "pattern", "command", "query", "url", "notebook_path"]
            .iter()
            .find_map(|k| i.get(k).and_then(Value::as_str))
    });
    match arg {
        Some(a) => {
            let a = if a.len() > 80 { &a[..80] } else { a };
            format!("{name} {a}")
        }
        None => name.to_string(),
    }
}

/// The stable tier aliases Claude Code resolves to the latest model of each
/// tier — `claude --model <alias>` documents `haiku`/`sonnet`/`opus`/`fable`.
/// Passing the alias (never a pinned `claude-*-5` id) is deliberate: the tier
/// auto-resolves to the newest model, so this list never needs maintenance as
/// new versions ship. `fable` is a first-class alias on the installed CLI, so
/// no version mapping is needed for it either.
pub const MODEL_ALIASES: [&str; 4] = ["haiku", "sonnet", "opus", "fable"];

/// Validate a user-chosen model against the stable aliases. Returns the
/// canonical alias to pass as `--model`, or None to fall back to the CLI's own
/// default — we never forward an unrecognized string (a pinned id, a typo) to
/// the CLI.
pub fn valid_model_alias(s: &str) -> Option<&'static str> {
    let s = s.trim().to_ascii_lowercase();
    MODEL_ALIASES.iter().copied().find(|a| *a == s)
}

/// Cap on how many open-file paths we list, so a user with dozens of tabs open
/// can't bloat every prompt with a giant file list.
const MAX_CONTEXT_FILES: usize = 20;

/// Build the weak-hint context preamble from the files the user has open in
/// Ken. Returns None when nothing is open (the message is then sent verbatim).
/// The wording deliberately frames the list as "just what's on screen, not
/// necessarily relevant" — the model must treat it as a hint, not a directive.
pub fn build_context_preamble(focused: Option<&str>, open: &[String]) -> Option<String> {
    if open.is_empty() {
        return None;
    }
    let shown = open.len().min(MAX_CONTEXT_FILES);
    let mut lines = String::new();
    for path in &open[..shown] {
        lines.push_str(&format!("\n- {path}"));
    }
    if open.len() > shown {
        lines.push_str(&format!("\n- … and {} more", open.len() - shown));
    }
    let focus = match focused {
        Some(f) if !f.trim().is_empty() => format!("\nCurrently focused: {f}"),
        _ => String::new(),
    };
    Some(format!(
        "[Context — files the user currently has open in Ken. These are just \
         what's on their screen, NOT necessarily the most relevant files to \
         your question:{lines}{focus}]"
    ))
}

/// Ensure Claude Code treats `project_root` as a trusted folder before we spawn
/// it. On a first interactive run in an unseen folder the CLI shows a blocking
/// "Do you trust the files in this folder?" onboarding dialog (it records the
/// answer as `hasTrustDialogAccepted` under `projects[<abs path>]` in
/// `~/.claude.json`); in Ken's PTY chat that dialog wedges the session. Because
/// the folder is one the user already chose as a Ken project, pre-accepting the
/// trust is honest consent — and it is scoped to exactly this project's path(s)
/// so it never affects the user's other Claude usage. Best-effort: any IO/parse
/// failure is swallowed so a chat still spawns (it just may hit the prompt).
pub fn ensure_folder_trusted(project_root: &Path) {
    let Some(cfg_path) = claude_config_path() else { return };

    // The CLI keys the map by the process cwd. Register both the path we pass
    // and its canonical (symlink-resolved) form, so we match whichever the
    // spawned process ends up reporting as its cwd.
    let mut keys = vec![project_root.to_string_lossy().into_owned()];
    if let Ok(canon) = std::fs::canonicalize(project_root) {
        let canon = canon.to_string_lossy().into_owned();
        if !keys.contains(&canon) {
            keys.push(canon);
        }
    }

    let existing = std::fs::read(&cfg_path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .unwrap_or(Value::Null);
    let updated = apply_folder_trust(existing, &keys);
    if let Ok(bytes) = serde_json::to_vec_pretty(&updated) {
        let _ = std::fs::write(&cfg_path, bytes);
    }
}

/// Locate Claude Code's `.claude.json`. Honors `CLAUDE_CONFIG_DIR` (which the
/// CLI itself respects) so a custom config location — and test isolation — work
/// the same way the CLI sees them; otherwise `~/.claude.json`.
fn claude_config_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return Some(PathBuf::from(dir).join(".claude.json"));
    }
    dirs::home_dir().map(|h| h.join(".claude.json"))
}

/// Set the trust/onboarding flags for exactly `path_keys` in a parsed
/// `~/.claude.json` value, creating the `projects` map and entries as needed
/// and leaving every other key (and every other project) untouched. Pure so it
/// is unit-tested without a real home directory.
fn apply_folder_trust(mut cfg: Value, path_keys: &[String]) -> Value {
    if !cfg.is_object() {
        cfg = Value::Object(serde_json::Map::new());
    }
    let root = cfg.as_object_mut().unwrap();
    let projects = root
        .entry("projects")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !projects.is_object() {
        *projects = Value::Object(serde_json::Map::new());
    }
    let projects = projects.as_object_mut().unwrap();
    for key in path_keys {
        let entry = projects
            .entry(key.clone())
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        if let Some(obj) = entry.as_object_mut() {
            obj.insert("hasTrustDialogAccepted".into(), Value::Bool(true));
            obj.insert("hasCompletedProjectOnboarding".into(), Value::Bool(true));
        }
    }
    cfg
}

struct Conversation {
    child: Child,
    /// Behind its own lock so a turn's (potentially blocking) stdin write can
    /// happen off the `live` map lock: we clone this handle out under a short
    /// map lock, release it, then write. Otherwise a busy turn that stops
    /// draining its stdin pipe would wedge the whole engine.
    stdin: Arc<Mutex<ChildStdin>>,
    started: Instant,
}

/// Manages conversation-mode processes for a project's chats.
pub struct ChatEngine {
    binary: PathBuf,
    project_root: PathBuf,
    live: Arc<Mutex<HashMap<String, Conversation>>>,
    on_update: Arc<dyn Fn(ChatUpdate) + Send + Sync>,
}

impl ChatEngine {
    pub fn new(
        binary: PathBuf,
        project_root: PathBuf,
        on_update: impl Fn(ChatUpdate) + Send + Sync + 'static,
    ) -> ChatEngine {
        ChatEngine {
            binary,
            project_root,
            live: Arc::new(Mutex::new(HashMap::new())),
            on_update: Arc::new(on_update),
        }
    }

    pub fn is_live(&self, chat_id: &str) -> bool {
        self.live.lock().unwrap().contains_key(chat_id)
    }

    /// Send one user turn. `resume` = the session already exists (any prior
    /// message), so a fresh process must `--resume` instead of `--session-id`.
    pub fn send(&self, chat_id: &str, text: &str, resume: bool, model: Option<&str>) -> Result<()> {
        self.ensure_process(chat_id, resume, model)?;
        let payload = serde_json::json!({
            "type": "user",
            "message": { "role": "user", "content": [{ "type": "text", "text": text }] }
        });
        // Take the stdin handle under a short map lock, then drop the map lock
        // before writing: the write can block for as long as the turn keeps
        // the pipe full (e.g. Claude busy searching/running tools), and holding
        // `live` across it would freeze every other engine call — is_live,
        // stop, other chats' sends, and the death pump.
        let stdin = {
            let live = self.live.lock().unwrap();
            live.get(chat_id)
                .ok_or_else(|| Error::Other("chat process vanished".into()))?
                .stdin
                .clone()
        };
        {
            let mut stdin = stdin.lock().unwrap();
            writeln!(stdin, "{payload}")
                .and_then(|_| stdin.flush())
                .map_err(|e| Error::Other(format!("chat send failed: {e}")))?;
        }
        (self.on_update)(ChatUpdate::Status {
            chat_id: chat_id.to_string(),
            status: "working".into(),
            detail: None,
        });
        Ok(())
    }

    /// Stop a chat's conversation process (mode switch, archive, shutdown).
    pub fn stop(&self, chat_id: &str) {
        if let Some(mut conv) = self.live.lock().unwrap().remove(chat_id) {
            let _ = conv.child.kill();
            let _ = conv.child.wait();
        }
    }

    pub fn stop_all(&self) {
        let ids: Vec<String> = self.live.lock().unwrap().keys().cloned().collect();
        for id in ids {
            self.stop(&id);
        }
    }

    fn ensure_process(&self, chat_id: &str, resume: bool, model: Option<&str>) -> Result<()> {
        let mut live = self.live.lock().unwrap();
        if let Some(conv) = live.get_mut(chat_id) {
            match conv.child.try_wait() {
                Ok(None) => return Ok(()), // alive
                _ => {
                    live.remove(chat_id);
                }
            }
        }
        // LRU cap.
        if live.len() >= MAX_LIVE_CONVERSATIONS {
            if let Some(oldest) = live
                .iter()
                .min_by_key(|(_, c)| c.started)
                .map(|(id, _)| id.clone())
            {
                if let Some(mut conv) = live.remove(&oldest) {
                    let _ = conv.child.kill();
                    let _ = conv.child.wait();
                }
            }
        }

        // Pre-accept folder trust so a first run in a fresh project doesn't hit
        // the blocking onboarding gate (scoped to this project's path only).
        ensure_folder_trusted(&self.project_root);

        let mut cmd = Command::new(&self.binary);
        cmd.args([
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--permission-mode",
            "acceptEdits",
        ]);
        // Only forward a validated stable alias; anything else falls back to the
        // CLI's own default model.
        if let Some(alias) = model.and_then(valid_model_alias) {
            cmd.args(["--model", alias]);
        }
        if resume {
            cmd.args(["--resume", chat_id]);
        } else {
            cmd.args(["--session-id", chat_id]);
        }
        cmd.current_dir(&self.project_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| Error::Other(format!("spawn {}: {e}", self.binary.display())))?;
        let stdin = child.stdin.take().ok_or_else(|| Error::Other("no stdin".into()))?;
        let stdout = child.stdout.take().ok_or_else(|| Error::Other("no stdout".into()))?;
        let stderr = child.stderr.take();

        // Event pump.
        let on_update = self.on_update.clone();
        let live_map = self.live.clone();
        let id = chat_id.to_string();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            let mut saw_result = false;
            for line in reader.lines().map_while(|l| l.ok()) {
                match parse_event(&line) {
                    ParsedEvent::AssistantText(text) => {
                        saw_result = false;
                        on_update(ChatUpdate::Message {
                            chat_id: id.clone(),
                            role: "assistant".into(),
                            content: text,
                        });
                    }
                    ParsedEvent::Activity(summary) => {
                        on_update(ChatUpdate::Message {
                            chat_id: id.clone(),
                            role: "activity".into(),
                            content: summary,
                        });
                    }
                    ParsedEvent::TurnResult { is_error } => {
                        saw_result = true;
                        on_update(ChatUpdate::Status {
                            chat_id: id.clone(),
                            status: if is_error { "error".into() } else { "done".into() },
                            detail: None,
                        });
                    }
                    ParsedEvent::Init | ParsedEvent::Other => {}
                }
            }
            // Stdout closed: process ended. Mid-turn death is an error the
            // user should see; a clean end after a result is unremarkable.
            let was_tracked = live_map.lock().unwrap().remove(&id).is_some();
            if was_tracked && !saw_result {
                let tail = stderr
                    .map(|mut s| {
                        let mut buf = String::new();
                        let _ = s.read_to_string(&mut buf);
                        buf.lines().rev().take(6).collect::<Vec<_>>().into_iter().rev()
                            .collect::<Vec<_>>().join("\n")
                    })
                    .unwrap_or_default();
                on_update(ChatUpdate::Status {
                    chat_id: id.clone(),
                    status: "error".into(),
                    detail: Some(if tail.is_empty() {
                        "The session ended unexpectedly. Your next message will resume it.".into()
                    } else {
                        format!("The session ended unexpectedly:\n{tail}")
                    }),
                });
            }
        });

        live.insert(
            chat_id.to_string(),
            Conversation {
                child,
                stdin: Arc::new(Mutex::new(stdin)),
                started: Instant::now(),
            },
        );
        Ok(())
    }
}

impl Drop for ChatEngine {
    fn drop(&mut self) {
        self.stop_all();
    }
}

// ---------- terminal attach ----------

/// A live PTY running the Claude TUI on a session.
pub struct ChatPty {
    writer: Box<dyn Write + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
}

/// Spawn the TUI attached to a session. `resume` = session has history.
pub fn attach_terminal(
    binary: &Path,
    project_root: &Path,
    session_id: &str,
    resume: bool,
    model: Option<&str>,
    on_data: impl Fn(&[u8]) + Send + 'static,
) -> Result<ChatPty> {
    use portable_pty::{native_pty_system, CommandBuilder, PtySize};
    // The interactive TUI is where the trust dialog actually blocks; pre-accept
    // it for this project folder before spawning.
    ensure_folder_trusted(project_root);
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows: 34,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| Error::Other(format!("pty: {e}")))?;
    let mut cmd = CommandBuilder::new(binary);
    if let Some(alias) = model.and_then(valid_model_alias) {
        cmd.args(["--model", alias]);
    }
    if resume {
        cmd.args(["--resume", session_id]);
    } else {
        cmd.args(["--session-id", session_id]);
    }
    cmd.cwd(project_root);
    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| Error::Other(format!("spawn tui: {e}")))?;
    drop(pair.slave);

    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| Error::Other(format!("pty reader: {e}")))?;
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            on_data(&buf[..n]);
        }
    });
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| Error::Other(format!("pty writer: {e}")))?;

    Ok(ChatPty {
        writer,
        master: pair.master,
        child,
    })
}

impl ChatPty {
    pub fn input(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer
            .write_all(bytes)
            .and_then(|_| self.writer.flush())
            .map_err(|e| Error::Other(format!("pty input: {e}")))
    }

    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        self.master
            .resize(portable_pty::PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| Error::Other(format!("pty resize: {e}")))
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::test_support::write_fake_claude;
    use std::sync::mpsc::{channel, Receiver};
    use std::time::Duration;

    /// Keep the folder-trust writes out of the developer's real ~/.claude.json:
    /// point CLAUDE_CONFIG_DIR at a throwaway dir shared by all tests (set once,
    /// so parallel test threads don't race on the env var).
    fn isolate_claude_config() {
        use std::sync::OnceLock;
        static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
        let d = DIR.get_or_init(|| tempfile::tempdir().unwrap());
        std::env::set_var("CLAUDE_CONFIG_DIR", d.path());
    }

    fn engine(behavior: &str) -> (tempfile::TempDir, ChatEngine, Receiver<ChatUpdate>) {
        isolate_claude_config();
        let dir = tempfile::tempdir().unwrap();
        let bin = write_fake_claude(dir.path(), behavior);
        let (tx, rx) = channel();
        let engine = ChatEngine::new(bin, dir.path().to_path_buf(), move |u| {
            let _ = tx.send(u);
        });
        (dir, engine, rx)
    }

    fn collect_until_done(rx: &Receiver<ChatUpdate>, secs: u64) -> Vec<ChatUpdate> {
        let mut out = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            if let Ok(u) = rx.recv_timeout(Duration::from_millis(200)) {
                let is_done = matches!(&u,
                    ChatUpdate::Status { status, .. } if status == "done" || status == "error");
                out.push(u);
                if is_done {
                    break;
                }
            }
        }
        out
    }

    #[test]
    fn model_alias_accepts_only_stable_tiers() {
        // The four stable tier aliases the CLI documents (`claude --help`).
        assert_eq!(valid_model_alias("haiku"), Some("haiku"));
        assert_eq!(valid_model_alias("sonnet"), Some("sonnet"));
        assert_eq!(valid_model_alias("opus"), Some("opus"));
        assert_eq!(valid_model_alias("fable"), Some("fable"));
        // Case/whitespace tolerant (the UI sends lowercase, but be robust).
        assert_eq!(valid_model_alias("  Opus "), Some("opus"));
        // Empty → default (no --model forwarded). Unknown/pinned ids rejected,
        // so we never forward an unrecognized string to the CLI.
        assert_eq!(valid_model_alias(""), None);
        assert_eq!(valid_model_alias("gpt-4"), None);
        assert_eq!(valid_model_alias("claude-fable-5"), None);
    }

    #[test]
    fn context_preamble_empty_when_nothing_open() {
        assert_eq!(build_context_preamble(None, &[]), None);
        assert_eq!(build_context_preamble(Some("a.md"), &[]), None);
    }

    #[test]
    fn context_preamble_lists_open_and_focused_with_caveat() {
        let open = vec!["notes/a.md".to_string(), "src/b.rs".to_string()];
        let p = build_context_preamble(Some("src/b.rs"), &open).unwrap();
        // The caveat wording must frame it as a weak hint, per the user.
        assert!(p.contains("NOT necessarily"), "missing caveat: {p}");
        assert!(p.contains("notes/a.md"));
        assert!(p.contains("src/b.rs"));
        assert!(p.contains("Currently focused: src/b.rs"), "no focus line: {p}");
    }

    #[test]
    fn context_preamble_without_focus_omits_focus_line() {
        let open = vec!["a.md".to_string()];
        let p = build_context_preamble(None, &open).unwrap();
        assert!(p.contains("a.md"));
        assert!(!p.contains("Currently focused"), "focus line leaked: {p}");
    }

    #[test]
    fn context_preamble_caps_long_lists() {
        let open: Vec<String> = (0..50).map(|i| format!("f{i}.md")).collect();
        let p = build_context_preamble(None, &open).unwrap();
        assert!(p.contains("f0.md"));
        // Well past the cap must be dropped and summarized, not listed.
        assert!(!p.contains("f49.md"), "list not capped: {p}");
        assert!(p.contains("more"), "no truncation note: {p}");
    }

    #[test]
    fn folder_trust_sets_flag_and_preserves_other_keys() {
        let existing = serde_json::json!({
            "anonymousId": "keep-me",
            "projects": {
                "/other/proj": { "hasTrustDialogAccepted": true, "lastCost": 1.5 }
            }
        });
        let out = apply_folder_trust(existing, &["/ken/proj".to_string()]);
        // Our project is now trusted.
        assert_eq!(out["projects"]["/ken/proj"]["hasTrustDialogAccepted"], true);
        assert_eq!(out["projects"]["/ken/proj"]["hasCompletedProjectOnboarding"], true);
        // Unrelated keys and other projects are untouched.
        assert_eq!(out["anonymousId"], "keep-me");
        assert_eq!(out["projects"]["/other/proj"]["lastCost"], 1.5);
    }

    #[test]
    fn folder_trust_from_empty_config_is_idempotent() {
        let a = apply_folder_trust(Value::Null, &["/p".to_string()]);
        let b = apply_folder_trust(a.clone(), &["/p".to_string()]);
        assert_eq!(a, b);
        assert_eq!(b["projects"]["/p"]["hasTrustDialogAccepted"], true);
    }

    #[test]
    fn parse_event_shapes() {
        assert_eq!(parse_event("not json"), ParsedEvent::Other);
        assert_eq!(
            parse_event(r#"{"type":"system","subtype":"init"}"#),
            ParsedEvent::Init
        );
        assert_eq!(
            parse_event(r#"{"type":"result","is_error":true}"#),
            ParsedEvent::TurnResult { is_error: true }
        );
        assert_eq!(
            parse_event(
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"}]}}"#
            ),
            ParsedEvent::AssistantText("hi".into())
        );
        assert_eq!(
            parse_event(
                r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"a.md"}}]}}"#
            ),
            ParsedEvent::Activity("Read a.md".into())
        );
        // Unknown types are tolerated.
        assert_eq!(parse_event(r#"{"type":"mystery"}"#), ParsedEvent::Other);
    }

    #[test]
    fn send_receive_turn() {
        let (_d, engine, rx) = engine("complete");
        engine.send("chat-1", "Who owns billing?", false, None).unwrap();
        let updates = collect_until_done(&rx, 15);
        assert!(updates.iter().any(|u| matches!(u,
            ChatUpdate::Status { status, .. } if status == "working")));
        assert!(updates.iter().any(|u| matches!(u,
            ChatUpdate::Message { role, content, .. }
                if role == "assistant" && content.contains("Who owns billing?"))));
        assert!(matches!(updates.last().unwrap(),
            ChatUpdate::Status { status, .. } if status == "done"));
    }

    #[test]
    fn tool_use_becomes_activity_line() {
        let (_d, engine, rx) = engine("complete");
        engine.send("chat-2", "usetool please", false, None).unwrap();
        let updates = collect_until_done(&rx, 15);
        assert!(updates.iter().any(|u| matches!(u,
            ChatUpdate::Message { role, content, .. }
                if role == "activity" && content == "Read notes/meeting.md")));
    }

    #[test]
    fn second_turn_reuses_process_and_death_recovers_with_resume() {
        let (_d, engine, rx) = engine("stream-die");
        // First turn completes, then the fake dies (exit 7).
        engine.send("chat-3", "one", false, None).unwrap();
        let first = collect_until_done(&rx, 15);
        assert!(matches!(first.last().unwrap(),
            ChatUpdate::Status { status, .. } if status == "done"));

        // Give the death a moment to be noticed, then send again: the engine
        // must respawn (with --resume) and the turn must complete.
        std::thread::sleep(Duration::from_millis(400));
        engine.send("chat-3", "two", true, None).unwrap();
        let second = collect_until_done(&rx, 15);
        assert!(second.iter().any(|u| matches!(u,
            ChatUpdate::Message { content, .. } if content.contains("two"))));
    }

    #[test]
    fn blocked_stdin_does_not_freeze_the_engine() {
        // A fake that stops draining stdin mid-session: a large send() will
        // wedge on the pipe write. The engine must not hold its `live` map
        // lock across that write, or every other engine call would freeze too.
        let (_d, engine, _rx) = engine("stream-stall");
        let engine = Arc::new(engine);

        let sender = engine.clone();
        std::thread::spawn(move || {
            // Well past any OS pipe buffer (64 KiB) so write_all blocks.
            let big = "x".repeat(2 * 1024 * 1024);
            let _ = sender.send("chat-stall", &big, false, None);
        });
        // Let the sender spawn the process and wedge on the write.
        std::thread::sleep(Duration::from_millis(500));

        // A concurrent engine call must answer promptly rather than block on
        // the map lock the wedged send would otherwise still hold.
        let probe = engine.clone();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(probe.is_live("chat-stall"));
        });
        assert!(
            rx.recv_timeout(Duration::from_secs(5)).is_ok(),
            "is_live() blocked behind a wedged stdin write — the engine froze"
        );
        // Dropping the engine kills the stalled child and unblocks the sender.
    }

    /// Live test against the real Claude CLI — run explicitly with
    /// `cargo test -p ken-core real_chat -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_chat_conversation_and_terminal() {
        isolate_claude_config();
        let Some(binary) = crate::runner::discover_claude() else {
            panic!("claude CLI not found");
        };
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("fact.md"),
            "# Project fact\nThe secret launch codename is Bluebird.\n",
        )
        .unwrap();

        let (tx, rx) = channel();
        let engine = ChatEngine::new(binary.clone(), dir.path().to_path_buf(), move |u| {
            let _ = tx.send(u);
        });
        let chat_id = uuid::Uuid::new_v4().to_string();

        // Turn 1.
        engine
            .send(&chat_id, "Read fact.md and reply with just the codename.", false, None)
            .unwrap();
        let updates = collect_until_done(&rx, 240);
        let reply: String = updates
            .iter()
            .filter_map(|u| match u {
                ChatUpdate::Message { role, content, .. } if role == "assistant" => {
                    Some(content.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        eprintln!("turn 1 reply: {reply}");
        assert!(reply.contains("Bluebird"), "reply was: {reply}");
        assert!(matches!(updates.last().unwrap(),
            ChatUpdate::Status { status, .. } if status == "done"));

        // Kill the process, then resume in a fresh one: context must survive.
        engine.stop(&chat_id);
        engine
            .send(&chat_id, "Repeat the codename you just told me, nothing else.", true, None)
            .unwrap();
        let updates2 = collect_until_done(&rx, 240);
        let reply2: String = updates2
            .iter()
            .filter_map(|u| match u {
                ChatUpdate::Message { role, content, .. } if role == "assistant" => {
                    Some(content.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        eprintln!("turn 2 reply (after resume): {reply2}");
        assert!(reply2.contains("Bluebird"), "resume lost context: {reply2}");
        engine.stop(&chat_id);

        // Terminal attach on the same session: the TUI must paint something.
        std::thread::sleep(Duration::from_millis(500));
        let (dtx, drx) = channel::<usize>();
        let mut pty = attach_terminal(&binary, dir.path(), &chat_id, true, None, move |b| {
            let _ = dtx.send(b.len());
        })
        .unwrap();
        let mut total = 0;
        let deadline = Instant::now() + Duration::from_secs(60);
        while total < 500 && Instant::now() < deadline {
            if let Ok(n) = drx.recv_timeout(Duration::from_millis(500)) {
                total += n;
            }
        }
        eprintln!("terminal painted {total} bytes");
        assert!(total >= 500, "TUI produced almost no output: {total} bytes");
        pty.kill();
    }

    #[test]
    fn terminal_attach_round_trip() {
        isolate_claude_config();
        let dir = tempfile::tempdir().unwrap();
        // The fake, given no special args, acts like a TUI: reads stdin.
        let bin = write_fake_claude(dir.path(), "complete");
        let (tx, rx) = channel::<Vec<u8>>();
        let mut pty = attach_terminal(&bin, dir.path(), "sess-t", false, None, move |b| {
            let _ = tx.send(b.to_vec());
        })
        .unwrap();
        assert!(pty.is_alive());
        pty.resize(40, 120).unwrap();
        pty.input(b"/exit\r").unwrap();
        // Fake exits on /exit; PTY reader ends.
        let deadline = Instant::now() + Duration::from_secs(10);
        while pty.is_alive() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(!pty.is_alive());
        drop(rx);
    }
}

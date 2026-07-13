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

struct Conversation {
    child: Child,
    stdin: ChildStdin,
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
    pub fn send(&self, chat_id: &str, text: &str, resume: bool) -> Result<()> {
        self.ensure_process(chat_id, resume)?;
        let payload = serde_json::json!({
            "type": "user",
            "message": { "role": "user", "content": [{ "type": "text", "text": text }] }
        });
        let mut live = self.live.lock().unwrap();
        let conv = live
            .get_mut(chat_id)
            .ok_or_else(|| Error::Other("chat process vanished".into()))?;
        writeln!(conv.stdin, "{payload}")
            .and_then(|_| conv.stdin.flush())
            .map_err(|e| Error::Other(format!("chat send failed: {e}")))?;
        drop(live);
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

    fn ensure_process(&self, chat_id: &str, resume: bool) -> Result<()> {
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
                stdin,
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
    on_data: impl Fn(&[u8]) + Send + 'static,
) -> Result<ChatPty> {
    use portable_pty::{native_pty_system, CommandBuilder, PtySize};
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

    fn engine(behavior: &str) -> (tempfile::TempDir, ChatEngine, Receiver<ChatUpdate>) {
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
        engine.send("chat-1", "Who owns billing?", false).unwrap();
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
        engine.send("chat-2", "usetool please", false).unwrap();
        let updates = collect_until_done(&rx, 15);
        assert!(updates.iter().any(|u| matches!(u,
            ChatUpdate::Message { role, content, .. }
                if role == "activity" && content == "Read notes/meeting.md")));
    }

    #[test]
    fn second_turn_reuses_process_and_death_recovers_with_resume() {
        let (_d, engine, rx) = engine("stream-die");
        // First turn completes, then the fake dies (exit 7).
        engine.send("chat-3", "one", false).unwrap();
        let first = collect_until_done(&rx, 15);
        assert!(matches!(first.last().unwrap(),
            ChatUpdate::Status { status, .. } if status == "done"));

        // Give the death a moment to be noticed, then send again: the engine
        // must respawn (with --resume) and the turn must complete.
        std::thread::sleep(Duration::from_millis(400));
        engine.send("chat-3", "two", true).unwrap();
        let second = collect_until_done(&rx, 15);
        assert!(second.iter().any(|u| matches!(u,
            ChatUpdate::Message { content, .. } if content.contains("two"))));
    }

    /// Live test against the real Claude CLI — run explicitly with
    /// `cargo test -p ken-core real_chat -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_chat_conversation_and_terminal() {
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
            .send(&chat_id, "Read fact.md and reply with just the codename.", false)
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
            .send(&chat_id, "Repeat the codename you just told me, nothing else.", true)
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
        let mut pty = attach_terminal(&binary, dir.path(), &chat_id, true, move |b| {
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
        let dir = tempfile::tempdir().unwrap();
        // The fake, given no special args, acts like a TUI: reads stdin.
        let bin = write_fake_claude(dir.path(), "complete");
        let (tx, rx) = channel::<Vec<u8>>();
        let mut pty = attach_terminal(&bin, dir.path(), "sess-t", false, move |b| {
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

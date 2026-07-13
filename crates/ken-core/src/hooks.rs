//! Claude Code hook plumbing: a localhost listener that receives hook
//! POSTs (Stop, Notification, …) and routes them to the run that owns the
//! session id, plus the merge logic that installs Ken's hooks into a
//! project's `.claude/settings.local.json` without touching user settings.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::{Error, Result};

pub const HOOK_PATH: &str = "/ken-hook";

#[derive(Debug, Clone)]
pub struct HookEvent {
    /// `Stop`, `Notification`, `SubagentStop`, …
    pub event: String,
    pub session_id: String,
    pub payload: Value,
}

type Subscribers = Arc<Mutex<HashMap<String, Sender<HookEvent>>>>;

pub struct HookListener {
    port: u16,
    subscribers: Subscribers,
    shutdown: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    server: Arc<tiny_http::Server>,
}

impl HookListener {
    pub fn start() -> Result<HookListener> {
        let server = tiny_http::Server::http("127.0.0.1:0")
            .map_err(|e| Error::Other(format!("hook listener: {e}")))?;
        let port = match server.server_addr() {
            tiny_http::ListenAddr::IP(addr) => addr.port(),
            _ => return Err(Error::Other("hook listener: no tcp port".into())),
        };
        let server = Arc::new(server);
        let subscribers: Subscribers = Arc::new(Mutex::new(HashMap::new()));
        let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let s = server.clone();
        let subs = subscribers.clone();
        let stop = shutdown.clone();
        let thread = std::thread::spawn(move || {
            for mut request in s.incoming_requests() {
                if stop.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                let mut body = String::new();
                let _ = request.as_reader().read_to_string(&mut body);
                if request.url().starts_with(HOOK_PATH) {
                    if let Ok(v) = serde_json::from_str::<Value>(&body) {
                        let event = v
                            .get("hook_event_name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        let session_id = v
                            .get("session_id")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        if !session_id.is_empty() {
                            if let Some(tx) = subs.lock().unwrap().get(&session_id) {
                                let _ = tx.send(HookEvent {
                                    event,
                                    session_id: session_id.clone(),
                                    payload: v,
                                });
                            }
                            // Unknown session ids (the user's own claude
                            // sessions) are ignored on purpose.
                        }
                    }
                }
                let _ = request.respond(tiny_http::Response::empty(200));
            }
        });

        Ok(HookListener {
            port,
            subscribers,
            shutdown,
            thread: Some(thread),
            server,
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn hook_url(&self) -> String {
        format!("http://127.0.0.1:{}{}", self.port, HOOK_PATH)
    }

    /// Subscribe to hook events for one session id.
    pub fn subscribe(&self, session_id: &str) -> Receiver<HookEvent> {
        let (tx, rx) = channel();
        self.subscribers
            .lock()
            .unwrap()
            .insert(session_id.to_string(), tx);
        rx
    }

    pub fn unsubscribe(&self, session_id: &str) {
        self.subscribers.lock().unwrap().remove(session_id);
    }
}

impl Drop for HookListener {
    fn drop(&mut self) {
        self.shutdown
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.server.unblock();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The command Claude Code runs for each hook event.
fn hook_command(hook_url: &str) -> String {
    format!("curl -s -X POST {hook_url} -H 'Content-Type: application/json' -d @- >/dev/null 2>&1 || true")
}

/// Install (or refresh) Ken's Stop + Notification hooks in the project's
/// `.claude/settings.local.json`. Any existing entry whose command mentions
/// `/ken-hook` is ours from a previous run (possibly another port) and is
/// replaced; every other setting in the file is preserved byte-for-byte in
/// meaning.
pub fn install_hooks(project_root: &Path, hook_url: &str) -> Result<()> {
    let dir = project_root.join(".claude");
    std::fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
    let path = dir.join("settings.local.json");

    let mut settings: Value = if path.exists() {
        let raw = std::fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
        serde_json::from_str(&raw)
            .map_err(|e| Error::Other(format!("can't update {}: {e}", path.display())))?
    } else {
        json!({})
    };

    let hooks = settings
        .as_object_mut()
        .ok_or_else(|| Error::Other("settings.local.json is not an object".into()))?
        .entry("hooks")
        .or_insert_with(|| json!({}));

    for event in ["Stop", "Notification"] {
        let list = hooks
            .as_object_mut()
            .ok_or_else(|| Error::Other("hooks is not an object".into()))?
            .entry(event)
            .or_insert_with(|| json!([]));
        let arr = list
            .as_array_mut()
            .ok_or_else(|| Error::Other(format!("hooks.{event} is not a list")))?;
        // Drop our previous entries (any group containing a /ken-hook command).
        arr.retain(|group| {
            !group
                .get("hooks")
                .and_then(Value::as_array)
                .is_some_and(|hs| {
                    hs.iter().any(|h| {
                        h.get("command")
                            .and_then(Value::as_str)
                            .is_some_and(|c| c.contains(HOOK_PATH))
                    })
                })
        });
        arr.push(json!({
            "hooks": [{ "type": "command", "command": hook_command(hook_url) }]
        }));
    }

    let out = serde_json::to_string_pretty(&settings)
        .map_err(|e| Error::Other(e.to_string()))?;
    std::fs::write(&path, out + "\n").map_err(|e| Error::io(&path, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpStream;

    fn post(port: u16, body: &str) {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let req = format!(
            "POST {HOOK_PATH} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(req.as_bytes()).unwrap();
        let mut buf = String::new();
        let _ = stream.read_to_string(&mut buf);
    }

    #[test]
    fn routes_events_by_session_id() {
        let listener = HookListener::start().unwrap();
        let rx = listener.subscribe("sess-a");

        post(
            listener.port(),
            r#"{"hook_event_name":"Stop","session_id":"sess-a"}"#,
        );
        let ev = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(ev.event, "Stop");
        assert_eq!(ev.session_id, "sess-a");
    }

    #[test]
    fn ignores_foreign_sessions() {
        let listener = HookListener::start().unwrap();
        let rx = listener.subscribe("mine");
        post(
            listener.port(),
            r#"{"hook_event_name":"Stop","session_id":"someone-elses"}"#,
        );
        assert!(rx
            .recv_timeout(std::time::Duration::from_millis(300))
            .is_err());
    }

    #[test]
    fn install_hooks_preserves_user_settings() {
        let dir = tempfile::tempdir().unwrap();
        let claude_dir = dir.path().join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        let path = claude_dir.join("settings.local.json");
        std::fs::write(
            &path,
            r#"{
  "permissions": {"allow": ["Bash(ls:*)"]},
  "hooks": {
    "Stop": [
      {"hooks": [{"type": "command", "command": "echo user-hook"}]},
      {"hooks": [{"type": "command", "command": "curl http://127.0.0.1:1111/ken-hook -d @-"}]}
    ]
  }
}"#,
        )
        .unwrap();

        install_hooks(dir.path(), "http://127.0.0.1:2222/ken-hook").unwrap();

        let v: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        // User permission block untouched.
        assert_eq!(v["permissions"]["allow"][0], "Bash(ls:*)");
        let stops = v["hooks"]["Stop"].as_array().unwrap();
        // user hook + exactly one Ken hook (old port replaced).
        assert_eq!(stops.len(), 2);
        let commands: Vec<&str> = stops
            .iter()
            .flat_map(|g| g["hooks"].as_array().unwrap())
            .map(|h| h["command"].as_str().unwrap())
            .collect();
        assert!(commands.iter().any(|c| c.contains("echo user-hook")));
        assert_eq!(
            commands.iter().filter(|c| c.contains("/ken-hook")).count(),
            1
        );
        assert!(commands.iter().any(|c| c.contains(":2222")));
        // Notification hook installed too.
        assert!(v["hooks"]["Notification"].as_array().unwrap().len() == 1);
    }

    #[test]
    fn install_hooks_creates_file_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        install_hooks(dir.path(), "http://127.0.0.1:3333/ken-hook").unwrap();
        let v: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join(".claude/settings.local.json")).unwrap(),
        )
        .unwrap();
        assert!(v["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains(":3333"));
    }
}

//! Drive the real ken-mcp binary over stdio, exactly as an MCP client
//! would: spawn, handshake, call tools, assert on the JSON lines that come
//! back. Everything lives in tempdirs via KEN_DATA_DIR — nothing touches
//! the real app data.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{json, Value};

use ken_core::db::Db;
use ken_core::project::Project;
use ken_core::registry::Registry;
use ken_core::scan;

/// A registered, indexed fixture project under a tempdir KEN_DATA_DIR —
/// the same layout the Ken app leaves behind.
struct Fixture {
    base: tempfile::TempDir,
    root: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let base = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("notes")).unwrap();
    std::fs::write(
        root.path().join("notes/meeting.md"),
        "# Meeting — Jul 8\nConfirmed the billing cutover date slips to Sept 12.\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join("People.md"),
        "Priya Natarajan owns billing cutover with Marcus as backup.\n",
    )
    .unwrap();

    let project = Project::create(root.path(), "Atlas").unwrap();
    let mut db = Db::open(base.path(), project.config.id).unwrap();
    scan::scan(&project, &mut db).unwrap();
    let mut registry = Registry::default();
    registry.add(&project);
    registry.save(base.path()).unwrap();

    Fixture { base, root }
}

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
}

impl Client {
    fn spawn(base: &Path, project: Option<&Path>) -> Client {
        Client::spawn_with(base, project, &[])
    }

    fn spawn_with(base: &Path, project: Option<&Path>, flags: &[&str]) -> Client {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_ken-mcp"));
        if let Some(p) = project {
            cmd.arg("--project").arg(p);
        }
        cmd.args(flags);
        let mut child = cmd
            .env("KEN_DATA_DIR", base)
            // open_in_ken is listed only when Ken's own chat started the server.
            .env_remove("KEN_APP_URL")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn ken-mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Client { child, stdin, stdout, next_id: 0 }
    }

    /// Send a request and read exactly one reply line.
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let msg = json!({ "jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params });
        self.send_raw(&msg.to_string());
        let reply = self.read_reply();
        assert_eq!(reply["id"], self.next_id, "reply out of order: {reply}");
        reply
    }

    fn notify(&mut self, method: &str) {
        self.send_raw(&json!({ "jsonrpc": "2.0", "method": method }).to_string());
    }

    fn send_raw(&mut self, line: &str) {
        writeln!(self.stdin, "{line}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn read_reply(&mut self) -> Value {
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        assert!(!line.is_empty(), "server closed stdout");
        serde_json::from_str(&line).expect("one JSON object per line")
    }

    /// Call a tool; return (text, isError).
    fn tool(&mut self, name: &str, args: Value) -> (String, bool) {
        let reply = self.request("tools/call", json!({ "name": name, "arguments": args }));
        let result = &reply["result"];
        assert!(
            result.is_object(),
            "tool call should be a result, not a protocol error: {reply}"
        );
        (
            result["content"][0]["text"].as_str().unwrap().to_string(),
            result["isError"].as_bool().unwrap_or(false),
        )
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn scoped_server_full_session() {
    let fx = fixture();
    let mut client = Client::spawn(fx.base.path(), Some(fx.root.path()));

    // initialize → echoes a known newer protocol version, names the server.
    let reply = client.request(
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "test", "version": "0" }
        }),
    );
    assert_eq!(reply["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(reply["result"]["serverInfo"]["name"], "ken");
    assert!(reply["result"]["capabilities"]["tools"].is_object());
    // The brief, as the server's instructions (knowledge-core decision 1).
    let brief = reply["result"]["instructions"].as_str().unwrap();
    assert!(brief.contains("The code is the truth"), "{brief}");

    // initialized notification gets no reply — proven by ping answering next.
    client.notify("notifications/initialized");
    let reply = client.request("ping", json!({}));
    assert_eq!(reply["result"], json!({}));

    // tools/list → exactly the knowledge server's tools (decision 22).
    let reply = client.request("tools/list", json!({}));
    let names: Vec<&str> = reply["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["search", "read", "related", "code_definition", "code_usages", "code_outline", "code_imports", "history", "tickets", "propose_decision", "add_source", "repos"]
    );

    // search finds seeded content.
    let (text, is_err) = client.tool("search", json!({ "query": "billing cutover" }));
    assert!(!is_err, "{text}");
    assert!(text.contains("notes/meeting.md"), "{text}");
    assert!(!text.contains("<mark>"), "{text}");

    // read round-trips the file content.
    let (text, is_err) = client.tool("read", json!({ "path": "notes/meeting.md" }));
    assert!(!is_err, "{text}");
    assert_eq!(
        text,
        std::fs::read_to_string(fx.root.path().join("notes/meeting.md")).unwrap()
    );

    // Path escape fails safely — and the file is definitely not read.
    let (text, is_err) = client.tool("read", json!({ "path": "../etc/passwt" }));
    assert!(is_err, "{text}");
    assert!(text.contains("outside the project"), "{text}");

    // Malformed line → -32700, then the server keeps serving.
    client.send_raw("{not json");
    let reply = client.read_reply();
    assert_eq!(reply["error"]["code"], -32700);
    let reply = client.request("ping", json!({}));
    assert!(reply["result"].is_object());

    // Unknown method → -32601.
    let reply = client.request("resources/list", json!({}));
    assert_eq!(reply["error"]["code"], -32601);
}

#[test]
fn unscoped_server_reads_a_path_in_its_project() {
    let fx = fixture();
    let mut client = Client::spawn(fx.base.path(), None);
    client.request("initialize", json!({ "protocolVersion": "2024-11-05" }));
    client.notify("notifications/initialized");

    // Search needs no project: every repo.
    let (text, is_err) = client.tool("search", json!({ "query": "billing" }));
    assert!(!is_err, "{text}");
    assert!(text.contains("notes/meeting.md"), "{text}");

    // A project-relative path without its project → a helpful error naming
    // the registered projects.
    let (text, is_err) = client.tool("read", json!({ "path": "People.md" }));
    assert!(is_err, "{text}");
    assert!(text.contains("Available projects: Atlas"), "{text}");

    // repos names the fixture.
    let (text, is_err) = client.tool("repos", json!({}));
    assert!(!is_err, "{text}");
    assert!(text.contains("Atlas"), "{text}");
    let root: PathBuf = fx.root.path().to_path_buf();
    assert!(text.contains(&root.to_string_lossy().to_string()), "{text}");

    // With the project named, the same call works.
    let (text, is_err) = client.tool("read", json!({ "path": "People.md", "project": "Atlas" }));
    assert!(!is_err, "{text}");
    assert!(text.contains("Priya Natarajan"), "{text}");
}

/// `--personal` serves the personal set only: tasks and memory, which Ken's
/// own chat registers beside the knowledge server (decision 22).
#[test]
fn personal_server_serves_tasks_and_memory_only() {
    let fx = fixture();
    let mut client = Client::spawn_with(fx.base.path(), None, &["--personal"]);
    let reply = client.request("initialize", json!({ "protocolVersion": "2024-11-05" }));
    assert_eq!(reply["result"]["serverInfo"]["name"], "ken-personal");
    client.notify("notifications/initialized");

    let reply = client.request("tools/list", json!({}));
    let names: Vec<&str> = reply["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["task_list", "task_create", "task_update", "memory_write", "journal_append"]);

    // The knowledge tools are not here.
    let reply = client.request("tools/call", json!({ "name": "search", "arguments": { "query": "billing" } }));
    assert_eq!(reply["error"]["code"], -32602, "{reply}");
}

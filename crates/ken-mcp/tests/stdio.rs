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
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_ken-mcp"));
        if let Some(p) = project {
            cmd.arg("--project").arg(p);
        }
        let mut child = cmd
            .env("KEN_DATA_DIR", base)
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

    // initialized notification gets no reply — proven by ping answering next.
    client.notify("notifications/initialized");
    let reply = client.request("ping", json!({}));
    assert_eq!(reply["result"], json!({}));

    // tools/list → exactly our four tools.
    let reply = client.request("tools/list", json!({}));
    let names: Vec<&str> = reply["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["search_knowledge", "read_document", "list_documents", "list_projects"]
    );

    // search finds seeded content, marks stripped to bold.
    let (text, is_err) = client.tool("search_knowledge", json!({ "query": "billing cutover" }));
    assert!(!is_err, "{text}");
    assert!(text.contains("notes/meeting.md"), "{text}");
    assert!(text.contains("**billing**"), "{text}");
    assert!(!text.contains("<mark>"), "{text}");

    // read_document round-trips the file content.
    let (text, is_err) = client.tool("read_document", json!({ "path": "notes/meeting.md" }));
    assert!(!is_err, "{text}");
    assert_eq!(
        text,
        std::fs::read_to_string(fx.root.path().join("notes/meeting.md")).unwrap()
    );

    // Path escape fails safely — and the file is definitely not read.
    let (text, is_err) = client.tool("read_document", json!({ "path": "../etc/passwt" }));
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
fn unscoped_server_requires_project() {
    let fx = fixture();
    let mut client = Client::spawn(fx.base.path(), None);
    client.request("initialize", json!({ "protocolVersion": "2024-11-05" }));
    client.notify("notifications/initialized");

    // Without a project → helpful error naming the registered projects.
    let (text, is_err) = client.tool("search_knowledge", json!({ "query": "billing" }));
    assert!(is_err, "{text}");
    assert!(text.contains("Available projects: Atlas"), "{text}");

    // list_projects names the fixture.
    let (text, is_err) = client.tool("list_projects", json!({}));
    assert!(!is_err, "{text}");
    assert!(text.contains("Atlas"), "{text}");
    let root: PathBuf = fx.root.path().to_path_buf();
    assert!(text.contains(&root.to_string_lossy().to_string()), "{text}");

    // With the project named, the same call works.
    let (text, is_err) =
        client.tool("search_knowledge", json!({ "query": "billing", "project": "Atlas" }));
    assert!(!is_err, "{text}");
    assert!(text.contains("notes/meeting.md"), "{text}");
}

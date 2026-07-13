//! ken-mcp — stdio MCP server over ken-core. Hand-rolled JSON-RPC 2.0:
//! one JSON object per line on stdout, stdin read line-by-line, nothing
//! but protocol on stdout (diagnostics go to stderr). Read-only on the
//! SQLite index by construction — the server never writes anything.
//!
//! Scoping: `ken-mcp --project <path>` locks every tool to that project;
//! unscoped, the project tools take a required `project` argument matched
//! against Ken's registry.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use ken_core::db::Db;
use ken_core::project::Project;
use ken_core::registry::{self, Registry};

/// The protocol revision we implement. Newer revisions we know to be
/// wire-compatible with our subset are echoed back on request.
const PROTOCOL_VERSION: &str = "2024-11-05";
const KNOWN_PROTOCOL_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18"];

/// Cap on `read_document` output.
const MAX_DOCUMENT_BYTES: usize = 200 * 1024;

struct Server {
    base_dir: PathBuf,
    /// Root the server is locked to (`--project <path>`), if any.
    scoped: Option<PathBuf>,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut scoped = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--project" => match args.next() {
                Some(path) => scoped = Some(PathBuf::from(path)),
                None => {
                    eprintln!("ken-mcp: --project needs a path");
                    std::process::exit(2);
                }
            },
            "--version" => {
                eprintln!("ken-mcp {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            other => {
                eprintln!("ken-mcp: unknown argument {other:?}\nusage: ken-mcp [--project <path>]");
                std::process::exit(2);
            }
        }
    }

    let base_dir = match registry::default_base_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("ken-mcp: {e}");
            std::process::exit(1);
        }
    };
    let mut server = Server { base_dir, scoped };

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if let Some(reply) = handle_line(&mut server, &line) {
            let mut out = stdout.lock();
            if writeln!(out, "{reply}").and_then(|_| out.flush()).is_err() {
                break; // client hung up
            }
        }
    }
}

/// One line in, at most one line out. Malformed input is answered (it has
/// no id to be a notification) and never kills the loop.
fn handle_line(server: &mut Server, line: &str) -> Option<String> {
    if line.trim().is_empty() {
        return None;
    }
    let reply = match serde_json::from_str::<Value>(line) {
        Ok(request) => handle_request(server, &request)?,
        Err(e) => rpc_error(&Value::Null, -32700, &format!("parse error: {e}")),
    };
    Some(reply.to_string())
}

/// Route one decoded JSON-RPC message. `None` means "no reply" — the rule
/// for notifications (no id), including unknown ones.
fn handle_request(server: &mut Server, request: &Value) -> Option<Value> {
    let Some(obj) = request.as_object() else {
        return Some(rpc_error(&Value::Null, -32600, "request must be a JSON object"));
    };
    let id = obj.get("id").cloned();
    let is_notification = matches!(id, None | Some(Value::Null));
    let id = id.unwrap_or(Value::Null);

    let Some(method) = obj.get("method").and_then(|m| m.as_str()) else {
        if is_notification {
            return None;
        }
        return Some(rpc_error(&id, -32600, "missing method"));
    };
    let params = obj.get("params").cloned().unwrap_or(Value::Null);

    let reply = match method {
        "initialize" => rpc_result(&id, initialize_result(&params)),
        "ping" => rpc_result(&id, json!({})),
        "tools/list" => rpc_result(&id, json!({ "tools": tool_definitions() })),
        "tools/call" => {
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match name {
                "search_knowledge" | "read_document" | "list_documents" | "list_projects" => {
                    let outcome = call_tool(server, name, &args);
                    rpc_result(&id, tool_content(outcome))
                }
                _ => rpc_error(&id, -32602, &format!("unknown tool: {name:?}")),
            }
        }
        _ => {
            if is_notification {
                return None; // e.g. notifications/initialized
            }
            rpc_error(&id, -32601, &format!("method not found: {method}"))
        }
    };
    if is_notification {
        return None;
    }
    Some(reply)
}

fn initialize_result(params: &Value) -> Value {
    let requested = params
        .get("protocolVersion")
        .and_then(|v| v.as_str())
        .unwrap_or(PROTOCOL_VERSION);
    let version = if KNOWN_PROTOCOL_VERSIONS.contains(&requested) {
        requested
    } else {
        PROTOCOL_VERSION
    };
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "ken", "version": env!("CARGO_PKG_VERSION") }
    })
}

fn rpc_result(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// Tool results are text content; tool failures are `isError` content so
/// the calling model sees the explanation and can self-correct.
fn tool_content(outcome: Result<String, String>) -> Value {
    match outcome {
        Ok(text) => json!({ "content": [{ "type": "text", "text": text }] }),
        Err(text) => json!({
            "content": [{ "type": "text", "text": text }],
            "isError": true
        }),
    }
}

fn tool_definitions() -> Value {
    let project_arg = json!({
        "type": "string",
        "description": "Which Ken project to use — a project name or folder \
path from list_projects. Required unless the server was started locked to \
one project."
    });
    json!([
        {
            "name": "search_knowledge",
            "description": "Full-text search across a Ken project's indexed \
documents (notes, docs, spreadsheets, PDFs…). Returns ranked hits with the \
file path and a snippet; matched terms are shown in **bold**. All words must \
match; the last word may be a prefix.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search terms." },
                    "limit": { "type": "integer", "description": "Maximum hits to return (default 20)." },
                    "project": project_arg
                },
                "required": ["query"]
            }
        },
        {
            "name": "read_document",
            "description": "Read one document from a Ken project by its \
project-relative path (as returned by search_knowledge or list_documents). \
Text files return their raw content (capped at 200 KB); binary formats \
(docx, xlsx, pptx, pdf, images) return the text Ken's indexer extracted.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Project-relative file path." },
                    "project": project_arg
                },
                "required": ["path"]
            }
        },
        {
            "name": "list_documents",
            "description": "List a Ken project's indexed files — path, kind, \
size, and modification time — optionally only those under a folder.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "folder": { "type": "string", "description": "Project-relative folder to list (default: whole project)." },
                    "project": project_arg
                }
            }
        },
        {
            "name": "list_projects",
            "description": "List the Ken projects registered on this machine \
— name, folder path, and whether the folder is currently available.",
            "inputSchema": { "type": "object", "properties": {} }
        }
    ])
}

// --- tools ---

fn call_tool(server: &Server, name: &str, args: &Value) -> Result<String, String> {
    match name {
        "list_projects" => list_projects(server),
        "search_knowledge" => {
            let query = require_str(args, "query")?;
            let limit = args
                .get("limit")
                .and_then(|l| l.as_u64())
                .map(|l| l.clamp(1, 200) as usize)
                .unwrap_or(20);
            let (project, note) = resolve_project(server, args)?;
            let db = open_index(server, &project)?;
            let hits = db
                .search(&query, limit)
                .map_err(|e| format!("search failed: {e}"))?;
            let mut out = note.unwrap_or_default();
            if hits.is_empty() {
                out.push_str(&format!(
                    "No matches for {query:?} in project \"{}\". Try fewer or \
different words — all terms must match.",
                    project.config.name
                ));
            } else {
                out.push_str(&format!(
                    "{} result{} for {query:?} in project \"{}\":\n",
                    hits.len(),
                    if hits.len() == 1 { "" } else { "s" },
                    project.config.name
                ));
                for (i, hit) in hits.iter().enumerate() {
                    let snippet = hit.snippet.replace("<mark>", "**").replace("</mark>", "**");
                    out.push_str(&format!("\n{}. {} — {}", i + 1, hit.rel_path, snippet));
                }
            }
            Ok(out)
        }
        "read_document" => {
            let path = require_str(args, "path")?;
            let (project, note) = resolve_project(server, args)?;
            // Validate before anything else so `..`/absolute paths are
            // refused outright, whatever the index says.
            let abs = project
                .resolve(&path)
                .map_err(|_| format!("{path:?} is outside the project — paths are relative to the project root and may not contain \"..\""))?;
            let db = open_index(server, &project)?;
            let row = db
                .get_file(&path)
                .map_err(|e| format!("index lookup failed: {e}"))?
                .ok_or_else(|| {
                    format!(
                        "{path:?} is not in project \"{}\"'s index. Use \
list_documents or search_knowledge to find valid paths.",
                        project.config.name
                    )
                })?;
            let mut out = note.unwrap_or_default();
            match row.kind.as_str() {
                "md" | "txt" | "code" => {
                    let bytes = std::fs::read(&abs)
                        .map_err(|e| format!("could not read {path:?}: {e}"))?;
                    let truncated = bytes.len() > MAX_DOCUMENT_BYTES;
                    let end = if truncated {
                        floor_char_boundary_at(&bytes, MAX_DOCUMENT_BYTES)
                    } else {
                        bytes.len()
                    };
                    out.push_str(&String::from_utf8_lossy(&bytes[..end]));
                    if truncated {
                        out.push_str("\n\n[truncated — file exceeds the 200 KB read limit]");
                    }
                }
                kind => {
                    let text = db
                        .get_text(&path)
                        .map_err(|e| format!("index lookup failed: {e}"))?
                        .filter(|t| !t.trim().is_empty())
                        .ok_or_else(|| match &row.error {
                            Some(reason) => format!(
                                "Ken could not extract text from {path:?} ({reason})."
                            ),
                            None => format!(
                                "{path:?} is a {kind} file with no extracted \
text in the index (it is indexed by name only)."
                            ),
                        })?;
                    out.push_str(&format!(
                        "[{kind} file — this is the text Ken's indexer \
extracted, not the original bytes]\n\n{text}"
                    ));
                }
            }
            Ok(out)
        }
        "list_documents" => {
            let folder = args
                .get("folder")
                .and_then(|f| f.as_str())
                .map(|f| f.trim_matches('/').to_string())
                .filter(|f| !f.is_empty());
            let (project, note) = resolve_project(server, args)?;
            let db = open_index(server, &project)?;
            let files = db
                .list_files()
                .map_err(|e| format!("index lookup failed: {e}"))?;
            let files: Vec<_> = match &folder {
                Some(f) => files
                    .into_iter()
                    .filter(|row| row.rel_path.starts_with(&format!("{f}/")))
                    .collect(),
                None => files,
            };
            let mut out = note.unwrap_or_default();
            let scope = match &folder {
                Some(f) => format!("under \"{f}\" in project \"{}\"", project.config.name),
                None => format!("in project \"{}\"", project.config.name),
            };
            if files.is_empty() {
                out.push_str(&format!("No indexed files {scope}."));
            } else {
                out.push_str(&format!(
                    "{} indexed file{} {scope} (path — kind, size, modified as unix seconds):\n",
                    files.len(),
                    if files.len() == 1 { "" } else { "s" },
                ));
                for row in files {
                    out.push_str(&format!(
                        "\n{} — {}, {} bytes, modified {}",
                        row.rel_path, row.kind, row.size, row.mtime
                    ));
                }
            }
            Ok(out)
        }
        _ => Err(format!("unknown tool: {name:?}")),
    }
}

fn require_str(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("the {key:?} argument is required"))
}

fn list_projects(server: &Server) -> Result<String, String> {
    let registry = Registry::load(&server.base_dir)
        .map_err(|e| format!("could not read Ken's project registry: {e}"))?;
    let statuses = registry.statuses();
    if statuses.is_empty() {
        return Ok("No Ken projects registered on this machine yet — open a \
folder in the Ken app first."
            .to_string());
    }
    let mut out = format!(
        "{} Ken project{}:\n",
        statuses.len(),
        if statuses.len() == 1 { "" } else { "s" }
    );
    for s in statuses {
        out.push_str(&format!(
            "\n{} — {}{}",
            s.entry.name,
            s.entry.path.display(),
            if s.available { "" } else { " (folder currently missing)" }
        ));
    }
    Ok(out)
}

/// Which project does this call target? Scoped servers always answer with
/// their own; unscoped servers require the `project` argument.
fn resolve_project(server: &Server, args: &Value) -> Result<(Project, Option<String>), String> {
    let requested = args.get("project").and_then(|p| p.as_str()).map(str::trim);

    if let Some(root) = &server.scoped {
        let project = Project::open(root).map_err(|e| {
            format!("could not open the project this server is locked to ({}): {e}", root.display())
        })?;
        let note = requested.filter(|r| !r.is_empty()).map(|_| {
            format!(
                "Note: this server is locked to project \"{}\" — the \
\"project\" argument was ignored.\n\n",
                project.config.name
            )
        });
        return Ok((project, note));
    }

    let registry = Registry::load(&server.base_dir)
        .map_err(|e| format!("could not read Ken's project registry: {e}"))?;
    let names: Vec<String> = registry.projects.iter().map(|p| p.name.clone()).collect();
    let available = if names.is_empty() {
        "No projects are registered yet — open a folder in the Ken app first.".to_string()
    } else {
        format!("Available projects: {}.", names.join(", "))
    };

    let Some(requested) = requested.filter(|r| !r.is_empty()) else {
        return Err(format!(
            "This server is not locked to a project, so the \"project\" \
argument is required (a name or folder path). {available}"
        ));
    };

    let entry = registry
        .projects
        .iter()
        .find(|p| {
            p.name.eq_ignore_ascii_case(requested)
                || p.path == Path::new(requested)
                || same_canonical(&p.path, Path::new(requested))
        })
        .ok_or_else(|| format!("No Ken project matches {requested:?}. {available}"))?;
    let project = Project::open(&entry.path)
        .map_err(|e| format!("could not open project \"{}\": {e}", entry.name))?;
    Ok((project, None))
}

fn same_canonical(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn open_index(server: &Server, project: &Project) -> Result<Db, String> {
    Db::open_read_only(&server.base_dir, project.config.id).map_err(|_| {
        format!(
            "Project \"{}\" has no index yet — open it in the Ken app once \
so Ken can build it.",
            project.config.name
        )
    })
}

/// Largest byte offset ≤ `at` that is a UTF-8 character boundary.
fn floor_char_boundary_at(bytes: &[u8], at: usize) -> usize {
    let mut end = at.min(bytes.len());
    while end > 0 && end < bytes.len() && (bytes[end] & 0b1100_0000) == 0b1000_0000 {
        end -= 1;
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;
    use ken_core::scan;

    /// A registered, indexed project in a tempdir KEN_DATA_DIR — the same
    /// shape the app leaves behind.
    struct Fixture {
        _base: tempfile::TempDir,
        _root: tempfile::TempDir,
        server: Server,
        root: PathBuf,
    }

    fn fixture(scoped: bool) -> Fixture {
        let base = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("notes")).unwrap();
        std::fs::write(
            root.path().join("notes/meeting.md"),
            "# Meeting\nConfirmed the billing cutover date slips to Sept 12.\n",
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

        let root_path = root.path().to_path_buf();
        Fixture {
            server: Server {
                base_dir: base.path().to_path_buf(),
                scoped: scoped.then(|| root_path.clone()),
            },
            root: root_path,
            _base: base,
            _root: root,
        }
    }

    fn call(server: &mut Server, raw: &str) -> Option<Value> {
        handle_line(server, raw).map(|s| serde_json::from_str(&s).unwrap())
    }

    fn tool(server: &mut Server, name: &str, args: Value) -> (String, bool) {
        let req = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args }
        });
        let reply = handle_request(server, &req).unwrap();
        let result = &reply["result"];
        (
            result["content"][0]["text"].as_str().unwrap().to_string(),
            result["isError"].as_bool().unwrap_or(false),
        )
    }

    #[test]
    fn initialize_echoes_known_versions_only() {
        let mut fx = fixture(true);
        let reply = call(
            &mut fx.server,
            r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
        )
        .unwrap();
        assert_eq!(reply["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(reply["result"]["serverInfo"]["name"], "ken");

        let reply = call(
            &mut fx.server,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2099-01-01"}}"#,
        )
        .unwrap();
        assert_eq!(reply["result"]["protocolVersion"], PROTOCOL_VERSION);
    }

    #[test]
    fn parse_tolerance_and_routing() {
        let mut fx = fixture(true);
        // Malformed line → -32700, and the server keeps answering.
        let reply = call(&mut fx.server, "this is not json").unwrap();
        assert_eq!(reply["error"]["code"], -32700);
        // Non-object JSON → -32600.
        let reply = call(&mut fx.server, "[1,2,3]").unwrap();
        assert_eq!(reply["error"]["code"], -32600);
        // Blank lines are ignored.
        assert!(call(&mut fx.server, "   ").is_none());
        // Notifications — known or unknown — never get replies.
        assert!(call(&mut fx.server, r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).is_none());
        assert!(call(&mut fx.server, r#"{"jsonrpc":"2.0","method":"no/such/notification"}"#).is_none());
        // Unknown method with an id → -32601.
        let reply = call(&mut fx.server, r#"{"jsonrpc":"2.0","id":7,"method":"resources/list"}"#).unwrap();
        assert_eq!(reply["error"]["code"], -32601);
        assert_eq!(reply["id"], 7);
        // Ping still works after all that.
        let reply = call(&mut fx.server, r#"{"jsonrpc":"2.0","id":8,"method":"ping"}"#).unwrap();
        assert_eq!(reply["result"], json!({}));
        // Unknown tool → invalid params, a protocol error.
        let reply = call(
            &mut fx.server,
            r#"{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"write_document"}}"#,
        )
        .unwrap();
        assert_eq!(reply["error"]["code"], -32602);
    }

    #[test]
    fn tools_list_names_four_tools() {
        let mut fx = fixture(true);
        let reply = call(&mut fx.server, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap();
        let tools = reply["result"]["tools"].as_array().unwrap();
        let names: Vec<_> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            ["search_knowledge", "read_document", "list_documents", "list_projects"]
        );
        for t in tools {
            assert!(t["inputSchema"]["type"] == "object", "schema for {}", t["name"]);
        }
    }

    #[test]
    fn scoped_tools_work_and_ignore_project_arg() {
        let mut fx = fixture(true);
        let (text, is_err) = tool(&mut fx.server, "search_knowledge", json!({"query": "billing cutover"}));
        assert!(!is_err, "{text}");
        assert!(text.contains("notes/meeting.md"), "{text}");
        assert!(text.contains("**billing**"), "{text}");
        assert!(!text.contains("<mark>"), "{text}");

        // A supplied project arg is ignored with a note.
        let (text, is_err) =
            tool(&mut fx.server, "search_knowledge", json!({"query": "billing", "project": "Other"}));
        assert!(!is_err);
        assert!(text.contains("locked to project \"Atlas\""), "{text}");

        let (text, is_err) = tool(&mut fx.server, "read_document", json!({"path": "People.md"}));
        assert!(!is_err);
        assert!(text.contains("Priya Natarajan"), "{text}");

        let (text, is_err) = tool(&mut fx.server, "list_documents", json!({"folder": "notes"}));
        assert!(!is_err);
        assert!(text.contains("notes/meeting.md"), "{text}");
        assert!(!text.contains("People.md"), "{text}");
    }

    #[test]
    fn read_document_refuses_escapes() {
        let mut fx = fixture(true);
        for path in ["../etc/passwd", "/etc/passwd", "notes/../../etc/passwd"] {
            let (text, is_err) = tool(&mut fx.server, "read_document", json!({"path": path}));
            assert!(is_err, "{path} should be refused");
            assert!(text.contains("outside the project"), "{text}");
        }
        // And an unknown-but-safe path is a helpful index miss.
        let (text, is_err) = tool(&mut fx.server, "read_document", json!({"path": "nope.md"}));
        assert!(is_err);
        assert!(text.contains("not in project"), "{text}");
    }

    #[test]
    fn read_document_caps_large_files() {
        let mut fx = fixture(true);
        let big = "é".repeat(150 * 1024); // 300 KB of two-byte chars
        std::fs::write(fx.root.join("big.txt"), &big).unwrap();
        let project = Project::open(&fx.root).unwrap();
        let mut db = Db::open(&fx.server.base_dir, project.config.id).unwrap();
        scan::scan(&project, &mut db).unwrap();
        drop(db);

        let (text, is_err) = tool(&mut fx.server, "read_document", json!({"path": "big.txt"}));
        assert!(!is_err, "{}", &text[..200.min(text.len())]);
        assert!(text.contains("[truncated"), "no truncation note");
        assert!(text.len() < 210 * 1024, "way over cap: {}", text.len());
    }

    #[test]
    fn unscoped_requires_and_matches_project() {
        let mut fx = fixture(false);
        // Missing project → helpful error naming projects.
        let (text, is_err) = tool(&mut fx.server, "search_knowledge", json!({"query": "billing"}));
        assert!(is_err);
        assert!(text.contains("Available projects: Atlas"), "{text}");

        // Unknown project → same courtesy.
        let (text, is_err) =
            tool(&mut fx.server, "search_knowledge", json!({"query": "billing", "project": "Zeus"}));
        assert!(is_err);
        assert!(text.contains("No Ken project matches"), "{text}");

        // Match by name (case-insensitive) and by path.
        let (text, is_err) =
            tool(&mut fx.server, "search_knowledge", json!({"query": "billing", "project": "atlas"}));
        assert!(!is_err, "{text}");
        assert!(text.contains("notes/meeting.md"));
        let root = fx.root.to_string_lossy().to_string();
        let (text, is_err) =
            tool(&mut fx.server, "list_documents", json!({"project": root}));
        assert!(!is_err, "{text}");
        assert!(text.contains("People.md"));

        // list_projects works without any scoping.
        let (text, is_err) = tool(&mut fx.server, "list_projects", json!({}));
        assert!(!is_err);
        assert!(text.contains("Atlas"), "{text}");
    }

    #[test]
    fn missing_required_args_are_tool_errors() {
        let mut fx = fixture(true);
        let (text, is_err) = tool(&mut fx.server, "search_knowledge", json!({}));
        assert!(is_err);
        assert!(text.contains("\"query\" argument is required"), "{text}");
        let (text, is_err) = tool(&mut fx.server, "read_document", json!({}));
        assert!(is_err);
        assert!(text.contains("\"path\" argument is required"), "{text}");
    }

    #[test]
    fn char_boundary_floor_is_safe() {
        let bytes = "aé".as_bytes(); // 61 C3 A9
        assert_eq!(floor_char_boundary_at(bytes, 3), 3);
        assert_eq!(floor_char_boundary_at(bytes, 2), 1);
        assert_eq!(floor_char_boundary_at(bytes, 99), 3);
        assert_eq!(floor_char_boundary_at(b"", 5), 0);
    }
}

//! ken-mcp — stdio MCP server over ken-core. Hand-rolled JSON-RPC 2.0:
//! one JSON object per line on stdout, stdin read line-by-line, nothing
//! but protocol on stdout (diagnostics go to stderr). Read-only on the
//! SQLite index. It writes only through ken-core and only these: Your day
//! tasks, Ken's memories, the workspace journal, and a team inbox item
//! (which commits and pushes to the team's inbox repo).
//!
//! Scoping: `ken-mcp --project <path>` locks every tool to that project;
//! unscoped, the project tools take a required `project` argument matched
//! against Ken's registry.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use uuid::Uuid;

use ken_core::day;
use ken_core::db::Db;
use ken_core::embedder::Embedder;
use ken_core::family::{self, FamilyManifest, InboxKind, InboxTaskPayload, Lane, NewInboxItem};
use ken_core::family_sync::{self, ConnectionState, GitTransport, PendingWrite, SyncEngine, SystemGit};
use ken_core::memory;
use ken_core::profiler::{self, ProjectProfile};
use ken_core::project::Project;
use ken_core::registry::{self, Registry};
use ken_core::routing::{self, MemberHits, MemberInfo, MemberStatus, RouteReason};
use ken_core::search::Source;
use ken_core::settings::AppSettings;
use ken_core::tasks;
use ken_core::workspace_kg_db::WorkspaceKgDb;

/// The protocol revision we implement. Newer revisions we know to be
/// wire-compatible with our subset are echoed back on request.
const PROTOCOL_VERSION: &str = "2024-11-05";
const KNOWN_PROTOCOL_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18"];

/// Cap on `read_document` output.
const MAX_DOCUMENT_BYTES: usize = 200 * 1024;

#[derive(Default)]
struct Server {
    base_dir: PathBuf,
    /// Root the server is locked to (`--project <path>`), if any.
    scoped: Option<PathBuf>,
    /// The embedding model, loaded by the first search that needs it.
    meaning: RefCell<Meaning>,
}

/// Meaning search in this process: not tried yet, the model, or why not.
#[derive(Default)]
enum Meaning {
    #[default]
    Unloaded,
    Ready(Box<dyn Embedder + Send>),
    Off(String),
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
    // The embedding model is found where the app installs it, and runs where
    // the app's graphics-card setting says: a query embeds in about 55 ms on
    // the graphics card and took 0.3 to 5 s on a busy CPU (2026-10-06).
    ken_core::local_llm::init(base_dir.clone());
    let use_gpu = AppSettings::load(&base_dir).extra.get("useGpu").and_then(Value::as_bool);
    ken_core::compute::set_use_gpu(use_gpu.unwrap_or(true));
    let mut server = Server { base_dir, scoped, ..Default::default() };

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut input = stdin.lock();
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match input.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = clean_line(&buf);
        if let Some(reply) = handle_line(&mut server, &line) {
            let mut out = stdout.lock();
            if writeln!(out, "{reply}").and_then(|_| out.flush()).is_err() {
                break; // client hung up
            }
        }
    }
}

/// A request line as text: invalid UTF-8 replaced (then answered as a parse
/// error rather than ending the loop), a leading byte-order mark (Windows
/// PowerShell adds one when it pipes) and the trailing CR/LF dropped.
fn clean_line(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    text.trim_start_matches('\u{feff}').trim_end_matches(['\r', '\n']).to_string()
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
        "tools/list" => rpc_result(&id, json!({ "tools": tool_definitions(server) })),
        "tools/call" => {
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            // Any tool the server lists is dispatched: the list and the
            // dispatch cannot drift apart (open_in_ken once was listed and
            // refused).
            let listed = tool_definitions(server)
                .as_array()
                .is_some_and(|tools| tools.iter().any(|t| t.get("name").and_then(Value::as_str) == Some(name)));
            if listed {
                let outcome = call_tool(server, name, &args);
                rpc_result(&id, tool_content(outcome))
            } else {
                rpc_error(&id, -32602, &format!("unknown tool: {name:?}"))
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

/// kg-routing (task 3.4) adds `kg_search`/`semantic_search`/`route_query` to
/// this list, gated on the `kgRouting` flag (spec: "With kgRouting off
/// these tools SHALL be absent from the tool list"). `list_projects`
/// deliberately does NOT move behind that gate even though design.md names
/// it as one of kg-routing's four new tools — it already existed,
/// unconditionally, before this change (see the final report's
/// "Conflicts"), and removing it when the flag is off would violate task
/// 5.2's "MCP tool list byte-identical to pre-feature behavior". Its
/// *content* still gets flag-scoped enrichment — see `list_projects` below.
fn tool_definitions(server: &Server) -> Value {
    let project_arg = json!({
        "type": "string",
        "description": "Which Ken project to use — a project name or folder \
path from list_projects. Required unless the server was started locked to \
one project."
    });
    let code_project = json!({ "type": "string", "description": "Only this project (by name). Default: every project Ken knows, so uses in other repos are found too." });
    let mut tools = vec![
        json!({
            "name": "find_definition",
            "description": "Where a symbol (function, method, class, struct, interface, module) is defined in the code, \
from Ken's code map (tree-sitter, every indexed repo). Returns each definition's ken:// address with its line, its \
kind and its doc comment. Use it like 'go to definition'; then read_document or Read the file at that line.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "The symbol's name, exactly as written in code (case-insensitive)." },
                    "project": code_project.clone()
                },
                "required": ["name"]
            }
        }),
        json!({
            "name": "find_usages",
            "description": "Every place a symbol is used (called, referenced, instantiated), each with the function or \
class it sits in, so its callers. Like 'find usages': who calls this, what breaks if it changes. Names only, no type \
resolution: two different symbols with one name both show.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "The symbol's name (case-insensitive)." },
                    "limit": { "type": "integer", "description": "Maximum uses to return (default 60)." },
                    "project": code_project.clone()
                },
                "required": ["name"]
            }
        }),
        json!({
            "name": "file_outline",
            "description": "A code file's definitions in order, nested (classes and their methods), each with its line. \
Drill into a file before reading all of it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "The file's path relative to its project root." },
                    "project": code_project.clone()
                },
                "required": ["path"]
            }
        }),
        json!({
            "name": "related_files",
            "description": "A code file's neighbours: the files it imports, the files that import it, and the libraries \
it uses. For how a module fits in and what depends on it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "The file's path relative to its project root." },
                    "project": code_project.clone()
                },
                "required": ["path"]
            }
        }),
        json!({
            "name": "history",
            "description": "When and why something changed (read-only, always current): a file's recent commits (path), the commits whose message mentions some words or whose change added or removed them (query), or with neither, what changed in the last days (what changed this week?): each git repo's commits in that window and, for a folder with no git, the files Ken saw change. Each commit with its date, author, message and the files it touched as ken:// addresses.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "A file's path relative to its project root: its history." },
                    "query": { "type": "string", "description": "Words to find in commit messages and in what changes added or removed." },
                    "days": { "type": "integer", "description": "The window, in days back from today (default 7). Narrows path and query too." },
                    "limit": { "type": "integer", "description": "Maximum commits (default 15)." },
                    "project": code_project.clone()
                }
            }
        }),
        json!({
            "name": "search_knowledge",
            "description": "Whole-file keyword search in ONE Ken project: best for an exact \
name, term or phrase you expect in a file. Files holding every word come first, one line each \
(path, ken:// address and a short excerpt with matched words in **bold**; cheap, about 200 \
characters a hit). When no file has every word it says so and gives the closest passages \
instead (any word, and meaning when Ken's embedding model is installed). For a question, or \
when you don't know the project, use route_query first.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search terms." },
                    "limit": { "type": "integer", "description": "Maximum hits to return (default 20)." },
                    "audience": { "type": "string", "enum": ["any", "business", "dev"], "description": "Who the results are for: business (pages for readers who never see code: Current, Design, Work, or audience: business), dev (everything else but the method pages, code included), or any (default)." },
                    "type": { "type": "string", "description": "Keep only these kinds of file, comma-separated: code, test, spec, doc, config, data, design, meeting, ticket (or any, the default). Every hit is tagged with its kind either way." },
                    "project": project_arg
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": "read_document",
            "description": "Read a document, or just some of its lines, after a search: the \
way to see more of a hit. Give a hit's ken:// address as `path`: with its #L<n> a Markdown page \
gives the whole section n is in (its heading to the next heading of the same level; in a \
decisions log, the entry), up to about 8,000 characters, and a code or text file the lines \
around n (10 before, 40 after), numbered — one cheap call. start_line/end_line pick other \
lines. With no line, the whole file comes back (up to 200 KB, so costly for a big file). Also \
takes a project-relative path (with `project`). Binary formats (docx, xlsx, pptx, pdf, images) \
return the text Ken's indexer extracted.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "A ken:// address from a hit (#L<n> included), or a project-relative file path." },
                    "start_line": { "type": "integer", "description": "First line to return, 1-based. Alone, it returns 50 lines from there." },
                    "end_line": { "type": "integer", "description": "Last line to return, inclusive." },
                    "project": project_arg
                },
                "required": ["path"]
            }
        }),
        json!({
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
        }),
        json!({
            "name": "list_projects",
            "description": "List the Ken projects registered on this machine \
— name, folder path, and whether the folder is currently available. When \
workspace routing (the kgRouting feature flag) is on, each project also \
shows its search readiness and, if it has one, its one-line \
`.ken/index-profile.json` summary.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
    ];

    if kg_routing_enabled(&AppSettings::load(&server.base_dir)) {
        tools.push(json!({
            "name": "kg_search",
            "description": "Search the workspace knowledge graph — entity \
names and summaries merged across every project's own knowledge model — and \
return kg:// ids. Use this to find out which project(s) know about a topic \
before calling semantic_search on one of them directly; for a one-shot \
answer where you don't need to see the entities first, call route_query \
instead. If the federatedKg flag is off, this returns an explanation rather \
than an error — semantic_search and route_query keep working without it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search terms — matched against entity names and summaries, case-insensitive." },
                    "limit": { "type": "integer", "description": "Maximum entities to return (default 50)." }
                },
                "required": ["query"]
            }
        }));
        tools.push(json!({
            "name": "semantic_search",
            "description": "route_query for ONE project you already know (the user named it, \
or list_projects or an earlier hit told you): the same keyword and meaning search, ranked the \
same way, and the same short hits — path:line, ken:// address, kind and about 300 characters \
around the best-matching lines. Call read_document with a hit's address for more. Meaning \
search uses Ken's installed embedding model; without it the answer says it was keyword only.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "A question or search terms." },
                    "limit": { "type": "integer", "description": "Maximum hits to return (default 10)." },
                    "audience": { "type": "string", "enum": ["any", "business", "dev"], "description": "Who the results are for: business (pages for readers who never see code: Current, Design, Work, or audience: business), dev (everything else but the method pages, code included), or any (default)." },
                    "type": { "type": "string", "description": "Keep only these kinds of file, comma-separated: code, test, spec, doc, config, data, design, meeting, ticket (or any, the default). Every hit is tagged with its kind either way." },
                    "project": project_arg
                },
                "required": ["query"]
            }
        }));
        tools.push(json!({
            "name": "route_query",
            "description": "START HERE to find where something is, or what the team knows \
about a question, in plain words. Searches every Ken project that matters (a project the \
query names, else those the workspace knowledge graph points to, else all of them) by \
keyword and by meaning, and returns one merged ranked list. Each hit is short — repo:path:line \
to cite, ken:// address, kind, and about 300 characters around the best-matching lines — so \
ten hits cost about 6,000 characters. For more of a hit call read_document with its ken:// \
address (the #L line included): the lines around it, one cheap call. Meaning search uses \
Ken's installed embedding model; without it the answer says it was keyword only.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "A question or search terms — also matched against project names for direct routing." },
                    "limit": { "type": "integer", "description": "Maximum merged hits to return (default 10)." },
                    "audience": { "type": "string", "enum": ["any", "business", "dev"], "description": "Who the results are for: business (pages for readers who never see code: Current, Design, Work, or audience: business), dev (everything else but the method pages, code included), or any (default)." },
                    "type": { "type": "string", "description": "Keep only these kinds of file, comma-separated: code, test, spec, doc, config, data, design, meeting, ticket (or any, the default). Every hit is tagged with its kind either way." },
                },
                "required": ["query"]
            }
        }));
    }

    // ken-memory (task 3.1) adds `memory_write`/`journal_append`. Ken's
    // memory is built in (`features::BUILT_IN`), so they are always listed.
    {
        tools.push(json!({
            "name": "memory_write",
            "description": "Create or update one of Ken's own long-term \
memory files — durable notes on ways of working, conventions, or standing \
decisions, either workspace-wide or scoped to one project. Distinct from \
the day-to-day journal (use journal_append for that): memories are small, \
curated, and always kept in context, not a running log. `scope` is \
\"workspace\" for a workspace-wide memory, or a Ken project name (from \
list_projects) for a project-scoped one. `slug` names the file \
(<slug>.md) and is the memory's identity. By default this creates a new \
memory and errors if that slug already exists — pass mode \"replace\" to \
update an existing memory's body instead.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "scope": { "type": "string", "description": "\"workspace\", or a Ken project name for a project-scoped memory." },
                    "slug": { "type": "string", "description": "File-name-safe identifier for the memory — creates <slug>.md." },
                    "content": { "type": "string", "description": "The memory's body text." },
                    "mode": {
                        "type": "string",
                        "enum": ["create", "replace"],
                        "description": "\"create\" (default): errors if the slug already exists. \"replace\": updates an existing memory's body (its slug must already exist)."
                    }
                },
                "required": ["scope", "slug", "content"]
            }
        }));
        tools.push(json!({
            "name": "journal_append",
            "description": "Append a timestamped entry to today's Ken \
workspace journal, for a finding the person wants kept, creating the \
day's file if it doesn't exist yet. Cite \
ken:// addresses for anything referenced so the entry stays traceable \
after Ken reindexes it. Optionally tag the entry with the Ken project it \
concerns and free-form tags.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "The journal entry's text." },
                    "project": { "type": "string", "description": "Ken project name this entry concerns, if any." },
                    "tags": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Free-form tags for this entry."
                    }
                },
                "required": ["text"]
            }
        }));
    }

    // Your day: the user's tasks and the team's tickets. Always listed.
    {
        tools.push(json!({
            "name": "task_create",
            "description": "Add a task to the user's Your day list in Ken: a markdown file \
in the workspace's tasks folder. A task is a title and, optionally, a target date, a \
description, a repeat, and links (a ticket id such as ATT-014, a file path, or a repo \
name). Returns the new task's id.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "The task's title." },
                    "target": { "type": "string", "description": "Target date, YYYY-MM-DD." },
                    "description": { "type": "string", "description": "Markdown description (the file's body)." },
                    "repeat": { "type": "string", "description": "daily, weekdays, weekly:mon … weekly:sun, or monthly:<1-31>. Omit for a one-off." },
                    "links": { "type": "array", "items": { "type": "string" }, "description": "Tickets (<repo>/<ID>, or a bare <ID> for that id in any repo), file paths or repo names this task is about." }
                },
                "required": ["title"]
            }
        }));
        tools.push(json!({
            "name": "task_update",
            "description": "Change one of the user's tasks by id (from task_list): its \
title, target date (\"\" clears it), description, repeat (\"\" makes it a one-off), \
links, or state (\"open\" or \"done\"). Only the fields given are written; everything \
else in the file is kept as it is. Marking a task done also writes a one-line entry to \
today's workspace journal. A recurring task marked done is done for today only.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "The task's id, from task_list." },
                    "title": { "type": "string" },
                    "target": { "type": "string", "description": "YYYY-MM-DD, or \"\" to clear." },
                    "description": { "type": "string", "description": "Replaces the description." },
                    "repeat": { "type": "string", "description": "daily, weekdays, weekly:<mon..sun>, monthly:<1-31>, or \"\" for none." },
                    "links": { "type": "array", "items": { "type": "string" }, "description": "Replaces the links." },
                    "state": { "type": "string", "enum": ["open", "done"] },
                    "as": { "type": "string", "description": "Only for a task on a family board when this device's member identity for that family isn't configured: your member id (from family_list)." }
                },
                "required": ["id"]
            }
        }));
        tools.push(json!({
            "name": "task_list",
            "description": "List the user's tasks (Your day's Other list): open ones by \
default. Each line shows the id (for task_update), the title, the target date, the \
repeat and the links. Filter by state, by a target date before a day, or by a linked \
ticket id.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "state": { "type": "string", "enum": ["open", "done", "all"], "description": "Default open." },
                    "target_before": { "type": "string", "description": "Only tasks with a target before this date, YYYY-MM-DD." },
                    "linked": { "type": "string", "description": "Only tasks linked to this ticket: <repo>/<ID>, or a bare <ID> for that id in any repo." }
                }
            }
        }));
        tools.push(json!({
            "name": "ticket_list",
            "description": "List the ticket files (tickets/<ID>.md) in the workspace's \
repos: the ones assigned to the user (matched against git's user.name and user.email) \
and still open, by default. Each line shows the id, title, state, repo, target date and \
how many of the user's tasks link to it. Read-only: tickets are edited as files.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "assignee": { "type": "string", "enum": ["me", "all"], "description": "Default me." },
                    "state": { "type": "string", "enum": ["open", "all"], "description": "Default open." }
                }
            }
        }));
    }

    // The team inbox (a family repo): always listed.
    {
        tools.push(json!({
            "name": "family_list",
            "description": "List this device's Ken family connections — \
each connection's name and id, its members, which member you are (if \
configured), and its local sync state (commits ahead/behind the remote, \
whether the working tree is dirty, whether a rebase is stuck). Call this \
first to get the family/member ids family_inbox and family_send need.",
            "inputSchema": { "type": "object", "properties": {} }
        }));
        tools.push(json!({
            "name": "family_inbox",
            "description": "List items in your own inbox for one Ken \
family connection (or every connection this device has a known identity \
for, if `family` is omitted) — each item's id, kind (task/message/\
notification), status (unread/seen/accepted/archived), sender, and title. \
Read-only: calling this never changes an item's status.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "family": { "type": "string", "description": "Family name or id, from family_list. Omitted: every family connection with a known identity." },
                    "as": { "type": "string", "description": "Override which family member's inbox to read, when this device's identity for the family isn't already configured — a member id from family_list." }
                }
            }
        }));
        tools.push(json!({
            "name": "family_send",
            "description": "Deliver a task, message, or notification into \
a teammate's inbox in a Ken family repo: creates exactly one new file \
under their members/<id>/inbox/ and pushes it — the only cross-member \
write Ken's write lanes allow (never an edit of anything the recipient \
already owns). Delivery is not assignment or acceptance, and there is no \
auto-accept: a task item becomes a Your \
day task only when the recipient accepts it in their Inbox; a message \
waits, unread, until they read it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "family": { "type": "string", "description": "Family name or id, from family_list." },
                    "to": { "type": "string", "description": "Recipient member id, from family_list." },
                    "kind": { "type": "string", "enum": ["task", "message", "notification"] },
                    "title": { "type": "string", "description": "Short title/subject line." },
                    "body": { "type": "string", "description": "Free-form markdown body — the message text, or task context/brief." },
                    "task": {
                        "type": "object",
                        "description": "Only meaningful when \"kind\" is \"task\" — the brief the recipient's accept flow copies onto a fresh board task. Defaults to \"title\"/kind \"human\" when omitted on a task item.",
                        "properties": {
                            "title": { "type": "string" },
                            "project": { "type": "string" },
                            "tags": { "type": "array", "items": { "type": "string" } },
                            "due": { "type": "string", "description": "Due date, e.g. YYYY-MM-DD." },
                            "kind": { "type": "string", "enum": ["human", "ai"] }
                        }
                    },
                    "as": { "type": "string", "description": "Override which family member is sending, when this device's identity for the family isn't already configured — a member id from family_list." }
                },
                "required": ["family", "to", "kind", "title"]
            }
        }));
    }

    // Only when Ken's own chat started this server: it can reach the app.
    if app_bridge().is_some() {
        tools.push(json!({
            "name": "open_in_ken",
            "description": "Open a file in Ken for the person, at a line or a heading. Use this ONLY when the \
person explicitly asks you to open, show or take them to something; otherwise cite the file as a link and let \
them click it. Accepts a ken:// address from Ken's search tools, or a project-relative path.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": { "type": "string", "description": "A ken://<project-id>/<path> address, or a project-relative path." },
                    "line": { "type": "integer", "description": "1-based line to scroll to." },
                    "heading": { "type": "string", "description": "A heading's text or slug to scroll to, in a Markdown page." },
                    "project": project_arg
                },
                "required": ["target"]
            }
        }));
    }

    Value::Array(tools)
}

/// Ken's app, when its chat started this server: the URL and token its
/// local listener checks.
fn app_bridge() -> Option<(String, String)> {
    let url = std::env::var("KEN_APP_URL").ok().filter(|u| !u.is_empty())?;
    let token = std::env::var("KEN_APP_TOKEN").ok().filter(|t| !t.is_empty())?;
    Some((url, token))
}

/// Ask Ken to open a file: one POST to its local listener.
fn open_in_ken(server: &Server, args: &Value) -> Result<String, String> {
    let bridge = app_bridge().ok_or("open_in_ken works only in Ken's own chat")?;
    open_in_ken_via(server, args, &bridge)
}

fn open_in_ken_via(server: &Server, args: &Value, (url, token): &(String, String)) -> Result<String, String> {
    use std::io::{Read, Write};
    let target = require_str(args, "target")?;
    let (project_id, path) = match target.strip_prefix("ken://").and_then(|r| r.split_once('/')) {
        Some((id, path)) => (id.to_string(), path.to_string()),
        None => {
            let (project, _) = resolve_project(server, args)?;
            (project.config.id.to_string(), target.trim_start_matches("./").replace('\\', "/"))
        }
    };
    let (path, frag_line) = match path.split_once("#L") {
        Some((p, l)) => (p.to_string(), l.split('-').next().and_then(|n| n.parse::<u64>().ok())),
        None => (path, None),
    };
    let line = args.get("line").and_then(Value::as_u64).or(frag_line);
    let anchor = args.get("heading").and_then(Value::as_str).map(str::to_string);
    let body = json!({ "projectId": project_id, "path": path, "line": line, "anchor": anchor }).to_string();
    let rest = url.strip_prefix("http://").ok_or("bad app URL")?;
    let (host, route) = rest.split_once('/').map(|(h, r)| (h, format!("/{r}"))).unwrap_or((rest, "/".into()));
    let mut stream = std::net::TcpStream::connect(host).map_err(|e| format!("Ken is not reachable: {e}"))?;
    let req = format!(
        "POST {route} HTTP/1.1\r\nHost: {host}\r\nX-Ken-Token: {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut reply = String::new();
    let _ = stream.read_to_string(&mut reply);
    if reply.starts_with("HTTP/1.1 200") {
        Ok(format!("Opened {path}{} in Ken.", line.map(|l| format!(" at line {l}")).unwrap_or_default()))
    } else {
        Err(format!("Ken did not open it: {}", reply.lines().next().unwrap_or("no reply")))
    }
}

// --- tools ---

fn call_tool(server: &Server, name: &str, args: &Value) -> Result<String, String> {
    match name {
        "list_projects" => list_projects(server),
        "kg_search" => kg_search(server, args),
        "semantic_search" => semantic_search(server, args),
        "route_query" => route_query(server, args),
        "find_definition" => find_definition(server, args),
        "find_usages" => find_usages(server, args),
        "file_outline" => file_outline(server, args),
        "related_files" => related_files(server, args),
        "history" => history(server, args),
        "open_in_ken" => open_in_ken(server, args),
        "memory_write" => memory_write_tool(server, args),
        "journal_append" => journal_append_tool(server, args),
        "task_create" => task_create_tool(server, args),
        "task_list" => task_list_tool(server, args),
        "task_update" => task_update_tool(server, args),
        "ticket_list" => ticket_list_tool(server, args),
        "family_list" => family_list_tool(server),
        "family_inbox" => family_inbox_tool(server, args),
        "family_send" => family_send_tool(server, args),
        "search_knowledge" => {
            let query = require_str(args, "query")?;
            let limit = args
                .get("limit")
                .and_then(|l| l.as_u64())
                .map(|l| l.clamp(1, 200) as usize)
                .unwrap_or(20);
            let (project, note) = resolve_project(server, args)?;
            let db = open_index(server, &project)?;
            let audience = audience_arg(args);
            let types = types_arg(args);
            let mut hits = db
                .search(&query, fetch_for(audience.as_deref(), &types, limit))
                .map_err(|e| format!("search failed: {e}"))?;
            hits.retain(|h| {
                ken_core::contenttype::is_wanted(&h.rel_path, &types)
                    && ken_core::pagemeta::suits(audience.as_deref(), ken_core::pagemeta::audience_at(&db, &h.rel_path))
            });
            hits.truncate(limit);
            let mut out = note.unwrap_or_default();
            if hits.is_empty() {
                out.push_str(&closest_passages(server, &db, &project, &query, limit, audience.as_deref(), &types)?);
            } else {
                out.push_str(&format!(
                    "{} result{} for {query:?} in project \"{}\":\n",
                    hits.len(),
                    if hits.len() == 1 { "" } else { "s" },
                    project.config.name
                ));
                for (i, hit) in hits.iter().enumerate() {
                    let snippet = hit.snippet.replace("<mark>", "**").replace("</mark>", "**");
                    let label = ken_core::authority::hit_label(&db, &hit.rel_path, None).ok().flatten();
                    out.push_str(&format!(
                        "\n{}. {} {} ({}){} — {}",
                        i + 1,
                        kind_tag(&hit.rel_path),
                        hit.rel_path,
                        ken_address(project.config.id, &hit.rel_path),
                        label.map(|l| format!(" ({l})")).unwrap_or_default(),
                        snippet
                    ));
                }
            }
            Ok(out)
        }
        "read_document" => {
            let path = require_str(args, "path")?;
            // A ken://<project id>/<path>#L12 address, as search returns it,
            // names its repo, its file and the line to read around.
            let mut named = args.clone();
            let path = match path.strip_prefix("ken://").and_then(|r| r.split_once('/')) {
                Some((id, rel)) => {
                    named["project"] = json!(id);
                    rel.replace("%20", " ")
                }
                None => path,
            };
            let (path, fragment) = match path.split_once('#') {
                Some((file, frag)) => (file.to_string(), Some(frag.to_string())),
                None => (path, None),
            };
            let range = line_range(args, fragment.as_deref());
            let pointed = address_line(args, fragment.as_deref());
            let (project, note) = resolve_project(server, &named)?;
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
            match (row.kind.as_str(), range) {
                // A page read from a hit's address: the section it is in.
                ("md", Some(_)) if pointed.is_some() => {
                    let bytes = std::fs::read(&abs)
                        .map_err(|e| format!("could not read {path:?}: {e}"))?;
                    out.push_str(&section_text(&db, &path, &String::from_utf8_lossy(&bytes), pointed.unwrap_or(1)));
                }
                ("md" | "txt" | "code", Some(r)) => {
                    let bytes = std::fs::read(&abs)
                        .map_err(|e| format!("could not read {path:?}: {e}"))?;
                    out.push_str(&numbered_lines(&String::from_utf8_lossy(&bytes), r));
                }
                ("md" | "txt" | "code", None) => {
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
                (kind, _) => {
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
                    let text = match range {
                        Some(r) => numbered_lines(&text, r),
                        None => text,
                    };
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

/// The lines read_document returns, 1-based and inclusive: `start_line` and
/// `end_line`, or the address's `#L88` (around it) or `#L88-L120`; None
/// reads the whole file.
fn line_range(args: &Value, fragment: Option<&str>) -> Option<(i64, i64)> {
    let arg = |k: &str| args.get(k).and_then(Value::as_i64).filter(|n| *n >= 1);
    let span = READ_BEFORE + READ_AFTER;
    match (arg("start_line"), arg("end_line")) {
        (Some(a), Some(b)) => return Some((a, b.max(a))),
        (Some(a), None) => return Some((a, a + span)),
        (None, Some(b)) => return Some(((b - span).max(1), b)),
        (None, None) => {}
    }
    let frag = fragment?.strip_prefix('L')?;
    let num = |s: &str| s.trim_start_matches('L').parse::<i64>().ok().filter(|n| *n >= 1);
    match frag.split_once('-') {
        Some((a, b)) => {
            let a = num(a)?;
            Some((a, num(b).unwrap_or(a).max(a)))
        }
        None => {
            let n = num(frag)?;
            Some(((n - READ_BEFORE).max(1), n + READ_AFTER))
        }
    }
}

/// The line an address points at (`#L88`), when nothing else picks the
/// lines: no `start_line`/`end_line`, and not a range (`#L88-L120`).
fn address_line(args: &Value, fragment: Option<&str>) -> Option<i64> {
    if args.get("start_line").is_some_and(|v| !v.is_null()) || args.get("end_line").is_some_and(|v| !v.is_null()) {
        return None;
    }
    let frag = fragment?.strip_prefix('L')?;
    if frag.contains('-') {
        return None;
    }
    frag.parse::<i64>().ok().filter(|n| *n >= 1)
}

/// About this many characters of a section read_document returns from an
/// address. Partly-right answers missed a fact in the same section as the
/// hit, just past the 50 lines an address used to give (Shattered Realms,
/// 2026-10-07); a decisions log's 50 lines were 24,000 characters of
/// other rulings.
const SECTION_CHARS: usize = 8_000;

/// The section of a page (or the log entry) that `line` is in, numbered,
/// cut at [`SECTION_CHARS`] with the cut said and where to read on. An entry
/// a later one supersedes (`supersede`) says so under it, with the line to
/// read.
fn section_text(db: &Db, path: &str, text: &str, line: i64) -> String {
    let Some((a, b, name)) = ken_core::chunker::section_at(path, text, line.max(1) as usize) else {
        return numbered_lines(text, ((line - READ_BEFORE).max(1), line + READ_AFTER));
    };
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    let what = match &name {
        Some(n) if n.starts_with('#') => format!("section {:?}", n.trim_start_matches('#').trim()),
        Some(id) => format!("entry {id}"),
        None => "section".to_string(),
    };
    let row = |n: usize| format!("{n}: {}\n", lines[n - 1]);
    let cost = |n: usize| row(n).chars().count();
    let size: usize = (a..=b).map(cost).sum();
    let (from, to) = if size <= SECTION_CHARS {
        (a, b)
    } else {
        // From the section's start when the line is near it, else from a
        // little above the line; as far as the cap allows, the line always.
        let line = (line as usize).clamp(a, b);
        let lead: usize = (a..=line).map(cost).sum();
        let from = if lead <= SECTION_CHARS / 2 { a } else { line.saturating_sub(READ_BEFORE as usize).max(a) };
        let (mut to, mut used) = (from, cost(from));
        while to < b && (to < line || used + cost(to + 1) <= SECTION_CHARS) {
            to += 1;
            used += cost(to);
        }
        (from, to)
    };
    let mut out = match (from, to) == (a, b) {
        true => format!("[{what}: lines {a}-{b} of {total}]\n"),
        false => format!(
            "[{what}: lines {a}-{b} of {total}; lines {from}-{to} shown, cut at {} characters: read on with start_line={}]\n",
            SECTION_CHARS,
            to + 1
        ),
    };
    for n in from..=to {
        out.push_str(&row(n));
    }
    if let Some(id) = name.as_deref().filter(|n| !n.starts_with('#')) {
        let on = |p: &str| if p.is_empty() { String::new() } else { format!(" on {p}") };
        let (by, of) = db.supersessions_of(path, id).unwrap_or_default();
        for s in by {
            out.push_str(&format!("[{} (line {}) {} this entry{}]\n", s.later, s.later_line, s.relation, on(&s.point)));
        }
        for s in of {
            out.push_str(&format!("[this entry {} {} (line {}){}]\n", s.relation, s.earlier, s.earlier_line, on(&s.point)));
        }
    }
    out
}

/// Lines `a` to `b` of `text`, each led by its number, under a line saying
/// which of how many they are.
fn numbered_lines(text: &str, (a, b): (i64, i64)) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len() as i64;
    if a > total {
        return format!("[the file has {total} lines; there is no line {a}]");
    }
    let b = b.min(total);
    let mut out = format!("[lines {a}-{b} of {total}]\n");
    for n in a..=b {
        out.push_str(&format!("{n}: {}\n", lines[(n - 1) as usize]));
    }
    if out.len() > MAX_DOCUMENT_BYTES {
        out.truncate(floor_char_boundary_at(out.as_bytes(), MAX_DOCUMENT_BYTES));
        out.push_str("\n\n[truncated — the lines exceed the 200 KB read limit]");
    }
    out
}

/// search_knowledge when no file holds every word: the passages
/// semantic_search would give (any word, ranked by how many match, and by
/// meaning when the model is installed), one per file, in search_knowledge's
/// own line shape. All-words found the right file for 1 of 45 natural
/// questions (Shattered Realms eval, 2026-10-06).
fn closest_passages(
    server: &Server,
    db: &Db,
    project: &Project,
    query: &str,
    limit: usize,
    audience: Option<&str>,
    types: &[ken_core::contenttype::ContentType],
) -> Result<String, String> {
    let qv = query_vector(server, query);
    let mut hits = routing::search_member_of(db, query, vector_for(db, &qv), fetch_for(audience, types, limit) * 2, types)
        .map_err(|e| format!("search failed: {e}"))?;
    hits.retain(|h| ken_core::pagemeta::suits(audience, h.page.as_ref().and_then(|p| p.audience)));
    let mut seen = std::collections::HashSet::new();
    hits.retain(|h| seen.insert(h.path.clone()));
    hits.truncate(limit);
    let name = &project.config.name;
    if hits.is_empty() {
        return Ok(format!("No matches for {query:?} in project \"{name}\". {LOOK_YOURSELF}"));
    }
    let mut out = format!(
        "No file in project \"{name}\" has every word of {query:?}; the {} closest passage{} instead (any word{}):\n",
        hits.len(),
        if hits.len() == 1 { "" } else { "s" },
        if qv.is_ok() { ", and meaning" } else { "" },
    );
    out.push_str(&keyword_only_note(&qv));
    for (i, hit) in hits.iter().enumerate() {
        let (snippet, line) = short_hit(db, Some(&project.root), query, &hit.path, hit.chunk_id, &hit.snippet, hit.line, EXCERPT_CHARS);
        out.push_str(&format!(
            "\n{}. {} {} ({}{}){} — {}",
            i + 1,
            kind_tag(&hit.path),
            hit.path,
            ken_address(project.config.id, &hit.path),
            line.map(|l| format!("#L{l}")).unwrap_or_default(),
            hit.page.as_ref().and_then(page_note).map(|n| format!(" ({n})")).unwrap_or_default(),
            snippet.split_whitespace().collect::<Vec<_>>().join(" "),
        ));
    }
    Ok(out)
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
    let inside = in_open_workspace(server, &registry);
    let statuses: Vec<_> = registry.statuses().into_iter().filter(|s| inside(&s.entry.path)).collect();
    if statuses.is_empty() {
        return Ok("No Ken projects registered on this machine yet — open a \
folder in the Ken app first."
            .to_string());
    }
    // Readiness + profile enrichment (task 3.1) is gated on `kgRouting` so a
    // flag-off caller's output is byte-identical to this tool's
    // pre-kg-routing behavior (spec 5.2) — see `tool_definitions`'s doc
    // comment on why `list_projects` itself stays unconditionally present
    // rather than moving behind the flag like the other three tools.
    let enrich = kg_routing_enabled(&AppSettings::load(&server.base_dir));
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
        if enrich && s.available {
            let readiness = match Db::open_read_only(&server.base_dir, s.entry.id) {
                Ok(db) if db.vector_count().unwrap_or(0) > 0 => "keyword and meaning search ready",
                Ok(_) => "keyword search ready",
                Err(_) => "no index yet",
            };
            out.push_str(&format!(" — {readiness}"));
            if profiler::profile_path(&s.entry.path).is_file() {
                let summary = ProjectProfile::load(&s.entry.path).summary;
                let summary = summary.trim();
                if !summary.is_empty() {
                    out.push_str(&format!(" — \"{summary}\""));
                }
            }
        }
    }
    Ok(out)
}

/// Search the workspace knowledge graph (task 3.2). Delegates to the same
/// `WorkspaceKgDb` reads src-tauri's `workspace_kg_search` command uses
/// (D4: "delegate, never reimplement" as far as this layer allows — the
/// name/summary substring match + name-first sort is duplicated from that
/// command's body rather than shared, since no reusable ken-core function
/// for it exists to call instead; see the final report).
fn kg_search(server: &Server, args: &Value) -> Result<String, String> {
    let query = require_str(args, "query")?;
    let limit = args
        .get("limit")
        .and_then(|l| l.as_u64())
        .map(|l| l.clamp(1, 200) as usize)
        .unwrap_or(50);
    let app_settings = AppSettings::load(&server.base_dir);
    if !kg_routing_enabled(&app_settings) {
        return Err("kg_search requires the kgRouting feature flag, which is off.".into());
    }
    if !federated_kg_enabled(&app_settings) {
        return Ok("The workspace knowledge graph is off (enable the \
\"workspace\" and \"federatedKg\" feature flags in Ken's settings) — \
kg_search has nothing to search yet. semantic_search and route_query still \
work without it (route_query just skips straight to broadcasting)."
            .to_string());
    }
    let kg = WorkspaceKgDb::open(&kg_root(server))
        .map_err(|e| format!("could not open the workspace knowledge graph: {e}"))?;
    let q = query.trim().to_lowercase();
    let mut hits: Vec<_> = kg
        .list_global_entities()
        .map_err(|e| format!("knowledge graph read failed: {e}"))?
        .into_iter()
        .filter(|e| e.name.to_lowercase().contains(&q) || e.summary.to_lowercase().contains(&q))
        .collect();
    // Name matches rank above summary-only matches; stable by id within each
    // tier — same ranking as the src-tauri command this mirrors.
    hits.sort_by(|a, b| {
        let a_name = a.name.to_lowercase().contains(&q);
        let b_name = b.name.to_lowercase().contains(&q);
        b_name.cmp(&a_name).then(a.id.cmp(&b.id))
    });
    hits.truncate(limit);
    if hits.is_empty() {
        return Ok(format!("No workspace knowledge-graph entities match {query:?}."));
    }
    let mut out = format!(
        "{} entit{} match {query:?}:\n",
        hits.len(),
        if hits.len() == 1 { "y" } else { "ies" }
    );
    for h in hits {
        out.push_str(&format!("\nkg://{} — {} ({}): {}", h.id, h.name, h.kind, h.summary));
    }
    Ok(out)
}

/// Single-member hybrid search (task 3.3): `routing::search_member_of`, the
/// same keyword + meaning composition src-tauri's `hybrid_search` command
/// uses, with the question embedded by the app's own model when it is
/// installed ([`query_vector`]) and keyword only, said in one line, when not.
fn semantic_search(server: &Server, args: &Value) -> Result<String, String> {
    let query = require_str(args, "query")?;
    let limit = args
        .get("limit")
        .and_then(|l| l.as_u64())
        .map(|l| l.clamp(1, 200) as usize)
        .unwrap_or(ROUTE_LIMIT);
    if !kg_routing_enabled(&AppSettings::load(&server.base_dir)) {
        return Err("semantic_search requires the kgRouting feature flag, which is off. Use search_knowledge instead."
            .into());
    }
    let (project, note) = resolve_project(server, args)?;
    let db = open_index(server, &project)?;
    let audience = audience_arg(args);
    let types = types_arg(args);
    let qv = query_vector(server, &query);
    let mut hits = routing::search_member_of(&db, &query, vector_for(&db, &qv), fetch_for(audience.as_deref(), &types, limit), &types)
        .map_err(|e| format!("search failed: {e}"))?;
    hits.retain(|h| ken_core::pagemeta::suits(audience.as_deref(), h.page.as_ref().and_then(|p| p.audience)));
    hits.truncate(limit);
    let mut out = note.unwrap_or_default();
    out.push_str(&keyword_only_note(&qv));
    if hits.is_empty() {
        out.push_str(&format!(
            "No matches for {query:?} in project \"{}\". {LOOK_YOURSELF}",
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
            let (snippet, line) = short_hit(&db, Some(&project.root), &query, &hit.path, hit.chunk_id, &hit.snippet, hit.line, SNIPPET_CHARS);
            out.push_str(&format!(
                "\n{}. {} {}{} ({}{}) [{}]",
                i + 1,
                kind_tag(&hit.path),
                hit.path,
                line.map(|l| format!(":{l}")).unwrap_or_default(),
                ken_address(project.config.id, &hit.path),
                line.map(|l| format!("#L{l}")).unwrap_or_default(),
                source_label(hit.source),
            ));
            if let Some(note) = hit.page.as_ref().and_then(page_note) {
                out.push_str(&format!(" ({note})"));
            }
            out.push_str(&indented(&snippet));
        }
    }
    Ok(out)
}

/// "When and why did this change": a file's commits, or commits whose message
/// or change matches words, asked of git directly (read-only), so it is as
/// current as the repo.
fn history(server: &Server, args: &Value) -> Result<String, String> {
    let path = args.get("path").and_then(|p| p.as_str()).map(|p| p.replace('\\', "/")).filter(|p| !p.is_empty());
    let query = args.get("query").and_then(|q| q.as_str()).map(str::trim).filter(|q| !q.is_empty()).map(str::to_string);
    // With neither a file nor words, the question is "what changed lately".
    let recent = path.is_none() && query.is_none();
    let days = args.get("days").and_then(|d| d.as_u64()).map(|d| d.clamp(1, 3650)).or(recent.then_some(7));
    let limit = args.get("limit").and_then(|l| l.as_u64()).map(|l| l.clamp(1, 100) as usize).unwrap_or(15);
    let roots: Vec<(Uuid, String, PathBuf)> = match args.get("project").and_then(|p| p.as_str()).map(str::trim).filter(|p| !p.is_empty()) {
        Some(name) => {
            let (id, root, pname) = resolve_project_by_name(server, name)?;
            vec![(id, pname, root)]
        }
        None => {
            let registry = Registry::load(&server.base_dir).map_err(|e| format!("could not read Ken's project registry: {e}"))?;
            let inside = in_open_workspace(server, &registry);
            registry
                .projects
                .iter()
                .filter(|e| e.path.is_dir() && inside(&e.path))
                // A folder without git still has recent changes, from the index.
                .filter(|e| recent || e.path.join(".git").exists())
                .filter(|e| path.as_ref().is_none_or(|p| e.path.join(p).exists()))
                .map(|e| (e.id, e.name.clone(), e.path.clone()))
                .collect()
        }
    };
    let fmt = "--format=%x1e%h%x1f%ad%x1f%an%x1f%s";
    let n = format!("-n{limit}");
    let since = days.map(|d| format!("--since={d}.days.ago"));
    let mut out = String::new();
    let mut count = 0;
    let mut changed_files = 0;
    for (id, project, root) in &roots {
        if recent && !root.join(".git").exists() {
            // No git history: the files Ken saw change, newest first.
            let Ok(db) = Db::open_read_only(&server.base_dir, *id) else { continue };
            let from = now_secs() - days.unwrap_or(7) as i64 * 86_400;
            let mut files: Vec<(String, i64)> =
                db.list_files().unwrap_or_default().into_iter().filter(|f| f.mtime >= from).map(|f| (f.rel_path, f.mtime)).collect();
            if files.is_empty() {
                continue;
            }
            files.sort_by(|a, b| b.1.cmp(&a.1));
            out.push_str(&format!("\n\n{project} has no git history; {} file(s) changed on disk (from Ken's index):", files.len()));
            for (rel, mtime) in files.iter().take(limit) {
                out.push_str(&format!("\n  {} {}", day_of(*mtime), ken_address(*id, rel)));
            }
            changed_files += files.len();
            continue;
        }
        let mut runs: Vec<Vec<String>> = Vec::new();
        let base = |extra: &[&str]| -> Vec<String> {
            let mut v: Vec<String> = vec!["log".into(), n.clone(), "--date=short".into(), fmt.into()];
            v.extend(since.iter().cloned());
            v.extend(extra.iter().map(|s| s.to_string()));
            v
        };
        match (&path, &query) {
            (Some(p), _) => runs.push(base(&["--follow", "--", p])),
            (None, Some(q)) => {
                runs.push(base(&["--name-only", "-i", &format!("--grep={q}")]));
                runs.push(base(&["--name-only", &format!("-S{q}")]));
            }
            (None, None) => runs.push(base(&["--name-only"])),
        }
        let mut seen: Vec<String> = Vec::new();
        for run in runs {
            let mut cmd = std::process::Command::new("git");
            let got = ken_core::proc::quiet(&mut cmd)
                .args(&run)
                .current_dir(root)
                .env("GIT_TERMINAL_PROMPT", "0")
                .output();
            let Ok(got) = got else { continue };
            if !got.status.success() {
                continue;
            }
            for record in String::from_utf8_lossy(&got.stdout).split('\u{1e}').filter(|r| !r.trim().is_empty()) {
                let mut lines = record.lines();
                let head: Vec<&str> = lines.next().unwrap_or("").split('\u{1f}').collect();
                let [sha, date, author, subject] = head[..] else { continue };
                if seen.iter().any(|s| s == sha) || count >= limit {
                    continue;
                }
                seen.push(sha.to_string());
                count += 1;
                out.push_str(&format!("\n{count}. {sha} {date} {author} — {subject} ({project})"));
                let files: Vec<&str> = lines.map(str::trim).filter(|l| !l.is_empty()).take(6).collect();
                // A path from an older commit may have moved or gone since:
                // an address only for what is there now.
                for f in files {
                    if root.join(f).exists() {
                        out.push_str(&format!("\n     {}", ken_address(*id, f)));
                    } else {
                        out.push_str(&format!("\n     {f} (its path then; not there now)"));
                    }
                }
            }
        }
    }
    let window = days.map(|d| format!(" in the last {d} day{}", if d == 1 { "" } else { "s" })).unwrap_or_default();
    if count == 0 && changed_files == 0 {
        return Ok(format!("Nothing changed{window} in {} repo(s). {LOOK_YOURSELF}", roots.len()));
    }
    let what = match (&path, &query) {
        (Some(p), _) => format!("history of {p}{window}"),
        (_, Some(q)) => format!("commits about {q:?} (message or change){window}"),
        _ => format!("what changed{window}"),
    };
    Ok(format!("{count} commit{} — {what}:{out}", if count == 1 { "" } else { "s" }))
}

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// `YYYY-MM-DD` (UTC) of a Unix time, without a date crate (Hinnant's
/// civil-from-days).
fn day_of(secs: i64) -> String {
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// The indexes a code question reads: the named project, or every project
/// Ken knows (uses cross repos).
fn code_indexes(server: &Server, args: &Value) -> Result<Vec<(Uuid, String, Db)>, String> {
    if let Some(name) = args.get("project").and_then(|p| p.as_str()).map(str::trim).filter(|p| !p.is_empty()) {
        let (id, _, pname) = resolve_project_by_name(server, name)?;
        let db = Db::open_read_only(&server.base_dir, id).map_err(|e| format!("project \"{pname}\" has no index: {e}"))?;
        return Ok(vec![(id, pname, db)]);
    }
    let registry = Registry::load(&server.base_dir).map_err(|e| format!("could not read Ken's project registry: {e}"))?;
    let inside = in_open_workspace(server, &registry);
    Ok(registry
        .projects
        .iter()
        .filter(|e| e.path.is_dir() && inside(&e.path))
        .filter_map(|e| Db::open_read_only(&server.base_dir, e.id).ok().map(|db| (e.id, e.name.clone(), db)))
        .collect())
}

fn find_definition(server: &Server, args: &Value) -> Result<String, String> {
    let name = require_str(args, "name")?;
    let indexes = code_indexes(server, args)?;
    let mut out = String::new();
    let mut n = 0;
    for (id, project, db) in &indexes {
        for d in ken_core::codemap::definitions(db, &name).map_err(|e| e.to_string())? {
            n += 1;
            let docs = d.docs.as_deref().map(|t| format!(" — {}", t.lines().next().unwrap_or("").trim())).unwrap_or_default();
            out.push_str(&format!(
                "\n{n}. {} {} — {}#L{} (lines {}-{}, {project}){docs}",
                d.kind,
                d.name,
                ken_address(*id, &d.path),
                d.line,
                d.line,
                d.end_line
            ));
        }
    }
    if n == 0 {
        return Ok(format!("No definition of {name:?} in the code map of {} project(s). {LOOK_YOURSELF}", indexes.len()));
    }
    Ok(format!("{n} definition{} of {name:?}:{out}", if n == 1 { "" } else { "s" }))
}

fn find_usages(server: &Server, args: &Value) -> Result<String, String> {
    let name = require_str(args, "name")?;
    let limit = args.get("limit").and_then(|l| l.as_u64()).map(|l| l.clamp(1, 500) as usize).unwrap_or(60);
    let indexes = code_indexes(server, args)?;
    let mut lines = Vec::new();
    let mut mention_lines = Vec::new();
    let mut callers: Vec<String> = Vec::new();
    let mut files: std::collections::BTreeSet<String> = Default::default();
    for (id, project, db) in &indexes {
        let calls = ken_core::codemap::usages(db, &name, limit).map_err(|e| e.to_string())?;
        let known: Vec<(String, i64)> = calls.iter().map(|u| (u.path.clone(), u.line)).collect();
        for u in &calls {
            if lines.len() >= limit {
                break;
            }
            files.insert(format!("{project}/{}", u.path));
            if let Some(w) = &u.within {
                if !callers.contains(w) {
                    callers.push(w.clone());
                }
            }
            let within = u.within.as_deref().map(|w| format!(" in {w}")).unwrap_or_default();
            lines.push(format!("{}#L{} ({}{within}, {project})", ken_address(*id, &u.path), u.line, u.kind));
        }
        // Uses the grammar does not count as calls: passed as a value, named
        // in a type, a decorator, an export.
        for m in ken_core::codemap::mentions(db, &name, &known, limit).map_err(|e| e.to_string())? {
            if mention_lines.len() >= limit {
                break;
            }
            files.insert(format!("{project}/{}", m.path));
            let within = m.within.as_deref().map(|w| format!(" in {w}")).unwrap_or_default();
            mention_lines.push(format!("{}#L{} (referenced{within}, {project})", ken_address(*id, &m.path), m.line));
        }
    }
    let calls_only = lines.len();
    lines.extend(mention_lines);
    if lines.is_empty() {
        return Ok(format!("No uses of {name:?} in the code map of {} project(s). {LOOK_YOURSELF}", indexes.len()));
    }
    let mut out = format!(
        "{} use{} of {name:?} in {} file{} ({calls_only} call{}, {} other reference{})",
        lines.len(),
        if lines.len() == 1 { "" } else { "s" },
        files.len(),
        if files.len() == 1 { "" } else { "s" },
        if calls_only == 1 { "" } else { "s" },
        lines.len() - calls_only,
        if lines.len() - calls_only == 1 { "" } else { "s" },
    );
    if !callers.is_empty() {
        out.push_str(&format!("; called from: {}", callers.join(", ")));
    }
    out.push(':');
    for (i, l) in lines.iter().enumerate() {
        out.push_str(&format!("\n{}. {l}", i + 1));
    }
    Ok(out)
}

/// The index holding `path`: the named project's, or the first that maps it.
fn index_with_file(server: &Server, args: &Value, path: &str) -> Result<(Uuid, String, Db), String> {
    let mut indexes = code_indexes(server, args)?;
    let at = indexes
        .iter()
        .position(|(_, _, db)| db.code_symbols(None, Some(path), None, 1).is_ok_and(|s| !s.is_empty()))
        .ok_or_else(|| format!("No code map for {path:?}: not an indexed code file in a mapped language (Rust, Python, JavaScript, TypeScript, Go, Java, C#). {LOOK_YOURSELF}"))?;
    Ok(indexes.swap_remove(at))
}

fn file_outline(server: &Server, args: &Value) -> Result<String, String> {
    let path = require_str(args, "path")?.replace('\\', "/");
    let (id, project, db) = index_with_file(server, args, &path)?;
    let outline = ken_core::codemap::outline(&db, &path).map_err(|e| e.to_string())?;
    let mut out = format!("{} ({project}) — {} definitions:", ken_address(id, &path), outline.len());
    for (depth, d) in outline {
        out.push_str(&format!("\n{}{} {} — L{}-{}", "  ".repeat(depth), d.kind, d.name, d.line, d.end_line));
    }
    Ok(out)
}

fn related_files(server: &Server, args: &Value) -> Result<String, String> {
    let path = require_str(args, "path")?.replace('\\', "/");
    let (id, project, db) = index_with_file(server, args, &path)?;
    let r = ken_core::codemap::related(&db, &path).map_err(|e| e.to_string())?;
    let list = |files: &[String]| -> String {
        if files.is_empty() {
            " (none)".to_string()
        } else {
            files.iter().map(|f| format!("\n  - {}", ken_address(id, f))).collect()
        }
    };
    Ok(format!(
        "{} ({project})\nImports:{}\nImported by:{}\nLibraries and unresolved: {}",
        ken_address(id, &path),
        list(&r.imports),
        list(&r.imported_by),
        if r.external.is_empty() { "(none)".to_string() } else { r.external.join(", ") }
    ))
}

/// Said with every empty search: the index is not the last word. An agent
/// that reads "no results" as "not there" tells a person something false.
const LOOK_YOURSELF: &str = "Ken's index can miss things (a file not indexed yet, a recent change, other wording), so this \
does not mean it isn't there: look directly with Grep, Glob and Read before saying so, and say where you looked.";

/// The reader a search is for (item 2b): `business`, `dev`, or none for any.
fn audience_arg(args: &Value) -> Option<String> {
    args.get("audience").and_then(|a| a.as_str()).map(str::to_string).filter(|a| !a.is_empty() && a != "any")
}

/// The kinds of file a search keeps (`type`: "code", "spec,doc"); empty keeps all.
fn types_arg(args: &Value) -> Vec<ken_core::contenttype::ContentType> {
    ken_core::contenttype::parse_filter(args.get("type").and_then(|t| t.as_str()))
}

/// Hits to read before filtering by audience or kind, so a filter still
/// fills the page.
fn fetch_for(audience: Option<&str>, types: &[ken_core::contenttype::ContentType], limit: usize) -> usize {
    if !types.is_empty() {
        limit * 5
    } else if audience.is_some() {
        limit * 3
    } else {
        limit
    }
}

/// A hit's kind as its tag in a result line: `[code]`.
fn kind_tag(path: &str) -> String {
    format!("[{}]", ken_core::contenttype::of(path).as_str())
}

// --- short hits and meaning search ---

/// About this many characters of each hit are shown: whole chunks made a
/// route_query answer a median 17,660 characters for 8 hits (Shattered
/// Realms eval, 2026-10-06), and read_document gives the rest on request.
const SNIPPET_CHARS: usize = 300;

/// search_knowledge's fallback excerpts stay one short line like its
/// whole-file hits: at 300 characters its answers grew from a median 1,947
/// to 14,819 characters over five repos (Shattered Realms eval, 2026-10-06).
const EXCERPT_CHARS: usize = 160;

/// Lines read_document shows before and after a single line it is pointed
/// at (`#L88`), so reading around a hit is one call of about 50 lines.
const READ_BEFORE: i64 = 10;
const READ_AFTER: i64 = 40;

/// A query word as matched in a hit's lines: its first two thirds, at least
/// five letters, so "validated" finds "validation".
fn stem(word: &str) -> String {
    let n = word.chars().count();
    let keep = if n > 5 { (n * 2).div_ceil(3).max(5) } else { n };
    word.chars().take(keep).collect()
}

/// The lines of `text` that best match `query`, about `width` characters
/// long, and the index of the best line; `pin` is a line the hit already
/// names (a definition's), shown in place of the best match.
fn snippet_around(text: &str, query: &str, pin: Option<usize>, width: usize) -> (String, usize) {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return (String::new(), 0);
    }
    let stems: Vec<String> = ken_core::db::significant_tokens(query)
        .iter()
        .map(|t| stem(&t.to_lowercase()))
        .filter(|s| s.chars().count() >= 2)
        .collect();
    let score = |l: &str| {
        let l = l.to_lowercase();
        stems.iter().filter(|s| l.contains(s.as_str())).count()
    };
    let best = match pin.filter(|&p| p < lines.len()) {
        Some(p) => p,
        None => {
            let (mut best, mut top) = (None, 0);
            for (i, l) in lines.iter().enumerate() {
                let s = score(l);
                if s > top {
                    (best, top) = (Some(i), s);
                }
            }
            best.unwrap_or_else(|| lines.iter().position(|l| !l.trim().is_empty()).unwrap_or(0))
        }
    };
    // Grow around the best line, the line below first, while it fits.
    let cost = |l: &str| l.trim().chars().count() + 1;
    let (mut lo, mut hi, mut used) = (best, best, cost(lines[best]));
    // A line below too long to fit is shown cut rather than dropped: a
    // heading that matched showed alone, without its paragraph (2026-10-06).
    let mut tail: Option<String> = None;
    loop {
        let mut grew = false;
        if tail.is_none() && hi + 1 < lines.len() {
            if used + cost(lines[hi + 1]) <= width {
                hi += 1;
                used += cost(lines[hi]);
                grew = true;
            } else {
                let room = width.saturating_sub(used + 1);
                let cut: String = lines[hi + 1].trim().chars().take(room).collect();
                tail = Some(if room > 40 { format!("{cut}…") } else { String::new() });
                used = width.min(used + room);
            }
        }
        if lo > 0 && used + cost(lines[lo - 1]) <= width {
            lo -= 1;
            used += cost(lines[lo]);
            grew = true;
        }
        if !grew {
            break;
        }
    }
    let shown: Vec<&str> = lines[lo..=hi].iter().copied().filter(|l| !l.trim().is_empty()).collect();
    let lead = |l: &str| l.chars().take_while(|c| c.is_whitespace()).count();
    let indent = shown.iter().map(|l| lead(l)).min().unwrap_or(0);
    let mut out: Vec<String> = shown
        .iter()
        .map(|l| {
            let cut: usize = l.chars().take(indent).map(char::len_utf8).sum();
            clip(l[cut..].trim_end(), &stems, width)
        })
        .collect();
    out.extend(tail.filter(|t| !t.is_empty()));
    (out.join("\n"), best)
}

/// One line cut to `width` characters around its first matching word: an
/// inlined data file is one line of tens of thousands of characters.
fn clip(line: &str, stems: &[String], width: usize) -> String {
    let chars: Vec<char> = line.chars().collect();
    if chars.len() <= width {
        return line.to_string();
    }
    let lower = line.to_lowercase();
    let at = stems
        .iter()
        .filter_map(|s| lower.find(s.as_str()))
        .min()
        .map(|b| lower[..b].chars().count())
        .unwrap_or(0);
    let from = at.saturating_sub(width / 4).min(chars.len() - width);
    let mut s: String = chars[from..from + width].iter().collect();
    if from > 0 {
        s.insert(0, '…');
    }
    if from + width < chars.len() {
        s.push('…');
    }
    s
}

/// The file line `needle` (a trimmed line of a hit) is on, nearest `near`:
/// a prose chunk starts with the tail of the chunk before and folds blank
/// lines, so counting its lines drifts from the file's.
fn file_line(path: &Path, needle: &str, near: i64) -> Option<i64> {
    if needle.is_empty() || std::fs::metadata(path).ok()?.len() > 4 * 1024 * 1024 {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    text.lines()
        .enumerate()
        .filter(|(_, l)| l.trim() == needle)
        .map(|(i, _)| i as i64 + 1)
        .min_by_key(|n| (n - near).abs())
}

/// A hit cut to its best lines and the file line the best one is on.
/// `line` is what search gave: the chunk's first line, or a definition's
/// own line, which the snippet then shows.
fn short_hit(db: &Db, root: Option<&Path>, query: &str, path: &str, chunk_id: i64, text: &str, line: Option<i64>, width: usize) -> (String, Option<i64>) {
    let mut start = db.chunk_line(chunk_id).ok().flatten();
    let mut text = text.to_string();
    let pinned = line.filter(|l| Some(*l) != start);
    if let (Some(l), Some(s)) = (pinned, start) {
        if l < s || l >= s + text.lines().count() as i64 {
            if let Ok(Some((id, t))) = db.chunk_at_line(path, l) {
                text = t;
                start = db.chunk_line(id).ok().flatten();
            }
        }
    }
    let pin = pinned.zip(start).and_then(|(l, s)| usize::try_from(l - s).ok());
    let (snippet, best) = snippet_around(&text, query, pin, width);
    let line = match (pinned, start) {
        (Some(l), _) => Some(l),
        (None, Some(s)) => {
            let near = s + best as i64;
            let needle = text.lines().nth(best).unwrap_or("").trim();
            Some(root.and_then(|r| file_line(&r.join(path), needle, near)).unwrap_or(near))
        }
        (None, None) => line,
    };
    (snippet, line)
}

/// A snippet on the lines under its hit, indented.
fn indented(snippet: &str) -> String {
    if snippet.is_empty() {
        return String::new();
    }
    format!("\n   {}", snippet.replace('\n', "\n   "))
}

/// The question as a vector, and the model that made it; or why there is
/// none, as one line for the answer.
type QueryVector = Result<(Vec<f32>, String), String>;

/// Embed the query with the app's embedding model, loaded once per server
/// process on first use: without it every MCP search was keyword only
/// (Shattered Realms eval, 2026-10-06).
fn query_vector(server: &Server, query: &str) -> QueryVector {
    let mut slot = server.meaning.borrow_mut();
    if matches!(*slot, Meaning::Unloaded) {
        *slot = load_meaning();
    }
    match &mut *slot {
        Meaning::Ready(model) => model
            .embed_query(query)
            .map(|v| (v, model.model_id()))
            .map_err(|e| format!("embedding the question failed ({e})")),
        Meaning::Off(why) => Err(why.clone()),
        Meaning::Unloaded => Err("the embedding model was not loaded".into()),
    }
}

/// The embedding model the app installed (`<data dir>/whisper`, chosen in
/// `models/selection.json`), or why meaning search is off.
fn load_meaning() -> Meaning {
    #[cfg(all(windows, target_env = "msvc"))]
    if !vulkan_loader_present() {
        return Meaning::Off("this PC has no Vulkan loader (vulkan-1.dll), which the embedding model's library needs".into());
    }
    if ken_core::embedder::selected_profile().is_none() {
        return Meaning::Off("no embedding model is installed (Ken: Settings, Models)".into());
    }
    match ken_core::embedder::installed_embedding_model() {
        Some(model) => Meaning::Ready(model),
        None => Meaning::Off("the embedding model did not load".into()),
    }
}

/// Whether vulkan-1.dll loads: it is delay-loaded (build.rs), and llama.cpp
/// calls it as it starts, so without it the model is never loaded.
#[cfg(all(windows, target_env = "msvc"))]
fn vulkan_loader_present() -> bool {
    #[link(name = "kernel32")]
    extern "system" {
        fn LoadLibraryW(name: *const u16) -> *mut std::ffi::c_void;
    }
    let name: Vec<u16> = "vulkan-1.dll\0".encode_utf16().collect();
    // SAFETY: a NUL-terminated wide string that outlives the call; the
    // module stays loaded, which is what the delay-load helper reuses.
    !unsafe { LoadLibraryW(name.as_ptr()) }.is_null()
}

/// The query vector for one index, only when that index was built by the
/// same model: another model's vectors have another size and meaning.
fn vector_for<'a>(db: &Db, qv: &'a QueryVector) -> Option<&'a [f32]> {
    let (v, model) = qv.as_ref().ok()?;
    (db.embed_model().ok().flatten().as_deref() == Some(model.as_str())).then_some(v.as_slice())
}

/// One line saying a search was keyword only and why; empty when meaning
/// search ran.
fn keyword_only_note(qv: &QueryVector) -> String {
    match qv {
        Ok(_) => String::new(),
        Err(why) => format!("Keyword search only: {why}.\n"),
    }
}

/// Hits route_query and semantic_search return unless asked: eight short
/// hits measured a median 4,554 characters (Shattered Realms eval,
/// 2026-10-06), so ten cost about 6,000, where eight whole chunks took 17,660.
const ROUTE_LIMIT: usize = 10;

/// Route + fan-out + merge (task 3.4): `routing::plan_route`, then a
/// per-target `routing::search_member_of_with`, then `routing::merge_routed`
/// — `execute_plan`'s own loop, written out so each member gets the query
/// vector only when its index was built by the same model ([`vector_for`]).
/// The question is embedded once for every member; hits are cut short last.
fn route_query(server: &Server, args: &Value) -> Result<String, String> {
    let query = require_str(args, "query")?;
    let limit = args
        .get("limit")
        .and_then(|l| l.as_u64())
        .map(|l| l.clamp(1, 200) as usize)
        .unwrap_or(ROUTE_LIMIT);
    let app_settings = AppSettings::load(&server.base_dir);
    if !kg_routing_enabled(&app_settings) {
        return Err("route_query requires the kgRouting feature flag, which is off.".into());
    }
    let registry = Registry::load(&server.base_dir)
        .map_err(|e| format!("could not read Ken's project registry: {e}"))?;
    if registry.projects.is_empty() {
        return Ok("No Ken projects registered on this machine yet — open a \
folder in the Ken app first."
            .to_string());
    }

    // Open every registered, present project's index once — reused both to
    // build `plan_route`'s `MemberInfo` metadata and, for whichever targets
    // the plan picks, the actual search (avoids opening each DB twice).
    // Missing folders are excluded outright: not real routing candidates.
    let mut members: Vec<MemberInfo> = Vec::new();
    let mut dbs: HashMap<Uuid, Db> = HashMap::new();
    let inside = in_open_workspace(server, &registry);
    for entry in &registry.projects {
        if !entry.path.is_dir() || !inside(&entry.path) {
            continue;
        }
        let (index_ready, last_activity) = match Db::open_read_only(&server.base_dir, entry.id) {
            Ok(db) => {
                let ready = db.vec_available();
                // `features/multi-project/README.md`: "recent activity ...
                // the member's most recent ingest completion timestamp" —
                // the existing `ingest_runs` table already tracks exactly
                // this (`Db::runs_with_status`), so no new state is needed.
                // Same computation src-tauri's `route_search` command uses
                // for the same field (lib.rs, `RouteMemberSnapshot`).
                let last_activity = db
                    .runs_with_status("fresh")
                    .ok()
                    .and_then(|rows| rows.iter().filter_map(|r| r.finished_at).max())
                    .unwrap_or(0);
                dbs.insert(entry.id, db);
                (ready, last_activity)
            }
            Err(_) => (false, 0),
        };
        members.push(MemberInfo {
            project_id: entry.id,
            name: entry.name.clone(),
            index_ready,
            last_activity,
            knowledge_base: routing::is_knowledge_base(&entry.kind),
        });
    }

    let kg = federated_kg_enabled(&app_settings)
        .then(|| WorkspaceKgDb::open(&kg_root(server)).ok())
        .flatten();
    let plan = routing::plan_route(&query, &members, kg.as_ref());
    let audience = audience_arg(args);
    let types = types_arg(args);
    // One vocabulary for the whole search (the team wiki's widens every repo).
    let shared_vocab = routing::workspace_vocabulary(dbs.values());
    let qv = if plan.targets.is_empty() { Err("nothing to search".to_string()) } else { query_vector(server, &query) };

    let mut member_hits: Vec<MemberHits> = Vec::with_capacity(plan.targets.len());
    for &project_id in &plan.targets {
        let Some(info) = members.iter().find(|m| m.project_id == project_id) else {
            member_hits.push(MemberHits {
                project_id,
                member_name: String::new(),
                status: MemberStatus::Unavailable,
                hits: Vec::new(),
            });
            continue;
        };
        if !info.index_ready {
            // Mirrors `execute_plan`: not-ready is skipped and reported, not
            // FTS-searched anyway (routing.rs D5 / spec "Fan-out hybrid
            // search"). `index_ready = db.vec_available()` (below) is
            // exactly the signal src-tauri's own `route_search` command
            // uses for the same field (see its doc comment, lib.rs ~6204:
            // "same readiness signal `hybrid_search` gates its KNN pass
            // on") — matched for cross-surface consistency, not derived
            // independently. Caveat worth flagging: `vec_available()` is a
            // per-connection "did the sqlite-vec extension load" flag
            // (`db.rs`'s `load_vec_extension`), not a per-project "were
            // this project's embeddings actually built" flag — it will
            // read `true` for most/every project's Db in a build where the
            // extension links successfully, regardless of that project's
            // own `semanticIndex` flag or embedding state. That's an
            // existing characteristic of the signal both callers share,
            // not something introduced here.
            member_hits.push(MemberHits {
                project_id,
                member_name: info.name.clone(),
                status: MemberStatus::IndexBuilding,
                hits: Vec::new(),
            });
            continue;
        }
        let Some(db) = dbs.get(&project_id) else {
            member_hits.push(MemberHits {
                project_id,
                member_name: info.name.clone(),
                status: MemberStatus::Unavailable,
                hits: Vec::new(),
            });
            continue;
        };
        match routing::search_member_of_with(db, &query, vector_for(db, &qv), fetch_for(audience.as_deref(), &types, limit), &types, Some(&shared_vocab)) {
            Ok(mut hits) => member_hits.push(MemberHits {
                hits: {
                    hits.retain(|h| ken_core::pagemeta::suits(audience.as_deref(), h.page.as_ref().and_then(|p| p.audience)));
                    hits.truncate(limit);
                    hits
                },
                project_id,
                member_name: info.name.clone(),
                status: MemberStatus::Searched,
            }),
            Err(_) => member_hits.push(MemberHits {
                project_id,
                member_name: info.name.clone(),
                status: MemberStatus::Unavailable,
                hits: Vec::new(),
            }),
        }
    }

    let mut report = routing::merge_routed(&plan, &member_hits, limit);
    for hit in &mut report.results {
        let Some(db) = dbs.get(&hit.project_id) else { continue };
        let root = registry.projects.iter().find(|e| e.id == hit.project_id).map(|e| e.path.as_path());
        let (snippet, line) = short_hit(db, root, &query, &hit.path, hit.chunk_id, &hit.snippet, hit.line, SNIPPET_CHARS);
        // The locator ends in the chunk's line; it now names the best line.
        if let (Some(old), Some(new)) = (hit.line, line) {
            if let Some(stem) = hit.locator.strip_suffix(&format!(":{old}")) {
                hit.locator = format!("{stem}:{new}");
            }
        }
        hit.snippet = snippet;
        hit.line = line;
    }
    let mut out = format_execution_report(&report);
    if !plan.targets.is_empty() {
        let note = keyword_only_note(&qv);
        // After the Route and Members lines, which a reader looks for first.
        match out.match_indices('\n').nth(1) {
            Some((at, _)) => out.insert_str(at + 1, &note),
            None => out.push_str(&note),
        }
    }
    Ok(out)
}

/// Renders a route_query [`routing::ExecutionReport`] as tool-call text
/// (this server has no structured-content channel — every tool returns
/// prose, matching the rest of this file's style).
fn format_execution_report(report: &routing::ExecutionReport) -> String {
    let reason = match &report.plan.reason {
        RouteReason::Named => "named project match".to_string(),
        RouteReason::KgEntities(ids) => format!(
            "knowledge-graph match ({} entit{})",
            ids.len(),
            if ids.len() == 1 { "y" } else { "ies" }
        ),
        RouteReason::Broadcast => "broadcast — no name or knowledge-graph match".to_string(),
    };
    let mut out = format!(
        "Route: {reason}, {} target project{}.\n",
        report.plan.targets.len(),
        if report.plan.targets.len() == 1 { "" } else { "s" }
    );
    out.push_str("Members: ");
    let member_lines: Vec<String> = report
        .member_status
        .iter()
        .map(|m| {
            let status = match m.status {
                MemberStatus::Searched => "searched",
                MemberStatus::IndexBuilding => "index-building",
                MemberStatus::Unavailable => "unavailable",
            };
            format!("{} ({status})", m.member_name)
        })
        .collect();
    out.push_str(&member_lines.join(", "));
    out.push('\n');

    if report.results.is_empty() {
        out.push_str(&format!("\nNo results. {LOOK_YOURSELF}"));
        return out;
    }
    for (i, hit) in report.results.iter().enumerate() {
        // The locator is what to cite; the ken:// address is what to open.
        out.push_str(&format!(
            "\n{}. {} {} ({}{}) [{}]",
            i + 1,
            kind_tag(&hit.path),
            hit.locator,
            hit.address,
            hit.line.map(|l| format!("#L{l}")).unwrap_or_default(),
            source_label(hit.source),
        ));
        if let Some(note) = hit.page.as_ref().and_then(page_note) {
            out.push_str(&format!(" ({note})"));
        }
        if !hit.kg_breadcrumbs.is_empty() {
            out.push_str(&format!(" ({})", hit.kg_breadcrumbs.join(" ")));
        }
        out.push_str(&indented(&hit.snippet));
    }
    out
}

/// `memory_write` (task 3.1): create or replace a Ken long-term memory file
/// via the same `ken_core::memory::write_memory` core the Ken chat tool of
/// the same name is specified to use (design D5: "one write core, two tool
/// surfaces").
fn memory_write_tool(server: &Server, args: &Value) -> Result<String, String> {
    let scope_arg = require_str(args, "scope")?;
    let slug = require_str(args, "slug")?;
    let content = require_str(args, "content")?;
    // Judgment call: the docs (tasks.md 1.2 / spec.md) specify create-mode
    // by default with an explicit `WriteMode::Replace` for updates — this
    // tool exposes both rather than create-only, so an agent that wrote a
    // memory earlier in a session can update it without a separate
    // out-of-band path; "create" stays the default either way an MCP
    // client omits `mode`.
    let mode = match args.get("mode").and_then(Value::as_str) {
        None | Some("create") => memory::WriteMode::Create,
        Some("replace") => memory::WriteMode::Replace,
        Some(other) => {
            return Err(format!("invalid \"mode\" {other:?} — use \"create\" or \"replace\""))
        }
    };
    let (today, _) = local_today_and_time();

    if scope_arg.eq_ignore_ascii_case("workspace") {
        let workspace_root = resolve_workspace_root(server)?;
        let scope = memory::MemoryScope::Workspace { workspace_root: &workspace_root };
        let path = memory::write_memory(scope, &slug, &content, mode, &today)
            .map_err(|e| e.to_string())?;
        Ok(format!(
            "Wrote workspace memory \"{slug}\" to {} (ken://workspace/memory/{slug}.md).",
            path.display()
        ))
    } else {
        let (project_id, project_root, project_name) = resolve_project_by_name(server, &scope_arg)?;
        let scope = memory::MemoryScope::Project { project_root: &project_root };
        let path = memory::write_memory(scope, &slug, &content, mode, &today)
            .map_err(|e| e.to_string())?;
        Ok(format!(
            "Wrote memory \"{slug}\" to project \"{project_name}\" at {} ({}).",
            path.display(),
            ken_address(project_id, &format!(".ken/memory/{slug}.md"))
        ))
    }
}

/// `journal_append` (task 3.1): append a `## HH:MM` entry to today's Ken
/// workspace journal via the same core `append_journal` the Ken chat tool
/// uses — this is how agent-desktop closes the loop, reporting task
/// findings back into Ken's searchable memory (spec: "agent-desktop reports
/// back").
fn journal_append_tool(server: &Server, args: &Value) -> Result<String, String> {
    let text = require_str(args, "text")?;
    let project = args
        .get("project")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|p| !p.is_empty());
    let tags: Vec<String> = args
        .get("tags")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).map(str::to_string).collect())
        .unwrap_or_default();

    let workspace_root = resolve_workspace_root(server)?;
    let (today, time_hhmm) = local_today_and_time();
    let path = memory::append_journal(&workspace_root, &text, project, &tags, &today, &time_hhmm)
        .map_err(|e| format!("could not append to the journal: {e}"))?;
    Ok(format!(
        "Appended a {today} {time_hhmm} entry to the workspace journal ({}).",
        path.display()
    ))
}

// --- Your day tools: task_create / task_update / task_list / ticket_list ---
//
// The file logic is `ken_core::day`, the same core the app's Your day
// commands use, so a task written here reads the same there.

/// One member's board within one family connection, as a task home.
/// Every member's board is scanned so `task_update` can find a task by id,
/// but only this device's own board is listed, and `family_authorize_write`
/// (via `family::lane_check`) refuses a write to anyone else's.
#[derive(Debug, Clone)]
struct FamilyBoardHome {
    family_id: Uuid,
    family_name: String,
    clone_root: PathBuf,
    /// Whose board this home scans.
    member_id: String,
    /// This device's own member id in the family, if known.
    my_member_id: Option<String>,
    /// `family::board_dir(&clone_root, &member_id)`.
    board_dir: PathBuf,
}

impl FamilyBoardHome {
    fn is_mine(&self) -> bool {
        self.my_member_id.as_deref() == Some(self.member_id.as_str())
    }
}

/// The task homes for one call, owned so borrowed `TaskHome`s can be built
/// from them: the workspace home (where new tasks go) and the boards of
/// the families attached to this workspace. A repo's own `.ken/tasks/` is
/// not a home (tasks live in your own folder, not in a team repo).
struct TaskHomes {
    workspace_root: PathBuf,
    family_boards: Vec<FamilyBoardHome>,
}

impl TaskHomes {
    fn homes(&self, only_mine: bool) -> Vec<tasks::TaskHome<'_>> {
        let mut out = vec![tasks::TaskHome::Workspace { workspace_root: &self.workspace_root }];
        for fb in self.family_boards.iter().filter(|fb| !only_mine || fb.is_mine()) {
            out.push(tasks::TaskHome::Family { board_dir: &fb.board_dir });
        }
        out
    }

    fn scan(&self, today: &str, only_mine: bool) -> Vec<day::DayTask> {
        day::scan(&self.homes(only_mine), today)
    }

    fn family_origin(&self, task: &day::DayTask) -> Option<&FamilyBoardHome> {
        self.family_boards.iter().find(|fb| fb.board_dir == task.home_dir)
    }

    /// The `ken://` host for a task: the family id for a board task, else
    /// the workspace pseudo-host.
    fn host_for(&self, task: &day::DayTask) -> String {
        match self.family_origin(task) {
            Some(fb) => fb.family_id.to_string(),
            None => memory::WORKSPACE_ADDRESS_ID.to_string(),
        }
    }

    fn address_for(&self, task: &day::DayTask) -> String {
        format!("ken://{}/{}", self.host_for(task), task.address_rel_path())
    }
}

/// Is this family one Your day reads for workspace `workspace_id`? The
/// app's `day_families` rule: a connection saved in settings
/// (`familyConnections`) that is attached to this workspace or to none. A
/// clone on disk with no saved connection is not read.
fn family_attached_to(settings: &AppSettings, family_id: &str, dir_name: &str, workspace_id: Option<Uuid>) -> bool {
    let Some(conns) = settings.extra.get("familyConnections").and_then(Value::as_array) else {
        return false;
    };
    conns.iter().filter_map(Value::as_object).any(|c| {
        let id_matches = c
            .get("familyId")
            .and_then(Value::as_str)
            .is_some_and(|v| v.eq_ignore_ascii_case(family_id) || v.eq_ignore_ascii_case(dir_name));
        let attached = match c.get("attachedWorkspaceId") {
            None | Some(Value::Null) => true,
            Some(v) => v
                .as_str()
                .and_then(|s| s.parse::<Uuid>().ok())
                .is_some_and(|id| Some(id) == workspace_id),
        };
        id_matches && attached
    })
}

fn resolve_task_homes(server: &Server) -> Result<TaskHomes, String> {
    let workspace_root = resolve_workspace_root(server)?;
    let workspace_id = Registry::load(&server.base_dir).ok().and_then(|r| r.last_workspace);
    let settings = AppSettings::load(&server.base_dir);
    let mut family_boards = Vec::new();
    for conn in discover_family_connections(server) {
        if conn.manifest.check_supported().is_err() {
            continue; // "needs a newer Ken": no sync, ingest, or write (spec)
        }
        if !family_attached_to(&settings, &conn.manifest.id.to_string(), &conn.dir_name, workspace_id) {
            continue; // attached to another workspace, or no longer connected
        }
        for member in &conn.manifest.members {
            let board_dir = family::board_dir(&conn.clone_root, &member.id);
            if !board_dir.is_dir() {
                continue;
            }
            family_boards.push(FamilyBoardHome {
                family_id: conn.manifest.id,
                family_name: conn.manifest.name.clone(),
                clone_root: conn.clone_root.clone(),
                member_id: member.id.clone(),
                my_member_id: conn.my_member_id.clone(),
                board_dir,
            });
        }
    }
    Ok(TaskHomes { workspace_root, family_boards })
}

/// `(today, now)` for a task read or write, in local time like the app's
/// `local_date_today`/`local_stamp_now`: `YYYY-MM-DD` and
/// `YYYY-MM-DDTHH:MM`. Local, not UTC, because Your day's "today" (done
/// on, done-today-stays-listed, a recurring task's day) is the user's day.
fn task_clock() -> (String, String) {
    task_clock_at(chrono::Local::now())
}

fn task_clock_at<Tz: chrono::TimeZone>(at: chrono::DateTime<Tz>) -> (String, String)
where
    Tz::Offset: std::fmt::Display,
{
    (at.format("%Y-%m-%d").to_string(), at.format("%Y-%m-%dT%H:%M").to_string())
}

fn str_list(args: &Value, key: &str) -> Option<Vec<String>> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
}

fn task_line(t: &day::DayTask, address: &str) -> String {
    let mut parts: Vec<String> = vec![t.state.as_str().to_string()];
    if let Some(d) = &t.target {
        parts.push(format!("target {d}"));
    }
    if let Some(r) = &t.repeat {
        parts.push(format!("repeat {r}"));
    }
    if let Some(f) = &t.from {
        parts.push(format!("from {f}"));
    }
    let links = if t.links.is_empty() { String::new() } else { format!(" links=[{}]", t.links.join(", ")) };
    format!("{} — \"{}\" [{}]{links} — {address}", t.id, t.title, parts.join(", "))
}

/// `task_create`: a new task in the workspace home, `updated_by: mcp`.
fn task_create_tool(server: &Server, args: &Value) -> Result<String, String> {
    let title = require_str(args, "title")?;
    let homes = resolve_task_homes(server)?;
    let input = day::DayTaskInput {
        title,
        target: opt_str(args, "target"),
        description: args.get("description").and_then(Value::as_str).map(str::to_string),
        repeat: opt_str(args, "repeat"),
        links: str_list(args, "links"),
    };
    let (today, now) = task_clock();
    let stamp = day::Stamp { today: &today, now: &now, by: day::BY_MCP };
    let task = day::create_task(&homes.workspace_root, &input, &stamp, None).map_err(|e| e.to_string())?;
    Ok(format!("Created task {}", task_line(&task, &homes.address_for(&task))))
}

/// `task_update`: change a task by id, `updated_by: mcp`. Marking it done
/// appends the one-line journal entry (memory is built in). A task on a
/// family board is lane-checked before anything touches disk, then
/// committed and pushed.
fn task_update_tool(server: &Server, args: &Value) -> Result<String, String> {
    let id = require_str(args, "id")?;
    let state = match args.get("state").and_then(Value::as_str) {
        None => None,
        Some(s) => Some(day::DayTaskState::parse(s).ok_or_else(|| format!("invalid \"state\" {s:?} — use open or done"))?),
    };
    // A present string is a change; "" clears.
    let cleared = |key: &str| args.get(key).and_then(Value::as_str).map(|s| Some(s.trim().to_string()).filter(|s| !s.is_empty()));
    let patch = day::DayTaskPatch {
        title: args.get("title").and_then(Value::as_str).map(str::to_string),
        target: cleared("target"),
        description: args.get("description").and_then(Value::as_str).map(str::to_string),
        repeat: cleared("repeat"),
        links: str_list(args, "links"),
        state,
    };
    let as_arg = opt_str(args, "as");
    let (today, now) = task_clock();
    let homes = resolve_task_homes(server)?;
    let all = homes.scan(&today, false);
    let task = day::find(&all, &id)
        .ok_or_else(|| format!("no task with id {id:?} — task_list shows the ids"))?
        .clone();
    let stamp = day::Stamp { today: &today, now: &now, by: day::BY_MCP };

    let mut sync_note = String::new();
    match homes.family_origin(&task).cloned() {
        Some(fb) => {
            let (identity, rel_path) = family_authorize_write(&fb, &task.file_name(), as_arg.as_deref())?;
            day::update_task(&task, &patch, &stamp).map_err(|e| e.to_string())?;
            sync_note = push_family_board_write(&fb, &identity, &rel_path, &task.path);
        }
        None => day::update_task(&task, &patch, &stamp).map_err(|e| e.to_string())?,
    }

    let raw = std::fs::read_to_string(&task.path).map_err(|e| format!("task was updated but could not be re-read: {e}"))?;
    let updated = day::parse_task(&task.path, task.home, &raw, &today);
    let address = homes.address_for(&updated);
    let mut msg = format!("Updated task {}", task_line(&updated, &address));
    // Journal the change to done only: a repeated "done" on a task that
    // already was done writes no second entry.
    if state == Some(day::DayTaskState::Done)
        && task.state != day::DayTaskState::Done
        && updated.state == day::DayTaskState::Done
    {
        let line = day::journal_line(&updated, &homes.host_for(&updated));
        let time_hhmm = now.get(11..16).unwrap_or("00:00").to_string();
        match memory::append_journal(&homes.workspace_root, &line, None, &[], &today, &time_hhmm) {
            Ok(_) => msg.push_str(" Journal entry recorded."),
            Err(e) => msg.push_str(&format!(" (warning: could not write the journal entry: {e})")),
        }
    }
    msg.push_str(&sync_note);
    Ok(msg)
}

/// `task_list`: the user's tasks (their own family board only), open by
/// default.
fn task_list_tool(server: &Server, args: &Value) -> Result<String, String> {
    let state = match args.get("state").and_then(Value::as_str).map(str::trim) {
        None | Some("") | Some("open") => Some(day::DayTaskState::Open),
        Some("done") => Some(day::DayTaskState::Done),
        Some("all") => None,
        Some(other) => return Err(format!("invalid \"state\" {other:?} — use open, done or all")),
    };
    let target_before = opt_str(args, "target_before");
    if let Some(d) = &target_before {
        if !day::is_date(d) {
            return Err(format!("\"target_before\" {d:?} is not a date (YYYY-MM-DD)"));
        }
    }
    let q = day::TaskQuery { state, target_before, linked: opt_str(args, "linked") };
    let (today, _) = task_clock();
    let homes = resolve_task_homes(server)?;
    let all = homes.scan(&today, true);
    let hits = day::query(&all, &q);
    if hits.is_empty() {
        return Ok("No tasks match.".to_string());
    }
    let mut out = format!("{} task{}:\n", hits.len(), if hits.len() == 1 { "" } else { "s" });
    for t in hits {
        out.push_str(&format!("\n{}", task_line(t, &homes.address_for(t))));
    }
    Ok(out)
}

/// `ticket_list`: the ticket files in the workspace's repos, read-only.
fn ticket_list_tool(server: &Server, args: &Value) -> Result<String, String> {
    let mine = match args.get("assignee").and_then(Value::as_str).map(str::trim) {
        None | Some("") | Some("me") => true,
        Some("all") => false,
        Some(other) => return Err(format!("invalid \"assignee\" {other:?} — use me or all")),
    };
    let open_only = match args.get("state").and_then(Value::as_str).map(str::trim) {
        None | Some("") | Some("open") => true,
        Some("all") => false,
        Some(other) => return Err(format!("invalid \"state\" {other:?} — use open or all")),
    };
    let workspace_root = resolve_workspace_root(server)?;
    let ws = ken_core::workspace::Workspace::open(&workspace_root)
        .map_err(|e| format!("could not open the workspace at {}: {e}", workspace_root.display()))?;
    let me = day::git_me();
    if mine && !me.is_known() {
        return Ok("git has no user.name or user.email on this machine, so no ticket can be matched to \
you. Pass assignee \"all\" to list every ticket."
            .to_string());
    }
    let (today, _) = task_clock();
    let tasks_all = resolve_task_homes(server).map(|h| h.scan(&today, true)).unwrap_or_default();

    let mut found: Vec<(day::Ticket, String, Uuid)> = Vec::new();
    for m in &ws.members {
        let ken_core::workspace::MemberStatus::Ok(p) = &m.status else { continue };
        for t in day::scan_tickets(&p.root) {
            if (open_only && !t.open) || (mine && !me.assigned(&t)) {
                continue;
            }
            found.push((t, m.name.clone(), p.config.id));
        }
    }
    if found.is_empty() {
        return Ok(format!(
            "No {}tickets{}.",
            if open_only { "open " } else { "" },
            if mine { " assigned to you" } else { "" }
        ));
    }
    found.sort_by(|a, b| day::ticket_order(&a.0, &b.0));
    let mut out = format!("{} ticket{}:\n", found.len(), if found.len() == 1 { "" } else { "s" });
    for (t, repo, pid) in &found {
        let (n, done) = day::linked_counts(&tasks_all, Some(repo.as_str()), &t.id);
        let mut parts = vec![if t.state.is_empty() { "no status".to_string() } else { t.state.clone() }];
        parts.push(format!("repo {repo}"));
        if let Some(d) = &t.target {
            parts.push(format!("target {d}"));
        }
        if !t.assignees.is_empty() {
            parts.push(format!("assignee {}", t.assignees.join(", ")));
        }
        if n > 0 {
            parts.push(format!("{n} task{}, {done} done", if n == 1 { "" } else { "s" }));
        }
        out.push_str(&format!("\n{} — \"{}\" [{}] — {}", t.id, t.title, parts.join(", "), ken_address(*pid, &t.rel_path)));
    }
    Ok(out)
}

/// What an agent must know before it trusts a page hit: its label (`ruling
/// D-410 · 2026-10-06`, `ticket · cancelled · 2026-07-21`), or for a hit
/// without one, that it is retired (and what replaces it), generated, dated
/// evidence, or when a person last verified it.
fn page_note(p: &ken_core::pagemeta::HitPage) -> Option<String> {
    if let Some(label) = &p.label {
        return Some(match p.generated {
            true => format!("{label}: fix the generator, not the page"),
            false => label.clone(),
        });
    }
    if p.retired {
        return Some(match p.replaced_by.is_empty() {
            true => "retired".to_string(),
            false => format!("retired, see {}", p.replaced_by.join(", ")),
        });
    }
    if p.generated {
        return Some("generated: fix the generator, not the page".to_string());
    }
    if let Some(d) = &p.dated {
        return Some(format!("evidence from {d}, not current state"));
    }
    p.verified.as_ref().map(|v| format!("verified {v}"))
}

fn source_label(source: Source) -> &'static str {
    match source {
        Source::Keyword => "keyword",
        Source::Semantic => "semantic",
        Source::Both => "keyword+semantic",
    }
}

/// `ken://<project-id>/<rel-path>` — same format as `routing::search_member`'s
/// private sibling in ken-core (`fn ken_address`, not `pub`); duplicated
/// here for this one caller rather than exported (`features/multi-project/
/// README.md` "Cross-feature contracts": "Rel-paths are always forward-slash
/// normalized, on Windows too").
fn ken_address(project_id: Uuid, rel_path: &str) -> String {
    format!("ken://{project_id}/{}", rel_path.replace('\\', "/"))
}

/// The workspace graph and routed search are how Ken works, not switches
/// (`features::BUILT_IN`), as in the app.
fn federated_kg_enabled(_app_settings: &AppSettings) -> bool {
    true
}

fn kg_routing_enabled(_app_settings: &AppSettings) -> bool {
    true
}

// --- ken-families tools (tasks 3.1-3.4) ---
//
// `family.rs`/`family_sync.rs` (section 1, another session, already
// landed) own every decision that matters for safety: `lane_check` is what
// actually refuses a cross-lane write, `commit_paths` is the only write
// path, and `SyncEngine` is the only state machine that talks to git. This
// layer's job is thin: discover what family clones exist on this device,
// figure out which manifest member this device is, and turn that into
// three read/write tools plus two extra homes for the existing task tools.

/// One family connection this device knows about, discovered from disk
/// (see `discover_family_connections`'s doc comment for why: task 2.1's
/// settings-backed connection store had not landed in `src-tauri` as of
/// this layer, confirmed by grepping `src-tauri/src/lib.rs` for
/// "family"/"families" — zero hits — so this can't read it. `family.json`
/// itself has no "who am I" field (it's the *shared*, synced manifest,
/// identical in every member's clone), so identity is resolved separately
/// per connection — see `my_member_id`'s doc comment).
struct FamilyConnection {
    /// The `families/<dir>` folder name under this device's app data
    /// (design D1: `<app data>/ken/families/<family-id>/`) — usually the
    /// manifest's own id as a string, but matched loosely by
    /// `find_family_connection` (id, name, or this folder name) since
    /// nothing enforces the two staying in sync on disk.
    dir_name: String,
    clone_root: PathBuf,
    manifest: FamilyManifest,
    /// This device's member id in this family, if it could be resolved —
    /// see `settings_member_id` (best-effort, forward-compatible with
    /// task 2.1's eventual settings shape) and its single-member fallback
    /// in `discover_family_connections`. `None` means a tool that needs to
    /// write (family_send, a family-board task_update) must
    /// be given an explicit `as` argument instead.
    my_member_id: Option<String>,
}

/// `<base_dir>/families` — `base_dir` is already `<app data>/ken`
/// (`registry::default_base_dir`), so this is exactly design D1's
/// `<app data>/ken/families/`.
fn families_root(server: &Server) -> PathBuf {
    server.base_dir.join("families")
}

/// Every family connection this device has a local clone for.
///
/// **Judgment call, recorded honestly**: task 2.1 (the settings-backed
/// connection store — remote URL, my member id, live-sync, attached
/// workspace) has not landed in `src-tauri` as of this layer (verified by
/// grep: no "family"/"families" hits anywhere under `src-tauri/src/`, nor
/// in `settings.rs`/`features.rs`). Rather than block on that session or
/// invent a second, competing store, this discovers connections the robust
/// way available right now: scan `<base_dir>/families/*/family.json`
/// directly. Every directory with a loadable manifest is a connection.
/// This also means "attached to workspace" (D6, which gates *search
/// indexing*, task 2.5 — a different concern from these MCP tools) isn't
/// checked here: every on-disk connection contributes to family_list,
/// family_inbox, family_send, and the task-tool board homes, regardless of
/// any future attachment flag. When 2.1 lands, `settings_member_id` below
/// already probes a couple of plausible future shapes for the identity
/// half; the discovery half (which connections exist) would only need to
/// prefer the settings list's `clonePath`s over this scan if their shapes
/// ever disagree, which they should not (both describe the same disk
/// state D1 defines).
fn discover_family_connections(server: &Server) -> Vec<FamilyConnection> {
    let root = families_root(server);
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let settings = AppSettings::load(&server.base_dir);
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Ok(manifest) = FamilyManifest::load(&path) else {
            continue; // not a family clone (or unreadable) — skip, don't error the whole scan
        };
        let dir_name = entry.file_name().to_string_lossy().into_owned();
        let my_member_id = settings_member_id(&settings, &manifest.id.to_string(), &dir_name).or_else(|| {
            // No identity on record anywhere: a family with exactly one
            // member is unambiguous (the solo/just-created case) — assume
            // that member is this device. A multi-member family with no
            // recorded identity stays `None` until an `as` argument or
            // task 2.1's store supplies one.
            match manifest.members.as_slice() {
                [only] => Some(only.id.clone()),
                _ => None,
            }
        });
        out.push(FamilyConnection { dir_name, clone_root: path, manifest, my_member_id });
    }
    out.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name).then(a.dir_name.cmp(&b.dir_name)));
    out
}

/// Best-effort, forward-compatible lookup for "which member am I in this
/// family" from `settings.json`'s passthrough `extra` map, in case task
/// 2.1's connection store has landed by the time this runs. Its exact
/// shape isn't known to this layer (it's owned by a parallel session), so
/// this tolerantly probes a few plausible key/field names rather than
/// assuming one; anything that doesn't match falls through to `None`, and
/// `discover_family_connections`'s single-member fallback (or an explicit
/// `as` argument) takes over from there.
fn settings_member_id(settings: &AppSettings, family_id: &str, dir_name: &str) -> Option<String> {
    for list_key in ["familyConnections", "families", "family_connections"] {
        let Some(arr) = settings.extra.get(list_key).and_then(Value::as_array) else {
            continue;
        };
        for conn in arr {
            let Some(obj) = conn.as_object() else { continue };
            let id_matches = ["familyId", "family_id", "id"].iter().any(|k| {
                obj.get(*k)
                    .and_then(Value::as_str)
                    .is_some_and(|v| v.eq_ignore_ascii_case(family_id) || v.eq_ignore_ascii_case(dir_name))
            });
            if !id_matches {
                continue;
            }
            for member_key in ["myMemberId", "my_member_id", "memberId", "member_id", "member"] {
                if let Some(m) = obj.get(member_key).and_then(Value::as_str) {
                    let m = m.trim();
                    if !m.is_empty() {
                        return Some(m.to_string());
                    }
                }
            }
        }
    }
    None
}

/// Resolve the `family` tool argument (name or id, matched case-
/// insensitively against a connection's manifest id, manifest name, or its
/// `families/<dir>` folder name) to one connection.
fn find_family_connection(server: &Server, family_arg: &str) -> Result<FamilyConnection, String> {
    let conns = discover_family_connections(server);
    conns
        .into_iter()
        .find(|c| {
            c.dir_name.eq_ignore_ascii_case(family_arg)
                || c.manifest.id.to_string().eq_ignore_ascii_case(family_arg)
                || c.manifest.name.eq_ignore_ascii_case(family_arg)
        })
        .ok_or_else(|| {
            let names: Vec<String> =
                discover_family_connections(server).iter().map(|c| c.manifest.name.clone()).collect();
            let available = if names.is_empty() {
                "No family connections were found on this device.".to_string()
            } else {
                format!("Known families: {}.", names.join(", "))
            };
            format!("No family connection matches {family_arg:?}. {available}")
        })
}

/// Which member this call acts as for `conn`: the connection's resolved
/// identity when there is one; only without one, an explicit `as`
/// argument (validated against the manifest); else a clear error telling
/// the caller how to supply one (family_list's members list is where to
/// find valid ids).
fn resolve_family_identity(conn: &FamilyConnection, as_arg: Option<&str>) -> Result<String, String> {
    // A configured identity wins: `as` is only for a device that has none.
    if let Some(me) = &conn.my_member_id {
        return Ok(me.clone());
    }
    if let Some(explicit) = as_arg {
        let explicit = explicit.trim();
        if !conn.manifest.has_member(explicit) {
            return Err(format!(
                "\"{explicit}\" is not a member of family \"{}\". Members: {}.",
                conn.manifest.name,
                conn.manifest.member_ids().join(", ")
            ));
        }
        return Ok(explicit.to_string());
    }
    Err(format!(
        "This device's member identity in family \"{}\" is not configured yet. \
Pass \"as\" with one of this family's member ids ({}) to say who you are.",
        conn.manifest.name,
        conn.manifest.member_ids().join(", ")
    ))
}

fn opt_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// Task 3.4's enforcement point: may this device, acting as `identity`
/// (`fb.my_member_id`, or an explicit `as` argument only when none is
/// configured), write to
/// `fb`'s board? Delegates to `family::lane_check` itself — the same
/// function `commit_paths` runs on every real family commit — so a claim
/// aimed at a teammate's board is refused by the lane rules, not by a
/// separate check this function could get out of sync with. Returns the
/// resolved identity and the write's repo-relative path on success, so
/// callers don't recompute either.
fn family_authorize_write(
    fb: &FamilyBoardHome,
    file_name: &str,
    as_arg: Option<&str>,
) -> Result<(String, String), String> {
    // A configured identity is who this device is; `as` only fills the gap
    // when none is configured, so it cannot be used to write as someone else.
    let identity = fb.my_member_id.clone().or_else(|| as_arg.map(str::to_string)).ok_or_else(|| {
        format!(
            "cannot write to family \"{}\"'s board — this device's member identity for that \
family isn't configured yet; pass \"as\" with your member id",
            fb.family_name
        )
    })?;
    let rel_path = format!("{}/{file_name}", family::board_rel(&fb.member_id));
    family::lane_check(&identity, &rel_path, false)
        .map_err(|v| format!("refused: {v} (family \"{}\")", fb.family_name))?;
    Ok((identity, rel_path))
}

/// Commit and push a family board write that already landed on disk (task
/// 3.4's "the completion is pushed for teammates to see"). Best-effort by
/// design: the local file write already succeeded and is the operation's
/// actual result, so any git failure here becomes a warning suffix on the
/// tool's success message rather than failing the call — the same posture
/// `task_update_tool` takes with journal-write failures.
fn push_family_board_write(fb: &FamilyBoardHome, identity: &str, rel_path: &str, path: &Path) -> String {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => return format!(" (warning: written locally; could not read it back to sync: {e})"),
    };
    let mut git = match SystemGit::open(&fb.clone_root) {
        Ok(g) => g,
        Err(e) => return format!(" (warning: written locally; could not open the family clone to sync: {e})"),
    };
    let lane = Lane::member(identity);
    let mut engine = SyncEngine::new();
    if let Err(e) =
        engine.commit(&mut git, &lane, &[PendingWrite::new(rel_path.to_string(), content)], "Ken: board update")
    {
        return format!(" (warning: written locally; could not commit to the family clone: {e})");
    }
    let report = engine.poll(&mut git);
    match report.state {
        ConnectionState::Idle => " Synced to the family remote.".to_string(),
        other => format!(" (warning: committed locally but not pushed yet — {other:?}; will retry on the next sync)"),
    }
}

fn parse_inbox_task_payload(v: &Value) -> Result<InboxTaskPayload, String> {
    let obj = v.as_object().ok_or_else(|| "\"task\" must be an object".to_string())?;
    let get_str = |k: &str| obj.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
    let tags: Vec<String> = obj
        .get("tags")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect::<Vec<_>>())
        .unwrap_or_default();
    let kind = get_str("kind");
    Ok(InboxTaskPayload {
        title: get_str("title"),
        project: get_str("project"),
        tags,
        due: get_str("due"),
        kind: if kind.is_empty() { "human".to_string() } else { kind },
    })
}

/// `family_list` (task 3.1): every connection this device knows about, its
/// members, this device's identity in it (if resolved), and its local sync
/// state. Deliberately reads local git state only (`head_status` —
/// ahead/behind/dirty/rebase) and never fetches: a list call should be
/// cheap and side-effect-free, unlike family_send's deliver-and-push.
fn family_list_tool(server: &Server) -> Result<String, String> {
    let conns = discover_family_connections(server);
    if conns.is_empty() {
        return Ok("No family connections found on this device.".to_string());
    }
    let mut out = format!("{} family connection{}:\n", conns.len(), if conns.len() == 1 { "" } else { "s" });
    for c in &conns {
        out.push_str(&format!("\n{} (id {})\n", c.manifest.name, c.manifest.id));
        if let Err(unsupported) = c.manifest.check_supported() {
            out.push_str(&format!("  unavailable: {unsupported}\n"));
            continue;
        }
        let identity = c.my_member_id.as_deref().unwrap_or("unknown — pass \"as\" to family_inbox/family_send");
        out.push_str(&format!("  you are: {identity}\n"));
        let members: Vec<String> = c
            .manifest
            .members
            .iter()
            .map(|m| {
                let you = if Some(m.id.as_str()) == c.my_member_id.as_deref() { " (you)" } else { "" };
                format!("{}{you}", m.id)
            })
            .collect();
        out.push_str(&format!("  members: {}\n", members.join(", ")));
        out.push_str(&format!("  clone: {}\n", c.clone_root.display()));
        match family_sync::git_available() {
            Err(reason) => out.push_str(&format!("  sync: unavailable — git is not usable ({reason})\n")),
            Ok(_) => match SystemGit::open(&c.clone_root).and_then(|g| g.head_status()) {
                Ok(status) => out.push_str(&format!(
                    "  sync: {} ahead, {} behind, {}{}\n",
                    status.ahead,
                    status.behind,
                    if status.dirty { "dirty" } else { "clean" },
                    if status.rebase_in_progress { ", rebase in progress" } else { "" }
                )),
                Err(e) => out.push_str(&format!("  sync: could not read local git status ({e})\n")),
            },
        }
    }
    Ok(out)
}

/// `family_inbox` (task 3.2): this device's own inbox items for one family
/// (or every family with a resolved identity, if `family` is omitted).
/// Read-only by design — surfacing an item and marking it `seen`/
/// `accepted`/`archived` are separate, deliberate actions (D4), not a side
/// effect of listing.
fn family_inbox_tool(server: &Server, args: &Value) -> Result<String, String> {
    let family_arg = opt_str(args, "family");
    let as_arg = opt_str(args, "as");

    let targets: Vec<FamilyConnection> = match &family_arg {
        Some(f) => vec![find_family_connection(server, f)?],
        None => discover_family_connections(server),
    };
    if targets.is_empty() {
        return Ok("No family connections found on this device.".to_string());
    }

    let mut out = String::new();
    for conn in &targets {
        if let Err(unsupported) = conn.manifest.check_supported() {
            out.push_str(&format!("{}: unavailable — {unsupported}\n", conn.manifest.name));
            continue;
        }
        let identity = match resolve_family_identity(conn, as_arg.as_deref()) {
            Ok(id) => id,
            Err(e) => {
                // A specific `family` argument that can't resolve an
                // identity is the caller's problem to fix; omitted `family`
                // just skips connections we don't know our identity in.
                if family_arg.is_some() {
                    return Err(e);
                }
                out.push_str(&format!("{}: {e}\n", conn.manifest.name));
                continue;
            }
        };
        let dir = family::inbox_dir(&conn.clone_root, &identity);
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "md"))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        files.sort();
        if files.is_empty() {
            out.push_str(&format!("{} (\"{identity}\"): inbox is empty.\n", conn.manifest.name));
            continue;
        }
        out.push_str(&format!(
            "{} (\"{identity}\") — {} item{}:\n",
            conn.manifest.name,
            files.len(),
            if files.len() == 1 { "" } else { "s" }
        ));
        for path in &files {
            let file_name = path.file_name().unwrap().to_string_lossy().into_owned();
            let raw = std::fs::read_to_string(path).unwrap_or_default();
            let item = family::parse_inbox_item(&file_name, &raw);
            out.push_str(&format!(
                "\n  {} [{}, {}] from {} — \"{}\"{}",
                item.id,
                item.kind.map(|k| k.as_str()).unwrap_or(item.kind_raw.as_str()),
                item.status.map(|s| s.as_str()).unwrap_or(item.status_raw.as_str()),
                if item.from.is_empty() { "unknown" } else { &item.from },
                item.title,
                if item.malformed { " (malformed — shown as-is)" } else { "" }
            ));
        }
        out.push('\n');
    }
    if out.trim().is_empty() {
        return Ok("No family connections with a known identity on this device — pass \"as\" or \
\"family\" explicitly, or configure identity in Families settings."
            .to_string());
    }
    Ok(out)
}

/// `family_send` (task 3.3): deliver one item into `to`'s inbox. **Locked
/// (design D4 / spec "no auto-accept in v1"): this is delivery, not
/// assignment.** The write is exactly lane rule 2 — one new file under
/// `members/<to>/inbox/`, nothing else touched — enforced by
/// `commit_paths`/`lane_check`, not by this function's own care. After the
/// commit, `SyncEngine::poll` fetches, integrates, and pushes so the item
/// actually reaches the recipient's next sync; a push failure degrades to
/// a warning (the commit already happened locally and the next sync will
/// retry it), matching `push_family_board_write`'s posture.
fn family_send_tool(server: &Server, args: &Value) -> Result<String, String> {
    let family_arg = require_str(args, "family")?;
    let to = require_str(args, "to")?;
    let kind_arg = require_str(args, "kind")?;
    let title = require_str(args, "title")?;
    let body = args.get("body").and_then(Value::as_str).unwrap_or("").to_string();
    let as_arg = opt_str(args, "as");

    let kind = InboxKind::parse(&kind_arg)
        .ok_or_else(|| format!("invalid \"kind\" {kind_arg:?} — use task, message, or notification"))?;
    let task = match args.get("task") {
        Some(v) if !v.is_null() => Some(parse_inbox_task_payload(v)?),
        _ => None,
    };

    let conn = find_family_connection(server, &family_arg)?;
    conn.manifest.check_supported().map_err(|e| e.to_string())?;
    if !conn.manifest.has_member(&to) {
        return Err(format!(
            "\"{to}\" is not a member of family \"{}\". Members: {}.",
            conn.manifest.name,
            conn.manifest.member_ids().join(", ")
        ));
    }
    let sender = resolve_family_identity(&conn, as_arg.as_deref())?;

    let id = tasks::new_ulid();
    let (today, time_hhmm) = today_and_time_utc();
    let created = format!("{today}T{time_hhmm}:00Z");
    let new = NewInboxItem {
        id: Some(id.clone()),
        kind: Some(kind),
        from: sender.clone(),
        title: title.clone(),
        body,
        task,
    };
    let content = family::render_inbox_item(&new, &id, &created);
    let file_name = family::inbox_item_file_name(&id, kind, &title);
    let rel_path = format!("{}/{file_name}", family::inbox_rel(&to));

    let mut git = SystemGit::open(&conn.clone_root)
        .map_err(|e| format!("could not open the family clone at {}: {e}", conn.clone_root.display()))?;
    let lane = Lane::member(&sender);
    let mut engine = SyncEngine::new();
    engine
        .commit(
            &mut git,
            &lane,
            &[PendingWrite::new(rel_path.clone(), content)],
            &format!("Ken: deliver {} to {to}", kind.as_str()),
        )
        .map_err(|e| format!("could not deliver to \"{to}\"'s inbox: {e}"))?;
    let report = engine.poll(&mut git);

    let mut msg = format!(
        "Delivered a new {} item to \"{to}\"'s inbox in family \"{}\" ({rel_path}). This only \
places the item in {to}'s inbox — delivery is not assignment or acceptance. {to} (or their Ken) \
must accept a task item in their Inbox (there is no auto-accept) before it becomes a task on their list; a message just \
waits, unread, until {to} reads it.",
        kind.as_str(),
        conn.manifest.name
    );
    match report.state {
        ConnectionState::Idle => msg.push_str(" Pushed to the family remote."),
        other => msg.push_str(&format!(
            " (warning: committed locally but not yet pushed — {other:?}; it will sync on the next poll)"
        )),
    }
    Ok(msg)
}

/// Which project does this call target? Scoped servers always answer with
/// their own; unscoped servers require the `project` argument.
fn resolve_project(server: &Server, args: &Value) -> Result<(Project, Option<String>), String> {
    let requested = args.get("project").and_then(|p| p.as_str()).map(str::trim);

    if let Some(root) = &server.scoped {
        // Another repo of the open workspace, when one is named (by name,
        // folder, id or a ken://<id>/ address): a route_query hit in a code
        // repo is read where it lives, not looked for in this one.
        if let Some(req) = requested.filter(|r| !r.is_empty()) {
            if let Ok(registry) = Registry::load(&server.base_dir) {
                let inside = in_open_workspace(server, &registry);
                let req_id = req.strip_prefix("ken://").unwrap_or(req).split('/').next().and_then(|s| s.parse::<Uuid>().ok());
                let found = registry.projects.iter().find(|p| {
                    p.name.eq_ignore_ascii_case(req) || Some(p.id) == req_id || same_canonical(&p.path, Path::new(req))
                });
                if let Some(entry) = found {
                    if !same_canonical(&entry.path, root) && inside(&entry.path) {
                        if let Ok(p) = Project::open(&entry.path) {
                            return Ok((p, None));
                        }
                    }
                }
            }
        }
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

    // By id too, as the scoped branch above does: a hit's ken://<id>/ address
    // read on an unscoped server failed with "No Ken project matches" (2026-10-06).
    let requested_id = requested.parse::<Uuid>().ok();
    let entry = registry
        .projects
        .iter()
        .find(|p| {
            p.name.eq_ignore_ascii_case(requested)
                || Some(p.id) == requested_id
                || p.path == Path::new(requested)
                || same_canonical(&p.path, Path::new(requested))
        })
        .ok_or_else(|| format!("No Ken project matches {requested:?}. {available}"))?;
    let project = Project::open(&entry.path)
        .map_err(|e| format!("could not open project \"{}\": {e}", entry.name))?;
    Ok((project, None))
}

/// The workspace ken-mcp should read/write memories and the journal for:
/// Ken's registry `lastWorkspace` entry (task brief: "resolve the current
/// workspace as last_workspace's entry; if none, the tools should return a
/// clear 'no workspace' message"). Unlike `resolve_project`, there is no
/// `--project`-style scoping flag for a workspace to fall back on — ken-mcp
/// only ever knows "the workspace Ken last had open," mirroring how the app
/// itself reopens `lastWorkspace` on launch.
/// Where the app keeps the merged graph: beside the last-opened workspace's
/// manifest, else app data (no workspace open) — the same rule as the app's
/// `workspace_kg_root`.
fn kg_root(server: &Server) -> PathBuf {
    resolve_workspace_root(server).unwrap_or_else(|_| server.base_dir.clone())
}

/// Whether a registered project belongs to the workspace open in Ken: its
/// folder is inside the workspace's. With no workspace open, every project
/// does. A search with no project named stays inside the workspace the person
/// has open, so another client's files are never found or cited.
fn in_open_workspace(server: &Server, registry: &Registry) -> impl Fn(&std::path::Path) -> bool {
    let ws = registry.last_workspace.and_then(|id| registry.workspaces.iter().find(|w| w.id == id));
    let root = ws.map(|w| norm_path(&w.path));
    // A workspace set up from picked repos lives in Ken's app data and its
    // repos anywhere: its members count as inside it.
    let members: Vec<String> = ws
        .and_then(|w| ken_core::workspace::Workspace::open(&w.path).ok())
        .map(|o| {
            o.members
                .iter()
                .filter_map(|m| match &m.status {
                    ken_core::workspace::MemberStatus::Ok(p) => Some(norm_path(&p.root)),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let _ = server;
    move |path: &std::path::Path| match &root {
        Some(r) => {
            let p = norm_path(path);
            p == *r || p.starts_with(&format!("{r}/")) || members.iter().any(|m| p == *m || p.starts_with(&format!("{m}/")))
        }
        None => true,
    }
}

/// A path for comparing: forward slashes, no trailing slash, no `\\?\`
/// prefix, and lower case on Windows (its paths ignore case).
fn norm_path(path: &std::path::Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let s = s.strip_prefix("//?/").unwrap_or(&s).trim_end_matches('/').to_string();
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s
    }
}

fn resolve_workspace_root(server: &Server) -> Result<PathBuf, String> {
    let registry = Registry::load(&server.base_dir)
        .map_err(|e| format!("could not read Ken's project registry: {e}"))?;
    let id = registry.last_workspace.ok_or_else(|| {
        "No Ken workspace is open — open a workspace folder in the Ken app first.".to_string()
    })?;
    registry
        .workspaces
        .iter()
        .find(|w| w.id == id)
        .map(|w| w.path.clone())
        .ok_or_else(|| {
            "No Ken workspace is open — open a workspace folder in the Ken app first.".to_string()
        })
}

/// Resolve a project-scoped memory's project (id, root, name) by name from
/// Ken's registry (task brief: "project scope needs the project root from
/// the registry by name") — the same name-or-path matching `resolve_project`
/// uses for the search tools' `project` argument, reused here for
/// `memory_write`'s `scope` argument instead of a dedicated workspace
/// member lookup (design.md doesn't scope memory projects to one open
/// workspace's members; any registered project is a valid memory scope).
/// The id comes along too so the caller can render a `ken://<id>/...`
/// address in its confirmation, same as the workspace-scope branch does.
fn resolve_project_by_name(server: &Server, name: &str) -> Result<(Uuid, PathBuf, String), String> {
    let registry = Registry::load(&server.base_dir)
        .map_err(|e| format!("could not read Ken's project registry: {e}"))?;
    let entry = registry
        .projects
        .iter()
        .find(|p| {
            p.name.eq_ignore_ascii_case(name)
                || p.path == Path::new(name)
                || same_canonical(&p.path, Path::new(name))
        })
        .ok_or_else(|| {
            let names: Vec<String> = registry.projects.iter().map(|p| p.name.clone()).collect();
            let available = if names.is_empty() {
                "No projects are registered yet — open a folder in the Ken app first.".to_string()
            } else {
                format!("Available projects: {}.", names.join(", "))
            };
            format!(
                "\"{name}\" is neither \"workspace\" nor a registered project \
name. {available}"
            )
        })?;
    Ok((entry.id, entry.path.clone(), entry.name.clone()))
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

/// UTC `(YYYY-MM-DD, HH:MM)` — the `today`/`time_hhmm` strings
/// `ken_core::memory::write_memory`/`append_journal` take as
/// caller-supplied arguments rather than reading the wall clock themselves
/// (`memory.rs`'s module doc: "no date/time crate is a ken-core
/// dependency"; neither `ken-core` nor `ken-mcp`'s `Cargo.toml` depends on
/// `chrono`/`time`, and this task adds none). Judgment call, recorded
/// honestly per the task brief: this is **UTC**, not the caller's local
/// time zone — `std::time::SystemTime` has no timezone-aware conversion
/// without a date crate. A journal entry appended late at night in a
/// negative-UTC-offset zone can therefore land under the *next* UTC
/// calendar day's file, and `## HH:MM` headers / a memory's `created`/
/// `updated` fields read as UTC wall-clock, not the agent's local time.
/// Entries stay correctly ordered and internally consistent regardless —
/// only the human-facing date/time label can be a day off from "local
/// today" — so this is acceptable for an MCP sidecar with no UI of its
/// own, not a correctness bug.
/// Today and the time of day on this computer's clock (`YYYY-MM-DD`,
/// `HH:MM`): what the journal and memories are dated with, the same day the
/// app calls today. A team inbox item's `created` stays in UTC
/// ([`today_and_time_utc`]), because teammates read it in other zones.
fn local_today_and_time() -> (String, String) {
    let now = chrono::Local::now();
    (now.format("%Y-%m-%d").to_string(), now.format("%H:%M").to_string())
}

fn today_and_time_utc() -> (String, String) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let hh = secs_of_day / 3600;
    let mm = (secs_of_day % 3600) / 60;
    (format!("{y:04}-{m:02}-{d:02}"), format!("{hh:02}:{mm:02}"))
}

/// Inverse of `ken_core::memory`'s private `days_from_civil` — Howard
/// Hinnant's public-domain `civil_from_days`
/// (https://howardhinnant.github.io/date_algorithms.html), converting a
/// day count since 1970-01-01 back to a proleptic-Gregorian `(y, m, d)`.
/// Not exported from `ken-core` (that module only ever needs the forward
/// direction, to diff two caller-supplied `YYYY-MM-DD` dates for the
/// archive roll) — duplicated here in miniature for this one caller rather
/// than made `pub` there.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (y + if m <= 2 { 1 } else { 0 }, m, d)
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
                ..Default::default()
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
    fn tools_list_names_the_base_tools() {
        let mut fx = fixture(true);
        let reply = call(&mut fx.server, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap();
        let tools = reply["result"]["tools"].as_array().unwrap();
        let names: Vec<_> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            ["find_definition", "find_usages", "file_outline", "related_files", "history", "search_knowledge", "read_document", "list_documents", "list_projects", "kg_search", "semantic_search", "route_query", "memory_write", "journal_append", "task_create", "task_update", "task_list", "ticket_list", "family_list", "family_inbox", "family_send"]
        );
        for t in tools {
            assert!(t["inputSchema"]["type"] == "object", "schema for {}", t["name"]);
        }
    }

    #[test]
    fn code_tools_find_definitions_usages_outlines_and_neighbours() {
        let mut fx = fixture(true);
        std::fs::create_dir_all(fx.root.join("app")).unwrap();
        std::fs::write(fx.root.join("app/store.py"), "# Keeps the world on disk.\ndef save_world(world):\n    write(world)\n\nclass Store:\n    def open(self):\n        pass\n").unwrap();
        std::fs::write(fx.root.join("app/game.py"), "from app.store import save_world\n\ndef tick(world):\n    save_world(world)\n").unwrap();
        let project = Project::open(&fx.root).unwrap();
        let mut db = Db::open(&fx.server.base_dir, project.config.id).unwrap();
        scan::scan(&project, &mut db).unwrap();

        let (text, is_err) = tool(&mut fx.server, "find_definition", json!({"name": "save_world"}));
        assert!(!is_err, "{text}");
        assert!(text.contains("function save_world") && text.contains("app/store.py#L2"), "{text}");
        assert!(text.contains("Keeps the world on disk"), "docs: {text}");

        let (text, _) = tool(&mut fx.server, "find_usages", json!({"name": "save_world"}));
        assert!(text.contains("app/game.py#L4") && text.contains("called from: tick"), "{text}");

        let (text, _) = tool(&mut fx.server, "file_outline", json!({"path": "app/store.py"}));
        assert!(text.contains("class Store") && text.contains("\n  method open") || text.contains("\n  function open"), "{text}");

        let (text, _) = tool(&mut fx.server, "related_files", json!({"path": "app/game.py"}));
        assert!(text.contains("Imports:\n  - ken://") && text.contains("app/store.py"), "{text}");
        let (text, _) = tool(&mut fx.server, "related_files", json!({"path": "app/store.py"}));
        assert!(text.contains("Imported by:\n  - ken://") && text.contains("app/game.py"), "{text}");

        let (text, _) = tool(&mut fx.server, "find_definition", json!({"name": "nothing_like_it"}));
        assert!(text.contains("look directly"), "empty says to look: {text}");
    }

    #[test]
    fn history_answers_what_changed_lately_with_or_without_git() {
        // No file, no words: the question is "what changed this week".
        let mut fx = fixture(true);
        let (text, is_err) = tool(&mut fx.server, "history", json!({"project": "Atlas"}));
        assert!(!is_err, "{text}");
        assert!(text.contains("has no git history") && text.contains("notes/meeting.md"), "a folder without git: {text}");

        let root = fx.root.clone();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(["-c", "user.name=Tess", "-c", "user.email=t@t"])
                .args(args)
                .current_dir(&root)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        git(&["init", "-q"]);
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "the billing notes"]);
        let (text, is_err) = tool(&mut fx.server, "history", json!({"project": "Atlas", "days": 3}));
        assert!(!is_err, "{text}");
        assert!(text.contains("the billing notes") && text.contains("in the last 3 days"), "{text}");
    }

    #[test]
    fn history_reads_a_files_commits_and_commits_about_words() {
        let fx = fixture(true);
        let root = fx.root.clone();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git").args(args).current_dir(&root).output().unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "Tess"]);
        std::fs::write(fx.root.join("auth.py"), "def check():\n    pass\n").unwrap();
        git(&["add", "auth.py"]);
        git(&["commit", "-q", "-m", "add the token check"]);
        std::fs::write(fx.root.join("auth.py"), "def check():\n    verify_signature()\n").unwrap();
        git(&["commit", "-q", "-am", "harden it"]);
        let mut fx = fx;
        let (text, is_err) = tool(&mut fx.server, "history", json!({"path": "auth.py", "project": "Atlas"}));
        assert!(!is_err, "{text}");
        assert!(text.contains("2 commits") && text.contains("harden it") && text.contains("Tess"), "{text}");
        let (text, _) = tool(&mut fx.server, "history", json!({"query": "verify_signature", "project": "Atlas"}));
        assert!(text.contains("harden it") && !text.contains("add the token check"), "by change: {text}");
        let (text, _) = tool(&mut fx.server, "history", json!({"query": "token check", "project": "Atlas"}));
        assert!(text.contains("add the token check"), "by message: {text}");
        // Moved since: the commit names its old path, not a dead address.
        std::fs::create_dir_all(fx.root.join("core")).unwrap();
        git(&["mv", "auth.py", "core/auth.py"]);
        git(&["commit", "-q", "-m", "move auth under core"]);
        let (text, _) = tool(&mut fx.server, "history", json!({"query": "token check", "project": "Atlas"}));
        assert!(text.contains("auth.py (its path then; not there now)"), "{text}");
        assert!(!text.contains("/auth.py#") && !text.ends_with("/auth.py"), "{text}");
    }

    #[test]
    fn hits_are_tagged_by_kind_and_a_kind_filter_keeps_only_that() {
        let mut fx = fixture(true);
        let (text, is_err) = tool(&mut fx.server, "search_knowledge", json!({"query": "billing"}));
        assert!(!is_err, "{text}");
        assert!(text.contains("[meeting] notes/meeting.md"), "a meeting note tagged: {text}");
        let (text, _) = tool(&mut fx.server, "search_knowledge", json!({"query": "billing", "type": "code"}));
        assert!(!text.contains("notes/meeting.md"), "code only: {text}");
        let (text, _) = tool(&mut fx.server, "search_knowledge", json!({"query": "billing", "type": "meeting,doc"}));
        assert!(text.contains("notes/meeting.md"), "{text}");
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

    // --- kg-routing (task 3.5) ---

    /// Builds a real (if tiny) semantic index with `FakeEmbedder` — the same
    /// production `engine::rebuild_semantic_index` src-tauri's semantic-index
    /// command uses, just with the deterministic no-model embedder tests get
    /// everywhere else in ken-core. Needed because `routing::MemberInfo::
    /// index_ready`/`execute_plan` gate *execution* on `db.vec_available()`
    /// (mirrored by `route_query` below), so an FTS-only fixture Db would
    /// make every route_query test see every member as `index-building` and
    /// return zero hits — not what task 3.5 needs to test the happy path.
    fn index_semantically(project: &Project, db: &mut Db) {
        let mut embedder = ken_core::embedder::FakeEmbedder::new();
        ken_core::engine::rebuild_semantic_index(
            project,
            db,
            &mut embedder,
            &ken_core::runner::CancelToken::new(),
            |_, _| {},
        )
        .unwrap();
    }

    /// Two registered, indexed (FTS + fake-semantic) projects sharing one
    /// base dir, unscoped, with `workspace`+`kgRouting` on — what the
    /// flag-on tool-visibility and per-project-isolation tests below need.
    /// Tempdirs are returned so the caller keeps them alive for the test's
    /// duration (same idiom as `Fixture`'s `_base`/`_root` fields).
    fn two_project_fixture() -> (tempfile::TempDir, tempfile::TempDir, tempfile::TempDir, Server) {
        let base = tempfile::tempdir().unwrap();
        let root_a = tempfile::tempdir().unwrap();
        let root_b = tempfile::tempdir().unwrap();
        std::fs::write(root_a.path().join("a.md"), "Atlas notes about the launch schedule.\n").unwrap();
        std::fs::write(root_b.path().join("b.md"), "Zeus notes about the launch schedule.\n").unwrap();
        let project_a = Project::create(root_a.path(), "Atlas").unwrap();
        let project_b = Project::create(root_b.path(), "Zeus").unwrap();
        let mut db_a = Db::open(base.path(), project_a.config.id).unwrap();
        scan::scan(&project_a, &mut db_a).unwrap();
        index_semantically(&project_a, &mut db_a);
        let mut db_b = Db::open(base.path(), project_b.config.id).unwrap();
        scan::scan(&project_b, &mut db_b).unwrap();
        index_semantically(&project_b, &mut db_b);
        let mut registry = Registry::default();
        registry.add(&project_a);
        registry.add(&project_b);
        registry.save(base.path()).unwrap();

        let mut settings = AppSettings::default();
        settings.features.insert("workspace".into(), true.into());
        settings.features.insert("kgRouting".into(), true.into());
        settings.save(base.path()).unwrap();

        let server = Server {
            base_dir: base.path().to_path_buf(),
            scoped: None,
            ..Default::default()
        };
        (base, root_a, root_b, server)
    }

    #[test]
    fn kg_routing_tools_appear_and_route_query_returns_a_named_plan_when_flag_on() {
        let (_base, _ra, _rb, mut server) = two_project_fixture();
        let reply = call(&mut server, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap();
        let tools = reply["result"]["tools"].as_array().unwrap();
        let names: Vec<_> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            [
                "find_definition",
                "find_usages",
                "file_outline",
                "related_files",
                "history",
                "search_knowledge",
                "read_document",
                "list_documents",
                "list_projects",
                "kg_search",
                "semantic_search",
                "route_query",
                "memory_write",
                "journal_append",
                "task_create",
                "task_update",
                "task_list",
                "ticket_list",
                "family_list",
                "family_inbox",
                "family_send"
            ]
        );
        for t in tools {
            assert_eq!(t["inputSchema"]["type"], "object", "schema for {}", t["name"]);
            assert!(t["description"].as_str().is_some_and(|d| !d.is_empty()), "{}", t["name"]);
        }

        // "Atlas" names a member, so this is a Named-tier plan targeting
        // only Atlas — no KG lookup, no broadcast.
        let (text, is_err) = tool(&mut server, "route_query", json!({"query": "Atlas launch schedule"}));
        assert!(!is_err, "{text}");
        assert!(text.starts_with("Route: named project match"), "{text}");
        assert!(text.contains("a.md"), "{text}");
        assert!(!text.contains("b.md"), "{text}");
    }

    #[test]
    fn open_in_ken_asks_the_app_with_its_token() {
        let (_base, _ra, _rb, server) = two_project_fixture();
        let listener = ken_core::hooks::HookListener::start().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        listener.set_ui_handler(move |v| {
            let _ = tx.send(v);
        });
        let bridge = (listener.ui_url(), listener.token().to_string());
        let said = open_in_ken_via(&server, &json!({"target": "ken://abc-123/Platform/Save.md#L12"}), &bridge).unwrap();
        assert_eq!(said, "Opened Platform/Save.md at line 12 in Ken.");
        let v = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert_eq!((v["projectId"].as_str(), v["path"].as_str(), v["line"].as_u64()), (Some("abc-123"), Some("Platform/Save.md"), Some(12)));

        let wrong = (listener.ui_url(), "not-the-token".to_string());
        assert!(open_in_ken_via(&server, &json!({"target": "ken://abc-123/a.md"}), &wrong).is_err());
    }

    #[test]
    fn semantic_search_with_explicit_project_touches_only_that_member() {
        let (_base, _ra, _rb, mut server) = two_project_fixture();
        let (text, is_err) = tool(
            &mut server,
            "semantic_search",
            json!({"query": "launch schedule", "project": "Atlas"}),
        );
        assert!(!is_err, "{text}");
        assert!(text.contains("project \"Atlas\""), "{text}");
        assert!(text.contains("a.md"), "{text}");
        assert!(!text.contains("b.md"), "{text}");
        assert!(text.contains("ken://"), "{text}");

        let (text, is_err) = tool(
            &mut server,
            "semantic_search",
            json!({"query": "launch schedule", "project": "Zeus"}),
        );
        assert!(!is_err, "{text}");
        assert!(text.contains("project \"Zeus\""), "{text}");
        assert!(text.contains("b.md"), "{text}");
        assert!(!text.contains("a.md"), "{text}");
    }

    #[test]
    fn search_uses_meaning_when_the_model_is_there_and_says_so_when_not() {
        let (_base, _ra, _rb, mut server) = two_project_fixture();
        // No model installed: keyword only, in one line.
        let (text, is_err) = tool(&mut server, "semantic_search", json!({"query": "launch schedule", "project": "Atlas"}));
        assert!(!is_err, "{text}");
        assert!(text.contains("Keyword search only: no embedding model is installed"), "{text}");
        assert!(text.contains("[keyword]") && !text.contains("semantic]"), "{text}");
        let (text, _) = tool(&mut server, "route_query", json!({"query": "launch schedule"}));
        assert!(text.starts_with("Route: "), "the note follows the route: {text}");
        assert!(text.contains("\nKeyword search only:"), "{text}");

        // The model the indexes were built with: meaning joins keyword.
        server.meaning = RefCell::new(Meaning::Ready(Box::new(ken_core::embedder::FakeEmbedder::new())));
        let (text, _) = tool(&mut server, "semantic_search", json!({"query": "launch schedule", "project": "Atlas"}));
        assert!(!text.contains("Keyword search only"), "{text}");
        assert!(text.contains("[keyword+semantic]"), "{text}");
        let (text, _) = tool(&mut server, "route_query", json!({"query": "launch schedule"}));
        assert!(!text.contains("Keyword search only"), "{text}");
        assert!(text.contains("semantic]"), "{text}");
    }

    #[test]
    fn a_vector_goes_only_to_an_index_built_by_its_model() {
        let (base, _ra, _rb, server) = two_project_fixture();
        let registry = Registry::load(&server.base_dir).unwrap();
        let db = Db::open(base.path(), registry.projects[0].id).unwrap();
        let qv: QueryVector = Ok((vec![0.0; 8], "fake-hash-8".to_string()));
        assert!(vector_for(&db, &qv).is_some());
        let other: QueryVector = Ok((vec![0.0; 768], "nomic-embed-text-v1.5".to_string()));
        assert!(vector_for(&db, &other).is_none());
        assert!(vector_for(&db, &Err("off".into())).is_none());
    }

    #[test]
    fn a_snippet_is_the_best_lines_and_about_300_characters() {
        let mut text = String::new();
        for i in 1..=60 {
            text.push_str(&format!("    line {i} says nothing in particular about anything here\n"));
        }
        text.push_str("    fn grace_period() { // offline players keep their party\n");
        for i in 62..=120 {
            text.push_str(&format!("    line {i} says nothing in particular about anything here\n"));
        }
        let (snip, best) = snippet_around(&text, "how long is the offline grace for players", None, SNIPPET_CHARS);
        assert_eq!(best, 60, "{snip}");
        assert!(snip.contains("fn grace_period()"), "{snip}");
        assert!(snip.chars().count() <= SNIPPET_CHARS + 10, "{} chars: {snip}", snip.chars().count());
        assert!(!snip.starts_with(' '), "dedented: {snip}");
        // A line the hit names is shown instead of the best match.
        let (snip, best) = snippet_around(&text, "offline grace", Some(3), SNIPPET_CHARS);
        assert_eq!(best, 3);
        assert!(snip.contains("line 4 says"), "{snip}");
        // One enormous line is cut around its match.
        let blob = format!("{}needle{}", "x".repeat(5000), "y".repeat(5000));
        let (snip, _) = snippet_around(&blob, "needle", None, SNIPPET_CHARS);
        assert!(snip.contains("needle") && snip.chars().count() <= SNIPPET_CHARS + 2, "{snip}");
        // A heading that matched brings the start of its long paragraph.
        let page = format!("## Grace for players\n\n{}\n", "Offline party members keep their place for a while. ".repeat(20));
        let (snip, _) = snippet_around(&page, "grace players", None, SNIPPET_CHARS);
        assert!(snip.starts_with("## Grace for players\nOffline party members"), "{snip}");
        assert!(snip.ends_with('…') && snip.chars().count() <= SNIPPET_CHARS + 2, "{snip}");
        assert_eq!(stem("validated"), "valida");
        assert_eq!(stem("party"), "party");
    }

    #[test]
    fn route_query_hits_are_short_and_point_at_the_matching_line() {
        let (_base, ra, _rb, mut server) = two_project_fixture();
        let mut long = String::from("# Launch\n\n");
        for i in 0..40 {
            long.push_str(&format!("Paragraph {i} of filler text that talks about nothing at all, at some length.\n\n"));
        }
        long.push_str("The rollback window for the launch is ninety minutes.\n");
        std::fs::write(ra.path().join("plan.md"), &long).unwrap();
        let project = Project::open(ra.path()).unwrap();
        let mut db = Db::open(&server.base_dir, project.config.id).unwrap();
        scan::scan(&project, &mut db).unwrap();
        index_semantically(&project, &mut db);
        drop(db);

        let (text, is_err) = tool(&mut server, "route_query", json!({"query": "Atlas rollback window ninety minutes"}));
        assert!(!is_err, "{text}");
        assert!(text.contains("plan.md:83 ("), "the line of the match, not the chunk's: {text}");
        assert!(text.contains("plan.md#L83)"), "{text}");
        assert!(text.contains("\n   The rollback window for the launch is ninety minutes."), "{text}");
        assert!(text.len() < 2_000, "{} characters: {text}", text.len());
    }

    #[test]
    fn every_page_hit_says_what_it_is_and_how_fresh() {
        let (_base, ra, _rb, mut server) = two_project_fixture();
        std::fs::create_dir_all(ra.path().join("tickets")).unwrap();
        std::fs::write(
            ra.path().join("tickets/AT-7.md"),
            "---\nstatus: cancelled\nupdated: 2026-07-21\n---\n# Companion pets\n\nCompanion pets that follow the player.\n",
        )
        .unwrap();
        std::fs::create_dir_all(ra.path().join("decisions")).unwrap();
        let mut log = String::from("# DECISIONS\n\n## THE LOG\n\n### 2026-10-06\n\n");
        for i in (1..=6).rev() {
            log.push_str(&format!("**D-{i:03}** · 2026-10-0{i} · pets — **RULING {i} ON SADDLES.** Chris, in chat.\n\n"));
        }
        log.push_str("**D-007** · 2026-10-07 · pets — **COMPANION PETS FOLLOW THE PLAYER.** Chris, in chat.\n");
        std::fs::write(ra.path().join("decisions/DECISIONS.md"), &log).unwrap();
        let project = Project::open(ra.path()).unwrap();
        let mut db = Db::open(&server.base_dir, project.config.id).unwrap();
        scan::scan(&project, &mut db).unwrap();
        index_semantically(&project, &mut db);
        drop(db);

        let (text, is_err) = tool(&mut server, "route_query", json!({"query": "companion pets follow the player"}));
        assert!(!is_err, "{text}");
        assert!(text.contains("tickets/AT-7.md"), "{text}");
        assert!(text.contains("(ticket · cancelled · 2026-07-21)"), "{text}");
        assert!(text.contains("(ruling D-007 · 2026-10-07)"), "the entry's own id and date: {text}");
        let (text, _) = tool(&mut server, "semantic_search", json!({"query": "companion pets follow the player", "project": "Atlas"}));
        assert!(text.contains("(ticket · cancelled · 2026-07-21)") && text.contains("(ruling D-007 · 2026-10-07)"), "{text}");
        let (text, _) = tool(&mut server, "search_knowledge", json!({"query": "companion pets", "project": "Atlas"}));
        assert!(text.contains("(ticket · cancelled · 2026-07-21) — "), "{text}");
        assert!(text.contains("decisions/DECISIONS.md (ken://") && text.contains(") (ruling) — "), "the log as a whole: {text}");
    }

    #[test]
    fn search_knowledge_falls_back_to_the_closest_passages_for_a_question() {
        let mut fx = fixture(true);
        // No file has "owns", "cutover" and "date" together.
        let (text, is_err) = tool(&mut fx.server, "search_knowledge", json!({"query": "Who owns the cutover date?"}));
        assert!(!is_err, "{text}");
        assert!(text.contains("has every word"), "{text}");
        assert!(text.contains("Keyword search only:"), "{text}");
        assert!(text.contains(". [doc] People.md (ken://"), "same line shape: {text}");
        assert!(text.contains("notes/meeting.md"), "{text}");
        // All words in one file: the file-level answer, unchanged.
        let (text, _) = tool(&mut fx.server, "search_knowledge", json!({"query": "billing cutover"}));
        assert!(!text.contains("has every word"), "{text}");
        // Words in no file at all: still empty.
        let (text, _) = tool(&mut fx.server, "search_knowledge", json!({"query": "zebra xylophone"}));
        assert!(text.starts_with("No matches"), "{text}");
    }

    #[test]
    fn read_document_reads_lines_around_an_address_or_a_range() {
        let mut fx = fixture(true);
        let body: String = (1..=100).map(|i| format!("row {i}\n")).collect();
        std::fs::write(fx.root.join("rows.txt"), &body).unwrap();
        let project = Project::open(&fx.root).unwrap();
        let mut db = Db::open(&fx.server.base_dir, project.config.id).unwrap();
        scan::scan(&project, &mut db).unwrap();
        drop(db);
        let address = format!("ken://{}/rows.txt#L50", project.config.id);

        let (text, is_err) = tool(&mut fx.server, "read_document", json!({"path": address}));
        assert!(!is_err, "{text}");
        assert!(text.contains("[lines 40-90 of 100]\n40: row 40\n"), "{text}");
        assert!(text.ends_with("90: row 90\n"), "{text}");

        let (text, _) = tool(&mut fx.server, "read_document", json!({"path": "rows.txt", "start_line": 3, "end_line": 4}));
        assert_eq!(text, "[lines 3-4 of 100]\n3: row 3\n4: row 4\n");
        let (text, _) = tool(&mut fx.server, "read_document", json!({"path": "rows.txt#L98-L120"}));
        assert!(text.starts_with("[lines 98-100 of 100]"), "{text}");
        let (text, _) = tool(&mut fx.server, "read_document", json!({"path": "rows.txt", "start_line": 500}));
        assert!(text.contains("no line 500"), "{text}");
        // No range: the whole file, as before.
        let (text, _) = tool(&mut fx.server, "read_document", json!({"path": "rows.txt"}));
        assert_eq!(text, body);

        // An unscoped server finds the project by the address's id.
        fx.server.scoped = None;
        let (text, is_err) = tool(&mut fx.server, "read_document", json!({"path": address}));
        assert!(!is_err, "{text}");
        assert!(text.starts_with("[lines 40-90 of 100]"), "{text}");
    }

    #[test]
    fn read_document_reads_the_section_or_log_entry_an_address_is_in() {
        let mut fx = fixture(true);
        let mut page = String::from("---\ntitle: Law\n---\n\n# Law\n\n## Grades\n\n");
        for i in 1..=60 {
            page.push_str(&format!("Grade lever {i}.\n"));
        }
        page.push_str("\n## Health\n\nLast and least.\n\n## Long\n\n");
        for i in 1..=400 {
            page.push_str(&format!("A long line of the long section, number {i}, padded out.\n"));
        }
        std::fs::write(fx.root.join("Law.md"), &page).unwrap();
        let mut log = String::from("# DECISIONS\n\n## THE LOG\n\n### 2026-10-06\n\n");
        for i in (1..=6).rev() {
            log.push_str(&format!("**D-{i:03}** · 2026-10-06 · pets — **RULING {i}.** Chris.\n\n"));
        }
        std::fs::write(fx.root.join("DECISIONS.md"), &log).unwrap();
        std::fs::write(fx.root.join("code.rs"), (1..=100).map(|i| format!("// line {i}\n")).collect::<String>()).unwrap();
        let project = Project::open(&fx.root).unwrap();
        let mut db = Db::open(&fx.server.base_dir, project.config.id).unwrap();
        scan::scan(&project, &mut db).unwrap();
        drop(db);

        // Line 65 is the 57th of 60 levers: the whole section, not 10 lines before and 40 after.
        let (text, is_err) = tool(&mut fx.server, "read_document", json!({"path": "Law.md#L65"}));
        assert!(!is_err, "{text}");
        assert!(text.starts_with("[section \"Grades\": lines 7-68 of"), "{text}");
        assert!(text.contains("\n9: Grade lever 1.\n") && text.ends_with("68: Grade lever 60.\n"), "{text}");
        assert!(!text.contains("Health"), "{text}");
        // A long section is cut at about 8,000 characters, and says where to read on.
        let (text, _) = tool(&mut fx.server, "read_document", json!({"path": "Law.md#L300"}));
        assert!(text.contains("cut at 8000 characters: read on with start_line="), "{text}");
        assert!(text.contains("\n300: A long line") && text.len() < 8_400, "{} characters", text.len());
        // A decisions log: the entry.
        let (text, _) = tool(&mut fx.server, "read_document", json!({"path": "DECISIONS.md#L9"}));
        assert_eq!(text, "[entry D-005: lines 9-9 of 18]\n9: **D-005** · 2026-10-06 · pets — **RULING 5.** Chris.\n");
        // Code keeps the line window; an explicit range still wins.
        let (text, _) = tool(&mut fx.server, "read_document", json!({"path": "code.rs#L50"}));
        assert!(text.starts_with("[lines 40-90 of 100]"), "{text}");
        let (text, _) = tool(&mut fx.server, "read_document", json!({"path": "Law.md#L65", "start_line": 65, "end_line": 66}));
        assert_eq!(text, "[lines 65-66 of 475]\n65: Grade lever 57.\n66: Grade lever 58.\n");
        let (text, _) = tool(&mut fx.server, "read_document", json!({"path": "Law.md#L65-L66"}));
        assert!(text.starts_with("[lines 65-66 of"), "{text}");
    }

    #[test]
    fn a_superseded_entry_says_so_in_its_hit_and_when_read() {
        let (_base, ra, _rb, mut server) = two_project_fixture();
        std::fs::create_dir_all(ra.path().join("decisions")).unwrap();
        let mut log = String::from("# DECISIONS\n\n## THE LOG\n\n### 2026-10-06\n\n");
        log.push_str("**D-410** · 2026-10-06 · anchors, tools — **ANCHORS ARE PLACED ONLY THROUGH THE TOOLS; THE MERGE FIX IS NOT BUILT.** Chris.\n\n");
        for i in (1..=5).rev() {
            log.push_str(&format!("**D-{:03}** · 2026-09-0{i} · lore — **THE MAP HAS {i} REGIONS.** Chris.\n\n", 380 + i));
        }
        log.push_str("**D-379** · 2026-10-05 · spawn anchors, world tool — **THE SERVER NEVER OVERWRITES SPAWN ANCHORS; BUILD THE MERGE.** Chris.\n");
        std::fs::write(ra.path().join("decisions/DECISIONS.md"), &log).unwrap();
        let project = Project::open(ra.path()).unwrap();
        let mut db = Db::open(&server.base_dir, project.config.id).unwrap();
        scan::scan(&project, &mut db).unwrap();
        index_semantically(&project, &mut db);
        let pair = ken_core::supersede::Supersession {
            log: "decisions/DECISIONS.md".into(),
            earlier: "D-379".into(),
            later: "D-410".into(),
            relation: "supersedes".into(),
            point: "who writes anchors".into(),
            evidence: "placed only through the tools".into(),
            earlier_line: 19,
            later_line: 7,
            source: "judged".into(),
        };
        db.replace_supersessions("decisions/DECISIONS.md", &[pair]).unwrap();
        drop(db);

        // The search finds D-379 by its words; D-410 comes in just above it.
        let (text, is_err) = tool(&mut server, "semantic_search", json!({"query": "server never overwrites spawn anchors merge", "project": "Atlas"}));
        assert!(!is_err, "{text}");
        let earlier = text.find("(ruling D-379 · 2026-10-05 · superseded by D-410 on who writes anchors)").expect(&text);
        let later = text.find("(ruling D-410 · 2026-10-06 · supersedes D-379)").expect(&text);
        assert!(later < earlier, "the later ruling ranks first: {text}");
        let (text, _) = tool(&mut server, "route_query", json!({"query": "server never overwrites spawn anchors merge"}));
        assert!(text.contains("superseded by D-410 on who writes anchors"), "{text}");

        let (text, _) = tool(&mut server, "read_document", json!({"path": "decisions/DECISIONS.md#L19", "project": "Atlas"}));
        assert!(text.starts_with("[entry D-379: lines 19-19 of 19]"), "{text}");
        assert!(text.ends_with("[D-410 (line 7) supersedes this entry on who writes anchors]\n"), "{text}");
        let (text, _) = tool(&mut server, "read_document", json!({"path": "decisions/DECISIONS.md#L7", "project": "Atlas"}));
        assert!(text.ends_with("[this entry supersedes D-379 (line 19) on who writes anchors]\n"), "{text}");
    }

    #[test]
    fn list_projects_says_which_projects_routed_search_can_use() {
        // Routed search is built in: on whatever settings.json says.
        let mut fx = fixture(true);
        let mut settings = AppSettings::default();
        settings.features.insert("kgRouting".into(), false.into());
        settings.save(&fx.server.base_dir).unwrap();
        let (text, is_err) = tool(&mut fx.server, "list_projects", json!({}));
        assert!(!is_err, "{text}");
        assert!(text.contains("search ready"), "{text}");
    }

    // --- ken-memory (task 3.2) ---

    /// `workspace` on, one registered workspace (`last_workspace`
    /// resolved) and one registered project ("Atlas") — what the memory-tool
    /// tests below need for both scope kinds. Tempdirs are returned so the
    /// caller keeps them alive for the test's duration (same idiom as
    /// `Fixture`'s `_base`/`_root` fields and `two_project_fixture`).
    fn memory_fixture() -> (tempfile::TempDir, tempfile::TempDir, tempfile::TempDir, Server) {
        let base = tempfile::tempdir().unwrap();
        let ws_parent = tempfile::tempdir().unwrap();
        let proj_root = tempfile::tempdir().unwrap();

        let workspace = ken_core::workspace::Workspace::create(ws_parent.path(), "WS", &[]).unwrap();
        let project = Project::create(proj_root.path(), "Atlas").unwrap();

        let mut registry = Registry::default();
        registry.add(&project);
        registry.add_workspace(&workspace, None, 0);
        registry.last_workspace = Some(workspace.config.id);
        registry.save(base.path()).unwrap();

        let mut settings = AppSettings::default();
        settings.features.insert("workspace".into(), true.into());
        settings.save(base.path()).unwrap();

        let server = Server { base_dir: base.path().to_path_buf(), scoped: None, ..Default::default() };
        (base, ws_parent, proj_root, server)
    }

    #[test]
    fn memory_tools_are_always_listed() {
        // Default settings: Ken's memory is built in, so no flag is needed.
        let mut fx = fixture(true);
        let reply = call(&mut fx.server, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap();
        let names: Vec<_> = reply["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert!(names.contains(&"memory_write".to_string()), "{names:?}");
        assert!(names.contains(&"journal_append".to_string()), "{names:?}");
    }

    #[test]
    fn memory_tools_appear_with_valid_schemas_when_flag_on() {
        let (_base, _ws, _proj, mut server) = memory_fixture();
        let reply = call(&mut server, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap();
        let tools = reply["result"]["tools"].as_array().unwrap();
        let names: Vec<_> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"memory_write"), "{names:?}");
        assert!(names.contains(&"journal_append"), "{names:?}");

        let mw = tools.iter().find(|t| t["name"] == "memory_write").unwrap();
        assert_eq!(mw["inputSchema"]["required"], json!(["scope", "slug", "content"]));
        let ja = tools.iter().find(|t| t["name"] == "journal_append").unwrap();
        assert_eq!(ja["inputSchema"]["required"], json!(["text"]));
        for t in tools {
            assert_eq!(t["inputSchema"]["type"], "object", "schema for {}", t["name"]);
            assert!(t["description"].as_str().is_some_and(|d| !d.is_empty()), "{}", t["name"]);
        }
    }

    #[test]
    fn memory_write_lands_in_workspace_and_project_memory_dirs() {
        let (_base, ws_parent, proj_root, mut server) = memory_fixture();

        let (text, is_err) = tool(
            &mut server,
            "memory_write",
            json!({"scope": "workspace", "slug": "ways-of-working", "content": "Ship small PRs."}),
        );
        assert!(!is_err, "{text}");
        let ws_path = ws_parent.path().join(".ken-workspace/memory/ways-of-working.md");
        assert!(ws_path.is_file(), "{}", ws_path.display());
        let raw = std::fs::read_to_string(&ws_path).unwrap();
        assert!(raw.contains("Ship small PRs."), "{raw}");
        assert!(text.contains("ken://workspace/memory/ways-of-working.md"), "{text}");

        // Create-mode collision errors and leaves the existing file untouched.
        let (text2, is_err2) = tool(
            &mut server,
            "memory_write",
            json!({"scope": "workspace", "slug": "ways-of-working", "content": "Different."}),
        );
        assert!(is_err2, "{text2}");
        assert_eq!(std::fs::read_to_string(&ws_path).unwrap(), raw, "collision must not overwrite");

        // Explicit replace mode swaps the body.
        let (text3, is_err3) = tool(
            &mut server,
            "memory_write",
            json!({"scope": "workspace", "slug": "ways-of-working", "content": "Updated body.", "mode": "replace"}),
        );
        assert!(!is_err3, "{text3}");
        assert!(std::fs::read_to_string(&ws_path).unwrap().contains("Updated body."));

        // Project scope, resolved by registry name.
        let (text4, is_err4) = tool(
            &mut server,
            "memory_write",
            json!({"scope": "Atlas", "slug": "conventions", "content": "One mob per file."}),
        );
        assert!(!is_err4, "{text4}");
        let proj_path = proj_root.path().join(".ken/memory/conventions.md");
        assert!(proj_path.is_file(), "{}", proj_path.display());
        assert!(std::fs::read_to_string(&proj_path).unwrap().contains("One mob per file."));
        assert!(text4.contains("ken://") && text4.contains(".ken/memory/conventions.md"), "{text4}");

        // An unrecognized scope is a helpful error, not a panic or a
        // silent workspace-folder write.
        let (text5, is_err5) =
            tool(&mut server, "memory_write", json!({"scope": "Nope", "slug": "x", "content": "y"}));
        assert!(is_err5);
        assert!(text5.contains("Nope"), "{text5}");
    }

    #[test]
    fn journal_append_lands_in_workspace_journal_dir() {
        let (_base, ws_parent, _proj, mut server) = memory_fixture();
        let (text, is_err) = tool(
            &mut server,
            "journal_append",
            json!({"text": "Completed the mob-loot audit.", "project": "Atlas", "tags": ["audit", "mobs"]}),
        );
        assert!(!is_err, "{text}");

        let journal_dir = ws_parent.path().join(".ken-workspace/journal");
        let entries: Vec<_> = std::fs::read_dir(&journal_dir).unwrap().flatten().collect();
        assert_eq!(entries.len(), 1, "expected exactly one journal file, today's");
        let raw = std::fs::read_to_string(entries[0].path()).unwrap();
        assert!(raw.starts_with("## "), "{raw}");
        assert!(raw.contains("Project: Atlas"), "{raw}");
        assert!(raw.contains("Tags: audit, mobs"), "{raw}");
        assert!(raw.contains("Completed the mob-loot audit."), "{raw}");
    }

    #[test]
    fn memory_tools_require_a_workspace_to_be_open() {
        // No workspace was ever registered/opened — no
        // `last_workspace` to resolve, so both tools must fail with a clear
        // message instead of panicking on a missing registry entry.
        let base = tempfile::tempdir().unwrap();
        let mut settings = AppSettings::default();
        settings.features.insert("workspace".into(), true.into());
        settings.save(base.path()).unwrap();
        let mut server = Server { base_dir: base.path().to_path_buf(), scoped: None, ..Default::default() };

        let (text, is_err) = tool(&mut server, "journal_append", json!({"text": "hi"}));
        assert!(is_err, "{text}");
        assert!(text.contains("No Ken workspace is open"), "{text}");

        let (text, is_err) = tool(
            &mut server,
            "memory_write",
            json!({"scope": "workspace", "slug": "x", "content": "y"}),
        );
        assert!(is_err, "{text}");
        assert!(text.contains("No Ken workspace is open"), "{text}");
    }

    // --- Your day tools ---

    /// One registered workspace (`last_workspace` resolved) whose one repo
    /// member, "atlas", is also a registered project and has a tickets
    /// folder.
    fn task_fixture() -> (tempfile::TempDir, tempfile::TempDir, Server) {
        let base = tempfile::tempdir().unwrap();
        let ws_parent = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(ws_parent.path().join("atlas/tickets")).unwrap();
        let workspace = ken_core::workspace::Workspace::create(ws_parent.path(), "WS", &["atlas".to_string()]).unwrap();
        let project = Project::open(&ws_parent.path().join("atlas")).unwrap();
        std::fs::write(
            ws_parent.path().join("atlas/tickets/ATT-014.md"),
            "---\nid: ATT-014\ntitle: Retry rule for the scheduler\nstatus: in progress\nassignee: nobody-here\ntarget: '2026-10-01'\n---\n\n# Retry rule\n",
        )
        .unwrap();
        std::fs::write(
            ws_parent.path().join("atlas/tickets/ATT-009.md"),
            "---\nstatus: done\nassignee: nobody-here\n---\n\n# Glossary names\n",
        )
        .unwrap();

        let mut registry = Registry::default();
        registry.add(&project);
        registry.add_workspace(&workspace, None, 0);
        registry.last_workspace = Some(workspace.config.id);
        registry.save(base.path()).unwrap();

        let server = Server { base_dir: base.path().to_path_buf(), scoped: None, ..Default::default() };
        (base, ws_parent, server)
    }

    fn only_task_file(ws_parent: &Path) -> PathBuf {
        let dir = ws_parent.join(".ken-workspace/tasks");
        let entries: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.path().is_file())
            .collect();
        assert_eq!(entries.len(), 1, "expected exactly one task file");
        entries[0].path()
    }

    fn id_of(raw: &str) -> String {
        raw.lines()
            .find(|l| l.starts_with("id:"))
            .unwrap()
            .trim_start_matches("id:")
            .trim()
            .trim_matches(|c| c == '\'' || c == '"')
            .to_string()
    }

    #[test]
    fn task_tools_have_valid_schemas() {
        let (_base, _ws, mut server) = task_fixture();
        let reply = call(&mut server, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap();
        let tools = reply["result"]["tools"].as_array().unwrap();
        let names: Vec<_> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        for n in ["task_create", "task_update", "task_list", "ticket_list"] {
            assert!(names.contains(&n), "{names:?}");
        }
        for gone in ["task_complete", "task_claim", "goal_create", "goal_list"] {
            assert!(!names.contains(&gone), "{gone} is gone: {names:?}");
        }
        let tc = tools.iter().find(|t| t["name"] == "task_create").unwrap();
        assert_eq!(tc["inputSchema"]["required"], json!(["title"]));
        assert!(tc["inputSchema"]["properties"]["links"].is_object());
        let tu = tools.iter().find(|t| t["name"] == "task_update").unwrap();
        assert_eq!(tu["inputSchema"]["required"], json!(["id"]));
        assert_eq!(tu["inputSchema"]["properties"]["state"]["enum"], json!(["open", "done"]));
        assert!(tu["description"].as_str().unwrap().contains("journal"));
        let tl = tools.iter().find(|t| t["name"] == "task_list").unwrap();
        assert!(tl["inputSchema"]["properties"]["target_before"].is_object());
        let tk = tools.iter().find(|t| t["name"] == "ticket_list").unwrap();
        assert!(tk["description"].as_str().unwrap().contains("Read-only"));
        for t in tools {
            assert_eq!(t["inputSchema"]["type"], "object", "schema for {}", t["name"]);
            assert!(t["description"].as_str().is_some_and(|d| !d.is_empty()), "{}", t["name"]);
        }
    }

    #[test]
    fn task_create_writes_the_workspace_home() {
        let (_base, ws_parent, mut server) = task_fixture();
        let (text, is_err) = tool(
            &mut server,
            "task_create",
            json!({"title": "Update the retry test", "target": "2026-10-02", "description": "Assert the delays.", "links": ["ATT-014"]}),
        );
        assert!(!is_err, "{text}");
        assert!(text.contains("ken://workspace/tasks/"), "{text}");
        let raw = std::fs::read_to_string(only_task_file(ws_parent.path())).unwrap();
        assert!(raw.contains("status: open"), "{raw}");
        assert!(raw.contains("target: '2026-10-02'"), "{raw}");
        assert!(raw.contains("links:\n  - ATT-014"), "{raw}");
        assert!(raw.contains("updated_by: mcp"), "{raw}");
        assert!(raw.contains("Assert the delays."), "{raw}");

        let (bad, bad_err) = tool(&mut server, "task_create", json!({"title": "x", "target": "Friday"}));
        assert!(bad_err, "{bad}");
        let (bad, bad_err) = tool(&mut server, "task_create", json!({"title": "x", "repeat": "yearly"}));
        assert!(bad_err, "{bad}");
    }

    #[test]
    fn task_update_changes_named_fields_and_journals_done() {
        let (_base, ws_parent, mut server) = task_fixture();
        let (text, is_err) = tool(&mut server, "task_create", json!({"title": "Audit loot tables", "target": "2026-10-02"}));
        assert!(!is_err, "{text}");
        let path = only_task_file(ws_parent.path());
        let created = std::fs::read_to_string(&path).unwrap();
        let hand = created.replacen("status: open\n", "status: open\ncustom_field: keep-me\n", 1);
        std::fs::write(&path, &hand).unwrap();
        let id = id_of(&hand);

        let (text, is_err) = tool(&mut server, "task_update", json!({"id": id, "target": "", "state": "done"}));
        assert!(!is_err, "{text}");
        assert!(text.contains("Journal entry recorded"), "{text}");
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("custom_field: keep-me"), "{after}");
        assert!(after.contains("status: done"), "{after}");
        assert!(after.contains("target: ''"), "{after}");
        assert!(after.contains("title: Audit loot tables"), "{after}");

        let journal_dir = ws_parent.path().join(".ken-workspace/journal");
        let jentries: Vec<_> = std::fs::read_dir(&journal_dir).unwrap().flatten().collect();
        assert_eq!(jentries.len(), 1, "today's journal");
        let jraw = std::fs::read_to_string(jentries[0].path()).unwrap();
        assert!(jraw.contains("Completed task \"Audit loot tables\""), "{jraw}");
        assert!(jraw.contains("ken://workspace/tasks/"), "{jraw}");

        let (text, is_err) = tool(&mut server, "task_update", json!({"id": id, "state": "finished"}));
        assert!(is_err, "{text}");
        let (text, is_err) = tool(&mut server, "task_update", json!({"id": "no-such-task", "title": "x"}));
        assert!(is_err, "{text}");
    }

    #[test]
    fn task_list_filters_by_state_target_and_link() {
        let (_base, _ws, mut server) = task_fixture();
        for args in [
            json!({"title": "Soon", "target": "2026-10-02", "links": ["ATT-014"]}),
            json!({"title": "Later", "target": "2026-10-20"}),
            json!({"title": "Whenever"}),
        ] {
            let (t, e) = tool(&mut server, "task_create", args);
            assert!(!e, "{t}");
        }
        let (all, _) = tool(&mut server, "task_list", json!({}));
        assert!(all.starts_with("3 tasks"), "{all}");
        let soon_id = all.lines().find(|l| l.contains("\"Soon\"")).unwrap().split(' ').next().unwrap().to_string();
        let (t, e) = tool(&mut server, "task_update", json!({"id": soon_id, "state": "done"}));
        assert!(!e, "{t}");

        let (open, _) = tool(&mut server, "task_list", json!({}));
        assert!(!open.contains("\"Soon\"") && open.contains("\"Later\""), "{open}");
        let (done, _) = tool(&mut server, "task_list", json!({"state": "done"}));
        assert!(done.contains("\"Soon\"") && !done.contains("\"Later\""), "{done}");
        let (before, _) = tool(&mut server, "task_list", json!({"state": "all", "target_before": "2026-10-10"}));
        assert!(before.contains("\"Soon\"") && !before.contains("\"Later\"") && !before.contains("Whenever"), "{before}");
        let (linked, _) = tool(&mut server, "task_list", json!({"state": "all", "linked": "att-014"}));
        assert!(linked.starts_with("1 task"), "{linked}");
        let (bad, bad_err) = tool(&mut server, "task_list", json!({"state": "bogus"}));
        assert!(bad_err, "{bad}");
    }

    #[test]
    fn ticket_list_reads_the_repos_ticket_files() {
        let (_base, _ws, mut server) = task_fixture();
        let (t, e) = tool(&mut server, "task_create", json!({"title": "Linked", "links": ["ATT-014"]}));
        assert!(!e, "{t}");
        let (text, is_err) = tool(&mut server, "ticket_list", json!({"assignee": "all"}));
        assert!(!is_err, "{text}");
        assert!(text.contains("ATT-014 — \"Retry rule for the scheduler\" [in progress, repo atlas"), "{text}");
        assert!(text.contains("1 task, 0 done"), "{text}");
        assert!(text.contains("/tickets/ATT-014.md"), "{text}");
        assert!(!text.contains("ATT-009"), "done tickets are not open: {text}");
        let (text, _) = tool(&mut server, "ticket_list", json!({"assignee": "all", "state": "all"}));
        assert!(text.contains("ATT-009 — \"Glossary names\""), "{text}");
        // Nobody on this machine is "nobody-here": "me" finds nothing (or
        // says git has no identity), and never errors.
        let (text, is_err) = tool(&mut server, "ticket_list", json!({}));
        assert!(!is_err, "{text}");
        assert!(!text.contains("ATT-014 —"), "{text}");
    }

    /// Cross-checked by hand against Howard Hinnant's reference algorithm at
    /// two independently verified anchors: day 0 is the Unix epoch itself,
    /// and day 10957 is 2000-01-01 (30 years incl. 7 leap days: 1972, 76,
    /// 80, 84, 88, 92, 96 — 1970-01-01 + 30*365 + 7 = 10957).
    #[test]
    fn civil_from_days_matches_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(10957), (2000, 1, 1));
    }

    // --- ken-families tools (tasks 3.1-3.4) ---

    /// A server with no family connections on disk — enough for the
    /// tool-list tests, which never touch `families/`.
    fn family_flag_fixture() -> (tempfile::TempDir, Server) {
        let base = tempfile::tempdir().unwrap();
        let server = Server { base_dir: base.path().to_path_buf(), scoped: None, ..Default::default() };
        (base, server)
    }

    /// A real family connection: `<base>/families/<id>/` is a genuine git
    /// clone of a local bare "remote", scaffolded via
    /// `family::scaffold_family` and committed through
    /// `Lane::bootstrap()` — the exact shape "Create family" produces in
    /// production (D2/1.7). Two members: `"owner"` (this device — task 2.1's
    /// settings store doesn't exist yet, so `discover_family_connections`'s
    /// single-member fallback can't apply here; tests that need "owner" as
    /// the resolved identity pass `as: "owner"` explicitly, exactly like a
    /// caller would before that store lands) and `"sarah"` (a teammate,
    /// used for the lane-refusal test). Returns `None` — never panics — when
    /// `git` isn't usable in the sandbox, mirroring
    /// `family_sync::system_git_drives_a_real_local_clone`'s own skip.
    fn family_fixture(
        with_tasks_workspace: bool,
    ) -> Option<(tempfile::TempDir, tempfile::TempDir, tempfile::TempDir, Server, Uuid, PathBuf)> {
        if family_sync::git_available().is_err() {
            return None;
        }
        let base = tempfile::tempdir().unwrap();
        let remote_dir = tempfile::tempdir().unwrap();
        let ws_parent = tempfile::tempdir().unwrap();
        let bare = remote_dir.path().join("family.git");

        let init = std::process::Command::new("git")
            .args(["init", "--bare", "--initial-branch=main"])
            .arg(&bare)
            .output();
        match init {
            Ok(o) if o.status.success() => {}
            _ => return None,
        }

        let family_id = Uuid::new_v4();
        let clone_root = base.path().join("families").join(family_id.to_string());
        std::fs::create_dir_all(clone_root.parent().unwrap()).unwrap();
        let clone_ok = std::process::Command::new("git")
            .args(family_sync::clone_config_args())
            .args(["clone", "--"])
            .arg(&bare)
            .arg(&clone_root)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !clone_ok {
            return None;
        }

        let mut git = SystemGit::new(&clone_root, "origin", "main");
        git.identity =
            Some(family_sync::GitIdentity { name: "Ken Test".into(), email: "ken@example.invalid".into() });
        if git.configure_clone().is_err() {
            return None;
        }

        let members = vec![family::FamilyMember::new("owner", "Owner"), family::FamilyMember::new("sarah", "Sarah")];
        let scaffold = family::scaffold_family("Test Family", family_id, &members).unwrap();
        let writes: Vec<PendingWrite> =
            scaffold.iter().map(|f| PendingWrite::new(f.rel_path.clone(), f.content.clone())).collect();
        git.commit_paths(&Lane::bootstrap(), &writes, "Create family").unwrap();
        git.push().unwrap();

        let mut registry = Registry::default();
        if with_tasks_workspace {
            let workspace = ken_core::workspace::Workspace::create(ws_parent.path(), "WS", &[]).unwrap();
            registry.add_workspace(&workspace, None, 0);
            registry.last_workspace = Some(workspace.config.id);
            // The app's saved connection: this device is "owner", attached
            // to no workspace (so every workspace reads it).
            let mut settings = AppSettings::default();
            settings.extra.insert(
                "familyConnections".into(),
                json!([{
                    "familyId": family_id.to_string(),
                    "name": "Test Family",
                    "remoteUrl": bare.to_string_lossy(),
                    "memberId": "owner",
                    "attachedWorkspaceId": null,
                }]),
            );
            settings.save(base.path()).unwrap();
        }
        registry.save(base.path()).unwrap();


        let server = Server { base_dir: base.path().to_path_buf(), scoped: None, ..Default::default() };
        Some((base, remote_dir, ws_parent, server, family_id, clone_root))
    }

    #[test]
    fn tool_list_is_fixed_with_no_flags() {
        // The task and team-inbox tools are always there: kenTasks and
        // kenFamilies are gone.
        let mut fx = fixture(true);
        let reply = call(&mut fx.server, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap();
        let names: Vec<_> = reply["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            names,
            ["find_definition", "find_usages", "file_outline", "related_files", "history", "search_knowledge", "read_document", "list_documents", "list_projects", "kg_search", "semantic_search", "route_query", "memory_write", "journal_append", "task_create", "task_update", "task_list", "ticket_list", "family_list", "family_inbox", "family_send"],
        );

        // No family connection on this device: an answer, not a flag error.
        let (text, is_err) = tool(&mut fx.server, "family_list", json!({}));
        assert!(!is_err, "{text}");
        assert!(text.contains("No family connections"), "{text}");
    }

    #[test]
    fn family_tools_appear_with_valid_schemas_when_flag_on() {
        let (_base, mut server) = family_flag_fixture();
        let reply = call(&mut server, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap();
        let tools = reply["result"]["tools"].as_array().unwrap();
        let names: Vec<_> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        for n in ["family_list", "family_inbox", "family_send"] {
            assert!(names.contains(&n), "{names:?}");
        }
        let fl = tools.iter().find(|t| t["name"] == "family_list").unwrap();
        assert_eq!(fl["inputSchema"]["type"], "object");
        let fi = tools.iter().find(|t| t["name"] == "family_inbox").unwrap();
        assert!(fi["inputSchema"]["properties"]["family"].is_object());
        let fs = tools.iter().find(|t| t["name"] == "family_send").unwrap();
        assert_eq!(fs["inputSchema"]["required"], json!(["family", "to", "kind", "title"]));
        assert!(fs["inputSchema"]["properties"]["task"]["properties"]["due"].is_object());
        for t in tools {
            assert_eq!(t["inputSchema"]["type"], "object", "schema for {}", t["name"]);
            assert!(t["description"].as_str().is_some_and(|d| !d.is_empty()), "{}", t["name"]);
        }
        // LOCKED (design D4 / spec "no auto-accept in v1"): family_send's
        // own description must say delivery is not assignment/acceptance.
        let fs_desc = fs["description"].as_str().unwrap();
        assert!(fs_desc.contains("not assignment"), "{fs_desc}");
        assert!(fs_desc.to_lowercase().contains("no auto-accept"), "{fs_desc}");
    }

    #[test]
    fn family_send_lands_a_new_file_in_the_recipient_inbox() {
        let Some((_base, remote, _ws, mut server, _family_id, clone_root)) = family_fixture(false) else {
            eprintln!("skipping: no usable git in this sandbox");
            return;
        };

        let (text, is_err) = tool(
            &mut server,
            "family_send",
            json!({
                "family": "Test Family",
                "to": "sarah",
                "kind": "task",
                "title": "Review the sync loop",
                "body": "Please double-check the retry logic.",
                "as": "owner"
            }),
        );
        assert!(!is_err, "{text}");
        assert!(text.contains("not assignment"), "{text}");
        assert!(text.contains("no auto-accept") || text.contains("There is no auto-accept"), "{text}");

        // The scaffold's `.gitkeep` marker (1.7) is also in this folder —
        // only the new `.md` item is this test's concern.
        let inbox_dir = clone_root.join("members/sarah/inbox");
        let entries: Vec<_> = std::fs::read_dir(&inbox_dir)
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
            .collect();
        assert_eq!(entries.len(), 1, "exactly one new file should land in sarah's inbox");
        let raw = std::fs::read_to_string(entries[0].path()).unwrap();
        let file_name = entries[0].file_name().to_string_lossy().into_owned();
        let item = family::parse_inbox_item(&file_name, &raw);
        assert!(!item.malformed, "{raw}");
        assert_eq!(item.kind, Some(family::InboxKind::Task));
        assert_eq!(item.status, Some(family::InboxStatus::Unread));
        assert_eq!(item.from, "owner");
        assert_eq!(item.title, "Review the sync loop");

        // And it was actually pushed to the "remote" — a fresh clone of the
        // bare repo this fixture still holds a handle to sees it.
        let verify_dir = clone_root.parent().unwrap().join("verify-clone");
        let cloned = std::process::Command::new("git")
            .args(["clone", "--"])
            .arg(remote.path().join("family.git"))
            .arg(&verify_dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        assert!(cloned, "push should have reached the bare remote");
        assert!(verify_dir.join("members/sarah/inbox").join(&file_name).is_file());
    }

    #[test]
    fn task_update_lane_refuses_a_foreign_board_write_but_allows_own_board() {
        let Some((_base, _remote, _ws, mut server, family_id, clone_root)) = family_fixture(true) else {
            eprintln!("skipping: no usable git in this sandbox");
            return;
        };

        // Seed one task file directly on each member's board — bypassing
        // family_send/accept entirely, since this test is only about
        // task_update's write-lane enforcement, not the acceptance flow.
        let seed = |member: &str, id: &str, title: &str| {
            let dir = clone_root.join(format!("members/{member}/board"));
            std::fs::create_dir_all(&dir).unwrap();
            let content = format!(
                "---\nid: {id}\ntitle: {title}\nstatus: backlog\nkind: human\nassignee: {member}\n\
project: ''\ntags: []\nboard: main\ncreated: '2026-08-01'\nupdated: '2026-08-01'\n---\n\nBody.\n"
            );
            std::fs::write(dir.join(format!("{id}-task.md")), content).unwrap();
        };
        seed("sarah", "01SARAHTASK", "Sarah's task");
        seed("owner", "01OWNERTASK", "Owner's task");

        // task_list lists only this device's own board, so sarah's task is
        // never shown as one of "your" tasks.
        let (list_text, list_err) = tool(&mut server, "task_list", json!({}));
        assert!(!list_err, "{list_text}");
        assert!(!list_text.contains("Sarah's task"), "{list_text}");
        let _ = family_id;

        // A change aimed at sarah's board, acting as owner, is refused by
        // the lane rules — not merely discouraged: the file on disk must be
        // byte-for-byte untouched afterward.
        let sarah_path = clone_root.join("members/sarah/board/01SARAHTASK-task.md");
        let before = std::fs::read_to_string(&sarah_path).unwrap();
        let (bad_text, bad_err) = tool(
            &mut server,
            "task_update",
            json!({"id": "01SARAHTASK", "state": "done", "as": "owner"}),
        );
        assert!(bad_err, "{bad_text}");
        assert!(bad_text.contains("refused"), "{bad_text}");
        let after = std::fs::read_to_string(&sarah_path).unwrap();
        assert_eq!(before, after, "a lane-refused write must not touch the file");

        // The same call against owner's own board succeeds, writes only the
        // given keys, and pushes.
        let (ok_text, ok_err) = tool(
            &mut server,
            "task_update",
            json!({"id": "01OWNERTASK", "state": "done", "as": "owner"}),
        );
        assert!(!ok_err, "{ok_text}");
        let owner_path = clone_root.join("members/owner/board/01OWNERTASK-task.md");
        let owner_raw = std::fs::read_to_string(&owner_path).unwrap();
        assert!(owner_raw.contains("status: done"), "{owner_raw}");
        assert!(owner_raw.contains("kind: human"), "{owner_raw}");
        assert!(owner_raw.contains("title: \"Owner's task\"") || owner_raw.contains("title: Owner's task"), "{owner_raw}");
        assert!(ok_text.contains("Journal entry recorded."), "{ok_text}");

        // Done again: no state change, so no second journal entry.
        let (again, again_err) = tool(&mut server, "task_update", json!({"id": "01OWNERTASK", "state": "done"}));
        assert!(!again_err, "{again}");
        assert!(!again.contains("Journal entry recorded."), "{again}");

        // This device is "owner" (configured): `as` cannot make it sarah.
        let (as_text, as_err) = tool(
            &mut server,
            "task_update",
            json!({"id": "01SARAHTASK", "state": "done", "as": "sarah"}),
        );
        assert!(as_err, "{as_text}");
        assert!(as_text.contains("refused"), "{as_text}");
        assert_eq!(before, std::fs::read_to_string(&sarah_path).unwrap());

        // A family attached to another workspace is not read.
        let mut settings = AppSettings::load(&server.base_dir);
        settings.extra["familyConnections"][0]["attachedWorkspaceId"] = json!(Uuid::new_v4().to_string());
        settings.save(&server.base_dir).unwrap();
        let (list_text, _) = tool(&mut server, "task_list", json!({"state": "all"}));
        assert!(!list_text.contains("Owner's task"), "{list_text}");
        let (gone, gone_err) = tool(&mut server, "task_update", json!({"id": "01OWNERTASK", "state": "open"}));
        assert!(gone_err && gone.contains("no task with id"), "{gone}");
    }

    #[test]
    fn task_stamps_are_local_and_parse_as_your_day_dates() {
        use chrono::TimeZone;
        // A fixed instant in a zone east of UTC: early morning there is
        // still yesterday in UTC, and the stamp must say the local day.
        let tz = chrono::FixedOffset::east_opt(10 * 3600).unwrap();
        let at = tz.with_ymd_and_hms(2026, 10, 1, 7, 30, 0).unwrap();
        assert_eq!(task_clock_at(at), ("2026-10-01".to_string(), "2026-10-01T07:30".to_string()));
        let utc_view = task_clock_at(at.with_timezone(&chrono::Utc));
        assert_eq!(utc_view, ("2026-09-30".to_string(), "2026-09-30T21:30".to_string()));

        // What the app writes, day.rs reads: `today` is a strict date, and
        // `now` reads as the same day.
        let (today, now) = task_clock();
        assert!(day::is_date(&today), "{today}");
        assert_eq!(now.len(), 16, "{now}");
        assert_eq!(&now[10..11], "T");
        assert_eq!(day::Ymd::parse(&now), day::Ymd::parse(&today));
        assert_eq!(today, chrono::Local::now().format("%Y-%m-%d").to_string());
    }
}

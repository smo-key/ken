//! ken-mcp — stdio MCP server over ken-core. Hand-rolled JSON-RPC 2.0:
//! one JSON object per line on stdout, stdin read line-by-line, nothing
//! but protocol on stdout (diagnostics go to stderr).
//!
//! Two tool sets in one binary (knowledge-core decision 22):
//! - **The knowledge server** (the default): `search`, `read`, `related`,
//!   the four code tools, `history`, `tickets`, `propose_decision`,
//!   `add_source` and `repos`, with the brief as its `instructions`
//!   (decision 1). It reads the SQLite indexes read-only. Its two writes only
//!   queue for a person: `propose_decision` files a ready ruling in the
//!   knowledge base's Inbox, `add_source` puts a source in its inbox folder
//!   for the next ingest. Nothing binding is written here.
//! - **The personal set** (`--personal`): the person's Your day tasks, Ken's
//!   memories and journal, and opening a file in Ken. Only Ken's own chat
//!   registers it, beside the knowledge server.
//!
//! Scoping: `ken-mcp --workspace <dir>` serves that workspace, whatever Ken
//! has open: search, repos, tickets and the knowledge base are its sources,
//! and a path with no `project` is read in its knowledge base. Without it
//! the workspace is the one Ken last had open. `ken-mcp --project <path>`
//! makes that project the one a path is read in (Ken's own chat); with
//! neither, a project-relative path needs a `project` argument matched
//! against Ken's registry. Search covers every source of the workspace
//! either way.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use uuid::Uuid;

use ken_core::day;
use ken_core::db::Db;
use ken_core::embedder::Embedder;
use ken_core::memory;
use ken_core::project::Project;
use ken_core::registry::{self, Registry};
use ken_core::routing::{self, MemberHits, MemberInfo, MemberStatus, RouteReason};
use ken_core::search::Source;
use ken_core::settings::AppSettings;
use ken_core::tasks;

/// The protocol revision we implement. Newer revisions we know to be
/// wire-compatible with our subset are echoed back on request.
const PROTOCOL_VERSION: &str = "2024-11-05";
const KNOWN_PROTOCOL_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18"];

/// Cap on `read` output.
const MAX_DOCUMENT_BYTES: usize = 200 * 1024;

#[derive(Default)]
struct Server {
    base_dir: PathBuf,
    /// Root the server is locked to (`--project <path>`), if any.
    scoped: Option<PathBuf>,
    /// The workspace the server serves (`--workspace <dir>`, the folder
    /// holding its `.ken-workspace`), if any; else the one Ken last had open.
    workspace: Option<PathBuf>,
    /// The personal tool set (`--personal`) rather than the knowledge server.
    personal: bool,
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
    let mut workspace = None;
    let mut personal = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--project" => match args.next() {
                Some(path) => scoped = Some(PathBuf::from(path)),
                None => {
                    eprintln!("ken-mcp: --project needs a path");
                    std::process::exit(2);
                }
            },
            "--workspace" => match args.next() {
                Some(dir) => workspace = Some(PathBuf::from(dir)),
                None => {
                    eprintln!("ken-mcp: --workspace needs a folder");
                    std::process::exit(2);
                }
            },
            "--personal" => personal = true,
            "--version" => {
                eprintln!("ken-mcp {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            other => {
                eprintln!("ken-mcp: unknown argument {other:?}\nusage: ken-mcp [--workspace <dir>] [--project <path>] [--personal]");
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
    if let Some(dir) = &workspace {
        if let Err(e) = ken_core::workspace::Workspace::open(dir) {
            eprintln!("ken-mcp: {} is not a Ken workspace ({e}); its folder is searched as one", dir.display());
        }
    }
    let mut server = Server { base_dir, scoped, workspace, personal, ..Default::default() };

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
        "initialize" => rpc_result(&id, initialize_result(server, &params)),
        "ping" => rpc_result(&id, json!({})),
        "tools/list" => rpc_result(&id, json!({ "tools": tool_definitions(server) })),
        "tools/call" => {
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            // Any tool the server lists is dispatched, and only those: the
            // list and the dispatch cannot drift apart (open_in_ken once was
            // listed and refused), and neither set answers the other's tools.
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

fn initialize_result(server: &Server, params: &Value) -> Value {
    let requested = params
        .get("protocolVersion")
        .and_then(|v| v.as_str())
        .unwrap_or(PROTOCOL_VERSION);
    let version = if KNOWN_PROTOCOL_VERSIONS.contains(&requested) {
        requested
    } else {
        PROTOCOL_VERSION
    };
    let name = if server.personal { "ken-personal" } else { "ken" };
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": name, "version": env!("CARGO_PKG_VERSION") },
        "instructions": instructions(server)
    })
}

/// The brief, short form (knowledge-core decision 1): what the wiki's
/// `CLAUDE.md` says about the library and how to answer from it, sent as the
/// knowledge server's `instructions` so any Claude session gets it. Where
/// the knowledge base is comes first ([`instructions`]).
const BRIEF: &str = "\
Ken indexes the team's knowledge base and its code repos; these tools search and read them with Ken's own ranking.

The knowledge base is a library kept beside the code. The code is the truth for how the product works today; the \
library complements it and never copies it: it holds why things are built as they are, what was decided and \
reversed, what came out of a meeting, and how the parts fit. Its sections: Rules/ (how the team works, binding), \
Decisions/DECISIONS.md (every ruling, added and never edited), Design/, Current/ (the project, roadmap and feature \
status, true now; RAID.md holds the risks and issues, R-nnn and I-nnn, each with an owner and a status), Reference/ (vocabulary, how-tos, Platform/ for each dependency), Research/ (dated findings; \
Ingestion/Raw/ is the inbox, Ingestion/Ingested/ the notes), tickets/ (the work in flight and done) and log.md.

How to answer:
- Search first with search, then read a hit by its ken:// address (the #L line included). For code, use the code \
index before Grep and Glob: code_definition, code_usages, code_outline and code_imports, then read the lines around a \
hit, not the whole file. For when and why something changed, history; for what something is tied to, related.
- Every hit carries one label: its kind, its tier (for code, its role) and its date (decision D-410 · binding · \
2026-10-06, doc · current, ticket · superseded, code · service · 2026-10-01). The tiers run binding, current, \
reference, record, superseded. Weigh hits by them: a later ruling over an earlier one on the same point (a hit marked \
superseded by a later ruling holds only where that ruling does not reach), a binding ruling or rule over a ticket or \
note, and a current page over a record.
- A ruling is settled. Do not reopen it or recommend against it; where the code does not match a ruling, say the code \
differs from it and cite both.
- On what was built, the code wins: a page, ticket or plan says what was intended. When asked how something works, \
read the code that does it; where code and a page disagree, say so and cite both. Several repos can hold the same \
product: say which repo each finding is from.
- An empty search proves nothing: the index can miss a file, a new change or other wording. Check the Vocabulary page \
for the other word, then look directly with Grep, Glob and Read before saying something is not there, and say where \
you looked.
- A Research page or an ingested note is evidence from its date: cite it with its date.
- Cite every fact by the ken:// address of the hit it came from, with #L<line> when it has one.
- The knowledge base changes through a person. A ruling the person states goes in with propose_decision and a source \
they want kept with add_source: both wait in Ken's Inbox for a person, and nothing binding is written here.";

/// What the personal set tells a session about itself.
const PERSONAL_BRIEF: &str = "Ken's personal tools: the person's Your day tasks (task_list, task_create, task_update), \
Ken's memory (memory_write) and the workspace journal (journal_append), and open_in_ken. Use them only when the person \
asks: their list is theirs, not yours. The team's knowledge is on Ken's knowledge server, not here.";

/// The server's `instructions`: for the knowledge server, where the open
/// workspace's knowledge base is, then [`BRIEF`].
fn instructions(server: &Server) -> String {
    if server.personal {
        return PERSONAL_BRIEF.to_string();
    }
    let place = match knowledge_base(server) {
        Ok((id, name, root)) => format!(
            "The knowledge base of the open workspace is {name}, at {} (ken://{id}/); repos lists every repo Ken searches.",
            root.display()
        ),
        Err(_) => "No knowledge base is set up in the open workspace yet; search still covers its repos, and repos lists them.".to_string(),
    };
    format!("{place}\n\n{BRIEF}")
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

/// The tools this server lists: the knowledge server's twelve
/// (knowledge-core decision 22), or with `--personal` the personal set.
fn tool_definitions(server: &Server) -> Value {
    if server.personal {
        personal_tools()
    } else {
        knowledge_tools()
    }
}

/// The knowledge server's tools, in decision 22's order. Search is one
/// pipeline (decision 15): `search` over every source through `routing`.
fn knowledge_tools() -> Value {
    let project_arg = json!({
        "type": "string",
        "description": "The Ken repo a project-relative path is in: a name from repos, or its folder. Not needed \
for a ken:// address, or when the server was started locked to one project."
    });
    let code_project = json!({ "type": "string", "description": "Only this repo (by name). Default: every repo of the workspace, so uses in other repos are found too." });
    let tools = vec![
        json!({
            "name": "search",
            "description": "START HERE to find where something is, or what the team knows about a question, in plain \
words. Searches every repo of the workspace (only the one the question names when it names one, or the `project` you \
give; a reference repo only for a question about the platform, when named, or when nothing else answers) by keyword \
and by meaning, and returns one merged ranked list, with any repo still building its meaning index or slow to answer \
said so. Each hit is short: its label (kind · tier or role · date), repo:path:line to cite, ken:// address, and about \
300 characters around the best-matching lines, so ten hits cost about 6,000 characters. For more of a hit call read \
with its ken:// address (the #L line included): the lines around it, one cheap call. A code hit names the definition \
it holds (`saveWorld`; plain words like \"save world\" find it) and the calls that go further (code_usages, \
code_outline, code_imports). Meaning search uses Ken's installed embedding model; without it the answer says it was \
keyword only.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "A question or search terms. A repo's name in it narrows the search to that repo." },
                    "project": { "type": "string", "description": "Only these repos: names from repos (or folders), comma-separated. Default: every repo of the workspace." },
                    "type": { "type": "string", "description": "Keep only these kinds of file, comma-separated: code, test, spec, doc, config, data, design, meeting, ticket, decision, note (or any, the default). Every hit is labelled with its kind either way." },
                    "limit": { "type": "integer", "description": "Maximum merged hits to return (default 10)." }
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": "read",
            "description": "Read a document, or just some of its lines, after a search: the way to see more of a hit. \
Give a hit's ken:// address as `path`: with its #L<n> a Markdown page gives the whole section n is in (its heading to \
the next heading of the same level; in a decisions log, the entry), up to about 8,000 characters, and a code or text \
file the lines around n (10 before, 40 after), numbered: one cheap call. start_line/end_line pick other lines. With no \
line, the whole file comes back (up to 200 KB, so costly for a big file). Also takes a project-relative path (with \
`project`). Binary formats (docx, xlsx, pptx, pdf, images) return the text Ken's indexer extracted. Given a folder \
(`.` for the whole repo), lists its indexed files instead: path, kind, size and modification time.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "A ken:// address from a hit (#L<n> included), a project-relative file path, or a folder to list (`.` for the whole repo)." },
                    "start_line": { "type": "integer", "description": "First line to return, 1-based. Alone, it returns 50 lines from there." },
                    "end_line": { "type": "integer", "description": "Last line to return, inclusive." },
                    "project": project_arg.clone()
                },
                "required": ["path"]
            }
        }),
        json!({
            "name": "related",
            "description": "What a file, ruling or ticket is related to, from Ken's project graph (exact sources only), \
nearest first: the pages that link to it or that it links to, what a page cites (code files, notes, rulings), the \
note that changed a page, the ruling that supersedes another, code comments and commits that cite a ruling or a \
ticket, the topics a page is tagged with and the pages that share them, and for a code file the files it imports and \
that import it (from Ken's code index). For how something fits in and what depends on it; use it before Grep or Glob \
for references.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "The file's path relative to its project root, or a ruling or ticket id (D-012, SR-101)." },
                    "project": code_project.clone()
                },
                "required": ["path"]
            }
        }),
        json!({
            "name": "code_definition",
            "description": "Where a symbol (function, method, class, struct, interface, module) is defined in the code, \
from Ken's code index (tree-sitter, every indexed repo). Returns each definition's ken:// address with its line, its \
kind and its doc comment. Use it like 'go to definition', before Grep or Glob: one call instead of a search through \
every file; then read the file at that line.",
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
            "name": "code_usages",
            "description": "Every place a symbol is used (called, referenced, instantiated), each with the function or \
class it sits in, so its callers. Like 'find usages': who calls this, what breaks if it changes. Use it before Grep \
for a symbol's uses. Names only, no type resolution: two different symbols with one name both show.",
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
            "name": "code_outline",
            "description": "A code file's definitions in order, nested (classes and their methods), each with its line, \
from Ken's code index. Drill into a file with it before reading all of it or grepping it.",
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
            "name": "code_imports",
            "description": "A code file's imports, from Ken's code index: the indexed files it imports, the files that \
import it, and the libraries (imports Ken could not tie to a file). Use it before Grep or Glob for what a file \
depends on and what depends on it.",
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
            "name": "tickets",
            "description": "The tickets (tickets/<ID>.md) in the workspace's repos: the work in flight and, with state \
all, the work done. Each line shows the id, title, status, repo, target date and assignee, with its ken:// address. \
Open ones by default; assignee me keeps the ones assigned to the person (matched against git's user.name and \
user.email). Read-only: a ticket is changed as a file.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "state": { "type": "string", "enum": ["open", "all"], "description": "Default open." },
                    "assignee": { "type": "string", "enum": ["all", "me"], "description": "Default all." }
                }
            }
        }),
        json!({
            "name": "propose_decision",
            "description": "Propose a ruling for the knowledge base's decisions log: one the person made or confirmed \
in this conversation, never your own view. It never writes the log. It files a ready entry in the log's one entry \
format in Ken's Inbox, and a person adds it in one click (it takes the next D-number then) or discards it; say that it \
waits for them. Give the ruling in one sentence and why; add what it is about, the words it was made in and who made \
it when you have them, and the rulings it replaces.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "ruling": { "type": "string", "description": "The ruling, in one sentence." },
                    "why": { "type": "string", "description": "Why it was decided, in a sentence or two." },
                    "topic": { "type": "string", "description": "A few words for what it is about." },
                    "quote": { "type": "string", "description": "The words it was made in, when the person said them." },
                    "who": { "type": "string", "description": "Who made it, as free text." },
                    "aliases": { "type": "array", "items": { "type": "string" }, "description": "Other words anyone might search for it by." },
                    "supersedes": { "type": "array", "items": { "type": "string" }, "description": "The rulings it replaces, by id (D-003)." },
                    "source": { "type": "string", "description": "The note or page it was said in, by its name (2026-10-12-standup), when there is one." }
                },
                "required": ["ruling", "why"]
            }
        }),
        json!({
            "name": "add_source",
            "description": "Put a source in the knowledge base's inbox (Research/Ingestion/Raw/) for Ken's next ingest \
pass, which reads it into a dated note and updates the pages it changes (edits that need a person are held for one). \
It never ingests here and never edits a page; say that it waits for the ingest. Give a file from one of the \
workspace's repos (a ken:// address, or a path with `project`), or pasted `text` with a `title`.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "A ken:// address, or a path relative to `project`, of the file to add." },
                    "text": { "type": "string", "description": "Text to add as a note, when there is no file (at most 400,000 characters)." },
                    "title": { "type": "string", "description": "The note's title, with `text`." },
                    "project": project_arg
                }
            }
        }),
        json!({
            "name": "repos",
            "description": "The repos Ken searches in the open workspace: name, folder, whether the folder is there, \
and which one is the knowledge base, each with its index health in one line (files indexed, failed or skipped; how \
far its meaning search has got; its controls; whether the embedding model is missing).",
            "inputSchema": { "type": "object", "properties": {} }
        }),
    ];
    Value::Array(tools)
}

/// The personal set (`--personal`): the person's Your day tasks, Ken's
/// memory and journal, and, when Ken's own chat started the server,
/// opening a file in Ken.
fn personal_tools() -> Value {
    let mut tools = vec![
        json!({
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
        }),
        json!({
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
        }),
        json!({
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
                    "state": { "type": "string", "enum": ["open", "done"] }
                },
                "required": ["id"]
            }
        }),
        json!({
            "name": "memory_write",
            "description": "Create or update one of Ken's own long-term \
memory files: durable notes on ways of working, conventions, or standing \
decisions, either workspace-wide or scoped to one repo. Distinct from \
the day-to-day journal (use journal_append for that): memories are small, \
curated, and always kept in context, not a running log. `scope` is \
\"workspace\" for a workspace-wide memory, or a Ken repo name for a \
repo-scoped one. `slug` names the file (<slug>.md) and is the memory's \
identity. By default this creates a new memory and errors if that slug \
already exists; pass mode \"replace\" to update an existing memory's body \
instead.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "scope": { "type": "string", "description": "\"workspace\", or a Ken repo name for a repo-scoped memory." },
                    "slug": { "type": "string", "description": "File-name-safe identifier for the memory: creates <slug>.md." },
                    "content": { "type": "string", "description": "The memory's body text." },
                    "mode": {
                        "type": "string",
                        "enum": ["create", "replace"],
                        "description": "\"create\" (default): errors if the slug already exists. \"replace\": updates an existing memory's body (its slug must already exist)."
                    }
                },
                "required": ["scope", "slug", "content"]
            }
        }),
        json!({
            "name": "journal_append",
            "description": "Append a timestamped entry to today's Ken \
workspace journal, for a finding the person wants kept, creating the \
day's file if it doesn't exist yet. Cite \
ken:// addresses for anything referenced so the entry stays traceable \
after Ken reindexes it. Optionally tag the entry with the Ken repo it \
concerns and free-form tags.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "The journal entry's text." },
                    "project": { "type": "string", "description": "Ken repo name this entry concerns, if any." },
                    "tags": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Free-form tags for this entry."
                    }
                },
                "required": ["text"]
            }
        }),
    ];

    // Only when Ken's own chat started this server: it can reach the app.
    if app_bridge().is_some() {
        tools.push(json!({
            "name": "open_in_ken",
            "description": "Open a file in Ken for the person, at a line or a heading. Use this ONLY when the \
person explicitly asks you to open, show or take them to something; otherwise cite the file as a link and let \
them click it. Accepts a ken:// address from Ken's tools, or a project-relative path.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": { "type": "string", "description": "A ken://<project-id>/<path> address, or a project-relative path." },
                    "line": { "type": "integer", "description": "1-based line to scroll to." },
                    "heading": { "type": "string", "description": "A heading's text or slug to scroll to, in a Markdown page." },
                    "project": { "type": "string", "description": "The Ken repo a project-relative path is in." }
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
        // The knowledge server (decision 22).
        "search" => search(server, args),
        "read" => read(server, args),
        "related" => related(server, args),
        "code_definition" => code_definition(server, args),
        "code_usages" => code_usages(server, args),
        "code_outline" => code_outline(server, args),
        "code_imports" => code_imports(server, args),
        "history" => history(server, args),
        "tickets" => tickets(server, args),
        "propose_decision" => propose_decision(server, args),
        "add_source" => add_source(server, args),
        "repos" => repos(server),
        // The personal set (`--personal`).
        "task_list" => task_list_tool(server, args),
        "task_create" => task_create_tool(server, args),
        "task_update" => task_update_tool(server, args),
        "memory_write" => memory_write_tool(server, args),
        "journal_append" => journal_append_tool(server, args),
        "open_in_ken" => open_in_ken(server, args),
        _ => Err(format!("unknown tool: {name:?}")),
    }
}

/// A path as a tool was given it: a `ken://<project id>/<path>#L12` address,
/// as search returns it, names its repo (put in `project`), its file and the
/// fragment; anything else is a project-relative path. Returns the arguments
/// to resolve the project with, the path and the fragment.
fn split_address(args: &Value, path: &str) -> (Value, String, Option<String>) {
    let mut named = args.clone();
    let path = match path.strip_prefix("ken://").and_then(|r| r.split_once('/')) {
        Some((id, rel)) => {
            named["project"] = json!(id);
            rel.replace("%20", " ")
        }
        None => path.replace('\\', "/"),
    };
    match path.split_once('#') {
        Some((file, frag)) => (named, file.to_string(), Some(frag.to_string())),
        None => (named, path, None),
    }
}

/// `read`: a file, some of its lines or the section a line is in, or, for a
/// folder, its indexed files (what `list_documents` was).
fn read(server: &Server, args: &Value) -> Result<String, String> {
    let given = require_str(args, "path")?;
    let (named, path, fragment) = split_address(args, &given);
    let (project, note) = resolve_project(server, &named)?;
    let folder = path.trim_matches('/');
    if folder.is_empty() || folder == "." {
        return list_folder(server, &project, note, None);
    }
    // Validate before anything else so `..`/absolute paths are refused
    // outright, whatever the index says.
    let abs = project
        .resolve(&path)
        .map_err(|_| format!("{path:?} is outside the project — paths are relative to the project root and may not contain \"..\""))?;
    if abs.is_dir() {
        return list_folder(server, &project, note, Some(folder));
    }
    let range = line_range(args, fragment.as_deref());
    let pointed = address_line(args, fragment.as_deref());
    let db = open_index(server, &project)?;
    let row = db
        .get_file(&path)
        .map_err(|e| format!("index lookup failed: {e}"))?
        .ok_or_else(|| {
            format!(
                "{path:?} is not in project \"{}\"'s index. Use search, or read on its folder, to find valid paths.",
                project.config.name
            )
        })?;
    let mut out = note.unwrap_or_default();
    match (row.kind.as_str(), range) {
        // A page read from a hit's address: the section it is in.
        ("md", Some(_)) if pointed.is_some() => {
            let bytes = std::fs::read(&abs).map_err(|e| format!("could not read {path:?}: {e}"))?;
            out.push_str(&section_text(&db, &path, &String::from_utf8_lossy(&bytes), pointed.unwrap_or(1)));
        }
        ("md" | "txt" | "code", Some(r)) => {
            let bytes = std::fs::read(&abs).map_err(|e| format!("could not read {path:?}: {e}"))?;
            out.push_str(&numbered_lines(&String::from_utf8_lossy(&bytes), r));
        }
        ("md" | "txt" | "code", None) => {
            let bytes = std::fs::read(&abs).map_err(|e| format!("could not read {path:?}: {e}"))?;
            let truncated = bytes.len() > MAX_DOCUMENT_BYTES;
            let end = if truncated { floor_char_boundary_at(&bytes, MAX_DOCUMENT_BYTES) } else { bytes.len() };
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
                    Some(reason) => format!("Ken could not extract text from {path:?} ({reason})."),
                    None => format!("{path:?} is a {kind} file with no extracted text in the index (it is indexed by name only)."),
                })?;
            let text = match range {
                Some(r) => numbered_lines(&text, r),
                None => text,
            };
            out.push_str(&format!("[{kind} file — this is the text Ken's indexer extracted, not the original bytes]\n\n{text}"));
        }
    }
    Ok(out)
}

/// A folder's indexed files (the whole project's with no folder): path,
/// kind, size and modification time.
fn list_folder(server: &Server, project: &Project, note: Option<String>, folder: Option<&str>) -> Result<String, String> {
    let db = open_index(server, project)?;
    let files = db.list_files().map_err(|e| format!("index lookup failed: {e}"))?;
    let files: Vec<_> = match folder {
        Some(f) => files.into_iter().filter(|row| row.rel_path.starts_with(&format!("{f}/"))).collect(),
        None => files,
    };
    let mut out = note.unwrap_or_default();
    let scope = match folder {
        Some(f) => format!("under \"{f}\" in project \"{}\"", project.config.name),
        None => format!("in project \"{}\"", project.config.name),
    };
    if files.is_empty() {
        out.push_str(&format!("No indexed files {scope}."));
        return Ok(out);
    }
    out.push_str(&format!(
        "{} indexed file{} {scope} (path — kind, size, modified as unix seconds):\n",
        files.len(),
        if files.len() == 1 { "" } else { "s" },
    ));
    for row in files {
        out.push_str(&format!("\n{} — {}, {} bytes, modified {}", row.rel_path, row.kind, row.size, row.mtime));
    }
    Ok(out)
}

/// The lines `read` returns, 1-based and inclusive: `start_line` and
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

/// About this many characters of a section `read` returns from an
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

fn require_str(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("the {key:?} argument is required"))
}

/// `repos`: the repos of the open workspace, the knowledge base marked, each
/// with the one health line (decision 19).
fn repos(server: &Server) -> Result<String, String> {
    let registry = Registry::load(&server.base_dir)
        .map_err(|e| format!("could not read Ken's project registry: {e}"))?;
    let inside = in_open_workspace(server, &registry);
    let statuses: Vec<_> = registry.statuses().into_iter().filter(|s| inside(&s.entry.path)).collect();
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
            "\n{}{} — {}{}",
            s.entry.name,
            if s.available && registry::is_library(&s.entry.path) { " (knowledge base)" } else { "" },
            s.entry.path.display(),
            if s.available { "" } else { " (folder currently missing)" }
        ));
        if s.available {
            // The one health line (knowledge-core decision 19).
            let readiness = match Db::open_read_only(&server.base_dir, s.entry.id).and_then(|db| db.index_health()) {
                Ok(mut h) => {
                    h.model_missing = ken_core::embedder::selected_profile().is_none();
                    h.line()
                }
                Err(_) => "no index yet".to_string(),
            };
            out.push_str(&format!(" — {readiness}"));
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
        // Read through the git helper drift shares (`ken_core::git`,
        // knowledge-core decision 27).
        let runs: Vec<Vec<String>> = match (&path, &query) {
            (Some(p), _) => vec![vec!["--follow".into(), "--".into(), p.clone()]],
            (None, Some(q)) => vec![
                vec!["--name-only".into(), "-i".into(), format!("--grep={q}")],
                vec!["--name-only".into(), format!("-S{q}")],
            ],
            (None, None) => vec![vec!["--name-only".into()]],
        };
        let mut seen: Vec<String> = Vec::new();
        for run in &runs {
            let extra: Vec<&str> = run.iter().map(String::as_str).collect();
            let Some(commits) = ken_core::git::log(root, limit, days, &extra) else { continue };
            for c in commits {
                if seen.contains(&c.sha) || count >= limit {
                    continue;
                }
                seen.push(c.sha.clone());
                count += 1;
                out.push_str(&format!("\n{count}. {} {} {} — {} ({project})", c.sha, c.date, c.author, c.subject));
                // A path from an older commit may have moved or gone since:
                // an address only for what is there now.
                for f in c.files.iter().take(6) {
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

fn code_definition(server: &Server, args: &Value) -> Result<String, String> {
    let name = require_str(args, "name")?;
    let indexes = code_indexes(server, args)?;
    let mut out = String::new();
    let mut n = 0;
    for (id, project, db) in &indexes {
        for d in ken_core::codeindex::definitions(db, &name).map_err(|e| e.to_string())? {
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
        return Ok(format!("No definition of {name:?} in the code index of {} project(s). {LOOK_YOURSELF}", indexes.len()));
    }
    Ok(format!("{n} definition{} of {name:?}:{out}", if n == 1 { "" } else { "s" }))
}

fn code_usages(server: &Server, args: &Value) -> Result<String, String> {
    let name = require_str(args, "name")?;
    let limit = args.get("limit").and_then(|l| l.as_u64()).map(|l| l.clamp(1, 500) as usize).unwrap_or(60);
    let indexes = code_indexes(server, args)?;
    let mut lines = Vec::new();
    let mut mention_lines = Vec::new();
    let mut callers: Vec<String> = Vec::new();
    let mut files: std::collections::BTreeSet<String> = Default::default();
    for (id, project, db) in &indexes {
        let calls = ken_core::codeindex::usages(db, &name, limit).map_err(|e| e.to_string())?;
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
        for m in ken_core::codeindex::mentions(db, &name, &known, limit).map_err(|e| e.to_string())? {
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
        return Ok(format!("No uses of {name:?} in the code index of {} project(s). {LOOK_YOURSELF}", indexes.len()));
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
        .ok_or_else(|| format!("{path:?} is not in the code index: not an indexed code file in a mapped language (Rust, Python, JavaScript, TypeScript, Go, Java, C#). {LOOK_YOURSELF}"))?;
    Ok(indexes.swap_remove(at))
}

fn code_outline(server: &Server, args: &Value) -> Result<String, String> {
    let path = require_str(args, "path")?.replace('\\', "/");
    let (id, project, db) = index_with_file(server, args, &path)?;
    let outline = ken_core::codeindex::outline(&db, &path).map_err(|e| e.to_string())?;
    let mut out = format!("{} ({project}) — {} definitions:", ken_address(id, &path), outline.len());
    for (depth, d) in outline {
        out.push_str(&format!("\n{}{} {} — L{}-{}", "  ".repeat(depth), d.kind, d.name, d.line, d.end_line));
    }
    Ok(out)
}

/// `code_imports`: what a code file imports (resolved to indexed files
/// where Ken can tell), what imports it, and the libraries, from the code
/// index (`codeindex::related`).
fn code_imports(server: &Server, args: &Value) -> Result<String, String> {
    let path = require_str(args, "path")?.replace('\\', "/");
    let (id, project, db) = index_with_file(server, args, &path)?;
    let r = ken_core::codeindex::related(&db, &path).map_err(|e| e.to_string())?;
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

/// Most neighbours `related` lists.
const RELATED_LIMIT: usize = 40;

/// What a file, ruling or ticket is related to (knowledge-core decision 21):
/// `graph::related` over the workspace's knowledge base (its stored edges)
/// and the code index of every other repo. The path is looked for in the
/// named project, else the knowledge base, else the first repo whose index
/// has it; an id in a ruling's or a ticket's shape is looked up in the
/// knowledge base.
fn related(server: &Server, args: &Value) -> Result<String, String> {
    use ken_core::graph::Node;
    let path = require_str(args, "path")?.trim().replace('\\', "/");
    let registry = Registry::load(&server.base_dir).map_err(|e| format!("could not read Ken's project registry: {e}"))?;
    let inside = in_open_workspace(server, &registry);
    let mut repos: Vec<(Uuid, String, PathBuf, Db)> = registry
        .projects
        .iter()
        .filter(|e| e.path.is_dir() && inside(&e.path))
        .filter_map(|e| Db::open_read_only(&server.base_dir, e.id).ok().map(|db| (e.id, e.name.clone(), e.path.clone(), db)))
        .collect();
    let kb_at = repos.iter().position(|r| registry::is_library(&r.2));
    let kb = kb_at.map(|i| repos.remove(i));
    let named = args.get("project").and_then(Value::as_str).map(str::trim).filter(|p| !p.is_empty());
    let has = |db: &Db| db.get_file(&path).is_ok_and(|f| f.is_some());
    let id_like = !path.contains('/') && !path.contains('.') && path.contains('-');
    let (node, label) = if let Some(k) = kb.as_ref().filter(|k| named.is_none_or(|n| k.1.eq_ignore_ascii_case(n))) {
        if has(&k.3) {
            (Node::of_path(&path), ken_address(k.0, &path))
        } else if id_like {
            let known = ken_core::graph::known_ids(&k.3, &k.2).map_err(|e| e.to_string())?;
            match known.get(&path) {
                Some(n) => (n.clone(), n.key()),
                None => return Ok(format!("{path} is not a ruling or a ticket the knowledge base has. {LOOK_YOURSELF}")),
            }
        } else {
            code_node(&repos, named, &path)?
        }
    } else {
        code_node(&repos, named, &path)?
    };
    let code: Vec<ken_core::ingest::CodeSource> =
        repos.into_iter().map(|(_, name, _, db)| ken_core::ingest::CodeSource { name, db }).collect();
    let found = ken_core::graph::related(kb.as_ref().map(|k| &k.3), &code, &node, RELATED_LIMIT).map_err(|e| e.to_string())?;
    if found.is_empty() {
        return Ok(format!("Nothing in the project graph is related to {label} yet. {LOOK_YOURSELF}"));
    }
    let address = |n: &Node| -> String {
        if let (Some(p), Some(k)) = (n.kb_path(), kb.as_ref()) {
            return ken_address(k.0, p);
        }
        if let Some((repo, p, symbol)) = n.code_place() {
            if let Some(id) = registry.projects.iter().find(|e| e.name == repo).map(|e| e.id) {
                return format!("{}{}", ken_address(id, p), symbol.map(|s| format!(" `{s}`")).unwrap_or_default());
            }
        }
        n.key()
    };
    let mut out = format!("{label}: {} related, nearest first:", found.len());
    for n in &found {
        let arrow = if n.outgoing { "" } else { "← " };
        let detail = if n.detail.is_empty() { String::new() } else { format!(" ({})", n.detail) };
        let via = n.via.as_ref().map(|v| format!(", through {}", address(v))).unwrap_or_default();
        out.push_str(&format!("\n  - {arrow}{} {}{detail}{via}", n.edge, address(&n.node)));
    }
    Ok(out)
}

/// The code node for `path`: in the named repo, else the first repo whose
/// index has the file. With its address, for the reply's first line.
fn code_node(repos: &[(Uuid, String, PathBuf, Db)], named: Option<&str>, path: &str) -> Result<(ken_core::graph::Node, String), String> {
    repos
        .iter()
        .filter(|r| named.is_none_or(|n| r.1.eq_ignore_ascii_case(n)))
        .find(|r| r.3.get_file(path).is_ok_and(|f| f.is_some()))
        .map(|r| (ken_core::graph::Node::code(&r.1, path), format!("{} ({})", ken_address(r.0, path), r.1)))
        .ok_or_else(|| format!("{path:?} is not indexed in any repo of the open workspace. {LOOK_YOURSELF}"))
}

/// Said with every empty search: the index is not the last word. An agent
/// that reads "no results" as "not there" tells a person something false.
const LOOK_YOURSELF: &str = "Ken's index can miss things (a file not indexed yet, a recent change, other wording), so this \
does not mean it isn't there: look directly with Grep, Glob and Read before saying so, and say where you looked.";

/// The kinds of file a search keeps (`type`: "code", "spec,doc"); empty keeps all.
fn types_arg(args: &Value) -> Vec<ken_core::contenttype::ContentType> {
    ken_core::contenttype::parse_filter(args.get("type").and_then(|t| t.as_str()))
}

/// Hits to read before filtering by kind, so a filter still fills the page.
fn fetch_for(types: &[ken_core::contenttype::ContentType], limit: usize) -> usize {
    if types.is_empty() {
        limit
    } else {
        limit * 5
    }
}

/// A hit's tag in a result line: its label, one per hit
/// (`[code · service · 2026-10-01]`, `[decision D-410 · binding · 2026-10-06]`,
/// knowledge-core decision 17), else its kind (`[code]`).
fn hit_tag(path: &str, label: Option<&str>) -> String {
    match label.filter(|l| !l.is_empty()) {
        Some(l) => format!("[{l}]"),
        None => format!("[{}]", ken_core::contenttype::of(path).as_str()),
    }
}

/// A code hit's symbol beside its path, in backticks (`` `saveWorld` ``):
/// every code hit names what it holds (knowledge-core decision 26), and the
/// chat card reads it from there. Empty for a hit with no symbol.
fn symbol_tag(symbol: Option<&str>) -> String {
    symbol.map(str::trim).filter(|s| !s.is_empty()).map(|s| format!(" `{s}`")).unwrap_or_default()
}

/// The calls that go further from a code hit, under its snippet
/// (`codeindex::next_calls`): its symbol's usages, the file's outline and
/// its imports. Empty for a file the code index does not parse.
fn next_line(path: &str, symbol: Option<&str>) -> String {
    ken_core::codeindex::next_calls(path, symbol).map(|c| format!("\n   next: {c}")).unwrap_or_default()
}

// --- short hits and meaning search ---

/// About this many characters of each hit are shown: whole chunks made a
/// search answer a median 17,660 characters for 8 hits (Shattered
/// Realms eval, 2026-10-06), and `read` gives the rest on request.
const SNIPPET_CHARS: usize = 300;

/// Lines `read` shows before and after a single line it is pointed
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

/// Hits `search` returns unless asked: eight short hits measured a median
/// 4,554 characters (Shattered Realms eval, 2026-10-06), so ten cost about
/// 6,000, where eight whole chunks took 17,660.
const ROUTE_LIMIT: usize = 10;

/// `search`: every source searched and the hits merged once (knowledge-core
/// decisions 15 and 20). `routing::plan_route` picks the sources, or the
/// `project` argument names them (`RoutePlan::chosen`); `routing::search_target`
/// searches each with the question's vector when its index was built by the
/// same model ([`vector_for`]), the held-back reference repos only when
/// nothing else answered, and `routing::merge_routed` merges. The question is
/// embedded once; hits are cut short last.
fn search(server: &Server, args: &Value) -> Result<String, String> {
    let query = require_str(args, "query")?;
    let limit = args
        .get("limit")
        .and_then(|l| l.as_u64())
        .map(|l| l.clamp(1, 200) as usize)
        .unwrap_or(ROUTE_LIMIT);
    let registry = Registry::load(&server.base_dir)
        .map_err(|e| format!("could not read Ken's project registry: {e}"))?;
    if registry.projects.is_empty() {
        return Ok("No Ken projects registered on this machine yet — open a \
folder in the Ken app first."
            .to_string());
    }

    // Every registered project of the open workspace whose folder is there is
    // a source, and the one the server is locked to. Each index is opened
    // once; one that will not open is searched as unavailable and reported.
    let inside = in_open_workspace(server, &registry);
    let locked = |p: &Path| server.scoped.as_deref().is_some_and(|r| same_canonical(p, r));
    let mut members: Vec<MemberInfo> = Vec::new();
    let mut dbs: HashMap<Uuid, Db> = HashMap::new();
    let mut join = |entry: &registry::RegistryEntry, members: &mut Vec<MemberInfo>| {
        if let Ok(db) = Db::open_read_only(&server.base_dir, entry.id) {
            dbs.insert(entry.id, db);
        }
        members.push(MemberInfo { project_id: entry.id, name: entry.name.clone(), reference: routing::is_reference(&entry.kind) });
    };
    for entry in &registry.projects {
        if entry.path.is_dir() && (inside(&entry.path) || locked(&entry.path)) {
            join(entry, &mut members);
        }
    }

    // `project` narrows the search to the repos it names, by name, id or
    // folder, comma-separated; a registered repo outside the workspace may be
    // named too.
    let chosen = opt_str(args, "project");
    let plan = match &chosen {
        Some(list) => {
            let mut ids: Vec<Uuid> = Vec::new();
            for want in list.split(',').map(str::trim).filter(|w| !w.is_empty()) {
                let want_id = want.strip_prefix("ken://").unwrap_or(want).split('/').next().and_then(|s| s.parse::<Uuid>().ok());
                let Some(entry) = registry.projects.iter().find(|p| {
                    p.name.eq_ignore_ascii_case(want) || Some(p.id) == want_id || p.path == Path::new(want) || same_canonical(&p.path, Path::new(want))
                }) else {
                    let names: Vec<&str> = registry.projects.iter().map(|p| p.name.as_str()).collect();
                    return Err(format!("No Ken project matches {want:?}. Available projects: {}.", names.join(", ")));
                };
                if !members.iter().any(|m| m.project_id == entry.id) {
                    join(entry, &mut members);
                }
                if !ids.contains(&entry.id) {
                    ids.push(entry.id);
                }
            }
            routing::RoutePlan::chosen(ids)
        }
        None => routing::plan_route(&query, &members),
    };
    let types = types_arg(args);
    // One vocabulary for the whole search (the knowledge base's widens every repo).
    let shared_vocab = routing::workspace_vocabulary(dbs.values());
    let qv = if members.is_empty() { Err("nothing to search".to_string()) } else { query_vector(server, &query) };
    let run = |ids: &[Uuid]| -> Vec<MemberHits> {
        ids.iter()
            .map(|id| {
                let name = members.iter().find(|m| m.project_id == *id).map_or("", |m| m.name.as_str());
                let db = dbs.get(id);
                let fetch = fetch_for(&types, limit);
                let mut found =
                    routing::search_target(*id, name, db, &query, db.and_then(|db| vector_for(db, &qv)), fetch, &types, Some(&shared_vocab));
                found.hits.truncate(limit);
                found
            })
            .collect()
    };
    let mut member_hits = run(&plan.targets);
    if routing::needs_reserve(&plan, &member_hits) {
        member_hits.extend(run(&plan.reserve));
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
    let mut out = format_execution_report(&report, &members, chosen.is_some());
    if !members.is_empty() {
        let note = keyword_only_note(&qv);
        // After the Route and Sources lines, which a reader looks for first.
        match out.match_indices('\n').nth(1) {
            Some((at, _)) => out.insert_str(at + 1, &note),
            None => out.push_str(&note),
        }
    }
    Ok(out)
}

/// `part` of `whole` as a whole percent, 0 for nothing.
fn percent(part: i64, whole: i64) -> i64 {
    if whole <= 0 {
        0
    } else {
        (part * 100 / whole).clamp(0, 100)
    }
}

/// Renders a `search` [`routing::ExecutionReport`] as tool-call text: the
/// route (`chosen` when the `project` argument named the sources), how each
/// source's search went, the reference repos held back, then the hits.
fn format_execution_report(report: &routing::ExecutionReport, members: &[MemberInfo], chosen: bool) -> String {
    let name_of = |id: &Uuid| members.iter().find(|m| m.project_id == *id).map_or_else(|| id.to_string(), |m| m.name.clone());
    let reason = match report.plan.reason {
        RouteReason::Named if chosen => "the project asked for",
        RouteReason::Named => "named in the question",
        RouteReason::Every => "every source",
    };
    let searched = report.member_status.len();
    let mut out = format!("Route: {reason} ({searched} searched).\n");
    let sources: Vec<String> = report
        .member_status
        .iter()
        .map(|m| {
            let mut status = match m.status {
                MemberStatus::Searched => "searched".to_string(),
                MemberStatus::Building { embedded, chunks } => {
                    format!("keyword only, meaning search {}% built", percent(embedded, chunks))
                }
                MemberStatus::Unavailable => "unavailable, its index did not open".to_string(),
            };
            if m.slow() {
                status.push_str(&format!(", slow: {:.1} s", m.millis as f64 / 1000.0));
            }
            format!("{} ({status})", m.member_name)
        })
        .collect();
    out.push_str(&format!("Sources: {}\n", sources.join(", ")));
    let held: Vec<String> = report
        .plan
        .reserve
        .iter()
        .filter(|id| !report.member_status.iter().any(|m| m.project_id == **id))
        .map(name_of)
        .collect();
    if !held.is_empty() {
        out.push_str(&format!(
            "Held back: {} (a reference repo is searched for a question about the platform, when named, or when nothing else answers)\n",
            held.join(", ")
        ));
    }

    if report.results.is_empty() {
        out.push_str(&format!("\nNo results. {LOOK_YOURSELF}"));
        return out;
    }
    for (i, hit) in report.results.iter().enumerate() {
        // The locator is what to cite; the ken:// address is what to open.
        out.push_str(&format!(
            "\n{}. {} {}{} ({}{}) [{}]",
            i + 1,
            hit_tag(&hit.path, hit.label.as_deref()),
            hit.locator,
            symbol_tag(hit.symbol.as_deref()),
            hit.address,
            hit.line.map(|l| format!("#L{l}")).unwrap_or_default(),
            source_label(hit.source),
        ));
        if let Some(note) = hit.page.as_ref().and_then(|p| page_note(p, hit.label.is_some())) {
            out.push_str(&format!(" ({note})"));
        }
        out.push_str(&indented(&hit.snippet));
        out.push_str(&next_line(&hit.path, hit.symbol.as_deref()));
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

// --- The personal set: task_create / task_update / task_list ---
//
// The file logic is `ken_core::day`, the same core the app's Your day
// commands use, so a task written here reads the same there.

/// The task home for one call: the workspace home, where every task lives.
/// A repo's own `.ken/tasks/` is not a home (tasks live in your own folder,
/// not in a repo).
struct TaskHomes {
    workspace_root: PathBuf,
}

impl TaskHomes {
    fn homes(&self) -> Vec<tasks::TaskHome<'_>> {
        vec![tasks::TaskHome::Workspace { workspace_root: &self.workspace_root }]
    }

    fn scan(&self, today: &str) -> Vec<day::DayTask> {
        day::scan(&self.homes(), today)
    }

    /// The `ken://` host for a task: the workspace pseudo-host.
    fn host_for(&self, _task: &day::DayTask) -> String {
        memory::WORKSPACE_ADDRESS_ID.to_string()
    }

    fn address_for(&self, task: &day::DayTask) -> String {
        format!("ken://{}/{}", self.host_for(task), task.address_rel_path())
    }
}

fn resolve_task_homes(server: &Server) -> Result<TaskHomes, String> {
    Ok(TaskHomes { workspace_root: resolve_workspace_root(server)? })
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
/// appends the one-line journal entry (memory is built in).
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
    let (today, now) = task_clock();
    let homes = resolve_task_homes(server)?;
    let all = homes.scan(&today);
    let task = day::find(&all, &id)
        .ok_or_else(|| format!("no task with id {id:?} — task_list shows the ids"))?
        .clone();
    let stamp = day::Stamp { today: &today, now: &now, by: day::BY_MCP };
    day::update_task(&task, &patch, &stamp).map_err(|e| e.to_string())?;

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
    Ok(msg)
}

/// `task_list`: the user's tasks, open by default.
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
    let all = homes.scan(&today);
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

/// `tickets`: the ticket files (`tickets/<ID>.md`) of the workspace's
/// repos, read-only: the work in flight and done (knowledge-core decision 8).
/// Every ticket by default; `assignee: me` keeps the person's.
fn tickets(server: &Server, args: &Value) -> Result<String, String> {
    let mine = match args.get("assignee").and_then(Value::as_str).map(str::trim) {
        None | Some("") | Some("all") => false,
        Some("me") => true,
        Some(other) => return Err(format!("invalid \"assignee\" {other:?} — use all or me")),
    };
    let open_only = match args.get("state").and_then(Value::as_str).map(str::trim) {
        None | Some("") | Some("open") => true,
        Some("all") => false,
        Some(other) => return Err(format!("invalid \"state\" {other:?} — use open or all")),
    };
    let me = day::git_me();
    if mine && !me.is_known() {
        return Ok("git has no user.name or user.email on this machine, so no ticket can be matched to \
you. Leave assignee out to list every ticket."
            .to_string());
    }
    let registry = Registry::load(&server.base_dir).map_err(|e| format!("could not read Ken's project registry: {e}"))?;
    let inside = in_open_workspace(server, &registry);
    let repos: Vec<_> = registry.projects.iter().filter(|e| e.path.is_dir() && inside(&e.path)).collect();
    let mut found: Vec<(day::Ticket, String, Uuid)> = Vec::new();
    for e in &repos {
        for t in day::scan_tickets(&e.path) {
            if (open_only && !t.open) || (mine && !me.assigned(&t)) {
                continue;
            }
            found.push((t, e.name.clone(), e.id));
        }
    }
    if found.is_empty() {
        return Ok(format!(
            "No {}tickets{} in {} repo(s).",
            if open_only { "open " } else { "" },
            if mine { " assigned to you" } else { "" },
            repos.len()
        ));
    }
    found.sort_by(|a, b| day::ticket_order(&a.0, &b.0));
    let mut out = format!("{} ticket{}:\n", found.len(), if found.len() == 1 { "" } else { "s" });
    for (t, repo, pid) in &found {
        let mut parts = vec![if t.state.is_empty() { "no status".to_string() } else { t.state.clone() }];
        parts.push(format!("repo {repo}"));
        if let Some(d) = &t.target {
            parts.push(format!("target {d}"));
        }
        if !t.assignees.is_empty() {
            parts.push(format!("assignee {}", t.assignees.join(", ")));
        }
        out.push_str(&format!("\n{} — \"{}\" [{}] — {}", t.id, t.title, parts.join(", "), ken_address(*pid, &t.rel_path)));
    }
    Ok(out)
}

/// The open workspace's knowledge base: the first of its repos with an
/// ingest inbox, else the first Ken counts as a library (`registry::is_library`).
fn knowledge_base(server: &Server) -> Result<(Uuid, String, PathBuf), String> {
    let registry = Registry::load(&server.base_dir).map_err(|e| format!("could not read Ken's project registry: {e}"))?;
    let inside = in_open_workspace(server, &registry);
    let repos: Vec<_> = registry.projects.iter().filter(|e| e.path.is_dir() && inside(&e.path)).collect();
    repos
        .iter()
        .find(|e| ken_core::ingest::has_inbox(&e.path))
        .or_else(|| repos.iter().find(|e| registry::is_library(&e.path)))
        .map(|e| (e.id, e.name.clone(), e.path.clone()))
        .ok_or_else(|| "The open workspace has no knowledge base yet: set one up in Ken first.".to_string())
}

/// A note named the way a ruling's `sources:` line names it: its file stem,
/// with any `[[ ]]`, folders and `.md` taken off.
fn note_name(given: &str) -> String {
    let s = given.trim().trim_start_matches("[[").trim_end_matches("]]");
    let s = s.rsplit(['/', '\\']).next().unwrap_or(s);
    s.strip_suffix(".md").unwrap_or(s).trim().to_string()
}

/// `propose_decision`: a ruling from chat as a ready entry for the knowledge
/// base's decisions log (knowledge-core decision 13), filed in its Inbox the
/// way ingest files one (`proposal::file_page_proposal`, with `append` set).
/// The log is never written here: a person adds the entry in one click, and
/// it takes the next number then. A running app is told, so its Inbox
/// shows it at once (`hooks::signal_app`).
fn propose_decision(server: &Server, args: &Value) -> Result<String, String> {
    use ken_core::ingest::DECISIONS_LOG;
    use ken_core::proposal::{self, Proposal, RulingEntry};
    let ruling = require_str(args, "ruling")?;
    let why = require_str(args, "why")?;
    let (id, name, root) = knowledge_base(server)?;
    // Only an index the app made: this never makes one of its own.
    Db::open_read_only(&server.base_dir, id)
        .map_err(|_| format!("The knowledge base \"{name}\" has no index yet: open it in Ken once so Ken can build it."))?;
    let mut db = Db::open(&server.base_dir, id).map_err(|e| format!("could not open the knowledge base's index: {e}"))?;
    let log = if root.join(DECISIONS_LOG).is_file() {
        DECISIONS_LOG.to_string()
    } else {
        db.paths_named(&["decisions.md"]).ok().and_then(|p| p.into_iter().next()).unwrap_or_else(|| DECISIONS_LOG.to_string())
    };
    let text = std::fs::read_to_string(root.join(&log)).unwrap_or_else(|_| "# Decisions\n".to_string());
    let (today, _) = local_today_and_time();
    let entry = RulingEntry {
        ruling: ruling.trim().to_string(),
        who: opt_str(args, "who"),
        date: today,
        note: opt_str(args, "source").map(|s| note_name(&s)).unwrap_or_default(),
        topic: opt_str(args, "topic").unwrap_or_default(),
        why: why.trim().to_string(),
        quote: opt_str(args, "quote").unwrap_or_default(),
        aliases: str_list(args, "aliases").unwrap_or_default(),
        supersedes: str_list(args, "supersedes").unwrap_or_default(),
    };
    // Shown as the entry against the log now; added against the log as it is
    // when a person adds it.
    let proposed = ken_core::ingest::decisions_entry(&text, &entry);
    let title = format!("Ruling: {}", entry.ruling.chars().take(60).collect::<String>());
    let who = entry.who.as_deref().map(|w| format!(", made by {w}")).unwrap_or_default();
    let body = format!(
        "Proposed in chat{who}: \"{}\". Add it to the decisions log in one click; it takes the next number then. Or discard it.",
        entry.ruling
    );
    let p = Proposal { append: Some(entry), from: Some(proposal::FROM_CHAT.to_string()), ..Proposal::new(log.clone(), text, proposed) };
    proposal::file_page_proposal(&mut db, &p, &title, &body, now_secs()).map_err(|e| format!("could not file the ruling: {e}"))?;
    // A running app refreshes its Inbox now; with none, it shows at the next start.
    ken_core::hooks::signal_app(&server.base_dir, &json!({ "signal": "proposed", "projectId": id.to_string() }));
    Ok(format!(
        "Proposed for {log} in {name}: \"{}\". It waits in Ken's Inbox for a person to add it in one click (it takes the \
next number then) or discard it. Nothing was written to the log.",
        ruling.trim()
    ))
}

/// `add_source`: a file of one of the workspace's repos, or a pasted text as
/// a note (`ingest::note_source`), put in the knowledge base's inbox
/// (`Research/Ingestion/Raw/`, under a free name: `ingest::raw_target`) for
/// the next ingest pass. Nothing is ingested or edited here; a running app
/// is told, and starts the ingest (`hooks::signal_app`).
fn add_source(server: &Server, args: &Value) -> Result<String, String> {
    use ken_core::ingest::{self, MAX_SOURCE_CHARS, RAW};
    let (kb_id, kb_name, kb_root) = knowledge_base(server)?;
    let text = args.get("text").and_then(Value::as_str).filter(|t| !t.trim().is_empty());
    let target = match (opt_str(args, "path"), text) {
        (Some(_), Some(_)) => return Err("Give a file's path or the text, not both.".into()),
        (None, None) => {
            return Err("Give `path` (a file in one of the workspace's repos: a ken:// address, or a path with `project`) \
or `text` with a `title`."
                .into())
        }
        (Some(given), None) => {
            // Only a file of a registered repo, refused outside it: a source
            // is read by Claude and lands in a repo the team shares.
            let (named, rel, _) = split_address(args, &given);
            let (project, _) = resolve_project(server, &named)?;
            let abs = project
                .resolve(&rel)
                .map_err(|_| format!("{rel:?} is outside the project — paths are relative to the project root and may not contain \"..\""))?;
            if !abs.is_file() {
                return Err(format!("{rel:?} is not a file in project \"{}\".", project.config.name));
            }
            let raw = kb_root.join(RAW);
            if abs.canonicalize().ok().zip(raw.canonicalize().ok()).is_some_and(|(a, r)| a.starts_with(r)) {
                return Err(format!("{rel} is in the inbox already; the next ingest pass reads it."));
            }
            let name = abs.file_name().map(|n| n.to_string_lossy().to_string()).ok_or("a file with no name")?;
            let target = ingest::raw_target(&kb_root, &name).map_err(|e| e.to_string())?;
            std::fs::copy(&abs, &target).map_err(|e| format!("could not copy {rel}: {e}"))?;
            target
        }
        (None, Some(text)) => {
            if text.len() > MAX_SOURCE_CHARS {
                return Err(format!(
                    "The text is longer than {MAX_SOURCE_CHARS} characters, the most one source may be; put it in a file in one of the repos and add that."
                ));
            }
            let stamp = chrono::Local::now().format("%Y-%m-%d %H.%M").to_string();
            let (name, doc) = ingest::note_source(&stamp, opt_str(args, "title").as_deref(), text, None);
            let target = ingest::raw_target(&kb_root, &name).map_err(|e| e.to_string())?;
            std::fs::write(&target, doc).map_err(|e| format!("could not write {name}: {e}"))?;
            target
        }
    };
    let rel = format!("{RAW}/{}", target.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
    // A running app starts the ingest now; with none, nothing is queued and
    // the next pass reads it.
    ken_core::hooks::signal_app(&server.base_dir, &json!({ "signal": "source-added", "projectId": kb_id.to_string(), "path": rel }));
    Ok(format!(
        "Put {rel} in {kb_name}'s inbox ({}). Ken's next ingest pass reads it into a dated note and updates the pages it \
changes; nothing was ingested or edited here.",
        ken_address(kb_id, &rel)
    ))
}

/// What an agent must know before it trusts a page hit, beyond its label
/// ([`hit_tag`]): that an older generated page is fixed at its generator.
/// For a hit without a label, that it is superseded (and what replaces it,
/// when an older page says), generated, dated evidence, or when the check
/// last re-read it.
fn page_note(p: &ken_core::pagemeta::HitPage, labelled: bool) -> Option<String> {
    if labelled {
        return p.generated.then(|| "generated: fix the generator, not the page".to_string());
    }
    if p.retired {
        return Some(match p.superseded_by.is_empty() {
            true => "superseded".to_string(),
            false => format!("superseded, see {}", p.superseded_by.join(", ")),
        });
    }
    if p.generated {
        return Some("generated: fix the generator, not the page".to_string());
    }
    if let Some(d) = &p.dated {
        return Some(format!("evidence from {d}, not current state"));
    }
    p.checked.as_ref().map(|c| format!("checked {c}"))
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

fn opt_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// Which project does this call target? Scoped servers always answer with
/// their own; unscoped servers require the `project` argument.
fn resolve_project(server: &Server, args: &Value) -> Result<(Project, Option<String>), String> {
    let requested = args.get("project").and_then(|p| p.as_str()).map(str::trim);

    if let Some(root) = &server.scoped {
        // Another repo of the open workspace, when one is named (by name,
        // folder, id or a ken://<id>/ address): a search hit in a code
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
        // A workspace's server reads a bare path in its knowledge base.
        if server.workspace.is_some() {
            if let Ok((_, name, root)) = knowledge_base(server) {
                let project = Project::open(&root).map_err(|e| format!("could not open the knowledge base \"{name}\": {e}"))?;
                return Ok((project, None));
            }
        }
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

/// The folder of the workspace this server serves: the one `--workspace`
/// names, else the one Ken last had open (the registry's `last_workspace`).
fn workspace_folder(server: &Server, registry: &Registry) -> Option<PathBuf> {
    if let Some(dir) = &server.workspace {
        return Some(dir.clone());
    }
    let id = registry.last_workspace?;
    registry.workspaces.iter().find(|w| w.id == id).map(|w| w.path.clone())
}

/// Whether a registered project belongs to the workspace this server serves
/// ([`workspace_folder`]): its folder is inside the workspace's, or it is
/// one of the workspace's sources. With no workspace, every project does. A
/// search with no project named stays inside that workspace, so another
/// client's files are never found or cited.
fn in_open_workspace(server: &Server, registry: &Registry) -> impl Fn(&std::path::Path) -> bool {
    let folder = workspace_folder(server, registry);
    let root = folder.as_deref().map(norm_path);
    // A workspace set up from picked repos lives in Ken's app data and its
    // repos anywhere: its members count as inside it.
    let members: Vec<String> = folder
        .and_then(|f| ken_core::workspace::Workspace::open(&f).ok())
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

/// The workspace ken-mcp should read/write memories and the journal for:
/// the one `--workspace` names, else Ken's registry `lastWorkspace` entry
/// (the app itself reopens it on launch), or a clear "no workspace" message.
fn resolve_workspace_root(server: &Server) -> Result<PathBuf, String> {
    let registry = Registry::load(&server.base_dir)
        .map_err(|e| format!("could not read Ken's project registry: {e}"))?;
    workspace_folder(server, &registry)
        .ok_or_else(|| "No Ken workspace is open — open a workspace folder in the Ken app first.".to_string())
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
/// app calls today.
fn local_today_and_time() -> (String, String) {
    let now = chrono::Local::now();
    (now.format("%Y-%m-%d").to_string(), now.format("%H:%M").to_string())
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

    /// The knowledge server's tools, in decision 22's order.
    const KNOWLEDGE_TOOLS: [&str; 12] = [
        "search",
        "read",
        "related",
        "code_definition",
        "code_usages",
        "code_outline",
        "code_imports",
        "history",
        "tickets",
        "propose_decision",
        "add_source",
        "repos",
    ];

    /// The personal set (`--personal`), without open_in_ken (no app here).
    const PERSONAL_TOOLS: [&str; 5] = ["task_list", "task_create", "task_update", "memory_write", "journal_append"];

    #[test]
    fn tools_list_names_the_base_tools() {
        let mut fx = fixture(true);
        let reply = call(&mut fx.server, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap();
        let tools = reply["result"]["tools"].as_array().unwrap();
        let names: Vec<_> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            KNOWLEDGE_TOOLS
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

        let (text, is_err) = tool(&mut fx.server, "code_definition", json!({"name": "save_world"}));
        assert!(!is_err, "{text}");
        assert!(text.contains("function save_world") && text.contains("app/store.py#L2"), "{text}");
        assert!(text.contains("Keeps the world on disk"), "docs: {text}");

        let (text, _) = tool(&mut fx.server, "code_usages", json!({"name": "save_world"}));
        assert!(text.contains("app/game.py#L4") && text.contains("called from: tick"), "{text}");

        let (text, _) = tool(&mut fx.server, "code_outline", json!({"path": "app/store.py"}));
        assert!(text.contains("class Store") && text.contains("\n  method open") || text.contains("\n  function open"), "{text}");

        // The project graph answers, with the code index's imports for a
        // code file (decision 21).
        let (text, _) = tool(&mut fx.server, "related", json!({"path": "app/game.py"}));
        assert!(text.contains("\n  - imports ken://") && text.contains("app/store.py"), "{text}");
        let (text, _) = tool(&mut fx.server, "related", json!({"path": "app/store.py"}));
        assert!(text.contains("\n  - ← imports ken://") && text.contains("app/game.py"), "{text}");

        // code_imports: a file's imports, importers and libraries (decision 22).
        let (text, is_err) = tool(&mut fx.server, "code_imports", json!({"path": "app/game.py"}));
        assert!(!is_err, "{text}");
        assert!(text.contains("\nImports:\n  - ken://") && text.contains("/app/store.py\nImported by: (none)"), "{text}");
        let (text, _) = tool(&mut fx.server, "code_imports", json!({"path": "app/store.py"}));
        assert!(text.contains("\nImports: (none)\nImported by:\n  - ken://") && text.contains("/app/game.py"), "{text}");
        let (text, is_err) = tool(&mut fx.server, "code_imports", json!({"path": "notes/meeting.md"}));
        assert!(is_err && text.contains("not in the code index"), "{text}");

        let (text, _) = tool(&mut fx.server, "code_definition", json!({"name": "nothing_like_it"}));
        assert!(text.contains("look directly"), "empty says to look: {text}");

        // Plain words reach the definition; the hit names its symbol and
        // offers the calls that go further (decision 26).
        let (text, _) = tool(&mut fx.server, "search", json!({"query": "save world"}));
        assert!(text.contains("app/store.py:2 `save_world`"), "{text}");
        assert!(text.contains("next: code_usages save_world · code_outline app/store.py · code_imports app/store.py"), "{text}");
        assert!(!text.contains("next: code_outline notes/"), "pages offer no code calls: {text}");
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
        let (text, is_err) = tool(&mut fx.server, "search", json!({"query": "billing"}));
        assert!(!is_err, "{text}");
        assert!(text.contains("[note · ") && text.contains("] Atlas:notes/meeting.md"), "a note labelled with its kind: {text}");
        let (text, _) = tool(&mut fx.server, "search", json!({"query": "billing", "type": "code"}));
        assert!(!text.contains("notes/meeting.md"), "code only: {text}");
        let (text, _) = tool(&mut fx.server, "search", json!({"query": "billing", "type": "note,doc"}));
        assert!(text.contains("notes/meeting.md"), "{text}");
    }

    #[test]
    fn scoped_tools_work_and_ignore_project_arg() {
        let mut fx = fixture(true);
        let (text, is_err) = tool(&mut fx.server, "search", json!({"query": "billing cutover"}));
        assert!(!is_err, "{text}");
        assert!(text.contains("notes/meeting.md"), "{text}");
        assert!(!text.contains("<mark>"), "{text}");

        let (text, is_err) = tool(&mut fx.server, "read", json!({"path": "People.md"}));
        assert!(!is_err);
        assert!(text.contains("Priya Natarajan"), "{text}");

        // A supplied project the registry lacks is ignored with a note.
        let (text, is_err) = tool(&mut fx.server, "read", json!({"path": "People.md", "project": "Other"}));
        assert!(!is_err);
        assert!(text.contains("locked to project \"Atlas\"") && text.contains("Priya Natarajan"), "{text}");

        // A folder lists its indexed files (what list_documents was), and `.`
        // the whole project's.
        let (text, is_err) = tool(&mut fx.server, "read", json!({"path": "notes"}));
        assert!(!is_err);
        assert!(text.contains("under \"notes\"") && text.contains("notes/meeting.md"), "{text}");
        assert!(!text.contains("People.md"), "{text}");
        let (text, _) = tool(&mut fx.server, "read", json!({"path": "."}));
        assert!(text.contains("notes/meeting.md") && text.contains("People.md"), "{text}");
    }

    #[test]
    fn read_refuses_escapes() {
        let mut fx = fixture(true);
        for path in ["../etc/passwd", "/etc/passwd", "notes/../../etc/passwd"] {
            let (text, is_err) = tool(&mut fx.server, "read", json!({"path": path}));
            assert!(is_err, "{path} should be refused");
            assert!(text.contains("outside the project"), "{text}");
        }
        // And an unknown-but-safe path is a helpful index miss.
        let (text, is_err) = tool(&mut fx.server, "read", json!({"path": "nope.md"}));
        assert!(is_err);
        assert!(text.contains("not in project"), "{text}");
    }

    #[test]
    fn read_caps_large_files() {
        let mut fx = fixture(true);
        let big = "é".repeat(150 * 1024); // 300 KB of two-byte chars
        std::fs::write(fx.root.join("big.txt"), &big).unwrap();
        let project = Project::open(&fx.root).unwrap();
        let mut db = Db::open(&fx.server.base_dir, project.config.id).unwrap();
        scan::scan(&project, &mut db).unwrap();
        drop(db);

        let (text, is_err) = tool(&mut fx.server, "read", json!({"path": "big.txt"}));
        assert!(!is_err, "{}", &text[..200.min(text.len())]);
        assert!(text.contains("[truncated"), "no truncation note");
        assert!(text.len() < 210 * 1024, "way over cap: {}", text.len());
    }

    #[test]
    fn unscoped_search_covers_every_repo_and_a_path_needs_its_project() {
        let mut fx = fixture(false);
        // Search needs no project: every repo of the workspace.
        let (text, is_err) = tool(&mut fx.server, "search", json!({"query": "billing"}));
        assert!(!is_err, "{text}");
        assert!(text.contains("notes/meeting.md"), "{text}");

        // An unknown project is a helpful error naming the projects.
        let (text, is_err) = tool(&mut fx.server, "search", json!({"query": "billing", "project": "Zeus"}));
        assert!(is_err);
        assert!(text.contains("No Ken project matches") && text.contains("Available projects: Atlas"), "{text}");

        // Match by name (case-insensitive) and by path.
        let (text, is_err) = tool(&mut fx.server, "search", json!({"query": "billing", "project": "atlas"}));
        assert!(!is_err, "{text}");
        assert!(text.contains("notes/meeting.md"));
        let root = fx.root.to_string_lossy().to_string();
        let (text, is_err) = tool(&mut fx.server, "read", json!({"path": ".", "project": root}));
        assert!(!is_err, "{text}");
        assert!(text.contains("People.md"));

        // A project-relative path needs its project.
        let (text, is_err) = tool(&mut fx.server, "read", json!({"path": "People.md"}));
        assert!(is_err);
        assert!(text.contains("Available projects: Atlas"), "{text}");

        // repos works without any scoping.
        let (text, is_err) = tool(&mut fx.server, "repos", json!({}));
        assert!(!is_err);
        assert!(text.contains("Atlas"), "{text}");
    }

    #[test]
    fn missing_required_args_are_tool_errors() {
        let mut fx = fixture(true);
        let (text, is_err) = tool(&mut fx.server, "search", json!({}));
        assert!(is_err);
        assert!(text.contains("\"query\" argument is required"), "{text}");
        let (text, is_err) = tool(&mut fx.server, "propose_decision", json!({"ruling": "x"}));
        assert!(is_err);
        assert!(text.contains("\"why\" argument is required"), "{text}");
        let (text, is_err) = tool(&mut fx.server, "read", json!({}));
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

    /// Builds a real (if tiny) semantic index with `FakeEmbedder`, the same
    /// production `engine::rebuild_semantic_index` the app uses, with the
    /// deterministic no-model embedder tests get everywhere else in ken-core,
    /// so a search can use meaning and no source reads as still building.
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
    /// base dir, unscoped: what the routing and per-project-isolation tests
    /// below need.
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

        AppSettings::default().save(base.path()).unwrap();

        let server = Server {
            base_dir: base.path().to_path_buf(),
            scoped: None,
            ..Default::default()
        };
        (base, root_a, root_b, server)
    }

    #[test]
    fn search_searches_only_the_source_the_question_names() {
        let (_base, _ra, _rb, mut server) = two_project_fixture();
        let reply = call(&mut server, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap();
        let tools = reply["result"]["tools"].as_array().unwrap();
        let names: Vec<_> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, KNOWLEDGE_TOOLS);
        for t in tools {
            assert_eq!(t["inputSchema"]["type"], "object", "schema for {}", t["name"]);
            assert!(t["description"].as_str().is_some_and(|d| !d.is_empty()), "{}", t["name"]);
        }

        // "Atlas" names a member, so only Atlas is searched.
        let (text, is_err) = tool(&mut server, "search", json!({"query": "Atlas launch schedule"}));
        assert!(!is_err, "{text}");
        assert!(text.starts_with("Route: named in the question (1 searched)."), "{text}");
        assert!(text.contains("a.md"), "{text}");
        assert!(!text.contains("b.md"), "{text}");

        // Named by nothing: every source, merged once.
        let (text, _) = tool(&mut server, "search", json!({"query": "launch schedule"}));
        assert!(text.starts_with("Route: every source (2 searched)."), "{text}");
        assert!(text.contains("Sources: Atlas (searched), Zeus (searched)") || text.contains("Sources: Zeus (searched), Atlas (searched)"), "{text}");
        assert!(text.contains("a.md") && text.contains("b.md"), "{text}");

        // Both named by `project`, in its order.
        let (text, _) = tool(&mut server, "search", json!({"query": "launch schedule", "project": "Zeus, Atlas"}));
        assert!(text.starts_with("Route: the project asked for (2 searched)."), "{text}");
        assert!(text.contains("a.md") && text.contains("b.md"), "{text}");
    }

    /// A reference repo is held back from a question about the team's own
    /// work and said so; it joins when nothing else answers.
    #[test]
    fn search_holds_a_reference_repo_back_until_nothing_else_answers() {
        let (_base, _ra, rb, mut server) = two_project_fixture();
        let mut registry = Registry::load(&server.base_dir).unwrap();
        let zeus = registry.projects.iter().find(|e| e.name == "Zeus").unwrap().id;
        assert!(registry.set_kind(zeus, vec![registry::RepoKind::Reference]));
        registry.save(&server.base_dir).unwrap();
        std::fs::write(rb.path().join("engine.md"), "The rendering pipeline draws every chunk.\n").unwrap();
        let project = Project::open(rb.path()).unwrap();
        let mut db = Db::open(&server.base_dir, project.config.id).unwrap();
        scan::scan(&project, &mut db).unwrap();
        index_semantically(&project, &mut db);
        drop(db);

        let (text, _) = tool(&mut server, "search", json!({"query": "launch schedule"}));
        assert!(text.contains("Held back: Zeus"), "{text}");
        assert!(text.contains("a.md") && !text.contains("b.md"), "{text}");

        let (text, _) = tool(&mut server, "search", json!({"query": "rendering pipeline"}));
        assert!(!text.contains("Held back"), "{text}");
        assert!(text.contains("engine.md"), "nothing else answered: {text}");
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
    fn search_with_a_project_touches_only_that_member() {
        let (_base, _ra, _rb, mut server) = two_project_fixture();
        let (text, is_err) = tool(&mut server, "search", json!({"query": "launch schedule", "project": "Atlas"}));
        assert!(!is_err, "{text}");
        assert!(text.starts_with("Route: the project asked for (1 searched).\nSources: Atlas (searched)"), "{text}");
        assert!(text.contains("a.md"), "{text}");
        assert!(!text.contains("b.md"), "{text}");
        assert!(text.contains("ken://"), "{text}");

        let (text, is_err) = tool(&mut server, "search", json!({"query": "launch schedule", "project": "Zeus"}));
        assert!(!is_err, "{text}");
        assert!(text.contains("Sources: Zeus (searched)"), "{text}");
        assert!(text.contains("b.md"), "{text}");
        assert!(!text.contains("a.md"), "{text}");
    }

    #[test]
    fn search_uses_meaning_when_the_model_is_there_and_says_so_when_not() {
        let (_base, _ra, _rb, mut server) = two_project_fixture();
        // No model installed: keyword only, in one line.
        let (text, is_err) = tool(&mut server, "search", json!({"query": "launch schedule", "project": "Atlas"}));
        assert!(!is_err, "{text}");
        assert!(text.contains("Keyword search only: no embedding model is installed"), "{text}");
        assert!(text.contains("[keyword]") && !text.contains("semantic]"), "{text}");
        let (text, _) = tool(&mut server, "search", json!({"query": "launch schedule"}));
        assert!(text.starts_with("Route: "), "the note follows the route: {text}");
        assert!(text.contains("\nKeyword search only:"), "{text}");

        // The model the indexes were built with: meaning joins keyword.
        server.meaning = RefCell::new(Meaning::Ready(Box::new(ken_core::embedder::FakeEmbedder::new())));
        let (text, _) = tool(&mut server, "search", json!({"query": "launch schedule", "project": "Atlas"}));
        assert!(!text.contains("Keyword search only"), "{text}");
        assert!(text.contains("[keyword+semantic]"), "{text}");
        let (text, _) = tool(&mut server, "search", json!({"query": "launch schedule"}));
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
    fn search_hits_are_short_and_point_at_the_matching_line() {
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

        let (text, is_err) = tool(&mut server, "search", json!({"query": "Atlas rollback window ninety minutes"}));
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

        let (text, is_err) = tool(&mut server, "search", json!({"query": "companion pets follow the player"}));
        assert!(!is_err, "{text}");
        assert!(text.contains("tickets/AT-7.md"), "{text}");
        assert!(text.contains("[ticket · superseded · 2026-07-21]"), "{text}");
        assert!(text.contains("[decision D-007 · binding · 2026-10-07]"), "the entry's own id and date: {text}");
        let (text, _) = tool(&mut server, "search", json!({"query": "companion pets follow the player", "project": "Atlas"}));
        assert!(text.contains("[ticket · superseded · 2026-07-21]") && text.contains("[decision D-007 · binding · 2026-10-07]"), "{text}");
    }

    #[test]
    fn read_reads_lines_around_an_address_or_a_range() {
        let mut fx = fixture(true);
        let body: String = (1..=100).map(|i| format!("row {i}\n")).collect();
        std::fs::write(fx.root.join("rows.txt"), &body).unwrap();
        let project = Project::open(&fx.root).unwrap();
        let mut db = Db::open(&fx.server.base_dir, project.config.id).unwrap();
        scan::scan(&project, &mut db).unwrap();
        drop(db);
        let address = format!("ken://{}/rows.txt#L50", project.config.id);

        let (text, is_err) = tool(&mut fx.server, "read", json!({"path": address}));
        assert!(!is_err, "{text}");
        assert!(text.contains("[lines 40-90 of 100]\n40: row 40\n"), "{text}");
        assert!(text.ends_with("90: row 90\n"), "{text}");

        let (text, _) = tool(&mut fx.server, "read", json!({"path": "rows.txt", "start_line": 3, "end_line": 4}));
        assert_eq!(text, "[lines 3-4 of 100]\n3: row 3\n4: row 4\n");
        let (text, _) = tool(&mut fx.server, "read", json!({"path": "rows.txt#L98-L120"}));
        assert!(text.starts_with("[lines 98-100 of 100]"), "{text}");
        let (text, _) = tool(&mut fx.server, "read", json!({"path": "rows.txt", "start_line": 500}));
        assert!(text.contains("no line 500"), "{text}");
        // No range: the whole file, as before.
        let (text, _) = tool(&mut fx.server, "read", json!({"path": "rows.txt"}));
        assert_eq!(text, body);

        // An unscoped server finds the project by the address's id.
        fx.server.scoped = None;
        let (text, is_err) = tool(&mut fx.server, "read", json!({"path": address}));
        assert!(!is_err, "{text}");
        assert!(text.starts_with("[lines 40-90 of 100]"), "{text}");
    }

    #[test]
    fn read_reads_the_section_or_log_entry_an_address_is_in() {
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
        let (text, is_err) = tool(&mut fx.server, "read", json!({"path": "Law.md#L65"}));
        assert!(!is_err, "{text}");
        assert!(text.starts_with("[section \"Grades\": lines 7-68 of"), "{text}");
        assert!(text.contains("\n9: Grade lever 1.\n") && text.ends_with("68: Grade lever 60.\n"), "{text}");
        assert!(!text.contains("Health"), "{text}");
        // A long section is cut at about 8,000 characters, and says where to read on.
        let (text, _) = tool(&mut fx.server, "read", json!({"path": "Law.md#L300"}));
        assert!(text.contains("cut at 8000 characters: read on with start_line="), "{text}");
        assert!(text.contains("\n300: A long line") && text.len() < 8_400, "{} characters", text.len());
        // A decisions log: the entry.
        let (text, _) = tool(&mut fx.server, "read", json!({"path": "DECISIONS.md#L9"}));
        assert_eq!(text, "[entry D-005: lines 9-9 of 18]\n9: **D-005** · 2026-10-06 · pets — **RULING 5.** Chris.\n");
        // Code keeps the line window; an explicit range still wins.
        let (text, _) = tool(&mut fx.server, "read", json!({"path": "code.rs#L50"}));
        assert!(text.starts_with("[lines 40-90 of 100]"), "{text}");
        let (text, _) = tool(&mut fx.server, "read", json!({"path": "Law.md#L65", "start_line": 65, "end_line": 66}));
        assert_eq!(text, "[lines 65-66 of 475]\n65: Grade lever 57.\n66: Grade lever 58.\n");
        let (text, _) = tool(&mut fx.server, "read", json!({"path": "Law.md#L65-L66"}));
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
        let (text, is_err) = tool(&mut server, "search", json!({"query": "server never overwrites spawn anchors merge", "project": "Atlas"}));
        assert!(!is_err, "{text}");
        let earlier = text.find("[decision D-379 · superseded · 2026-10-05 · superseded by D-410 on who writes anchors]").expect(&text);
        let later = text.find("[decision D-410 · binding · 2026-10-06 · supersedes D-379]").expect(&text);
        assert!(later < earlier, "the later ruling ranks first: {text}");
        let (text, _) = tool(&mut server, "search", json!({"query": "server never overwrites spawn anchors merge"}));
        assert!(text.contains("superseded by D-410 on who writes anchors"), "{text}");

        let (text, _) = tool(&mut server, "read", json!({"path": "decisions/DECISIONS.md#L19", "project": "Atlas"}));
        assert!(text.starts_with("[entry D-379: lines 19-19 of 19]"), "{text}");
        assert!(text.ends_with("[D-410 (line 7) supersedes this entry on who writes anchors]\n"), "{text}");
        let (text, _) = tool(&mut server, "read", json!({"path": "decisions/DECISIONS.md#L7", "project": "Atlas"}));
        assert!(text.ends_with("[this entry supersedes D-379 (line 19) on who writes anchors]\n"), "{text}");
    }

    #[test]
    fn repos_says_which_repos_search_can_use() {
        // Routed search is built in, whatever settings.json says.
        let mut fx = fixture(true);
        let mut settings = AppSettings::default();
        settings.features.insert("kgRouting".into(), false.into());
        settings.save(&fx.server.base_dir).unwrap();
        let (text, is_err) = tool(&mut fx.server, "repos", json!({}));
        assert!(!is_err, "{text}");
        assert!(text.contains(" indexed") && text.contains("controls"), "the health line: {text}");
        assert!(!text.contains("(knowledge base)"), "Atlas has no inbox: {text}");
    }

    // --- the brief, propose_decision and add_source (slice 10) ---

    /// The knowledge server sends the brief as its `instructions` (decision
    /// 1), led by where the knowledge base is; the personal set names itself
    /// and says what it is for.
    #[test]
    fn initialize_sends_the_brief_as_instructions() {
        let (_kb, mut server) = kb_fixture();
        let reply = call(&mut server, r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{}}"#).unwrap();
        let text = reply["result"]["instructions"].as_str().unwrap().replace('\n', " ");
        assert!(text.starts_with("The knowledge base of the open workspace is Wiki, at "), "{text}");
        for words in [
            "The code is the truth for how the product works today",
            "never copies it",
            "Decisions/DECISIONS.md",
            "use the code index before Grep and Glob",
            "a later ruling over an earlier one",
            "over a ticket or note",
            "a current page over a record",
            "A ruling is settled",
            "An empty search proves nothing",
            "Cite every fact by the ken:// address",
            "propose_decision",
            "add_source",
        ] {
            assert!(text.contains(words), "{words}: {text}");
        }
        for tool in KNOWLEDGE_TOOLS {
            assert!(text.contains(tool), "the brief names {tool}");
        }
        assert_eq!(reply["result"]["serverInfo"]["name"], "ken");

        // No knowledge base yet: said, and the brief still follows.
        let mut fx = fixture(true);
        let reply = call(&mut fx.server, r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{}}"#).unwrap();
        let text = reply["result"]["instructions"].as_str().unwrap();
        assert!(text.starts_with("No knowledge base is set up in the open workspace yet"), "{text}");
        assert!(text.contains("The code is the truth"), "{text}");

        fx.server.personal = true;
        let reply = call(&mut fx.server, r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{}}"#).unwrap();
        assert_eq!(reply["result"]["serverInfo"]["name"], "ken-personal");
        let text = reply["result"]["instructions"].as_str().unwrap();
        assert!(text.contains("task_list") && text.contains("only when the person asks"), "{text}");
    }

    /// A registered knowledge base (an inbox and a decisions log) and a code
    /// repo beside it, indexed, unscoped.
    fn kb_fixture() -> (Vec<tempfile::TempDir>, Server) {
        let base = tempfile::tempdir().unwrap();
        let kb = tempfile::tempdir().unwrap();
        let code = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(kb.path().join(ken_core::ingest::RAW)).unwrap();
        std::fs::create_dir_all(kb.path().join("Decisions")).unwrap();
        std::fs::write(kb.path().join("Decisions/DECISIONS.md"), "# Decisions\n\n## D-001 · 2026-10-01 · saves\n\n**Saves are regions.**\n").unwrap();
        std::fs::create_dir_all(code.path().join("docs")).unwrap();
        std::fs::write(code.path().join("docs/spec.md"), "# Save spec\n\nRegions are written whole.\n").unwrap();
        let mut registry = Registry::default();
        for (root, name) in [(kb.path(), "Wiki"), (code.path(), "Game")] {
            let project = Project::create(root, name).unwrap();
            let mut db = Db::open(base.path(), project.config.id).unwrap();
            scan::scan(&project, &mut db).unwrap();
            registry.add(&project);
        }
        registry.save(base.path()).unwrap();
        let server = Server { base_dir: base.path().to_path_buf(), ..Default::default() };
        (vec![base, kb, code], server)
    }

    /// A ruling from chat is filed as a ready entry in the knowledge base's
    /// Inbox, the way ingest files one; the log is not touched (decision 13).
    #[test]
    fn propose_decision_files_a_ready_entry_and_never_writes_the_log() {
        let (dirs, mut server) = kb_fixture();
        let log_path = dirs[1].path().join("Decisions/DECISIONS.md");
        let before = std::fs::read_to_string(&log_path).unwrap();
        let (text, is_err) = tool(
            &mut server,
            "propose_decision",
            json!({
                "ruling": "Combat ships after region saves",
                "why": "combat needs the region format",
                "topic": "combat",
                "who": "Ana",
                "aliases": ["combat order"],
                "supersedes": ["D-001"],
                "source": "[[2026-10-12-standup]]"
            }),
        );
        assert!(!is_err, "{text}");
        assert!(text.contains("waits in Ken's Inbox") && text.contains("Nothing was written to the log"), "{text}");
        assert_eq!(std::fs::read_to_string(&log_path).unwrap(), before, "the log is never written here");

        let registry = Registry::load(&server.base_dir).unwrap();
        let wiki = registry.projects.iter().find(|e| e.name == "Wiki").unwrap().id;
        let db = Db::open_read_only(&server.base_dir, wiki).unwrap();
        let items = db.review_items_of_kind(ken_core::proposal::PROPOSAL_KIND, 10).unwrap();
        assert_eq!(items.len(), 1);
        assert!(items[0].title.starts_with("Ruling: Combat ships after region saves"), "{}", items[0].title);
        let p: ken_core::proposal::Proposal = serde_json::from_str(items[0].payload.as_deref().unwrap()).unwrap();
        assert_eq!(ken_core::proposal::kind_of(&p), "ruling");
        assert_eq!(p.page, "Decisions/DECISIONS.md");
        assert_eq!(p.from.as_deref(), Some(ken_core::proposal::FROM_CHAT));
        let entry = p.append.as_ref().unwrap();
        assert_eq!((entry.note.as_str(), entry.who.as_deref()), ("2026-10-12-standup", Some("Ana")));
        assert!(p.proposed.contains("\n## D-002 · ") && p.proposed.contains("**Combat ships after region saves.**"), "{}", p.proposed);
        assert!(p.proposed.contains("sources: [[2026-10-12-standup]]\naliases: combat order\nsupersedes: D-001\n"), "{}", p.proposed);

        // Without a knowledge base there is nowhere to propose to.
        let mut fx = fixture(false);
        let (text, is_err) = tool(&mut fx.server, "propose_decision", json!({"ruling": "x", "why": "y"}));
        assert!(is_err && text.contains("no knowledge base"), "{text}");
    }

    /// A source lands in the knowledge base's Raw/ under a free name, as a
    /// pasted text or a file of a workspace repo; nothing is ingested.
    #[test]
    fn add_source_puts_a_text_or_a_repo_file_in_raw() {
        let (dirs, mut server) = kb_fixture();
        let raw = dirs[1].path().join(ken_core::ingest::RAW);

        let (text, is_err) = tool(&mut server, "add_source", json!({"text": "Ana: ship combat after saves.", "title": "Combat call"}));
        assert!(!is_err, "{text}");
        assert!(text.contains("Research/Ingestion/Raw/") && text.contains("Note - Combat call.md") && text.contains("nothing was ingested"), "{text}");
        let notes: Vec<_> = std::fs::read_dir(&raw).unwrap().flatten().map(|e| e.path()).collect();
        assert_eq!(notes.len(), 1, "{notes:?}");
        let doc = std::fs::read_to_string(&notes[0]).unwrap();
        assert!(doc.starts_with("---\nkind: note\n") && doc.contains("# Combat call\n\nAna: ship combat after saves.\n"), "{doc}");

        // A file of another repo, by project and path; twice gets a number.
        for _ in 0..2 {
            let (text, is_err) = tool(&mut server, "add_source", json!({"path": "docs/spec.md", "project": "Game"}));
            assert!(!is_err, "{text}");
        }
        assert!(raw.join("spec.md").is_file() && raw.join("spec (2).md").is_file());
        assert_eq!(std::fs::read_to_string(raw.join("spec.md")).unwrap(), "# Save spec\n\nRegions are written whole.\n");

        // Refused: outside a repo, not a file, both or neither, and a file
        // already in the inbox.
        let (text, is_err) = tool(&mut server, "add_source", json!({"path": "../secret.txt", "project": "Game"}));
        assert!(is_err && text.contains("outside the project"), "{text}");
        let (text, is_err) = tool(&mut server, "add_source", json!({"path": "docs", "project": "Game"}));
        assert!(is_err && text.contains("not a file"), "{text}");
        let (_, is_err) = tool(&mut server, "add_source", json!({"path": "docs/spec.md", "project": "Game", "text": "x"}));
        assert!(is_err);
        let (_, is_err) = tool(&mut server, "add_source", json!({}));
        assert!(is_err);
        let (text, is_err) = tool(&mut server, "add_source", json!({"path": "Research/Ingestion/Raw/spec.md", "project": "Wiki"}));
        assert!(is_err && text.contains("in the inbox already"), "{text}");
    }

    /// repos marks the knowledge base.
    #[test]
    fn repos_marks_the_knowledge_base() {
        let (_dirs, mut server) = kb_fixture();
        let (text, is_err) = tool(&mut server, "repos", json!({}));
        assert!(!is_err, "{text}");
        assert!(text.contains("\nWiki (knowledge base) — ") && text.contains("\nGame — "), "{text}");
    }

    /// `--workspace` serves the workspace it names, whatever Ken last had
    /// open: search, repos and the knowledge base are its sources, and a
    /// bare path is read in its knowledge base. Without it the server
    /// follows the workspace Ken last had open.
    #[test]
    fn a_workspace_server_serves_its_whole_workspace() {
        let base = tempfile::tempdir().unwrap();
        let ws_a = tempfile::tempdir().unwrap();
        let ws_b = tempfile::tempdir().unwrap();
        // The knowledge base is the source holding the ingest inbox.
        std::fs::create_dir_all(ws_a.path().join("wiki").join(ken_core::ingest::RAW)).unwrap();
        std::fs::create_dir_all(ws_a.path().join("game")).unwrap();
        std::fs::create_dir_all(ws_b.path().join("zeus")).unwrap();
        std::fs::write(ws_a.path().join("wiki/plan.md"), "The launch schedule is in May.\n").unwrap();
        std::fs::write(ws_a.path().join("game/notes.md"), "Game notes about the launch schedule.\n").unwrap();
        std::fs::write(ws_b.path().join("zeus/b.md"), "Zeus notes about the launch schedule.\n").unwrap();
        let a = ken_core::workspace::Workspace::create(ws_a.path(), "A", &["wiki".to_string(), "game".to_string()]).unwrap();
        let b = ken_core::workspace::Workspace::create(ws_b.path(), "B", &["zeus".to_string()]).unwrap();
        let mut registry = Registry::default();
        for root in [ws_a.path().join("wiki"), ws_a.path().join("game"), ws_b.path().join("zeus")] {
            let project = Project::open(&root).unwrap();
            let mut db = Db::open(base.path(), project.config.id).unwrap();
            scan::scan(&project, &mut db).unwrap();
            index_semantically(&project, &mut db);
            registry.add(&project);
        }
        registry.add_workspace(&a, None, 0);
        registry.add_workspace(&b, None, 0);
        // Ken last had the other workspace open.
        registry.last_workspace = Some(b.config.id);
        registry.save(base.path()).unwrap();
        AppSettings::default().save(base.path()).unwrap();

        let mut server = Server { base_dir: base.path().to_path_buf(), workspace: Some(ws_a.path().to_path_buf()), ..Default::default() };
        let (text, is_err) = tool(&mut server, "search", json!({"query": "launch schedule"}));
        assert!(!is_err, "{text}");
        assert!(text.contains("plan.md") && text.contains("notes.md"), "every source of the workspace: {text}");
        assert!(!text.contains("b.md"), "never the other workspace: {text}");
        let (text, _) = tool(&mut server, "repos", json!({}));
        assert!(text.contains("wiki") && text.contains("game") && !text.contains("zeus"), "{text}");
        let (text, is_err) = tool(&mut server, "read", json!({"path": "plan.md"}));
        assert!(!is_err, "a bare path is read in the knowledge base: {text}");
        assert!(text.contains("The launch schedule is in May."), "{text}");
        let instructions = instructions(&server);
        assert!(instructions.contains("The knowledge base of the open workspace is wiki"), "{instructions}");

        // Without `--workspace`: the workspace Ken last had open.
        let mut server = Server { base_dir: base.path().to_path_buf(), ..Default::default() };
        let (text, _) = tool(&mut server, "search", json!({"query": "launch schedule"}));
        assert!(text.contains("b.md") && !text.contains("plan.md"), "{text}");
        let (text, is_err) = tool(&mut server, "read", json!({"path": "plan.md"}));
        assert!(is_err && text.contains("\"project\" argument is required"), "{text}");
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

        let server = Server { base_dir: base.path().to_path_buf(), scoped: None, personal: true, ..Default::default() };
        (base, ws_parent, proj_root, server)
    }

    /// Memory and tasks left the knowledge server (decision 22): they are
    /// the personal set, which only Ken's own chat registers.
    #[test]
    fn memory_and_task_tools_are_the_personal_set_only() {
        let mut fx = fixture(true);
        let list = |server: &mut Server| -> Vec<String> {
            let reply = call(server, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap();
            reply["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect()
        };
        let knowledge = list(&mut fx.server);
        for gone in ["memory_write", "journal_append", "task_create", "task_update", "task_list", "open_in_ken"] {
            assert!(!knowledge.iter().any(|n| n == gone), "{gone}: {knowledge:?}");
        }
        fx.server.personal = true;
        assert_eq!(list(&mut fx.server), PERSONAL_TOOLS);

        // Neither set answers the other's tools.
        let reply = call(&mut fx.server, r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"search","arguments":{"query":"x"}}}"#).unwrap();
        assert_eq!(reply["error"]["code"], -32602, "{reply}");
        fx.server.personal = false;
        let reply = call(&mut fx.server, r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"task_list","arguments":{}}}"#).unwrap();
        assert_eq!(reply["error"]["code"], -32602, "{reply}");
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
        let mut server = Server { base_dir: base.path().to_path_buf(), scoped: None, personal: true, ..Default::default() };

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

        let server = Server { base_dir: base.path().to_path_buf(), scoped: None, personal: true, ..Default::default() };
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
        for n in ["task_create", "task_update", "task_list"] {
            assert!(names.contains(&n), "{names:?}");
        }
        for gone in ["task_complete", "task_claim", "goal_create", "goal_list", "ticket_list", "tickets"] {
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
    fn tickets_reads_the_repos_ticket_files() {
        let (_base, _ws, server) = task_fixture();
        // A knowledge-server tool: the same registry, not the personal set.
        let mut server = Server { base_dir: server.base_dir.clone(), ..Default::default() };
        let (text, is_err) = tool(&mut server, "tickets", json!({}));
        assert!(!is_err, "{text}");
        assert!(text.contains("ATT-014 — \"Retry rule for the scheduler\" [in progress, repo atlas"), "every assignee by default: {text}");
        assert!(text.contains("assignee nobody-here"), "{text}");
        assert!(text.contains("/tickets/ATT-014.md"), "{text}");
        assert!(!text.contains("ATT-009"), "done tickets are not open: {text}");
        assert!(!text.contains(" task"), "the person's tasks are not the knowledge server's: {text}");
        let (text, _) = tool(&mut server, "tickets", json!({"state": "all"}));
        assert!(text.contains("ATT-009 — \"Glossary names\""), "{text}");
        // Nobody on this machine is "nobody-here": "me" finds nothing (or
        // says git has no identity), and never errors.
        let (text, is_err) = tool(&mut server, "tickets", json!({"assignee": "me"}));
        assert!(!is_err, "{text}");
        assert!(!text.contains("ATT-014 —"), "{text}");
        let (text, is_err) = tool(&mut server, "tickets", json!({"assignee": "nobody"}));
        assert!(is_err, "{text}");
    }

    #[test]
    fn tool_list_is_fixed_with_no_flags() {
        // The task tools are always in the personal set: kenTasks is gone.
        let mut fx = fixture(true);
        let names = |server: &mut Server| -> Vec<String> {
            let reply = call(server, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap();
            reply["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect()
        };
        assert_eq!(names(&mut fx.server), KNOWLEDGE_TOOLS);
        fx.server.personal = true;
        assert_eq!(names(&mut fx.server), PERSONAL_TOOLS);
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

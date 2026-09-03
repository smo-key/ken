//! ken-mcp — stdio MCP server over ken-core. Hand-rolled JSON-RPC 2.0:
//! one JSON object per line on stdout, stdin read line-by-line, nothing
//! but protocol on stdout (diagnostics go to stderr). Read-only on the
//! SQLite index by construction — the server never writes anything.
//!
//! Scoping: `ken-mcp --project <path>` locks every tool to that project;
//! unscoped, the project tools take a required `project` argument matched
//! against Ken's registry.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use uuid::Uuid;

use ken_core::db::Db;
use ken_core::family::{self, FamilyManifest, InboxKind, InboxTaskPayload, Lane, NewInboxItem};
use ken_core::family_sync::{self, ConnectionState, GitTransport, PendingWrite, SyncEngine, SystemGit};
use ken_core::features;
use ken_core::memory;
use ken_core::pipeline;
use ken_core::profiler::{self, ProjectProfile};
use ken_core::project::Project;
use ken_core::registry::{self, Registry, RegistryEntry};
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
    /// ken-pipeline (task 3.3): run ids **this server process** has itself
    /// flipped to `running` via `pipeline_claim`, mirroring src-tauri's
    /// `WorkspaceState::pipeline_known_running` (task 2.2) for a process
    /// that has no watcher of its own. Used only as `derive_queue`'s/
    /// `compose_digest`'s `known_running_ids` staleness input (D13): a
    /// `running` record this process didn't itself observe is reported
    /// stale, exactly the "no live run after restart" rule read literally
    /// for an MCP server that may be a fresh process per client connection.
    /// Interior mutability because every tool function takes `&Server`
    /// (unlike Tauri's `State<SharedState>`, ken-mcp's dispatch never
    /// hands out `&mut Server` to individual tools) — never persisted to
    /// disk, so this is purely an in-session hint, not a second ledger.
    known_running: RefCell<BTreeSet<String>>,
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
    let mut server = Server { base_dir, scoped, ..Default::default() };

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
        "tools/list" => rpc_result(&id, json!({ "tools": tool_definitions(server) })),
        "tools/call" => {
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match name {
                "search_knowledge" | "read_document" | "list_documents" | "list_projects"
                | "kg_search" | "semantic_search" | "route_query"
                | "memory_write" | "journal_append"
                | "task_create" | "task_list" | "task_update" | "task_complete"
                | "pipeline_list" | "pipeline_get" | "pipeline_claim" | "pipeline_advance"
                | "pipeline_block" | "pipeline_runs" | "pipeline_digest"
                | "family_list" | "family_inbox" | "family_send" => {
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
    let mut tools = vec![
        json!({
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
        }),
        json!({
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
            "description": "Hybrid keyword+meaning search of one Ken \
project's indexed documents, ranked the same way Ken's own hybrid search \
ranks it. Prefer this over route_query when you already know which project \
to search (e.g. from list_projects, kg_search, or because the user named \
it) — it searches only that project and is faster. This ken-mcp server has \
no embedding model of its own, so there is no fresh query embedding: \
results are keyword (FTS) matches reranked by the hybrid merge logic, not \
true meaning-based matches, regardless of the project's own index \
maturity. Hits carry ken:// addresses.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search terms." },
                    "limit": { "type": "integer", "description": "Maximum hits to return (default 20)." },
                    "project": project_arg
                },
                "required": ["query"]
            }
        }));
        tools.push(json!({
            "name": "route_query",
            "description": "One-shot workspace search: figures out which \
project(s) are relevant — a named project first, then the workspace \
knowledge graph, then broadcasting to every project with a ready semantic \
index — searches each with the same hybrid ranking semantic_search uses, \
and returns one merged, ranked, cited list. Prefer semantic_search instead \
when you already know the target project; use this when you don't. Every \
hit carries a ken:// address, plus kg:// breadcrumbs for the entities that \
picked the target when the knowledge graph did the routing.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search terms — also matched against project names for direct routing." },
                    "limit": { "type": "integer", "description": "Maximum merged hits to return (default 20)." }
                },
                "required": ["query"]
            }
        }));
    }

    // ken-memory (task 3.1) adds `memory_write`/`journal_append`, gated on
    // `kenMemory` the same way the kgRouting block above gates its three
    // tools — absent from the list entirely when the flag is off (spec.md
    // "Flag-scoped activation": "register no memory tools on either
    // surface").
    if ken_memory_enabled(&AppSettings::load(&server.base_dir)) {
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
workspace journal — this is where agent tasks report their findings back \
as they happen, creating the day's file if it doesn't exist yet. Cite \
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

    // ken-tasks (task 3.1) adds the four task-lifecycle tools, gated on
    // `kenTasks` the same way the kenMemory block above gates its two tools
    // — absent from the list entirely when the flag is off (spec.md
    // "Flag-scoped activation": "register no task tools on either
    // surface").
    if ken_tasks_enabled(&AppSettings::load(&server.base_dir)) {
        let patch_fields_schema = json!({
            "type": "object",
            "properties": {
                "title": { "type": "string" },
                "status": { "type": "string", "enum": ["backlog", "todo", "doing", "review", "done"] },
                "kind": { "type": "string", "enum": ["human", "ai"] },
                "assignee": { "type": "string", "description": "Who owns this task. Empty string clears it." },
                "project": { "type": "string", "description": "Ken project name this task concerns." },
                "tags": { "type": "array", "items": { "type": "string" } },
                "due": { "type": "string", "description": "Due date, e.g. YYYY-MM-DD." },
                "goal": { "type": "string", "description": "id of a goal (from tasks/goals/) this task tags." },
                "board": { "type": "string", "enum": ["main", "daily"] }
            }
        });
        tools.push(json!({
            "name": "task_create",
            "description": "Create a task on Ken's task board — a markdown \
file with frontmatter, filed in the workspace's shared tasks home by \
default. Status defaults to \"backlog\" (the intake column) and kind to \
\"human\" when not given in `fields`. Pass `home` as a registered Ken \
project name to file the task in that project's own `.ken/tasks/` home \
instead (created on first use — this IS the opt-in for a per-repo task \
home); omit it or pass \"workspace\" for the shared workspace home. Returns \
the new task's id and ken:// address.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "The task's title." },
                    "body": { "type": "string", "description": "Free markdown body — description, acceptance criteria, etc." },
                    "home": { "type": "string", "description": "\"workspace\" (default) or a registered Ken project name to file this task in that project's .ken/tasks/ home." },
                    "fields": patch_fields_schema.clone()
                },
                "required": ["title"]
            }
        }));
        tools.push(json!({
            "name": "task_list",
            "description": "List tasks from Ken's task board — scans the \
workspace tasks home, every registered project's `.ken/tasks/` home, and \
(when the kenFamilies flag is also on) every member's board in every \
family connection this device knows about — teammates' boards included, \
for visibility, though only your own board accepts writes — merged into \
one list. Filter by any combination of status, project, tag, assignee, \
kind, goal, and board. To find unclaimed work, set `assignee` to \"none\" \
(or \"unassigned\") — e.g. {\"kind\": \"ai\", \"assignee\": \"none\", \
\"status\": \"todo\"} lists unclaimed AI-kind tasks ready to start. Each \
result shows its id (use with task_update/task_complete) and ken:// \
address.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "filter": {
                        "type": "object",
                        "properties": {
                            "status": { "type": "string", "enum": ["backlog", "todo", "doing", "review", "done"] },
                            "project": { "type": "string" },
                            "tag": { "type": "string" },
                            "assignee": { "type": "string", "description": "An assignee name, or \"none\"/\"unassigned\" for unclaimed tasks." },
                            "kind": { "type": "string", "enum": ["human", "ai"] },
                            "goal": { "type": "string", "description": "id of a goal (from tasks/goals/)." },
                            "board": { "type": "string", "enum": ["main", "daily"] }
                        }
                    }
                }
            }
        }));
        tools.push(json!({
            "name": "task_update",
            "description": "Patch a task's frontmatter by id (from \
task_list) — rewrites only the keys given in `patch` plus `updated`; \
everything else in the file, including hand-added keys and the body, is \
left byte-for-byte untouched. This is also how an agent claims a task: \
first call task_list to confirm the task is unclaimed (empty `assignee`), \
then call task_update with `patch` set to {\"assignee\": \"<you>\", \
\"status\": \"doing\"} — set both in the same call. Searches the workspace \
tasks home, every registered project's task home, and every family board \
task_list can see. A family board task only accepts the write when it is \
your own board — Ken's write lanes refuse any patch aimed at a teammate's \
board, even though task_list shows it to you.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "The task's id, from task_list." },
                    "patch": patch_fields_schema,
                    "as": { "type": "string", "description": "Only needed when the task lives on a family board and this device's member identity for that family isn't already configured — your family member id (from family_list) to write as." }
                },
                "required": ["id"]
            }
        }));
        tools.push(json!({
            "name": "task_complete",
            "description": "Finish a task by id (from task_list): sets \
`status` to \"done\", bumps `updated`, and appends `report` under a \
\"## Log\" heading in the task file with a timestamp — this is how \
agent-desktop reports findings back after doing the work. When the \
kenMemory feature is also on, this additionally writes a one-line summary \
of the report to today's workspace journal, citing the task's ken:// \
address, so the completion shows up in Ken's day-to-day record without a \
separate journal_append call. Searches the workspace tasks home, every \
registered project's task home, and every family board task_list can see \
— same own-board-only write restriction as task_update, and a completed \
family task is pushed to the family remote so teammates see it on their \
next sync.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "The task's id, from task_list." },
                    "report": { "type": "string", "description": "Findings/summary of the completed work — appended to the task's ## Log." },
                    "as": { "type": "string", "description": "Only needed when the task lives on a family board and this device's member identity for that family isn't already configured — your family member id (from family_list) to write as." }
                },
                "required": ["id", "report"]
            }
        }));
    }

    // ken-pipeline (task 3.1-3.8): seven tools over the pipeline board —
    // list/get/claim/advance/block/runs/digest — gated on `kenPipeline` the
    // same way the kenTasks block above gates its four (spec.md "Flag-
    // scoped activation": "register no pipeline tools on either surface").
    // `ken_pipeline_enabled` already AND-s in `ken_tasks_enabled`, which
    // itself AND-s in `workspace_enabled`, so this one check transitively
    // requires all three flags.
    //
    // Pipeline tickets live in the workspace tasks home only in v1 (D15) —
    // these tools scan `.ken-workspace/tasks/` and `.ken-workspace/runs/`,
    // never a project's or family's own task home, unlike the classic
    // `task_*` tools above.
    if ken_pipeline_enabled(&AppSettings::load(&server.base_dir)) {
        let blocked_filter_schema = json!({
            "type": "string",
            "enum": ["any", "blocked", "notBlocked", "newlyUnblocked"],
            "description": "\"blocked\": carries blocked_by and/or a block_reason (this is \"what is stuck and why\"). \"newlyUnblocked\": every blocker cleared but the ticket hasn't been moved back to its return lane yet — check this after finishing a ticket to see what freed up. \"notBlocked\"/\"any\" are the complement / no-op."
        });
        tools.push(json!({
            "name": "pipeline_list",
            "description": "THE CLAIM PROTOCOL, STEP 1 — discover work: page \
through Ken's pipeline board (the ticket backlog can be large; this is the \
only way an agent reads it — never all at once). Returns compact rows only \
(id, title, lane, project symbol, model, assignee, block summary, updated) \
— never a ticket's body, scope, or verify command; call pipeline_get with \
an id for those. Default page size is 20, hard maximum 100; when more rows \
match, the reply's last line is `cursor: <token>` — pass that back as \
`cursor` to continue, and an empty/absent cursor on the reply means you've \
reached the end. `filter.blocked: \"blocked\"` is the one call that answers \
\"what is stuck and why\" — each row in that view names the ticket's \
blockers, its reason, its return lane, and how long it has been blocked. \
Note this tool does NOT tell you which tickets are ready for an agent to \
claim right now — a ticket only becomes claimable once a human has \
approved a run for it in the Ken app (kickoff), which files a `queued` run \
record. Call pipeline_runs({\"state\": \"queued\"}) to see those, or \
pipeline_digest for the daily-update view — then pipeline_claim(id, agent) \
on the ticket named by a queued run's `ticket` field.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "filter": {
                        "type": "object",
                        "properties": {
                            "lane": { "type": "string", "description": "A lane id from the ticket's pipeline definition (e.g. \"tester\", \"blocked\")." },
                            "pipeline": { "type": "string", "description": "A pipeline definition id (e.g. \"default\")." },
                            "project": { "type": "string" },
                            "model": { "type": "string", "description": "The ticket's own model override, or the lane's default model when the ticket has none." },
                            "assignee": { "type": "string" },
                            "blocked": blocked_filter_schema,
                            "blockedBy": { "type": "string", "description": "A ticket id — return only tickets whose blocked_by lists it (a dependency search, e.g. \"what does finishing ticket X unblock\")." }
                        }
                    },
                    "limit": { "type": "integer", "description": "Max rows to return (default 20, hard max 100)." },
                    "cursor": { "type": "string", "description": "Opaque continuation token from a previous pipeline_list reply's `cursor:` line. Do not construct one by hand." }
                }
            }
        }));
        tools.push(json!({
            "name": "pipeline_get",
            "description": "The one pipeline tool that returns a full \
ticket: its body, `scope` (the path globs that are the agent's file \
boundary — D3: do not write outside them), `verify` (the command that \
proves the lane's work; Ken never runs it, the claiming agent does and \
reports the result via pipeline_advance), `bounces`, its blockers and \
return lane, and a tail of its `## Log`. Call this right after \
pipeline_claim, before doing any work, to read `scope`/`verify` for the \
ticket you just claimed.",
            "inputSchema": {
                "type": "object",
                "properties": { "id": { "type": "string", "description": "The ticket's id, from pipeline_list or pipeline_runs." } },
                "required": ["id"]
            }
        }));
        tools.push(json!({
            "name": "pipeline_claim",
            "description": "THE CLAIM PROTOCOL, STEP 2 — claim a ticket a \
human has already approved a run for (see pipeline_list's description for \
how to find one: pipeline_runs({\"state\": \"queued\"}) or \
pipeline_digest). This call is not optional ceremony: it re-runs Ken's own \
admission check server-side and, on success, writes a `running` run record \
— that record IS what makes your work visible to Ken and to its stale-run \
detector. An agent that edits files without calling pipeline_claim first is \
invisible to Ken; its work will not be found, and the ticket will look \
untouched. Refused when the ticket is blocked (no exceptions — an agent \
cannot claim around a blocked ticket, ever), when the workspace's \
concurrency cap is already full (the run stays queued; retry later), or \
when the ticket still needs a human confirmation Ken hasn't gotten yet. On \
success the reply names the ticket's `scope` and `verify` — call \
pipeline_get for the full brief before writing anything. When you finish, \
you MUST call pipeline_advance with the outcome; a claimed ticket with no \
pipeline_advance call is a run Ken will eventually report as stale.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "The ticket's id, from pipeline_list or pipeline_runs." },
                    "agent": { "type": "string", "description": "Your agent identity — recorded on the run so a human can see who is doing the work." },
                    "model": { "type": "string", "description": "Which model is doing the work. Defaults to the ticket's own model override, then the lane's default model." }
                },
                "required": ["id", "agent"]
            }
        }));
        tools.push(json!({
            "name": "pipeline_advance",
            "description": "THE CLAIM PROTOCOL, STEP 3 — report back after \
doing the work inside a claimed ticket's `scope` and running its `verify` \
command yourself (Ken never runs `verify`; it only records what you \
report). `outcome: \"pass\"` moves the ticket to its lane's `on_pass` \
target; `outcome: \"fail\"` moves it to `on_fail` and counts as a bounce — \
enough consecutive bounces trip the pipeline's retry cap, which blocks the \
ticket for a human rather than looping forever. `report` is appended to \
the ticket's ## Log and becomes the closed run's report. This closes \
whichever run you opened with pipeline_claim (`running` → `pass`/`fail`, \
or `blocked` if the retry cap tripped) — it is the other half of the claim \
protocol; a claim with no matching advance call leaves your run stuck at \
`running` until Ken reports it stale.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "The ticket's id you claimed with pipeline_claim." },
                    "outcome": { "type": "string", "enum": ["pass", "fail"] },
                    "report": { "type": "string", "description": "What you did / found — appended to the ticket's ## Log and recorded as the run's report." },
                    "artifacts": { "type": "array", "items": { "type": "string" }, "description": "Optional: filenames written under .ken-workspace/artifacts/<ticket-id>/ for this run (D9 throwaway review material — never a member repo)." }
                },
                "required": ["id", "outcome", "report"]
            }
        }));
        tools.push(json!({
            "name": "pipeline_block",
            "description": "Set or clear a ticket's block. `blockedBy` \
takes ticket ids (ULIDs) ONLY, never file paths — a dependency must survive \
the blocking ticket being renamed or moved, which a path cannot. Setting a \
block removes the ticket from every agent's claimable queue immediately \
(pipeline_claim on a blocked ticket always fails, with no exception for an \
`auto` lane) and records `return_lane` — the lane the ticket will resume \
in once unblocked — automatically; you cannot set `return_lane` yourself. \
A dependency that would close a cycle (directly, or through a longer \
chain) is refused before anything is written, with the full cycle path in \
the error. To clear a block, pass `clear`: `{\"deps\": true}` and/or \
`{\"reason\": true}` clear independently — a ticket blocked by both a \
dependency and a stated reason stays blocked until both are cleared. \
Clearing never restarts an agent by itself: the ticket returns to its \
lane awaiting a human confirmation, exactly like a fresh kickoff.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "The ticket's id." },
                    "blockedBy": { "type": "array", "items": { "type": "string" }, "description": "Ticket ids (ULIDs) to add as dependencies. Never file paths." },
                    "reason": { "type": "string", "description": "Free-text reason to set (or replace) — everything not captured by a ticket dependency." },
                    "clear": {
                        "type": "object",
                        "description": "Present only to unblock. Independent flags: a dependency-blocked ticket keeps its bounce history when cleared; a retry-cap block resets its bounce counter.",
                        "properties": {
                            "deps": { "type": "boolean", "description": "Clear every blocked_by dependency." },
                            "reason": { "type": "boolean", "description": "Clear the free-text block_reason." }
                        }
                    }
                },
                "required": ["id"]
            }
        }));
        tools.push(json!({
            "name": "pipeline_runs",
            "description": "The watch surface over Ken's run ledger — the \
append-only history pipeline_claim/pipeline_advance write to. Returns \
`running` (currently claimed and being worked), `queued` (approved by a \
human, waiting to be claimed — this is your claimable-work list), \
`blocked` (runs closed because the ticket's retry cap tripped), \
`waitingHuman` (tickets sitting at a confirmation gate — nobody has \
approved a run yet, so there is nothing to claim), and `stale` (a `running` \
record this Ken session has no live memory of — e.g. Ken restarted mid-run \
— never treated as passed). Pass `state` to see only one group.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "state": { "type": "string", "enum": ["running", "queued", "blocked", "waitingHuman", "stale", "all"], "description": "Default \"all\"." }
                }
            }
        }));
        tools.push(json!({
            "name": "pipeline_digest",
            "description": "The daily update, grouped in a fixed order: \
tickets awaiting human review first, then tickets newly unblocked \
overnight (each with its return lane — the ticket is waiting at a \
confirmation, nothing started by itself), then blocked tickets oldest \
first (each with its root blocker, reason, and how long it's been stuck), \
then tickets that moved today, new ideas filed by the documentation lane, \
and stale runs. When Ken's memory feature is on, calling this also \
appends the digest to today's workspace journal.",
            "inputSchema": {
                "type": "object",
                "properties": { "day": { "type": "string", "description": "YYYY-MM-DD; defaults to today. Changes what counts as \"today\" for the moved/new-ideas groups — the board itself is always current." } }
            }
        }));
    }

    // ken-families (tasks 3.1-3.4): three new tools, gated on `kenFamilies`
    // the same way the blocks above gate theirs — absent from the list
    // entirely when the flag is off. `task_list`/`task_update`/
    // `task_complete` above stay listed regardless of this flag (they are
    // ken-tasks tools first); their *content* only grows family boards when
    // `kenFamilies` is also on, mirroring how `list_projects` above enriches
    // under `kgRouting` without moving behind it.
    if ken_families_enabled(&AppSettings::load(&server.base_dir)) {
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
already owns). IMPORTANT: delivery is not assignment or acceptance. \
Nothing sent by this tool enters the recipient's board, daily board, or \
any agent-claimable queue by itself — for a task item, the recipient (or \
their Ken) must explicitly accept it first; for a message or notification, \
it just waits, unread, until they read or dismiss it. There is no \
auto-accept in this system.",
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

    Value::Array(tools)
}

// --- tools ---

fn call_tool(server: &Server, name: &str, args: &Value) -> Result<String, String> {
    match name {
        "list_projects" => list_projects(server),
        "kg_search" => kg_search(server, args),
        "semantic_search" => semantic_search(server, args),
        "route_query" => route_query(server, args),
        "memory_write" => memory_write_tool(server, args),
        "journal_append" => journal_append_tool(server, args),
        "task_create" => task_create_tool(server, args),
        "task_list" => task_list_tool(server, args),
        "task_update" => task_update_tool(server, args),
        "task_complete" => task_complete_tool(server, args),
        "pipeline_list" => pipeline_list_tool(server, args),
        "pipeline_get" => pipeline_get_tool(server, args),
        "pipeline_claim" => pipeline_claim_tool(server, args),
        "pipeline_advance" => pipeline_advance_tool(server, args),
        "pipeline_block" => pipeline_block_tool(server, args),
        "pipeline_runs" => pipeline_runs_tool(server, args),
        "pipeline_digest" => pipeline_digest_tool(server, args),
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
                Ok(db) if db.vec_available() => "keyword + semantic search ready",
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
    let kg = WorkspaceKgDb::open(&server.base_dir)
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

/// Single-member hybrid search (task 3.3), delegating to
/// `routing::search_member` — the same FTS+KNN+merge_and_rerank composition
/// src-tauri's `hybrid_search` command uses, generalized for reuse
/// (routing.rs module doc). `query_vec` is unconditionally `None`: this
/// build has no embedder (`ken-core = { default-features = false }` in
/// Cargo.toml — ken-mcp is the FTS-only sidecar), and substituting
/// `FakeEmbedder` is the one thing that type's own doc says never to do (it
/// would silently return meaningless hash-based vectors against a real
/// semantic index). `search_member` degrades exactly as designed: keyword
/// (FTS) hits, reranked by hybrid search's own merge logic — honestly
/// short of true semantic search, whatever the project's own index
/// maturity is.
fn semantic_search(server: &Server, args: &Value) -> Result<String, String> {
    let query = require_str(args, "query")?;
    let limit = args
        .get("limit")
        .and_then(|l| l.as_u64())
        .map(|l| l.clamp(1, 200) as usize)
        .unwrap_or(20);
    if !kg_routing_enabled(&AppSettings::load(&server.base_dir)) {
        return Err("semantic_search requires the kgRouting feature flag, \
which is off. Use search_knowledge instead."
            .into());
    }
    let (project, note) = resolve_project(server, args)?;
    let db = open_index(server, &project)?;
    let hits = routing::search_member(&db, &query, None, limit).map_err(|e| format!("search failed: {e}"))?;
    let mut out = note.unwrap_or_default();
    if hits.is_empty() {
        out.push_str(&format!(
            "No matches for {query:?} in project \"{}\".",
            project.config.name
        ));
    } else {
        out.push_str(&format!(
            "{} result{} for {query:?} in project \"{}\" (keyword-ranked — \
this server has no query embedding model; see the tool description):\n",
            hits.len(),
            if hits.len() == 1 { "" } else { "s" },
            project.config.name
        ));
        for (i, hit) in hits.iter().enumerate() {
            out.push_str(&format!(
                "\n{}. {} [{}] {} — {}",
                i + 1,
                ken_address(project.config.id, &hit.path),
                source_label(hit.source),
                hit.path,
                hit.snippet
            ));
        }
    }
    Ok(out)
}

/// Route + fan-out + merge (task 3.4): `routing::plan_route`, then a
/// per-target `routing::search_member`, then `routing::merge_routed`.
///
/// This does NOT call `routing::execute_plan` even though that's the
/// ken-core composition of exactly these three steps — `execute_plan`
/// requires a live `&mut dyn Embedder` to call `.embed_query()`, and (per
/// `semantic_search`'s doc above) this build has none to give it; faking
/// one with `FakeEmbedder` is the one thing that module's doc says not to
/// do. So this is `execute_plan`'s own per-target loop, copied by hand with
/// `query_vec` unconditionally `None` — the "honest adaptation" its module
/// doc explicitly anticipates for a caller in ken-mcp's position, ending in
/// the same pure `merge_routed` call `execute_plan` itself makes.
fn route_query(server: &Server, args: &Value) -> Result<String, String> {
    let query = require_str(args, "query")?;
    let limit = args
        .get("limit")
        .and_then(|l| l.as_u64())
        .map(|l| l.clamp(1, 200) as usize)
        .unwrap_or(20);
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
    for entry in &registry.projects {
        if !entry.path.is_dir() {
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
        });
    }

    let kg = federated_kg_enabled(&app_settings)
        .then(|| WorkspaceKgDb::open(&server.base_dir).ok())
        .flatten();
    let plan = routing::plan_route(&query, &members, kg.as_ref());

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
        match routing::search_member(db, &query, None, limit) {
            Ok(hits) => member_hits.push(MemberHits {
                project_id,
                member_name: info.name.clone(),
                status: MemberStatus::Searched,
                hits,
            }),
            Err(_) => member_hits.push(MemberHits {
                project_id,
                member_name: info.name.clone(),
                status: MemberStatus::Unavailable,
                hits: Vec::new(),
            }),
        }
    }

    let report = routing::merge_routed(&plan, &member_hits, limit);
    Ok(format_execution_report(&report))
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
        out.push_str("\nNo results.");
        return out;
    }
    for (i, hit) in report.results.iter().enumerate() {
        out.push_str(&format!(
            "\n{}. {} [{}, {}] — {}",
            i + 1,
            hit.address,
            hit.member_name,
            source_label(hit.source),
            hit.snippet
        ));
        if !hit.kg_breadcrumbs.is_empty() {
            out.push_str(&format!(" ({})", hit.kg_breadcrumbs.join(" ")));
        }
    }
    out
}

/// `memory_write` (task 3.1): create or replace a Ken long-term memory file
/// via the same `ken_core::memory::write_memory` core the Ken chat tool of
/// the same name is specified to use (design D5: "one write core, two tool
/// surfaces"). Defense-in-depth: `tool_definitions` already hides this tool
/// when `kenMemory` is off, but the flag is re-checked here too — same
/// posture as `kg_search`/`semantic_search`/`route_query` above — since an
/// MCP client can call any tool name whether or not `tools/list` advertised
/// it.
fn memory_write_tool(server: &Server, args: &Value) -> Result<String, String> {
    if !ken_memory_enabled(&AppSettings::load(&server.base_dir)) {
        return Err("memory_write requires the kenMemory feature flag, which is off.".into());
    }
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
    let (today, _) = today_and_time_utc();

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
/// back"). Same defense-in-depth flag re-check as `memory_write_tool`.
fn journal_append_tool(server: &Server, args: &Value) -> Result<String, String> {
    if !ken_memory_enabled(&AppSettings::load(&server.base_dir)) {
        return Err("journal_append requires the kenMemory feature flag, which is off.".into());
    }
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
    let (today, time_hhmm) = today_and_time_utc();
    let path = memory::append_journal(&workspace_root, &text, project, &tags, &today, &time_hhmm)
        .map_err(|e| format!("could not append to the journal: {e}"))?;
    Ok(format!(
        "Appended a {today} {time_hhmm} entry to the workspace journal ({}).",
        path.display()
    ))
}

// --- ken-tasks tools (task 3.1) ---
//
// The four tools share `ken_core::tasks`'s pure core with the (future) UI
// and chat tools (design D4: "no new protocol: the four tools are the
// lifecycle"). Every call re-checks `ken_tasks_enabled` — defense-in-depth,
// same posture as the memory tools above, since an MCP client can invoke
// any tool name whether or not `tools/list` advertised it.

/// One member's board within one family connection, as an extra task home
/// (task 3.4). Every manifest member's board is included, not just this
/// device's own, so `task_list` shows the whole team's board for
/// visibility — `family_authorize_write` is what actually restricts writes
/// to `my_member_id`'s own board via `family::lane_check`.
#[derive(Debug, Clone)]
struct FamilyBoardHome {
    family_id: Uuid,
    family_name: String,
    clone_root: PathBuf,
    /// Whose board this home scans.
    member_id: String,
    /// This device's own member id in the family, if known — the only
    /// identity a write to this home may lane-check as `member_id`.
    my_member_id: Option<String>,
    /// `family::board_dir(&clone_root, &member_id)`, resolved once at
    /// construction (`resolve_task_homes`) rather than recomputed per call —
    /// `tasks::TaskHome::Family` borrows this field, so it needs to outlive
    /// the borrow the same way `member_infos`/`family_infos` do in
    /// src-tauri's `task_homes_scan` ("collect first, borrow after").
    board_dir: PathBuf,
}

/// Owned scan-home data for one call: the open workspace's tasks home,
/// every registered project's `.ken/tasks/` home (task brief: "Scan across
/// the workspace home AND every registered project's .ken/tasks/ home for
/// list/update/complete"), and — task 3.4 — every member's board in every
/// family connection this device knows about, when `kenFamilies` is on.
/// `tasks::TaskHome` borrows its paths/names, so this owns them once and
/// hands out borrowed `TaskHome`s from one place — homes with no `tasks/`
/// folder yet simply contribute no tasks (`list_tasks`'s "missing folder
/// reads as no tasks").
///
/// Family boards ARE now representable as a `tasks::TaskHome` —
/// `TaskHome::Family { board_dir, family_name }` (ken-tasks debt follow-up).
/// `family_boards` still owns the resolved `(clone_root, member_id, …)` data
/// (a `TaskHome::Family` only borrows `board_dir`/`family_name`, and this
/// struct needs to keep the rest — `family_id`, `my_member_id` — around for
/// `host_for`/`family_authorize_write`), but scanning itself is now a single
/// `tasks::scan_tasks` call over every home, workspace/project/family alike
/// (mirrors src-tauri's `task_homes_scan`).
struct TaskHomes {
    workspace_root: PathBuf,
    projects: Vec<RegistryEntry>,
    family_boards: Vec<FamilyBoardHome>,
}

impl TaskHomes {
    fn homes(&self) -> Vec<tasks::TaskHome<'_>> {
        let mut out = vec![tasks::TaskHome::Workspace { workspace_root: &self.workspace_root }];
        for p in &self.projects {
            out.push(tasks::TaskHome::Project { project_root: &p.path, project: &p.name });
        }
        for fb in &self.family_boards {
            out.push(tasks::TaskHome::Family { board_dir: &fb.board_dir, family_name: &fb.family_name });
        }
        out
    }

    /// Every task across every home in one `tasks::scan_tasks` pass —
    /// workspace, then registered projects, then family boards, the same
    /// order (and therefore the same dedupe-by-id winner) `homes()` builds
    /// them in.
    fn scan_all(&self) -> Result<Vec<tasks::Task>, String> {
        tasks::scan_tasks(&self.homes()).map_err(|e| e.to_string())
    }

    /// Which family board a task found via `scan_all` came from, if any —
    /// matched by `home_dir` (`tasks::parse_task` sets it from the file's
    /// own parent, so it is exact regardless of which `HomeKind` the task
    /// was tagged with).
    fn family_origin(&self, task: &tasks::Task) -> Option<&FamilyBoardHome> {
        self.family_boards.iter().find(|fb| fb.board_dir == task.home_dir)
    }

    /// The `ken://` host for a task found via `scan_all`: the workspace
    /// pseudo-host for workspace-home tasks, the owning project's registry
    /// id for a per-repo task, or the family id (D6: `ken://<family-id>/…`)
    /// for a family board task — `tasks::Task::address` takes this as an
    /// argument because `tasks.rs` has no `Project`/family *connection*
    /// handle of its own (it only knows paths, not which family a board
    /// belongs to) — mirrors `journal_summary_line`'s doc comment on the
    /// same point.
    fn host_for(&self, task: &tasks::Task) -> String {
        if let Some(fb) = self.family_origin(task) {
            return fb.family_id.to_string();
        }
        if task.home == tasks::HomeKind::Workspace {
            return memory::WORKSPACE_ADDRESS_ID.to_string();
        }
        for p in &self.projects {
            if tasks::project_tasks_dir(&p.path) == task.home_dir {
                return p.id.to_string();
            }
        }
        // Shouldn't happen (every project-home task came from a home built
        // out of `self.projects`), but degrade to the task's own `project`
        // frontmatter value rather than panicking.
        task.project.clone()
    }

    /// `ken://` address for a task found via `scan_all`. `Task::address`
    /// (via `Task::address_rel_path`) now derives a family task's
    /// `members/<id>/board/<file>` tail natively — `HomeKind::Family`'s
    /// member id is recovered structurally from `home_dir`'s parent, which
    /// is always exactly `family::board_dir(clone_root, member_id)` because
    /// that's the literal path `TaskHome::Family { board_dir, .. }` above was
    /// built from — so this no longer needs its own override.
    fn address_for(&self, task: &tasks::Task) -> String {
        task.address(&self.host_for(task))
    }
}

fn resolve_task_homes(server: &Server) -> Result<TaskHomes, String> {
    let workspace_root = resolve_workspace_root(server)?;
    let registry = Registry::load(&server.base_dir)
        .map_err(|e| format!("could not read Ken's project registry: {e}"))?;
    let mut family_boards = Vec::new();
    if ken_families_enabled(&AppSettings::load(&server.base_dir)) {
        for conn in discover_family_connections(server) {
            if conn.manifest.check_supported().is_err() {
                continue; // "needs a newer Ken": no sync, ingest, or write (spec)
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
    }
    Ok(TaskHomes { workspace_root, projects: registry.projects, family_boards })
}

/// Shared by `task_create`'s `fields` and `task_update`'s `patch` — both
/// are the same `TaskPatch` shape on the wire (tool schemas' `patch_fields_schema`).
fn parse_task_patch(value: &Value) -> Result<tasks::TaskPatch, String> {
    let obj = value.as_object();
    let get_str = |key: &str| -> Option<String> {
        obj.and_then(|o| o.get(key)).and_then(Value::as_str).map(str::to_string)
    };
    let status = match get_str("status") {
        None => None,
        Some(s) => Some(tasks::TaskStatus::parse(&s).ok_or_else(|| {
            format!("invalid \"status\" {s:?} — use backlog, todo, doing, review, or done")
        })?),
    };
    let kind = match get_str("kind") {
        None => None,
        Some(s) => {
            Some(tasks::TaskKind::parse(&s).ok_or_else(|| format!("invalid \"kind\" {s:?} — use human or ai"))?)
        }
    };
    let board = match get_str("board") {
        None => None,
        Some(s) => {
            Some(tasks::BoardKind::parse(&s).ok_or_else(|| format!("invalid \"board\" {s:?} — use main or daily"))?)
        }
    };
    let tags = obj.and_then(|o| o.get("tags")).and_then(Value::as_array).map(|arr| {
        arr.iter().filter_map(|v| v.as_str()).map(str::to_string).collect::<Vec<_>>()
    });
    Ok(tasks::TaskPatch {
        title: get_str("title"),
        status,
        kind,
        assignee: get_str("assignee"),
        project: get_str("project"),
        tags,
        due: get_str("due"),
        goal: get_str("goal"),
        board,
        // The pipeline fields (lane, blocked_by, scope, …) are not part of
        // this tool's schema: an external agent claims and completes work,
        // it does not move tickets between lanes or block them.
        ..Default::default()
    })
}

fn parse_task_filter(value: &Value) -> Result<tasks::TaskFilter, String> {
    let obj = value.as_object();
    let get_str = |key: &str| -> Option<String> {
        obj.and_then(|o| o.get(key))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let status = match get_str("status") {
        None => None,
        Some(s) => Some(tasks::TaskStatus::parse(&s).ok_or_else(|| {
            format!("invalid \"status\" {s:?} in filter — use backlog, todo, doing, review, or done")
        })?),
    };
    let kind = match get_str("kind") {
        None => None,
        Some(s) => Some(
            tasks::TaskKind::parse(&s).ok_or_else(|| format!("invalid \"kind\" {s:?} in filter — use human or ai"))?,
        ),
    };
    let board = match get_str("board") {
        None => None,
        Some(s) => Some(
            tasks::BoardKind::parse(&s)
                .ok_or_else(|| format!("invalid \"board\" {s:?} in filter — use main or daily"))?,
        ),
    };
    let assignee = get_str("assignee").map(|s| tasks::AssigneeFilter::parse(&s));
    Ok(tasks::TaskFilter {
        status,
        project: get_str("project"),
        tag: get_str("tag"),
        assignee,
        kind,
        goal: get_str("goal"),
        board,
        // Lane/pipeline/blocked filtering is added by ken-pipeline's own
        // tools (section 3 of that change), not retrofitted onto task_list.
        ..Default::default()
    })
}

/// `task_create` (task 3.1): file a new task on the board, in the
/// workspace's shared home by default or a registered project's own
/// `.ken/tasks/` home when `home` names one (D2: "folder existence is the
/// opt-in" — `tasks::create_task` creates it on first write).
fn task_create_tool(server: &Server, args: &Value) -> Result<String, String> {
    if !ken_tasks_enabled(&AppSettings::load(&server.base_dir)) {
        return Err("task_create requires the kenTasks feature flag, which is off.".into());
    }
    let title = require_str(args, "title")?;
    let body = args.get("body").and_then(Value::as_str).unwrap_or("").to_string();
    let fields = match args.get("fields") {
        Some(v) => parse_task_patch(v)?,
        None => tasks::TaskPatch::default(),
    };
    let home_arg = args
        .get("home")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let homes = resolve_task_homes(server)?;
    let (today, _) = today_and_time_utc();
    let new = tasks::NewTask { id: None, title, body, fields };

    let task = match home_arg {
        None => {
            let home = tasks::TaskHome::Workspace { workspace_root: &homes.workspace_root };
            tasks::create_task(home, &new, &today).map_err(|e| e.to_string())?
        }
        Some(h) if h.eq_ignore_ascii_case("workspace") => {
            let home = tasks::TaskHome::Workspace { workspace_root: &homes.workspace_root };
            tasks::create_task(home, &new, &today).map_err(|e| e.to_string())?
        }
        Some(h) => {
            let entry = homes.projects.iter().find(|p| p.name.eq_ignore_ascii_case(h)).ok_or_else(|| {
                let names: Vec<String> = homes.projects.iter().map(|p| p.name.clone()).collect();
                let available = if names.is_empty() {
                    "No projects are registered yet.".to_string()
                } else {
                    format!("Available projects: {}.", names.join(", "))
                };
                format!("\"{h}\" is neither \"workspace\" nor a registered project name. {available}")
            })?;
            let home = tasks::TaskHome::Project { project_root: &entry.path, project: &entry.name };
            tasks::create_task(home, &new, &today).map_err(|e| e.to_string())?
        }
    };

    let host = homes.host_for(&task);
    Ok(format!(
        "Created task \"{}\" (id {}, status {}, kind {}) in the {} tasks home ({}).",
        task.title,
        task.id,
        task.status.map(|s| s.as_str().to_string()).unwrap_or(task.status_raw.clone()),
        task.kind.as_str(),
        match task.home {
            tasks::HomeKind::Workspace => "workspace",
            tasks::HomeKind::Project => "project",
            tasks::HomeKind::Family => "family",
        },
        task.address(&host)
    ))
}

/// `task_list` (task 3.1): the board's read side — every home, one merged,
/// filtered list. `filter` matches the UI filter shape 1:1 (D4).
fn task_list_tool(server: &Server, args: &Value) -> Result<String, String> {
    if !ken_tasks_enabled(&AppSettings::load(&server.base_dir)) {
        return Err("task_list requires the kenTasks feature flag, which is off.".into());
    }
    let filter = match args.get("filter") {
        Some(v) => parse_task_filter(v)?,
        None => tasks::TaskFilter::default(),
    };
    let homes = resolve_task_homes(server)?;
    let all = homes.scan_all()?;
    let hits = tasks::filter_tasks(&all, &filter);
    if hits.is_empty() {
        return Ok("No tasks match that filter.".to_string());
    }
    let mut out = format!("{} task{} match:\n", hits.len(), if hits.len() == 1 { "" } else { "s" });
    for t in hits {
        let family_tag = homes
            .family_origin(t)
            .map(|fb| format!(" [family:{}/{}]", fb.family_name, fb.member_id))
            .unwrap_or_default();
        out.push_str(&format!(
            "\n{} — \"{}\" [{}, {}] assignee={} project={} tags=[{}]{}{}{} — {}",
            t.id,
            t.title,
            t.status.map(|s| s.as_str().to_string()).unwrap_or_else(|| format!("invalid:{}", t.status_raw)),
            t.kind.as_str(),
            if t.assignee.is_empty() { "none" } else { &t.assignee },
            if t.project.is_empty() { "-" } else { &t.project },
            t.tags.join(", "),
            t.goal.as_deref().map(|g| format!(" goal={g}")).unwrap_or_default(),
            if t.board == tasks::BoardKind::Daily { " [daily]" } else { "" },
            family_tag,
            homes.address_for(t)
        ));
    }
    Ok(out)
}

/// `task_update` (task 3.1): patch by id, searching every home. This is the
/// claim path (D4): call with `patch: {"assignee": "<you>", "status":
/// "doing"}` after confirming via `task_list` that `assignee` is empty.
///
/// Task 3.4: when the found task lives on a family board,
/// `family_authorize_write` lane-checks the write *before* anything touches
/// disk — a patch aimed at a teammate's board is refused by
/// `family::lane_check` itself, not by a convention this function could
/// forget to enforce. A successful family write is then committed and
/// pushed to the family remote (best-effort — see `push_family_board_write`).
fn task_update_tool(server: &Server, args: &Value) -> Result<String, String> {
    if !ken_tasks_enabled(&AppSettings::load(&server.base_dir)) {
        return Err("task_update requires the kenTasks feature flag, which is off.".into());
    }
    let id = require_str(args, "id")?;
    let patch = match args.get("patch") {
        Some(v) => parse_task_patch(v)?,
        None => tasks::TaskPatch::default(),
    };
    let as_arg = opt_str(args, "as");
    let homes = resolve_task_homes(server)?;
    let all = homes.scan_all()?;
    let target = tasks::find_by_id(&all, &id).ok_or_else(|| {
        format!(
            "no task with id {id:?} found in the workspace tasks home, any registered \
project's task home, or any attached family board"
        )
    })?;
    let path = target.path.clone();
    let file_name = target.file_name();
    let family_ctx = homes.family_origin(target).cloned();

    let (today, _) = today_and_time_utc();
    let mut sync_note = String::new();
    if let Some(fb) = &family_ctx {
        let (identity, rel_path) = family_authorize_write(fb, &file_name, as_arg.as_deref())?;
        tasks::apply_patch(&path, &patch, &today).map_err(|e| e.to_string())?;
        sync_note = push_family_board_write(fb, &identity, &rel_path, &path);
    } else {
        tasks::apply_patch(&path, &patch, &today).map_err(|e| e.to_string())?;
    }

    let all_after = homes.scan_all()?;
    let updated = tasks::find_by_id(&all_after, &id)
        .ok_or_else(|| "task was updated but could not be re-read".to_string())?;
    Ok(format!(
        "Updated task \"{}\" (id {}) — status={}, assignee={} ({}).{sync_note}",
        updated.title,
        id,
        updated.status.map(|s| s.as_str().to_string()).unwrap_or_else(|| format!("invalid:{}", updated.status_raw)),
        if updated.assignee.is_empty() { "none".to_string() } else { updated.assignee.clone() },
        homes.address_for(updated)
    ))
}

/// `task_complete` (task 3.1): the complete→journal flow (D4). Sets `done`,
/// appends `report` under the task's `## Log` heading with a timestamp, and
/// — when `kenMemory` is on — writes a one-line journal summary citing the
/// task's `ken://` address via the same `journal_summary_line`/
/// `memory::append_journal` core the ken-memory tools use. A journal-write
/// failure is reported as a warning rather than failing the whole call: the
/// task is already marked done and logged by that point, so surfacing it as
/// a hard error would wrongly suggest the completion itself didn't happen.
fn task_complete_tool(server: &Server, args: &Value) -> Result<String, String> {
    if !ken_tasks_enabled(&AppSettings::load(&server.base_dir)) {
        return Err("task_complete requires the kenTasks feature flag, which is off.".into());
    }
    let id = require_str(args, "id")?;
    let report = require_str(args, "report")?;
    let as_arg = opt_str(args, "as");
    let homes = resolve_task_homes(server)?;
    let all = homes.scan_all()?;
    let task = tasks::find_by_id(&all, &id).ok_or_else(|| {
        format!(
            "no task with id {id:?} found in the workspace tasks home, any registered \
project's task home, or any attached family board"
        )
    })?;

    let (today, time_hhmm) = today_and_time_utc();
    // Task 3.4: same own-board-only enforcement as task_update — lane-check
    // before the write, not after.
    let family_ctx = homes.family_origin(task).cloned();
    let mut sync_note = String::new();
    if let Some(fb) = &family_ctx {
        let (identity, rel_path) = family_authorize_write(fb, &task.file_name(), as_arg.as_deref())?;
        tasks::complete_task(task, &report, &today, &time_hhmm).map_err(|e| e.to_string())?;
        sync_note = push_family_board_write(fb, &identity, &rel_path, &task.path);
    } else {
        tasks::complete_task(task, &report, &today, &time_hhmm).map_err(|e| e.to_string())?;
    }

    let address = homes.address_for(task);
    let mut msg = format!(
        "Completed task \"{}\" (id {}) — status done, report appended under ## Log ({}).",
        task.title, id, address
    );

    if ken_memory_enabled(&AppSettings::load(&server.base_dir)) {
        // `tasks::journal_summary_line` directly now — it calls
        // `Task::address` internally, which (via `Task::address_rel_path`)
        // now natively expresses a family board's `members/<id>/board/`
        // path, so there's no longer a need for a local override that takes
        // a precomputed address instead.
        let line = tasks::journal_summary_line(task, &homes.host_for(task), &report);
        let project_opt = if task.project.trim().is_empty() { None } else { Some(task.project.as_str()) };
        match memory::append_journal(&homes.workspace_root, &line, project_opt, &[], &today, &time_hhmm) {
            Ok(_) => msg.push_str(" Journal summary recorded."),
            Err(e) => msg.push_str(&format!(" (warning: could not write journal summary: {e})")),
        }
    }
    msg.push_str(&sync_note);
    Ok(msg)
}

// --- ken-pipeline tools (task 3.1-3.9) ---
//
// The seven tools below are **the runner** for design D14's v1 `mcp` pull
// model: Ken itself never spawns a process; an external agent discovers
// claimable work (pipeline_list / pipeline_runs / pipeline_digest), claims
// it (pipeline_claim — re-runs `pipeline::admit` server-side, which is the
// ONLY function allowed to decide whether a run may start; this layer
// never re-derives the blocked-refusal or cap logic), does the work inside
// the ticket's `scope`, and reports back (pipeline_advance — routes
// through `pipeline::advance`, which owns bounce accounting and the
// retry-cap-to-block escalation). `pipeline_block` is the third write
// path, routed through `pipeline::block`/`unblock` so cycle detection and
// `return_lane` capture cannot be bypassed (D5).
//
// D15: pipeline tickets live in the workspace tasks home only in v1, so —
// unlike the classic `task_*` tools' `TaskHomes` above — every tool here
// scans exactly one home: `.ken-workspace/tasks/`, plus
// `.ken-workspace/pipelines/` and `.ken-workspace/runs/`. Every call
// re-checks `ken_pipeline_enabled` (`require_pipeline_flag`) — defense in
// depth, since an MCP client can invoke any tool name whether or not
// `tools/list` advertised it.

/// `.ken-workspace/pipelines/*.md`, parsed. Reproduction of src-tauri's
/// own `pipelines_scan` (`lib.rs`) — duplicated rather than exported from
/// ken-core because `crates/**` is read-only for this session (task
/// brief); kept byte-identical in behaviour (sorted paths, tolerant parse,
/// missing directory reads as no pipelines).
fn pipelines_scan(ws_root: &Path) -> Vec<pipeline::Pipeline> {
    let dir = pipeline::pipelines_dir(ws_root);
    let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "md"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .filter_map(|p| std::fs::read_to_string(&p).ok().map(|raw| pipeline::parse_pipeline(&p, &raw)))
        .collect()
}

/// Every run record under `.ken-workspace/runs/YYYY-MM/*.md`, with its
/// path (needed to close a specific run — `RunRecord` carries no path,
/// only `id`). Mirrors src-tauri's `run_ledger_scan_with_paths`.
fn run_ledger_scan_with_paths(ws_root: &Path) -> Vec<(PathBuf, pipeline::RunRecord)> {
    let mut out = Vec::new();
    let runs_dir = pipeline::runs_dir(ws_root);
    let Ok(months) = std::fs::read_dir(&runs_dir) else { return out };
    for month in months.flatten() {
        let month_path = month.path();
        if !month_path.is_dir() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&month_path) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().is_some_and(|x| x == "md") {
                if let Ok(raw) = std::fs::read_to_string(&path) {
                    out.push((path.clone(), pipeline::parse_run(&path, &raw)));
                }
            }
        }
    }
    out
}

fn run_ledger_scan(ws_root: &Path) -> Vec<pipeline::RunRecord> {
    run_ledger_scan_with_paths(ws_root).into_iter().map(|(_, r)| r).collect()
}

/// The most recently started still-open (`queued`/`running`) run for a
/// ticket. Mirrors src-tauri's `find_open_run`.
fn find_open_run<'a>(runs: &'a [(PathBuf, pipeline::RunRecord)], ticket_id: &str) -> Option<&'a (PathBuf, pipeline::RunRecord)> {
    runs.iter()
        .filter(|(_, r)| {
            r.ticket.eq_ignore_ascii_case(ticket_id)
                && matches!(r.outcome, Some(pipeline::RunOutcome::Queued) | Some(pipeline::RunOutcome::Running))
        })
        .max_by(|a, b| a.1.started.cmp(&b.1.started))
}

/// Load the workspace's pipeline board (D15: workspace tasks home only) —
/// every pipeline definition, plus every workspace-home task with
/// `Task::lane` resolved against them (`pipeline::resolve_board`).
fn load_pipeline_board(server: &Server) -> Result<(PathBuf, Vec<tasks::Task>, Vec<pipeline::Pipeline>), String> {
    let ws_root = resolve_workspace_root(server)?;
    let pipelines = pipelines_scan(&ws_root);
    let home = tasks::TaskHome::Workspace { workspace_root: &ws_root };
    let mut tasks_list = tasks::scan_tasks(&[home]).map_err(|e| e.to_string())?;
    pipeline::resolve_board(&mut tasks_list, &pipelines);
    Ok((ws_root, tasks_list, pipelines))
}

/// A ticket by id that resolved to a real pipeline lane
/// (`Task::lane.is_some()`, set only by `pipeline::resolve_board` when
/// both the ticket's `pipeline:` id and its `status` lane resolve).
/// Mirrors src-tauri's `find_pipeline_ticket`: a classic (non-pipeline)
/// ticket, or a pipeline ticket sitting in an unresolvable lane/pipeline,
/// is refused here rather than partially handled — that belongs to Ken's
/// needs-attention tray, not an agent run.
fn find_pipeline_ticket<'a>(tasks_list: &'a [tasks::Task], id: &str) -> Result<&'a tasks::Task, String> {
    let t = tasks::find_by_id(tasks_list, id)
        .ok_or_else(|| format!("no pipeline ticket with id '{id}' in the workspace tasks home"))?;
    if t.lane.is_none() {
        return Err(format!(
            "ticket '{id}' is not a pipeline ticket, or its lane/pipeline could not be resolved — call pipeline_get or check Ken's needs-attention tray"
        ));
    }
    Ok(t)
}

/// `(today, now)` — `today` (`YYYY-MM-DD`) is the `updated` convention
/// every ken-tasks patch uses; `now` (`YYYY-MM-DDTHH:MM:SSZ`) is the full
/// timestamp convention `started`/`ended`/`blocked_at` use. Both derived
/// from the one UTC clock read `today_and_time_utc` already owns (module
/// doc: "no date/time crate is a ken-core dependency"), so this session
/// adds no new time source, just the two renderings ken-pipeline's own
/// fields need.
fn today_and_iso_now_utc() -> (String, String) {
    let (today, time_hhmm) = today_and_time_utc();
    let now = format!("{today}T{time_hhmm}:00Z");
    (today, now)
}

/// Reproduction of `tasks::TaskPatch`'s private `edits()` method. Needed
/// because closing a transition/block in one guarded write requires a
/// frontmatter patch AND a `## Log` body append together (`tasks::
/// apply_edits`'s whole point), and `ken_core::tasks::apply_patch_with_
/// pipelines` hard-codes `append_body: None`. Field list, key order, and
/// the `lane`/`status` mutual-exclusivity rule are read off `tasks.rs`
/// directly and reuse the same `pub` `scalar_lines`/`seq_lines` renderers,
/// so the rendered bytes are byte-identical to what the private method
/// would produce — a reproduction of public rendering rules, not a second
/// patch core (mirrors src-tauri's own `pipeline_patch_edits`, forced by
/// the same `crates/**` read-only constraint).
fn pipeline_patch_edits(patch: &tasks::TaskPatch) -> Vec<(&'static str, Vec<String>)> {
    let mut out: Vec<(&'static str, Vec<String>)> = Vec::new();
    if let Some(v) = &patch.title {
        out.push(("title", tasks::scalar_lines("title", v)));
    }
    if let Some(v) = &patch.lane {
        out.push(("status", tasks::scalar_lines("status", v)));
    } else if let Some(v) = patch.status {
        out.push(("status", tasks::scalar_lines("status", v.as_str())));
    }
    if let Some(v) = patch.kind {
        out.push(("kind", tasks::scalar_lines("kind", v.as_str())));
    }
    if let Some(v) = &patch.assignee {
        out.push(("assignee", tasks::scalar_lines("assignee", v)));
    }
    if let Some(v) = &patch.project {
        out.push(("project", tasks::scalar_lines("project", v)));
    }
    if let Some(v) = &patch.tags {
        out.push(("tags", tasks::seq_lines("tags", v)));
    }
    if let Some(v) = &patch.due {
        out.push(("due", tasks::scalar_lines("due", v)));
    }
    if let Some(v) = &patch.goal {
        out.push(("goal", tasks::scalar_lines("goal", v)));
    }
    if let Some(v) = patch.board {
        out.push(("board", tasks::scalar_lines("board", v.as_str())));
    }
    if let Some(v) = &patch.pipeline {
        out.push(("pipeline", tasks::scalar_lines("pipeline", v)));
    }
    if let Some(v) = &patch.model {
        out.push(("model", tasks::scalar_lines("model", v)));
    }
    if let Some(v) = &patch.agent {
        out.push(("agent", tasks::scalar_lines("agent", v)));
    }
    if let Some(v) = &patch.scope {
        out.push(("scope", tasks::seq_lines("scope", v)));
    }
    if let Some(v) = &patch.verify {
        out.push(("verify", tasks::scalar_lines("verify", v)));
    }
    if let Some(v) = patch.bounces {
        out.push(("bounces", tasks::scalar_lines("bounces", &v.to_string())));
    }
    if let Some(v) = &patch.return_lane {
        out.push(("return_lane", tasks::scalar_lines("return_lane", v)));
    }
    if let Some(v) = &patch.blocked_by {
        out.push(("blocked_by", tasks::seq_lines("blocked_by", v)));
    }
    if let Some(v) = &patch.block_reason {
        out.push(("block_reason", tasks::scalar_lines("block_reason", v)));
    }
    if let Some(v) = &patch.blocked_at {
        out.push(("blocked_at", tasks::scalar_lines("blocked_at", v)));
    }
    if let Some(v) = &patch.parent {
        out.push(("parent", tasks::scalar_lines("parent", v)));
    }
    if let Some(v) = &patch.spawned_by {
        out.push(("spawned_by", tasks::scalar_lines("spawned_by", v)));
    }
    if let Some(v) = &patch.origin {
        out.push(("origin", tasks::scalar_lines("origin", v)));
    }
    if let Some(v) = &patch.projects {
        out.push(("projects", tasks::seq_lines("projects", v)));
    }
    if let Some(v) = &patch.target {
        out.push(("target", tasks::scalar_lines("target", v)));
    }
    out
}

/// The `## Log` body addition a transition/block/unblock composes: a
/// heading only if the body doesn't already have one, then `line`
/// verbatim. Mirrors src-tauri's `compose_pipeline_log_addition`.
fn compose_pipeline_log_addition(body: &str, line: &str) -> String {
    let has_heading = body.lines().any(|l| l.trim_end().eq_ignore_ascii_case(tasks::LOG_HEADING));
    let mut out = String::new();
    if !has_heading {
        out.push_str(tasks::LOG_HEADING);
        out.push_str("\n\n");
    }
    out.push_str(line.trim());
    out.push('\n');
    out
}

/// Apply a `TaskPatch` (as composed by `pipeline::advance`/`block`/
/// `unblock`) plus an optional `## Log` addition, in one guarded write.
/// Mirrors src-tauri's `apply_pipeline_patch`: refuses to touch a ticket
/// whose on-disk lane/status this pipeline set can't resolve, unless the
/// patch itself resolves it (`patch.sets_status()`) — "never write around
/// a lane it can't validate" (D2).
fn apply_pipeline_patch(
    path: &Path,
    patch: &tasks::TaskPatch,
    updated: &str,
    pipelines: &[pipeline::Pipeline],
    log_addition: Option<&str>,
) -> Result<(), String> {
    let raw = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut current = tasks::parse_task(path, tasks::HomeKind::Workspace, "", &raw);
    pipeline::resolve_task_lane(&mut current, pipelines);
    if current.has_invalid_status() && !patch.sets_status() {
        return Err(format!(
            "ticket '{}' has an unrecognized lane — resolve it before patching other keys",
            current.id
        ));
    }
    let mut edits = pipeline_patch_edits(patch);
    edits.push(("updated", tasks::scalar_lines("updated", updated)));
    match log_addition {
        Some(line) => {
            let f = |body: &str| compose_pipeline_log_addition(body, line);
            tasks::apply_edits(path, &edits, Some(&f)).map_err(|e| e.to_string())
        }
        None => tasks::apply_edits(path, &edits, None).map_err(|e| e.to_string()),
    }
}

/// Close a run record: `ended`/`outcome`/(optionally) `artifacts` as
/// frontmatter edits, the agent's `report` as the body (the run's body
/// starts empty at claim time, so this is the report becoming the whole
/// body). Mirrors src-tauri's `close_run_record`.
fn close_run_record(path: &Path, outcome: pipeline::RunOutcome, report: &str, artifacts: &[String]) -> Result<(), String> {
    let raw = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let (_, now) = today_and_iso_now_utc();
    let mut edits: Vec<(&str, Vec<String>)> = vec![
        ("ended", tasks::scalar_lines("ended", &now)),
        ("outcome", tasks::scalar_lines("outcome", outcome.as_str())),
    ];
    if !artifacts.is_empty() {
        edits.push(("artifacts", tasks::seq_lines("artifacts", artifacts)));
    }
    let next = tasks::patch_text(&raw, &edits, Some(report));
    std::fs::write(path, next).map_err(|e| e.to_string())
}

/// A fresh run record for a ticket a claim is about to start or queue.
fn new_run_record(
    ticket: &tasks::Task,
    pl: &pipeline::Pipeline,
    lane: &pipeline::Lane,
    fields: &pipeline::TicketFields,
    agent: &str,
    model_arg: Option<&str>,
    outcome: pipeline::RunOutcome,
    now: &str,
) -> pipeline::RunRecord {
    let model = model_arg
        .map(str::to_string)
        .or_else(|| fields.model.clone())
        .or_else(|| lane.model.clone())
        .unwrap_or_default();
    pipeline::RunRecord {
        id: tasks::new_ulid(),
        ticket: ticket.id.clone(),
        pipeline: pl.id.clone(),
        lane: lane.id.clone(),
        agent: agent.to_string(),
        model,
        scope: fields.scope.clone(),
        verify: fields.verify.clone().unwrap_or_default(),
        started: now.to_string(),
        ended: String::new(),
        outcome: Some(outcome),
        outcome_raw: outcome.as_str().to_string(),
        artifacts: Vec::new(),
        report: String::new(),
    }
}

fn write_new_run(ws_root: &Path, record: &pipeline::RunRecord) -> Result<(), String> {
    let month = pipeline::run_month(&record.started);
    let dir = pipeline::run_month_dir(ws_root, &month);
    let path = pipeline::run_path(ws_root, &month, &record.id);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(&path, pipeline::compose_run(record)).map_err(|e| e.to_string())
}

// -- Readable error strings for the ken-core refusal/issue enums that do
// NOT derive `Serialize`-to-text on their own (only their "successful
// outcome" siblings matter to a caller that just wants JSON — a text tool
// reply needs prose). Converting rather than deriving `Display`:
// `crates/**` is read-only this session. Mirrors src-tauri's own
// `describe_*` functions (`lib.rs`), independently reproduced here for the
// same reason `pipeline_patch_edits` is. --

fn describe_confirm_reason(r: &pipeline::ConfirmReason) -> String {
    match r {
        pipeline::ConfirmReason::LaneGate => {
            "this lane requires a human confirmation (kickoff: confirm) in the Ken app before any run may start".to_string()
        }
        pipeline::ConfirmReason::ManualKickoff => {
            "this lane is kickoff: manual — it never starts a run by itself; a human must kick it off in the Ken app first".to_string()
        }
        pipeline::ConfirmReason::MissingBoundary { scope, verify } => {
            let mut missing = Vec::new();
            if *scope {
                missing.push("scope");
            }
            if *verify {
                missing.push("verify");
            }
            format!(
                "the ticket is missing {} — a ticket lacking either can never auto-run; add it, or a human must confirm the run in the Ken app",
                missing.join(" and ")
            )
        }
        pipeline::ConfirmReason::Unblocked => {
            "the ticket just returned from being blocked — an unblock always re-enters through a confirmation, never straight into a run".to_string()
        }
        pipeline::ConfirmReason::AutoDisabled => {
            "this lane is kickoff: auto but the pipeline's auto master switch is off".to_string()
        }
    }
}

fn describe_refusal_reason(r: &pipeline::RefusalReason) -> String {
    match r {
        pipeline::RefusalReason::Blocked { return_lane, blocked_by, block_reason } => {
            let mut parts = vec!["the ticket is blocked".to_string()];
            if let Some(rl) = return_lane {
                parts.push(format!("returns to '{rl}'"));
            }
            if !blocked_by.is_empty() {
                parts.push(format!("blocked by {}", blocked_by.join(", ")));
            }
            if let Some(reason) = block_reason {
                parts.push(reason.clone());
            }
            parts.join(" — ")
        }
        pipeline::RefusalReason::HumanLane { lane: l } => format!("lane '{l}' is a human sign-off lane — no agent, no kickoff"),
        pipeline::RefusalReason::NoAgent { lane: l } => format!("lane '{l}' has no agent — it is a holding column"),
        pipeline::RefusalReason::ManualLane { lane: l } => {
            format!("lane '{l}' is kickoff: manual and was reached by something other than a human asking")
        }
    }
}

fn describe_transition_refusal(r: &pipeline::TransitionRefusal) -> String {
    match r {
        pipeline::TransitionRefusal::UnknownLane { status: s } => format!("ticket's status '{s}' names no lane"),
        pipeline::TransitionRefusal::NoEdge { lane, outcome } => {
            format!("lane '{lane}' declares no {} edge", outcome.as_str())
        }
        pipeline::TransitionRefusal::UnknownTarget { lane, target } => {
            format!("lane '{lane}' points to unknown lane '{target}'")
        }
        pipeline::TransitionRefusal::NoBlockedLane => {
            "this pipeline declares no blocked lane to escalate the retry-cap breach into".to_string()
        }
    }
}

fn describe_block_refusal(r: &pipeline::BlockRefusal) -> String {
    match r {
        pipeline::BlockRefusal::NoBlockedLane => "this pipeline declares no blocked lane".to_string(),
        pipeline::BlockRefusal::UnknownLane { status: s } => format!("ticket's current status '{s}' names no lane"),
        pipeline::BlockRefusal::MissingReturnLane => {
            "ticket is already blocked but its return lane is missing or orphaned — resolve it in Ken's needs-attention tray first".to_string()
        }
        pipeline::BlockRefusal::Empty => "neither a dependency nor a reason was given".to_string(),
        pipeline::BlockRefusal::PathBlocker(s) => {
            format!("'{s}' looks like a path, not a ticket id — blockedBy must hold ticket ids (ULIDs), never paths")
        }
        pipeline::BlockRefusal::MalformedBlocker(s) => format!("'{s}' is not a valid ticket id (ULID)"),
        pipeline::BlockRefusal::SelfBlock(s) => format!("a ticket cannot block itself ('{s}')"),
        pipeline::BlockRefusal::Cycle(c) => format!("blocking on this would close a dependency cycle: {}", c.render()),
    }
}

fn describe_unblock_refusal(r: &pipeline::UnblockRefusal) -> String {
    match r {
        pipeline::UnblockRefusal::NotBlocked => "ticket is not blocked".to_string(),
        pipeline::UnblockRefusal::MissingReturnLane => {
            "ticket's return lane is missing or names a lane the pipeline no longer declares".to_string()
        }
        pipeline::UnblockRefusal::Empty => "neither clear.deps nor clear.reason was set".to_string(),
    }
}

/// A project's display symbol (D12/OPEN-2, `ProjectConfig::symbol`), by
/// project name — best-effort: an unregistered or unreadable project name
/// simply contributes no symbol rather than failing the whole call (a
/// pipeline ticket's `project` string is free text, not a foreign key).
fn project_symbol(server: &Server, project_name: &str) -> Option<String> {
    if project_name.trim().is_empty() {
        return None;
    }
    let registry = Registry::load(&server.base_dir).ok()?;
    let entry = registry.projects.iter().find(|p| p.name.eq_ignore_ascii_case(project_name))?;
    let project = Project::open(&entry.path).ok()?;
    project.config.symbol().map(str::to_string)
}

/// The `## Log` section of a ticket's body, tail-truncated to `max_chars`
/// on a UTF-8 boundary (`pipeline_get`'s "log tail"). Empty when the body
/// has no `## Log` heading yet.
fn log_tail(body: &str, max_chars: usize) -> String {
    let lower = body.to_ascii_lowercase();
    let Some(pos) = lower.find(&tasks::LOG_HEADING.to_ascii_lowercase()) else {
        return String::new();
    };
    let section = &body[pos..];
    if section.len() <= max_chars {
        return section.to_string();
    }
    let start = floor_char_boundary_at(section.as_bytes(), section.len() - max_chars);
    format!("... (truncated)\n{}", &section[start..])
}

/// `pipeline_list` (task 3.1): the pipeline board's read side, paged.
/// Compact rows only — id, title, lane, project symbol, model, assignee,
/// block summary, updated — **never a ticket's body**; `pipeline_get`
/// returns that. Default page size 20, hard max 100, opaque cursor (the
/// last row's id — paging asks for everything strictly after it, so a
/// board mutated between pages can only ever skip or repeat a boundary
/// row, never loop).
fn pipeline_list_tool(server: &Server, args: &Value) -> Result<String, String> {
    require_pipeline_flag(server, "pipeline_list")?;
    let (_ws_root, tasks_list, pipelines) = load_pipeline_board(server)?;

    let filter_obj = args.get("filter");
    let get_str = |key: &str| -> Option<String> {
        filter_obj
            .and_then(|o| o.get(key))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let mut filter = tasks::TaskFilter::default();
    filter.lane = get_str("lane");
    filter.pipeline = get_str("pipeline");
    filter.project = get_str("project");
    filter.assignee = get_str("assignee").map(|a| tasks::AssigneeFilter::parse(&a));
    let model_filter = get_str("model");
    if let Some(id) = get_str("blockedBy") {
        filter.blocked = Some(tasks::BlockedFilter::By(id));
    } else if let Some(b) = get_str("blocked") {
        filter.blocked = Some(match b.as_str() {
            "any" => tasks::BlockedFilter::Any,
            "blocked" => tasks::BlockedFilter::Blocked,
            "notBlocked" => tasks::BlockedFilter::NotBlocked,
            "newlyUnblocked" => tasks::BlockedFilter::NewlyUnblocked,
            other => {
                return Err(format!(
                    "invalid \"blocked\" {other:?} in filter — use any, blocked, notBlocked, or newlyUnblocked"
                ))
            }
        });
    }

    let mut hits: Vec<&tasks::Task> = tasks::filter_tasks(&tasks_list, &filter)
        .into_iter()
        .filter(|t| t.lane.is_some())
        .collect();
    if let Some(m) = &model_filter {
        hits.retain(|t| {
            let f = pipeline::ticket_fields(t);
            let lane_default = f
                .pipeline
                .as_deref()
                .and_then(|pid| pipeline::find_pipeline(&pipelines, pid))
                .and_then(|p| pipeline::resolve_lane(p, &t.status_raw))
                .and_then(|l| l.model.clone());
            f.model.clone().or(lane_default).is_some_and(|em| em.eq_ignore_ascii_case(m))
        });
    }
    // Deterministic paging order: id ascending — ULIDs are time-sortable,
    // so this also reads as roughly creation order.
    hits.sort_by(|a, b| a.id.cmp(&b.id));

    let hard_max = 100usize;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map(|l| (l as usize).clamp(1, hard_max))
        .unwrap_or(20);
    let cursor = args.get("cursor").and_then(Value::as_str);
    let start = match cursor {
        None => 0,
        Some(c) => hits.iter().position(|t| t.id == c).map(|i| i + 1).unwrap_or(0),
    };
    let page: Vec<&tasks::Task> = hits.iter().skip(start).take(limit).copied().collect();

    if page.is_empty() {
        return Ok("No tickets match that filter.".to_string());
    }
    let mut out = format!(
        "{} ticket{} match (showing {}):\n",
        hits.len(),
        if hits.len() == 1 { "" } else { "s" },
        page.len()
    );
    for t in &page {
        let f = pipeline::ticket_fields(t);
        let symbol = project_symbol(server, &t.project);
        let block_summary = if !f.blocked_by.is_empty() || f.block_reason.is_some() {
            let mut parts = Vec::new();
            if !f.blocked_by.is_empty() {
                parts.push(format!("by {}", f.blocked_by.join(",")));
            }
            if let Some(r) = &f.block_reason {
                parts.push(r.clone());
            }
            format!(" BLOCKED({})", parts.join("; "))
        } else if f.return_lane.is_some() {
            " newly-unblocked".to_string()
        } else {
            String::new()
        };
        out.push_str(&format!(
            "\n{} — \"{}\" [lane={}{}] model={} assignee={} updated={}{}",
            t.id,
            t.title,
            t.lane.as_deref().unwrap_or("?"),
            symbol.map(|s| format!(" {s}")).unwrap_or_default(),
            f.model.as_deref().unwrap_or("-"),
            if t.assignee.is_empty() { "none" } else { &t.assignee },
            t.updated,
            block_summary,
        ));
    }
    if start + page.len() < hits.len() {
        out.push_str(&format!("\n\ncursor: {}", page.last().unwrap().id));
    }
    Ok(out)
}

/// `pipeline_get` (task 3.2): the one tool returning a full ticket.
fn pipeline_get_tool(server: &Server, args: &Value) -> Result<String, String> {
    require_pipeline_flag(server, "pipeline_get")?;
    let id = require_str(args, "id")?;
    let (_ws_root, tasks_list, _pipelines) = load_pipeline_board(server)?;
    let ticket = find_pipeline_ticket(&tasks_list, &id)?;
    let f = pipeline::ticket_fields(ticket);
    let graph = pipeline::BlockGraph::from_tasks(&tasks_list);
    let root = pipeline::root_blockers(&graph, &ticket.id);

    let mut out = format!(
        "{} — \"{}\"\nlane={} pipeline={} model={} agent={} bounces={} updated={}\nscope=[{}]\nverify={}\n",
        ticket.id,
        ticket.title,
        ticket.lane.as_deref().unwrap_or("?"),
        f.pipeline.as_deref().unwrap_or("-"),
        f.model.as_deref().unwrap_or("-"),
        f.agent.as_deref().unwrap_or("-"),
        f.bounces,
        ticket.updated,
        f.scope.join(", "),
        f.verify.as_deref().unwrap_or("(none — cannot auto-run under any lane setting)"),
    );
    if !f.blocked_by.is_empty() || f.block_reason.is_some() {
        out.push_str(&format!(
            "BLOCKED — blocked_by=[{}] reason={} return_lane={} root_blockers=[{}]\n",
            f.blocked_by.join(", "),
            f.block_reason.as_deref().unwrap_or("-"),
            f.return_lane.as_deref().unwrap_or("-"),
            root.join(", "),
        ));
    }
    out.push_str("\n--- body ---\n");
    out.push_str(&ticket.body);
    let tail = log_tail(&ticket.body, 2000);
    if !tail.is_empty() {
        out.push_str("\n\n--- log tail ---\n");
        out.push_str(&tail);
    }
    Ok(out)
}

/// `pipeline_claim` (task 3.3): re-runs `pipeline::admit` server-side with
/// `EntryKind::Claim` — the single admission function (D5) — and only on
/// `Admission::Start` does this write a `running` run record. Reuses an
/// already-`queued` run (filed by a human's kickoff in the Ken app) when
/// one exists, flipping it to `running`; otherwise files a fresh one, so
/// the claim protocol works whether or not a UI kickoff preceded it.
/// Refuses (writing nothing) when the ticket is blocked or the lane is
/// structurally unrunnable, and refuses — leaving/filing a `queued` record
/// — when `admit` reports the concurrency cap is full or the ticket still
/// needs a human confirmation an MCP agent cannot supply on its own.
fn pipeline_claim_tool(server: &Server, args: &Value) -> Result<String, String> {
    require_pipeline_flag(server, "pipeline_claim")?;
    let id = require_str(args, "id")?;
    let agent = require_str(args, "agent")?;
    let model_arg = opt_str(args, "model");

    let (ws_root, tasks_list, pipelines) = load_pipeline_board(server)?;
    let ticket = find_pipeline_ticket(&tasks_list, &id)?;
    let fields = pipeline::ticket_fields(ticket);
    let pipeline_id = fields.pipeline.clone().ok_or_else(|| format!("ticket '{id}' has no pipeline key"))?;
    let pl = pipeline::find_pipeline(&pipelines, &pipeline_id).ok_or_else(|| format!("pipeline '{pipeline_id}' is not loaded"))?;
    let lane = pipeline::resolve_lane(pl, &ticket.status_raw)
        .ok_or_else(|| format!("ticket '{id}' status '{}' names no lane in pipeline '{pipeline_id}'", ticket.status_raw))?;

    let runs_with_paths = run_ledger_scan_with_paths(&ws_root);
    // Judgment call beyond `admit`'s own cross-product (which counts
    // running records workspace-wide against the cap, not per-ticket): a
    // ticket already `running` cannot be claimed a second time, whatever
    // the cap says. `admit` has no opinion on this (it has no per-ticket
    // "already claimed" concept), so it is checked here, before `admit`
    // runs at all, and — like every other refusal path — writes nothing.
    if let Some((_, existing)) = runs_with_paths
        .iter()
        .find(|(_, r)| r.ticket.eq_ignore_ascii_case(&id) && r.outcome == Some(pipeline::RunOutcome::Running))
    {
        return Err(format!(
            "ticket '{id}' is already claimed and running (agent '{}', run '{}') — call pipeline_advance to close it before claiming again",
            existing.agent, existing.id
        ));
    }

    let runs: Vec<pipeline::RunRecord> = runs_with_paths.iter().map(|(_, r)| r.clone()).collect();
    let admission = pipeline::admit(ticket, lane, pl, &runs, pipeline::EntryKind::Claim);
    match admission {
        pipeline::Admission::Refused { reason } => Err(format!("claim refused: {}", describe_refusal_reason(&reason))),
        pipeline::Admission::Confirm { reason } => Err(format!(
            "claim refused: {} — an MCP agent cannot accept a confirmation on the ticket's behalf; ask a human to approve it in the Ken app, then retry the claim",
            describe_confirm_reason(&reason)
        )),
        pipeline::Admission::Queued { running, cap } => {
            let (_, now) = today_and_iso_now_utc();
            let run_id = match find_open_run(&runs_with_paths, &id) {
                Some((_, existing)) => existing.id.clone(),
                None => {
                    let record = new_run_record(ticket, pl, lane, &fields, &agent, model_arg.as_deref(), pipeline::RunOutcome::Queued, &now);
                    write_new_run(&ws_root, &record)?;
                    record.id
                }
            };
            Err(format!(
                "claim refused: the concurrency cap is full ({running}/{cap} running) — ticket '{id}' stays queued as run '{run_id}'; retry later or call pipeline_runs to watch it"
            ))
        }
        pipeline::Admission::Start => {
            let (_, now) = today_and_iso_now_utc();
            let model = model_arg
                .clone()
                .or_else(|| fields.model.clone())
                .or_else(|| lane.model.clone())
                .unwrap_or_default();
            let record = match find_open_run(&runs_with_paths, &id) {
                Some((path, existing)) if existing.outcome == Some(pipeline::RunOutcome::Queued) => {
                    let edits: Vec<(&str, Vec<String>)> = vec![
                        ("agent", tasks::scalar_lines("agent", &agent)),
                        ("model", tasks::scalar_lines("model", &model)),
                        ("started", tasks::scalar_lines("started", &now)),
                        ("outcome", tasks::scalar_lines("outcome", pipeline::RunOutcome::Running.as_str())),
                    ];
                    let raw = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
                    let next = tasks::patch_text(&raw, &edits, None);
                    std::fs::write(path, next).map_err(|e| e.to_string())?;
                    pipeline::RunRecord {
                        agent: agent.clone(),
                        model,
                        started: now,
                        outcome: Some(pipeline::RunOutcome::Running),
                        outcome_raw: pipeline::RunOutcome::Running.as_str().to_string(),
                        ..existing.clone()
                    }
                }
                _ => {
                    let record = new_run_record(ticket, pl, lane, &fields, &agent, model_arg.as_deref(), pipeline::RunOutcome::Running, &now);
                    write_new_run(&ws_root, &record)?;
                    record
                }
            };
            server.known_running.borrow_mut().insert(record.id.clone());
            Ok(format!(
                "Claimed ticket '{}' (\"{}\") in lane '{}' — run '{}' now running (agent '{}', model '{}'). scope=[{}] verify={}. Call pipeline_get(\"{}\") for the full brief before writing anything, then pipeline_advance(\"{}\", \"pass\"|\"fail\", report) when done — an unreported claim eventually shows up as a stale run.",
                ticket.id,
                ticket.title,
                lane.id,
                record.id,
                record.agent,
                record.model,
                record.scope.join(", "),
                if record.verify.is_empty() { "(none)".to_string() } else { record.verify.clone() },
                ticket.id,
                ticket.id,
            ))
        }
    }
}

/// `pipeline_advance` (task 3.4): resolve the target lane via `pipeline::
/// advance` (bounce accounting and the retry-cap-to-block escalation both
/// live there, never re-derived here), apply the resulting patch + `##
/// Log` line in one guarded write, and close whichever run is currently
/// open for this ticket (best-effort — a ticket advanced with no matching
/// open run on the ledger still advances, D13: "derived, not owned").
fn pipeline_advance_tool(server: &Server, args: &Value) -> Result<String, String> {
    require_pipeline_flag(server, "pipeline_advance")?;
    let id = require_str(args, "id")?;
    let outcome_str = require_str(args, "outcome")?;
    let outcome = pipeline::AdvanceOutcome::parse(&outcome_str)
        .ok_or_else(|| format!("invalid \"outcome\" {outcome_str:?} — use pass or fail"))?;
    let report = require_str(args, "report")?;
    let artifacts: Vec<String> = args
        .get("artifacts")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str()).map(str::to_string).collect())
        .unwrap_or_default();

    let (ws_root, tasks_list, pipelines) = load_pipeline_board(server)?;
    let ticket = find_pipeline_ticket(&tasks_list, &id)?;
    let path = ticket.path.clone();
    let pipeline_id = pipeline::ticket_fields(ticket).pipeline.clone().ok_or_else(|| format!("ticket '{id}' has no pipeline key"))?;
    let pl = pipeline::find_pipeline(&pipelines, &pipeline_id).ok_or_else(|| format!("pipeline '{pipeline_id}' is not loaded"))?;

    let (today, now) = today_and_iso_now_utc();
    let transition = pipeline::advance(ticket, pl, outcome, &now);
    let (patch, log) = match &transition {
        pipeline::Transition::Moved { patch, log, .. } => (patch, log.clone()),
        pipeline::Transition::Blocked { patch, log, .. } => (patch, log.clone()),
        pipeline::Transition::Refused { reason } => {
            return Err(format!("advance refused: {}", describe_transition_refusal(reason)))
        }
    };
    apply_pipeline_patch(&path, patch, &today, &pipelines, Some(&log))?;

    let runs_with_paths = run_ledger_scan_with_paths(&ws_root);
    let mut closed_note = String::new();
    if let Some((run_path, open_run)) = find_open_run(&runs_with_paths, &id) {
        let run_outcome = match &transition {
            pipeline::Transition::Blocked { .. } => pipeline::RunOutcome::Blocked,
            _ => match outcome {
                pipeline::AdvanceOutcome::Pass => pipeline::RunOutcome::Pass,
                pipeline::AdvanceOutcome::Fail => pipeline::RunOutcome::Fail,
            },
        };
        match close_run_record(run_path, run_outcome, &report, &artifacts) {
            Ok(()) => closed_note = format!(" Run '{}' closed as {}.", open_run.id, run_outcome.as_str()),
            Err(e) => closed_note = format!(" (warning: could not close run '{}': {e})", open_run.id),
        }
        server.known_running.borrow_mut().remove(&open_run.id);
    }

    Ok(match &transition {
        pipeline::Transition::Moved { from, to, backward, bounces, .. } => format!(
            "Ticket '{id}' advanced {from} → {to}{}.{closed_note}",
            if *backward { format!(" (bounce {bounces})") } else { String::new() }
        ),
        pipeline::Transition::Blocked { from, would_have_entered, block, .. } => format!(
            "Ticket '{id}' bounced past the retry cap at '{from}' (was heading to '{would_have_entered}') — now blocked, returns to '{}': {}.{closed_note}",
            block.return_lane(),
            block.reason().unwrap_or("")
        ),
        pipeline::Transition::Refused { .. } => unreachable!("handled above"),
    })
}

/// `pipeline_block` (task 3.5): set or clear a block, routed through
/// `pipeline::block`/`unblock` so cycle detection and `return_lane`
/// capture cannot be bypassed. `clear` (present) means unblock;
/// `blockedBy`/`reason` (absent `clear`) means block — the two are
/// mutually exclusive by which arguments are given, not by a separate
/// mode flag.
fn pipeline_block_tool(server: &Server, args: &Value) -> Result<String, String> {
    require_pipeline_flag(server, "pipeline_block")?;
    let id = require_str(args, "id")?;
    let (_ws_root, tasks_list, pipelines) = load_pipeline_board(server)?;
    let ticket = find_pipeline_ticket(&tasks_list, &id)?;
    let path = ticket.path.clone();
    let pipeline_id = pipeline::ticket_fields(ticket).pipeline.clone().ok_or_else(|| format!("ticket '{id}' has no pipeline key"))?;
    let pl = pipeline::find_pipeline(&pipelines, &pipeline_id).ok_or_else(|| format!("pipeline '{pipeline_id}' is not loaded"))?;
    let (today, now) = today_and_iso_now_utc();

    if let Some(c) = args.get("clear") {
        let clear_deps = c.get("deps").and_then(Value::as_bool).unwrap_or(false);
        let clear_reason = c.get("reason").and_then(Value::as_bool).unwrap_or(false);
        let request = pipeline::UnblockRequest { clear_deps, clear_reason };
        return match pipeline::unblock(ticket, pl, &request) {
            pipeline::UnblockOutcome::Released { return_lane, patch, log, .. } => {
                apply_pipeline_patch(&path, &patch, &today, &pipelines, Some(&log))?;
                Ok(format!(
                    "Ticket '{id}' unblocked — returns to '{return_lane}' awaiting a confirmation there (unblocking never starts a run by itself)."
                ))
            }
            pipeline::UnblockOutcome::StillBlocked { remaining_deps, remaining_reason, patch } => {
                apply_pipeline_patch(&path, &patch, &today, &pipelines, None)?;
                Ok(format!(
                    "Ticket '{id}' partially cleared — still blocked: remaining deps=[{}] reason={}",
                    remaining_deps.join(", "),
                    remaining_reason.as_deref().unwrap_or("-")
                ))
            }
            pipeline::UnblockOutcome::Refused { reason } => Err(format!("unblock refused: {}", describe_unblock_refusal(&reason))),
        };
    }

    let blocked_by: Vec<String> = args
        .get("blockedBy")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str()).map(str::to_string).collect())
        .unwrap_or_default();
    let reason = opt_str(args, "reason");
    if blocked_by.is_empty() && reason.is_none() {
        return Err("pipeline_block needs \"blockedBy\", \"reason\", or \"clear\" — nothing to do".to_string());
    }
    let graph = pipeline::BlockGraph::from_tasks(&tasks_list);
    let request = pipeline::BlockRequest { blocked_by, reason, now };
    match pipeline::block(ticket, pl, &graph, &request) {
        pipeline::BlockOutcome::Blocked { block, patch, log } => {
            apply_pipeline_patch(&path, &patch, &today, &pipelines, Some(&log))?;
            Ok(format!(
                "Ticket '{id}' blocked — returns to '{}'. blocked_by=[{}] reason={}. It is now refused by pipeline_claim on every lane, including an auto lane with the pipeline's master switch on.",
                block.return_lane(),
                block.blocked_by().join(", "),
                block.reason().unwrap_or("-")
            ))
        }
        pipeline::BlockOutcome::Refused { reason } => Err(format!("block refused: {}", describe_block_refusal(&reason))),
    }
}

/// `pipeline_runs` (task 3.6): the watch surface over `pipeline::
/// derive_queue` — running/queued/blocked/waitingHuman/stale, entirely
/// derived from the ledger plus the board (D13), never a second state
/// store. `known_running` is this server process's own claims this
/// session (see `Server::known_running`'s doc comment) — a `running`
/// record this process has no live memory of (e.g. claimed by another
/// agent, or before this process started) reports as `stale`.
fn pipeline_runs_tool(server: &Server, args: &Value) -> Result<String, String> {
    require_pipeline_flag(server, "pipeline_runs")?;
    let (ws_root, tasks_list, pipelines) = load_pipeline_board(server)?;
    let runs = run_ledger_scan(&ws_root);
    let known = server.known_running.borrow().clone();
    let queue = pipeline::derive_queue(&tasks_list, &pipelines, &runs, &known);
    let state = args.get("state").and_then(Value::as_str).unwrap_or("all");

    let fmt_run = |r: &pipeline::RunRecord| {
        format!(
            "  {} — ticket={} lane={} agent={} model={} started={}\n",
            r.id, r.ticket, r.lane, r.agent, r.model, r.started
        )
    };
    let mut out = String::new();
    if state == "all" || state == "running" {
        out.push_str(&format!("running ({}):\n", queue.running.len()));
        queue.running.iter().for_each(|r| out.push_str(&fmt_run(r)));
    }
    if state == "all" || state == "queued" {
        out.push_str(&format!("queued ({}):\n", queue.queued.len()));
        queue.queued.iter().for_each(|r| out.push_str(&fmt_run(r)));
    }
    if state == "all" || state == "blocked" {
        out.push_str(&format!("blocked ({}):\n", queue.blocked.len()));
        queue.blocked.iter().for_each(|r| out.push_str(&fmt_run(r)));
    }
    if state == "all" || state == "waitingHuman" {
        out.push_str(&format!("waitingHuman ({}):\n", queue.waiting_human.len()));
        queue.waiting_human.iter().for_each(|id| out.push_str(&format!("  {id}\n")));
    }
    if state == "all" || state == "stale" {
        out.push_str(&format!("stale ({}):\n", queue.stale.len()));
        queue.stale.iter().for_each(|r| out.push_str(&fmt_run(r)));
    }
    Ok(out)
}

/// `pipeline_digest` (task 3.7): the daily update, via `pipeline::
/// compose_digest` + `render_digest_markdown` — "the one renderer
/// chat/MCP/journal_append all call" (pipeline.rs doc comment), so this
/// tool's output can never drift from the desktop app's own digest text.
/// Journals through `memory::append_journal` when `kenMemory` is on
/// (task 2.10's sibling behaviour for this surface); skipped cleanly,
/// with no attempt and no note, when it is off.
fn pipeline_digest_tool(server: &Server, args: &Value) -> Result<String, String> {
    require_pipeline_flag(server, "pipeline_digest")?;
    let (ws_root, tasks_list, pipelines) = load_pipeline_board(server)?;
    let runs = run_ledger_scan(&ws_root);
    let known = server.known_running.borrow().clone();
    let (today_default, _) = today_and_time_utc();
    let today = args.get("day").and_then(Value::as_str).map(str::to_string).unwrap_or(today_default);
    let digest = pipeline::compose_digest(&tasks_list, &pipelines, &runs, &known, &today);
    let mut out = pipeline::render_digest_markdown(&digest, &today);

    if ken_memory_enabled(&AppSettings::load(&server.base_dir)) {
        let (d, t) = today_and_time_utc();
        match memory::append_journal(&ws_root, &out, None, &["pipeline-digest".to_string()], &d, &t) {
            Ok(_) => out.push_str("\n\n(journaled)"),
            Err(e) => out.push_str(&format!("\n\n(warning: could not write journal: {e})")),
        }
    }
    Ok(out)
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

/// Global-layer value of a flag: `settings.json`'s `features` map, else the
/// flag's registered default (false if unregistered — e.g. `kgRouting`
/// before its own registration lands; treating an unrecognized name as off
/// matches `effective_flag`'s behavior for the same case). Deliberately
/// global-only rather than `effective_flag` + a project: `workspace`,
/// `federatedKg`, and `kgRouting` are workspace-/global-scoped flags with no
/// single "the" project to ask when ken-mcp is unscoped — mirrors
/// src-tauri's own dedicated `workspace_enabled`/`federated_kg_enabled`
/// helpers (`lib.rs`), which read the same way for the same reason rather
/// than through `effective_flag`'s project layer.
fn global_flag(app_settings: &AppSettings, name: &str) -> bool {
    let default = features::flag(name).map(|f| f.default).unwrap_or(false);
    app_settings
        .features
        .get(name)
        .and_then(Value::as_bool)
        .unwrap_or(default)
}

fn workspace_enabled(app_settings: &AppSettings) -> bool {
    global_flag(app_settings, "workspace")
}

/// Mirrors src-tauri's `federated_kg_enabled` (proposal: federatedKg
/// "Requires the workspace flag").
fn federated_kg_enabled(app_settings: &AppSettings) -> bool {
    workspace_enabled(app_settings) && global_flag(app_settings, "federatedKg")
}

/// design.md: "kgRouting (workspace-level). Requires semanticIndex and
/// workspace". The `workspace` half is checked the same way
/// `federated_kg_enabled` checks it; `semanticIndex` is per-project, not a
/// single global toggle, so it isn't checked here — it instead shows up as
/// `MemberInfo::index_ready` gating which *members* `route_query` can
/// actually target, which is the only place a per-project flag can mean
/// anything for a workspace-wide gate.
fn kg_routing_enabled(app_settings: &AppSettings) -> bool {
    workspace_enabled(app_settings) && global_flag(app_settings, "kgRouting")
}

/// `kenMemory` (workspace-level, requires `workspace` — proposal.md
/// "Flag"), gated exactly like `kg_routing_enabled` above. Note:
/// `kenMemory` is not yet a registered entry in `ken_core::features::FLAGS`
/// as of this layer — `openspec/changes/ken-memory/tasks.md` section 1
/// (ken-core) has no task registering it there, unlike `kgRouting`'s own
/// change, which registered its flag in ken-core before any tool surface
/// read it. `global_flag`'s unregistered-name fallback (`default = false`)
/// makes this correctly inert either way: off until both the registration
/// and an explicit `true` in `settings.json`'s `features` map exist, and
/// automatically picking up the real default the moment ken-core
/// registers the flag — no ken-mcp change needed when that lands.
fn ken_memory_enabled(app_settings: &AppSettings) -> bool {
    workspace_enabled(app_settings) && global_flag(app_settings, "kenMemory")
}

/// `kenTasks` (workspace-level, requires `workspace` — proposal.md
/// "Flag"), gated exactly like `ken_memory_enabled` above, including the
/// same tolerance for a not-yet-registered flag name: a parallel session is
/// registering `kenTasks` in `ken_core::features::FLAGS` (task brief), and
/// `global_flag`'s unregistered-name fallback (`default = false`) makes
/// this correct regardless of which change lands first.
fn ken_tasks_enabled(app_settings: &AppSettings) -> bool {
    workspace_enabled(app_settings) && global_flag(app_settings, "kenTasks")
}

/// `kenPipeline` (ken-pipeline task 3.1-3.8), mirroring src-tauri's own
/// `ken_pipeline_enabled` (`lib.rs`) exactly: AND-ed with `ken_tasks_
/// enabled`, which itself AND-s in `workspace_enabled` — the registry
/// description states "Requires the workspace and kenTasks flags", and this
/// transitively requires both without re-checking `workspace_enabled` a
/// second time. Gates all seven pipeline tools' presence in `tools/list`
/// (spec "Flag-scoped activation": "register no pipeline tools on either
/// surface"); every pipeline tool function below re-checks this itself too
/// (defense-in-depth, same posture as every other flag-gated tool in this
/// file — an MCP client can call any tool name whether or not `tools/list`
/// advertised it).
fn ken_pipeline_enabled(app_settings: &AppSettings) -> bool {
    ken_tasks_enabled(app_settings) && global_flag(app_settings, "kenPipeline")
}

/// Defense-in-depth flag re-check every pipeline tool function opens with
/// (same posture as every other flag-gated tool's inline `if !x_enabled
/// {...}` check — factored into one helper here only because seven tools
/// share the identical wording).
fn require_pipeline_flag(server: &Server, tool: &str) -> Result<(), String> {
    if ken_pipeline_enabled(&AppSettings::load(&server.base_dir)) {
        Ok(())
    } else {
        Err(format!("{tool} requires the kenPipeline feature flag, which is off."))
    }
}

/// `kenFamilies` (tasks 3.1-3.4). Deliberately **not** gated behind
/// `workspace` the way `ken_memory_enabled`/`ken_tasks_enabled` above are:
/// proposal.md introduces it as a plain "New global flag `kenFamilies`",
/// not "workspace-level, requires workspace" the way kenMemory/kenTasks are
/// documented, and nothing in design.md or spec.md ties a family
/// *connection* (a per-device app-data thing) to any one workspace being
/// open — a connection can exist, sync, and be read from before a workspace
/// is ever opened. Same registration-order tolerance as `ken_tasks_enabled`:
/// a parallel session is registering `kenFamilies` in
/// `ken_core::features::FLAGS`, and `global_flag`'s unregistered-name
/// fallback (`default = false`) keeps this correct regardless of which
/// change lands first.
fn ken_families_enabled(app_settings: &AppSettings) -> bool {
    global_flag(app_settings, "kenFamilies")
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
// Every call re-checks `ken_families_enabled` — same defense-in-depth
// posture as the memory/task tools above.

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
    /// write (family_send, a family-board task_update/task_complete) must
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

/// Which member this call acts as for `conn`: an explicit `as` argument
/// (validated against the manifest) if given, else the connection's
/// resolved identity, else a clear error telling the caller how to supply
/// one (family_list's members list is where to find valid ids).
fn resolve_family_identity(conn: &FamilyConnection, as_arg: Option<&str>) -> Result<String, String> {
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
    conn.my_member_id.clone().ok_or_else(|| {
        format!(
            "This device's member identity in family \"{}\" is not configured yet. \
Pass \"as\" with one of this family's member ids ({}) to say who you are.",
            conn.manifest.name,
            conn.manifest.member_ids().join(", ")
        )
    })
}

fn opt_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// Task 3.4's enforcement point: may this device, acting as `identity` (an
/// explicit `as` argument if given, else `fb.my_member_id`), write to
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
    let identity = as_arg.map(str::to_string).or_else(|| fb.my_member_id.clone()).ok_or_else(|| {
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
/// `task_complete_tool` already takes with journal-write failures.
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
    if !ken_families_enabled(&AppSettings::load(&server.base_dir)) {
        return Err("family_list requires the kenFamilies feature flag, which is off.".into());
    }
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
    if !ken_families_enabled(&AppSettings::load(&server.base_dir)) {
        return Err("family_inbox requires the kenFamilies feature flag, which is off.".into());
    }
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
    if !ken_families_enabled(&AppSettings::load(&server.base_dir)) {
        return Err("family_send requires the kenFamilies feature flag, which is off.".into());
    }
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
must explicitly accept a task item before it becomes a task on their board; a message or \
notification just waits, unread, until {to} reads or dismisses it. Nothing here enters {to}'s \
board, daily board, or any claimable queue automatically — there is no auto-accept.",
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

/// The workspace ken-mcp should read/write memories and the journal for:
/// Ken's registry `lastWorkspace` entry (task brief: "resolve the current
/// workspace as last_workspace's entry; if none, the tools should return a
/// clear 'no workspace' message"). Unlike `resolve_project`, there is no
/// `--project`-style scoping flag for a workspace to fall back on — ken-mcp
/// only ever knows "the workspace Ken last had open," mirroring how the app
/// itself reopens `lastWorkspace` on launch.
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
    fn kg_routing_tools_absent_and_erroring_when_flag_off() {
        let mut fx = fixture(true); // default settings — kgRouting unset, off
        let reply = call(&mut fx.server, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap();
        let names: Vec<_> = reply["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            names,
            ["search_knowledge", "read_document", "list_documents", "list_projects"],
            "flag off must be byte-identical to pre-kg-routing tool list"
        );

        // Dispatch still recognizes the tool names (not "unknown tool") and
        // explains the flag rather than erroring opaquely.
        for name in ["kg_search", "semantic_search", "route_query"] {
            let (text, is_err) = tool(&mut fx.server, name, json!({"query": "billing"}));
            assert!(is_err, "{name}: {text}");
            assert!(text.contains("kgRouting"), "{name}: {text}");
        }
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
                "search_knowledge",
                "read_document",
                "list_documents",
                "list_projects",
                "kg_search",
                "semantic_search",
                "route_query"
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
    fn kg_search_reports_federated_kg_off_without_erroring() {
        // two_project_fixture turns on workspace+kgRouting but not
        // federatedKg — kg_search should explain, not fail the call.
        let (_base, _ra, _rb, mut server) = two_project_fixture();
        let (text, is_err) = tool(&mut server, "kg_search", json!({"query": "anything"}));
        assert!(!is_err, "{text}");
        assert!(text.contains("federatedKg"), "{text}");
    }

    #[test]
    fn list_projects_enriches_only_when_kg_routing_is_on() {
        let mut fx = fixture(true);
        let (text_off, is_err) = tool(&mut fx.server, "list_projects", json!({}));
        assert!(!is_err);
        assert!(!text_off.contains("search ready"), "{text_off}");

        let mut settings = AppSettings::default();
        settings.features.insert("workspace".into(), true.into());
        settings.features.insert("kgRouting".into(), true.into());
        settings.save(&fx.server.base_dir).unwrap();

        let (text_on, is_err) = tool(&mut fx.server, "list_projects", json!({}));
        assert!(!is_err, "{text_on}");
        assert!(text_on.contains("search ready"), "{text_on}");
    }

    // --- ken-memory (task 3.2) ---

    /// `workspace`+`kenMemory` on, one registered workspace (`last_workspace`
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
        settings.features.insert("kenMemory".into(), true.into());
        settings.save(base.path()).unwrap();

        let server = Server { base_dir: base.path().to_path_buf(), scoped: None, ..Default::default() };
        (base, ws_parent, proj_root, server)
    }

    #[test]
    fn memory_tools_absent_and_erroring_when_flag_off() {
        let mut fx = fixture(true); // default settings — kenMemory unset, off
        let reply = call(&mut fx.server, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap();
        let names: Vec<_> = reply["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            names,
            ["search_knowledge", "read_document", "list_documents", "list_projects"],
            "flag off must be byte-identical to pre-ken-memory tool list"
        );

        // Dispatch still recognizes the tool names and explains the flag
        // rather than erroring opaquely (defense-in-depth, same posture as
        // the kgRouting tools' flag-off test above).
        for name in ["memory_write", "journal_append"] {
            let (text, is_err) = tool(&mut fx.server, name, json!({}));
            assert!(is_err, "{name}: {text}");
            assert!(text.contains("kenMemory"), "{name}: {text}");
        }
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
        // kenMemory on, but no workspace was ever registered/opened — no
        // `last_workspace` to resolve, so both tools must fail with a clear
        // message instead of panicking on a missing registry entry.
        let base = tempfile::tempdir().unwrap();
        let mut settings = AppSettings::default();
        settings.features.insert("workspace".into(), true.into());
        settings.features.insert("kenMemory".into(), true.into());
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

    // --- ken-tasks (task 3.2) ---

    /// `workspace`+`kenTasks` on, one registered workspace (`last_workspace`
    /// resolved) and one registered project ("Atlas") — mirrors
    /// `memory_fixture` for the task tools. `ken_memory` additionally turns
    /// on `kenMemory` so the complete→journal flow can be exercised.
    fn task_fixture(ken_memory: bool) -> (tempfile::TempDir, tempfile::TempDir, tempfile::TempDir, Server) {
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
        settings.features.insert("kenTasks".into(), true.into());
        if ken_memory {
            settings.features.insert("kenMemory".into(), true.into());
        }
        settings.save(base.path()).unwrap();

        let server = Server { base_dir: base.path().to_path_buf(), scoped: None, ..Default::default() };
        (base, ws_parent, proj_root, server)
    }

    #[test]
    fn task_tools_absent_and_erroring_when_flag_off() {
        let mut fx = fixture(true); // default settings — kenTasks unset, off
        let reply = call(&mut fx.server, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap();
        let names: Vec<_> = reply["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            names,
            ["search_knowledge", "read_document", "list_documents", "list_projects"],
            "flag off must be byte-identical to pre-ken-tasks tool list"
        );

        // Dispatch still recognizes the tool names and explains the flag
        // rather than erroring opaquely (defense-in-depth, same posture as
        // the memory tools' flag-off test above).
        for name in ["task_create", "task_list", "task_update", "task_complete"] {
            let (text, is_err) = tool(&mut fx.server, name, json!({}));
            assert!(is_err, "{name}: {text}");
            assert!(text.contains("kenTasks"), "{name}: {text}");
        }
    }

    #[test]
    fn task_tools_appear_with_valid_schemas_when_flag_on() {
        let (_base, _ws, _proj, mut server) = task_fixture(false);
        let reply = call(&mut server, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap();
        let tools = reply["result"]["tools"].as_array().unwrap();
        let names: Vec<_> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        for n in ["task_create", "task_list", "task_update", "task_complete"] {
            assert!(names.contains(&n), "{names:?}");
        }
        let tc = tools.iter().find(|t| t["name"] == "task_create").unwrap();
        assert_eq!(tc["inputSchema"]["required"], json!(["title"]));
        let tl = tools.iter().find(|t| t["name"] == "task_list").unwrap();
        assert_eq!(tl["inputSchema"]["type"], "object");
        assert!(tl["inputSchema"]["properties"]["filter"]["properties"]["goal"].is_object());
        let tu = tools.iter().find(|t| t["name"] == "task_update").unwrap();
        assert_eq!(tu["inputSchema"]["required"], json!(["id"]));
        let tcm = tools.iter().find(|t| t["name"] == "task_complete").unwrap();
        assert_eq!(tcm["inputSchema"]["required"], json!(["id", "report"]));
        for t in tools {
            assert_eq!(t["inputSchema"]["type"], "object", "schema for {}", t["name"]);
            assert!(t["description"].as_str().is_some_and(|d| !d.is_empty()), "{}", t["name"]);
        }
        // Tool descriptions must document the claim convention and the
        // complete→journal flow (spec: "Tool descriptions SHALL state the
        // claim convention").
        assert!(tu["description"].as_str().unwrap().contains("unclaimed"));
        assert!(tu["description"].as_str().unwrap().contains("doing"));
        assert!(tcm["description"].as_str().unwrap().contains("kenMemory"));
        assert!(tcm["description"].as_str().unwrap().contains("Log"));
    }

    #[test]
    fn task_create_files_into_workspace_and_project_homes_and_is_findable_across_homes() {
        let (_base, ws_parent, proj_root, mut server) = task_fixture(false);

        // Workspace-home task (default `home`).
        let (t1, e1) = tool(&mut server, "task_create", json!({"title": "Workspace task"}));
        assert!(!e1, "{t1}");

        // Per-repo task, filed via `home: "Atlas"` — a registered project
        // name, not a home-kind literal.
        let (t2, e2) = tool(&mut server, "task_create", json!({"title": "Atlas task", "home": "Atlas"}));
        assert!(!e2, "{t2}");

        let ws_dir = ws_parent.path().join(".ken-workspace/tasks");
        assert_eq!(std::fs::read_dir(&ws_dir).unwrap().flatten().count(), 1, "workspace home");
        let proj_dir = proj_root.path().join(".ken/tasks");
        let proj_entries: Vec<_> = std::fs::read_dir(&proj_dir).unwrap().flatten().collect();
        assert_eq!(proj_entries.len(), 1, "Atlas task must land in the project's own .ken/tasks/");
        let proj_path = proj_entries[0].path();
        let proj_raw = std::fs::read_to_string(&proj_path).unwrap();
        assert!(proj_raw.contains("project: Atlas") || proj_raw.contains("project: 'Atlas'"), "{proj_raw}");
        let atlas_id = proj_raw
            .lines()
            .find(|l| l.starts_with("id:"))
            .unwrap()
            .trim_start_matches("id:")
            .trim()
            .trim_matches(|c| c == '\'' || c == '"')
            .to_string();

        // task_list must see both homes merged into one list.
        let (list_text, list_err) = tool(&mut server, "task_list", json!({}));
        assert!(!list_err, "{list_text}");
        assert!(list_text.contains("Workspace task"), "{list_text}");
        assert!(list_text.contains("Atlas task"), "{list_text}");

        // task_update must find the project-home task by id — the board
        // scans the workspace home AND every registered project's home.
        let (upd_text, upd_err) =
            tool(&mut server, "task_update", json!({"id": atlas_id, "patch": {"assignee": "dev"}}));
        assert!(!upd_err, "{upd_text}");
        assert!(std::fs::read_to_string(&proj_path).unwrap().contains("assignee: dev"));

        // task_complete must find it there too.
        let (cmp_text, cmp_err) = tool(
            &mut server,
            "task_complete",
            json!({"id": atlas_id, "report": "Done via cross-home lookup."}),
        );
        assert!(!cmp_err, "{cmp_text}");
        let done = std::fs::read_to_string(&proj_path).unwrap();
        assert!(done.contains("status: done"), "{done}");
        assert!(done.contains("Done via cross-home lookup."), "{done}");
    }

    #[test]
    fn task_list_filters_by_status_kind_and_unassigned() {
        let (_base, _ws, _proj, mut server) = task_fixture(false);
        let (c1, e1) = tool(&mut server, "task_create", json!({"title": "AI task one", "fields": {"kind": "ai"}}));
        assert!(!e1, "{c1}");
        let (c2, e2) = tool(&mut server, "task_create", json!({"title": "Human task", "fields": {"kind": "human"}}));
        assert!(!e2, "{c2}");

        let (text, is_err) =
            tool(&mut server, "task_list", json!({"filter": {"kind": "ai", "assignee": "none"}}));
        assert!(!is_err, "{text}");
        assert!(text.contains("AI task one"), "{text}");
        assert!(!text.contains("Human task"), "{text}");

        let (text2, is_err2) = tool(&mut server, "task_list", json!({"filter": {"status": "backlog"}}));
        assert!(!is_err2, "{text2}");
        assert!(text2.contains("AI task one") && text2.contains("Human task"), "{text2}");

        let (text3, is_err3) = tool(&mut server, "task_list", json!({"filter": {"status": "bogus"}}));
        assert!(is_err3, "{text3}");
        assert!(text3.contains("invalid"), "{text3}");
    }

    #[test]
    fn task_update_claim_touches_only_assignee_status_updated() {
        let (_base, ws_parent, _proj, mut server) = task_fixture(false);
        let (text, is_err) = tool(
            &mut server,
            "task_create",
            json!({
                "title": "Investigate mob spawner",
                "body": "Deep dive into the decompiled spawner logic.",
                "fields": {"tags": ["mobs", "spawner"]}
            }),
        );
        assert!(!is_err, "{text}");

        let tasks_dir = ws_parent.path().join(".ken-workspace/tasks");
        let entries: Vec<_> = std::fs::read_dir(&tasks_dir).unwrap().flatten().collect();
        assert_eq!(entries.len(), 1, "expected exactly one task file");
        let path = entries[0].path();

        // Hand-add an unknown key inside the frontmatter to prove it
        // survives the patch untouched too (S6's byte-fidelity contract:
        // "patches SHALL rewrite only the named keys plus updated").
        let created = std::fs::read_to_string(&path).unwrap();
        let before = created.replacen("kind: human\n", "kind: human\ncustom_field: keep-me\n", 1);
        assert_ne!(before, created, "fixture assumption 'kind: human\\n' not found in:\n{created}");
        std::fs::write(&path, &before).unwrap();

        let id = before
            .lines()
            .find(|l| l.starts_with("id:"))
            .unwrap()
            .trim_start_matches("id:")
            .trim()
            .trim_matches(|c| c == '\'' || c == '"')
            .to_string();

        // The claim convention (spec/D4): set `assignee` + `status: doing`
        // in one call after confirming the task is unclaimed.
        let (text2, is_err2) = tool(
            &mut server,
            "task_update",
            json!({"id": id, "patch": {"assignee": "agent-desktop", "status": "doing"}}),
        );
        assert!(!is_err2, "{text2}");

        let after = std::fs::read_to_string(&path).unwrap();
        let before_lines: Vec<&str> = before.lines().collect();
        let after_lines: Vec<&str> = after.lines().collect();
        assert_eq!(
            before_lines.len(),
            after_lines.len(),
            "line count changed:\nbefore:\n{before}\nafter:\n{after}"
        );

        let mut changed_keys: Vec<String> = before_lines
            .iter()
            .zip(after_lines.iter())
            .filter(|(b, a)| b != a)
            .map(|(_, a)| a.split(':').next().unwrap_or("").trim().to_string())
            .collect();
        changed_keys.sort();
        changed_keys.dedup();
        // `updated` is always rewritten by `apply_patch`, but when the
        // patch lands on the same UTC calendar day as creation the emitted
        // line is byte-identical to what was already there, so it may or
        // may not show up as a *diff* — only `assignee` and `status`
        // (the claimed keys) are guaranteed to visibly change. Either way,
        // nothing outside {assignee, status, updated} may appear.
        let allowed: std::collections::HashSet<&str> = ["assignee", "status", "updated"].into_iter().collect();
        assert!(
            changed_keys.iter().all(|k| allowed.contains(k.as_str())),
            "diff touched more than assignee/status/updated ({changed_keys:?}):\nbefore:\n{before}\nafter:\n{after}"
        );
        assert!(
            changed_keys.contains(&"assignee".to_string()) && changed_keys.contains(&"status".to_string()),
            "expected assignee and status to change ({changed_keys:?})"
        );
        assert!(after.contains("custom_field: keep-me"), "{after}");
        assert!(after.contains("status: doing"), "{after}");
        assert!(after.contains("assignee: agent-desktop"), "{after}");
    }

    #[test]
    fn task_complete_appends_log_and_journal_summary() {
        let (_base, ws_parent, _proj, mut server) = task_fixture(true);
        let (text, is_err) =
            tool(&mut server, "task_create", json!({"title": "Audit loot tables", "fields": {"kind": "ai"}}));
        assert!(!is_err, "{text}");

        let tasks_dir = ws_parent.path().join(".ken-workspace/tasks");
        let entries: Vec<_> = std::fs::read_dir(&tasks_dir).unwrap().flatten().collect();
        assert_eq!(entries.len(), 1);
        let path = entries[0].path();
        let raw0 = std::fs::read_to_string(&path).unwrap();
        let id = raw0
            .lines()
            .find(|l| l.starts_with("id:"))
            .unwrap()
            .trim_start_matches("id:")
            .trim()
            .trim_matches(|c| c == '\'' || c == '"')
            .to_string();

        let (text2, is_err2) = tool(
            &mut server,
            "task_complete",
            json!({"id": id, "report": "Loot tables match the decompiled drop weights."}),
        );
        assert!(!is_err2, "{text2}");
        assert!(text2.contains("Journal summary recorded"), "{text2}");

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("status: done"), "{raw}");
        assert!(raw.contains("## Log"), "{raw}");
        assert!(raw.contains("Loot tables match the decompiled drop weights."), "{raw}");

        let journal_dir = ws_parent.path().join(".ken-workspace/journal");
        let jentries: Vec<_> = std::fs::read_dir(&journal_dir).unwrap().flatten().collect();
        assert_eq!(jentries.len(), 1, "expected exactly one journal file, today's");
        let jraw = std::fs::read_to_string(jentries[0].path()).unwrap();
        assert!(jraw.contains("Completed task \"Audit loot tables\""), "{jraw}");
        assert!(jraw.contains("ken://workspace/tasks/"), "{jraw}");

        // With kenMemory off, task_complete still succeeds but writes no
        // journal entry and says nothing was recorded.
        let (_base2, ws_parent2, _proj2, mut server2) = task_fixture(false);
        let (text3, is_err3) =
            tool(&mut server2, "task_create", json!({"title": "No journal task"}));
        assert!(!is_err3, "{text3}");
        let tasks_dir2 = ws_parent2.path().join(".ken-workspace/tasks");
        let path2 = std::fs::read_dir(&tasks_dir2).unwrap().flatten().next().unwrap().path();
        let raw2 = std::fs::read_to_string(&path2).unwrap();
        let id2 = raw2
            .lines()
            .find(|l| l.starts_with("id:"))
            .unwrap()
            .trim_start_matches("id:")
            .trim()
            .trim_matches(|c| c == '\'' || c == '"')
            .to_string();
        let (text4, is_err4) =
            tool(&mut server2, "task_complete", json!({"id": id2, "report": "No journal expected."}));
        assert!(!is_err4, "{text4}");
        assert!(!text4.contains("Journal summary recorded"), "{text4}");
        let journal_dir2 = ws_parent2.path().join(".ken-workspace/journal");
        assert!(!journal_dir2.exists(), "kenMemory off must write no journal file");
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

    /// A server with `kenFamilies` on but no family connections on disk —
    /// enough for the flag-off/flag-on tool-list tests, which never touch
    /// `families/`.
    fn family_flag_fixture() -> (tempfile::TempDir, Server) {
        let base = tempfile::tempdir().unwrap();
        let mut settings = AppSettings::default();
        settings.features.insert("kenFamilies".into(), true.into());
        settings.save(base.path()).unwrap();
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
        }
        registry.save(base.path()).unwrap();

        let mut settings = AppSettings::default();
        settings.features.insert("kenFamilies".into(), true.into());
        if with_tasks_workspace {
            settings.features.insert("workspace".into(), true.into());
            settings.features.insert("kenTasks".into(), true.into());
        }
        settings.save(base.path()).unwrap();

        let server = Server { base_dir: base.path().to_path_buf(), scoped: None, ..Default::default() };
        Some((base, remote_dir, ws_parent, server, family_id, clone_root))
    }

    #[test]
    fn family_tools_absent_and_erroring_when_flag_off() {
        let mut fx = fixture(true); // default settings — kenFamilies unset, off
        let reply = call(&mut fx.server, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap();
        let names: Vec<_> = reply["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            names,
            ["search_knowledge", "read_document", "list_documents", "list_projects"],
            "flag off must be byte-identical to pre-ken-families tool list"
        );

        for name in ["family_list", "family_inbox", "family_send"] {
            let (text, is_err) = tool(&mut fx.server, name, json!({}));
            assert!(is_err, "{name}: {text}");
            assert!(text.contains("kenFamilies"), "{name}: {text}");
        }
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

        // task_list must show both boards (visibility for the whole team).
        let (list_text, list_err) = tool(&mut server, "task_list", json!({}));
        assert!(!list_err, "{list_text}");
        assert!(list_text.contains("Sarah's task"), "{list_text}");
        assert!(list_text.contains("Owner's task"), "{list_text}");
        assert!(list_text.contains(&format!("ken://{family_id}/members/sarah/board/")), "{list_text}");

        // A claim aimed at sarah's board, acting as owner, is refused by
        // the lane rules — not merely discouraged: the file on disk must be
        // byte-for-byte untouched afterward.
        let sarah_path = clone_root.join("members/sarah/board/01SARAHTASK-task.md");
        let before = std::fs::read_to_string(&sarah_path).unwrap();
        let (bad_text, bad_err) = tool(
            &mut server,
            "task_update",
            json!({"id": "01SARAHTASK", "patch": {"assignee": "owner", "status": "doing"}, "as": "owner"}),
        );
        assert!(bad_err, "{bad_text}");
        assert!(bad_text.contains("refused"), "{bad_text}");
        let after = std::fs::read_to_string(&sarah_path).unwrap();
        assert_eq!(before, after, "a lane-refused write must not touch the file");

        // The same call against owner's own board succeeds, patches only
        // the given keys, and pushes.
        let (ok_text, ok_err) = tool(
            &mut server,
            "task_update",
            json!({"id": "01OWNERTASK", "patch": {"assignee": "owner", "status": "doing"}, "as": "owner"}),
        );
        assert!(!ok_err, "{ok_text}");
        let owner_path = clone_root.join("members/owner/board/01OWNERTASK-task.md");
        let owner_raw = std::fs::read_to_string(&owner_path).unwrap();
        assert!(owner_raw.contains("status: doing"), "{owner_raw}");
        assert!(owner_raw.contains("title: \"Owner's task\"") || owner_raw.contains("title: Owner's task"), "{owner_raw}");
    }

    // --- ken-pipeline (task 3.9) ---

    /// A small pipeline definition covering everything section 3's tests
    /// need: a plain holding lane (`todo`), two confirm-gated agent lanes
    /// with a bounce edge between them (`doing` <-> `review`), an `auto`
    /// lane under a pipeline whose master switch is on (`autolane`), a
    /// terminal lane (`done`), and the one `blocked: true` lane every
    /// pipeline needs. Lane ids `todo`/`doing`/`review`/`done` deliberately
    /// match `TaskStatus`'s own keywords so `tasks::create_task`'s typed
    /// `fields.status` can seed a ticket directly into one of them; `bounce_
    /// cap: 2` keeps the retry-cap test's setup short.
    // A raw string, not a `\n`-joined literal: Rust's backslash-newline
    // line-continuation strips the *leading whitespace* of the following
    // line too, which silently flattens YAML's indentation-sensitive
    // `lanes:` list into something `serde_yaml` can't parse as a sequence
    // of mappings (found the hard way — an earlier `"...\n\` version of
    // this constant parsed to a pipeline with zero lanes).
    const TEST_PIPELINE_MD: &str = r#"---
id: default
name: Test pipeline
auto: true
concurrency_cap: 1
bounce_cap: 2
lanes:
  - id: todo
    name: To Do
    maps_to: todo
    agent: none
    kickoff: manual
    on_pass: doing
  - id: doing
    name: Doing
    maps_to: doing
    agent: programmer
    model: sonnet
    kickoff: confirm
    on_pass: review
    on_fail: doing
  - id: review
    name: Review
    maps_to: review
    agent: tester
    model: sonnet
    kickoff: confirm
    on_pass: done
    on_fail: doing
  - id: autolane
    name: Auto lane
    maps_to: doing
    agent: programmer
    model: sonnet
    kickoff: auto
    on_pass: done
  - id: done
    name: Done
    maps_to: done
    agent: none
    kickoff: manual
    terminal: true
  - id: blocked
    name: Blocked
    maps_to: doing
    agent: none
    kickoff: manual
    blocked: true
---
Test pipeline body.
"#;

    /// `workspace`+`kenTasks`+`kenPipeline` on (plus `kenMemory` when
    /// asked), one registered workspace, `TEST_PIPELINE_MD` written to
    /// `.ken-workspace/pipelines/default.md`. Mirrors `task_fixture` above.
    fn pipeline_fixture(ken_memory: bool) -> (tempfile::TempDir, tempfile::TempDir, PathBuf, Server) {
        let base = tempfile::tempdir().unwrap();
        let ws_parent = tempfile::tempdir().unwrap();
        let workspace = ken_core::workspace::Workspace::create(ws_parent.path(), "WS", &[]).unwrap();
        let ws_root = workspace.root.clone();

        let mut registry = Registry::default();
        registry.add_workspace(&workspace, None, 0);
        registry.last_workspace = Some(workspace.config.id);
        registry.save(base.path()).unwrap();

        let mut settings = AppSettings::default();
        settings.features.insert("workspace".into(), true.into());
        settings.features.insert("kenTasks".into(), true.into());
        settings.features.insert("kenPipeline".into(), true.into());
        if ken_memory {
            settings.features.insert("kenMemory".into(), true.into());
        }
        settings.save(base.path()).unwrap();

        let pdir = pipeline::pipelines_dir(&ws_root);
        std::fs::create_dir_all(&pdir).unwrap();
        std::fs::write(pdir.join("default.md"), TEST_PIPELINE_MD).unwrap();

        let server = Server { base_dir: base.path().to_path_buf(), scoped: None, ..Default::default() };
        (base, ws_parent, ws_root, server)
    }

    /// Hand-write a pipeline ticket directly (bypassing `tasks::create_task`
    /// — needed for tests that seed an arbitrary lane id, `blocked_by`, or a
    /// large number of tickets fast). `id` must be exactly 26 chars for
    /// anything exercising `blocked_by` (`pipeline::is_ulid_like`).
    fn write_pipeline_ticket(ws_root: &Path, id: &str, title: &str, lane: &str, extra: &[(&str, &str)]) -> PathBuf {
        let dir = tasks::workspace_tasks_dir(ws_root);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{id}-{}.md", tasks::slugify(title)));
        let mut fm = format!(
            "---\nid: {id}\ntitle: \"{title}\"\nstatus: {lane}\nkind: ai\nassignee: ''\nproject: TestProj\nboard: main\ncreated: 2026-01-01\nupdated: 2026-01-01\npipeline: default\n"
        );
        for (k, v) in extra {
            fm.push_str(&format!("{k}: {v}\n"));
        }
        fm.push_str("---\n\nBody text.\n");
        std::fs::write(&path, &fm).unwrap();
        path
    }

    /// A 26-char, all-digit (therefore Crockford-valid) test ticket id —
    /// digits are always in the Crockford alphabet, so this never collides
    /// with `pipeline::is_ulid_like`'s path/malformed checks.
    fn test_id(n: u32) -> String {
        format!("{n:026}")
    }

    #[test]
    fn pipeline_tools_absent_and_erroring_when_flag_off() {
        // kenTasks on, kenPipeline left at its registered default (off).
        let (_base, _ws, _proj, mut server) = task_fixture(false);
        let reply = call(&mut server, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap();
        let names: Vec<_> = reply["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        for n in [
            "pipeline_list",
            "pipeline_get",
            "pipeline_claim",
            "pipeline_advance",
            "pipeline_block",
            "pipeline_runs",
            "pipeline_digest",
        ] {
            assert!(!names.contains(&n.to_string()), "{names:?} must not list {n} when kenPipeline is off");
        }
        // task_* tools stay listed — kenPipeline off must not regress
        // kenTasks (byte-identical tool list otherwise).
        assert!(names.contains(&"task_create".to_string()));

        // Defense-in-depth: dispatch still recognizes the tool names and
        // explains the flag rather than erroring opaquely.
        for name in [
            "pipeline_list",
            "pipeline_get",
            "pipeline_claim",
            "pipeline_advance",
            "pipeline_block",
            "pipeline_runs",
            "pipeline_digest",
        ] {
            let (text, is_err) = tool(&mut server, name, json!({}));
            assert!(is_err, "{name}: {text}");
            assert!(text.contains("kenPipeline"), "{name}: {text}");
        }
    }

    #[test]
    fn pipeline_tools_appear_with_valid_schemas_when_flag_on() {
        let (_base, _ws, _root, mut server) = pipeline_fixture(false);
        let reply = call(&mut server, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap();
        let tools = reply["result"]["tools"].as_array().unwrap();
        let names: Vec<_> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        for n in [
            "pipeline_list",
            "pipeline_get",
            "pipeline_claim",
            "pipeline_advance",
            "pipeline_block",
            "pipeline_runs",
            "pipeline_digest",
        ] {
            assert!(names.contains(&n), "{names:?}");
        }
        for t in tools {
            assert_eq!(t["inputSchema"]["type"], "object", "schema for {}", t["name"]);
            assert!(t["description"].as_str().is_some_and(|d| !d.is_empty()), "{}", t["name"]);
        }
        let get = tools.iter().find(|t| t["name"] == "pipeline_get").unwrap();
        assert_eq!(get["inputSchema"]["required"], json!(["id"]));
        let claim = tools.iter().find(|t| t["name"] == "pipeline_claim").unwrap();
        assert_eq!(claim["inputSchema"]["required"], json!(["id", "agent"]));
        let advance = tools.iter().find(|t| t["name"] == "pipeline_advance").unwrap();
        assert_eq!(advance["inputSchema"]["required"], json!(["id", "outcome", "report"]));
        assert_eq!(advance["inputSchema"]["properties"]["outcome"]["enum"], json!(["pass", "fail"]));
        let block = tools.iter().find(|t| t["name"] == "pipeline_block").unwrap();
        assert_eq!(block["inputSchema"]["required"], json!(["id"]));
    }

    #[test]
    fn pipeline_claim_refused_when_blocked_even_in_auto_lane() {
        let (_base, _ws, ws_root, mut server) = pipeline_fixture(false);
        // `autolane` has kickoff: auto and the pipeline's master switch is
        // on — the strongest case D5 asks for: blocked beats an auto lane.
        write_pipeline_ticket(
            &ws_root,
            &test_id(1),
            "Blocked in an auto lane",
            "autolane",
            &[("scope", "['src/**']"), ("verify", "'cargo test'"), ("block_reason", "'waiting on vendor'")],
        );
        let (text, is_err) = tool(&mut server, "pipeline_claim", json!({"id": test_id(1), "agent": "agent-a"}));
        assert!(is_err, "{text}");
        assert!(text.to_lowercase().contains("blocked"), "{text}");

        // No run record was created at all.
        let runs = run_ledger_scan(&ws_root);
        assert!(runs.is_empty(), "a refused claim must create no run record: {runs:?}");
    }

    #[test]
    fn pipeline_claim_refused_when_over_cap() {
        let (_base, _ws, ws_root, mut server) = pipeline_fixture(false);
        // Occupy the single concurrency slot with a running run for a
        // ticket that isn't the one under test.
        let occupied = pipeline::RunRecord {
            id: "01OCCUPIEDRUNRUNRUNRUNRUN".to_string(),
            ticket: "some-other-ticket".to_string(),
            pipeline: "default".to_string(),
            lane: "doing".to_string(),
            agent: "someone-else".to_string(),
            model: "sonnet".to_string(),
            scope: vec!["src/**".to_string()],
            verify: "cargo test".to_string(),
            started: "2026-01-01T00:00:00Z".to_string(),
            ended: String::new(),
            outcome: Some(pipeline::RunOutcome::Running),
            outcome_raw: "running".to_string(),
            artifacts: Vec::new(),
            report: String::new(),
        };
        let month_dir = pipeline::run_month_dir(&ws_root, "2026-01");
        std::fs::create_dir_all(&month_dir).unwrap();
        std::fs::write(pipeline::run_path(&ws_root, "2026-01", &occupied.id), pipeline::compose_run(&occupied)).unwrap();

        write_pipeline_ticket(
            &ws_root,
            &test_id(2),
            "Wants the one slot",
            "doing",
            &[("scope", "['src/**']"), ("verify", "'cargo test'")],
        );
        let (text, is_err) = tool(&mut server, "pipeline_claim", json!({"id": test_id(2), "agent": "agent-b"}));
        assert!(is_err, "{text}");
        assert!(text.to_lowercase().contains("cap"), "{text}");
        // The claim attempt records the ticket as queued rather than
        // silently doing nothing — "the run stays queued" (spec scenario).
        let runs = run_ledger_scan(&ws_root);
        assert!(
            runs.iter().any(|r| r.ticket == test_id(2) && r.outcome == Some(pipeline::RunOutcome::Queued)),
            "{runs:?}"
        );
        assert!(
            !runs.iter().any(|r| r.ticket == test_id(2) && r.outcome == Some(pipeline::RunOutcome::Running)),
            "an over-cap claim must never write a running record: {runs:?}"
        );
    }

    #[test]
    fn pipeline_claim_writes_running_then_advance_closes_with_exact_frontmatter_diff() {
        let (_base, _ws, ws_root, mut server) = pipeline_fixture(false);
        let path = write_pipeline_ticket(
            &ws_root,
            &test_id(3),
            "Do the work",
            "doing",
            &[("scope", "['src/**']"), ("verify", "'cargo test'"), ("hand_added_key", "'preserve me'")],
        );
        let before = std::fs::read_to_string(&path).unwrap();

        let (claim_text, claim_err) = tool(&mut server, "pipeline_claim", json!({"id": test_id(3), "agent": "agent-c", "model": "opus"}));
        assert!(!claim_err, "{claim_text}");
        assert!(claim_text.contains(&test_id(3)), "{claim_text}");

        let runs = run_ledger_scan(&ws_root);
        let running: Vec<_> = runs.iter().filter(|r| r.ticket == test_id(3)).collect();
        assert_eq!(running.len(), 1, "{runs:?}");
        assert_eq!(running[0].outcome, Some(pipeline::RunOutcome::Running));
        assert_eq!(running[0].agent, "agent-c");
        assert_eq!(running[0].model, "opus");

        // Claiming again while already running is refused, not duplicated.
        let (text2, is_err2) = tool(&mut server, "pipeline_claim", json!({"id": test_id(3), "agent": "agent-d"}));
        assert!(is_err2, "{text2}");
        assert!(text2.to_lowercase().contains("already"), "{text2}");

        // Ticket file itself is untouched by the claim (claim only writes
        // the run ledger) — the intended-keys-only assertion is about
        // pipeline_advance below.
        let after_claim = std::fs::read_to_string(&path).unwrap();
        assert_eq!(before, after_claim, "pipeline_claim must not touch the ticket file");

        // Now report pass — advances doing -> review, closes the run.
        let (advance_text, advance_err) =
            tool(&mut server, "pipeline_advance", json!({"id": test_id(3), "outcome": "pass", "report": "Implemented and verified."}));
        assert!(!advance_err, "{advance_text}");
        assert!(advance_text.contains("review"), "{advance_text}");

        let after_advance = std::fs::read_to_string(&path).unwrap();
        assert!(after_advance.contains("status: review"), "{after_advance}");
        assert!(after_advance.contains("hand_added_key"), "unrelated hand-added key must survive: {after_advance}");
        assert!(after_advance.contains("Do the work"), "{after_advance}");
        // Only status/updated/## Log changed — bounces is untouched
        // because this was a forward move, and scope/verify/hand_added_key
        // are byte-identical to before.
        assert!(after_advance.contains("scope:"), "{after_advance}");
        assert!(after_advance.contains("verify:"), "{after_advance}");
        assert!(after_advance.contains("## Log"), "{after_advance}");
        // The transition line lands on the ticket's ## Log; the agent's
        // full report is the closed run's body, not the ticket's — asserted
        // via `closed[0].report` below.
        assert!(after_advance.contains("doing → review (pass)"), "{after_advance}");

        let runs_after = run_ledger_scan(&ws_root);
        let closed: Vec<_> = runs_after.iter().filter(|r| r.ticket == test_id(3)).collect();
        assert_eq!(closed.len(), 1, "{runs_after:?}");
        assert_eq!(closed[0].outcome, Some(pipeline::RunOutcome::Pass));
        assert!(closed[0].report.contains("Implemented and verified."), "{:?}", closed[0].report);
    }

    #[test]
    fn pipeline_block_refuses_cycle_and_leaves_both_files_unchanged() {
        let (_base, _ws, ws_root, mut server) = pipeline_fixture(false);
        let a = test_id(10);
        let b = test_id(11);
        let path_a = write_pipeline_ticket(&ws_root, &a, "Ticket A", "doing", &[]);
        let path_b = write_pipeline_ticket(&ws_root, &b, "Ticket B", "doing", &[]);

        // A is blocked by B.
        let (block_text, block_err) = tool(&mut server, "pipeline_block", json!({"id": a, "blockedBy": [b.clone()]}));
        assert!(!block_err, "{block_text}");

        let before_a = std::fs::read_to_string(&path_a).unwrap();
        let before_b = std::fs::read_to_string(&path_b).unwrap();

        // Now try to block B by A — closes the cycle, must be refused with
        // neither file touched.
        let (cycle_text, cycle_err) = tool(&mut server, "pipeline_block", json!({"id": b, "blockedBy": [a.clone()]}));
        assert!(cycle_err, "{cycle_text}");
        assert!(cycle_text.to_lowercase().contains("cycle"), "{cycle_text}");

        let after_a = std::fs::read_to_string(&path_a).unwrap();
        let after_b = std::fs::read_to_string(&path_b).unwrap();
        assert_eq!(before_a, after_a, "ticket A must be unchanged by a refused block");
        assert_eq!(before_b, after_b, "ticket B must be unchanged by a refused block");

        // A blocked ticket is refused by pipeline_claim, with no exception.
        let (claim_text, claim_err) = tool(&mut server, "pipeline_claim", json!({"id": a, "agent": "agent-e"}));
        assert!(claim_err, "{claim_text}");
        assert!(claim_text.to_lowercase().contains("blocked"), "{claim_text}");
    }

    #[test]
    fn pipeline_get_never_shown_in_list_and_list_never_leaks_a_body() {
        let (_base, _ws, ws_root, mut server) = pipeline_fixture(false);
        write_pipeline_ticket(
            &ws_root,
            &test_id(20),
            "Has a distinctive body",
            "doing",
            &[("scope", "['src/**']"), ("verify", "'cargo test'")],
        );
        // Overwrite the body with a distinctive marker `pipeline_list`
        // must never leak.
        let path = tasks::workspace_tasks_dir(&ws_root).join(format!("{}-has-a-distinctive-body.md", test_id(20)));
        let raw = std::fs::read_to_string(&path).unwrap();
        let raw = raw.replace("Body text.", "SECRET-BODY-MARKER-not-for-pipeline-list");
        std::fs::write(&path, raw).unwrap();

        let (list_text, list_err) = tool(&mut server, "pipeline_list", json!({}));
        assert!(!list_err, "{list_text}");
        assert!(!list_text.contains("SECRET-BODY-MARKER"), "{list_text}");
        assert!(list_text.contains(&test_id(20)), "{list_text}");

        let (get_text, get_err) = tool(&mut server, "pipeline_get", json!({"id": test_id(20)}));
        assert!(!get_err, "{get_text}");
        assert!(get_text.contains("SECRET-BODY-MARKER"), "{get_text}");
    }

    #[test]
    fn pipeline_list_pages_a_1000_ticket_lane_exactly_once_never_exceeding_hard_max() {
        let (_base, _ws, ws_root, mut server) = pipeline_fixture(false);
        for n in 0..1000u32 {
            write_pipeline_ticket(&ws_root, &test_id(n), &format!("Ticket {n}"), "todo", &[]);
        }

        // No limit given: default page (<=20), well under the hard max.
        let (first_text, first_err) = tool(&mut server, "pipeline_list", json!({}));
        assert!(!first_err, "{first_text}");
        assert!(first_text.contains("1000 tickets match"), "{first_text}");
        assert!(first_text.contains("cursor:"), "{first_text}");

        // A `limit` above the hard max is clamped, never exceeded.
        let (big_text, big_err) = tool(&mut server, "pipeline_list", json!({"limit": 100000}));
        assert!(!big_err, "{big_text}");
        let row_count = big_text.lines().filter(|l| l.starts_with(&test_id(0)[0..2])).count();
        assert!(row_count <= 100, "hard max exceeded: {row_count} rows");

        // Page through with limit=100 until the cursor disappears,
        // collecting every id — must cover exactly 1000, no dupes.
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut args = json!({"limit": 100});
            if let Some(c) = &cursor {
                args["cursor"] = json!(c);
            }
            let (text, is_err) = tool(&mut server, "pipeline_list", args);
            assert!(!is_err, "{text}");
            for line in text.lines() {
                if line.len() >= 26 && line.as_bytes()[0].is_ascii_digit() && line.contains(" — ") {
                    let id = &line[0..26];
                    assert!(seen.insert(id.to_string()), "ticket {id} seen twice while paging");
                }
            }
            cursor = text
                .lines()
                .find_map(|l| l.strip_prefix("cursor: "))
                .map(str::to_string);
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(seen.len(), 1000, "paging must cover every ticket exactly once");
    }

    #[test]
    fn pipeline_digest_journals_when_ken_memory_is_on_and_skips_cleanly_when_off() {
        let (_base, _ws, ws_root, mut server) = pipeline_fixture(true);
        write_pipeline_ticket(&ws_root, &test_id(30), "For the digest", "todo", &[]);
        let (text, is_err) = tool(&mut server, "pipeline_digest", json!({}));
        assert!(!is_err, "{text}");
        assert!(text.contains("Pipeline digest"), "{text}");
        assert!(text.contains("journaled"), "{text}");
        let journal_dir = ws_root.join(".ken-workspace").join("journal");
        assert!(journal_dir.is_dir(), "kenMemory on must write a journal entry");

        let (_base2, _ws2, ws_root2, mut server2) = pipeline_fixture(false);
        write_pipeline_ticket(&ws_root2, &test_id(31), "For the digest, no memory", "todo", &[]);
        let (text2, is_err2) = tool(&mut server2, "pipeline_digest", json!({}));
        assert!(!is_err2, "{text2}");
        assert!(!text2.contains("journaled"), "{text2}");
        let journal_dir2 = ws_root2.join(".ken-workspace").join("journal");
        assert!(!journal_dir2.is_dir(), "kenMemory off must write no journal file: {text2}");
    }
}

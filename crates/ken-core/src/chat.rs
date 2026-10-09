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
    /// working | done | error | needs_input
    Status { chat_id: String, status: String, detail: Option<String> },
    /// The model is asking the user to choose. `payload` is the JSON contract
    /// the UI and the answer command share:
    /// `{"requestId":…,"toolUseId":…,"questions":[…]}`.
    Question { chat_id: String, payload: String },
    /// Claude wants to edit a file: an [`EditProposal`] as JSON, for the
    /// user to accept or decline change by change. The CLI waits.
    EditProposal { chat_id: String, payload: String },
    /// A piece of the reply as it streams. Not kept: the whole reply follows
    /// as a `Message`, which replaces what streamed.
    Delta { chat_id: String, text: String },
    /// Claude called a tool: a card to keep. `payload` is
    /// `{"toolUseId":…,"name":…,"summary":…,"status":"running"}`.
    Tool { chat_id: String, payload: String },
    /// A tool's result, for the card with that `tool_use_id`.
    ToolResult { chat_id: String, tool_use_id: String, is_error: bool, preview: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParsedEvent {
    Init,
    AssistantText(String),
    /// A piece of the reply as it is written (`--include-partial-messages`).
    /// The whole text follows as `AssistantText`, which replaces it.
    TextDelta(String),
    /// Claude called a tool: its id, name, and a one-line summary.
    Tool { id: String, name: String, summary: String },
    /// A tool's result came back.
    ToolDone { id: String, is_error: bool, preview: String },
    TurnResult { is_error: bool },
    /// A `can_use_tool` permission request the CLI expects an answer to.
    ControlRequest {
        request_id: String,
        tool_name: String,
        input: Value,
        tool_use_id: String,
    },
    Other,
}

/// The tool whose permission request is a user-facing question rather than a
/// permission decision.
const ASK_TOOL: &str = "AskUserQuestion";

/// Parse one stream-json stdout line into its first event. Tolerant:
/// unknown shapes → Other.
pub fn parse_event(line: &str) -> ParsedEvent {
    parse_events(line).into_iter().next().unwrap_or(ParsedEvent::Other)
}

/// Every event in one stream-json stdout line, in order: an assistant
/// message can carry text and several tool calls, and a user message
/// several tool results.
pub fn parse_events(line: &str) -> Vec<ParsedEvent> {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return vec![ParsedEvent::Other];
    };
    let blocks = || {
        v.pointer("/message/content")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let str_of = |b: &Value, k: &str| b.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    let events: Vec<ParsedEvent> = match v.get("type").and_then(Value::as_str) {
        Some("system") => vec![ParsedEvent::Init],
        Some("result") => vec![ParsedEvent::TurnResult {
            is_error: v.get("is_error").and_then(Value::as_bool).unwrap_or(false),
        }],
        Some("stream_event") => {
            // Only the main conversation streams into the transcript; a
            // subagent's partial text (parent_tool_use_id set) does not.
            let top = v.get("parent_tool_use_id").is_none_or(Value::is_null);
            let delta = v.pointer("/event/delta");
            match delta {
                Some(d) if top && d.get("type").and_then(Value::as_str) == Some("text_delta") => {
                    let t = str_of(d, "text");
                    if t.is_empty() { vec![] } else { vec![ParsedEvent::TextDelta(t)] }
                }
                _ => vec![],
            }
        }
        Some("assistant") => blocks()
            .iter()
            .filter_map(|b| match b.get("type").and_then(Value::as_str) {
                Some("text") => {
                    let t = str_of(b, "text");
                    (!t.trim().is_empty()).then_some(ParsedEvent::AssistantText(t))
                }
                // The question card renders AskUserQuestion; a tool card for
                // it would just be noise.
                Some("tool_use") if b.get("name").and_then(Value::as_str) != Some(ASK_TOOL) => {
                    Some(ParsedEvent::Tool {
                        id: str_of(b, "id"),
                        name: b.get("name").and_then(Value::as_str).unwrap_or("Tool").to_string(),
                        summary: summarize_tool(b),
                    })
                }
                _ => None,
            })
            .collect(),
        Some("user") => blocks()
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
            .map(|b| ParsedEvent::ToolDone {
                id: str_of(b, "tool_use_id"),
                is_error: b.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                preview: result_preview(b.get("content")),
            })
            .collect(),
        Some("control_request") => {
            let req = v.get("request");
            if req.and_then(|r| r.get("subtype")).and_then(Value::as_str) != Some("can_use_tool") {
                return vec![ParsedEvent::Other];
            }
            let req = req.unwrap();
            vec![ParsedEvent::ControlRequest {
                request_id: v
                    .get("request_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                tool_name: req
                    .get("tool_name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                input: req.get("input").cloned().unwrap_or(Value::Null),
                tool_use_id: req
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            }]
        }
        _ => vec![],
    };
    if events.is_empty() { vec![ParsedEvent::Other] } else { events }
}

/// The start of a tool result, for its card: text only, at most a few lines.
fn result_preview(content: Option<&Value>) -> String {
    let text = match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    let mut out: String = text.lines().take(8).collect::<Vec<_>>().join("\n");
    if out.chars().count() > 600 {
        out = out.chars().take(600).collect::<String>() + "…";
    } else if text.lines().count() > 8 {
        out.push_str("\n…");
    }
    out
}

/// "Read notes/meeting.md" — a human-readable one-liner for a tool_use block.
fn summarize_tool(block: &Value) -> String {
    let raw = block.get("name").and_then(Value::as_str).unwrap_or("Tool");
    let name = match ken_mcp_tool(raw) {
        Some(t) => format!("Ken {}", t.replace('_', " ")),
        None => raw.to_string(),
    };
    let input = block.get("input");
    let arg = input.and_then(|i| {
        ["file_path", "path", "pattern", "command", "query", "url", "notebook_path", "description"]
            .iter()
            .find_map(|k| i.get(k).and_then(Value::as_str))
    });
    match arg {
        Some(a) => {
            // By characters: a byte cut can land inside one and panic.
            let a: String = a.chars().take(80).collect();
            format!("{name} {a}")
        }
        None => name,
    }
}

/// Tools whose permission request is an edit for the user to review as a
/// diff, accepting or declining each change, instead of a write that just
/// happens.
pub const EDIT_TOOLS: [&str; 3] = ["Edit", "MultiEdit", "Write"];

/// What every chat session is told about working inside Ken, appended to
/// Claude Code's own system prompt.
pub const KEN_GUIDE: &str = "You are working inside Ken, a desktop app where a person reads and edits their team's \
wiki, docs and code. Rules for this app:\n\
1. Edits are reviewed. Every Edit, MultiEdit or Write you make is shown to the person as a diff, and they accept or \
decline each change. If they decline some, the tool result says which; read the file again before editing it further, \
and do not retry a declined change unless they ask.\n\
2. Cite your sources so they can click them. Every fact that comes from a file gets a Markdown link to that file, \
project-relative, with the line when you know it: [Save.md, line 12](Platform/Save.md#L12), or a heading: \
[Save format](Platform/Save.md#save-format). A hit from Ken's search tools that carries a ken:// address is linked \
by that address, with #L<line> when it has a line. A file in another of the workspace's repos is linked by that \
repo's ken:// address (each turn lists them), never a project-relative path; write a space in a path as %20. \
Never open files or switch the person's screen yourself; a link \
is how they go there.\n\
3. Search with Ken first. Ken's tools (mcp__ken__route_query across the workspace, mcp__ken__semantic_search in one \
project, mcp__ken__kg_search over the knowledge graph, mcp__ken__search_knowledge, mcp__ken__read_document) search \
the team's wiki, docs and code with Ken's own ranking and return ken:// addresses to cite. For code, navigate like an \
IDE: mcp__ken__find_definition to go to a symbol, mcp__ken__find_usages for every use and its callers, \
mcp__ken__file_outline to drill into a file, mcp__ken__related_files for what it imports and what imports it; then \
read the lines they point to. mcp__ken__history tells when and why something changed (a file's commits, commits \
about some words, or with neither, what changed in the last `days` across every repo, git or not). Ken's tools and \
Grep, Glob and Read cover every question about the files; do not run shell commands to look at them. \
mcp__ken__open_in_ken opens a file for the person: use it only when they explicitly ask you to open or show \
something.\n\
4. Never say something isn't there because Ken's search did not find it. Its index can miss a file, a new change or \
different wording. When Ken finds nothing, or nothing that answers, look yourself with Grep, Glob and Read, in this \
project and the workspace's other repos (you may read them all), then answer, say you found it by looking directly, \
and cite the file. Glob and Grep search only the folder you start in unless given a path: pass each repo's folder \
as the path. Only after looking may you say it isn't there, and say where you looked.\n\
5. On what the system does, the code wins: a ticket, plan or doc says what was intended, not what was built. When \
asked how something works, open the code that does it and cite that file; where the code and a doc disagree, say so. \
Several repos can hold the same product (a prototype and the build that replaced it): before saying the code does \
not do something, look in every repo of the workspace, and say which repo each finding is from.\n\
6. The person's list is Ken's, not yours. Their tasks are read with mcp__ken__task_list and their tickets with \
mcp__ken__ticket_list; add or change a task with mcp__ken__task_create and mcp__ken__task_update when they ask, and \
say what you added. Your own task or todo tools are not their list. A ## Memories block at the top of a turn holds \
the team's standing notes: follow them, and add one with mcp__ken__memory_write only when the person asks you to \
remember something.\n\
7. On what the team decided, the team's wiki wins. Who decides, what was ruled and why, the rules and conventions, \
how work moves and what something is called are in the team's wiki and decisions log: search them first and answer \
from them, citing the page. A ruling there is settled. Never recommend against it or present the question as open; \
when the code does not match a ruling, say the code differs from the ruling and cite both. Every hit from Ken's \
search carries a label (ruling D-410 · 2026-10-06, rule, current, reference, record, ticket · cancelled), and you \
weigh them by it: a later ruling over an earlier one on the same point (a hit marked superseded by a later ruling \
holds only where that ruling does not reach), a ruling or rule over a ticket or note, and a current page over a \
record.";

/// Ken's own MCP tools the chat may use without asking: they read Ken's
/// index, open a file for the person when they asked, or keep the person's
/// own list and Ken's memory when they ask (each write is dated and says who
/// made it). Sending to a teammate through the team inbox is declined in
/// chat: the person does that in Ken.
pub const KEN_MCP_ALLOWED: [&str; 21] = [
    "history",
    "find_definition",
    "find_usages",
    "file_outline",
    "related_files",
    "search_knowledge",
    "read_document",
    "list_documents",
    "list_projects",
    "kg_search",
    "semantic_search",
    "route_query",
    "task_list",
    "task_create",
    "task_update",
    "ticket_list",
    "memory_write",
    "journal_append",
    "family_list",
    "family_inbox",
    "open_in_ken",
];

/// Tools of Claude Code's own that the chat process does not get: no shell
/// (Ken's tools and Read, Grep and Glob cover the files), no web, and no task
/// lists of its own (the person's list is Ken's). Notebook edits are not
/// reviewed as diffs, so they are off too.
pub const CHAT_DISALLOWED: [&str; 10] = [
    "Bash",
    "PowerShell",
    "WebFetch",
    "WebSearch",
    "TaskCreate",
    "TaskUpdate",
    "TaskList",
    "TaskGet",
    "TodoWrite",
    "NotebookEdit",
];

/// The name Claude Code gives a tool of Ken's MCP server (`ken`).
fn ken_mcp_tool(tool: &str) -> Option<&str> {
    tool.strip_prefix("mcp__ken__")
}

/// An edit Claude asked to make, worked out against the file as it is now,
/// for the user to review: the text before and the text after.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditProposal {
    pub request_id: String,
    pub tool_use_id: String,
    pub tool: String,
    /// The file, absolute, as the tool named it.
    pub path: String,
    /// Relative to the project, when inside it.
    pub rel_path: Option<String>,
    pub base: String,
    pub proposed: String,
    /// The tool's own input, sent back unchanged when every change is accepted.
    pub input: Value,
    /// `accepted` · `declined` · `partial`, once decided.
    #[serde(default)]
    pub decision: Option<String>,
    /// What the decision told Claude, once decided.
    #[serde(default)]
    pub note: Option<String>,
}

fn replace_once(text: &str, old: &str, new: &str, all: bool) -> std::result::Result<String, String> {
    if old.is_empty() {
        return Err("the text to replace is empty".into());
    }
    let n = text.matches(old).count();
    match (n, all) {
        (0, _) => Err("the text to replace is not in the file".into()),
        (1, _) | (_, true) => Ok(if all { text.replace(old, new) } else { text.replacen(old, new, 1) }),
        (n, false) => Err(format!("the text to replace appears {n} times; it must be unique")),
    }
}

/// Work out an edit tool's result against the file on disk: the text before
/// and after. `Err` says why it cannot apply (the CLI would fail it too).
pub fn propose_edit(project_root: &Path, tool: &str, input: &Value) -> std::result::Result<(String, Option<String>, String, String), String> {
    let path = input.get("file_path").and_then(Value::as_str).ok_or("the edit names no file")?;
    let abs = if Path::new(path).is_absolute() { PathBuf::from(path) } else { project_root.join(path) };
    let rel = abs.strip_prefix(project_root).ok().map(|r| r.to_string_lossy().replace('\\', "/"));
    let base = std::fs::read_to_string(&abs).unwrap_or_default();
    // Claude Code reads and edits text with LF line ends. A file saved with
    // CRLF (most of a Windows checkout with core.autocrlf) would never match
    // a multi-line edit, so the edit is worked out on LF text and the result
    // is put back in the file's own line ends.
    let crlf = base.contains("\r\n");
    let lf = |s: &str| if crlf { s.replace("\r\n", "\n") } else { s.to_string() };
    let text = lf(&base);
    let proposed = match tool {
        "Write" => lf(input.get("content").and_then(Value::as_str).unwrap_or_default()),
        "Edit" => {
            let old = lf(input.get("old_string").and_then(Value::as_str).unwrap_or_default());
            let new = lf(input.get("new_string").and_then(Value::as_str).unwrap_or_default());
            let all = input.get("replace_all").and_then(Value::as_bool).unwrap_or(false);
            replace_once(&text, &old, &new, all)?
        }
        "MultiEdit" => {
            let mut text = text.clone();
            for e in input.get("edits").and_then(Value::as_array).cloned().unwrap_or_default() {
                let old = lf(e.get("old_string").and_then(Value::as_str).unwrap_or_default());
                let new = lf(e.get("new_string").and_then(Value::as_str).unwrap_or_default());
                let all = e.get("replace_all").and_then(Value::as_bool).unwrap_or(false);
                text = replace_once(&text, &old, &new, all)?;
            }
            text
        }
        other => return Err(format!("{other} is not an edit")),
    };
    let proposed = if crlf { proposed.replace("\n", "\r\n") } else { proposed };
    Ok((abs.to_string_lossy().to_string(), rel, base, proposed))
}

fn control_reply(request_id: &str, response: Value) -> Value {
    serde_json::json!({
        "type": "control_response",
        "response": { "subtype": "success", "request_id": request_id, "response": response }
    })
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

/// Cap on how many sibling projects we name, for the same reason
/// `MAX_CONTEXT_FILES` exists.
const MAX_SCOPE_PROJECTS: usize = 12;

/// Build the cross-project scope preamble for an "all projects" (or group)
/// chat.
///
/// Ken's chat is a Claude Code session whose cwd is the FOCUSED project, so
/// widening scope is a matter of telling the session which sibling folders
/// it may read — it can open absolute paths once those folders are trusted.
/// That falls out of the architecture rather than fighting it, and it gives
/// exactly the ruling this feature was specified under: **read across every
/// project in scope, write only inside the focused one** unless the user
/// names another. A session that could freely edit seven repos would turn
/// one misread instruction into seven repos' worth of damage.
///
/// Returns `None` when there is nothing to widen to (no siblings), so a
/// single-project workspace sends exactly what it sends today.
/// How to link a file in each repo of the workspace, as (name, id), for a
/// chat turn: a project-relative link resolves only in the chat's own
/// project, so a file read in a sibling needs its repo's address. None
/// outside a workspace.
pub fn build_cite_preamble(members: &[(String, uuid::Uuid)]) -> Option<String> {
    if members.len() < 2 {
        return None;
    }
    let mut s = String::from("[Links to files in the workspace's repos:");
    for (name, id) in members.iter().take(MAX_SCOPE_PROJECTS) {
        s.push_str(&format!("\n- {name}: ken://{id}/<path>#L<line>"));
    }
    s.push(']');
    Some(s)
}

pub fn build_scope_preamble(
    focused_name: &str,
    focused_root: &str,
    siblings: &[(String, String)],
    group_name: Option<&str>,
) -> Option<String> {
    if siblings.is_empty() {
        return None;
    }
    let shown = siblings.len().min(MAX_SCOPE_PROJECTS);
    let mut lines = String::new();
    for (name, root) in &siblings[..shown] {
        lines.push_str(&format!("\n- {name}: {root}"));
    }
    if siblings.len() > shown {
        lines.push_str(&format!("\n- … and {} more", siblings.len() - shown));
    }
    let scope_label = match group_name {
        Some(g) => format!("the \"{g}\" group of projects"),
        None => "every project in this workspace".to_string(),
    };
    Some(format!(
        "[Scope — this question is about {scope_label}, not just one. \
         Besides the current project ({focused_name}, at {focused_root}) you \
         may READ files in these sibling projects by absolute path when they \
         are relevant:{lines}\n\
         Make edits only inside {focused_name} unless the user explicitly \
         names another project to change. If answering needs a file from a \
         sibling, read it rather than guessing.]"
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
/// failure is swallowed so a session still spawns (it just may hit the prompt).
///
/// Only the interactive TUI needs this (`attach_terminal`, the runner's hidden
/// TUI); print mode (`-p`) never shows the dialog. The file is shared with
/// every running Claude Code, so it is written only when a flag is missing,
/// through a temp file and a rename, and never when it cannot be parsed (a
/// half-written file must not be replaced by one holding only our keys).
pub fn ensure_folder_trusted(project_root: &Path) {
    if let Some(cfg_path) = claude_config_path() {
        trust_folder_in(&cfg_path, project_root);
    }
}

/// [`ensure_folder_trusted`] against the config file at `cfg_path`.
fn trust_folder_in(cfg_path: &Path, project_root: &Path) {
    let (keys, stale) = trust_keys(project_root);
    let existing = match std::fs::read(cfg_path) {
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) => v,
            Err(_) => return,
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Value::Null,
        Err(_) => return,
    };
    if !needs_trust_write(&existing, &keys, &stale) {
        return;
    }
    let updated = apply_folder_trust(existing, &keys, &stale);
    if let Ok(bytes) = serde_json::to_vec_pretty(&updated) {
        let _ = write_atomically(cfg_path, &bytes);
    }
}

/// The keys Claude Code reads for `project_root` (the path as given and its
/// canonical form, in Claude Code's own spelling), and the keys older Ken
/// versions wrote on Windows that Claude Code never reads (backslashes, the
/// `\\?\` prefix), to be dropped when they hold nothing but Ken's flags.
fn trust_keys(project_root: &Path) -> (Vec<String>, Vec<String>) {
    let raw = project_root.to_string_lossy().into_owned();
    let canonical = crate::setup::plain_canonical(project_root).to_string_lossy().into_owned();
    let mut keys = Vec::new();
    for p in [&raw, &canonical] {
        let k = claude_config_key(p, cfg!(windows));
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    let mut stale = Vec::new();
    if cfg!(windows) {
        let verbatim = std::fs::canonicalize(project_root)
            .map(|c| c.to_string_lossy().into_owned())
            .unwrap_or_default();
        for p in [raw, canonical, verbatim] {
            if !p.is_empty() && !keys.contains(&p) && !stale.contains(&p) {
                stale.push(p);
            }
        }
    }
    (keys, stale)
}

/// A folder's key in `~/.claude.json` as Claude Code writes it: the path its
/// process sees as the cwd. On Windows that has no `\\?\` prefix and uses
/// forward slashes (`C:/Code/ken`); elsewhere it is the path unchanged.
pub(crate) fn claude_config_key(path: &str, windows: bool) -> String {
    if !windows {
        return path.to_string();
    }
    let plain = if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else {
        path.strip_prefix(r"\\?\").unwrap_or(path).to_string()
    };
    plain.replace('\\', "/")
}

const TRUST_FLAGS: [&str; 2] = ["hasTrustDialogAccepted", "hasCompletedProjectOnboarding"];

/// Whether `cfg` lacks a trust flag for any of `keys`, or still holds one of
/// Ken's own `stale` entries.
fn needs_trust_write(cfg: &Value, keys: &[String], stale: &[String]) -> bool {
    let projects = cfg.get("projects");
    let missing = keys.iter().any(|k| {
        let entry = projects.and_then(|p| p.get(k));
        TRUST_FLAGS.iter().any(|f| entry.and_then(|e| e.get(*f)) != Some(&Value::Bool(true)))
    });
    missing || stale.iter().any(|k| projects.and_then(|p| p.get(k)).is_some_and(is_only_ken_flags))
}

/// An entry holding nothing but the flags Ken writes (so Ken wrote it).
fn is_only_ken_flags(entry: &Value) -> bool {
    entry.as_object().is_some_and(|o| o.keys().all(|k| TRUST_FLAGS.contains(&k.as_str())))
}

/// Replace `path` with `bytes` through a temp file in the same folder and a
/// rename, so a reader never sees a half-written file.
fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = path.with_file_name(format!("{name}.ken-{}.tmp", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Locate Claude Code's `.claude.json`. Honors `CLAUDE_CONFIG_DIR` (which the
/// CLI itself respects) so a custom config location — and test isolation — work
/// the same way the CLI sees them; otherwise `~/.claude.json`.
fn claude_config_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return Some(PathBuf::from(dir).join(".claude.json"));
    }
    // Unit tests that start a hidden TUI (runner, research, ingest) never
    // write into the developer's own ~/.claude.json.
    if cfg!(test) {
        return None;
    }
    dirs::home_dir().map(|h| h.join(".claude.json"))
}

/// Set the trust/onboarding flags for exactly `path_keys` in a parsed
/// `~/.claude.json` value, creating the `projects` map and entries as needed,
/// drop the `stale` entries that hold only Ken's flags, and leave every other
/// key (and every other project) untouched. Pure so it is unit-tested without
/// a real home directory.
fn apply_folder_trust(mut cfg: Value, path_keys: &[String], stale: &[String]) -> Value {
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
    // `retain` keeps the order of the person's other entries.
    projects.retain(|k, v| !(stale.contains(k) && is_only_ken_flags(v)));
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
    /// Ken's MCP server for this project, as a Claude Code `--mcp-config`
    /// file, when ken-mcp was found.
    mcp_config: Mutex<Option<PathBuf>>,
    /// The workspace's other repos, readable in chat without asking
    /// (`--add-dir`), so a search Ken's index missed can be done by hand.
    read_dirs: Mutex<Vec<PathBuf>>,
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
            mcp_config: Mutex::new(None),
            read_dirs: Mutex::new(Vec::new()),
            live: Arc::new(Mutex::new(HashMap::new())),
            on_update: Arc::new(on_update),
        }
    }

    /// Give new chat sessions Ken's MCP server (a `--mcp-config` file).
    pub fn set_mcp_config(&self, path: Option<PathBuf>) {
        *self.mcp_config.lock().unwrap() = path;
    }

    /// The other folders a chat may read (the workspace's repos). Edits
    /// there still come back as proposals to review, like any edit.
    pub fn set_read_dirs(&self, dirs: Vec<PathBuf>) {
        *self.read_dirs.lock().unwrap() = dirs;
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
        // Working before the write, never after: on Windows the write can
        // wait until the CLI reads its stdin, and a short turn can finish in
        // that time, so a "working" sent afterwards would land after "done"
        // and leave the chat looking busy for good.
        (self.on_update)(ChatUpdate::Status {
            chat_id: chat_id.to_string(),
            status: "working".into(),
            detail: None,
        });
        let written = {
            let mut stdin = stdin.lock().unwrap();
            writeln!(stdin, "{payload}").and_then(|_| stdin.flush())
        };
        if let Err(e) = written {
            (self.on_update)(ChatUpdate::Status {
                chat_id: chat_id.to_string(),
                status: "error".into(),
                detail: None,
            });
            return Err(Error::Other(format!("chat send failed: {e}")));
        }
        Ok(())
    }

    /// Answer a pending AskUserQuestion. `answers` is keyed by the exact
    /// question text (multi-select values are comma-separated labels); the
    /// waiting CLI resumes the turn as soon as the line lands.
    pub fn answer_question(
        &self,
        chat_id: &str,
        request_id: &str,
        tool_use_id: &str,
        questions: Value,
        answers: Value,
    ) -> Result<()> {
        let payload = serde_json::json!({
            "type": "control_response",
            "response": {
                "subtype": "success",
                "request_id": request_id,
                "response": {
                    "behavior": "allow",
                    "updatedInput": { "questions": questions, "answers": answers },
                    "toolUseID": tool_use_id,
                }
            }
        });
        // Same lock dance as `send`: clone the handle out under a short map
        // lock, then write with the map lock released.
        let stdin = {
            let live = self.live.lock().unwrap();
            live.get(chat_id)
                .ok_or_else(|| Error::Other("this chat is no longer running".into()))?
                .stdin
                .clone()
        };
        // Working first, as in `send`.
        (self.on_update)(ChatUpdate::Status {
            chat_id: chat_id.to_string(),
            status: "working".into(),
            detail: None,
        });
        let written = {
            let mut stdin = stdin.lock().unwrap();
            writeln!(stdin, "{payload}").and_then(|_| stdin.flush())
        };
        if let Err(e) = written {
            (self.on_update)(ChatUpdate::Status {
                chat_id: chat_id.to_string(),
                status: "error".into(),
                detail: None,
            });
            return Err(Error::Other(format!("answer failed: {e}")));
        }
        Ok(())
    }

    /// Answer a pending edit: `allow` lets the CLI make the edit exactly as
    /// it asked; otherwise it is denied with `message`, which says what the
    /// user declined (and what Ken applied itself, for a partial accept).
    pub fn answer_edit(&self, chat_id: &str, proposal: &EditProposal, allow: bool, message: &str) -> Result<()> {
        let response = if allow {
            serde_json::json!({ "behavior": "allow", "updatedInput": proposal.input, "toolUseID": proposal.tool_use_id })
        } else {
            serde_json::json!({ "behavior": "deny", "message": message })
        };
        let payload = control_reply(&proposal.request_id, response);
        let stdin = {
            let live = self.live.lock().unwrap();
            live.get(chat_id)
                .ok_or_else(|| Error::Other("this chat is no longer running".into()))?
                .stdin
                .clone()
        };
        // Working first, as in `send`.
        (self.on_update)(ChatUpdate::Status {
            chat_id: chat_id.to_string(),
            status: "working".into(),
            detail: None,
        });
        {
            let mut stdin = stdin.lock().unwrap();
            writeln!(stdin, "{payload}")
                .and_then(|_| stdin.flush())
                .map_err(|e| Error::Other(format!("answer failed: {e}")))?;
        }
        Ok(())
    }

    /// Stop a chat's conversation process (mode switch, archive, shutdown).
    pub fn stop(&self, chat_id: &str) {
        let stopped = self.live.lock().unwrap().remove(chat_id);
        if let Some(mut conv) = stopped {
            crate::proc::kill_tree(&mut conv.child);
            let _ = conv.child.wait();
            self.stopped(chat_id);
        }
    }

    /// A chat whose process Ken ended: removed from `live` first, so its
    /// reader sees no death to report, and a reply cut off mid-turn would
    /// otherwise show "working" for good.
    fn stopped(&self, chat_id: &str) {
        (self.on_update)(ChatUpdate::Status { chat_id: chat_id.to_string(), status: "done".into(), detail: None });
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
                    crate::proc::kill_tree(&mut conv.child);
                    let _ = conv.child.wait();
                    self.stopped(&oldest);
                }
            }
        }

        // Print mode (`-p`) never shows the trust dialog, so no folder trust
        // is written here; only the interactive TUI needs it (attach_terminal,
        // the runner's hidden TUI).
        let mut cmd = Command::new(&self.binary);
        // No console window for the chat process (see `proc::quiet`).
        crate::proc::quiet(&mut cmd);
        cmd.args([
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            // The reply streams in as it is written, not all at once.
            "--include-partial-messages",
            // Default, not acceptEdits: every Edit/MultiEdit/Write comes back
            // as a permission request, which Ken shows as a diff to accept or
            // decline change by change.
            "--permission-mode",
            "default",
            // Routes permission requests to our stdin/stdout control channel,
            // which is what makes AskUserQuestion reach the user at all.
            "--permission-prompt-tool",
            "stdio",
        ]);
        // One line: on Windows the CLI is a `.cmd` launcher, and a line break
        // in any argument to one fails the spawn.
        cmd.arg("--append-system-prompt").arg(KEN_GUIDE.replace('\n', " "));
        cmd.arg("--disallowedTools").arg(CHAT_DISALLOWED.join(","));
        if let Some(cfg) = self.mcp_config.lock().unwrap().clone() {
            cmd.arg("--mcp-config").arg(cfg);
        }
        for dir in self.read_dirs.lock().unwrap().iter().filter(|d| **d != self.project_root) {
            cmd.arg("--add-dir").arg(dir);
        }
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
        crate::proc::track(&child);
        let stdin = child.stdin.take().ok_or_else(|| Error::Other("no stdin".into()))?;
        let stdout = child.stdout.take().ok_or_else(|| Error::Other("no stdout".into()))?;
        let stderr = child.stderr.take();
        let stdin = Arc::new(Mutex::new(stdin));

        // Event pump.
        let on_update = self.on_update.clone();
        let live_map = self.live.clone();
        let id = chat_id.to_string();
        let pump_stdin = stdin.clone();
        let root = self.project_root.clone();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            let mut saw_result = false;
            for line in reader.lines().map_while(|l| l.ok()) {
                for event in parse_events(&line) {
                match event {
                    ParsedEvent::AssistantText(text) => {
                        saw_result = false;
                        on_update(ChatUpdate::Message {
                            chat_id: id.clone(),
                            role: "assistant".into(),
                            content: text,
                        });
                    }
                    ParsedEvent::TextDelta(text) => {
                        on_update(ChatUpdate::Delta { chat_id: id.clone(), text });
                    }
                    ParsedEvent::Tool { id: tool_use_id, name, summary } => {
                        let payload = serde_json::json!({
                            "toolUseId": tool_use_id,
                            "name": name,
                            "summary": summary,
                            "status": "running",
                        });
                        on_update(ChatUpdate::Tool { chat_id: id.clone(), payload: payload.to_string() });
                    }
                    ParsedEvent::ToolDone { id: tool_use_id, is_error, preview } => {
                        on_update(ChatUpdate::ToolResult { chat_id: id.clone(), tool_use_id, is_error, preview });
                    }
                    ParsedEvent::TurnResult { is_error } => {
                        saw_result = true;
                        on_update(ChatUpdate::Status {
                            chat_id: id.clone(),
                            status: if is_error { "error".into() } else { "done".into() },
                            detail: None,
                        });
                    }
                    ParsedEvent::ControlRequest {
                        request_id,
                        tool_name,
                        input,
                        tool_use_id,
                    } => {
                        if tool_name == ASK_TOOL {
                            let payload = serde_json::json!({
                                "requestId": request_id,
                                "toolUseId": tool_use_id,
                                "questions": input.get("questions").cloned()
                                    .unwrap_or(Value::Array(Vec::new())),
                            });
                            on_update(ChatUpdate::Question {
                                chat_id: id.clone(),
                                payload: payload.to_string(),
                            });
                            on_update(ChatUpdate::Status {
                                chat_id: id.clone(),
                                status: "needs_input".into(),
                                detail: None,
                            });
                        } else if let Some(tool) = ken_mcp_tool(&tool_name) {
                            // Ken's read tools run; its write tools are for
                            // the person to do in Ken.
                            let response = if KEN_MCP_ALLOWED.contains(&tool) {
                                serde_json::json!({ "behavior": "allow", "updatedInput": input, "toolUseID": tool_use_id })
                            } else {
                                serde_json::json!({ "behavior": "deny", "message": format!("{tool} sends something to a teammate; the person does that in Ken.") })
                            };
                            let reply = control_reply(&request_id, response);
                            let mut w = pump_stdin.lock().unwrap();
                            let _ = writeln!(w, "{reply}").and_then(|_| w.flush());
                        } else if tool_name == "NotebookEdit" {
                            // Notebooks keep the old behaviour: allowed as asked.
                            let reply = control_reply(
                                &request_id,
                                serde_json::json!({ "behavior": "allow", "updatedInput": input, "toolUseID": tool_use_id }),
                            );
                            let mut w = pump_stdin.lock().unwrap();
                            let _ = writeln!(w, "{reply}").and_then(|_| w.flush());
                        } else if EDIT_TOOLS.contains(&tool_name.as_str()) {
                            match propose_edit(&root, &tool_name, &input) {
                                Ok((path, rel_path, base, proposed)) => {
                                    let proposal = EditProposal {
                                        request_id,
                                        tool_use_id,
                                        tool: tool_name,
                                        path,
                                        rel_path,
                                        base,
                                        proposed,
                                        input,
                                        decision: None,
                                        note: None,
                                    };
                                    on_update(ChatUpdate::EditProposal {
                                        chat_id: id.clone(),
                                        payload: serde_json::to_string(&proposal).unwrap_or_default(),
                                    });
                                    on_update(ChatUpdate::Status {
                                        chat_id: id.clone(),
                                        status: "needs_input".into(),
                                        detail: None,
                                    });
                                }
                                Err(why) => {
                                    let reply = control_reply(
                                        &request_id,
                                        serde_json::json!({ "behavior": "deny", "message": format!("This edit cannot apply: {why}. Read the file again and retry.") }),
                                    );
                                    let mut w = pump_stdin.lock().unwrap();
                                    let _ = writeln!(w, "{reply}").and_then(|_| w.flush());
                                }
                            }
                        } else {
                            // Every other permission request is denied, which
                            // preserves the headless behavior this session had
                            // before the control channel existed: only the
                            // acceptEdits policy grants tools, never a prompt.
                            let reply = serde_json::json!({
                                "type": "control_response",
                                "response": {
                                    "subtype": "success",
                                    "request_id": request_id,
                                    "response": {
                                        "behavior": "deny",
                                        "message": "Denied: this embedded session cannot grant tool permissions",
                                    }
                                }
                            });
                            let mut w = pump_stdin.lock().unwrap();
                            if let Err(e) = writeln!(w, "{reply}").and_then(|_| w.flush()) {
                                eprintln!("chat {id}: deny control_response failed: {e}");
                            }
                        }
                    }
                    ParsedEvent::Init | ParsedEvent::Other => {}
                }
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
    fn a_workspace_turn_says_how_to_link_each_repos_files() {
        let (a, b) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        assert_eq!(build_cite_preamble(&[("wiki".into(), a)]), None, "one project: relative links work");
        let p = build_cite_preamble(&[("wiki".into(), a), ("Project Documents".into(), b)]).unwrap();
        assert!(p.contains(&format!("- Project Documents: ken://{b}/<path>#L<line>")), "{p}");
        assert!(KEN_GUIDE.contains("%20") && KEN_GUIDE.contains("never a project-relative path"));
    }

    #[test]
    fn the_guide_says_how_to_weigh_a_hits_label() {
        let guide = KEN_GUIDE.replace('\n', " ");
        for word in ["carries a label", "a later ruling over an earlier one", "over a ticket or note", "a current page over a record"] {
            assert!(guide.contains(word), "{word}");
        }
    }

    #[test]
    fn every_ken_tool_the_guide_names_is_one_the_chat_may_use() {
        let named: Vec<&str> = KEN_GUIDE
            .split("mcp__ken__")
            .skip(1)
            .map(|rest| rest.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).next().unwrap_or(""))
            .collect();
        assert!(named.len() > 8, "the guide names Ken's tools in full: {named:?}");
        for tool in named {
            assert!(KEN_MCP_ALLOWED.contains(&tool), "{tool} is named in the guide but not allowed");
        }
        assert!(!KEN_MCP_ALLOWED.contains(&"family_send"), "sending to a teammate stays with the person");
    }

    #[test]
    fn a_multi_line_edit_on_a_crlf_file_matches_and_keeps_crlf() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.md");
        std::fs::write(&file, "# Title\r\n\r\nFirst line.\r\nSecond line.\r\n").unwrap();
        let input = serde_json::json!({
            "file_path": file.to_string_lossy(),
            "old_string": "First line.\nSecond line.",
            "new_string": "First line.\nA new second line.",
        });
        let (_, _, base, proposed) = propose_edit(dir.path(), "Edit", &input).expect("the LF edit matches the CRLF file");
        assert!(base.contains("\r\n"));
        assert_eq!(proposed, "# Title\r\n\r\nFirst line.\r\nA new second line.\r\n");
        // An LF file is left as it is.
        std::fs::write(&file, "one\ntwo\n").unwrap();
        let input = serde_json::json!({ "file_path": file.to_string_lossy(), "old_string": "two", "new_string": "three" });
        assert_eq!(propose_edit(dir.path(), "Edit", &input).unwrap().3, "one\nthree\n");
    }

    #[test]
    fn scope_preamble_none_without_siblings() {
        assert_eq!(build_scope_preamble("ken", "/w/ken", &[], None), None);
    }

    #[test]
    fn scope_preamble_names_siblings_and_pins_writes_to_the_focused_project() {
        let siblings = vec![
            ("realms".to_string(), "/w/realms".to_string()),
            ("realms-tools".to_string(), "/w/realms-tools".to_string()),
        ];
        let p = build_scope_preamble("ken", "/w/ken", &siblings, None).unwrap();
        assert!(p.contains("/w/realms-tools"), "sibling path missing: {p}");
        assert!(p.contains("every project in this workspace"));
        // The read/write asymmetry is the whole safety property.
        assert!(p.contains("READ"), "read permission not stated: {p}");
        assert!(
            p.contains("edits only inside ken"),
            "write restriction not stated: {p}"
        );
    }

    #[test]
    fn scope_preamble_names_the_group_when_scoped_to_one() {
        let siblings = vec![("realms-tools".to_string(), "/w/realms-tools".to_string())];
        let p = build_scope_preamble("realms", "/w/realms", &siblings, Some("Shattered Realms"))
            .unwrap();
        assert!(p.contains("\"Shattered Realms\" group"), "{p}");
        assert!(!p.contains("every project in this workspace"));
    }

    #[test]
    fn scope_preamble_caps_long_lists() {
        let siblings: Vec<(String, String)> = (0..30)
            .map(|i| (format!("p{i}"), format!("/w/p{i}")))
            .collect();
        let p = build_scope_preamble("ken", "/w/ken", &siblings, None).unwrap();
        assert!(p.contains("and 18 more"), "list not capped: {p}");
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
        let out = apply_folder_trust(existing, &["/ken/proj".to_string()], &[]);
        // Our project is now trusted.
        assert_eq!(out["projects"]["/ken/proj"]["hasTrustDialogAccepted"], true);
        assert_eq!(out["projects"]["/ken/proj"]["hasCompletedProjectOnboarding"], true);
        // Unrelated keys and other projects are untouched.
        assert_eq!(out["anonymousId"], "keep-me");
        assert_eq!(out["projects"]["/other/proj"]["lastCost"], 1.5);
    }

    #[test]
    fn folder_trust_from_empty_config_is_idempotent() {
        let a = apply_folder_trust(Value::Null, &["/p".to_string()], &[]);
        let b = apply_folder_trust(a.clone(), &["/p".to_string()], &[]);
        assert_eq!(a, b);
        assert_eq!(b["projects"]["/p"]["hasTrustDialogAccepted"], true);
    }

    /// Claude Code on Windows keys a folder by its plain path with forward
    /// slashes; on macOS and Linux by the path as it is.
    #[test]
    fn trust_keys_are_written_in_claude_codes_form() {
        assert_eq!(claude_config_key(r"C:\ken-eval\ATT\ATT-Wiki", true), "C:/ken-eval/ATT/ATT-Wiki");
        assert_eq!(claude_config_key(r"\\?\C:\Code\ken", true), "C:/Code/ken");
        assert_eq!(claude_config_key(r"\\?\UNC\server\share\repo", true), "//server/share/repo");
        assert_eq!(claude_config_key("/Users/a/My Repo", false), "/Users/a/My Repo");
    }

    /// Nothing is written when every flag is already set, and the entries old
    /// Ken versions wrote under keys Claude Code never reads are dropped, but
    /// only when they hold nothing besides Ken's flags.
    #[test]
    fn trust_write_is_skipped_when_set_and_drops_only_kens_stale_entries() {
        let keys = vec!["C:/Code/ken".to_string()];
        let stale = vec![r"C:\Code\ken".to_string(), r"\\?\C:\Code\ken".to_string()];
        let set = serde_json::json!({ "projects": { "C:/Code/ken": {
            "hasTrustDialogAccepted": true, "hasCompletedProjectOnboarding": true, "lastCost": 2.0 } } });
        assert!(!needs_trust_write(&set, &keys, &stale));
        assert!(needs_trust_write(&Value::Null, &keys, &stale));
        let half = serde_json::json!({ "projects": { "C:/Code/ken": { "hasTrustDialogAccepted": false } } });
        assert!(needs_trust_write(&half, &keys, &stale));

        let mut with_stale = set.clone();
        with_stale["projects"][r"C:\Code\ken"] =
            serde_json::json!({ "hasTrustDialogAccepted": true, "hasCompletedProjectOnboarding": true });
        with_stale["projects"][r"\\?\C:\Code\ken"] =
            serde_json::json!({ "hasTrustDialogAccepted": true, "allowedTools": ["Read"] });
        assert!(needs_trust_write(&with_stale, &keys, &stale));
        let out = apply_folder_trust(with_stale, &keys, &stale);
        assert!(out["projects"].get(r"C:\Code\ken").is_none(), "Ken's own stale entry is dropped");
        assert_eq!(out["projects"][r"\\?\C:\Code\ken"]["allowedTools"][0], "Read", "an entry with more in it stays");
        assert_eq!(out["projects"]["C:/Code/ken"]["lastCost"], 2.0);
    }

    /// The write goes through a temp file and leaves no temp file behind; a
    /// config that does not parse is never replaced.
    #[test]
    fn trust_is_written_atomically_and_never_over_an_unparsable_config() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let path = dir.path().join(".claude.json");

        // No config yet: one is written, and no temp file is left behind.
        trust_folder_in(&path, &repo);
        let cfg: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let key = claude_config_key(&crate::setup::plain_canonical(&repo).to_string_lossy(), cfg!(windows));
        assert_eq!(cfg["projects"][&key]["hasTrustDialogAccepted"], true, "got {cfg}");
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n != "repo")
            .collect();
        assert_eq!(names, vec![".claude.json".to_string()], "no temp file left behind");

        // Already trusted: the file is not rewritten.
        std::fs::write(&path, serde_json::to_vec(&cfg).unwrap()).unwrap();
        let before = std::fs::read(&path).unwrap();
        trust_folder_in(&path, &repo);
        assert_eq!(std::fs::read(&path).unwrap(), before);

        // A half-written config is left alone.
        std::fs::write(&path, b"{\"projects\": {").unwrap();
        trust_folder_in(&path, &repo);
        assert_eq!(std::fs::read(&path).unwrap(), b"{\"projects\": {");
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
            ParsedEvent::Tool { id: String::new(), name: "Read".into(), summary: "Read a.md".into() }
        );
        // Unknown types are tolerated.
        assert_eq!(parse_event(r#"{"type":"mystery"}"#), ParsedEvent::Other);
    }

    #[test]
    fn parse_event_control_request() {
        let line = r#"{"type":"control_request","request_id":"req-1","request":{"subtype":"can_use_tool","tool_name":"AskUserQuestion","display_name":"AskUserQuestion","input":{"questions":[{"question":"Color?","header":"Color","options":[{"label":"Red","description":"warm"}],"multiSelect":false}]},"tool_use_id":"toolu_1","requires_user_interaction":true}}"#;
        match parse_event(line) {
            ParsedEvent::ControlRequest { request_id, tool_name, input, tool_use_id } => {
                assert_eq!(request_id, "req-1");
                assert_eq!(tool_name, "AskUserQuestion");
                assert_eq!(tool_use_id, "toolu_1");
                assert_eq!(input["questions"][0]["header"], "Color");
            }
            other => panic!("expected ControlRequest, got {other:?}"),
        }
        // Other tools still parse (the engine denies them).
        assert!(matches!(
            parse_event(
                r#"{"type":"control_request","request_id":"r2","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{},"tool_use_id":"t2"}}"#
            ),
            ParsedEvent::ControlRequest { .. }
        ));
        // Non-permission control requests are not our business.
        assert_eq!(
            parse_event(r#"{"type":"control_request","request_id":"r3","request":{"subtype":"interrupt"}}"#),
            ParsedEvent::Other
        );
    }

    #[test]
    fn ask_user_question_tool_use_is_not_a_tool_card() {
        // The question card replaces the activity line, so the tool_use block
        // must be skipped — and other blocks in the same event still scanned.
        assert_eq!(
            parse_event(
                r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"AskUserQuestion","input":{"questions":[]}}]}}"#
            ),
            ParsedEvent::Other
        );
        assert_eq!(
            parse_event(
                r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"AskUserQuestion","input":{}},{"type":"tool_use","name":"Read","input":{"file_path":"a.md"}}]}}"#
            ),
            ParsedEvent::Tool { id: String::new(), name: "Read".into(), summary: "Read a.md".into() }
        );
    }

    #[test]
    fn a_message_yields_every_block_in_order() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Looking."},{"type":"tool_use","id":"t1","name":"Grep","input":{"pattern":"save"}},{"type":"tool_use","id":"t2","name":"mcp__ken__route_query","input":{"query":"save format"}}]}}"#;
        assert_eq!(
            parse_events(line),
            vec![
                ParsedEvent::AssistantText("Looking.".into()),
                ParsedEvent::Tool { id: "t1".into(), name: "Grep".into(), summary: "Grep save".into() },
                ParsedEvent::Tool { id: "t2".into(), name: "mcp__ken__route_query".into(), summary: "Ken route query save format".into() },
            ]
        );
    }

    #[test]
    fn tool_results_and_text_deltas_parse() {
        let line = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","is_error":true,"content":[{"type":"text","text":"no such file"}]}]}}"#;
        assert_eq!(
            parse_events(line),
            vec![ParsedEvent::ToolDone { id: "t1".into(), is_error: true, preview: "no such file".into() }]
        );
        let delta = r#"{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}}"#;
        assert_eq!(parse_events(delta), vec![ParsedEvent::TextDelta("Hel".into())]);
        // A subagent's stream and a tool's input stream are not reply text.
        let sub = r#"{"type":"stream_event","parent_tool_use_id":"t9","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"x"}}}"#;
        assert_eq!(parse_events(sub), vec![ParsedEvent::Other]);
        let json = r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"{"}}}"#;
        assert_eq!(parse_events(json), vec![ParsedEvent::Other]);
    }

    #[test]
    fn a_long_result_is_cut_for_its_card_and_a_long_arg_by_characters() {
        let long = (1..=20).map(|i| format!("line {i}")).collect::<Vec<_>>().join("
");
        let preview = result_preview(Some(&Value::String(long)));
        assert!(preview.starts_with("line 1
line 2") && preview.ends_with("…"), "{preview}");
        assert!(!preview.contains("line 9"));
        let wide = serde_json::json!({ "name": "Grep", "input": { "pattern": "é".repeat(100) } });
        assert_eq!(summarize_tool(&wide).chars().count(), "Grep ".len() + 80);
    }

    fn control_responses(dir: &Path) -> Vec<Value> {
        let raw = std::fs::read_to_string(dir.join("control_response.txt")).unwrap_or_default();
        raw.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str::<Value>(l).unwrap_or(Value::Null))
            .collect()
    }

    #[test]
    #[cfg_attr(windows, ignore = "runs the bash fake CLI; covered on macOS/Linux")]
    fn ask_user_question_round_trip() {
        let (dir, engine, rx) = engine("ask-question");
        engine.send("chat-q", "askq please", false, None).unwrap();

        // Wait for the question + needs_input pause.
        let mut seen = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut payload = None;
        while Instant::now() < deadline && payload.is_none() {
            if let Ok(u) = rx.recv_timeout(Duration::from_millis(200)) {
                if let ChatUpdate::Question { payload: p, .. } = &u {
                    payload = Some(p.clone());
                }
                seen.push(u);
            }
        }
        let payload = payload.expect("no Question update");
        let v: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["requestId"], "req-fake-1");
        assert_eq!(v["toolUseId"], "toolu_fake1");
        assert_eq!(v["questions"][0]["question"], "Favorite color?");
        assert_eq!(v["questions"][0]["options"][1]["label"], "Blue");
        // No tool card for the AskUserQuestion tool_use itself.
        assert!(!seen.iter().any(|u| matches!(u,
            ChatUpdate::Tool { payload, .. } if payload.contains("AskUserQuestion"))));
        // The turn pauses for the user.
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline
            && !seen.iter().any(|u| matches!(u,
                ChatUpdate::Status { status, .. } if status == "needs_input"))
        {
            if let Ok(u) = rx.recv_timeout(Duration::from_millis(200)) {
                seen.push(u);
            }
        }
        assert!(seen.iter().any(|u| matches!(u,
            ChatUpdate::Status { status, .. } if status == "needs_input")),
            "no needs_input status: {seen:?}");

        engine
            .answer_question(
                "chat-q",
                "req-fake-1",
                "toolu_fake1",
                v["questions"].clone(),
                serde_json::json!({ "Favorite color?": "Blue" }),
            )
            .unwrap();

        let rest = collect_until_done(&rx, 15);
        assert!(rest.iter().any(|u| matches!(u,
            ChatUpdate::Message { content, .. } if content.contains("you chose: done"))),
            "turn did not continue: {rest:?}");
        assert!(matches!(rest.last().unwrap(),
            ChatUpdate::Status { status, .. } if status == "done"));

        let replies = control_responses(dir.path());
        assert_eq!(replies.len(), 1, "expected exactly one control_response");
        let r = &replies[0]["response"];
        assert_eq!(replies[0]["type"], "control_response");
        assert_eq!(r["subtype"], "success");
        assert_eq!(r["request_id"], "req-fake-1");
        assert_eq!(r["response"]["behavior"], "allow");
        assert_eq!(r["response"]["toolUseID"], "toolu_fake1");
        assert_eq!(r["response"]["updatedInput"]["answers"]["Favorite color?"], "Blue");
        assert_eq!(
            r["response"]["updatedInput"]["questions"][0]["question"],
            "Favorite color?"
        );
    }

    #[test]
    #[cfg_attr(windows, ignore = "runs the bash fake CLI; covered on macOS/Linux")]
    fn other_permission_requests_are_auto_denied() {
        let (dir, engine, rx) = engine("ask-question");
        engine.send("chat-b", "askbash now", false, None).unwrap();
        let updates = collect_until_done(&rx, 15);
        assert!(!updates.iter().any(|u| matches!(u, ChatUpdate::Question { .. })),
            "a non-AskUserQuestion request must not prompt the user");
        assert!(matches!(updates.last().unwrap(),
            ChatUpdate::Status { status, .. } if status == "done"));

        let replies = control_responses(dir.path());
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0]["response"]["request_id"], "req-fake-2");
        assert_eq!(replies[0]["response"]["response"]["behavior"], "deny");
    }

    #[test]
    #[cfg_attr(windows, ignore = "runs the bash fake CLI; covered on macOS/Linux")]
    fn answer_question_on_unknown_chat_errors() {
        let (_d, engine, _rx) = engine("ask-question");
        assert!(engine
            .answer_question(
                "no-such-chat",
                "req-x",
                "toolu-x",
                serde_json::json!([]),
                serde_json::json!({}),
            )
            .is_err());
    }

    #[test]
    #[cfg_attr(windows, ignore = "runs the bash fake CLI; covered on macOS/Linux")]
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
    #[cfg_attr(windows, ignore = "runs the bash fake CLI; covered on macOS/Linux")]
    fn tool_use_becomes_a_tool_card() {
        let (_d, engine, rx) = engine("complete");
        engine.send("chat-2", "usetool please", false, None).unwrap();
        let updates = collect_until_done(&rx, 15);
        assert!(updates.iter().any(|u| matches!(u,
            ChatUpdate::Tool { payload, .. } if payload.contains("Read notes/meeting.md"))));
    }

    #[test]
    #[cfg_attr(windows, ignore = "runs the bash fake CLI; covered on macOS/Linux")]
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
    #[cfg_attr(windows, ignore = "runs the bash fake CLI; covered on macOS/Linux")]
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

    /// Live AskUserQuestion round-trip against the real Claude CLI — run
    /// explicitly with `cargo test -p ken-core real_ask -- --ignored --nocapture`.
    /// Uses the real user config (an isolated CLAUDE_CONFIG_DIR has no login and
    /// the turn dies with "Not logged in"); leaves a trust entry for a temp dir.
    #[test]
    #[ignore]
    fn real_ask_user_question_round_trip() {
        let Some(binary) = crate::runner::discover_claude() else {
            panic!("claude CLI not found");
        };
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = channel();
        let engine = ChatEngine::new(binary, dir.path().to_path_buf(), move |u| {
            let _ = tx.send(u);
        });
        let chat_id = uuid::Uuid::new_v4().to_string();
        engine
            .send(
                &chat_id,
                "Use the AskUserQuestion tool to ask me exactly one question: my \
                 favorite color, options Red and Blue. Wait for my answer, then \
                 reply with one sentence naming my choice.",
                false,
                Some("haiku"),
            )
            .unwrap();

        // Wait for the question.
        let deadline = Instant::now() + Duration::from_secs(240);
        let mut payload = None;
        let mut seen = Vec::new();
        while Instant::now() < deadline && payload.is_none() {
            if let Ok(u) = rx.recv_timeout(Duration::from_millis(500)) {
                eprintln!("update: {u:?}");
                if let ChatUpdate::Question { payload: p, .. } = &u {
                    payload = Some(p.clone());
                }
                seen.push(u);
            }
        }
        let payload =
            payload.unwrap_or_else(|| panic!("no Question update; saw: {seen:#?}"));
        let v: Value = serde_json::from_str(&payload).unwrap();
        eprintln!("question payload: {v:#}");
        let question = v["questions"][0]["question"].as_str().unwrap().to_string();
        assert!(!v["requestId"].as_str().unwrap().is_empty());
        assert!(!v["toolUseId"].as_str().unwrap().is_empty());

        engine
            .answer_question(
                &chat_id,
                v["requestId"].as_str().unwrap(),
                v["toolUseId"].as_str().unwrap(),
                v["questions"].clone(),
                serde_json::json!({ question: "Blue" }),
            )
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
        eprintln!("reply after answer: {reply}");
        assert!(reply.contains("Blue"), "answer did not reach the model: {reply}");
        assert!(matches!(updates.last().unwrap(),
            ChatUpdate::Status { status, .. } if status == "done"));
    }

    #[test]
    #[cfg_attr(windows, ignore = "runs the bash fake CLI; covered on macOS/Linux")]
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
    #[test]
    fn an_edit_is_worked_out_against_the_file_as_it_is() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        std::fs::create_dir_all(root.join("Platform")).unwrap();
        std::fs::write(root.join("Platform/Save.md"), "# Save\n\nSaves are files.\nOne per region.\n").unwrap();
        let file = root.join("Platform/Save.md").to_string_lossy().to_string();

        let edit = serde_json::json!({ "file_path": file, "old_string": "Saves are files.", "new_string": "Saves are region files." });
        let (path, rel, base, proposed) = propose_edit(root, "Edit", &edit).unwrap();
        assert_eq!(path, file);
        assert_eq!(rel.as_deref(), Some("Platform/Save.md"));
        assert!(base.contains("Saves are files.") && proposed.contains("Saves are region files."));

        let multi = serde_json::json!({ "file_path": "Platform/Save.md", "edits": [
            { "old_string": "# Save", "new_string": "# Saving" },
            { "old_string": "One per region.", "new_string": "One file per region." }
        ]});
        let (_, _, _, proposed) = propose_edit(root, "MultiEdit", &multi).unwrap();
        assert_eq!(proposed, "# Saving\n\nSaves are files.\nOne file per region.\n");

        let new = serde_json::json!({ "file_path": "Platform/New.md", "content": "# New\n" });
        let (_, rel, base, proposed) = propose_edit(root, "Write", &new).unwrap();
        assert_eq!((rel.as_deref(), base.as_str(), proposed.as_str()), (Some("Platform/New.md"), "", "# New\n"));

        let missing = serde_json::json!({ "file_path": "Platform/Save.md", "old_string": "not there", "new_string": "x" });
        assert!(propose_edit(root, "Edit", &missing).unwrap_err().contains("not in the file"));
        std::fs::write(root.join("twice.md"), "a a").unwrap();
        let twice = serde_json::json!({ "file_path": "twice.md", "old_string": "a", "new_string": "b" });
        assert!(propose_edit(root, "Edit", &twice).unwrap_err().contains("2 times"));
        let all = serde_json::json!({ "file_path": "twice.md", "old_string": "a", "new_string": "b", "replace_all": true });
        assert_eq!(propose_edit(root, "Edit", &all).unwrap().3, "b b");
    }
}

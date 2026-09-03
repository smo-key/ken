//! Ken's work pipeline (`openspec/changes/ken-pipeline`): lane definitions
//! that are *data* rather than a Rust enum, the gates that decide whether a
//! lane's agent may start, the loop-back accounting that stops a
//! tester↔programmer ping-pong, and the single Blocked mechanism every kind
//! of stuck work uses.
//!
//! Everything in this module is pure. No filesystem access, no clock, no
//! watcher — callers own all three (the same posture `tasks.rs` takes) and
//! supply dates/ids the way `memory.rs` does. Writes are expressed as a
//! [`crate::tasks::TaskPatch`] for the caller to hand to
//! `tasks::apply_patch_with_pipelines`, so **every** frontmatter write in
//! this feature goes through the S6 byte-fidelity patch core and nothing
//! here ever rebuilds a file.
//!
//! ## The two rules that carry the safety weight
//!
//! 1. **[`admit`] refuses a blocked ticket first** — before the lane's
//!    gate mode, before the concurrency cap, before anything (D3 brake 4,
//!    D5's hard invariant). Every path that could start work — UI kickoff,
//!    `pipeline_claim`, and any future auto-transition — calls this one
//!    function, so the invariant cannot be routed around. The ordering in
//!    [`admit`] is load-bearing, not stylistic; the tests assert the full
//!    cross-product including "blocked ticket in an `auto` lane with the
//!    pipeline master switch on".
//! 2. **A blocked ticket always has a return lane** — [`block`] captures it
//!    from the ticket's *current* lane and no caller can supply one. That
//!    is enforced by the shape of the API, not by convention: [`Block`]'s
//!    fields are private, its `return_lane` is a plain `String` rather than
//!    an `Option`, and this module contains its only constructors.
//!
//! ## What is deliberately absent
//!
//! There is **no `halted` state** (D4). A ticket that blew the retry cap
//! and a ticket waiting on an upstream release are the same thing to the
//! human who has to unstick them, so they share fields, lane, filters and
//! digest group. [`advance`] returns a *block* when the cap trips.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::tasks::{
    map_list, map_str, scalar_lines, seq_lines, split_fm_body, AttentionReason, NewTask, Task,
    TaskPatch, TaskStatus,
};

// ---------------------------------------------------------------------
// 1.1 Paths and defaults
// ---------------------------------------------------------------------

const PIPELINES_SUBDIR: &str = "pipelines";

/// OPEN-3, pinned: one heavy build at a time is this machine's real limit.
pub const DEFAULT_CONCURRENCY_CAP: u32 = 1;
/// OPEN-3, pinned: enough for a genuine fix-retest-fix, short enough to
/// catch a loop on the same day.
pub const DEFAULT_BOUNCE_CAP: u32 = 3;

/// The `block_reason` prefix D4 mandates for a retry-cap escalation. It is
/// a prefix rather than an exact string because the reason carries the
/// bounce count; [`is_retry_cap_reason`] is the only reader.
pub const RETRY_CAP_REASON: &str = "exceeded retry cap";

/// `.ken-workspace/pipelines/`, relative to the workspace parent folder —
/// the same convention as [`crate::tasks::workspace_tasks_dir`].
pub fn pipelines_dir(workspace_root: &Path) -> PathBuf {
    workspace_root
        .join(crate::workspace::CONFIG_DIR)
        .join(PIPELINES_SUBDIR)
}

/// `.ken-workspace/pipelines/<id>.md`.
pub fn pipeline_path(workspace_root: &Path, id: &str) -> PathBuf {
    pipelines_dir(workspace_root).join(format!("{id}.md"))
}

// ---------------------------------------------------------------------
// 1.1 Lane / pipeline model
// ---------------------------------------------------------------------

/// When Ken may start a lane's agent (D3 brake 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kickoff {
    /// Never starts itself.
    Manual,
    /// Ken proposes a run; a dialog showing lane, agent, model, scope and
    /// verify must be accepted.
    Confirm,
    /// Starts on lane entry — and only when the pipeline's `auto` master
    /// switch is also true (D6).
    Auto,
}

impl Kickoff {
    pub fn as_str(self) -> &'static str {
        match self {
            Kickoff::Manual => "manual",
            Kickoff::Confirm => "confirm",
            Kickoff::Auto => "auto",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "manual" => Some(Kickoff::Manual),
            "confirm" => Some(Kickoff::Confirm),
            "auto" => Some(Kickoff::Auto),
            _ => None,
        }
    }
}

/// How a lane's work reaches an agent (D14). v1 ships `mcp` only — Ken
/// spawns no processes (OPEN-1, user ruling 2026-08-03). `command` is
/// modelled so the definition file can express it and so a later change
/// touches one module, but nothing in ken-core acts on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Runner {
    Mcp,
    Command,
}

impl Runner {
    pub fn as_str(self) -> &'static str {
        match self {
            Runner::Mcp => "mcp",
            Runner::Command => "command",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "mcp" => Some(Runner::Mcp),
            "command" => Some(Runner::Command),
            _ => None,
        }
    }
}

/// One column. `id` is exactly the string a ticket's `status` holds — no
/// second key, no rename of anything `ken-tasks` already writes (D1).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Lane {
    pub id: String,
    pub name: String,
    /// The classic status this lane projects onto (D2), so every existing
    /// consumer keeps working untouched.
    pub maps_to: TaskStatus,
    /// Exactly what the file said, so a typo is visible rather than
    /// silently normalized — the same posture as `Task::status_raw`.
    pub maps_to_raw: String,
    /// `None` for a holding column (`agent: none` or absent). A lane with
    /// no agent can never start a run.
    pub agent: Option<String>,
    pub model: Option<String>,
    pub kickoff: Kickoff,
    pub kickoff_raw: String,
    pub on_pass: Option<String>,
    pub on_fail: Option<String>,
    pub writes_code: bool,
    /// D11: no agent, no kickoff, a review action instead of a run button.
    pub human: bool,
    /// Reaching this lane resolves dependents (D5 / OPEN-10).
    pub terminal: bool,
    /// D7: this lane files new ideas back into the ideas lane.
    pub generative: bool,
    /// D5: at most one lane in a pipeline may set this.
    pub blocked: bool,
    pub runner: Runner,
    #[serde(skip)]
    extra: serde_yaml::Mapping,
}

impl Lane {
    /// Unknown lane keys, preserved for display. Writes never rebuild a
    /// lane from this — the patch core leaves untouched lines alone.
    pub fn extra(&self) -> &serde_yaml::Mapping {
        &self.extra
    }

    /// A lane that can never start a run, whatever the gates say: no
    /// agent, a human lane, or the blocked lane itself.
    pub fn is_holding(&self) -> bool {
        self.agent.is_none() || self.human || self.blocked
    }
}

/// One ordered lane set, parsed from `.ken-workspace/pipelines/<id>.md`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Pipeline {
    pub id: String,
    pub name: String,
    /// The master switch (D6). Ships `false`; `auto` lanes are treated as
    /// `confirm` until it is true.
    pub auto: bool,
    pub concurrency_cap: u32,
    pub bounce_cap: u32,
    pub lanes: Vec<Lane>,
    /// The per-lane brief material handed to agents at kickoff (D1: "the
    /// body is not decoration").
    pub body: String,
    pub path: PathBuf,
    #[serde(skip)]
    extra: serde_yaml::Mapping,
}

impl Pipeline {
    pub fn extra(&self) -> &serde_yaml::Mapping {
        &self.extra
    }

    pub fn lane(&self, id: &str) -> Option<&Lane> {
        resolve_lane(self, id)
    }

    /// The one lane with `blocked: true`, if the definition declares one.
    pub fn blocked_lane(&self) -> Option<&Lane> {
        self.lanes.iter().find(|l| l.blocked)
    }

    /// The one lane with `human: true`, if the definition declares one.
    pub fn human_lane(&self) -> Option<&Lane> {
        self.lanes.iter().find(|l| l.human)
    }
}

// ---------------------------------------------------------------------
// 1.1 Parse
// ---------------------------------------------------------------------

/// Keys the model owns at the pipeline level; everything else in the
/// frontmatter is preserved as an unknown key.
const PIPELINE_KEYS: &[&str] = &["id", "name", "auto", "concurrency_cap", "bounce_cap", "lanes"];

/// Keys the model owns per lane.
const LANE_KEYS: &[&str] = &[
    "id",
    "name",
    "maps_to",
    "agent",
    "model",
    "kickoff",
    "on_pass",
    "on_fail",
    "writes_code",
    "human",
    "terminal",
    "generative",
    "blocked",
    "runner",
];

fn m_bool(m: &serde_yaml::Mapping, key: &str) -> bool {
    match m.get(serde_yaml::Value::String(key.to_string())) {
        Some(serde_yaml::Value::Bool(b)) => *b,
        // A hand edit like `human: "true"` should still read as true.
        Some(serde_yaml::Value::String(s)) => s.trim().eq_ignore_ascii_case("true"),
        _ => false,
    }
}

fn m_u32(m: &serde_yaml::Mapping, key: &str) -> Option<u32> {
    match m.get(serde_yaml::Value::String(key.to_string())) {
        Some(serde_yaml::Value::Number(n)) => n.as_u64().map(|v| v as u32),
        Some(serde_yaml::Value::String(s)) => s.trim().parse::<u32>().ok(),
        _ => None,
    }
}

fn without_keys(m: &serde_yaml::Mapping, known: &[&str]) -> serde_yaml::Mapping {
    let mut out = m.clone();
    for k in known {
        out.remove(serde_yaml::Value::String((*k).to_string()));
    }
    out
}

/// `none` / empty reads as "no value" — D1 writes `agent: none` for a
/// holding column and that must not become an agent literally named
/// "none".
fn opt_word(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("none") {
        None
    } else {
        Some(t.to_string())
    }
}

fn parse_lane(m: &serde_yaml::Mapping) -> Lane {
    let agent = opt_word(&map_str(m, "agent"));
    let kickoff_raw = map_str(m, "kickoff").trim().to_string();
    let maps_to_raw = map_str(m, "maps_to").trim().to_string();
    Lane {
        id: map_str(m, "id").trim().to_string(),
        name: map_str(m, "name").trim().to_string(),
        // An unparseable `maps_to` falls back to the intake column so the
        // board still renders, and `validate_pipeline` reports it. The raw
        // value is kept so the report can name the typo.
        maps_to: TaskStatus::parse(&maps_to_raw).unwrap_or(TaskStatus::Backlog),
        maps_to_raw,
        // D3: `confirm` is the default for any lane with an agent;
        // agentless holding columns default to `manual`.
        kickoff: Kickoff::parse(&kickoff_raw).unwrap_or(if agent.is_some() {
            Kickoff::Confirm
        } else {
            Kickoff::Manual
        }),
        kickoff_raw,
        agent,
        model: opt_word(&map_str(m, "model")),
        on_pass: opt_word(&map_str(m, "on_pass")),
        on_fail: opt_word(&map_str(m, "on_fail")),
        writes_code: m_bool(m, "writes_code"),
        human: m_bool(m, "human"),
        terminal: m_bool(m, "terminal"),
        generative: m_bool(m, "generative"),
        blocked: m_bool(m, "blocked"),
        runner: Runner::parse(&map_str(m, "runner")).unwrap_or(Runner::Mcp),
        extra: without_keys(m, LANE_KEYS),
    }
}

/// Parse a pipeline definition file. Infallible and tolerant, exactly like
/// [`crate::tasks::parse_task`]: a malformed definition degrades to
/// something renderable that [`validate_pipeline`] can complain about,
/// rather than an error that takes the board down.
///
/// Read-side only: the frontmatter is read with `serde_yaml`, and the only
/// write path is [`patch_pipeline_text`], which is the S6 raw line
/// splitter. Unknown keys, key order, and the body therefore survive every
/// programmatic rewrite byte-for-byte.
pub fn parse_pipeline(path: &Path, raw: &str) -> Pipeline {
    let (fm, body) = split_fm_body(raw);
    let map = fm
        .and_then(|f| serde_yaml::from_str::<serde_yaml::Mapping>(f).ok())
        .unwrap_or_default();

    let lanes = match map.get(serde_yaml::Value::String("lanes".to_string())) {
        Some(serde_yaml::Value::Sequence(items)) => items
            .iter()
            .filter_map(|v| match v {
                serde_yaml::Value::Mapping(m) => Some(parse_lane(m)),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };

    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let id = {
        let declared = map_str(&map, "id").trim().to_string();
        if declared.is_empty() {
            stem
        } else {
            declared
        }
    };

    Pipeline {
        name: {
            let n = map_str(&map, "name").trim().to_string();
            if n.is_empty() {
                id.clone()
            } else {
                n
            }
        },
        id,
        auto: m_bool(&map, "auto"),
        concurrency_cap: m_u32(&map, "concurrency_cap").unwrap_or(DEFAULT_CONCURRENCY_CAP),
        bounce_cap: m_u32(&map, "bounce_cap").unwrap_or(DEFAULT_BOUNCE_CAP),
        lanes,
        body: body.trim().to_string(),
        path: path.to_path_buf(),
        extra: without_keys(&map, PIPELINE_KEYS),
    }
}

/// Rewrite named top-level keys of a definition file through the S6 patch
/// core. Ken barely writes these files — they are human-owned — but the
/// `auto` master switch and cap values are legitimately toggled from the
/// UI, and when they are, every other byte must survive.
pub fn patch_pipeline_text(raw: &str, edits: &[(&str, Vec<String>)]) -> String {
    crate::tasks::patch_text(raw, edits, None)
}

// ---------------------------------------------------------------------
// 1.1 Validation
// ---------------------------------------------------------------------

/// Why a definition file can't be used as written. Surfaced, never
/// repaired — a definition is human-owned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "issue")]
pub enum PipelineIssue {
    NoLanes,
    LaneMissingId { index: usize },
    DuplicateLaneId { id: String },
    /// D5: at most one lane may be the blocked lane.
    MultipleBlockedLanes { first: String, second: String },
    /// D11: at most one lane may be the human lane.
    MultipleHumanLanes { first: String, second: String },
    InvalidMapsTo { lane: String, value: String },
    InvalidKickoff { lane: String, value: String },
    /// `on_pass`/`on_fail` naming a lane the definition doesn't declare.
    UnknownTransition { lane: String, edge: String, target: String },
}

/// Everything wrong with a definition, in a stable order. Empty ⇒ usable.
pub fn validate_pipeline(pipeline: &Pipeline) -> Vec<PipelineIssue> {
    let mut out = Vec::new();
    if pipeline.lanes.is_empty() {
        out.push(PipelineIssue::NoLanes);
        return out;
    }

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut blocked_first: Option<&str> = None;
    let mut human_first: Option<&str> = None;

    for (index, lane) in pipeline.lanes.iter().enumerate() {
        if lane.id.is_empty() {
            out.push(PipelineIssue::LaneMissingId { index });
            continue;
        }
        if !seen.insert(lane.id.to_ascii_lowercase()) {
            out.push(PipelineIssue::DuplicateLaneId { id: lane.id.clone() });
        }
        if !lane.maps_to_raw.is_empty() && TaskStatus::parse(&lane.maps_to_raw).is_none() {
            out.push(PipelineIssue::InvalidMapsTo {
                lane: lane.id.clone(),
                value: lane.maps_to_raw.clone(),
            });
        }
        if !lane.kickoff_raw.is_empty() && Kickoff::parse(&lane.kickoff_raw).is_none() {
            out.push(PipelineIssue::InvalidKickoff {
                lane: lane.id.clone(),
                value: lane.kickoff_raw.clone(),
            });
        }
        if lane.blocked {
            match blocked_first {
                None => blocked_first = Some(lane.id.as_str()),
                Some(first) => out.push(PipelineIssue::MultipleBlockedLanes {
                    first: first.to_string(),
                    second: lane.id.clone(),
                }),
            }
        }
        if lane.human {
            match human_first {
                None => human_first = Some(lane.id.as_str()),
                Some(first) => out.push(PipelineIssue::MultipleHumanLanes {
                    first: first.to_string(),
                    second: lane.id.clone(),
                }),
            }
        }
    }

    for lane in &pipeline.lanes {
        for (edge, target) in [("on_pass", &lane.on_pass), ("on_fail", &lane.on_fail)] {
            if let Some(t) = target {
                if resolve_lane(pipeline, t).is_none() {
                    out.push(PipelineIssue::UnknownTransition {
                        lane: lane.id.clone(),
                        edge: edge.to_string(),
                        target: t.clone(),
                    });
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------
// 1.2 Lane resolution — the single home of the board-scoped vocabulary
// ---------------------------------------------------------------------

fn eq_ci(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// **The** lane vocabulary check (D2). Nothing else in the codebase parses
/// a lane id: if you need to know whether a status string names a lane,
/// call this.
pub fn resolve_lane<'a>(pipeline: &'a Pipeline, status_raw: &str) -> Option<&'a Lane> {
    let s = status_raw.trim();
    if s.is_empty() {
        // Mirrors `parse_task`'s "absent status ⇒ the intake column": for a
        // board-scoped vocabulary the intake column is whichever lane the
        // human put first, since lane order *is* column order (D1).
        return pipeline.lanes.first();
    }
    pipeline.lanes.iter().find(|l| eq_ci(&l.id, s))
}

/// Position of a lane in definition order — the ordering [`advance`] uses
/// to classify a transition as backward (D4).
pub fn lane_index(pipeline: &Pipeline, lane_id: &str) -> Option<usize> {
    pipeline.lanes.iter().position(|l| eq_ci(&l.id, lane_id))
}

pub fn find_pipeline<'a>(pipelines: &'a [Pipeline], id: &str) -> Option<&'a Pipeline> {
    pipelines.iter().find(|p| eq_ci(&p.id, id))
}

/// The `maps_to` projection (D2): a lane id in, a classic [`TaskStatus`]
/// out, so the classic Kanban, `task_list`'s status filter, the daily
/// board and goal progress all keep working on pipeline tickets.
pub fn maps_to(pipeline: &Pipeline, status_raw: &str) -> Option<TaskStatus> {
    resolve_lane(pipeline, status_raw).map(|l| l.maps_to)
}

/// Fill in [`Task::lane`] and re-derive [`Task::status`] from the matched
/// lane's `maps_to`.
///
/// A ticket with no `pipeline:` key is left **completely** untouched — the
/// classic path is not merely equivalent, it is not executed at all. A
/// ticket naming an unknown pipeline, or sitting in a lane the definition
/// doesn't declare, gets `status: None`, which is exactly how `tasks.rs`
/// already spells "needs attention, do not rewrite".
pub fn resolve_task_lane(task: &mut Task, pipelines: &[Pipeline]) {
    let Some(pipeline_id) = ticket_pipeline(task) else {
        return;
    };
    let Some(pipeline) = find_pipeline(pipelines, &pipeline_id) else {
        task.lane = None;
        task.status = None;
        return;
    };
    match resolve_lane(pipeline, &task.status_raw) {
        Some(lane) => {
            task.lane = Some(lane.id.clone());
            task.status = Some(lane.maps_to);
        }
        None => {
            task.lane = None;
            task.status = None;
        }
    }
}

/// [`resolve_task_lane`] over a whole scanned board.
pub fn resolve_board(tasks: &mut [Task], pipelines: &[Pipeline]) {
    for task in tasks.iter_mut() {
        resolve_task_lane(task, pipelines);
    }
}

// ---------------------------------------------------------------------
// 1.5 Ticket pipeline fields
// ---------------------------------------------------------------------

/// Where a ticket's demo/recording recipe comes from (D10).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    Web,
    Tauri,
    /// Written walkthrough only — always available, never blocked on a
    /// driver. The default.
    #[default]
    None,
}

impl Target {
    pub fn as_str(self) -> &'static str {
        match self {
            Target::Web => "web",
            Target::Tauri => "tauri",
            Target::None => "none",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "web" => Some(Target::Web),
            "tauri" => Some(Target::Tauri),
            "none" => Some(Target::None),
            _ => None,
        }
    }
}

/// The ken-pipeline half of a ticket's frontmatter. Every one of these
/// rides the `extra` flatten `ken-tasks` already round-trips, so a Ken
/// build without this feature reads and rewrites a pipeline ticket without
/// losing a byte.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TicketFields {
    pub pipeline: Option<String>,
    pub model: Option<String>,
    pub agent: Option<String>,
    /// Path globs — the file boundary a lane's agent works inside (D3).
    pub scope: Vec<String>,
    /// The command that proves the lane's work. Ken never runs it; it
    /// hands the string to the agent and records the reported result.
    pub verify: Option<String>,
    pub bounces: u32,
    pub return_lane: Option<String>,
    /// Ticket ULIDs, never paths (D5).
    pub blocked_by: Vec<String>,
    pub block_reason: Option<String>,
    pub blocked_at: Option<String>,
    pub parent: Option<String>,
    pub spawned_by: Option<String>,
    pub origin: Option<String>,
    pub projects: Vec<String>,
    pub target: Target,
    pub target_raw: String,
}

/// Read the ken-pipeline keys off a parsed ticket.
pub fn ticket_fields(task: &Task) -> TicketFields {
    let m = task.extra();
    let target_raw = map_str(m, "target").trim().to_string();
    TicketFields {
        pipeline: non_empty(&map_str(m, "pipeline")),
        model: non_empty(&map_str(m, "model")),
        agent: opt_word(&map_str(m, "agent")),
        scope: map_list(m, "scope"),
        verify: non_empty(&map_str(m, "verify")),
        bounces: map_str(m, "bounces").trim().parse::<u32>().unwrap_or(0),
        return_lane: non_empty(&map_str(m, "return_lane")),
        blocked_by: map_list(m, "blocked_by"),
        block_reason: non_empty(&map_str(m, "block_reason")),
        blocked_at: non_empty(&map_str(m, "blocked_at")),
        parent: non_empty(&map_str(m, "parent")),
        spawned_by: non_empty(&map_str(m, "spawned_by")),
        origin: non_empty(&map_str(m, "origin")),
        projects: map_list(m, "projects"),
        target: Target::parse(&target_raw).unwrap_or(Target::None),
        target_raw,
    }
}

/// The ticket's `pipeline:` id, or `None` for a classic ticket. Cheap
/// enough to call from `tasks::matches`.
pub fn ticket_pipeline(task: &Task) -> Option<String> {
    non_empty(&map_str(task.extra(), "pipeline"))
}

fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// D4's retry-cap escalation, recognised by its reason prefix. The only
/// place the two kinds of stuck work are told apart — and they are told
/// apart for exactly one purpose: unblocking a retry-capped ticket resets
/// `bounces`, while unblocking a dependency-blocked one must not erase its
/// bounce history.
pub fn is_retry_cap_reason(reason: &str) -> bool {
    reason.trim().to_ascii_lowercase().starts_with(RETRY_CAP_REASON)
}

// ---------------------------------------------------------------------
// 1.3 Block-state filter support (called from `tasks::matches`)
// ---------------------------------------------------------------------

/// True when the ticket carries block evidence in its own frontmatter.
/// Pure over one ticket, so `tasks::matches` can use it.
pub fn has_block_evidence(task: &Task) -> bool {
    let f = ticket_fields(task);
    !f.blocked_by.is_empty() || f.block_reason.is_some()
}

/// Evaluate a [`crate::tasks::BlockedFilter`] against one ticket.
pub fn matches_block_filter(task: &Task, filter: &crate::tasks::BlockedFilter) -> bool {
    use crate::tasks::BlockedFilter as B;
    let f = ticket_fields(task);
    let blocked = !f.blocked_by.is_empty() || f.block_reason.is_some();
    match filter {
        B::Any => true,
        B::Blocked => blocked,
        B::NotBlocked => !blocked,
        B::By(id) => f.blocked_by.iter().any(|b| eq_ci(b, id)),
        // "Was blocked, isn't any more, hasn't been moved home yet": a
        // surviving `return_lane` with nothing left holding it.
        B::NewlyUnblocked => !blocked && f.return_lane.is_some(),
    }
}

// ---------------------------------------------------------------------
// 1.4 Tray reasons (called from `tasks::needs_attention_with_pipelines`)
// ---------------------------------------------------------------------

/// The ken-pipeline half of the needs-attention tray. Every reason here is
/// a *hand-edit or lane-rename orphan*: surfaced with the file untouched,
/// never migrated (D2's "silent bulk status rewrites are exactly what the
/// patch core exists to prevent").
pub fn attention_reasons(
    task: &Task,
    board: &[Task],
    pipelines: &[Pipeline],
) -> Vec<AttentionReason> {
    let mut out = Vec::new();
    let fields = ticket_fields(task);
    let Some(pipeline_id) = fields.pipeline.clone() else {
        // A classic ticket has no pipeline obligations at all.
        return out;
    };
    let Some(pipeline) = find_pipeline(pipelines, &pipeline_id) else {
        out.push(AttentionReason::UnknownPipeline(pipeline_id));
        return out;
    };

    let lane = resolve_lane(pipeline, &task.status_raw);
    if lane.is_none() {
        out.push(AttentionReason::UnknownLane(task.status_raw.clone()));
    }

    // A blocked ticket must know where it is going home to. Both the
    // "hand edit produced `status: blocked` with no return lane" case and
    // the "someone renamed the lane the return_lane pointed at" case land
    // here.
    let in_blocked_lane = lane.map(|l| l.blocked).unwrap_or(false);
    let blocked = in_blocked_lane || !fields.blocked_by.is_empty() || fields.block_reason.is_some();
    match &fields.return_lane {
        Some(rl) if resolve_lane(pipeline, rl).is_none() => {
            out.push(AttentionReason::UnknownReturnLane(rl.clone()));
        }
        None if blocked => {
            out.push(AttentionReason::UnknownReturnLane(String::new()));
        }
        _ => {}
    }

    for blocker in &fields.blocked_by {
        if !board.iter().any(|t| eq_ci(&t.id, blocker)) {
            out.push(AttentionReason::UnknownBlocker(blocker.clone()));
        }
    }
    out
}

// ---------------------------------------------------------------------
// 1.7 Write-time cycle detection (D5)
// ---------------------------------------------------------------------

fn norm(id: &str) -> String {
    id.trim().to_ascii_uppercase()
}

/// The dependency graph over blocked tickets: ticket → the tickets it is
/// blocked by. Small by construction (only blocked tickets have edges),
/// which is why a depth-first walk at write time is affordable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockGraph {
    edges: BTreeMap<String, Vec<String>>,
}

impl BlockGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build from a scanned board. Only `blocked_by` matters — a
    /// `block_reason` is not an edge, it is a note to a human.
    pub fn from_tasks(tasks: &[Task]) -> Self {
        let mut g = Self::new();
        for task in tasks {
            let deps = ticket_fields(task).blocked_by;
            if !deps.is_empty() {
                g.insert(&task.id, &deps);
            }
        }
        g
    }

    pub fn insert(&mut self, ticket: &str, blockers: &[String]) {
        self.edges.insert(norm(ticket), blockers.to_vec());
    }

    pub fn blockers(&self, ticket: &str) -> &[String] {
        self.edges
            .get(&norm(ticket))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }
}

/// The chain an offered edge would close, in walk order and ending where
/// it started, so the error message can show a human the whole loop rather
/// than the one hop they happened to type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CyclePath {
    pub path: Vec<String>,
}

impl CyclePath {
    /// `A → B → C → A`, reading as "A is blocked by B is blocked by C is
    /// blocked by A".
    pub fn render(&self) -> String {
        self.path.join(" → ")
    }
}

impl std::fmt::Display for CyclePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.render())
    }
}

/// Would "`from` is blocked by `to`" close a cycle?
///
/// Refusing here — at write time, before any file is touched — is the
/// whole point (D5): a cycle discovered later is a board that has quietly
/// stopped moving. `Err` carries the full path, never just the offending
/// hop.
pub fn check_cycle(graph: &BlockGraph, from: &str, to: &str) -> Result<(), CyclePath> {
    if norm(from) == norm(to) {
        return Err(CyclePath {
            path: vec![from.trim().to_string(), to.trim().to_string()],
        });
    }
    let target = norm(from);
    let mut path = vec![from.trim().to_string(), to.trim().to_string()];
    let mut seen: BTreeSet<String> = BTreeSet::new();
    seen.insert(norm(to));
    if walk(graph, &norm(to), &target, &mut path, &mut seen) {
        return Err(CyclePath { path });
    }
    Ok(())
}

/// Depth-first over `blocked_by`. `seen` both bounds the walk and makes it
/// safe against a cycle that is already on disk (a hand edit can create
/// one this function was never asked about).
fn walk(
    graph: &BlockGraph,
    current: &str,
    target: &str,
    path: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
) -> bool {
    for blocker in graph.blockers(current) {
        let b = norm(blocker);
        path.push(blocker.trim().to_string());
        if b == *target {
            return true;
        }
        if seen.insert(b.clone()) && walk(graph, &b, target, path, seen) {
            return true;
        }
        path.pop();
    }
    false
}

// ---------------------------------------------------------------------
// 1.6 The block model (D5)
// ---------------------------------------------------------------------

/// What a caller may ask for when blocking. Note what is **not** here:
/// `return_lane`. It is captured by [`block`] from the ticket's current
/// lane and there is no way to pass one in — that is the API-shape
/// enforcement D5 asks for, rather than a convention a future caller could
/// forget.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BlockRequest {
    /// Ticket ULIDs to add to the existing set.
    pub blocked_by: Vec<String>,
    pub reason: Option<String>,
    /// Caller-supplied timestamp (this module owns no clock), written to
    /// `blocked_at` so the digest can age the entry.
    pub now: String,
}

/// A recorded block. Constructible **only** by [`block`] and [`advance`]
/// inside this module: the fields are private, there is no public
/// constructor, and `return_lane` is a `String` rather than an
/// `Option<String>`. So "a blocked ticket with no return lane" is not a
/// state a caller can reach — it is a state that does not typecheck.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Block {
    return_lane: String,
    blocked_by: Vec<String>,
    reason: Option<String>,
    blocked_at: String,
}

impl Block {
    /// The lane this ticket will resume in — always present.
    pub fn return_lane(&self) -> &str {
        &self.return_lane
    }
    pub fn blocked_by(&self) -> &[String] {
        &self.blocked_by
    }
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
    pub fn blocked_at(&self) -> &str {
        &self.blocked_at
    }
}

/// Why a block was refused. Every variant leaves the ticket file
/// untouched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "refusal")]
pub enum BlockRefusal {
    /// The definition declares no `blocked: true` lane, so there is
    /// nowhere to put stuck work.
    NoBlockedLane,
    /// The ticket's `status` names no lane, so there is no current lane to
    /// capture as the return lane. Struct variant for the serde reason
    /// documented on `RefusalReason`.
    UnknownLane { status: String },
    /// Extending a block on a ticket already in the blocked lane whose
    /// `return_lane` is missing or orphaned. It is a tray entry, and it is
    /// not this function's job to invent one.
    MissingReturnLane,
    /// Neither a dependency nor a reason — nothing to be blocked on.
    Empty,
    /// A `blocked_by` entry that is a path, not a ticket id (D5).
    PathBlocker(String),
    /// A `blocked_by` entry that isn't a ULID.
    MalformedBlocker(String),
    /// A ticket cannot block itself.
    SelfBlock(String),
    /// The edge would close a dependency cycle.
    Cycle(CyclePath),
}

/// The result of asking to block a ticket.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum BlockOutcome {
    Blocked {
        block: Block,
        /// Exactly the frontmatter keys to write, and no others.
        patch: TaskPatch,
        /// The `## Log` line the caller composes into the body.
        log: String,
    },
    Refused {
        reason: BlockRefusal,
    },
}

/// ULID shape: 26 Crockford base32 characters. Deliberately strict —
/// anything path-shaped fails it, which is the point (D5: "`blocked_by`
/// holds ULIDs, never paths").
fn is_ulid_like(s: &str) -> bool {
    let t = s.trim();
    t.len() == 26
        && t.bytes()
            .all(|b| b.is_ascii_digit() || (b.is_ascii_alphabetic() && !matches!(b.to_ascii_uppercase(), b'I' | b'L' | b'O' | b'U')))
}

fn looks_like_path(s: &str) -> bool {
    let t = s.trim();
    t.contains('/') || t.contains('\\') || t.contains('.') || t.contains(':')
}

/// Block a ticket (D5).
///
/// `return_lane` is captured here, from the ticket's **current** lane,
/// before the move — the caller has no say in it. Extending a block on a
/// ticket that is already in the blocked lane preserves the return lane it
/// already recorded rather than overwriting it with the blocked lane
/// itself, which would silently lose where the work belonged.
///
/// Cycle detection (1.7) runs before anything is written, per offered
/// edge, and a refusal means no patch at all.
pub fn block(
    ticket: &Task,
    pipeline: &Pipeline,
    graph: &BlockGraph,
    request: &BlockRequest,
) -> BlockOutcome {
    let refuse = |reason| BlockOutcome::Refused { reason };

    let Some(blocked_lane) = pipeline.blocked_lane() else {
        return refuse(BlockRefusal::NoBlockedLane);
    };
    let Some(current) = resolve_lane(pipeline, &ticket.status_raw) else {
        return refuse(BlockRefusal::UnknownLane { status: ticket.status_raw.clone() });
    };

    let existing = ticket_fields(ticket);

    // --- return lane capture: the one field the caller cannot supply ---
    let return_lane = if current.blocked {
        match existing
            .return_lane
            .as_deref()
            .filter(|rl| resolve_lane(pipeline, rl).is_some())
        {
            Some(rl) => rl.to_string(),
            None => return refuse(BlockRefusal::MissingReturnLane),
        }
    } else {
        current.id.clone()
    };

    // --- validate and merge dependencies ---
    let mut deps: Vec<String> = existing.blocked_by.clone();
    for raw in &request.blocked_by {
        let dep = raw.trim();
        if dep.is_empty() {
            continue;
        }
        if looks_like_path(dep) {
            return refuse(BlockRefusal::PathBlocker(dep.to_string()));
        }
        if !is_ulid_like(dep) {
            return refuse(BlockRefusal::MalformedBlocker(dep.to_string()));
        }
        if eq_ci(dep, &ticket.id) {
            return refuse(BlockRefusal::SelfBlock(dep.to_string()));
        }
        if deps.iter().any(|d| eq_ci(d, dep)) {
            continue;
        }
        // Write-time refusal, before any file is touched. Checked against
        // the graph *plus* the edges we are about to add, so a request
        // that closes a cycle only in combination with its own siblings is
        // still caught.
        let mut probe = graph.clone();
        probe.insert(&ticket.id, &deps);
        if let Err(cycle) = check_cycle(&probe, &ticket.id, dep) {
            return refuse(BlockRefusal::Cycle(cycle));
        }
        deps.push(dep.to_string());
    }

    let reason = match request.reason.as_deref().map(str::trim) {
        Some(r) if !r.is_empty() => Some(r.to_string()),
        Some(_) => existing.block_reason.clone(),
        None => existing.block_reason.clone(),
    };

    if deps.is_empty() && reason.is_none() {
        return refuse(BlockRefusal::Empty);
    }

    let block = Block {
        return_lane,
        blocked_by: deps,
        reason,
        blocked_at: request.now.trim().to_string(),
    };
    let patch = block_patch(blocked_lane, &block, None);
    let log = block_log(&block);
    BlockOutcome::Blocked { block, patch, log }
}

/// The exact frontmatter keys a block writes — `status`, `return_lane`,
/// `blocked_by`, `block_reason`, `blocked_at`, plus `bounces` when the
/// caller is D4's retry-cap path. Nothing else.
fn block_patch(blocked_lane: &Lane, block: &Block, bounces: Option<u32>) -> TaskPatch {
    TaskPatch {
        lane: Some(blocked_lane.id.clone()),
        return_lane: Some(block.return_lane.clone()),
        blocked_by: Some(block.blocked_by.clone()),
        // Cleared to empty rather than removed: the patch core has no
        // "remove key" verb on purpose (it would move the file's key
        // order), so an absent reason is written as `block_reason: ''`.
        block_reason: Some(block.reason.clone().unwrap_or_default()),
        blocked_at: Some(block.blocked_at.clone()),
        bounces,
        ..TaskPatch::default()
    }
}

fn block_log(block: &Block) -> String {
    let mut parts = vec![format!("blocked · returns to {}", block.return_lane)];
    if !block.blocked_by.is_empty() {
        parts.push(format!("blocked by {}", block.blocked_by.join(", ")));
    }
    if let Some(r) = &block.reason {
        parts.push(r.clone());
    }
    parts.join(" — ")
}

/// What to clear when unblocking. The two are independent because
/// dependencies and reasons coexist (D5): clearing one must not unblock a
/// ticket the other still applies to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UnblockRequest {
    pub clear_deps: bool,
    pub clear_reason: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "refusal")]
pub enum UnblockRefusal {
    NotBlocked,
    /// The ticket is blocked but its `return_lane` is missing or names a
    /// lane the definition no longer declares — a tray entry, not
    /// something to guess at.
    MissingReturnLane,
    /// The request clears neither dependencies nor the reason.
    Empty,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum UnblockOutcome {
    /// Nothing is holding the ticket any more. **The caller must re-enter
    /// it with [`EntryKind::Unblock`]** — never with a start.
    Released {
        return_lane: String,
        patch: TaskPatch,
        /// D4/spec: unblocking a retry-capped ticket resets `bounces`.
        /// A dependency-blocked ticket keeps its bounce history.
        reset_bounces: bool,
        log: String,
    },
    /// One of the two holds cleared, the other still applies.
    StillBlocked {
        remaining_deps: Vec<String>,
        remaining_reason: Option<String>,
        patch: TaskPatch,
    },
    Refused {
        reason: UnblockRefusal,
    },
}

/// Clear a block's dependencies and/or its reason (D5).
pub fn unblock(ticket: &Task, pipeline: &Pipeline, request: &UnblockRequest) -> UnblockOutcome {
    let refuse = |reason| UnblockOutcome::Refused { reason };
    if !request.clear_deps && !request.clear_reason {
        return refuse(UnblockRefusal::Empty);
    }

    let fields = ticket_fields(ticket);
    let in_blocked_lane = resolve_lane(pipeline, &ticket.status_raw)
        .map(|l| l.blocked)
        .unwrap_or(false);
    if !in_blocked_lane && fields.blocked_by.is_empty() && fields.block_reason.is_none() {
        return refuse(UnblockRefusal::NotBlocked);
    }

    let deps = if request.clear_deps {
        Vec::new()
    } else {
        fields.blocked_by.clone()
    };
    let reason = if request.clear_reason {
        None
    } else {
        fields.block_reason.clone()
    };

    if !deps.is_empty() || reason.is_some() {
        return UnblockOutcome::StillBlocked {
            patch: TaskPatch {
                blocked_by: Some(deps.clone()),
                block_reason: Some(reason.clone().unwrap_or_default()),
                ..TaskPatch::default()
            },
            remaining_deps: deps,
            remaining_reason: reason,
        };
    }

    let Some(return_lane) = fields
        .return_lane
        .as_deref()
        .filter(|rl| resolve_lane(pipeline, rl).is_some())
    else {
        return refuse(UnblockRefusal::MissingReturnLane);
    };

    let reset_bounces = fields
        .block_reason
        .as_deref()
        .map(is_retry_cap_reason)
        .unwrap_or(false);

    UnblockOutcome::Released {
        return_lane: return_lane.to_string(),
        patch: TaskPatch {
            lane: Some(return_lane.to_string()),
            blocked_by: Some(Vec::new()),
            block_reason: Some(String::new()),
            blocked_at: Some(String::new()),
            // `return_lane` is deliberately *left in place*: it costs
            // nothing, and it is what `BlockedFilter::NewlyUnblocked`
            // reads to answer "what freed up overnight".
            bounces: if reset_bounces { Some(0) } else { None },
            ..TaskPatch::default()
        },
        reset_bounces,
        log: format!("unblocked · returning to {return_lane}"),
    }
}

// ---------------------------------------------------------------------
// 1.11 (partial) Run record model — the shape `admit` needs
// ---------------------------------------------------------------------

/// A run's lifecycle state (D13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunOutcome {
    Queued,
    Running,
    Pass,
    Fail,
    Blocked,
    Cancelled,
}

impl RunOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            RunOutcome::Queued => "queued",
            RunOutcome::Running => "running",
            RunOutcome::Pass => "pass",
            RunOutcome::Fail => "fail",
            RunOutcome::Blocked => "blocked",
            RunOutcome::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "queued" => Some(RunOutcome::Queued),
            "running" => Some(RunOutcome::Running),
            "pass" => Some(RunOutcome::Pass),
            "fail" => Some(RunOutcome::Fail),
            "blocked" => Some(RunOutcome::Blocked),
            "cancelled" => Some(RunOutcome::Cancelled),
            _ => None,
        }
    }

    /// A run occupying a concurrency slot right now.
    pub fn is_live(self) -> bool {
        matches!(self, RunOutcome::Running)
    }
}

/// One append-only ledger entry (D13). **Model only** — the pathing
/// (`runs/YYYY-MM/<ulid>.md`), the ledger scan, and the derived queue view
/// including stale detection are tasks.md 1.11 and are deliberately not
/// here. The struct carries the full field list from the spec so 1.11 can
/// adopt it as-is; `admit` reads `outcome` and nothing else.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRecord {
    pub id: String,
    pub ticket: String,
    pub pipeline: String,
    pub lane: String,
    pub agent: String,
    pub model: String,
    pub scope: Vec<String>,
    pub verify: String,
    pub started: String,
    pub ended: String,
    pub outcome: Option<RunOutcome>,
    pub outcome_raw: String,
    pub artifacts: Vec<String>,
    /// The agent's report — the file body.
    pub report: String,
}

/// How many concurrency slots the ledger currently occupies (D3 brake 2).
pub fn running_runs(runs: &[RunRecord]) -> usize {
    runs.iter()
        .filter(|r| r.outcome.map(RunOutcome::is_live).unwrap_or(false))
        .count()
}

// ---------------------------------------------------------------------
// 1.11 Run ledger: pathing, parse/write, ledger scan, derived queue,
// stale detection (D13)
// ---------------------------------------------------------------------

const RUNS_SUBDIR: &str = "runs";

/// `.ken-workspace/runs/`, the append-only ledger root (D13).
pub fn runs_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join(crate::workspace::CONFIG_DIR).join(RUNS_SUBDIR)
}

/// `.ken-workspace/runs/<yyyy-mm>/` — monthly folders mirror ken-tasks'
/// `archive/YYYY-MM/` convention, keeping the directory listable.
pub fn run_month_dir(workspace_root: &Path, yyyy_mm: &str) -> PathBuf {
    runs_dir(workspace_root).join(yyyy_mm)
}

/// `.ken-workspace/runs/<yyyy-mm>/<ulid>.md`. `yyyy_mm` is caller-supplied
/// (this module owns no clock, same convention as `tasks::create_task`'s
/// `today`); see [`run_month`] to derive it from a run's `started` value.
pub fn run_path(workspace_root: &Path, yyyy_mm: &str, id: &str) -> PathBuf {
    run_month_dir(workspace_root, yyyy_mm).join(format!("{id}.md"))
}

/// The `yyyy-mm` folder a run files under, taken from the first 7
/// characters of its (caller-supplied) `started` timestamp. Falls back to
/// the input unchanged if it's too short to slice — defensive only; every
/// real caller passes a validated ISO date/datetime.
pub fn run_month(started: &str) -> String {
    started.get(0..7).unwrap_or(started).to_string()
}

/// Parse a run-record file. Infallible and tolerant, the same posture as
/// [`parse_pipeline`]/`tasks::parse_task`: a malformed record degrades to
/// something renderable rather than an error that takes the tray down. An
/// empty/missing `id` key falls back to the file stem, mirroring
/// [`parse_pipeline`]'s same fallback for a definition's `id`.
pub fn parse_run(path: &Path, raw: &str) -> RunRecord {
    let (fm, body) = split_fm_body(raw);
    let map = fm
        .and_then(|f| serde_yaml::from_str::<serde_yaml::Mapping>(f).ok())
        .unwrap_or_default();
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let id = {
        let declared = map_str(&map, "id").trim().to_string();
        if declared.is_empty() {
            stem
        } else {
            declared
        }
    };
    let outcome_raw = map_str(&map, "outcome").trim().to_string();
    RunRecord {
        id,
        ticket: map_str(&map, "ticket").trim().to_string(),
        pipeline: map_str(&map, "pipeline").trim().to_string(),
        lane: map_str(&map, "lane").trim().to_string(),
        agent: map_str(&map, "agent").trim().to_string(),
        model: map_str(&map, "model").trim().to_string(),
        scope: map_list(&map, "scope"),
        verify: map_str(&map, "verify").trim().to_string(),
        started: map_str(&map, "started").trim().to_string(),
        ended: map_str(&map, "ended").trim().to_string(),
        outcome: RunOutcome::parse(&outcome_raw),
        outcome_raw,
        artifacts: map_list(&map, "artifacts"),
        report: body.trim().to_string(),
    }
}

/// Render a fresh run-record file: every [`RunRecord`] field as
/// frontmatter, in field-declaration order, then the agent's report as the
/// body. Pure text composition — the caller writes the bytes (module-wide
/// "no filesystem access" posture); reuses `tasks::scalar_lines`/
/// `seq_lines` for the same quoting guarantees every other write in this
/// feature gets.
pub fn compose_run(record: &RunRecord) -> String {
    let outcome_str = record
        .outcome
        .map(RunOutcome::as_str)
        .unwrap_or(record.outcome_raw.as_str());
    let mut lines: Vec<String> = Vec::new();
    lines.extend(scalar_lines("id", &record.id));
    lines.extend(scalar_lines("ticket", &record.ticket));
    lines.extend(scalar_lines("pipeline", &record.pipeline));
    lines.extend(scalar_lines("lane", &record.lane));
    lines.extend(scalar_lines("agent", &record.agent));
    lines.extend(scalar_lines("model", &record.model));
    lines.extend(seq_lines("scope", &record.scope));
    lines.extend(scalar_lines("verify", &record.verify));
    lines.extend(scalar_lines("started", &record.started));
    lines.extend(scalar_lines("ended", &record.ended));
    lines.extend(scalar_lines("outcome", outcome_str));
    lines.extend(seq_lines("artifacts", &record.artifacts));

    let mut out = String::from("---\n");
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
    out.push_str("---\n\n");
    let report = record.report.trim();
    out.push_str(report);
    if !report.is_empty() {
        out.push('\n');
    }
    out
}

/// Rewrite named top-level keys of an existing run record through the S6
/// patch core (closing a run: `ended`, `outcome`, `artifacts`), the same
/// shape [`patch_pipeline_text`] gives definitions.
pub fn patch_run_text(raw: &str, edits: &[(&str, Vec<String>)]) -> String {
    crate::tasks::patch_text(raw, edits, None)
}

/// Parse every `(path, raw)` pair already read off disk into a
/// [`RunRecord`] — 1.11's "ledger scan". The caller does the directory
/// listing (this module has no filesystem access by design), so this is a
/// `map` over already-read bytes, not a scanner; it exists so every caller
/// shares exactly one parse path for the whole ledger, the same shape
/// `tasks::list_tasks`/`scan_tasks` give the board.
pub fn scan_runs<'a, I>(files: I) -> Vec<RunRecord>
where
    I: IntoIterator<Item = (&'a Path, &'a str)>,
{
    files.into_iter().map(|(path, raw)| parse_run(path, raw)).collect()
}

/// The ledger-derived queue view (D13 / spec: "running, queued, blocked,
/// and waiting-on-human state SHALL be derived from the ledger and the
/// tickets"). `waiting_human` is not run-ledger data — no run record
/// exists yet for work nobody has authorised — so it is derived from the
/// board via [`admit`] instead, never guessed at from `outcome`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunQueue {
    pub running: Vec<RunRecord>,
    pub queued: Vec<RunRecord>,
    pub blocked: Vec<RunRecord>,
    /// [`RunOutcome::Running`] records this session has no live memory of
    /// (D13: "a `running` record with no live run after restart ... SHALL
    /// NOT be recorded as passed").
    pub stale: Vec<RunRecord>,
    /// Ticket ids currently sitting at a confirmation gate.
    pub waiting_human: Vec<String>,
}

/// Derive the full queue view from the ledger plus the board.
///
/// `known_running_ids` is the set of run ids *this Ken session* has itself
/// observed as running — never persisted, never read from disk (this
/// module owns no clock or watcher, and D13 gives the ledger, not an
/// in-memory registry, as the source of truth). A `running` record whose
/// id is **not** in that set is one this session has no live memory of.
/// Concretely: on a fresh workspace-open the caller passes an *empty* set,
/// so every `running` record already on disk is reported stale by
/// construction — exactly D13's "no live run after restart", for a runner
/// that spawns no processes to track in the first place (D14/OPEN-1).
/// Mid-session, a freshly claimed run's id belongs to the caller's own
/// running set and is therefore never stale.
pub fn derive_queue(
    tasks: &[Task],
    pipelines: &[Pipeline],
    runs: &[RunRecord],
    known_running_ids: &BTreeSet<String>,
) -> RunQueue {
    let mut q = RunQueue::default();
    for r in runs {
        match r.outcome {
            Some(RunOutcome::Running) => {
                if known_running_ids.contains(&r.id) {
                    q.running.push(r.clone());
                } else {
                    q.stale.push(r.clone());
                }
            }
            Some(RunOutcome::Queued) => q.queued.push(r.clone()),
            Some(RunOutcome::Blocked) => q.blocked.push(r.clone()),
            // pass/fail/cancelled/unparseable: closed, not part of the
            // live queue.
            _ => {}
        }
    }
    q.waiting_human = waiting_on_human(tasks, pipelines, runs)
        .into_iter()
        .map(|t| t.id.clone())
        .collect();
    q
}

/// Tickets sitting at a confirmation gate right now, computed by asking
/// [`admit`] what a human-initiated kickoff would do — so this can never
/// drift from the one admission function (D5's single-home rule). A
/// blocked ticket is excluded: it belongs to the `blocked` bucket, not
/// this one, so stuck work has exactly one home in the derived view too.
pub fn waiting_on_human<'a>(
    tasks: &'a [Task],
    pipelines: &[Pipeline],
    runs: &[RunRecord],
) -> Vec<&'a Task> {
    tasks
        .iter()
        .filter(|t| {
            let Some(pipeline_id) = ticket_pipeline(t) else {
                return false;
            };
            let Some(pipeline) = find_pipeline(pipelines, &pipeline_id) else {
                return false;
            };
            let Some(lane) = resolve_lane(pipeline, &t.status_raw) else {
                return false;
            };
            matches!(
                admit(t, lane, pipeline, runs, EntryKind::Kickoff),
                Admission::Confirm { .. }
            )
        })
        .collect()
}

// ---------------------------------------------------------------------
// 1.8 Admission — the single gate every start goes through
// ---------------------------------------------------------------------

/// Why work is being offered to [`admit`]. This is not decoration: the
/// `Unblock` variant is the load-bearing safety input (D5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EntryKind {
    /// A human pressed the button.
    Kickoff,
    /// An external agent is pulling already-authorised queued work
    /// (`pipeline_claim`, D14's `mcp` runner).
    Claim,
    /// The ticket's last blocker just cleared.
    Unblock,
    /// Lane entry under `kickoff: auto` + pipeline `auto: true` (2.12,
    /// gated behind a manual end-to-end pass).
    AutoTransition,
}

/// Why a start became a confirmation instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "reason")]
pub enum ConfirmReason {
    /// The lane declares `kickoff: confirm`.
    LaneGate,
    /// The lane declares `kickoff: manual` and a human asked anyway.
    ManualKickoff,
    /// D3 brake 3: no `scope` and/or no `verify`, so no auto-run under any
    /// lane setting.
    MissingBoundary { scope: bool, verify: bool },
    /// D5: the ticket arrived by unblock. Always a confirmation.
    Unblocked,
    /// The lane is `auto` but the pipeline master switch is off (D6).
    AutoDisabled,
}

/// Why work was refused outright.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "reason")]
pub enum RefusalReason {
    /// **The** refusal (D5). Checked first, always.
    Blocked {
        return_lane: Option<String>,
        blocked_by: Vec<String>,
        block_reason: Option<String>,
    },
    // Every payload-carrying variant below is a STRUCT variant, not a
    // newtype. This enum is internally tagged (`tag = "reason"`, no
    // `content`), and serde cannot serialize `Variant(String)` in that
    // representation: it fails at RUNTIME with "cannot serialize tagged
    // newtype variant". The compiler says nothing, and only refusal paths
    // reach it — so the failure would land exactly when the UI needed to
    // explain why work was refused. Same rule applies to `TransitionRefusal`
    // and `BlockRefusal`; `every_refusal_variant_actually_serializes` guards
    // all three.
    /// A `human: true` lane has no agent and no kickoff (D11).
    HumanLane { lane: String },
    /// A holding column — `agent: none`. Nothing to run.
    NoAgent { lane: String },
    /// A `manual` lane reached by something other than a human asking.
    ManualLane { lane: String },
}

/// [`admit`]'s verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "admission")]
pub enum Admission {
    /// Start the run now.
    Start,
    /// Propose a run and wait for a human.
    Confirm { reason: ConfirmReason },
    /// Authorised, but the cap is full.
    Queued { running: usize, cap: u32 },
    /// Do not start, and do not create a run record.
    Refused { reason: RefusalReason },
}

/// Everything holding a ticket, if anything is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockedState {
    pub in_blocked_lane: bool,
    pub return_lane: Option<String>,
    pub blocked_by: Vec<String>,
    pub block_reason: Option<String>,
    pub blocked_at: Option<String>,
}

/// Is this ticket blocked, by any of the three routes — sitting in the
/// blocked lane, carrying dependencies, or carrying a reason?
///
/// All three count, on purpose. The lane is the *presentation* of a block;
/// the fields are the *fact* of one. Requiring both to agree would make
/// the D5 invariant depend on two things staying in sync, and the invariant
/// is precisely the one a future change is most likely to break.
pub fn blocked_state(ticket: &Task, pipeline: &Pipeline) -> Option<BlockedState> {
    let fields = ticket_fields(ticket);
    let in_blocked_lane = resolve_lane(pipeline, &ticket.status_raw)
        .map(|l| l.blocked)
        .unwrap_or(false);
    if !in_blocked_lane && fields.blocked_by.is_empty() && fields.block_reason.is_none() {
        return None;
    }
    Some(BlockedState {
        in_blocked_lane,
        return_lane: fields.return_lane,
        blocked_by: fields.blocked_by,
        block_reason: fields.block_reason,
        blocked_at: fields.blocked_at,
    })
}

/// **The single admission function.** Every path that could start work —
/// UI kickoff, MCP `pipeline_claim`, and any future auto-transition — must
/// call this and must not re-implement any part of it (tasks.md's D5 note:
/// "It lives in exactly one function ... Never re-implement it").
///
/// The order below is load-bearing and is asserted as such by the tests:
///
/// 1. **blocked ⇒ `Refused`, before anything else.** Not after the gate,
///    not after the cap. A blocked ticket in an `auto` lane with the
///    pipeline master switch on and a free concurrency slot is still
///    refused, because this check runs before any of those are read.
/// 2. structurally unrunnable lanes (human, no agent) ⇒ `Refused`.
/// 3. missing `scope`/`verify` ⇒ downgrade to `Confirm` (D3 brake 3).
/// 4. `entry == Unblock` ⇒ `Confirm`, unconditionally and by early
///    return — an `auto` lane cannot out-vote it (D5).
/// 5. the lane's gate mode.
/// 6. the concurrency cap over currently-`running` records ⇒ `Queued`.
///
/// On step 4: under today's `mcp` pull runner (D14/OPEN-1) nothing spawns
/// a process, so this downgrade is a convenience. **If a `command` runner
/// is ever added, this line is the rule that stops a dependency completing
/// at 2am from meaning a code-writing agent ran at 2am.** It is written as
/// an unconditional early return, and tested as an exhaustive "no
/// `EntryKind::Unblock` input reaches `Start`" property, so that a future
/// reader cannot mistake it for an optimisation.
pub fn admit(
    ticket: &Task,
    lane: &Lane,
    pipeline: &Pipeline,
    runs: &[RunRecord],
    entry: EntryKind,
) -> Admission {
    // (1) Blocked. First. Always.
    if let Some(state) = blocked_state(ticket, pipeline) {
        return Admission::Refused {
            reason: RefusalReason::Blocked {
                return_lane: state.return_lane,
                blocked_by: state.blocked_by,
                block_reason: state.block_reason,
            },
        };
    }
    // Entering the blocked lane itself is the same refusal seen from the
    // other side — a lane with `blocked: true` never runs anything.
    if lane.blocked {
        return Admission::Refused {
            reason: RefusalReason::Blocked {
                return_lane: ticket_fields(ticket).return_lane,
                blocked_by: Vec::new(),
                block_reason: None,
            },
        };
    }

    // (2) Structurally unrunnable lanes.
    if lane.human {
        return Admission::Refused {
            reason: RefusalReason::HumanLane { lane: lane.id.clone() },
        };
    }
    if lane.agent.is_none() {
        return Admission::Refused {
            reason: RefusalReason::NoAgent { lane: lane.id.clone() },
        };
    }

    // (3) The ticket-carried boundary (D3 brake 3).
    let fields = ticket_fields(ticket);
    let missing_scope = fields.scope.is_empty();
    let missing_verify = fields.verify.is_none();
    let boundary = if missing_scope || missing_verify {
        Some(ConfirmReason::MissingBoundary {
            scope: missing_scope,
            verify: missing_verify,
        })
    } else {
        None
    };

    // (4) D5. An unblocked ticket is ALWAYS a confirmation. No lane
    //     setting, no master switch, and no entry-specific shortcut below
    //     can reach `Start` from here, because this returns.
    if entry == EntryKind::Unblock {
        return Admission::Confirm {
            reason: boundary.unwrap_or(ConfirmReason::Unblocked),
        };
    }

    // (5) Gate mode.
    let confirm = match lane.kickoff {
        Kickoff::Manual => match entry {
            // A human clicking "run" on a manual lane still gets a
            // confirmation — "never starts a run *by itself*" (spec) is
            // about the lane acting, not about the human asking.
            EntryKind::Kickoff => Some(boundary.unwrap_or(ConfirmReason::ManualKickoff)),
            // The queued run record is the authorisation an agent pulls
            // against; a boundary problem still stops it.
            EntryKind::Claim => boundary,
            EntryKind::AutoTransition | EntryKind::Unblock => {
                return Admission::Refused {
                    reason: RefusalReason::ManualLane { lane: lane.id.clone() },
                }
            }
        },
        Kickoff::Confirm => match entry {
            EntryKind::Claim => boundary,
            _ => Some(boundary.unwrap_or(ConfirmReason::LaneGate)),
        },
        Kickoff::Auto => {
            if pipeline.auto {
                boundary
            } else {
                Some(boundary.unwrap_or(ConfirmReason::AutoDisabled))
            }
        }
    };
    if let Some(reason) = confirm {
        return Admission::Confirm { reason };
    }

    // (6) The cap, over currently-running records only.
    let running = running_runs(runs);
    if running >= pipeline.concurrency_cap as usize {
        return Admission::Queued {
            running,
            cap: pipeline.concurrency_cap,
        };
    }
    Admission::Start
}

// ---------------------------------------------------------------------
// 1.9 Transition resolution and bounce accounting (D4)
// ---------------------------------------------------------------------

/// What a lane's agent reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AdvanceOutcome {
    Pass,
    Fail,
}

impl AdvanceOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            AdvanceOutcome::Pass => "pass",
            AdvanceOutcome::Fail => "fail",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "pass" => Some(AdvanceOutcome::Pass),
            "fail" => Some(AdvanceOutcome::Fail),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "refusal")]
pub enum TransitionRefusal {
    /// The ticket's `status` names no lane in this pipeline. Struct variant
    /// for the serde reason documented on `RefusalReason`.
    UnknownLane { status: String },
    /// The lane declares no edge for this outcome (a terminal lane on
    /// `pass`, or any lane with no `on_fail`).
    NoEdge { lane: String, outcome: AdvanceOutcome },
    /// `on_pass`/`on_fail` names a lane the definition doesn't declare —
    /// the lane-rename orphan, surfaced rather than guessed at.
    UnknownTarget { lane: String, target: String },
    /// A bounce blew the cap but the definition has no blocked lane to
    /// escalate into.
    NoBlockedLane,
}

/// The result of advancing a ticket. Note there is no `Halted` variant and
/// no `halted` field anywhere in this codebase: D4's retry-cap escalation
/// produces [`Transition::Blocked`], reusing the one mechanism for stuck
/// work rather than inventing a second place a human has to look.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Transition {
    Moved {
        from: String,
        to: String,
        /// A move to an earlier lane in definition order (D4).
        backward: bool,
        /// The counter's value *after* this move.
        bounces: u32,
        patch: TaskPatch,
        log: String,
    },
    /// D4: the bounce would have exceeded `bounce_cap`, so the ticket is
    /// blocked instead of entering the target lane again.
    Blocked {
        from: String,
        /// The lane it was bouncing to — and therefore its `return_lane`.
        would_have_entered: String,
        block: Block,
        patch: TaskPatch,
        log: String,
    },
    Refused {
        reason: TransitionRefusal,
    },
}

/// Resolve `on_pass`/`on_fail`, classify backward moves as bounces, and
/// escalate a cap breach into a block (D4).
///
/// `now` is caller-supplied — this module owns no clock (the same
/// convention `tasks::create_task` and `memory.rs` use). It is only read
/// on the cap-breach path, where it becomes `blocked_at`.
pub fn advance(
    ticket: &Task,
    pipeline: &Pipeline,
    outcome: AdvanceOutcome,
    now: &str,
) -> Transition {
    let refuse = |reason| Transition::Refused { reason };

    let Some(from) = resolve_lane(pipeline, &ticket.status_raw) else {
        return refuse(TransitionRefusal::UnknownLane { status: ticket.status_raw.clone() });
    };
    let target_id = match outcome {
        AdvanceOutcome::Pass => from.on_pass.clone(),
        AdvanceOutcome::Fail => from.on_fail.clone(),
    };
    let Some(target_id) = target_id else {
        return refuse(TransitionRefusal::NoEdge {
            lane: from.id.clone(),
            outcome,
        });
    };
    let Some(to) = resolve_lane(pipeline, &target_id) else {
        return refuse(TransitionRefusal::UnknownTarget {
            lane: from.id.clone(),
            target: target_id,
        });
    };

    let from_idx = lane_index(pipeline, &from.id).unwrap_or(0);
    let to_idx = lane_index(pipeline, &to.id).unwrap_or(0);
    let backward = to_idx < from_idx;

    let fields = ticket_fields(ticket);
    if !backward {
        return Transition::Moved {
            log: format!("{} → {} ({})", from.id, to.id, outcome.as_str()),
            from: from.id.clone(),
            to: to.id.clone(),
            backward: false,
            bounces: fields.bounces,
            patch: TaskPatch {
                lane: Some(to.id.clone()),
                ..TaskPatch::default()
            },
        };
    }

    let next = fields.bounces.saturating_add(1);
    if next > pipeline.bounce_cap {
        // D4: refuse the transition, block the ticket instead. The return
        // lane is the lane it was *bouncing to* — not its current lane —
        // because that is where the work still needs to happen.
        let Some(blocked_lane) = pipeline.blocked_lane() else {
            return refuse(TransitionRefusal::NoBlockedLane);
        };
        let block = Block {
            return_lane: to.id.clone(),
            blocked_by: fields.blocked_by.clone(),
            reason: Some(format!("{RETRY_CAP_REASON} ({next} bounces)")),
            blocked_at: now.trim().to_string(),
        };
        // `bounces` is still written: the count is the evidence, and the
        // human unsticking the ticket should see how deep the loop got.
        let patch = block_patch(blocked_lane, &block, Some(next));
        let log = format!(
            "{} → {} refused: {} — blocked, returns to {}",
            from.id,
            to.id,
            block.reason.clone().unwrap_or_default(),
            to.id
        );
        return Transition::Blocked {
            from: from.id.clone(),
            would_have_entered: to.id.clone(),
            block,
            patch,
            log,
        };
    }

    Transition::Moved {
        log: format!(
            "{} → {} (fail, bounce {next}/{})",
            from.id, to.id, pipeline.bounce_cap
        ),
        from: from.id.clone(),
        to: to.id.clone(),
        backward: true,
        bounces: next,
        patch: TaskPatch {
            lane: Some(to.id.clone()),
            bounces: Some(next),
            ..TaskPatch::default()
        },
    }
}

// ---------------------------------------------------------------------
// 1.10 Unblock evaluation (D5, OPEN-10)
// ---------------------------------------------------------------------

/// A ticket whose blockers have all cleared. The caller re-enters it with
/// [`EntryKind::Unblock`] — which [`admit`] turns into a confirmation, and
/// can never turn into a start.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Unblocked {
    pub ticket_id: String,
    pub return_lane: String,
    pub blocked_at: Option<String>,
    /// The patch that moves it home.
    pub patch: TaskPatch,
}

fn is_terminal(task: &Task, pipeline: &Pipeline) -> bool {
    resolve_lane(pipeline, &task.status_raw)
        .map(|l| l.terminal)
        .unwrap_or(false)
}

/// Which dependents of `terminal_ticket_id` are now free (D5, OPEN-10:
/// resolve on transition, not on a timer).
///
/// Free means **both** halves are clear: every `blocked_by` entry resolves
/// to a ticket sitting in a `terminal: true` lane, *and* `block_reason` is
/// empty. A blocker id that matches no ticket is not terminal — it is an
/// `UnknownBlocker` tray entry, and an unresolvable dependency must not
/// read as a satisfied one.
pub fn evaluate_unblocks(
    tasks: &[Task],
    pipeline: &Pipeline,
    terminal_ticket_id: &str,
) -> Vec<Unblocked> {
    evaluate_unblocks_inner(tasks, pipeline, Some(terminal_ticket_id))
}

/// The whole-board sweep run on workspace open, so nothing is missed
/// across a restart (spec: "and again on workspace open").
pub fn evaluate_all_unblocks(tasks: &[Task], pipeline: &Pipeline) -> Vec<Unblocked> {
    evaluate_unblocks_inner(tasks, pipeline, None)
}

fn evaluate_unblocks_inner(
    tasks: &[Task],
    pipeline: &Pipeline,
    naming: Option<&str>,
) -> Vec<Unblocked> {
    let mut out = Vec::new();
    for task in tasks {
        let fields = ticket_fields(task);
        let in_blocked_lane = resolve_lane(pipeline, &task.status_raw)
            .map(|l| l.blocked)
            .unwrap_or(false);
        if !in_blocked_lane {
            continue;
        }
        if let Some(id) = naming {
            if !fields.blocked_by.iter().any(|b| eq_ci(b, id)) {
                continue;
            }
        }
        // Both halves, independently (D5: "clearing one SHALL NOT unblock
        // the ticket while the other still applies").
        if fields.block_reason.is_some() {
            continue;
        }
        let all_terminal = fields.blocked_by.iter().all(|b| {
            tasks
                .iter()
                .find(|t| eq_ci(&t.id, b))
                .map(|t| is_terminal(t, pipeline))
                .unwrap_or(false)
        });
        if !all_terminal {
            continue;
        }
        let Some(return_lane) = fields
            .return_lane
            .as_deref()
            .filter(|rl| resolve_lane(pipeline, rl).is_some())
        else {
            // Missing/orphaned return lane: a tray entry
            // (`UnknownReturnLane`), never a guess.
            continue;
        };
        // No `bounces` reset here on purpose: this path only ever fires
        // for a ticket whose `block_reason` is already empty, and a
        // retry-cap block always *has* a reason. A retry-capped ticket is
        // therefore released by an explicit human [`unblock`] — which is
        // where the reset lives — never by a blocker completing.
        out.push(Unblocked {
            ticket_id: task.id.clone(),
            return_lane: return_lane.to_string(),
            blocked_at: fields.blocked_at.clone(),
            patch: TaskPatch {
                lane: Some(return_lane.to_string()),
                blocked_by: Some(Vec::new()),
                block_reason: Some(String::new()),
                blocked_at: Some(String::new()),
                ..TaskPatch::default()
            },
        });
    }
    out
}

// ---------------------------------------------------------------------
// 1.12 Sign-off child composition (D11)
// ---------------------------------------------------------------------

/// The lane a sign-off comment's child ticket lands in (D11/spec: "a new
/// ticket in the `todo` lane"). Literal, per the spec's exact wording —
/// not derived from any per-lane flag, because the definition model has
/// none for "the intake-adjacent lane". See [`SignoffRefusal::NoTodoLane`]
/// for what happens when a pipeline doesn't declare one.
const SIGNOFF_CHILD_LANE: &str = "todo";

/// What "accept with comments" produces, as one pure result (D11: "both
/// things happen ... in the same action"): the child ticket to create, the
/// parent's `on_pass` advance (composed via [`advance`] so the bounce/cap
/// machinery is never duplicated), and the line appended to the *parent's*
/// `## Log` so the parent's history is self-contained. Plain accept is
/// just `advance(parent, pipeline, AdvanceOutcome::Pass, now)` with no
/// child — nothing here to compose for it. Reject is `advance(..,
/// AdvanceOutcome::Fail, ..)`, which already counts as a bounce.
// `NewTask` (`tasks.rs`) does not derive `PartialEq`, so this struct can't
// either — tests compare `child`'s fields individually instead of the
// whole `SignoffChild`.
#[derive(Debug, Clone)]
pub struct SignoffChild {
    pub child: NewTask,
    pub parent_transition: Transition,
    pub parent_log: String,
}

/// Why a comment-spawned child cannot be composed. Every variant leaves
/// both files untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignoffRefusal {
    /// The reviewed ticket isn't sitting in a `human: true` lane — nothing
    /// to sign off on.
    NotHumanLane(String),
    /// An empty comment is "accept", not "accept with comments".
    EmptyComment,
    /// The lane declares no `on_pass` edge for the parent to advance
    /// along.
    NoOnPass(String),
    /// The pipeline declares no lane literally named `todo` for the child
    /// to land in (D2: never write around a lane that can't be
    /// validated).
    NoTodoLane,
}

/// Compose the child ticket + parent advance for "accept with comments"
/// (D11). Pure: returns everything a caller needs to write in one shot, so
/// the action's two writes (new child file, parent patch) can never
/// disagree about the parent's target lane.
///
/// The child inherits `pipeline`, `project`, and `projects` from the
/// parent, and carries `parent` + `origin: signoff`. It deliberately does
/// **not** inherit `scope`/`verify` (D11/D3: "the child is new work and
/// must earn its own boundary").
pub fn compose_signoff_child(
    parent: &Task,
    pipeline: &Pipeline,
    comment: &str,
    now: &str,
) -> Result<SignoffChild, SignoffRefusal> {
    let comment = comment.trim();
    if comment.is_empty() {
        return Err(SignoffRefusal::EmptyComment);
    }
    let Some(lane) = resolve_lane(pipeline, &parent.status_raw) else {
        return Err(SignoffRefusal::NotHumanLane(parent.status_raw.clone()));
    };
    if !lane.human {
        return Err(SignoffRefusal::NotHumanLane(lane.id.clone()));
    }
    if pipeline.lane(SIGNOFF_CHILD_LANE).is_none() {
        return Err(SignoffRefusal::NoTodoLane);
    }

    let parent_transition = advance(parent, pipeline, AdvanceOutcome::Pass, now);
    if matches!(
        parent_transition,
        Transition::Refused {
            reason: TransitionRefusal::NoEdge { .. }
        }
    ) {
        return Err(SignoffRefusal::NoOnPass(lane.id.clone()));
    }

    let parent_fields = ticket_fields(parent);
    let child = NewTask {
        id: None,
        title: format!("Sign-off comment on {}", parent.title),
        body: comment.to_string(),
        fields: TaskPatch {
            lane: Some(SIGNOFF_CHILD_LANE.to_string()),
            pipeline: parent_fields.pipeline.clone(),
            project: Some(parent.project.clone()),
            projects: if parent_fields.projects.is_empty() {
                None
            } else {
                Some(parent_fields.projects.clone())
            },
            parent: Some(parent.id.clone()),
            origin: Some("signoff".to_string()),
            ..TaskPatch::default()
        },
    };
    let parent_log = format!("sign-off comment → new ticket \"{}\": {comment}", child.title);
    Ok(SignoffChild {
        child,
        parent_transition,
        parent_log,
    })
}

// ---------------------------------------------------------------------
// 1.13 Idea proposal + dedupe scoring (D7)
// ---------------------------------------------------------------------

/// The lane a landed idea occupies (D7/spec: `status: ideas`, an inert
/// holding column by construction).
const IDEA_LANE: &str = "ideas";
/// D7/spec: every generated idea carries this origin.
const IDEA_ORIGIN: &str = "generated";

/// A proposed idea, before it becomes a ticket. `spawned_by` is required
/// at construction — [`propose_idea`] is the one place that can produce an
/// [`IdeaCandidate`], and it refuses to build one without a citation (D7:
/// "An idea without a citation is refused at creation").
#[derive(Debug, Clone, PartialEq)]
pub struct IdeaCandidate {
    pub title: String,
    pub body: String,
    pub spawned_by: String,
    pub project: String,
    pub projects: Vec<String>,
    pub pipeline: String,
}

/// Why an idea candidate could not be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdeaRefusal {
    /// D7: "An idea without a citation is refused at creation."
    MissingCitation,
    EmptyTitle,
    /// The pipeline declares no `ideas` lane, so there's nowhere inert for
    /// it to land (only raised by [`compose_idea_ticket`]).
    NoIdeasLane,
}

/// Build an idea candidate, refusing at construction rather than letting a
/// missing citation slip through to dedupe or landing (D7).
pub fn propose_idea(
    title: &str,
    body: &str,
    spawned_by: &str,
    project: &str,
    projects: &[String],
    pipeline: &str,
) -> Result<IdeaCandidate, IdeaRefusal> {
    if spawned_by.trim().is_empty() {
        return Err(IdeaRefusal::MissingCitation);
    }
    if title.trim().is_empty() {
        return Err(IdeaRefusal::EmptyTitle);
    }
    Ok(IdeaCandidate {
        title: title.trim().to_string(),
        body: body.trim().to_string(),
        spawned_by: spawned_by.trim().to_string(),
        project: project.trim().to_string(),
        projects: projects.to_vec(),
        pipeline: pipeline.trim().to_string(),
    })
}

/// Dedupe scope (D7/D12): the candidate's project plus any project linked
/// to the idea's project. Pure set membership so callers (2.8) can filter
/// their search results to this scope before ever calling [`dedupe_idea`]
/// — this module has no workspace handle, so the caller resolves
/// `linked` via `WorkspaceConfig::linked_projects` and passes it in.
pub fn in_dedupe_scope(idea_project: &str, candidate_project: &str, linked: &[&str]) -> bool {
    eq_ci(idea_project, candidate_project) || linked.iter().any(|l| eq_ci(l, candidate_project))
}

/// A ticket already on the board, offered to [`dedupe_idea`] as a possible
/// match. Deliberately narrower than [`Task`]: the same shape serves both
/// the semantic-search path (2.8's `semantic_search`/`kg_search` results,
/// mapped down to this) and the FTS fallback (`search_knowledge` results),
/// so `dedupe_idea` never has to know which produced its input.
#[derive(Debug, Clone, PartialEq)]
pub struct DedupeCandidate {
    pub ticket_id: String,
    pub title: String,
    pub project: String,
    /// `Some` when the caller already has a similarity score (the
    /// semantic path); `None` when only a title is available (FTS/
    /// title-match fallback), in which case [`dedupe_idea`] computes one
    /// itself from [`normalized_title_score`].
    pub score: Option<f32>,
}

/// The dedupe verdict (D7).
#[derive(Debug, Clone, PartialEq)]
pub enum DedupeVerdict {
    Land,
    NearDuplicate { ticket_id: String, score: f32 },
}

/// Similarity threshold above which a candidate counts as a duplicate.
/// Applies uniformly to a caller-supplied semantic score and this
/// module's own normalized-title score, so the two dedupe paths (D7:
/// "the same function serves the semantic path and the FTS fallback")
/// agree on what "above threshold" means.
pub const DEDUPE_THRESHOLD: f32 = 0.82;

/// Crude normalized-title similarity: lowercase, tokenize on non-
/// alphanumerics, Jaccard overlap. This is the FTS-path fallback D7
/// requires when `semanticIndex`/`federatedKg` are off — not a
/// replacement for real embeddings, which the semantic path already
/// supplies as a `DedupeCandidate::score`.
fn normalized_title_score(a: &str, b: &str) -> f32 {
    let tokens = |s: &str| -> BTreeSet<String> {
        s.to_ascii_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_string)
            .collect()
    };
    let ta = tokens(a);
    let tb = tokens(b);
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    let inter = ta.intersection(&tb).count() as f32;
    let union = ta.union(&tb).count() as f32;
    inter / union
}

/// Score every offered candidate and return the verdict (D7). The same
/// function serves the semantic path (candidates carry a `score` from
/// `semantic_search`/`kg_search`) and the FTS fallback (candidates carry
/// `None`, so [`normalized_title_score`] is computed here) — the caller
/// never branches on which index produced its input. Dedupe scope
/// ([`in_dedupe_scope`]) is the caller's job to apply *before* calling
/// this; this module has no workspace handle to enforce it itself.
pub fn dedupe_idea(idea: &IdeaCandidate, candidates: &[DedupeCandidate]) -> DedupeVerdict {
    let mut best: Option<(&DedupeCandidate, f32)> = None;
    for c in candidates {
        let score = c
            .score
            .unwrap_or_else(|| normalized_title_score(&idea.title, &c.title));
        if score >= DEDUPE_THRESHOLD && best.map(|(_, b)| score > b).unwrap_or(true) {
            best = Some((c, score));
        }
    }
    match best {
        Some((c, score)) => DedupeVerdict::NearDuplicate {
            ticket_id: c.ticket_id.clone(),
            score,
        },
        None => DedupeVerdict::Land,
    }
}

/// The `## Log` line appended to the *matched* ticket when an idea is
/// deduped away instead of landing (D7/spec: "a near-duplicate note SHALL
/// be appended to the matched ticket's `## Log`"). `None` for
/// `DedupeVerdict::Land` — a landed idea gets no such note, it gets a
/// ticket.
pub fn dedupe_log_line(idea: &IdeaCandidate, verdict: &DedupeVerdict) -> Option<String> {
    match verdict {
        DedupeVerdict::NearDuplicate { score, .. } => Some(format!(
            "near-duplicate idea proposed from {} — \"{}\" ({:.0}% match), not filed",
            idea.spawned_by,
            idea.title,
            score * 100.0
        )),
        DedupeVerdict::Land => None,
    }
}

/// Compose the ticket-creation payload for an idea that survived dedupe
/// (`DedupeVerdict::Land`). Requires the pipeline to declare an `ideas`
/// lane — the same defensive "never write around a lane that can't be
/// validated" check [`compose_signoff_child`] makes for `todo`. D7: "the
/// Ideas lane is inert by construction" — landing here can never start a
/// run because the lane itself has no agent, not because this function
/// checks admission.
pub fn compose_idea_ticket(idea: &IdeaCandidate, pipeline: &Pipeline) -> Result<NewTask, IdeaRefusal> {
    if pipeline.lane(IDEA_LANE).is_none() {
        return Err(IdeaRefusal::NoIdeasLane);
    }
    Ok(NewTask {
        id: None,
        title: idea.title.clone(),
        body: idea.body.clone(),
        fields: TaskPatch {
            lane: Some(IDEA_LANE.to_string()),
            pipeline: Some(idea.pipeline.clone()),
            project: Some(idea.project.clone()),
            projects: if idea.projects.is_empty() {
                None
            } else {
                Some(idea.projects.clone())
            },
            spawned_by: Some(idea.spawned_by.clone()),
            origin: Some(IDEA_ORIGIN.to_string()),
            ..TaskPatch::default()
        },
    })
}

// ---------------------------------------------------------------------
// 1.14 Artifact manifest model (D9)
// ---------------------------------------------------------------------

const ARTIFACTS_SUBDIR: &str = "artifacts";
const ARTIFACT_MANIFEST_FILE: &str = "manifest.md";
/// OPEN-5, pinned: 30 days, surfaced as a prune action in the tray, never
/// auto-deleted (D9: "Ken does not delete a human's review material on a
/// timer").
pub const DEFAULT_ARTIFACT_TTL_DAYS: i64 = 30;

/// `.ken-workspace/artifacts/`.
pub fn artifacts_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join(crate::workspace::CONFIG_DIR).join(ARTIFACTS_SUBDIR)
}

/// `.ken-workspace/artifacts/<ticket-id>/` — never inside any member repo
/// (D9's physical boundary).
pub fn artifact_ticket_dir(workspace_root: &Path, ticket_id: &str) -> PathBuf {
    artifacts_dir(workspace_root).join(ticket_id)
}

/// `.ken-workspace/artifacts/<ticket-id>/manifest.md`.
pub fn artifact_manifest_path(workspace_root: &Path, ticket_id: &str) -> PathBuf {
    artifact_ticket_dir(workspace_root, ticket_id).join(ARTIFACT_MANIFEST_FILE)
}

/// One throwaway-artifact folder's manifest (D9). `durable` has no setter
/// and no frontmatter reader that could make it anything but `false` —
/// the same "cannot typecheck the unsafe state" posture as [`Block`]'s
/// `return_lane`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactManifest {
    pub ticket: String,
    pub created: String,
    pub expires: String,
    pub files: Vec<String>,
}

impl ArtifactManifest {
    /// Always `false` (D9). There is no field to set it any other way.
    pub fn durable(&self) -> bool {
        false
    }
}

/// Build a fresh manifest, defaulting `expires` to `created` + 30 days
/// (OPEN-5). Falls back to `created` unchanged if it isn't a parseable
/// `YYYY-MM-DD` — an unparseable `expires` is a tray entry for a human to
/// fix, never a panic.
pub fn new_artifact_manifest(ticket: &str, created: &str, files: Vec<String>) -> ArtifactManifest {
    ArtifactManifest {
        ticket: ticket.to_string(),
        expires: shift_iso_date(created, DEFAULT_ARTIFACT_TTL_DAYS)
            .unwrap_or_else(|| created.to_string()),
        created: created.to_string(),
        files,
    }
}

/// Parse a manifest file. Infallible and tolerant, same posture as
/// [`parse_run`]/[`parse_pipeline`]. `durable` is read-and-discarded — the
/// type only ever reports `false` ([`ArtifactManifest::durable`]) — so a
/// hand-edited `durable: true` cannot make an artifact folder look
/// durable to the rest of Ken.
pub fn parse_artifact_manifest(path: &Path, raw: &str) -> ArtifactManifest {
    let (fm, _body) = split_fm_body(raw);
    let map = fm
        .and_then(|f| serde_yaml::from_str::<serde_yaml::Mapping>(f).ok())
        .unwrap_or_default();
    let ticket = {
        let declared = map_str(&map, "ticket").trim().to_string();
        if declared.is_empty() {
            path.parent()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        } else {
            declared
        }
    };
    ArtifactManifest {
        ticket,
        created: map_str(&map, "created").trim().to_string(),
        expires: map_str(&map, "expires").trim().to_string(),
        files: map_list(&map, "files"),
    }
}

/// Render a fresh manifest file. `durable: false` is always the first
/// line and is never taken from `self` — it isn't a field on
/// [`ArtifactManifest`] at all (D9).
pub fn compose_artifact_manifest(manifest: &ArtifactManifest) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.extend(scalar_lines("durable", "false"));
    lines.extend(scalar_lines("ticket", &manifest.ticket));
    lines.extend(scalar_lines("created", &manifest.created));
    lines.extend(scalar_lines("expires", &manifest.expires));
    lines.extend(seq_lines("files", &manifest.files));

    let mut out = String::from("---\n");
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
    out.push_str("---\n\n");
    out.push_str("Throwaway artifacts for this ticket — not part of the test suite.\n");
    out
}

/// Rewrite named top-level keys of an existing manifest (e.g. appending a
/// file to `files`) through the S6 patch core, same shape as
/// [`patch_run_text`]/[`patch_pipeline_text`].
pub fn patch_artifact_manifest_text(raw: &str, edits: &[(&str, Vec<String>)]) -> String {
    crate::tasks::patch_text(raw, edits, None)
}

/// Whether `manifest` has passed its `expires` date, as of `today` — a
/// pure predicate and nothing more (D9: "Expiry is surfaced, never
/// automatic ... prune is a UI action"; there is deliberately no delete/
/// prune function anywhere in this module). ISO dates compare correctly
/// as strings (same trick `tasks::rollover_candidates` uses); an
/// unparseable `expires` or `today` reads as *not* expired rather than
/// guessed at — surfaced elsewhere (the tray), never a silent prune
/// trigger. Exactly `today == expires` is not yet expired: it expires the
/// day *after*.
pub fn is_artifact_expired(manifest: &ArtifactManifest, today: &str) -> bool {
    let expires = manifest.expires.trim();
    let today = today.trim();
    if !is_iso_date_like(expires) || !is_iso_date_like(today) {
        return false;
    }
    today > expires
}

fn is_iso_date_like(s: &str) -> bool {
    s.len() == 10
        && s.as_bytes()[4] == b'-'
        && s.as_bytes()[7] == b'-'
        && s.bytes().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                true
            } else {
                b.is_ascii_digit()
            }
        })
}

/// Days since 1970-01-01 for a proleptic-Gregorian `(y, m, d)` date —
/// Howard Hinnant's public-domain `days_from_civil` algorithm
/// (https://howardhinnant.github.io/date_algorithms.html). Duplicated
/// locally rather than shared from `memory.rs` (whose copy is private to
/// that module and this session's scope is this file only); used only to
/// compute [`shift_iso_date`]'s default `expires`, never to compare dates
/// (string comparison suffices for that, see [`is_artifact_expired`]).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// The inverse of [`days_from_civil`]: a day count back to `(y, m, d)`.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn parse_iso_date_parts(s: &str) -> Option<(i64, i64, i64)> {
    if !is_iso_date_like(s) {
        return None;
    }
    let mut parts = s.splitn(3, '-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next()?.parse().ok()?;
    let d: i64 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some((y, m, d))
}

/// `date` shifted by `days` (may be negative), as `YYYY-MM-DD`. `None` for
/// an unparseable input.
pub fn shift_iso_date(date: &str, days: i64) -> Option<String> {
    let (y, m, d) = parse_iso_date_parts(date)?;
    let (y2, m2, d2) = civil_from_days(days_from_civil(y, m, d) + days);
    Some(format!("{y2:04}-{m2:02}-{d2:02}"))
}

// ---------------------------------------------------------------------
// 1.15 Digest composition (spec: "produce a grouped daily update")
// ---------------------------------------------------------------------

/// One entry in the `blocked` digest group — the ticket plus the *root*
/// blocker(s) of its chain, not the nearest (1.15's subtle part; see
/// [`root_blockers`]).
#[derive(Debug, Clone, PartialEq)]
pub struct BlockedDigestEntry {
    pub ticket_id: String,
    pub title: String,
    pub blocked_at: Option<String>,
    pub root_blockers: Vec<String>,
    pub block_reason: Option<String>,
    pub run_count: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UnblockedDigestEntry {
    pub ticket_id: String,
    pub title: String,
    pub return_lane: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AwaitingReviewEntry {
    pub ticket_id: String,
    pub title: String,
    pub updated: String,
    pub run_count: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MovedEntry {
    pub ticket_id: String,
    pub title: String,
    pub lane: String,
    pub run_count: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IdeaEntry {
    pub ticket_id: String,
    pub title: String,
    pub spawned_by: Option<String>,
}

/// The whole daily update, groups in the spec's exact order: awaiting
/// review, newly unblocked, blocked (root-first, oldest first), moved
/// today, new ideas, stale runs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Digest {
    pub awaiting_review: Vec<AwaitingReviewEntry>,
    pub newly_unblocked: Vec<UnblockedDigestEntry>,
    pub blocked: Vec<BlockedDigestEntry>,
    pub moved_today: Vec<MovedEntry>,
    pub new_ideas: Vec<IdeaEntry>,
    pub stale_runs: Vec<RunRecord>,
}

/// The ticket(s) at the top of a blocked ticket's dependency chain — the
/// digest's "root blocker of each chain, not the nearest" (1.15). Walks
/// [`BlockGraph::blockers`] to its leaves (tickets with no `blocked_by` of
/// their own), de-duplicated, in first-seen order. A `seen` set (the same
/// discipline [`check_cycle`] uses) means a cycle an existing hand-edited
/// file already contains still terminates, even though write-time
/// detection stops any *new* one.
pub fn root_blockers(graph: &BlockGraph, ticket_id: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut stack: Vec<String> = graph.blockers(ticket_id).to_vec();
    while let Some(current) = stack.pop() {
        if !seen.insert(norm(&current)) {
            continue;
        }
        let next = graph.blockers(&current);
        if next.is_empty() {
            if !out.iter().any(|o| eq_ci(o, &current)) {
                out.push(current);
            }
        } else {
            stack.extend(next.iter().cloned());
        }
    }
    out
}

/// Compose the digest (1.15). Pure over a caller-supplied board + ledger +
/// `today` (this module owns no clock). `known_running_ids` is
/// [`derive_queue`]'s staleness input — see its docs.
pub fn compose_digest(
    tasks: &[Task],
    pipelines: &[Pipeline],
    runs: &[RunRecord],
    known_running_ids: &BTreeSet<String>,
    today: &str,
) -> Digest {
    let graph = BlockGraph::from_tasks(tasks);
    let run_count = |id: &str| runs.iter().filter(|r| eq_ci(&r.ticket, id)).count();

    // awaiting_review: sitting in a human lane, oldest (by `updated`)
    // first.
    let mut awaiting: Vec<&Task> = tasks
        .iter()
        .filter(|t| {
            ticket_pipeline(t)
                .and_then(|pid| find_pipeline(pipelines, &pid).map(|p| (p, t.status_raw.clone())))
                .and_then(|(p, status_raw)| resolve_lane(p, &status_raw).map(|l| l.human))
                .unwrap_or(false)
        })
        .collect();
    awaiting.sort_by(|a, b| a.updated.cmp(&b.updated));
    let awaiting_review = awaiting
        .into_iter()
        .map(|t| AwaitingReviewEntry {
            ticket_id: t.id.clone(),
            title: t.title.clone(),
            updated: t.updated.clone(),
            run_count: run_count(&t.id),
        })
        .collect();

    // newly_unblocked: with return lanes.
    let newly_unblocked = tasks
        .iter()
        .filter(|t| matches_block_filter(t, &crate::tasks::BlockedFilter::NewlyUnblocked))
        .map(|t| UnblockedDigestEntry {
            ticket_id: t.id.clone(),
            title: t.title.clone(),
            return_lane: ticket_fields(t).return_lane.unwrap_or_default(),
        })
        .collect();

    // blocked: oldest first by blocked_at, root blocker(s) shown.
    let mut blocked_tasks: Vec<&Task> = tasks.iter().filter(|t| has_block_evidence(t)).collect();
    blocked_tasks.sort_by(|a, b| {
        let ba = ticket_fields(a).blocked_at.unwrap_or_default();
        let bb = ticket_fields(b).blocked_at.unwrap_or_default();
        ba.cmp(&bb)
    });
    let blocked = blocked_tasks
        .into_iter()
        .map(|t| {
            let f = ticket_fields(t);
            BlockedDigestEntry {
                ticket_id: t.id.clone(),
                title: t.title.clone(),
                blocked_at: f.blocked_at.clone(),
                root_blockers: root_blockers(&graph, &t.id),
                block_reason: f.block_reason.clone(),
                run_count: run_count(&t.id),
            }
        })
        .collect();

    // moved_today: `updated` is today.
    let moved_today = tasks
        .iter()
        .filter(|t| t.updated == today)
        .map(|t| MovedEntry {
            ticket_id: t.id.clone(),
            title: t.title.clone(),
            lane: t.lane.clone().unwrap_or_else(|| t.status_raw.clone()),
            run_count: run_count(&t.id),
        })
        .collect();

    // new_ideas: generated today.
    let new_ideas = tasks
        .iter()
        .filter(|t| {
            let f = ticket_fields(t);
            f.origin.as_deref() == Some(IDEA_ORIGIN) && t.created == today
        })
        .map(|t| IdeaEntry {
            ticket_id: t.id.clone(),
            title: t.title.clone(),
            spawned_by: ticket_fields(t).spawned_by.clone(),
        })
        .collect();

    // stale_runs
    let stale_runs = derive_queue(tasks, pipelines, runs, known_running_ids).stale;

    Digest {
        awaiting_review,
        newly_unblocked,
        blocked,
        moved_today,
        new_ideas,
        stale_runs,
    }
}

/// Render the digest to markdown — the one function chat, MCP, and
/// `journal_append` all call (1.15), so the three surfaces can never drift
/// text apart. Empty groups are omitted; an entirely empty digest renders
/// a single "nothing to report" line.
pub fn render_digest_markdown(digest: &Digest, today: &str) -> String {
    let mut out = format!("# Pipeline digest — {today}\n\n");
    let mut any = false;

    if !digest.awaiting_review.is_empty() {
        any = true;
        out.push_str("## Awaiting your review\n\n");
        for e in &digest.awaiting_review {
            out.push_str(&format!(
                "- **{}** ({}) — updated {}, {} run(s)\n",
                e.title, e.ticket_id, e.updated, e.run_count
            ));
        }
        out.push('\n');
    }

    if !digest.newly_unblocked.is_empty() {
        any = true;
        out.push_str("## Unblocked overnight\n\n");
        for e in &digest.newly_unblocked {
            out.push_str(&format!(
                "- **{}** ({}) — returns to `{}`\n",
                e.title, e.ticket_id, e.return_lane
            ));
        }
        out.push('\n');
    }

    if !digest.blocked.is_empty() {
        any = true;
        out.push_str("## Blocked\n\n");
        for e in &digest.blocked {
            let root = if e.root_blockers.is_empty() {
                String::new()
            } else {
                format!(", root blocker(s): {}", e.root_blockers.join(", "))
            };
            let reason = e
                .block_reason
                .as_deref()
                .map(|r| format!(" — {r}"))
                .unwrap_or_default();
            out.push_str(&format!(
                "- **{}** ({}) — blocked since {}{root}{reason}, {} run(s)\n",
                e.title,
                e.ticket_id,
                e.blocked_at.as_deref().unwrap_or("unknown"),
                e.run_count
            ));
        }
        out.push('\n');
    }

    if !digest.moved_today.is_empty() {
        any = true;
        out.push_str("## Moved today\n\n");
        for e in &digest.moved_today {
            out.push_str(&format!("- **{}** ({}) → `{}`\n", e.title, e.ticket_id, e.lane));
        }
        out.push('\n');
    }

    if !digest.new_ideas.is_empty() {
        any = true;
        out.push_str("## New ideas\n\n");
        for e in &digest.new_ideas {
            let cite = e
                .spawned_by
                .as_deref()
                .map(|s| format!(" — from {s}"))
                .unwrap_or_default();
            out.push_str(&format!("- **{}** ({}){cite}\n", e.title, e.ticket_id));
        }
        out.push('\n');
    }

    if !digest.stale_runs.is_empty() {
        any = true;
        out.push_str("## Stale runs\n\n");
        for r in &digest.stale_runs {
            out.push_str(&format!("- run `{}` on ticket {} (lane `{}`)\n", r.id, r.ticket, r.lane));
        }
        out.push('\n');
    }

    if !any {
        out.push_str("Nothing to report.\n");
    }
    out
}

// ---------------------------------------------------------------------
// 1.19 Default pipeline scaffold (D1's twelve lanes)
// ---------------------------------------------------------------------

/// `.ken-workspace/pipelines/default.md`'s content on first enable —
/// D1's twelve lanes (eleven flow lanes plus `blocked`) verbatim, with the
/// per-lane brief body a lane's agent is handed at kickoff. Written only
/// when the file doesn't already exist ([`scaffold_default_pipeline`]) —
/// a user's edited pipeline is never overwritten.
pub const DEFAULT_PIPELINE_MD: &str = r#"---
id: default
name: Standard delivery pipeline
auto: false
concurrency_cap: 1
bounce_cap: 3
lanes:
  - id: ideas
    name: Ideas backlog
    maps_to: backlog
    agent: none
    kickoff: manual
    on_pass: backlog
  - id: backlog
    name: Backlog
    maps_to: backlog
    agent: none
    kickoff: manual
    on_pass: todo
  - id: todo
    name: To Do
    maps_to: todo
    agent: none
    kickoff: manual
    on_pass: investigation
  - id: investigation
    name: Investigation
    maps_to: doing
    agent: investigator
    model: sonnet
    kickoff: confirm
    on_pass: refinement
  - id: refinement
    name: Refinement
    maps_to: doing
    agent: refiner
    model: opus
    kickoff: confirm
    on_pass: programmer
  - id: programmer
    name: Programmer
    maps_to: doing
    agent: programmer
    model: sonnet
    kickoff: confirm
    writes_code: true
    on_pass: tester
  - id: tester
    name: Tester
    maps_to: review
    agent: tester
    model: sonnet
    kickoff: confirm
    on_pass: architect
    on_fail: programmer
  - id: architect
    name: Architect review
    maps_to: review
    agent: architect
    model: opus
    kickoff: confirm
    on_pass: qa
    on_fail: refinement
  - id: qa
    name: QA tester
    maps_to: review
    agent: qa
    model: sonnet
    kickoff: confirm
    on_pass: signoff
    on_fail: programmer
  - id: signoff
    name: Sign-off
    maps_to: review
    human: true
    kickoff: manual
    on_pass: documentation
    on_fail: refinement
  - id: documentation
    name: Documentation
    maps_to: done
    agent: documenter
    model: sonnet
    kickoff: confirm
    terminal: true
    generative: true
  - id: blocked
    name: Blocked
    maps_to: doing
    agent: none
    kickoff: manual
    blocked: true
---

# Standard delivery pipeline

Eleven flow lanes plus Blocked (D1). Manual kickoff only until `auto` is
flipped on (D6) — every lane with an agent defaults to `kickoff: confirm`,
so nothing runs without an explicit accept.

## Ideas / Backlog / To Do

Pure holding columns — no agent, no risk. Groom Ideas into Backlog, and
Backlog into To Do, by hand.

## Investigation (sonnet)

Read the ticket and its `scope`. Confirm the ask is well-specified before
anything is designed; report ambiguity rather than guessing.

## Refinement (opus)

Design-heavy: decide the shape of the work. This is the lane most worth a
stronger model (D8) because it sets the plan every later lane executes.

## Programmer (sonnet, writes_code)

Implement inside `scope` only. Prove the work with the ticket's `verify`
command and report the result — Ken does not run `verify` itself.

## Tester (sonnet)

Write and run real tests. A fail bounces back to Programmer; three bounces
(`bounce_cap`) blocks the ticket for a human rather than looping forever
(D4).

## Architect review (opus)

Risk-bearing: the lane that catches a bad shape before it ships. A fail
bounces back to Refinement, not Programmer — the shape needs to change,
not just the code.

## QA tester (sonnet)

Two outputs, two destinations (D9): durable long-term/E2E test plans go
into the repo inside `scope`; throwaway review material (walkthrough,
screenshots, a demo recording) goes only under
`.ken-workspace/artifacts/<ticket-id>/`, never into a member repo, with a
`manifest.md` recording `durable: false` and an `expires` date. Pick the
recording recipe from the ticket's `target` (D10): `web` → Playwright,
`tauri` → `tauri-driver` over WebDriver, `none` → a written walkthrough.

## Sign-off (human)

No agent. Accept moves to Documentation. Accept with comments spawns a
child ticket in To Do carrying the comment — the parent still advances,
so a comment never blocks work that's already done (D11). Reject bounces
to Refinement.

## Documentation (sonnet, terminal, generative)

Update docs, then look at the finished ticket for follow-up ideas. Every
proposed idea must cite the ticket that produced it (`spawned_by`) and is
deduped against existing tickets in this project and any linked project
before landing (D7); a near-duplicate gets a log note on the existing
ticket instead of a new file.

## Blocked

Never picked up by any lane's agent (D5) — whatever blocked a ticket,
dependency or retry cap, it resumes at its recorded `return_lane` only
after a human (or an unblock re-entry, which still waits at a
confirmation) says so.
"#;

/// Whether to write [`DEFAULT_PIPELINE_MD`] on first enable. `exists` is
/// the caller's own `Path::exists()` check (this module does no
/// filesystem I/O) — the scaffold is written **only if the file does not
/// already exist** (1.19): a user's edited pipeline is never overwritten.
pub fn scaffold_default_pipeline(exists: bool) -> Option<&'static str> {
    if exists {
        None
    } else {
        Some(DEFAULT_PIPELINE_MD)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::{self, BlockedFilter, HomeKind, TaskFilter};
    use tempfile::tempdir;

    // -----------------------------------------------------------------
    // Fixtures
    // -----------------------------------------------------------------

    /// D1's twelve lanes. A test-local fixture, not the `default.md`
    /// scaffold constant — that is tasks.md 1.19 and belongs to a later
    /// session; this one exists so the tests exercise the real shape.
    const DEFAULT_MD: &str = r#"---
id: default
name: Standard delivery pipeline
auto: false
concurrency_cap: 1
bounce_cap: 3
lanes:
  - id: ideas
    name: Ideas backlog
    maps_to: backlog
    agent: none
    kickoff: manual
    on_pass: backlog
  - id: backlog
    name: Backlog
    maps_to: backlog
    agent: none
    kickoff: manual
    on_pass: todo
  - id: todo
    name: To Do
    maps_to: todo
    agent: none
    kickoff: manual
    on_pass: investigation
  - id: investigation
    name: Investigation
    maps_to: doing
    agent: investigator
    model: sonnet
    kickoff: confirm
    on_pass: refinement
  - id: refinement
    name: Refinement
    maps_to: doing
    agent: refiner
    model: opus
    kickoff: confirm
    on_pass: programmer
  - id: programmer
    name: Programmer
    maps_to: doing
    agent: programmer
    model: sonnet
    kickoff: confirm
    writes_code: true
    on_pass: tester
  - id: tester
    name: Tester
    maps_to: review
    agent: tester
    model: sonnet
    kickoff: confirm
    on_pass: architect
    on_fail: programmer
  - id: architect
    name: Architect review
    maps_to: review
    agent: architect
    model: opus
    kickoff: confirm
    on_pass: qa
    on_fail: refinement
  - id: qa
    name: QA tester
    maps_to: review
    agent: qa
    model: sonnet
    kickoff: confirm
    on_pass: signoff
    on_fail: programmer
  - id: signoff
    name: Sign-off
    maps_to: review
    human: true
    kickoff: manual
    on_pass: documentation
    on_fail: refinement
  - id: documentation
    name: Documentation
    maps_to: done
    agent: documenter
    model: sonnet
    kickoff: confirm
    terminal: true
    generative: true
  - id: blocked
    name: Blocked
    maps_to: doing
    agent: none
    kickoff: manual
    blocked: true
---

Per-lane briefs live here.
"#;

    fn pipe() -> Pipeline {
        parse_pipeline(Path::new("/ws/.ken-workspace/pipelines/default.md"), DEFAULT_MD)
    }

    /// The same pipeline with `programmer` set to `kickoff: auto` and the
    /// master switch on — the "every switch flipped the dangerous way"
    /// fixture the blocked-refusal tests need.
    fn auto_pipe() -> Pipeline {
        let md = DEFAULT_MD
            .replace("auto: false", "auto: true")
            .replace(
                "    kickoff: confirm\n    writes_code: true",
                "    kickoff: auto\n    writes_code: true",
            );
        let p = parse_pipeline(Path::new("/ws/p.md"), &md);
        assert!(p.auto, "fixture: master switch must be on");
        assert_eq!(
            p.lane("programmer").unwrap().kickoff,
            Kickoff::Auto,
            "fixture: programmer must be an auto lane"
        );
        p
    }

    /// A 26-character Crockford ULID with a recognisable tail.
    fn uid(tag: char) -> String {
        format!("01J{}{tag}", "0".repeat(22))
    }

    fn task_from(raw: &str) -> Task {
        tasks::parse_task(Path::new("/ws/.ken-workspace/tasks/t.md"), HomeKind::Workspace, "", raw)
    }

    /// A pipeline ticket with a full boundary (`scope` + `verify`), so it
    /// is admissible on every axis except the one a given test varies.
    fn ticket(id: &str, status: &str, extra: &str) -> Task {
        let raw = format!(
            "---\nid: {id}\ntitle: Ticket {id}\nstatus: {status}\npipeline: default\n\
             scope:\n  - crates/ken-core/**\nverify: cargo test -p ken-core\n{extra}---\n\nBody.\n"
        );
        task_from(&raw)
    }

    /// The same, with no `scope`/`verify` — D3 brake 3's unbounded ticket.
    fn unbounded(id: &str, status: &str, extra: &str) -> Task {
        let raw = format!(
            "---\nid: {id}\ntitle: Ticket {id}\nstatus: {status}\npipeline: default\n{extra}---\n\nBody.\n"
        );
        task_from(&raw)
    }

    fn running_run() -> RunRecord {
        RunRecord {
            outcome: Some(RunOutcome::Running),
            ..RunRecord::default()
        }
    }

    // -----------------------------------------------------------------
    // 1.1 Definition parse, round-trip, validation
    // -----------------------------------------------------------------

    #[test]
    fn parses_twelve_lanes_with_defaults_and_flags() {
        let p = pipe();
        assert_eq!(p.id, "default");
        assert_eq!(p.name, "Standard delivery pipeline");
        assert!(!p.auto);
        assert_eq!(p.concurrency_cap, 1);
        assert_eq!(p.bounce_cap, 3);
        assert_eq!(p.lanes.len(), 12);
        // Lane order in the file is column order.
        let ids: Vec<&str> = p.lanes.iter().map(|l| l.id.as_str()).collect();
        assert_eq!(ids[0], "ideas");
        assert_eq!(ids[11], "blocked");

        let ideas = p.lane("ideas").unwrap();
        assert_eq!(ideas.agent, None, "`agent: none` is not an agent named none");
        assert_eq!(ideas.maps_to, TaskStatus::Backlog);

        let prog = p.lane("programmer").unwrap();
        assert_eq!(prog.agent.as_deref(), Some("programmer"));
        assert_eq!(prog.model.as_deref(), Some("sonnet"));
        assert!(prog.writes_code);
        assert_eq!(prog.kickoff, Kickoff::Confirm);
        assert_eq!(prog.runner, Runner::Mcp, "v1 default runner is the pull model");

        assert!(p.lane("signoff").unwrap().human);
        assert!(p.lane("documentation").unwrap().terminal);
        assert!(p.lane("documentation").unwrap().generative);
        assert_eq!(p.blocked_lane().unwrap().id, "blocked");
        assert_eq!(p.human_lane().unwrap().id, "signoff");
        assert!(p.body.contains("Per-lane briefs"));
        assert!(validate_pipeline(&p).is_empty());
    }

    #[test]
    fn caps_default_to_one_and_three_when_absent() {
        let p = parse_pipeline(
            Path::new("/ws/mini.md"),
            "---\nid: mini\nlanes:\n  - id: a\n    maps_to: todo\n---\n",
        );
        assert_eq!(p.concurrency_cap, DEFAULT_CONCURRENCY_CAP);
        assert_eq!(p.bounce_cap, DEFAULT_BOUNCE_CAP);
        assert_eq!(DEFAULT_CONCURRENCY_CAP, 1);
        assert_eq!(DEFAULT_BOUNCE_CAP, 3);
    }

    #[test]
    fn kickoff_defaults_to_confirm_with_an_agent_and_manual_without() {
        let p = parse_pipeline(
            Path::new("/ws/x.md"),
            "---\nid: x\nlanes:\n  - id: a\n    maps_to: todo\n  - id: b\n    maps_to: doing\n    agent: bob\n---\n",
        );
        assert_eq!(p.lane("a").unwrap().kickoff, Kickoff::Manual);
        assert_eq!(p.lane("b").unwrap().kickoff, Kickoff::Confirm);
    }

    #[test]
    fn definition_round_trips_unknown_keys_and_a_hand_edited_body() {
        let raw = "---\n# a comment the user wrote\nid: custom\nnotes: 'kept verbatim'\nauto: false\nlanes:\n  - id: a\n    name: A\n    maps_to: todo\n    lane_note: mine\n---\n\nHand   edited   body.\r\n  trailing spaces   \n";
        let p = parse_pipeline(Path::new("/ws/custom.md"), raw);
        assert_eq!(p.id, "custom");
        assert_eq!(map_str(p.extra(), "notes"), "kept verbatim");
        assert_eq!(map_str(p.lanes[0].extra(), "lane_note"), "mine");

        // No edits ⇒ byte-identical.
        assert_eq!(patch_pipeline_text(raw, &[]), raw);

        // One edit ⇒ only that line differs; the comment, the unknown
        // keys, the lane block and the body all survive byte-for-byte.
        //
        // Note the `'true'`: `scalar_lines` quotes every YAML-ambiguous
        // scalar on purpose ("dates, numbers, ids with `:`, empty strings
        // — single-quoted, which is always safe"). This module reuses that
        // renderer rather than forking a bare-boolean one, so the assertion
        // that matters is that the quoted form reads back as a boolean.
        let next = patch_pipeline_text(raw, &[("auto", tasks::scalar_lines("auto", "true"))]);
        assert_eq!(next, raw.replace("auto: false", "auto: 'true'"));
        assert!(next.contains("# a comment the user wrote"));
        assert!(next.contains("    lane_note: mine"));
        assert!(next.contains("Hand   edited   body."));
        assert!(parse_pipeline(Path::new("/ws/custom.md"), &next).auto);
    }

    #[test]
    fn two_blocked_lanes_are_rejected() {
        let p = parse_pipeline(
            Path::new("/ws/x.md"),
            "---\nid: x\nlanes:\n  - id: a\n    maps_to: doing\n    blocked: true\n  - id: b\n    maps_to: doing\n    blocked: true\n---\n",
        );
        assert_eq!(
            validate_pipeline(&p),
            vec![PipelineIssue::MultipleBlockedLanes {
                first: "a".into(),
                second: "b".into()
            }]
        );
    }

    #[test]
    fn two_human_lanes_are_rejected() {
        let p = parse_pipeline(
            Path::new("/ws/x.md"),
            "---\nid: x\nlanes:\n  - id: a\n    maps_to: review\n    human: true\n  - id: b\n    maps_to: review\n    human: true\n---\n",
        );
        assert_eq!(
            validate_pipeline(&p),
            vec![PipelineIssue::MultipleHumanLanes {
                first: "a".into(),
                second: "b".into()
            }]
        );
    }

    #[test]
    fn validation_catches_typos_duplicates_and_dangling_edges() {
        let p = parse_pipeline(
            Path::new("/ws/x.md"),
            "---\nid: x\nlanes:\n  - id: a\n    maps_to: reviewing\n    kickoff: sometimes\n    on_pass: nowhere\n  - id: a\n    maps_to: todo\n---\n",
        );
        let issues = validate_pipeline(&p);
        assert!(issues.contains(&PipelineIssue::DuplicateLaneId { id: "a".into() }));
        assert!(issues.contains(&PipelineIssue::InvalidMapsTo {
            lane: "a".into(),
            value: "reviewing".into()
        }));
        assert!(issues.contains(&PipelineIssue::InvalidKickoff {
            lane: "a".into(),
            value: "sometimes".into()
        }));
        assert!(issues.contains(&PipelineIssue::UnknownTransition {
            lane: "a".into(),
            edge: "on_pass".into(),
            target: "nowhere".into()
        }));
        assert_eq!(validate_pipeline(&parse_pipeline(Path::new("/ws/e.md"), "---\nid: e\n---\n")), vec![PipelineIssue::NoLanes]);
    }

    // -----------------------------------------------------------------
    // 1.2 / 1.3 Lane resolution and the maps_to projection
    // -----------------------------------------------------------------

    #[test]
    fn lane_resolution_is_case_insensitive_and_ordered() {
        let p = pipe();
        assert_eq!(resolve_lane(&p, "Architect").unwrap().id, "architect");
        assert_eq!(resolve_lane(&p, "  qa  ").unwrap().id, "qa");
        assert!(resolve_lane(&p, "nonesuch").is_none());
        assert_eq!(lane_index(&p, "ideas"), Some(0));
        assert!(lane_index(&p, "programmer").unwrap() < lane_index(&p, "tester").unwrap());
        // Empty status ⇒ the intake column, i.e. whichever lane is first.
        assert_eq!(resolve_lane(&p, "").unwrap().id, "ideas");
    }

    #[test]
    fn pipeline_ticket_projects_onto_the_classic_board() {
        let p = pipe();
        let mut t = ticket(&uid('A'), "architect", "");
        // Before resolution the classic parse can't place it — that is why
        // resolution exists, and why it has exactly one home.
        assert_eq!(t.status, None);
        resolve_task_lane(&mut t, &[p.clone()]);
        assert_eq!(t.lane.as_deref(), Some("architect"));
        assert_eq!(t.status, Some(TaskStatus::Review));
        assert_eq!(t.status_raw, "architect", "status_raw still says what the file said");
        assert_eq!(maps_to(&p, "documentation"), Some(TaskStatus::Done));

        // ...and `task_list({status: "review"})` still returns it.
        let f = TaskFilter { status: Some(TaskStatus::Review), ..TaskFilter::default() };
        assert!(tasks::matches(&t, &f));
    }

    #[test]
    fn pipeline_less_ticket_takes_the_classic_path_untouched() {
        let raw = "---\nid: X1\ntitle: classic\nstatus: doing\n---\n\nBody.\n";
        let mut t = task_from(raw);
        let before = t.clone();
        resolve_task_lane(&mut t, &[pipe()]);
        assert_eq!(t, before, "a ticket with no `pipeline:` key is not touched at all");
        assert_eq!(t.status, Some(TaskStatus::Doing));
        assert_eq!(t.lane, None);
        // A pipeline-less ticket matches no lane filter, exactly as an
        // out-of-vocabulary status matches no status filter.
        let f = TaskFilter { lane: Some("programmer".into()), ..TaskFilter::default() };
        assert!(!tasks::matches(&t, &f));
    }

    #[test]
    fn unknown_lane_and_unknown_pipeline_land_in_the_tray_not_on_the_board() {
        let p = pipe();
        let mut orphan = ticket(&uid('A'), "programmerr", "");
        let mut wrong_pipeline = task_from(&format!(
            "---\nid: {}\ntitle: t\nstatus: todo\npipeline: ghost\n---\n\nBody.\n",
            uid('B')
        ));
        assert_eq!(ticket_pipeline(&wrong_pipeline).as_deref(), Some("ghost"));

        resolve_task_lane(&mut orphan, &[p.clone()]);
        resolve_task_lane(&mut wrong_pipeline, &[p.clone()]);
        assert_eq!(orphan.status, None);
        assert_eq!(orphan.lane, None);
        assert_eq!(wrong_pipeline.status, None);

        let board = vec![orphan.clone(), wrong_pipeline.clone()];
        let tray = tasks::needs_attention_with_pipelines(&board, &[], &[p]);
        assert_eq!(tray.len(), 2);
        assert!(tray[0].reasons.contains(&AttentionReason::UnknownLane("programmerr".into())));
        assert!(tray[1].reasons.contains(&AttentionReason::UnknownPipeline("ghost".into())));
        // `UnknownLane` replaces `InvalidStatus`; it does not double up.
        assert!(!tray[0]
            .reasons
            .iter()
            .any(|r| matches!(r, AttentionReason::InvalidStatus(_))));
    }

    #[test]
    fn unknown_blocker_and_missing_return_lane_are_surfaced() {
        let p = pipe();
        let ghost = uid('Z');
        let blocked = ticket(
            &uid('A'),
            "blocked",
            &format!("blocked_by:\n  - {ghost}\nreturn_lane: gone\n"),
        );
        let no_return = ticket(&uid('B'), "blocked", "block_reason: waiting on legal\n");
        let board = vec![blocked.clone(), no_return.clone()];

        let r = attention_reasons(&blocked, &board, &[p.clone()]);
        assert!(r.contains(&AttentionReason::UnknownBlocker(ghost)));
        assert!(r.contains(&AttentionReason::UnknownReturnLane("gone".into())));

        let r2 = attention_reasons(&no_return, &board, &[p]);
        assert!(r2.contains(&AttentionReason::UnknownReturnLane(String::new())));
    }

    #[test]
    fn an_unknown_lane_ticket_is_refused_a_patch_and_its_file_is_unchanged() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("t.md");
        let raw = "---\nid: T1\ntitle: x\nstatus: programmerr\npipeline: default\nkeep: me\n---\n\nBody.\n";
        std::fs::write(&path, raw).unwrap();
        let p = pipe();

        let patch = TaskPatch { assignee: Some("ken".into()), ..TaskPatch::default() };
        let err = tasks::apply_patch_with_pipelines(&path, &patch, "2026-08-03", &[p.clone()])
            .unwrap_err();
        assert!(format!("{err}").contains("programmerr"), "error names the bad value: {err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), raw, "file untouched");

        // Resolving the lane explicitly is always allowed.
        let fix = TaskPatch { lane: Some("programmer".into()), ..TaskPatch::default() };
        tasks::apply_patch_with_pipelines(&path, &fix, "2026-08-03", &[p]).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("status: programmer\n"));
        assert!(after.contains("keep: me"), "unknown keys survive");
        assert!(after.contains("Body."));
    }

    #[test]
    fn a_valid_pipeline_lane_is_patchable_and_writes_only_the_named_keys() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("t.md");
        let raw = "---\nid: T1\ntitle: x\nstatus: programmer\npipeline: default\nupdated: 2026-01-01\nkeep: me\n---\n\nBody.\n";
        std::fs::write(&path, raw).unwrap();
        let patch = TaskPatch { lane: Some("tester".into()), bounces: Some(2), ..TaskPatch::default() };
        tasks::apply_patch_with_pipelines(&path, &patch, "2026-08-03", &[pipe()]).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        // Exactly `status`, `updated` and `bounces` differ; key order,
        // unknown keys and the body are untouched. `bounces` is quoted for
        // the same reason `updated` is — see the note in the round-trip
        // test — and reads back as the integer 2.
        assert_eq!(
            after,
            "---\nid: T1\ntitle: x\nstatus: tester\npipeline: default\nupdated: '2026-08-03'\nkeep: me\nbounces: '2'\n---\n\nBody.\n"
        );
        assert_eq!(ticket_fields(&task_from(&after)).bounces, 2);
    }

    // -----------------------------------------------------------------
    // 1.3 Filters
    // -----------------------------------------------------------------

    #[test]
    fn block_filters_answer_what_is_stuck() {
        let dep = uid('D');
        let blocked = ticket(&uid('A'), "blocked", &format!("blocked_by:\n  - {dep}\nreturn_lane: programmer\n"));
        let reasoned = ticket(&uid('B'), "blocked", "block_reason: upstream 2.0\nreturn_lane: tester\n");
        let free = ticket(&uid('C'), "programmer", "");
        let freed = ticket(&uid('E'), "programmer", "return_lane: programmer\n");

        let by = |f: BlockedFilter, t: &Task| {
            tasks::matches(t, &TaskFilter { blocked: Some(f), ..TaskFilter::default() })
        };
        assert!(by(BlockedFilter::Blocked, &blocked));
        assert!(by(BlockedFilter::Blocked, &reasoned));
        assert!(!by(BlockedFilter::Blocked, &free));
        assert!(by(BlockedFilter::NotBlocked, &free));
        assert!(by(BlockedFilter::By(dep.clone()), &blocked));
        assert!(!by(BlockedFilter::By(dep), &reasoned));
        assert!(by(BlockedFilter::NewlyUnblocked, &freed));
        assert!(!by(BlockedFilter::NewlyUnblocked, &blocked));
        assert!(!by(BlockedFilter::NewlyUnblocked, &free));
        assert!(by(BlockedFilter::Any, &free) && by(BlockedFilter::Any, &blocked));

        // `pipeline` filter, and the existing filters still behave.
        assert!(tasks::matches(
            &free,
            &TaskFilter { pipeline: Some("DEFAULT".into()), ..TaskFilter::default() }
        ));
        assert!(!tasks::matches(
            &free,
            &TaskFilter { pipeline: Some("other".into()), ..TaskFilter::default() }
        ));
    }

    // -----------------------------------------------------------------
    // 1.5 Ticket fields
    // -----------------------------------------------------------------

    #[test]
    fn ticket_fields_read_every_pipeline_key_off_the_extra_flatten() {
        let t = ticket(
            &uid('A'),
            "qa",
            "model: opus\nagent: qa\nbounces: 2\nreturn_lane: programmer\nblock_reason: waiting\n\
             blocked_at: '2026-08-01T09:00:00Z'\nparent: P1\nspawned_by: S1\norigin: generated\n\
             projects:\n  - alpha\n  - beta\ntarget: tauri\n",
        );
        let f = ticket_fields(&t);
        assert_eq!(f.pipeline.as_deref(), Some("default"));
        assert_eq!(f.model.as_deref(), Some("opus"));
        assert_eq!(f.agent.as_deref(), Some("qa"));
        assert_eq!(f.scope, vec!["crates/ken-core/**".to_string()]);
        assert_eq!(f.verify.as_deref(), Some("cargo test -p ken-core"));
        assert_eq!(f.bounces, 2);
        assert_eq!(f.return_lane.as_deref(), Some("programmer"));
        assert_eq!(f.block_reason.as_deref(), Some("waiting"));
        assert_eq!(f.blocked_at.as_deref(), Some("2026-08-01T09:00:00Z"));
        assert_eq!(f.parent.as_deref(), Some("P1"));
        assert_eq!(f.spawned_by.as_deref(), Some("S1"));
        assert_eq!(f.origin.as_deref(), Some("generated"));
        assert_eq!(f.projects, vec!["alpha".to_string(), "beta".to_string()]);
        assert_eq!(f.target, Target::Tauri);
        assert_eq!(ticket_fields(&ticket(&uid('B'), "todo", "")).target, Target::None);
    }

    // -----------------------------------------------------------------
    // 1.7 Cycle detection — table
    // -----------------------------------------------------------------

    fn graph(edges: &[(char, &[char])]) -> BlockGraph {
        let mut g = BlockGraph::new();
        for (t, deps) in edges {
            let d: Vec<String> = deps.iter().map(|c| uid(*c)).collect();
            g.insert(&uid(*t), &d);
        }
        g
    }

    #[test]
    fn cycle_detection_table() {
        // (name, graph edges, offered edge from→to, expect_refused)
        let a = 'A';
        let b = 'B';
        let c = 'C';
        let d = 'D';
        let e = 'E';
        let f = 'F';

        // self-edge
        let g0 = BlockGraph::new();
        assert!(check_cycle(&g0, &uid(a), &uid(a)).is_err(), "self-edge");

        // direct: A blocked by B, now B blocked by A
        let g1 = graph(&[(a, &[b])]);
        let err = check_cycle(&g1, &uid(b), &uid(a)).unwrap_err();
        assert_eq!(err.path, vec![uid(b), uid(a), uid(b)]);
        assert!(err.render().contains(" → "));

        // 3-hop: A→B→C, offer C→A
        let g2 = graph(&[(a, &[b]), (b, &[c])]);
        let err = check_cycle(&g2, &uid(c), &uid(a)).unwrap_err();
        assert_eq!(err.path, vec![uid(c), uid(a), uid(b), uid(c)]);

        // 6-hop: A→B→C→D→E→F, offer F→A
        let g3 = graph(&[(a, &[b]), (b, &[c]), (c, &[d]), (d, &[e]), (e, &[f])]);
        let err = check_cycle(&g3, &uid(f), &uid(a)).unwrap_err();
        assert_eq!(err.path, vec![uid(f), uid(a), uid(b), uid(c), uid(d), uid(e), uid(f)]);

        // A legal diamond MUST be allowed: A blocked by B and C, both
        // blocked by D. A false positive here silently blocks legitimate
        // work, which is worse than the bug it would be guarding against.
        let g4 = graph(&[(a, &[b]), (b, &[d]), (c, &[d])]);
        assert!(check_cycle(&g4, &uid(a), &uid(c)).is_ok(), "legal diamond must be allowed");
        // ...and the diamond's tip still cannot be closed back onto A.
        let g5 = graph(&[(a, &[b, c]), (b, &[d]), (c, &[d])]);
        assert!(check_cycle(&g5, &uid(d), &uid(a)).is_err());

        // A shortcut *along* an existing chain is not a cycle: A is
        // already transitively blocked by F, and saying so directly adds
        // no loop. Refusing this would be exactly the false positive that
        // silently blocks legitimate work.
        assert!(check_cycle(&g3, &uid(a), &uid(f)).is_ok(), "A→F is a shortcut, not a cycle");
        // Unrelated edges are fine.
        let g6 = graph(&[(a, &[b])]);
        assert!(check_cycle(&g6, &uid(c), &uid(d)).is_ok());
    }

    #[test]
    fn cycle_walk_terminates_on_a_graph_that_is_already_cyclic() {
        // A hand edit can put a cycle on disk that this function was never
        // asked about; the walk must still terminate.
        let g = graph(&[('A', &['B']), ('B', &['A'])]);
        let _ = check_cycle(&g, &uid('C'), &uid('A'));
    }

    #[test]
    fn block_graph_is_built_from_the_board() {
        let dep = uid('D');
        let t = ticket(&uid('A'), "blocked", &format!("blocked_by:\n  - {dep}\nreturn_lane: tester\n"));
        let free = ticket(&uid('C'), "todo", "");
        let g = BlockGraph::from_tasks(&[t, free]);
        assert_eq!(g.blockers(&uid('A')), &[dep]);
        assert!(g.blockers(&uid('C')).is_empty());
    }

    // -----------------------------------------------------------------
    // 1.6 Block / unblock
    // -----------------------------------------------------------------

    #[test]
    fn block_captures_the_return_lane_from_the_tickets_current_lane() {
        let p = pipe();
        let t = ticket(&uid('A'), "programmer", "");
        let req = BlockRequest {
            blocked_by: vec![uid('B')],
            reason: None,
            now: "2026-08-03T10:00:00Z".into(),
        };
        let BlockOutcome::Blocked { block, patch, log } = block(&t, &p, &BlockGraph::new(), &req)
        else {
            panic!("expected a block");
        };
        assert_eq!(block.return_lane(), "programmer");
        assert_eq!(block.blocked_by(), &[uid('B')]);
        assert_eq!(block.blocked_at(), "2026-08-03T10:00:00Z");
        assert_eq!(patch.lane.as_deref(), Some("blocked"));
        assert_eq!(patch.return_lane.as_deref(), Some("programmer"));
        assert_eq!(patch.blocked_by.as_deref(), Some(&[uid('B')][..]));
        assert_eq!(patch.block_reason.as_deref(), Some(""));
        // Exactly the block keys and nothing else.
        assert_eq!(patch.status, None);
        assert_eq!(patch.assignee, None);
        assert_eq!(patch.bounces, None);
        assert!(log.contains("returns to programmer"));
    }

    #[test]
    fn extending_a_block_preserves_the_original_return_lane() {
        let p = pipe();
        let t = ticket(
            &uid('A'),
            "blocked",
            &format!("return_lane: refinement\nblocked_by:\n  - {}\n", uid('B')),
        );
        let req = BlockRequest {
            blocked_by: vec![uid('C')],
            reason: Some("and the upstream 2.0 release".into()),
            now: "2026-08-04T10:00:00Z".into(),
        };
        let BlockOutcome::Blocked { block, .. } = block(&t, &p, &BlockGraph::new(), &req) else {
            panic!("expected a block");
        };
        assert_eq!(
            block.return_lane(),
            "refinement",
            "re-blocking must not overwrite the return lane with the blocked lane"
        );
        assert_eq!(block.blocked_by(), &[uid('B'), uid('C')]);
        assert_eq!(block.reason(), Some("and the upstream 2.0 release"));
    }

    #[test]
    fn a_block_with_no_recoverable_return_lane_is_refused_not_invented() {
        let p = pipe();
        // Already in the blocked lane, no `return_lane` on the file — the
        // hand-edit case. There is no lane to capture and none is guessed.
        let t = ticket(&uid('A'), "blocked", "");
        let req = BlockRequest { reason: Some("x".into()), now: "n".into(), ..Default::default() };
        assert_eq!(
            block(&t, &p, &BlockGraph::new(), &req),
            BlockOutcome::Refused { reason: BlockRefusal::MissingReturnLane }
        );
    }

    #[test]
    fn block_refuses_paths_non_ulids_self_edges_and_empty_requests() {
        let p = pipe();
        let id = uid('A');
        let t = ticket(&id, "programmer", "");
        let g = BlockGraph::new();
        let req = |deps: Vec<String>, reason: Option<&str>| BlockRequest {
            blocked_by: deps,
            reason: reason.map(str::to_string),
            now: "n".into(),
        };
        let refusal = |o: BlockOutcome| match o {
            BlockOutcome::Refused { reason } => reason,
            other => panic!("expected refusal, got {other:?}"),
        };
        assert_eq!(
            refusal(block(&t, &p, &g, &req(vec![".ken-workspace/tasks/x.md".into()], None))),
            BlockRefusal::PathBlocker(".ken-workspace/tasks/x.md".into())
        );
        assert_eq!(
            refusal(block(&t, &p, &g, &req(vec!["not-a-ulid".into()], None))),
            BlockRefusal::MalformedBlocker("not-a-ulid".into())
        );
        assert_eq!(
            refusal(block(&t, &p, &g, &req(vec![id.clone()], None))),
            BlockRefusal::SelfBlock(id)
        );
        assert_eq!(refusal(block(&t, &p, &g, &req(vec![], None))), BlockRefusal::Empty);
        // A reason alone is a perfectly good block.
        assert!(matches!(
            block(&t, &p, &g, &req(vec![], Some("waiting on a person"))),
            BlockOutcome::Blocked { .. }
        ));
    }

    #[test]
    fn block_refuses_a_cycle_before_producing_any_patch() {
        let p = pipe();
        let a = uid('A');
        let b = uid('B');
        // B is already blocked by A; blocking A by B would close the loop.
        let g = graph(&[('B', &['A'])]);
        let t = ticket(&a, "programmer", "");
        let out = block(
            &t,
            &p,
            &g,
            &BlockRequest { blocked_by: vec![b.clone()], reason: None, now: "n".into() },
        );
        match out {
            BlockOutcome::Refused { reason: BlockRefusal::Cycle(path) } => {
                assert_eq!(path.path, vec![a, b.clone(), uid('A')]);
                assert!(path.render().contains(&b));
            }
            other => panic!("expected a cycle refusal, got {other:?}"),
        }
    }

    #[test]
    fn block_refuses_a_cycle_closed_by_two_deps_in_the_same_request() {
        let p = pipe();
        // C is blocked by A. Asking to block A by [B, C] must be refused
        // on the C edge even though B alone is fine.
        let g = graph(&[('C', &['A'])]);
        let t = ticket(&uid('A'), "programmer", "");
        let out = block(
            &t,
            &p,
            &g,
            &BlockRequest {
                blocked_by: vec![uid('B'), uid('C')],
                reason: None,
                now: "n".into(),
            },
        );
        assert!(matches!(
            out,
            BlockOutcome::Refused { reason: BlockRefusal::Cycle(_) }
        ));
    }

    #[test]
    fn unblock_clears_dependencies_and_reason_independently() {
        let p = pipe();
        let t = ticket(
            &uid('A'),
            "blocked",
            &format!("return_lane: programmer\nblocked_by:\n  - {}\nblock_reason: upstream 2.0\n", uid('B')),
        );
        // Clearing only the dependency leaves the reason holding it.
        let out = unblock(&t, &p, &UnblockRequest { clear_deps: true, clear_reason: false });
        match out {
            UnblockOutcome::StillBlocked { remaining_deps, remaining_reason, patch } => {
                assert!(remaining_deps.is_empty());
                assert_eq!(remaining_reason.as_deref(), Some("upstream 2.0"));
                assert_eq!(patch.lane, None, "it does not go home yet");
            }
            other => panic!("expected StillBlocked, got {other:?}"),
        }
        // Clearing only the reason leaves the dependency holding it.
        assert!(matches!(
            unblock(&t, &p, &UnblockRequest { clear_deps: false, clear_reason: true }),
            UnblockOutcome::StillBlocked { .. }
        ));
        // Clearing both releases it back to the recorded return lane.
        match unblock(&t, &p, &UnblockRequest { clear_deps: true, clear_reason: true }) {
            UnblockOutcome::Released { return_lane, patch, reset_bounces, .. } => {
                assert_eq!(return_lane, "programmer");
                assert_eq!(patch.lane.as_deref(), Some("programmer"));
                assert_eq!(patch.blocked_by.as_deref(), Some(&[][..]));
                assert_eq!(patch.block_reason.as_deref(), Some(""));
                assert!(!reset_bounces, "a dependency block keeps its bounce history");
                assert_eq!(patch.bounces, None);
            }
            other => panic!("expected Released, got {other:?}"),
        }
        assert_eq!(
            unblock(&t, &p, &UnblockRequest::default()),
            UnblockOutcome::Refused { reason: UnblockRefusal::Empty }
        );
        assert_eq!(
            unblock(&ticket(&uid('C'), "todo", ""), &p, &UnblockRequest { clear_deps: true, clear_reason: true }),
            UnblockOutcome::Refused { reason: UnblockRefusal::NotBlocked }
        );
    }

    #[test]
    fn unblocking_a_retry_capped_ticket_resets_bounces() {
        let p = pipe();
        let t = ticket(
            &uid('A'),
            "blocked",
            "return_lane: programmer\nbounces: 4\nblock_reason: exceeded retry cap (4 bounces)\n",
        );
        match unblock(&t, &p, &UnblockRequest { clear_deps: true, clear_reason: true }) {
            UnblockOutcome::Released { patch, reset_bounces, .. } => {
                assert!(reset_bounces);
                assert_eq!(patch.bounces, Some(0));
            }
            other => panic!("expected Released, got {other:?}"),
        }
        assert!(is_retry_cap_reason("exceeded retry cap (4 bounces)"));
        assert!(!is_retry_cap_reason("waiting on the upstream 2.0 release"));
    }

    // -----------------------------------------------------------------
    // 1.8 Admission — the safety-critical table
    // -----------------------------------------------------------------

    const ALL_ENTRIES: [EntryKind; 4] = [
        EntryKind::Kickoff,
        EntryKind::Claim,
        EntryKind::Unblock,
        EntryKind::AutoTransition,
    ];

    /// D5's hard invariant, asserted across the full cross-product of
    /// everything that could plausibly out-vote it.
    #[test]
    fn blocked_is_refused_before_gate_cap_or_anything_else() {
        let dep = uid('D');
        // Every way a ticket can be blocked...
        let blocked_tickets = vec![
            ("in the blocked lane", ticket(&uid('A'), "blocked", "return_lane: programmer\nblock_reason: waiting\n")),
            ("by a dependency, still in-lane", ticket(&uid('B'), "programmer", &format!("blocked_by:\n  - {dep}\n"))),
            ("by a reason, still in-lane", ticket(&uid('C'), "programmer", "block_reason: upstream 2.0\n")),
            ("by the retry cap", ticket(&uid('E'), "blocked", "return_lane: programmer\nbounces: 4\nblock_reason: exceeded retry cap (4 bounces)\n")),
            // ...including one with no boundary, which would otherwise be
            // a *downgrade* rather than a refusal.
            ("blocked and unbounded", unbounded(&uid('F'), "blocked", "return_lane: programmer\nblock_reason: waiting\n")),
        ];

        // ...against every way a lane can invite work, with the master
        // switch on, an auto lane, and a completely free concurrency cap.
        let auto = auto_pipe();
        let plain = pipe();
        let ledger: [&[RunRecord]; 2] = [&[], &[running_run()]];

        for (what, t) in &blocked_tickets {
            for p in [&auto, &plain] {
                for lane_id in ["programmer", "tester", "documentation", "blocked", "signoff", "todo"] {
                    let lane = p.lane(lane_id).unwrap();
                    for entry in ALL_ENTRIES {
                        for runs in ledger {
                            let got = admit(t, lane, p, runs, entry);
                            assert!(
                                matches!(got, Admission::Refused { reason: RefusalReason::Blocked { .. } }),
                                "blocked ticket ({what}) in lane {lane_id} (auto={}, kickoff={:?}, entry={entry:?}) \
                                 must be Refused-Blocked before any other condition, got {got:?}",
                                p.auto,
                                lane.kickoff
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn blocked_beats_an_auto_lane_with_the_master_switch_on() {
        // The single scenario from the spec, spelled out on its own so a
        // regression names itself.
        let p = auto_pipe();
        let lane = p.lane("programmer").unwrap();
        assert_eq!(lane.kickoff, Kickoff::Auto);
        assert!(p.auto);
        let t = ticket(&uid('A'), "blocked", "return_lane: programmer\nblock_reason: waiting\n");
        match admit(&t, lane, &p, &[], EntryKind::AutoTransition) {
            Admission::Refused { reason: RefusalReason::Blocked { return_lane, block_reason, .. } } => {
                assert_eq!(return_lane.as_deref(), Some("programmer"));
                assert_eq!(block_reason.as_deref(), Some("waiting"));
            }
            other => panic!("expected a blocked refusal, got {other:?}"),
        }
        // An unblocked sibling in the same lane *does* start, so the test
        // above is proving the block and not a broken fixture.
        let ok = ticket(&uid('B'), "programmer", "");
        assert_eq!(admit(&ok, lane, &p, &[], EntryKind::AutoTransition), Admission::Start);
    }

    /// D5 again, from the other direction: no input whatsoever that
    /// arrives by unblock may reach `Start`. Under today's pull runner
    /// this is a convenience; under a process-spawning runner it is the
    /// rule that stops a 2am dependency completion from writing code.
    #[test]
    fn nothing_arriving_by_unblock_can_ever_start() {
        let auto = auto_pipe();
        let plain = pipe();
        let tickets = [
            ticket(&uid('A'), "programmer", ""),
            unbounded(&uid('B'), "programmer", ""),
            ticket(&uid('C'), "programmer", "return_lane: programmer\n"),
        ];
        for p in [&auto, &plain] {
            for lane in &p.lanes {
                for t in &tickets {
                    for runs in [&[][..], &[running_run()][..]] {
                        let got = admit(t, lane, p, runs, EntryKind::Unblock);
                        assert!(
                            !matches!(got, Admission::Start),
                            "unblock reached Start in lane {} (kickoff {:?}, auto {}): {got:?}",
                            lane.id,
                            lane.kickoff,
                            p.auto
                        );
                    }
                }
            }
        }
        // And the positive half: it is a *confirmation*, not a refusal,
        // for a runnable lane — the ticket really does come home.
        let lane = auto.lane("programmer").unwrap();
        assert_eq!(
            admit(&tickets[0], lane, &auto, &[], EntryKind::Unblock),
            Admission::Confirm { reason: ConfirmReason::Unblocked }
        );
    }

    #[test]
    fn every_refusal_variant_actually_serializes() {
        // Regression guard for a bug that shipped silently: these three enums
        // are internally tagged, and serde cannot serialize a newtype variant
        // in that representation — it fails at RUNTIME, not compile time, and
        // only on refusal paths. So the UI would break exactly when it needed
        // to say why work was refused. Every payload-carrying variant must be
        // a struct variant; this test is what keeps that true.
        let cases: Vec<(&str, serde_json::Result<String>)> = vec![
            (
                "RefusalReason::Blocked",
                serde_json::to_string(&RefusalReason::Blocked {
                    return_lane: Some("programmer".into()),
                    blocked_by: vec![uid('B')],
                    block_reason: None,
                }),
            ),
            (
                "RefusalReason::HumanLane",
                serde_json::to_string(&RefusalReason::HumanLane { lane: "signoff".into() }),
            ),
            (
                "RefusalReason::NoAgent",
                serde_json::to_string(&RefusalReason::NoAgent { lane: "todo".into() }),
            ),
            (
                "RefusalReason::ManualLane",
                serde_json::to_string(&RefusalReason::ManualLane { lane: "a".into() }),
            ),
            (
                "TransitionRefusal::UnknownLane",
                serde_json::to_string(&TransitionRefusal::UnknownLane { status: "bogus".into() }),
            ),
            (
                "BlockRefusal::UnknownLane",
                serde_json::to_string(&BlockRefusal::UnknownLane { status: "bogus".into() }),
            ),
        ];
        for (name, result) in cases {
            let json = result.unwrap_or_else(|e| panic!("{name} failed to serialize: {e}"));
            assert!(
                json.starts_with('{') && json.contains("\":"),
                "{name} serialized to a non-object: {json}"
            );
        }
    }

    #[test]
    fn admission_gate_table_for_unblocked_tickets() {
        let plain = pipe();
        let auto = auto_pipe();
        let t = ticket(&uid('A'), "programmer", "");

        // Holding columns and the human lane refuse outright.
        assert_eq!(
            admit(&t, plain.lane("todo").unwrap(), &plain, &[], EntryKind::Kickoff),
            Admission::Refused { reason: RefusalReason::NoAgent { lane: "todo".into() } }
        );
        assert_eq!(
            admit(&t, plain.lane("signoff").unwrap(), &plain, &[], EntryKind::Kickoff),
            Admission::Refused { reason: RefusalReason::HumanLane { lane: "signoff".into() } }
        );
        // A manual lane reached by automation, rather than by a human.
        let manual_agent = parse_pipeline(
            Path::new("/ws/m.md"),
            "---\nid: m\nauto: true\nlanes:\n  - id: a\n    maps_to: doing\n    agent: bob\n    kickoff: manual\n---\n",
        );
        let m_lane = manual_agent.lane("a").unwrap();
        let mt = task_from(&format!(
            "---\nid: {}\nstatus: a\npipeline: m\nscope:\n  - x/**\nverify: v\n---\n",
            uid('A')
        ));
        assert_eq!(
            admit(&mt, m_lane, &manual_agent, &[], EntryKind::AutoTransition),
            Admission::Refused { reason: RefusalReason::ManualLane { lane: "a".into() } }
        );
        assert_eq!(
            admit(&mt, m_lane, &manual_agent, &[], EntryKind::Kickoff),
            Admission::Confirm { reason: ConfirmReason::ManualKickoff }
        );

        // A `confirm` lane confirms, whatever the master switch says.
        assert_eq!(
            admit(&t, plain.lane("tester").unwrap(), &plain, &[], EntryKind::Kickoff),
            Admission::Confirm { reason: ConfirmReason::LaneGate }
        );
        // An `auto` lane with the master switch off is still a confirm.
        let mut master_off = auto_pipe();
        master_off.auto = false;
        assert_eq!(
            admit(&t, master_off.lane("programmer").unwrap(), &master_off, &[], EntryKind::AutoTransition),
            Admission::Confirm { reason: ConfirmReason::AutoDisabled }
        );
        // Master switch on + auto lane + boundary + free cap ⇒ Start.
        assert_eq!(
            admit(&t, auto.lane("programmer").unwrap(), &auto, &[], EntryKind::AutoTransition),
            Admission::Start
        );
    }

    #[test]
    fn an_unbounded_ticket_can_never_auto_run() {
        let auto = auto_pipe();
        let lane = auto.lane("programmer").unwrap();
        let no_verify = task_from(&format!(
            "---\nid: {}\nstatus: programmer\npipeline: default\nscope:\n  - x/**\n---\n",
            uid('A')
        ));
        assert_eq!(
            admit(&no_verify, lane, &auto, &[], EntryKind::AutoTransition),
            Admission::Confirm {
                reason: ConfirmReason::MissingBoundary { scope: false, verify: true }
            }
        );
        let neither = unbounded(&uid('B'), "programmer", "");
        assert_eq!(
            admit(&neither, lane, &auto, &[], EntryKind::AutoTransition),
            Admission::Confirm {
                reason: ConfirmReason::MissingBoundary { scope: true, verify: true }
            }
        );
        // The boundary reason outranks the unblock reason so the tray can
        // say the more actionable thing, but it is still a Confirm.
        assert_eq!(
            admit(&neither, lane, &auto, &[], EntryKind::Unblock),
            Admission::Confirm {
                reason: ConfirmReason::MissingBoundary { scope: true, verify: true }
            }
        );
        // ...and an external agent cannot claim past a missing boundary.
        assert!(matches!(
            admit(&neither, lane, &auto, &[], EntryKind::Claim),
            Admission::Confirm { .. }
        ));
    }

    #[test]
    fn the_cap_queues_rather_than_parallelises() {
        let auto = auto_pipe();
        let lane = auto.lane("programmer").unwrap();
        let t = ticket(&uid('A'), "programmer", "");
        assert_eq!(auto.concurrency_cap, 1);
        assert_eq!(admit(&t, lane, &auto, &[], EntryKind::AutoTransition), Admission::Start);
        assert_eq!(
            admit(&t, lane, &auto, &[running_run()], EntryKind::AutoTransition),
            Admission::Queued { running: 1, cap: 1 }
        );
        // Finished runs don't hold a slot.
        let done = RunRecord { outcome: Some(RunOutcome::Pass), ..RunRecord::default() };
        let queued = RunRecord { outcome: Some(RunOutcome::Queued), ..RunRecord::default() };
        assert_eq!(running_runs(&[done, queued]), 0);
        // A claim respects the cap too.
        assert_eq!(
            admit(&t, auto.lane("tester").unwrap(), &auto, &[running_run()], EntryKind::Claim),
            Admission::Queued { running: 1, cap: 1 }
        );
    }

    // -----------------------------------------------------------------
    // 1.9 Transitions and the bounce cap
    // -----------------------------------------------------------------

    fn moved(t: Transition) -> (String, String, bool, u32) {
        match t {
            Transition::Moved { from, to, backward, bounces, .. } => (from, to, backward, bounces),
            other => panic!("expected a move, got {other:?}"),
        }
    }

    #[test]
    fn forward_transition_table() {
        let p = pipe();
        let now = "2026-08-03T10:00:00Z";
        for (from, to) in [
            ("ideas", "backlog"),
            ("backlog", "todo"),
            ("todo", "investigation"),
            ("investigation", "refinement"),
            ("refinement", "programmer"),
            ("programmer", "tester"),
            ("tester", "architect"),
            ("architect", "qa"),
            ("qa", "signoff"),
            ("signoff", "documentation"),
        ] {
            let t = ticket(&uid('A'), from, "");
            let (f, got, backward, bounces) =
                moved(advance(&t, &p, AdvanceOutcome::Pass, now));
            assert_eq!((f.as_str(), got.as_str()), (from, to));
            assert!(!backward);
            assert_eq!(bounces, 0, "a forward move never touches the counter");
        }
        // Documentation is terminal: no `on_pass` edge.
        assert_eq!(
            advance(&ticket(&uid('A'), "documentation", ""), &p, AdvanceOutcome::Pass, now),
            Transition::Refused {
                reason: TransitionRefusal::NoEdge {
                    lane: "documentation".into(),
                    outcome: AdvanceOutcome::Pass
                }
            }
        );
        // A lane with no `on_fail` refuses a fail.
        assert!(matches!(
            advance(&ticket(&uid('A'), "programmer", ""), &p, AdvanceOutcome::Fail, now),
            Transition::Refused { reason: TransitionRefusal::NoEdge { .. } }
        ));
        // An unresolvable current lane refuses rather than guessing.
        assert_eq!(
            advance(&ticket(&uid('A'), "programmerr", ""), &p, AdvanceOutcome::Pass, now),
            Transition::Refused { reason: TransitionRefusal::UnknownLane { status: "programmerr".into() } }
        );
    }

    #[test]
    fn every_bounce_edge_counts_as_a_bounce() {
        let p = pipe();
        let now = "n";
        for (from, to) in [
            ("tester", "programmer"),
            ("architect", "refinement"),
            ("qa", "programmer"),
            ("signoff", "refinement"),
        ] {
            let t = ticket(&uid('A'), from, "");
            let out = advance(&t, &p, AdvanceOutcome::Fail, now);
            let (f, got, backward, bounces) = moved(out);
            assert_eq!((f.as_str(), got.as_str()), (from, to));
            assert!(backward, "{from} → {to} is backward in definition order");
            assert_eq!(bounces, 1);
        }
    }

    #[test]
    fn the_bounce_cap_boundary_produces_a_block_not_a_halt() {
        let p = pipe();
        let now = "2026-08-03T10:00:00Z";
        assert_eq!(p.bounce_cap, 3);

        // 0→1, 1→2, 2→3 all move; bounces==3 is exactly at the cap.
        for (before, after) in [(0u32, 1u32), (1, 2), (2, 3)] {
            let t = ticket(&uid('A'), "tester", &format!("bounces: {before}\n"));
            let (_, to, backward, bounces) = moved(advance(&t, &p, AdvanceOutcome::Fail, now));
            assert_eq!(to, "programmer");
            assert!(backward);
            assert_eq!(bounces, after);
        }

        // The next one would make 4, which exceeds 3 ⇒ block.
        let t = ticket(&uid('A'), "tester", "bounces: 3\n");
        match advance(&t, &p, AdvanceOutcome::Fail, now) {
            Transition::Blocked { from, would_have_entered, block, patch, log } => {
                assert_eq!(from, "tester");
                assert_eq!(would_have_entered, "programmer");
                // D4: the return lane is the lane it was bouncing *to*.
                assert_eq!(block.return_lane(), "programmer");
                assert_eq!(block.reason(), Some("exceeded retry cap (4 bounces)"));
                assert!(is_retry_cap_reason(block.reason().unwrap()));
                assert_eq!(block.blocked_at(), now);
                assert_eq!(patch.lane.as_deref(), Some("blocked"), "it goes to the Blocked lane");
                assert_eq!(patch.return_lane.as_deref(), Some("programmer"));
                assert_eq!(patch.bounces, Some(4));
                assert!(log.contains("refused"));
            }
            other => panic!("expected a block at the cap boundary, got {other:?}"),
        }
        // And well past the cap it stays a block, never a second state.
        assert!(matches!(
            advance(&ticket(&uid('A'), "architect", "bounces: 99\n"), &p, AdvanceOutcome::Fail, now),
            Transition::Blocked { .. }
        ));
    }

    #[test]
    fn a_retry_cap_block_and_a_dependency_block_are_the_same_thing() {
        let p = pipe();
        let capped = ticket(&uid('A'), "blocked", "return_lane: programmer\nbounces: 4\nblock_reason: exceeded retry cap (4 bounces)\n");
        let dependency = ticket(&uid('B'), "blocked", &format!("return_lane: tester\nblocked_by:\n  - {}\n", uid('D')));
        let f = TaskFilter { blocked: Some(BlockedFilter::Blocked), ..TaskFilter::default() };
        // Same lane, same filter, and both refused by the same function.
        assert_eq!(capped.status_raw, dependency.status_raw);
        assert!(tasks::matches(&capped, &f) && tasks::matches(&dependency, &f));
        let lane = p.lane("programmer").unwrap();
        for t in [&capped, &dependency] {
            assert!(matches!(
                admit(t, lane, &p, &[], EntryKind::Kickoff),
                Admission::Refused { reason: RefusalReason::Blocked { .. } }
            ));
        }
    }

    // -----------------------------------------------------------------
    // 1.10 Unblock evaluation
    // -----------------------------------------------------------------

    #[test]
    fn unblock_evaluation_needs_every_dependency_terminal_and_the_reason_cleared() {
        let p = pipe();
        let d1 = uid('D');
        let d2 = uid('E');
        let blocker_done = ticket(&d1, "documentation", "");
        let blocker_open = ticket(&d2, "programmer", "");

        let dependent = |deps: &[&str], extra: &str| {
            let list: String = deps.iter().map(|d| format!("  - {d}\n")).collect();
            ticket(&uid('A'), "blocked", &format!("return_lane: programmer\nblocked_by:\n{list}{extra}"))
        };

        // One terminal dependency, no reason ⇒ free.
        let board = vec![dependent(&[&d1], ""), blocker_done.clone(), blocker_open.clone()];
        let freed = evaluate_unblocks(&board, &p, &d1);
        assert_eq!(freed.len(), 1);
        assert_eq!(freed[0].ticket_id, uid('A'));
        assert_eq!(freed[0].return_lane, "programmer");
        assert_eq!(freed[0].patch.lane.as_deref(), Some("programmer"));
        assert_eq!(freed[0].patch.blocked_by.as_deref(), Some(&[][..]));

        // A second, non-terminal dependency holds it.
        let board = vec![dependent(&[&d1, &d2], ""), blocker_done.clone(), blocker_open.clone()];
        assert!(evaluate_unblocks(&board, &p, &d1).is_empty());

        // A reason holds it even with every dependency terminal.
        let board = vec![
            dependent(&[&d1], "block_reason: upstream 2.0\n"),
            blocker_done.clone(),
        ];
        assert!(evaluate_unblocks(&board, &p, &d1).is_empty());

        // An unresolvable blocker is not a satisfied one.
        let board = vec![dependent(&[&uid('Z')], ""), blocker_done.clone()];
        assert!(evaluate_all_unblocks(&board, &p).is_empty());

        // A missing/orphaned return lane is a tray entry, not a guess.
        let orphan = ticket(&uid('B'), "blocked", &format!("return_lane: gone\nblocked_by:\n  - {d1}\n"));
        assert!(evaluate_unblocks(&[orphan, blocker_done.clone()], &p, &d1).is_empty());

        // The targeted evaluation only considers dependents naming the
        // ticket that just went terminal; the open-time sweep sees all.
        let board = vec![dependent(&[&d1], ""), blocker_done];
        assert!(evaluate_unblocks(&board, &p, &d2).is_empty());
        assert_eq!(evaluate_all_unblocks(&board, &p).len(), 1);
    }

    #[test]
    fn a_freed_ticket_re_enters_through_the_gate_and_starts_nothing() {
        // The 1.10 → 1.8 handoff, asserted end to end: what
        // `evaluate_unblocks` returns, re-admitted with `EntryKind::
        // Unblock`, is a confirmation even in an `auto` lane.
        let auto = auto_pipe();
        let d = uid('D');
        let blocker = ticket(&d, "documentation", "");
        let dependent = ticket(
            &uid('A'),
            "blocked",
            &format!("return_lane: programmer\nblocked_by:\n  - {d}\n"),
        );
        let board = vec![dependent.clone(), blocker];
        let freed = evaluate_unblocks(&board, &auto, &d);
        assert_eq!(freed.len(), 1);

        // Apply the patch the way a caller would, then re-admit.
        let home = ticket(&uid('A'), &freed[0].return_lane, "");
        let lane = auto.lane(&freed[0].return_lane).unwrap();
        assert_eq!(lane.kickoff, Kickoff::Auto);
        assert!(auto.auto);
        assert_eq!(
            admit(&home, lane, &auto, &[], EntryKind::Unblock),
            Admission::Confirm { reason: ConfirmReason::Unblocked }
        );
    }

    // -----------------------------------------------------------------
    // Paths
    // -----------------------------------------------------------------

    #[test]
    fn pipeline_paths_follow_the_workspace_convention() {
        let root = Path::new("/ws");
        assert!(pipelines_dir(root).ends_with("pipelines"));
        assert_eq!(
            pipelines_dir(root),
            Path::new("/ws").join(crate::workspace::CONFIG_DIR).join("pipelines")
        );
        assert!(pipeline_path(root, "default").ends_with("default.md"));
    }

    // -----------------------------------------------------------------
    // 1.11 Run ledger: pathing, parse/write, ledger scan, derived queue,
    // stale detection
    // -----------------------------------------------------------------

    fn run(id: &str, ticket_id: &str, outcome: RunOutcome) -> RunRecord {
        RunRecord {
            id: id.to_string(),
            ticket: ticket_id.to_string(),
            pipeline: "default".to_string(),
            lane: "programmer".to_string(),
            agent: "programmer".to_string(),
            model: "sonnet".to_string(),
            scope: vec!["crates/ken-core/**".to_string()],
            verify: "cargo test -p ken-core".to_string(),
            started: "2026-08-01T10:00:00Z".to_string(),
            ended: String::new(),
            outcome: Some(outcome),
            outcome_raw: outcome.as_str().to_string(),
            artifacts: Vec::new(),
            report: "in progress".to_string(),
        }
    }

    #[test]
    fn run_paths_follow_the_monthly_ledger_convention() {
        let root = Path::new("/ws");
        assert_eq!(
            runs_dir(root),
            Path::new("/ws").join(crate::workspace::CONFIG_DIR).join("runs")
        );
        assert_eq!(run_month_dir(root, "2026-08"), runs_dir(root).join("2026-08"));
        assert_eq!(
            run_path(root, "2026-08", &uid('A')),
            runs_dir(root).join("2026-08").join(format!("{}.md", uid('A')))
        );
        assert_eq!(run_month("2026-08-03T10:00:00Z"), "2026-08");
        assert_eq!(run_month("bad"), "bad");
    }

    #[test]
    fn run_record_round_trips_through_compose_and_parse() {
        let r = run(&uid('R'), &uid('A'), RunOutcome::Pass);
        let text = compose_run(&r);
        assert!(text.contains("outcome: pass"));
        let parsed = parse_run(Path::new("/ws/.ken-workspace/runs/2026-08/x.md"), &text);
        assert_eq!(parsed, r);
    }

    #[test]
    fn parse_run_falls_back_to_the_file_stem_when_id_is_absent() {
        let text = "---\nticket: T1\noutcome: queued\n---\n\nreport body\n";
        let parsed = parse_run(Path::new("/ws/.ken-workspace/runs/2026-08/RUN123.md"), text);
        assert_eq!(parsed.id, "RUN123");
        assert_eq!(parsed.ticket, "T1");
        assert_eq!(parsed.outcome, Some(RunOutcome::Queued));
        assert_eq!(parsed.report, "report body");
    }

    #[test]
    fn scan_runs_parses_every_file_pair_in_order() {
        let a = compose_run(&run(&uid('A'), &uid('X'), RunOutcome::Running));
        let b = compose_run(&run(&uid('B'), &uid('Y'), RunOutcome::Queued));
        let pa = PathBuf::from("/ws/a.md");
        let pb = PathBuf::from("/ws/b.md");
        let files: Vec<(&Path, &str)> = vec![(pa.as_path(), a.as_str()), (pb.as_path(), b.as_str())];
        let scanned = scan_runs(files);
        assert_eq!(scanned.len(), 2);
        assert_eq!(scanned[0].id, uid('A'));
        assert_eq!(scanned[1].id, uid('B'));
    }

    #[test]
    fn stale_detection_is_every_running_record_the_session_has_no_memory_of() {
        let live = run(&uid('L'), &uid('T'), RunOutcome::Running);
        let orphaned = run(&uid('O'), &uid('T'), RunOutcome::Running);
        let queued = run(&uid('Q'), &uid('T'), RunOutcome::Queued);
        let blocked = run(&uid('K'), &uid('T'), RunOutcome::Blocked);
        let done = run(&uid('N'), &uid('T'), RunOutcome::Pass);
        let runs = vec![live.clone(), orphaned.clone(), queued.clone(), blocked.clone(), done];

        // Fresh workspace-open: nothing known ⇒ every `running` record is
        // stale (D13's "no live run after restart").
        let empty = BTreeSet::new();
        let q = derive_queue(&[], &[], &runs, &empty);
        assert_eq!(q.running.len(), 0);
        assert_eq!(q.stale.len(), 2);
        assert!(q.stale.iter().any(|r| r.id == live.id));
        assert!(q.stale.iter().any(|r| r.id == orphaned.id));
        assert_eq!(q.queued, vec![queued.clone()]);
        assert_eq!(q.blocked, vec![blocked.clone()]);

        // Mid-session: the caller's own claim is remembered, so only the
        // orphan is stale, and the live one is not silently marked passed.
        let mut known = BTreeSet::new();
        known.insert(live.id.clone());
        let q2 = derive_queue(&[], &[], &runs, &known);
        assert_eq!(q2.running, vec![live]);
        assert_eq!(q2.stale, vec![orphaned]);
    }

    #[test]
    fn waiting_human_is_derived_from_admit_not_guessed() {
        let p = pipe();
        let t = ticket(&uid('A'), "tester", ""); // kickoff: confirm
        let blocked_t = ticket(&uid('B'), "programmer", "blocked_by:\n  - unrelated\n");
        let board = vec![t.clone(), blocked_t];
        let q = derive_queue(&board, &[p], &[], &BTreeSet::new());
        assert_eq!(q.waiting_human, vec![t.id.clone()], "blocked ticket must not appear");
    }

    // -----------------------------------------------------------------
    // 1.12 Sign-off child composition
    // -----------------------------------------------------------------

    #[test]
    fn accept_with_comments_composes_child_and_parent_advance() {
        let p = pipe();
        let parent = ticket(&uid('P'), "signoff", "project: alpha\nprojects:\n  - beta\n");
        let out = compose_signoff_child(&parent, &p, "please add a migration note", "2026-08-03T00:00:00Z");
        let Ok(SignoffChild { child, parent_transition, parent_log }) = out else {
            panic!("expected Ok, got {out:?}");
        };
        assert_eq!(child.fields.lane.as_deref(), Some("todo"));
        assert_eq!(child.fields.pipeline.as_deref(), Some("default"));
        assert_eq!(child.fields.project.as_deref(), Some("alpha"));
        assert_eq!(child.fields.projects, Some(vec!["beta".to_string()]));
        assert_eq!(child.fields.parent.as_deref(), Some(parent.id.as_str()));
        assert_eq!(child.fields.origin.as_deref(), Some("signoff"));
        assert!(child.fields.scope.is_none(), "child must not inherit scope");
        assert!(child.fields.verify.is_none(), "child must not inherit verify");
        assert_eq!(child.body, "please add a migration note");

        match parent_transition {
            Transition::Moved { from, to, backward, .. } => {
                assert_eq!(from, "signoff");
                assert_eq!(to, "documentation");
                assert!(!backward);
            }
            other => panic!("expected Moved, got {other:?}"),
        }
        assert!(parent_log.contains("please add a migration note"));
    }

    #[test]
    fn accept_with_comments_refuses_a_non_human_lane() {
        let p = pipe();
        let parent = ticket(&uid('P'), "programmer", "");
        assert_eq!(
            compose_signoff_child(&parent, &p, "a comment", "2026-08-03").unwrap_err(),
            SignoffRefusal::NotHumanLane("programmer".to_string())
        );
    }

    #[test]
    fn accept_with_comments_refuses_an_empty_comment() {
        let p = pipe();
        let parent = ticket(&uid('P'), "signoff", "");
        assert_eq!(
            compose_signoff_child(&parent, &p, "   ", "2026-08-03").unwrap_err(),
            SignoffRefusal::EmptyComment
        );
    }

    #[test]
    fn accept_with_comments_refuses_when_the_pipeline_has_no_on_pass_edge() {
        let md = DEFAULT_MD.replace(
            "    on_pass: documentation\n    on_fail: refinement\n",
            "    on_fail: refinement\n",
        );
        let p = parse_pipeline(Path::new("/ws/p.md"), &md);
        assert!(p.lane("signoff").unwrap().on_pass.is_none(), "fixture: on_pass must be gone");
        let parent = ticket(&uid('P'), "signoff", "");
        assert_eq!(
            compose_signoff_child(&parent, &p, "a comment", "2026-08-03").unwrap_err(),
            SignoffRefusal::NoOnPass("signoff".to_string())
        );
    }

    #[test]
    fn accept_with_comments_refuses_when_no_todo_lane_exists() {
        let md = r#"---
id: mini
lanes:
  - id: signoff
    name: Sign-off
    maps_to: review
    human: true
    kickoff: manual
    on_pass: documentation
  - id: documentation
    name: Documentation
    maps_to: done
    terminal: true
---
"#;
        let p = parse_pipeline(Path::new("/ws/mini.md"), md);
        let parent = task_from(&format!(
            "---\nid: {id}\ntitle: T\nstatus: signoff\npipeline: mini\nscope:\n  - x\nverify: y\n---\n\nBody.\n",
            id = uid('P')
        ));
        assert_eq!(
            compose_signoff_child(&parent, &p, "a comment", "2026-08-03").unwrap_err(),
            SignoffRefusal::NoTodoLane
        );
    }

    // -----------------------------------------------------------------
    // 1.13 Idea proposal + dedupe scoring
    // -----------------------------------------------------------------

    #[test]
    fn propose_idea_refuses_without_a_citation() {
        assert_eq!(
            propose_idea("A new idea", "body", "", "alpha", &[], "default").unwrap_err(),
            IdeaRefusal::MissingCitation
        );
        assert_eq!(
            propose_idea("A new idea", "body", "   ", "alpha", &[], "default").unwrap_err(),
            IdeaRefusal::MissingCitation
        );
    }

    #[test]
    fn propose_idea_refuses_an_empty_title() {
        assert_eq!(
            propose_idea("  ", "body", &uid('S'), "alpha", &[], "default").unwrap_err(),
            IdeaRefusal::EmptyTitle
        );
    }

    #[test]
    fn propose_idea_builds_a_trimmed_candidate() {
        let idea = propose_idea(
            " A new idea ",
            " body ",
            &uid('S'),
            "alpha",
            &["beta".to_string()],
            "default",
        )
        .unwrap();
        assert_eq!(idea.title, "A new idea");
        assert_eq!(idea.body, "body");
        assert_eq!(idea.spawned_by, uid('S'));
        assert_eq!(idea.projects, vec!["beta".to_string()]);
    }

    #[test]
    fn dedupe_scope_is_project_plus_linked_projects() {
        assert!(in_dedupe_scope("alpha", "alpha", &[]));
        assert!(in_dedupe_scope("alpha", "beta", &["beta", "gamma"]));
        assert!(!in_dedupe_scope("alpha", "delta", &["beta", "gamma"]));
    }

    #[test]
    fn dedupe_idea_lands_with_no_candidates() {
        let idea = propose_idea("Ship the thing", "body", &uid('S'), "alpha", &[], "default").unwrap();
        assert_eq!(dedupe_idea(&idea, &[]), DedupeVerdict::Land);
    }

    #[test]
    fn dedupe_idea_trusts_a_caller_supplied_semantic_score() {
        let idea = propose_idea("Ship the thing", "body", &uid('S'), "alpha", &[], "default").unwrap();
        let candidates = vec![DedupeCandidate {
            ticket_id: uid('X'),
            title: "totally different words".to_string(),
            project: "alpha".to_string(),
            score: Some(0.95),
        }];
        assert_eq!(
            dedupe_idea(&idea, &candidates),
            DedupeVerdict::NearDuplicate { ticket_id: uid('X'), score: 0.95 }
        );
    }

    #[test]
    fn dedupe_idea_falls_back_to_normalized_title_matching() {
        let idea = propose_idea("Add dark mode toggle", "body", &uid('S'), "alpha", &[], "default").unwrap();
        let exact = DedupeCandidate {
            ticket_id: uid('X'),
            title: "add dark mode toggle".to_string(),
            project: "alpha".to_string(),
            score: None,
        };
        let unrelated = DedupeCandidate {
            ticket_id: uid('Y'),
            title: "fix the login timeout bug".to_string(),
            project: "alpha".to_string(),
            score: None,
        };
        assert_eq!(dedupe_idea(&idea, &[unrelated.clone()]), DedupeVerdict::Land);
        assert_eq!(
            dedupe_idea(&idea, &[unrelated, exact]),
            DedupeVerdict::NearDuplicate { ticket_id: uid('X'), score: 1.0 }
        );
    }

    #[test]
    fn dedupe_idea_picks_the_highest_scoring_candidate() {
        let idea = propose_idea("Add dark mode toggle", "body", &uid('S'), "alpha", &[], "default").unwrap();
        let low = DedupeCandidate {
            ticket_id: uid('L'),
            title: "l".into(),
            project: "alpha".into(),
            score: Some(0.85),
        };
        let high = DedupeCandidate {
            ticket_id: uid('H'),
            title: "h".into(),
            project: "alpha".into(),
            score: Some(0.99),
        };
        assert_eq!(
            dedupe_idea(&idea, &[low, high]),
            DedupeVerdict::NearDuplicate { ticket_id: uid('H'), score: 0.99 }
        );
    }

    #[test]
    fn dedupe_log_line_only_fires_on_a_near_duplicate() {
        let idea = propose_idea("Add dark mode toggle", "body", &uid('S'), "alpha", &[], "default").unwrap();
        assert_eq!(dedupe_log_line(&idea, &DedupeVerdict::Land), None);
        let line =
            dedupe_log_line(&idea, &DedupeVerdict::NearDuplicate { ticket_id: uid('X'), score: 0.9 }).unwrap();
        assert!(line.contains(idea.spawned_by.as_str()));
        assert!(line.contains("Add dark mode toggle"));
        assert!(line.contains("90%"));
    }

    #[test]
    fn compose_idea_ticket_lands_in_the_ideas_lane() {
        let p = pipe();
        let idea = propose_idea("Add dark mode toggle", "body", &uid('S'), "alpha", &[], "default").unwrap();
        let nt = compose_idea_ticket(&idea, &p).unwrap();
        assert_eq!(nt.fields.lane.as_deref(), Some("ideas"));
        assert_eq!(nt.fields.spawned_by.as_deref(), Some(idea.spawned_by.as_str()));
        assert_eq!(nt.fields.origin.as_deref(), Some("generated"));
        assert_eq!(nt.title, "Add dark mode toggle");
    }

    #[test]
    fn compose_idea_ticket_refuses_without_an_ideas_lane() {
        let md = r#"---
id: mini
lanes:
  - id: backlog
    name: Backlog
    maps_to: backlog
---
"#;
        let p = parse_pipeline(Path::new("/ws/mini.md"), md);
        let idea = propose_idea("Add dark mode toggle", "body", &uid('S'), "alpha", &[], "mini").unwrap();
        assert_eq!(compose_idea_ticket(&idea, &p).unwrap_err(), IdeaRefusal::NoIdeasLane);
    }

    // -----------------------------------------------------------------
    // 1.14 Artifact manifest model
    // -----------------------------------------------------------------

    #[test]
    fn artifact_paths_stay_outside_any_member_repo() {
        let root = Path::new("/ws");
        assert_eq!(
            artifacts_dir(root),
            Path::new("/ws").join(crate::workspace::CONFIG_DIR).join("artifacts")
        );
        let id = uid('T');
        assert_eq!(artifact_ticket_dir(root, &id), artifacts_dir(root).join(&id));
        assert_eq!(
            artifact_manifest_path(root, &id),
            artifacts_dir(root).join(&id).join("manifest.md")
        );
    }

    #[test]
    fn shift_iso_date_adds_days_across_month_and_year_boundaries() {
        assert_eq!(shift_iso_date("2026-08-03", 30).as_deref(), Some("2026-09-02"));
        assert_eq!(shift_iso_date("2025-12-15", 30).as_deref(), Some("2026-01-14"));
        assert_eq!(shift_iso_date("2026-08-03", -1).as_deref(), Some("2026-08-02"));
        assert_eq!(shift_iso_date("not-a-date", 30), None);
    }

    #[test]
    fn new_artifact_manifest_defaults_expires_to_thirty_days_out() {
        let m = new_artifact_manifest(&uid('T'), "2026-08-03", vec!["walkthrough.md".to_string()]);
        assert_eq!(m.expires, "2026-09-02");
        assert!(!m.durable());
    }

    #[test]
    fn artifact_expiry_is_a_pure_predicate() {
        let m = ArtifactManifest {
            ticket: uid('T'),
            created: "2026-08-01".to_string(),
            expires: "2026-08-31".to_string(),
            files: vec![],
        };
        assert!(!is_artifact_expired(&m, "2026-08-31"), "exactly on expires: not yet");
        assert!(is_artifact_expired(&m, "2026-09-01"), "the day after: expired");
        assert!(!is_artifact_expired(&m, "2026-08-15"), "well before: not expired");
        assert!(!is_artifact_expired(&m, "not-a-date"), "malformed today: never guess");
    }

    #[test]
    fn artifact_manifest_round_trips_and_durable_cannot_be_forged() {
        let m = new_artifact_manifest(&uid('T'), "2026-08-03", vec!["demo.mp4".to_string()]);
        let text = compose_artifact_manifest(&m);
        // `scalar_lines` quotes YAML-ambiguous scalars (D1.5's precedent:
        // `bounces: '2'`), so the literal `false` renders quoted.
        assert!(text.starts_with("---\ndurable: 'false'\n"), "{text}");
        let parsed = parse_artifact_manifest(Path::new("/ws/.ken-workspace/artifacts/x/manifest.md"), &text);
        assert_eq!(parsed, m);

        // A hand edit claiming `durable: true` still reports `false` —
        // there is no field to hold anything else (D9).
        let forged =
            "---\ndurable: true\nticket: T1\ncreated: 2026-08-03\nexpires: 2026-09-02\nfiles: []\n---\n\nbody\n";
        let parsed_forged =
            parse_artifact_manifest(Path::new("/ws/.ken-workspace/artifacts/x/manifest.md"), forged);
        assert!(!parsed_forged.durable());
    }

    // -----------------------------------------------------------------
    // 1.15 Digest composition
    // -----------------------------------------------------------------

    #[test]
    fn root_blockers_walks_a_three_deep_chain_to_the_leaf() {
        let mut g = BlockGraph::new();
        g.insert("A", &["B".to_string()]);
        g.insert("B", &["C".to_string()]);
        g.insert("C", &[]); // leaf: not itself blocked
        assert_eq!(root_blockers(&g, "A"), vec!["C".to_string()]);
        assert_ne!(root_blockers(&g, "A"), vec!["B".to_string()], "must not report the nearest blocker");
    }

    #[test]
    fn root_blockers_deduplicates_a_diamond() {
        let mut g = BlockGraph::new();
        g.insert("A", &["B".to_string(), "C".to_string()]);
        g.insert("B", &["D".to_string()]);
        g.insert("C", &["D".to_string()]);
        g.insert("D", &[]);
        assert_eq!(root_blockers(&g, "A"), vec!["D".to_string()]);
    }

    #[test]
    fn root_blockers_terminates_on_an_already_cyclic_graph() {
        let mut g = BlockGraph::new();
        g.insert("A", &["B".to_string()]);
        g.insert("B", &["A".to_string()]);
        // No leaf exists; the walk must still terminate rather than loop.
        assert!(root_blockers(&g, "A").is_empty());
    }

    #[test]
    fn digest_groups_in_the_specified_order_with_root_blocker_shown() {
        let p = pipe();
        let today = "2026-08-03";

        // awaiting_review: sitting in signoff.
        let reviewing = ticket(&uid('R'), "signoff", "updated: 2026-08-02\n");

        // newly_unblocked: block evidence cleared, return_lane survives.
        let unblocked = ticket(&uid('U'), "programmer", "return_lane: programmer\n");

        // blocked: a 3-deep chain — the leaf is the root blocker, not
        // `mid`'s immediate blocker.
        let leaf = ticket(&uid('L'), "documentation", "");
        let mid = ticket(
            &uid('M'),
            "blocked",
            &format!(
                "return_lane: programmer\nblocked_by:\n  - {}\nblocked_at: '2026-08-02'\n",
                uid('L')
            ),
        );
        let deep = ticket(
            &uid('D'),
            "blocked",
            &format!(
                "return_lane: tester\nblocked_by:\n  - {}\nblocked_at: '2026-08-01'\n",
                uid('M')
            ),
        );

        // moved_today, and the source of a generated idea.
        let moved = ticket(&uid('X'), "tester", "updated: 2026-08-03\n");

        // new_ideas: generated today, citing `moved`.
        let idea = ticket(
            &uid('I'),
            "ideas",
            &format!("origin: generated\nspawned_by: {}\ncreated: 2026-08-03\n", uid('X')),
        );

        let board = vec![
            reviewing.clone(),
            unblocked.clone(),
            mid.clone(),
            deep.clone(),
            leaf,
            moved.clone(),
            idea.clone(),
        ];

        let stale = run(&uid('S'), &uid('X'), RunOutcome::Running);
        let runs = vec![stale.clone(), run(&uid('N'), &uid('X'), RunOutcome::Pass)];

        let digest = compose_digest(&board, &[p], &runs, &BTreeSet::new(), today);

        assert_eq!(digest.awaiting_review.len(), 1);
        assert_eq!(digest.awaiting_review[0].ticket_id, reviewing.id);
        assert_eq!(digest.awaiting_review[0].run_count, 0);

        assert_eq!(digest.newly_unblocked.len(), 1);
        assert_eq!(digest.newly_unblocked[0].ticket_id, unblocked.id);
        assert_eq!(digest.newly_unblocked[0].return_lane, "programmer");

        assert_eq!(digest.blocked.len(), 2);
        // oldest first by blocked_at
        assert_eq!(digest.blocked[0].ticket_id, deep.id);
        assert_eq!(digest.blocked[1].ticket_id, mid.id);
        // the deep chain shows the ROOT blocker, not the nearest (`mid`'s
        // own blocker).
        assert_eq!(digest.blocked[0].root_blockers, vec![uid('L')]);
        assert_eq!(digest.blocked[1].root_blockers, vec![uid('L')]);

        assert_eq!(digest.moved_today.len(), 1);
        assert_eq!(digest.moved_today[0].ticket_id, moved.id);
        assert_eq!(digest.moved_today[0].run_count, 2);

        assert_eq!(digest.new_ideas.len(), 1);
        assert_eq!(digest.new_ideas[0].ticket_id, idea.id);
        assert_eq!(digest.new_ideas[0].spawned_by.as_deref(), Some(moved.id.as_str()));

        assert_eq!(digest.stale_runs.len(), 1);
        assert_eq!(digest.stale_runs[0].id, stale.id);
    }

    #[test]
    fn render_digest_markdown_orders_sections_and_handles_empty() {
        let d = Digest::default();
        assert_eq!(
            render_digest_markdown(&d, "2026-08-03"),
            "# Pipeline digest — 2026-08-03\n\nNothing to report.\n"
        );

        let mut d = Digest::default();
        d.awaiting_review.push(AwaitingReviewEntry {
            ticket_id: "A".into(),
            title: "Review me".into(),
            updated: "2026-08-02".into(),
            run_count: 1,
        });
        d.newly_unblocked.push(UnblockedDigestEntry {
            ticket_id: "B".into(),
            title: "Freed".into(),
            return_lane: "programmer".into(),
        });
        d.blocked.push(BlockedDigestEntry {
            ticket_id: "C".into(),
            title: "Stuck".into(),
            blocked_at: Some("2026-08-01".into()),
            root_blockers: vec!["D".into()],
            block_reason: None,
            run_count: 0,
        });
        d.moved_today.push(MovedEntry {
            ticket_id: "E".into(),
            title: "Moved".into(),
            lane: "tester".into(),
            run_count: 2,
        });
        d.new_ideas.push(IdeaEntry {
            ticket_id: "F".into(),
            title: "Idea".into(),
            spawned_by: Some("E".into()),
        });
        d.stale_runs.push(run(&uid('S'), "E", RunOutcome::Running));

        let md = render_digest_markdown(&d, "2026-08-03");
        let idx = |needle: &str| md.find(needle).unwrap_or_else(|| panic!("missing '{needle}' in {md}"));
        let review = idx("## Awaiting your review");
        let unblocked_i = idx("## Unblocked overnight");
        let blocked_i = idx("## Blocked");
        let moved_i = idx("## Moved today");
        let ideas_i = idx("## New ideas");
        let stale_i = idx("## Stale runs");
        assert!(review < unblocked_i);
        assert!(unblocked_i < blocked_i);
        assert!(blocked_i < moved_i);
        assert!(moved_i < ideas_i);
        assert!(ideas_i < stale_i);
        assert!(md.contains("root blocker(s): D"));
    }

    // -----------------------------------------------------------------
    // 1.19 Default pipeline scaffold
    // -----------------------------------------------------------------

    #[test]
    fn scaffold_is_written_only_when_the_file_is_absent() {
        assert_eq!(scaffold_default_pipeline(true), None);
        assert_eq!(scaffold_default_pipeline(false), Some(DEFAULT_PIPELINE_MD));
    }

    #[test]
    fn default_pipeline_scaffold_reproduces_d1s_twelve_lanes() {
        let p = parse_pipeline(
            Path::new("/ws/.ken-workspace/pipelines/default.md"),
            DEFAULT_PIPELINE_MD,
        );
        let ids: Vec<&str> = p.lanes.iter().map(|l| l.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "ideas",
                "backlog",
                "todo",
                "investigation",
                "refinement",
                "programmer",
                "tester",
                "architect",
                "qa",
                "signoff",
                "documentation",
                "blocked",
            ]
        );
        assert!(p.blocked_lane().is_some());
        assert!(p.human_lane().is_some());
        assert!(validate_pipeline(&p).is_empty(), "{:?}", validate_pipeline(&p));
        assert!(!p.auto, "ships auto:false (D6)");
    }
}

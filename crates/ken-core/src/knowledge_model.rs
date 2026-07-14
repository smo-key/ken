//! Knowledge model extraction: compose the corpus-wide entity/event
//! prompt, parse the model's JSON answer tolerantly, and store the
//! result in the local DB. The Map and Timeline screens are pure read
//! models over what this module writes — no project files involved.

use std::path::Path;
use std::time::{Duration, Instant};

use crate::assistant::{self, OneshotOutcome};
use crate::db::{Db, EntityInput, EventInput};
use crate::engine;
use crate::project::Project;
use crate::runner::CancelToken;
use crate::{Error, Result};

/// How many indexed paths the prompt lists at most — enough to orient
/// the agent, which reads the files itself. The list is orientation
/// only, so its token cost (~10 tokens/path → a few thousand tokens at
/// this cap) buys broader project visibility inside the extraction
/// budget without meaningfully growing the prompt.
const MAX_PROMPT_FILES: usize = 400;

/// Extraction caps: high enough that a real project yields a rich Map
/// and Timeline. The Map declutters labels client-side, so a dense
/// graph stays legible; the answer is still small by construction.
const MAX_ENTITIES: usize = 200;
const MAX_EVENTS: usize = 150;

/// Per-file sanity caps for incremental extraction — bound the junk one bad
/// generation can inject. Deliberately NOT a global cap; the whole model grows
/// with the corpus.
pub const FILE_MAX_ENTITIES: usize = 40;
pub const FILE_MAX_RELATIONS: usize = 60;
pub const FILE_MAX_EVENTS: usize = 20;

/// The file's extracted text is truncated to this many characters before
/// prompting — ~6k tokens at 4 chars/token, comfortably inside the local
/// model's context alongside the instructions.
pub const EXTRACT_CHAR_BUDGET: usize = 24_000;

/// Corpus-wide reading is slower than a digest.
pub const EXTRACTION_TIMEOUT: Duration = Duration::from_secs(600);

const ENTITY_KINDS: [&str; 5] =
    ["person", "organization", "topic", "decision", "other"];

/// A parsed extraction, ready for `Db::replace_knowledge_model`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Extraction {
    pub entities: Vec<EntityInput>,
    pub events: Vec<EventInput>,
}

/// What a successful build stored.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelCounts {
    pub entities: usize,
    pub edges: usize,
    pub events: usize,
}

/// The extraction prompt: read the material, answer with ONE JSON
/// object of entities (with connections) and dated events.
pub fn compose_extraction_prompt(files: &[String], today: &str) -> String {
    let mut p = format!(
        "You are Ken, building the knowledge model for this project — the \
entities (people, organizations, topics, decisions) and dated events \
that the Map and Timeline views draw.\n\n\
Read the project's source material listed below (open any of these \
files as needed; never modify anything). Today's date is {today}.\n\n\
Output ONLY a JSON object — no prose before or after, no code fences — \
shaped exactly like this:\n\
{{\n\
  \"entities\": [\n\
    {{\n\
      \"kind\": \"person|organization|topic|decision|other\",\n\
      \"name\": \"short display name\",\n\
      \"summary\": \"one plain sentence about it\",\n\
      \"sources\": [\"relative/path.md\"],\n\
      \"connections\": [{{\"to\": \"another entity's name\", \"label\": \"short relation\"}}]\n\
    }}\n\
  ],\n\
  \"events\": [\n\
    {{\n\
      \"date\": \"yyyy-mm-dd\",\n\
      \"category\": \"one lowercase word, e.g. decision, people, vendor\",\n\
      \"text\": \"one plain sentence\",\n\
      \"source\": \"relative/path.md\"\n\
    }}\n\
  ]\n\
}}\n\n\
Rules:\n\
- At most {MAX_ENTITIES} entities and {MAX_EVENTS} events — pick the ones that matter.\n\
- Only include entities and events actually grounded in the material; never invent.\n\
- Dates are best effort — use document dates from file names or content; \
omit events with no inferable date entirely.\n\
- Every connections.to must exactly name another entity in your list.\n\
- sources and source are project-relative paths from the list below.\n\n\
Indexed source files:\n"
    );
    if files.is_empty() {
        p.push_str("- none\n");
    }
    for path in files.iter().take(MAX_PROMPT_FILES) {
        p.push_str(&format!("- {path}\n"));
    }
    if files.len() > MAX_PROMPT_FILES {
        p.push_str(&format!("- …and {} more\n", files.len() - MAX_PROMPT_FILES));
    }
    p
}

/// A deterministic 64-bit FNV-1a of the extracted text, hex-encoded. Stored in
/// `extractions.content_hash` to detect when a re-index actually changed a
/// file's content (mtime/size churn without content change is common on sync
/// clients, and must not re-run extraction).
pub fn content_hash(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    format!("{h:016x}")
}

/// The per-file extraction prompt: read ONE file's already-extracted text and
/// answer with a single strict-JSON delta. Shapes match `parse_delta_value`:
/// entities carry no sources (the merge attributes them to this file), and
/// relations name entities by their display name.
pub fn compose_file_prompt(rel_path: &str, text: &str, today: &str) -> String {
    // Budget the whole prompt, not just the body: the instructions must fit
    // "alongside" the document inside EXTRACT_CHAR_BUDGET. Build the fixed
    // preamble first, then let the body fill whatever characters remain.
    let head = format!(
        "You are Ken, extracting the knowledge in ONE document for a project's \
Map and Timeline. Today's date is {today}.\n\n\
Read the document below (path: {rel_path}) and output ONLY a JSON object — no \
prose before or after, no code fences — shaped exactly like this:\n\
{{\n\
  \"entities\": [\n\
    {{\"kind\": \"person|organization|topic|decision|other\", \
\"name\": \"short display name\", \"summary\": \"one plain sentence\"}}\n\
  ],\n\
  \"relations\": [\n\
    {{\"a\": \"one entity's name\", \"b\": \"another entity's name\", \
\"label\": \"short relation\"}}\n\
  ],\n\
  \"events\": [\n\
    {{\"date\": \"yyyy-mm-dd\", \"category\": \"one lowercase word\", \
\"text\": \"one plain sentence\"}}\n\
  ]\n\
}}\n\n\
Rules:\n\
- At most {FILE_MAX_ENTITIES} entities, {FILE_MAX_RELATIONS} relations, and \
{FILE_MAX_EVENTS} events — only what THIS document grounds; never invent.\n\
- relations.a and relations.b must each name an entity in your list.\n\
- Dates are best effort — omit events with no inferable yyyy-mm-dd date.\n\n\
Document:\n"
    );
    // Reserve the preamble (and the trailing newline) so the full prompt stays
    // within budget.
    let body_budget = EXTRACT_CHAR_BUDGET.saturating_sub(head.chars().count() + 1);
    let body: String = if text.chars().count() > body_budget {
        text.chars().take(body_budget).collect()
    } else {
        text.to_string()
    };
    format!("{head}{body}\n")
}

/// Parse an extraction answer tolerantly: find the JSON object (fences
/// and prose stripped), ignore unknown fields, drop malformed records,
/// resolve connections by case-insensitive name, drop dangling and self
/// references, collapse duplicate pairs, enforce the caps. Only an
/// answer with no parseable JSON object is an error.
pub fn parse_extraction(raw: &str) -> Result<Extraction> {
    let no_json =
        || Error::Other("the model's answer contained no JSON object".into());
    let start = raw.find('{').ok_or_else(no_json)?;
    let end = raw.rfind('}').filter(|e| *e > start).ok_or_else(no_json)?;
    let value: serde_json::Value = serde_json::from_str(&raw[start..=end])
        .map_err(|e| Error::Other(format!("the model's answer wasn't valid JSON: {e}")))?;

    // Entities first (names must exist before connections can resolve).
    let mut entities: Vec<EntityInput> = Vec::new();
    let mut raw_connections: Vec<Vec<(String, String)>> = Vec::new();
    for item in value["entities"].as_array().unwrap_or(&Vec::new()) {
        if entities.len() >= MAX_ENTITIES {
            break;
        }
        let Some(name) = non_empty_str(&item["name"]) else {
            continue; // no usable name — drop the record
        };
        let kind = item["kind"]
            .as_str()
            .map(|k| k.trim().to_lowercase())
            .filter(|k| ENTITY_KINDS.contains(&k.as_str()))
            .unwrap_or_else(|| "other".into());
        let sources = string_list(&item["sources"]);
        let conns = item["connections"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|c| {
                        let to = non_empty_str(&c["to"])?;
                        let label = c["label"].as_str().unwrap_or("").trim().to_string();
                        Some((to, label))
                    })
                    .collect()
            })
            .unwrap_or_default();
        entities.push(EntityInput {
            kind,
            name,
            summary: item["summary"].as_str().unwrap_or("").trim().to_string(),
            sources,
            connections: Vec::new(),
        });
        raw_connections.push(conns);
    }

    // Resolve connections: case-insensitive name → index; dangling and
    // self references drop; duplicate pairs (either direction) collapse.
    let mut by_name: std::collections::HashMap<String, usize> = Default::default();
    for (i, e) in entities.iter().enumerate() {
        by_name.entry(e.name.to_lowercase()).or_insert(i);
    }
    let mut seen: std::collections::HashSet<(usize, usize)> = Default::default();
    for (from, conns) in raw_connections.into_iter().enumerate() {
        for (to_name, label) in conns {
            let Some(&to) = by_name.get(&to_name.trim().to_lowercase()) else {
                continue;
            };
            if to == from {
                continue;
            }
            let pair = (from.min(to), from.max(to));
            if !seen.insert(pair) {
                continue;
            }
            entities[from].connections.push((to, label));
        }
    }

    let mut events: Vec<EventInput> = Vec::new();
    for item in value["events"].as_array().unwrap_or(&Vec::new()) {
        if events.len() >= MAX_EVENTS {
            break;
        }
        let Some(text) = non_empty_str(&item["text"]) else {
            continue;
        };
        let Some(date) = item["date"].as_str().map(str::trim).filter(|d| valid_date(d))
        else {
            continue; // no usable date — the timeline can't place it
        };
        let category = item["category"]
            .as_str()
            .and_then(|c| c.to_lowercase().split_whitespace().next().map(String::from))
            .unwrap_or_else(|| "other".into());
        events.push(EventInput {
            date: date.to_string(),
            category,
            text,
            source: item["source"].as_str().unwrap_or("").trim().to_string(),
        });
    }

    Ok(Extraction { entities, events })
}

/// Build (or rebuild) the knowledge model: compose from the DB's
/// indexed files, run one headless session, parse, store. A failed
/// build returns an error and leaves the previous model untouched.
pub fn build_knowledge_model(
    binary: &Path,
    project: &Project,
    db: &mut Db,
    today: &str,
    cancel: &CancelToken,
) -> Result<ModelCounts> {
    let files: Vec<String> = db
        .list_files()?
        .into_iter()
        .filter(|f| f.status == "indexed")
        .map(|f| f.rel_path)
        .collect();
    let prompt = compose_extraction_prompt(&files, today);
    match assistant::oneshot(binary, &project.root, &prompt, EXTRACTION_TIMEOUT, cancel)? {
        OneshotOutcome::Completed(text) => {
            let extraction = parse_extraction(&text)?;
            let edges: usize =
                extraction.entities.iter().map(|e| e.connections.len()).sum();
            db.replace_knowledge_model(
                &extraction.entities,
                &extraction.events,
                engine::now_epoch(),
            )?;
            Ok(ModelCounts {
                entities: extraction.entities.len(),
                edges,
                events: extraction.events.len(),
            })
        }
        OneshotOutcome::Cancelled => {
            Err(Error::Other("the refresh was cancelled".into()))
        }
        OneshotOutcome::TimedOut => Err(Error::Other(
            "mapping the project took too long and was stopped — try again".into(),
        )),
        OneshotOutcome::Failed(detail) => Err(Error::Other(detail)),
    }
}

// ---------- automatic rebuild policy ----------
//
// Every build spends a real Claude Code session over the whole corpus, so
// over-triggering is the expensive failure here, not under-triggering.
// The policy is therefore: one build when a project first opens, and at
// most one automatic rebuild per cooldown afterwards — always behind a
// quiet window so a burst of edits (or a OneDrive sync dumping a folder)
// collapses into a single run.

/// How long the index must be quiet before the FIRST build of a project.
/// Short, because an empty Map/Timeline is what the user came to see —
/// but long enough that the initial scan's own indexing settles first.
pub const FIRST_BUILD_SETTLE: Duration = Duration::from_secs(60);

/// How long the index must be quiet before an automatic REBUILD. Editing
/// a document takes pauses; five minutes is past the pauses inside one
/// work session, and past the tail of a large sync.
pub const CHANGE_QUIET: Duration = Duration::from_secs(5 * 60);

/// Floor between automatic build attempts (successes and failures alike).
/// Bounds automatic spend at two sessions an hour no matter how busy the
/// folder is, and turns a persistently failing CLI into a slow retry
/// instead of a storm. Manual refresh ignores this.
pub const MIN_AUTO_INTERVAL: Duration = Duration::from_secs(30 * 60);

/// A folder that never goes quiet (a sync client writing continuously)
/// would otherwise defer forever, so pending changes build anyway once
/// they're this old — the watcher's `max_hold` guarantee, one level up.
pub const MAX_DEFER: Duration = Duration::from_secs(30 * 60);

/// Everything the policy needs that the tracker can't know by itself.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AutoBuildContext {
    /// The Claude Code CLI was found. Without it a build can only fail,
    /// so automatic builds simply don't happen — no error, no retry.
    pub claude_available: bool,
    /// A build thread is running right now (the `knowledge_running` guard).
    pub in_flight: bool,
    /// Indexed files available to read. Nothing to read → nothing to map.
    pub indexed_files: usize,
    /// This project has no stored knowledge model yet.
    pub never_built: bool,
}

/// The full input to `should_auto_build` — a snapshot, no clocks inside.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AutoBuildState {
    pub claude_available: bool,
    pub in_flight: bool,
    /// The project's scan is still running; the file list isn't final.
    pub scanning: bool,
    pub indexed_files: usize,
    pub never_built: bool,
    /// Source files changed since the last build started.
    pub dirty: bool,
    pub since_last_change: Option<Duration>,
    pub since_first_change: Option<Duration>,
    /// Since the last automatic OR manual build attempt finished.
    pub since_last_attempt: Option<Duration>,
}

/// The whole "should Ken rebuild the knowledge model right now?" decision,
/// as one pure function of observed state. Called on a slow tick; it must
/// stay false for every tick of a burst and true exactly once after it.
pub fn should_auto_build(s: &AutoBuildState) -> bool {
    if !s.claude_available || s.in_flight || s.scanning || s.indexed_files == 0 {
        return false;
    }
    if s.since_last_attempt.is_some_and(|since| since < MIN_AUTO_INTERVAL) {
        return false;
    }
    // Settled = quiet long enough, or pending so long that waiting for
    // quiet has become the bug.
    let settled = |quiet: Duration| match s.since_last_change {
        None => true, // nothing has changed since we started watching
        Some(last) => {
            last >= quiet || s.since_first_change.is_some_and(|first| first >= MAX_DEFER)
        }
    };
    if s.never_built {
        // A project with no model gets one even if nothing changed today.
        return settled(FIRST_BUILD_SETTLE);
    }
    s.dirty && settled(CHANGE_QUIET)
}

/// Change/scan/build bookkeeping for one open project, shared between the
/// watcher callback, the scan thread, and the tick that decides. Holds no
/// thread of its own.
pub struct AutoBuildTracker {
    inner: std::sync::Mutex<TrackerInner>,
}

struct TrackerInner {
    scanning: bool,
    /// Set by source changes, cleared when a build starts reading them —
    /// so a change arriving mid-build survives into the next rebuild.
    dirty: bool,
    first_change: Option<Instant>,
    last_change: Option<Instant>,
    last_attempt: Option<Instant>,
}

impl Default for AutoBuildTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl AutoBuildTracker {
    /// Starts out scanning: activation always kicks off an initial scan,
    /// and no build may run against a half-walked folder.
    pub fn new() -> AutoBuildTracker {
        AutoBuildTracker {
            inner: std::sync::Mutex::new(TrackerInner {
                scanning: true,
                dirty: false,
                first_change: None,
                last_change: None,
                last_attempt: None,
            }),
        }
    }

    pub fn scan_finished(&self) {
        self.inner.lock().unwrap().scanning = false;
    }

    /// Source files changed (a scan reported `changed_paths`). Ken's own
    /// `.ken/` writes never reach here — the scanner and watcher both skip
    /// hidden folders, so a build can't feed itself.
    pub fn changed(&self) {
        self.changed_at(Instant::now());
    }

    pub fn changed_at(&self, at: Instant) {
        let mut i = self.inner.lock().unwrap();
        i.dirty = true;
        i.first_change.get_or_insert(at);
        i.last_change = Some(at);
    }

    /// A build is about to read the index: the pending changes are its
    /// input, so the dirty mark resets and only later changes count.
    pub fn build_started(&self, at: Instant) {
        let mut i = self.inner.lock().unwrap();
        i.dirty = false;
        i.first_change = None;
        i.last_change = None;
        i.last_attempt = Some(at);
    }

    /// Stamped for successes and failures alike — the cooldown is a spend
    /// and retry limit, not a success limit.
    pub fn build_finished(&self, at: Instant) {
        self.inner.lock().unwrap().last_attempt = Some(at);
    }

    pub fn snapshot(&self, ctx: AutoBuildContext, now: Instant) -> AutoBuildState {
        let i = self.inner.lock().unwrap();
        let since = |t: Option<Instant>| t.map(|t| now.saturating_duration_since(t));
        AutoBuildState {
            claude_available: ctx.claude_available,
            in_flight: ctx.in_flight,
            scanning: i.scanning,
            indexed_files: ctx.indexed_files,
            never_built: ctx.never_built,
            dirty: i.dirty,
            since_last_change: since(i.last_change),
            since_first_change: since(i.first_change),
            since_last_attempt: since(i.last_attempt),
        }
    }

    pub fn should_build(&self, ctx: AutoBuildContext, now: Instant) -> bool {
        should_auto_build(&self.snapshot(ctx, now))
    }
}

fn non_empty_str(v: &serde_json::Value) -> Option<String> {
    v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from)
}

fn string_list(v: &serde_json::Value) -> Vec<String> {
    v.as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(non_empty_str)
                .collect()
        })
        .unwrap_or_default()
}

/// Strictly-shaped `yyyy-mm-dd` with sane ranges — best-effort dates
/// that don't fit drop the event, never the batch.
fn valid_date(d: &str) -> bool {
    let b = d.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return false;
    }
    let digits = |r: std::ops::Range<usize>| {
        d[r.clone()].bytes().all(|c| c.is_ascii_digit()).then(|| {
            d[r].parse::<u32>().unwrap_or(0)
        })
    };
    let (Some(_y), Some(m), Some(day)) = (digits(0..4), digits(5..7), digits(8..10))
    else {
        return false;
    };
    (1..=12).contains(&m) && (1..=31).contains(&day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::test_support::write_fake_claude;

    #[test]
    fn content_hash_is_stable_and_sensitive() {
        assert_eq!(content_hash("hello"), content_hash("hello"));
        assert_ne!(content_hash("hello"), content_hash("hello "));
        // 16 lowercase hex digits.
        let h = content_hash("anything");
        assert_eq!(h.len(), 16);
        assert!(h.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn file_prompt_is_single_file_and_strict() {
        let p = compose_file_prompt("notes/kickoff.md", "We hired Priya.", "2026-07-14");
        assert!(p.contains("notes/kickoff.md"));
        assert!(p.contains("We hired Priya."));
        assert!(p.contains("ONLY a JSON object"));
        assert!(p.contains("\"relations\""));
        assert!(p.contains("\"events\""));
        assert!(p.contains("person|organization|topic|decision|other"));
        assert!(p.contains("yyyy-mm-dd"));
        assert!(p.contains("2026-07-14"));
        // Per-file caps are stated so the model self-limits.
        assert!(p.contains("40 entities"));
    }

    #[test]
    fn file_prompt_truncates_to_budget() {
        let big = "x".repeat(EXTRACT_CHAR_BUDGET + 5_000);
        let p = compose_file_prompt("big.md", &big, "2026-07-14");
        // The document body is capped; the surrounding instructions are small.
        assert!(p.matches('x').count() <= EXTRACT_CHAR_BUDGET);
    }

    // ---------- auto-build policy ----------

    /// A settled project with a model and nothing pending — the common
    /// case, in which nothing should ever run.
    fn idle() -> AutoBuildState {
        AutoBuildState {
            claude_available: true,
            in_flight: false,
            scanning: false,
            indexed_files: 12,
            never_built: false,
            dirty: false,
            since_last_change: None,
            since_first_change: None,
            since_last_attempt: None,
        }
    }

    #[test]
    fn settled_project_with_a_model_never_rebuilds_on_its_own() {
        assert!(!should_auto_build(&idle()));
    }

    #[test]
    fn fresh_project_builds_once_the_scan_settles() {
        let mut s = AutoBuildState { never_built: true, ..idle() };
        // The initial scan is still walking the folder.
        s.scanning = true;
        assert!(!should_auto_build(&s));

        // Scan done, but the index is still churning (watcher flushes).
        s.scanning = false;
        s.dirty = true;
        s.since_last_change = Some(Duration::from_secs(10));
        s.since_first_change = Some(Duration::from_secs(40));
        assert!(!should_auto_build(&s));

        // Quiet for the settle window → the first build runs.
        s.since_last_change = Some(FIRST_BUILD_SETTLE);
        assert!(should_auto_build(&s));
    }

    #[test]
    fn fresh_project_with_nothing_to_read_stays_quiet() {
        let s = AutoBuildState { never_built: true, indexed_files: 0, ..idle() };
        assert!(!should_auto_build(&s));
    }

    #[test]
    fn a_burst_of_changes_coalesces_into_one_build() {
        let mut s = AutoBuildState {
            dirty: true,
            since_last_attempt: Some(MIN_AUTO_INTERVAL),
            since_first_change: Some(Duration::from_secs(0)),
            since_last_change: Some(Duration::from_secs(0)),
            ..idle()
        };
        // Every tick during the burst says "not yet" — one decision per
        // tick, never one build per change.
        for elapsed in [0, 30, 60, 120, 240] {
            s.since_last_change = Some(Duration::from_secs(elapsed));
            s.since_first_change = Some(Duration::from_secs(elapsed));
            assert!(!should_auto_build(&s), "should still be waiting at {elapsed}s");
        }
        s.since_last_change = Some(CHANGE_QUIET);
        s.since_first_change = Some(CHANGE_QUIET);
        assert!(should_auto_build(&s));
    }

    #[test]
    fn an_endless_stream_of_changes_still_gets_a_build() {
        // A sync client writing continuously never gives us a quiet
        // window; the max-defer cap builds anyway (same guarantee the
        // watcher's max_hold gives the scanner).
        let s = AutoBuildState {
            dirty: true,
            since_last_change: Some(Duration::from_secs(1)),
            since_first_change: Some(MAX_DEFER),
            since_last_attempt: Some(MIN_AUTO_INTERVAL),
            ..idle()
        };
        assert!(should_auto_build(&s));
    }

    #[test]
    fn a_build_in_flight_is_never_double_started() {
        let s = AutoBuildState {
            in_flight: true,
            never_built: true,
            dirty: true,
            since_last_change: Some(Duration::from_secs(3600)),
            since_first_change: Some(Duration::from_secs(3600)),
            ..idle()
        };
        assert!(!should_auto_build(&s));
    }

    #[test]
    fn rebuilds_wait_out_the_cooldown() {
        let mut s = AutoBuildState {
            dirty: true,
            since_last_change: Some(CHANGE_QUIET),
            since_first_change: Some(CHANGE_QUIET),
            since_last_attempt: Some(MIN_AUTO_INTERVAL - Duration::from_secs(1)),
            ..idle()
        };
        assert!(!should_auto_build(&s), "too soon after the last attempt");
        s.since_last_attempt = Some(MIN_AUTO_INTERVAL);
        assert!(should_auto_build(&s));
    }

    #[test]
    fn a_failed_attempt_does_not_retry_storm() {
        // Failures stamp the same attempt clock as successes, so a broken
        // CLI login retries at most once per cooldown — and never at all
        // when the CLI is missing.
        let s = AutoBuildState {
            never_built: true,
            since_last_attempt: Some(Duration::from_secs(5)),
            ..idle()
        };
        assert!(!should_auto_build(&s));

        let missing = AutoBuildState {
            claude_available: false,
            never_built: true,
            ..idle()
        };
        assert!(!should_auto_build(&missing));
    }

    #[test]
    fn tracker_coalesces_changes_and_reports_elapsed_time() {
        let t = AutoBuildTracker::new();
        let start = Instant::now();
        let ctx = |never_built| AutoBuildContext {
            claude_available: true,
            in_flight: false,
            indexed_files: 5,
            never_built,
        };

        // The initial scan holds everything off until it finishes.
        assert!(!t.should_build(ctx(true), start));
        t.scan_finished();
        assert!(t.should_build(ctx(true), start), "first build once the scan settles");

        // A burst of watcher flushes marks one pending rebuild, not many:
        // the quiet window slides with the last change, the max-defer cap
        // measures from the first.
        t.changed_at(start);
        t.changed_at(start + Duration::from_secs(3));
        t.changed_at(start + Duration::from_secs(10));
        let s = t.snapshot(ctx(false), start + Duration::from_secs(20));
        assert!(s.dirty);
        assert_eq!(s.since_last_change, Some(Duration::from_secs(10)));
        assert_eq!(s.since_first_change, Some(Duration::from_secs(20)));

        // A build takes the pending work; a change landing mid-build marks
        // the model dirty again so exactly one more rebuild follows.
        t.build_started(start + Duration::from_secs(400));
        assert!(!t.snapshot(ctx(false), start + Duration::from_secs(401)).dirty);
        t.changed_at(start + Duration::from_secs(450));
        t.build_finished(start + Duration::from_secs(500));
        let s = t.snapshot(ctx(false), start + Duration::from_secs(501));
        assert!(s.dirty, "the mid-build change still needs a rebuild");
        assert_eq!(s.since_last_attempt, Some(Duration::from_secs(1)));
    }

    #[test]
    fn compose_carries_the_contract() {
        let files = vec!["notes/meeting-jul-8.md".to_string(), "knowledge/People.md".to_string()];
        let prompt = compose_extraction_prompt(&files, "2026-07-12");
        assert!(prompt.contains("ONLY a JSON object"));
        assert!(prompt.contains("\"entities\""));
        assert!(prompt.contains("\"connections\""));
        assert!(prompt.contains("\"events\""));
        assert!(prompt.contains("person|organization|topic|decision|other"));
        assert!(prompt.contains("yyyy-mm-dd"));
        assert!(prompt.contains("At most 200 entities and 150 events"));
        assert!(prompt.contains("never invent"));
        assert!(prompt.contains("omit events with no inferable date"));
        assert!(prompt.contains("2026-07-12"));
        assert!(prompt.contains("- notes/meeting-jul-8.md"));
        assert!(prompt.contains("- knowledge/People.md"));
    }

    #[test]
    fn compose_caps_the_file_list() {
        let files: Vec<String> = (0..430).map(|i| format!("notes/f{i}.md")).collect();
        let prompt = compose_extraction_prompt(&files, "2026-07-12");
        assert!(prompt.contains("- notes/f399.md"));
        assert!(!prompt.contains("- notes/f400.md"));
        assert!(prompt.contains("…and 30 more"));
        // An empty index is named honestly.
        assert!(compose_extraction_prompt(&[], "2026-07-12").contains("- none"));
    }

    const GOOD: &str = r#"{
        "entities": [
            {"kind": "topic", "name": "Billing cutover", "summary": "The migration.",
             "sources": ["notes/meeting.md"],
             "connections": [{"to": "priya n.", "label": "owned by"},
                             {"to": "Nobody Known", "label": "dangling"},
                             {"to": "Billing cutover", "label": "self"}]},
            {"kind": "person", "name": "Priya N.", "summary": "Owns it.",
             "sources": ["knowledge/People.md"],
             "connections": [{"to": "BILLING CUTOVER", "label": "duplicate pair"}],
             "confidence": 0.9},
            {"kind": "sorcerer", "name": "LangdonSoft", "summary": "Vendor."},
            {"kind": "person", "summary": "no name — dropped"}
        ],
        "events": [
            {"date": "2026-07-11", "category": "Decision Made", "text": "Sign-off confirmed.", "source": "notes/standup.md"},
            {"date": "sometime in July", "category": "decision", "text": "bad date — dropped"},
            {"date": "2026-13-40", "category": "decision", "text": "impossible date — dropped"},
            {"date": "2026-06-26", "text": "No category defaults."},
            {"date": "2026-06-01", "category": "people", "text": ""}
        ],
        "extra": "ignored"
    }"#;

    #[test]
    fn parse_salvages_and_resolves() {
        let ex = parse_extraction(GOOD).unwrap();
        assert_eq!(ex.entities.len(), 3, "nameless entity dropped");
        assert_eq!(ex.entities[0].name, "Billing cutover");
        assert_eq!(ex.entities[2].kind, "other", "unknown kind coerced");
        // Case-insensitive resolution; dangling + self dropped; the
        // reverse duplicate collapsed into the first edge.
        assert_eq!(ex.entities[0].connections, vec![(1, "owned by".to_string())]);
        assert!(ex.entities[1].connections.is_empty());
        // Events: bad/impossible dates and empty text dropped, category
        // normalized to one lowercase word, missing category defaults.
        assert_eq!(ex.events.len(), 2);
        assert_eq!(ex.events[0].category, "decision");
        assert_eq!(ex.events[1].category, "other");
    }

    #[test]
    fn parse_strips_fences_and_prose() {
        let fenced = format!("Here is the model:\n```json\n{GOOD}\n```\nDone!");
        assert_eq!(parse_extraction(&fenced).unwrap(), parse_extraction(GOOD).unwrap());
    }

    #[test]
    fn parse_enforces_caps() {
        let entities: Vec<String> = (0..210)
            .map(|i| format!(r#"{{"kind":"topic","name":"E{i}","summary":""}}"#))
            .collect();
        let events: Vec<String> = (0..160)
            .map(|i| format!(r#"{{"date":"2026-01-{:02}","category":"x","text":"e{i}"}}"#, i % 28 + 1))
            .collect();
        let raw = format!(
            r#"{{"entities":[{}],"events":[{}]}}"#,
            entities.join(","),
            events.join(",")
        );
        let ex = parse_extraction(&raw).unwrap();
        assert_eq!(ex.entities.len(), 200);
        assert_eq!(ex.events.len(), 150);
    }

    #[test]
    fn parse_without_json_is_an_error() {
        assert!(parse_extraction("I couldn't find anything.").is_err());
        assert!(parse_extraction("").is_err());
        assert!(parse_extraction("{not json}").is_err());
        // An empty-but-valid object parses to an empty model.
        let ex = parse_extraction("{}").unwrap();
        assert!(ex.entities.is_empty() && ex.events.is_empty());
    }

    #[test]
    fn build_stores_the_model_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let bin = write_fake_claude(dir.path(), "complete");
        std::fs::write(dir.path().join("oneshot_result"), GOOD).unwrap();
        let project =
            crate::project::Project::create(dir.path(), "Atlas").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        db.upsert_file("notes/meeting.md", "md", 10, 1, "indexed", None, "text")
            .unwrap();

        let counts =
            build_knowledge_model(&bin, &project, &mut db, "2026-07-12", &CancelToken::new())
                .unwrap();
        assert_eq!(counts, ModelCounts { entities: 3, edges: 1, events: 2 });
        let (entities, edges) = db.list_entities_with_edges().unwrap();
        assert_eq!(entities.len(), 3);
        assert_eq!(edges.len(), 1);
        assert_eq!(db.list_events().unwrap().len(), 2);
        assert!(db.knowledge_model_built_at().unwrap().is_some());
    }

    #[test]
    fn failed_build_keeps_the_old_model() {
        let dir = tempfile::tempdir().unwrap();
        let project =
            crate::project::Project::create(dir.path(), "Atlas").unwrap();
        let mut db = Db::open_in_memory().unwrap();

        // Seed a model, then fail a rebuild two ways: process failure
        // and a JSON-free answer.
        let bin = write_fake_claude(dir.path(), "complete");
        std::fs::write(dir.path().join("oneshot_result"), GOOD).unwrap();
        build_knowledge_model(&bin, &project, &mut db, "2026-07-12", &CancelToken::new())
            .unwrap();

        let bin = write_fake_claude(dir.path(), "headless-fail");
        assert!(build_knowledge_model(&bin, &project, &mut db, "2026-07-12", &CancelToken::new())
            .is_err());
        let bin = write_fake_claude(dir.path(), "complete");
        std::fs::write(dir.path().join("oneshot_result"), "no json here").unwrap();
        assert!(build_knowledge_model(&bin, &project, &mut db, "2026-07-12", &CancelToken::new())
            .is_err());

        let (entities, _) = db.list_entities_with_edges().unwrap();
        assert_eq!(entities.len(), 3, "old model untouched");
        assert_eq!(db.list_events().unwrap().len(), 2);
    }
}

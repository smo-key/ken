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
/// prompting — ~3k tokens at 4 chars/token, comfortably inside the local
/// model's context alongside the instructions. Halved from a former 24k: the
/// per-file delta is small and bounded (≤ 40 entities / 60 relations / 20
/// events), so the first ~3k tokens of a document ground it well while roughly
/// halving prefill — the dominant per-file cost when indexing a large project.
pub const EXTRACT_CHAR_BUDGET: usize = 12_000;

/// Corpus-wide reading is slower than a digest.
pub const EXTRACTION_TIMEOUT: Duration = Duration::from_secs(600);

/// The closed list of entity kinds, in the order the prompts name them; any
/// other kind a model answers is stored as `other`.
///
/// - `person`: a human being. Never a bot or an AI agent ([`is_bot_or_agent`]).
/// - `organization`: a company, studio, team or group.
/// - `repo`: a repository, service or app the workspace knows by name. Ken
///   calls these repos. Added 2026-10-06: the Shattered Realms dry run filed
///   all five of its repos (and the wiki and docs vault) under `other`,
///   because no kind fit them.
/// - `topic`: a feature, system, idea or area of work.
/// - `decision`: a ruling or choice that was made.
/// - `other`: what fits none of the above.
pub const ENTITY_KINDS: [&str; 6] =
    ["person", "organization", "repo", "topic", "decision", "other"];

/// The kind list as the prompts spell it: `person|organization|...`.
fn kind_choices() -> String {
    ENTITY_KINDS.join("|")
}

/// A model's kind, lowercased and checked against [`ENTITY_KINDS`]; anything
/// unknown is `other`, and "repository" is the same kind as `repo`.
fn coerce_kind(raw: &serde_json::Value) -> String {
    let kind = raw.as_str().map(|k| k.trim().to_lowercase()).unwrap_or_default();
    let kind = if kind == "repository" { "repo".to_string() } else { kind };
    if ENTITY_KINDS.contains(&kind.as_str()) {
        kind
    } else {
        "other".into()
    }
}

/// Bots and AI agents that commit to repos, by name or commit address
/// (compared after `federation::normalize_name`, or as a raw address).
const BOTS_AND_AGENTS: &[&str] = &[
    "claude",
    "claude code",
    "cursor",
    "cursor agent",
    "copilot",
    "github copilot",
    "codex",
    "devin",
    "dependabot",
    "renovate",
    "github actions",
    "noreply@anthropic.com",
    "cursoragent@cursor.com",
];

/// A person entity is dropped as a bot or AI agent when its name is a known
/// bot or agent, holds a `[bot]` account or a known agent address, or ends in
/// the word "bot". Measured 2026-10-06: the Shattered Realms dry run made
/// people of Claude, Cursor Agent, dependabot and the Hytale Sync Bot (4 of
/// 14 people). A human who is really called Claude would be dropped too; no
/// teammate in any measured workspace is.
pub fn is_bot_or_agent(name: &str) -> bool {
    let raw = name.trim().to_lowercase();
    let norm = crate::federation::normalize_name(name);
    raw.contains("[bot]")
        || norm.split(' ').next_back() == Some("bot")
        || BOTS_AND_AGENTS
            .iter()
            .any(|b| if b.contains('@') { raw.contains(b) } else { norm == *b })
}

/// An edge label is git activity, not knowledge, when one of its words is
/// commit, commits, committed, committer(s) or committing. Measured
/// 2026-10-06: 15 of the dry run's 96 map edges were "commits to", "one
/// commit", "top committer", "major committer" or "commits from account".
pub fn is_activity_label(label: &str) -> bool {
    const ACTIVITY: [&str; 6] =
        ["commit", "commits", "committed", "committer", "committers", "committing"];
    label
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| ACTIVITY.contains(&w))
}

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
    let kinds = kind_choices();
    let mut p = format!(
        "You are Ken, building the knowledge model for this project — the \
entities (people, organizations, repos, topics, decisions) and dated events \
that the Map and Timeline views draw.\n\n\
Read the project's source material listed below (open any of these \
files as needed; never modify anything). Today's date is {today}.\n\n\
Output ONLY a JSON object — no prose before or after, no code fences — \
shaped exactly like this:\n\
{{\n\
  \"entities\": [\n\
    {{\n\
      \"kind\": \"{kinds}\",\n\
      \"name\": \"short display name\",\n\
      \"aliases\": [\"another name the material gives this same person\"],\n\
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
- sources and source are project-relative paths from the list below.\n\
- Kinds are exactly these: person is a human being; organization a company, \
studio or group; repo a repository, service or app the material names (a code \
repo, a wiki, a docs vault, a tool the team ships); topic a feature, system or \
idea; decision a ruling that was made; other only when none of these fits.\n\
- People are people. Bots and AI agents (Claude, Cursor Agent, Copilot, \
dependabot, any [bot] account or sync bot) are tools, not people: leave them out.\n\
- One person is one entity. When the material ties two names to one person \
(the same commit email, or a people/ page or CODEOWNERS that lists both), make \
one entity and put the other names in aliases. Never join two names because \
they share a first name.\n\
- A connection says what one thing does to another: owns, decides, depends on, \
replaces, implements, documents, part of. Never commit counts, commit activity \
or rankings (\"commits to\", \"top committer\").\n\n\
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
    compose_file_prompt_with_addendum(rel_path, text, today, "")
}

/// Same as [`compose_file_prompt`], with an optional project-profiler
/// addendum (design D3: `profiler::profile_prompt_addendum` — summary +
/// focus hints, already capped at 500 chars) inserted into the fixed
/// preamble so it counts against `EXTRACT_CHAR_BUDGET` like everything else
/// in the prompt. An empty/blank `addendum` reproduces `compose_file_prompt`
/// exactly byte-for-byte — this is how the profiler flag being off (or no
/// profile existing) stays inert here: the caller simply never passes a
/// non-empty addendum in that case, so nothing about this function's output
/// changes.
pub fn compose_file_prompt_with_addendum(
    rel_path: &str,
    text: &str,
    today: &str,
    addendum: &str,
) -> String {
    let addendum_block = if addendum.trim().is_empty() {
        String::new()
    } else {
        format!("\n{}\n", addendum.trim())
    };
    // Budget the whole prompt, not just the body: the instructions must fit
    // "alongside" the document inside EXTRACT_CHAR_BUDGET. Build the fixed
    // preamble first, then let the body fill whatever characters remain.
    let kinds = kind_choices();
    let head = format!(
        "You are Ken, extracting the knowledge in ONE document for a project's \
Map and Timeline. Today's date is {today}.\n\
{addendum_block}\n\
Read the document below (path: {rel_path}) and output ONLY a JSON object — no \
prose before or after, no code fences — shaped exactly like this:\n\
{{\n\
  \"entities\": [\n\
    {{\"kind\": \"{kinds}\", \
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
- Dates are best effort — omit events with no inferable yyyy-mm-dd date.\n\
- repo is a repository, service or app; bots and AI agents are never people; \
relations say what one thing does to another, never commit counts.\n\n\
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
    parse_extraction_for(raw, &[])
}

/// Whether a person entity called `name` is dropped as a bot or an agent:
/// never when the team's `roster` lists the name, an alias or an address
/// ([`crate::people::by_name`]). The team says who is a person; a teammate
/// whose handle ends in "bot" or reads like an agent's stays.
fn dropped_as_bot(name: &str, roster: &[crate::people::Person]) -> bool {
    crate::people::by_name(roster, name).is_none() && is_bot_or_agent(name)
}

/// [`parse_extraction`], with the team's `roster` checked before a person
/// is dropped as a bot ([`dropped_as_bot`]).
pub fn parse_extraction_for(raw: &str, roster: &[crate::people::Person]) -> Result<Extraction> {
    let no_json =
        || Error::Other("the model's answer contained no JSON object".into());
    let start = raw.find('{').ok_or_else(no_json)?;
    let end = raw.rfind('}').filter(|e| *e > start).ok_or_else(no_json)?;
    let value: serde_json::Value = serde_json::from_str(&raw[start..=end])
        .map_err(|e| Error::Other(format!("the model's answer wasn't valid JSON: {e}")))?;

    // Entities first (names must exist before connections can resolve).
    let mut entities: Vec<EntityInput> = Vec::new();
    let mut raw_connections: Vec<Vec<(String, String)>> = Vec::new();
    let mut aliases: Vec<Vec<String>> = Vec::new();
    for item in value["entities"].as_array().unwrap_or(&Vec::new()) {
        if entities.len() >= MAX_ENTITIES {
            break;
        }
        let Some(name) = non_empty_str(&item["name"]) else {
            continue; // no usable name — drop the record
        };
        let kind = coerce_kind(&item["kind"]);
        if kind == "person" && dropped_as_bot(&name, roster) {
            continue; // a tool, not a teammate; its connections now dangle and drop
        }
        aliases.push(if kind == "person" { person_aliases(&name, &item["aliases"]) } else { Vec::new() });
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

    // One person, one entity: names tied by aliases fold together.
    let (mut entities, raw_connections, by_name) = fold_aliases(entities, raw_connections, &aliases);

    // Resolve connections: case-insensitive name (or alias) → index;
    // dangling and self references drop, as do git-activity labels;
    // duplicate pairs (either direction) collapse.
    let mut seen: std::collections::HashSet<(usize, usize)> = Default::default();
    for (from, conns) in raw_connections.into_iter().enumerate() {
        for (to_name, label) in conns {
            let Some(&to) = by_name.get(&to_name.trim().to_lowercase()) else {
                continue;
            };
            if to == from || is_activity_label(&label) {
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

/// Parse an already-decoded per-file extraction `Value` into a delta. Same
/// hygiene as `parse_extraction` (name-required, kind coercion, dangling/self
/// edge drops, unordered-pair dedup, date validation) but with per-file caps,
/// a top-level `relations` array (a/b name entities), and no sources/source in
/// the JSON — the merge attributes everything to the file being extracted.
/// Infallible: a malformed value yields an empty delta.
pub fn parse_delta_value(value: &serde_json::Value) -> Extraction {
    parse_delta_value_for(value, &[])
}

/// [`parse_delta_value`], with the team's `roster` checked before a person
/// is dropped as a bot ([`dropped_as_bot`]).
pub fn parse_delta_value_for(value: &serde_json::Value, roster: &[crate::people::Person]) -> Extraction {
    let empty = Vec::new();
    let mut entities: Vec<EntityInput> = Vec::new();
    for item in value["entities"].as_array().unwrap_or(&empty) {
        if entities.len() >= FILE_MAX_ENTITIES {
            break;
        }
        let Some(name) = non_empty_str(&item["name"]) else {
            continue;
        };
        let kind = coerce_kind(&item["kind"]);
        if kind == "person" && dropped_as_bot(&name, roster) {
            continue;
        }
        entities.push(EntityInput {
            kind,
            name,
            summary: item["summary"].as_str().unwrap_or("").trim().to_string(),
            sources: Vec::new(),
            connections: Vec::new(),
        });
    }

    // Resolve relations by case-insensitive name; drop dangling/self; collapse
    // duplicate unordered pairs (first label wins); cap total relations.
    let mut by_name: std::collections::HashMap<String, usize> = Default::default();
    for (i, e) in entities.iter().enumerate() {
        by_name.entry(e.name.to_lowercase()).or_insert(i);
    }
    let mut seen: std::collections::HashSet<(usize, usize)> = Default::default();
    let mut relation_count = 0usize;
    for rel in value["relations"].as_array().unwrap_or(&empty) {
        if relation_count >= FILE_MAX_RELATIONS {
            break;
        }
        let (Some(a_name), Some(b_name)) =
            (non_empty_str(&rel["a"]), non_empty_str(&rel["b"]))
        else {
            continue;
        };
        let (Some(&a), Some(&b)) = (
            by_name.get(&a_name.to_lowercase()),
            by_name.get(&b_name.to_lowercase()),
        ) else {
            continue;
        };
        let label = rel["label"].as_str().unwrap_or("").trim().to_string();
        if a == b || is_activity_label(&label) {
            continue;
        }
        let pair = (a.min(b), a.max(b));
        if !seen.insert(pair) {
            continue;
        }
        entities[a].connections.push((b, label));
        relation_count += 1;
    }

    let mut events: Vec<EventInput> = Vec::new();
    for item in value["events"].as_array().unwrap_or(&empty) {
        if events.len() >= FILE_MAX_EVENTS {
            break;
        }
        let Some(text) = non_empty_str(&item["text"]) else {
            continue;
        };
        let Some(date) = item["date"].as_str().map(str::trim).filter(|d| valid_date(d))
        else {
            continue;
        };
        let category = item["category"]
            .as_str()
            .and_then(|c| c.to_lowercase().split_whitespace().next().map(String::from))
            .unwrap_or_else(|| "other".into());
        events.push(EventInput {
            date: date.to_string(),
            category,
            text,
            source: String::new(),
        });
    }

    Extraction { entities, events }
}

/// Extract one file: read its indexed text, prompt the model, parse the delta,
/// and merge it. Marks the extraction row `done` on success (stamping the hash
/// that was queued) or `error` on generation failure — a failed file leaves
/// the model untouched and does NOT return to `pending`, so a persistently
/// failing file can't wedge the queue.
pub fn extract_one<G>(
    db: &mut Db,
    rel_path: &str,
    content_hash: &str,
    today: &str,
    at: i64,
    generate: &G,
) -> Result<()>
where
    G: Fn(&str) -> Result<serde_json::Value>,
{
    extract_one_with_addendum(db, rel_path, content_hash, today, at, generate, "")
}

/// Same as [`extract_one`], with an optional project-profiler prompt
/// addendum (design D3) threaded into [`compose_file_prompt_with_addendum`].
/// `addendum` is normally `profiler::profile_prompt_addendum(&profile)` when
/// the `profiler` flag is on and a profile exists — an empty string (what
/// [`extract_one`] always passes) reproduces the plain extraction path
/// exactly, which is how flag-off/no-profile inertness holds here.
pub fn extract_one_with_addendum<G>(
    db: &mut Db,
    rel_path: &str,
    content_hash: &str,
    today: &str,
    at: i64,
    generate: &G,
    addendum: &str,
) -> Result<()>
where
    G: Fn(&str) -> Result<serde_json::Value>,
{
    extract_one_for(db, rel_path, content_hash, today, at, generate, addendum, &[])
}

/// [`extract_one_with_addendum`], with the team's `roster`: a person it
/// lists is never dropped as a bot ([`dropped_as_bot`]).
#[allow(clippy::too_many_arguments)]
pub fn extract_one_for<G>(
    db: &mut Db,
    rel_path: &str,
    content_hash: &str,
    today: &str,
    at: i64,
    generate: &G,
    addendum: &str,
    roster: &[crate::people::Person],
) -> Result<()>
where
    G: Fn(&str) -> Result<serde_json::Value>,
{
    let text = db.get_text(rel_path)?.unwrap_or_default();
    // An empty (or whitespace-only) file has nothing to extract — mark it done
    // and skip the generation. A blank `.md`, a stub, or a file whose extractor
    // produced no text would otherwise burn a full LLM call to yield an empty
    // delta every time it's (re)enqueued.
    if text.trim().is_empty() {
        db.mark_extraction_done(rel_path, content_hash, at)?;
        return Ok(());
    }
    let prompt = compose_file_prompt_with_addendum(rel_path, &text, today, addendum);
    match generate(&prompt) {
        Ok(value) => {
            let delta = parse_delta_value_for(&value, roster);
            db.merge_knowledge_delta(rel_path, &delta, at)?;
            db.mark_extraction_done(rel_path, content_hash, at)?;
            Ok(())
        }
        Err(e) => {
            db.mark_extraction_error(rel_path, content_hash, &e.to_string(), at)?;
            Err(e)
        }
    }
}

/// Pop the oldest pending file and extract it. `Ok(None)` when the queue is
/// empty. The caller (one background thread per project) loops on this,
/// emitting `knowledge-updated` after each `Ok(Some(_))`.
pub fn process_next_pending<G>(
    db: &mut Db,
    today: &str,
    at: i64,
    generate: &G,
) -> Result<Option<String>>
where
    G: Fn(&str) -> Result<serde_json::Value>,
{
    process_next_pending_with_addendum(db, today, at, generate, "")
}

/// Same as [`process_next_pending`], threading an optional project-profiler
/// addendum (design D3) through to [`extract_one_with_addendum`].
pub fn process_next_pending_with_addendum<G>(
    db: &mut Db,
    today: &str,
    at: i64,
    generate: &G,
    addendum: &str,
) -> Result<Option<String>>
where
    G: Fn(&str) -> Result<serde_json::Value>,
{
    process_next_pending_for(db, today, at, generate, addendum, &[])
}

/// [`process_next_pending_with_addendum`], with the team's `roster`: a
/// person it lists is never dropped as a bot ([`dropped_as_bot`]).
pub fn process_next_pending_for<G>(
    db: &mut Db,
    today: &str,
    at: i64,
    generate: &G,
    addendum: &str,
    roster: &[crate::people::Person],
) -> Result<Option<String>>
where
    G: Fn(&str) -> Result<serde_json::Value>,
{
    let Some((rel_path, content_hash)) = db.next_pending_extraction()? else {
        return Ok(None);
    };
    extract_one_for(db, &rel_path, &content_hash, today, at, generate, addendum, roster)?;
    Ok(Some(rel_path))
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
    // Full tier only: a search-only file (a code or reference repo, a `~`
    // line) never reaches the graph, the deep rebuild included.
    let files = db.entity_tier_paths()?;
    // What was waiting before the build reads: the build covers it.
    let waiting: Vec<(String, String)> = {
        let listed: std::collections::HashSet<&str> = files.iter().map(String::as_str).collect();
        db.waiting_extractions()?.into_iter().filter(|(p, _)| listed.contains(p.as_str())).collect()
    };
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
            // The whole project was just read: the per-file queue it covered
            // is done, so the local model works only on what changes next,
            // not hours of files the map already holds. A file edited while
            // the build ran keeps its new hash and stays queued.
            db.settle_extractions(&waiting, engine::now_epoch())?;
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

/// A person's usable aliases: its other names, minus its own name and minus a
/// lone first name ("Kyle" for "Kyle Ahlstrom" could be any Kyle).
fn person_aliases(name: &str, raw: &serde_json::Value) -> Vec<String> {
    let own = name.trim().to_lowercase();
    let first = own.split_whitespace().next().unwrap_or("").to_string();
    string_list(raw)
        .into_iter()
        .filter(|a| {
            let a = a.to_lowercase();
            a != own && !(own.contains(' ') && a == first)
        })
        .collect()
}

/// One person is one entity: a person whose name another person lists in
/// its aliases is folded into that person (sources, connections, and its
/// name as a lookup). Measured 2026-10-06: the dry run kept "Chris" and
/// "Stingbro" apart though both commit from one GitHub noreply address.
/// Returns the kept entities, their raw connections, and the lowercased
/// name-or-alias → index lookup that connections resolve through.
fn fold_aliases(
    entities: Vec<EntityInput>,
    raw_connections: Vec<Vec<(String, String)>>,
    aliases: &[Vec<String>],
) -> (Vec<EntityInput>, Vec<Vec<(String, String)>>, std::collections::HashMap<String, usize>) {
    let n = entities.len();
    let mut person_by_name: std::collections::HashMap<String, usize> = Default::default();
    for (i, e) in entities.iter().enumerate() {
        if e.kind == "person" {
            person_by_name.entry(e.name.to_lowercase()).or_insert(i);
        }
    }
    // into[j] = the entity j folds into (itself when kept). One level only:
    // a person already folded, or already holding a fold, is not folded again.
    let mut into: Vec<usize> = (0..n).collect();
    for i in 0..n {
        for alias in &aliases[i] {
            let Some(&j) = person_by_name.get(&alias.to_lowercase()) else {
                continue;
            };
            let free = |x: usize, into: &[usize]| into[x] == x && (0..n).all(|k| k == x || into[k] != x);
            if j != i && into[i] == i && free(j, &into) {
                into[j] = i;
            }
        }
    }

    let mut new_index: Vec<usize> = vec![0; n];
    let mut kept: Vec<EntityInput> = Vec::new();
    let mut kept_conns: Vec<Vec<(String, String)>> = Vec::new();
    let mut aka: Vec<Vec<String>> = Vec::new();
    for i in (0..n).filter(|&i| into[i] == i) {
        new_index[i] = kept.len();
        kept.push(entities[i].clone());
        kept_conns.push(raw_connections[i].clone());
        aka.push(aliases[i].clone());
    }
    for i in (0..n).filter(|&i| into[i] != i) {
        let t = new_index[into[i]];
        let e = &entities[i];
        for s in &e.sources {
            if !kept[t].sources.contains(s) {
                kept[t].sources.push(s.clone());
            }
        }
        if kept[t].summary.is_empty() {
            kept[t].summary = e.summary.clone();
        }
        if !aka[t].iter().any(|a| a.to_lowercase() == e.name.to_lowercase()) {
            aka[t].push(e.name.clone());
        }
        kept_conns[t].extend(raw_connections[i].iter().cloned());
    }
    // A merged person says what else it is called, so a reader sees the fold.
    for (e, names) in kept.iter_mut().zip(&aka) {
        let lower = e.summary.to_lowercase();
        let missing: Vec<&str> =
            names.iter().filter(|a| !lower.contains(&a.to_lowercase())).map(String::as_str).collect();
        if e.kind == "person" && !missing.is_empty() {
            let sep = if e.summary.is_empty() { "" } else { " " };
            e.summary = format!("{}{sep}Also known as {}.", e.summary, missing.join(", "));
        }
    }

    let mut by_name: std::collections::HashMap<String, usize> = Default::default();
    for (i, e) in kept.iter().enumerate() {
        by_name.entry(e.name.to_lowercase()).or_insert(i);
    }
    for (i, names) in aka.iter().enumerate() {
        for a in names {
            by_name.entry(a.to_lowercase()).or_insert(i);
        }
    }
    (kept, kept_conns, by_name)
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
    fn worker_drains_queue_merges_and_marks_done() {
        let mut db = Db::open_in_memory().unwrap();
        db.upsert_file("kickoff.md", "md", 1, 1, "indexed", None, "We hired Priya N.").unwrap();
        let hash = content_hash("We hired Priya N.");
        db.enqueue_extraction_if_changed("kickoff.md", &hash).unwrap();

        // Fake local model: returns a canned delta regardless of prompt.
        let generate = |_prompt: &str| -> Result<serde_json::Value> {
            Ok(serde_json::json!({
                "entities": [{"kind": "person", "name": "Priya N.", "summary": "New hire."}],
                "relations": [],
                "events": [{"date": "2026-07-14", "category": "people", "text": "Priya joined."}]
            }))
        };

        let done = process_next_pending(&mut db, "2026-07-14", 100, &generate).unwrap();
        assert_eq!(done, Some("kickoff.md".to_string()));
        assert_eq!(db.extraction_coverage().unwrap(), (1, 1));
        assert_eq!(db.list_entities_with_edges().unwrap().0.len(), 1);
        assert_eq!(db.list_events().unwrap().len(), 1);
        // Queue now empty.
        assert_eq!(process_next_pending(&mut db, "2026-07-14", 101, &generate).unwrap(), None);
    }

    #[test]
    fn worker_records_generation_failure_without_touching_the_model() {
        let mut db = Db::open_in_memory().unwrap();
        db.upsert_file("bad.md", "md", 1, 1, "indexed", None, "content").unwrap();
        db.enqueue_extraction_if_changed("bad.md", &content_hash("content")).unwrap();
        let generate = |_: &str| -> Result<serde_json::Value> {
            Err(Error::Other("model timed out".into()))
        };
        assert!(process_next_pending(&mut db, "2026-07-14", 100, &generate).is_err());
        // Model untouched; the row is 'error', not 'pending' (no retry storm).
        assert!(db.list_entities_with_edges().unwrap().0.is_empty());
        assert_eq!(db.next_pending_extraction().unwrap(), None);
    }

    #[test]
    fn process_next_pending_with_addendum_reaches_the_generated_prompt() {
        let mut db = Db::open_in_memory().unwrap();
        db.upsert_file("notes/kickoff.md", "md", 1, 1, "indexed", None, "We hired Priya.").unwrap();
        db.enqueue_extraction_if_changed("notes/kickoff.md", &content_hash("We hired Priya.")).unwrap();
        let seen_prompt = std::cell::RefCell::new(String::new());
        let generate = |p: &str| -> Result<serde_json::Value> {
            *seen_prompt.borrow_mut() = p.to_string();
            Ok(serde_json::json!({"entities": [], "relations": [], "events": []}))
        };
        process_next_pending_with_addendum(
            &mut db, "2026-07-14", 100, &generate, "Project summary: A billing tool.\nFocus areas: vendors",
        )
        .unwrap();
        assert!(seen_prompt.borrow().contains("Project summary: A billing tool."));
        assert!(seen_prompt.borrow().contains("Focus areas: vendors"));
    }

    #[test]
    fn process_next_pending_with_empty_addendum_matches_plain_path() {
        // Consumer inertness (project-profiler 1.6): the addendum-aware entry
        // point called with "" must behave exactly like the plain one, so a
        // caller that never resolves a profile (flag off, or none exists)
        // gets byte-identical prompts either way.
        let mut db = Db::open_in_memory().unwrap();
        db.upsert_file("notes/kickoff.md", "md", 1, 1, "indexed", None, "We hired Priya.").unwrap();
        db.enqueue_extraction_if_changed("notes/kickoff.md", &content_hash("We hired Priya.")).unwrap();
        let seen_prompt = std::cell::RefCell::new(String::new());
        let generate = |p: &str| -> Result<serde_json::Value> {
            *seen_prompt.borrow_mut() = p.to_string();
            Ok(serde_json::json!({"entities": [], "relations": [], "events": []}))
        };
        process_next_pending_with_addendum(&mut db, "2026-07-14", 100, &generate, "").unwrap();
        assert_eq!(
            seen_prompt.into_inner(),
            compose_file_prompt("notes/kickoff.md", "We hired Priya.", "2026-07-14")
        );
    }

    #[test]
    fn empty_file_is_marked_done_without_generating() {
        let mut db = Db::open_in_memory().unwrap();
        // A blank file: whitespace-only extracted text.
        db.upsert_file("blank.md", "md", 1, 1, "indexed", None, "   \n\t").unwrap();
        db.enqueue_extraction_if_changed("blank.md", &content_hash("   \n\t")).unwrap();
        // The generator must never run for an empty file.
        let generate = |_: &str| -> Result<serde_json::Value> {
            panic!("generate() called for an empty file");
        };
        let done = process_next_pending(&mut db, "2026-07-14", 100, &generate).unwrap();
        assert_eq!(done, Some("blank.md".to_string()));
        // Counted as analyzed, no entities, queue drained.
        assert_eq!(db.extraction_coverage().unwrap(), (1, 1));
        assert!(db.list_entities_with_edges().unwrap().0.is_empty());
        assert_eq!(db.next_pending_extraction().unwrap(), None);
    }

    #[test]
    fn file_prompt_is_single_file_and_strict() {
        let p = compose_file_prompt("notes/kickoff.md", "We hired Priya.", "2026-07-14");
        assert!(p.contains("notes/kickoff.md"));
        assert!(p.contains("We hired Priya."));
        assert!(p.contains("ONLY a JSON object"));
        assert!(p.contains("\"relations\""));
        assert!(p.contains("\"events\""));
        assert!(p.contains("person|organization|repo|topic|decision|other"));
        assert!(p.contains("yyyy-mm-dd"));
        assert!(p.contains("2026-07-14"));
        // Per-file caps are stated so the model self-limits.
        assert!(p.contains("40 entities"));
    }

    #[test]
    fn file_prompt_truncates_to_budget() {
        // The extraction prompt is intentionally lean to keep prefill cheap.
        assert_eq!(EXTRACT_CHAR_BUDGET, 12_000);
        let big = "x".repeat(EXTRACT_CHAR_BUDGET + 5_000);
        let p = compose_file_prompt("big.md", &big, "2026-07-14");
        // The document body is capped; the surrounding instructions are small.
        assert!(p.matches('x').count() <= EXTRACT_CHAR_BUDGET);
    }

    #[test]
    fn compose_file_prompt_is_byte_identical_with_empty_addendum() {
        // project-profiler D3: the profile addendum seam must be inert when
        // there is nothing to add — the plain `compose_file_prompt` and the
        // addendum variant called with "" must produce the exact same text.
        let a = compose_file_prompt("notes/kickoff.md", "We hired Priya.", "2026-07-14");
        let b = compose_file_prompt_with_addendum("notes/kickoff.md", "We hired Priya.", "2026-07-14", "");
        assert_eq!(a, b);
    }

    #[test]
    fn compose_file_prompt_with_addendum_inserts_profile_context_within_budget() {
        let addendum = "Project summary: A billing migration tool.\nFocus areas: vendors, cutover";
        let p = compose_file_prompt_with_addendum(
            "notes/kickoff.md",
            "We hired Priya.",
            "2026-07-14",
            addendum,
        );
        assert!(p.contains("Project summary: A billing migration tool."));
        assert!(p.contains("Focus areas: vendors, cutover"));
        assert!(p.contains("notes/kickoff.md"));
        assert!(p.chars().count() <= EXTRACT_CHAR_BUDGET);
    }

    #[test]
    fn parse_delta_resolves_relations_and_enforces_caps() {
        let v: serde_json::Value = serde_json::from_str(r#"{
            "entities": [
                {"kind": "person", "name": "Priya N.", "summary": "Owns billing."},
                {"kind": "topic", "name": "Billing cutover", "summary": "The migration."},
                {"kind": "person"},
                {"kind": "sorcerer", "name": "LangdonSoft", "summary": "Vendor."}
            ],
            "relations": [
                {"a": "priya n.", "b": "BILLING CUTOVER", "label": "owns"},
                {"a": "Billing cutover", "b": "Priya N.", "label": "dup pair"},
                {"a": "Priya N.", "b": "Priya N.", "label": "self"},
                {"a": "Priya N.", "b": "Nobody", "label": "dangling"}
            ],
            "events": [
                {"date": "2026-07-11", "category": "Decision Made", "text": "Sign-off."},
                {"date": "sometime", "category": "x", "text": "bad date dropped"},
                {"date": "2026-06-01", "text": ""}
            ]
        }"#).unwrap();
        let ex = parse_delta_value(&v);
        assert_eq!(ex.entities.len(), 3, "nameless dropped");
        assert_eq!(ex.entities[2].kind, "other", "unknown kind coerced");
        assert!(ex.entities[0].sources.is_empty(), "sources injected at merge");
        // Relation resolved once (reverse duplicate + self + dangling dropped).
        assert_eq!(ex.entities[0].connections, vec![(1, "owns".to_string())]);
        assert!(ex.entities[1].connections.is_empty());
        assert_eq!(ex.events.len(), 1);
        assert_eq!(ex.events[0].category, "decision");
        assert!(ex.events[0].source.is_empty());
    }

    #[test]
    fn parse_delta_caps_are_per_file() {
        let entities: Vec<String> = (0..60)
            .map(|i| format!(r#"{{"kind":"topic","name":"E{i}","summary":""}}"#))
            .collect();
        let events: Vec<String> = (0..40)
            .map(|i| format!(r#"{{"date":"2026-01-{:02}","category":"x","text":"e{i}"}}"#, i % 28 + 1))
            .collect();
        let v: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"entities":[{}],"events":[{}]}}"#,
            entities.join(","), events.join(",")
        )).unwrap();
        let ex = parse_delta_value(&v);
        assert_eq!(ex.entities.len(), FILE_MAX_ENTITIES);
        assert_eq!(ex.events.len(), FILE_MAX_EVENTS);
    }

    #[test]
    fn parse_delta_empty_is_empty() {
        let ex = parse_delta_value(&serde_json::json!({}));
        assert!(ex.entities.is_empty() && ex.events.is_empty());
    }

    // ---------- graph cleanup (2026-10-06 dry run) ----------

    #[test]
    fn kinds_are_a_closed_list_with_repo() {
        let k = |s: &str| coerce_kind(&serde_json::json!(s));
        assert_eq!(k("repo"), "repo");
        assert_eq!(k(" Repository "), "repo");
        assert_eq!(k("Person"), "person");
        assert_eq!(k("service"), "other", "only the listed kinds survive");
        assert_eq!(coerce_kind(&serde_json::Value::Null), "other");
        assert_eq!(kind_choices(), "person|organization|repo|topic|decision|other");
    }

    #[test]
    fn bots_and_agents_are_not_people() {
        for bot in [
            "Claude",
            "Cursor Agent",
            "dependabot",
            "dependabot[bot]",
            "cursor[bot] <206951365+cursor[bot]@users.noreply.github.com>",
            "Hytale Sync Bot",
            "Claude <noreply@anthropic.com>",
            "GitHub Actions",
        ] {
            assert!(is_bot_or_agent(bot), "{bot} is a bot or agent");
        }
        for person in ["Chris", "Zoë Ångström", "AlpahSignalAI", "Claudette Ruiz", "Abbott", "PixelPat / Pat"] {
            assert!(!is_bot_or_agent(person), "{person} is a person");
        }
    }

    /// A teammate the roster lists is never dropped as a bot at extraction,
    /// however their name reads; a bot nobody lists still is.
    #[test]
    fn a_roster_person_named_like_a_bot_is_kept() {
        let roster = vec![crate::people::Person {
            id: "mabel".into(),
            name: "Mabel Bot".into(),
            emails: vec![],
            aliases: vec!["claude".into()],
        }];
        let v = serde_json::json!({"entities": [
            {"name": "Mabel Bot", "kind": "person"},
            {"name": "Claude", "kind": "person"},
            {"name": "Hytale Sync Bot", "kind": "person"}
        ]});
        let names = |ex: &Extraction| ex.entities.iter().map(|e| e.name.clone()).collect::<Vec<_>>();
        assert!(names(&parse_delta_value(&v)).is_empty(), "no roster: all three read as bots");
        assert_eq!(names(&parse_delta_value_for(&v, &roster)), vec!["Mabel Bot", "Claude"]);
        let raw = serde_json::to_string(&v).unwrap();
        assert_eq!(names(&parse_extraction_for(&raw, &roster).unwrap()), vec!["Mabel Bot", "Claude"]);
    }

    #[test]
    fn activity_labels_are_git_statistics() {
        for l in ["commits to", "one commit", "top committer", "major committer", "commits from account", "Committed"] {
            assert!(is_activity_label(l), "{l}");
        }
        for l in ["owns", "decides", "depends on", "reports to committee", "replaces", ""] {
            assert!(!is_activity_label(l), "{l}");
        }
    }

    #[test]
    fn parse_drops_bots_and_activity_edges() {
        let raw = r#"{"entities": [
            {"kind": "person", "name": "Chris", "summary": "Lead.",
             "connections": [{"to": "Shattered-Realms", "label": "top committer"},
                             {"to": "Decision log renumber", "label": "ruled"},
                             {"to": "Claude", "label": "works with"}]},
            {"kind": "person", "name": "Claude", "summary": "An AI agent.",
             "connections": [{"to": "Shattered-Realms", "label": "commits to"}]},
            {"kind": "person", "name": "Hytale Sync Bot", "summary": "Syncs."},
            {"kind": "repo", "name": "Shattered-Realms", "summary": "The mod."},
            {"kind": "decision", "name": "Decision log renumber", "summary": "Renumbered."},
            {"kind": "topic", "name": "Claude", "summary": "Using Claude to review code is a topic, not a person."}
        ]}"#;
        let ex = parse_extraction(raw).unwrap();
        let names: Vec<(&str, &str)> = ex.entities.iter().map(|e| (e.kind.as_str(), e.name.as_str())).collect();
        assert_eq!(
            names,
            vec![("person", "Chris"), ("repo", "Shattered-Realms"), ("decision", "Decision log renumber"), ("topic", "Claude")]
        );
        // "top committer" dropped; "ruled" kept; "works with" now names the
        // topic Claude (the person is gone), which is what the name says.
        assert_eq!(ex.entities[0].connections, vec![(2, "ruled".to_string()), (3, "works with".to_string())]);

        // The per-file path drops them too.
        let v = serde_json::json!({
            "entities": [
                {"kind": "person", "name": "dependabot[bot]", "summary": ""},
                {"kind": "repository", "name": "skyboxeditor", "summary": ""},
                {"kind": "person", "name": "BinaryConstruct", "summary": ""}
            ],
            "relations": [
                {"a": "BinaryConstruct", "b": "skyboxeditor", "label": "one commit"},
                {"a": "BinaryConstruct", "b": "skyboxeditor", "label": "maintains"}
            ]
        });
        let ex = parse_delta_value(&v);
        assert_eq!(ex.entities.len(), 2);
        assert_eq!(ex.entities[0].kind, "repo");
        assert_eq!(ex.entities[1].connections, vec![(0, "maintains".to_string())]);
    }

    #[test]
    fn one_person_with_two_names_is_one_entity() {
        // Stingbro listed first, Chris second naming it as an alias: the fold
        // works in either order, and connections to either name land on Chris.
        let raw = r#"{"entities": [
            {"kind": "person", "name": "Stingbro", "summary": "",
             "sources": ["Repo-Map/Shattered-Realms-Tools.md"],
             "connections": [{"to": "Shattered-Realms-Tools", "label": "owns"}]},
            {"kind": "person", "name": "Chris", "aliases": ["Stingbro", "Chris"],
             "summary": "Makes the rulings.", "sources": ["Current/Team.md"]},
            {"kind": "repo", "name": "Shattered-Realms-Tools", "summary": "Tools.",
             "connections": [{"to": "stingbro", "label": "hosted by"}]}
        ]}"#;
        let ex = parse_extraction(raw).unwrap();
        assert_eq!(ex.entities.len(), 2);
        let chris = &ex.entities[0];
        assert_eq!(chris.name, "Chris");
        assert_eq!(chris.summary, "Makes the rulings. Also known as Stingbro.");
        assert_eq!(chris.sources, vec!["Current/Team.md", "Repo-Map/Shattered-Realms-Tools.md"]);
        // "owns" moved to Chris; the repo's "hosted by" Stingbro resolved to
        // Chris and collapsed into the same pair.
        assert_eq!(chris.connections, vec![(1, "owns".to_string())]);
        assert!(ex.entities[1].connections.is_empty());
    }

    #[test]
    fn a_first_name_alone_never_joins_two_people() {
        let raw = r#"{"entities": [
            {"kind": "person", "name": "Kyle Ahlstrom", "aliases": ["Kyle"], "summary": "One Kyle."},
            {"kind": "person", "name": "Kyle", "summary": "Some Kyle."},
            {"kind": "topic", "name": "Payments", "aliases": ["Kyle Ahlstrom"], "summary": "Not a person: no fold."}
        ]}"#;
        let ex = parse_extraction(raw).unwrap();
        assert_eq!(ex.entities.len(), 3);
        assert_eq!(ex.entities[0].summary, "One Kyle.");
    }

    #[test]
    fn prompts_name_the_cleanup_rules() {
        let p = compose_extraction_prompt(&[], "2026-10-06");
        assert!(p.contains("\"aliases\""));
        assert!(p.contains("Bots and AI agents"));
        assert!(p.contains("the same commit email"));
        assert!(p.contains("share a first name"));
        assert!(p.contains("Never commit counts"));
        assert!(p.contains("repo a repository, service or app"));
        let f = compose_file_prompt("a.md", "text", "2026-10-06");
        assert!(f.contains("bots and AI agents are never people"));
        assert!(f.contains("never commit counts"));
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
        assert!(prompt.contains("person|organization|repo|topic|decision|other"));
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
    #[cfg_attr(windows, ignore = "runs the bash fake CLI; covered on macOS/Linux")]
    fn build_stores_the_model_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let bin = write_fake_claude(dir.path(), "complete");
        std::fs::write(dir.path().join("oneshot_result"), GOOD).unwrap();
        let project =
            crate::project::Project::create(dir.path(), "Atlas").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        db.upsert_file("notes/meeting.md", "md", 10, 1, "indexed", None, "text")
            .unwrap();
        db.enqueue_extraction_if_changed("notes/meeting.md", "h1").unwrap();

        let counts =
            build_knowledge_model(&bin, &project, &mut db, "2026-07-12", &CancelToken::new())
                .unwrap();
        assert_eq!(counts, ModelCounts { entities: 3, edges: 1, events: 2 });
        assert!(db.next_pending_extraction().unwrap().is_none(), "the build covered the queue");
        let (entities, edges) = db.list_entities_with_edges().unwrap();
        assert_eq!(entities.len(), 3);
        assert_eq!(edges.len(), 1);
        assert_eq!(db.list_events().unwrap().len(), 2);
        assert!(db.knowledge_model_built_at().unwrap().is_some());
    }

    #[test]
    #[cfg_attr(windows, ignore = "runs the bash fake CLI; covered on macOS/Linux")]
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

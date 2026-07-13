//! Knowledge model extraction: compose the corpus-wide entity/event
//! prompt, parse the model's JSON answer tolerantly, and store the
//! result in the local DB. The Map and Timeline screens are pure read
//! models over what this module writes — no project files involved.

use std::path::Path;
use std::time::Duration;

use crate::assistant::{self, OneshotOutcome};
use crate::db::{Db, EntityInput, EventInput};
use crate::engine;
use crate::project::Project;
use crate::runner::CancelToken;
use crate::{Error, Result};

/// How many indexed paths the prompt lists at most — enough to orient
/// the agent, which reads the files itself.
const MAX_PROMPT_FILES: usize = 200;

/// Extraction caps: the views stay legible and the answer stays small.
const MAX_ENTITIES: usize = 40;
const MAX_EVENTS: usize = 60;

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
    fn compose_carries_the_contract() {
        let files = vec!["notes/meeting-jul-8.md".to_string(), "knowledge/People.md".to_string()];
        let prompt = compose_extraction_prompt(&files, "2026-07-12");
        assert!(prompt.contains("ONLY a JSON object"));
        assert!(prompt.contains("\"entities\""));
        assert!(prompt.contains("\"connections\""));
        assert!(prompt.contains("\"events\""));
        assert!(prompt.contains("person|organization|topic|decision|other"));
        assert!(prompt.contains("yyyy-mm-dd"));
        assert!(prompt.contains("At most 40 entities and 60 events"));
        assert!(prompt.contains("never invent"));
        assert!(prompt.contains("omit events with no inferable date"));
        assert!(prompt.contains("2026-07-12"));
        assert!(prompt.contains("- notes/meeting-jul-8.md"));
        assert!(prompt.contains("- knowledge/People.md"));
    }

    #[test]
    fn compose_caps_the_file_list() {
        let files: Vec<String> = (0..230).map(|i| format!("notes/f{i}.md")).collect();
        let prompt = compose_extraction_prompt(&files, "2026-07-12");
        assert!(prompt.contains("- notes/f199.md"));
        assert!(!prompt.contains("- notes/f200.md"));
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
        let entities: Vec<String> = (0..50)
            .map(|i| format!(r#"{{"kind":"topic","name":"E{i}","summary":""}}"#))
            .collect();
        let events: Vec<String> = (0..70)
            .map(|i| format!(r#"{{"date":"2026-01-{:02}","category":"x","text":"e{i}"}}"#, i % 28 + 1))
            .collect();
        let raw = format!(
            r#"{{"entities":[{}],"events":[{}]}}"#,
            entities.join(","),
            events.join(",")
        );
        let ex = parse_extraction(&raw).unwrap();
        assert_eq!(ex.entities.len(), 40);
        assert_eq!(ex.events.len(), 60);
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

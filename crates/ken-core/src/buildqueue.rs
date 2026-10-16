//! The build queue: how the library is built from its sources
//! (knowledge-core decision 33). There is no one-shot draft.
//!
//! A queue of the workspace's sources, the heavy hitters first, ingested
//! through ordinary ingest one item at a time (or a few, once trusted:
//! [`Queue::at_once`]). Two passes:
//!
//! 1. **Overview**, for every source first: one note of the broad strokes,
//!    made without a model from its README, its layout and its code index
//!    outline ([`overview_of`]), written into the knowledge base's Raw like
//!    any source.
//! 2. **File by file**, broad to fine: a source's files nearest its top and
//!    its prose before its code, each copied into Raw, at most
//!    [`FILES_PER_SOURCE`] a source.
//!
//! The app's background worker places the next item in Raw when there is
//! room, on a turn of the one throttle (`throttle::step_turn`), and the
//! ingest pass reads it, each Claude call a throttle turn too. An item is
//! done when ingest claims its source out of Raw, and failed with the
//! reason ingest recorded. The queue lives in the knowledge base's index
//! (`Db::check_state`), so the status panel shows it and a restart resumes
//! it.
//!
//! The exact order is still open (DECISIONS, "Still open"); this one is the
//! simplest: sources by the size of what they index, largest first.

use std::cmp::Reverse;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::db::Db;
use crate::{Error, Result};

/// Most files of one source the second pass reads. The rest of a source is
/// still searched and in the code index; the pass covers what is broadest.
pub const FILES_PER_SOURCE: usize = 50;

/// Where the queue is kept in the knowledge base's index.
const STATE: &str = "build_queue";

/// File kinds the second pass reads: prose and code.
const READ_KINDS: &[&str] = &["md", "txt", "pdf", "docx", "code"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Pass {
    Overview,
    File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ItemState {
    Queued,
    /// Placed in Raw, waiting for ingest or being read.
    InRaw,
    Done,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    /// The source's name in the workspace.
    pub source: String,
    pub pass: Pass,
    /// The file, for the second pass.
    #[serde(default)]
    pub path: Option<String>,
    pub state: ItemState,
    /// Where it was placed in the knowledge base (`Research/Ingestion/Raw/…`).
    #[serde(default)]
    pub raw: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

impl Item {
    /// The item in a person's words: `Game: overview`, `Game: src/save.rs`.
    pub fn label(&self) -> String {
        match (&self.pass, &self.path) {
            (Pass::File, Some(p)) => format!("{}: {p}", self.source),
            _ => format!("{}: overview", self.source),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Queue {
    /// How many items may wait in Raw at once: one to start, a few once
    /// trusted.
    pub at_once: usize,
    pub items: Vec<Item>,
}

/// One source as the plan reads it: its name and its indexed files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub name: String,
    pub files: Vec<SourceFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    pub path: String,
    /// The index's file kind (`md`, `code`, …).
    pub kind: String,
    pub size: i64,
}

/// A source's indexed files from its index.
pub fn source_of(name: &str, db: &Db) -> Result<Source> {
    let files = db
        .list_files()?
        .into_iter()
        .filter(|f| f.status == "indexed")
        .map(|f| SourceFile { path: f.rel_path, kind: f.kind, size: f.size })
        .collect();
    Ok(Source { name: name.to_string(), files })
}

/// A README, index or overview page: the broadest file a folder has.
fn is_lead(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    let stem = name.split('.').next().unwrap_or("");
    matches!(stem, "readme" | "index" | "overview" | "start-here" | "architecture" | "claude" | "agents")
}

/// The order broad to fine: nearer the top first; at one depth a README or
/// overview, then prose, then code; larger before smaller.
fn broadness(f: &SourceFile) -> (usize, u8, Reverse<i64>, String) {
    let depth = f.path.matches('/').count();
    let rank = if is_lead(&f.path) {
        0
    } else if f.kind == "code" {
        2
    } else {
        1
    };
    (depth, rank, Reverse(f.size), f.path.clone())
}

/// The queue for `sources`: every source's overview first, heavy hitters
/// first (the size of what each indexes), then each source's files broad to
/// fine, at most [`FILES_PER_SOURCE`] a source.
pub fn plan(sources: &[Source], at_once: usize) -> Queue {
    let weight = |s: &Source| s.files.iter().map(|f| f.size.max(0)).sum::<i64>();
    let mut order: Vec<&Source> = sources.iter().collect();
    order.sort_by_key(|s| (Reverse(weight(s)), s.name.to_lowercase()));
    let mut items: Vec<Item> = order
        .iter()
        .map(|s| Item { source: s.name.clone(), pass: Pass::Overview, path: None, state: ItemState::Queued, raw: None, reason: None })
        .collect();
    for s in &order {
        let mut files: Vec<&SourceFile> = s.files.iter().filter(|f| READ_KINDS.contains(&f.kind.as_str())).collect();
        files.sort_by_key(|f| broadness(f));
        items.extend(files.into_iter().take(FILES_PER_SOURCE).map(|f| Item {
            source: s.name.clone(),
            pass: Pass::File,
            path: Some(f.path.clone()),
            state: ItemState::Queued,
            raw: None,
            reason: None,
        }));
    }
    Queue { at_once: at_once.max(1), items }
}

/// The queue kept in the knowledge base's index, if one was started.
pub fn load(db: &Db) -> Result<Option<Queue>> {
    Ok(db.check_state(STATE)?.and_then(|s| serde_json::from_str(&s).ok()))
}

pub fn save(db: &Db, queue: &Queue) -> Result<()> {
    let text = serde_json::to_string(queue).map_err(|e| Error::Other(e.to_string()))?;
    db.store_check_state(STATE, &text)
}

/// Stop the queue: what has not gone into Raw yet is dropped; what was read
/// or failed stays to be seen, and what waits in Raw is read as any source.
pub fn stop(queue: &mut Queue) {
    queue.items.retain(|i| i.state != ItemState::Queued);
}

/// Bring the queue up to date with the inbox at `kb_root`: an item whose
/// source left Raw was read (ingest claims a source as its note is
/// written); one whose source has a failure recorded (`failures`: raw path
/// and reason, from `ingest::failures`) failed, with that reason. A failed
/// item whose source was read after all is done. Returns whether anything
/// changed.
pub fn refresh(queue: &mut Queue, kb_root: &Path, failures: &[(String, String)]) -> bool {
    let mut changed = false;
    for item in queue.items.iter_mut().filter(|i| matches!(i.state, ItemState::InRaw | ItemState::Failed)) {
        let Some(raw) = item.raw.clone() else { continue };
        let next = if !kb_root.join(&raw).exists() {
            (ItemState::Done, None)
        } else if let Some((_, why)) = failures.iter().find(|(r, _)| *r == raw) {
            (ItemState::Failed, Some(why.clone()))
        } else {
            (ItemState::InRaw, None)
        };
        if (item.state, &item.reason) != (next.0, &next.1) {
            item.state = next.0;
            item.reason = next.1;
            changed = true;
        }
    }
    changed
}

/// The next item to place in Raw, when there is room: fewer than
/// `at_once` wait there.
pub fn next(queue: &Queue) -> Option<usize> {
    let waiting = queue.items.iter().filter(|i| i.state == ItemState::InRaw).count();
    if waiting >= queue.at_once.max(1) {
        return None;
    }
    queue.items.iter().position(|i| i.state == ItemState::Queued)
}

/// Whether the source at `raw` (`Research/Ingestion/Raw/…`) is one the
/// queue placed. Its ingest is background work, held to the throttle's hour;
/// any other source in Raw is a person's own, which is not
/// (`throttle::own_turn`).
pub fn placed(queue: &Queue, raw: &str) -> bool {
    queue.items.iter().any(|i| i.raw.as_deref() == Some(raw))
}

/// The file name a second-pass item takes in Raw: the source and the path,
/// made safe, its extension kept, so ingest reads it as what it is.
pub fn raw_name(source: &str, path: &str) -> String {
    let (stem, ext) = match path.rsplit_once('.') {
        Some((s, e)) if !e.contains('/') => (s, format!(".{e}")),
        _ => (path, String::new()),
    };
    let safe = crate::ingest::file_safe(&format!("{source} - {}", stem.replace('/', " › ")));
    format!("{safe}{ext}")
}

/// The overview note of a source, for the first pass: its README, its
/// layout (top folders by indexed files) and its code index outline (the
/// top definitions of its broadest code files). Made without a model;
/// ingest reads it like any note. Returns its file name and its text.
pub fn overview_of(name: &str, root: &Path, db: &Db, stamp: &str) -> Result<(String, String)> {
    let source = source_of(name, db)?;
    let readme = ["README.md", "readme.md", "README", "START-HERE.md", "CLAUDE.md", "AGENTS.md"]
        .iter()
        .find_map(|f| std::fs::read_to_string(root.join(f)).ok())
        .unwrap_or_default();
    let mut outline: Vec<(String, Vec<String>)> = Vec::new();
    let mut code: Vec<&SourceFile> = source.files.iter().filter(|f| f.kind == "code").collect();
    code.sort_by_key(|f| broadness(f));
    for f in code.into_iter().take(30) {
        let defs: Vec<String> = db
            .code_symbols(None, Some(&f.path), Some(true), 200)?
            .into_iter()
            .filter(|s| !s.name.is_empty())
            .map(|s| s.name)
            .take(12)
            .collect();
        if !defs.is_empty() {
            outline.push((f.path.clone(), defs));
        }
    }
    Ok(overview_note(name, &readme, &source.files, &outline, stamp))
}

/// [`overview_of`]'s note from what it read.
pub fn overview_note(name: &str, readme: &str, files: &[SourceFile], outline: &[(String, Vec<String>)], stamp: &str) -> (String, String) {
    let file = format!("{stamp} Build - {} overview.md", crate::ingest::file_safe(name));
    let mut text = format!("---\nkind: document\nwritten: {}\n---\n\n# {name}: overview\n\n", stamp.replacen('.', ":", 1));
    text.push_str(&format!(
        "The broad strokes of the source `{name}`, for the first pass of building the library. The code is the truth \
         for how it works today; write what the library needs beside it (what it is for, how its parts fit, where \
         things live), not a copy of the code.\n\n"
    ));
    let readme: String = readme.chars().take(12_000).collect();
    if !readme.trim().is_empty() {
        text.push_str("## Its README\n\n");
        text.push_str(readme.trim());
        text.push_str("\n\n");
    }
    let mut folders: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for f in files {
        let top = f.path.split_once('/').map_or("(top level)".to_string(), |(d, _)| format!("{d}/"));
        *folders.entry(top).or_default() += 1;
    }
    if !folders.is_empty() {
        text.push_str(&format!("## Layout\n\n{} indexed files.\n\n", files.len()));
        let mut rows: Vec<(String, usize)> = folders.into_iter().collect();
        rows.sort_by_key(|(d, n)| (Reverse(*n), d.clone()));
        for (dir, n) in rows.into_iter().take(40) {
            text.push_str(&format!("- `{dir}`: {n} files\n"));
        }
        text.push('\n');
    }
    if !outline.is_empty() {
        text.push_str("## Code outline\n\n");
        for (path, defs) in outline {
            text.push_str(&format!("- `{path}`: {}\n", defs.join(", ")));
        }
    }
    (file, text)
}

/// The queue in numbers, for the status panel.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub total: usize,
    pub done: usize,
    pub queued: usize,
    /// In Raw: waiting for ingest or being read.
    pub reading: Vec<String>,
    /// Each failed item with its reason.
    pub failed: Vec<(String, String)>,
    pub at_once: usize,
}

pub fn progress(queue: &Queue) -> Progress {
    let mut p = Progress { total: queue.items.len(), at_once: queue.at_once, ..Default::default() };
    for i in &queue.items {
        match i.state {
            ItemState::Done => p.done += 1,
            ItemState::Queued => p.queued += 1,
            ItemState::InRaw => p.reading.push(i.label()),
            ItemState::Failed => p.failed.push((i.label(), i.reason.clone().unwrap_or_default())),
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(path: &str, kind: &str, size: i64) -> SourceFile {
        SourceFile { path: path.into(), kind: kind.into(), size }
    }

    fn sources() -> Vec<Source> {
        vec![
            Source { name: "Tools".into(), files: vec![f("README.md", "md", 10), f("cli/main.rs", "code", 50)] },
            Source {
                name: "Game".into(),
                files: vec![
                    f("src/world/save.rs", "code", 900),
                    f("src/lib.rs", "code", 400),
                    f("docs/design.md", "md", 300),
                    f("README.md", "md", 100),
                    f("assets/logo.png", "image", 5000),
                    f("src/README.md", "md", 20),
                ],
            },
        ]
    }

    #[test]
    fn overviews_come_first_heavy_hitters_first_then_files_broad_to_fine() {
        let q = plan(&sources(), 0);
        assert_eq!(q.at_once, 1, "one at a time to start");
        let labels: Vec<String> = q.items.iter().map(Item::label).collect();
        assert_eq!(
            labels,
            vec![
                "Game: overview",
                "Tools: overview",
                "Game: README.md",
                "Game: src/README.md",
                "Game: docs/design.md",
                "Game: src/lib.rs",
                "Game: src/world/save.rs",
                "Tools: README.md",
                "Tools: cli/main.rs",
            ],
            "an image is not read; the README leads each folder"
        );
        let many = Source { name: "Big".into(), files: (0..80).map(|i| f(&format!("f{i}.md"), "md", 1)).collect() };
        assert_eq!(plan(&[many], 1).items.len(), 1 + FILES_PER_SOURCE);
    }

    #[test]
    fn one_item_waits_in_raw_at_a_time_and_the_inbox_says_how_it_went() {
        let d = tempfile::tempdir().unwrap();
        let kb = d.path();
        std::fs::create_dir_all(kb.join(crate::ingest::RAW)).unwrap();
        let mut q = plan(&sources(), 1);
        let first = next(&q).unwrap();
        let raw = format!("{}/a.md", crate::ingest::RAW);
        std::fs::write(kb.join(&raw), "x").unwrap();
        q.items[first].state = ItemState::InRaw;
        q.items[first].raw = Some(raw.clone());
        assert_eq!(next(&q), None, "one waits in Raw");
        assert!(placed(&q, &raw), "the queue's source is background work");
        assert!(!placed(&q, &format!("{}/dropped.md", crate::ingest::RAW)), "a source a person dropped is their own");

        assert!(!refresh(&mut q, kb, &[]), "still waiting");
        assert!(refresh(&mut q, kb, &[(raw.clone(), "The CLI timed out.".into())]));
        assert_eq!(q.items[first].state, ItemState::Failed);
        assert_eq!(progress(&q).failed, vec![("Game: overview".to_string(), "The CLI timed out.".to_string())]);
        assert!(next(&q).is_some(), "a failed item leaves room");

        std::fs::remove_file(kb.join(&raw)).unwrap();
        assert!(refresh(&mut q, kb, &[]));
        assert_eq!(q.items[first].state, ItemState::Done, "read after all: claimed out of Raw");
        let p = progress(&q);
        assert_eq!((p.done, p.queued, p.total), (1, q.items.len() - 1, q.items.len()));

        stop(&mut q);
        assert_eq!(q.items.len(), 1, "stopping drops what never went into Raw");
    }

    #[test]
    fn the_overview_note_has_the_readme_layout_and_outline() {
        let files = vec![f("src/a.rs", "code", 1), f("src/b.rs", "code", 1), f("README.md", "md", 1)];
        let outline = vec![("src/a.rs".to_string(), vec!["World".to_string(), "save".to_string()])];
        let (name, text) = overview_note("Game", "# Game\n\nThe client.", &files, &outline, "2026-10-13 09.30");
        assert_eq!(name, "2026-10-13 09.30 Build - Game overview.md");
        assert!(text.starts_with("---\nkind: document\nwritten: 2026-10-13 09:30\n---\n"));
        assert!(text.contains("The client."));
        assert!(text.contains("- `src/`: 2 files"));
        assert!(text.contains("- `src/a.rs`: World, save"));
        assert_eq!(raw_name("Game", "src/world/save.rs"), "Game - src › world › save.rs");
    }
}

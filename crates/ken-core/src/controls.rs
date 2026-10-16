//! Controls (knowledge-core decision 19): a few known questions per source,
//! each with the file it must return near the top, run through the real
//! search pipeline (`routing::search_member`) after every rebuild. They are
//! the index's health check: a control that stops passing says search got
//! worse for that source, where a count of files never would.
//!
//! The questions live in the knowledge base's `.ken/knowledge.json`
//! (`controls`, each naming its source; see `knowledge_settings`). The repo
//! analysis proposes them in the Inbox when a source is added
//! (`repo_analysis`); once a person accepts they are written there, and a
//! person edits them there. The last run's results are stored in the
//! source's index, for the health line.
//!
//! When they run: at the end of a semantic rebuild that changed something,
//! and of every catch-up after a scan or an accepted proposal (a change is
//! what asks for one), with the question embedded as a search would embed
//! it; and at the end of a scan while the source has no meaning index, by
//! keyword alone, which is then how it is searched. Controls that are not
//! the ones last run are always due.

use serde::{Deserialize, Serialize};

use crate::db::Db;
use crate::Result;

/// A control passes when its file is among this many hits.
pub const TOP: usize = 5;
/// Most controls the repo analysis proposes for one source.
pub const MAX_PROPOSED: usize = 5;

/// One known question and the file it must find.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Control {
    pub question: String,
    /// The file's path in its source, from the source's root.
    pub expect: String,
    /// The source the control belongs to, by name. None in a knowledge
    /// base's own file means the knowledge base itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// How one control did on the last run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlResult {
    pub question: String,
    pub expect: String,
    /// Where its file came, from 1; None when it was not in the top [`TOP`].
    pub rank: Option<usize>,
    pub passed: bool,
}

/// The controls of the source at `root`, from its knowledge base's settings.
pub fn for_source(root: &std::path::Path) -> Vec<Control> {
    crate::knowledge_settings::for_repo(root).map(|s| s.controls()).unwrap_or_default()
}

/// Whether two source paths name the same file: slashes and case aside.
fn same_file(a: &str, b: &str) -> bool {
    let n = |p: &str| p.replace('\\', "/").trim_start_matches("./").trim_start_matches('/').to_lowercase();
    n(a) == n(b)
}

/// Run `controls` against `db`. `embed` gives a question's meaning vector,
/// or None to search by keyword alone.
pub fn run(db: &Db, controls: &[Control], embed: &mut dyn FnMut(&str) -> Option<Vec<f32>>) -> Result<Vec<ControlResult>> {
    let mut out = Vec::new();
    for c in controls {
        let qv = embed(&c.question);
        let hits = crate::routing::search_member(db, &c.question, qv.as_deref(), TOP)?;
        let rank = hits.iter().take(TOP).position(|h| same_file(&h.path, &c.expect)).map(|i| i + 1);
        out.push(ControlResult { question: c.question.clone(), expect: c.expect.clone(), rank, passed: rank.is_some() });
    }
    Ok(out)
}

/// [`run`], then store the results in the index for the health line. A
/// source with no controls stores an empty run, so the line says so.
pub fn run_and_store(db: &Db, controls: &[Control], embed: &mut dyn FnMut(&str) -> Option<Vec<f32>>) -> Result<Vec<ControlResult>> {
    let results = run(db, controls, embed)?;
    let json = serde_json::to_string(&results).map_err(|e| crate::Error::Other(e.to_string()))?;
    db.store_control_results(&json)?;
    Ok(results)
}

/// Whether `results` are a run of exactly `controls`: the same questions
/// and files, in order. When they are not, the controls changed since the
/// last run and are due again.
pub fn same_set(results: &[ControlResult], controls: &[Control]) -> bool {
    results.len() == controls.len()
        && results.iter().zip(controls).all(|(r, c)| r.question == c.question && r.expect == c.expect)
}

/// The results the last run stored, empty when none ran yet.
pub fn stored(db: &Db) -> Vec<ControlResult> {
    db.control_results()
        .ok()
        .flatten()
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn indexed(db: &mut Db, path: &str, text: &str) {
        db.upsert_file(path, "md", 1, 1, "indexed", None, text).unwrap();
        let chunks = crate::chunker::chunk_file(path, text, &crate::chunker::IndexProfile::default_for(path));
        db.upsert_chunks(path, &chunks).unwrap();
    }

    #[test]
    fn a_control_passes_when_its_file_comes_near_the_top_through_routing() {
        let mut db = Db::open_in_memory().unwrap();
        indexed(&mut db, "Design/Saving.md", "# Saving\n\nWorlds save as region files every five minutes.\n");
        indexed(&mut db, "Design/Combat.md", "# Combat\n\nSwords swing twice a second.\n");
        let controls = vec![
            Control { question: "how often do worlds save".into(), expect: "design/saving.md".into(), source: None },
            Control { question: "how often do worlds save".into(), expect: "Design/Combat.md".into(), source: None },
            Control { question: "trading routes".into(), expect: "Design/Trade.md".into(), source: None },
        ];
        let results = run_and_store(&db, &controls, &mut |_| None).unwrap();
        assert_eq!(results[0].rank, Some(1), "slashes and case aside: {results:?}");
        assert!(results[0].passed);
        assert!(!results[2].passed && results[2].rank.is_none());
        assert_eq!(stored(&db), results, "the run is kept for the health line");
        assert_eq!(db.index_health().unwrap().controls_total, 3);
    }

    #[test]
    fn no_controls_is_an_empty_run() {
        let db = Db::open_in_memory().unwrap();
        assert!(stored(&db).is_empty());
        assert!(run_and_store(&db, &[], &mut |_| None).unwrap().is_empty());
        assert!(db.control_results().unwrap().is_some(), "ran, with nothing to run");
    }
}

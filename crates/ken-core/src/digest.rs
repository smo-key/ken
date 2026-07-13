//! Daily digest: gather the last day's activity from the index, compose
//! the one-paragraph prompt, and parse the model's answer. The same
//! `parse_digest` contract (body + optional `SOURCES:` line) also backs
//! the ⌘K quick answer.

use crate::db::{Db, RunRow};
use crate::Result;

/// What today's digest can talk about — all of it already known to the
/// index. Derived review kinds (stale ingests, failed files) are
/// intentionally not folded in; stored items carry the "waiting on you"
/// color the paragraph needs.
#[derive(Debug, Clone, Default)]
pub struct DigestSources {
    /// Project-relative paths changed in the window, newest first.
    pub changed_files: Vec<String>,
    /// Ingest runs that finished in the window, newest first.
    pub finished_runs: Vec<RunRow>,
    /// Titles of open stored review items.
    pub waiting: Vec<String>,
}

impl DigestSources {
    /// Nothing to say — store the quiet fallback instead of calling AI.
    pub fn is_quiet(&self) -> bool {
        self.changed_files.is_empty()
            && self.finished_runs.is_empty()
            && self.waiting.is_empty()
    }
}

/// Stored verbatim as the digest on days with nothing to report.
pub const QUIET_DIGEST: &str = "A quiet day — nothing new since yesterday.";

/// How many changed paths the prompt lists at most.
const MAX_PROMPT_FILES: usize = 20;

/// Collect digest inputs: files whose indexed mtime is at or past
/// `since`, runs finished since then, and open stored review items.
pub fn gather(db: &Db, since: i64) -> Result<DigestSources> {
    let mut changed: Vec<(String, i64)> = db
        .list_files()?
        .into_iter()
        .filter(|f| f.mtime >= since)
        .map(|f| (f.rel_path, f.mtime))
        .collect();
    changed.sort_by(|a, b| b.1.cmp(&a.1));
    Ok(DigestSources {
        changed_files: changed.into_iter().map(|(p, _)| p).collect(),
        finished_runs: db.runs_finished_since(since)?,
        waiting: db
            .list_open_review_items()?
            .into_iter()
            .map(|i| i.title)
            .collect(),
    })
}

/// The morning-digest prompt: one warm paragraph plus a SOURCES line.
pub fn compose_digest_prompt(project_name: &str, sources: &DigestSources) -> String {
    let mut p = format!(
        "You are Ken, writing the user's morning digest for the project \
\"{project_name}\".\n\n\
Write ONE warm, concrete paragraph (at most 120 words) in plain language \
summarizing what changed in the last day and what's waiting on the user. \
You may use **bold** for the few things that matter most. No headings, \
no lists, no preamble — just the paragraph. You may read files in this \
project if a path below needs more context; never modify anything.\n\n\
After the paragraph, on its own final line, write exactly:\n\
SOURCES: path1, path2\n\
naming up to 5 of the project-relative paths below the digest draws on \
(omit the line if none apply).\n\n\
What the index saw in the last 24 hours:\n"
    );

    p.push_str("\nFiles changed:\n");
    if sources.changed_files.is_empty() {
        p.push_str("- none\n");
    }
    for path in sources.changed_files.iter().take(MAX_PROMPT_FILES) {
        p.push_str(&format!("- {path}\n"));
    }
    if sources.changed_files.len() > MAX_PROMPT_FILES {
        p.push_str(&format!(
            "- …and {} more\n",
            sources.changed_files.len() - MAX_PROMPT_FILES
        ));
    }

    p.push_str("\nIngest runs finished:\n");
    if sources.finished_runs.is_empty() {
        p.push_str("- none\n");
    }
    for run in &sources.finished_runs {
        match &run.summary {
            Some(s) => p.push_str(&format!("- {}: {} — {}\n", run.slug, run.status, s)),
            None => p.push_str(&format!("- {}: {}\n", run.slug, run.status)),
        }
    }

    p.push_str("\nWaiting on the user (open review items):\n");
    if sources.waiting.is_empty() {
        p.push_str("- nothing\n");
    }
    for title in &sources.waiting {
        p.push_str(&format!("- {title}\n"));
    }
    p
}

/// A digest (or quick answer) split into prose and source paths.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedDigest {
    pub body: String,
    pub sources: Vec<String>,
}

/// How many source paths survive parsing.
const MAX_SOURCES: usize = 5;

/// Split a model result into `{body, sources}`. The SOURCES line is
/// optional — without one the whole text is the body.
pub fn parse_digest(result: &str) -> ParsedDigest {
    let mut body_lines: Vec<&str> = Vec::new();
    let mut sources: Vec<String> = Vec::new();
    for line in result.lines() {
        let trimmed = line.trim();
        if let Some(rest) = strip_sources_prefix(trimmed) {
            sources = rest
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .take(MAX_SOURCES)
                .map(String::from)
                .collect();
        } else {
            body_lines.push(line);
        }
    }
    ParsedDigest {
        body: body_lines.join("\n").trim().to_string(),
        sources,
    }
}

fn strip_sources_prefix(line: &str) -> Option<&str> {
    let upper = line.to_ascii_uppercase();
    if upper.starts_with("SOURCES:") {
        Some(line["SOURCES:".len()..].trim())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> Db {
        let mut db = Db::open_in_memory().unwrap();
        db.upsert_file("notes/old.md", "md", 10, 100, "indexed", None, "old")
            .unwrap();
        db.upsert_file("notes/fresh.md", "md", 10, 1_000, "indexed", None, "fresh")
            .unwrap();
        db.upsert_file("vendor/Contract v3.pdf", "pdf", 10, 2_000, "failed",
            Some("encrypted"), "")
            .unwrap();
        let run = db.insert_run("people", None, 900).unwrap();
        db.update_run(run, "fresh", Some(950), Some("Updated 1 document."), None, Some(0.1))
            .unwrap();
        db.insert_review_item("conflict", "Merge conflict — Decisions.md", "", "Decisions.md", None, 980)
            .unwrap();
        db
    }

    #[test]
    fn gather_windows_and_orders() {
        let mut db = seeded();
        let s = gather(&db, 500).unwrap();
        // Newest changed file first; the old one is outside the window.
        assert_eq!(s.changed_files, vec!["vendor/Contract v3.pdf", "notes/fresh.md"]);
        assert_eq!(s.finished_runs.len(), 1);
        assert_eq!(s.finished_runs[0].slug, "people");
        assert_eq!(s.waiting, vec!["Merge conflict — Decisions.md"]);
        assert!(!s.is_quiet());

        // Open review items are timeless — still not quiet outside the
        // window until they're resolved.
        assert!(!gather(&db, 10_000).unwrap().is_quiet());
        db.resolve_review_item(1, 990).unwrap();
        assert!(gather(&db, 10_000).unwrap().is_quiet());
    }

    #[test]
    fn compose_carries_the_days_activity() {
        let db = seeded();
        let s = gather(&db, 500).unwrap();
        let prompt = compose_digest_prompt("Atlas", &s);
        // The contract.
        assert!(prompt.contains("\"Atlas\""));
        assert!(prompt.contains("ONE warm, concrete paragraph"));
        assert!(prompt.contains("at most 120 words"));
        assert!(prompt.contains("SOURCES: path1, path2"));
        // The inputs.
        assert!(prompt.contains("- notes/fresh.md"));
        assert!(prompt.contains("- people: fresh — Updated 1 document."));
        assert!(prompt.contains("- Merge conflict — Decisions.md"));
    }

    #[test]
    fn compose_names_empty_sections_honestly() {
        let prompt = compose_digest_prompt("Atlas", &DigestSources::default());
        assert!(prompt.contains("Files changed:\n- none"));
        assert!(prompt.contains("Ingest runs finished:\n- none"));
        assert!(prompt.contains("open review items):\n- nothing"));
    }

    #[test]
    fn compose_caps_the_file_list() {
        let s = DigestSources {
            changed_files: (0..30).map(|i| format!("notes/f{i}.md")).collect(),
            ..Default::default()
        };
        let prompt = compose_digest_prompt("Atlas", &s);
        assert!(prompt.contains("- notes/f19.md"));
        assert!(!prompt.contains("- notes/f20.md"));
        assert!(prompt.contains("…and 10 more"));
    }

    #[test]
    fn parse_splits_body_and_sources() {
        let parsed = parse_digest(
            "The cutover is now **Sept 12** and People gained two engineers.\n\
             SOURCES: Standup — Jul 11.md, knowledge/People.md",
        );
        assert_eq!(
            parsed.body,
            "The cutover is now **Sept 12** and People gained two engineers."
        );
        assert_eq!(
            parsed.sources,
            vec!["Standup — Jul 11.md", "knowledge/People.md"]
        );
    }

    #[test]
    fn parse_tolerates_missing_sources_line() {
        let parsed = parse_digest("Just prose.\nAcross two lines.");
        assert_eq!(parsed.body, "Just prose.\nAcross two lines.");
        assert!(parsed.sources.is_empty());
    }

    #[test]
    fn parse_caps_sources_and_survives_noise() {
        let parsed = parse_digest("Body.\nsources: a.md, b.md, c.md, d.md, e.md, f.md, ,");
        assert_eq!(parsed.body, "Body.");
        assert_eq!(parsed.sources.len(), 5);
        // Empty input parses to an empty body, never panics.
        assert_eq!(parse_digest("").body, "");
    }
}

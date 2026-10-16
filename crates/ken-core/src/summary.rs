//! One-line summaries of knowledge-base chunks (knowledge-core decision 16).
//!
//! Only for the knowledge base's pages, notes, decisions and tickets, never
//! code. Cached by the chunk's content hash (`Db::store_chunk_summaries`),
//! so a chunk whose text has not changed is never asked about again. Used
//! for retrieval only: a summary goes in the keyword index's `title` field
//! and in the text embedded with its chunk, and is never shown.
//!
//! Claude writes them, never the local model, in batches of many chunks per
//! call. The app runs [`summarize_next`] in the background for a
//! knowledge-base repo (kind wiki), each call a turn of the one
//! background throttle (`throttle::claude_turn`, decision 34): paused with
//! everything else, paced and counted against the pace's hourly calls.

use crate::db::Db;
use crate::Result;

/// Most chunks one Claude call summarizes.
pub const BATCH_CHUNKS: usize = 40;
/// Most bytes of passage text in one call: a batch stops before the chunk
/// that would pass it.
pub const BATCH_BYTES: usize = 48_000;
/// Longest passage sent, in bytes; the rest of a longer chunk is cut.
const PASSAGE_BYTES: usize = 2_400;
/// Longest summary kept, in characters.
const SUMMARY_MAX: usize = 240;

/// One chunk waiting for its summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wanted {
    pub hash: String,
    pub path: String,
    /// Its heading breadcrumb.
    pub title: String,
    pub text: String,
}

/// The next batch of chunks to summarize, empty when none wait.
pub fn next_batch(db: &Db) -> Result<Vec<Wanted>> {
    let mut out = Vec::new();
    let mut bytes = 0usize;
    for (hash, path, title, text) in db.chunks_wanting_summaries(BATCH_CHUNKS)? {
        let n = text.len().min(PASSAGE_BYTES);
        if !out.is_empty() && bytes + n > BATCH_BYTES {
            break;
        }
        bytes += n;
        out.push(Wanted { hash, path, title, text });
    }
    Ok(out)
}

/// `s` cut to at most `max` bytes, at a character boundary.
fn cut(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// The prompt for one batch: the passages, numbered from 1, each with its
/// page and breadcrumb.
pub fn prompt(batch: &[Wanted]) -> String {
    let mut p = String::from(
        "Below are numbered passages from a project's knowledge base: its pages, notes, decisions and tickets. \
         Write a one-line summary of each. A summary is used only to find its passage in search and is never shown, \
         so say what the passage is about in the words someone looking for it would type, and keep the names, \
         terms and ids it uses. At most 30 words each. Do not start with \"This passage\".\n\n\
         Reply with one line per passage and nothing else, in the passages' order, each line `N: summary`.\n\n",
    );
    for (i, w) in batch.iter().enumerate() {
        p.push_str(&format!("[{}] {}", i + 1, w.path));
        if !w.title.is_empty() {
            p.push_str(&format!(" ({})", w.title));
        }
        p.push('\n');
        p.push_str(cut(w.text.trim(), PASSAGE_BYTES));
        p.push_str("\n\n");
    }
    p
}

/// The summaries in a reply, as (0-based passage index, summary): each line
/// `N: summary` (also `[N] summary`, `N. summary`, `N) summary`, or as a list
/// item) for a passage of the `n` asked about. The first line for a passage
/// wins; anything else is ignored.
pub fn parse(reply: &str, n: usize) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    for line in reply.lines() {
        let t = line.trim().trim_start_matches(['-', '*']).trim_start();
        let t = t.strip_prefix('[').unwrap_or(t);
        let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits == 0 {
            continue;
        }
        let Ok(k) = t[..digits].parse::<usize>() else { continue };
        let rest = t[digits..].strip_prefix(']').unwrap_or(&t[digits..]);
        let Some(rest) = [":", ".", ")", " -", " —"].iter().find_map(|sep| rest.strip_prefix(sep)).or_else(|| rest.starts_with(' ').then_some(rest))
        else {
            continue;
        };
        let summary = rest.trim().trim_matches('"').trim();
        if k == 0 || k > n || summary.is_empty() || out.iter().any(|(i, _)| *i == k - 1) {
            continue;
        }
        let summary: String = summary.chars().take(SUMMARY_MAX).collect();
        out.push((k - 1, summary));
    }
    out
}

/// Summarize the next batch with `generate` (one Claude call) and cache
/// the results. None when no chunk waits (no call is made), else how many
/// summaries were written. A passage the reply gave no line for is cached
/// with an empty summary, so a batch Claude cannot answer is not asked
/// again and again; a failed call caches nothing and is the caller's to
/// retry later.
pub fn summarize_next(db: &mut Db, generate: &mut dyn FnMut(&str) -> Result<String>, at: i64) -> Result<Option<usize>> {
    let batch = next_batch(db)?;
    if batch.is_empty() {
        return Ok(None);
    }
    let reply = generate(&prompt(&batch))?;
    let mut got: Vec<Option<String>> = vec![None; batch.len()];
    for (i, s) in parse(&reply, batch.len()) {
        got[i] = Some(s);
    }
    let rows: Vec<(String, String)> = batch.iter().zip(got).map(|(w, s)| (w.hash.clone(), s.unwrap_or_default())).collect();
    db.store_chunk_summaries(&rows, at)?;
    Ok(Some(rows.iter().filter(|(_, s)| !s.is_empty()).count()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page_db() -> Db {
        let mut db = Db::open_in_memory().unwrap();
        let text = "---\ntitle: Payments\n---\n# Payments\n\nHow money moves.\n\n## Retries\n\nEach wait doubles.\n";
        db.upsert_file("Design/Payments.md", "md", 1, 1, "indexed", None, text).unwrap();
        let chunks = crate::chunker::chunk_file("Design/Payments.md", text, &crate::chunker::IndexProfile::default_for("x.md"));
        db.upsert_chunks("Design/Payments.md", &chunks).unwrap();
        db.upsert_file("src/pay.rs", "code", 1, 1, "indexed", None, "fn pay() {}").unwrap();
        let code = crate::codeindex::map_file("src/pay.rs", "fn pay() {}\n").unwrap();
        let chunked = crate::chunker::chunk_with("src/pay.rs", "fn pay() {}\n", &crate::chunker::IndexProfile::default_for("src/pay.rs"), &code.symbols);
        db.store_chunked("src/pay.rs", &chunked).unwrap();
        db
    }

    #[test]
    fn a_reply_is_read_line_by_line() {
        let reply = "Here you go:\n1: How payments retry.\n[2] Refunds within a week.\n3. \"Quoted\"\n- 4) A list item\n9: out of range\n1: a second line for one\n5:\n";
        assert_eq!(
            parse(reply, 5),
            vec![
                (0, "How payments retry.".to_string()),
                (1, "Refunds within a week.".to_string()),
                (2, "Quoted".to_string()),
                (3, "A list item".to_string()),
            ]
        );
        assert!(parse("2026: a year, not a passage", 3).is_empty());
    }

    #[test]
    fn the_prompt_numbers_each_passage_with_its_page_and_breadcrumb() {
        let batch = vec![
            Wanted { hash: "a".into(), path: "Design/Payments.md".into(), title: "Payments › Retries".into(), text: "Each wait doubles.".into() },
            Wanted { hash: "b".into(), path: "tickets/T-4.md".into(), title: String::new(), text: "x".repeat(PASSAGE_BYTES + 100) },
        ];
        let p = prompt(&batch);
        assert!(p.contains("[1] Design/Payments.md (Payments › Retries)\nEach wait doubles."), "{p}");
        assert!(p.contains("[2] tickets/T-4.md\n"), "{p}");
        assert!(!p.contains(&"x".repeat(PASSAGE_BYTES + 1)), "a long passage is cut");
    }

    #[test]
    fn pages_are_summarized_once_and_code_never() {
        let mut db = page_db();
        let batch = next_batch(&db).unwrap();
        assert!(!batch.is_empty() && batch.iter().all(|w| w.path == "Design/Payments.md"), "{batch:?}");
        let n = batch.len();
        let mut calls = 0;
        let mut generate = |p: &str| -> Result<String> {
            calls += 1;
            assert!(!p.contains("fn pay"), "code is never sent");
            // A line for the first passage only.
            Ok("1: How payments move and retry.".to_string())
        };
        assert_eq!(summarize_next(&mut db, &mut generate, 5).unwrap(), Some(1));
        assert_eq!(calls, 1);
        assert_eq!(db.chunk_summary(&batch[0].hash).unwrap().as_deref(), Some("How payments move and retry."));
        if n > 1 {
            assert_eq!(db.chunk_summary(&batch[1].hash).unwrap().as_deref(), Some(""), "no line: asked once, cached empty");
        }
        // Nothing waits now: no call.
        let mut never = |_: &str| -> Result<String> { panic!("no chunk waits") };
        assert_eq!(summarize_next(&mut db, &mut never, 6).unwrap(), None);
    }

    #[test]
    fn a_failed_call_caches_nothing() {
        let mut db = page_db();
        let mut failing = |_: &str| -> Result<String> { Err(crate::Error::Other("timed out".into())) };
        assert!(summarize_next(&mut db, &mut failing, 5).is_err());
        assert!(!next_batch(&db).unwrap().is_empty(), "still waiting");
    }
}

//! The library inbox (Ways of Working, docs-system "Ingest"; knowledge-core
//! decisions 9 to 14). An ingest runs as soon as a source is dropped in, and
//! nobody confirms it.
//!
//! **The note.** A source dropped in `Research/Ingestion/Raw/` is read and
//! written up as one dated note from `Templates/Ingested-note.md` (Summary,
//! Key Takeaways, What It Overturns, Contradictions, What Was Said, Rulings,
//! Actions, Open Questions, as that kind of source needs), straight into its
//! kind's folder. Claude reads it against what the knowledge base and the
//! code say on the source's subject, found by the real ranker ([`context`]).
//! The source is claimed as its note is written: it moves beside the note at
//! once ([`claim`]), so a second machine never reads it again.
//!
//! **What follows** ([`follow_ups`]): a plan names the pages the note changes
//! and the new pages it calls for. Each change is one section of a page,
//! replaced ([`replace_section`]); it is written at once, or held for a person
//! when it rewrites more than a fifth of the page, lands on a page that
//! changed during the run, touches a Rules page, or goes against a ruling or
//! the code ([`hold`]), and when the page holds a person's work not yet
//! committed. A new page starts from its section's blank and joins
//! its section index. Rulings wait as ready entries for the decisions log,
//! one click each; actions wait as drafted tickets; contradictions and open
//! questions become findings ([`crate::findings`]).
//!
//! **Git is the undo.** Each ingest is one commit in the knowledge base
//! ([`finish_ingest`]): the note, the source's move, every page it changed,
//! the section indexes and its `log.md` line, named path by path. Ken never
//! pushes and keeps no undo records of its own.
//!
//! [`run_pass`] reads every new source in turn (a recording is transcribed
//! first); one that fails is recorded and passed over. The model call is
//! passed in (`generate`), so the app runs Claude headless and a test a
//! stand-in. It is never the local model.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::db::Db;
use crate::{Error, Result};

pub const RAW: &str = "Research/Ingestion/Raw";
pub const INGESTED: &str = "Research/Ingestion/Ingested";
pub const TEMPLATE: &str = "Templates/Ingested-note.md";
pub const REVIEW_KIND: &str = "ingest";

/// The method's note template (the bundled wiki template's copy), used when
/// the library has none of its own.
const DEFAULT_TEMPLATE: &str = include_str!("../templates/wiki/Templates/Ingested-note.md");

/// The template a note is written from: the library's own, unless it is
/// missing or older than the headings the card and the follow-ups read (a
/// wiki made before `## Open Questions` was in it), then the bundled one.
pub fn note_template(root: &Path) -> String {
    match fs::read_to_string(root.join(TEMPLATE)) {
        Ok(own) if own.contains("## Summary") && own.contains("## Open Questions") => own,
        _ => DEFAULT_TEMPLATE.to_string(),
    }
}

/// The Review-store kind of a source that could not be read: one open item
/// per source in `Raw/`, its body the reason. Resolved when the source is
/// read, retried or removed.
pub const FAILED_KIND: &str = "ingest-failed";

/// The open failure recorded for `raw`, if any.
pub fn failure_of(db: &Db, raw: &str) -> Result<Option<crate::db::ReviewItemRow>> {
    Ok(db.list_open_review_items()?.into_iter().find(|it| it.kind == FAILED_KIND && it.source_ref == raw))
}

/// Every source with an open failure, and why.
pub fn failures(db: &Db) -> Result<Vec<(String, String)>> {
    Ok(db
        .list_open_review_items()?
        .into_iter()
        .filter(|it| it.kind == FAILED_KIND)
        .map(|it| (it.source_ref, it.body))
        .collect())
}

/// Record that `raw` could not be read, unless that is already recorded.
pub fn record_failure(db: &mut Db, raw: &str, reason: &str, now: i64) -> Result<()> {
    if failure_of(db, raw)?.is_some() {
        return Ok(());
    }
    let name = raw.rsplit('/').next().unwrap_or(raw);
    db.insert_review_item(FAILED_KIND, &format!("Could not ingest {name}"), &format!("It stays in Raw/. {reason}"), raw, None, now)?;
    Ok(())
}

/// Clear `raw`'s failures: it was read, is to be tried again, or left Raw.
/// Returns how many were open.
pub fn resolve_failure(db: &mut Db, raw: &str, now: i64) -> Result<usize> {
    let open: Vec<i64> = db
        .list_open_review_items()?
        .into_iter()
        .filter(|it| it.kind == FAILED_KIND && it.source_ref == raw)
        .map(|it| it.id)
        .collect();
    for id in &open {
        db.resolve_review_item(*id, now)?;
    }
    Ok(open.len())
}

/// What a source was, which decides its folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    Meeting,
    Recording,
    Document,
    /// Written in Ken's Ingest screen (Write a note).
    Note,
    /// A chat with Ken, sent to Ingest.
    Session,
}

const KINDS: [SourceKind; 5] = [SourceKind::Meeting, SourceKind::Recording, SourceKind::Document, SourceKind::Note, SourceKind::Session];

impl SourceKind {
    pub fn folder(self) -> &'static str {
        match self {
            SourceKind::Meeting => "Meetings",
            SourceKind::Recording => "Recordings",
            SourceKind::Document => "Documents",
            SourceKind::Note => "Notes",
            SourceKind::Session => "Sessions",
        }
    }

    fn parse(k: &str) -> Option<SourceKind> {
        let k = k.trim().trim_matches('"').to_lowercase();
        Some(match k.as_str() {
            k if k.starts_with("meeting") => SourceKind::Meeting,
            k if k.starts_with("recording") => SourceKind::Recording,
            k if k.starts_with("document") => SourceKind::Document,
            k if k.starts_with("note") => SourceKind::Note,
            k if k.starts_with("session") => SourceKind::Session,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::Meeting => "meeting",
            SourceKind::Recording => "recording",
            SourceKind::Document => "document",
            SourceKind::Note => "note",
            SourceKind::Session => "session",
        }
    }
}

/// The kind a source says it is in its own frontmatter (`kind: note`, as
/// Ken writes a note, a chat or a recording into the inbox).
pub fn declared_kind(source_text: &str) -> Option<SourceKind> {
    let mut lines = source_text.lines();
    if lines.next()?.trim_end() != "---" {
        return None;
    }
    lines
        .take_while(|l| l.trim_end() != "---")
        .find_map(|l| l.trim().strip_prefix("kind:"))
        .and_then(SourceKind::parse)
}

/// Where one ingested source ends up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Placement {
    /// Where it was dropped (`Research/Ingestion/Raw/standup.txt`).
    pub raw: String,
    /// The note (`…/Ingested/Meetings/2026-09/2026-09-24-standup.md`).
    pub note: String,
    /// Where the source goes as its note is written, beside the note and
    /// named for it (`…/2026-09/2026-09-24-standup/2026-09-24-standup.source.txt`).
    /// A card from before keeps the source's own name
    /// (`…/2026-09-24-standup/standup.txt`).
    pub source: String,
}

/// What an ingest card carries: where things went, the pages the ingest
/// wrote, and the commit that holds it all. Git is the undo (decision 12):
/// there are no undo records. A card from before keeps reading; what it
/// recorded for its own undo is ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Card {
    #[serde(flatten)]
    pub placement: Placement,
    /// The pages written from the note, in the order they were written.
    #[serde(default)]
    pub pages: Vec<PageWritten>,
    /// The knowledge base's commit holding the ingest (short sha). None when
    /// it is not a git repo, nothing was committed, or the commit failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// Why the commit failed, when it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_error: Option<String>,
}

/// One page an ingest wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageWritten {
    /// The page, relative to the knowledge base.
    pub path: String,
    /// The change in a line, or the new page's purpose.
    #[serde(default)]
    pub label: String,
    /// A new page, not an edit.
    #[serde(default)]
    pub new: bool,
}

/// Whether this project has the inbox at all.
pub fn has_inbox(root: &Path) -> bool {
    root.join(RAW).is_dir()
}

/// Files in `Raw/`, oldest name first: every file there except dotfiles and
/// an index page. Includes sources an older card still holds; see
/// [`waiting_new`].
pub fn waiting(root: &Path) -> Vec<String> {
    let names: Vec<String> = fs::read_dir(root.join(RAW))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_file())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| !n.starts_with('.') && !n.eq_ignore_ascii_case("index.md") && !n.eq_ignore_ascii_case("readme.md"))
        .collect();
    let split = |n: &str| n.rsplit_once('.').map(|(s, e)| (s.to_lowercase(), e.to_lowercase())).unwrap_or((n.to_lowercase(), String::new()));
    const MEDIA: [&str; 15] = ["mp4", "mov", "m4v", "avi", "m4a", "aac", "mp3", "wav", "flac", "ogg", "oga", "opus", "webm", "mkv", "wma"];
    // A transcript named like a recording beside it is that recording's
    // transcript: one source, read once, not two notes of one meeting.
    let media: std::collections::HashSet<String> =
        names.iter().map(|n| split(n)).filter(|(_, e)| MEDIA.contains(&e.as_str())).map(|(s, _)| s).collect();
    let mut out: Vec<String> = names
        .into_iter()
        .filter(|n| {
            let (stem, ext) = split(n);
            !((ext == "vtt" || ext == "srt") && media.contains(&stem))
        })
        .map(|n| format!("{RAW}/{n}"))
        .collect();
    out.sort();
    out
}

/// Sources still in `Raw/` that an open card already holds: read before a
/// source was claimed as its note was written, and waiting for Seen to move
/// it. A source read now never waits there.
pub fn in_review(root: &Path, db: &Db) -> Result<Vec<String>> {
    Ok(db
        .list_open_review_items()?
        .into_iter()
        .filter(|it| it.kind == REVIEW_KIND)
        .filter_map(|it| card_of(it.payload.as_deref()))
        .map(|c| c.placement.raw)
        .filter(|raw| root.join(raw).is_file())
        .collect())
}

/// Sources in `Raw/` not yet processed: [`waiting`] less those in review.
pub fn waiting_new(root: &Path, db: &Db) -> Result<Vec<String>> {
    let reviewing = in_review(root, db)?;
    Ok(waiting(root).into_iter().filter(|r| !reviewing.contains(r)).collect())
}

fn slug(name: &str) -> String {
    let s: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let s = s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-");
    if s.is_empty() { "source".into() } else { s }
}

/// Whether a name holds a date (`2026-10-14`) anywhere.
fn has_date(name: &str) -> bool {
    name.as_bytes()
        .windows(10)
        .any(|w| w.iter().enumerate().all(|(i, c)| if i == 4 || i == 7 { *c == b'-' } else { c.is_ascii_digit() }))
}

/// Where a source of `kind` goes on `date` (`YYYY-MM-DD`): its kind's
/// folder, then the month, not colliding with an earlier one. The note is
/// named for the day it was read, unless the source's name has a date of
/// its own (`meeting-2026-10-14`): then that name is enough, so a name
/// never carries two dates. The source moves into a folder of the note's
/// name as `<note>.source.<ext>`: never the note's link name, even when the
/// source kept its own name for the note (`meeting-2026-10-14.md` beside
/// `meeting-2026-10-14.md` made `[[meeting-2026-10-14]]` two pages), nor
/// another source's of the same name. A card from before keeps the path it
/// recorded, so its source is found where it was moved.
pub fn place(root: &Path, raw: &str, date: &str, kind: SourceKind) -> Placement {
    let file = raw.rsplit('/').next().unwrap_or(raw).to_string();
    let (stem, ext) = file.rsplit_once('.').map_or((file.as_str(), None), |(s, e)| (s, Some(e)));
    let month = date.get(..7).unwrap_or(date);
    let dir = format!("{INGESTED}/{}/{month}", kind.folder());
    let name = if has_date(&slug(stem)) { slug(stem) } else { format!("{date}-{}", slug(stem)) };
    let mut base = name.clone();
    let mut n = 2;
    while root.join(&dir).join(format!("{base}.md")).exists() || root.join(&dir).join(&base).exists() {
        base = format!("{name}-{n}");
        n += 1;
    }
    Placement {
        raw: raw.to_string(),
        note: format!("{dir}/{base}.md"),
        source: match ext {
            Some(ext) => format!("{dir}/{base}/{base}.source.{ext}"),
            None => format!("{dir}/{base}/{base}.source"),
        },
    }
}

/// The kind a note's folder says (`…/Ingested/Meetings/2026-09/…` is a
/// meeting): how a written note's kind is read, so the note carries no
/// `kind:` key of its own (decision 5's keys).
pub fn kind_at(note_path: &str) -> Option<SourceKind> {
    let rest = note_path.strip_prefix(INGESTED)?.strip_prefix('/')?;
    let folder = rest.split('/').next()?;
    KINDS.into_iter().find(|k| k.folder().eq_ignore_ascii_case(folder))
}

/// What kind of source a reply is the note of, before it has a folder: a
/// source with people present is a meeting; else a transcript or a recording
/// is a recording, and anything else a document. The source's own
/// `kind:` ([`declared_kind`]) comes first, and is the caller's to check.
pub fn kind_of(note: &str, raw: &str) -> SourceKind {
    if !present_of(note).is_empty() {
        return SourceKind::Meeting;
    }
    let ext = raw.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
    if matches!(ext.as_str(), "vtt" | "srt") || crate::extract::FileKind::from_path(Path::new(raw)) == crate::extract::FileKind::Video {
        SourceKind::Recording
    } else {
        SourceKind::Document
    }
}

/// Who was there, from the note's `Present:` line under its title (or a
/// `present:` key in an older note's frontmatter). Empty when it does not
/// say, or still holds the template's placeholder.
pub fn present_of(note: &str) -> Vec<String> {
    note.lines()
        .take_while(|l| !l.starts_with("## "))
        .find_map(|l| {
            let t = l.trim();
            t.get(..8).filter(|k| k.eq_ignore_ascii_case("present:")).map(|_| t[8..].trim().trim_end_matches('.').to_string())
        })
        // The placeholder is read whole: it holds a comma of its own.
        .filter(|v| !v.contains("{{"))
        .map(|v| {
            v.trim_matches(|c| c == '[' || c == ']')
                .split(',')
                .map(|s| s.trim().trim_matches('"').to_string())
                .filter(|s| !s.is_empty() && !s.contains("{{"))
                .collect()
        })
        .unwrap_or_default()
}

/// The prompt: the template, the source's text, and the rules the method
/// sets for a note (quotes verbatim, evidence not doctrine, what it
/// overturns first).
pub fn prompt(template: &str, raw: &str, text: &str, date: &str) -> String {
    prompt_against(template, raw, text, date, "")
}

/// [`prompt`], with what the knowledge base and the code say now about the
/// source's subject (`context`, from [`context`]), so the note can say what
/// the source overturns and what it contradicts.
pub fn prompt_against(template: &str, raw: &str, text: &str, date: &str, context: &str) -> String {
    let file = raw.rsplit('/').next().unwrap_or(raw);
    let context = if context.trim().is_empty() {
        String::new()
    } else {
        format!(
            "WHAT THE KNOWLEDGE BASE AND THE CODE SAY NOW (passages Ken's search found on the source's subject, each \
             headed by where it is and its label; name a page, ruling, ticket or file as its heading does):\n{context}\n\n"
        )
    };
    format!(
        "You are writing up one source from a team's library inbox as one dated note. What the note \
         calls for is written from it at once, with nobody to confirm it, so say only what the source shows.\n\n\
         Fill in this template. Replace every {{{{…}}}} placeholder. Keep the sections this kind of source \
         needs and drop the rest, as the template says. Today is {date}. The source file is `{file}`.\n\n\
         Rules:\n\
         - Quotes are the speaker's exact words, typos included, marked [sic]. Never paraphrase inside quotes.\n\
         - This is evidence at the time, not current doctrine. Do not state anything the source does not.\n\
         - \"Summary\": what the source is and what it comes to, in a few sentences. \"Key Takeaways\": one line \
           per fact, decision or change the team should know.\n\
         - Lead \"What It Overturns\" with the one thing that changes what the team wrote, and name the page.\n\
         - \"Contradictions\": one line per place the source disagrees with the knowledge base, a ruling, a ticket \
           or the code, as the passages below show, or with itself: `` - `what it contradicts`: how they differ — \
           \"the source's words\" ``. What it contradicts is a page path, a ruling id (D-nnn), a ticket id, a code \
           file as its heading names it, or `source` when the source disagrees with itself.\n\
         - A section with nothing in it is left out, heading and all. Never write that something is absent, that \
           there are none, or what the source does not touch.\n\
         - \"Rulings\": one line per ruling made, `- the ruling, in the words it was made in — who made it`, the \
           name left out when the source does not say.\n\
         - \"Actions\": one line per piece of work someone asked for, `- the action → who it is for`, the arrow \
           left out when the source does not say.\n\
         - \"Open Questions\": one line per question the source leaves open, `- the question`.\n\
         - Name who said what. List who was present if the source shows it.\n\
         - Reply with the finished note only, in Markdown, starting with its `---` frontmatter. No preamble, no code fences.\n\n\
         TEMPLATE:\n{template}\n\n{context}SOURCE ({file}):\n{text}\n",
    )
}

/// A repo of the workspace whose code a source is checked against
/// (decision 10), with its index opened for reading.
pub struct CodeSource {
    /// The repo's name, as a citation names it (`game:src/save.rs:12`).
    pub name: String,
    pub db: Db,
}

/// A source's subject in a few words: its frontmatter title, else its first
/// heading, else its file name.
pub fn subject(raw: &str, text: &str) -> String {
    let front = text.strip_prefix("---").and_then(|rest| {
        rest.lines()
            .take_while(|l| l.trim_end() != "---")
            .find_map(|l| l.trim().strip_prefix("title:").map(|t| t.trim().trim_matches('"').to_string()))
    });
    let heading = || text.lines().find_map(|l| l.strip_prefix("# ")).map(|h| h.trim().to_string());
    let name = || {
        let file = raw.rsplit('/').next().unwrap_or(raw);
        let stem = file.rsplit_once('.').map_or(file, |(s, _)| s);
        stem.replace(['_', '-', '.'], " ").split_whitespace().collect::<Vec<_>>().join(" ")
    };
    front.filter(|t| !t.is_empty()).or_else(heading).filter(|t| !t.is_empty()).unwrap_or_else(name)
}

/// The words a source keeps coming back to: those the ranker itself keeps
/// from a question (`db::significant_tokens`), said at least twice, the most
/// said first, at most eight.
pub fn key_terms(text: &str) -> Vec<String> {
    let mut end = text.len().min(60_000);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut counts: HashMap<String, usize> = HashMap::new();
    for w in crate::db::significant_tokens(&text[..end]) {
        if w.chars().count() >= 3 && !w.chars().all(|c| c.is_ascii_digit()) {
            *counts.entry(w.to_lowercase()).or_default() += 1;
        }
    }
    let mut words: Vec<(String, usize)> = counts.into_iter().filter(|(_, n)| *n > 1).collect();
    words.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    words.into_iter().take(8).map(|(w, _)| w).collect()
}

/// Names a source writes like code (`save_world`, `saveWorld`, a name in
/// backticks), at most ten: each is looked up in the code index.
pub fn mentioned_names(text: &str) -> Vec<String> {
    let ident = |s: &str| !s.is_empty() && s.len() <= 60 && s.chars().all(|c| c.is_alphanumeric() || c == '_');
    let mut out: Vec<String> = Vec::new();
    let mut add = |s: &str| {
        if !out.iter().any(|o| o == s) {
            out.push(s.to_string());
        }
    };
    for span in text.split('`').skip(1).step_by(2) {
        if ident(span.trim()) {
            add(span.trim());
        }
    }
    for w in text.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
        let snake = w.contains('_') && w.trim_matches('_').contains('_') && w.len() > 3;
        let camel = w.chars().any(|c| c.is_lowercase()) && w.chars().skip(1).any(|c| c.is_uppercase());
        if (snake || camel) && ident(w) {
            add(w);
        }
    }
    out.truncate(10);
    out
}

/// Pages of the knowledge base that are not what a source is checked
/// against: the inbox and its notes, and the blanks.
fn context_page(path: &str) -> bool {
    !path.starts_with("Research/Ingestion") && !path.starts_with("Templates/") && crate::chunker::is_markdown(path)
}

/// What the knowledge base and the code say about a source's subject
/// (decisions 9 and 10): the passages the real ranker finds for its subject
/// and its key terms, with the workspace's vocabulary, in the knowledge
/// base's pages, decisions log and tickets (a few per page, each headed by
/// its path and label), then the code hits and the definitions of the names
/// the source mentions in every other repo (`code`). Given to the note and
/// to the plan, so they can say what the source overturns and contradicts.
pub fn context(db: &Db, code: &[CodeSource], raw: &str, text: &str) -> String {
    let title = subject(raw, text);
    let terms = key_terms(text).join(" ");
    let vocab = crate::routing::workspace_vocabulary(std::iter::once(db).chain(code.iter().map(|c| &c.db)));
    let search = |db: &Db, q: &str, limit: usize| {
        if q.trim().is_empty() {
            Vec::new()
        } else {
            crate::routing::search_member_with(db, q, None, limit, Some(&vocab)).unwrap_or_default()
        }
    };
    let mut out = String::new();
    let mut seen: Vec<i64> = Vec::new();
    let mut pages: Vec<String> = Vec::new();
    for hit in search(db, &title, 12).into_iter().chain(search(db, &terms, 24)) {
        if !context_page(&hit.path) || seen.contains(&hit.chunk_id) {
            continue;
        }
        if pages.iter().filter(|p| **p == hit.path).count() >= 2 || (!pages.contains(&hit.path) && pages.len() >= 8) {
            continue;
        }
        seen.push(hit.chunk_id);
        pages.push(hit.path.clone());
        let label = hit.label.as_deref().map(|l| format!(" · {l}")).unwrap_or_default();
        let excerpt: String = hit.snippet.chars().take(700).collect();
        out.push_str(&format!("\n=== {}{label} ===\n{}\n", hit.path, excerpt.trim()));
    }
    let code_query = if terms.is_empty() { title.clone() } else { terms.clone() };
    let names = mentioned_names(text);
    let mut code_hits = 0;
    let mut defs = 0;
    for src in code {
        let mut here = 0;
        for hit in search(&src.db, &code_query, 12) {
            if code_hits >= 10 || here >= 4 {
                break;
            }
            if crate::contenttype::of(&hit.path) != crate::contenttype::ContentType::Code {
                continue;
            }
            here += 1;
            code_hits += 1;
            let at = hit.line.map(|l| format!(":{l}")).unwrap_or_default();
            let symbol = hit.symbol.as_deref().map(|s| format!(" · `{s}`")).unwrap_or_default();
            let label = hit.label.as_deref().map(|l| format!(" · {l}")).unwrap_or_default();
            let excerpt: String = hit.snippet.chars().take(500).collect();
            out.push_str(&format!("\n=== {}:{}{at}{symbol}{label} ===\n{}\n", src.name, hit.path, excerpt.trim()));
        }
        for name in &names {
            for d in crate::codeindex::definitions_named(&src.db, name, 2).unwrap_or_default() {
                if defs >= 10 {
                    break;
                }
                defs += 1;
                let docs = d.docs.as_deref().unwrap_or("").trim();
                out.push_str(&format!("\n=== {}:{}:{} · defines `{}` ({}) ===\n{docs}\n", src.name, d.path, d.line, d.name, d.kind));
            }
        }
    }
    out
}

/// The model's reply as a note: fences stripped, and the frontmatter
/// `sources:` set to where the source is filed (a `[[link]]` to it; the
/// older `source:` key is dropped). A `kind:` key is dropped too: the note's
/// folder says its kind ([`kind_at`]).
pub fn finish_note(reply: &str, placement: &Placement) -> Result<String> {
    let t = strip_fences(reply);
    if !t.starts_with("---") {
        return Err(Error::Other("the note did not start with its frontmatter".into()));
    }
    let mut out = String::with_capacity(t.len() + 64);
    let mut in_fm = false;
    let mut fm_done = false;
    // Inside a `sources:` block list the model wrote: its items are dropped.
    let mut in_sources = false;
    for (i, line) in t.lines().enumerate() {
        if line.trim_end() == "---" {
            if i == 0 {
                in_fm = true;
            } else if in_fm && !fm_done {
                out.push_str(&format!("sources:\n  - \"[[{}]]\"\n", placement.source));
                in_fm = false;
                fm_done = true;
            }
            out.push_str("---\n");
            continue;
        }
        if in_fm {
            let trimmed = line.trim_start();
            if in_sources && (trimmed.starts_with("- ") || trimmed == "-") {
                continue;
            }
            in_sources = false;
            if let Some(rest) = trimmed.strip_prefix("sources:").or_else(|| trimmed.strip_prefix("source:")) {
                in_sources = rest.trim().is_empty();
                continue;
            }
            if trimmed.starts_with("kind:") {
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    if !fm_done {
        return Err(Error::Other("the note's frontmatter was never closed".into()));
    }
    Ok(out)
}

fn strip_fences(reply: &str) -> &str {
    let mut t = reply.trim();
    if let Some(rest) = t.strip_prefix("```markdown").or_else(|| t.strip_prefix("```md")).or_else(|| t.strip_prefix("```json")).or_else(|| t.strip_prefix("```")) {
        t = rest.trim_start();
        t = t.strip_suffix("```").unwrap_or(t).trim_end();
    }
    t
}

/// The card's body: the note's summary, what it overturns, contradictions,
/// rulings and open questions, which are what a person reads to find a wrong
/// write.
pub fn card_body(note: &str, placement: &Placement) -> String {
    let mut s = String::new();
    for names in [SUMMARY, OVERTURNS, CONTRADICTIONS, RULINGS, QUESTIONS] {
        if let Some((heading, body)) = section(note, names) {
            s.push_str(&format!("## {heading}\n\n{}\n\n", body.trim()));
        }
    }
    s.push_str(&format!(
        "Note: {}\nWhat it changed, what it found and what waits are on its card on the Ingest screen. The ingest is one \
         commit in the knowledge base, so git takes it back.\n",
        placement.note
    ));
    s
}

/// The most of a source's text one read sends (about 100k tokens). Claude
/// Code refuses a prompt past its window outright, so a longer source is
/// read from its start, and the note says so.
pub const MAX_SOURCE_CHARS: usize = 400_000;

/// `text` within [`MAX_SOURCE_CHARS`], cut at a line where it can be.
pub fn cap_source(text: String) -> String {
    if text.len() <= MAX_SOURCE_CHARS {
        return text;
    }
    let mut end = MAX_SOURCE_CHARS;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let cut = text[..end].rfind('\n').filter(|n| *n > end / 2).unwrap_or(end);
    format!(
        "{}\n\n[Ken sent the first {} of {} characters of this source. Say in the note's Summary that it covers the start of the source only.]",
        &text[..cut],
        cut,
        text.len()
    )
}

/// One source read: where it went, and what the knowledge base and the code
/// said on its subject (for the plan).
#[derive(Debug, Clone)]
pub struct Read {
    pub placement: Placement,
    pub context: String,
}

/// Read one new source, write its note into its kind's folder, claim the
/// source (it moves beside the note at once) and file the card. `code` is
/// the workspace's other repos, to check the source against. `generate`
/// turns the prompt into the note.
pub fn ingest_one(
    root: &Path,
    db: &mut Db,
    code: &[CodeSource],
    raw: &str,
    date: &str,
    now: i64,
    generate: impl FnOnce(&str) -> Result<String>,
) -> Result<Read> {
    let abs = root.join(raw);
    let text = crate::extract::extract(&abs)?.text;
    if text.trim().is_empty() {
        return Err(Error::Other(format!("{raw} has no text Ken can read (a recording needs its transcript first)")));
    }
    let text = cap_source(text);
    let template = note_template(root);
    let declared = declared_kind(&text);
    let context = context(db, code, raw, &text);
    let reply = generate(&prompt_against(&template, raw, &text, date, &context))?;
    let kind = declared.unwrap_or_else(|| kind_of(strip_fences(&reply), raw));
    let placement = place(root, raw, date, kind);
    let note = finish_note(&reply, &placement)?;

    let note_abs = root.join(&placement.note);
    if let Some(dir) = note_abs.parent() {
        fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
    }
    fs::write(&note_abs, &note).map_err(|e| Error::io(&note_abs, e))?;
    // Claimed as the note is written: a note whose source could not move is
    // taken back, so the source is read again rather than twice.
    if let Err(e) = claim(root, &placement) {
        let _ = fs::remove_file(&note_abs);
        return Err(e);
    }

    let stem = note_stem(&placement.note);
    let card = Card { placement: placement.clone(), pages: Vec::new(), commit: None, commit_error: None };
    let payload = serde_json::to_string(&card).map_err(|e| Error::Other(e.to_string()))?;
    db.insert_review_item(REVIEW_KIND, &format!("Ingested: {stem}"), &card_body(&note, &placement), &placement.note, Some(&payload), now)?;
    Ok(Read { placement, context })
}

/// Claim a source (decision 12): it moves from `Raw/` beside its note, with
/// a recording's generated transcript. Already beside its note is already
/// claimed.
pub fn claim(root: &Path, placement: &Placement) -> Result<()> {
    let raw = root.join(&placement.raw);
    let dest = root.join(&placement.source);
    if !raw.exists() {
        return if dest.exists() { Ok(()) } else { Err(Error::Other(format!("{} is no longer in Raw", placement.raw))) };
    }
    if let Some(dir) = dest.parent() {
        fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
    }
    fs::rename(&raw, &dest).map_err(|e| Error::io(&raw, e))?;
    // A recording's generated transcript is keyed by its path: it moves too.
    crate::transcript::move_cached(root, &placement.raw, &placement.source);
    Ok(())
}

/// A note's file name without `.md`, as a `[[link]]` names it.
fn note_stem(note: &str) -> String {
    note.rsplit('/').next().unwrap_or(note).trim_end_matches(".md").to_string()
}

// --- One pass over the inbox: what the app runs when a source lands. -----

/// Why a model call failed: for every source alike (Claude Code missing,
/// not logged in, timed out), which stops the pass, or for this source
/// only, which the pass records and moves past.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallError {
    Stop(String),
    Source(String),
}

/// A Claude one-shot's outcome as a pass reads it.
pub fn call_result(out: Result<crate::assistant::OneshotOutcome>) -> std::result::Result<String, CallError> {
    use crate::assistant::OneshotOutcome as O;
    match out {
        // The CLI could not be started at all.
        Err(e) => Err(CallError::Stop(e.to_string())),
        Ok(O::Completed(text)) => Ok(text),
        Ok(O::TimedOut) => Err(CallError::Stop("Claude Code did not answer within ten minutes.".into())),
        Ok(O::Cancelled) => Err(CallError::Stop("The read was cancelled.".into())),
        Ok(O::Failed(d)) if cli_failed(&d) => Err(CallError::Stop(d)),
        Ok(O::Failed(d)) => Err(CallError::Source(d)),
    }
}

/// Whether a failed run's detail says the CLI itself cannot work (not
/// installed, not logged in, out of credit), so every source would fail.
pub fn cli_failed(detail: &str) -> bool {
    if detail == crate::runner::MISSING_CLAUDE_HELP {
        return true;
    }
    let d = detail.to_lowercase();
    ["not logged in", "/login", "invalid api key", "authentication_error", "oauth token", "credit balance is too low"]
        .iter()
        .any(|p| d.contains(p))
}

/// Where a pass is with the source it is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassPhase {
    /// Making the transcript of a recording (Whisper's percent).
    Transcribing(u8),
    /// The model is reading it.
    Reading,
    /// Its note is written and the source claimed; the pages it changes are
    /// being written, then committed.
    Writing,
}

/// What one pass did.
#[derive(Debug, Clone, Default)]
pub struct PassReport {
    pub read: Vec<Placement>,
    /// Sources that could not be read, and why. Each stays in `Raw/` with a
    /// failure recorded ([`FAILED_KIND`]).
    pub failed: Vec<(String, String)>,
    /// Recordings that wait for something Ken cannot make here (a model,
    /// ffmpeg), and what.
    pub waiting: Vec<(String, String)>,
    /// Why the pass stopped before the end, when the CLI itself failed.
    pub stopped: Option<String>,
}

/// What a pass transcribes recordings with: the Whisper model and ffmpeg,
/// each when there is one.
#[derive(Debug, Clone, Copy, Default)]
pub struct Transcriber<'a> {
    pub model: Option<&'a Path>,
    pub ffmpeg: Option<&'a Path>,
}

/// Whether a source in the inbox is a recording with no transcript yet.
pub fn needs_transcript(root: &Path, raw: &str) -> bool {
    crate::extract::FileKind::from_path(Path::new(raw)) == crate::extract::FileKind::Video
        && !crate::transcript::has_transcript(root, raw)
}

/// Read every new source in `Raw/`, one at a time: a recording is
/// transcribed first; each source becomes its note, is claimed, and what
/// follows from it is written and committed ([`ingest_one`],
/// [`follow_ups`], [`finish_ingest`]). `code` is the workspace's other
/// repos, which each source is checked against. A source that fails is
/// recorded and passed over, and a recording that needs a model or ffmpeg
/// waits; the pass stops only when the CLI itself fails
/// ([`CallError::Stop`]). Sources with a failure recorded are left until
/// someone tries them again ([`resolve_failure`]).
#[allow(clippy::too_many_arguments)]
pub fn run_pass(
    root: &Path,
    db: &mut Db,
    code: &[CodeSource],
    today: &str,
    now: impl Fn() -> i64,
    transcriber: Transcriber,
    mut call: impl FnMut(&str) -> std::result::Result<String, CallError>,
    on_phase: &std::sync::Arc<dyn Fn(&str, PassPhase) + Send + Sync>,
) -> Result<PassReport> {
    // Failed sources are left alone until tried again, or until the file
    // changes after it failed (a fixed copy dropped over it).
    let failed_before: Vec<(String, i64, i64)> = db
        .list_open_review_items()?
        .into_iter()
        .filter(|it| it.kind == FAILED_KIND)
        .map(|it| (it.source_ref, it.id, it.created_at))
        .collect();
    let mut report = PassReport::default();
    for raw in waiting_new(root, db)? {
        if let Some((_, id, at)) = failed_before.iter().find(|(r, _, _)| *r == raw) {
            let changed = fs::metadata(root.join(&raw))
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .is_some_and(|d| d.as_secs() as i64 > *at);
            if !changed {
                continue;
            }
            db.resolve_review_item(*id, now())?;
        }
        if needs_transcript(root, &raw) {
            let abs = root.join(&raw);
            if let Some(why) = crate::transcript::waiting_reason(&abs, transcriber.ffmpeg.is_some(), transcriber.model.is_some()) {
                report.waiting.push((raw, why));
                continue;
            }
            on_phase(&raw, PassPhase::Transcribing(0));
            let progress: crate::transcript::ProgressFn = {
                let (f, raw) = (on_phase.clone(), raw.clone());
                std::sync::Arc::new(move |p| {
                    if let crate::transcript::TranscriptPhase::Transcribing(pct) = p {
                        f(&raw, PassPhase::Transcribing(pct));
                    }
                })
            };
            let model = transcriber.model.unwrap_or(Path::new(""));
            if let Err(e) = crate::transcript::generate_and_cache_with_progress(transcriber.ffmpeg, model, root, &raw, progress) {
                let why = format!("Could not transcribe it: {e}");
                record_failure(db, &raw, &why, now())?;
                report.failed.push((raw, why));
                continue;
            }
        }
        on_phase(&raw, PassPhase::Reading);
        let mut stop: Option<String> = None;
        let done = ingest_one(root, db, code, &raw, today, now(), |p| match call(p) {
            Ok(text) => Ok(text),
            Err(CallError::Stop(m)) => {
                stop = Some(m.clone());
                Err(Error::Other(m))
            }
            Err(CallError::Source(m)) => Err(Error::Other(m)),
        });
        match done {
            Ok(read) => {
                resolve_failure(db, &raw, now())?;
                on_phase(&raw, PassPhase::Writing);
                let note = fs::read_to_string(root.join(&read.placement.note)).unwrap_or_default();
                let followed = follow_ups(root, db, &read.placement, &note, &read.context, today, now(), |p| match call(p) {
                    Ok(text) => Ok(text),
                    Err(CallError::Stop(m)) => {
                        stop = Some(m.clone());
                        Err(Error::Other(m))
                    }
                    Err(CallError::Source(m)) => Err(Error::Other(m)),
                });
                let followed = followed.unwrap_or_else(|e| {
                    eprintln!("warning: ingest follow-ups for {raw} failed: {e}");
                    FollowUps::default()
                });
                // The note and the claim are committed whatever the follow-ups did.
                if let Err(e) = finish_ingest(root, db, &read.placement, &note, &followed, today) {
                    eprintln!("warning: ingest of {raw} could not be finished: {e}");
                }
                report.read.push(read.placement);
                if let Some(m) = stop {
                    report.stopped = Some(m);
                    break;
                }
            }
            Err(_) if stop.is_some() => {
                report.stopped = stop;
                break;
            }
            Err(e) => {
                record_failure(db, &raw, &e.to_string(), now())?;
                report.failed.push((raw, e.to_string()));
            }
        }
    }
    Ok(report)
}

// --- Sources Ken writes into the inbox itself: a note written on the
// Ingest screen, and a chat sent to it. -----------------------------------

/// Where a file named `name` goes in a library's Raw/: its own name, or with
/// a number when that is taken. The folder is made if missing. The app's
/// drops and the MCP `add_source` both land here.
pub fn raw_target(root: &Path, name: &str) -> Result<std::path::PathBuf> {
    let raw_dir = root.join(RAW);
    fs::create_dir_all(&raw_dir).map_err(|e| Error::io(&raw_dir, e))?;
    // A dropped name is only a name: no folders, nothing above Raw.
    let name = name.rsplit(['/', '\\']).next().unwrap_or(name).trim();
    if name.is_empty() || name == "." || name == ".." {
        return Err(Error::Other("a file with no name".into()));
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    // Raw's own README and Index are not sources, so a source by that name
    // gets another.
    let (name, stem) = if name.eq_ignore_ascii_case("readme.md") || name.eq_ignore_ascii_case("index.md") {
        let s = format!("{stem} (source)");
        (format!("{s}{ext}"), s)
    } else {
        (name.to_string(), stem)
    };
    let mut target = raw_dir.join(&name);
    let mut n = 2;
    while target.exists() {
        target = raw_dir.join(format!("{stem} ({n}){ext}"));
        n += 1;
    }
    Ok(target)
}

/// Most characters of a ruling or an action in an Inbox card's title.
const CARD_TITLE_CHARS: usize = 90;

/// A ruling or an action as an Inbox card's title: whole when it is short,
/// else cut at the last word that fits, with an ellipsis, never mid-word
/// ("…to write one file, by").
pub fn card_title(text: &str) -> String {
    cut_at_word(text, CARD_TITLE_CHARS).0
}

/// `text` cut for a title of at most `max` characters: whole when it fits,
/// else at the last word that fits, never mid-word and never inside a
/// `code` span, with an ellipsis. The second part is what the cut left,
/// starting with an ellipsis, so a body can carry on where the title
/// stopped; it is empty when nothing was cut.
pub fn cut_at_word(text: &str, max: usize) -> (String, String) {
    let text = text.trim();
    if text.chars().count() <= max {
        return (text.to_string(), String::new());
    }
    let end = text.char_indices().nth(max).map_or(text.len(), |(i, _)| i);
    let mut at = text[..end].rfind(char::is_whitespace).unwrap_or(end);
    // An odd number of backticks before the cut leaves a code span open:
    // the cut moves to before the span, unless that leaves nothing.
    if text[..at].matches('`').count() % 2 == 1 {
        if let Some(tick) = text[..at].rfind('`') {
            let before = text[..tick].trim_end().len();
            if before > 0 {
                at = before;
            }
        }
    }
    let head = text[..at].trim_end_matches([',', ';', ':', ' ']);
    let rest = text[at..].trim_start();
    (format!("{head}…"), format!("…{rest}"))
}

/// A page as a title names it: `Current/Project.md` → "Current › Project".
pub fn page_name(path: &str) -> String {
    path.trim_end_matches(".md").split('/').filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" › ")
}

/// A name made safe for a file: no path separators or characters Windows
/// refuses, trimmed, at most 80 characters.
pub fn file_safe(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { ' ' } else { c })
        .collect();
    let one = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let short: String = one.chars().take(80).collect();
    short.trim().trim_end_matches('.').trim().to_string()
}

/// A note written on the Ingest screen, as its source file:
/// `YYYY-MM-DD HH.MM Note - <title>.md` (`stamp` is `YYYY-MM-DD HH.MM`) and
/// its text, with `kind: note` and who wrote it.
pub fn note_source(stamp: &str, title: Option<&str>, text: &str, by: Option<&str>) -> (String, String) {
    let heading = title.map(str::trim).filter(|t| !t.is_empty());
    let name = match heading.map(file_safe).filter(|t| !t.is_empty()) {
        Some(t) => format!("{stamp} Note - {t}.md"),
        None => format!("{stamp} Note.md"),
    };
    let mut doc = String::from("---\nkind: note\n");
    if let Some(by) = by.map(str::trim).filter(|b| !b.is_empty()) {
        doc.push_str(&format!("by: {}\n", yaml_str(by)));
    }
    doc.push_str(&format!("written: {}\n---\n\n", stamp.replacen('.', ":", 1)));
    if let Some(t) = heading {
        doc.push_str(&format!("# {t}\n\n"));
    }
    doc.push_str(text.trim());
    doc.push('\n');
    (name, doc)
}

/// One turn of a chat sent to Ingest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatTurn {
    /// `user` or `assistant`; anything else is left out.
    pub role: String,
    pub content: String,
}

/// A chat sent to Ingest, as its source file:
/// `YYYY-MM-DD HH.MM Chat - <chat title>.md` with `kind: session`, the chat's
/// id and who was present, and only the person's and Ken's turns, as
/// **Me** and **Ken**. `None` when no turn is left.
pub fn chat_source(stamp: &str, title: &str, chat_id: &str, me: Option<&str>, turns: &[ChatTurn]) -> Option<(String, String)> {
    let kept: Vec<&ChatTurn> = turns
        .iter()
        .filter(|t| (t.role == "user" || t.role == "assistant") && !t.content.trim().is_empty())
        .collect();
    if kept.is_empty() {
        return None;
    }
    let heading = Some(title.trim()).filter(|t| !t.is_empty()).unwrap_or("Chat");
    let name = format!("{stamp} Chat - {}.md", Some(file_safe(heading)).filter(|t| !t.is_empty()).unwrap_or_else(|| "Chat".into()));
    let me = me.map(str::trim).filter(|m| !m.is_empty()).unwrap_or("Me");
    let mut doc = format!(
        "---\nkind: session\nchat: {}\npresent: [{}, Ken]\nheld: {}\n---\n\n# {heading}\n\nA chat with Ken. **Me** is {}; **Ken** is Ken's answers.\n\n",
        yaml_str(chat_id),
        yaml_str(me),
        stamp.replacen('.', ":", 1),
        me
    );
    for t in kept {
        let who = if t.role == "user" { "Me" } else { "Ken" };
        doc.push_str(&format!("**{who}:** {}\n\n", t.content.trim()));
    }
    Some((name, doc))
}

/// A YAML string, quoted.
fn yaml_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

// --- What follows from a note (decisions 10, 11, 13 and 14): page edits
// and new pages, written at once or held; rulings as ready entries; actions
// as drafted tickets; contradictions and open questions as findings. -------

/// Sections an ingest never edits: evidence stays evidence, the blanks and
/// the wiki's own meta pages have their own ways in, the decisions log takes
/// rulings only as ready entries, and tickets come only as drafts. The RAID
/// register ([`crate::raid::RAID_PAGE`]) takes risks and issues only as
/// entries a person adds.
const OFF_LIMITS: [&str; 6] = ["Research/", "Templates/", "_meta/", "Decisions/", "tickets/", ".ken/"];

/// Rules pages: binding, so an edit to one is held for a person. `Rules/`
/// holds them; the older layout kept them in Ways-of-Working and Conventions.
const METHOD: [&str; 3] = ["Rules/", "Ways-of-Working/", "Conventions/"];

/// Where the decisions log sits in a knowledge base.
pub const DECISIONS_LOG: &str = "Decisions/DECISIONS.md";

/// The knowledge base's log: one dated line per ingest, drift run and lint
/// pass (decision 2).
pub const LOG_PAGE: &str = "log.md";

/// Most existing pages one note may change (decision 11).
pub const MAX_UPDATES: usize = 10;
/// Most new pages one note may make.
pub const MAX_NEW: usize = 10;

fn may_propose(page: &str) -> bool {
    page.ends_with(".md")
        && !page.contains("..")
        && !page.contains('\\')
        && !page.starts_with('/')
        && !OFF_LIMITS.iter().any(|p| page.starts_with(p))
        && !matches!(page, "START-HERE.md" | "CLAUDE.md" | "README.md" | LOG_PAGE | crate::raid::RAID_PAGE)
        && !page.rsplit('/').next().is_some_and(|f| f.eq_ignore_ascii_case("index.md"))
}

/// A page in Rules (or, in the older layout, Ways-of-Working or
/// Conventions): an edit to it is held for a person.
pub fn is_method_page(page: &str) -> bool {
    METHOD.iter().any(|p| page.starts_with(p))
}

/// The pages a note could change, with their titles, for the plan to choose
/// from.
fn page_list(db: &Db) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for page in db.page_paths()?.into_iter().filter(|p| may_propose(p)) {
        let title = db.page_meta(&page)?.and_then(|m| m.title).unwrap_or_default();
        out.push((page, title));
        if out.len() >= 400 {
            break;
        }
    }
    Ok(out)
}

/// The plan: which pages the note changes, which new pages it calls for,
/// which pages it retires, its rulings written up for the decisions log, the
/// risks and issues it raises for the RAID register, the renames it tells
/// of, and the topics of the note and each page it names (decision 21:
/// tagged at write time, in the same call).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    pub update: Vec<PageChange>,
    pub create: Vec<NewPage>,
    pub retire: Vec<Retire>,
    pub rulings: Vec<RulingDraft>,
    /// Something that might go wrong.
    pub risks: Vec<RaidItem>,
    /// A problem now.
    pub issues: Vec<RaidItem>,
    pub renames: Vec<Rename>,
    /// (page, topics); the note itself is [`NOTE_TOPICS`].
    pub topics: Vec<(String, Vec<String>)>,
}

/// What the plan calls the note when it gives the note's topics.
pub const NOTE_TOPICS: &str = "note";

/// A page the note makes untrue as a whole, and the page that replaces it
/// (decision 7), a path or a `[[link]]`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Retire {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub by: String,
}

/// A name the note says changed, for the Vocabulary page's Renames table
/// (decision 18).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Rename {
    #[serde(default)]
    pub old: String,
    #[serde(default)]
    pub new: String,
}

/// One page the note changes, the change in a line, and the ruling or code
/// file it goes against, if any.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct PageChange {
    pub path: String,
    #[serde(default)]
    pub change: String,
    #[serde(default)]
    pub against: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewPage {
    pub path: String,
    #[serde(default)]
    pub purpose: String,
}

/// A ruling as the plan writes it up for its entry (decision 13).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct RulingDraft {
    #[serde(default)]
    pub ruling: String,
    #[serde(default)]
    pub topic: String,
    #[serde(default)]
    pub why: String,
    #[serde(default)]
    pub quote: String,
    #[serde(default)]
    pub who: String,
    #[serde(default, deserialize_with = "list_or_string")]
    pub aliases: Vec<String>,
    #[serde(default, deserialize_with = "list_or_string")]
    pub supersedes: Vec<String>,
}

/// A risk or issue as the plan writes it up for the RAID register: a title
/// of a few words, what it is in a line, and who watches it when the note
/// says.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct RaidItem {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub owner: String,
}

/// Who a risk or issue belongs to when the plan left its owner empty: the
/// one person present (the note's `Present:` line) named in the note's
/// sentence that says most of what the item says. None when no sentence
/// shares two of its words, or that sentence names no one or several.
fn owner_from_note(note: &str, item: &RaidItem) -> Option<String> {
    let people = present_of(note);
    if people.is_empty() {
        return None;
    }
    let mut words: Vec<String> = format!("{} {}", item.title, item.detail)
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 4)
        .map(str::to_string)
        .collect();
    words.sort();
    words.dedup();
    let sentences = note
        .lines()
        .filter(|l| !l.trim_start().to_lowercase().starts_with("present"))
        .flat_map(|l| l.split(". "));
    let (shared, best) = sentences
        .map(|s| (words.iter().filter(|w| s.to_lowercase().contains(w.as_str())).count(), s))
        .max_by_key(|(n, _)| *n)?;
    if shared < 2 {
        return None;
    }
    let tokens: Vec<&str> = best.split(|c: char| !c.is_alphanumeric()).collect();
    let named: Vec<&String> =
        people.iter().filter(|p| p.split_whitespace().next().is_some_and(|first| tokens.contains(&first))).collect();
    (named.len() == 1).then(|| named[0].clone())
}

/// A list the model may write as one comma-separated string.
fn list_or_string<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum L {
        One(String),
        Many(Vec<String>),
    }
    let split = |s: String| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect::<Vec<_>>();
    Ok(match Option::<L>::deserialize(d)? {
        Some(L::One(s)) => split(s),
        Some(L::Many(v)) => v.into_iter().flat_map(split).collect(),
        None => Vec::new(),
    })
}

/// A plan as the model may write it: an update is a path, or a path and
/// its change.
#[derive(Deserialize)]
#[serde(untagged)]
enum UpdateEntry {
    Path(String),
    Change(PageChange),
}

#[derive(Default, Deserialize)]
struct RawPlan {
    #[serde(default)]
    update: Vec<UpdateEntry>,
    #[serde(default)]
    create: Vec<NewPage>,
    #[serde(default)]
    retire: Vec<Retire>,
    #[serde(default)]
    rulings: Vec<RulingDraft>,
    #[serde(default)]
    risks: Vec<RaidItem>,
    #[serde(default)]
    issues: Vec<RaidItem>,
    #[serde(default)]
    renames: Vec<Rename>,
    #[serde(default)]
    topics: serde_json::Value,
}

/// The plan step: the note, the knowledge base's pages, what the knowledge
/// base and the code say on the source's subject (`context`), and the topics
/// already in use for its tags.
pub fn plan_prompt(pages: &[(String, String)], note: &str, context: &str, topics_in_use: &[String]) -> String {
    let mut s = String::from(
        "A note was just ingested into a team's knowledge base. Plan what follows from it. Only what the note itself \
         supports; most notes change a few pages, many change none.\n\n\
         - update: existing pages the note changes, what it overturns first: something a page says that is no longer \
           true, or something it should now say. One line for each change. When the change goes against a ruling \
           (D-nnn) or the code (a file), name it in `against`: that edit waits for a person. Use paths from the list. \
           What is now true about the project changes its page in Current.\n\
         - create: new pages it calls for, a topic nothing covers yet, in the section it belongs to (Current, Design, \
           Reference, Reference/Platform, Rules).\n\
         - retire: existing pages the note makes untrue as a whole, each with the page that replaces it in `by` (a \
           page from the list or one you create). A page that is partly wrong is an update; most notes retire none.\n\
         - rulings: each ruling under the note's Rulings, written up for the decisions log: the ruling in one sentence, \
           a topic of two or three words, why it was made in one line (from the note), the words it was made in, who \
           made it when the note says, a few aliases (other words anyone might search for it by), and the ids of the \
           rulings it replaces, from the passages below.\n\
         - risks: what the note says might go wrong and has not yet, each with a title of a few words, what would \
           show it in one line, and its owner: the person the note says raised it, watches it or will deal with it \
           (\"Ana worried that…\" makes Ana its owner). Leave the owner empty only when the note names no one. Most \
           notes raise none.\n\
         - issues: problems the note says exist now that no ruling or action settles, each the same way, its owner \
           too. A question the note leaves open is not an issue.\n\
         - renames: each thing the note says now goes by another name, old and new, for the Vocabulary page.\n\
         - topics: for the note (page \"note\") and each page you update, create or retire, ",
    );
    s.push_str(&crate::graph::topics_ask(topics_in_use));
    s.push_str(
        ".\n\n\
         At most 10 updates and 10 new pages. Reply with JSON only: {\"update\": [{\"path\": \"path\", \"change\": \
         \"what changes, in one line\", \"against\": \"\"}, …], \"create\": [{\"path\": \"Section/Name.md\", \"purpose\": \
         \"what the page is for\"}, …], \"retire\": [{\"path\": \"path\", \"by\": \"path\"}, …], \"rulings\": \
         [{\"ruling\": \"…\", \"topic\": \"…\", \"why\": \"…\", \"quote\": \"…\", \"who\": \"…\", \"aliases\": [\"…\"], \
         \"supersedes\": [\"D-nnn\"]}, …], \"risks\": [{\"title\": \"…\", \"detail\": \"…\", \"owner\": \"…\"}, …], \
         \"issues\": [{\"title\": \"…\", \"detail\": \"…\", \"owner\": \"…\"}, …], \"renames\": [{\"old\": \"…\", \"new\": \
         \"…\"}, …], \"topics\": [{\"page\": \"note\", \"topics\": [\"…\"]}, {\"page\": \"path\", \"topics\": [\"…\"]}, …]}.\n\n\
         PAGES IN THE KNOWLEDGE BASE:\n",
    );
    for (p, t) in pages {
        if t.is_empty() {
            s.push_str(&format!("- {p}\n"));
        } else {
            s.push_str(&format!("- {p} · {t}\n"));
        }
    }
    if !context.trim().is_empty() {
        s.push_str(&format!("\nWHAT THE KNOWLEDGE BASE AND THE CODE SAY NOW:\n{context}\n"));
    }
    s.push_str(&format!("\nTHE NOTE:\n{note}\n"));
    s
}

/// The model's plan, kept to real pages to update and new pages that may be
/// made, at most [`MAX_UPDATES`] and [`MAX_NEW`], and rulings with words.
pub fn read_plan(reply: &str, pages: &[(String, String)], root: &Path) -> Plan {
    let t = strip_fences(reply);
    let json = t.find('{').and_then(|a| t.rfind('}').map(|b| &t[a..=b])).unwrap_or("{}");
    let raw: RawPlan = serde_json::from_str(json).unwrap_or_default();
    let mut update: Vec<PageChange> = Vec::new();
    for entry in raw.update {
        let change = match entry {
            UpdateEntry::Path(path) => PageChange { path, ..Default::default() },
            UpdateEntry::Change(c) => c,
        };
        if pages.iter().any(|(q, _)| *q == change.path) && !update.iter().any(|u| u.path == change.path) {
            update.push(change);
        }
    }
    update.truncate(MAX_UPDATES);
    let mut create = raw.create;
    create.retain(|n| may_propose(&n.path) && !root.join(&n.path).exists());
    create.dedup_by(|a, b| a.path == b.path);
    create.truncate(MAX_NEW);
    let mut retire: Vec<Retire> = Vec::new();
    for r in raw.retire {
        let known = pages.iter().any(|(q, _)| *q == r.path);
        let by = r.by.trim().to_string();
        if known && !by.is_empty() && by != r.path && !retire.iter().any(|x| x.path == r.path) {
            retire.push(Retire { by, ..r });
        }
    }
    retire.truncate(MAX_UPDATES);
    let rulings = raw.rulings.into_iter().filter(|r| !r.ruling.trim().is_empty()).collect();
    let raid = |items: Vec<RaidItem>| -> Vec<RaidItem> {
        let mut items: Vec<RaidItem> = items.into_iter().filter(|r| !r.title.trim().is_empty()).collect();
        items.truncate(MAX_NEW);
        items
    };
    let (risks, issues) = (raid(raw.risks), raid(raw.issues));
    let mut renames = raw.renames;
    renames.retain(|r| !r.old.trim().is_empty() && !r.new.trim().is_empty());
    renames.truncate(MAX_NEW);
    let named: Vec<String> = std::iter::once(NOTE_TOPICS.to_string())
        .chain(update.iter().map(|u| u.path.clone()))
        .chain(create.iter().map(|n| n.path.clone()))
        .chain(retire.iter().map(|r| r.path.clone()))
        .collect();
    let topics = crate::graph::topics_from(&raw.topics, &named);
    Plan { update, create, retire, rulings, risks, issues, renames, topics }
}

/// The prompt for one page edit from a note: the section that holds the
/// change, sent back whole, which replaces that section only (decision 11).
pub fn edit_prompt(page: &str, change: &str, existing: &str, note_stem: &str, note: &str) -> String {
    let current = if page.starts_with("Current/") {
        "- This page says what is true now: rewrite what changed in place. Never append a dated entry.\n"
    } else {
        ""
    };
    let change = if change.trim().is_empty() { "what the note changes or adds to this page".to_string() } else { change.trim().to_string() };
    format!(
        "You are editing one page of a team's knowledge base, `{page}`, from a note just ingested, [[{note_stem}]]. \
         The change: {change}.\n\n\
         Rules:\n\
         - Reply with the one section that holds the change: its heading line, then everything down to the next \
           heading of the same or a higher level. Change only what the note changes or adds; keep every other line \
           of the section exactly as it is, in the same order. The section you send replaces that section of the page.\n\
         - To add something no section holds, reply with a new section, its heading included; it goes at the end of the page.\n\
         - Use only what the note says. Cite it as [[{note_stem}]] beside what you changed.\n\
         {current}\
         - Leave the frontmatter alone; Ken sets `updated:`.\n\
         - If the note changes nothing on this page, reply exactly NO CHANGE.\n\
         - No preamble, no code fences.\n\n\
         THE PAGE NOW:\n{existing}\n\nTHE NOTE ([[{note_stem}]]):\n{note}\n"
    )
}

/// The model's reply as the section to put in, or None for no change. A
/// reply that does not start with a heading is refused: it names no section.
pub fn finish_section(reply: &str) -> Result<Option<String>> {
    let t = reply.trim();
    if t.eq_ignore_ascii_case("NO CHANGE") {
        return Ok(None);
    }
    let t = strip_fences(t);
    if heading_level(t.lines().next().unwrap_or("")).is_none() {
        return Err(Error::Other("the edit did not start with its section's heading".into()));
    }
    Ok(Some(format!("{}\n", t.trim_end())))
}

/// A heading line's level (`## Pages` is 2).
fn heading_level(line: &str) -> Option<usize> {
    let t = line.trim_start();
    let n = t.chars().take_while(|c| *c == '#').count();
    ((1..=6).contains(&n) && t[n..].starts_with(' ')).then_some(n)
}

/// A heading's words, without its marks and case.
fn heading_words(line: &str) -> String {
    line.trim().trim_start_matches('#').trim().trim_end_matches('#').trim().to_lowercase()
}

/// The heading lines of a page's body (index and level), the frontmatter and
/// fenced code left out.
fn headings(lines: &[&str]) -> Vec<(usize, usize)> {
    let front_end = (lines.first().map(|l| l.trim_end()) == Some("---"))
        .then(|| lines.iter().skip(1).position(|l| l.trim_end() == "---").map(|p| p + 1))
        .flatten();
    let mut fence = false;
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if front_end.is_some_and(|e| i <= e) {
            continue;
        }
        if l.trim_start().starts_with("```") || l.trim_start().starts_with("~~~") {
            fence = !fence;
            continue;
        }
        if let Some(level) = heading_level(l).filter(|_| !fence) {
            out.push((i, level));
        }
    }
    out
}

/// `page` with one section replaced by `section` (decision 11): the section
/// whose heading has the same level and words, from its heading to the next
/// heading of the same or a higher level. A section the page lacks is added
/// at its end. The page's line endings are kept.
pub fn replace_section(page: &str, section: &str) -> String {
    let crlf = page.contains("\r\n");
    let page = page.replace("\r\n", "\n");
    let section = section.replace("\r\n", "\n");
    let section = section.trim_matches('\n');
    let head = section.lines().next().unwrap_or("");
    let lines: Vec<&str> = page.lines().collect();
    let marks = headings(&lines);
    let found = heading_level(head).and_then(|lv| {
        marks
            .iter()
            .find(|(i, l)| *l == lv && heading_words(lines[*i]) == heading_words(head))
            .map(|(i, _)| (*i, lv))
    });
    let mut out: Vec<String> = Vec::new();
    match found {
        Some((i, lv)) => {
            let end = marks.iter().find(|(j, l)| *j > i && *l <= lv).map_or(lines.len(), |(j, _)| *j);
            out.extend(lines[..i].iter().map(|s| s.to_string()));
            out.extend(section.lines().map(str::to_string));
            if end < lines.len() {
                out.push(String::new());
            }
            out.extend(lines[end..].iter().map(|s| s.to_string()));
        }
        None => {
            out.extend(lines.iter().map(|s| s.to_string()));
            while out.last().is_some_and(|l| l.trim().is_empty()) {
                out.pop();
            }
            if !out.is_empty() {
                out.push(String::new());
            }
            out.extend(section.lines().map(str::to_string));
        }
    }
    let text = out.join("\n") + "\n";
    if crlf { text.replace('\n', "\r\n") } else { text }
}

/// `page` with `updated:` set to `today` in its frontmatter, when it has one.
pub fn stamp_updated(page: &str, today: &str) -> String {
    let crlf = page.contains("\r\n");
    let text = page.replace("\r\n", "\n");
    let lines: Vec<&str> = text.lines().collect();
    let Some(close) = (lines.first() == Some(&"---")).then(|| lines.iter().skip(1).position(|l| l.trim_end() == "---").map(|p| p + 1)).flatten() else {
        return page.to_string();
    };
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    let stamp = format!("updated: {today}");
    match (1..close).find(|&i| lines[i].trim_start().starts_with("updated:")) {
        Some(i) => out[i] = stamp,
        None => out.insert(close, stamp),
    }
    let text = out.join("\n") + "\n";
    if crlf { text.replace('\n', "\r\n") } else { text }
}

/// The section with the note cited: as written when it cites it, else with a
/// line saying which note changed it.
fn cite(section: &str, note_stem: &str) -> String {
    if section.contains(&format!("[[{note_stem}")) {
        section.to_string()
    } else {
        format!("{}\n\nChanged from [[{note_stem}]].\n", section.trim_end())
    }
}

/// The size rule: an edit that rewrites more than a fifth of its page, by
/// changed lines against the page's lines, is held for a person.
pub fn rewrites_a_fifth(base: &str, proposed: &str) -> bool {
    let (base, proposed) = (base.replace("\r\n", "\n"), proposed.replace("\r\n", "\n"));
    let diff = similar::TextDiff::from_lines(&base, &proposed);
    let (mut removed, mut added) = (0usize, 0usize);
    for change in diff.iter_all_changes() {
        match change.tag() {
            similar::ChangeTag::Delete => removed += 1,
            similar::ChangeTag::Insert => added += 1,
            similar::ChangeTag::Equal => {}
        }
    }
    removed.max(added) * 5 > base.lines().count().max(1)
}

/// Why an edit was held for a person (decision 11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Held {
    /// The page no longer reads as it did when the run started: a person
    /// changed it.
    Changed,
    /// The page holds a person's work not yet committed: Ken's commit would
    /// carry it (decision 36).
    Uncommitted,
    /// A Rules page: binding, so a person makes the change.
    Rules,
    /// It goes against a ruling or the code, named.
    Against(String),
    /// It rewrites more than a fifth of its page.
    Large,
}

impl Held {
    pub fn reason(&self) -> String {
        match self {
            Held::Changed => "the page changed while the source was read".into(),
            Held::Uncommitted => "the page has changes not yet committed".into(),
            Held::Rules => "it changes a Rules page".into(),
            Held::Against(what) => format!("it goes against {what}"),
            Held::Large => "it rewrites more than a fifth of the page".into(),
        }
    }
}

/// Whether an edit to `page` made against `base` (the page when the run
/// started) is written, or held. `now` is the page on disk as the edit lands;
/// `against` is the ruling or code file the plan says it goes against.
pub fn hold(page: &str, base: &str, now: &str, proposed: &str, against: &str) -> Option<Held> {
    if now.replace("\r\n", "\n") != base.replace("\r\n", "\n") {
        Some(Held::Changed)
    } else if is_method_page(page) {
        Some(Held::Rules)
    } else if matches!(against_kind(against), "ruling" | "code") {
        Some(Held::Against(against.trim().trim_matches('`').to_string()))
    } else if rewrites_a_fifth(base, proposed) {
        Some(Held::Large)
    } else {
        None
    }
}

/// What a contradiction or an edit goes against, by its name: `ruling`
/// (`D-012`), `ticket` (a `tickets/` path or a ticket id), `page` (a
/// Markdown path or a bare page name), `code` (a file with any other
/// extension, or `repo:path`), `source` (the source against itself), or
/// empty.
pub fn against_kind(against: &str) -> &'static str {
    let a = against.trim().trim_matches('`').trim();
    if a.is_empty() {
        return "";
    }
    if matches!(a.to_lowercase().as_str(), "source" | "the source" | "itself") {
        return "source";
    }
    if is_ruling_id(a) {
        return "ruling";
    }
    if a.starts_with("tickets/") || is_series_id(a) {
        return "ticket";
    }
    if crate::chunker::is_markdown(a) {
        return "page";
    }
    let file = a.rsplit('/').next().unwrap_or(a);
    let file = file.split(':').next().unwrap_or(file);
    if a.contains(':') || file.rsplit_once('.').is_some_and(|(s, e)| !s.is_empty() && !e.is_empty()) {
        "code"
    } else {
        "page"
    }
}

/// `D-012`: a ruling's id.
fn is_ruling_id(s: &str) -> bool {
    s.strip_prefix("D-").is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// `SR-012`: an id in a numbered series.
fn is_series_id(s: &str) -> bool {
    let s = s.split(|c: char| c.is_whitespace() || c == ',').next().unwrap_or("");
    match s.rsplit_once('-') {
        Some((k, n)) => {
            !k.is_empty() && k.chars().all(|c| c.is_ascii_uppercase()) && !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())
        }
        None => false,
    }
}

// The note's headings (decision 14), each with the older names that still
// read: a note written before the headings changed keeps its rulings,
// actions and questions.
const SUMMARY: &[&str] = &["Summary"];
const TAKEAWAYS: &[&str] = &["Key Takeaways"];
const OVERTURNS: &[&str] = &["What It Overturns"];
const CONTRADICTIONS: &[&str] = &["Contradictions"];
const RULINGS: &[&str] = &["Rulings", "Rulings Said in the Room"];
const ACTIONS: &[&str] = &["Actions", "Actions and Requests"];
const QUESTIONS: &[&str] = &["Open Questions", "Escalations"];

/// The note's `## ` section under the first of `names` it has (case aside):
/// the heading's name and the section's body.
fn section<'a>(note: &'a str, names: &[&'static str]) -> Option<(&'static str, &'a str)> {
    for name in names {
        let mut offset = 0;
        let mut start: Option<usize> = None;
        for line in note.split_inclusive('\n') {
            let t = line.trim_end();
            match start {
                Some(s) if t.starts_with("## ") || t.starts_with("# ") => return Some((name, &note[s..offset])),
                Some(_) => {}
                None if t.strip_prefix("## ").is_some_and(|h| h.trim().eq_ignore_ascii_case(name)) => start = Some(offset + line.len()),
                None => {}
            }
            offset += line.len();
        }
        if let Some(s) = start {
            return Some((name, &note[s..]));
        }
    }
    None
}

/// A section's bullet lines, placeholders and "nothing" lines left out.
fn section_bullets(note: &str, names: &[&'static str]) -> Vec<String> {
    let Some((_, body)) = section(note, names) else { return Vec::new() };
    body.lines()
        .map(str::trim)
        .filter_map(|l| l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")))
        .map(str::trim)
        .filter(|l| {
            let lower = l.trim_end_matches('.').to_lowercase();
            !l.is_empty()
                && !l.contains("{{")
                && !lower.starts_with("none")
                && !lower.starts_with("nothing")
                && !matches!(lower.as_str(), "n/a" | "none found")
        })
        .map(str::to_string)
        .collect()
}

/// A line's text and what its arrow points at (`Fix it → Ben`).
fn split_target(line: &str) -> (String, Option<String>) {
    match line.rsplit_once(" → ").or_else(|| line.rsplit_once(" -> ")) {
        Some((what, to)) => {
            let to = to.trim().trim_end_matches('.').trim();
            (what.trim().to_string(), (!to.is_empty()).then(|| to.to_string()))
        }
        None => (line.trim().trim_end_matches('.').to_string(), None),
    }
}

/// "The ruling — who" lines from the note's Rulings. What follows the name
/// (". Still a quote.") is left out.
pub fn rulings_of(note: &str) -> Vec<(String, Option<String>)> {
    section_bullets(note, RULINGS)
        .into_iter()
        .map(|l| match l.rsplit_once(" — ") {
            Some((ruling, who)) => {
                let d = who.trim();
                let d = d.split(". ").next().unwrap_or(d).trim().trim_end_matches('.').trim();
                (ruling.trim().to_string(), (!d.is_empty()).then(|| d.to_string()))
            }
            None => (l, None),
        })
        .collect()
}

/// "Action → who" lines from the note's Actions. An arrow that names Intake,
/// a ticket or an idea names no one.
pub fn actions_of(note: &str) -> Vec<(String, Option<String>)> {
    section_bullets(note, ACTIONS)
        .into_iter()
        .map(|l| {
            let (what, to) = split_target(&l);
            let who = to.filter(|t| {
                let t = t.to_lowercase();
                !t.contains("intake") && !t.starts_with("idea") && !is_series_id(t.to_uppercase().trim())
            });
            (what, who)
        })
        .collect()
}

/// The note's Open Questions (or, in an older note, its Escalations), one
/// line each.
pub fn questions_of(note: &str) -> Vec<String> {
    section_bullets(note, QUESTIONS).into_iter().map(|l| l.trim_end_matches('.').to_string()).collect()
}

/// One line of the note's Contradictions: what it contradicts, how they
/// differ, and the source's words.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Contradiction {
    pub against: String,
    pub text: String,
    pub quote: String,
}

impl Contradiction {
    /// The contradiction in one line, for the commit message.
    pub fn line(&self) -> String {
        match self.against.is_empty() {
            true => self.text.clone(),
            false => format!("{}: {}", self.against, self.text),
        }
    }
}

/// Whether a name at the end of an older contradiction line points at
/// something (`… — Current/Project.md.`).
fn names_a_place(s: &str) -> bool {
    crate::chunker::is_markdown(s) || s.contains('/') || is_ruling_id(s) || is_series_id(s)
}

/// One Contradictions line, as the prompt asks for it
/// (`` `D-012`: how they differ — "the source's words" ``), or in the older
/// form (`how they differ — the page.`).
pub fn contradiction(line: &str) -> Contradiction {
    let mut rest = line.trim();
    let mut against = String::new();
    if let Some((a, r)) = rest.strip_prefix('`').and_then(|after| after.split_once('`')) {
        against = a.trim().to_string();
        rest = r.trim_start_matches(|c: char| c == ':' || c.is_whitespace());
    }
    let mut text = rest.to_string();
    let mut quote = String::new();
    for (open, close) in [('"', '"'), ('\u{201c}', '\u{201d}')] {
        let Some(end) = text.rfind(close) else { continue };
        let Some(start) = text[..end].rfind(open) else { continue };
        quote = text[start + open.len_utf8()..end].trim().to_string();
        text = format!("{}{}", &text[..start], &text[end + close.len_utf8()..]);
        break;
    }
    let mut text = text
        .trim()
        .trim_end_matches(|c: char| matches!(c, '—' | '-' | '.' | '*' | '_' | ':') || c.is_whitespace())
        .trim()
        .to_string();
    if against.is_empty() {
        if let Some((what, at)) = text.rsplit_once(" — ") {
            let at = at.trim().trim_end_matches('.').trim_matches('`').trim().to_string();
            if names_a_place(&at) {
                against = at;
                text = what.trim().to_string();
            }
        }
    }
    Contradiction { against, text, quote }
}

/// Every contradiction the note lists.
pub fn contradictions_of(note: &str) -> Vec<Contradiction> {
    section_bullets(note, CONTRADICTIONS).iter().map(|l| contradiction(l)).filter(|c| !c.text.is_empty()).collect()
}

/// The next ruling number in a decisions log: one past the highest `D-nnn`
/// any entry has, in any of its forms, the format example in a fence left out.
fn next_ruling(log: &str) -> u32 {
    let mut fence = false;
    log.lines()
        .filter(|l| {
            if l.trim_start().starts_with("```") {
                fence = !fence;
                return false;
            }
            !fence
        })
        .filter_map(|l| {
            let plain = crate::chunker::unbold_id(l);
            let rest = plain.trim().strip_prefix("D-")?;
            rest.split(|c: char| !c.is_ascii_digit()).next()?.parse::<u32>().ok()
        })
        .max()
        .unwrap_or(0)
        + 1
}

/// The decisions log with a new entry appended in the one entry format
/// (decision 13): `## D-nnn · date · topic`, the bold ruling, the quote,
/// Why, sources, aliases and supersedes. Its number is the next one the log
/// has. A log in an older form keeps its entries as they are.
pub fn decisions_entry(log: &str, e: &crate::proposal::RulingEntry) -> String {
    let next = next_ruling(log);
    let ruling = e.ruling.trim().trim_end_matches('.').trim();
    let topic = match e.topic.trim() {
        "" => ruling.split_whitespace().take(4).collect::<Vec<_>>().join(" ").to_lowercase(),
        t => t.to_string(),
    };
    let crlf = log.contains("\r\n");
    let mut out = log.replace("\r\n", "\n").trim_end().to_string();
    out.push_str(&format!("\n\n## D-{next:03} · {} · {topic}\n\n**{ruling}.**\n", e.date));
    let who = e.who.as_deref().map(str::trim).filter(|w| !w.is_empty());
    match (e.quote.trim(), who) {
        ("", None) => {}
        ("", Some(w)) => out.push_str(&format!("\n> Made by {w}.\n")),
        (q, None) => out.push_str(&format!("\n> \"{}\"\n", q.trim_matches('"'))),
        (q, Some(w)) => out.push_str(&format!("\n> \"{}\" ({w})\n", q.trim_matches('"'))),
    }
    // A ruling proposed in chat may come from no note: then it has no
    // sources line, and its Why says where it was proposed.
    let note = e.note.trim();
    let why = match (e.why.trim(), note) {
        ("", "") => "proposed in a chat with Ken.".to_string(),
        ("", n) => format!("as said in [[{n}]]."),
        (w, _) => w.to_string(),
    };
    out.push_str(&format!("\nWhy: {why}\n"));
    if !note.is_empty() {
        out.push_str(&format!("sources: [[{note}]]\n"));
    }
    let aliases: Vec<&str> = e.aliases.iter().map(|a| a.trim()).filter(|a| !a.is_empty()).collect();
    if !aliases.is_empty() {
        out.push_str(&format!("aliases: {}\n", aliases.join(", ")));
    }
    let replaces: Vec<&str> = e.supersedes.iter().map(|s| s.trim()).filter(|s| is_ruling_id(s)).collect();
    if !replaces.is_empty() {
        out.push_str(&format!("supersedes: {}\n", replaces.join(", ")));
    }
    if crlf { out.replace('\n', "\r\n") } else { out }
}

/// The ticket key and next number in a repo's `tickets/`: the key its
/// tickets already use (`SR-012.md` → `SR`, 13), else the repo name's
/// initials.
pub fn next_ticket(repo: &Path) -> (String, u32) {
    let mut key: Option<String> = None;
    let mut max = 0;
    for e in fs::read_dir(repo.join("tickets")).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(stem) = name.strip_suffix(".md") else { continue };
        let Some((k, n)) = stem.rsplit_once('-') else { continue };
        let (Ok(n), true) = (n.parse::<u32>(), !k.is_empty() && k.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())) else {
            continue;
        };
        key.get_or_insert_with(|| k.to_string());
        max = max.max(n);
    }
    let key = key.unwrap_or_else(|| {
        let leaf = repo.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let initials: String = leaf
            .split(|c: char| !c.is_alphanumeric())
            .filter_map(|w| w.chars().next())
            .map(|c| c.to_ascii_uppercase())
            .filter(|c| c.is_ascii_alphanumeric())
            .collect();
        if initials.is_empty() { "T".into() } else { initials }
    });
    (key, max + 1)
}

/// One ticket from one action said in a note, in the template's format
/// (`Templates/Ticket.md`; decision 8's keys: id, status, type, assignee).
pub fn ticket_text(id: &str, action: &str, who: Option<&str>, note_stem: &str, date: &str) -> String {
    let action = action.trim().trim_end_matches('.');
    format!(
        "---\nid: {id}\ntitle: {}\nstatus: todo\ntype:\nassignee: {}\nupdated: {date}\n---\n\n\
         ## What and Why\n\n{action}. Asked for in [[{note_stem}]].\n\n\
         ## Evidence\n\nSaid in [[{note_stem}]].\n\n\
         ## What Not to Do\n\n(not known yet)\n\n\
         ## How You Would Know It Worked\n\n(to be written before the work starts)\n",
        yaml_str(action),
        who.unwrap_or("")
    )
}

/// The blank in `Templates/` a new page in `page`'s section starts from.
fn blank_for(page: &str) -> Option<&'static str> {
    Some(if page.starts_with("Rules/") {
        "Rule.md"
    } else if page.starts_with("Design/") {
        "Design-Note.md"
    } else if page.eq_ignore_ascii_case("Current/Roadmap.md") {
        "Roadmap.md"
    } else if page.eq_ignore_ascii_case("Current/Feature-Status.md") {
        "Feature-Status.md"
    } else if page.starts_with("Reference/Platform/") {
        "Platform.md"
    } else if page.starts_with("Reference/") {
        "How-to.md"
    } else {
        return None;
    })
}

/// The knowledge base's blank for a new page at `page`, when its section has
/// one.
pub fn section_blank(root: &Path, page: &str) -> Option<String> {
    fs::read_to_string(root.join("Templates").join(blank_for(page)?)).ok()
}

/// The prompt for a new page an ingested note calls for, written only from
/// the note and from its section's blank (decision 11: the new-page prompt
/// belongs to ingest).
pub fn page_prompt(page: &str, purpose: &str, note_stem: &str, note: &str, blank: Option<&str>, today: &str) -> String {
    let from_blank = match blank {
        Some(_) => "- Start from the section's blank below: keep its headings and frontmatter keys, fill in or delete \
                    every {{placeholder}}, and leave out a section the note has nothing for.\n",
        None => "",
    };
    let mut s = format!(
        "You are writing one new page of a team's knowledge base, `{page}`, from a note just ingested, [[{note_stem}]]. \
         The page is: {purpose}.\n\n\
         Rules:\n\
         - Use only what the note says. Leave out anything it does not say; never write that something is unknown.\n\
         - Cite it as [[{note_stem}]] beside what it supports.\n\
         - The knowledge base complements the code, it does not copy it: say what the note says about why and what was \
           decided, and point to a file by its path in backticks rather than restating it.\n\
         {from_blank}\
         - Frontmatter: title, aliases, status: draft, updated: {today}, and sources: - \"[[{note_stem}]]\".\n\
         - Plain words for a reader who has not seen the code.\n\
         - Reply with the finished page only, in Markdown, starting with `---`. No preamble, no code fences around it.\n\n"
    );
    if let Some(b) = blank {
        s.push_str(&format!("THE BLANK:\n{b}\n\n"));
    }
    s.push_str(&format!("THE NOTE ([[{note_stem}]]):\n{note}\n"));
    s
}

/// The reply as a new page: fences stripped, `status: draft` set, and any
/// `checked:`, `reviewed_by:` or `superseded_by:` line dropped (and the
/// older `verified:` and `replaced_by:`): a draft has been neither checked
/// nor reviewed, and nothing replaces it yet.
pub fn finish_page(reply: &str) -> Result<String> {
    let t = strip_fences(reply);
    if !t.starts_with("---") {
        return Err(Error::Other("the page did not start with its frontmatter".into()));
    }
    let mut out = String::new();
    let (mut in_fm, mut closed, mut has_status) = (false, false, false);
    for (i, line) in t.lines().enumerate() {
        let trimmed = line.trim_start();
        if line.trim_end() == "---" {
            if i == 0 {
                in_fm = true;
            } else if in_fm && !closed {
                if !has_status {
                    out.push_str("status: draft\n");
                }
                in_fm = false;
                closed = true;
            }
            out.push_str("---\n");
            continue;
        }
        if in_fm && ["checked:", "reviewed_by:", "verified:", "superseded_by:", "replaced_by:"].iter().any(|k| trimmed.starts_with(k)) {
            continue;
        }
        if in_fm && trimmed.starts_with("status:") {
            out.push_str("status: draft\n");
            has_status = true;
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if !closed {
        return Err(Error::Other("the page's frontmatter was never closed".into()));
    }
    Ok(out)
}

/// A new page with the note cited, as written when it cites it.
fn cite_page(page: &str, note_stem: &str) -> String {
    if page.contains(&format!("[[{note_stem}")) {
        page.to_string()
    } else {
        format!("{}\n\nFrom [[{note_stem}]].\n", page.trim_end())
    }
}

/// Add a new page to its section's index with one line (decision 2): a row
/// of the index's `## Pages` table, else a list line at its end. Returns the
/// index's path when it changed; None when the section has no index or it
/// already links the page.
pub fn add_to_index(root: &Path, page: &str, purpose: &str) -> Result<Option<String>> {
    let Some((section, _)) = page.split_once('/') else { return Ok(None) };
    let index = format!("{section}/Index.md");
    let path = root.join(&index);
    let Ok(text) = fs::read_to_string(&path) else { return Ok(None) };
    let name = note_stem(page);
    if text.contains(&format!("[[{name}]]")) || text.contains(&format!("[[{name}|")) {
        return Ok(None);
    }
    let purpose = purpose.trim().replace(['|', '\n', '\r'], " ");
    let crlf = text.contains("\r\n");
    let text = text.replace("\r\n", "\n");
    let lines: Vec<&str> = text.lines().collect();
    let table_end = lines.iter().position(|l| l.trim().eq_ignore_ascii_case("## Pages")).and_then(|p| {
        let first = (p + 1..lines.len()).take_while(|&i| !lines[i].starts_with("## ")).find(|&i| lines[i].trim_start().starts_with('|'))?;
        let mut end = first;
        while end + 1 < lines.len() && lines[end + 1].trim_start().starts_with('|') {
            end += 1;
        }
        Some(end)
    });
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    match table_end {
        Some(e) => out.insert(e + 1, format!("| [[{name}]] | {purpose} |")),
        None => {
            while out.last().is_some_and(|l| l.trim().is_empty()) {
                out.pop();
            }
            out.push(String::new());
            out.push(if purpose.is_empty() { format!("- [[{name}]]") } else { format!("- [[{name}]]: {purpose}") });
        }
    }
    let text = out.join("\n") + "\n";
    write_file(&path, &if crlf { text.replace('\n', "\r\n") } else { text })?;
    Ok(Some(index))
}

/// Add one dated line to the knowledge base's `log.md` (decision 2):
/// `## [YYYY-MM-DD] kind | title`. The log is made when there is none.
/// Returns its path, for the run's commit.
pub fn append_log(root: &Path, date: &str, kind: &str, title: &str) -> Result<String> {
    let path = root.join(LOG_PAGE);
    let mut text = fs::read_to_string(&path).unwrap_or_else(|_| "# Log\n".to_string());
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    if !text.ends_with('\n') {
        text.push_str(nl);
    }
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    text.push_str(&format!("{nl}## [{date}] {kind} | {title}{nl}"));
    write_file(&path, &text)?;
    Ok(LOG_PAGE.to_string())
}

/// A note's takeaways, section by section, for the Ingest card. A section
/// the note lacks is empty.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Takeaways {
    pub title: String,
    /// meeting | recording | document | note | session, from the note's
    /// folder.
    pub kind: String,
    pub present: Vec<String>,
    /// The source's length as the note gives it (`48 min`, `14 pages`).
    pub length: String,
    pub summary: String,
    pub key_takeaways: Vec<String>,
    pub overturns: String,
    pub contradictions: Vec<String>,
    pub rulings: Vec<String>,
    pub actions: Vec<String>,
    pub open_questions: Vec<String>,
}

/// The takeaways of the note at `note_path` with this text.
pub fn takeaways(note_path: &str, note: &str) -> Takeaways {
    let body = |names: &[&'static str]| -> String {
        match section(note, names) {
            Some((_, b)) if !b.contains("{{") => b.trim().to_string(),
            _ => String::new(),
        }
    };
    // `Source: `standup.txt` (48 min).`
    let length = note
        .lines()
        .find_map(|l| l.trim().strip_prefix("Source:"))
        .and_then(|l| l.split_once("` (").map(|(_, r)| r))
        .and_then(|r| r.split_once(')').map(|(len, _)| len.trim().to_string()))
        .filter(|l| !l.contains("{{"))
        .unwrap_or_default();
    Takeaways {
        title: note.lines().find_map(|l| l.strip_prefix("# ")).unwrap_or("").trim().to_string(),
        kind: kind_at(note_path).map(|k| k.as_str().to_string()).unwrap_or_default(),
        present: present_of(note),
        length,
        summary: body(SUMMARY),
        key_takeaways: section_bullets(note, TAKEAWAYS),
        overturns: body(OVERTURNS),
        contradictions: section_bullets(note, CONTRADICTIONS),
        rulings: section_bullets(note, RULINGS),
        actions: section_bullets(note, ACTIONS),
        open_questions: section_bullets(note, QUESTIONS),
    }
}

/// What waits from a note (ready rulings, drafted tickets, held edits), each
/// a Review item a person applies or discards.
pub fn proposals_from(db: &Db, note: &str) -> Result<Vec<crate::db::ReviewItemRow>> {
    Ok(db
        .list_open_review_items()?
        .into_iter()
        .filter(|it| {
            it.kind == crate::proposal::PROPOSAL_KIND
                && it
                    .payload
                    .as_deref()
                    .and_then(|p| serde_json::from_str::<crate::proposal::Proposal>(p).ok())
                    .and_then(|p| p.from)
                    .as_deref()
                    == Some(note)
        })
        .collect())
}

/// How many proposals wait from each note, counted from open items already
/// listed: one pass, where calling [`proposals_from`] for every note lists
/// all the open items again each time.
pub fn proposal_counts(open: &[crate::db::ReviewItemRow]) -> HashMap<String, usize> {
    let mut out = HashMap::new();
    for it in open.iter().filter(|it| it.kind == crate::proposal::PROPOSAL_KIND) {
        let from = it
            .payload
            .as_deref()
            .and_then(|p| serde_json::from_str::<crate::proposal::Proposal>(p).ok())
            .and_then(|p| p.from);
        if let Some(from) = from {
            *out.entry(from).or_insert(0) += 1;
        }
    }
    out
}

fn write_file(path: &Path, text: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
    }
    fs::write(path, text).map_err(|e| Error::io(path, e))
}

/// What an ingest's follow-ups did.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FollowUps {
    /// The pages written, in order.
    pub pages: Vec<PageWritten>,
    /// Pages held for a person, each now waiting as its diff.
    pub held: Vec<String>,
    /// Rulings waiting as ready entries.
    pub rulings: usize,
    /// Risks and issues waiting for the RAID register.
    pub risks: usize,
    pub issues: usize,
    /// Tickets drafted, waiting to be accepted.
    pub tickets: Vec<String>,
    /// Contradictions filed as findings, one line each.
    pub contradictions: Vec<String>,
    /// Open questions filed as findings.
    pub questions: Vec<String>,
    /// Renames added to the Vocabulary page, `old → new`.
    pub renames: Vec<String>,
    /// What held a person's uncommitted work before the ingest wrote: an
    /// index, the Vocabulary page or the log written to anyway is left out of
    /// the commit, for the person to commit with their own change.
    pub dirty: Vec<String>,
    /// Every file written besides the note: the pages, the section indexes
    /// and the Vocabulary page, for the ingest's commit.
    pub touched: Vec<String>,
}

/// After a note is written: plan which pages it changes and which new pages
/// it calls for, write each (or hold it for a person), then file what waits:
/// its rulings as ready entries, its risks and issues as entries for the
/// RAID register, its actions as drafted tickets, and its contradictions and
/// open questions as findings. Each write cites the note.
/// `context` is what the knowledge base and the code said on the source's
/// subject. `generate` is asked once for the plan, then once per page.
#[allow(clippy::too_many_arguments)]
pub fn follow_ups(
    root: &Path,
    db: &mut Db,
    placement: &Placement,
    note: &str,
    context: &str,
    date: &str,
    now: i64,
    mut generate: impl FnMut(&str) -> Result<String>,
) -> Result<FollowUps> {
    use crate::proposal::{file_page_proposal, Proposal, RulingEntry};
    let from = Some(placement.note.clone());
    let stem = note_stem(&placement.note);
    let mut out = FollowUps::default();

    // A person's work not yet committed, before Ken writes anything.
    out.dirty = crate::git::uncommitted(root);
    let pages = page_list(db)?;
    let in_use = crate::graph::topics_in_use(db, 80)?;
    let plan = generate(&plan_prompt(&pages, note, context, &in_use)).map(|r| read_plan(&r, &pages, root)).unwrap_or_default();

    // Each change is one section of its page, written at once or held.
    for change in &plan.update {
        let page = &change.path;
        let Ok(base) = fs::read_to_string(root.join(page)) else { continue };
        let reply = generate(&edit_prompt(page, &change.change, &base, &stem, note)).and_then(|r| finish_section(&r));
        let Ok(Some(section)) = reply else { continue };
        let section = cite(&section, &stem);
        let proposed = stamp_updated(&replace_section(&base, &section), date);
        if proposed.replace("\r\n", "\n") == base.replace("\r\n", "\n") {
            continue;
        }
        let on_disk = fs::read_to_string(root.join(page)).unwrap_or_default();
        let held = match hold(page, &base, &on_disk, &proposed, &change.against) {
            None if out.dirty.contains(page) => Some(Held::Uncommitted),
            why => why,
        };
        match held {
            None => {
                write_file(&root.join(page), &proposed)?;
                out.pages.push(PageWritten { path: page.clone(), label: change.change.clone(), new: false });
                out.touched.push(page.clone());
            }
            Some(why) => {
                // Held against the page as it is now: the same section, put
                // into a person's newer page, applies without undoing them.
                let proposed = match why {
                    Held::Changed => stamp_updated(&replace_section(&on_disk, &section), date),
                    _ => proposed,
                };
                let p = Proposal { from: from.clone(), ..Proposal::new(page.clone(), on_disk, proposed) };
                let body = format!(
                    "Held from {stem}: {}. Read the change and apply it, or discard it. It applies only while the page \
                     still reads as it does now.",
                    why.reason()
                );
                file_page_proposal(db, &p, &page_name(page), &body, now)?;
                out.held.push(page.clone());
            }
        }
    }

    // New pages, each from its section's blank, each added to its index.
    for new in &plan.create {
        let blank = section_blank(root, &new.path);
        let reply = generate(&page_prompt(&new.path, &new.purpose, &stem, note, blank.as_deref(), date)).and_then(|r| finish_page(&r));
        let Ok(proposed) = reply else { continue };
        let proposed = cite_page(&proposed, &stem);
        let path = root.join(&new.path);
        let made = fs::read_to_string(&path).ok();
        let why = match &made {
            Some(_) => Some(Held::Changed),
            None if is_method_page(&new.path) => Some(Held::Rules),
            None => None,
        };
        match why {
            None => {
                write_file(&path, &proposed)?;
                out.pages.push(PageWritten { path: new.path.clone(), label: new.purpose.clone(), new: true });
                out.touched.push(new.path.clone());
                if let Some(index) = add_to_index(root, &new.path, &new.purpose)? {
                    out.touched.push(index);
                }
            }
            Some(why) => {
                let p = Proposal { from: from.clone(), ..Proposal::new(new.path.clone(), made.unwrap_or_default(), proposed) };
                let body = format!("Held from {stem}: {}. Read the page and apply it, or discard it.", why.reason());
                file_page_proposal(db, &p, &page_name(&new.path), &body, now)?;
                out.held.push(new.path.clone());
            }
        }
    }

    // A retired page keeps its text and names its successor (decision 7):
    // `status: superseded` and `superseded_by:`, written at once unless it is
    // a Rules page or holds a person's work. A page this run already edited,
    // or a successor path that is not there, is left as it is.
    for r in &plan.retire {
        let page = &r.path;
        let successor = crate::pagemeta::successor_name(&r.by);
        let linked = r.by.starts_with("[[");
        if out.pages.iter().any(|p| &p.path == page) || out.held.contains(page) || (!linked && !root.join(&successor).is_file()) {
            continue;
        }
        let Ok(base) = fs::read_to_string(root.join(page)) else { continue };
        let proposed = stamp_updated(&crate::pagemeta::retire(&base, &r.by), date);
        let label = format!("retired, superseded by {successor}");
        let held = if is_method_page(page) {
            Some(Held::Rules)
        } else if out.dirty.contains(page) {
            Some(Held::Uncommitted)
        } else {
            None
        };
        match held {
            None => {
                write_file(&root.join(page), &proposed)?;
                out.pages.push(PageWritten { path: page.clone(), label, new: false });
                out.touched.push(page.clone());
            }
            Some(why) => {
                let p = Proposal { from: from.clone(), ..Proposal::new(page.clone(), base, proposed) };
                let body = format!("Held from {stem}: {}. It retires the page, {label}. Apply it, or discard it.", why.reason());
                file_page_proposal(db, &p, &format!("Retire {}", page_name(page)), &body, now)?;
                out.held.push(page.clone());
            }
        }
    }

    // The topics the plan gave the note and the pages written (decision 21);
    // a held page is tagged by nothing until a person applies it.
    for (page, topics) in &plan.topics {
        let page = if page == NOTE_TOPICS { &placement.note } else { page };
        if *page == placement.note || out.pages.iter().any(|p| &p.path == page) {
            crate::graph::tag_written(db, page, topics)?;
        }
    }

    // Renames join the Vocabulary page's Renames table (decision 18).
    for r in &plan.renames {
        if crate::vocab::add_rename(root, &r.old, &r.new, date)? {
            out.renames.push(format!("{} → {}", r.old.trim(), r.new.trim()));
            if !out.touched.iter().any(|t| t == crate::vocab::VOCABULARY_PAGE) {
                out.touched.push(crate::vocab::VOCABULARY_PAGE.to_string());
            }
        }
    }

    // Rulings wait as ready entries for the decisions log: one click adds
    // each, whoever made it (decision 13).
    let drafts: Vec<RulingDraft> = if plan.rulings.is_empty() {
        rulings_of(note).into_iter().map(|(ruling, who)| RulingDraft { ruling, who: who.unwrap_or_default(), ..Default::default() }).collect()
    } else {
        plan.rulings.clone()
    };
    if !drafts.is_empty() {
        let log = if root.join(DECISIONS_LOG).is_file() {
            DECISIONS_LOG.to_string()
        } else {
            db.paths_named(&["decisions.md"])?.into_iter().next().unwrap_or_else(|| DECISIONS_LOG.to_string())
        };
        let text = fs::read_to_string(root.join(&log)).unwrap_or_else(|_| "# Decisions\n".to_string());
        for d in &drafts {
            let entry = RulingEntry {
                ruling: d.ruling.trim().to_string(),
                who: Some(d.who.trim().to_string()).filter(|w| !w.is_empty()),
                date: date.to_string(),
                note: stem.clone(),
                topic: d.topic.trim().to_string(),
                why: d.why.trim().to_string(),
                quote: d.quote.trim().to_string(),
                aliases: d.aliases.clone(),
                supersedes: d.supersedes.clone(),
            };
            // Shown as the entry against the log now; added against the log
            // as it is then, so rulings from one note go in in any order.
            let proposed = decisions_entry(&text, &entry);
            let title = format!("Ruling: {}", card_title(&entry.ruling));
            let who = entry.who.as_deref().map(|w| format!(", made by {w}")).unwrap_or_default();
            let body = format!(
                "Said in {stem}{who}: \"{}\". Add it to the decisions log in one click; it takes the next number then. \
                 Or discard it.",
                entry.ruling
            );
            let p = Proposal { append: Some(entry), from: from.clone(), ..Proposal::new(log.clone(), text.clone(), proposed) };
            file_page_proposal(db, &p, &title, &body, now)?;
            out.rulings += 1;
        }
    }

    // Risks and issues wait like rulings: one click adds each to the RAID
    // register, which numbers it then.
    let register = fs::read_to_string(root.join(crate::raid::RAID_PAGE)).unwrap_or_default();
    for (kind, items) in [(crate::raid::Kind::Risk, &plan.risks), (crate::raid::Kind::Issue, &plan.issues)] {
        for item in items {
            let owner = Some(item.owner.trim().to_string()).filter(|o| !o.is_empty()).or_else(|| owner_from_note(note, item));
            let draft = crate::raid::Draft {
                kind,
                title: item.title.trim().to_string(),
                owner: owner.clone(),
                text: item.detail.trim().to_string(),
                date: date.to_string(),
                note: stem.clone(),
            };
            let (proposed, _) = crate::raid::append(&register, &draft);
            let name = match kind {
                crate::raid::Kind::Risk => "Risk",
                crate::raid::Kind::Issue => "Issue",
            };
            let card = format!("{name}: {}", card_title(&draft.title));
            let who = owner.map(|o| format!(", for {o}")).unwrap_or_default();
            let body = format!(
                "{name} from {stem}{who}: \"{}\". Add it to the RAID register in one click; it takes the next number then. \
                 Or discard it.",
                draft.title
            );
            let p = Proposal { raid: Some(draft), from: from.clone(), ..Proposal::new(crate::raid::RAID_PAGE, register.clone(), proposed) };
            file_page_proposal(db, &p, &card, &body, now)?;
            match kind {
                crate::raid::Kind::Risk => out.risks += 1,
                crate::raid::Kind::Issue => out.issues += 1,
            }
        }
    }

    // Each action waits as a drafted ticket in the knowledge base's tickets/.
    let actions = actions_of(note);
    if !actions.is_empty() {
        let (key, mut next) = next_ticket(root);
        for (action, who) in &actions {
            let id = format!("{key}-{next:03}");
            next += 1;
            let page = format!("tickets/{id}.md");
            let p = Proposal { from: from.clone(), ..Proposal::new(page.clone(), "", ticket_text(&id, action, who.as_deref(), &stem, date)) };
            let title = format!("Ticket {id}: {}", card_title(action));
            let body = format!(
                "An action from {stem}{}. Ken drafted {page} in the ticket format; accept it to write it, or discard it. \
                 Its type is for the team to set.",
                who.as_deref().map(|w| format!(", for {w}")).unwrap_or_default()
            );
            file_page_proposal(db, &p, &title, &body, now)?;
            out.tickets.push(id);
        }
    }

    // Contradictions and open questions are findings (decisions 10 and 14).
    for c in contradictions_of(note) {
        let f = crate::findings::Finding {
            kind: "contradiction".into(),
            source: placement.note.clone(),
            against_kind: against_kind(&c.against).to_string(),
            against: c.against.clone(),
            text: c.text.clone(),
            quote: c.quote.clone(),
            ..Default::default()
        };
        crate::findings::file(db, &f, now)?;
        out.contradictions.push(c.line());
    }
    for q in questions_of(note) {
        let f = crate::findings::Finding { kind: "question".into(), source: placement.note.clone(), text: q.clone(), ..Default::default() };
        crate::findings::file(db, &f, now)?;
        out.questions.push(q);
    }

    record(db, &placement.note, |card| card.pages = out.pages.clone())?;
    Ok(out)
}

/// The commit message of one ingest (decision 12): the subject names the
/// note; the body lists the pages changed and made, what was held, what
/// waits, the contradictions and the open questions.
pub fn commit_message(placement: &Placement, f: &FollowUps) -> String {
    let mut s = format!("ingest: {}\n\nNote: {}\nSource: {}\n", note_stem(&placement.note), placement.note, placement.source);
    let mut list = |title: &str, items: Vec<String>| {
        if !items.is_empty() {
            s.push_str(&format!("\n{title}:\n"));
            for i in items {
                s.push_str(&format!("- {i}\n"));
            }
        }
    };
    let line = |p: &PageWritten| if p.label.trim().is_empty() { p.path.clone() } else { format!("{}: {}", p.path, p.label.trim()) };
    list("Pages changed", f.pages.iter().filter(|p| !p.new).map(line).collect());
    list("New pages", f.pages.iter().filter(|p| p.new).map(line).collect());
    list("Held for a person", f.held.clone());
    list("Contradictions", f.contradictions.clone());
    list("Open questions", f.questions.clone());
    list("Renames", f.renames.clone());
    let mut waits = Vec::new();
    if f.rulings > 0 {
        waits.push(format!("{} {} to add to the decisions log", f.rulings, if f.rulings == 1 { "ruling" } else { "rulings" }));
    }
    let raid: Vec<String> = [(f.risks, "risk"), (f.issues, "issue")]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, kind)| format!("{n} {kind}{}", if n == 1 { "" } else { "s" }))
        .collect();
    if !raid.is_empty() {
        waits.push(format!("{} to add to the RAID register", raid.join(" and ")));
    }
    if !f.tickets.is_empty() {
        waits.push(format!("tickets {} drafted", f.tickets.join(", ")));
    }
    list("Waiting", waits);
    s
}

/// The end of one ingest: its line in `log.md`, then one commit in the
/// knowledge base holding the note, the source's move, every page written,
/// the section indexes and the log, named path by path ([`crate::git::commit_paths`];
/// never `git add -A`, never pushed). The card records the commit, or why
/// there is none.
pub fn finish_ingest(root: &Path, db: &mut Db, placement: &Placement, note: &str, f: &FollowUps, date: &str) -> Result<()> {
    let title = takeaways(&placement.note, note).title;
    let title = if title.is_empty() { note_stem(&placement.note) } else { title };
    let log = append_log(root, date, "ingest", &title)?;
    // The note and the source's move are the ingest's own; anything else a
    // person had work in before it wrote stays out.
    let mut paths = vec![placement.note.clone(), placement.raw.clone(), placement.source.clone()];
    paths.extend(f.touched.iter().cloned().chain(std::iter::once(log)).filter(|p| !f.dirty.contains(p)));
    let (commit, error) = match crate::git::commit_paths(root, &paths, &commit_message(placement, f)) {
        Ok(c) => (c, None),
        Err(e) => (None, Some(e)),
    };
    // The pages written wait for the lint after an edit (decision 29): one
    // Claude call compares them with their neighbours, in the background.
    // Their topic tags came with the plan.
    let written: Vec<String> = f.pages.iter().map(|p| p.path.clone()).collect();
    crate::lint::queue_after_edit(db, &written)?;
    record(db, &placement.note, |card| {
        card.pages = f.pages.clone();
        card.commit = commit;
        card.commit_error = error;
    })
}

/// After a person applies what waited in the knowledge base (a ready ruling,
/// a risk or issue for the register, a drafted ticket, a held edit): a held
/// new page, or a register made by its first entry, joins its section index,
/// and the change is its own commit. Returns the commit; None when the
/// proposal did not come from an ingest or a drift run (Ken commits only in
/// the knowledge base, decision 36), is in another repo, or the knowledge
/// base is not a git repo.
pub fn accepted(root: &Path, p: &crate::proposal::Proposal) -> std::result::Result<Option<String>, String> {
    let (Some(from), None) = (p.from.as_deref(), p.root.as_ref()) else { return Ok(None) };
    let from = note_stem(from);
    let kind = crate::proposal::kind_of(p);
    let mut paths = vec![p.page.clone()];
    if kind == "new page" {
        let title = subject(&p.page, &p.proposed);
        if let Ok(Some(index)) = add_to_index(root, &p.page, &title) {
            paths.push(index);
        }
    }
    if p.raid.is_some() {
        if let Ok(Some(index)) = add_to_index(root, &p.page, crate::raid::PURPOSE) {
            paths.push(index);
        }
    }
    let message = match (kind, &p.append) {
        // An edit a drift run held (decision 28).
        _ if from == crate::drift::HELD_FROM => format!("drift: apply the held edit to {}", p.page),
        // A ruling a chat proposed (MCP `propose_decision`, decision 13).
        ("ruling", Some(r)) if from == crate::proposal::FROM_CHAT => format!("decisions: add the ruling proposed in chat\n\n{}", r.ruling),
        ("ruling", Some(r)) => format!("decisions: add the ruling from {from}\n\n{}", r.ruling),
        ("risk" | "issue", _) => format!("raid: add the {kind} from {from}\n\n{}", p.raid.as_ref().map_or("", |d| d.title.as_str())),
        ("ticket", _) => format!("tickets: {} from {from}", note_stem(&p.page)),
        ("new page", _) => format!("ingest: add {} from {from}", p.page),
        _ => format!("ingest: apply the held edit to {} from {from}", p.page),
    };
    crate::git::commit_paths(root, &paths, &message)
}

/// The note a waiting item came from: a held edit's, ruling's or ticket's
/// `from`, or the `source` of a finding an ingest filed (a contradiction or
/// an open question). None for anything else.
pub fn note_of(it: &crate::db::ReviewItemRow) -> Option<String> {
    let payload = it.payload.as_deref()?;
    let note = if it.kind == crate::proposal::PROPOSAL_KIND {
        serde_json::from_str::<crate::proposal::Proposal>(payload).ok()?.from?
    } else if it.kind == crate::findings::KIND {
        serde_json::from_str::<crate::findings::Finding>(payload).ok()?.source
    } else {
        return None;
    };
    crate::chunker::is_markdown(&note).then_some(note)
}

/// Close the ingest card of `note` once nothing waits from it (Chris,
/// 2026-10-14): every held edit, ruling and ticket from the note applied or
/// discarded, and every finding it filed resolved or dismissed. Called after
/// a person settles one of them; Seen stays as the manual close. A source an
/// older card still holds in Raw moves beside its note first, as Seen does.
/// Returns the card's id when it closed.
pub fn close_if_settled(root: &Path, db: &mut Db, note: &str, now: i64) -> Result<Option<i64>> {
    let open = db.list_open_review_items()?;
    if open.iter().any(|it| note_of(it).as_deref() == Some(note)) {
        return Ok(None);
    }
    let Some(item) = open.iter().find(|it| it.kind == REVIEW_KIND && it.source_ref == note) else { return Ok(None) };
    if let Some(card) = card_of(item.payload.as_deref()) {
        if root.join(&card.placement.raw).is_file() {
            claim(root, &card.placement)?;
        }
    }
    db.resolve_review_item(item.id, now)?;
    Ok(Some(item.id))
}

/// Change the card of the ingest whose note is `note`, and store it.
fn record(db: &mut Db, note: &str, change: impl FnOnce(&mut Card)) -> Result<()> {
    let Some(item) = db.review_items_of_kind(REVIEW_KIND, 500)?.into_iter().find(|it| it.source_ref == note) else { return Ok(()) };
    let Some(mut card) = card_of(item.payload.as_deref()) else { return Ok(()) };
    change(&mut card);
    let payload = serde_json::to_string(&card).map_err(|e| Error::Other(e.to_string()))?;
    db.set_review_item_payload(item.id, &payload)
}

/// The card stored on an ingest Review item.
pub fn card_of(payload: Option<&str>) -> Option<Card> {
    payload.and_then(|p| serde_json::from_str(p).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn a_risk_or_issue_left_without_an_owner_takes_who_the_note_names() {
        let note = "---\ntitle: Save format sync\n---\n# Save format sync\n\nPresent: Ana, Ben\n\n## Risks and problems\n\n\
                    Ana worried that a single save file could lose the whole world if the game crashes halfway through a \
                    write. Ben said saving already takes about four seconds on the biggest test world.\n";
        let risk = RaidItem {
            title: "Crash mid-write loses world".into(),
            detail: "A crash partway through writing leaves the whole world corrupt".into(),
            owner: String::new(),
        };
        assert_eq!(owner_from_note(note, &risk).as_deref(), Some("Ana"));
        let issue = RaidItem { title: "Saving already takes four seconds".into(), ..Default::default() };
        assert_eq!(owner_from_note(note, &issue).as_deref(), Some("Ben"));
        let nobody = RaidItem { title: "The build server is slow".into(), ..Default::default() };
        assert_eq!(owner_from_note(note, &nobody), None, "no sentence says it");
        assert_eq!(owner_from_note("# A note\n\nThe world may be lost in a crash.\n", &risk), None, "no one present");
        assert!(plan_prompt(&[], note, "", &[]).contains("makes Ana its owner"), "the plan is asked for the owner");
    }

    #[test]
    fn a_note_named_for_a_dated_source_carries_one_date() {
        let d = tempfile::tempdir().unwrap();
        let p = place(d.path(), &format!("{RAW}/meeting-2026-10-14.md"), "2026-10-15", SourceKind::Meeting);
        assert_eq!(p.note, format!("{INGESTED}/Meetings/2026-10/meeting-2026-10-14.md"));
        assert_eq!(p.source, format!("{INGESTED}/Meetings/2026-10/meeting-2026-10-14/meeting-2026-10-14.source.md"), "never the note's name");
        let p = place(d.path(), &format!("{RAW}/2026-10-14 standup.txt"), "2026-10-15", SourceKind::Meeting);
        assert_eq!(p.note, format!("{INGESTED}/Meetings/2026-10/2026-10-14-standup.md"));
        fs::create_dir_all(d.path().join(format!("{INGESTED}/Meetings/2026-10"))).unwrap();
        fs::write(d.path().join(&p.note), "x").unwrap();
        let again = place(d.path(), &format!("{RAW}/2026-10-14 standup.txt"), "2026-10-15", SourceKind::Meeting);
        assert_eq!(again.note, format!("{INGESTED}/Meetings/2026-10/2026-10-14-standup-2.md"));
        assert_eq!(again.source, format!("{INGESTED}/Meetings/2026-10/2026-10-14-standup-2/2026-10-14-standup-2.source.txt"));
        let undated = place(d.path(), &format!("{RAW}/Standup notes.txt"), "2026-10-15", SourceKind::Meeting);
        assert_eq!(undated.note, format!("{INGESTED}/Meetings/2026-10/2026-10-15-standup-notes.md"));
        let bare = place(d.path(), &format!("{RAW}/README"), "2026-10-15", SourceKind::Document);
        assert_eq!(bare.source, format!("{INGESTED}/Documents/2026-10/2026-10-15-readme/2026-10-15-readme.source"));
    }

    /// The walkthrough's `meeting-2026-10-14.md`: its note keeps the name,
    /// the source moved beside it does not take it too, so lint files no
    /// "two pages claim the name" and every link the ingest wrote reaches
    /// one page.
    #[test]
    fn a_dated_source_and_its_note_never_share_a_link_name() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        fs::create_dir_all(root.join(RAW)).unwrap();
        fs::create_dir_all(root.join(INGESTED)).unwrap();
        fs::create_dir_all(root.join("Current")).unwrap();
        fs::write(root.join("Current/Project.md"), SHORT_PROJECT).unwrap();
        fs::write(root.join(RAW).join("meeting-2026-10-14.md"), "# Meeting

Ana: \"we ship on Friday\". Ben will fix the save bug.
").unwrap();
        let project = crate::project::Project::create(root, "Wiki").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        crate::scan::scan(&project, &mut db).unwrap();
        let model = |p: &str| -> std::result::Result<String, CallError> {
            if writing(p, "Current/Project.md") {
                return Ok(SHORT_PROJECT.replace("We ship on Monday.", "We ship on Friday ([[meeting-2026-10-14]])."));
            }
            pass_model(p)
        };
        let report = run_pass(root, &mut db, &[], "2026-10-15", || 5, Transcriber::default(), model, &quiet()).unwrap();
        assert_eq!(report.read.len(), 1, "{report:?}");
        let p = &report.read[0];
        assert_eq!(p.note, format!("{INGESTED}/Meetings/2026-10/meeting-2026-10-14.md"), "one date");
        assert_eq!(p.source, format!("{INGESTED}/Meetings/2026-10/meeting-2026-10-14/meeting-2026-10-14.source.md"));
        assert!(root.join(&p.source).is_file());
        let note = fs::read_to_string(root.join(&p.note)).unwrap();
        assert!(note.contains(&format!("sources:
  - \"[[{}]]\"
", p.source)), "{note}");
        assert!(fs::read_to_string(root.join("Current/Project.md")).unwrap().contains("[[meeting-2026-10-14]]"), "the ingest cited its note");

        crate::scan::scan(&project, &mut db).unwrap();
        let links = crate::links::report(&db).unwrap();
        assert!(links.duplicate_names.is_empty() && links.missing_names.is_empty() && links.broken_paths.is_empty(), "{links:?}");
        let resolver = crate::links::Resolver::from_db(&db).unwrap();
        let name = |t: &str| crate::links::Link { kind: crate::links::LinkKind::Name, target: t.into() };
        assert_eq!(resolver.resolve(&name("meeting-2026-10-14")), vec![p.note.clone()]);
        assert_eq!(resolver.resolve(&name(&p.source)), vec![p.source.clone()], "the note's sources: link");
        let found = crate::lint::scan_findings(&db).unwrap();
        assert!(!found.iter().any(|f| f.kind == "ambiguous-name" || f.text.contains("meeting-2026-10-14")), "{found:#?}");
    }

    #[test]
    fn a_card_title_is_whole_or_cut_at_a_word() {
        let short = "Change `save_world` in `src/world.py` to write one file, by Friday";
        assert_eq!(card_title(short), short, "a title that fits is whole");
        let long = "Change `save_world` in `src/world.py` to write the whole world to one file, `saves/world.json`, by Friday next week";
        let t = card_title(long);
        assert!(t.ends_with('…') && t.chars().count() <= CARD_TITLE_CHARS + 1, "{t}");
        assert!(long.starts_with(t.trim_end_matches('…')), "{t}");
        let rest = &long[t.trim_end_matches('…').len()..];
        assert!(rest.starts_with(' ') || rest.starts_with(", "), "cut at a word: {t}");
    }

    #[test]
    fn a_cut_carries_on_where_the_title_stopped_and_never_opens_a_code_span() {
        let (head, rest) = cut_at_word("one two three four", 9);
        assert_eq!((head.as_str(), rest.as_str()), ("one two…", "…three four"));
        assert_eq!(cut_at_word("short", 9), ("short".to_string(), String::new()));
        let (head, rest) = cut_at_word("call `save world now` today", 18);
        assert_eq!(head, "call…", "the cut is before the code span, not inside it");
        assert_eq!(rest, "…`save world now` today");
    }

    #[test]
    fn a_long_source_is_cut_at_a_line_and_says_so() {
        let short = "one line\n".to_string();
        assert_eq!(cap_source(short.clone()), short);
        let long = "ä line of words\n".repeat(MAX_SOURCE_CHARS / 8);
        let got = cap_source(long.clone());
        assert!(got.len() < MAX_SOURCE_CHARS + 300);
        assert!(got.contains("covers the start of the source only"));
        let body = got.split("\n\n[Ken sent").next().unwrap();
        assert!(body.ends_with("ä line of words"), "cut at a line");
    }

    fn library() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join(RAW)).unwrap();
        fs::create_dir_all(d.path().join(INGESTED)).unwrap();
        fs::write(d.path().join(RAW).join("Standup notes.txt"), "Ana: \"we ship on Friday\". Ben will fix the save bug.\n").unwrap();
        fs::write(d.path().join(RAW).join(".DS_Store"), "x").unwrap();
        d
    }

    const REPLY: &str = "```markdown\n---\ntitle: \"Standup - 2026-09-24\"\nstatus: current\nkind: meeting\nsource: wrong/path.txt\n---\n\n# Standup - 2026-09-24\n\nSource: `Standup notes.txt` (12 min).\n\nPresent: Ana, Ben\n\n## Summary\n\nThe team moved the ship date.\n\n## Key Takeaways\n\n- We ship on Friday.\n\n## What It Overturns\n\n**Ship date is Friday, not Monday.** Current/Project.md said Monday.\n\n- Ana: *\"we ship on Friday\"*\n\n## What Was Said\n\n- **Ship.** Ana: *\"we ship on Friday\"*.\n\n## Actions\n\n- Fix the save bug → Ben.\n\n## Open Questions\n\n- Who tells the players?\n```";

    #[test]
    fn a_note_is_read_as_its_takeaways() {
        let note = "---\ntitle: \"Review - 2026-09-22\"\n---\n\n# Review - 2026-09-22\n\n\
                    Source: `review.vtt` (48 min). Ends mid-sentence.\n\nPresent: chris, dee\n\n\
                    ## Summary\n\nThe room read presets as effort.\n\n\
                    ## Key Takeaways\n\n- Presets are effort.\n- Design is a ticket.\n\n\
                    ## What It Overturns\n\n**Presets are effort, not size.** Sizes page says otherwise.\n\n\
                    ## Contradictions\n\n- `Rules/Sizes.md`: the page says a preset is a size — \"presets are effort\"\n\n\
                    ## Rulings\n\n- Presets are level of effort — chris. Still a quote.\n\n\
                    ## Actions\n\n- Rewrite the sizes page → chris.\n\n\
                    ## Open Questions\n\n- Which Jira types map to tune?\n";
        let t = takeaways(&format!("{INGESTED}/Meetings/2026-09/2026-09-22-review.md"), note);
        assert_eq!(t.title, "Review - 2026-09-22");
        assert_eq!(t.kind, "meeting", "the folder says the kind");
        assert_eq!(t.present, vec!["chris", "dee"]);
        assert_eq!(t.length, "48 min");
        assert_eq!(t.summary, "The room read presets as effort.");
        assert_eq!(t.key_takeaways, vec!["Presets are effort.", "Design is a ticket."]);
        assert!(t.overturns.starts_with("**Presets are effort"));
        assert_eq!(t.contradictions.len(), 1);
        assert_eq!(rulings_of(note), vec![("Presets are level of effort".to_string(), Some("chris".to_string()))]);
        assert_eq!(t.actions, vec!["Rewrite the sizes page → chris."]);
        assert_eq!(t.open_questions, vec!["Which Jira types map to tune?"]);
        assert_eq!(takeaways("Notes/x.md", note).kind, "", "not an ingested note: no kind");
    }

    #[test]
    fn a_note_with_the_old_headings_still_parses() {
        let old = "---\ntitle: x\n---\n\n# Old\n\n## What It Overturns\n\nNothing.\n\n\
                   ## Rulings Said in the Room\n\n- Saves are regions — Ana.\n\n\
                   ## Actions and Requests\n\n- Fix it → Ben.\n- Sizes from Jira → idea, from arthur.\n\n\
                   ## Escalations\n\n- Which save format? → Ana.\n- None.\n\n## Next Steps\n\n- Ben will fix it.\n";
        let t = takeaways("x.md", old);
        assert_eq!(t.open_questions, vec!["Which save format? → Ana."], "Escalations read as open questions");
        assert_eq!(rulings_of(old), vec![("Saves are regions".to_string(), Some("Ana".to_string()))]);
        assert_eq!(actions_of(old), vec![("Fix it".to_string(), Some("Ben".to_string())), ("Sizes from Jira".to_string(), None)]);
        assert!(t.summary.is_empty() && t.key_takeaways.is_empty());
        // The template's own placeholders are no takeaways.
        let blank = takeaways("x.md", DEFAULT_TEMPLATE);
        assert!(blank.summary.is_empty() && blank.key_takeaways.is_empty() && blank.open_questions.is_empty() && blank.length.is_empty());
        assert!(rulings_of(DEFAULT_TEMPLATE).is_empty() && contradictions_of(DEFAULT_TEMPLATE).is_empty());
        // "## Rulings" is not "## Rulings Said in the Room", and the reverse.
        assert!(section("## Rulings Said in the Room\n\n- a\n", &["Rulings"]).is_none());
    }

    #[test]
    fn the_template_and_the_prompt_use_the_new_headings() {
        for h in ["## Summary", "## Key Takeaways", "## What It Overturns", "## Contradictions", "## What Was Said", "## Rulings\n", "## Actions\n", "## Open Questions"] {
            assert!(DEFAULT_TEMPLATE.contains(h), "the template has {h:?}");
        }
        for gone in ["Rulings Said in the Room", "Actions and Requests", "## Escalations", "## Next Steps", "kind:"] {
            assert!(!DEFAULT_TEMPLATE.contains(gone), "the template dropped {gone:?}");
        }
        let p = prompt(DEFAULT_TEMPLATE, "Research/Ingestion/Raw/standup.txt", "text", "2026-09-24");
        assert!(p.contains("\"Open Questions\"") && p.contains("\"Actions\"") && p.contains("`what it contradicts`"), "{p}");
        assert!(!p.contains("Set `kind:`") && !p.contains("→ idea") && !p.contains("Next Steps") && !p.contains("Escalations"), "{p}");
    }

    /// A knowledge base with a Current page, a decisions log and a ticket, a
    /// note in the inbox, and a code repo beside it.
    #[test]
    fn the_context_comes_from_the_ranker_across_pages_rulings_tickets_and_code() {
        let d = library();
        let root = d.path();
        fs::create_dir_all(root.join("Current")).unwrap();
        fs::create_dir_all(root.join("Decisions")).unwrap();
        fs::create_dir_all(root.join("tickets")).unwrap();
        fs::write(root.join("Current/Project.md"), "# Project\n\nThe release ships on Monday after the payment review.\n").unwrap();
        fs::write(root.join("Decisions/DECISIONS.md"), "# Decisions\n\n## D-001 · 2026-09-01 · release day\n\n**The release ships after the payment review.**\n\nWhy: payments first.\nsources: [[x]]\n").unwrap();
        fs::write(root.join("tickets/SR-1.md"), "---\nid: SR-1\nstatus: todo\n---\n\n## What and Why\n\nMove the release after the payment review.\n").unwrap();
        fs::write(root.join(format!("{INGESTED}/old.md")), "# Old\n\nThe release ships on Monday after the payment review.\n").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        crate::scan::scan(&crate::project::Project::create(root, "Wiki").unwrap(), &mut db).unwrap();

        let code_dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(code_dir.path().join("src")).unwrap();
        fs::write(code_dir.path().join("src/release.rs"), "/// Ships the release once the payment review passes.\npub fn ship_release() {}\n").unwrap();
        let mut cdb = Db::open_in_memory().unwrap();
        crate::scan::scan(&crate::project::Project::create(code_dir.path(), "game").unwrap(), &mut cdb).unwrap();
        let code = [CodeSource { name: "game".into(), db: cdb }];

        let source = "# Release review\n\nAna: the release ships Friday. The release is Friday, the payment review moved. Call `ship_release` later.";
        assert_eq!(subject("Research/Ingestion/Raw/r.md", source), "Release review");
        assert_eq!(subject("Research/Ingestion/Raw/sprint_review-notes.txt", "no heading"), "sprint review notes");
        assert!(key_terms(source).contains(&"release".to_string()), "{:?}", key_terms(source));
        assert_eq!(mentioned_names(source), vec!["ship_release"]);
        let ctx = context(&db, &code, "Research/Ingestion/Raw/r.md", source);
        assert!(ctx.contains("=== Current/Project.md · ") && ctx.contains("ships on Monday"), "a page, with its label: {ctx}");
        assert!(ctx.contains("=== Decisions/DECISIONS.md · "), "a ruling: {ctx}");
        assert!(ctx.contains("=== tickets/SR-1.md · "), "a ticket: {ctx}");
        assert!(!ctx.contains(INGESTED), "the inbox is left out: {ctx}");
        assert!(ctx.contains("=== game:src/release.rs"), "the code: {ctx}");
        assert!(ctx.contains("defines `ship_release`"), "a definition the source names: {ctx}");
        let p = prompt_against(DEFAULT_TEMPLATE, "Research/Ingestion/Raw/r.md", source, "2026-09-24", &ctx);
        assert!(p.contains("WHAT THE KNOWLEDGE BASE AND THE CODE SAY NOW") && p.contains("nobody to confirm it"), "{p}");
    }

    #[test]
    fn processing_writes_the_note_and_claims_the_source_at_once() {
        let d = library();
        let root = d.path();
        let raw = format!("{RAW}/Standup notes.txt");
        assert_eq!(waiting(root), vec![raw.clone()]);
        let mut db = Db::open_in_memory().unwrap();
        let mut seen_prompt = String::new();
        let read = ingest_one(root, &mut db, &[], &raw, "2026-09-24", 100, |prompt| {
            seen_prompt = prompt.to_string();
            Ok(REPLY.to_string())
        })
        .unwrap();
        let p = read.placement;

        assert_eq!(p.note, format!("{INGESTED}/Meetings/2026-09/2026-09-24-standup-notes.md"), "people present: a meeting");
        assert_eq!(p.source, format!("{INGESTED}/Meetings/2026-09/2026-09-24-standup-notes/2026-09-24-standup-notes.source.txt"));
        assert!(seen_prompt.contains("we ship on Friday"));
        assert!(!root.join(&raw).exists() && root.join(&p.source).exists(), "claimed as the note is written");
        let note = fs::read_to_string(root.join(&p.note)).unwrap();
        assert!(note.starts_with("---\n") && note.contains(&format!("sources:\n  - \"[[{}]]\"\n---", p.source)) && !note.contains("wrong/path"));
        assert!(!note.contains("kind:"), "the folder says the kind");
        assert!(waiting(root).is_empty() && waiting_new(root, &db).unwrap().is_empty(), "not read twice");
        assert!(in_review(root, &db).unwrap().is_empty());

        let (_, body) = db.open_review_item_of_kind(REVIEW_KIND).unwrap().unwrap();
        assert!(body.contains("The team moved the ship date") && body.contains("Ship date is Friday"), "{body}");
        assert!(body.contains("Who tells the players?") && body.contains("git takes it back"), "{body}");
        let card = card_of(db.list_open_review_items().unwrap()[0].payload.as_deref()).unwrap();
        assert!(card.pages.is_empty() && card.commit.is_none());
        assert!(claim(root, &p).is_ok(), "claiming twice is a no-op");
    }

    #[test]
    fn a_card_from_before_still_reads() {
        let old = "{\"raw\":\"r\",\"note\":\"n\",\"source\":\"s\",\"noteHash\":\"h\",\"filed\":false,\"writes\":[{\"kind\":\"idea\",\"path\":\"ideas/I-1.md\",\"label\":\"x\"}],\"listed\":[],\"undone\":false}";
        let c = card_of(Some(old)).unwrap();
        assert!(c.pages.is_empty() && c.commit.is_none() && c.placement.raw == "r");
        // A source an older card holds in Raw/ is not read again.
        let d = library();
        let mut db = Db::open_in_memory().unwrap();
        let raw = format!("{RAW}/Standup notes.txt");
        let card = Card { placement: Placement { raw: raw.clone(), note: "n.md".into(), source: "s".into() }, pages: Vec::new(), commit: None, commit_error: None };
        db.insert_review_item(REVIEW_KIND, "Ingested", "", "n.md", Some(&serde_json::to_string(&card).unwrap()), 1).unwrap();
        assert_eq!(in_review(d.path(), &db).unwrap(), vec![raw]);
        assert!(waiting_new(d.path(), &db).unwrap().is_empty());
    }

    #[test]
    fn a_card_closes_by_itself_once_nothing_waits_from_it() {
        let d = library();
        let root = d.path();
        let mut db = Db::open_in_memory().unwrap();
        let note = format!("{INGESTED}/Meetings/2026-10/2026-10-14-standup.md");
        let card = Card {
            placement: Placement { raw: format!("{RAW}/gone.txt"), note: note.clone(), source: "s".into() },
            pages: Vec::new(),
            commit: None,
            commit_error: None,
        };
        let card_id = db.insert_review_item(REVIEW_KIND, "Ingested", "", &note, Some(&serde_json::to_string(&card).unwrap()), 1).unwrap();
        let held = crate::proposal::Proposal { from: Some(note.clone()), ..crate::proposal::Proposal::new("Design/Card.md", "a", "b") };
        crate::proposal::file_page_proposal(&mut db, &held, "Held", "", 1).unwrap();
        let other = crate::proposal::Proposal { from: Some("Research/other.md".into()), ..crate::proposal::Proposal::new("Design/Save.md", "a", "b") };
        crate::proposal::file_page_proposal(&mut db, &other, "Held", "", 1).unwrap();
        let question = crate::findings::Finding { kind: "question".into(), source: note.clone(), text: "Who tells the players?".into(), ..Default::default() };
        let finding_id = crate::findings::file(&mut db, &question, 1).unwrap();

        let open = db.list_open_review_items().unwrap();
        let held_item = open.iter().find(|it| it.source_ref == "Design/Card.md").unwrap();
        assert_eq!(note_of(held_item).as_deref(), Some(note.as_str()));
        assert_eq!(note_of(open.iter().find(|it| it.id == finding_id).unwrap()).as_deref(), Some(note.as_str()));
        assert_eq!(note_of(open.iter().find(|it| it.id == card_id).unwrap()), None, "the card itself waits from nothing");

        db.resolve_review_item(held_item.id, 2).unwrap();
        assert_eq!(close_if_settled(root, &mut db, &note, 2).unwrap(), None, "its question is still open");
        db.resolve_review_item(finding_id, 3).unwrap();
        assert_eq!(close_if_settled(root, &mut db, &note, 3).unwrap(), Some(card_id), "nothing waits from it now");
        assert!(db.open_review_item_of_kind(REVIEW_KIND).unwrap().is_none());
        assert!(
            db.list_open_review_items().unwrap().iter().any(|it| it.source_ref == "Design/Save.md"),
            "another note's held edit still waits"
        );
        assert_eq!(close_if_settled(root, &mut db, &note, 4).unwrap(), None, "closed once");
    }

    #[test]
    fn a_reply_without_frontmatter_is_refused_and_the_source_stays() {
        let d = library();
        let root = d.path();
        let mut db = Db::open_in_memory().unwrap();
        let raw = format!("{RAW}/Standup notes.txt");
        assert!(ingest_one(root, &mut db, &[], &raw, "2026-09-24", 1, |_| Ok("Sure! Here is the note.".into())).is_err());
        assert!(root.join(&raw).exists());
        assert!(db.open_review_item_of_kind(REVIEW_KIND).unwrap().is_none());
        let again = place(root, &raw, "2026-09-24", SourceKind::Meeting);
        assert_eq!(again.note, format!("{INGESTED}/Meetings/2026-09/2026-09-24-standup-notes.md"), "nothing was left behind");
        fs::create_dir_all(root.join(&again.note).parent().unwrap()).unwrap();
        fs::write(root.join(&again.note), "x").unwrap();
        assert_eq!(place(root, &raw, "2026-09-24", SourceKind::Meeting).note, format!("{INGESTED}/Meetings/2026-09/2026-09-24-standup-notes-2.md"));
    }

    #[test]
    fn the_kind_comes_from_the_folder_else_from_the_source() {
        assert_eq!(kind_at(&format!("{INGESTED}/Recordings/2026-09/x.md")), Some(SourceKind::Recording));
        assert_eq!(kind_at(&format!("{INGESTED}/Sessions/2026-09/x.md")), Some(SourceKind::Session));
        assert_eq!(kind_at("Current/Project.md"), None);
        assert_eq!(kind_of("---\ntitle: x\n---\n", "Raw/call.vtt"), SourceKind::Recording);
        assert_eq!(kind_of("---\ntitle: x\n---\n", "Raw/call.m4a"), SourceKind::Recording);
        assert_eq!(kind_of("---\npresent: [Ana, Ben]\n---\n", "Raw/x.txt"), SourceKind::Meeting, "an older note");
        assert_eq!(kind_of("---\ntitle: x\n---\n\n# X\n\nPresent: Ana, Ben\n\n## Summary\n", "Raw/call.vtt"), SourceKind::Meeting);
        assert_eq!(kind_of("---\nkind: recording\n---\n", "Raw/x.txt"), SourceKind::Document, "a note's kind: key is not read");
        assert_eq!(present_of("---\ntitle: x\n---\n\n# X\n\nPresent: Ana, Ben.\n"), vec!["Ana", "Ben"]);
        assert_eq!(kind_of("---\ntitle: x\n---\n\n# X\n\nPresent: {{who was there, comma-separated}}\n", "Raw/spec.pdf"), SourceKind::Document);
    }

    #[test]
    fn contradictions_name_what_they_contradict() {
        let c = contradiction("`D-012`: the room wants one save file, the ruling says regions — \"one file is simpler\"");
        assert_eq!(c, Contradiction { against: "D-012".into(), text: "the room wants one save file, the ruling says regions".into(), quote: "one file is simpler".into() });
        let c = contradiction("`src/save.rs`: the code writes regions — *\u{201c}it is all one file\u{201d}*");
        assert_eq!((c.against.as_str(), c.quote.as_str()), ("src/save.rs", "it is all one file"));
        assert_eq!(c.text, "the code writes regions");
        let old = contradiction("The Sizes page says a preset is a size — Ways-of-Working/Sizes.md.");
        assert_eq!((old.against.as_str(), old.text.as_str()), ("Ways-of-Working/Sizes.md", "The Sizes page says a preset is a size"));
        assert_eq!(contradiction("It says Monday, then Friday").against, "");
        for (a, k) in [
            ("D-012", "ruling"),
            ("`D-7`", "ruling"),
            ("SR-101", "ticket"),
            ("tickets/SR-101.md", "ticket"),
            ("Current/Project.md", "page"),
            ("Project", "page"),
            ("src/save.rs", "code"),
            ("game:src/save.rs:12", "code"),
            ("source", "source"),
            ("", ""),
        ] {
            assert_eq!(against_kind(a), k, "{a}");
        }
    }

    #[test]
    fn rulings_actions_and_questions_are_read_and_placeholders_skipped() {
        assert_eq!(rulings_of(NOTE), vec![("We ship region saves before combat".to_string(), Some("Ana".to_string()))]);
        assert!(rulings_of("## Rulings\n\n- None said.\n").is_empty());
        assert_eq!(actions_of(NOTE), vec![("Fix the save bug".to_string(), Some("Ben".to_string()))]);
        assert_eq!(
            actions_of("## Actions\n\n- Map presets → on Intake.\n- Old one → I-030.\n- Plain one.\n"),
            vec![("Map presets".to_string(), None), ("Old one".to_string(), None), ("Plain one".to_string(), None)]
        );
        assert_eq!(questions_of(NOTE), vec!["Which Jira types map to tune?"]);
        assert_eq!(contradictions_of(NOTE).len(), 2);
    }

    #[test]
    fn a_ruling_is_written_in_the_one_entry_format_and_read_back() {
        let entry = crate::proposal::RulingEntry {
            ruling: "We ship region saves before combat".into(),
            who: Some("Ana".into()),
            date: "2026-09-24".into(),
            note: "2026-09-24-standup".into(),
            topic: "region saves".into(),
            why: "combat needs the region format".into(),
            quote: "regions first, then combat".into(),
            aliases: vec!["save regions".into(), "world saves".into()],
            supersedes: vec!["D-001".into(), "not an id".into()],
        };
        let template = include_str!("../templates/wiki/Decisions/DECISIONS.md");
        let out = decisions_entry(template, &entry);
        assert!(out.starts_with(template.trim_end()), "the log is appended to, never rewritten");
        assert!(out.contains("\n\n## D-001 · 2026-09-24 · region saves\n\n**We ship region saves before combat.**\n"), "the fenced example is no ruling: {out}");
        assert!(
            out.contains("\n> \"regions first, then combat\" (Ana)\n\nWhy: combat needs the region format\nsources: [[2026-09-24-standup]]\naliases: save regions, world saves\nsupersedes: D-001\n"),
            "{out}"
        );

        // Every parser reads it.
        let rulings = crate::drift::parse_rulings(&out);
        assert_eq!(rulings.len(), 1);
        assert_eq!((rulings[0].id.as_str(), rulings[0].date.as_deref()), ("D-001", Some("2026-09-24")));
        assert_eq!(rulings[0].sources, vec!["[[2026-09-24-standup]]"]);
        assert_eq!(crate::vocab::parse_decisions(&out), vec![vec!["region saves".to_string(), "save regions".into(), "world saves".into()]]);
        let at = out.find("## D-001 · 2026-09-24").unwrap();
        assert_eq!(crate::chunker::chunk_entry(&out[at..]), Some(("D-001".to_string(), "We ship region saves before combat".to_string())));

        // Older entries still count, bold or plain, and the next is numbered on.
        let old = "# Decisions\n\n**D-410** · 2026-10-06 · anchors — **WRITTEN BY THE TOOLS.**\n\nD-007 · 2026-09-10 · combat — Later.\n";
        let bare = crate::proposal::RulingEntry { ruling: "Anchors stay tool-written.".into(), date: "2026-10-08".into(), note: "n".into(), ..Default::default() };
        let next = decisions_entry(old, &bare);
        assert!(next.contains("\n\n## D-411 · 2026-10-08 · anchors stay tool-written\n\n**Anchors stay tool-written.**\n\nWhy: as said in [[n]].\nsources: [[n]]\n"), "{next}");
        assert!(!next.contains("aliases:") && !next.contains("supersedes:") && !next.contains("> "));

        // A ruling proposed in chat from no note: no sources line.
        let chat = crate::proposal::RulingEntry { ruling: "Saves are regions".into(), date: "2026-10-13".into(), ..Default::default() };
        let out = decisions_entry("# Decisions\n", &chat);
        assert!(out.ends_with("**Saves are regions.**\n\nWhy: proposed in a chat with Ken.\n"), "{out}");
        assert!(!out.contains("sources:"), "{out}");
    }

    #[test]
    fn a_plan_keeps_real_pages_new_ones_and_rulings() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("Reference/Platform")).unwrap();
        fs::write(d.path().join("Reference/Platform/Save.md"), "x").unwrap();
        let pages = vec![("Current/Project.md".to_string(), "Project".to_string()), ("Design/Card.md".to_string(), String::new())];
        let reply = "```json\n{\"update\": [\"Current/Project.md\", {\"path\": \"Design/Card.md\", \"change\": \"the label\", \"against\": \"D-003\"}, \"Current/Project.md\", \"Nope/Missing.md\"], \"create\": [{\"path\": \"Reference/Platform/Release-dates.md\", \"purpose\": \"when we ship\"}, {\"path\": \"Rules/New-rule.md\"}, {\"path\": \"Research/Ingestion/x.md\"}, {\"path\": \"Decisions/DECISIONS.md\"}, {\"path\": \"tickets/SR-9.md\"}, {\"path\": \"Reference/Platform/Save.md\"}, {\"path\": \"../escape.md\"}, {\"path\": \"Current/Index.md\"}, {\"path\": \"Current/RAID.md\"}], \"rulings\": [{\"ruling\": \"Saves are regions\", \"topic\": \"saves\", \"aliases\": \"region save, world file\"}, {\"ruling\": \"\"}], \"risks\": [{\"title\": \"Saves may corrupt\", \"detail\": \"a crash mid-write\", \"owner\": \"Ana\"}, {\"title\": \" \"}], \"issues\": [{\"title\": \"The build is red\"}]}\n```";
        let plan = read_plan(reply, &pages, d.path());
        assert_eq!(
            plan.update,
            vec![
                PageChange { path: "Current/Project.md".into(), ..Default::default() },
                PageChange { path: "Design/Card.md".into(), change: "the label".into(), against: "D-003".into() }
            ]
        );
        assert_eq!(
            plan.create.iter().map(|n| n.path.as_str()).collect::<Vec<_>>(),
            vec!["Reference/Platform/Release-dates.md", "Rules/New-rule.md"],
            "a Rules page is planned (it is held); evidence, the log, the register, tickets and indexes never are"
        );
        assert_eq!(plan.rulings.len(), 1);
        assert_eq!(plan.risks, vec![RaidItem { title: "Saves may corrupt".into(), detail: "a crash mid-write".into(), owner: "Ana".into() }]);
        assert_eq!(plan.issues.iter().map(|r| r.title.as_str()).collect::<Vec<_>>(), vec!["The build is red"]);
        assert_eq!(plan.rulings[0].aliases, vec!["region save", "world file"], "a list written as one string");
        let retire = "{\"retire\": [{\"path\": \"Design/Card.md\", \"by\": \" Design/New.md \"}, {\"path\": \"Design/Card.md\", \"by\": \"x.md\"}, \
                      {\"path\": \"Nope/Missing.md\", \"by\": \"Design/New.md\"}, {\"path\": \"Current/Project.md\", \"by\": \"\"}]}";
        assert_eq!(
            read_plan(retire, &pages, d.path()).retire,
            vec![Retire { path: "Design/Card.md".into(), by: "Design/New.md".into() }],
            "a real page, once, with a successor"
        );
        let tagged = "{\"update\": [\"Current/Project.md\"], \"topics\": [{\"page\": \"note\", \"topics\": [\"Ship dates\"]}, \
                      {\"page\": \"Current/Project.md\", \"topics\": \"ship dates\"}, {\"page\": \"Design/Card.md\", \"topics\": [\"cards\"]}]}";
        assert_eq!(
            read_plan(tagged, &pages, d.path()).topics,
            vec![(NOTE_TOPICS.to_string(), vec!["ship dates".to_string()]), ("Current/Project.md".to_string(), Vec::new())],
            "the note and the pages the plan names; topics that are not a list give none"
        );
        assert_eq!(read_plan("no json here", &pages, d.path()), Plan::default());
        let many: Vec<String> = (0..14).map(|i| format!("{{\"path\": \"Design/P{i}.md\"}}")).collect();
        assert_eq!(read_plan(&format!("{{\"create\": [{}]}}", many.join(",")), &pages, d.path()).create.len(), MAX_NEW);
    }

    #[test]
    fn an_edit_replaces_one_section_and_nothing_else() {
        let page = "---\ntitle: Save\nupdated: 2026-01-01\n---\n# Save\n\nIntro.\n\n## Format\n\nRegions.\n\n### Details\n\nOld detail.\n\n## Later\n\nKeep me.\n```\n## Format\n```\n";
        let section = finish_section("```md\n## Format\n\nOne file per world.\n\n### Details\n\nNew detail.\n```").unwrap().unwrap();
        let out = replace_section(page, &section);
        assert!(out.contains("## Format\n\nOne file per world.\n\n### Details\n\nNew detail.\n\n## Later\n\nKeep me.\n"), "{out}");
        assert!(out.contains("# Save\n\nIntro.\n") && !out.contains("Regions.") && !out.contains("Old detail."));
        assert!(out.ends_with("```\n## Format\n```\n"), "a heading in a fence is not a section: {out}");
        // A section the page lacks is added at its end; CRLF stays CRLF.
        let crlf = "# Card\r\n\r\nShows a name.\r\n";
        assert_eq!(replace_section(crlf, "## Label\n\nShows display_name.\n"), "# Card\r\n\r\nShows a name.\r\n\r\n## Label\r\n\r\nShows display_name.\r\n");
        assert_eq!(finish_section("NO CHANGE").unwrap(), None);
        assert!(finish_section("The page, whole, with no heading first.").is_err());
        // Ken sets `updated:`.
        assert!(stamp_updated(page, "2026-09-24").starts_with("---\ntitle: Save\nupdated: 2026-09-24\n---\n"));
        assert!(stamp_updated("---\ntitle: x\n---\nBody.\n", "2026-09-24").starts_with("---\ntitle: x\nupdated: 2026-09-24\n---\n"));
        assert_eq!(stamp_updated("No frontmatter.\n", "2026-09-24"), "No frontmatter.\n");
        assert!(cite("## Format\n\nX.\n", "n").ends_with("X.\n\nChanged from [[n]].\n"));
        assert_eq!(cite("## Format\n\nX ([[n]]).\n", "n"), "## Format\n\nX ([[n]]).\n");
    }

    #[test]
    fn each_hold_rule_holds_its_edit() {
        let page: String = (1..=10).map(|i| format!("line {i}\n")).collect();
        let one = page.replace("line 4\n", "line four\n");
        let three = page.replace("line 2\n", "two\n").replace("line 5\n", "five\n").replace("line 8\n", "eight\n");
        assert!(!rewrites_a_fifth(&page, &one), "1 of 10 lines");
        assert!(!rewrites_a_fifth(&page, &page.replace("line 2\n", "two\n").replace("line 5\n", "five\n")), "2 of 10 is a fifth, not more");
        assert!(rewrites_a_fifth(&page, &three), "3 of 10 lines");
        assert_eq!(hold("Current/P.md", &page, &page, &one, ""), None);
        assert_eq!(hold("Current/P.md", &page, &page.replace('\n', "\r\n"), &one, ""), None, "line endings are no edit");
        assert_eq!(hold("Current/P.md", &page, &page, &three, ""), Some(Held::Large));
        assert_eq!(hold("Current/P.md", &page, &format!("{page}a person's line\n"), &one, ""), Some(Held::Changed));
        assert_eq!(hold("Rules/Sizes.md", &page, &page, &one, ""), Some(Held::Rules));
        assert_eq!(hold("Ways-of-Working/Sizes.md", &page, &page, &one, ""), Some(Held::Rules), "the older layout");
        assert_eq!(hold("Current/P.md", &page, &page, &one, "D-012"), Some(Held::Against("D-012".into())));
        assert_eq!(hold("Current/P.md", &page, &page, &one, "`src/save.rs`"), Some(Held::Against("src/save.rs".into())));
        assert_eq!(hold("Current/P.md", &page, &page, &one, "Design/Card.md"), None, "a page is no ruling");
    }

    #[test]
    fn a_new_page_joins_its_section_index_and_the_log_gets_its_line() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        fs::create_dir_all(root.join("Reference")).unwrap();
        fs::create_dir_all(root.join("Design")).unwrap();
        fs::write(root.join("Reference/Index.md"), "# Reference\n\n## Pages\n\n| page | holds |\n|---|---|\n| [[Vocabulary]] | the words |\n\n## After\n\nText.\n").unwrap();
        fs::write(root.join("Design/Index.md"), "# Design\r\n\r\nNotes.\r\n").unwrap();
        assert_eq!(add_to_index(root, "Reference/Platform/Release-dates.md", "when | we ship").unwrap().as_deref(), Some("Reference/Index.md"));
        let index = fs::read_to_string(root.join("Reference/Index.md")).unwrap();
        assert!(index.contains("| [[Vocabulary]] | the words |\n| [[Release-dates]] | when   we ship |\n\n## After"), "{index}");
        assert_eq!(add_to_index(root, "Reference/Platform/Release-dates.md", "again").unwrap(), None, "listed once");
        add_to_index(root, "Design/Saves.md", "why regions").unwrap();
        assert_eq!(fs::read_to_string(root.join("Design/Index.md")).unwrap(), "# Design\r\n\r\nNotes.\r\n\r\n- [[Saves]]: why regions\r\n");
        assert_eq!(add_to_index(root, "Current/Roadmap.md", "x").unwrap(), None, "no index, no line");

        assert_eq!(append_log(root, "2026-09-24", "ingest", "Standup  -\n2026-09-24").unwrap(), LOG_PAGE);
        append_log(root, "2026-09-25", "ingest", "Review").unwrap();
        assert_eq!(fs::read_to_string(root.join(LOG_PAGE)).unwrap(), "# Log\n\n## [2026-09-24] ingest | Standup - 2026-09-24\n\n## [2026-09-25] ingest | Review\n");
    }

    const NOTE: &str = "---\ntitle: \"Standup - 2026-09-24\"\nstatus: current\n---\n\n# Standup - 2026-09-24\n\nPresent: Ana, Ben\n\n\
        ## Summary\n\nThe ship date moved.\n\n\
        ## What It Overturns\n\n**Ship date is Friday, not Monday.** Current/Project.md said Monday.\n\n\
        ## Contradictions\n\n- `Current/Project.md`: the page says Monday, the room said Friday — \"we ship on Friday\"\n\
        - `D-001`: the room wants one save file, the ruling says regions — \"one file is simpler\"\n\n\
        ## Rulings\n\n- We ship region saves before combat — Ana.\n- {{The ruling, in the words it was made in}} — {{who}}.\n\n\
        ## Actions\n\n- Fix the save bug → Ben.\n\n\
        ## Open Questions\n\n- Which Jira types map to tune?\n";

    /// A knowledge base with a long Current page, a short Design page, a
    /// Rules page, a page a ruling covers, a decisions log, a ticket and the
    /// Reference index; the note above, written and claimed.
    struct Run {
        _wiki: tempfile::TempDir,
        root: PathBuf,
        db: Db,
        placement: Placement,
        item: i64,
    }

    const PROJECT: &str = "---\ntitle: Project\n---\n# Project\n\nThe goal: a tactics game.\nWe ship on Monday.\nThe audience: players of tactics games.\nOut of scope: multiplayer.\nThe engine: our own.\nThe team: five people.\nThe repo: game.\n";
    const CARD: &str = "---\ntitle: Card\n---\nThe card shows a name.\n";
    const SAVE: &str = "---\ntitle: Save\n---\n# Save\n\n## Format\n\nWorlds save as regions, per D-001.\nOne region per file.\nIndexed by chunk.\nCompressed.\nVersioned.\nBacked up nightly.\n";

    fn run() -> Run {
        let wiki = tempfile::tempdir().unwrap();
        let root = wiki.path().join("Realms-Docs");
        for dir in ["Current", "Design", "Rules", "Decisions", "tickets", "Reference", "Templates"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        fs::write(root.join("Current/Project.md"), PROJECT).unwrap();
        fs::write(root.join("Design/Card.md"), CARD).unwrap();
        fs::write(root.join("Design/Save.md"), SAVE).unwrap();
        fs::write(root.join("Rules/Sizes.md"), "---\ntitle: Sizes\n---\n# Sizes\n\nA preset is a size.\n").unwrap();
        fs::write(root.join(DECISIONS_LOG), "# Decisions\n\n## D-001 · 2026-09-01 · saves\n\n**Worlds save as regions.**\n\nWhy: size.\nsources: [[x]]\n").unwrap();
        fs::write(root.join("tickets/RD-004.md"), "---\nid: RD-004\n---\n").unwrap();
        fs::write(root.join("Reference/Index.md"), "# Reference\n\n## Pages\n\n| page | holds |\n|---|---|\n").unwrap();
        fs::write(root.join("Templates/Platform.md"), "---\ntitle: \"{{Dependency}}\"\n---\n# {{Dependency}}\n\n## Pin\n").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        crate::scan::scan(&crate::project::Project::create(&root, "Wiki").unwrap(), &mut db).unwrap();
        let placement = place(&root, "Research/Ingestion/Raw/standup.txt", "2026-09-24", SourceKind::Meeting);
        fs::create_dir_all(root.join(&placement.note).parent().unwrap()).unwrap();
        fs::write(root.join(&placement.note), NOTE).unwrap();
        let c = Card { placement: placement.clone(), pages: Vec::new(), commit: None, commit_error: None };
        let item = db
            .insert_review_item(REVIEW_KIND, "Ingested", "body", &placement.note, Some(&serde_json::to_string(&c).unwrap()), 1)
            .unwrap();
        Run { _wiki: wiki, root, db, placement, item }
    }

    fn plan_reply() -> String {
        "{\"update\": [{\"path\": \"Current/Project.md\", \"change\": \"ship on Friday\"}, {\"path\": \"Design/Card.md\", \"change\": \"the label\"}, \
         {\"path\": \"Rules/Sizes.md\", \"change\": \"a preset is effort\"}, {\"path\": \"Design/Save.md\", \"change\": \"one file\", \"against\": \"D-001\"}], \
         \"create\": [{\"path\": \"Reference/Platform/Release-dates.md\", \"purpose\": \"when we ship\"}, {\"path\": \"Rules/Ship-on-Fridays.md\", \"purpose\": \"the rule\"}], \
         \"rulings\": [{\"ruling\": \"We ship region saves before combat\", \"topic\": \"region saves\", \"why\": \"combat needs them\", \"quote\": \"regions first\", \"who\": \"Ana\", \"aliases\": [\"save regions\"]}], \
         \"risks\": [{\"title\": \"Friday may slip\", \"detail\": \"the payment review is not booked\", \"owner\": \"Ben\"}], \
         \"issues\": [{\"title\": \"The save bug loses worlds\"}], \
         \"renames\": [{\"old\": \"Guild\", \"new\": \"Clan\"}, {\"old\": \"\", \"new\": \"x\"}], \
         \"topics\": [{\"page\": \"note\", \"topics\": [\"Ship dates\", \"standups\"]}, {\"page\": \"Current/Project.md\", \"topics\": [\"ship dates\"]}, \
         {\"page\": \"Design/Card.md\", \"topics\": [\"cards\"]}]}"
            .into()
    }

    /// Whether a prompt is the edit or the new page for `page` (the note in
    /// every prompt names pages too).
    fn writing(p: &str, page: &str) -> bool {
        p.contains(&format!("knowledge base, `{page}`"))
    }

    /// The model: Project gets one line changed in its section, Card gains a
    /// section (more than a fifth of it), Sizes and Save change a line each.
    fn model(p: &str) -> Result<String> {
        Ok(if p.contains("PAGES IN THE KNOWLEDGE BASE") {
            assert!(p.contains("- Rules/Sizes.md") && !p.contains("- Decisions/DECISIONS.md") && !p.contains("- tickets/"), "{p}");
            plan_reply()
        } else if writing(p, "Current/Project.md") {
            assert!(p.contains("rewrite what changed in place"), "a Current page is rewritten in place");
            PROJECT.split_once("---\n# ").map(|(_, b)| format!("# {b}")).unwrap().replace("We ship on Monday.", "We ship on Friday ([[2026-09-24-standup]]).")
        } else if writing(p, "Design/Card.md") {
            "## Label\n\nThe card shows display_name as its label.\nIts colour is the faction's.\n".into()
        } else if writing(p, "Rules/Sizes.md") {
            "# Sizes\n\nA preset is effort.\n".into()
        } else if writing(p, "Design/Save.md") {
            "## Format\n\nWorlds save as one file.\nOne region per file.\nIndexed by chunk.\nCompressed.\nVersioned.\nBacked up nightly.\n".into()
        } else if writing(p, "Reference/Platform/Release-dates.md") {
            assert!(p.contains("THE BLANK:\n---\ntitle: \"{{Dependency}}\""), "the section's blank: {p}");
            "---\ntitle: Release dates\nstatus: current\nchecked: 2026-01-01 abc\n---\n# Release dates\n\n## Pin\n\nFriday.\n".into()
        } else if writing(p, "Rules/Ship-on-Fridays.md") {
            "---\ntitle: Ship on Fridays\n---\n# Ship on Fridays\n\nWe ship on Fridays ([[2026-09-24-standup]]).\n".into()
        } else {
            "NO CHANGE".into()
        })
    }

    fn waiting_from(r: &Run) -> Vec<crate::proposal::Proposal> {
        proposals_from(&r.db, &r.placement.note)
            .unwrap()
            .into_iter()
            .map(|i| serde_json::from_str(i.payload.as_deref().unwrap()).unwrap())
            .collect()
    }

    #[test]
    fn a_note_writes_what_it_may_and_holds_or_files_the_rest() {
        let mut r = run();
        let done = follow_ups(&r.root, &mut r.db, &r.placement, NOTE, "ctx", "2026-09-24", 9, model).unwrap();

        // Written at once: the one section of Project, and the new Platform
        // page from its blank, in its index.
        let project = fs::read_to_string(r.root.join("Current/Project.md")).unwrap();
        assert!(project.contains("We ship on Friday ([[2026-09-24-standup]])") && !project.contains("Monday"));
        assert!(project.starts_with("---\ntitle: Project\nupdated: 2026-09-24\n---\n# Project\n"), "{project}");
        let release = fs::read_to_string(r.root.join("Reference/Platform/Release-dates.md")).unwrap();
        assert!(release.contains("status: draft") && !release.contains("checked:") && release.contains("[[2026-09-24-standup]]"), "{release}");
        assert!(fs::read_to_string(r.root.join("Reference/Index.md")).unwrap().contains("| [[Release-dates]] | when we ship |"));
        assert_eq!(
            done.pages,
            vec![
                PageWritten { path: "Current/Project.md".into(), label: "ship on Friday".into(), new: false },
                PageWritten { path: "Reference/Platform/Release-dates.md".into(), label: "when we ship".into(), new: true },
            ]
        );
        assert_eq!(done.touched, vec!["Current/Project.md", "Reference/Platform/Release-dates.md", "Reference/Index.md", "Reference/Vocabulary.md"]);
        assert_eq!(done.renames, vec!["Guild → Clan"]);
        // Topic tags came with the plan: the note's and the written page's,
        // none for a held one.
        assert_eq!(crate::graph::topics_of(&r.db, &r.placement.note).unwrap(), vec!["ship dates", "standups"]);
        assert_eq!(crate::graph::topics_of(&r.db, "Current/Project.md").unwrap(), vec!["ship dates"]);
        assert!(crate::graph::topics_of(&r.db, "Design/Card.md").unwrap().is_empty(), "held, not tagged");
        let vocabulary = fs::read_to_string(r.root.join("Reference/Vocabulary.md")).unwrap();
        assert!(vocabulary.contains("| Guild | Clan | 2026-09-24 | |"), "{vocabulary}");

        // Held for a person: too large, a Rules page, against a ruling, a new Rules page.
        assert_eq!(done.held, vec!["Design/Card.md", "Rules/Sizes.md", "Design/Save.md", "Rules/Ship-on-Fridays.md"]);
        assert_eq!(fs::read_to_string(r.root.join("Design/Card.md")).unwrap(), CARD, "held, not written");
        assert!(fs::read_to_string(r.root.join("Rules/Sizes.md")).unwrap().contains("a size"), "a Rules page is never written at once");
        assert_eq!(fs::read_to_string(r.root.join("Design/Save.md")).unwrap(), SAVE, "against a ruling: held");
        assert!(!r.root.join("Rules/Ship-on-Fridays.md").exists());

        // Waiting: four held edits, one ready ruling, a risk and an issue for
        // the register, one drafted ticket.
        let waits = waiting_from(&r);
        assert_eq!(waits.len(), 8);
        assert_eq!(proposal_counts(&r.db.list_open_review_items().unwrap()).get(&r.placement.note), Some(&8));
        let kinds: Vec<&str> = waits.iter().map(crate::proposal::kind_of).collect();
        assert_eq!(kinds.iter().filter(|k| **k == "page").count(), 3);
        assert!(kinds.contains(&"new page") && kinds.contains(&"ruling") && kinds.contains(&"ticket"));
        assert!(kinds.contains(&"risk") && kinds.contains(&"issue"));
        // Added in any order, each takes the next number of its kind then,
        // and the register is made from its template the first time.
        let issue = waits.iter().find(|p| crate::proposal::kind_of(p) == "issue").unwrap();
        let risk = waits.iter().find(|p| crate::proposal::kind_of(p) == "risk").unwrap();
        assert_eq!(risk.page, crate::raid::RAID_PAGE);
        assert!(risk.proposed.contains("## R-001 · 2026-09-24 · Friday may slip\n\nstatus: open\nowner: Ben\nsources: [[2026-09-24-standup]]\n\nthe payment review is not booked\n"), "{}", risk.proposed);
        crate::proposal::apply(&r.root, issue).unwrap();
        crate::proposal::apply(&r.root, risk).unwrap();
        let register = crate::raid::parse(&fs::read_to_string(r.root.join(crate::raid::RAID_PAGE)).unwrap());
        assert_eq!(register.iter().map(|e| (e.id.as_str(), e.title.as_str())).collect::<Vec<_>>(), vec![("I-001", "The save bug loses worlds"), ("R-001", "Friday may slip")]);
        assert_eq!((done.risks, done.issues), (1, 1));
        let ruling = waits.iter().find(|p| p.append.is_some()).unwrap();
        assert_eq!(ruling.page, DECISIONS_LOG);
        let entry = ruling.append.as_ref().unwrap();
        assert_eq!((entry.topic.as_str(), entry.why.as_str(), entry.who.as_deref()), ("region saves", "combat needs them", Some("Ana")));
        crate::proposal::apply(&r.root, ruling).unwrap();
        let log = fs::read_to_string(r.root.join(DECISIONS_LOG)).unwrap();
        assert!(log.contains("## D-002 · 2026-09-24 · region saves\n\n**We ship region saves before combat.**"), "one click, no decider check: {log}");
        let ticket = waits.iter().find(|p| p.page == "tickets/RD-005.md").unwrap();
        assert!(ticket.proposed.contains("id: RD-005\ntitle: \"Fix the save bug\"\nstatus: todo\ntype:\nassignee: Ben\n"), "{}", ticket.proposed);
        assert!(ticket.root.is_none(), "the knowledge base's own tickets/");
        crate::proposal::apply(&r.root, ticket).unwrap();
        assert!(r.root.join("tickets/RD-005.md").exists(), "accepting writes the ticket");
        assert_eq!(done.rulings, 1);
        assert_eq!(done.tickets, vec!["RD-005"]);

        // Contradictions and open questions are findings.
        let found = crate::findings::open(&r.db).unwrap();
        assert_eq!(found.len(), 3);
        let ruling_finding = found.iter().find(|(_, f)| f.against == "D-001").unwrap();
        assert!(ruling_finding.1.binds() && ruling_finding.1.quote == "one file is simpler" && ruling_finding.1.source == r.placement.note);
        assert!(found.iter().any(|(_, f)| f.kind == "contradiction" && f.against_kind == "page"));
        assert!(found.iter().any(|(_, f)| f.kind == "question" && f.text == "Which Jira types map to tune?"));
        assert_eq!(done.contradictions.len(), 2);
        assert_eq!(done.questions, vec!["Which Jira types map to tune?"]);

        // What was cut leaves nothing behind: no ideas, escalations or tasks.
        for gone in ["ideas", "escalations", "tasks"] {
            assert!(!r.root.join(gone).exists(), "{gone}");
        }

        // The pages are on the card.
        let stored = card_of(r.db.get_review_item(r.item).unwrap().unwrap().payload.as_deref()).unwrap();
        assert_eq!(stored.pages, done.pages);

        let msg = commit_message(&r.placement, &done);
        assert!(msg.starts_with("ingest: 2026-09-24-standup\n\nNote: "), "{msg}");
        for part in [
            "Pages changed:\n- Current/Project.md: ship on Friday",
            "New pages:\n- Reference/Platform/Release-dates.md",
            "Held for a person:\n- Design/Card.md",
            "Contradictions:\n- Current/Project.md: ",
            "Open questions:\n- Which Jira",
            "Renames:\n- Guild → Clan",
            "Waiting:\n- 1 ruling to add to the decisions log\n- 1 risk and 1 issue to add to the RAID register\n- tickets RD-005 drafted",
        ] {
            assert!(msg.contains(part), "{part}: {msg}");
        }
    }

    #[test]
    fn a_retired_page_names_its_successor() {
        let mut r = run();
        let plan = "{\"retire\": [{\"path\": \"Design/Card.md\", \"by\": \"Design/Save.md\"}, {\"path\": \"Rules/Sizes.md\", \"by\": \"[[Save]]\"}, \
                    {\"path\": \"Current/Project.md\", \"by\": \"Design/Gone.md\"}]}";
        let model = |p: &str| -> Result<String> { Ok(if p.contains("PAGES IN THE KNOWLEDGE BASE") { plan.to_string() } else { "NO CHANGE".into() }) };
        let done = follow_ups(&r.root, &mut r.db, &r.placement, NOTE, "", "2026-09-24", 9, model).unwrap();

        // Written at once, its text kept.
        let card = fs::read_to_string(r.root.join("Design/Card.md")).unwrap();
        assert_eq!(card, "---\ntitle: Card\nstatus: superseded\nsuperseded_by: Design/Save.md\nupdated: 2026-09-24\n---\nThe card shows a name.\n");
        assert_eq!(done.pages, vec![PageWritten { path: "Design/Card.md".into(), label: "retired, superseded by Design/Save.md".into(), new: false }]);
        assert!(done.touched.contains(&"Design/Card.md".to_string()));

        // A Rules page is held; a successor that is not there retires nothing.
        assert_eq!(done.held, vec!["Rules/Sizes.md"]);
        let held = waiting_from(&r).into_iter().find(|p| p.page == "Rules/Sizes.md").unwrap();
        assert!(held.proposed.contains("status: superseded\nsuperseded_by: \"[[Save]]\"\n"), "{}", held.proposed);
        assert_eq!(fs::read_to_string(r.root.join("Current/Project.md")).unwrap(), PROJECT);
    }

    #[test]
    fn a_page_a_person_changes_during_the_run_is_held_against_their_version() {
        let mut r = run();
        let path = r.root.join("Current/Project.md");
        let edited = format!("{PROJECT}\n# Notes\n\nA person's line.\n");
        let done = follow_ups(&r.root, &mut r.db, &r.placement, NOTE, "", "2026-09-24", 9, |p| {
            if writing(p, "Current/Project.md") {
                fs::write(&path, &edited).unwrap(); // while the model reads
            }
            model(p)
        })
        .unwrap();
        assert!(done.held.contains(&"Current/Project.md".to_string()));
        assert_eq!(fs::read_to_string(&path).unwrap(), edited, "the person's edit stands");
        let held = waiting_from(&r).into_iter().find(|p| p.page == "Current/Project.md").unwrap();
        assert_eq!(held.base, edited, "the diff is against the page as it is now");
        crate::proposal::apply(&r.root, &held).unwrap();
        let applied = fs::read_to_string(&path).unwrap();
        assert!(applied.contains("We ship on Friday") && applied.contains("A person's line."), "applying keeps their section: {applied}");
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git").args(args).current_dir(dir).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn git_repo(root: &Path) {
        git(root, &["init", "-q"]);
        git(root, &["config", "user.name", "Tess"]);
        git(root, &["config", "user.email", "t@t"]);
        git(root, &["add", "-A"]);
        git(root, &["commit", "-q", "-m", "start"]);
    }

    #[test]
    fn a_page_with_a_persons_uncommitted_work_is_held() {
        let mut r = run();
        git_repo(&r.root);
        let edited = PROJECT.replace("title: Project\n", "title: Project\naliases: [\"the project\"]\n");
        fs::write(r.root.join("Current/Project.md"), &edited).unwrap();
        let done = follow_ups(&r.root, &mut r.db, &r.placement, NOTE, "", "2026-09-24", 9, model).unwrap();
        assert!(done.dirty.contains(&"Current/Project.md".to_string()));
        assert!(done.held.contains(&"Current/Project.md".to_string()), "Ken's commit would carry their work: {:?}", done.held);
        assert_eq!(fs::read_to_string(r.root.join("Current/Project.md")).unwrap(), edited);
        let held = waiting_from(&r).into_iter().find(|p| p.page == "Current/Project.md").unwrap();
        assert!(held.proposed.contains("We ship on Friday") && held.proposed.contains("aliases: [\"the project\"]"));
    }

    fn quiet() -> std::sync::Arc<dyn Fn(&str, PassPhase) + Send + Sync> {
        std::sync::Arc::new(|_, _| {})
    }

    const SHORT_PROJECT: &str = "# Project\n\nWe ship on Monday.\nThe goal: a game.\nThe team: five.\nThe engine: ours.\nThe repo: game.\n";

    /// A pass's model: the source named `bad` gets a reply with no
    /// frontmatter, every other source the standup note; the plan changes
    /// Project when there is one.
    fn pass_model(p: &str) -> std::result::Result<String, CallError> {
        if p.contains("SOURCE (") {
            if p.contains("SOURCE (0-bad.txt)") {
                return Ok("Sorry, I cannot do that.".into());
            }
            return Ok(REPLY.into());
        }
        if p.contains("PAGES IN THE KNOWLEDGE BASE") && p.contains("- Current/Project.md") {
            return Ok("{\"update\": [{\"path\": \"Current/Project.md\", \"change\": \"ship on Friday\"}]}".into());
        }
        if writing(p, "Current/Project.md") {
            return Ok(SHORT_PROJECT.replace("We ship on Monday.", "We ship on Friday ([[2026-09-24-standup-notes]])."));
        }
        Ok("NO CHANGE".into())
    }

    #[test]
    fn an_ingest_is_one_commit_of_its_own_paths() {
        let d = library();
        let root = d.path();
        fs::create_dir_all(root.join("Current")).unwrap();
        fs::write(root.join("Current/Project.md"), SHORT_PROJECT).unwrap();
        fs::write(root.join(LOG_PAGE), "# Log\n").unwrap();
        fs::write(root.join("Mine.md"), "a person's page\n").unwrap();
        git(root, &["init", "-q"]);
        git(root, &["config", "user.name", "Tess"]);
        git(root, &["config", "user.email", "t@t"]);
        git(root, &["add", "-A"]);
        git(root, &["commit", "-q", "-m", "start"]);
        fs::write(root.join("Mine.md"), "a person's page, edited\n").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        crate::scan::scan(&crate::project::Project::create(root, "Wiki").unwrap(), &mut db).unwrap();

        let report = run_pass(root, &mut db, &[], "2026-09-24", || 5, Transcriber::default(), pass_model, &quiet()).unwrap();
        assert_eq!(report.read.len(), 1, "{report:?}");
        let p = &report.read[0];
        let files = crate::git::output(root, &["show", "--name-status", "--format=%s", "HEAD"]).unwrap();
        assert!(files.starts_with("ingest: 2026-09-24-standup-notes\n"), "{files}");
        for f in [p.note.as_str(), p.source.as_str(), "Current/Project.md", LOG_PAGE] {
            assert!(files.contains(f), "{f} in the commit: {files}");
        }
        assert!(files.contains(&format!("D\t{}", p.raw)) || files.contains(&format!("R100\t{}", p.raw)), "the source left Raw/: {files}");
        assert!(!files.contains("Mine.md"), "a person's edit is never Ken's to commit");
        let log = fs::read_to_string(root.join(LOG_PAGE)).unwrap();
        assert!(log.ends_with("\n## [2026-09-24] ingest | Standup - 2026-09-24\n"), "{log}");
        let card = card_of(db.review_items_of_kind(REVIEW_KIND, 5).unwrap()[0].payload.as_deref()).unwrap();
        assert_eq!(card.commit, crate::git::output(root, &["rev-parse", "--short", "HEAD"]));
        assert_eq!(card.pages.len(), 1);

        // What waited, applied later, is its own commit.
        let waits = proposals_from(&db, &p.note).unwrap();
        let ticket: crate::proposal::Proposal = serde_json::from_str(waits[0].payload.as_deref().unwrap()).unwrap();
        crate::proposal::apply(root, &ticket).unwrap();
        let sha = accepted(root, &ticket).unwrap().unwrap();
        assert!(!sha.is_empty());
        assert_eq!(
            crate::git::output(root, &["log", "-1", "--format=%s"]).unwrap(),
            format!("tickets: {} from 2026-09-24-standup-notes", note_stem(&ticket.page))
        );

        // So is a risk: the register, made by its first entry, and the
        // section index it joins.
        fs::write(root.join("Current/Index.md"), "# Current\n\n## Pages\n\n| page | holds |\n|---|---|\n").unwrap();
        git(root, &["add", "Current/Index.md"]);
        git(root, &["commit", "-q", "-m", "index"]);
        let draft = crate::raid::Draft { title: "Friday may slip".into(), date: "2026-09-24".into(), note: "2026-09-24-standup-notes".into(), ..Default::default() };
        let risk = crate::proposal::Proposal { raid: Some(draft), from: Some(p.note.clone()), ..crate::proposal::Proposal::new(crate::raid::RAID_PAGE, "", "") };
        crate::proposal::apply(root, &risk).unwrap();
        accepted(root, &risk).unwrap().unwrap();
        let files = crate::git::output(root, &["show", "--name-status", "--format=%s", "HEAD"]).unwrap();
        assert!(files.starts_with("raid: add the risk from 2026-09-24-standup-notes\n"), "{files}");
        assert!(files.contains("A\tCurrent/RAID.md") && files.contains("M\tCurrent/Index.md") && !files.contains("Mine.md"), "{files}");
    }

    #[test]
    fn a_bad_first_source_does_not_stop_the_ones_after_it() {
        let d = library();
        let root = d.path();
        fs::write(root.join(RAW).join("0-bad.txt"), "Something Ken will fail on.\n").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        let report = run_pass(root, &mut db, &[], "2026-09-24", crate::engine::now_epoch, Transcriber::default(), pass_model, &quiet()).unwrap();
        assert_eq!(report.failed.len(), 1, "{report:?}");
        assert_eq!(report.failed[0].0, format!("{RAW}/0-bad.txt"));
        assert_eq!(report.read.len(), 1, "the good source after it is read");
        assert!(report.stopped.is_none());
        assert!(failure_of(&db, &format!("{RAW}/0-bad.txt")).unwrap().is_some());
        let card = card_of(db.review_items_of_kind(REVIEW_KIND, 5).unwrap()[0].payload.as_deref()).unwrap();
        assert!(card.commit.is_none() && card.commit_error.is_none(), "not a git repo: no commit, and no error");
        assert!(fs::read_to_string(root.join(LOG_PAGE)).unwrap().contains("ingest | Standup - 2026-09-24"));
        // The next pass leaves the failed source alone until it is tried again.
        let mut asked = 0;
        let again = run_pass(root, &mut db, &[], "2026-09-24", crate::engine::now_epoch, Transcriber::default(), |p| {
            asked += 1;
            pass_model(p)
        }, &quiet())
        .unwrap();
        assert!(again.read.is_empty() && again.failed.is_empty() && asked == 0);
        // Try again: the failure is cleared and the source read once more.
        assert_eq!(resolve_failure(&mut db, &format!("{RAW}/0-bad.txt"), 7).unwrap(), 1);
        let retried = run_pass(root, &mut db, &[], "2026-09-24", crate::engine::now_epoch, Transcriber::default(), pass_model, &quiet()).unwrap();
        assert_eq!(retried.failed.len(), 1);
        // A file changed since it failed is read again without asking.
        let f = fs::File::options().write(true).open(root.join(RAW).join("0-bad.txt")).unwrap();
        f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(30)).unwrap();
        drop(f);
        let changed = run_pass(root, &mut db, &[], "2026-09-24", crate::engine::now_epoch, Transcriber::default(), pass_model, &quiet()).unwrap();
        assert_eq!(changed.failed.len(), 1, "tried again");
        assert_eq!(failures(&db).unwrap().len(), 1, "one failure open, not two");
    }

    #[test]
    fn a_cli_failure_stops_the_pass_and_marks_nothing_failed() {
        let d = library();
        let root = d.path();
        fs::write(root.join(RAW).join("b-second.txt"), "More.\n").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        let mut calls = 0;
        let report = run_pass(root, &mut db, &[], "2026-09-24", || 5, Transcriber::default(), |_| {
            calls += 1;
            Err(CallError::Stop("Not logged in · Please run /login".into()))
        }, &quiet())
        .unwrap();
        assert_eq!(calls, 1, "one call, then the pass stops");
        assert!(report.stopped.unwrap().contains("/login"));
        assert!(report.failed.is_empty() && failures(&db).unwrap().is_empty());
        assert!(root.join(RAW).join("Standup notes.txt").exists(), "nothing claimed");
        assert!(cli_failed("Not logged in · Please run /login"));
        assert!(!cli_failed("Prompt is too long"));
        assert_eq!(call_result(Ok(crate::assistant::OneshotOutcome::Failed("Prompt is too long".into()))), Err(CallError::Source("Prompt is too long".into())));
        assert!(matches!(call_result(Ok(crate::assistant::OneshotOutcome::TimedOut)), Err(CallError::Stop(_))));
    }

    #[test]
    fn a_recording_with_no_model_waits_and_the_rest_are_read() {
        let d = library();
        let root = d.path();
        fs::write(root.join(RAW).join("0 call.m4a"), b"not really audio").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        let phases = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = phases.clone();
        let on_phase: std::sync::Arc<dyn Fn(&str, PassPhase) + Send + Sync> =
            std::sync::Arc::new(move |raw, p| seen.lock().unwrap().push((raw.to_string(), p)));
        let report = run_pass(root, &mut db, &[], "2026-09-24", || 5, Transcriber::default(), pass_model, &on_phase).unwrap();
        assert_eq!(report.waiting.len(), 1);
        assert!(report.waiting[0].1.contains("Install a transcription model in Settings"));
        assert_eq!(report.read.len(), 1);
        assert!(report.failed.is_empty(), "waiting is not failing");
        let standup = format!("{RAW}/Standup notes.txt");
        assert_eq!(phases.lock().unwrap().as_slice(), &[(standup.clone(), PassPhase::Reading), (standup, PassPhase::Writing)]);
        // A recording that cannot be decoded, with a model there, fails.
        let model = root.join("model.bin");
        fs::write(&model, b"x").unwrap();
        let t = Transcriber { model: Some(&model), ffmpeg: None };
        let report = run_pass(root, &mut db, &[], "2026-09-24", || 6, t, pass_model, &quiet()).unwrap();
        assert_eq!(report.failed.len(), 1);
        assert!(report.failed[0].1.starts_with("Could not transcribe it"), "{:?}", report.failed);
    }

    #[test]
    fn a_wiki_template_without_the_new_headings_gives_way_to_the_bundled_one() {
        let d = library();
        let root = d.path();
        assert!(note_template(root).contains("## Summary"), "no template: the bundled one");
        fs::create_dir_all(root.join("Templates")).unwrap();
        fs::write(root.join(TEMPLATE), "---\ntitle: x\n---\n# {{What}}\n\n## What It Overturns\n").unwrap();
        assert_eq!(note_template(root), DEFAULT_TEMPLATE, "an old template: the bundled one");
        fs::write(root.join(TEMPLATE), "---\ntitle: x\n---\n# Ours\n\n## Summary\n\n{{it}}\n\n## Escalations\n").unwrap();
        assert_eq!(note_template(root), DEFAULT_TEMPLATE, "Escalations and no Open Questions: the bundled one");
        fs::write(root.join(TEMPLATE), "---\ntitle: x\n---\n# Ours\n\n## Summary\n\n{{it}}\n\n## Open Questions\n").unwrap();
        assert!(note_template(root).contains("# Ours"), "a current template is the wiki's own");
    }

    #[test]
    fn notes_and_chats_land_in_their_own_folders() {
        let (name, doc) = note_source("2026-10-01 14.02", Some("Pricing: what we heard?"), "  Ana thinks the tier is too high.\n", Some("Chris Staud"));
        assert_eq!(name, "2026-10-01 14.02 Note - Pricing what we heard.md");
        assert!(doc.starts_with("---\nkind: note\nby: \"Chris Staud\"\nwritten: 2026-10-01 14:02\n---\n"), "{doc}");
        assert!(doc.contains("# Pricing: what we heard?\n\nAna thinks the tier is too high.\n"));
        assert_eq!(declared_kind(&doc), Some(SourceKind::Note));
        assert_eq!(note_source("2026-10-01 14.02", Some("  "), "x", None).0, "2026-10-01 14.02 Note.md");

        let turns = vec![
            ChatTurn { role: "user".into(), content: "What did we decide on saves?".into() },
            ChatTurn { role: "tool".into(), content: "{\"name\":\"route_query\"}".into() },
            ChatTurn { role: "assistant".into(), content: "Regions, per D-001.".into() },
            ChatTurn { role: "activity".into(), content: "Reading".into() },
        ];
        let (name, doc) = chat_source("2026-10-01 14.05", "Saves / regions", "c-123", Some("Chris Staud"), &turns).unwrap();
        assert_eq!(name, "2026-10-01 14.05 Chat - Saves regions.md");
        assert!(doc.starts_with("---\nkind: session\nchat: \"c-123\"\npresent: [\"Chris Staud\", Ken]\n"), "{doc}");
        assert!(doc.contains("**Me:** What did we decide on saves?\n\n**Ken:** Regions, per D-001.\n"));
        assert!(!doc.contains("route_query") && !doc.contains("Reading"));
        assert_eq!(declared_kind(&doc), Some(SourceKind::Session));
        assert!(chat_source("s", "t", "c", None, &turns[1..2]).is_none(), "no turn left, no file");

        let d = library();
        let root = d.path();
        fs::write(root.join(RAW).join(&name), &doc).unwrap();
        let p = place(root, &format!("{RAW}/{name}"), "2026-10-01", SourceKind::Session);
        assert!(p.note.starts_with(&format!("{INGESTED}/Sessions/2026-10/")), "{}", p.note);
        assert_eq!(SourceKind::Note.folder(), "Notes");
    }

    #[test]
    fn a_declared_kind_decides_the_folder() {
        let d = library();
        let root = d.path();
        let (name, doc) = note_source("2026-10-01 14.02", Some("Idea"), "Ana: \"we ship on Friday\".", None);
        fs::remove_file(root.join(RAW).join("Standup notes.txt")).unwrap();
        fs::write(root.join(RAW).join(&name), doc).unwrap();
        let mut db = Db::open_in_memory().unwrap();
        let read = ingest_one(root, &mut db, &[], &format!("{RAW}/{name}"), "2026-10-01", 3, |_| Ok(REPLY.to_string())).unwrap();
        assert!(read.placement.note.starts_with(&format!("{INGESTED}/Notes/2026-10/")), "people present; the source says note: {}", read.placement.note);
        assert_eq!(takeaways(&read.placement.note, REPLY).kind, "note");
    }

    #[test]
    fn ticket_keys() {
        let d = tempfile::tempdir().unwrap();
        let repo = d.path().join("Shattered-Realms");
        fs::create_dir_all(&repo).unwrap();
        assert_eq!(next_ticket(&repo), ("SR".to_string(), 1));
        fs::create_dir_all(repo.join("tickets")).unwrap();
        fs::write(repo.join("tickets/SRX-012.md"), "").unwrap();
        fs::write(repo.join("tickets/TICKET.md"), "").unwrap();
        assert_eq!(next_ticket(&repo), ("SRX".to_string(), 13));
        let t = ticket_text("SRX-013", "Fix the save bug.", Some("Ben"), "n", "2026-09-24");
        assert!(t.contains("\n## What and Why\n\nFix the save bug. Asked for in [[n]].") && t.contains("updated: 2026-09-24\n"), "{t}");
        assert!(!t.contains("size:") && !t.contains("scope:") && !t.contains("verify:"), "Wright's keys are gone: {t}");
    }

    #[test]
    fn a_source_lands_in_raw_under_a_free_name() {
        let d = tempfile::tempdir().unwrap();
        let first = raw_target(d.path(), "notes.txt").unwrap();
        assert_eq!(first, d.path().join(RAW).join("notes.txt"), "Raw is made when missing");
        fs::write(&first, "x").unwrap();
        assert_eq!(raw_target(d.path(), "../../notes.txt").unwrap(), d.path().join(RAW).join("notes (2).txt"), "a name only, numbered");
        assert_eq!(raw_target(d.path(), "README.md").unwrap(), d.path().join(RAW).join("README (source).md"));
        assert!(raw_target(d.path(), "..").is_err());
    }
}

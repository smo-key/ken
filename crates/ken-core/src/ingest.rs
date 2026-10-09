//! The library inbox (Ways of Working, docs-system "Ingest"), in two parts.
//! An ingest runs as soon as a source is dropped in, and nobody confirms it.
//!
//! **Processing.** A source dropped in `Research/Ingestion/Raw/` is read and
//! written up as one dated note from `Templates/Ingested-note.md` (a
//! summary, the key takeaways, what it overturns, contradictions, rulings,
//! actions, escalations and next steps, as that kind of source needs),
//! straight into its organized home. What follows from it is written at once
//! and cites the note ([`follow_ups`]): page edits and new pages, ideas and
//! escalations in the team repo, and my next steps on Your day. Each write is
//! recorded on the source's card ([`Written`]) so a person can undo it.
//! Four things wait instead: a ruling, for its decider; a change to
//! Ways-of-Working, Conventions or a rule, as a ticket; an edit staging held
//! ([`rewrites_a_fifth`], or the page changed while the source was read);
//! and an action, as a ticket. The source stays in `Raw/` until it is seen,
//! and is not read again.
//!
//! **Filing.** When the person has read the card ([`file`]), the source
//! moves beside its note, under `Research/Ingestion/Ingested/<Meetings|
//! Recordings|Documents|Notes|Sessions>/<YYYY-MM>/`. [`undo_write`]
//! reverses one write; [`undo_all`] reverses every one, then the note and
//! the move.
//!
//! [`run_pass`] reads every new source in turn (a recording is transcribed
//! first); one that fails is recorded and passed over. The model call is
//! passed in (`generate`), so the app runs Claude headless and a test a
//! stand-in.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

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
/// missing or older than the sections the card and the follow-ups read (a
/// wiki made before `## Summary` was in it), then the bundled one.
pub fn note_template(root: &Path) -> String {
    match fs::read_to_string(root.join(TEMPLATE)) {
        Ok(own) if own.contains("## Summary") => own,
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

/// What a source was, which decides its folder once filed.
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
    /// Where the source goes when filed, beside the note
    /// (`…/Ingested/Meetings/2026-09/2026-09-24-standup/standup.txt`).
    pub source: String,
}

/// What an ingest card carries: where things went, a hash of the note as
/// written (so Undo can tell whether a person has edited it since), whether
/// the source has been filed yet, every write made from it, and what stayed
/// listed on the card only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Card {
    #[serde(flatten)]
    pub placement: Placement,
    pub note_hash: String,
    /// The source moved beside its note. Cards from before the two parts
    /// were always filed.
    #[serde(default = "filed_before")]
    pub filed: bool,
    /// What was written from the note, in the order it was written.
    #[serde(default)]
    pub writes: Vec<Written>,
    /// What the note calls for that Ken had nowhere to write.
    #[serde(default)]
    pub listed: Vec<Listed>,
    /// Undo all ran. The card stays open and the source waits in `Raw/`
    /// without being read again, until a person asks for it to be.
    #[serde(default)]
    pub undone: bool,
}

fn filed_before() -> bool {
    true
}

fn hash(text: &str) -> String {
    format!("{:016x}", twox_hash::XxHash64::oneshot(0, text.as_bytes()))
}

/// A file's hash with its line endings set aside, so a checkout that turns
/// LF into CRLF does not read as a person's edit.
fn text_hash(text: &str) -> String {
    hash(&text.replace("\r\n", "\n"))
}

/// What one write from a note was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WriteKind {
    /// An existing page, edited.
    Edit,
    /// A new page.
    Page,
    /// `ideas/I-nnn.md` in the team repo.
    Idea,
    /// `escalations/E-nnn.md` in the team repo.
    Escalation,
    /// A task on Your day.
    Task,
}

/// One write from a note, with what it takes to reverse it: for an edit the
/// page before (`base`), for a created file its content's hash, for a task
/// its id. The page after is kept as its hash: Undo needs only to know
/// whether the file still reads as written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Written {
    pub kind: WriteKind,
    /// The file, relative to `root`.
    pub path: String,
    /// The repo or workspace the file is in, when not the library's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    /// What the row says: the change, the page's purpose, the idea, the
    /// question or the task.
    pub label: String,
    /// An escalation's person, or whose step a task is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// An edit: the page as it read before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// The file as written ([`text_hash`]).
    #[serde(default)]
    pub hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default)]
    pub undone: bool,
}

/// Something the note calls for that stays on the card only: an idea or an
/// escalation with no team repo to write it to, a next step for someone
/// else, an action or a method change with no team repo to ticket it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Listed {
    /// `idea` | `escalation` | `next step` | `ticket`
    pub kind: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
}

/// Whether this project has the inbox at all.
pub fn has_inbox(root: &Path) -> bool {
    root.join(RAW).is_dir()
}

/// Files in `Raw/`, oldest name first: every file there except dotfiles and
/// an index page. Includes sources being reviewed; see [`waiting_new`].
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

/// Sources in `Raw/` whose note is written and waiting for the person
/// (an open, unfiled ingest card).
pub fn in_review(db: &Db) -> Result<Vec<String>> {
    Ok(db
        .list_open_review_items()?
        .into_iter()
        .filter(|it| it.kind == REVIEW_KIND)
        .filter_map(|it| card_of(it.payload.as_deref()))
        .filter(|c| !c.filed)
        .map(|c| c.placement.raw)
        .collect())
}

/// Sources in `Raw/` not yet processed: [`waiting`] less those in review.
pub fn waiting_new(root: &Path, db: &Db) -> Result<Vec<String>> {
    let reviewing = in_review(db)?;
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

/// Where a source of `kind` goes on `date` (`YYYY-MM-DD`): its kind's
/// folder, then the month, not colliding with an earlier one.
pub fn place(root: &Path, raw: &str, date: &str, kind: SourceKind) -> Placement {
    let file = raw.rsplit('/').next().unwrap_or(raw).to_string();
    let stem = file.rsplit_once('.').map_or(file.as_str(), |(s, _)| s);
    let month = date.get(..7).unwrap_or(date);
    let dir = format!("{INGESTED}/{}/{month}", kind.folder());
    let mut base = format!("{date}-{}", slug(stem));
    let mut n = 2;
    while root.join(&dir).join(format!("{base}.md")).exists() || root.join(&dir).join(&base).exists() {
        base = format!("{date}-{}-{n}", slug(stem));
        n += 1;
    }
    Placement {
        raw: raw.to_string(),
        note: format!("{dir}/{base}.md"),
        source: format!("{dir}/{base}/{file}"),
    }
}

/// What kind of source a note came from: its frontmatter `kind:` when the
/// model set it, else a transcript file is a recording, a source with people
/// present a meeting, and anything else a document.
pub fn kind_of(note: &str, raw: &str) -> SourceKind {
    let field = |key: &str| {
        note.lines()
            .skip(1)
            .take_while(|l| l.trim_end() != "---")
            .find_map(|l| l.trim().strip_prefix(key).map(|v| v.trim().trim_matches('"').to_lowercase()))
    };
    if let Some(k) = field("kind:").as_deref().and_then(SourceKind::parse) {
        return k;
    }
    let ext = raw.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
    if matches!(ext.as_str(), "vtt" | "srt") {
        return SourceKind::Recording;
    }
    let present = field("present:").unwrap_or_default();
    let people = present.trim_matches(|c| c == '[' || c == ']').trim();
    if !people.is_empty() && !people.contains("{{") {
        SourceKind::Meeting
    } else {
        SourceKind::Document
    }
}

/// The prompt: the template, the source's text, and the rules the method
/// sets for a note (quotes verbatim, evidence not doctrine, what it
/// overturns first).
pub fn prompt(template: &str, raw: &str, text: &str, date: &str) -> String {
    prompt_against(template, raw, text, date, "")
}

/// [`prompt`], with what the wiki says now about the source's subject
/// (`wiki`, from [`wiki_context`]), so the note can say what the source
/// overturns and where it contradicts a page.
pub fn prompt_against(template: &str, raw: &str, text: &str, date: &str, wiki: &str) -> String {
    let file = raw.rsplit('/').next().unwrap_or(raw);
    let wiki = if wiki.trim().is_empty() {
        String::new()
    } else {
        format!(
            "WHAT THE WIKI SAYS NOW (passages Ken found on the source's subject; cite a page by its path):\n{wiki}\n\n"
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
         - Lead \"What It Overturns\" with the one thing that changes what the team wrote.\n\
         - Under \"Contradictions\", list each place the source disagrees with what the wiki says (name the page) or \
           with itself (name both places). Only what the source and the passages below show.\n\
         - A section with nothing in it is left out, heading and all. Never write that something is absent, that \
           there are none, or what the source does not touch.\n\
         - \"Rulings Said in the Room\": one line per ruling, `- the ruling, in the decider's words — decider`.\n\
         - \"Actions and Requests\": one line per piece of work someone asked for, `- the action → who it is for`; \
           a rough idea nobody has asked to be done is `- the idea → idea, from who raised it`.\n\
         - \"Escalations\": one line per question only a named person can answer, `- the question → who`.\n\
         - \"Next Steps\": one line per step, `- Who will what`.\n\
         - Name who said what. List who was present if the source shows it.\n\
         - Set `kind:` in the frontmatter to meeting (people talking: a standup, review or call), recording (a \
           transcript of audio or video that is not a meeting), or document (anything written).\n\
         - Reply with the finished note only, in Markdown, starting with its `---` frontmatter. No preamble, no code fences.\n\n\
         TEMPLATE:\n{template}\n\n{wiki}SOURCE ({file}):\n{text}\n",
    )
}

/// What the wiki says about a source's subject: the passages Ken's search
/// ranks for the source's most repeated words, a few per page, the inbox and
/// the templates left out. Given to the note so it can name what the source
/// overturns and contradicts.
pub fn wiki_context(db: &Db, text: &str) -> String {
    const STOP: &[&str] = &[
        "about", "after", "again", "being", "could", "every", "first", "going", "great", "might", "never", "other",
        "right", "should", "still", "their", "there", "these", "thing", "think", "those", "under", "where", "which",
        "while", "would", "yeah", "really", "because", "maybe", "doing", "gonna", "wanna", "we're", "that's", "there's",
    ];
    let mut counts: HashMap<String, usize> = HashMap::new();
    for w in text.split(|c: char| !c.is_alphanumeric() && c != '-').map(str::to_lowercase) {
        if w.chars().count() >= 5 && !STOP.contains(&w.as_str()) && !w.chars().all(|c| c.is_ascii_digit()) {
            *counts.entry(w).or_default() += 1;
        }
    }
    let mut words: Vec<(String, usize)> = counts.into_iter().filter(|(_, n)| *n > 1).collect();
    words.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let query = words.iter().take(10).map(|(w, _)| w.as_str()).collect::<Vec<_>>().join(" ");
    if query.is_empty() {
        return String::new();
    }
    let hits = crate::routing::search_member(db, &query, None, 24).unwrap_or_default();
    let mut out = String::new();
    let mut pages: Vec<String> = Vec::new();
    for hit in hits {
        if hit.path.starts_with("Research/Ingestion") || hit.path.starts_with("Templates/") || !hit.path.ends_with(".md") {
            continue;
        }
        if pages.iter().filter(|p| **p == hit.path).count() >= 2 || (!pages.contains(&hit.path) && pages.len() >= 6) {
            continue;
        }
        pages.push(hit.path.clone());
        let excerpt: String = hit.snippet.chars().take(700).collect();
        out.push_str(&format!("\n=== {} ===\n{}\n", hit.path, excerpt.trim()));
    }
    out
}

/// The model's reply as a note: fences stripped, and the frontmatter
/// `source:` set to where the source is filed.
pub fn finish_note(reply: &str, placement: &Placement) -> Result<String> {
    let t = strip_fences(reply);
    if !t.starts_with("---") {
        return Err(Error::Other("the note did not start with its frontmatter".into()));
    }
    let mut out = String::with_capacity(t.len() + 64);
    let mut in_fm = false;
    let mut fm_done = false;
    let mut wrote_source = false;
    for (i, line) in t.lines().enumerate() {
        if line.trim_end() == "---" {
            if i == 0 {
                in_fm = true;
            } else if in_fm && !fm_done {
                if !wrote_source {
                    out.push_str(&format!("source: {}\n", placement.source));
                }
                in_fm = false;
                fm_done = true;
            }
            out.push_str("---\n");
            continue;
        }
        if in_fm && line.trim_start().starts_with("source:") {
            out.push_str(&format!("source: {}\n", placement.source));
            wrote_source = true;
            continue;
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
/// rulings, escalations and next steps, which are what a person reads to
/// find a wrong write, and what Seen will do.
pub fn card_body(note: &str, placement: &Placement) -> String {
    let mut s = String::new();
    for heading in [
        "## Summary",
        "## What It Overturns",
        "## Contradictions",
        "## Rulings Said in the Room",
        "## Escalations",
        "## Open Questions",
        "## Next Steps",
    ] {
        if let Some(start) = note.find(heading) {
            let rest = &note[start..];
            let end = rest[heading.len()..].find("\n## ").map_or(rest.len(), |e| e + heading.len());
            s.push_str(rest[..end].trim());
            s.push_str("\n\n");
        }
    }
    let folder = placement.note.rsplit_once('/').map_or(placement.note.as_str(), |(d, _)| d);
    s.push_str(&format!(
        "Note: {}\nWhat was written from it, and what waits, is on its card on the Ingest screen; each write \
         can be undone there. **Seen** moves the source from Raw to {folder}/.\n",
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

/// Part one: read one new source, write its note into its organized home and
/// file the Review card. The source stays in `Raw/` until [`file`].
/// `generate` turns the prompt into the note.
pub fn ingest_one(
    root: &Path,
    db: &mut Db,
    raw: &str,
    date: &str,
    now: i64,
    generate: impl FnOnce(&str) -> Result<String>,
) -> Result<Placement> {
    let abs = root.join(raw);
    let text = crate::extract::extract(&abs)?.text;
    if text.trim().is_empty() {
        return Err(Error::Other(format!("{raw} has no text Ken can read (a recording needs its transcript first)")));
    }
    let text = cap_source(text);
    let template = note_template(root);
    let declared = declared_kind(&text);
    let mut ask = prompt_against(&template, raw, &text, date, &wiki_context(db, &text));
    if let Some(k) = declared {
        ask.push_str(&format!("\nThe source says it is a {0}: set `kind: {0}` in the note's frontmatter.\n", k.as_str()));
    }
    let reply = generate(&ask)?;
    let kind = declared.unwrap_or_else(|| kind_of(strip_fences(&reply), raw));
    let placement = place(root, raw, date, kind);
    let note = finish_note(&reply, &placement)?;

    let note_abs = root.join(&placement.note);
    if let Some(dir) = note_abs.parent() {
        fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
    }
    fs::write(&note_abs, &note).map_err(|e| Error::io(&note_abs, e))?;

    let stem = placement.note.rsplit('/').next().unwrap_or(&placement.note).trim_end_matches(".md");
    let card = Card { placement: placement.clone(), note_hash: hash(&note), filed: false, writes: Vec::new(), listed: Vec::new(), undone: false };
    let payload = serde_json::to_string(&card).map_err(|e| Error::Other(e.to_string()))?;
    db.insert_review_item(REVIEW_KIND, &format!("Ingested: {stem}"), &card_body(&note, &placement), &placement.note, Some(&payload), now)?;
    Ok(placement)
}

/// Part two: the person has read the card (Seen); its source moves from
/// `Raw/` beside its note. Returns the card, filed.
pub fn file(root: &Path, card: &Card) -> Result<Card> {
    if card.filed {
        return Ok(card.clone());
    }
    let raw = root.join(&card.placement.raw);
    let dest = root.join(&card.placement.source);
    if !raw.exists() {
        return Err(Error::Other(format!("{} is no longer in Raw", card.placement.raw)));
    }
    if let Some(dir) = dest.parent() {
        fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
    }
    fs::rename(&raw, &dest).map_err(|e| Error::io(&raw, e))?;
    // A recording's generated transcript is keyed by its path: it moves too.
    crate::transcript::move_cached(root, &card.placement.raw, &card.placement.source);
    Ok(Card { filed: true, ..card.clone() })
}

// --- One pass over the inbox: what the app runs when a source lands, and
// what the evaluation harness runs headless. ----------------------------

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
/// transcribed first; each source becomes its note and what follows from it
/// is written ([`ingest_one`], [`follow_ups`]). A source that fails is
/// recorded and passed over, and a recording that needs a model or ffmpeg
/// waits; the pass stops only when the CLI itself fails
/// ([`CallError::Stop`]). Sources with a failure recorded are left until
/// someone tries them again ([`resolve_failure`]).
#[allow(clippy::too_many_arguments)]
pub fn run_pass(
    root: &Path,
    db: &mut Db,
    targets: &Targets,
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
        let done = ingest_one(root, db, &raw, today, now(), |p| match call(p) {
            Ok(text) => Ok(text),
            Err(CallError::Stop(m)) => {
                stop = Some(m.clone());
                Err(Error::Other(m))
            }
            Err(CallError::Source(m)) => Err(Error::Other(m)),
        });
        match done {
            Ok(placement) => {
                resolve_failure(db, &raw, now())?;
                let note = fs::read_to_string(root.join(&placement.note)).unwrap_or_default();
                let followed = follow_ups(root, db, &placement, &note, today, now(), targets, |p| match call(p) {
                    Ok(text) => Ok(text),
                    Err(CallError::Stop(m)) => {
                        stop = Some(m.clone());
                        Err(Error::Other(m))
                    }
                    Err(CallError::Source(m)) => Err(Error::Other(m)),
                });
                if let Err(e) = followed {
                    eprintln!("warning: ingest follow-ups for {raw} failed: {e}");
                }
                report.read.push(placement);
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

// --- What follows from a note. Written at once, each citing the note and
// recorded on the card: page edits and new pages, ideas, escalations, my
// next steps. Waiting, as Review items from the note: a ruling for its
// decider, a change to how the team works as a ticket, an edit staging
// held, and an action as a ticket. Nothing else waits. -------------------

/// Sections an ingest never writes to: evidence stays evidence, and the
/// templates and the wiki's own meta pages have their own ways in.
const OFF_LIMITS: [&str; 3] = ["Research/", "Templates/", "_meta/"];

/// Sections a change to goes to the team as a ticket, never as a write:
/// work and reviews are read against them. Ways-of-Working holds the rules
/// (`Ways-of-Working/Rules/`).
const METHOD: [&str; 2] = ["Ways-of-Working/", "Conventions/"];

/// The decisions log in a team repo, which wins over the wiki's.
pub const TEAM_DECISIONS: &str = "decisions/DECISIONS.md";

/// Most existing pages and new pages one note may change.
pub const MAX_PAGES: usize = 5;

/// Why an undo was refused: the file no longer reads as Ken wrote it.
pub const CHANGED_SINCE: &str = "The page changed since; open it to undo by hand.";

fn may_propose(page: &str) -> bool {
    page.ends_with(".md")
        && !page.contains("..")
        && !page.starts_with('/')
        && !OFF_LIMITS.iter().any(|p| page.starts_with(p))
        && !matches!(page, "START-HERE.md" | "CLAUDE.md" | "README.md")
}

/// A page of Ways-of-Working (its rules included) or Conventions: a change
/// to it is a ticket.
pub fn is_method_page(page: &str) -> bool {
    METHOD.iter().any(|p| page.starts_with(p))
}

/// The wiki's pages a note could change, with their titles, for the model to
/// choose from.
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

/// The plan: which pages the note changes, which new pages it calls for.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    pub update: Vec<PageChange>,
    pub create: Vec<NewPage>,
}

/// One page the note changes, and the change in a line.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PageChange {
    pub path: String,
    #[serde(default)]
    pub change: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewPage {
    pub path: String,
    #[serde(default)]
    pub purpose: String,
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
}

pub fn plan_prompt(pages: &[(String, String)], note: &str) -> String {
    let mut s = String::from(
        "A note was just ingested into a team's wiki. Decide which existing pages it changes (something they say is \
         no longer true, or something they should now say) and which new pages it calls for (a topic nothing covers \
         yet). Only what the note itself supports; most notes change a few pages, many change none. What you name is \
         written at once, citing the note; a change to Ways-of-Working or Conventions goes to the team as a ticket \
         instead, so name it the same way.\n\n\
         Reply with JSON only: {\"update\": [{\"path\": \"path\", \"change\": \"what changes, in one line\"}, …], \
         \"create\": [{\"path\": \"Section/Name.md\", \"purpose\": \"what the page is for\"}, …]}. \
         Use paths from the list for updates; put a new page in the section it belongs to (Current, Platform, \
         Design, Conventions, Ways-of-Working, Work, Reference). What is now true about the project or the team \
         changes its page in Current. At most 5 of each.\n\nPAGES IN THE WIKI:\n",
    );
    for (p, t) in pages {
        if t.is_empty() {
            s.push_str(&format!("- {p}\n"));
        } else {
            s.push_str(&format!("- {p} — {t}\n"));
        }
    }
    s.push_str(&format!("\nTHE NOTE:\n{note}\n"));
    s
}

/// The model's plan, kept to real pages to update and new pages that may be
/// made, at most [`MAX_PAGES`] of each.
pub fn read_plan(reply: &str, pages: &[(String, String)], root: &Path) -> Plan {
    let t = strip_fences(reply);
    let json = t.find('{').and_then(|a| t.rfind('}').map(|b| &t[a..=b])).unwrap_or("{}");
    let raw: RawPlan = serde_json::from_str(json).unwrap_or_default();
    let mut update: Vec<PageChange> = Vec::new();
    for entry in raw.update {
        let change = match entry {
            UpdateEntry::Path(path) => PageChange { path, change: String::new() },
            UpdateEntry::Change(c) => c,
        };
        if pages.iter().any(|(q, _)| *q == change.path) && !update.iter().any(|u| u.path == change.path) {
            update.push(change);
        }
    }
    update.truncate(MAX_PAGES);
    let mut create = raw.create;
    create.retain(|n| may_propose(&n.path) && !root.join(&n.path).exists());
    create.truncate(MAX_PAGES);
    Plan { update, create }
}

/// The prompt for one page edit from a note. The edit is written as it
/// comes back, so it changes only what the note supports.
pub fn edit_prompt(page: &str, change: &str, existing: &str, note_stem: &str, note: &str, today: &str) -> String {
    let current = if page.starts_with("Current/") {
        "- This page says what is true now: rewrite what changed in place. Never append a dated entry.\n"
    } else {
        ""
    };
    let change = if change.trim().is_empty() { "what the note changes or adds to this page".to_string() } else { change.trim().to_string() };
    format!(
        "You are editing one page of a team's wiki, `{page}`, from a note just ingested, [[{note_stem}]]. The edit is \
         written as you reply; a person reads it afterwards and can undo it. The change: {change}.\n\n\
         Rules:\n\
         - Change only what the note changes or adds to this page. Keep every other line exactly as it is, in the same order.\n\
         - Use only what the note says. Cite it as [[{note_stem}]] beside what you changed.\n\
         {current}\
         - Set `updated: {today}` in the frontmatter if there is one; leave any `verified:` line as it is.\n\
         - If the note changes nothing on this page, reply exactly NO CHANGE.\n\
         - Otherwise reply with the whole page, starting with its `---` frontmatter if it has one. No preamble, no code fences.\n\n\
         THE PAGE NOW:\n{existing}\n\nTHE NOTE ([[{note_stem}]]):\n{note}\n"
    )
}

/// The page with the note cited: as written when it cites it, else with a
/// line saying which note changed it.
fn cite(text: &str, note_stem: &str) -> String {
    if text.contains(&format!("[[{note_stem}")) {
        text.to_string()
    } else {
        format!("{}\n\nChanged from [[{note_stem}]].\n", text.trim_end())
    }
}

/// Staging's size rule: an edit that rewrites more than a fifth of its page,
/// by changed lines against the page's lines, is held for a person.
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

/// Why staging held an edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Held {
    /// It rewrites more than a fifth of its page.
    Large,
    /// The page no longer reads as it did when the run started: a person
    /// changed it, and Undo could not give their edit back.
    Changed,
}

/// Staging: whether an edit made against `base` (the page when the run
/// started) is written, or held. `now` is the page on disk as the edit
/// lands.
pub fn staging(base: &str, now: &str, proposed: &str) -> Option<Held> {
    if now.replace("\r\n", "\n") != base.replace("\r\n", "\n") {
        Some(Held::Changed)
    } else if rewrites_a_fifth(base, proposed) {
        Some(Held::Large)
    } else {
        None
    }
}

/// "The ruling — decider" lines from the note's "Rulings Said in the Room".
/// What follows the decider (". Still a quote.") is left out.
pub fn rulings_of(note: &str) -> Vec<(String, Option<String>)> {
    section_bullets(note, "## Rulings Said in the Room")
        .into_iter()
        .map(|l| match l.rsplit_once(" — ") {
            Some((ruling, decider)) => {
                let d = decider.trim();
                let d = d.split(". ").next().unwrap_or(d).trim().trim_end_matches('.').trim();
                (ruling.trim().to_string(), (!d.is_empty()).then(|| d.to_string()))
            }
            None => (l, None),
        })
        .collect()
}

/// A section's bullet lines, placeholders and "nothing" lines left out.
fn section_bullets(note: &str, heading: &str) -> Vec<String> {
    let Some(start) = note.find(heading) else { return Vec::new() };
    let rest = &note[start + heading.len()..];
    let body = &rest[..rest.find("\n## ").unwrap_or(rest.len())];
    body.lines()
        .map(str::trim)
        .filter_map(|l| l.strip_prefix("- "))
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.contains("{{") && !l.to_lowercase().starts_with("none") && !l.to_lowercase().starts_with("nothing"))
        .map(str::to_string)
        .collect()
}

/// The decisions log with a new entry appended: the next `D-nnn`, today, the
/// ruling in the decider's words, and the note it was said in as its source.
pub fn decisions_entry(log: &str, ruling: &str, decider: Option<&str>, date: &str, note_stem: &str) -> String {
    let next = log
        .lines()
        .map(crate::chunker::unbold_id)
        .filter_map(|l| l.trim().strip_prefix("D-").and_then(|r| r.split(|c: char| !c.is_ascii_digit()).next()?.parse::<u32>().ok()))
        .max()
        .unwrap_or(0)
        + 1;
    let topic: String = ruling.split_whitespace().take(6).collect::<Vec<_>>().join(" ");
    // In the log's own style: a bold id when its entries have one.
    let bold = log.lines().any(|l| crate::chunker::unbold_id(l).len() != l.len());
    let id = if bold { format!("**D-{next:03}**") } else { format!("D-{next:03}") };
    let mut out = log.trim_end().to_string();
    out.push_str(&format!(
        "\n\n{id} · {date} · {topic} — {ruling}\n  Why: said in the room; see the note.\n  sources: [[{note_stem}]]\n"
    ));
    if let Some(d) = decider {
        out.push_str(&format!("  decider: {d}\n"));
    }
    out
}

/// What an ingest's follow-ups did.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FollowUps {
    /// What was written, in order.
    pub written: Vec<Written>,
    /// Pages staging held, each now waiting as its diff.
    pub held: Vec<String>,
    /// Rulings waiting for their decider.
    pub rulings: usize,
    /// Tickets waiting in the team repo: the method change, then the actions.
    pub tickets: Vec<String>,
    /// What stayed on the card only.
    pub listed: Vec<Listed>,
}

/// Where an ingest writes besides the library: the team repo (ideas,
/// escalations, tickets, its decisions log), the workspace (Your day), and
/// who "me" is, for which next steps are mine.
pub struct Targets<'a> {
    pub team_repo: Option<&'a Path>,
    pub workspace: Option<&'a Path>,
    pub me: &'a crate::day::Me,
    /// `YYYY-MM-DDTHH:MM`, for a task's `created`.
    pub stamp: &'a str,
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

/// `I-030`, `E-002`, `SR-012`: an id in a numbered series.
fn is_series_id(s: &str) -> bool {
    let s = s.split(|c: char| c.is_whitespace() || c == ',').next().unwrap_or("");
    match s.rsplit_once('-') {
        Some((k, n)) => {
            !k.is_empty() && k.chars().all(|c| c.is_ascii_uppercase()) && !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())
        }
        None => false,
    }
}

/// An arrow that marks an idea (`→ idea, from arthur`, `→ I-030`).
fn is_idea_target(t: &str) -> bool {
    let l = t.trim().to_lowercase();
    l.starts_with("idea") || (is_series_id(t.trim()) && t.trim().starts_with("I-"))
}

/// "Action → who" lines from the note's "Actions and Requests", ideas left
/// out. An arrow that names Intake or a ticket names no one.
pub fn actions_of(note: &str) -> Vec<(String, Option<String>)> {
    section_bullets(note, "## Actions and Requests")
        .into_iter()
        .filter_map(|l| {
            let (what, to) = split_target(&l);
            if to.as_deref().is_some_and(is_idea_target) {
                return None;
            }
            let who = to.filter(|t| !t.to_lowercase().contains("intake") && !is_series_id(t));
            Some((what, who))
        })
        .collect()
}

/// The ideas in "Actions and Requests" (`- the idea → idea, from who`), with
/// who raised each.
pub fn ideas_of(note: &str) -> Vec<(String, Option<String>)> {
    section_bullets(note, "## Actions and Requests")
        .into_iter()
        .filter_map(|l| {
            let (what, to) = split_target(&l);
            let to = to?;
            if !is_idea_target(&to) {
                return None;
            }
            let rest = to.trim().split_once([' ', ',', '(']).map_or("", |(_, r)| r);
            let rest = rest.trim().trim_matches(|c: char| c == '(' || c == ')' || c == ',' || c == '.' || c.is_whitespace());
            let rest = rest.strip_prefix("from ").or_else(|| rest.strip_prefix("raised by ")).unwrap_or(rest).trim();
            Some((what, (!rest.is_empty()).then(|| rest.to_string())))
        })
        .collect()
}

/// "Question → who" lines from "Escalations", or from "Open Questions" in a
/// note written before the section had its name.
pub fn escalations_of(note: &str) -> Vec<(String, Option<String>)> {
    let mut lines = section_bullets(note, "## Escalations");
    if lines.is_empty() {
        lines = section_bullets(note, "## Open Questions");
    }
    lines.into_iter().map(|l| split_target(&l)).collect()
}

/// One line of "Next Steps": who will do what.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextStep {
    pub who: Option<String>,
    pub what: String,
    pub line: String,
}

/// "Who will what" lines from "Next Steps".
pub fn next_steps_of(note: &str) -> Vec<NextStep> {
    section_bullets(note, "## Next Steps")
        .into_iter()
        .map(|line| {
            let (left, _) = split_target(&line);
            let left = left.trim_end_matches('.').trim().to_string();
            match left.split_once(" will ") {
                Some((who, what)) => NextStep { who: Some(who.trim().to_string()), what: what.trim().to_string(), line },
                None => NextStep { who: None, what: left, line },
            }
        })
        .collect()
}

/// Whether a name said in the room is me: [`crate::day::Me::matches`], or
/// my first name alone, as people are named in a meeting.
pub fn is_me(me: &crate::day::Me, who: &str) -> bool {
    let who = who.trim().trim_start_matches('@').trim();
    if who.is_empty() {
        return false;
    }
    if me.matches(who) {
        return true;
    }
    let first = me.name.as_deref().and_then(|n| n.split_whitespace().next());
    !who.contains(' ') && first.is_some_and(|f| f.eq_ignore_ascii_case(who))
}

/// The ticket key and next number in a team repo's `tickets/`: the key its
/// tickets already use (`SR-012.md` → `SR`, 13), else the repo name's
/// initials.
pub fn next_ticket(team_repo: &Path) -> (String, u32) {
    let mut key: Option<String> = None;
    let mut max = 0;
    for e in fs::read_dir(team_repo.join("tickets")).into_iter().flatten().flatten() {
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
        let leaf = team_repo.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
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

/// The next number in a series of `<prefix>-nnn.md` files in `dir`
/// (`ideas/`, `escalations/`): one past the highest there, from 1.
pub fn next_number(dir: &Path, prefix: &str) -> u32 {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.strip_suffix(".md")?.strip_prefix(prefix)?.strip_prefix('-')?.parse::<u32>().ok()
        })
        .max()
        .unwrap_or(0)
        + 1
}

/// A YAML string, quoted.
fn yaml_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// One ticket in the method's format (`templates/team/tickets/TICKET.md`).
fn ticket_doc(id: &str, kind: &str, who: Option<&str>, what_and_why: &str, note_stem: &str) -> String {
    format!(
        "---\nid: {id}\nkind: ticket\nstatus: todo\ntype: {kind}\nsize:\nscope: []\nverify: []\nassignee: {}\n---\n\n\
         ## What and Why\n\n{what_and_why}\n\n\
         ## Evidence\n\nSaid in [[{note_stem}]].\n\n\
         ## What Not to Do\n\n(not known yet)\n\n\
         ## How You Would Know It Worked\n\n(to be written before the work starts)\n\n\
         ## thread\n\n",
        who.unwrap_or("")
    )
}

/// One ticket from one action said in a note.
pub fn ticket_text(id: &str, action: &str, who: Option<&str>, note_stem: &str) -> String {
    ticket_doc(id, "", who, &format!("{}. Asked for in [[{note_stem}]].", action.trim_end_matches('.')), note_stem)
}

/// The one ticket for every change a note calls for to Ways-of-Working,
/// Conventions or a rule.
pub fn method_ticket_text(id: &str, changes: &[PageChange], note_stem: &str) -> String {
    let mut w = format!(
        "[[{note_stem}]] calls for changes to how the team works. Work and reviews are read against these pages, so \
         they change through this ticket, not an edit:\n"
    );
    for c in changes {
        let change = if c.change.trim().is_empty() { "what the note says about it" } else { c.change.trim() };
        w.push_str(&format!("\n- `{}`: {change}", c.path));
    }
    ticket_doc(id, "process", None, &w, note_stem)
}

/// An idea in the team repo's `ideas/`, from `templates/team/ideas/IDEA.md`:
/// raised by Wright, unseen until a person sees it.
pub fn idea_text(id: &str, idea: &str, from: Option<&str>, date: &str, note_stem: &str) -> String {
    let idea = idea.trim().trim_end_matches('.');
    format!(
        "---\nid: {id}\nkind: idea\ntitle: {}\narea:\nraised_by: wright\nseen: false\nraised: {date}\ntimes_raised: 1\n\
         source: ingest\nstatus: open\nwent_to:\nreason:\n---\n\n{idea}.{} From [[{note_stem}]].\n",
        yaml_str(idea),
        from.map(|f| format!(" Raised by {f}.")).unwrap_or_default()
    )
}

/// An escalation in the team repo's `escalations/`, from
/// `templates/team/escalations/ESCALATION.md`: to the person named, open,
/// the question as its title, the note linked.
pub fn escalation_text(id: &str, question: &str, to: Option<&str>, date: &str, note_stem: &str) -> String {
    let question = question.trim();
    format!(
        "---\nid: {id}\nraised_by: ingest of {note_stem}\nto: {}\nticket:\nraised: {date}\nstatus: open\n\
         blocks: nothing\nanswer:\n---\n\n# {question}\n\n## Options\n\n- (none drafted)\n\n## Recommendation\n\n\
         (none)\n\n## Thread\n\n- **ingest, {date}:** {question} Raised in [[{note_stem}]].\n",
        to.unwrap_or("")
    )
}

/// A note's key takeaways, section by section, for the Ingest card. A
/// section the note lacks is empty. A note from before "Escalations" had
/// its name reads its "Open Questions" as escalations.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Takeaways {
    pub title: String,
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
    pub escalations: Vec<String>,
    pub next_steps: Vec<String>,
}

pub fn takeaways(note: &str) -> Takeaways {
    let section = |heading: &str| -> String {
        let Some(start) = note.find(heading) else { return String::new() };
        let rest = &note[start + heading.len()..];
        let end = rest.find("\n## ").unwrap_or(rest.len());
        let body = rest[..end].trim();
        if body.contains("{{") { String::new() } else { body.to_string() }
    };
    let front = |key: &str| -> String {
        note.lines()
            .take_while(|l| !l.starts_with("# "))
            .find_map(|l| l.trim_start().strip_prefix(&format!("{key}:")).map(|v| v.trim().trim_matches('"').to_string()))
            .unwrap_or_default()
    };
    // A section that says there is nothing ("None.", "- none") has no rows.
    let rows = |heading: &str| -> Vec<String> {
        section_bullets(note, heading)
            .into_iter()
            .filter(|b| !matches!(b.trim_end_matches('.').to_lowercase().as_str(), "none" | "nothing" | "n/a" | "none found"))
            .collect()
    };
    let mut escalations = rows("## Escalations");
    if escalations.is_empty() {
        escalations = rows("## Open Questions");
    }
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
        kind: front("kind"),
        present: front("present").trim_matches(['[', ']']).split(',').map(str::trim).filter(|s| !s.is_empty() && !s.contains("{{")).map(str::to_string).collect(),
        length,
        summary: section("## Summary"),
        key_takeaways: rows("## Key Takeaways"),
        overturns: section("## What It Overturns"),
        contradictions: rows("## Contradictions"),
        rulings: rows("## Rulings Said in the Room"),
        actions: rows("## Actions and Requests"),
        escalations,
        next_steps: rows("## Next Steps"),
    }
}

/// What waits from a note (rulings, tickets, held edits), each a Review
/// item a person accepts, applies or discards.
pub fn proposals_from(db: &Db, note: &str) -> Result<Vec<crate::db::ReviewItemRow>> {
    Ok(db
        .list_open_review_items()?
        .into_iter()
        .filter(|it| {
            it.kind == crate::wikidraft::PROPOSAL_KIND
                && it
                    .payload
                    .as_deref()
                    .and_then(|p| serde_json::from_str::<crate::wikidraft::Proposal>(p).ok())
                    .and_then(|p| p.from)
                    .as_deref()
                    == Some(note)
        })
        .collect())
}

/// How many proposals wait from each note, counted from open items already
/// listed: one pass, where calling [`proposals_from`] for every note lists
/// all the open items again each time.
pub fn proposal_counts(open: &[crate::db::ReviewItemRow]) -> std::collections::HashMap<String, usize> {
    let mut out = std::collections::HashMap::new();
    for it in open.iter().filter(|it| it.kind == crate::wikidraft::PROPOSAL_KIND) {
        let from = it
            .payload
            .as_deref()
            .and_then(|p| serde_json::from_str::<crate::wikidraft::Proposal>(p).ok())
            .and_then(|p| p.from);
        if let Some(from) = from {
            *out.entry(from).or_insert(0) += 1;
        }
    }
    out
}

/// Close what an undone ingest left waiting: every open item from its note
/// is resolved. Returns how many.
pub fn withdraw(db: &mut Db, note: &str, now: i64) -> Result<usize> {
    let mut n = 0;
    for it in db.list_open_review_items()? {
        if it.kind != crate::wikidraft::PROPOSAL_KIND {
            continue;
        }
        let from = it
            .payload
            .as_deref()
            .and_then(|p| serde_json::from_str::<crate::wikidraft::Proposal>(p).ok())
            .and_then(|p| p.from);
        if from.as_deref() == Some(note) {
            db.resolve_review_item(it.id, now)?;
            n += 1;
        }
    }
    Ok(n)
}

fn write_file(path: &Path, text: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
    }
    fs::write(path, text).map_err(|e| Error::io(path, e))
}

/// Store what was written so far on the card, so Undo has it after a
/// restart and a crash mid-run loses nothing it wrote.
fn record(db: &mut Db, item: Option<i64>, card: &mut Option<Card>, out: &FollowUps) -> Result<()> {
    let (Some(id), Some(c)) = (item, card.as_mut()) else { return Ok(()) };
    c.writes = out.written.clone();
    c.listed = out.listed.clone();
    let payload = serde_json::to_string(c).map_err(|e| Error::Other(e.to_string()))?;
    db.set_review_item_payload(id, &payload)
}

fn listed(kind: &str, text: &str, to: Option<String>) -> Listed {
    Listed { kind: kind.to_string(), text: text.to_string(), to }
}

/// A page's name as a row says it: `Ways-of-Working/Sizes.md` → `Sizes`.
fn page_name(path: &str) -> &str {
    let leaf = path.rsplit('/').next().unwrap_or(path);
    leaf.strip_suffix(".md").unwrap_or(leaf)
}

/// After a note is written: ask which pages it changes and which new pages
/// it calls for, write each (staging may hold one), then the rest of what
/// it calls for. Each write cites the note and is recorded on the source's
/// card as it lands. `generate` is asked once for the plan, then once per
/// page.
#[allow(clippy::too_many_arguments)]
pub fn follow_ups(
    root: &Path,
    db: &mut Db,
    placement: &Placement,
    note: &str,
    date: &str,
    now: i64,
    targets: &Targets,
    mut generate: impl FnMut(&str) -> Result<String>,
) -> Result<FollowUps> {
    use crate::wikidraft::{file_page_proposal, Proposal};
    let from = Some(placement.note.clone());
    let mut out = FollowUps::default();
    let stem = placement.note.rsplit('/').next().unwrap_or(&placement.note).trim_end_matches(".md").to_string();
    let source = [crate::wikidraft::Source { label: format!("[[{stem}]]"), text: note.to_string() }];
    let item = db.list_open_review_items()?.into_iter().find(|it| it.kind == REVIEW_KIND && it.source_ref == placement.note);
    let item_id = item.as_ref().map(|it| it.id);
    let mut card = item.and_then(|it| card_of(it.payload.as_deref()));

    let pages = page_list(db)?;
    let plan = generate(&plan_prompt(&pages, note)).map(|r| read_plan(&r, &pages, root)).unwrap_or_default();
    // Each page as it read when the run started: staging holds an edit to a
    // page that moved since, and Undo restores this.
    let bases: Vec<(PageChange, String)> =
        plan.update.iter().filter_map(|c| fs::read_to_string(root.join(&c.path)).ok().map(|t| (c.clone(), t))).collect();
    let mut method: Vec<PageChange> = Vec::new();

    for (change, base) in &bases {
        let page = &change.path;
        if is_method_page(page) {
            method.push(change.clone());
            continue;
        }
        let reply = generate(&edit_prompt(page, &change.change, base, &stem, note, date))
            .and_then(|r| crate::wikidraft::finish_update(&r, base));
        let Ok(Some(proposed)) = reply else { continue };
        let proposed = cite(&proposed, &stem);
        let on_disk = fs::read_to_string(root.join(page)).unwrap_or_default();
        match staging(base, &on_disk, &proposed) {
            None => {
                write_file(&root.join(page), &proposed)?;
                out.written.push(Written {
                    kind: WriteKind::Edit,
                    path: page.clone(),
                    root: None,
                    label: change.change.clone(),
                    to: None,
                    base: Some(base.clone()),
                    hash: text_hash(&proposed),
                    task_id: None,
                    undone: false,
                });
                record(db, item_id, &mut card, &out)?;
            }
            Some(why) => {
                let reason = match why {
                    Held::Large => "it rewrites more than a fifth of the page",
                    Held::Changed => "the page changed while the source was read",
                };
                let p = Proposal { from: from.clone(), ..Proposal::new(page.clone(), on_disk, proposed) };
                let body = format!(
                    "Staging held this edit from {stem}: {reason}. Read the change and apply it, or discard it. It \
                     applies only while the page still reads as it does now."
                );
                file_page_proposal(db, &p, &format!("Held: {page} from {stem}"), &body, now)?;
                out.held.push(page.clone());
            }
        }
    }

    for new in &plan.create {
        if is_method_page(&new.path) {
            method.push(PageChange { path: new.path.clone(), change: format!("a new page: {}", new.purpose.trim()) });
            continue;
        }
        let reply = generate(&crate::wikidraft::prompt(&new.path, &new.purpose, None, &source, date))
            .and_then(|r| crate::wikidraft::finish(&r));
        let Ok(proposed) = reply else { continue };
        let proposed = cite(&proposed, &stem);
        let path = root.join(&new.path);
        if let Ok(made) = fs::read_to_string(&path) {
            // A person made a page by that name while the source was read.
            let p = Proposal { from: from.clone(), ..Proposal::new(new.path.clone(), made, proposed) };
            let body = format!(
                "Staging held this page from {stem}: a page by that name was made while the source was read. Read \
                 the change and apply it, or discard it."
            );
            file_page_proposal(db, &p, &format!("Held: {} from {stem}", new.path), &body, now)?;
            out.held.push(new.path.clone());
            continue;
        }
        write_file(&path, &proposed)?;
        out.written.push(Written {
            kind: WriteKind::Page,
            path: new.path.clone(),
            root: None,
            label: new.purpose.clone(),
            to: None,
            base: None,
            hash: text_hash(&proposed),
            task_id: None,
            undone: false,
        });
        record(db, item_id, &mut card, &out)?;
    }

    // Rulings wait for their decider, in the team repo's log when it has one.
    let rulings = rulings_of(note);
    if !rulings.is_empty() {
        let team_log = targets.team_repo.filter(|t| t.join(TEAM_DECISIONS).is_file());
        let (log_root, log) = match team_log {
            Some(t) => (Some(t.to_string_lossy().to_string()), TEAM_DECISIONS.to_string()),
            None => (None, db.paths_named(&["decisions.md"])?.into_iter().next().unwrap_or_else(|| "_meta/DECISIONS.md".to_string())),
        };
        let log_abs = log_root.as_deref().map(PathBuf::from).unwrap_or_else(|| root.to_path_buf()).join(&log);
        let text = fs::read_to_string(&log_abs).unwrap_or_else(|_| "# Decisions\n".to_string());
        for (ruling, decider) in &rulings {
            // Shown as the entry against the log now; applied against the log
            // as it is then, so rulings from one note apply in any order.
            let proposed = decisions_entry(&text, ruling, decider.as_deref(), date, &stem);
            let p = Proposal {
                append: Some(crate::wikidraft::RulingEntry {
                    ruling: ruling.clone(),
                    decider: decider.clone(),
                    date: date.to_string(),
                    note: stem.clone(),
                }),
                root: log_root.clone(),
                from: from.clone(),
                ..Proposal::new(log.clone(), text.clone(), proposed)
            };
            let who = decider.as_deref().unwrap_or("its decider");
            let title = format!("Ruling for {who}: {}", ruling.chars().take(60).collect::<String>());
            let body = format!(
                "Said in the room, per {stem}: \"{ruling}\". A ruling is written by its decider: {who} accepts it into \
                 the decisions log, or drops it. It takes the next number when accepted."
            );
            file_page_proposal(db, &p, &title, &body, now)?;
            out.rulings += 1;
        }
    }

    // A change to how the team works is one ticket; each action is a ticket.
    let actions = actions_of(note);
    match targets.team_repo {
        Some(team) => {
            let (key, mut next) = next_ticket(team);
            let team_name = team.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            if !method.is_empty() {
                let id = format!("{key}-{next:03}");
                next += 1;
                let page = format!("tickets/{id}.md");
                let names = method.iter().map(|c| page_name(&c.path)).collect::<Vec<_>>().join(", ");
                let p = Proposal {
                    root: Some(team.to_string_lossy().to_string()),
                    from: from.clone(),
                    ..Proposal::new(page.clone(), "", method_ticket_text(&id, &method, &stem))
                };
                let n = method.len();
                let title = format!(
                    "Ticket {id} in {team_name}: {n} {} to Ways-of-Working ({names})",
                    if n == 1 { "change" } else { "changes" }
                );
                let body = format!(
                    "{stem} calls for changes to how the team works: {names}. Work and reviews are read against \
                     those pages, so they change through a ticket. Ken drafted {page} in {team_name}, citing the \
                     note; accept it to write it, or discard it."
                );
                file_page_proposal(db, &p, &title, &body, now)?;
                out.tickets.push(id);
            }
            for (action, who) in &actions {
                let id = format!("{key}-{next:03}");
                next += 1;
                let page = format!("tickets/{id}.md");
                let p = Proposal {
                    root: Some(team.to_string_lossy().to_string()),
                    from: from.clone(),
                    ..Proposal::new(page.clone(), "", ticket_text(&id, action, who.as_deref(), &stem))
                };
                let title = format!("Ticket {id} in {team_name}: {}", action.chars().take(60).collect::<String>());
                let body = format!(
                    "An action from {stem}{}. Ken drafted {page} in {team_name} in the method's ticket format; accept \
                     it to write it, or discard it. Its type and size are for the team to set.",
                    who.as_deref().map(|w| format!(", for {w}")).unwrap_or_default()
                );
                file_page_proposal(db, &p, &title, &body, now)?;
                out.tickets.push(id);
            }
        }
        None => {
            for c in &method {
                out.listed.push(listed("ticket", &format!("{}: {}", c.path, c.change), None));
            }
            for (action, who) in &actions {
                out.listed.push(listed("ticket", action, who.clone()));
            }
        }
    }

    // Ideas and escalations go to the team repo, written at once.
    let ideas = ideas_of(note);
    let escalations = escalations_of(note);
    match targets.team_repo {
        Some(team) => {
            let team_root = Some(team.to_string_lossy().to_string());
            for (idea, by) in &ideas {
                let id = format!("I-{:03}", next_number(&team.join("ideas"), "I"));
                let rel = format!("ideas/{id}.md");
                let text = idea_text(&id, idea, by.as_deref(), date, &stem);
                write_file(&team.join(&rel), &text)?;
                out.written.push(Written {
                    kind: WriteKind::Idea,
                    path: rel,
                    root: team_root.clone(),
                    label: idea.clone(),
                    to: by.clone(),
                    base: None,
                    hash: text_hash(&text),
                    task_id: None,
                    undone: false,
                });
                record(db, item_id, &mut card, &out)?;
            }
            for (question, to) in &escalations {
                let id = format!("E-{:03}", next_number(&team.join("escalations"), "E"));
                let rel = format!("escalations/{id}.md");
                let text = escalation_text(&id, question, to.as_deref(), date, &stem);
                write_file(&team.join(&rel), &text)?;
                out.written.push(Written {
                    kind: WriteKind::Escalation,
                    path: rel,
                    root: team_root.clone(),
                    label: question.clone(),
                    to: to.clone(),
                    base: None,
                    hash: text_hash(&text),
                    task_id: None,
                    undone: false,
                });
                record(db, item_id, &mut card, &out)?;
            }
        }
        None => {
            for (idea, by) in &ideas {
                out.listed.push(listed("idea", idea, by.clone()));
            }
            for (question, to) in &escalations {
                out.listed.push(listed("escalation", question, to.clone()));
            }
        }
    }

    // My next steps go on my day; anyone else's stay on the card.
    for step in next_steps_of(note) {
        let mine = step.who.as_deref().is_some_and(|w| is_me(targets.me, w));
        let task = match (mine, targets.workspace) {
            (true, Some(ws)) => {
                let mut title = step.what.clone();
                if let Some(first) = title.get(..1) {
                    title = format!("{}{}", first.to_uppercase(), &title[1..]);
                }
                let input = crate::day::DayTaskInput {
                    title,
                    links: Some(vec![placement.note.clone()]),
                    description: Some(format!("From [[{stem}]]: {}", step.line)),
                    ..Default::default()
                };
                let stamp = crate::day::Stamp { today: date, now: targets.stamp, by: crate::day::BY_INGEST };
                crate::day::create_task(ws, &input, &stamp, None).ok().map(|t| (ws, t))
            }
            _ => None,
        };
        match task {
            Some((ws, t)) => {
                let text = fs::read_to_string(&t.path).unwrap_or_default();
                let rel = t.path.strip_prefix(ws).unwrap_or(&t.path).to_string_lossy().replace('\\', "/");
                out.written.push(Written {
                    kind: WriteKind::Task,
                    path: rel,
                    root: Some(ws.to_string_lossy().to_string()),
                    label: t.title.clone(),
                    to: step.who.clone(),
                    base: None,
                    hash: text_hash(&text),
                    task_id: Some(t.id.clone()),
                    undone: false,
                });
            }
            None => out.listed.push(listed("next step", &step.line, step.who.clone())),
        }
    }
    record(db, item_id, &mut card, &out)?;
    Ok(out)
}

/// Whether an undo was refused because the file moved on since Ken wrote it.
pub fn refused(e: &Error) -> bool {
    matches!(e, Error::Other(m) if m == CHANGED_SINCE)
}

/// Reverse one write: an edit's page goes back to how it read before, a
/// created page, idea or escalation is removed, a task is archived. A file
/// that no longer reads exactly as written is left, and the undo refused
/// ([`CHANGED_SINCE`]). A created file already gone is already undone.
pub fn undo_write(wiki: &Path, w: &Written, today: &str) -> Result<()> {
    if w.undone {
        return Ok(());
    }
    let root = w.root.as_deref().map(PathBuf::from).unwrap_or_else(|| wiki.to_path_buf());
    let path = root.join(&w.path);
    match w.kind {
        WriteKind::Task => {
            let home = crate::tasks::TaskHome::Workspace { workspace_root: &root };
            let tasks = crate::day::list_home(home, today);
            if let Some(t) = crate::day::find(&tasks, w.task_id.as_deref().unwrap_or("")) {
                crate::day::archive_task(t, today)?;
            }
            Ok(())
        }
        WriteKind::Edit => {
            let now = fs::read_to_string(&path).map_err(|_| Error::Other(CHANGED_SINCE.into()))?;
            if text_hash(&now) != w.hash {
                return Err(Error::Other(CHANGED_SINCE.into()));
            }
            fs::write(&path, w.base.as_deref().unwrap_or("")).map_err(|e| Error::io(&path, e))
        }
        WriteKind::Page | WriteKind::Idea | WriteKind::Escalation => match fs::read_to_string(&path) {
            Err(_) => Ok(()),
            Ok(now) if text_hash(&now) == w.hash => fs::remove_file(&path).map_err(|e| Error::io(&path, e)),
            Ok(_) => Err(Error::Other(CHANGED_SINCE.into())),
        },
    }
}

/// [`undo_write`] on the card's write at `index`, marked undone when it is.
pub fn undo_write_at(wiki: &Path, card: &mut Card, index: usize, today: &str) -> Result<()> {
    let w = card.writes.get_mut(index).ok_or_else(|| Error::Other("no such write on this card".into()))?;
    undo_write(wiki, w, today)?;
    w.undone = true;
    Ok(())
}

/// What Undo all did.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoReport {
    /// The note was removed (else a person edited it, and it stays).
    pub note_removed: bool,
    /// Writes left in place because their file changed since, by label.
    pub kept: Vec<String>,
}

/// Undo all: every write from the source, last first, then the note and the
/// move of the source ([`undo`]). A write whose file changed since is kept
/// and named. The caller closes what waits ([`withdraw`]).
pub fn undo_all(root: &Path, card: &mut Card, today: &str) -> Result<UndoReport> {
    let mut kept = Vec::new();
    for w in card.writes.iter_mut().rev() {
        if w.undone {
            continue;
        }
        match undo_write(root, w, today) {
            Ok(()) => w.undone = true,
            Err(e) if refused(&e) => kept.push(if w.label.is_empty() { w.path.clone() } else { w.label.clone() }),
            Err(e) => return Err(e),
        }
    }
    let note_removed = undo(root, card)?;
    // The source is back in Raw/; an open, unfiled card keeps it out of the
    // next pass, which would otherwise write everything again.
    card.filed = false;
    card.undone = true;
    Ok(UndoReport { note_removed, kept })
}

/// Undo one ingest's two parts: a filed source goes back to `Raw/`, and the
/// note is removed unless someone edited it since (then it is kept and said
/// so). Returns whether the note was removed. [`undo_all`] calls this after
/// reversing the writes.
pub fn undo(root: &Path, card: &Card) -> Result<bool> {
    let placement = &card.placement;
    let src = root.join(&placement.source);
    let raw = root.join(&placement.raw);
    if card.filed && src.exists() {
        if let Some(dir) = raw.parent() {
            fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        }
        fs::rename(&src, &raw).map_err(|e| Error::io(&src, e))?;
        if let Some(dir) = src.parent() {
            let _ = fs::remove_dir(dir); // only when empty
        }
    }
    let note = root.join(&placement.note);
    let untouched = match fs::read_to_string(&note) {
        Ok(now) => hash(&now) == card.note_hash,
        Err(_) => return Ok(false),
    };
    if untouched {
        fs::remove_file(&note).map_err(|e| Error::io(&note, e))?;
    }
    Ok(untouched)
}

/// The card stored on an ingest Review item.
pub fn card_of(payload: Option<&str>) -> Option<Card> {
    payload.and_then(|p| serde_json::from_str(p).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    const REPLY: &str = "```markdown\n---\ntitle: \"Standup - 2026-09-24\"\nstatus: evidence\nkind: meeting\nsource: wrong/path.txt\npresent: [Ana, Ben]\n---\n\n# Standup - 2026-09-24\n\nSource: `Standup notes.txt` (12 min).\n\n## Summary\n\nThe team moved the ship date.\n\n## Key Takeaways\n\n- We ship on Friday.\n\n## What It Overturns\n\n**Ship date is Friday, not Monday.** Current/Project.md said Monday.\n\n- Ana: *\"we ship on Friday\"*\n\n## What Was Said\n\n- **Ship.** Ana: *\"we ship on Friday\"*.\n\n## Actions and Requests\n\n- Fix the save bug → Ben.\n\n## Next Steps\n\n- Ben will fix the save bug.\n```";

    fn me() -> crate::day::Me {
        crate::day::Me { name: Some("Chris Staud".into()), email: Some("chris.staud@example.com".into()) }
    }

    fn card(p: &Placement, note: &str) -> Card {
        Card { placement: p.clone(), note_hash: hash(note), filed: false, writes: Vec::new(), listed: Vec::new(), undone: false }
    }

    #[test]
    fn a_note_is_read_as_its_takeaways() {
        let note = "---\ntitle: \"Review - 2026-09-22\"\nkind: meeting\npresent: [chris, dee]\n---\n\n# Review - 2026-09-22\n\n\
                    Source: `review.vtt` (48 min). Ends mid-sentence.\n\n\
                    ## Summary\n\nThe room read presets as effort.\n\n\
                    ## Key Takeaways\n\n- Presets are effort.\n- Design is a ticket.\n\n\
                    ## What It Overturns\n\n**Presets are effort, not size.** Sizes page says otherwise.\n\n\
                    ## Contradictions\n\n- The Sizes page says a preset is a size — Ways-of-Working/Sizes.md.\n\n\
                    ## Rulings Said in the Room\n\n- Presets are level of effort — chris. Still a quote.\n\n\
                    ## Actions and Requests\n\n- Rewrite the sizes page → chris.\n\n\
                    ## Escalations\n\n- Which Jira types map to tune? → kate.\n\n\
                    ## Next Steps\n\n- chris will rewrite the sizes page this week.\n";
        let t = takeaways(note);
        assert_eq!(t.title, "Review - 2026-09-22");
        assert_eq!(t.kind, "meeting");
        assert_eq!(t.present, vec!["chris", "dee"]);
        assert_eq!(t.length, "48 min");
        assert_eq!(t.summary, "The room read presets as effort.");
        assert_eq!(t.key_takeaways, vec!["Presets are effort.", "Design is a ticket."]);
        assert!(t.overturns.starts_with("**Presets are effort"));
        assert_eq!(t.contradictions, vec!["The Sizes page says a preset is a size — Ways-of-Working/Sizes.md."]);
        assert_eq!(t.rulings.len(), 1);
        assert_eq!(rulings_of(note), vec![("Presets are level of effort".to_string(), Some("chris".to_string()))]);
        assert_eq!(t.actions, vec!["Rewrite the sizes page → chris."]);
        assert_eq!(t.escalations, vec!["Which Jira types map to tune? → kate."]);
        assert_eq!(t.next_steps, vec!["chris will rewrite the sizes page this week."]);
    }

    #[test]
    fn a_note_with_the_old_headings_still_parses() {
        let old = "---\ntitle: x\n---\n\n# Old\n\n## What It Overturns\n\nNothing.\n\n\
                   ## Actions and Requests\n\n- Fix it → Ben.\n\n## Open Questions\n\n- Which save format? → Ana.\n- None.\n";
        let t = takeaways(old);
        assert_eq!(t.escalations, vec!["Which save format? → Ana."], "Open Questions read as escalations");
        assert!(t.summary.is_empty() && t.key_takeaways.is_empty() && t.next_steps.is_empty());
        assert_eq!(escalations_of(old), vec![("Which save format?".to_string(), Some("Ana".to_string()))]);
        assert_eq!(actions_of(old), vec![("Fix it".to_string(), Some("Ben".to_string()))]);
        // The template's own placeholders are no takeaways.
        let blank = takeaways(DEFAULT_TEMPLATE);
        assert!(blank.summary.is_empty() && blank.key_takeaways.is_empty() && blank.escalations.is_empty() && blank.length.is_empty());
    }

    #[test]
    fn the_note_is_written_against_what_the_wiki_says() {
        let d = library();
        let root = d.path();
        fs::create_dir_all(root.join("Current")).unwrap();
        fs::write(root.join("Current/Project.md"), "# Project\n\nThe release ships on Monday after the payment review.\n").unwrap();
        fs::write(root.join(format!("{INGESTED}/old.md")), "# Old\n\nThe release ships on Monday after the payment review.\n").unwrap();
        let project = crate::project::Project::create(root, "Wiki").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        crate::scan::scan(&project, &mut db).unwrap();

        let source = "Ana: the release ships Friday. The release is Friday, the payment review moved.";
        let wiki = wiki_context(&db, source);
        assert!(wiki.contains("=== Current/Project.md ===") && wiki.contains("ships on Monday"), "{wiki}");
        assert!(!wiki.contains(INGESTED), "the inbox is not what the wiki says: {wiki}");
        let p = prompt_against(DEFAULT_TEMPLATE, "Research/Ingestion/Raw/standup.txt", source, "2026-09-24", &wiki);
        assert!(p.contains("WHAT THE WIKI SAYS NOW") && p.contains("Contradictions"), "{p}");
        assert!(p.contains("## Key Takeaways") && p.contains("## Escalations") && p.contains("## Next Steps"), "the new template");
        assert!(p.contains("nobody to confirm it") && p.contains("→ idea, from"), "{p}");
    }

    #[test]
    fn processing_writes_the_note_in_its_home_and_leaves_the_source_for_its_card() {
        let d = library();
        let root = d.path();
        let raw = format!("{RAW}/Standup notes.txt");
        assert_eq!(waiting(root), vec![raw.clone()]);
        let mut db = Db::open_in_memory().unwrap();
        let mut seen_prompt = String::new();
        let p = ingest_one(root, &mut db, &raw, "2026-09-24", 100, |prompt| {
            seen_prompt = prompt.to_string();
            Ok(REPLY.to_string())
        })
        .unwrap();

        assert_eq!(p.note, format!("{INGESTED}/Meetings/2026-09/2026-09-24-standup-notes.md"));
        assert_eq!(p.source, format!("{INGESTED}/Meetings/2026-09/2026-09-24-standup-notes/Standup notes.txt"));
        assert!(seen_prompt.contains("we ship on Friday") && seen_prompt.contains("Set `kind:`"));
        assert!(root.join(&raw).exists(), "the source waits in Raw until it is seen");
        let note = fs::read_to_string(root.join(&p.note)).unwrap();
        assert!(note.starts_with("---\n") && note.contains(&format!("source: {}", p.source)) && !note.contains("wrong/path"));
        assert_eq!(waiting(root), vec![raw.clone()]);
        assert!(waiting_new(root, &db).unwrap().is_empty(), "not read twice");
        assert_eq!(in_review(&db).unwrap(), vec![raw.clone()]);

        let (_, body) = db.open_review_item_of_kind(REVIEW_KIND).unwrap().unwrap();
        assert!(body.contains("The team moved the ship date") && body.contains("Ship date is Friday"), "{body}");
        assert!(body.contains("Ben will fix the save bug") && body.contains("**Seen**"), "{body}");
        let card = card_of(db.list_open_review_items().unwrap()[0].payload.as_deref()).unwrap();
        assert!(!card.filed && card.writes.is_empty());

        // Seen moves the source beside its note.
        let filed = file(root, &card).unwrap();
        assert!(filed.filed && root.join(&p.source).exists() && !root.join(&raw).exists());
        assert!(file(root, &filed).is_ok(), "filing twice is a no-op");

        // Undo takes both parts back.
        assert!(undo(root, &filed).unwrap());
        assert_eq!(waiting(root), vec![raw.clone()]);
        assert!(!root.join(&p.note).exists());
    }

    #[test]
    fn a_card_from_before_writes_were_recorded_still_reads() {
        let old = "{\"raw\":\"r\",\"note\":\"n\",\"source\":\"s\",\"noteHash\":\"h\",\"filed\":false}";
        let c = card_of(Some(old)).unwrap();
        assert!(c.writes.is_empty() && c.listed.is_empty() && !c.filed);
    }

    #[test]
    fn an_edited_note_survives_undo_and_a_second_source_does_not_collide() {
        let d = library();
        let root = d.path();
        let mut db = Db::open_in_memory().unwrap();
        let raw = format!("{RAW}/Standup notes.txt");
        let p = ingest_one(root, &mut db, &raw, "2026-09-24", 1, |_| Ok(REPLY.into())).unwrap();
        let written = fs::read_to_string(root.join(&p.note)).unwrap();
        fs::write(root.join(&p.note), format!("{written}\nA person added this.\n")).unwrap();
        assert!(!undo(root, &card(&p, &written)).unwrap(), "edited: kept");
        assert!(root.join(&p.note).exists() && root.join(&raw).exists());

        let again = place(root, &raw, "2026-09-24", SourceKind::Meeting);
        assert_eq!(again.note, format!("{INGESTED}/Meetings/2026-09/2026-09-24-standup-notes-2.md"));
        let doc = place(root, &raw, "2026-10-02", SourceKind::Document);
        assert_eq!(doc.note, format!("{INGESTED}/Documents/2026-10/2026-10-02-standup-notes.md"));
    }

    #[test]
    fn a_reply_without_frontmatter_is_refused_and_nothing_is_written() {
        let d = library();
        let root = d.path();
        let mut db = Db::open_in_memory().unwrap();
        let raw = format!("{RAW}/Standup notes.txt");
        assert!(ingest_one(root, &mut db, &raw, "2026-09-24", 1, |_| Ok("Sure! Here is the note.".into())).is_err());
        assert!(root.join(&raw).exists());
        assert!(db.open_review_item_of_kind(REVIEW_KIND).unwrap().is_none());
    }

    #[test]
    fn the_kind_comes_from_the_note_else_from_the_source() {
        assert_eq!(kind_of("---\nkind: recording\npresent: [Ana]\n---\n", "Raw/x.txt"), SourceKind::Recording);
        assert_eq!(kind_of("---\ntitle: x\n---\n", "Raw/call.vtt"), SourceKind::Recording);
        assert_eq!(kind_of("---\npresent: [Ana, Ben]\n---\n", "Raw/x.txt"), SourceKind::Meeting);
        assert_eq!(kind_of("---\npresent: [{{who was there}}]\n---\n", "Raw/spec.pdf"), SourceKind::Document);
    }

    const NOTE: &str = "---\ntitle: \"Standup - 2026-09-24\"\nstatus: evidence\n---\n\n# Standup - 2026-09-24\n\n\
        ## Summary\n\nThe ship date moved.\n\n\
        ## What It Overturns\n\n**Ship date is Friday, not Monday.** Current/Project.md said Monday.\n\n\
        ## Rulings Said in the Room\n\n- We ship region saves before combat — Ana.\n- {{The ruling, in the decider's words}} — {{decider}}.\n\n\
        ## Actions and Requests\n\n- Fix the save bug → Ben.\n- T-shirt sizes from Jira stories → idea, from arthur.\n\n\
        ## Escalations\n\n- Which Jira types map to tune? → kate.\n\n\
        ## Next Steps\n\n- Chris will rewrite the sizes page this week → on his day.\n- Dee will check the reward card.\n";

    #[test]
    fn rulings_are_read_with_their_decider_and_placeholders_skipped() {
        assert_eq!(rulings_of(NOTE), vec![("We ship region saves before combat".to_string(), Some("Ana".to_string()))]);
        assert!(rulings_of("## Rulings Said in the Room\n\n- None said.\n").is_empty());
    }

    #[test]
    fn actions_ideas_escalations_and_next_steps_are_told_apart() {
        assert_eq!(actions_of(NOTE), vec![("Fix the save bug".to_string(), Some("Ben".to_string()))]);
        assert_eq!(ideas_of(NOTE), vec![("T-shirt sizes from Jira stories".to_string(), Some("arthur".to_string()))]);
        assert_eq!(escalations_of(NOTE), vec![("Which Jira types map to tune?".to_string(), Some("kate".to_string()))]);
        let steps = next_steps_of(NOTE);
        assert_eq!(steps[0].who.as_deref(), Some("Chris"));
        assert_eq!(steps[0].what, "rewrite the sizes page this week");
        assert_eq!(steps[1].who.as_deref(), Some("Dee"));
        assert!(is_me(&me(), "Chris") && is_me(&me(), "chris.staud") && !is_me(&me(), "Dee") && !is_me(&me(), "Chris Other"));
        assert_eq!(actions_of("## Actions and Requests\n\n- Map presets → on Intake.\n- Old one → I-030.\n"), vec![("Map presets".to_string(), None)]);
    }

    #[test]
    fn a_ruling_becomes_the_next_decisions_entry() {
        let log = "# Decisions\n\nD-001 · 2026-09-01 · saves — Worlds save as regions.\n  sources: [[Save]]\n\nD-007 · 2026-09-10 · combat — Later.\n";
        let out = decisions_entry(log, "We ship region saves before combat", Some("Ana"), "2026-09-24", "2026-09-24-standup");
        assert!(out.starts_with(log.trim_end()), "the log is appended to, never rewritten");
        assert!(out.contains("D-008 · 2026-09-24 ·"));
        assert!(out.contains("sources: [[2026-09-24-standup]]") && out.contains("decider: Ana"));
        assert!(decisions_entry("# Decisions\n", "x", None, "2026-09-24", "n").contains("D-001 ·"));
    }

    #[test]
    fn a_plan_keeps_real_pages_and_allowed_new_ones() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("Platform")).unwrap();
        fs::write(d.path().join("Platform/Save.md"), "x").unwrap();
        let pages = vec![("Current/Project.md".to_string(), "Project".to_string()), ("Design/Card.md".to_string(), String::new())];
        let reply = "```json\n{\"update\": [\"Current/Project.md\", {\"path\": \"Design/Card.md\", \"change\": \"the label\"}, \"Current/Project.md\", \"Nope/Missing.md\"], \"create\": [{\"path\": \"Platform/Release-dates.md\", \"purpose\": \"when we ship\"}, {\"path\": \"Ways-of-Working/New-rule.md\"}, {\"path\": \"Research/Ingestion/x.md\"}, {\"path\": \"Platform/Save.md\"}, {\"path\": \"../escape.md\"}]}\n```";
        let plan = read_plan(reply, &pages, d.path());
        assert_eq!(
            plan.update,
            vec![
                PageChange { path: "Current/Project.md".into(), change: String::new() },
                PageChange { path: "Design/Card.md".into(), change: "the label".into() }
            ]
        );
        assert_eq!(
            plan.create.iter().map(|n| n.path.as_str()).collect::<Vec<_>>(),
            vec!["Platform/Release-dates.md", "Ways-of-Working/New-rule.md"],
            "a method page is planned (it becomes a ticket); evidence never is"
        );
        assert_eq!(read_plan("no json here", &pages, d.path()), Plan::default());
    }

    #[test]
    fn staging_holds_an_edit_that_rewrites_a_fifth_or_lands_on_a_moved_page() {
        let page: String = (1..=10).map(|i| format!("line {i}\n")).collect();
        let one = page.replace("line 4\n", "line four\n");
        let three = page.replace("line 2\n", "two\n").replace("line 5\n", "five\n").replace("line 8\n", "eight\n");
        assert!(!rewrites_a_fifth(&page, &one), "1 of 10 lines");
        assert!(!rewrites_a_fifth(&page, &page.replace("line 2\n", "two\n").replace("line 5\n", "five\n")), "2 of 10 is a fifth, not more");
        assert!(rewrites_a_fifth(&page, &three), "3 of 10 lines");
        assert!(rewrites_a_fifth("", "a new line\n"), "anything on an empty page");
        assert_eq!(staging(&page, &page, &one), None);
        assert_eq!(staging(&page, &page.replace('\n', "\r\n"), &one), None, "line endings are no edit");
        assert_eq!(staging(&page, &page, &three), Some(Held::Large));
        assert_eq!(staging(&page, &format!("{page}a person's line\n"), &one), Some(Held::Changed));
    }

    #[test]
    fn ideas_and_escalations_number_on_from_what_is_there() {
        let d = tempfile::tempdir().unwrap();
        let ideas = d.path().join("ideas");
        assert_eq!(next_number(&ideas, "I"), 1, "no folder yet");
        fs::create_dir_all(&ideas).unwrap();
        fs::write(ideas.join("I-030.md"), "").unwrap();
        fs::write(ideas.join("I-007.md"), "").unwrap();
        fs::write(ideas.join("IDEA.md"), "").unwrap();
        fs::write(ideas.join("README.md"), "").unwrap();
        assert_eq!(next_number(&ideas, "I"), 31);
        assert_eq!(next_number(&d.path().join("escalations"), "E"), 1);
        let e = escalation_text("E-001", "Which types?", Some("kate"), "2026-09-24", "2026-09-24-review");
        assert!(e.contains("id: E-001\nraised_by: ingest of 2026-09-24-review\nto: kate\n") && e.contains("status: open"));
        assert!(e.contains("# Which types?") && e.contains("[[2026-09-24-review]]"));
        let i = idea_text("I-031", "T-shirt sizes: from Jira", Some("arthur"), "2026-09-24", "n");
        assert!(i.contains("title: \"T-shirt sizes: from Jira\"") && i.contains("raised_by: wright\nseen: false") && i.contains("source: ingest"));
    }

    /// A wiki with a long Current page, a short Design page, a decisions log,
    /// and a team repo; the note above; a model that edits what it is asked.
    struct Run {
        _wiki: tempfile::TempDir,
        _team: tempfile::TempDir,
        root: PathBuf,
        team_repo: PathBuf,
        ws: PathBuf,
        db: Db,
        placement: Placement,
        item: i64,
    }

    const PROJECT: &str = "---\ntitle: Project\n---\n# Project\n\nThe goal: a tactics game.\nWe ship on Monday.\nThe audience: players of tactics games.\nOut of scope: multiplayer.\nThe engine: our own.\nThe team: five people.\nThe repo: game.\n";
    const CARD: &str = "---\ntitle: Card\n---\nThe card shows a name.\n";

    fn run() -> Run {
        let wiki = tempfile::tempdir().unwrap();
        let root = wiki.path().to_path_buf();
        fs::create_dir_all(root.join("Current")).unwrap();
        fs::create_dir_all(root.join("Design")).unwrap();
        fs::create_dir_all(root.join("Ways-of-Working")).unwrap();
        fs::create_dir_all(root.join("_meta")).unwrap();
        fs::write(root.join("Current/Project.md"), PROJECT).unwrap();
        fs::write(root.join("Design/Card.md"), CARD).unwrap();
        fs::write(root.join("Ways-of-Working/Sizes.md"), "---\ntitle: Sizes\n---\nA preset is a size.\n").unwrap();
        fs::write(root.join("_meta/DECISIONS.md"), "# Decisions\n\nD-001 · 2026-09-01 · saves — Regions.\n").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        crate::scan::scan(&crate::project::Project::create(&root, "Wiki").unwrap(), &mut db).unwrap();
        let team = tempfile::tempdir().unwrap();
        let team_repo = team.path().join("Realms-Team");
        fs::create_dir_all(team_repo.join("tickets")).unwrap();
        fs::create_dir_all(team_repo.join("ideas")).unwrap();
        fs::create_dir_all(team_repo.join("decisions")).unwrap();
        fs::write(team_repo.join("tickets/RT-004.md"), "---\nid: RT-004\n---\n").unwrap();
        fs::write(team_repo.join("ideas/I-030.md"), "---\nid: I-030\n---\n").unwrap();
        fs::write(team_repo.join(TEAM_DECISIONS), "# Decisions\n\nD-091 · 2026-09-20 · x — y.\n").unwrap();
        let ws = team.path().join("workspace");
        fs::create_dir_all(&ws).unwrap();
        let placement = place(&root, "Research/Ingestion/Raw/standup.txt", "2026-09-24", SourceKind::Meeting);
        fs::create_dir_all(root.join(&placement.note).parent().unwrap()).unwrap();
        fs::write(root.join(&placement.note), NOTE).unwrap();
        let c = card(&placement, NOTE);
        let item = db
            .insert_review_item(REVIEW_KIND, "Ingested", "body", &placement.note, Some(&serde_json::to_string(&c).unwrap()), 1)
            .unwrap();
        Run { _wiki: wiki, _team: team, root, team_repo, ws, db, placement, item }
    }

    fn plan_reply() -> String {
        "{\"update\": [{\"path\": \"Current/Project.md\", \"change\": \"ship on Friday\"}, {\"path\": \"Design/Card.md\", \"change\": \"the label\"}, {\"path\": \"Ways-of-Working/Sizes.md\", \"change\": \"a preset is effort\"}], \"create\": [{\"path\": \"Platform/Release-dates.md\", \"purpose\": \"when we ship\"}]}".into()
    }

    /// The model: Project gets one line changed, Card is rewritten whole.
    fn model(p: &str) -> Result<String> {
        Ok(if p.contains("PAGES IN THE WIKI") {
            assert!(p.contains("- Ways-of-Working/Sizes.md") && !p.contains("_meta/DECISIONS.md"), "{p}");
            plan_reply()
        } else if p.contains("`Current/Project.md`") {
            assert!(p.contains("rewrite what changed in place"), "a Current page is rewritten in place");
            PROJECT.replace("We ship on Monday.", "We ship on Friday ([[2026-09-24-standup]]).")
        } else if p.contains("`Design/Card.md`") {
            "---\ntitle: Card\n---\nThe card shows display_name as its label.\n".into()
        } else if p.contains("`Platform/Release-dates.md`") {
            "---\ntitle: Release dates\nsources:\n  - \"[[2026-09-24-standup]]\"\n---\n# Release dates\nFriday.\n".into()
        } else {
            "NO CHANGE".into()
        })
    }

    #[test]
    fn a_note_writes_at_once_and_only_four_kinds_wait() {
        let mut r = run();
        let me = me();
        let targets = Targets { team_repo: Some(&r.team_repo), workspace: Some(&r.ws), me: &me, stamp: "2026-09-24T10:00" };
        let done = follow_ups(&r.root, &mut r.db, &r.placement, NOTE, "2026-09-24", 9, &targets, model).unwrap();

        // Written at once, each citing the note.
        let project = fs::read_to_string(r.root.join("Current/Project.md")).unwrap();
        assert!(project.contains("We ship on Friday ([[2026-09-24-standup]])") && !project.contains("Monday"));
        let release = fs::read_to_string(r.root.join("Platform/Release-dates.md")).unwrap();
        assert!(release.contains("status: draft") && release.contains("[[2026-09-24-standup]]"));
        let idea = fs::read_to_string(r.team_repo.join("ideas/I-031.md")).unwrap();
        assert!(idea.contains("id: I-031") && idea.contains("T-shirt sizes") && idea.contains("Raised by arthur"));
        let esc = fs::read_to_string(r.team_repo.join("escalations/E-001.md")).unwrap();
        assert!(esc.contains("to: kate") && esc.contains("raised_by: ingest of 2026-09-24-standup"));
        let tasks = crate::day::list_home(crate::tasks::TaskHome::Workspace { workspace_root: &r.ws }, "2026-09-24");
        assert_eq!(tasks.len(), 1, "only my next step goes on my day");
        assert_eq!(tasks[0].title, "Rewrite the sizes page this week");
        assert_eq!(tasks[0].links, vec![r.placement.note.clone()]);
        assert_eq!(tasks[0].updated_by.as_deref(), Some("ingest"));

        let kinds: Vec<WriteKind> = done.written.iter().map(|w| w.kind).collect();
        assert_eq!(kinds, vec![WriteKind::Edit, WriteKind::Page, WriteKind::Idea, WriteKind::Escalation, WriteKind::Task]);
        assert_eq!(done.written[0].base.as_deref(), Some(PROJECT));
        assert_eq!(done.written[3].to.as_deref(), Some("kate"));
        assert_eq!(done.listed, vec![listed("next step", "Dee will check the reward card.", Some("Dee".into()))]);

        // Waiting: the held edit, the ruling, one ticket for Ways-of-Working, the action's ticket.
        assert_eq!(done.held, vec!["Design/Card.md"]);
        assert_eq!(fs::read_to_string(r.root.join("Design/Card.md")).unwrap(), CARD, "held, not written");
        assert_eq!(done.rulings, 1);
        assert_eq!(done.tickets, vec!["RT-005", "RT-006"]);
        assert!(fs::read_to_string(r.root.join("Ways-of-Working/Sizes.md")).unwrap().contains("a size"), "a method page is never edited");
        let waits: Vec<crate::wikidraft::Proposal> = proposals_from(&r.db, &r.placement.note)
            .unwrap()
            .into_iter()
            .map(|i| serde_json::from_str(i.payload.as_deref().unwrap()).unwrap())
            .collect();
        assert_eq!(waits.len(), 4);
        let counts = proposal_counts(&r.db.list_open_review_items().unwrap());
        assert_eq!(counts.get(&r.placement.note), Some(&4), "one pass counts what proposals_from lists");
        let held = waits.iter().find(|p| p.page == "Design/Card.md").unwrap();
        assert_eq!(held.base, CARD);
        let method = waits.iter().find(|p| p.page == "tickets/RT-005.md").unwrap();
        assert!(method.proposed.contains("type: process") && method.proposed.contains("`Ways-of-Working/Sizes.md`: a preset is effort"));
        assert!(method.proposed.contains("[[2026-09-24-standup]]") && method.proposed.contains("\n## What and Why\n"));
        crate::wikidraft::apply(&r.root, method).unwrap();
        assert!(r.team_repo.join("tickets/RT-005.md").exists(), "accept writes the ticket");
        let action = waits.iter().find(|p| p.page == "tickets/RT-006.md").unwrap();
        assert!(action.proposed.contains("assignee: Ben") && action.proposed.contains("Fix the save bug"));
        let ruling = waits.iter().find(|p| p.append.is_some()).unwrap();
        assert_eq!(ruling.page, TEAM_DECISIONS, "the team repo's log wins");
        crate::wikidraft::apply(&r.root, ruling).unwrap();
        assert!(fs::read_to_string(r.team_repo.join(TEAM_DECISIONS)).unwrap().contains("D-092 · 2026-09-24"));

        // Every write is on the card, so Undo has it after a restart.
        let stored = card_of(r.db.get_review_item(r.item).unwrap().unwrap().payload.as_deref()).unwrap();
        assert_eq!(stored.writes, done.written);
        assert_eq!(stored.listed, done.listed);

        // Undo all reverses every write, the note, and closes what waits.
        let mut c = stored;
        let report = undo_all(&r.root, &mut c, "2026-09-24").unwrap();
        assert!(report.note_removed && report.kept.is_empty(), "{report:?}");
        assert!(c.writes.iter().all(|w| w.undone));
        assert_eq!(fs::read_to_string(r.root.join("Current/Project.md")).unwrap(), PROJECT);
        assert!(!r.root.join("Platform/Release-dates.md").exists() && !r.team_repo.join("ideas/I-031.md").exists());
        assert!(!r.team_repo.join("escalations/E-001.md").exists() && !r.root.join(&r.placement.note).exists());
        assert!(crate::day::list_home(crate::tasks::TaskHome::Workspace { workspace_root: &r.ws }, "2026-09-24").is_empty(), "archived");
        assert_eq!(withdraw(&mut r.db, &r.placement.note, 10).unwrap(), 4, "what waited is closed");
        assert!(proposals_from(&r.db, &r.placement.note).unwrap().is_empty());

        // An undone card stays open and keeps its source out of the next
        // pass, which would otherwise write it all again.
        assert!(c.undone && !c.filed);
        r.db.set_review_item_payload(r.item, &serde_json::to_string(&c).unwrap()).unwrap();
        assert!(in_review(&r.db).unwrap().contains(&r.placement.raw));
        assert!(!waiting_new(&r.root, &r.db).unwrap().contains(&r.placement.raw));
    }

    #[test]
    fn with_no_team_repo_ideas_escalations_and_tickets_stay_on_the_card() {
        let mut r = run();
        let me = crate::day::Me::default();
        let targets = Targets { team_repo: None, workspace: None, me: &me, stamp: "2026-09-24T10:00" };
        let done = follow_ups(&r.root, &mut r.db, &r.placement, NOTE, "2026-09-24", 9, &targets, model).unwrap();
        let kinds: Vec<&str> = done.listed.iter().map(|l| l.kind.as_str()).collect();
        assert_eq!(kinds, vec!["ticket", "ticket", "idea", "escalation", "next step", "next step"]);
        assert!(done.listed[0].text.starts_with("Ways-of-Working/Sizes.md"), "not dropped: {:?}", done.listed);
        assert!(done.tickets.is_empty() && !r.team_repo.join("ideas/I-031.md").exists());
        let ruling = proposals_from(&r.db, &r.placement.note)
            .unwrap()
            .into_iter()
            .map(|i| serde_json::from_str::<crate::wikidraft::Proposal>(i.payload.as_deref().unwrap()).unwrap())
            .find(|p| p.append.is_some())
            .unwrap();
        assert_eq!(ruling.page, "_meta/DECISIONS.md", "the wiki's log, as before");
        assert!(ruling.root.is_none());
    }

    #[test]
    fn a_page_a_person_changes_during_the_run_is_held() {
        let mut r = run();
        let me = me();
        let targets = Targets { team_repo: None, workspace: None, me: &me, stamp: "2026-09-24T10:00" };
        let path = r.root.join("Current/Project.md");
        let edited = format!("{PROJECT}A person's line.\n");
        let done = follow_ups(&r.root, &mut r.db, &r.placement, NOTE, "2026-09-24", 9, &targets, |p| {
            if p.contains("`Current/Project.md`") {
                fs::write(&path, &edited).unwrap(); // while the model reads
            }
            model(p)
        })
        .unwrap();
        assert!(done.held.contains(&"Current/Project.md".to_string()));
        assert_eq!(fs::read_to_string(&path).unwrap(), edited, "the person's edit stands");
        let held = proposals_from(&r.db, &r.placement.note)
            .unwrap()
            .into_iter()
            .map(|i| serde_json::from_str::<crate::wikidraft::Proposal>(i.payload.as_deref().unwrap()).unwrap())
            .find(|p| p.page == "Current/Project.md")
            .unwrap();
        assert_eq!(held.base, edited, "the diff is against the page as it is now");
        crate::wikidraft::apply(&r.root, &held).unwrap();
    }

    #[test]
    fn one_write_is_undone_only_while_its_file_reads_as_written() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        fs::write(root.join("Page.md"), "after\n").unwrap();
        let edit = Written {
            kind: WriteKind::Edit,
            path: "Page.md".into(),
            root: None,
            label: "x".into(),
            to: None,
            base: Some("before\n".into()),
            hash: text_hash("after\n"),
            task_id: None,
            undone: false,
        };
        fs::write(root.join("New.md"), "new\n").unwrap();
        let page = Written { kind: WriteKind::Page, path: "New.md".into(), base: None, hash: text_hash("new\n"), ..edit.clone() };

        // Changed since: refused, left as it is.
        fs::write(root.join("Page.md"), "after\nand a person's line\n").unwrap();
        let e = undo_write(root, &edit, "2026-09-24").unwrap_err();
        assert!(refused(&e) && e.to_string() == "The page changed since; open it to undo by hand.");
        assert_eq!(fs::read_to_string(root.join("Page.md")).unwrap(), "after\nand a person's line\n");
        fs::write(root.join("New.md"), "new\nedited\n").unwrap();
        assert!(refused(&undo_write(root, &page, "2026-09-24").unwrap_err()));
        assert!(root.join("New.md").exists());

        // Unchanged: restored, removed. CRLF is no change.
        fs::write(root.join("Page.md"), "after\r\n").unwrap();
        fs::write(root.join("New.md"), "new\n").unwrap();
        let mut c = Card {
            placement: Placement { raw: "r".into(), note: "n.md".into(), source: "s".into() },
            note_hash: String::new(),
            filed: false,
            writes: vec![edit, page],
            listed: Vec::new(),
            undone: false,
        };
        undo_write_at(root, &mut c, 0, "2026-09-24").unwrap();
        assert_eq!(fs::read_to_string(root.join("Page.md")).unwrap(), "before\n");
        assert!(c.writes[0].undone && !c.writes[1].undone);
        undo_write_at(root, &mut c, 1, "2026-09-24").unwrap();
        assert!(!root.join("New.md").exists());
        assert!(undo_write_at(root, &mut c, 1, "2026-09-24").is_ok(), "twice is a no-op");
        assert!(undo_write_at(root, &mut c, 7, "2026-09-24").is_err());
    }

    #[test]
    fn undo_all_goes_last_write_first() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        // Two edits to one page, A → B → C: only last-first gets back to A.
        fs::write(root.join("Page.md"), "C\n").unwrap();
        let w = |base: &str, after: &str| Written {
            kind: WriteKind::Edit,
            path: "Page.md".into(),
            root: None,
            label: format!("{base} to {after}"),
            to: None,
            base: Some(format!("{base}\n")),
            hash: text_hash(&format!("{after}\n")),
            task_id: None,
            undone: false,
        };
        fs::write(root.join("Other.md"), "mine now\n").unwrap();
        let other = Written { path: "Other.md".into(), label: "other".into(), base: Some("x\n".into()), hash: text_hash("theirs\n"), ..w("x", "y") };
        let mut c = Card {
            placement: Placement { raw: "r".into(), note: "n.md".into(), source: "s".into() },
            note_hash: hash("note"),
            filed: false,
            writes: vec![w("A", "B"), other, w("B", "C")],
            listed: Vec::new(),
            undone: false,
        };
        fs::write(root.join("n.md"), "note").unwrap();
        let report = undo_all(root, &mut c, "2026-09-24").unwrap();
        assert_eq!(fs::read_to_string(root.join("Page.md")).unwrap(), "A\n");
        assert_eq!(report.kept, vec!["other"], "a page changed since is kept and named");
        assert!(report.note_removed && !root.join("n.md").exists());
        assert!(c.writes[0].undone && !c.writes[1].undone && c.writes[2].undone);
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
        let t = ticket_text("SRX-013", "Fix the save bug.", Some("Ben"), "n");
        assert!(t.contains("\n## What and Why\n\nFix the save bug. Asked for in [[n]].") && t.contains("\n## thread\n"), "{t}");
    }

    fn quiet() -> std::sync::Arc<dyn Fn(&str, PassPhase) + Send + Sync> {
        std::sync::Arc::new(|_, _| {})
    }

    /// A pass's model: the source named `bad` gets a reply with no
    /// frontmatter, every other source the standup note, follow-ups nothing.
    fn pass_model(p: &str) -> std::result::Result<String, CallError> {
        if p.contains("SOURCE (") {
            if p.contains("SOURCE (0-bad.txt)") {
                return Ok("Sorry, I cannot do that.".into());
            }
            return Ok(REPLY.into());
        }
        Ok("NO CHANGE".into())
    }

    #[test]
    fn a_bad_first_source_does_not_stop_the_ones_after_it() {
        let d = library();
        let root = d.path();
        fs::write(root.join(RAW).join("0-bad.txt"), "Something Ken will fail on.\n").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        let me = me();
        let targets = Targets { team_repo: None, workspace: None, me: &me, stamp: "2026-09-24T10:00" };
        let report = run_pass(root, &mut db, &targets, "2026-09-24", crate::engine::now_epoch, Transcriber::default(), pass_model, &quiet()).unwrap();
        assert_eq!(report.failed.len(), 1, "{report:?}");
        assert_eq!(report.failed[0].0, format!("{RAW}/0-bad.txt"));
        assert_eq!(report.read.len(), 1, "the good source after it is read");
        assert!(report.stopped.is_none());
        assert!(failure_of(&db, &format!("{RAW}/0-bad.txt")).unwrap().is_some());
        // The next pass leaves the failed source alone until it is tried again.
        let mut asked = 0;
        let again = run_pass(root, &mut db, &targets, "2026-09-24", crate::engine::now_epoch, Transcriber::default(), |p| {
            asked += 1;
            pass_model(p)
        }, &quiet())
        .unwrap();
        assert!(again.read.is_empty() && again.failed.is_empty() && asked == 0);
        // Try again: the failure is cleared and the source read once more.
        assert_eq!(resolve_failure(&mut db, &format!("{RAW}/0-bad.txt"), 7).unwrap(), 1);
        let retried = run_pass(root, &mut db, &targets, "2026-09-24", crate::engine::now_epoch, Transcriber::default(), pass_model, &quiet()).unwrap();
        assert_eq!(retried.failed.len(), 1);
        // A file changed since it failed is read again without asking.
        let f = fs::File::options().write(true).open(root.join(RAW).join("0-bad.txt")).unwrap();
        f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(30)).unwrap();
        drop(f);
        let changed = run_pass(root, &mut db, &targets, "2026-09-24", crate::engine::now_epoch, Transcriber::default(), pass_model, &quiet()).unwrap();
        assert_eq!(changed.failed.len(), 1, "tried again");
        assert_eq!(failures(&db).unwrap().len(), 1, "one failure open, not two");
    }

    #[test]
    fn a_cli_failure_stops_the_pass_and_marks_nothing_failed() {
        let d = library();
        let root = d.path();
        fs::write(root.join(RAW).join("b-second.txt"), "More.\n").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        let me = me();
        let targets = Targets { team_repo: None, workspace: None, me: &me, stamp: "2026-09-24T10:00" };
        let mut calls = 0;
        let report = run_pass(root, &mut db, &targets, "2026-09-24", || 5, Transcriber::default(), |_| {
            calls += 1;
            Err(CallError::Stop("Not logged in · Please run /login".into()))
        }, &quiet())
        .unwrap();
        assert_eq!(calls, 1, "one call, then the pass stops");
        assert!(report.stopped.unwrap().contains("/login"));
        assert!(report.failed.is_empty() && failures(&db).unwrap().is_empty());
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
        let me = me();
        let targets = Targets { team_repo: None, workspace: None, me: &me, stamp: "2026-09-24T10:00" };
        let phases = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = phases.clone();
        let on_phase: std::sync::Arc<dyn Fn(&str, PassPhase) + Send + Sync> =
            std::sync::Arc::new(move |raw, p| seen.lock().unwrap().push((raw.to_string(), p)));
        let report = run_pass(root, &mut db, &targets, "2026-09-24", || 5, Transcriber::default(), pass_model, &on_phase).unwrap();
        assert_eq!(report.waiting.len(), 1);
        assert!(report.waiting[0].1.contains("Install a transcription model in Settings"));
        assert_eq!(report.read.len(), 1);
        assert!(report.failed.is_empty(), "waiting is not failing");
        assert_eq!(phases.lock().unwrap().as_slice(), &[(format!("{RAW}/Standup notes.txt"), PassPhase::Reading)]);
        // A recording that cannot be decoded, with a model there, fails.
        let model = root.join("model.bin");
        fs::write(&model, b"x").unwrap();
        let t = Transcriber { model: Some(&model), ffmpeg: None };
        let report = run_pass(root, &mut db, &targets, "2026-09-24", || 6, t, pass_model, &quiet()).unwrap();
        assert_eq!(report.failed.len(), 1);
        assert!(report.failed[0].1.starts_with("Could not transcribe it"), "{:?}", report.failed);
    }

    #[test]
    fn a_wiki_template_without_a_summary_gives_way_to_the_bundled_one() {
        let d = library();
        let root = d.path();
        assert!(note_template(root).contains("## Summary"), "no template: the bundled one");
        fs::create_dir_all(root.join("Templates")).unwrap();
        fs::write(root.join(TEMPLATE), "---\ntitle: x\n---\n# {{What}}\n\n## What It Overturns\n").unwrap();
        assert_eq!(note_template(root), DEFAULT_TEMPLATE, "an old template: the bundled one");
        fs::write(root.join(TEMPLATE), "---\ntitle: x\n---\n# Ours\n\n## Summary\n\n{{it}}\n").unwrap();
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
        let mut asked = String::new();
        let p = ingest_one(root, &mut db, &format!("{RAW}/{name}"), "2026-10-01", 3, |prompt| {
            asked = prompt.to_string();
            Ok(REPLY.to_string())
        })
        .unwrap();
        assert!(asked.contains("set `kind: note`"));
        assert!(p.note.starts_with(&format!("{INGESTED}/Notes/2026-10/")), "the reply said meeting; the source says note: {}", p.note);
    }

    #[test]
    fn a_bold_log_gets_its_next_entry_bold() {
        let log = "# Decisions\n\n**D-410** · 2026-10-06 · anchors — **WRITTEN BY THE TOOLS.**\n";
        let out = decisions_entry(log, "Anchors stay tool-written", None, "2026-10-08", "note");
        assert!(out.contains("\n\n**D-411** · 2026-10-08 · Anchors stay tool-written — "), "{out}");
    }
}

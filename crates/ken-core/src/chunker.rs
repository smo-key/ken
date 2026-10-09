//! Pure text chunking for the semantic index. No I/O, no database access —
//! this module only turns already-read file text into a list of [`Chunk`]s
//! according to an [`IndexProfile`]. Callers (the ingest pipeline, `db.rs`)
//! are responsible for reading files and persisting the result.
//!
//! Two chunking strategies:
//!
//! * **Prose** — heading/paragraph-aware. Markdown ATX headings (`# ...`)
//!   always start a new chunk; otherwise paragraphs (blank-line separated)
//!   are packed up to `target_tokens`, with `overlap_pct` of the previous
//!   chunk's tail carried into the next chunk so a search hit near a chunk
//!   boundary still has surrounding context.
//! * **Code** — line-based blocks of ~`target_tokens`, no overlap. Splitting
//!   mid-statement is acceptable; the goal is bounded, roughly-uniform chunks
//!   for embedding, not syntactic correctness.
//!
//! Token counts are estimated as `chars / 4` — cheap and stable. Exactness
//! doesn't matter here, only consistency between index time and any future
//! re-chunking. `content_hash` is an xxHash64 of the chunk text, used by
//! `db::upsert_chunks` to diff against the previous chunk set so an unchanged
//! file costs zero re-embeddings.

use serde::{Deserialize, Serialize};

/// Chunking strategy for a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkMode {
    /// Heading/paragraph-aware splitting with overlap. Markdown, plain text,
    /// extracted PDF text.
    Prose,
    /// Line-based fixed-size blocks, no overlap. Source code and other
    /// structured/dense text.
    Code,
    /// Never chunked or embedded. The file still keyword-searches (that's
    /// driven by kenignore's tier, not this), but produces zero semantic
    /// chunks — used by project-profiler pattern entries (e.g. `*.log`) to
    /// opt noisy/generated text out of embedding without excluding it
    /// entirely.
    Skip,
}

/// How a file should be split into chunks. Per-path selection lives in
/// [`IndexProfile::default_for`]; callers may also construct one directly to
/// override the default (e.g. a future user setting).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct IndexProfile {
    pub mode: ChunkMode,
    /// Target chunk size, in estimated tokens (`chars / 4`).
    pub target_tokens: usize,
    /// Fraction (0.0-1.0) of `target_tokens` worth of trailing text from a
    /// chunk that is repeated at the start of the next chunk. Only applies
    /// to prose mode; code mode uses 0.0.
    pub overlap_pct: f32,
}

impl Default for IndexProfile {
    fn default() -> Self {
        IndexProfile {
            mode: ChunkMode::Prose,
            target_tokens: 350,
            overlap_pct: 0.15,
        }
    }
}

/// Prose-ish extensions: markdown, plain text, extracted PDF text. Module
/// level (not just local to `default_for`) so other modules can classify by
/// the same taxonomy instead of duplicating it — project-profiler's doc-ratio
/// scan and default chunking table reuse this exact list.
pub const PROSE_EXTS: &[&str] = &["md", "mdx", "txt", "pdf"];
/// Source-code and other structured/dense-text extensions. See [`PROSE_EXTS`].
pub const CODE_EXTS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts", "py", "go", "java", "c", "h", "cc",
    "cpp", "hpp", "cs", "rb", "php", "swift", "kt", "kts", "gradle", "properties", "sql", "sh",
    "bash", "ps1", "toml", "yaml", "yml", "json", "css", "scss", "html", "svelte", "vue", "ui",
    "lang",
];

impl IndexProfile {
    /// v1 default profile selection by file extension: prose-ish formats
    /// (markdown, plain text, extracted PDF text) get the prose profile;
    /// source-code extensions get the code profile; anything else falls back
    /// to prose defaults.
    pub fn default_for(rel_path: &str) -> IndexProfile {
        let ext = rel_path
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if PROSE_EXTS.contains(&ext.as_str()) {
            IndexProfile {
                mode: ChunkMode::Prose,
                target_tokens: 350,
                overlap_pct: 0.15,
            }
        } else if CODE_EXTS.contains(&ext.as_str()) {
            IndexProfile {
                mode: ChunkMode::Code,
                target_tokens: 500,
                overlap_pct: 0.0,
            }
        } else {
            IndexProfile::default()
        }
    }
}

/// One chunk of a file's text, ready to be persisted (`db::upsert_chunks`
/// assigns `id`/`path`/`tier`) and embedded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// 0-based position of this chunk within the file.
    pub seq: usize,
    pub text: String,
    /// Estimated token count (`text.len() / 4`, minimum 1).
    pub token_est: usize,
    /// xxHash64 of `text`, hex-encoded. Used for incremental diffing.
    pub content_hash: String,
    /// 1-based line in the file where this chunk's own content starts (not
    /// the overlap carried from the chunk before), so a hit can be cited as
    /// `repo:path:line`.
    pub line: usize,
}

/// Hard cap on chunks per file. Guards against pathological inputs
/// (generated/minified files, huge data dumps) blowing up chunk count and,
/// downstream, embedding cost.
const CHUNK_CAP: usize = 200;

/// Split `text` (the contents of `rel_path`) into chunks per `profile`.
/// Pure and infallible: empty/whitespace-only text yields no chunks.
pub fn chunk_file(rel_path: &str, text: &str, profile: &IndexProfile) -> Vec<Chunk> {
    if text.trim().is_empty() {
        return Vec::new();
    }

    // A log (a decisions log, a run of rulings) is cut one entry per chunk
    // and is not held to the cap: every entry is its own answer.
    let log = (profile.mode == ChunkMode::Prose && is_markdown(rel_path)).then(|| chunk_log(text, profile)).flatten();
    let cap = if log.is_some() { LOG_CHUNK_CAP } else { CHUNK_CAP };
    let pieces = match (log, profile.mode) {
        (Some(entries), _) => entries,
        (None, ChunkMode::Prose) => chunk_prose(text, profile),
        (None, ChunkMode::Code) => chunk_code(text, profile),
        (None, ChunkMode::Skip) => Vec::new(),
    };
    let mut pieces: Vec<(usize, String)> = pieces.into_iter().filter(|(_, p)| !p.trim().is_empty()).collect();
    pieces.truncate(cap);
    if profile.mode == ChunkMode::Code && rel_path.to_ascii_lowercase().ends_with(".json") {
        pieces = with_json_headers(rel_path, text, pieces);
    }

    pieces
        .into_iter()
        .take(cap)
        .enumerate()
        .map(|(seq, (line, text))| {
            let token_est = (text.len() / 4).max(1);
            let content_hash = hash_text(&text);
            Chunk {
                seq,
                text,
                token_est,
                content_hash,
                line,
            }
        })
        .collect()
}

fn hash_text(text: &str) -> String {
    let h = twox_hash::XxHash64::oneshot(0, text.as_bytes());
    format!("{h:016x}")
}

/// Whether `rel_path` is a Markdown page.
pub(crate) fn is_markdown(rel_path: &str) -> bool {
    let p = rel_path.to_ascii_lowercase();
    p.ends_with(".md") || p.ends_with(".markdown") || p.ends_with(".mdx")
}

/// Most chunks a log-shaped file is cut into: one per entry, far past
/// [`CHUNK_CAP`], only a guard against a pathological file. On 2026-10-07 the
/// cap packed the 446 rulings of a 1,246-line decisions log into 200 chunks
/// of two or three unrelated rulings each, and D-410's chunk began inside
/// D-411.
const LOG_CHUNK_CAP: usize = 5_000;
/// Fewest entries that make a file a log.
const LOG_MIN_ENTRIES: usize = 5;
/// The share of a file's text its entries must hold for it to be a log: a
/// page with a few bold ids among its prose is chunked as prose.
const LOG_MIN_SHARE: f64 = 0.5;
/// An entry longer than this, in bytes, is cut at its line ends.
const LOG_ENTRY_MAX: usize = 12_000;
/// Longest title an entry carries into the index's names.
const LOG_TITLE_MAX: usize = 300;

/// An entry's id and title when `line` starts one, else None: a bold id at
/// the start of the line, also as a list item (`**D-123** · 2026-10-06 ·
/// topics — **THE RULING.**`, its title the next bold span or else the rest
/// of the line); a plain id followed by its date, the form the method's
/// template writes (`D-001 - 2026-01-01 - topic - THE RULING.`, see
/// [`plain_entry`]); or a heading that starts with an id (`## R-12: Title`).
/// An id is a capital letter, up to seven more capitals or digits, a dash
/// and digits (`D-123`, `SR-001`, `U-4`).
pub fn entry_start(line: &str) -> Option<(String, String)> {
    let t = line.trim_start();
    if t.starts_with('#') {
        let h = t.trim_start_matches('#');
        if !h.starts_with(' ') {
            return None;
        }
        let h = h.trim_start();
        let h = h.strip_prefix("**").unwrap_or(h);
        let (id, rest) = take_id(h)?;
        let rest = rest.strip_prefix("**").unwrap_or(rest);
        if rest.chars().next().is_some_and(|c| c.is_alphanumeric()) {
            return None;
        }
        return Some((id, entry_title(rest)));
    }
    let t = ["- ", "* ", "+ "].iter().find_map(|m| t.strip_prefix(m)).unwrap_or(t);
    let Some(bold) = t.strip_prefix("**") else {
        let (id, _, ruling) = plain_entry(t)?;
        let title = match ruling.split_once("**") {
            Some((_, after)) => after.split_once("**").map_or(after, |(bold, _)| bold),
            None => ruling,
        };
        return Some((id, entry_title(title)));
    };
    let (id, rest) = take_id(bold)?;
    let rest = rest.strip_prefix("**")?;
    let title = match rest.split_once("**") {
        Some((_, after)) => after.split_once("**").map_or(after, |(bold, _)| bold),
        None => rest,
    };
    Some((id, entry_title(title)))
}

/// A plain entry line: an id, then ` · ` or ` - ` and the entry's date
/// (`D-001 - 2026-01-01 - topic - THE RULING.`, or `D-001 · 2026-09-02 ·
/// save format — Worlds save as region files.`). Returns the id, the head
/// (the date and topics, up to the ruling) and the ruling: after ` — ` when
/// the line has one, else after the third separator. The date right after
/// the id is what tells an entry from a sentence that starts with an id.
fn plain_entry(t: &str) -> Option<(String, &str, &str)> {
    let (id, rest) = take_id(t)?;
    let sep = [" · ", " - "].into_iter().find(|s| rest.starts_with(s))?;
    crate::pagemeta::date_in(rest.get(sep.len()..sep.len() + 10)?)?;
    let split = match rest.find(" — ") {
        Some(i) => i + " — ".len(),
        None => {
            let mut at = 0;
            for _ in 0..3 {
                match rest[at..].find(sep) {
                    Some(j) => at += j + sep.len(),
                    None => {
                        at = rest.len();
                        break;
                    }
                }
            }
            at
        }
    };
    Some((id, &rest[..split], &rest[split..]))
}

/// An entry line with the bold taken off its id, so `**D-410** · …` reads
/// as `D-410 · …`: the parsers written for the template's plain form (the
/// drift check, the vocabulary, ingest's next id) then read a bold log too.
/// Any other line comes back as it was.
pub fn unbold_id(line: &str) -> std::borrow::Cow<'_, str> {
    let t = line.trim_start();
    let t = ["- ", "* ", "+ "].iter().find_map(|m| t.strip_prefix(m)).unwrap_or(t);
    if let Some((id, rest)) = t.strip_prefix("**").and_then(take_id) {
        if let Some(rest) = rest.strip_prefix("**") {
            return std::borrow::Cow::Owned(format!("{id}{rest}"));
        }
    }
    std::borrow::Cow::Borrowed(line)
}

/// `D-123` at the start of `s`, and what follows it.
fn take_id(s: &str) -> Option<(String, &str)> {
    let b = s.as_bytes();
    if !b.first()?.is_ascii_uppercase() {
        return None;
    }
    let mut i = 1;
    while i < b.len() && i < 8 && (b[i].is_ascii_uppercase() || b[i].is_ascii_digit()) {
        i += 1;
    }
    if b.get(i) != Some(&b'-') {
        return None;
    }
    let digits = b[i + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 || digits > 6 {
        return None;
    }
    let end = i + 1 + digits;
    Some((s[..end].to_string(), &s[end..]))
}

/// A title without the punctuation around it, cut to [`LOG_TITLE_MAX`].
fn entry_title(raw: &str) -> String {
    let t = raw.trim_matches(|c: char| c.is_whitespace() || matches!(c, '*' | '#' | ':' | '.' | '-' | '—' | '–' | '·' | '|'));
    let mut end = t.len().min(LOG_TITLE_MAX);
    while !t.is_char_boundary(end) {
        end -= 1;
    }
    t[..end].trim_end().to_string()
}

/// Whether the page at `rel_path` with this text is chunked as a log, one
/// entry per chunk.
pub fn is_log(rel_path: &str, text: &str) -> bool {
    is_markdown(rel_path) && chunk_log(text, &IndexProfile::default_for(rel_path)).is_some()
}

/// The entry a chunk of a log holds (id and title), read from its first
/// lines: the chunk may start with the heading the entry sits under.
pub fn chunk_entry(chunk_text: &str) -> Option<(String, String)> {
    entry_line(chunk_text).and_then(entry_start)
}

/// The line that starts the entry a chunk of a log holds (see
/// [`chunk_entry`]).
pub fn entry_line(chunk_text: &str) -> Option<&str> {
    // A fence ends the search: what follows is an example, not an entry.
    chunk_text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(3)
        .take_while(|l| !l.trim_start().starts_with("```"))
        .find(|l| entry_start(l).is_some())
}

/// What an entry line carries between its id and its title: its date and
/// topics (` · 2026-10-06 · anchors, tools — ` in `**D-410** · 2026-10-06 ·
/// anchors, tools — **SPAWN ANCHORS…**`). None when the line starts no
/// entry; empty for a heading entry (`## R-12: Title`).
pub fn entry_head(line: &str) -> Option<&str> {
    entry_start(line)?;
    let t = line.trim_start();
    if t.starts_with('#') {
        return Some("");
    }
    let t = ["- ", "* ", "+ "].iter().find_map(|m| t.strip_prefix(m)).unwrap_or(t);
    let Some(bold) = t.strip_prefix("**") else {
        return plain_entry(t).map(|(_, head, _)| head);
    };
    let (_, rest) = take_id(bold)?;
    let rest = rest.strip_prefix("**")?;
    Some(rest.split("**").next().unwrap_or(rest))
}

/// The date an entry carries in its head, if any.
pub fn entry_date(line: &str) -> Option<String> {
    entry_head(line).and_then(crate::pagemeta::date_in)
}

/// The topics an entry carries in its head, lowercased: `anchors`, `tools`
/// for `**D-410** · 2026-10-06 · anchors, tools — **…**`. The head's parts
/// that are not a date, split at commas.
pub fn entry_topics(line: &str) -> Vec<String> {
    let Some(head) = entry_head(line) else { return Vec::new() };
    // The template's plain form separates its fields with ` - `.
    let parts: Vec<&str> = if head.contains('·') { head.split('·').collect() } else { head.split(" - ").collect() };
    parts
        .into_iter()
        .map(|p| p.trim_matches(|c: char| c.is_whitespace() || matches!(c, '—' | '–' | '-' | ':' | '|')))
        .filter(|p| !p.is_empty() && crate::pagemeta::date_in(p).is_none())
        .flat_map(|p| p.split(','))
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty() && t.chars().count() <= 40)
        .collect()
}

/// The part of a Markdown page that holds line `line` (1-based): the entry
/// it is in when the page is a log (a decisions log: that entry, to the next
/// entry or heading), else its section, from the heading above it to the
/// next heading of the same or a higher level. Lines before the first
/// heading are a section of their own, unless they are only frontmatter:
/// then the first heading's section is read with them. Headings inside a
/// fenced code block or the frontmatter do not count.
///
/// Returns the first and last line (1-based, inclusive) and the heading or
/// entry id that names it; None when `line` is past the end.
pub fn section_at(rel_path: &str, text: &str, line: usize) -> Option<(usize, usize, Option<String>)> {
    let lines: Vec<&str> = text.lines().collect();
    if line == 0 || line > lines.len() {
        return None;
    }
    let target = line - 1;
    // The frontmatter's last line, and each line's heading level.
    let front_end = (lines.first().map(|l| l.trim_end()) == Some("---"))
        .then(|| lines.iter().skip(1).position(|l| matches!(l.trim_end(), "---" | "...")).map(|p| p + 1))
        .flatten();
    let mut level: Vec<Option<usize>> = vec![None; lines.len()];
    let mut fence: Option<&str> = None;
    for (i, l) in lines.iter().enumerate() {
        if front_end.is_some_and(|e| i <= e) {
            continue;
        }
        let t = l.trim_start();
        let mark = if t.starts_with("```") { Some("```") } else if t.starts_with("~~~") { Some("~~~") } else { None };
        match (fence, mark) {
            (Some(f), Some(m)) if f == m => fence = None,
            (None, Some(m)) => fence = Some(m),
            _ => {}
        }
        if fence.is_some() || mark.is_some() {
            continue;
        }
        let hashes = t.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) && t[hashes..].starts_with(' ') {
            level[i] = Some(hashes);
        }
    }
    let trim = |a: usize, mut b: usize| {
        while b > a && lines[b].trim().is_empty() {
            b -= 1;
        }
        b
    };
    if is_markdown(rel_path) && chunk_log(text, &IndexProfile::default_for(rel_path)).is_some() {
        let start = (0..=target).rev().find(|&i| level[i].is_some() || entry_start(lines[i]).is_some());
        if let Some(s) = start.filter(|&s| level[s].is_none()) {
            let end = (s + 1..lines.len()).find(|&i| level[i].is_some() || entry_start(lines[i]).is_some()).unwrap_or(lines.len());
            let id = entry_start(lines[s]).map(|(id, _)| id);
            return Some((s + 1, trim(s, end - 1) + 1, id));
        }
    }
    let section = |h: usize| {
        let lv = level[h].unwrap_or(1);
        let end = (h + 1..lines.len()).find(|&i| level[i].is_some_and(|l| l <= lv)).unwrap_or(lines.len());
        (h, end - 1)
    };
    let (a, b, head) = match (0..=target).rev().find(|&i| level[i].is_some()) {
        Some(h) => {
            let (a, b) = section(h);
            (a, b, Some(lines[h].trim().to_string()))
        }
        None => {
            let first = (0..lines.len()).find(|&i| level[i].is_some());
            let body = (front_end.map_or(0, |e| e + 1)..first.unwrap_or(lines.len())).any(|i| !lines[i].trim().is_empty());
            match first {
                Some(f) if !body => {
                    let (_, b) = section(f);
                    (0, b, Some(lines[f].trim().to_string()))
                }
                Some(f) => (0, f - 1, None),
                None => (0, lines.len() - 1, None),
            }
        }
    };
    Some((a + 1, trim(a, b) + 1, head))
}

/// A Markdown file whose body is a run of entries, one chunk per entry, or
/// None when it is not one (fewer than [`LOG_MIN_ENTRIES`] entries, or the
/// entries hold less than [`LOG_MIN_SHARE`] of the text). An entry runs from
/// its first line to the next entry or heading, and carries the heading it
/// sits under (`### 2026-10-06`) as its first line, as each piece of a long
/// table carries the table's header. Text that is not an entry (the preamble,
/// an index, a section of notes) is chunked as prose, as before.
fn chunk_log(text: &str, profile: &IndexProfile) -> Option<Vec<(usize, String)>> {
    let lines: Vec<&str> = text.lines().collect();
    // A line in a fence is never an entry: the template's format example
    // (`D-001 - 2026-01-01 - topic - …`) sits in one.
    let mut in_fence = false;
    let entries: Vec<bool> = lines
        .iter()
        .map(|l| {
            if l.trim_start().starts_with("```") {
                in_fence = !in_fence;
                return false;
            }
            !in_fence && entry_start(l).is_some()
        })
        .collect();
    if entries.iter().filter(|e| **e).count() < LOG_MIN_ENTRIES {
        return None;
    }
    let is_heading = |l: &str| l.trim_start().starts_with('#');
    // (is an entry, first line index, end line index)
    let mut regions: Vec<(bool, usize, usize)> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let open_other = regions.last().is_some_and(|r| !r.0);
        if entries[i] || (is_heading(line) && !open_other) || regions.is_empty() {
            regions.push((entries[i], i, i + 1));
        } else if let Some(r) = regions.last_mut() {
            r.2 = i + 1;
        }
    }
    let entry_bytes: usize = regions.iter().filter(|r| r.0).flat_map(|r| &lines[r.1..r.2]).map(|l| l.len() + 1).sum();
    if (entry_bytes as f64) < LOG_MIN_SHARE * text.len() as f64 {
        return None;
    }
    let mut out = Vec::new();
    let mut context: Option<&str> = None;
    for (entry, a, b) in regions {
        let body = lines[a..b].join("\n");
        if entry {
            let body = body.trim();
            let with_context = |piece: &str| match context {
                Some(h) => format!("{h}\n\n{piece}"),
                None => piece.to_string(),
            };
            if body.len() > LOG_ENTRY_MAX {
                out.extend(split_long_block(a + 1, body, LOG_ENTRY_MAX).into_iter().map(|(l, p)| (l, with_context(&p))));
            } else {
                out.push((a + 1, with_context(body)));
            }
            continue;
        }
        // Headings alone (`## THE LOG`, then `### 2026-10-06`) are context for
        // the entries under them, not chunks of their own.
        if lines[a..b].iter().any(|l| !l.trim().is_empty() && !is_heading(l)) {
            out.extend(chunk_prose(&body, profile).into_iter().map(|(l, p)| (a + l, p)));
        }
        if let Some(h) = lines[a..b].iter().rev().find(|l| is_heading(l)) {
            context = Some(h.trim());
        }
    }
    Some(out)
}

/// Split text into paragraph/heading blocks: blank lines separate
/// paragraphs; any line starting with `#` (markdown ATX heading) is always
/// its own block, regardless of surrounding blank lines. Each block carries
/// the 1-based line it starts on.
fn split_prose_blocks(text: &str) -> Vec<(usize, String)> {
    let mut blocks = Vec::new();
    let mut current = String::new();
    let mut start = 0usize;

    for (i, line) in text.lines().enumerate() {
        let n = i + 1;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !current.trim().is_empty() {
                blocks.push((start, current.trim().to_string()));
            }
            current.clear();
            continue;
        }
        if trimmed.starts_with('#') {
            if !current.trim().is_empty() {
                blocks.push((start, current.trim().to_string()));
            }
            current.clear();
            blocks.push((n, trimmed.to_string()));
            continue;
        }
        if current.is_empty() {
            start = n;
        } else {
            current.push('\n');
        }
        current.push_str(line);
    }
    if !current.trim().is_empty() {
        blocks.push((start, current.trim().to_string()));
    }
    blocks
}

/// Take the last `n` bytes of `s`, adjusted forward to the nearest char
/// boundary so the result is always valid UTF-8.
fn tail(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let mut start = s.len() - n;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    s[start..].to_string()
}

/// Chunks as (first line of the chunk's own content, text).
fn chunk_prose(text: &str, profile: &IndexProfile) -> Vec<(usize, String)> {
    // A file too long for `CHUNK_CAP` chunks of the target size gets bigger
    // chunks rather than losing its end: sized to fill three quarters of the
    // cap, which leaves room for the chunks that end short of the size.
    let target_chars = (profile.target_tokens * 4).max(1).max(text.len() / (CHUNK_CAP * 3 / 4));
    let overlap_chars = ((target_chars as f32) * profile.overlap_pct).round() as usize;

    let blocks = split_prose_blocks(text);
    let mut chunks: Vec<(usize, String)> = Vec::new();
    let mut current = String::new();
    // The line of the first block added since the last boundary; the
    // overlap tail carried into a chunk does not move it.
    let mut line: Option<usize> = None;

    for (block_line, block) in blocks {
        // One paragraph or table longer than a chunk is cut at its line ends
        // into chunks of its own. Kept whole, the 2,300-row table of
        // decisions/Cited-in-Code.md was one 445 KB chunk on 2026-10-06: it
        // held every word of nearly every question, so it matched them all
        // and answered none.
        if block.len() > target_chars {
            if !current.trim().is_empty() {
                chunks.push((line.unwrap_or(block_line), std::mem::take(&mut current)));
            }
            current.clear();
            line = None;
            chunks.extend(split_long_block(block_line, &block, target_chars));
            continue;
        }
        let is_heading = block.starts_with('#');
        let would_exceed = !current.is_empty() && current.len() + block.len() + 2 > target_chars;
        // Headings always start a fresh chunk (no merging a new section into
        // the tail of the previous one), but never against an empty buffer.
        let force_boundary = is_heading && !current.is_empty();

        if would_exceed || force_boundary {
            chunks.push((line.unwrap_or(block_line), current.clone()));
            line = None;
            current = if overlap_chars > 0 && !force_boundary {
                tail(&current, overlap_chars)
            } else {
                String::new()
            };
        }

        if !current.is_empty() {
            current.push_str("\n\n");
        }
        current.push_str(&block);
        line.get_or_insert(block_line);
    }
    if !current.trim().is_empty() {
        chunks.push((line.unwrap_or(1), current));
    }
    chunks
}

/// A block starting on line `start` cut at line ends into pieces of at most
/// `max` bytes (a single longer line stays whole), each with the line it
/// starts on. A Markdown table's header and rule rows start every piece, so
/// each still reads as the table it came from.
fn split_long_block(start: usize, block: &str, max: usize) -> Vec<(usize, String)> {
    let lines: Vec<&str> = block.lines().collect();
    let is_rule = |l: &str| {
        let l = l.trim();
        l.starts_with('|') && l.contains('-') && l.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
    };
    let header = (lines.len() > 2 && lines[0].trim_start().starts_with('|') && is_rule(lines[1]))
        .then(|| format!("{}\n{}\n", lines[0], lines[1]));
    let mut pieces = Vec::new();
    let mut current = String::new();
    let mut first: Option<usize> = None;
    for (i, row) in lines.iter().enumerate() {
        if first.is_some() && current.len() + row.len() + 1 > max {
            pieces.push((first.take().unwrap_or(start), std::mem::take(&mut current)));
        }
        if first.is_none() {
            if let Some(h) = header.as_deref().filter(|_| i >= 2) {
                current.push_str(h);
            }
            first = Some(start + i);
        } else {
            current.push('\n');
        }
        current.push_str(row);
    }
    if let Some(l) = first {
        pieces.push((l, current));
    }
    pieces
}

/// Chunks as (first line, text).
fn chunk_code(text: &str, profile: &IndexProfile) -> Vec<(usize, String)> {
    let target_chars = (profile.target_tokens * 4).max(1);
    let mut chunks: Vec<(usize, String)> = Vec::new();
    let mut current = String::new();
    let mut start = 1usize;

    for (i, line) in text.lines().enumerate() {
        if !current.is_empty() && current.len() + line.len() + 1 > target_chars {
            chunks.push((start, current.clone()));
            current.clear();
        }
        if current.is_empty() {
            start = i + 1;
        } else {
            current.push('\n');
        }
        current.push_str(line);
    }
    if !current.trim().is_empty() {
        chunks.push((start, current));
    }
    chunks
}

/// Keys whose string values go in a JSON chunk's header: what its numbers
/// belong to.
const JSON_NAME_KEYS: &[&str] = &["$Comment", "Description", "Name"];
/// Longest key outline in a JSON chunk's header, in bytes, cut between keys.
const JSON_KEYS_MAX: usize = 600;
/// Longest list of names in a JSON chunk's header, in bytes, cut between names.
const JSON_NAMES_MAX: usize = 300;

/// Each JSON chunk starts with a short header: the file's path, the key
/// paths it holds (`Weapons[].Tiers[].DamageMin`), and the `$Comment`,
/// `Description` and `Name` strings of it and of the objects it sits in,
/// then its raw text. On 2026-10-06 "base damage ranges for each weapon"
/// could not find data/equipment/WeaponBases.json, whose chunks are keys and
/// numbers only; the header says what the numbers are.
fn with_json_headers(rel_path: &str, text: &str, pieces: Vec<(usize, String)>) -> Vec<(usize, String)> {
    let starts: Vec<usize> = pieces.iter().map(|(line, _)| *line).collect();
    let outline = json_outline(text, &starts);
    pieces
        .into_iter()
        .zip(outline.at_start)
        .map(|((line, body), (enclosing, ancestor_names))| {
            let end = line + body.lines().count().saturating_sub(1);
            let in_chunk = |l: &usize| (line..=end).contains(l);
            let keys = std::iter::once(enclosing)
                .filter(|p| !p.is_empty())
                .chain(outline.keys.iter().filter(|(l, _)| in_chunk(l)).map(|(_, p)| p.clone()));
            let names = ancestor_names
                .into_iter()
                .chain(outline.names.iter().filter(|(l, _)| in_chunk(l)).map(|(_, n)| n.clone()));
            let mut header = format!("{rel_path}\nkeys: {}\n", joined_within(keys, " ", JSON_KEYS_MAX));
            let names = joined_within(names, " · ", JSON_NAMES_MAX);
            if !names.is_empty() {
                header.push_str(&names);
                header.push('\n');
            }
            (line, header + &body)
        })
        .collect()
}

/// Distinct `items` joined by `sep`, stopping before the one that would pass
/// `max` bytes.
fn joined_within(items: impl Iterator<Item = String>, sep: &str, max: usize) -> String {
    let mut out = String::new();
    let mut seen = std::collections::HashSet::new();
    for item in items {
        if item.is_empty() || !seen.insert(item.clone()) {
            continue;
        }
        if !out.is_empty() && out.len() + sep.len() + item.len() > max {
            break;
        }
        if !out.is_empty() {
            out.push_str(sep);
        }
        out.push_str(&item);
    }
    out
}

/// What [`json_outline`] reads from a JSON text.
#[derive(Debug, Default)]
struct JsonOutline {
    /// (line, key path) for every key, in order.
    keys: Vec<(usize, String)>,
    /// (line, value) for every string under a [`JSON_NAME_KEYS`] key.
    names: Vec<(usize, String)>,
    /// For each asked start line: the path of the container open there, and
    /// the names already read in the objects open there.
    at_start: Vec<(String, Vec<String>)>,
}

/// One open object or array while reading JSON.
struct JsonFrame {
    array: bool,
    /// The key this container is the value of.
    key: Option<String>,
    names: Vec<String>,
}

fn json_path(stack: &[JsonFrame], key: Option<&str>) -> String {
    let mut path = String::new();
    for frame in stack {
        if let Some(k) = &frame.key {
            if !path.is_empty() {
                path.push('.');
            }
            path.push_str(k);
        }
        if frame.array {
            path.push_str("[]");
        }
    }
    if let Some(k) = key {
        if !path.is_empty() {
            path.push('.');
        }
        path.push_str(k);
    }
    path
}

/// Read a JSON text's keys, line by line, without parsing it into values: a
/// file that is not quite JSON (comments, a trailing comma) still yields
/// what it can. `starts` are the 1-based lines whose surroundings are wanted,
/// in order.
fn json_outline(text: &str, starts: &[usize]) -> JsonOutline {
    let mut out = JsonOutline::default();
    let mut stack: Vec<JsonFrame> = Vec::new();
    // The key whose value comes next, and a string just read that is not yet
    // known to be a key or a value.
    let mut pending_key: Option<String> = None;
    let mut last_string: Option<(usize, String)> = None;
    let mut in_string = false;
    let mut escaped = false;
    let mut buf = String::new();
    let mut line = 1usize;
    let mut next_start = 0usize;
    let snapshot = |stack: &[JsonFrame], line: usize, next_start: &mut usize, at_start: &mut Vec<(String, Vec<String>)>| {
        while *next_start < starts.len() && starts[*next_start] <= line {
            at_start.push((json_path(stack, None), stack.iter().flat_map(|f| f.names.iter().cloned()).collect()));
            *next_start += 1;
        }
    };
    // A string that turned out to be a value: kept when its key is a name.
    let value = |stack: &mut Vec<JsonFrame>, pending_key: &mut Option<String>, last: Option<(usize, String)>, names: &mut Vec<(usize, String)>| {
        let key = pending_key.take();
        let (Some((l, s)), Some(k)) = (last, key) else { return };
        if JSON_NAME_KEYS.iter().any(|n| n.eq_ignore_ascii_case(&k)) && !s.trim().is_empty() {
            if let Some(top) = stack.last_mut() {
                top.names.push(s.clone());
            }
            names.push((l, s));
        }
    };
    snapshot(&stack, line, &mut next_start, &mut out.at_start);
    for c in text.chars() {
        if c == '\n' {
            line += 1;
            if !in_string {
                snapshot(&stack, line, &mut next_start, &mut out.at_start);
            }
        }
        if in_string {
            if escaped {
                escaped = false;
                buf.push(c);
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
                last_string = Some((line, std::mem::take(&mut buf)));
            } else {
                buf.push(c);
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                buf.clear();
            }
            ':' => {
                if let Some((l, key)) = last_string.take() {
                    if stack.last().is_some_and(|f| !f.array) {
                        out.keys.push((l, json_path(&stack, Some(&key))));
                    }
                    pending_key = Some(key);
                }
            }
            '{' | '[' => {
                last_string = None;
                stack.push(JsonFrame { array: c == '[', key: pending_key.take(), names: Vec::new() });
            }
            '}' | ']' => {
                value(&mut stack, &mut pending_key, last_string.take(), &mut out.names);
                stack.pop();
            }
            ',' => value(&mut stack, &mut pending_key, last_string.take(), &mut out.names),
            _ => {}
        }
    }
    // Start lines past the end (a text with no final newline).
    while out.at_start.len() < starts.len() {
        out.at_start.push((String::new(), Vec::new()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_for_picks_prose_for_markdown() {
        let p = IndexProfile::default_for("docs/readme.md");
        assert_eq!(p.mode, ChunkMode::Prose);
        assert_eq!(p.target_tokens, 350);
        assert!((p.overlap_pct - 0.15).abs() < f32::EPSILON);
    }

    #[test]
    fn default_for_picks_code_for_rust() {
        let p = IndexProfile::default_for("crates/ken-core/src/lib.rs");
        assert_eq!(p.mode, ChunkMode::Code);
        assert_eq!(p.target_tokens, 500);
        assert_eq!(p.overlap_pct, 0.0);
    }

    #[test]
    fn skip_mode_produces_no_chunks() {
        let profile = IndexProfile { mode: ChunkMode::Skip, target_tokens: 350, overlap_pct: 0.0 };
        assert!(chunk_file("noisy.log", "line one\nline two\nline three\n", &profile).is_empty());
    }

    #[test]
    fn default_for_falls_back_to_prose_for_unknown_extension() {
        let p = IndexProfile::default_for("weird_file.xyz123");
        assert_eq!(p, IndexProfile::default());
    }

    #[test]
    fn empty_text_yields_no_chunks() {
        let profile = IndexProfile::default();
        assert!(chunk_file("empty.md", "   \n\n  ", &profile).is_empty());
    }

    #[test]
    fn chunking_is_deterministic() {
        let text = "# Heading\n\nSome paragraph text that repeats a fair bit to build up length. "
            .repeat(20);
        let profile = IndexProfile::default_for("notes.md");
        let a = chunk_file("notes.md", &text, &profile);
        let b = chunk_file("notes.md", &text, &profile);
        assert_eq!(a, b);
        assert!(!a.is_empty());
    }

    #[test]
    fn prose_mode_respects_heading_boundaries() {
        let text = "Intro paragraph one.\n\nIntro paragraph two continues the thought.\n\n\
                     # Section Two\n\nBody of section two goes here.";
        let profile = IndexProfile {
            mode: ChunkMode::Prose,
            target_tokens: 350,
            overlap_pct: 0.15,
        };
        let chunks = chunk_file("doc.md", text, &profile);
        // The heading starts its own chunk rather than being folded into the
        // middle of the previous section's chunk.
        assert!(chunks
            .iter()
            .any(|c| c.text.trim_start().starts_with("# Section Two")));
        // And it is not glued onto the tail of the intro paragraphs either.
        assert!(!chunks[0].text.contains("# Section Two"));
    }

    #[test]
    fn prose_mode_produces_overlap_between_adjacent_chunks() {
        let mut text = String::new();
        for i in 0..30 {
            text.push_str(&format!(
                "Paragraph number {i} has some unique filler content padding it out.\n\n"
            ));
        }
        let profile = IndexProfile {
            mode: ChunkMode::Prose,
            target_tokens: 50, // target_chars = 200
            overlap_pct: 0.2,  // overlap_chars = 40
        };
        let chunks = chunk_file("doc.md", &text, &profile);
        assert!(
            chunks.len() >= 2,
            "expected multiple chunks, got {}",
            chunks.len()
        );
        let overlap_chars = 40usize;
        assert!(chunks[0].text.len() >= overlap_chars);
        let expected_tail = &chunks[0].text[chunks[0].text.len() - overlap_chars..];
        assert!(
            chunks[1].text.starts_with(expected_tail),
            "chunk 1 should open with chunk 0's trailing {overlap_chars} chars;\n\
             chunk0={:?}\nchunk1={:?}",
            chunks[0].text,
            chunks[1].text
        );
    }

    #[test]
    fn code_mode_splits_into_line_blocks_without_overlap() {
        let mut text = String::new();
        for i in 0..200 {
            text.push_str(&format!("let x{i} = {i}; // padding line to build length\n"));
        }
        let profile = IndexProfile {
            mode: ChunkMode::Code,
            target_tokens: 50,
            overlap_pct: 0.0,
        };
        let chunks = chunk_file("main.rs", &text, &profile);
        assert!(chunks.len() >= 2, "expected multiple chunks");
        let last_line_of_first = chunks[0].text.lines().last().unwrap();
        let first_line_of_second = chunks[1].text.lines().next().unwrap();
        assert_ne!(
            last_line_of_first, first_line_of_second,
            "code mode must not repeat lines across chunk boundaries"
        );
    }

    /// Each chunk knows the line its own content starts on: the heading's
    /// line for a section, the first new paragraph after an overlap tail,
    /// and the first line of a code block.
    #[test]
    fn chunks_carry_the_line_their_content_starts_on() {
        let text = "Intro.\n\n\n# Section Two\n\nBody of two.\n";
        let profile = IndexProfile { mode: ChunkMode::Prose, target_tokens: 350, overlap_pct: 0.15 };
        let chunks = chunk_file("doc.md", text, &profile);
        assert_eq!(chunks[0].line, 1);
        assert_eq!(chunks[1].line, 4, "the heading's line");

        let mut prose = String::new();
        for i in 0..30 {
            prose.push_str(&format!("Paragraph {i} with filler content to pad it.\n\n"));
        }
        let profile = IndexProfile { mode: ChunkMode::Prose, target_tokens: 50, overlap_pct: 0.2 };
        for c in chunk_file("doc.md", &prose, &profile) {
            // The chunk's first new paragraph is the one on its line.
            let own = prose.lines().nth(c.line - 1).unwrap();
            assert!(c.text.contains(own), "line {} = {own:?} not in {:?}", c.line, c.text);
        }

        let code: String = (0..200).map(|i| format!("let x{i} = {i}; // padding line\n")).collect();
        let profile = IndexProfile { mode: ChunkMode::Code, target_tokens: 50, overlap_pct: 0.0 };
        for c in chunk_file("main.rs", &code, &profile) {
            assert_eq!(code.lines().nth(c.line - 1).unwrap(), c.text.lines().next().unwrap());
        }
    }

    #[test]
    fn chunking_enforces_two_hundred_chunk_cap() {
        let mut text = String::new();
        for i in 0..500 {
            text.push_str(&format!("line {i} of a very large generated file padding content\n"));
        }
        let profile = IndexProfile {
            mode: ChunkMode::Code,
            target_tokens: 5, // tiny target -> far more than 200 blocks pre-cap
            overlap_pct: 0.0,
        };
        let chunks = chunk_file("generated.rs", &text, &profile);
        assert_eq!(chunks.len(), CHUNK_CAP);
        assert_eq!(chunks.last().unwrap().seq, CHUNK_CAP - 1);
    }

    #[test]
    fn a_table_longer_than_a_chunk_is_cut_at_its_rows_and_keeps_its_header() {
        let header = "| id | cited at | the comment |\n|---|---|---|\n";
        let mut text = format!("# Decisions Cited in Code\n\n{header}");
        for i in 0..200 {
            text.push_str(&format!("| D-{i:03} | `game:src/save{i}.rs:{i}` | // D-{i:03}: worlds save as region files |\n"));
        }
        let chunks = chunk_file("decisions/Cited-in-Code.md", &text, &IndexProfile::default_for("x.md"));
        assert_eq!(chunks[0].text, "# Decisions Cited in Code", "the heading before it stays its own chunk");
        assert!(chunks.len() > 10, "{} chunks", chunks.len());
        for c in &chunks[1..] {
            assert!(c.text.len() <= 350 * 4, "{} bytes", c.text.len());
            assert!(c.text.starts_with(header), "every piece reads as the table: {:?}", c.text);
        }
        let rows: Vec<&str> = chunks.iter().flat_map(|c| c.text.lines().filter(|l| l.starts_with("| D-"))).collect();
        assert_eq!(rows.len(), 200, "each row once");
        // A piece is cited at its first row, not at the header it repeats.
        let first_row = chunks[2].text.lines().nth(2).unwrap();
        assert_eq!(chunks[2].line, text.lines().position(|l| l == first_row).unwrap() + 1);
    }

    #[test]
    fn a_prose_file_too_long_for_the_cap_keeps_its_end() {
        let text: String = (0..6000).map(|i| format!("row {i:04} of a generated listing, wide enough to fill a page\n")).collect();
        let chunks = chunk_file("evidence/boot.txt", &text, &IndexProfile::default_for("boot.txt"));
        assert!(chunks.len() <= CHUNK_CAP, "{} chunks", chunks.len());
        assert!(chunks.last().unwrap().text.ends_with("row 5999 of a generated listing, wide enough to fill a page"));
    }

    #[test]
    fn content_hash_is_stable_and_reflects_text() {
        let profile = IndexProfile::default();
        let a = chunk_file(
            "a.md",
            "Hello world, this is a stable chunk of prose text.",
            &profile,
        );
        let b = chunk_file(
            "a.md",
            "Hello world, this is a stable chunk of prose text.",
            &profile,
        );
        assert_eq!(a[0].content_hash, b[0].content_hash);

        let c = chunk_file(
            "a.md",
            "Hello world, this is a DIFFERENT chunk of prose text.",
            &profile,
        );
        assert_ne!(a[0].content_hash, c[0].content_hash);
    }

    #[test]
    fn a_json_chunk_says_what_its_numbers_are() {
        let mut text = String::from("{\n  \"$Comment\": \"Base damage per weapon, by tier.\",\n  \"Weapons\": [\n");
        for i in 0..40 {
            text.push_str(&format!(
                "    {{\n      \"Name\": \"Sword {i}\",\n      \"Tiers\": [\n        {{ \"DamageMin\": {i}, \"DamageMax\": {} }}\n      ]\n    }},\n",
                i + 5
            ));
        }
        text.push_str("    { \"Name\": \"Last\" }\n  ]\n}\n");
        let profile = IndexProfile { mode: ChunkMode::Code, target_tokens: 60, overlap_pct: 0.0 };
        let chunks = chunk_file("data/equipment/WeaponBases.json", &text, &profile);
        assert!(chunks.len() > 3, "{}", chunks.len());

        let first = &chunks[0].text;
        assert!(first.starts_with("data/equipment/WeaponBases.json\nkeys: $Comment Weapons Weapons[].Name Weapons[].Tiers"), "{first}");
        assert!(first.contains("Base damage per weapon, by tier."), "{first}");
        // The raw text is still there, after the header.
        assert!(first.contains("\"DamageMin\": 0"), "{first}");

        // A chunk from the middle knows where it sits and what holds it.
        let mid = &chunks[2].text;
        assert!(mid.contains("keys: Weapons[]"), "{mid}");
        assert!(mid.contains("Weapons[].Tiers[].DamageMin"), "{mid}");
        assert!(mid.contains("Base damage per weapon, by tier."), "the file's comment holds every chunk: {mid}");
        // Its line is still the file line its own text starts on, the one
        // after the three header lines (path, keys, names).
        assert_eq!(mid.lines().nth(3).unwrap(), text.lines().nth(chunks[2].line - 1).unwrap());
    }

    #[test]
    fn json_outline_reads_keys_names_and_what_is_open() {
        let text = "{\n \"Bases\": {\n  \"Sword_T1\": {\n   \"MinLow\": 4,\n   \"Name\": \"Stone \\\"Crude\\\" sword\"\n  }\n },\n \"List\": [1, 2, \"x\"]\n}\n";
        let o = json_outline(text, &[1, 4, 8]);
        let keys: Vec<&str> = o.keys.iter().map(|(_, k)| k.as_str()).collect();
        assert_eq!(keys, ["Bases", "Bases.Sword_T1", "Bases.Sword_T1.MinLow", "Bases.Sword_T1.Name", "List"]);
        assert_eq!(o.names, vec![(5, "Stone \"Crude\" sword".to_string())]);
        assert_eq!(o.at_start[0], (String::new(), vec![]));
        assert_eq!(o.at_start[1].0, "Bases.Sword_T1");
        assert_eq!(o.at_start[2].0, "", "back at the top by line 8");
        // Not JSON at all: no keys, no panic, one answer per start line.
        let o = json_outline("not { json [ at \" all", &[1, 9]);
        assert!(o.keys.is_empty() && o.at_start.len() == 2);
    }

    #[test]
    fn only_json_gets_a_header() {
        let profile = IndexProfile::default_for("a.yaml");
        let chunks = chunk_file("a.yaml", "Name: x\nDamage: 4\n", &profile);
        assert_eq!(chunks[0].text, "Name: x\nDamage: 4");
    }

    fn ruling(i: usize) -> String {
        format!("**D-{i:03}** · 2026-10-06 · anchors, tools — **SPAWN ANCHORS RULE {i}.** Chris, in chat. Placed only through the tools, never the commands; case {i} of many, padded so two rulings would fill a chunk together.")
    }

    #[test]
    fn a_decisions_log_is_one_entry_per_chunk_past_the_cap() {
        let mut text = String::from("---\ntags: [decisions]\n---\n\n# DECISIONS\n\nThe permanent record of every ruling.\n\n## THE LOG\n\n### 2026-10-06\n\n");
        for i in (1..=300).rev() {
            if i == 150 {
                text.push_str("### 2026-10-05\n\n");
            }
            text.push_str(&ruling(i));
            text.push_str("\n\n");
        }
        let chunks = chunk_file("decisions/DECISIONS.md", &text, &IndexProfile::default_for("x.md"));
        let entries: Vec<&Chunk> = chunks.iter().filter(|c| c.text.contains("**D-")).collect();
        assert_eq!(entries.len(), 300, "every ruling, past the cap of {CHUNK_CAP}");
        for c in &entries {
            assert_eq!(c.text.matches("**D-").count(), 1, "one ruling per chunk: {}", c.text);
            let (id, title) = chunk_entry(&c.text).unwrap();
            let line = text.lines().nth(c.line - 1).unwrap();
            assert!(line.starts_with(&format!("**{id}**")), "cited at the ruling's own line: {line}");
            assert!(title.starts_with("SPAWN ANCHORS RULE"), "{title}");
        }
        let d150 = entries.iter().find(|c| c.text.contains("**D-150**")).unwrap();
        assert!(d150.text.starts_with("### 2026-10-05\n\n**D-150**"), "the heading it sits under: {}", d150.text);
        assert!(entries.iter().find(|c| c.text.contains("**D-151**")).unwrap().text.starts_with("### 2026-10-06"));
        let preamble = chunks.iter().position(|c| c.text.contains("The permanent record")).expect("the preamble is chunked as prose");
        assert!(preamble < 3 && chunks[..preamble].iter().all(|c| !c.text.contains("**D-")), "{chunks:?}");
        assert!(!chunks.iter().any(|c| c.text.trim() == "### 2026-10-05"), "a heading alone is context, not a chunk");
        assert!(is_log("decisions/DECISIONS.md", &text) && !is_log("decisions/DECISIONS.txt", &text));
    }

    #[test]
    fn a_heading_per_entry_log_and_its_titles() {
        let mut text = String::from("# Research log\n\n");
        for i in 1..=6 {
            text.push_str(&format!("## R-{i}: Spike number {i}\n\nWhat the spike found, {i}.\n\nAnd a second paragraph.\n\n"));
        }
        let chunks = chunk_file("Research/LOG.md", &text, &IndexProfile::default_for("x.md"));
        assert_eq!(chunks.len(), 6, "{chunks:?}");
        assert_eq!(chunk_entry(&chunks[2].text), Some(("R-3".to_string(), "Spike number 3".to_string())));
        assert!(chunks[0].text.starts_with("# Research log\n\n## R-1"), "{}", chunks[0].text);

        assert_eq!(entry_start("- **SR-012** · done — **Bump the engine.**"), Some(("SR-012".into(), "Bump the engine".into())));
        assert_eq!(entry_start("**U-4** undated: no second bold"), Some(("U-4".into(), "undated: no second bold".into())));
        for not in ["### 2026-10-06", "**Note** a bold word", "**D-12x** not an id", "## D-12abc", "D-12 not bold", "**d-12** lower case"] {
            assert_eq!(entry_start(not), None, "{not}");
        }
    }

    #[test]
    fn a_line_reads_as_its_section_or_its_log_entry() {
        let page = "---\ntitle: Grades\n---\n\n# Grades\n\nIntro.\n\n## Levers\n\nVariety first.\n\n```md\n# not a heading\n```\n\n### Detail\n\nWind-up.\n\n## Health\n\nLast and least.\n";
        assert_eq!(section_at("Design/Law.md", page, 11), Some((9, 19, Some("## Levers".into()))), "to the next heading of its level, past a ###");
        assert_eq!(section_at("Design/Law.md", page, 18), Some((17, 19, Some("### Detail".into()))));
        assert_eq!(section_at("Design/Law.md", page, 14), Some((9, 19, Some("## Levers".into()))), "a # in a fence is not a heading");
        assert_eq!(section_at("Design/Law.md", page, 2), Some((1, 23, Some("# Grades".into()))), "frontmatter alone reads on into the first section");
        assert_eq!(section_at("Design/Law.md", page, 99), None);
        let notes = "Preamble line.\n\n# Title\n\nBody.\n";
        assert_eq!(section_at("Notes.md", notes, 1), Some((1, 1, None)));

        let mut log = String::from("# DECISIONS\n\nThe record.\n\n## THE LOG\n\n### 2026-10-06\n\n");
        for i in (1..=6).rev() {
            log.push_str(&format!("**D-{i:03}** · 2026-10-06 · pets — **RULING {i}.** Chris.\nA second line of ruling {i}.\n\n"));
        }
        // D-006 starts on line 9: its two lines, not the next entry.
        assert_eq!(section_at("decisions/DECISIONS.md", &log, 10), Some((9, 10, Some("D-006".into()))));
        assert_eq!(section_at("decisions/DECISIONS.md", &log, 12), Some((12, 13, Some("D-005".into()))));
        assert_eq!(section_at("decisions/DECISIONS.md", &log, 3).map(|s| s.2), Some(Some("# DECISIONS".into())), "not an entry: its section");
    }

    #[test]
    fn an_entry_head_gives_its_date_and_topics() {
        let line = "**D-410** · 2026-10-06 · anchors, World Tool — **SPAWN ANCHORS ARE PLACED ONLY THROUGH THE TOOLS.** Chris, 2026-10-07.";
        assert_eq!(entry_date(line).as_deref(), Some("2026-10-06"), "the head's date, not one in the body");
        assert_eq!(entry_topics(line), vec!["anchors", "world tool"]);
        assert_eq!(entry_line(&format!("### 2026-10-06\n\n{line}\n")), Some(line));
        assert_eq!(entry_date("- **SR-012** · done — **Bump the engine.**"), None);
        assert_eq!(entry_topics("- **SR-012** · done — **Bump the engine.**"), vec!["done"]);
        assert_eq!(entry_date("## R-3: Spike number 3, 2026-01-02"), None, "a heading entry has no head");
        assert_eq!(entry_head("plain prose"), None);
    }

    /// Prose with a few bold ids, or a few entries in a long page, is chunked
    /// exactly as before.
    #[test]
    fn a_page_that_is_not_a_log_is_chunked_as_prose() {
        let profile = IndexProfile::default_for("x.md");
        let prose: String = (0..40).map(|i| format!("Paragraph {i} of the design notes, long enough to pack a few to a chunk.\n\n")).collect();
        let few = format!("{prose}{}\n\n{}\n\n", ruling(1), ruling(2));
        let mut padded = "A long section of notes that is not a ruling at all, and outweighs them.\n\n".repeat(60);
        padded.push_str(&(1..=6).map(ruling).collect::<Vec<_>>().join("\n\n"));
        for text in [few, padded] {
            let expected: Vec<String> = chunk_prose(&text, &profile).into_iter().map(|(_, t)| t).collect();
            let got: Vec<String> = chunk_file("Design/Notes.md", &text, &profile).into_iter().map(|c| c.text).collect();
            assert_eq!(got, expected);
        }
    }

    #[test]
    fn token_estimate_is_len_over_four() {
        let profile = IndexProfile::default();
        let text = "0123456789"; // 10 chars
        let chunks = chunk_file("a.txt", text, &profile);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].token_est, 10 / 4);
    }

    #[test]
    fn the_templates_plain_entries_make_a_log_too() {
        // The method's template writes `D-001 - date - topic - RULING.`, with
        // its format example in a fence above the log.
        let mut text = String::from("# Decisions\n\n## Entry Format\n\n```\nD-001 - 2026-01-01 - topic - THE RULING IN ONE BOLD SENTENCE.\n```\n\n## Log\n\n");
        for i in 1..=6 {
            text.push_str(&format!("D-00{i} - 2026-09-0{i} - saves, regions - RULING NUMBER {i}.\n  Why it was ruled, the traps around it and what it replaced: {i}.\n  sources: game:src/save.rs:{i}\n\n"));
        }
        let chunks = chunk_file("decisions/DECISIONS.md", &text, &IndexProfile::default_for("x.md"));
        let entries: Vec<_> = chunks.iter().filter_map(|c| chunk_entry(&c.text)).collect();
        assert_eq!(entries.len(), 6, "one chunk per ruling, the fenced example none: {chunks:?}");
        assert_eq!(entries[2], ("D-003".to_string(), "RULING NUMBER 3".to_string()));
        assert!(is_log("decisions/DECISIONS.md", &text));

        let line = "D-003 - 2026-09-03 - saves, regions - RULING NUMBER 3.";
        assert_eq!(entry_date(line).as_deref(), Some("2026-09-03"));
        assert_eq!(entry_topics(line), vec!["saves", "regions"]);
        let dot = "D-001 · 2026-09-02 · save format — Worlds save as region files.";
        assert_eq!(entry_start(dot), Some(("D-001".into(), "Worlds save as region files".into())));
        assert_eq!(entry_topics(dot), vec!["save format"]);
        for not in ["D-12 not bold", "SR-121 - was cancelled", "D-4 - see the board, 2026-09-01"] {
            assert_eq!(entry_start(not), None, "a sentence that starts with an id: {not}");
        }

        assert_eq!(unbold_id("**D-410** · 2026-10-06 · anchors — **X.**"), "D-410 · 2026-10-06 · anchors — **X.**");
        assert_eq!(unbold_id("- **SR-12** · done"), "SR-12 · done");
        assert_eq!(unbold_id("**Note** a bold word"), "**Note** a bold word");
    }
}

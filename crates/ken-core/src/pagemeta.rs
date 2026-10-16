//! What a wiki page says about itself, read from its frontmatter, and which
//! library section it sits in. Search uses both: binding knowledge ranks
//! first, Research is dated evidence, and a superseded page is labelled and
//! ranked last (knowledge-core decisions 5, 7 and 23).
//!
//! A page has seven keys (decisions 5 and 7): `title`, `aliases`, `status`
//! (draft, current or superseded), `updated`, `sources` (each
//! `repo@sha:path` or `[[Note]]`), `checked` (the date and commit of the
//! check that last re-read the page) and, on a retired page,
//! `superseded_by` (the page that replaces it), plus an optional
//! `reviewed_by`. Two older keys are still read, never written:
//! `replaced_by`, read as `superseded_by`, and `generated`, so pages written
//! before keep their rank and label.
//!
//! The parser is deliberately small and tolerant, not YAML: pages copied
//! from the templates carry `{{date}}` placeholders and trailing `#`
//! comments, which a YAML parser rejects outright. It reads only the keys
//! above and ignores everything else.

use std::fmt;

use serde::Serialize;

/// The frontmatter keys Ken reads from a page.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageMeta {
    pub title: Option<String>,
    /// The questions people type; a name link also resolves by these.
    pub aliases: Vec<String>,
    /// A page's `draft` | `current` | `superseded`, lowercased. The older
    /// `retired` and `deprecated` read as superseded. A ticket keeps its own
    /// statuses (`todo`, `done`, `cancelled`…), read the same way.
    pub status: Option<String>,
    /// When it last changed (`updated`, or `date` on an older note).
    pub updated: Option<String>,
    pub sources: Vec<String>,
    /// When the check last re-read the page against its sources, and at
    /// which commit.
    pub checked: Option<Checked>,
    /// Who reviewed the page, when a person did.
    pub reviewed_by: Option<String>,
    /// The pages that replace a retired one, each a path or a `[[link]]`:
    /// `superseded_by`, or an older page's `replaced_by`. Written beside
    /// `status: superseded` when a page is retired ([`retire`]).
    pub superseded_by: Vec<String>,
    /// Legacy, read only: built by a generator (a `generated` key, or
    /// `status: generated`).
    pub generated: bool,
}

/// What the check that re-read a page stamps on it: `checked: 2026-10-13
/// f00218c`, the date and then the commit it read at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Checked {
    pub date: String,
    pub sha: Option<String>,
}

impl Checked {
    /// The written form: a `YYYY-MM-DD` date, then optionally the commit
    /// (7 to 40 hex digits), set apart by a space, `@` or a comma. None
    /// without a date.
    pub fn parse(v: &str) -> Option<Checked> {
        let v = v.trim();
        let date = date(v)?;
        let sha = v[10..]
            .split(|c: char| c.is_whitespace() || c == '@' || c == ',')
            .find(|w| !w.is_empty())
            .filter(|w| (7..=40).contains(&w.len()) && w.chars().all(|c| c.is_ascii_hexdigit()))
            .map(str::to_ascii_lowercase);
        Some(Checked { date, sha })
    }
}

impl fmt::Display for Checked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.sha {
            Some(sha) => write!(f, "{} {sha}", self.date),
            None => f.write_str(&self.date),
        }
    }
}

impl PageMeta {
    /// No longer true: `status: superseded` (or an older `retired` or
    /// `deprecated`), or a page that names its successor. Kept as evidence
    /// and ranked last.
    pub fn retired(&self) -> bool {
        matches!(self.status.as_deref(), Some("superseded" | "retired" | "deprecated"))
            || !self.superseded_by.is_empty()
    }
}

/// A successor as a hit's label names it: `[[Design/New|the new design]]`
/// is `Design/New`; a path stays as written.
pub fn successor_name(s: &str) -> String {
    let s = s.trim();
    let inner = s.strip_prefix("[[").and_then(|t| t.strip_suffix("]]")).unwrap_or(s);
    inner.split('|').next().unwrap_or(inner).trim().to_string()
}

/// `text` retired in favour of `successor` (a path or a `[[link]]`):
/// `status: superseded` and `superseded_by:` set in its frontmatter, the
/// lines they replace (an older `replaced_by` too) dropped. A page with no
/// frontmatter gets one. The page's line endings are kept.
pub fn retire(text: &str, successor: &str) -> String {
    let crlf = text.contains("\r\n");
    let successor = successor.trim();
    let written = if successor.starts_with("[[") { format!("\"{successor}\"") } else { successor.to_string() };
    let body = text.replace("\r\n", "\n");
    let lines: Vec<&str> = body.lines().collect();
    let close = (lines.first().map(|l| l.trim_end()) == Some("---"))
        .then(|| lines.iter().skip(1).position(|l| l.trim_end() == "---").map(|p| p + 1))
        .flatten();
    let stamp = ["status: superseded".to_string(), format!("superseded_by: {written}")];
    let mut out: Vec<String> = vec!["---".into()];
    match close {
        Some(close) => {
            // A key's block list goes with it.
            let mut dropping = false;
            for l in &lines[1..close] {
                if !l.starts_with([' ', '\t', '-']) {
                    let key = l.split_once(':').map(|(k, _)| k.trim().to_ascii_lowercase());
                    dropping = matches!(key.as_deref(), Some("status" | "superseded_by" | "replaced_by"));
                }
                if !dropping {
                    out.push(l.to_string());
                }
            }
            out.extend(stamp);
            out.extend(lines[close..].iter().map(|l| l.to_string()));
        }
        None => {
            out.extend(stamp);
            out.push("---".into());
            out.extend(lines.iter().map(|l| l.to_string()));
        }
    }
    let text = out.join("\n") + "\n";
    if crlf { text.replace('\n', "\r\n") } else { text }
}

/// Read the frontmatter at the top of `text`, if it has any. Accepts LF and
/// CRLF. Keys it does not know are skipped; values that are placeholders
/// (`{{…}}`) are dropped; dates must be `YYYY-MM-DD`.
pub fn parse(text: &str) -> Option<PageMeta> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.lines();
    if lines.next()?.trim_end() != "---" {
        return None;
    }
    let mut meta = PageMeta::default();
    let mut list_key: Option<String> = None;
    let mut closed = false;
    for raw in lines {
        let line = raw.trim_end();
        if line == "---" || line == "..." {
            closed = true;
            break;
        }
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(item) = trimmed.strip_prefix("- ").or_else(|| (trimmed == "-").then_some("")) {
            if let Some(key) = &list_key {
                if let Some(v) = scalar(item) {
                    push_list(&mut meta, key, v);
                }
            }
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = strip_comment(value).trim();
        list_key = None;
        if value.is_empty() {
            // A block list may follow (`sources:` then `  - …`).
            list_key = Some(key);
            continue;
        }
        if let Some(inner) = value.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
            for item in split_inline_list(inner) {
                if let Some(v) = scalar(&item) {
                    push_list(&mut meta, &key, v);
                }
            }
            continue;
        }
        let Some(v) = scalar(value) else { continue };
        match key.as_str() {
            "title" => meta.title = Some(v),
            "status" => {
                meta.generated |= v.eq_ignore_ascii_case("generated");
                meta.status = Some(v.to_ascii_lowercase());
            }
            "checked" => meta.checked = Checked::parse(&v),
            "reviewed_by" => push_list(&mut meta, &key, v),
            "updated" | "date" => {
                if meta.updated.is_none() {
                    meta.updated = date(&v);
                }
            }
            "generated" | "generated_by" | "generator" => {
                meta.generated = !matches!(v.to_ascii_lowercase().as_str(), "false" | "no");
            }
            "aliases" | "sources" | "superseded_by" | "replaced_by" => push_list(&mut meta, &key, v),
            _ => {}
        }
    }
    closed.then_some(meta)
}

fn push_list(meta: &mut PageMeta, key: &str, v: String) {
    match key {
        "aliases" => meta.aliases.push(v),
        "sources" => meta.sources.push(v),
        "superseded_by" | "replaced_by" => meta.superseded_by.push(v),
        // A list of reviewers reads as one line.
        "reviewed_by" => {
            meta.reviewed_by = Some(match meta.reviewed_by.take() {
                Some(r) => format!("{r}, {v}"),
                None => v,
            })
        }
        _ => {}
    }
}

/// A trailing ` # comment` is dropped; a `#` inside quotes is kept.
fn strip_comment(value: &str) -> &str {
    let mut quote: Option<char> = None;
    let mut prev_space = true;
    for (i, c) in value.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (None, '"' | '\'') => quote = Some(c),
            (None, '#') if prev_space => return &value[..i],
            _ => {}
        }
        prev_space = c.is_whitespace();
    }
    value
}

fn split_inline_list(inner: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in inner.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => {
                quote = None;
                cur.push(c);
            }
            (None, '"' | '\'') => {
                quote = Some(c);
                cur.push(c);
            }
            (None, ',') => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// Unquote a value; None for empty or a `{{placeholder}}`.
fn scalar(raw: &str) -> Option<String> {
    let v = strip_comment(raw).trim();
    let v = v
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(v)
        .trim();
    (!v.is_empty() && !v.contains("{{")).then(|| v.to_string())
}

/// `YYYY-MM-DD` at the start of `v`, else None.
fn date(v: &str) -> Option<String> {
    let d = v.get(..10)?;
    let b = d.as_bytes();
    let ok = b.iter().enumerate().all(|(i, c)| if i == 4 || i == 7 { *c == b'-' } else { c.is_ascii_digit() });
    ok.then(|| d.to_string())
}

/// A date in a file name (`Standup - 2026-09-12.md`), for dating a Research
/// note that has no frontmatter date.
pub fn date_in_name(rel_path: &str) -> Option<String> {
    date_in(rel_path.rsplit('/').next().unwrap_or(rel_path))
}

/// The first `YYYY-MM-DD` date anywhere in `text`.
pub fn date_in(text: &str) -> Option<String> {
    (0..text.len().saturating_sub(9)).find_map(|i| text.get(i..).and_then(date))
}

/// The knowledge base's top-level sections. Folders from the older layout
/// still map in: Ways-of-Working and Conventions are Rules, Platform is
/// Reference, Work is Current.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Section {
    Rules,
    Decisions,
    Current,
    Design,
    Reference,
    Research,
}

impl Section {
    pub fn name(self) -> &'static str {
        match self {
            Section::Rules => "Rules",
            Section::Decisions => "Decisions",
            Section::Current => "Current",
            Section::Design => "Design",
            Section::Reference => "Reference",
            Section::Research => "Research",
        }
    }
}

/// The section a page sits in: the first folder in its path named for one.
/// Only Markdown pages have one, so a code repo's `Reference/` folder of
/// sources is not mistaken for the knowledge base's.
pub fn section_of(rel_path: &str) -> Option<Section> {
    let path = rel_path.replace('\\', "/");
    if !(path.ends_with(".md") || path.ends_with(".markdown")) {
        return None;
    }
    let mut dirs: Vec<&str> = path.split('/').collect();
    dirs.pop();
    dirs.into_iter().find_map(|d| match d.to_ascii_lowercase().replace(['_', ' '], "-").as_str() {
        "rules" | "ways-of-working" | "conventions" => Some(Section::Rules),
        "decisions" => Some(Section::Decisions),
        "current" | "work" => Some(Section::Current),
        "design" => Some(Section::Design),
        "reference" | "platform" => Some(Section::Reference),
        "research" => Some(Section::Research),
        _ => None,
    })
}

/// What a search hit on a page carries: where it sits, how fresh it is, and
/// whether it is still current.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HitPage {
    pub section: Option<&'static str>,
    pub title: Option<String>,
    /// The date the check last re-read it against its sources.
    pub checked: Option<String>,
    /// For Research: the date the evidence is from, so nothing reads a past
    /// session's conclusion as current state.
    pub dated: Option<String>,
    /// Superseded ([`PageMeta::retired`]).
    pub retired: bool,
    /// Legacy: built by a generator.
    pub generated: bool,
    /// What replaces a retired page, when it says (`superseded_by`).
    pub superseded_by: Vec<String>,
}

/// The page facts for a hit on `rel_path`, or None when it is not a
/// Markdown page.
pub fn hit_page(rel_path: &str, meta: Option<PageMeta>) -> Option<HitPage> {
    let is_page = rel_path.ends_with(".md") || rel_path.ends_with(".markdown");
    if !is_page {
        return None;
    }
    let section = section_of(rel_path);
    let meta = meta.unwrap_or_default();
    let checked = meta.checked.as_ref().map(|c| c.date.clone());
    let dated = (section == Some(Section::Research))
        .then(|| meta.updated.clone().or_else(|| checked.clone()).or_else(|| date_in_name(rel_path)))
        .flatten();
    Some(HitPage {
        section: section.map(Section::name),
        retired: meta.retired(),
        generated: meta.generated,
        title: meta.title,
        checked,
        dated,
        superseded_by: meta.superseded_by,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_template_page_with_placeholders_and_comments() {
        let text = "---\r\ntitle: \"Project\"\r\naliases: [\"project\", \"what are we building\", \"what is the next milestone\"]\r\nstatus: current\r\nupdated: {{date}}\r\nchecked: 2026-09-20 f00218c      # written by the check that re-read it\r\nreviewed_by: Chris\r\nsources:\r\n  - \"[[Standup - 2026-09-19]]\"\r\n  - \"D-012\"\r\n---\r\n\r\n# Project\r\n";
        let m = parse(text).unwrap();
        assert_eq!(m.title.as_deref(), Some("Project"));
        assert_eq!(m.aliases, vec!["project", "what are we building", "what is the next milestone"]);
        assert_eq!(m.status.as_deref(), Some("current"));
        assert_eq!(m.checked, Some(Checked { date: "2026-09-20".into(), sha: Some("f00218c".into()) }));
        assert_eq!(m.reviewed_by.as_deref(), Some("Chris"));
        assert_eq!(m.updated, None, "a placeholder is not a date");
        assert_eq!(m.sources, vec!["[[Standup - 2026-09-19]]", "D-012"]);
        assert!(!m.retired() && !m.generated);
    }

    #[test]
    fn checked_is_a_date_then_a_commit() {
        let c = |v: &str| Checked::parse(v);
        assert_eq!(c("2026-10-13 f00218c").map(|c| c.to_string()).as_deref(), Some("2026-10-13 f00218c"));
        assert_eq!(c("2026-10-13 @ F00218C").and_then(|c| c.sha).as_deref(), Some("f00218c"));
        assert_eq!(c("2026-10-13, f00218cabc").and_then(|c| c.sha).as_deref(), Some("f00218cabc"));
        assert_eq!(c("2026-10-13"), Some(Checked { date: "2026-10-13".into(), sha: None }));
        assert_eq!(c("2026-10-13 by hand").and_then(|c| c.sha), None, "not a commit");
        assert_eq!(c("f00218c"), None, "a commit alone is not a check");
        assert_eq!(parse("---\nchecked: {{date}} {{sha}}\n---\n").unwrap().checked, None);
    }

    #[test]
    fn the_dropped_keys_are_not_read() {
        let m = parse("---\nverified: 2026-09-20\naudience: business\nlens: dev\nchanged_by: \"[[Standup]]\"\npin: abc1234\n---\n").unwrap();
        assert_eq!(m, PageMeta::default());
    }

    #[test]
    fn superseded_and_the_legacy_keys_say_so() {
        assert!(parse("---\nstatus: superseded\n---\n").unwrap().retired());
        assert!(parse("---\nstatus: retired\n---\n").unwrap().retired(), "an older value");
        assert!(parse("---\nstatus: deprecated\n---\n").unwrap().retired(), "an older value");
        assert!(!parse("---\nstatus: draft\n---\n").unwrap().retired());
        let retired = parse("---\nstatus: superseded\nreplaced_by: [\"[[Rules]]\"]\n---\n").unwrap();
        assert_eq!(retired.superseded_by, vec!["[[Rules]]"], "the older key reads as superseded_by");
        let named = parse("---\nstatus: superseded\nsuperseded_by: Design/New.md\n---\n").unwrap();
        assert_eq!(named.superseded_by, vec!["Design/New.md"]);
        assert!(parse("---\nsuperseded_by: \"[[New]]\"\n---\n").unwrap().retired());
        assert!(parse("---\ngenerated: tools/gen.py --all @ abc1234\n---\n").unwrap().generated);
        assert!(parse("---\nstatus: generated\n---\n").unwrap().generated);
        assert!(!parse("---\ngenerated: false\n---\n").unwrap().generated);
        assert_eq!(parse("---\nreviewed_by: [Ana, Ben]\n---\n").unwrap().reviewed_by.as_deref(), Some("Ana, Ben"));
    }

    #[test]
    fn retiring_a_page_names_its_successor() {
        let page = "---\ntitle: Old design\nstatus: current\nreplaced_by:\n  - \"[[Older]]\"\nupdated: 2026-07-21\n---\n\n# Old design\n";
        let out = retire(page, "Design/New.md");
        assert_eq!(out, "---\ntitle: Old design\nupdated: 2026-07-21\nstatus: superseded\nsuperseded_by: Design/New.md\n---\n\n# Old design\n");
        let m = parse(&out).unwrap();
        assert!(m.retired());
        assert_eq!(m.superseded_by, vec!["Design/New.md"]);
        let linked = retire("# Bare\r\n", "[[New]]");
        assert_eq!(linked, "---\r\nstatus: superseded\r\nsuperseded_by: \"[[New]]\"\r\n---\r\n# Bare\r\n");
        assert_eq!(parse(&linked).unwrap().superseded_by, vec!["[[New]]"]);
        assert_eq!(successor_name("[[Design/New|the new one]]"), "Design/New");
        assert_eq!(successor_name("Design/new.md"), "Design/new.md");
    }

    #[test]
    fn no_frontmatter_or_an_unclosed_block_reads_as_none() {
        assert_eq!(parse("# Just a page\n"), None);
        assert_eq!(parse("---\ntitle: never closed\n"), None);
        assert_eq!(parse(""), None);
    }

    #[test]
    fn a_hash_inside_quotes_is_not_a_comment() {
        let m = parse("---\ntitle: \"C# conventions\"  # the page\n---\n").unwrap();
        assert_eq!(m.title.as_deref(), Some("C# conventions"));
    }

    #[test]
    fn sections_come_from_the_folder_and_only_for_pages() {
        assert_eq!(section_of("Rules/Never-weaken-a-test.md"), Some(Section::Rules));
        assert_eq!(section_of("Ways-of-Working/Rules.md"), Some(Section::Rules), "the older name");
        assert_eq!(section_of("docs/Platform/Engine.md"), Some(Section::Reference), "the older name");
        assert_eq!(section_of("Reference/Platform/Engine.md"), Some(Section::Reference));
        assert_eq!(section_of("Decisions/DECISIONS.md"), Some(Section::Decisions));
        assert_eq!(section_of("Research/Ingestion/Ingested/Standup.md"), Some(Section::Research));
        assert_eq!(section_of("Reference/config.rs"), None, "not a page");
        assert_eq!(section_of("README.md"), None);
    }

    #[test]
    fn a_hit_page_dates_research_and_names_what_replaces_a_retired_page() {
        let research = hit_page("Research/Ingested/Standup - 2026-09-12.md", None).unwrap();
        assert_eq!((research.section, research.dated.as_deref()), (Some("Research"), Some("2026-09-12")));
        let meta = PageMeta { status: Some("superseded".into()), superseded_by: vec!["[[Rules]]".into()], ..Default::default() };
        let old = hit_page("Ways-of-Working/Old.md", Some(meta)).unwrap();
        assert!(old.retired);
        assert_eq!(old.superseded_by, vec!["[[Rules]]"]);
        assert_eq!(old.dated, None, "only Research is stamped");
        let checked = PageMeta { checked: Checked::parse("2026-09-20 f00218c"), ..Default::default() };
        assert_eq!(hit_page("Design/Why.md", Some(checked)).unwrap().checked.as_deref(), Some("2026-09-20"));
        assert_eq!(hit_page("src/main.rs", None), None);
    }

    #[test]
    fn a_date_in_the_file_name_dates_a_note() {
        assert_eq!(date_in_name("Research/Ingested/Standup - 2026-09-12.md").as_deref(), Some("2026-09-12"));
        assert_eq!(date_in_name("Research/2026-09-12-kickoff.md").as_deref(), Some("2026-09-12"));
        assert_eq!(date_in_name("Research/notes.md"), None);
    }
}

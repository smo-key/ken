//! What a wiki page says about itself, read from its frontmatter, and which
//! library section it sits in. Search uses both: binding and verified pages
//! rank first, Research is dated evidence, and a retired or generated page
//! is labelled and ranked last (Ways of Working, docs-system: "Binding and
//! verified first", "Retired Pages", "Generated Pages").
//!
//! The parser is deliberately small and tolerant, not YAML: pages copied
//! from the templates carry `{{date}}` placeholders and trailing `#`
//! comments, which a YAML parser rejects outright. It reads only the keys
//! the method defines and ignores everything else.

use serde::Serialize;

/// The frontmatter keys Ken reads from a page.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageMeta {
    pub title: Option<String>,
    /// The questions people type; a name link also resolves by these.
    pub aliases: Vec<String>,
    /// `current` | `evidence` | `superseded` | `retired` | `generated` | …
    pub status: Option<String>,
    /// When a person last read the page against its sources (`YYYY-MM-DD`).
    pub verified: Option<String>,
    /// When it last changed, if it says (`updated`, or `date` on a note).
    pub updated: Option<String>,
    pub sources: Vec<String>,
    /// The pages that replace a retired one.
    pub replaced_by: Vec<String>,
    /// Built by a generator (a `generated` key, or `status: generated`).
    pub generated: bool,
    /// `audience: business` or `audience: dev`, when the page says; else
    /// its section decides (see [`audience_of`]).
    pub audience: Option<String>,
}

impl PageMeta {
    /// No longer true: kept as evidence, served under its replacement.
    pub fn retired(&self) -> bool {
        matches!(self.status.as_deref(), Some("superseded" | "retired" | "deprecated"))
            || !self.replaced_by.is_empty()
    }
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
            "verified" => meta.verified = date(&v),
            "updated" | "date" => {
                if meta.updated.is_none() {
                    meta.updated = date(&v);
                }
            }
            "generated" | "generated_by" | "generator" => {
                meta.generated = !matches!(v.to_ascii_lowercase().as_str(), "false" | "no");
            }
            "audience" => meta.audience = Some(v.to_ascii_lowercase()),
            "aliases" | "sources" | "replaced_by" | "superseded_by" => push_list(&mut meta, &key, v),
            _ => {}
        }
    }
    closed.then_some(meta)
}

fn push_list(meta: &mut PageMeta, key: &str, v: String) {
    match key {
        "aliases" => meta.aliases.push(v),
        "sources" => meta.sources.push(v),
        "replaced_by" | "superseded_by" => meta.replaced_by.push(v),
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

/// The library's top-level sections (docs-system, "Sections").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Section {
    WaysOfWorking,
    Platform,
    Conventions,
    Design,
    Work,
    Reference,
    Current,
    Research,
}

impl Section {
    pub fn name(self) -> &'static str {
        match self {
            Section::WaysOfWorking => "Ways-of-Working",
            Section::Platform => "Platform",
            Section::Conventions => "Conventions",
            Section::Design => "Design",
            Section::Work => "Work",
            Section::Reference => "Reference",
            Section::Current => "Current",
            Section::Research => "Research",
        }
    }
}

/// The section a page sits in: the first folder in its path named for one.
/// Only Markdown pages have one, so a code repo's `Reference/` folder of
/// sources is not mistaken for the library's.
pub fn section_of(rel_path: &str) -> Option<Section> {
    let path = rel_path.replace('\\', "/");
    if !(path.ends_with(".md") || path.ends_with(".markdown")) {
        return None;
    }
    let mut dirs: Vec<&str> = path.split('/').collect();
    dirs.pop();
    dirs.into_iter().find_map(|d| match d.to_ascii_lowercase().replace(['_', ' '], "-").as_str() {
        "ways-of-working" => Some(Section::WaysOfWorking),
        "platform" => Some(Section::Platform),
        "conventions" => Some(Section::Conventions),
        "design" => Some(Section::Design),
        "work" => Some(Section::Work),
        "reference" => Some(Section::Reference),
        "current" => Some(Section::Current),
        "research" => Some(Section::Research),
        _ => None,
    })
}

/// Who a page is written for (item 2b). Business pages are for people who
/// never read code: what the product does, what was decided and why, what
/// changed in each release. Dev pages are conventions, architecture,
/// how-tos and code references. The method's own pages are neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Audience {
    Business,
    Dev,
    Method,
}

impl Audience {
    pub fn name(self) -> &'static str {
        match self {
            Audience::Business => "business",
            Audience::Dev => "dev",
            Audience::Method => "method",
        }
    }
}

/// Whether a search result suits the reader chosen: `business`, `dev`, or
/// none/`any` for everything. Business readers get pages written for them;
/// developers get everything else but the method's own pages, code and
/// documents outside the sections included. `audience` is the hit's
/// ([`HitPage::audience`], none for a file that is not a page).
pub fn suits(want: Option<&str>, audience: Option<&str>) -> bool {
    match want.map(str::trim).filter(|w| !w.is_empty() && *w != "any") {
        Some("business") => audience == Some("business"),
        Some("dev") => !matches!(audience, Some("business") | Some("method")),
        _ => true,
    }
}

/// The audience of the file at `path`, read from the index: a Markdown
/// page's frontmatter and section, none for anything else.
pub fn audience_at(db: &crate::db::Db, path: &str) -> Option<&'static str> {
    if !path.ends_with(".md") {
        return None;
    }
    let meta = db.page_meta(path).ok().flatten();
    audience_of(section_of(path), meta.as_ref()).map(Audience::name)
}

/// A page's audience: its frontmatter `audience:` when it says, else its
/// section. Current, Design and Work are business; Conventions, Platform and
/// Reference are dev; Ways-of-Working is the method. Research is evidence
/// for everyone and has none, nor does a page outside the sections.
pub fn audience_of(section: Option<Section>, meta: Option<&PageMeta>) -> Option<Audience> {
    match meta.and_then(|m| m.audience.as_deref()) {
        Some("business") => return Some(Audience::Business),
        Some("dev") | Some("developer") | Some("code") => return Some(Audience::Dev),
        Some("method") => return Some(Audience::Method),
        _ => {}
    }
    match section? {
        Section::Current | Section::Design | Section::Work => Some(Audience::Business),
        Section::Conventions | Section::Platform | Section::Reference => Some(Audience::Dev),
        Section::WaysOfWorking => Some(Audience::Method),
        Section::Research => None,
    }
}

/// Search bands, best first: binding and verified, then the rest, then
/// evidence and pages that are not current (Research, retired, generated).
pub fn band(section: Option<Section>, meta: Option<&PageMeta>) -> u8 {
    let retired_or_generated = meta.is_some_and(|m| m.retired() || m.generated);
    if retired_or_generated || section == Some(Section::Research) {
        2
    } else if matches!(section, Some(Section::WaysOfWorking | Section::Platform)) {
        0
    } else {
        1
    }
}

/// What a search hit on a page carries: where it sits, how fresh it is, and
/// whether it is still current.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HitPage {
    pub section: Option<&'static str>,
    pub title: Option<String>,
    pub verified: Option<String>,
    /// For Research: the date the evidence is from, so nothing reads a past
    /// session's conclusion as current state.
    pub dated: Option<String>,
    pub retired: bool,
    pub generated: bool,
    pub replaced_by: Vec<String>,
    /// [`band`]: 0 binding, 1 the rest, 2 evidence or not current.
    pub band: u8,
    /// Who it is written for ([`audience_of`]).
    pub audience: Option<&'static str>,
    /// How much the hit counts, in one short line (`authority::hit_label`):
    /// filled by the search that has the index, None until then.
    pub label: Option<String>,
}

/// The page facts for a hit on `rel_path`, or None when it is not a
/// Markdown page.
pub fn hit_page(rel_path: &str, meta: Option<PageMeta>) -> Option<HitPage> {
    let is_page = rel_path.ends_with(".md") || rel_path.ends_with(".markdown");
    if !is_page {
        return None;
    }
    let section = section_of(rel_path);
    let band = band(section, meta.as_ref());
    let audience = audience_of(section, meta.as_ref()).map(Audience::name);
    let meta = meta.unwrap_or_default();
    let dated = (section == Some(Section::Research))
        .then(|| meta.updated.clone().or_else(|| meta.verified.clone()).or_else(|| date_in_name(rel_path)))
        .flatten();
    Some(HitPage {
        section: section.map(Section::name),
        retired: meta.retired(),
        generated: meta.generated,
        title: meta.title,
        verified: meta.verified,
        dated,
        replaced_by: meta.replaced_by,
        band,
        audience,
        label: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_template_page_with_placeholders_and_comments() {
        let text = "---\r\ntitle: \"Project\"\r\naliases: [\"project\", \"what are we building\", \"what is the next milestone\"]\r\nstatus: current\r\nupdated: {{date}}\r\nverified: 2026-09-20      # a person read this against its sources; never stamped in bulk\r\nchanged_by: \"[[Standup - 2026-09-19]]\"\r\nsources:\r\n  - \"[[Standup - 2026-09-19]]\"\r\n  - \"D-012\"\r\n---\r\n\r\n# Project\r\n";
        let m = parse(text).unwrap();
        assert_eq!(m.title.as_deref(), Some("Project"));
        assert_eq!(m.aliases, vec!["project", "what are we building", "what is the next milestone"]);
        assert_eq!(m.status.as_deref(), Some("current"));
        assert_eq!(m.verified.as_deref(), Some("2026-09-20"));
        assert_eq!(m.updated, None, "a placeholder is not a date");
        assert_eq!(m.sources, vec!["[[Standup - 2026-09-19]]", "D-012"]);
        assert!(!m.retired() && !m.generated);
    }

    #[test]
    fn retired_and_generated_pages_say_so() {
        let retired = parse("---\nstatus: superseded\nreplaced_by: [\"[[Rules]]\"]\n---\n").unwrap();
        assert!(retired.retired());
        assert_eq!(retired.replaced_by, vec!["[[Rules]]"]);
        assert!(parse("---\nsuperseded_by: \"[[New]]\"\n---\n").unwrap().retired());
        assert!(parse("---\ngenerated: tools/gen.py --all @ abc1234\n---\n").unwrap().generated);
        assert!(parse("---\nstatus: generated\n---\n").unwrap().generated);
        assert!(!parse("---\ngenerated: false\n---\n").unwrap().generated);
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
        assert_eq!(section_of("Ways-of-Working/Rules.md"), Some(Section::WaysOfWorking));
        assert_eq!(section_of("docs/Platform/Engine.md"), Some(Section::Platform));
        assert_eq!(section_of("Research/Ingestion/Ingested/Standup.md"), Some(Section::Research));
        assert_eq!(section_of("Reference/config.rs"), None, "not a page");
        assert_eq!(section_of("README.md"), None);
    }

    #[test]
    fn bands_put_binding_first_and_evidence_last() {
        let current = PageMeta { status: Some("current".into()), ..Default::default() };
        let retired = PageMeta { status: Some("retired".into()), ..Default::default() };
        assert_eq!(band(Some(Section::WaysOfWorking), Some(&current)), 0);
        assert_eq!(band(Some(Section::Platform), None), 0);
        assert_eq!(band(Some(Section::Current), Some(&current)), 1);
        assert_eq!(band(None, None), 1, "a code file is ranked on its merits");
        assert_eq!(band(Some(Section::Research), None), 2);
        assert_eq!(band(Some(Section::WaysOfWorking), Some(&retired)), 2, "retired beats binding");
    }

    #[test]
    fn a_hit_page_dates_research_and_names_what_replaces_a_retired_page() {
        let research = hit_page("Research/Ingested/Standup - 2026-09-12.md", None).unwrap();
        assert_eq!((research.section, research.dated.as_deref(), research.band), (Some("Research"), Some("2026-09-12"), 2));
        let meta = PageMeta { status: Some("superseded".into()), replaced_by: vec!["[[Rules]]".into()], ..Default::default() };
        let old = hit_page("Ways-of-Working/Old.md", Some(meta)).unwrap();
        assert!(old.retired);
        assert_eq!(old.replaced_by, vec!["[[Rules]]"]);
        assert_eq!(old.dated, None, "only Research is stamped");
        assert_eq!(hit_page("src/main.rs", None), None);
    }

    #[test]
    fn a_reader_gets_business_pages_or_everything_else_or_all() {
        for any in [None, Some(""), Some("any")] {
            assert!(suits(any, Some("business")) && suits(any, Some("method")) && suits(any, None));
        }
        assert!(suits(Some("business"), Some("business")));
        assert!(!suits(Some("business"), Some("dev")) && !suits(Some("business"), None), "code is not for business readers");
        assert!(suits(Some("dev"), Some("dev")) && suits(Some("dev"), None), "developers get pages and code");
        assert!(!suits(Some("dev"), Some("business")) && !suits(Some("dev"), Some("method")));
    }

    #[test]
    fn the_section_decides_the_audience_unless_the_page_says() {
        assert_eq!(audience_of(section_of("Current/Project.md"), None), Some(Audience::Business));
        assert_eq!(audience_of(section_of("Work/Releases.md"), None), Some(Audience::Business));
        assert_eq!(audience_of(section_of("Conventions/Architecture.md"), None), Some(Audience::Dev));
        assert_eq!(audience_of(section_of("Ways-of-Working/Rules.md"), None), Some(Audience::Method));
        assert_eq!(audience_of(section_of("Research/spike.md"), None), None);
        let says = parse("---
audience: Business
---
").unwrap();
        assert_eq!(audience_of(section_of("Platform/Pricing.md"), Some(&says)), Some(Audience::Business));
        assert_eq!(hit_page("Design/Why.md", None).unwrap().audience, Some("business"));
    }

    #[test]
    fn a_date_in_the_file_name_dates_a_note() {
        assert_eq!(date_in_name("Research/Ingested/Standup - 2026-09-12.md").as_deref(), Some("2026-09-12"));
        assert_eq!(date_in_name("Research/2026-09-12-kickoff.md").as_deref(), Some("2026-09-12"));
        assert_eq!(date_in_name("Research/notes.md"), None);
    }
}

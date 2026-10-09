//! What a file is for: code, a test, a spec, a doc, config, data, a design,
//! a meeting note or a ticket. Every search hit carries one as a tag, and a
//! search can keep only some ("code only", "specs and docs").
//!
//! Worked out from the path alone, so it needs no index column and holds on
//! every existing index at once. Rules run most specific first: a
//! `tests/test_auth.py` is a test before it is code, an
//! `openspec/.../spec.md` a spec before it is a doc.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentType {
    Code,
    Test,
    Spec,
    Doc,
    Config,
    Data,
    Design,
    Meeting,
    Ticket,
}

pub const ALL: [ContentType; 9] = [
    ContentType::Code,
    ContentType::Test,
    ContentType::Spec,
    ContentType::Doc,
    ContentType::Config,
    ContentType::Data,
    ContentType::Design,
    ContentType::Meeting,
    ContentType::Ticket,
];

impl ContentType {
    pub fn as_str(self) -> &'static str {
        match self {
            ContentType::Code => "code",
            ContentType::Test => "test",
            ContentType::Spec => "spec",
            ContentType::Doc => "doc",
            ContentType::Config => "config",
            ContentType::Data => "data",
            ContentType::Design => "design",
            ContentType::Meeting => "meeting",
            ContentType::Ticket => "ticket",
        }
    }

    pub fn parse(s: &str) -> Option<ContentType> {
        ALL.into_iter().find(|t| t.as_str().eq_ignore_ascii_case(s.trim()))
    }
}

const PROGRAM_EXTS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts", "py", "go", "java", "c", "h", "cc", "cpp", "hpp",
    "cs", "rb", "php", "swift", "kt", "kts", "sh", "bash", "ps1", "svelte", "vue", "scala", "lua", "dart",
];
const CONFIG_EXTS: &[&str] = &["json", "yaml", "yml", "toml", "ini", "cfg", "conf", "properties", "lock", "env", "xml", "gradle"];
const CONFIG_NAMES: &[&str] = &[
    "dockerfile", "makefile", "justfile", "procfile", ".gitignore", ".gitattributes", ".editorconfig", ".npmrc",
    ".nvmrc", ".kenignore", "package.json", "tsconfig.json", "cargo.toml",
];
const DATA_EXTS: &[&str] = &["csv", "tsv", "xlsx", "xls", "ods", "parquet", "jsonl", "ndjson", "sql", "sqlite", "db"];
const DESIGN_EXTS: &[&str] = &["svg", "png", "jpg", "jpeg", "gif", "webp", "fig", "sketch", "psd", "ai", "drawio", "css", "scss"];
const SPEC_NAMES: &[&str] = &["spec.md", "proposal.md", "design.md", "requirements.md", "prd.md", "tasks.md"];
const SPEC_DIRS: &[&str] = &["openspec", "specs", "spec", "rfc", "rfcs", "adr", "adrs", "proposals"];
const TEST_DIRS: &[&str] = &["test", "tests", "__tests__", "e2e", "spec", "specs", "testing", "fixtures"];
const MEETING_WORDS: &[&str] = &["meeting", "meetings", "standup", "stand-up", "transcript", "transcripts", "minutes", "retro"];
const TICKET_DIRS: &[&str] = &["tickets", "issues", "backlog", "stories", "epics"];

/// The content type of a project-relative path.
pub fn of(path: &str) -> ContentType {
    let lower = path.replace('\\', "/").to_lowercase();
    let segments: Vec<&str> = lower.split('/').filter(|s| !s.is_empty()).collect();
    let name = segments.last().copied().unwrap_or("");
    let dirs = &segments[..segments.len().saturating_sub(1)];
    let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    let in_dir = |set: &[&str]| dirs.iter().any(|d| set.contains(d));
    let dir_mentions = |words: &[&str]| dirs.iter().any(|d| words.iter().any(|w| d.contains(w)));
    let program = PROGRAM_EXTS.contains(&ext);
    let prose = matches!(ext, "md" | "mdx" | "txt" | "pdf" | "docx" | "doc" | "rtf" | "html" | "htm" | "adoc" | "rst");

    // A test: test code by its folder or its name.
    let test_name = name.starts_with("test_")
        || [".test.", ".spec.", "_test.", "_spec.", ".tests."].iter().any(|m| name.contains(m));
    if program && (test_name || in_dir(TEST_DIRS)) {
        return ContentType::Test;
    }
    // A meeting: a transcript or a note about one.
    if matches!(ext, "vtt" | "srt") || (prose && (dir_mentions(MEETING_WORDS) || MEETING_WORDS.iter().any(|w| name.contains(w)))) {
        return ContentType::Meeting;
    }
    // A spec: specs, proposals, designs and requirements written down.
    if prose && (in_dir(SPEC_DIRS) || SPEC_NAMES.contains(&name) || name.contains("-spec.") || name.contains("-design.")) {
        return ContentType::Spec;
    }
    // A ticket: a work item in a tickets folder, or named like one (ABC-123).
    // `ROADMAP-0.1.0.md` is a version, not ticket 0: it was tagged a ticket
    // beside a real one, and a reader took both for tickets (2026-10-07).
    let head = name.split(['.', ' ', '_']).next().unwrap_or("");
    let version = name[head.len()..].strip_prefix('.').is_some_and(|r| r.starts_with(|c: char| c.is_ascii_digit()));
    let ticket_name = !version
        && head.split_once('-').is_some_and(|(key, num)| {
            !key.is_empty() && key.chars().all(|c| c.is_ascii_alphabetic()) && !num.is_empty() && num.chars().all(|c| c.is_ascii_digit())
        });
    if prose && (dir_mentions(TICKET_DIRS) || ticket_name) {
        return ContentType::Ticket;
    }
    if DATA_EXTS.contains(&ext) {
        return ContentType::Data;
    }
    if CONFIG_NAMES.contains(&name) || CONFIG_EXTS.contains(&ext) || name.starts_with(".env") {
        return ContentType::Config;
    }
    let design_dir = dirs.iter().any(|d| d.contains("design") || *d == "assets" || *d == "brand" || *d == "brands");
    if DESIGN_EXTS.contains(&ext) || (design_dir && matches!(ext, "html" | "htm")) {
        return ContentType::Design;
    }
    if program {
        return ContentType::Code;
    }
    ContentType::Doc
}

/// Whether `path` is one of `wanted` (none wanted keeps everything).
pub fn is_wanted(path: &str, wanted: &[ContentType]) -> bool {
    wanted.is_empty() || wanted.contains(&of(path))
}

/// Parse a filter the way callers pass it (`"code"`, `"spec,doc"`, `"any"`):
/// unknown names are ignored, and `any` or nothing means no filter.
pub fn parse_filter(raw: Option<&str>) -> Vec<ContentType> {
    raw.unwrap_or("")
        .split([',', ' '])
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("any"))
        .filter_map(ContentType::parse)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ContentType::*;

    #[test]
    fn paths_from_real_repos_get_the_type_a_person_would_give_them() {
        let cases = [
            ("BE_PACK/backend/app/core/auth.py", Code),
            ("BE_PACK/backend/tests/unit/test_auth.py", Test),
            ("FE_PACK/frontend/e2e/live-run-progress.spec.ts", Test),
            ("src/lib/citation.test.ts", Test),
            ("crates/ken-core/src/drift.rs", Code),
            ("BE_PACK/openspec/changes/db-model-reconformance/proposal.md", Spec),
            ("openspec/changes/mcp-server/specs/mcp-server/spec.md", Spec),
            ("docs/superpowers/specs/2026-09-01-shared-bundle-format-design.md", Spec),
            ("03 Meetings/2026-07-14 Kickoff.md", Meeting),
            ("research/2026-09-10-team-review.vtt", Meeting),
            ("05 Tickets/A1.1 — Migration branch + backend skeleton.md", Ticket),
            ("tickets/KEN-012.md", Ticket),
            ("Design/Game/ROADMAP-0.1.0.md", Doc),
            ("Design/SR-121.md", Ticket),
            ("BE_PACK/seed/data/ATT Op Model Dev V2.ods", Data),
            ("package.json", Config),
            ("BE_PACK/backend/alembic.ini", Config),
            ("Dockerfile", Config),
            ("mgo-designs/prototype_v4.html", Design),
            ("design/guidelines/colors-brand.card.html", Design),
            ("assets/logo.svg", Design),
            ("README.md", Doc),
            ("04 References/SOW.md", Doc),
            ("docs/manual/drift.html", Doc),
        ];
        for (path, want) in cases {
            assert_eq!(of(path), want, "{path}");
        }
    }

    #[test]
    fn a_filter_parses_leniently_and_any_means_all() {
        assert_eq!(parse_filter(Some("code")), vec![Code]);
        assert_eq!(parse_filter(Some("Spec, doc")), vec![Spec, Doc]);
        assert!(parse_filter(Some("any")).is_empty());
        assert!(parse_filter(Some("nonsense")).is_empty());
        assert!(parse_filter(None).is_empty());
        assert!(is_wanted("x.md", &[]));
        assert!(!is_wanted("x.md", &[Code]));
    }
}

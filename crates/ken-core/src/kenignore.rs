//! Index rules: which paths are indexed (knowledge-core decision 34).
//!
//! Two states: a path is indexed or it isn't. Pure module, no filesystem IO:
//! `parse` turns rule text into an ordered list of [`Rule`]s, and `classify`
//! folds rule sets (the fixed built-ins first, a person's rules last) against
//! a path to decide its [`Tier`]. `Project::index_rules` gathers every rule a
//! source reads: the built-ins, the repo switched off as a whole, the older
//! `.kenignore` files (read, never written), and the rules in the knowledge
//! base's `.ken/knowledge.json`, which is where a person writes them now.
//!
//! ## Syntax
//!
//! - A bare pattern (`build/`) is not indexed.
//! - A `!`-prefixed pattern (`!README.md`) is indexed, overriding an earlier
//!   broader rule via last-match-wins.
//! - A `~`-prefixed pattern, from the time a path could be searchable but not
//!   read for the graph, reads as indexed: it was searchable.
//! - `#` starts a comment; blank lines are skipped.
//! - `\~` / `\!` escape a literal leading `~`/`!` in the glob itself, and
//!   always classify as not indexed.
//! - After the prefix is stripped, the remainder is a gitignore-style glob
//!   (`dir/` is directory-only, a leading `/` anchors, `**` crosses folders),
//!   delegated to the `ignore` crate's matcher.
//! - Malformed lines (a bare `~`/`!`) are skipped, never a hard error.
//!
//! ## Precedence
//!
//! [`classify`] folds the given rule sets in order and defaults to
//! [`Tier::Indexed`]. Hard ignores come first and nothing brings them back:
//! VCS and dependency folders and secrets. `project.json`'s `excluded` list
//! is checked by the caller.

use std::path::Path;

/// Whether a path is indexed: the two states of decision 34. An ignored
/// path never produces a `files` or `chunks` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    /// Indexed: searchable by keyword and meaning, in the code index and the
    /// graph.
    Indexed,
    /// Not indexed at all.
    Ignored,
}

/// One parsed line from a `.kenignore` file (or a built-in rule set).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// The state this rule assigns when it's the last matching rule.
    pub tier: Tier,
    /// The gitignore-style glob, with the tier prefix (`~`/`!`) already
    /// stripped and any `\~`/`\!` escape already resolved to a literal
    /// leading `~`/`!`. This is handed to the `ignore` crate's matcher
    /// as-is (re-escaped internally in [`classify`] if it starts with a
    /// character gitignore itself treats specially).
    pub pattern: String,
}

/// Folders that are never indexed, whatever a rule says: no `!` line brings
/// them back. VCS and the biggest dependency folder. `project.json`'s
/// `excluded` list is checked by the caller.
///
/// `.ken` is not listed: the scan's walker keeps the real `.ken/` folder out
/// before any path reaches `classify`, and walks only its allowlisted
/// folders (`.ken/memory/`, `.ken/tasks/`) on purpose.
const HARD_IGNORE_DIRS: &[&str] = &[".git", ".hg", ".svn", "node_modules"];

/// Parse `.kenignore`-style text into an ordered list of rules. Pure: no
/// filesystem access, no validation beyond what's needed to skip a
/// malformed line tolerantly (D4). Order is preserved — callers fold
/// built-in rule sets before the user's own, per D2.
pub fn parse(text: &str) -> Vec<Rule> {
    let mut rules = Vec::new();
    for raw_line in text.lines() {
        let line = raw_line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let (tier, pattern) = if let Some(rest) = line.strip_prefix("\\~") {
            (Tier::Ignored, format!("~{rest}"))
        } else if let Some(rest) = line.strip_prefix("\\!") {
            (Tier::Ignored, format!("!{rest}"))
        } else if let Some(rest) = line.strip_prefix('~').or_else(|| line.strip_prefix('!')) {
            // `~` was search-only, which was indexed for search: with two
            // states it reads as indexed.
            (Tier::Indexed, rest.to_string())
        } else {
            (Tier::Ignored, line.to_string())
        };

        if pattern.is_empty() {
            // A bare "~" or "!" with nothing after it: malformed, skip
            // tolerantly rather than erroring.
            continue;
        }

        rules.push(Rule { tier, pattern });
    }
    rules
}

/// The lines of a workspace-level `.kenignore` that apply inside `member`
/// (its folder relative to the workspace), rewritten relative to it:
/// `Game/assets/raw/` becomes `/assets/raw/` for member `Game`, a pattern
/// with no folder in it (`*.log`, `tmp/`) applies in every member as it is,
/// and lines about other members, or about a member folder as a whole (which
/// decide whether it is a member at all), are left out.
pub fn lines_for_member(workspace_text: &str, member: &str) -> String {
    let member = member.trim_matches('/');
    let mut out = String::new();
    for raw in workspace_text.lines() {
        let line = raw.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (prefix, pattern) = [r"\~", r"\!", "~", "!"]
            .iter()
            .find_map(|p| line.strip_prefix(p).map(|rest| (*p, rest)))
            .unwrap_or(("", line));
        let body = pattern.trim_end_matches('/');
        let anchored = pattern.starts_with('/') || body.contains('/');
        if !anchored {
            // A folder line here (`Game-worktrees/`) is about the workspace's
            // own folders: setup writes them to say what is not a member.
            // A file pattern (`*.log`) holds everywhere.
            if !pattern.ends_with('/') {
                out.push_str(line);
                out.push('\n');
            }
            continue;
        }
        let rel = pattern.trim_start_matches('/');
        let rest = rel
            .get(..member.len())
            .filter(|head| head.eq_ignore_ascii_case(member))
            .and_then(|_| rel.get(member.len()..))
            .and_then(|tail| tail.strip_prefix('/'))
            .filter(|rest| !rest.is_empty());
        if let Some(rest) = rest {
            out.push_str(&format!("{prefix}/{rest}\n"));
        }
    }
    out
}

/// Returns the 1-based line numbers of lines that [`parse`] silently skips
/// as malformed (a bare `~` or `!` with no pattern after it), so callers
/// that want to surface a warning (e.g. src-tauri's `.kenignore` watcher)
/// don't have to duplicate `parse`'s line-classification logic. Comments,
/// blank lines, and well-formed rules (including `\~`/`\!` escapes) are
/// never reported.
pub fn malformed_lines(text: &str) -> Vec<usize> {
    text.lines()
        .enumerate()
        .filter_map(|(i, raw_line)| {
            let line = raw_line.trim_end();
            if line == "~" || line == "!" {
                Some(i + 1)
            } else {
                None
            }
        })
        .collect()
}

/// Classify `path` against one or more rule sets, folded in the given order
/// (built-ins first, a person's rules last) with gitignore's last-match-wins:
/// the last rule, across every set, that matches `path` decides. With no
/// rule matching, a path is indexed.
///
/// `path` is project-relative, forward-slash-separated, without a leading
/// `/`. `is_dir` tells the matcher whether `path` names a folder, which
/// matters for folder-only patterns (`dir/`).
pub fn classify(path: &str, is_dir: bool, rule_sets: &[&[Rule]]) -> Tier {
    let norm = path.trim_start_matches('/');

    if is_hard_ignored(norm) {
        return Tier::Ignored;
    }

    let mut result = Tier::Indexed;
    for rules in rule_sets {
        for rule in rules.iter() {
            if rule_matches(rule, norm, is_dir) {
                result = rule.tier;
            }
        }
    }
    result
}

/// The fixed built-ins every source reads first (knowledge-core decision
/// 34): dependencies, build output, tests, minified and generated files,
/// binaries and archives, and a knowledge base's page templates. VCS folders
/// and secrets are hard ignores ([`HARD_IGNORE_DIRS`], [`is_secret_name`]),
/// so no line brings them back; a `!` line in `.ken/knowledge.json` brings
/// back any of these. Media is not listed: Ken transcribes video.
///
/// The workspace's own memory folder has rules of its own
/// (`memory::workspace_builtin_rules`), folded only for it.
pub fn built_in_rule_sets() -> Vec<Rule> {
    DEPENDENCY_PATTERNS
        .iter()
        .chain(BUILD_PATTERNS)
        .chain(ARCHIVE_PATTERNS)
        .chain(BINARY_PATTERNS)
        .chain(GENERATED_PATTERNS)
        .chain(TEMPLATE_PATTERNS)
        .chain(TEST_PATTERNS)
        .map(|p| Rule { tier: Tier::Ignored, pattern: (*p).into() })
        .collect()
}

/// Tests are not indexed by default (knowledge-core decision 17): a test
/// restates the code it checks in a hundred near-copies, and outranked the
/// code it tests. Test folders, and test files named the way each language
/// names them (the ones `contenttype` calls a test). A team brings them back
/// with a `!` line in `.ken/knowledge.json` (`"!tests/"`, `"!*.test.ts"`),
/// which, folded last, wins. A file already indexed leaves the index on the
/// next scan, like any other newly ignored path.
pub const TEST_PATTERNS: &[&str] = &[
    "test/",
    "tests/",
    "__tests__/",
    "e2e/",
    "*.test.[jt]s",
    "*.test.[jt]sx",
    "*.test.mjs",
    "*.spec.[jt]s",
    "*.spec.[jt]sx",
    "*_test.go",
    "*_test.py",
    "test_*.py",
    "*_test.rs",
    "*_spec.rb",
    "*Test.java",
    "*Tests.java",
    "*Test.kt",
    "*Tests.cs",
    "*Test.cs",
];

/// Folders a package manager fills. `node_modules` is a hard ignore.
const DEPENDENCY_PATTERNS: &[&str] = &["vendor/", "bower_components/", "Pods/", "venv/", "__pycache__/", "site-packages/"];

/// Folders a build writes. The scan's walker skips the common ones before
/// they are read (`scan::JUNK_DIRS`); these say so for every caller.
const BUILD_PATTERNS: &[&str] = &["target/", "dist/", "build/", "out/", "obj/", ".next/", ".gradle/"];

/// Compiled files: nothing in them a person reads.
const BINARY_PATTERNS: &[&str] =
    &["*.exe", "*.dll", "*.so", "*.dylib", "*.o", "*.obj", "*.a", "*.lib", "*.class", "*.pyc", "*.wasm", "*.pdb"];

/// Files a tool writes and no one reads for meaning: minified bundles,
/// source maps, lockfiles. They match many searches a little and answer none.
const GENERATED_PATTERNS: &[&str] = &[
    "*.min.js", "*.min.css", "*.map", "package-lock.json", "yarn.lock", "pnpm-lock.yaml", "Cargo.lock", "poetry.lock",
    "uv.lock", "Gemfile.lock", "composer.lock",
];

/// A knowledge base's page templates: copied to write a page, never an
/// answer. Each reads like every question (`{{the rule as a sentence}}`) and
/// answers none.
const TEMPLATE_PATTERNS: &[&str] = &["Templates/"];

/// An archive or disk image answers no question by its name and Ken cannot
/// read inside it.
const ARCHIVE_PATTERNS: &[&str] = &["*.zip", "*.7z", "*.rar", "*.tar", "*.gz", "*.tgz", "*.iso", "*.jar"];

/// File names that hold secrets, never indexed: search results and chunks
/// would carry their contents. Dotfiles are already hidden by the walker;
/// this also covers the plain-named ones. `*.key` includes Keynote decks,
/// which Ken cannot read anyway.
pub fn is_secret_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n == ".env"
        || n.starts_with(".env.")
        || n.ends_with(".pem")
        || n.ends_with(".key")
        || n.ends_with(".p12")
        || n.ends_with(".pfx")
        || n.starts_with("id_rsa")
        || n.starts_with("id_ed25519")
        || n.starts_with("id_ecdsa")
        || n.starts_with("secrets.")
        || n == "credentials.json"
}

/// The repo switched off as a whole: a person's choice in the registry,
/// never written into the repo. Folded after the built-ins.
pub fn repo_rules_for(root: &Path) -> Vec<Rule> {
    if crate::registry::index_of(root).1 == Some(crate::registry::IndexState::Off) {
        vec![Rule { tier: Tier::Ignored, pattern: "*".into() }]
    } else {
        Vec::new()
    }
}

/// Rule lines as `.ken/knowledge.json` holds them (`"build/"`,
/// `"!tests/"`), parsed like any rule text.
pub fn parse_lines(lines: &[String]) -> Vec<Rule> {
    parse(&lines.join("\n"))
}

fn is_hard_ignored(path: &str) -> bool {
    let p = Path::new(path);
    p.components()
        .any(|c| matches!(c.as_os_str().to_str(), Some(name) if HARD_IGNORE_DIRS.contains(&name)))
        || p.file_name().and_then(|n| n.to_str()).is_some_and(is_secret_name)
}

/// Whether `rule`'s glob matches `path` (or one of its parent directories
/// — so a directory-only rule like `build/` also covers everything under
/// `build/`, not just the `build` entry itself).
///
/// One single-line `Gitignore` matcher is built per call. This keeps the
/// per-rule tier lookup trivial (no need to map the crate's own matched
/// `Glob` back to a rule index) at the cost of rebuilding a tiny glob set
/// per rule per path — acceptable for `.kenignore`'s expected rule counts
/// (dozens, not thousands) and classify's call frequency (once per file
/// per index rebuild, not a hot per-keystroke path). Not memoized here;
/// a caller classifying many paths against the same rule sets may want to
/// cache compiled matchers, but that's an optimization for later, not a
/// correctness requirement of this module.
fn rule_matches(rule: &Rule, path: &str, is_dir: bool) -> bool {
    // kenignore's own tier prefixes (~/!) fully own the negation/tier
    // semantics; gitignore must never additionally reinterpret a leading
    // `!` (its own whitelist syntax) or `#` (its own comment syntax) in
    // the already-stripped pattern. Re-escape defensively so `add_line`
    // treats the pattern as a plain, literal-prefixed glob.
    let pattern = if rule.pattern.starts_with('!') || rule.pattern.starts_with('#') {
        format!("\\{}", rule.pattern)
    } else {
        rule.pattern.clone()
    };

    // One compiled matcher per distinct pattern, kept per thread: a scan
    // classifies every path against every rule, and building a matcher per
    // rule per path made a rescan of an unchanged repo cost seconds at tens
    // of thousands of files. Bounded so a stream of new patterns cannot grow
    // it without end.
    thread_local! {
        static MATCHERS: std::cell::RefCell<std::collections::HashMap<String, Option<ignore::gitignore::Gitignore>>> =
            std::cell::RefCell::new(std::collections::HashMap::new());
    }
    MATCHERS.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.len() > 4096 {
            cache.clear();
        }
        let matcher = cache.entry(pattern.clone()).or_insert_with(|| {
            let mut builder = ignore::gitignore::GitignoreBuilder::new(".");
            builder.add_line(None, &pattern).ok()?;
            builder.build().ok()
        });
        matcher
            .as_ref()
            .is_some_and(|m| m.matched_path_or_any_parents(path, is_dir).is_ignore())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- parse ----

    #[test]
    fn a_workspace_kenignore_reaches_into_the_member_it_names() {
        let ws = "# setup\nGame-worktrees/\nGame/assets/raw/\n~/Game/docs/old/\n!Game/assets/raw/keep.png\nDocs/drafts/\n*.log\n~tmp/\nGame/\n";
        assert_eq!(
            lines_for_member(ws, "Game"),
            "/assets/raw/\n~/docs/old/\n!/assets/raw/keep.png\n*.log\n",
            "its own lines rewritten and file patterns kept; other members' lines and the workspace's folder lines left out"
        );
        assert_eq!(lines_for_member(ws, "Docs"), "/drafts/\n*.log\n");
        assert_eq!(lines_for_member("SR/tools/build/\n", "SR/tools"), "/build/\n", "a member in a group folder");
    }

    #[test]
    fn parse_bare_pattern_is_ignore_tier() {
        let rules = parse("build/\n");
        assert_eq!(rules, vec![Rule { tier: Tier::Ignored, pattern: "build/".into() }]);
    }

    #[test]
    fn an_older_tilde_line_reads_as_indexed() {
        let rules = parse("~.ken/tasks/\n");
        assert_eq!(rules, vec![Rule { tier: Tier::Indexed, pattern: ".ken/tasks/".into() }]);
    }

    #[test]
    fn parse_bang_prefix_is_indexed() {
        let rules = parse("!README.md\n");
        assert_eq!(rules, vec![Rule { tier: Tier::Indexed, pattern: "README.md".into() }]);
    }

    #[test]
    fn parse_skips_comments_and_blank_lines() {
        let rules = parse("# a comment\n\nbuild/\n   \n# another\n");
        assert_eq!(rules, vec![Rule { tier: Tier::Ignored, pattern: "build/".into() }]);
    }

    #[test]
    fn parse_escaped_tilde_is_literal_and_ignore_tier() {
        let rules = parse("\\~weird-dir/\n");
        assert_eq!(
            rules,
            vec![Rule { tier: Tier::Ignored, pattern: "~weird-dir/".into() }]
        );
    }

    #[test]
    fn parse_escaped_bang_is_literal_and_ignore_tier() {
        let rules = parse("\\!important.txt\n");
        assert_eq!(
            rules,
            vec![Rule { tier: Tier::Ignored, pattern: "!important.txt".into() }]
        );
    }

    #[test]
    fn parse_bare_prefix_with_no_pattern_is_malformed_and_skipped() {
        let rules = parse("~\n!\nbuild/\n");
        assert_eq!(rules, vec![Rule { tier: Tier::Ignored, pattern: "build/".into() }]);
    }

    #[test]
    fn parse_preserves_declared_order() {
        let rules = parse("a\n~b\n!c\n");
        assert_eq!(
            rules,
            vec![
                Rule { tier: Tier::Ignored, pattern: "a".into() },
                Rule { tier: Tier::Indexed, pattern: "b".into() },
                Rule { tier: Tier::Indexed, pattern: "c".into() },
            ]
        );
    }

    // ---- classify ----

    #[test]
    fn classify_defaults_to_indexed_with_no_rules() {
        assert_eq!(classify("src/main.rs", false, &[]), Tier::Indexed);
    }

    #[test]
    fn no_rules_index_everything() {
        // The stated migration (design.md): no `.kenignore` at all means
        // every non-hard-ignored path is indexed.
        let rules = parse(""); // empty file
        assert_eq!(classify("anything/at/all.rs", false, &[&rules]), Tier::Indexed);
    }

    #[test]
    fn classify_bare_pattern_ignores_matching_path() {
        let rules = parse("build/\n");
        assert_eq!(classify("build", true, &[&rules]), Tier::Ignored);
        assert_eq!(classify("build/output.txt", false, &[&rules]), Tier::Ignored);
    }

    #[test]
    fn a_tilde_line_after_a_bare_one_brings_the_path_back() {
        let rules = parse("docs/\n~docs/drafts/\n");
        assert_eq!(classify("docs/drafts/note.md", false, &[&rules]), Tier::Indexed);
        assert_eq!(classify("docs/other.md", false, &[&rules]), Tier::Ignored);
    }

    #[test]
    fn classify_non_matching_path_is_indexed() {
        let rules = parse("build/\n");
        assert_eq!(classify("src/lib.rs", false, &[&rules]), Tier::Indexed);
    }

    #[test]
    fn classify_last_match_wins_within_one_rule_set() {
        // A later broad ignore, then a narrower carve-out.
        let rules = parse("docs/\n!docs/README.md\n");
        assert_eq!(classify("docs/README.md", false, &[&rules]), Tier::Indexed);
        assert_eq!(classify("docs/other.md", false, &[&rules]), Tier::Ignored);
    }

    #[test]
    fn classify_user_rules_override_builtin_rules_last_match_wins() {
        let builtins = parse(".ken/tasks/\n");
        let user = parse("!.ken/tasks/important.md\n");
        assert_eq!(classify(".ken/tasks/important.md", false, &[&builtins, &user]), Tier::Indexed);
        assert_eq!(classify(".ken/tasks/other.md", false, &[&builtins, &user]), Tier::Ignored);
    }

    #[test]
    fn classify_hard_ignore_short_circuits_and_bang_cannot_resurrect() {
        let user = parse("!node_modules/keep-me.js\n");
        assert_eq!(
            classify("node_modules/keep-me.js", false, &[&user]),
            Tier::Ignored
        );
        assert_eq!(classify(".git/config", false, &[&user]), Tier::Ignored);
    }

    #[test]
    fn classify_root_anchored_pattern_only_matches_at_root() {
        let rules = parse("/only-root.txt\n");
        assert_eq!(classify("only-root.txt", false, &[&rules]), Tier::Ignored);
        assert_eq!(
            classify("nested/only-root.txt", false, &[&rules]),
            Tier::Indexed
        );
    }

    #[test]
    fn classify_double_star_glob_matches_across_segments() {
        let rules = parse("**/*.generated.ts\n");
        assert_eq!(classify("a/b/c/x.generated.ts", false, &[&rules]), Tier::Ignored);
    }

    #[test]
    fn the_fixed_built_ins_cover_dependencies_build_output_and_binaries() {
        let built = built_in_rule_sets();
        for path in ["vendor/lib/x.go", "app/build/out.txt", "target/release/ken.exe", "bin/tool.dll", "Main.class", ".hg/store", "a/.svn/x"] {
            assert_eq!(classify(path, false, &[&built]), Tier::Ignored, "{path}");
        }
        for path in ["src/build.rs", "docs/building.md", "scripts/run.sh"] {
            assert_eq!(classify(path, false, &[&built]), Tier::Indexed, "{path}");
        }
        // Rules as `.ken/knowledge.json` holds them, folded last.
        let user = parse_lines(&["assets/raw/".to_string(), "!vendor/ours/".to_string()]);
        assert_eq!(classify("assets/raw/a.png", false, &[&built, &user]), Tier::Ignored);
        assert_eq!(classify("vendor/ours/x.go", false, &[&built, &user]), Tier::Indexed, "a ! line brings a built-in back");
    }

    #[test]
    fn classify_escaped_literal_bang_filename_matches_as_ignore() {
        let rules = parse("\\!important.txt\n");
        assert_eq!(classify("!important.txt", false, &[&rules]), Tier::Ignored);
    }

    #[test]
    fn classify_multiple_rule_sets_fold_in_given_order() {
        let a = parse("x\n");
        let b = parse("!x\n");
        // b comes after a: last-match-wins across sets, not just within.
        assert_eq!(classify("x", false, &[&a, &b]), Tier::Indexed);
        assert_eq!(classify("x", false, &[&b, &a]), Tier::Ignored);
    }

    #[test]
    fn built_ins_skip_archives_and_a_user_line_brings_them_back() {
        let built = built_in_rule_sets();
        for path in ["backups/world.zip", "a.7z", "dist/app.jar", "disk.iso", "x.tar.gz"] {
            assert_eq!(classify(path, false, &[&built]), Tier::Ignored, "{path}");
        }
        assert_eq!(classify("notes.md", false, &[&built]), Tier::Indexed);
        assert_eq!(classify("clip.mp4", false, &[&built]), Tier::Indexed, "media stays: Ken transcribes it");
        let user = parse("!*.zip\n");
        assert_eq!(classify("backups/world.zip", false, &[&built, &user]), Tier::Indexed);
        // What a tool writes: bundles, maps, lockfiles.
        for path in ["dist/app.min.js", "web/site.min.css", "dist/app.js.map", "package-lock.json", "api/uv.lock", "Cargo.lock"] {
            assert_eq!(classify(path, false, &[&built]), Tier::Ignored, "{path}");
        }
        assert_eq!(classify("src/app.js", false, &[&built]), Tier::Indexed);
        assert_eq!(classify("package.json", false, &[&built]), Tier::Indexed);
        // The knowledge base's page templates.
        assert_eq!(classify("Templates/Rule.md", false, &[&built]), Tier::Ignored);
        assert_eq!(classify("Templates", true, &[&built]), Tier::Ignored);
        assert_eq!(classify("Rules/Templates and forms.md", false, &[&built]), Tier::Indexed);
    }

    #[test]
    fn tests_are_not_indexed_and_a_user_line_brings_them_back() {
        let built = built_in_rule_sets();
        for path in [
            "crates/ken-core/tests/qa_probe.rs",
            "src/lib/citation.test.ts",
            "src/__tests__/App.tsx",
            "FE/e2e/live-run.spec.ts",
            "BE/backend/tests/unit/test_auth.py",
            "pkg/store/store_test.go",
            "app/models/user_spec.rb",
            "src/test/java/WorldTest.java",
            "Game.Tests/SaveTests.cs",
        ] {
            assert_eq!(classify(path, false, &[&built]), Tier::Ignored, "{path}");
        }
        for path in ["src/lib/citation.ts", "src/contest.rs", "docs/testing-strategy.md", "openspec/specs/mcp/spec.md", "src/Latest.java"] {
            assert_eq!(classify(path, false, &[&built]), Tier::Indexed, "{path}");
        }
        let user = parse("!tests/\n!*.test.ts\n");
        assert_eq!(classify("crates/ken-core/tests/qa_probe.rs", false, &[&built, &user]), Tier::Indexed);
        assert_eq!(classify("src/lib/citation.test.ts", false, &[&built, &user]), Tier::Indexed);
        assert_eq!(classify("pkg/store/store_test.go", false, &[&built, &user]), Tier::Ignored, "only what the line names");
    }

    #[test]
    fn secrets_are_hard_ignored_and_no_line_brings_them_back() {
        let user = parse("!*\n!.env\n!config/credentials.json\n");
        for path in [
            ".env",
            "app/.env.production",
            "certs/server.pem",
            "tls.key",
            "home/id_rsa",
            "home/id_ed25519.pub",
            "config/credentials.json",
            "deploy/secrets.yaml",
            "Secrets.JSON",
        ] {
            assert_eq!(classify(path, false, &[&user]), Tier::Ignored, "{path}");
        }
        for path in ["docs/secrets-policy.md", "keys.md", "src/credentials.rs", "monkey.txt"] {
            assert_eq!(classify(path, false, &[&user]), Tier::Indexed, "{path}");
        }
    }
}

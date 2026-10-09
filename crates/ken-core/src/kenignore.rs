//! `.kenignore` parsing and tier classification (kenignore tasks 1.1-1.3).
//!
//! Pure module, no filesystem IO (per design D6): `parse` turns `.kenignore`
//! file text into an ordered list of [`Rule`]s, and `classify` folds one or
//! more rule sets (built-ins first, user file last, per D2) against a path
//! to decide its [`Tier`]. Rule sets are plain data — callers (scan.rs's
//! walk, engine.rs's index rebuild) own reading `.kenignore` off disk and
//! composing the built-in/user rule sets; this module never touches a file
//! handle.
//!
//! ## Syntax (D1)
//!
//! - A bare pattern (`build/`) ⇒ [`Tier::Ignore`] (today's default: not
//!   indexed at all).
//! - A `~`-prefixed pattern (`~.ken/tasks/`) ⇒ [`Tier::SearchOnly`].
//! - A `!`-prefixed pattern (`!README.md`) ⇒ [`Tier::Full`] (i.e. this is
//!   *kenignore's own* negation, not gitignore's — it always means "index
//!   this fully", overriding an earlier broader rule via last-match-wins).
//! - `#` starts a comment; blank lines are skipped.
//! - `\~` / `\!` escape a literal leading `~`/`!` in the glob itself (so a
//!   file *named* `~foo` or `!foo` can still be matched), and always
//!   classify as [`Tier::Ignore`] (no tier prefix was consumed).
//! - After the tier prefix is stripped, the remainder is a gitignore-style
//!   glob: `dir/` is directory-only, a leading `/` anchors to the rule
//!   set's root, `**` matches across path segments, etc. — delegated
//!   entirely to the `ignore` crate's `GitignoreBuilder`/`Gitignore`
//!   (D6's "candidate base").
//! - Malformed lines (a bare `~`/`!` with no pattern, or a glob the
//!   underlying matcher rejects) are skipped tolerantly, like every other
//!   parser in this plan (D4) — never a hard error.
//! - Last-match-wins across the whole effective rule list (built-ins then
//!   user file, in file order): a later rule overrides an earlier one for
//!   any path both match, including a user `!pattern` overriding a
//!   built-in `~pattern` or bare pattern.
//!
//! ## Precedence (D2)
//!
//! [`classify`] only implements steps 2-4: fold the given rule sets in
//! order, default to [`Tier::Full`] if nothing matches. Step 1 (hard
//! ignores: `project.json`'s `excluded` list, plus a small built-in skip
//! list of directories that are never indexed at any tier) is split
//! between this module's own always-on directory check and the caller —
//! `project.json` exclusion is a `Project`-level concern this pure module
//! has no access to, so callers must additionally check that themselves
//! (e.g. `project.is_excluded(path) || classify(..) == Tier::Ignore`)
//! before treating a path as indexable. `!` can never resurrect a
//! hard-ignored path; that's why the hard-ignore check short-circuits
//! before any rule folding happens.

use std::path::Path;

/// How deeply a path is indexed. Numeric values match the `chunks.tier`
/// column (`0` = full, `1` = search-only) added in the same schema bump as
/// semantic-index; `Ignore` never produces a `chunks` (or `files`) row at
/// all, so it has no on-disk representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    /// Indexed for both keyword/semantic search AND fed to consumers like
    /// knowledge-model extraction and profiler doc sampling.
    Full = 0,
    /// Indexed for keyword/semantic search only; excluded from knowledge
    /// model extraction and profiler doc sampling (kenignore task 1.5).
    SearchOnly = 1,
    /// Not indexed at all — identical to today's exclusion behavior.
    Ignore = 2,
}

/// One parsed line from a `.kenignore` file (or a built-in rule set).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// The tier this rule assigns when it's the last matching rule.
    pub tier: Tier,
    /// The gitignore-style glob, with the tier prefix (`~`/`!`) already
    /// stripped and any `\~`/`\!` escape already resolved to a literal
    /// leading `~`/`!`. This is handed to the `ignore` crate's matcher
    /// as-is (re-escaped internally in [`classify`] if it starts with a
    /// character gitignore itself treats specially).
    pub pattern: String,
}

/// Directories that are never indexed at any tier, regardless of any
/// `.kenignore` rule — `!` cannot resurrect these (D2, step 1). This is
/// deliberately a small, fixed list; `project.json`'s `excluded` list is
/// the other half of "hard ignores" and is checked by the caller, not
/// here (this module has no `Project` access — see the module doc).
///
/// Deliberately does NOT include `.ken` as a blanket path component. D2's
/// hard-ignore text is "`.git/`, `node_modules/`, the project's own `.ken/`
/// DB internals, etc." — that `.ken/` clause describes scan.rs's existing
/// walker filter (`WalkBuilder::filter_entry`, `name != ".ken"`), which
/// already keeps the real on-disk `.ken/` directory out of every scan
/// before any path ever reaches `classify`. It is not this module's job to
/// re-block it. Blocking the whole `.ken` component here would also make
/// D2 step 2's own worked example unreachable: `~.ken/tasks/` (ken-tasks
/// D6) is a built-in tier rule specifically meant to route
/// non-DB-internal `.ken/tasks/...` paths (e.g. synthetic chunk paths a
/// future ken-tasks feature indexes directly, bypassing the file walk) to
/// `SearchOnly`/`Full`, overridable by the user file. A user `!` line
/// could never resurrect that if this list swallowed all of `.ken` first.
const HARD_IGNORE_DIRS: &[&str] = &[".git", "node_modules"];

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
            (Tier::Ignore, format!("~{rest}"))
        } else if let Some(rest) = line.strip_prefix("\\!") {
            (Tier::Ignore, format!("!{rest}"))
        } else if let Some(rest) = line.strip_prefix('~') {
            (Tier::SearchOnly, rest.to_string())
        } else if let Some(rest) = line.strip_prefix('!') {
            (Tier::Full, rest.to_string())
        } else {
            (Tier::Ignore, line.to_string())
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

/// Classify `path` against one or more rule sets, folded in the given
/// order (built-ins first, user `.kenignore` last, per D2) using
/// gitignore's last-match-wins semantics: the last rule (across *all*
/// rule sets, in order) that matches `path` decides the tier. If nothing
/// matches, the default is [`Tier::Full`] (D2 step 4) — with no
/// `.kenignore` at all, every non-hard-ignored path is `Full`, identical
/// to today's behavior (the design's stated migration: none).
///
/// `path` should be project-relative, forward-slash-separated, without a
/// leading `/` (matching `rel_path` conventions used elsewhere in this
/// crate). `is_dir` tells the matcher whether `path` names a directory,
/// which matters for directory-only patterns (`dir/`).
pub fn classify(path: &str, is_dir: bool, rule_sets: &[&[Rule]]) -> Tier {
    let norm = path.trim_start_matches('/');

    if is_hard_ignored(norm) {
        return Tier::Ignore;
    }

    let mut result = Tier::Full;
    for rules in rule_sets {
        for rule in rules.iter() {
            if rule_matches(rule, norm, is_dir) {
                result = rule.tier;
            }
        }
    }
    result
}

/// Built-in rule sets that ship with Ken itself, ahead of any user
/// `.kenignore` (D2 step 2 — built-ins first, user rules appended after so a
/// `!` line can override them). Per design.md D2/1.3, this is meant to hold
/// per-member pseudo-tier rules from ken-memory and the `~.ken/tasks/`
/// search-only rule from ken-tasks. It stays an empty rule set, on purpose:
/// every built-in rule those features actually needed turned out to be
/// **scoped to one member**, and this function is parameterless and folded
/// into *every* project's classify call (`scan::scan`,
/// `scan::refresh_path`), so putting them here would apply one member's
/// semantics to all of them.
///
/// The per-member rule sets live next to the feature that owns them, and
/// whoever ingests that member folds them into that classify call's
/// `rule_sets` — the same seam `Project::kenignore_rules()` uses for the
/// user tier:
///
/// - `memory::workspace_builtin_rules()` — the workspace pseudo-member
///   (ken-memory 1.6).
/// - `family::family_builtin_rules()` — a family clone (ken-families 1.6).
///
/// What does belong here is a rule global to every project: archives. An
/// archive or disk image answers no question by its name and Ken cannot
/// read inside it, so it is not indexed; a user `!*.zip` line brings the
/// names back. Media is not listed: Ken transcribes video. Secrets are not
/// a rule at all but a hard ignore ([`is_secret_name`]), so no `!` line can
/// bring them back.
pub fn built_in_rule_sets() -> Vec<Rule> {
    ARCHIVE_PATTERNS
        .iter()
        .chain(GENERATED_PATTERNS)
        .map(|p| Rule { tier: Tier::Ignore, pattern: (*p).into() })
        .collect()
}

/// Files a tool writes and no one reads for meaning: minified bundles,
/// source maps, lockfiles. They match many searches a little and answer none.
/// A `!` line brings one back.
const GENERATED_PATTERNS: &[&str] = &[
    "*.min.js", "*.min.css", "*.map", "package-lock.json", "yarn.lock", "pnpm-lock.yaml", "Cargo.lock", "poetry.lock",
    "uv.lock", "Gemfile.lock", "composer.lock",
];

const ARCHIVE_PATTERNS: &[&str] = &["*.zip", "*.7z", "*.rar", "*.tar", "*.gz", "*.tgz", "*.iso", "*.jar"];

/// File names that hold secrets, never indexed at any tier: search results,
/// chunks and entity prompts would all carry their contents. Dotfiles are
/// already hidden by the walker; this also covers the plain-named ones.
/// `*.key` includes Keynote decks, which Ken cannot read anyway.
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

/// The rule a repo's kind contributes: its index state, set once for the
/// whole repo. A code or reference repo is searchable only; a repo that is
/// also team or wiki, or has no kind yet, adds nothing, so it reads as today.
/// Callers fold this after the built-ins and before the user's
/// `.kenignore`, so a `!docs/` line there still reads a code repo's docs
/// for entities. Held in the registry, not written into the repo.
pub fn kind_rules(kind: &[crate::registry::RepoKind]) -> Vec<Rule> {
    if kind.is_empty() || kind.iter().any(|k| k.reads_entities()) {
        return Vec::new();
    }
    vec![Rule { tier: Tier::SearchOnly, pattern: "*".into() }]
}

/// The repo's rule from the default registry: a person's index choice when
/// they made one, else what its kind implies ([`kind_rules`]).
pub fn kind_rules_for(root: &Path) -> Vec<Rule> {
    use crate::registry::IndexState;
    let (kind, index) = crate::registry::index_of(root);
    match index {
        Some(IndexState::Search) => vec![Rule { tier: Tier::SearchOnly, pattern: "*".into() }],
        Some(IndexState::Entities) => Vec::new(),
        Some(IndexState::Off) => vec![Rule { tier: Tier::Ignore, pattern: "*".into() }],
        None => kind_rules(&kind),
    }
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
        assert_eq!(rules, vec![Rule { tier: Tier::Ignore, pattern: "build/".into() }]);
    }

    #[test]
    fn parse_tilde_prefix_is_search_only_tier() {
        let rules = parse("~.ken/tasks/\n");
        assert_eq!(
            rules,
            vec![Rule { tier: Tier::SearchOnly, pattern: ".ken/tasks/".into() }]
        );
    }

    #[test]
    fn parse_bang_prefix_is_full_tier() {
        let rules = parse("!README.md\n");
        assert_eq!(rules, vec![Rule { tier: Tier::Full, pattern: "README.md".into() }]);
    }

    #[test]
    fn parse_skips_comments_and_blank_lines() {
        let rules = parse("# a comment\n\nbuild/\n   \n# another\n");
        assert_eq!(rules, vec![Rule { tier: Tier::Ignore, pattern: "build/".into() }]);
    }

    #[test]
    fn parse_escaped_tilde_is_literal_and_ignore_tier() {
        let rules = parse("\\~weird-dir/\n");
        assert_eq!(
            rules,
            vec![Rule { tier: Tier::Ignore, pattern: "~weird-dir/".into() }]
        );
    }

    #[test]
    fn parse_escaped_bang_is_literal_and_ignore_tier() {
        let rules = parse("\\!important.txt\n");
        assert_eq!(
            rules,
            vec![Rule { tier: Tier::Ignore, pattern: "!important.txt".into() }]
        );
    }

    #[test]
    fn parse_bare_prefix_with_no_pattern_is_malformed_and_skipped() {
        let rules = parse("~\n!\nbuild/\n");
        assert_eq!(rules, vec![Rule { tier: Tier::Ignore, pattern: "build/".into() }]);
    }

    #[test]
    fn parse_preserves_declared_order() {
        let rules = parse("a\n~b\n!c\n");
        assert_eq!(
            rules,
            vec![
                Rule { tier: Tier::Ignore, pattern: "a".into() },
                Rule { tier: Tier::SearchOnly, pattern: "b".into() },
                Rule { tier: Tier::Full, pattern: "c".into() },
            ]
        );
    }

    // ---- classify ----

    #[test]
    fn classify_defaults_to_full_with_no_rules() {
        assert_eq!(classify("src/main.rs", false, &[]), Tier::Full);
    }

    #[test]
    fn classify_no_kenignore_file_is_byte_identical_to_full_everywhere() {
        // The stated migration (design.md): no `.kenignore` at all means
        // every non-hard-ignored path classifies as Full.
        let rules = parse(""); // empty file
        assert_eq!(classify("anything/at/all.rs", false, &[&rules]), Tier::Full);
    }

    #[test]
    fn classify_bare_pattern_ignores_matching_path() {
        let rules = parse("build/\n");
        assert_eq!(classify("build", true, &[&rules]), Tier::Ignore);
        assert_eq!(classify("build/output.txt", false, &[&rules]), Tier::Ignore);
    }

    #[test]
    fn classify_tilde_pattern_is_search_only() {
        let rules = parse("~docs/drafts/\n");
        assert_eq!(
            classify("docs/drafts/note.md", false, &[&rules]),
            Tier::SearchOnly
        );
    }

    #[test]
    fn classify_non_matching_path_defaults_full() {
        let rules = parse("build/\n");
        assert_eq!(classify("src/lib.rs", false, &[&rules]), Tier::Full);
    }

    #[test]
    fn classify_last_match_wins_within_one_rule_set() {
        // A later broad ignore, then a narrower full-tier carve-out.
        let rules = parse("docs/\n!docs/README.md\n");
        assert_eq!(classify("docs/README.md", false, &[&rules]), Tier::Full);
        assert_eq!(classify("docs/other.md", false, &[&rules]), Tier::Ignore);
    }

    #[test]
    fn classify_user_rules_override_builtin_rules_last_match_wins() {
        // Built-in prelude marks .ken/tasks/ SearchOnly; user file
        // overrides a specific file back to Full.
        let builtins = parse("~.ken/tasks/\n");
        let user = parse("!.ken/tasks/important.md\n");
        assert_eq!(
            classify(".ken/tasks/important.md", false, &[&builtins, &user]),
            Tier::Full
        );
        assert_eq!(
            classify(".ken/tasks/other.md", false, &[&builtins, &user]),
            Tier::SearchOnly
        );
    }

    #[test]
    fn classify_hard_ignore_short_circuits_and_bang_cannot_resurrect() {
        let user = parse("!node_modules/keep-me.js\n");
        assert_eq!(
            classify("node_modules/keep-me.js", false, &[&user]),
            Tier::Ignore
        );
        assert_eq!(classify(".git/config", false, &[&user]), Tier::Ignore);
    }

    #[test]
    fn classify_does_not_blanket_hard_ignore_dot_ken() {
        // `.ken/` DB internals (the real on-disk SQLite file etc.) never
        // reach `classify` at all in practice: scan.rs's walker filters the
        // whole `.ken` directory out unconditionally before rel paths are
        // ever computed (see `HARD_IGNORE_DIRS`'s doc comment). This module
        // must NOT re-implement that as a blanket `.ken` path-component
        // hard-ignore, because D2 step 2 names `~.ken/tasks/` as a built-in
        // tier rule that has to stay reachable and user-overridable — a
        // blanket `.ken` hard-ignore would swallow it and make `!` unable
        // to resurrect it, contradicting D2's own worked example.
        let builtins = parse("~.ken/tasks/\n");
        assert_eq!(
            classify(".ken/tasks/notes.md", false, &[&builtins]),
            Tier::SearchOnly
        );
    }

    #[test]
    fn classify_root_anchored_pattern_only_matches_at_root() {
        let rules = parse("/only-root.txt\n");
        assert_eq!(classify("only-root.txt", false, &[&rules]), Tier::Ignore);
        assert_eq!(
            classify("nested/only-root.txt", false, &[&rules]),
            Tier::Full
        );
    }

    #[test]
    fn classify_double_star_glob_matches_across_segments() {
        let rules = parse("~**/*.generated.ts\n");
        assert_eq!(
            classify("a/b/c/x.generated.ts", false, &[&rules]),
            Tier::SearchOnly
        );
    }

    #[test]
    fn kind_sets_the_repo_index_state_and_the_users_file_still_wins() {
        use crate::registry::RepoKind;
        let code = kind_rules(&[RepoKind::Code]);
        assert_eq!(classify("src/main.rs", false, &[&code]), Tier::SearchOnly);
        assert_eq!(classify("README.md", false, &[&code]), Tier::SearchOnly);
        assert_eq!(classify("a/b/c/deep.md", false, &[&code]), Tier::SearchOnly);
        assert_eq!(
            classify("x.md", false, &[&kind_rules(&[RepoKind::Reference])]),
            Tier::SearchOnly
        );

        // Team, wiki, a wiki that holds code, and not-yet-said read in full.
        for kind in [vec![RepoKind::Team], vec![RepoKind::Wiki], vec![RepoKind::Wiki, RepoKind::Code], vec![]] {
            assert!(kind_rules(&kind).is_empty(), "{kind:?}");
        }

        // A user line after the kind rule reads a code repo's docs in full,
        // and a bare line still takes a path out.
        let user = parse("!docs/\nsecret/\n");
        assert_eq!(classify("docs/arch.md", false, &[&code, &user]), Tier::Full);
        assert_eq!(classify("secret/x.md", false, &[&code, &user]), Tier::Ignore);
        assert_eq!(classify("src/lib.rs", false, &[&code, &user]), Tier::SearchOnly);
    }

    #[test]
    fn classify_escaped_literal_bang_filename_matches_as_ignore() {
        let rules = parse("\\!important.txt\n");
        assert_eq!(classify("!important.txt", false, &[&rules]), Tier::Ignore);
    }

    #[test]
    fn classify_multiple_rule_sets_fold_in_given_order() {
        let a = parse("x\n");
        let b = parse("!x\n");
        // b comes after a: last-match-wins across sets, not just within.
        assert_eq!(classify("x", false, &[&a, &b]), Tier::Full);
        assert_eq!(classify("x", false, &[&b, &a]), Tier::Ignore);
    }

    #[test]
    fn built_ins_skip_archives_and_a_user_line_brings_them_back() {
        let built = built_in_rule_sets();
        for path in ["backups/world.zip", "a.7z", "dist/app.jar", "disk.iso", "x.tar.gz"] {
            assert_eq!(classify(path, false, &[&built]), Tier::Ignore, "{path}");
        }
        assert_eq!(classify("notes.md", false, &[&built]), Tier::Full);
        assert_eq!(classify("clip.mp4", false, &[&built]), Tier::Full, "media stays: Ken transcribes it");
        let user = parse("!*.zip\n");
        assert_eq!(classify("backups/world.zip", false, &[&built, &user]), Tier::Full);
        // What a tool writes: bundles, maps, lockfiles.
        for path in ["dist/app.min.js", "web/site.min.css", "dist/app.js.map", "package-lock.json", "api/uv.lock", "Cargo.lock"] {
            assert_eq!(classify(path, false, &[&built]), Tier::Ignore, "{path}");
        }
        assert_eq!(classify("src/app.js", false, &[&built]), Tier::Full);
        assert_eq!(classify("package.json", false, &[&built]), Tier::Full);
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
            assert_eq!(classify(path, false, &[&user]), Tier::Ignore, "{path}");
        }
        for path in ["docs/secrets-policy.md", "keys.md", "src/credentials.rs", "monkey.txt"] {
            assert_eq!(classify(path, false, &[&user]), Tier::Full, "{path}");
        }
    }
}

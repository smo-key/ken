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
/// This still returns empty rather than being deleted: it is the
/// D2-correct plug point for a rule that really is global to every
/// project, and callers already thread it through.
pub fn built_in_rule_sets() -> Vec<Rule> {
    Vec::new()
}

fn is_hard_ignored(path: &str) -> bool {
    Path::new(path)
        .components()
        .any(|c| matches!(c.as_os_str().to_str(), Some(name) if HARD_IGNORE_DIRS.contains(&name)))
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

    let mut builder = ignore::gitignore::GitignoreBuilder::new(".");
    if builder.add_line(None, &pattern).is_err() {
        return false;
    }
    let matcher = match builder.build() {
        Ok(m) => m,
        Err(_) => return false,
    };
    matcher.matched_path_or_any_parents(path, is_dir).is_ignore()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- parse ----

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
    fn built_in_rule_sets_is_currently_an_empty_placeholder() {
        // ken-memory and ken-tasks (design.md D2/task 1.3) don't exist in
        // this codebase yet, so there are no built-in rules to contribute.
        // This test pins that honest-placeholder behavior so a future
        // implementation change is a deliberate edit, not a silent drift.
        assert!(built_in_rule_sets().is_empty());
    }
}

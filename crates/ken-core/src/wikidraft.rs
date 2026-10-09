//! The first wiki, drafted from an analysis at set-up (knowledge-layer item
//! 4b). Offered, never forced: Claude reads the repos (briefs by section,
//! READMEs, build files, top docs, layout, imports, changelogs by version,
//! who has committed recently), and any folder of documents a person adds (a
//! Confluence export, say), and drafts the pages the templates README says
//! Ken drafts (Current, Conventions, Platform, Reference, the release notes),
//! a Repo Map page for every repo and an architecture page for every code
//! repo, and START-HERE's Traps rows, for a person to review. Every part of
//! every brief goes to one page; what the checkout says now (what the code
//! wires at start-up, the versions and data values) outranks a doc.
//!
//! Four rules keep it honest:
//! - A page a person wrote is never touched. A page is drafted only when it
//!   is missing or is still an untouched template (it has `{{…}}` left).
//! - Every drafted page is `status: draft` with no `verified:` date, so the
//!   age rule keeps raising it until someone reads it against its sources.
//! - Every page names the sources it came from, in its frontmatter and
//!   inline, as `repo:path` locators.
//! - Every repo path, code name and pinned version a drafted page states is
//!   checked against the checkout; a path with one match is rewritten in
//!   place, and a page with anything else wrong goes back once.
//!
//! The model call is passed in (`generate`), as for ingest.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::db::Db;
use crate::{Error, Result};

pub const REVIEW_KIND: &str = "wiki-draft";
/// Characters of source text given to the model per page, all sources
/// together, so a large folder cannot overflow the prompt.
const SOURCE_BUDGET: usize = 120_000;
const PER_FILE: usize = 8_000;
/// Characters of one section of a brief. A brief is read by section, each up
/// to this, so a rule deep in a long brief still reaches the page: the
/// 2026-10-06 dry run read the first 8,000 characters of the mod's 152 KB
/// `CLAUDE.md`, and the architecture page said no source set out which layer
/// may call which while its lines 834 and 1,135 did.
const SECTION_MAX: usize = 12_000;
/// Characters of one version's release notes.
const VERSION_MAX: usize = 4_000;
/// Characters of one build file's summary.
const BUILD_MAX: usize = 6_000;
/// Characters a team page reads from the repos themselves (brief sections,
/// release notes, people files), all repos together, beside the repo pages.
const PAGE_BUDGET: usize = 80_000;

/// One thing read for the draft, labelled the way a page cites it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Source {
    /// `repo:path`, or `repo:(layout)` / `repo:(git authors)` for derived facts.
    pub label: String,
    pub text: String,
}

/// The pages drafted, in order: (path in the wiki, what the page is for).
/// The library pages the templates README says Ken drafts; a repo's own
/// pages are [`REPO_MAP`] and [`KIND_PAGES`].
pub const PAGES: &[(&str, &str)] = &[
    (
        "Current/Project.md",
        "what the product is and what it does for the people who use it, for someone who never reads code, from the READMEs and briefs; \
         who it is for, the goal and what is out of scope only where a source says so",
    ),
    (
        "Current/Team.md",
        "how the team is structured: roles, who decides what, how work is handed off; \
         roles and deciders only as CODEOWNERS, `people/` files or the repos' own docs name them; \
         every person on the team's roster (`people/`) is listed, whatever git says",
    ),
    (
        "Current/Who-Does-What.md",
        "who owns each area and repo, and who to ask, from CODEOWNERS, `people/` files and what the repos' own docs say; \
         every person on the team's roster (`people/`) gets a row; \
         git adds only a column of who has committed to each area recently (the recent contributors by folder), never an owner",
    ),
    (
        "Current/Feature-Status.md",
        "each feature, from the changelog and the code that registers it (the source `(what the code registers…)`), with the tests \
         that name it and the commit read; whether a user can reach it, what tests it and when it was checked are a person's to say: \
         leave those cells empty and name them once in the To fill line",
    ),
    (
        "Conventions/Architecture.md",
        "how the code repos fit together: each code repo, what it builds, which repos import or call which and how, with a mermaid diagram; \
         each repo's own layers are on its page, `Conventions/Architecture-<repo>.md`, so link it rather than repeat it",
    ),
    (
        "Conventions/Code.md",
        "the code conventions the tools enforce, from the lint and formatter config, and the helpers that exist and where they live",
    ),
    (
        "Conventions/Data-and-Config.md",
        "the data and config folders, their formats and the code that loads each",
    ),
    (
        "Conventions/Testing.md",
        "the test suites and how each is run, from `.ken/gates.json` and the test config, and the fixture folders",
    ),
    (
        "Conventions/Registries.md",
        "the registries in the code (installers, routes, command lists): where each is and what registers in it",
    ),
    (
        "Platform/Index.md",
        "one row per reference repo and pinned dependency: what it is, the version pinned and where the pin is",
    ),
    (
        "Work/Releases.md",
        "a business doc (item 2b): what changed in each release, newest first, in words for someone who never reads code, from changelogs and release tags; \
         first an Unreleased section of what landed since the newest tag, grouped by what it does for the user, from the changes since it",
    ),
    (
        "Reference/Systems.md",
        "one row per system, from the code that registers it (the source `(what the code registers…)`, one row per registration \
         a source describes): its code and data paths, and the rulings (D-nnn) its code cites; the registrations no source \
         describes are named on one line, with no row",
    ),
    (
        "Reference/Config-Map.md",
        "every config file and key, its default, and the code that reads it",
    ),
    (
        "Reference/Build-and-Run.md",
        "per repo: the tools and versions the build files pin, the scripts and tasks the build files define, \
         the `.ken/gates.json` commands, and the environment variables the code reads",
    ),
    (
        "Reference/Vocabulary.md",
        "the traps where a search in one word misses a page in another, and nothing else: a word the team, the users or the briefs use \
         that differs from the code's or the platform's word; a word with two meanings; a rename, from commit messages and the briefs. \
         A word that means what it says gets no row; glossaries first",
    ),
];

fn clip(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let mut end = n;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[… clipped]", &s[..end])
}

fn read_text(path: &Path) -> Option<String> {
    let t = crate::extract::extract(path).ok()?.text;
    (!t.trim().is_empty()).then_some(t)
}

/// A file's text as it is on disk, line numbers intact, for files read by
/// section (a brief, a changelog) or by line (a build file).
fn read_plain(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let t = String::from_utf8_lossy(&bytes).replace("\r\n", "\n");
    (!t.trim().is_empty()).then_some(t)
}

/// One line of text within `n` characters, cut at a character boundary
/// with `…`: a line quoted as evidence, never a source.
pub(crate) fn clip_line(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let mut end = n;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// Run git in `root` and read what it prints as UTF-8, whatever the
/// console's code page: on Windows a contributor's accented name came out
/// garbled in the 2026-10-06 dry run. None when git fails.
pub(crate) fn git_text(root: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = Command::new("git");
    let out = crate::proc::quiet(&mut cmd)
        .args(["-c", "i18n.logOutputEncoding=UTF-8", "-c", "core.quotePath=false"])
        .args(args)
        .current_dir(root)
        .output()
        .ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The lines `git grep -F` finds for `needle` in `root`'s code and data,
/// docs left out, as `repo:path:line: text`, test files last, at most 40;
/// with how many there were in all.
pub(crate) fn grep_repo(repo: &str, root: &Path, needle: &str, ignore_case: bool) -> (usize, Vec<String>) {
    let mut args = vec!["grep", "-n", "-I", "-F"];
    if ignore_case {
        args.push("-i");
    }
    args.extend(["-e", needle, "--", ".", ":!*.md", ":!*.mdx", ":!*.txt", ":!*.rst", ":!*.adoc"]);
    let text = git_text(root, &args).unwrap_or_default();
    let mut lines: Vec<(bool, String)> = text
        .lines()
        .filter_map(|l| {
            let (path, rest) = l.split_once(':')?;
            let (n, body) = rest.split_once(':')?;
            Some((crate::checkout::is_test_path(path), format!("{repo}:{path}:{n}: {}", clip_line(body.trim(), 200))))
        })
        .collect();
    let count = lines.len();
    lines.sort_by_key(|(test, _)| *test);
    (count, lines.into_iter().take(40).map(|(_, l)| l).collect())
}

/// [`grep_repo`] for `word` as a whole word, in any case: `KING` finds the
/// `"King"` a loader still accepts, not `making`.
pub(crate) fn grep_repo_word(repo: &str, root: &Path, word: &str) -> (usize, Vec<String>) {
    let text = git_text(root, &["grep", "-n", "-I", "-F", "-i", "-w", "-e", word, "--", ".", ":!*.md", ":!*.txt"]).unwrap_or_default();
    let lines: Vec<String> = text
        .lines()
        .filter_map(|l| {
            let (path, rest) = l.split_once(':')?;
            let (n, body) = rest.split_once(':')?;
            Some(format!("{repo}:{path}:{n}: {}", clip_line(body.trim(), 200)))
        })
        .collect();
    (lines.len(), lines.into_iter().take(20).collect())
}

/// Every file git tracks in `root`, as `/`-separated paths.
fn ls_files(root: &Path) -> Option<Vec<String>> {
    let text = git_text(root, &["ls-files", "-z"])?;
    Some(text.split('\0').filter(|f| !f.is_empty()).map(str::to_string).collect())
}

/// The files at the top of `root` named in `wanted`, matched without regard
/// to case and each read once, under its real name, in `wanted`'s order. On
/// Windows `README.md` and `readme.md` are one file, and the 2026-10-06 dry
/// run read it under both names and told the page the repo had two.
fn top_files(root: &Path, wanted: &[&str]) -> Vec<(String, PathBuf)> {
    let names: Vec<String> = fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut out = Vec::new();
    for w in wanted {
        let mut matching: Vec<&String> = names.iter().filter(|n| n.eq_ignore_ascii_case(w)).collect();
        matching.sort();
        for n in matching {
            let path = root.join(n);
            let canon = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            if !seen.contains(&canon) {
                seen.push(canon);
                out.push((n.clone(), path));
            }
        }
    }
    out
}

/// Dot-folders listed in the layout although other dot-folders are not: they
/// say how the repo is worked on (Ken's gates, CI, the team manifest).
const DOT_FOLDERS: &[&str] = &[".ken", ".github", ".wright", ".gitlab", ".circleci", ".devcontainer"];
/// Entries listed under one top-level folder.
const LEVEL_TWO_MAX: usize = 200;
/// Characters of the whole folder listing.
const LAYOUT_BUDGET: usize = 16_000;

/// The repo's shape: its top two levels, folders first, skipping dotfiles
/// (the dot-folders of [`DOT_FOLDERS`] aside) and dependency folders; then the
/// folders git tracks files in, one depth at a time, while
/// [`LAYOUT_BUDGET`] lasts. Two levels at 15 entries a folder hid
/// `packages/weather-compositor`, `.ken/` and `HytaleServer/builtin`, and never
/// reached the mod's 37 `data/` folders (2026-10-06).
fn layout(root: &Path, tracked: &[String]) -> String {
    let shown = |name: &str| !name.starts_with('.') || DOT_FOLDERS.contains(&name);
    let mut out = String::new();
    let list = |dir: &Path| -> Vec<(String, bool)> {
        let mut v: Vec<(String, bool)> = fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                let is_dir = e.path().is_dir();
                let keep = if is_dir { shown(&name) && !crate::scan::is_junk_dir_name(&name) } else { !name.starts_with('.') };
                keep.then_some((name, is_dir))
            })
            .collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v
    };
    for (name, is_dir) in list(root) {
        out.push_str(&format!("{name}{}\n", if is_dir { "/" } else { "" }));
        if is_dir {
            let children = list(&root.join(&name));
            let more = children.len().saturating_sub(LEVEL_TWO_MAX);
            for (child, cdir) in children.into_iter().take(LEVEL_TWO_MAX) {
                out.push_str(&format!("  {child}{}\n", if cdir { "/" } else { "" }));
            }
            if more > 0 {
                out.push_str(&format!("  (… {more} more)\n"));
            }
        }
    }
    if out.len() >= LAYOUT_BUDGET {
        return clip(&out, LAYOUT_BUDGET);
    }
    // Deeper, from the files git tracks: folders only, a whole depth at a time.
    let mut by_depth: BTreeMap<usize, BTreeSet<String>> = BTreeMap::new();
    for f in tracked {
        let segs: Vec<&str> = f.split('/').collect();
        if !shown(segs[0]) {
            continue;
        }
        for d in 3..segs.len() {
            by_depth.entry(d).or_default().insert(segs[..d].join("/"));
        }
    }
    let mut deeper = String::new();
    for (depth, folders) in by_depth {
        let block: String = folders.iter().map(|f| format!("{f}/\n")).collect();
        if out.len() + deeper.len() + block.len() > LAYOUT_BUDGET {
            deeper.push_str(&format!("({} folders {depth} levels down, and any deeper, not listed)\n", folders.len()));
            break;
        }
        deeper.push_str(&block);
    }
    if !deeper.is_empty() {
        out.push_str("\nfolders further down, from the files git tracks:\n");
        out.push_str(&deeper);
    }
    out
}

/// Whether a tag names a version: `v1.2`, `1.2.3`, `v0.0.5.1`. An
/// `archive/…` tag is not one.
fn is_version_tag(tag: &str) -> bool {
    tag.strip_prefix(['v', 'V']).unwrap_or(tag).starts_with(|c: char| c.is_ascii_digit())
}

/// Every version tag, newest first, as `tag date subject`: all of them (a
/// 40-line cap showed 5 of the mod's 13 version tags on 2026-10-06), and only
/// version tags.
fn git_tags(root: &Path) -> Option<String> {
    let text = git_text(
        root,
        &["for-each-ref", "--sort=-creatordate", "--format=%(refname:short) %(creatordate:short) %(subject)", "refs/tags"],
    )?;
    let lines: Vec<&str> = text.lines().filter(|l| is_version_tag(l.split(' ').next().unwrap_or(""))).collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// What landed after the newest version tag the checkout contains (`v1.2`,
/// else `1.2`), as `date subject` lines, newest first, every commit of the
/// range; the last 200 commits when the repo has no version tag. The label
/// says which. Only a version tag counts: a plain `describe --tags` picked
/// `archive/ui-faction-profession-combat-2026-09-30` on 2026-10-06, and the
/// Unreleased notes started five days late. A 200-commit cap then hid six of
/// the ten days since `v0.0.15` (508 commits); [`clip_by_day`] keeps them.
fn git_unreleased(root: &Path) -> Option<(String, String)> {
    let describe = |pattern: &str| {
        git_text(root, &["describe", "--tags", "--abbrev=0", "--match", pattern])
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
    };
    let tag = describe("v[0-9]*").or_else(|| describe("[0-9]*"));
    let mut args = vec!["log", "--no-merges", "--date=short", "--format=%ad %s"];
    let range = match &tag {
        Some(t) => format!("{t}..HEAD"),
        None => {
            args.extend(["-n", "200"]);
            "HEAD".to_string()
        }
    };
    args.push(&range);
    let log = git_text(root, &args).map(|l| l.trim().to_string()).filter(|l| !l.is_empty())?;
    let label = match &tag {
        Some(t) => format!("(changes since {t})"),
        None => "(recent changes, no tags)".to_string(),
    };
    Some((label, log))
}

/// Characters of what landed since the newest version tag, for Releases.
const UNRELEASED_MAX: usize = 16_000;

/// `log`, `date subject` lines newest first, within `budget`: each day its
/// fair share ([`fair_shares`]), a day cut short ending with how many more
/// it had, so every day since the tag is there however busy the newest was.
fn clip_by_day(log: &str, budget: usize) -> String {
    if log.len() <= budget {
        return log.to_string();
    }
    let mut days: Vec<(&str, Vec<&str>)> = Vec::new();
    for line in log.lines() {
        let day = line.split(' ').next().unwrap_or_default();
        match days.last_mut() {
            Some((d, lines)) if *d == day => lines.push(line),
            _ => days.push((day, vec![line])),
        }
    }
    // Room for each day's "more" line comes off the top.
    let room = budget.saturating_sub(days.len() * 40);
    let sizes: Vec<usize> = days.iter().map(|(_, l)| l.iter().map(|x| x.len() + 1).sum()).collect();
    let mut out = String::new();
    for ((day, lines), share) in days.iter().zip(fair_shares(&sizes, room)) {
        let mut used = 0;
        let mut kept = 0;
        for l in lines {
            if used + l.len() + 1 > share {
                break;
            }
            used += l.len() + 1;
            kept += 1;
            out.push_str(l);
            out.push('\n');
        }
        if kept < lines.len() {
            out.push_str(&format!("{day} (… {} more that day)\n", lines.len() - kept));
        }
    }
    out
}

/// Bots and AI agents, by name or email: never a person to ask. Dependabot,
/// GitHub Actions, any `[bot]`, Copilot, Claude, Cursor Agent, a name ending
/// in "Bot" (Hytale Sync Bot), and the noreply addresses bots commit from.
pub fn is_bot(name: &str, email: &str) -> bool {
    let n = name.trim().to_lowercase();
    let e = email.trim().to_lowercase();
    let local = e.split('@').next().unwrap_or("");
    ["copilot", "claude", "cursor agent", "cursoragent", "github-actions", "dependabot"].contains(&n.as_str())
        || n.starts_with("dependabot")
        || n.starts_with("github-actions")
        || n.ends_with("[bot]")
        || name.trim().ends_with("Bot")
        || n.ends_with(" bot")
        || local.ends_with("[bot]")
        || ["noreply@anthropic.com", "cursoragent@cursor.com", "copilot@github.com"].contains(&e.as_str())
        || (e.contains("noreply") && local.contains("bot"))
}

/// A file's area one level down: its first two folders, or past a source
/// set's root (`src/main/java/`, `src/main/resources/`, `src/test/kotlin/`)
/// the two after it, as `a/b/`. None for a file less deep.
fn second_level(path: &str) -> Option<String> {
    let segs: Vec<&str> = path.split('/').collect();
    let dirs = &segs[..segs.len().saturating_sub(1)];
    let skip = match dirs {
        ["src", _, lang, ..] if ["java", "kotlin", "scala", "groovy", "resources"].contains(lang) => 3,
        _ => 0,
    };
    (dirs.len() >= skip + 2).then(|| format!("{}/", dirs[..skip + 2].join("/")))
}

/// Who has committed to each top-level folder in the last 90 days, as
/// `folder/: Name (commits), …`, at most three a folder, most first; then
/// each folder one level down ([`second_level`]) where someone not in its
/// top-level row has at least 5% of its commits, so a person who owns one
/// area shows up behind those who commit everywhere (Andrew's `data/spawns`
/// work sat behind 1,615 commits under `src/` on 2026-10-06). A commit
/// identity the team's `roster` lists counts as that person, under their
/// roster name, and is never dropped as a bot; any other is one person per
/// email ([`crate::people::person_key`]), shown under the name they commit
/// with most, bots and AI agents left out. This is a column of recent hands,
/// never an owner: the 2026-10-06 draft read a whole-history shortlog and
/// put the template's committers from before the team existed into the
/// team's roles table.
pub fn recent_contributors(root: &Path, roster: &[crate::people::Person]) -> Option<String> {
    let text = git_text(root, &["log", "--since=90.days", "--no-merges", "--format=%x1e%aN%x1f%aE", "--name-only"])?;
    let mut counts: BTreeMap<String, HashMap<String, usize>> = BTreeMap::new();
    let mut counts_two: BTreeMap<String, HashMap<String, usize>> = BTreeMap::new();
    let mut names: HashMap<String, HashMap<String, usize>> = HashMap::new();
    for rec in text.split('\x1e').filter(|r| !r.trim().is_empty()) {
        let mut lines = rec.lines();
        let Some((name, email)) = lines.next().and_then(|l| l.split_once('\x1f')) else { continue };
        let (key, shown) = match crate::people::by_commit(roster, name, email) {
            Some(p) => (format!("roster:{}", p.id), p.name.clone()),
            None if is_bot(name, email) => continue,
            None => (crate::people::person_key(email), name.trim().to_string()),
        };
        *names.entry(key.clone()).or_default().entry(shown).or_default() += 1;
        let files: Vec<&str> = lines.map(str::trim).filter(|l| !l.is_empty()).collect();
        let areas: BTreeSet<String> = files
            .iter()
            .map(|f| f.split_once('/').map_or_else(|| "(top-level files)".to_string(), |(a, _)| format!("{a}/")))
            .collect();
        for a in areas {
            *counts.entry(a).or_default().entry(key.clone()).or_default() += 1;
        }
        let deeper: BTreeSet<String> = files.iter().filter_map(|f| second_level(f)).collect();
        for a in deeper {
            *counts_two.entry(a).or_default().entry(key.clone()).or_default() += 1;
        }
    }
    let name_of = |k: &str| {
        names
            .get(k)
            .and_then(|m| m.iter().max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0))))
            .map(|(n, _)| n.clone())
            .unwrap_or_default()
    };
    // The people of an area, most commits first, one row per name.
    let ranked = |people: &HashMap<String, usize>| -> Vec<(String, usize)> {
        let mut by_key: Vec<(&String, &usize)> = people.iter().collect();
        by_key.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        let mut shown: Vec<(String, usize)> = Vec::new();
        for (k, n) in by_key {
            let name = name_of(k);
            if !shown.iter().any(|(s, _)| *s == name) {
                shown.push((name, *n));
            }
        }
        shown
    };
    let row = |v: &[(String, usize)]| v.iter().map(|(s, n)| format!("{s} ({n})")).collect::<Vec<_>>().join(", ");
    let mut out = String::new();
    let mut top_rows: HashMap<String, Vec<String>> = HashMap::new();
    for (area, people) in &counts {
        let shown: Vec<(String, usize)> = ranked(people).into_iter().take(3).collect();
        top_rows.insert(area.clone(), shown.iter().map(|(s, _)| s.clone()).collect());
        out.push_str(&format!("{area}: {}\n", row(&shown)));
    }
    let mut deeper = String::new();
    for (area, people) in &counts_two {
        let total: usize = people.values().sum();
        let top = area.split_once('/').map(|(a, _)| format!("{a}/")).unwrap_or_default();
        let above = top_rows.get(&top).cloned().unwrap_or_default();
        let shown: Vec<(String, usize)> = ranked(people).into_iter().filter(|(_, n)| n * 20 >= total).take(3).collect();
        if shown.iter().any(|(s, _)| !above.contains(s)) {
            deeper.push_str(&format!("{area}: {}\n", row(&shown)));
        }
    }
    if !deeper.is_empty() {
        out.push_str("\none level down, where someone not listed above has commits:\n");
        out.push_str(&deeper);
    }
    (!out.is_empty()).then_some(out)
}

/// Where a code file's source tree starts: the folder up to
/// `src/<set>/<java|kotlin|scala|groovy>/`, else up to `packages/<name>/` or
/// `apps/<name>/`, else up to the first `src/` or `lib/`; the repo's top when
/// it has none of them.
fn source_marker(path: &str) -> Vec<&str> {
    let segs: Vec<&str> = path.split('/').collect();
    let dirs = &segs[..segs.len().saturating_sub(1)];
    for i in 0..dirs.len() {
        if dirs[i] == "src" && i + 2 < dirs.len() && ["java", "kotlin", "scala", "groovy"].contains(&dirs[i + 2]) {
            return dirs[..=i + 2].to_vec();
        }
    }
    if let Some(i) = dirs.iter().position(|d| *d == "packages" || *d == "apps").filter(|i| i + 1 < dirs.len()) {
        return dirs[..=i + 1].to_vec();
    }
    if let Some(i) = dirs.iter().position(|d| *d == "src" || *d == "lib") {
        return dirs[..=i].to_vec();
    }
    Vec::new()
}

/// Each code file's folder for the import map, `depth` folders below its
/// source root, where the source root is the deepest folder that every code
/// file under the same [`source_marker`] shares (`src/main/java/dev/hytalemodding`).
/// A fixed first four segments named every Java file `src/main/java/dev`, so
/// the mod's 1,349 plugin files were one folder and the services <-> systems
/// cycle (13 files one way, 139 the other) never showed (2026-10-06).
fn folders_below_source_root(code: &[&String], depth: usize) -> HashMap<String, String> {
    let dirs_of = |f: &str| -> Vec<String> {
        let segs: Vec<&str> = f.split('/').collect();
        segs[..segs.len() - 1].iter().map(|s| s.to_string()).collect()
    };
    let mut roots: HashMap<Vec<&str>, Vec<String>> = HashMap::new();
    for f in code {
        let marker = source_marker(f);
        let dirs = dirs_of(f);
        match roots.get_mut(&marker) {
            None => {
                roots.insert(marker, dirs);
            }
            Some(common) => {
                let n = common.iter().zip(&dirs).take_while(|(a, b)| a == b).count();
                common.truncate(n);
            }
        }
    }
    code.iter()
        .map(|f| {
            let root = &roots[&source_marker(f)];
            let dirs = dirs_of(f);
            let name: Vec<&str> = dirs.iter().take(root.len() + depth).map(String::as_str).collect();
            let name = if name.is_empty() { ".".to_string() } else { name.join("/") };
            (f.to_string(), name)
        })
        .collect()
}

/// The language a file's imports are read in: its code map language, and a
/// Svelte or Vue component's as TypeScript (its `<script>` imports are).
fn import_lang(path: &str) -> Option<crate::codemap::Lang> {
    crate::codemap::Lang::of(path).or_else(|| (path.ends_with(".svelte") || path.ends_with(".vue")).then_some(crate::codemap::Lang::TypeScript))
}

/// The repo's own packages by the name its code imports them by, as (name,
/// folder), longest name first: each package.json's `name` below the top,
/// and each Cargo package's name with `-` as `_`.
fn workspace_packages(root: &Path, files: &[String]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for f in files {
        let Some((dir, file)) = f.rsplit_once('/') else { continue };
        let name = match file {
            "package.json" => read_plain(&root.join(f))
                .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                .and_then(|v| v.get("name").and_then(|n| n.as_str()).map(String::from)),
            "Cargo.toml" => read_plain(&root.join(f)).and_then(|t| {
                toml_tables(&t, &["[package]"])
                    .lines()
                    .find_map(|l| l.trim().strip_prefix("name").map(|r| r.trim_start().trim_start_matches('=').trim().trim_matches('"').replace('-', "_")))
            }),
            _ => None,
        };
        if let Some(n) = name.filter(|n| !n.is_empty()) {
            out.push((n, dir.to_string()));
        }
    }
    out.sort_by_key(|(n, _)| std::cmp::Reverse(n.len()));
    out
}

/// The file an import of one of the repo's own packages by name lands on:
/// `@app/schema` and `@app/schema/types` in a pnpm or npm workspace,
/// `app_core::x` in a Cargo one. Its entry (`src/index.ts`, `index.js`,
/// `src/lib/index.ts`, `src/lib.rs`), or the path under it, else its first
/// code file: the import map counts folders, and the folder is known. The
/// Tools import map missed 749 files that import a sibling package by name
/// on 2026-10-06.
fn resolve_package(target: &str, packages: &[(String, String)], files: &[String]) -> Option<String> {
    let (dir, sub) = packages.iter().find_map(|(name, dir)| {
        let rest = target.strip_prefix(name.as_str())?;
        (rest.is_empty() || rest.starts_with('/') || rest.starts_with("::")).then(|| (dir, rest.trim_start_matches(['/', ':'])))
    })?;
    let exists = |p: &str| files.iter().any(|f| f == p);
    let exts = ["ts", "tsx", "js", "mjs", "jsx", "svelte", "rs"];
    let mut bases: Vec<String> = Vec::new();
    if sub.is_empty() || sub.contains("::") || target.contains("::") {
        bases.extend(["src/index", "index", "src/lib/index", "src/lib", "src/main"].iter().map(|b| format!("{dir}/{b}")));
    } else {
        for at in ["", "src/", "src/lib/"] {
            bases.push(format!("{dir}/{at}{sub}"));
            bases.push(format!("{dir}/{at}{sub}/index"));
        }
    }
    for b in &bases {
        if exists(b) && import_lang(b).is_some() {
            return Some(b.clone());
        }
        if let Some(hit) = exts.iter().map(|e| format!("{b}.{e}")).find(|c| exists(c)) {
            return Some(hit);
        }
    }
    let prefix = format!("{dir}/");
    files.iter().filter(|f| f.starts_with(&prefix) && import_lang(f).is_some()).min().cloned()
}

/// Which folders' code imports which, from the code itself: one line per
/// pair, `from -> to (imports)`, most first, each folder named two levels
/// below its source root. Then every pair that imports both ways, one level
/// and two levels below the root, so a cycle between two layers shows however
/// its files are spread. What a layer may call, as the code has it, for a
/// repo whose docs never say. None for a repo with no mapped code.
pub fn folder_imports(root: &Path) -> Option<String> {
    let files = ls_files(root)?;
    let code: Vec<&String> = files.iter().filter(|f| import_lang(f).is_some()).collect();
    let packages = workspace_packages(root, &files);
    let two = folders_below_source_root(&code, 2);
    let one = folders_below_source_root(&code, 1);
    let mut edges: HashMap<(String, String), usize> = HashMap::new();
    let mut edges_one: HashMap<(String, String), usize> = HashMap::new();
    // How many other files import each file: the hubs to start reading at.
    let mut importers: HashMap<String, HashSet<String>> = HashMap::new();
    for rel in &code {
        let Some(lang) = import_lang(rel) else { continue };
        let Ok(text) = fs::read_to_string(root.join(rel)) else { continue };
        if text.len() > 1_000_000 {
            continue;
        }
        // A component's imports resolve as its script's would.
        let from = if crate::codemap::Lang::of(rel).is_none() { format!("{rel}.ts") } else { rel.to_string() };
        for target in crate::codemap::imports_of(lang, &text) {
            let resolved = crate::codemap::resolve_import(&from, &target, &files).or_else(|| resolve_package(&target, &packages, &files));
            let Some(to) = resolved.filter(|t| import_lang(t).is_some()) else { continue };
            if &to != *rel {
                importers.entry(to.clone()).or_default().insert(rel.to_string());
            }
            let (Some(a), Some(b)) = (two.get(*rel), two.get(&to)) else { continue };
            if a != b {
                *edges.entry((a.clone(), b.clone())).or_default() += 1;
            }
            let (a, b) = (&one[*rel], &one[&to]);
            if a != b {
                *edges_one.entry((a.clone(), b.clone())).or_default() += 1;
            }
        }
    }
    if edges.is_empty() {
        return None;
    }
    // Pairs that import each other: not a one-way layering, whatever the
    // docs say.
    // A pair named the same at both depths (a folder with no subfolders) is
    // listed once, with the one-level counts, which take in its subfolders.
    let both_ways = |edges: &HashMap<(String, String), usize>| -> Vec<(String, String, usize)> {
        edges
            .iter()
            .filter(|((a, b), _)| a < b)
            .filter_map(|((a, b), n)| edges.get(&(b.clone(), a.clone())).map(|m| (format!("{a} <-> {b}"), format!("({n} / {m})"), n + m)))
            .collect()
    };
    let mut both: Vec<(String, String, usize)> = both_ways(&edges_one);
    for (pair, counts, n) in both_ways(&edges) {
        if !both.iter().any(|(p, _, _)| *p == pair) {
            both.push((pair, counts, n));
        }
    }
    let mut both: Vec<(String, usize)> = both.into_iter().map(|(p, c, n)| (format!("{p} {c}"), n)).collect();
    // The most-used pairs when there are many, listed in name order.
    both.sort_by(|x, y| y.1.cmp(&x.1).then_with(|| x.0.cmp(&y.0)));
    let more = both.len().saturating_sub(80);
    let mut both: Vec<String> = both.into_iter().take(80).map(|(p, _)| p).collect();
    both.sort();
    if more > 0 {
        both.push(format!("(… {more} more pairs)"));
    }
    let mut rows: Vec<_> = edges.into_iter().collect();
    rows.sort_by(|x, y| y.1.cmp(&x.1).then_with(|| x.0.cmp(&y.0)));
    let mut out = rows.into_iter().take(80).map(|((a, b), n)| format!("{a} -> {b} ({n})")).collect::<Vec<_>>().join("\n");
    if !both.is_empty() {
        out.push_str("\n\nimport each other (a -> b / b -> a):\n");
        out.push_str(&both.join("\n"));
    }
    let mut hubs: Vec<(String, usize)> = importers.into_iter().map(|(f, by)| (f, by.len())).filter(|(_, n)| *n > 1).collect();
    hubs.sort_by(|x, y| y.1.cmp(&x.1).then_with(|| x.0.cmp(&y.0)));
    if !hubs.is_empty() {
        out.push_str("\n\nmost imported files (how many files import each):\n");
        out.push_str(&hubs.iter().take(15).map(|(f, n)| format!("{f} ({n})")).collect::<Vec<_>>().join("\n"));
    }
    Some(out)
}

/// Characters of source text read from one repo for its Repo Map page, so
/// every repo gets its own budget however many the team has. Raised from
/// 60,000 when briefs began to be read by section and build files, gates and
/// deeper folders joined the read (2026-10-06).
const REPO_BUDGET: usize = 90_000;

fn pusher(budget: usize) -> impl Fn(String, String, &mut Vec<Source>) {
    pusher_of(budget, PER_FILE)
}

/// A push into a source list that stops at `budget` characters in all, each
/// source clipped to `each`.
fn pusher_of(budget: usize, each: usize) -> impl Fn(String, String, &mut Vec<Source>) {
    move |label: String, text: String, out: &mut Vec<Source>| {
        let used: usize = out.iter().map(|s| s.text.len()).sum();
        if used < budget {
            out.push(Source { label, text: clip(&text, each.min(budget - used)) });
        }
    }
}

/// One part of a Markdown file: the line its heading is on (1-based), the
/// heading's words, the heading of the `##` section it was split from (empty
/// for a whole section), and its text, heading included.
#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    pub line: usize,
    pub heading: String,
    pub parent: String,
    pub text: String,
}

/// Split Markdown at the lines `is_head` picks, never inside a code fence.
/// What comes before the first is a section with no heading, at line 1,
/// when it has any words.
fn split_at(text: &str, is_head: impl Fn(&str) -> bool) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    let mut cur = Section { line: 1, heading: String::new(), parent: String::new(), text: String::new() };
    let mut fence = false;
    for (i, line) in text.lines().enumerate() {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            fence = !fence;
        }
        if !fence && is_head(line) {
            if !cur.text.trim().is_empty() {
                out.push(cur);
            }
            cur = Section { line: i + 1, heading: line.trim_start_matches('#').trim().to_string(), parent: String::new(), text: String::new() };
        }
        cur.text.push_str(line);
        cur.text.push('\n');
    }
    if !cur.text.trim().is_empty() {
        out.push(cur);
    }
    out
}

/// `s` in parts of at most `max` characters, cut only at a blank line
/// outside a code fence (or, for one paragraph longer than `max`, at a line
/// end), each part starting at its own line and keeping the heading.
fn split_paragraphs(s: Section, max: usize) -> Vec<Section> {
    if s.text.len() <= max {
        return vec![s];
    }
    let mut out: Vec<Section> = Vec::new();
    let mut cur = String::new();
    let (mut start, mut fence) = (s.line, false);
    let mut cut_at: Option<usize> = None; // byte in `cur` after its last blank line
    for (i, line) in s.text.lines().enumerate() {
        if line.trim_start().starts_with("```") || line.trim_start().starts_with("~~~") {
            fence = !fence;
        }
        if cur.len() + line.len() + 1 > max && !cur.trim().is_empty() {
            let at = cut_at.filter(|a| *a > 0).unwrap_or(cur.len());
            let rest = cur.split_off(at);
            out.push(Section { line: start, heading: s.heading.clone(), parent: s.parent.clone(), text: cur });
            start = s.line + i - rest.lines().count();
            cur = rest;
            cut_at = None;
        }
        cur.push_str(line);
        cur.push('\n');
        if line.trim().is_empty() && !fence {
            cut_at = Some(cur.len());
        }
    }
    if !cur.trim().is_empty() {
        out.push(Section { line: start, heading: s.heading.clone(), parent: s.parent.clone(), text: cur });
    }
    out
}

/// A brief split at its `##` headings, a `###` inside its section. A section
/// longer than [`SECTION_MAX`] is split again at its `###` headings, each
/// part knowing its section's heading, and a part still longer at its blank
/// lines: nothing is cut. On 2026-10-06 the mod's 45,929-character Common
/// Gotchas and 20,979-character Key Architectural Patterns were each clipped
/// to 12,000, 28% of its brief.
pub fn brief_sections(text: &str) -> Vec<Section> {
    let mut out = Vec::new();
    for s in split_at(text, |l| l.starts_with("## ")) {
        if s.text.len() <= SECTION_MAX {
            out.push(s);
            continue;
        }
        for mut part in split_at(&s.text, |l| l.starts_with("### ")) {
            part.line += s.line - 1;
            if part.line == s.line {
                part.heading = s.heading.clone();
            } else {
                part.parent = s.heading.clone();
            }
            out.extend(split_paragraphs(part, SECTION_MAX));
        }
    }
    out
}

/// Whether a line is a version's heading in a changelog: `#` to `###`, then
/// words that, past any `[` or `v`, start with a digit or say Unreleased.
fn is_version_heading(line: &str) -> bool {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if !(1..=3).contains(&hashes) || !line[hashes..].starts_with(' ') {
        return false;
    }
    let w = line[hashes..].trim().trim_start_matches('[').trim_start_matches(['v', 'V']);
    w.starts_with(|c: char| c.is_ascii_digit()) || w.to_ascii_lowercase().starts_with("unreleased")
}

/// A changelog split into one section per version, in the file's order;
/// what comes before the first version is left out. The Releases page gets
/// each version's own notes: on 2026-10-06 it got the first 3,000 characters
/// of the mod's 339 KB changelog, and 0.0.11 and 0.0.12 came out empty.
pub fn version_sections(text: &str) -> Vec<Section> {
    split_at(text, is_version_heading).into_iter().filter(|s| is_version_heading(s.text.lines().next().unwrap_or(""))).collect()
}

/// What a brief's section is about, which says which pages it may serve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topic {
    /// How to build, run, test and deploy: the repo's page.
    Build,
    /// Layers, structure, patterns, systems: the architecture page.
    Architecture,
    /// Glossaries, naming: Vocabulary.
    Vocabulary,
    /// Gotchas, pitfalls, warnings: START-HERE's traps.
    Traps,
    /// Team, owners, roles, reviews: Team and Who Does What.
    People,
    /// Releases and versions: Releases.
    Releases,
    /// What the project is, its goal: Project.
    Project,
    Other,
}

/// The words that route a section, checked in this order. A heading picks
/// the first topic one of whose words begins a word of the heading (a word
/// ending in a space must be the whole word).
const TOPIC_WORDS: &[(Topic, &[&str])] = &[
    (Topic::Traps, &["gotcha", "pitfall", "trap", "warning", "caveat", "footgun", "beware", "known issue", "lessons"]),
    (
        Topic::Build,
        &[
            "build", "run", "install", "setup", "set up", "getting started", "quick start", "quickstart", "develop", "deploy",
            "test", "command", "script", "usage", "prerequisite", "requirement", "toolchain", "environment", "debug", "ci ",
        ],
    ),
    (
        Topic::Architecture,
        &[
            "architect", "layer", "structure", "pattern", "wiring", "module", "package", "thread", "dependenc", "design",
            "system", "pipeline", "flow", "entry point", "config", "convention",
        ],
    ),
    (Topic::Vocabulary, &["vocabulary", "glossary", "term", "naming", "names", "jargon", "words", "called", "spelling"]),
    (
        Topic::People,
        &["team", "people", "owner", "maintainer", "contributor", "contact", "role", "who ", "review", "governance", "decision"],
    ),
    (Topic::Releases, &["release", "changelog", "version", "patch note", "history"]),
    (
        Topic::Project,
        &["what this is", "what it is", "about", "overview", "introduction", "goal", "roadmap", "feature", "purpose", "vision", "status"],
    ),
];

/// The topic of a brief's section, from its heading by [`TOPIC_WORDS`]. The
/// text before a brief's first heading says what the repo is: Project.
pub fn topic_of(heading: &str) -> Topic {
    if heading.trim().is_empty() {
        return Topic::Project;
    }
    let spaced = spaced_words(heading);
    TOPIC_WORDS
        .iter()
        .find(|(_, keys)| keys.iter().any(|k| spaced.contains(&format!(" {k}"))))
        .map_or(Topic::Other, |(t, _)| *t)
}

/// `text` lowercased as ` word word … `, punctuation gone, for matching a
/// word's start with `" {key}"`.
fn spaced_words(text: &str) -> String {
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    format!(" {} ", words.join(" "))
}

/// START-HERE, whose Traps rows Ken drafts from the briefs' gotchas.
pub const START_HERE: &str = "START-HERE.md";

/// The pages a part of each topic may go to, the topic's own page first.
const TOPIC_PAGES: &[(Topic, &[&str])] = &[
    (Topic::Build, &["Reference/Build-and-Run.md", "Conventions/Testing.md"]),
    (
        Topic::Architecture,
        &[
            "Conventions/Architecture.md",
            "Reference/Systems.md",
            "Conventions/Registries.md",
            "Conventions/Data-and-Config.md",
            "Reference/Config-Map.md",
            "Conventions/Code.md",
            "Platform/Index.md",
        ],
    ),
    (Topic::Vocabulary, &["Reference/Vocabulary.md"]),
    (Topic::Traps, &[START_HERE]),
    (Topic::People, &["Current/Team.md", "Current/Who-Does-What.md"]),
    (Topic::Releases, &["Work/Releases.md"]),
    (Topic::Project, &["Current/Project.md", "Current/Feature-Status.md"]),
];

/// The words that say which page a part fits best, beside its topic: a word
/// of the heading counts ten times a word of the body.
const PAGE_WORDS: &[(&str, &[&str])] = &[
    ("Reference/Build-and-Run.md", &["build", "run", "install", "launch", "server", "gradle", "command", "script", "deploy", "environment", "prerequisite", "toolchain", "debug", "setup", "start"]),
    ("Conventions/Testing.md", &["test", "fixture", "gate", "coverage", "suite", "ci ", "assert", "verif"]),
    ("Conventions/Architecture.md", &["architect", "layer", "repo", "module", "package", "dependenc", "boundary", "structure", "import", "calls"]),
    ("Reference/Systems.md", &["system", "service", "mechanic", "pipeline", "subsystem", "lifecycle"]),
    ("Conventions/Registries.md", &["regist", "installer", "wiring", "wire", "route", "plugin", "hook"]),
    ("Conventions/Data-and-Config.md", &["data", "json", "asset", "schema", "folder", "loader", "codec", "resource"]),
    ("Reference/Config-Map.md", &["config", "key", "default", "setting", "option", "propert", "reload", "multiplier"]),
    ("Conventions/Code.md", &["convention", "pattern", "style", "lint", "format", "helper", "util", "comment", "practice", "minimal"]),
    ("Platform/Index.md", &["platform", "engine", "sdk", "upstream", "vendor", "librar", "api ", "version", "pin ", "pinned", "base game"]),
    ("Reference/Vocabulary.md", &["vocabular", "glossar", "term", "word", "rename", "called", "spelling", "jargon", "alias"]),
    (START_HERE, &["gotcha", "pitfall", "trap", "warning", "caveat", "never", "beware", "mistake", "lesson", "careful", "footgun", "avoid"]),
    ("Current/Team.md", &["team", "people", "role", "decid", "review", "approv", "ruling", "contact", "who "]),
    ("Current/Who-Does-What.md", &["owner", "owns", "area", "contributor", "maintain", "ask "]),
    ("Work/Releases.md", &["release", "changelog", "tag", "patch note", "history", "ship"]),
    ("Current/Project.md", &["overview", "about", "goal", "vision", "purpose", "product", "introduction", "scope"]),
    ("Current/Feature-Status.md", &["feature", "status", "implemented", "built", "roadmap", "todo", "shipped", "progress"]),
];

/// How well a part fits each page of [`PAGE_WORDS`]: ten for each key that
/// starts a word of its heading, one for each word of its body a key starts.
fn page_scores(heading: &str, body: &str) -> Vec<(&'static str, usize)> {
    let head = spaced_words(heading);
    let words: Vec<String> = spaced_words(body).split(' ').filter(|w| !w.is_empty()).map(|w| format!(" {w} ")).collect();
    PAGE_WORDS
        .iter()
        .map(|(page, keys)| {
            let in_head = keys.iter().filter(|k| head.contains(&format!(" {k}"))).count();
            let in_body = words.iter().filter(|w| keys.iter().any(|k| w.starts_with(&format!(" {k}")))).count();
            (*page, in_head * 10 + in_body)
        })
        .collect()
}

/// The pages a part may go to, best first: its topic's pages ([`topic_of`]
/// its heading, else its section's heading), ranked by [`page_scores`], the
/// topic's own page on a tie; then every other page that scores, best
/// first, for when those are full; and last START-HERE's traps. A part no
/// topic claims ranks every page by its words, and goes to START-HERE when
/// none scores: on 2026-10-06, 32% of the mod's `CLAUDE.md` (its four repos,
/// where the base game's assets are, the energy word) had a heading no topic
/// claimed and reached no page.
fn rank_pages(s: &Section) -> Vec<&'static str> {
    let topic = match topic_of(&s.heading) {
        Topic::Other if !s.parent.is_empty() => topic_of(&s.parent),
        t => t,
    };
    let scores = page_scores(&s.heading, &s.text);
    let score = |p: &str| scores.iter().find(|(q, _)| *q == p).map_or(0, |(_, n)| *n);
    let own: Vec<&'static str> = TOPIC_PAGES.iter().find(|(t, _)| *t == topic).map(|(_, p)| p.to_vec()).unwrap_or_default();
    let mut ranked: Vec<&'static str> = own.clone();
    ranked.sort_by_key(|p| std::cmp::Reverse(score(p)));
    let mut others: Vec<(&'static str, usize)> = scores.iter().filter(|(p, n)| *n > 0 && !own.contains(p)).copied().collect();
    others.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    ranked.extend(others.into_iter().map(|(p, _)| p));
    if !ranked.contains(&START_HERE) {
        ranked.push(START_HERE);
    }
    ranked
}

/// Pages one part of a brief goes to at most.
const PART_PAGES: usize = 3;

/// Every part of a repo's briefs, sent to every team page it fits, up to
/// [`PART_PAGES`], each within [`PAGE_BUDGET`]: the first of [`rank_pages`]
/// with room left, so every part reaches a page while any has room; then
/// each other page whose words fit it at least half as well as the page it
/// fits best ([`page_scores`]). One fact, one page is the drafting's rule,
/// not the reading's: the page that owns a fact states it and the others
/// link. Sent to one page only, on 2026-10-07 six pages thinned, Systems
/// said no source described the registrations Feature-Status described from
/// the same source, and Team lost who approves the mod's protected paths.
fn route_parts(name: &str, briefs: &[(String, Vec<Section>)]) -> BTreeMap<&'static str, Vec<Source>> {
    let parts: Vec<(&String, &Section)> = briefs.iter().flat_map(|(f, secs)| secs.iter().map(move |s| (f, s))).collect();
    let mut sent: Vec<Vec<&'static str>> = vec![Vec::new(); parts.len()];
    let mut used: HashMap<&'static str, usize> = HashMap::new();
    let room = |page: &'static str, n: usize, used: &mut HashMap<&'static str, usize>| {
        let fits = used.get(page).copied().unwrap_or(0) + n <= PAGE_BUDGET;
        if fits {
            *used.entry(page).or_default() += n;
        }
        fits
    };
    // Each part's best page first, for every part, then the others it
    // fits while room is left: a second page never crowds out a first.
    for (k, (_, s)) in parts.iter().enumerate() {
        if let Some(page) = rank_pages(s).into_iter().find(|p| room(*p, s.text.len(), &mut used)) {
            sent[k].push(page);
        }
    }
    for (k, (_, s)) in parts.iter().enumerate() {
        let scores = page_scores(&s.heading, &s.text);
        let score = |p: &str| scores.iter().find(|(q, _)| *q == p).map_or(0, |(_, n)| *n);
        let best = scores.iter().map(|(_, n)| *n).max().unwrap_or(0);
        for page in rank_pages(s) {
            if sent[k].len() >= PART_PAGES || sent[k].is_empty() {
                break;
            }
            if !sent[k].contains(&page) && score(page) > 0 && score(page) * 2 >= best && room(page, s.text.len(), &mut used) {
                sent[k].push(page);
            }
        }
    }
    let mut out: BTreeMap<&'static str, Vec<Source>> = BTreeMap::new();
    for (k, (file, s)) in parts.iter().enumerate() {
        for page in &sent[k] {
            out.entry(*page).or_default().push(Source { label: part_label(name, file, s.line), text: s.text.clone() });
        }
    }
    out
}

/// The lines of a repo's briefs that tell of a word: a rename, what
/// something is called, a spelling, an alias. For Vocabulary, whichever page
/// their section went to; each line cited by its own line.
fn word_lines(name: &str, briefs: &[(String, Vec<Section>)]) -> Option<String> {
    const SIGNS: &[&str] = &["renamed", "rename", "is called", "are called", "the word", "spelled", "spelling", "a.k.a", "aka ", "alias", "formerly"];
    lines_with(name, briefs, SIGNS)
}

/// The lines of a repo's briefs that name who decides, approves or owns
/// something, whatever section they sit in, for Team and Who Does What: on
/// 2026-10-07 the mod's protected paths, which need Chris's yes, sat in a
/// build section, and Team wrote that all of his decisions were in Tools.
fn decider_lines(name: &str, briefs: &[(String, Vec<Section>)]) -> Option<String> {
    const SIGNS: &[&str] = &[
        "decide", "decider", "decision", "approv", "sign off", "sign-off", "'s yes", "’s yes", "owner", " owns ", "protected", "must ask",
        "permission", "rules on", "ruling", "review",
    ];
    lines_with(name, briefs, SIGNS)
}

/// Each line of a repo's briefs that names one of `signs`, cited by its
/// own line, as `repo:FILE:line: text`.
fn lines_with(name: &str, briefs: &[(String, Vec<Section>)], signs: &[&str]) -> Option<String> {
    let mut out = String::new();
    for (file, sections) in briefs {
        for s in sections {
            for (i, line) in s.text.lines().enumerate() {
                let lower = line.to_lowercase();
                if signs.iter().any(|k| lower.contains(k)) {
                    out.push_str(&format!("{name}:{file}:{}: {}\n", s.line + i, clip(line.trim(), 600)));
                }
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

/// The briefs read by section, in this order, matched without regard to case.
const BRIEFS: &[&str] = &["CLAUDE.md", "AGENTS.md", "README.md", "README", "CONTRIBUTING.md"];

/// Each brief of `root`, split into its sections.
fn briefs(root: &Path) -> Vec<(String, Vec<Section>)> {
    top_files(root, BRIEFS).into_iter().filter_map(|(n, p)| read_plain(&p).map(|t| (n, brief_sections(&t)))).collect()
}

/// The folders whose Markdown is a repo's other docs.
const DOC_DIRS: &[&str] = &["docs", "doc", "documentation"];

/// A repo's other docs (up to ten Markdown files of each of [`DOC_DIRS`]),
/// split by section like a brief, so a team page they fit reads them: on
/// 2026-10-07 Tools' `docs/CODE-STANDARDS.md` reached only the repo pages,
/// and Conventions/Code was four purpose sentences.
fn doc_briefs(root: &Path) -> Vec<(String, Vec<Section>)> {
    let mut out = Vec::new();
    for dir in DOC_DIRS {
        let mut docs: Vec<PathBuf> =
            fs::read_dir(root.join(dir)).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "md")).collect();
        docs.sort();
        for p in docs.into_iter().take(10) {
            let Some(t) = read_plain(&p) else { continue };
            let Some(file) = p.file_name().map(|f| format!("{dir}/{}", f.to_string_lossy())) else { continue };
            out.push((file, brief_sections(&t)));
        }
    }
    out
}

/// Every heading of the briefs with the line it is on: what each brief
/// covers, sent whole, so a page knows where a section it was not given is.
fn brief_outline(briefs: &[(String, Vec<Section>)]) -> String {
    let mut out = String::new();
    for (file, sections) in briefs {
        out.push_str(&format!("{file}:\n"));
        for s in sections.iter().filter(|s| !s.heading.is_empty()) {
            out.push_str(&format!("  {}: {}\n", s.line, s.heading));
        }
    }
    out
}

/// How a part of a file is cited: `repo:FILE:line`, or `repo:FILE` for the
/// part at the top of the file.
fn part_label(name: &str, file: &str, line: usize) -> String {
    if line <= 1 {
        format!("{name}:{file}")
    } else {
        format!("{name}:{file}:{line}")
    }
}

/// A repo's brief sections whose topic `pick` takes, in file order, each
/// labelled by [`part_label`], while `budget` lasts.
fn sections_for(name: &str, briefs: &[(String, Vec<Section>)], pick: impl Fn(Topic) -> bool, budget: usize) -> Vec<Source> {
    parts_for(name, briefs, |s| pick(topic_of(&s.heading)), budget)
}

/// A repo's brief parts that `pick` takes, in file order, each labelled by
/// [`part_label`], while `budget` lasts. A part is whole: one that does not
/// fit is skipped for a later, smaller one. Only a part longer than
/// [`SECTION_MAX`] (one paragraph with no blank line in it) is cut, and
/// marked so.
fn parts_for(name: &str, briefs: &[(String, Vec<Section>)], pick: impl Fn(&Section) -> bool, budget: usize) -> Vec<Source> {
    let mut out = Vec::new();
    let mut used = 0;
    let picked = briefs.iter().flat_map(|(f, secs)| secs.iter().map(move |s| (f, s))).filter(|(_, s)| pick(s));
    for (file, s) in picked {
        if budget.saturating_sub(used) < 200 {
            break;
        }
        let text = if s.text.len() > SECTION_MAX {
            clip(&s.text, SECTION_MAX.min(budget - used))
        } else if used + s.text.len() <= budget {
            s.text.clone()
        } else {
            continue;
        };
        used += text.len();
        out.push(Source { label: part_label(name, file, s.line), text });
    }
    out
}

/// The tables of a TOML file named in `tables` (`[package]`, `[[bin]]`),
/// each with its header line.
fn toml_tables(text: &str, tables: &[&str]) -> String {
    let mut out = String::new();
    let mut on = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            on = tables.contains(&t);
        }
        if on {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// A Makefile's or justfile's targets, each with up to four lines of its
/// recipe: what a person types to build, test or run.
fn make_targets(text: &str) -> String {
    let mut out = String::new();
    let mut recipe_left = 0;
    for line in text.lines() {
        if line.starts_with('\t') || line.starts_with("    ") {
            if recipe_left > 0 {
                out.push_str(line);
                out.push('\n');
                recipe_left -= 1;
            }
            continue;
        }
        recipe_left = 0;
        let t = line.trim_end();
        let Some((head, _)) = t.split_once(':') else { continue };
        if head.trim().is_empty() || t.starts_with(['#', '.', ' ']) || head.contains(['=', '$']) || t.contains(":=") {
            continue;
        }
        out.push_str(t);
        out.push('\n');
        recipe_left = 4;
    }
    out
}

/// A Gradle build file's head (plugins, versions) and every line that
/// registers or configures a task, with its line number: the mod's 35 KB
/// `build.gradle.kts` is mostly task bodies.
fn gradle_summary(text: &str) -> String {
    let head: String = text.lines().take(60).map(|l| format!("{l}\n")).collect();
    let tasks: Vec<String> = text
        .lines()
        .enumerate()
        .skip(60)
        .filter(|(_, l)| {
            let t = l.trim_start();
            ["tasks.register", "tasks.named", "tasks.create", "by registering", "by tasks", "by creating"].iter().any(|k| t.contains(k))
                || t.starts_with("task(")
                || t.starts_with("task ")
        })
        .map(|(i, l)| format!("{}: {}", i + 1, l.trim()))
        .take(80)
        .collect();
    if tasks.is_empty() {
        head
    } else {
        format!("{head}\ntask lines further down:\n{}\n", tasks.join("\n"))
    }
}

/// package.json's name and scripts.
fn package_scripts(text: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let scripts = v.get("scripts")?.as_object()?;
    let mut out = String::new();
    if let Some(n) = v.get("name").and_then(|n| n.as_str()) {
        out.push_str(&format!("name: {n}\n"));
    }
    out.push_str("scripts:\n");
    for (k, c) in scripts {
        out.push_str(&format!("  {k}: {}\n", c.as_str().unwrap_or_default()));
    }
    Some(out)
}

/// What a repo's build files say a person can run, as (file, summary):
/// package.json scripts, the Gradle build and settings, Cargo.toml's
/// `[package]`, `[workspace]` and `[[bin]]`, pyproject's scripts, Makefile
/// and justfile targets, and `.ken/gates.json` (what each gate runs). On
/// 2026-10-06 none was read, and the Tools page said how the tools start was
/// unknown while `package.json` had `"start"`.
fn build_files(root: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let wanted = [
        "package.json",
        "build.gradle",
        "build.gradle.kts",
        "settings.gradle",
        "settings.gradle.kts",
        "Cargo.toml",
        "pyproject.toml",
        "Makefile",
        "GNUmakefile",
        "justfile",
        ".justfile",
    ];
    for (file, path) in top_files(root, &wanted) {
        let Some(text) = read_plain(&path) else { continue };
        let lower = file.to_lowercase();
        let summary = match lower.as_str() {
            "package.json" => package_scripts(&text),
            "build.gradle" | "build.gradle.kts" => Some(gradle_summary(&text)),
            "settings.gradle" | "settings.gradle.kts" => Some(text),
            "cargo.toml" => Some(toml_tables(&text, &["[package]", "[workspace]", "[[bin]]"])),
            "pyproject.toml" => Some(toml_tables(&text, &["[project.scripts]", "[tool.poetry.scripts]"])),
            _ => Some(make_targets(&text)),
        };
        if let Some(s) = summary.filter(|s| !s.trim().is_empty()) {
            out.push((file, s));
        }
    }
    if let Some(t) = read_plain(&root.join(".ken").join("gates.json")) {
        out.push((".ken/gates.json".to_string(), t));
    }
    out
}

/// How a page cites what the checkout says now, beside a doc that may be
/// out of date ([`checkout_sources`]).
pub const REGISTERS: &str = "(what the code registers, from its entry points)";
pub const VALUES: &str = "(values in the checkout: build and data files)";
/// Characters of one of them on a repo's page.
const CHECKOUT_MAX: usize = 16_000;
/// Characters of the values a team page that states values reads from one
/// repo, every data file whole ([`crate::checkout::values`]).
const TEAM_VALUES_MAX: usize = 40_000;

/// What the checkout says now: what the code wires together at start-up
/// ([`crate::checkout::registrations`]) when `wiring`, and the versions the
/// build files pin with the values of the data files the briefs name
/// ([`crate::checkout::values`]), within `values` characters, when it is
/// given. Wiring is never read for a reference repo: its code is the
/// platform's, and large.
fn checkout_sources(
    name: &str,
    root: &Path,
    tracked: &[String],
    briefs: &[(String, Vec<Section>)],
    wiring: bool,
    values: Option<usize>,
) -> Vec<Source> {
    let mut out = Vec::new();
    if wiring {
        if let Some(t) = crate::checkout::registrations(root, tracked) {
            out.push(Source { label: format!("{name}:{REGISTERS}"), text: t });
        }
    }
    if let Some(budget) = values {
        let brief_text: String = briefs.iter().flat_map(|(_, secs)| secs.iter().map(|s| s.text.as_str())).collect();
        if let Some(t) = crate::checkout::values(root, tracked, &brief_text, budget) {
            out.push(Source { label: format!("{name}:{VALUES}"), text: t });
        }
    }
    out
}

/// Whether a source is one of [`checkout_sources`]: given whole or not at
/// all, never cut.
fn is_checkout(label: &str) -> bool {
    label.ends_with(REGISTERS) || label.ends_with(VALUES)
}

/// The release-notes files read per version.
const RELEASE_NOTES: &[&str] = &["CHANGELOG.md", "PATCHNOTES.md", "RELEASES.md", "HISTORY.md", "CHANGELOG"];

/// A repo's release notes, one source per version labelled `repo:FILE:line`,
/// in each file's order (newest first, as changelogs keep them), at most
/// `per_file` versions a file, while `budget` lasts.
fn release_notes(name: &str, root: &Path, per_file: usize, budget: usize) -> Vec<Source> {
    let mut out = Vec::new();
    let push = pusher_of(budget, VERSION_MAX);
    for (file, path) in top_files(root, RELEASE_NOTES) {
        let Some(text) = read_plain(&path) else { continue };
        for s in version_sections(&text).into_iter().take(per_file) {
            push(part_label(name, &file, s.line), s.text, &mut out);
        }
    }
    out
}

/// A repo's `people/` files, its README and the PERSON.md template aside.
fn people_files(name: &str, root: &Path) -> Vec<Source> {
    crate::people::files(root)
        .into_iter()
        .filter_map(|p| {
            let text = read_plain(&p)?;
            Some(Source { label: format!("{name}:people/{}", p.file_name()?.to_string_lossy()), text: clip(&text, 3_000) })
        })
        .collect()
}

/// `source` pushed whole while `budget` characters last in all, else named
/// with its size: a source of [`checkout_sources`] is never cut.
fn push_whole(label: String, text: String, out: &mut Vec<Source>, budget: usize) {
    let used: usize = out.iter().map(|s| s.text.len()).sum();
    let text = if used + text.len() <= budget { text } else { format!("(left out for room: {} characters; never cut)", text.len()) };
    out.push(Source { label, text });
}

/// What one repo's page is made from, in this order, clipped to
/// [`REPO_BUDGET`]: its description; the outline of its briefs; its build
/// files; the brief sections on building, structure and what it is, up to
/// half the budget, ahead of what the checkout says now ([`checkout_sources`],
/// each whole or left out); its layout and import map; INSTALL; its newest
/// release notes; CODEOWNERS, `people/` and who has committed recently; its
/// version tags; up to ten docs; then every other brief section, in order,
/// while the budget lasts. On 2026-10-07 the checkout's sources came first
/// and the brief's sections got what was left. `roster` is the team's
/// ([`team_roster`]), for the recent contributors.
pub fn gather_repo(name: &str, root: &Path, roster: &[crate::people::Person]) -> Vec<Source> {
    let mut out: Vec<Source> = Vec::new();
    let push = pusher(REPO_BUDGET);
    // What a person said this repo is for, at set-up: read first, so the
    // draft knows how to use each source.
    if let Some(d) = crate::registry::description_of(root) {
        push(format!("{name}:(what this repo is for, in the team's words)"), d, &mut out);
    }
    let briefs = briefs(root);
    if !briefs.is_empty() {
        push(format!("{name}:(headings of its briefs, with their lines)"), brief_outline(&briefs), &mut out);
    }
    for (file, summary) in build_files(root) {
        push(format!("{name}:{file}"), clip(&summary, BUILD_MAX), &mut out);
    }
    let wants = [Topic::Build, Topic::Architecture, Topic::Project];
    out.extend(sections_for(name, &briefs, |t| wants.contains(&t), REPO_BUDGET / 2));
    let tracked = ls_files(root).unwrap_or_default();
    let reference = crate::registry::index_of(root).0.contains(&crate::registry::RepoKind::Reference);
    for s in checkout_sources(name, root, &tracked, &briefs, !reference, Some(CHECKOUT_MAX)) {
        push_whole(s.label, s.text, &mut out, REPO_BUDGET);
    }
    // The shape of the repo before its long docs, so a documentation-heavy
    // repo cannot crowd it out of the budget; each whole up to its own budget.
    let push_map = pusher_of(REPO_BUDGET, LAYOUT_BUDGET);
    push_map(format!("{name}:(layout)"), layout(root, &tracked), &mut out);
    // Not for a reference repo: the team reads it and never changes it, and
    // its import map was the slowest part of a draft (1,514 s on a 74,000-file
    // platform source, against 26 s for the team's own mod, 2026-10-05).
    if !reference {
        if let Some(d) = folder_imports(root) {
            push_map(format!("{name}:(imports between folders, from the code)"), d, &mut out);
        }
    }
    for (file, path) in top_files(root, &["INSTALL.md", "INSTALL"]) {
        if let Some(t) = read_plain(&path) {
            push(format!("{name}:{file}"), t, &mut out);
        }
    }
    let used: usize = out.iter().map(|s| s.text.len()).sum();
    out.extend(release_notes(name, root, 3, (REPO_BUDGET - used.min(REPO_BUDGET)).min(3 * VERSION_MAX)));
    for f in ["CODEOWNERS", ".github/CODEOWNERS", "docs/CODEOWNERS"] {
        if let Ok(t) = fs::read_to_string(root.join(f)) {
            push(format!("{name}:{f}"), t, &mut out);
        }
    }
    for s in people_files(name, root) {
        push(s.label, s.text, &mut out);
    }
    if let Some(a) = recent_contributors(root, roster) {
        push(format!("{name}:(recent contributors by folder, last 90 days, from git)"), a, &mut out);
    }
    if let Some(t) = git_tags(root) {
        push(format!("{name}:(version tags, newest first)"), t, &mut out);
    }
    for dir in ["docs", "doc", "documentation"] {
        let mut docs: Vec<PathBuf> = fs::read_dir(root.join(dir))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "md"))
            .collect();
        docs.sort();
        for p in docs.into_iter().take(10) {
            if let Some(t) = read_text(&p) {
                let rel = format!("{dir}/{}", p.file_name().unwrap().to_string_lossy());
                push(format!("{name}:{rel}"), t, &mut out);
            }
        }
    }
    // Everything else the briefs say, in order, while the budget lasts.
    let used: usize = out.iter().map(|s| s.text.len()).sum();
    out.extend(sections_for(name, &briefs, |t| !wants.contains(&t), REPO_BUDGET.saturating_sub(used)));
    out
}

/// The team pages whose words make a brief's part one about a repo's layers.
const LAYER_PAGES: &[&str] = &["Conventions/Architecture.md", "Conventions/Code.md", "Conventions/Registries.md", "Reference/Systems.md"];

/// Whether a part of a brief is about layers, patterns, wiring or the
/// rules of the code: its topic is architecture, or a page of
/// [`LAYER_PAGES`] is among the pages it fits best.
fn about_layers(s: &Section) -> bool {
    let topic = match topic_of(&s.heading) {
        Topic::Other if !s.parent.is_empty() => topic_of(&s.parent),
        t => t,
    };
    if topic == Topic::Architecture {
        return true;
    }
    let scores = page_scores(&s.heading, &s.text);
    let best = scores.iter().map(|(_, n)| *n).max().unwrap_or(0);
    best > 0 && scores.iter().any(|(p, n)| LAYER_PAGES.contains(p) && *n * 2 >= best)
}

/// What a code repo's layers page is made from, within [`REPO_BUDGET`]:
/// its description and the outline of its briefs, then every part of its
/// briefs about layers, patterns, wiring and rules ([`about_layers`]), in
/// order, up to half the budget, then its import map, what its code
/// registers and its layout from `map` (what its Repo Map page read), then
/// its other brief parts while the budget lasts. On 2026-10-07 the mod's
/// architecture page read the brief by budget in file order: its
/// architecture sections start 56,000 characters in, and the page lost 9 of
/// its 11 "may not" rules.
pub fn layers_sources(name: &str, root: &Path, map: &[Source]) -> Vec<Source> {
    let briefs = briefs(root);
    let pick = |what: &str| map.iter().find(|s| s.label == format!("{name}:{what}")).cloned();
    let mut out: Vec<Source> = ["(what this repo is for, in the team's words)", "(headings of its briefs, with their lines)"]
        .iter()
        .filter_map(|w| pick(w))
        .collect();
    out.extend(parts_for(name, &briefs, about_layers, REPO_BUDGET / 2));
    for what in ["(imports between folders, from the code)", REGISTERS, "(layout)"] {
        if let Some(s) = pick(what) {
            push_whole(s.label, s.text, &mut out, REPO_BUDGET);
        }
    }
    let used: usize = out.iter().map(|s| s.text.len()).sum();
    out.extend(parts_for(name, &briefs, |s| !about_layers(s), REPO_BUDGET.saturating_sub(used)));
    out
}

/// Every readable document in a folder a person added (a Confluence
/// export, say), clipped to [`SOURCE_BUDGET`].
pub fn gather_extra(dir: &Path) -> Vec<Source> {
    let mut out: Vec<Source> = Vec::new();
    let push = pusher(SOURCE_BUDGET);
    let label = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "documents".into());
    let walker = ignore::WalkBuilder::new(dir).hidden(true).build().flatten();
    for e in walker.filter(|e| e.path().is_file()).take(200) {
        if let Some(t) = read_text(e.path()) {
            let rel = e.path().strip_prefix(dir).unwrap_or(e.path()).to_string_lossy().replace('\\', "/");
            push(format!("{label}:{rel}"), t, &mut out);
        }
    }
    out
}

/// Every repo's sources, each within its own budget, then `extra`.
pub fn gather(repos: &[(String, PathBuf)], extra: Option<&Path>) -> Vec<Source> {
    let roster = crate::people::roster_of(repos.iter().map(|(_, r)| r.as_path()));
    let mut out: Vec<Source> = repos.iter().flat_map(|(n, r)| gather_repo(n, r, &roster)).collect();
    if let Some(dir) = extra {
        out.extend(gather_extra(dir));
    }
    out
}

// A page Ken may write: missing, or a template nobody has filled in.
pub fn may_draft(existing: Option<&str>) -> bool {
    existing.is_none_or(|t| t.contains("{{"))
}

pub fn prompt(page: &str, purpose: &str, template: Option<&str>, sources: &[Source], today: &str) -> String {
    let mut s = format!(
        "You are drafting one page of a team's wiki, `{page}`, for a person to review. The page is: {purpose}.\n\n\
         Rules:\n\
         - Use only what the sources below say. Where they say nothing, leave the fact out: never write a row, a cell or a field that says something is unknown or not in the sources. Keep a template's table rows only where you have a source for the row's main column, and leave a cell empty when only it has none.\n\
         - End the page with one line, `To fill: …`, naming once what a person should add that the sources do not say. Before you name an item, look for its answer in the page's own frontmatter, in the sources below and in the pages of this wiki: name only what none of them answers, and never the `pin` (Ken writes it). Leave the line out when nothing is missing.\n\
         - Every statement cites its source. Each table row, list item and paragraph ends with the label of the source that says it, in backticks: `repo:FILE:line` at the line that says it when the source has lines (e.g. `ken:README.md:12`), else the source's label (e.g. `ken:README.md`); or a link, `[[Page]]`, when another page of the wiki owns the fact. In a table, the citations go in its source column (`shown in`, `evidence`, `sources`), else at the end of its last cell. A statement with no citation is taken out before the page is published, and every one is checked against what it cites.\n\
         - Write only what the cited source says: never a reason, purpose or consequence it does not give, and never a wider claim (\"all\", \"only\", \"every\", \"none\") than it makes. Give a list or set whole, as the source gives it; where a source gives only part of a set (\"the first 5 of 49\"), say so.\n\
         - Frontmatter: keep the template's keys; set `status: draft`; set `updated: {today}`; do NOT write a `verified:` line (a person verifies it later); list every source you used under `sources:` as its label.\n\
         - Plain words for a reader who has not seen the code. Tables where the template has them.\n\
         - A source labelled `(what this repo is for, in the team's words)` is the team's own description of that repo: use it to know what each repo is and how it is used.\n\
         - A source labelled `repo:FILE:line` is the part of FILE that starts at that line; cite it by that label.\n\
         - A source labelled `…:Repo-Map/<repo>.md` is Ken's own draft of that repo's page, a summary: cite the repo files it cites (its backticked `repo:path` labels), not the page.\n\
         - A fact another page of the wiki owns (the source `(pages of this wiki, and the facts each owns)`) gets a link to that page, `[[Page]]`, not a restatement.\n\
         - The checkout outranks a doc. Sources labelled `{REGISTERS}` and `{VALUES}`, and the build files, say what the code is now; a brief, README or note says what was true when it was written. Write a version, default, count, class, test, command or list of what is registered only as the checkout has it, and cite the file it is in.\n\
         - When a doc and the checkout disagree on something the checkout can answer (a value, a name, a command, a path, a setting), write the checkout's answer and cite it; do not write that the sources disagree. Ask a person to rule only where the checkout cannot settle it, in one line, on the one page that owns the fact.\n\
         - People: write an owner, a role or a decider only where CODEOWNERS, a `people/` file or a repo's own docs name one. Never write a role nobody stated. The recent contributors from git say who has committed lately, nothing more. Never speculate whether two names are one person.\n\
         - Reply with the finished page only, in Markdown, starting with `---`. No preamble, no code fences around it.\n\n"
    );
    match template {
        Some(t) => s.push_str(&format!("TEMPLATE (fill it in, replacing every {{{{…}}}}):\n{t}\n\n")),
        None => s.push_str("There is no template; use frontmatter with title, status, updated and sources, then sections that fit the page.\n\n"),
    }
    s.push_str("SOURCES:\n");
    for src in sources {
        s.push_str(&format!("\n=== {} ===\n{}\n", src.label, src.text));
    }
    s
}

/// What a draft wrote for a fact it had no source for, before it was told
/// to leave such a fact out: 738 of them on 2026-10-06, 138 on one page.
const UNKNOWN: &[&str] = &["(not in the sources yet)", "not in the sources yet", "(not in the sources)"];

/// A line of a drafted page with its unknowns taken out, or None when
/// nothing would be left of it: a table row whose cells after the first are
/// all unknown or empty, or a short line that is mostly an unknown.
fn without_unknowns(line: &str) -> Option<String> {
    if !UNKNOWN.iter().any(|u| line.contains(u)) {
        return Some(line.to_string());
    }
    let strip = |t: &str| UNKNOWN.iter().fold(t.to_string(), |acc, u| acc.replace(u, ""));
    let t = line.trim();
    if t.starts_with('|') {
        let cells: Vec<&str> = t.trim_matches('|').split('|').collect();
        let empty = cells.iter().skip(1).all(|c| strip(c).trim().trim_matches(['.', '`', '*', '-']).trim().is_empty());
        return (!empty).then(|| strip(line));
    }
    let left = strip(t);
    let left = left.trim().trim_start_matches(['-', '*', '+']).trim();
    (left.len() >= 80).then(|| strip(line))
}

/// The reply as a page: fences stripped, `status: draft` forced, any
/// `verified:` line dropped (only a person sets it), and what only says a
/// fact is unknown taken out ([`without_unknowns`]): the prompt says to leave
/// such a fact out, and a model does not always.
pub fn finish(reply: &str) -> Result<String> {
    let mut t = reply.trim();
    if let Some(rest) = t.strip_prefix("```markdown").or_else(|| t.strip_prefix("```md")).or_else(|| t.strip_prefix("```")) {
        t = rest.trim_start().strip_suffix("```").unwrap_or(rest).trim();
    }
    if !t.starts_with("---") {
        return Err(Error::Other("the draft did not start with its frontmatter".into()));
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
        if in_fm && trimmed.starts_with("verified:") {
            continue;
        }
        if in_fm && trimmed.starts_with("status:") {
            out.push_str("status: draft\n");
            has_status = true;
            continue;
        }
        // In the frontmatter an unknown is an empty value: `pin:` is Ken's
        // to fill from the commit it read.
        let line = if in_fm {
            Some(UNKNOWN.iter().fold(line.to_string(), |acc, u| acc.replace(u, "")).trim_end().to_string())
        } else {
            without_unknowns(line)
        };
        if let Some(line) = line {
            out.push_str(&line);
            out.push('\n');
        }
    }
    if !closed {
        return Err(Error::Other("the draft's frontmatter was never closed".into()));
    }
    Ok(out)
}

// --- The paths a drafted page names, checked against the checkout. Seven
// of the eight stale claims of the 2026-10-06 dry run repeated a path a doc
// named and the code no longer had (`utils/CombatCalculator`, `data/*.json`). --

/// File extensions that make a backticked name a file a repo may track.
const FILE_EXTS: &[&str] = &[
    "md", "java", "kt", "kts", "gradle", "rs", "toml", "json", "jsonc", "ts", "tsx", "js", "mjs", "cjs", "jsx", "svelte", "vue",
    "py", "go", "cs", "csproj", "sln", "c", "h", "cpp", "hpp", "yml", "yaml", "xml", "properties", "sh", "ps1", "cmd", "bat",
    "txt", "sql", "html", "css", "scss", "lock", "cfg", "ini", "ui", "lang", "png", "svg", "csv",
];

/// Whether the last segment of `p` ends in a file extension of [`FILE_EXTS`].
fn has_file_ext(p: &str) -> bool {
    let last = p.trim_end_matches('/').rsplit('/').next().unwrap_or(p);
    last.rsplit_once('.').is_some_and(|(stem, ext)| !stem.is_empty() && FILE_EXTS.contains(&ext.to_ascii_lowercase().as_str()))
}

/// Whether a path segment stands for many: it has `*`, `<name>` or `{name}`.
fn is_wild(seg: &str) -> bool {
    seg.contains(['*', '<', '{'])
}

/// `src/x.rs:12` and `src/x.rs:12-30` without their lines.
fn strip_line(p: &str) -> &str {
    match p.rsplit_once(':') {
        Some((head, tail)) if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit() || c == '-') => head,
        _ => p,
    }
}

/// A relative path a repo could hold: not absolute or above the repo, not a
/// command, flag or variable, not cut short with `...`, and nothing a path
/// in prose never has (spaces, quotes, `=`, brackets, backslashes).
fn plain_path(p: &str) -> bool {
    !p.is_empty()
        && !p.starts_with("..")
        && !p.contains("...")
        && !p.contains('…')
        && !p.starts_with(['/', '~', '$', '%', '-', '#', '@', '!', '+'])
        && !p.contains(|c: char| c.is_whitespace() || "\\=()\"',|;&?^[]".contains(c))
        && !p.contains("//")
}

/// One backticked token as a repo path: `(Some(repo), path)` for a
/// `repo:path` (or `repo@sha:path`) citation, its `:line` dropped;
/// `(None, path)` for a token with a `/` or a file extension. None for
/// anything else: a command, a URL, a word.
fn as_path(token: &str, repos: &[&str]) -> Option<(Option<String>, String)> {
    let t = token.trim().trim_end_matches(['.', ',', ';', ')']);
    for r in repos {
        let Some(rest) = t.strip_prefix(r) else { continue };
        let path = if let Some(p) = rest.strip_prefix(':') {
            p
        } else if let Some((_, p)) = rest.strip_prefix('@').and_then(|p| p.split_once(':')) {
            p
        } else {
            continue;
        };
        let path = strip_line(path);
        return (!path.starts_with('(') && plain_path(path)).then(|| (Some(r.to_string()), path.to_string()));
    }
    if t.contains(':') {
        return None;
    }
    let t = t.strip_prefix("./").unwrap_or(t);
    (plain_path(t) && (t.contains('/') || has_file_ext(t))).then(|| (None, t.to_string()))
}

/// A token with `{a,b,c}` in it, once per alternative, every list expanded;
/// as it is when it has none. A brace list is several paths, never a
/// wildcard: `data/mob_stats/t1/{light,medium,heavy}.json` was passed on
/// 2026-10-06 while the files sit one folder deeper. A `{name}` with no comma
/// stands for any one name and stays.
pub fn expand_braces(token: &str) -> Vec<String> {
    let mut from = 0;
    while let Some(open) = token[from..].find('{').map(|i| from + i) {
        let Some(close) = token[open..].find('}').map(|i| open + i) else { break };
        let inner = &token[open + 1..close];
        if inner.contains(',') {
            let (head, tail) = (&token[..open], &token[close + 1..]);
            return inner.split(',').flat_map(|alt| expand_braces(&format!("{head}{}{tail}", alt.trim()))).collect();
        }
        from = close + 1;
    }
    vec![token.to_string()]
}

/// The page's frontmatter list items and the backticked tokens of its body
/// outside code fences, in order, each with whether it is in the frontmatter
/// and the line of the body it is on.
fn page_tokens(page: &str) -> Vec<(bool, usize, String)> {
    let mut out = Vec::new();
    let (mut in_front, mut fence) = (false, false);
    for (i, line) in page.lines().enumerate() {
        let t = line.trim();
        if t == "---" && (i == 0 || in_front) {
            in_front = i == 0;
            continue;
        }
        if in_front {
            if let Some(item) = t.strip_prefix("- ") {
                out.push((true, i, item.trim_matches(['"', '\'']).to_string()));
            }
            continue;
        }
        if t.starts_with("```") || t.starts_with("~~~") {
            fence = !fence;
            continue;
        }
        if !fence {
            out.extend(line.split('`').skip(1).step_by(2).map(|token| (false, i, token.to_string())));
        }
    }
    out
}

/// The repo paths a page names, in order: each `repo:path` of its
/// `sources:`, and each backticked token outside code fences that
/// [`as_path`] reads as a path, a brace list once per path in it
/// ([`expand_braces`]). `repos` are the repo names a citation may start with.
pub fn named_paths(page: &str, repos: &[&str]) -> Vec<(Option<String>, String)> {
    named_paths_at(page, repos).into_iter().map(|(r, p, _)| (r, p)).collect()
}

/// [`named_paths`], each with the line of the page it is first named on.
fn named_paths_at(page: &str, repos: &[&str]) -> Vec<(Option<String>, String, usize)> {
    let mut names: Vec<&str> = repos.to_vec();
    // `Game-Tools` before `Game`, so a longer name is never cut short.
    names.sort_by_key(|n| std::cmp::Reverse(n.len()));
    let mut out: Vec<(Option<String>, String, usize)> = Vec::new();
    for (front, at, token) in page_tokens(page) {
        for t in expand_braces(&token) {
            match as_path(&t, &names) {
                Some((r, p)) if !front || r.is_some() => out.push((r, p, at)),
                _ => {}
            }
        }
    }
    let mut seen = HashSet::new();
    out.retain(|(r, p, _)| seen.insert((r.clone(), p.clone())));
    out
}

/// A path that stands for many by a placeholder: `tickets/SR-NNN.md`,
/// `logs/XXXX.log`.
fn is_placeholder_path(path: &str) -> bool {
    path.split(['/', '-', '_', '.']).any(|seg| seg.len() >= 3 && (seg.chars().all(|c| c == 'N') || seg.chars().all(|c| c == 'X') || seg.chars().all(|c| c == 'n')))
}

/// Whether a line places what it names in a repo Ken did not read: a
/// `../Other-Repo/` path, a vault or another repo named in words, or a
/// repo-like name (`Team-Docs`, `sr-docs`) that is none of `repos`.
fn names_unread_repo(line: &str, repos: &[&str]) -> bool {
    let l = line.to_lowercase();
    let above = line.split(|c: char| c.is_whitespace() || c == '`' || c == '(').any(|t| t.starts_with("../"));
    if above || [" vault", "other repo", "another repo", "docs repo", "team repo"].iter().any(|k| l.contains(k)) {
        return true;
    }
    let repo_like = |t: &str| {
        let t = t.trim_matches(|c: char| !c.is_alphanumeric());
        t.len() >= 4
            && t.contains('-')
            && t.chars().all(|c| c.is_alphanumeric() || c == '-')
            && !repos.iter().any(|r| r.eq_ignore_ascii_case(t) || crate::workspace::member_leaf(r).eq_ignore_ascii_case(t))
            && (t.split('-').filter(|p| p.starts_with(|c: char| c.is_uppercase())).count() >= 2
                || ["docs", "wiki", "vault"].iter().any(|s| t.to_lowercase().ends_with(s)))
    };
    line.split('`').skip(1).step_by(2).any(repo_like) || line.split_whitespace().any(|w| repo_like(w) && w.trim_matches(|c: char| !c.is_alphanumeric()).split('-').count() >= 3)
}

/// One path a page names that no repo tracks.
#[derive(Debug, Clone, PartialEq)]
pub struct MissingPath {
    pub path: String,
    /// The repo the page cites it in, when it says (`repo:path`).
    pub repo: Option<String>,
    /// Where a file or folder of the same name is now, as `repo:path`; at
    /// most five, empty when no repo has one.
    pub now: Vec<String>,
}

/// One version a page writes where a build file pins another of the same
/// line (`0.7.0-pre.3` where `gradle.properties` has `0.7.0-pre.5`).
#[derive(Debug, Clone, PartialEq)]
pub struct StaleVersion {
    pub written: String,
    pub repo: String,
    pub pin: crate::checkout::Pin,
}

/// What the check found in one page: paths no repo has, names written as
/// code that no repo has, and versions a build file pins otherwise.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Checked {
    pub paths: Vec<MissingPath>,
    pub names: Vec<String>,
    pub versions: Vec<StaleVersion>,
    /// What the check cannot judge and left alone, each with why: a
    /// front-matter key, an example name, a name on a line that says it is
    /// gone, a range's bound, a glob, a path in a repo Ken did not read.
    pub skipped: Vec<String>,
    /// Ken's search of the checkout for each of `names`, in its order.
    pub searched: Vec<String>,
}

impl Checked {
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty() && self.names.is_empty() && self.versions.is_empty()
    }
}

/// One repo's tracked files, split into segments.
pub(crate) struct Tracked {
    pub(crate) name: String,
    pub(crate) root: PathBuf,
    pub(crate) files: Vec<Vec<String>>,
    /// Every folder name anywhere in the repo.
    folders: HashSet<String>,
    /// The names its code defines and uses ([`crate::checkout::names`]).
    names: crate::checkout::Names,
    /// The versions its build files pin.
    pins: Vec<crate::checkout::Pin>,
    /// Whether its code was read for names; a shallow one is searched.
    pub(crate) deep: bool,
    /// Whether it is the team's library, the wiki itself: a page may name
    /// its paths, but a path is never rewritten to one of them.
    pub(crate) library: bool,
}

/// Whether segment `s` matches pattern segment `pat`, where `*`, `<name>`
/// and `{name}` stand for any run of characters.
fn seg_match(pat: &str, s: &str) -> bool {
    let mut p = String::new();
    let mut skip: Option<char> = None;
    for c in pat.chars() {
        match (skip, c) {
            (Some(end), c) if c == end => skip = None,
            (Some(_), _) => {}
            (None, '<') => {
                skip = Some('>');
                p.push('*');
            }
            (None, '{') => {
                skip = Some('}');
                p.push('*');
            }
            (None, c) => p.push(c),
        }
    }
    let parts: Vec<&str> = p.split('*').collect();
    if parts.len() == 1 {
        return p == s;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !s.starts_with(first) || s.len() < first.len() + last.len() || !s.ends_with(last) {
        return false;
    }
    let mut rest = &s[first.len()..s.len() - last.len()];
    for part in &parts[1..parts.len() - 1] {
        match rest.find(part) {
            Some(j) => rest = &rest[j + part.len()..],
            None => return false,
        }
    }
    true
}

/// A file name without its extension: what a doc calls a class or module.
fn stem(name: &str) -> &str {
    name.rsplit_once('.').map_or(name, |(s, _)| s)
}

impl Tracked {
    /// Whether the repo has `path`: a tracked file that is it or ends with
    /// `/` and it (docs often name a path from a source root), or, for a
    /// path with no extension, a folder whose path is it or ends so, or a
    /// file that is it once its extension is dropped
    /// (`systems/combat/SRDamageFilterSystem` is the `.java` file).
    fn has(&self, path: &str, is_file: bool) -> bool {
        let pat: Vec<&str> = path.trim_end_matches('/').split('/').collect();
        let tail_matches = |f: &[String], last: &dyn Fn(&str) -> &str| {
            f.len() >= pat.len()
                && f[f.len() - pat.len()..].iter().zip(&pat).enumerate().all(|(i, (s, p))| {
                    let s = if i + 1 == pat.len() { last(s) } else { s.as_str() };
                    seg_match(p, s)
                })
        };
        self.files.iter().any(|f| {
            if is_file {
                tail_matches(f, &|s| s)
            } else {
                f[..f.len() - 1].windows(pat.len()).any(|w| w.iter().zip(&pat).all(|(s, p)| seg_match(p, s)))
                    || (!path.ends_with('/') && tail_matches(f, &stem))
            }
        })
    }

    /// Whether `path` is in the checkout though git does not track it: on
    /// disk, or a path git ignores (a build output, a local file).
    fn untracked(&self, path: &str) -> bool {
        if path.split('/').any(is_wild) {
            return false;
        }
        if self.root.join(path).exists() {
            return true;
        }
        let mut cmd = Command::new("git");
        crate::proc::quiet(&mut cmd)
            .args(["check-ignore", "-q", "--no-index", "--", path])
            .current_dir(&self.root)
            .status()
            .is_ok_and(|s| s.success())
    }

    /// Where the repo has a file (or, for a folder or a pattern, a folder)
    /// named as the last plain segment of `path`: at most five paths.
    fn lookup(&self, path: &str, is_file: bool) -> Vec<String> {
        let segs: Vec<&str> = path.trim_end_matches('/').split('/').collect();
        let (name, as_file) = match segs.last() {
            Some(l) if !is_wild(l) => (*l, is_file),
            _ => match segs.iter().rev().skip(1).find(|s| !is_wild(s)) {
                Some(s) => (*s, false),
                None => return Vec::new(),
            },
        };
        let mut found: BTreeSet<String> = BTreeSet::new();
        for f in &self.files {
            let last = f.last().map(String::as_str).unwrap_or_default();
            if last == name || (!as_file && !path.ends_with('/') && stem(last) == name && stem(last) != last) {
                found.insert(f.join("/"));
            }
            if !as_file {
                for i in (0..f.len() - 1).filter(|i| f[*i] == name) {
                    found.insert(format!("{}/", f[..=i].join("/")));
                }
            }
        }
        found.into_iter().take(5).collect()
    }
}

/// The files each of the team's repos tracks, the names its code defines
/// and uses, and the versions its build files pin: what a drafted page is
/// checked against.
#[derive(Default)]
pub struct PathCheck {
    pub(crate) repos: Vec<Tracked>,
    /// The commit each repo was read at.
    pub(crate) heads: HashMap<String, String>,
    /// What `git grep` found for a needle in a repo, for the run.
    #[allow(clippy::type_complexity)]
    pub(crate) greps: std::cell::RefCell<HashMap<String, (usize, Vec<String>)>>,
}

impl PathCheck {
    /// Read from the checkouts: a reference repo's names are its files' and
    /// folders' only, since its code is the platform's and large.
    pub fn of(repos: &[(String, PathBuf)]) -> PathCheck {
        let tracked = repos
            .iter()
            .filter_map(|(name, root)| {
                let tracked = ls_files(root)?;
                let files: Vec<Vec<String>> = tracked.iter().map(|f| f.split('/').map(str::to_string).collect()).collect();
                let folders = files.iter().flat_map(|f| f[..f.len() - 1].iter().cloned()).collect();
                let deep = !crate::registry::kind_of(root).contains(&crate::registry::RepoKind::Reference);
                let names = crate::checkout::names(root, &tracked, deep);
                let pins = crate::checkout::pins(root, &tracked);
                Some(Tracked { name: name.clone(), root: root.clone(), files, folders, names, pins, deep, library: false })
            })
            .collect();
        PathCheck { repos: tracked, heads: repo_heads(repos), ..Default::default() }
    }

    /// The check with the team's library added for paths alone: a page may
    /// name the wiki's own `Templates/` or `decisions/`, which no code repo has.
    pub fn with_library(mut self, name: &str, root: &Path) -> PathCheck {
        let Some(tracked) = ls_files(root) else { return self };
        let files: Vec<Vec<String>> = tracked.iter().map(|f| f.split('/').map(str::to_string).collect()).collect();
        let folders = files.iter().flat_map(|f| f[..f.len() - 1].iter().cloned()).collect();
        let names = crate::checkout::names(root, &tracked, false);
        self.repos.push(Tracked {
            name: name.to_string(),
            root: root.to_path_buf(),
            files,
            folders,
            names,
            pins: Vec::new(),
            deep: true,
            library: true,
        });
        self
    }

    /// Where `git grep -F` finds `needle` in the team's code repos (the
    /// library and a reference repo left out), as `repo:path:line: text`:
    /// at most `max` lines, and how many there were in all.
    pub(crate) fn search(&self, needle: &str, max: usize) -> (usize, Vec<String>) {
        let mut count = 0;
        let mut out = Vec::new();
        for t in self.repos.iter().filter(|t| t.deep && !t.library) {
            let key = format!("{}\0false\0{needle}", t.name);
            let hit = self.greps.borrow().get(&key).cloned();
            let (n, lines) = hit.unwrap_or_else(|| {
                let found = grep_repo(&t.name, &t.root, needle, false);
                self.greps.borrow_mut().insert(key, found.clone());
                found
            });
            count += n;
            out.extend(lines);
        }
        out.truncate(max);
        (count, out)
    }

    /// The paths `page` names that no repo has, each with where a file or
    /// folder of its name is now. A cited path is looked for in the repo it
    /// cites, any other in every repo, `home` (the repo a Repo Map page is
    /// about) first. A path with no file extension is checked only when its
    /// first segment is a folder the repo has somewhere, so `owner/repo` and
    /// branch names are left alone.
    pub fn missing(&self, page: &str, home: Option<&str>) -> Vec<MissingPath> {
        self.paths_checked(page, home).0
    }

    /// [`PathCheck::missing`], and the paths left unchecked with why: a
    /// placeholder (`tickets/SR-NNN.md`), and a bare path on a line that
    /// places it in a repo Ken did not read (the docs vault's
    /// `_meta/ticketcheck.py`, 2026-10-07). A missing path's match is never
    /// the wiki's own page: the brief's `sr-docs/Engine/Reference/Vocabulary.md`
    /// became the wiki's `Reference/Vocabulary.md`, and the page pointed at itself.
    fn paths_checked(&self, page: &str, home: Option<&str>) -> (Vec<MissingPath>, Vec<String>) {
        if self.repos.is_empty() {
            return (Vec::new(), Vec::new());
        }
        let names: Vec<&str> = self.repos.iter().map(|r| r.name.as_str()).collect();
        let lines: Vec<&str> = page.lines().collect();
        let mut skipped: Vec<String> = Vec::new();
        let mut out = Vec::new();
        for (repo, path, at) in named_paths_at(page, &names) {
            let why = if is_placeholder_path(&path) {
                Some("a pattern")
            } else if repo.is_none() && names_unread_repo(lines.get(at).copied().unwrap_or_default(), &names) {
                Some("in a repo Ken did not read")
            } else {
                None
            };
            if let Some(why) = why {
                let line = format!("`{path}` ({why})");
                if !skipped.contains(&line) {
                    skipped.push(line);
                }
                continue;
            }
            let mut scope: Vec<&Tracked> = self.repos.iter().filter(|t| repo.as_ref().is_none_or(|r| *r == t.name)).collect();
            scope.sort_by_key(|t| Some(t.name.as_str()) != home);
            let is_file = has_file_ext(&path);
            if !is_file {
                let first = path.split('/').next().unwrap_or("");
                if !path.trim_end_matches('/').contains('/') || is_wild(first) || !scope.iter().any(|t| t.folders.contains(first)) {
                    continue;
                }
            }
            if scope.iter().any(|t| t.has(&path, is_file) || t.untracked(&path)) {
                continue;
            }
            let mut now: Vec<String> = scope
                .iter()
                .filter(|t| !t.library)
                .flat_map(|t| t.lookup(&path, is_file).into_iter().map(move |p| format!("{}:{p}", t.name)))
                .collect();
            // A test fixture's copy is never what a page means when the real
            // file is there too.
            if now.iter().any(|p| !crate::checkout::is_test_path(p)) {
                now.retain(|p| !crate::checkout::is_test_path(p));
            }
            now.truncate(5);
            if !out.iter().any(|m: &MissingPath| m.path == path && m.repo == repo) {
                out.push(MissingPath { path, repo, now });
            }
        }
        (out, skipped)
    }

    /// The names `page` writes as code ([`crate::checkout::as_symbol`]) that
    /// no repo has: a test no repo's code map defines and no file is named
    /// after, or any other name (a class, a method, a constant, a key) that
    /// no repo defines or uses in its code, data or build files. A doc keeps
    /// naming a test after the test is gone, and a comment may still say its
    /// name: `OneNameLineComposerTest` and `InventoryIsNotPackTest` were
    /// drafted from the mod's brief on 2026-10-06, after the test audit had
    /// removed both.
    pub fn unknown_names(&self, page: &str) -> Vec<String> {
        self.names_checked(page).0
    }

    /// [`PathCheck::unknown_names`], and what was left unchecked with why.
    /// A name is known whole or inside a longer identifier
    /// ([`crate::checkout::Names::has`]); never judged: a key of the page's
    /// own front matter, a name a brief gives as an example (`MyComponent`,
    /// `myConfig`, `FooBar`), and a name on a line that says it does not
    /// exist (2026-10-07: `changed_by`, `MyComponent` and `stopServer` each
    /// cost a correction call).
    fn names_checked(&self, page: &str) -> (Vec<String>, Vec<String>) {
        if self.repos.is_empty() {
            return (Vec::new(), Vec::new());
        }
        let repos: Vec<&str> = self.repos.iter().map(|r| r.name.as_str()).collect();
        let keys = front_keys(page);
        let lines: Vec<&str> = page.lines().collect();
        let mut out: Vec<String> = Vec::new();
        let mut skipped: Vec<String> = Vec::new();
        let skip = |name: &str, why: &str, skipped: &mut Vec<String>| {
            let line = format!("`{name}` ({why})");
            if !skipped.contains(&line) {
                skipped.push(line);
            }
        };
        for (front, at, token) in page_tokens(page) {
            if front || as_path(&token, &repos).is_some() {
                continue;
            }
            let Some((name, is_class)) = crate::checkout::as_symbol(&token) else { continue };
            if keys.contains(&name) {
                skip(&name, "a key of the page's front matter", &mut skipped);
                continue;
            }
            if is_example_name(&name) {
                skip(&name, "an example name", &mut skipped);
                continue;
            }
            if says_absent(lines.get(at).copied().unwrap_or_default()) {
                skip(&name, "on a line that says it does not exist", &mut skipped);
                continue;
            }
            let test = is_class && ["Test", "Tests", "Spec", "IT"].iter().any(|s| name.ends_with(s));
            let known = self.repos.iter().any(|t| t.names.defined.contains(&name) || (!test && t.names.has(&name)));
            if !known && !out.contains(&name) {
                out.push(name);
            }
        }
        // A repo read shallow (a platform the team only reads) is searched
        // for what is left, once: its code is too large to map up front.
        for t in self.repos.iter().filter(|t| !t.deep) {
            let open: Vec<&String> = out.iter().filter(|n| !n.ends_with("Test")).collect();
            if open.is_empty() {
                break;
            }
            let mut args: Vec<String> = ["grep", "-o", "-h", "-w", "-F", "-I"].iter().map(|a| a.to_string()).collect();
            for n in open {
                args.push("-e".into());
                args.push(n.clone());
            }
            args.extend(["--".to_string(), ":!*.md".to_string()]);
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            if let Some(hits) = git_text(&t.root, &args) {
                let hits: HashSet<&str> = hits.lines().map(str::trim).collect();
                out.retain(|n| !hits.contains(n.as_str()));
            }
        }
        (out, skipped)
    }

    /// The versions `page` states that a build file of some repo pins
    /// otherwise: a version of the same line as a pinned one (`0.7.0-pre.3`
    /// against `0.7.0-pre.5`), or any version on a line that names a pinned
    /// key (`hytale_version`), where the page's is none of the pinned ones.
    /// Six pages carried the engine pin a dated note gave, `pre.3`, while
    /// `gradle.properties` said `pre.5` (2026-10-06).
    pub fn stale_versions(&self, page: &str) -> Vec<StaleVersion> {
        self.versions_checked(page).0
    }

    /// [`PathCheck::stale_versions`], and the versions left unchecked: a
    /// range's bound (`<0.8.0` in `>=0.7.0-pre.1 <0.8.0`) is no pin, and
    /// cost a correction call on 2026-10-07.
    fn versions_checked(&self, page: &str) -> (Vec<StaleVersion>, Vec<String>) {
        let pinned: HashSet<&str> = self.repos.iter().flat_map(|t| t.pins.iter().map(|p| p.value.as_str())).collect();
        let mut out: Vec<StaleVersion> = Vec::new();
        let mut skipped: Vec<String> = Vec::new();
        let (mut in_front, mut fence) = (false, false);
        for (i, line) in page.lines().enumerate() {
            let t = line.trim();
            if t == "---" && (i == 0 || in_front) {
                in_front = i == 0;
                continue;
            }
            if t.starts_with("```") || t.starts_with("~~~") {
                fence = !fence;
            }
            if in_front || fence {
                continue;
            }
            for written in crate::checkout::versions_in(line) {
                if pinned.contains(written.as_str()) {
                    continue;
                }
                if range_bound(line, &written) {
                    let why = format!("`{written}` (a range's bound)");
                    if !skipped.contains(&why) {
                        skipped.push(why);
                    }
                    continue;
                }
                let core = crate::checkout::version_core(&written);
                let pre = written.contains(['-', '+']);
                for repo in &self.repos {
                    let named_key = |key: &str| {
                        let leaf = key.rsplit('.').next().unwrap_or(key);
                        leaf != "version" && leaf.len() >= 4 && line.contains(key)
                    };
                    let hit = repo.pins.iter().find(|p| {
                        let same_line = crate::checkout::version_core(&p.value) == core && p.value.contains(['-', '+']) == pre;
                        same_line || named_key(&p.key)
                    });
                    if let Some(p) = hit {
                        if !out.iter().any(|s| s.written == written && s.pin == *p) {
                            out.push(StaleVersion { written: written.clone(), repo: repo.name.clone(), pin: p.clone() });
                        }
                    }
                }
            }
        }
        (out, skipped)
    }

    /// Everything [`PathCheck`] finds in the page at `page_path`. A page of
    /// history ([`is_history`]) names old versions on purpose, so its
    /// versions are not checked.
    pub fn check(&self, page_path: &str, page: &str, home: Option<&str>) -> Checked {
        let (versions, mut skipped) = if is_history(page_path) { (Vec::new(), Vec::new()) } else { self.versions_checked(page) };
        let (paths, path_skips) = self.paths_checked(page, home);
        let (names, name_skips) = self.names_checked(page);
        skipped.extend(path_skips);
        skipped.extend(name_skips);
        let searched = names.iter().map(|n| search_line(self, n)).collect();
        Checked { paths, names, versions, skipped, searched }
    }
}

/// The keys of a page's front matter: `changed_by`, `pin`, `sources`.
fn front_keys(page: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut lines = page.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return out;
    }
    for l in lines.take_while(|l| l.trim_end() != "---") {
        if l.starts_with([' ', '-', '\t']) {
            continue;
        }
        if let Some((k, _)) = l.split_once(':') {
            out.insert(k.trim().to_string());
        }
    }
    out
}

/// A name a brief or a template gives as an example, not the team's code:
/// `MyComponent`, `myConfig`, `FooBar`, `ExampleService`, `YourType`.
fn is_example_name(name: &str) -> bool {
    let parts = crate::checkout::ident_parts(name);
    parts.first().is_some_and(|p| ["my", "your", "foo", "bar", "baz", "example", "sample", "some"].contains(&p.as_str()))
}

/// Whether a line says something is gone, removed or missing: the kind of
/// line a correction wrote on 2026-10-07 for `kill_stats`, which
/// `reset_kill_stats` still registered.
pub(crate) fn says_gone(text: &str) -> bool {
    let l = text.to_lowercase();
    [
        "is gone", "are gone", "no longer exist", "no longer in", "does not exist", "doesn't exist", "do not exist", "don't exist",
        "no repo has", "was removed", "were removed", "has been removed", "have been removed", "was deleted", "were deleted",
        "no longer has",
    ]
    .iter()
    .any(|k| l.contains(k))
}

/// Whether a line says the thing it names does not exist: "there is no
/// `stopServer` task", "no such command", "is gone".
fn says_absent(line: &str) -> bool {
    let l = line.to_lowercase();
    says_gone(line) || ["there is no ", "there are no ", "no such ", "not a task", "never existed"].iter().any(|k| l.contains(k))
}

/// Whether `version` stands in `line` as a range's bound: right after `<`,
/// `>`, `=`, `^` or `~` (`>=0.7.0-pre.1 <0.8.0`).
fn range_bound(line: &str, version: &str) -> bool {
    let mut from = 0;
    while let Some(i) = line[from..].find(version).map(|i| from + i) {
        let before = line[..i].trim_end_matches(['v', 'V']).trim_end();
        if before.ends_with(['<', '>', '=', '^', '~']) {
            return true;
        }
        from = i + version.len();
    }
    false
}

/// The one call that corrects a drafted page: each path the checkout lacks
/// with where the code has it now or that no repo does, each name written as
/// code that no repo has, and each version a build file pins otherwise.
pub fn correction_prompt(page_path: &str, page: &str, found: &Checked) -> String {
    let mut s = format!(
        "You drafted `{page_path}`, one page of a team's wiki. Ken checked the page against the checkout of every repo \
         of the team, and found these:\n\n"
    );
    for m in &found.paths {
        let cited = m.repo.as_deref().map(|r| format!(" (cited in {r})")).unwrap_or_default();
        if m.now.is_empty() {
            s.push_str(&format!("- path `{}`{cited}: no repo of the team has a file or folder of that name\n", m.path));
        } else {
            let at: Vec<String> = m.now.iter().map(|p| format!("`{p}`")).collect();
            s.push_str(&format!("- path `{}`{cited}: not there; a file or folder of that name is at {}\n", m.path, at.join(", ")));
        }
    }
    for (n, search) in found.names.iter().zip(found.searched.iter().map(Some).chain(std::iter::repeat(None))) {
        s.push_str(&format!("- name `{n}`: written as code, and no repo defines it, has a file of that name or uses it"));
        if let Some(search) = search {
            s.push_str(&format!("; {search}"));
        }
        s.push('\n');
    }
    for v in &found.versions {
        s.push_str(&format!("- version `{}`: `{}:{}` has `{} = {}`\n", v.written, v.repo, v.pin.file, v.pin.key, v.pin.value));
    }
    s.push_str(
        "\nCorrect the page: write each path where the code has it now, choosing by what the page says about it; a \
         name the code no longer has, leave out, and say it is gone only when Ken's search above found no hits; a \
         version, write as the build file has it and cite the file. Kept as they are: a path the sources place outside \
         these repos (another repo, the docs vault), a \
         file the sources say something makes (a build, the server at run time, a person), a name that is a product, \
         a tool, an example or a word rather than the team's code, and a version the page gives as history. Change nothing else. \
         Reply with the whole corrected page, starting with `---`. No preamble, no code fences around it.\n\nTHE PAGE:\n",
    );
    s.push_str(page);
    s
}

/// Ken's search of the checkout for `name`, in words for a prompt and a log:
/// `git grep -F` over the team's code repos, docs left out.
fn search_line(check: &PathCheck, name: &str) -> String {
    let repos: Vec<&str> = check.repos.iter().filter(|t| t.deep && !t.library).map(|t| t.name.as_str()).collect();
    let (n, hits) = check.search(name, 3);
    if n == 0 {
        format!("Ken's search (`git grep -F {name}` over {}, docs left out): no hits", repos.join(", "))
    } else {
        format!("Ken's search (`git grep -F {name}` over {}): {n} lines, e.g. {}", repos.join(", "), hits.join(" · "))
    }
}

/// `fixed`, a corrected page, with every line the correction made say that
/// something is gone put back as `before` had it, unless Ken's search of
/// the checkout finds nothing for each name the line says is gone: a row by
/// its first cell, another line dropped. On 2026-10-07 a correction wrote
/// that `kill_stats` "is gone: no repo has it now" while
/// `reset_kill_stats` registered it. Each line put back or dropped, with
/// the search, as (before, after, search).
fn guard_absences(before: &str, fixed: &str, check: &PathCheck) -> (String, Vec<(String, String, String)>) {
    let old: HashSet<&str> = before.lines().collect();
    let repos: Vec<&str> = check.repos.iter().map(|t| t.name.as_str()).collect();
    let mut out: Vec<String> = Vec::new();
    let mut undone = Vec::new();
    for line in fixed.lines() {
        if old.contains(line) || !says_gone(line) {
            out.push(line.to_string());
            continue;
        }
        let names: Vec<String> = line
            .split('`')
            .skip(1)
            .step_by(2)
            .filter(|t| as_path(t, &repos).is_none())
            .filter_map(|t| crate::checkout::as_symbol(t).map(|(n, _)| n).or_else(|| (t.len() >= 4 && !t.contains(' ')).then(|| t.to_string())))
            .collect();
        let found: Vec<String> = names.iter().filter(|n| check.search(n, 1).0 > 0).map(|n| search_line(check, n)).collect();
        if found.is_empty() {
            out.push(line.to_string());
            continue;
        }
        let first_cell = |l: &str| l.trim().trim_matches('|').split('|').next().map(|c| c.trim().to_string());
        let original = line
            .trim()
            .starts_with('|')
            .then(|| before.lines().find(|o| o.trim().starts_with('|') && first_cell(o) == first_cell(line)))
            .flatten();
        match original {
            Some(o) => {
                out.push(o.to_string());
                undone.push((line.to_string(), o.to_string(), found.join("; ")));
            }
            None => undone.push((line.to_string(), String::new(), found.join("; "))),
        }
    }
    let mut text = out.join("\n");
    if fixed.ends_with('\n') {
        text.push('\n');
    }
    (text, undone)
}

/// `page` with each backticked token and frontmatter item that `f` maps
/// written as what it maps to.
fn map_tokens(page: &str, f: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(page.len());
    let (mut in_front, mut fence) = (false, false);
    for (i, line) in page.split_inclusive('\n').enumerate() {
        let t = line.trim();
        if t == "---" && (i == 0 || in_front) {
            in_front = i == 0;
            out.push_str(line);
            continue;
        }
        if t.starts_with("```") || t.starts_with("~~~") {
            fence = !fence;
        }
        if in_front {
            let item = t.strip_prefix("- ").map(|x| x.trim_matches(['"', '\'']));
            match item.and_then(&f) {
                Some(new) => out.push_str(&line.replacen(item.unwrap_or_default(), &new, 1)),
                None => out.push_str(line),
            }
            continue;
        }
        if fence {
            out.push_str(line);
            continue;
        }
        for (k, part) in line.split('`').enumerate() {
            if k > 0 {
                out.push('`');
            }
            match f(part).filter(|_| k % 2 == 1) {
                Some(new) => out.push_str(&new),
                None => out.push_str(part),
            }
        }
    }
    out
}

/// `page` with each missing path that the checkout has exactly one file or
/// folder of that name for written as that one, in backticks and in the
/// frontmatter, bare or cited: there is nothing for a model to choose, so no
/// call is spent on it. A pattern, a cited line (`repo:path:12`, the line of
/// another file) and a path from a brace list are left to the call. Each
/// rewrite as (from, to).
fn rewrite_single_matches(page: &str, missing: &[MissingPath]) -> (String, Vec<(String, String)>) {
    let mut out = page.to_string();
    let mut done = Vec::new();
    for m in missing {
        let [only] = m.now.as_slice() else { continue };
        let Some((repo, to)) = only.split_once(':') else { continue };
        // A file stays a file; a folder or a class named without its
        // extension may become either.
        if m.path.split('/').any(is_wild) || (has_file_ext(&m.path) && to.ends_with('/')) {
            continue;
        }
        let cited = format!("{repo}:{}", m.path);
        let next = map_tokens(&out, |t| {
            let t = t.trim();
            if t == m.path {
                Some(to.to_string())
            } else if t == cited {
                Some(format!("{repo}:{to}"))
            } else {
                None
            }
        });
        if next != out {
            out = next;
            done.push((m.path.clone(), only.clone()));
        }
    }
    (out, done)
}

/// `text`, a page just drafted, checked against the checkout ([`PathCheck`]).
/// A missing path the checkout has one match for is rewritten in place
/// ([`rewrite_single_matches`]); for the rest, and every unknown name and
/// stale version, ONE call to the model lists them and takes the corrected
/// page back. Never a second call: what is still wrong after it is for the
/// person reading the draft. The page as it stood when the call fails. Each
/// rewrite and each call is logged in `report.corrected`.
fn check_paths(
    page_path: &str,
    text: String,
    check: &PathCheck,
    report: &mut DraftReport,
    generate: &mut impl FnMut(&str) -> Result<String>,
) -> String {
    let home = check.repos.iter().find(|t| repo_page(&t.name) == page_path).map(|t| t.name.as_str());
    let missing = check.missing(&text, home);
    let (text, rewritten) = rewrite_single_matches(&text, &missing);
    for (from, to) in &rewritten {
        report.corrected.push(format!("{page_path}: `{from}` -> `{to}` (rewritten, one match)"));
    }
    let found = check.check(page_path, &text, home);
    if !found.skipped.is_empty() {
        report.corrected.push(format!("{page_path}: not checked: {}", found.skipped.join(", ")));
    }
    if found.is_empty() {
        return text;
    }
    let listed = found_list(&found);
    match generate(&correction_prompt(page_path, &text, &found)).and_then(|r| finish(&r)) {
        Ok(fixed) => {
            report.corrected.push(format!("{page_path}: sent back once for {listed}"));
            let (fixed, undone) = guard_absences(&text, &fixed, check);
            for (said, back, search) in undone {
                let now = if back.is_empty() { "dropped".to_string() } else { format!("put back as `{back}`") };
                report.corrected.push(format!("{page_path}: the correction wrote `{said}`; {now}, since {search}"));
            }
            fixed
        }
        Err(e) => {
            report.corrected.push(format!("{page_path}: the call to correct {listed} failed: {e}"));
            text
        }
    }
}

/// Whether a page is history (release notes, incidents): what was true then.
fn is_history(page_path: &str) -> bool {
    page_path.starts_with("Work/")
}

/// What a check found, in one line for the run's log.
pub fn found_list(found: &Checked) -> String {
    let quoted = |v: Vec<String>| v.iter().map(|x| format!("`{x}`")).collect::<Vec<_>>().join(", ");
    let mut parts: Vec<String> = Vec::new();
    if !found.paths.is_empty() {
        parts.push(format!("paths {}", quoted(found.paths.iter().map(|m| m.path.clone()).collect())));
    }
    if !found.names.is_empty() {
        parts.push(format!("names {}", quoted(found.names.clone())));
    }
    if !found.versions.is_empty() {
        let v: Vec<String> = found
            .versions
            .iter()
            .map(|v| format!("`{}` ({}:{} {} = {})", v.written, v.repo, v.pin.file, v.pin.key, v.pin.value))
            .collect();
        parts.push(format!("versions {}", v.join(", ")));
    }
    parts.join("; ")
}

/// What a draft run did.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftReport {
    pub drafted: Vec<String>,
    /// Pages a person already wrote, left alone.
    pub kept: Vec<String>,
    pub failed: Vec<(String, String)>,
    pub sources: Vec<String>,
    /// Changes to pages a person keeps, filed for them to apply or discard.
    #[serde(default)]
    pub proposed: Vec<String>,
    /// Placeholders left in the wiki after the draft, as `page: placeholder`.
    #[serde(default)]
    pub to_fill: Vec<String>,
    /// What the check against the checkout changed, one line each: a path
    /// rewritten to its one match, or a page sent back once with the paths,
    /// names and versions listed ([`check_paths`]).
    #[serde(default)]
    pub corrected: Vec<String>,
    /// What the check of each page against its sources found, one line a
    /// page ([`crate::wikiverify::check`]).
    #[serde(default)]
    pub verified: Vec<String>,
    /// Every statement that check changed or removed, and every `To fill:`
    /// item it answered, with before, after and evidence.
    #[serde(default)]
    pub changes: Vec<crate::wikiverify::Change>,
}

/// The template placeholders still in `wiki`'s pages, as `page: placeholder`.
/// A `verified:` date is a person's to set, and `Templates/` are copied from,
/// so neither counts.
pub fn placeholders_left(wiki: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let pages = crate::wikinew::TEMPLATE
        .iter()
        .map(|(p, _)| *p)
        .filter(|p| p.ends_with(".md") && !p.starts_with("Templates/"));
    for page in pages {
        let Ok(text) = fs::read_to_string(wiki.join(page)) else { continue };
        for line in text.lines().filter(|l| !l.trim_start().starts_with("verified:")) {
            let mut rest = line;
            while let Some(i) = rest.find("{{") {
                let Some(j) = rest[i..].find("}}") else { break };
                out.push(format!("{page}: {}", &rest[i..i + j + 2]));
                rest = &rest[i + j + 2..];
            }
        }
    }
    out.dedup();
    out
}

fn write_page(path: &Path, text: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
    }
    fs::write(path, text).map_err(|e| Error::io(path, e))
}

/// Each git repo's head on the branch drift measures, by name: what a draft
/// read, so its citations can say so.
fn repo_heads(repos: &[(String, PathBuf)]) -> HashMap<String, String> {
    repos
        .iter()
        .filter_map(|(name, root)| {
            // The commit the draft read: what is checked out, whichever branch.
            let mut cmd = std::process::Command::new("git");
            let out = crate::proc::quiet(&mut cmd).args(["rev-parse", "--short=12", "HEAD"]).current_dir(root).output().ok()?;
            let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
            (out.status.success() && !sha.is_empty()).then(|| (name.clone(), sha))
        })
        .collect()
}

/// Pin each code citation in a page's `sources:` to the commit its repo
/// was read at (`repo:path` → `repo@sha:path`). A draft dated today would
/// otherwise take in every commit made later that same day, and a change
/// right after drafting would never read as drift. A frontmatter `pin:` the
/// draft left empty or a placeholder is Ken's to fill: the commit of each
/// repo the sources cite, `sha` for one, `repo@sha, …` for several. The
/// 2026-10-06 architecture pages said their pin was not in the sources.
pub fn pin_sources(page: &str, heads: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(page.len() + 64);
    let mut in_front = false;
    let mut in_sources = false;
    let cited: Vec<&String> = {
        let mut repos: Vec<&String> = heads.keys().filter(|r| page_cites(page, r)).collect();
        repos.sort();
        repos
    };
    for (i, line) in page.split_inclusive('\n').enumerate() {
        let bare = line.trim_end();
        if bare == "---" {
            in_front = i == 0;
            in_sources = false;
            out.push_str(line);
            continue;
        }
        if in_front && bare.starts_with("pin:") && !cited.is_empty() {
            let value = bare["pin:".len()..].split(" #").next().unwrap_or_default().trim();
            // `""` and `''` are empty too: eight pages kept `pin: ""` on 2026-10-07.
            if value.trim_matches(['"', '\'']).trim().is_empty() || value.contains("{{") {
                let pin = match cited.as_slice() {
                    [one] => heads[*one].clone(),
                    many => many.iter().map(|r| format!("{r}@{}", heads[*r])).collect::<Vec<_>>().join(", "),
                };
                let eol = &line[bare.len()..];
                out.push_str(&format!("pin: {pin}{eol}"));
                continue;
            }
        }
        if in_front && !line.starts_with(' ') && !line.starts_with('-') {
            in_sources = bare.starts_with("sources:");
        }
        let item = bare.trim_start().strip_prefix("- ");
        let pinned = item.filter(|_| in_front && in_sources).and_then(|item| {
            let quote = item.chars().next().filter(|q| *q == '"' || *q == '\'');
            let inner = quote.map_or(item, |q| item.trim_matches(q));
            let (repo, path) = inner.split_once(':')?;
            if repo.contains('@') || path.starts_with('(') {
                return None;
            }
            let sha = heads.get(repo)?;
            Some(line.replacen(&format!("{repo}:"), &format!("{repo}@{sha}:"), 1))
        });
        out.push_str(pinned.as_deref().unwrap_or(line));
    }
    out
}

/// Whether a page's `sources:` cite `repo` (`repo:path` or `repo@sha:path`).
fn page_cites(page: &str, repo: &str) -> bool {
    let mut in_front = false;
    let mut in_sources = false;
    for (i, line) in page.lines().enumerate() {
        let bare = line.trim_end();
        if bare == "---" {
            if !(i == 0 || in_front) {
                break;
            }
            in_front = i == 0;
            continue;
        }
        if !line.starts_with(' ') && !line.starts_with('-') {
            in_sources = bare.starts_with("sources:");
            continue;
        }
        let item = bare.trim_start().strip_prefix("- ").map(|x| x.trim_matches(['"', '\'']));
        if let Some(item) = item.filter(|_| in_sources) {
            if item.strip_prefix(repo).is_some_and(|rest| rest.starts_with(':') || rest.starts_with('@')) {
                return true;
            }
        }
    }
    false
}

/// Pin the sources of the pages just drafted (see [`pin_sources`]).
fn pin_drafted(wiki: &Path, drafted: &[String], repos: &[(String, PathBuf)]) -> Result<()> {
    let heads = repo_heads(repos);
    if heads.is_empty() {
        return Ok(());
    }
    for page in drafted {
        let path = wiki.join(page);
        let Ok(text) = fs::read_to_string(&path) else { continue };
        let pinned = pin_sources(&text, &heads);
        if pinned != text {
            write_page(&path, &pinned)?;
        }
    }
    Ok(())
}

/// Draft each of `pages` the wiki does not already have into `report`, each
/// checked for paths the checkout does not have ([`check_paths`]).
#[allow(clippy::too_many_arguments)]
fn draft_pages(
    wiki: &Path,
    pages: &[(String, String)],
    sources: &[Source],
    today: &str,
    report: &mut DraftReport,
    generate: &mut impl FnMut(&str) -> Result<String>,
    check: &PathCheck,
) -> Result<()> {
    for (page, purpose) in pages {
        draft_page(wiki, page, purpose, None, sources, today, report, generate, check)?;
    }
    Ok(())
}

/// Draft `page` unless the wiki already has it, from the page itself while
/// it is a template, else from `blank` (a copy of a `Templates/` blank).
#[allow(clippy::too_many_arguments)]
fn draft_page(
    wiki: &Path,
    page: &str,
    purpose: &str,
    blank: Option<&str>,
    sources: &[Source],
    today: &str,
    report: &mut DraftReport,
    generate: &mut impl FnMut(&str) -> Result<String>,
    check: &PathCheck,
) -> Result<()> {
    let path = wiki.join(page);
    let existing = fs::read_to_string(&path).ok();
    if !may_draft(existing.as_deref()) {
        report.kept.push(page.to_string());
        return Ok(());
    }
    let template = existing.as_deref().or(blank);
    match generate(&prompt(page, purpose, template, sources, today)).and_then(|r| finish(&r)) {
        Ok(text) => {
            let text = check_paths(page, text, check, report, generate);
            let lines = template_lines(template.unwrap_or_default());
            match verify(page, &text, &lines, sources, wiki, check, report, generate) {
                Ok(text) => {
                    write_page(&path, &text)?;
                    report.drafted.push(page.to_string());
                }
                Err(e) => report.failed.push((page.to_string(), e.to_string())),
            }
        }
        Err(e) => report.failed.push((page.to_string(), e.to_string())),
    }
    Ok(())
}

/// The lines of a template, trimmed: a drafted line that is one of them is
/// the template's, never a claim to check.
fn template_lines(template: &str) -> HashSet<String> {
    template.lines().map(str::trim).filter(|l| !l.is_empty() && !l.contains("{{")).map(String::from).collect()
}

/// `text`, a page just drafted, checked against its sources
/// ([`crate::wikiverify::check`]) and logged in `report`. With no checkout
/// to read (the single-folder [`draft`]), as it is. An error when the check
/// could not be made: nothing unverified is published.
#[allow(clippy::too_many_arguments)]
fn verify(
    page: &str,
    text: &str,
    template: &HashSet<String>,
    sources: &[Source],
    wiki: &Path,
    check: &PathCheck,
    report: &mut DraftReport,
    generate: &mut impl FnMut(&str) -> Result<String>,
) -> Result<String> {
    if check.repos.is_empty() {
        return Ok(text.to_string());
    }
    let drafted = report.drafted.clone();
    let ctx = crate::wikiverify::Ctx { check, sources, wiki, drafted: &drafted };
    let (page_text, done) = crate::wikiverify::check(page, text, template, &ctx, generate)?;
    if !done.summary.is_empty() {
        report.verified.push(done.summary);
    }
    report.changes.extend(done.changes);
    Ok(page_text)
}

/// A draft's result as the wiki keeps it, for the Team screen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Drafted {
    pub title: String,
    pub at: i64,
    pub report: DraftReport,
}

/// Keep the draft's result (the latest one replaces the one before).
fn file_card(db: &mut Db, report: &DraftReport, title: &str, now: i64) -> Result<()> {
    let d = Drafted { title: title.to_string(), at: now, report: report.clone() };
    db.store_wiki_draft(&serde_json::to_string(&d).map_err(|e| Error::Other(e.to_string()))?)
}

/// The last draft's result, if there was one.
pub fn last_draft(db: &Db) -> Option<Drafted> {
    db.wiki_draft().ok().flatten().and_then(|j| serde_json::from_str(&j).ok())
}

/// One thing a draft left for a person, for the Team screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    /// `draft` (a page drafted, still to read) · `draft-failed` · `proposal`
    /// (a change to a kept page, to apply or discard) · `repo-removed`
    pub kind: String,
    pub title: String,
    pub detail: String,
    pub path: String,
    /// A proposal's Review-store id, for Apply and Discard.
    pub item_id: Option<i64>,
}

/// Whether a page still says `status: draft`.
fn still_draft(wiki: &Path, page: &str) -> bool {
    let Ok(text) = fs::read_to_string(wiki.join(page)) else { return false };
    let mut lines = text.lines();
    lines.next().is_some_and(|l| l.trim_end() == "---")
        && lines
            .take_while(|l| l.trim_end() != "---")
            .any(|l| l.trim().strip_prefix("status:").is_some_and(|v| v.trim().trim_matches('"') == "draft"))
}

/// What the wiki's drafts left for a person: the pages drafted and not yet
/// read (still `status: draft`), the pages Ken could not draft and that are
/// still missing, the open proposals to pages a person keeps, and repos that
/// left the team while pages still cite them.
pub fn findings(wiki: &Path, db: &Db) -> Result<Vec<Finding>> {
    let mut out = Vec::new();
    if let Some(d) = last_draft(db) {
        for page in d.report.drafted.iter().filter(|p| still_draft(wiki, p)) {
            out.push(Finding {
                kind: "draft".into(),
                title: format!("{page} is a draft to read"),
                detail: "Ken drafted it from the team's repos. Read it against its sources, then set its status.".into(),
                path: page.clone(),
                item_id: None,
            });
        }
        for (page, why) in &d.report.failed {
            if may_draft(fs::read_to_string(wiki.join(page)).ok().as_deref()) {
                out.push(Finding {
                    kind: "draft-failed".into(),
                    title: format!("Ken could not draft {page}"),
                    detail: why.clone(),
                    path: page.clone(),
                    item_id: None,
                });
            }
        }
    }
    for it in db.list_open_review_items()? {
        if it.kind != PROPOSAL_KIND {
            continue;
        }
        let Some(p) = it.payload.as_deref().and_then(|j| serde_json::from_str::<Proposal>(j).ok()) else { continue };
        // A proposal from an ingest waits on its card on the Ingest screen.
        if p.from.is_some() {
            continue;
        }
        out.push(Finding { kind: "proposal".into(), title: it.title, detail: it.body, path: p.page, item_id: Some(it.id) });
    }
    for repo in removed_repos(db) {
        let pages = citing_pages(db, &repo)?;
        if pages.is_empty() {
            continue;
        }
        out.push(Finding {
            kind: "repo-removed".into(),
            title: format!("{repo} left the team: {} page{} cite it", pages.len(), if pages.len() == 1 { "" } else { "s" }),
            detail: format!(
                "Rewrite each without it, or retire it (`status: retired` with what replaced it): {}.",
                pages.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>().join(", ")
            ),
            path: pages[0].0.clone(),
            item_id: None,
        });
    }
    Ok(out)
}

/// Repos recorded as having left the team ([`record_removed_repo`]).
pub fn removed_repos(db: &Db) -> Vec<String> {
    db.removed_repos().ok().flatten().and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default()
}

/// The facts each page owns: it states them, and every other page links to
/// it for them. Round two restated the mod's purpose on nine pages and a
/// stale engine pin on six, each with a citation that looked checked.
pub const OWNS: &[(&str, &str)] = &[
    ("Current/Project.md", "what the product is, who it is for, its goal and what is out of scope"),
    ("Current/Team.md", "the roles, who decides what, who approves the protected paths, how work is handed off"),
    ("Current/Who-Does-What.md", "who owns each repo and area, who to ask, and who has committed to each recently"),
    ("Current/Feature-Status.md", "whether each feature is built and released"),
    ("Conventions/Architecture.md", "how the repos fit together and which repo calls or imports which"),
    ("Conventions/Code.md", "the code conventions, the lint and format rules, and the shared helpers"),
    ("Conventions/Data-and-Config.md", "the data and config folders, their formats and the code that loads them"),
    ("Conventions/Testing.md", "the test suites, how each is run, the gates and the fixtures"),
    ("Conventions/Registries.md", "the registries and the start-up wiring order, and how to add to each"),
    ("Platform/Index.md", "each platform or reference dependency, the version pinned and where the pin is"),
    ("Work/Releases.md", "what changed in each release, the version tags and what is unreleased"),
    ("Reference/Systems.md", "each system: what it does, its code and its data"),
    ("Reference/Config-Map.md", "each config file and key, its default, the code that reads it and how it is reloaded"),
    ("Reference/Build-and-Run.md", "how to build, run and test each repo, the toolchain versions, the run commands and the known build failures"),
    ("Reference/Vocabulary.md", "the words that differ between the team, the code and the platform, and the renames"),
    (START_HERE, "the traps that cost a newcomer the most time"),
];

/// What `page` owns ([`OWNS`]).
pub(crate) fn owns(page: &str) -> &'static str {
    OWNS.iter().find(|(p, _)| *p == page).map_or("", |(_, o)| o)
}

/// The team pages to draft, each purpose ending with what the page owns.
fn team_pages() -> Vec<(String, String)> {
    PAGES
        .iter()
        .map(|(p, u)| {
            let purpose = format!("{u}. It owns {}; a fact another page owns gets a link to that page, never restated", owns(p));
            (p.to_string(), purpose)
        })
        .collect()
}

/// Draft every page of [`PAGES`] the wiki does not already have, then file
/// keep the result (what was drafted and kept) for the Team screen.
pub fn draft(
    wiki: &Path,
    db: &mut Db,
    sources: &[Source],
    today: &str,
    now: i64,
    mut generate: impl FnMut(&str) -> Result<String>,
) -> Result<DraftReport> {
    let mut report = DraftReport { sources: sources.iter().map(|s| s.label.clone()).collect(), ..Default::default() };
    draft_pages(wiki, &team_pages(), sources, today, &mut report, &mut generate, &PathCheck::default())?;
    report.to_fill = placeholders_left(wiki);
    let title = format!("First wiki drafted: {} pages to read", report.drafted.len());
    file_card(db, &report, &title, now)?;
    Ok(report)
}

// --- The wiki at team scale: a Repo Map page per repo, then the team pages
// drafted from those pages, so twenty repos never overflow one prompt. ----

/// The section for what lives where in the code (the template's `Repo-Map/`).
pub const REPO_MAP: &str = "Repo-Map";

const REPO_PURPOSE: &str = "one repo's page in the Repo Map: what the repo is for (the team's own description first), \
    what lives where in it (its layout and entry points), which of its folders call which and the files most imported \
    (from the imports between folders), \
    who owns it (from CODEOWNERS, `people/` files and its docs) and who has committed recently (from the recent contributors), \
    how it is built, run and released (from its build files, `.ken/gates.json` and its brief's build sections), \
    and how it connects to the team's other repos";

/// A repo's page in the wiki: `Repo-Map/<name>.md`.
pub fn repo_page(name: &str) -> String {
    format!("{REPO_MAP}/{}.md", crate::workspace::member_leaf(name))
}

/// A repo's own pages beside its Repo Map page, by its kind, each from a
/// blank in `Templates/` and copied where `Templates/Index.md` says: (the
/// kind, the blank, where the copy goes with `{}` for the repo, the blank's
/// placeholders that are the repo's name, what the page is for). A code repo
/// gets its layers; a reference repo, which the team reads and never
/// changes, gets a dependency page and never an architecture page.
pub const KIND_PAGES: &[(crate::registry::RepoKind, &str, &str, &[&str], &str)] = &[
    (
        crate::registry::RepoKind::Code,
        "Templates/Repo-Architecture.md",
        "Conventions/Architecture-{}.md",
        &["{{Repo}}", "{{repo}}"],
        "one code repo's layers: each layer, the folder it lives in, what it holds and which layers it may call; \
         what must never call what, and why where a source says; as known gaps, a pair of folders that import each other only where a \
         source states the rule it breaks, and otherwise one line with how many pairs import both ways and the five largest; \
         the checks that enforce a layer rule; and a mermaid diagram",
    ),
    (
        crate::registry::RepoKind::Reference,
        "Templates/Dependency.md",
        "Platform/{}.md",
        &["{{Dependency Name}}", "{{dependency}}"],
        "one reference repo the team reads and never changes: what it is, the version pinned and where the pin is, \
         where its source, docs and data live, and its licence; its quirks and update procedure only where a source says",
    ),
];

/// The pages of [`KIND_PAGES`] a repo of `kinds` gets: (page, blank, purpose).
pub fn kind_pages(name: &str, kinds: &[crate::registry::RepoKind]) -> Vec<(String, &'static str, &'static str)> {
    use crate::registry::RepoKind;
    let leaf = crate::workspace::member_leaf(name);
    KIND_PAGES
        .iter()
        .filter(|(k, ..)| kinds.contains(k) && !(*k == RepoKind::Code && kinds.contains(&RepoKind::Reference)))
        .map(|(_, blank, at, _, purpose)| (at.replace("{}", leaf), *blank, *purpose))
        .collect()
}

/// The blank a repo's page is drafted from: the wiki's own copy of it, else
/// the bundled one, with the repo's name filled in.
fn blank_for(wiki: &Path, blank: &str, name: &str) -> String {
    let text = fs::read_to_string(wiki.join(blank))
        .ok()
        .or_else(|| crate::wikinew::TEMPLATE.iter().find(|(p, _)| *p == blank).map(|(_, t)| t.to_string()))
        .unwrap_or_default();
    let leaf = crate::workspace::member_leaf(name);
    let names = KIND_PAGES.iter().find(|(_, b, ..)| *b == blank).map_or(&[][..], |(_, _, _, n, _)| *n);
    names.iter().fold(text, |t, p| t.replace(p, leaf))
}

/// The Repo Map's index: a table of every repo page with what it is for.
/// Written by Ken, not the model: it is a list, and a list has no facts to
/// get wrong.
pub fn repo_map_index(repos: &[(String, String)], today: &str) -> String {
    let mut s = format!(
        "---\ntitle: \"Repo Map\"\naliases: [\"Repo-Map\", \"what lives where\"]\ntags: [moc]\nupdated: {today}\n---\n\n\
         # Repo Map\n\nWhat lives where in the code, one page per repo. Open the repo's page before searching its files.\n\n\
         | repo | what it is for |\n|---|---|\n"
    );
    for (name, description) in repos {
        s.push_str(&index_row(name, description));
    }
    s
}

fn index_row(name: &str, description: &str) -> String {
    let leaf = crate::workspace::member_leaf(name);
    let d = description.split_whitespace().collect::<Vec<_>>().join(" ").replace('|', "/");
    format!("| [[{leaf}]] | {} |\n", if d.is_empty() { "(not said yet)" } else { &d })
}

/// What a drafted Repo Map page says its repo is for: the first sentence
/// of its first plain paragraph (past the frontmatter, the title, the
/// drafted-by line, headings, tables, lists and quotes), citations taken
/// out. Tools had no README, so on 2026-10-06 the index and START-HERE said
/// "(not said yet)" while its own page said what it is.
pub fn page_purpose(page: &str) -> Option<String> {
    let body = page
        .strip_prefix("---\n")
        .and_then(|rest| rest.find("\n---").map(|i| &rest[i + 4..]))
        .unwrap_or(page);
    let mut para: Vec<&str> = Vec::new();
    for line in body.lines().map(str::trim) {
        let plain = !line.is_empty()
            && !line.starts_with(['#', '|', '-', '*', '+', '>', '!', '`', '<', '['])
            && !line.starts_with(|c: char| c.is_ascii_digit())
            && !line.starts_with("Drafted by Ken");
        if plain {
            para.push(line);
        } else if !para.is_empty() {
            break;
        }
    }
    // Backticked `repo:path` citations are for checking, not for a summary.
    let joined = para.join(" ");
    let mut text = String::new();
    for (i, part) in joined.split('`').enumerate() {
        let citation = i % 2 == 1 && part.contains(':') && !part.contains(' ');
        if !citation {
            text.push_str(&if i % 2 == 1 { format!("`{part}`") } else { part.to_string() });
        }
    }
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ").replace("**", "");
    let sentence = match text.find(". ") {
        Some(i) => &text[..=i],
        None => text.as_str(),
    };
    let sentence = sentence.trim();
    if sentence.is_empty() {
        return None;
    }
    Some(if sentence.chars().count() > 220 { format!("{}…", sentence.chars().take(219).collect::<String>()) } else { sentence.to_string() })
}

/// What a repo is for, for the Repo Map index and START-HERE: what the team
/// said at set-up, else what its drafted Repo Map page says ([`page_purpose`]).
fn describe_repo(wiki: &Path, name: &str, root: &Path) -> String {
    crate::registry::description_of(root)
        .filter(|d| !d.trim().is_empty())
        .or_else(|| fs::read_to_string(wiki.join(repo_page(name))).ok().and_then(|p| page_purpose(&p)))
        .unwrap_or_default()
}

/// START-HERE's repo rows that set-up left as "(not said yet)", filled in
/// from `described` once a repo's purpose is known. Only those exact rows:
/// a row a person wrote is theirs. Whether anything changed.
fn fill_start_here(wiki: &Path, described: &[(String, String)]) -> Result<bool> {
    let path = wiki.join("START-HERE.md");
    let Ok(text) = fs::read_to_string(&path) else { return Ok(false) };
    let mut out = String::with_capacity(text.len());
    let mut changed = false;
    for line in text.split_inclusive('\n') {
        let row = described.iter().find(|(n, d)| !d.trim().is_empty() && line.trim_end() == format!("| `{n}` | (not said yet) |"));
        match row {
            Some((n, d)) => {
                let d = d.split_whitespace().collect::<Vec<_>>().join(" ").replace('|', "/");
                out.push_str(&format!("| `{n}` | {d} |\n"));
                changed = true;
            }
            None => out.push_str(line),
        }
    }
    if changed {
        write_page(&path, &out)?;
    }
    Ok(changed)
}

/// What every team page is drafted from: each repo's description, the
/// wiki's pages with the facts each owns ([`page_directory`]), so a page
/// links rather than restates, then `extra`. The repo pages are no longer
/// sent whole to every page: on 2026-10-06 that was 44% of all a draft sent.
/// What one page needs, the repo pages' sections included, is in
/// [`page_sources`].
pub fn team_sources(wiki_name: &str, repos: &[(String, PathBuf)], extra: &[Source]) -> Vec<Source> {
    let mut out = Vec::new();
    for (name, root) in repos {
        if let Some(d) = crate::registry::description_of(root) {
            out.push(Source { label: format!("{name}:(what this repo is for, in the team's words)"), text: clip(&d, 2_000) });
        }
    }
    out.push(Source { label: format!("{wiki_name}:(pages of this wiki, and the facts each owns)"), text: page_directory(repos) });
    let used: usize = out.iter().map(|s| s.text.len()).sum();
    let mut left = SOURCE_BUDGET.saturating_sub(used);
    for s in extra {
        if left == 0 {
            break;
        }
        let text = clip(&s.text, left.min(PER_FILE));
        left = left.saturating_sub(text.len());
        out.push(Source { label: s.label.clone(), text });
    }
    out
}

/// Characters of the Repo Map pages' sections one team page reads, all
/// repos together.
const REPO_SECTIONS_MAX: usize = 24_000;
/// Where a Repo Map page's section that fits no page by its words goes.
const REPO_SECTION_DEFAULT: &[&str] = &["Current/Project.md", "Conventions/Architecture.md"];

/// The sections of the drafted Repo Map pages one team page needs: each
/// `##` section goes to the two pages its words fit best ([`page_scores`]),
/// and one that fits none to [`REPO_SECTION_DEFAULT`]; each repo an equal
/// share of [`REPO_SECTIONS_MAX`]. Each cited at its line of the page.
fn repo_page_sections(page: &str, wiki: &Path, wiki_name: &str, repos: &[(String, PathBuf)]) -> Vec<Source> {
    let share = REPO_SECTIONS_MAX / repos.len().max(1);
    let mut out = Vec::new();
    for (name, _) in repos {
        let rel = repo_page(name);
        let Some(text) = read_plain(&wiki.join(&rel)) else { continue };
        // Past the frontmatter, so its `sources:` are not a section.
        let body_at = text.strip_prefix("---\n").and_then(|rest| rest.find("\n---\n")).map_or(0, |i| i + 9);
        let skipped = text[..body_at].lines().count();
        let mine: Vec<Source> = split_at(&text[body_at..], |l| l.starts_with("## "))
            .into_iter()
            .filter(|sec| {
                let mut scores: Vec<(&str, usize)> =
                    page_scores(&sec.heading, &sec.text).into_iter().filter(|(p, n)| *n > 0 && *p != START_HERE).collect();
                scores.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
                if scores.is_empty() {
                    REPO_SECTION_DEFAULT.contains(&page)
                } else {
                    scores.iter().take(2).any(|(p, _)| *p == page)
                }
            })
            .map(|sec| Source { label: part_label(wiki_name, &rel, sec.line + skipped), text: sec.text })
            .collect();
        out.extend(within(mine, share));
    }
    out
}

/// Which of what the checkout says now ([`checkout_sources`]) a team page
/// reads, as (what the code registers, the values): the pages that list
/// systems, features and registries take the wiring, one row per
/// registration; the pages that state versions, defaults and data take the
/// values.
fn checkout_wanted(page: &str) -> (bool, bool) {
    match page {
        "Reference/Systems.md" | "Current/Feature-Status.md" | "Conventions/Registries.md" => (true, false),
        "Reference/Config-Map.md" | "Reference/Build-and-Run.md" | "Platform/Index.md" | "Conventions/Data-and-Config.md" => (false, true),
        _ => (false, false),
    }
}

/// Split `budget` between repos that would read `sizes`: an equal share
/// each, and what a repo needs less than its share goes to the others. A
/// strict equal share gave the mod's 40,000 characters of architecture the
/// same 15,000 as a repo with 1,000.
fn fair_shares(sizes: &[usize], budget: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..sizes.len()).collect();
    order.sort_by_key(|i| sizes[*i]);
    let mut out = vec![0; sizes.len()];
    let mut left = budget;
    for (k, i) in order.into_iter().enumerate() {
        out[i] = sizes[i].min(left / (sizes.len() - k));
        left -= out[i];
    }
    out
}

/// `sources` in order while `budget` lasts, the last one clipped to fit;
/// a source of [`checkout_sources`] is never cut, but named with its size.
fn within(sources: Vec<Source>, budget: usize) -> Vec<Source> {
    let mut out = Vec::new();
    let mut used = 0;
    for s in sources {
        if used >= budget {
            break;
        }
        let text = if is_checkout(&s.label) && used + s.text.len() > budget {
            format!("(left out for room: {} characters; never cut)", s.text.len())
        } else {
            clip(&s.text, budget - used)
        };
        used += text.len();
        out.push(Source { label: s.label, text });
    }
    out
}

/// `sources` in order, each whole, while `budget` lasts: one that does not
/// fit is skipped for a later, smaller one.
fn whole_within(sources: Vec<Source>, budget: usize) -> Vec<Source> {
    let mut used = 0;
    sources
        .into_iter()
        .filter(|s| {
            let fits = used + s.text.len() <= budget;
            if fits {
                used += s.text.len();
            }
            fits
        })
        .collect()
}

/// What one team page reads from the repos themselves, beside the repo
/// pages, within [`PAGE_BUDGET`] shared fairly between the repos
/// ([`fair_shares`]): for Releases the version tags, what landed since the
/// newest and each version's own notes; for Team and Who Does What the
/// CODEOWNERS, `people/` files (the wiki's too) and the recent contributors;
/// and the lines of the briefs that name who decides ([`decider_lines`]),
/// with `.ken/gates.json` (its protected paths); for the architecture page
/// the import map; for Project the newest version tags; for Vocabulary the
/// briefs' lines about words; then for every page the parts of the briefs
/// and the repo's other docs ([`doc_briefs`]) routed to it ([`route_parts`]).
/// `roster` is the team's ([`team_roster`]).
pub fn page_sources(
    page: &str,
    wiki: &Path,
    wiki_name: &str,
    repos: &[(String, PathBuf)],
    roster: &[crate::people::Person],
) -> Vec<Source> {
    let people = matches!(page, "Current/Team.md" | "Current/Who-Does-What.md");
    let mut per_repo: Vec<Vec<Source>> = Vec::new();
    for (name, root) in repos {
        let mut mine: Vec<Source> = Vec::new();
        let push = pusher_of(PAGE_BUDGET, SECTION_MAX.max(LAYOUT_BUDGET));
        let used = |v: &[Source]| v.iter().map(|s| s.text.len()).sum::<usize>();
        // What the checkout says now, first, so no doc's older figure wins.
        let (wiring, values) = checkout_wanted(page);
        if wiring || values {
            let reference = crate::registry::index_of(root).0.contains(&crate::registry::RepoKind::Reference);
            let tracked = ls_files(root).unwrap_or_default();
            mine.extend(checkout_sources(name, root, &tracked, &briefs(root), wiring && !reference, values.then_some(TEAM_VALUES_MAX)));
        }
        match page {
            "Work/Releases.md" => {
                if let Some(t) = git_tags(root) {
                    push(format!("{name}:(version tags, newest first)"), t, &mut mine);
                }
                if let Some((label, log)) = git_unreleased(root) {
                    mine.push(Source { label: format!("{name}:{label}"), text: clip_by_day(&log, UNRELEASED_MAX) });
                }
                let left = PAGE_BUDGET.saturating_sub(used(&mine));
                mine.extend(release_notes(name, root, usize::MAX, left));
            }
            "Current/Project.md" => {
                if let Some(t) = git_tags(root) {
                    let newest: Vec<&str> = t.lines().take(5).collect();
                    push(format!("{name}:(version tags, newest first)"), newest.join("\n"), &mut mine);
                }
            }
            "Conventions/Architecture.md" => {
                let reference = crate::registry::index_of(root).0.contains(&crate::registry::RepoKind::Reference);
                if !reference {
                    if let Some(d) = folder_imports(root) {
                        push(format!("{name}:(imports between folders, from the code)"), d, &mut mine);
                    }
                }
            }
            _ if people => {
                for f in ["CODEOWNERS", ".github/CODEOWNERS", "docs/CODEOWNERS"] {
                    if let Ok(t) = fs::read_to_string(root.join(f)) {
                        push(format!("{name}:{f}"), t, &mut mine);
                    }
                }
                for s in people_files(name, root) {
                    push(s.label, s.text, &mut mine);
                }
                if let Some(a) = recent_contributors(root, roster) {
                    push(format!("{name}:(recent contributors by folder, last 90 days, from git)"), a, &mut mine);
                }
                if let Some(t) = read_plain(&root.join(".ken").join("gates.json")) {
                    push(format!("{name}:.ken/gates.json"), t, &mut mine);
                }
                if let Some(d) = decider_lines(name, &briefs(root)) {
                    push(format!("{name}:(lines of its briefs about who decides)"), d, &mut mine);
                }
            }
            _ => {}
        }
        // The parts of its briefs and other docs routed to this page.
        let mut briefs = briefs(root);
        if page == "Reference/Vocabulary.md" {
            if let Some(w) = word_lines(name, &briefs) {
                push(format!("{name}:(lines of its briefs about words and names)"), w, &mut mine);
            }
        }
        briefs.extend(doc_briefs(root));
        let routed = route_parts(name, &briefs).remove(page).unwrap_or_default();
        let left = PAGE_BUDGET.saturating_sub(used(&mine));
        mine.extend(whole_within(routed, left));
        per_repo.push(mine);
    }
    let sizes: Vec<usize> = per_repo.iter().map(|v| v.iter().map(|s| s.text.len()).sum()).collect();
    let mut out: Vec<Source> = if page == START_HERE { Vec::new() } else { repo_page_sections(page, wiki, wiki_name, repos) };
    out.extend(per_repo.into_iter().zip(fair_shares(&sizes, PAGE_BUDGET)).flat_map(|(v, share)| within(v, share)));
    if people {
        out.extend(people_files(wiki_name, wiki));
        if !roster.is_empty() {
            let listed: Vec<String> = roster
                .iter()
                .map(|p| if p.aliases.is_empty() { format!("- {}", p.name) } else { format!("- {} (also {})", p.name, p.aliases.join(", ")) })
                .collect();
            out.push(Source { label: format!("{wiki_name}:(the team's roster, from people/)"), text: listed.join("\n") });
        }
    }
    out
}

/// Every label once, in the order first seen.
fn dedup_labels(labels: &mut Vec<String>) {
    let mut seen = HashSet::new();
    labels.retain(|l| seen.insert(l.clone()));
}

/// Pass one: each repo's Repo Map page and its pages by kind
/// ([`kind_pages`]), from that repo alone.
fn draft_repo_pages(
    wiki: &Path,
    repos: &[(String, PathBuf)],
    roster: &[crate::people::Person],
    today: &str,
    report: &mut DraftReport,
    generate: &mut impl FnMut(&str) -> Result<String>,
    check: &PathCheck,
) -> Result<()> {
    for (name, root) in repos {
        let mut pages = vec![(repo_page(name), REPO_PURPOSE, None)];
        for (page, blank, purpose) in kind_pages(name, &crate::registry::kind_of(root)) {
            pages.push((page, purpose, Some(blank_for(wiki, blank, name))));
        }
        let mut sources: Option<Vec<Source>> = None;
        for (page, purpose, blank) in pages {
            // A page a person keeps is not drafted, so its sources are not read.
            if !may_draft(fs::read_to_string(wiki.join(&page)).ok().as_deref()) {
                report.kept.push(page);
                continue;
            }
            if sources.is_none() {
                let read = gather_repo(name, root, roster);
                report.sources.extend(read.iter().map(|s| s.label.clone()));
                sources = Some(read);
            }
            let map = sources.as_deref().unwrap_or_default();
            // A layers page reads the briefs' parts about layers first.
            let layered = page.starts_with("Conventions/Architecture-").then(|| layers_sources(name, root, map));
            let sources = layered.as_deref().unwrap_or(map);
            draft_page(wiki, &page, purpose, blank.as_deref(), sources, today, report, generate, check)?;
        }
    }
    Ok(())
}

/// Draft each team page of `pages` from the sources every page shares and
/// its own ([`page_sources`]), read only for a page that will be drafted.
#[allow(clippy::too_many_arguments)]
fn draft_team_pages(
    wiki: &Path,
    wiki_name: &str,
    pages: &[(String, String)],
    common: &[Source],
    repos: &[(String, PathBuf)],
    roster: &[crate::people::Person],
    today: &str,
    report: &mut DraftReport,
    generate: &mut impl FnMut(&str) -> Result<String>,
    check: &PathCheck,
) -> Result<()> {
    for (page, purpose) in pages {
        let mut sources = common.to_vec();
        if may_draft(fs::read_to_string(wiki.join(page)).ok().as_deref()) {
            sources.extend(page_sources(page, wiki, wiki_name, repos, roster));
            report.sources.extend(sources.iter().map(|s| s.label.clone()));
        }
        draft_pages(wiki, &[(page.clone(), purpose.clone())], &sources, today, report, generate, check)?;
    }
    Ok(())
}

/// The team's roster: the `people/` files of the wiki and of `repos`, so a
/// team repo is read wherever it sits, and a wiki that holds the team's
/// folders too.
fn team_roster(wiki: &Path, repos: &[(String, PathBuf)]) -> Vec<crate::people::Person> {
    crate::people::roster_of(std::iter::once(wiki).chain(repos.iter().map(|(_, r)| r.as_path())))
}

/// Rebuild the team's list of the rulings its code cites
/// ([`crate::teamnew::write_cited_in_code`]) from the code repos of `repos`,
/// in the team repo: the wiki when it holds the decisions log, else the
/// first of `repos` that does. Nothing to do without a log.
fn cite_rulings(wiki: &Path, repos: &[(String, PathBuf)], today: &str) -> Result<()> {
    use crate::registry::RepoKind;
    let roots = std::iter::once(wiki).chain(repos.iter().map(|(_, r)| r.as_path()));
    let Some(team) = roots.into_iter().find(|r| r.join(crate::ingest::TEAM_DECISIONS).is_file()) else { return Ok(()) };
    let code: Vec<(String, PathBuf)> = repos
        .iter()
        .filter(|(_, r)| {
            let kinds = crate::registry::kind_of(r);
            kinds.contains(&RepoKind::Code) && !kinds.contains(&RepoKind::Reference)
        })
        .cloned()
        .collect();
    crate::teamnew::write_cited_in_code(team, &code, today).map(|_| ())
}

// --- START-HERE's Traps rows: drafted from the briefs' gotchas. Set-up
// fills the rest of START-HERE (its repo table); Ken writes only these rows,
// and only while they are still the template's. -----------------------------

/// The start of START-HERE's Traps row while it is the template's.
const TRAPS_PLACEHOLDER: &str = "| {{the trap that costs a newcomer the most time";
/// Rows of the Traps table Ken drafts at most.
const TRAPS_MAX: usize = 8;

/// The wiki's pages a drafted page can link to, one line each: the team
/// pages with the facts each owns ([`OWNS`]), and each repo's Repo Map page.
pub fn page_directory(repos: &[(String, PathBuf)]) -> String {
    let mut out = String::new();
    for (page, owned) in OWNS {
        out.push_str(&format!("- [[{}]] ({page}): {owned}\n", stem(page.rsplit('/').next().unwrap_or(page))));
    }
    for (name, _) in repos {
        out.push_str(&format!("- [[{}]] ({}): what lives where in {name}, and how it is built\n", crate::workspace::member_leaf(name), repo_page(name)));
    }
    out
}

/// The one call that drafts START-HERE's Traps rows.
pub fn traps_prompt(directory: &str, sources: &[Source]) -> String {
    let mut s = format!(
        "You are filling one table of a team's wiki, the Traps table at the top of `{START_HERE}`: the traps that cost a \
         newcomer the most time.\n\n\
         Rules:\n\
         - Use only the sources below: the gotchas, warnings and \"never\" rules of the team's own briefs. Pick at most \
         {TRAPS_MAX}, the ones that cost a newcomer the most time: where the platform's content lives, a word the code spells \
         differently, a step that fails without saying so, a file that must never be edited by hand.\n\
         - One row each: `| trap | what happens | page |`. The trap and what happens in plain words, one line each; end the \
         what-happens cell with the source's label in backticks. The page: a link from the list below to the page that holds \
         it, or nothing when none does.\n\
         - Where the code and a brief disagree, the code wins; leave out a trap the sources do not state.\n\
         - Reply with the rows only, one per line, each starting and ending with `|`. No header, no preamble.\n\n\
         PAGES OF THIS WIKI:\n{directory}\nSOURCES:\n"
    );
    for src in sources {
        s.push_str(&format!("\n=== {} ===\n{}\n", src.label, src.text));
    }
    s
}

/// The Traps rows of a reply: lines that start and end with `|` and have
/// three cells, never a header, a separator or a placeholder; at most
/// [`TRAPS_MAX`].
pub fn traps_rows(reply: &str) -> Vec<String> {
    reply
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('|') && l.ends_with('|') && l.len() > 2)
        .filter(|l| l[1..l.len() - 1].split('|').count() == 3)
        .filter(|l| !l.contains("{{") && !l.contains("---") && !l.to_lowercase().starts_with("| trap |"))
        .take(TRAPS_MAX)
        .map(String::from)
        .collect()
}

/// `start_here` with its Traps placeholder row replaced by `rows`; None
/// when that row is gone (a person wrote the table) or there are no rows.
/// Nothing else of the page changes.
pub fn fill_traps(start_here: &str, rows: &[String]) -> Option<String> {
    if rows.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(start_here.len());
    let mut done = false;
    for line in start_here.split_inclusive('\n') {
        if !done && line.trim_start().starts_with(TRAPS_PLACEHOLDER) {
            let eol = if line.ends_with("\r\n") { "\r\n" } else { "\n" };
            for r in rows {
                out.push_str(r);
                out.push_str(eol);
            }
            done = true;
        } else {
            out.push_str(line);
        }
    }
    done.then_some(out)
}

/// Draft START-HERE's Traps rows, when it still has the template's row,
/// from the parts of the briefs routed to it ([`page_sources`]): one call,
/// the rows spliced in, the rest of the page as set-up wrote it.
#[allow(clippy::too_many_arguments)]
fn draft_traps(
    wiki: &Path,
    wiki_name: &str,
    repos: &[(String, PathBuf)],
    roster: &[crate::people::Person],
    report: &mut DraftReport,
    generate: &mut impl FnMut(&str) -> Result<String>,
    check: &PathCheck,
) -> Result<()> {
    let path = wiki.join(START_HERE);
    let Ok(text) = fs::read_to_string(&path) else { return Ok(()) };
    if !text.lines().any(|l| l.trim_start().starts_with(TRAPS_PLACEHOLDER)) {
        report.kept.push(START_HERE.to_string());
        return Ok(());
    }
    let sources = page_sources(START_HERE, wiki, wiki_name, repos, roster);
    if sources.is_empty() {
        return Ok(());
    }
    report.sources.extend(sources.iter().map(|s| s.label.clone()));
    let filled = generate(&traps_prompt(&page_directory(repos), &sources)).map(|r| fill_traps(&text, &traps_rows(&r)));
    match filled {
        Ok(Some(page)) => {
            // Only the rows are Ken's: every line set-up wrote is left alone.
            let set_up: HashSet<String> = text.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
            let kept_a_row = |p: &str| p.lines().any(|l| l.trim().starts_with('|') && !set_up.contains(l.trim()));
            match verify(START_HERE, &page, &set_up, &sources, wiki, check, report, generate) {
                Ok(checked) if kept_a_row(&checked) => {
                    write_page(&path, &checked)?;
                    report.drafted.push(START_HERE.to_string());
                }
                Ok(_) => report.failed.push((START_HERE.to_string(), "no Traps row was borne out by its source".into())),
                Err(e) => report.failed.push((START_HERE.to_string(), e.to_string())),
            }
        }
        Ok(None) => report.failed.push((START_HERE.to_string(), "the reply had no Traps rows".into())),
        Err(e) => report.failed.push((START_HERE.to_string(), e.to_string())),
    }
    Ok(())
}

/// Draft a team's wiki: a Repo Map page per repo (each from that repo
/// alone) and its pages by kind, the Repo Map index, then the team pages of
/// [`PAGES`] from the repo pages and descriptions, START-HERE's Traps rows
/// ([`draft_traps`]), and the team repo's rulings cited in code
/// ([`cite_rulings`]). `repos` are the team's repos,
/// the wiki itself left out. The result is kept for the Team screen.
#[allow(clippy::too_many_arguments)]
pub fn draft_team(
    wiki: &Path,
    wiki_name: &str,
    db: &mut Db,
    repos: &[(String, PathBuf)],
    extra: Option<&Path>,
    today: &str,
    now: i64,
    mut generate: impl FnMut(&str) -> Result<String>,
) -> Result<DraftReport> {
    let mut report = DraftReport::default();
    let check = PathCheck::of(repos).with_library(wiki_name, wiki);
    let roster = team_roster(wiki, repos);
    draft_repo_pages(wiki, repos, &roster, today, &mut report, &mut generate, &check)?;
    let described: Vec<(String, String)> = repos.iter().map(|(n, r)| (n.clone(), describe_repo(wiki, n, r))).collect();
    let index = format!("{REPO_MAP}/Index.md");
    let index_path = wiki.join(&index);
    if may_draft(fs::read_to_string(&index_path).ok().as_deref()) {
        write_page(&index_path, &repo_map_index(&described, today))?;
        report.drafted.push(index);
    }
    fill_start_here(wiki, &described)?;
    let extra = extra.map(gather_extra).unwrap_or_default();
    let common = team_sources(wiki_name, repos, &extra);
    draft_team_pages(wiki, wiki_name, &team_pages(), &common, repos, &roster, today, &mut report, &mut generate, &check)?;
    draft_traps(wiki, wiki_name, repos, &roster, &mut report, &mut generate, &check)?;
    cite_rulings(wiki, repos, today)?;
    dedup_labels(&mut report.sources);
    pin_drafted(wiki, &report.drafted, repos)?;
    report.to_fill = placeholders_left(wiki);
    let title = format!("First wiki drafted: {} pages to read", report.drafted.len());
    file_card(db, &report, &title, now)?;
    Ok(report)
}

// --- A repo added later: its page is new, so Ken drafts it; the pages a
// person already keeps get a proposed change, never a write. -------------

/// Review kind of a proposed change to one page.
pub const PROPOSAL_KIND: &str = "page-proposal";

/// A change to one page, held until a person applies it. `base` is the page
/// as Ken read it; the change applies only while the page still reads so.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Proposal {
    pub page: String,
    pub base: String,
    pub proposed: String,
    /// The repo the page is in, when not the one whose Review holds the card
    /// (a ticket in the team repo, proposed from a note in the wiki).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    /// A decisions-log entry to append, worked out against the log as it is
    /// when applied: rulings from one note then apply in any order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub append: Option<RulingEntry>,
    /// The ingested note this came from, so undoing the ingest withdraws it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

/// A ruling to append to a decisions log.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RulingEntry {
    pub ruling: String,
    pub decider: Option<String>,
    pub date: String,
    pub note: String,
}

impl Proposal {
    pub fn new(page: impl Into<String>, base: impl Into<String>, proposed: impl Into<String>) -> Proposal {
        Proposal { page: page.into(), base: base.into(), proposed: proposed.into(), ..Default::default() }
    }
}

/// Pages a new repo changes, and what each should gain.
const ON_ADD: &[(&str, &str)] = &[
    ("Conventions/Architecture.md", "work the new repos into the repo table, what may call what, and the diagram"),
    ("Current/Who-Does-What.md", "add who owns each new repo and who to ask"),
    ("Current/Project.md", "say what the new repos add to the project, only if they change what it is or does"),
    (
        "Reference/Vocabulary.md",
        "add a row for each trap the new repos bring: a word they use for something the team already names differently, \
         a word with two meanings, a rename; never a word that means what it says",
    ),
];

pub fn update_prompt(page: &str, change: &str, existing: &str, sources: &[Source], today: &str) -> String {
    let mut s = format!(
        "You are proposing a change to one page of a team's wiki, `{page}`, because repos were added to the team. \
         A person keeps this page; they will see your change as a diff and apply it or not. The change: {change}.\n\n\
         Rules:\n\
         - Change only what the new repos change. Keep every other line exactly as it is, in the same order.\n\
         - Use only what the sources below say, and cite each new fact inline as its label in backticks.\n\
         - Set `updated: {today}` in the frontmatter; leave any `verified:` line as it is.\n\
         - If the new repos change nothing on this page, reply exactly NO CHANGE.\n\
         - Otherwise reply with the whole page, starting with `---`. No preamble, no code fences around it.\n\n\
         THE PAGE NOW:\n{existing}\n\nSOURCES (the new repos):\n"
    );
    for src in sources {
        s.push_str(&format!("\n=== {} ===\n{}\n", src.label, src.text));
    }
    s
}

/// The model's reply as a proposed page, or `None` for no change.
pub fn finish_update(reply: &str, existing: &str) -> Result<Option<String>> {
    let mut t = reply.trim();
    if t.eq_ignore_ascii_case("NO CHANGE") {
        return Ok(None);
    }
    if let Some(rest) = t.strip_prefix("```markdown").or_else(|| t.strip_prefix("```md")).or_else(|| t.strip_prefix("```")) {
        t = rest.trim_start().strip_suffix("```").unwrap_or(rest).trim();
    }
    if existing.starts_with("---") && !t.starts_with("---") {
        return Err(Error::Other("the proposed page lost its frontmatter".into()));
    }
    let page = format!("{t}\n");
    Ok((page.trim() != existing.trim()).then_some(page))
}

fn file_proposal(db: &mut Db, p: &Proposal, added: &[String], now: i64) -> Result<()> {
    let names = added.join(", ");
    let title = format!("Proposed: {} with {names}", p.page);
    let body = format!(
        "Ken read {names} and proposes this change to {}, a page a person keeps. Apply it to write it, or discard it. \
         It applies only while the page still reads as it did when Ken proposed it.",
        p.page
    );
    file_page_proposal(db, p, &title, &body, now)
}

/// Store one proposed page change, to apply or discard (on Team, or on its
/// ingest card when an ingest proposed it).
pub fn file_page_proposal(db: &mut Db, p: &Proposal, title: &str, body: &str, now: i64) -> Result<()> {
    let payload = serde_json::to_string(p).map_err(|e| Error::Other(e.to_string()))?;
    db.insert_review_item(PROPOSAL_KIND, title, body, &p.page, Some(&payload), now)?;
    Ok(())
}

/// Repos joined the team: draft their Repo Map pages, then for each page of
/// [`ON_ADD`] and the Repo Map index, draft it when it is still missing or a
/// template, or file a proposed change when a person keeps it. `all` is
/// every repo of the team now, the added ones included.
#[allow(clippy::too_many_arguments)]
pub fn draft_added(
    wiki: &Path,
    wiki_name: &str,
    db: &mut Db,
    added: &[(String, PathBuf)],
    all: &[(String, PathBuf)],
    today: &str,
    now: i64,
    mut generate: impl FnMut(&str) -> Result<String>,
) -> Result<DraftReport> {
    let mut report = DraftReport::default();
    let names: Vec<String> = added.iter().map(|(n, _)| n.clone()).collect();
    let check = PathCheck::of(all).with_library(wiki_name, wiki);
    // The roster from every repo of the team: the team repo may not be one
    // of those added.
    let roster = team_roster(wiki, all);
    draft_repo_pages(wiki, added, &roster, today, &mut report, &mut generate, &check)?;

    // The index: a new one lists every repo; a kept one gains a row per new repo.
    let index = format!("{REPO_MAP}/Index.md");
    let existing = fs::read_to_string(wiki.join(&index)).ok();
    match existing.as_deref().filter(|t| !may_draft(Some(t))) {
        None => {
            let listed: Vec<(String, String)> = all.iter().map(|(n, r)| (n.clone(), describe_repo(wiki, n, r))).collect();
            write_page(&wiki.join(&index), &repo_map_index(&listed, today))?;
            report.drafted.push(index);
        }
        Some(text) => {
            let mut proposed = text.trim_end().to_string();
            proposed.push('\n');
            let mut changed = false;
            for (n, r) in added {
                if !text.contains(&format!("[[{}]]", crate::workspace::member_leaf(n))) {
                    proposed.push_str(&index_row(n, &describe_repo(wiki, n, r)));
                    changed = true;
                }
            }
            if changed {
                let p = Proposal::new(index.clone(), text.to_string(), proposed);
                file_proposal(db, &p, &names, now)?;
                report.proposed.push(index);
            }
        }
    }

    let common = team_sources(wiki_name, added, &[]);
    for (page, change) in ON_ADD {
        let existing = fs::read_to_string(wiki.join(page)).ok();
        if may_draft(existing.as_deref()) {
            // Still missing or a template: draft it whole, from every repo.
            let purpose =
                PAGES.iter().find(|(p, _)| p == page).map(|(_, u)| u.to_string()).unwrap_or_else(|| change.to_string());
            let everything = team_sources(wiki_name, all, &[]);
            let pages = [(page.to_string(), purpose)];
            draft_team_pages(wiki, wiki_name, &pages, &everything, all, &roster, today, &mut report, &mut generate, &check)?;
            continue;
        }
        let mut sources = common.clone();
        sources.extend(page_sources(page, wiki, wiki_name, added, &roster));
        report.sources.extend(sources.iter().map(|s| s.label.clone()));
        let existing = existing.unwrap_or_default();
        match generate(&update_prompt(page, change, &existing, &sources, today)).and_then(|r| finish_update(&r, &existing)) {
            Ok(Some(proposed)) => {
                file_proposal(db, &Proposal::new(page.to_string(), existing, proposed), &names, now)?;
                report.proposed.push(page.to_string());
            }
            Ok(None) => report.kept.push(page.to_string()),
            Err(e) => report.failed.push((page.to_string(), e.to_string())),
        }
    }
    cite_rulings(wiki, all, today)?;
    dedup_labels(&mut report.sources);
    pin_drafted(wiki, &report.drafted, all)?;
    let title =
        format!("{} added to the wiki: {} drafted, {} proposed", names.join(", "), report.drafted.len(), report.proposed.len());
    file_card(db, &report, &title, now)?;
    Ok(report)
}

// --- Which repos a wiki covers: a wiki belongs to a team, and covers the
// team's repos. -----------------------------------------------------------

/// A workspace member as the wiki sees it: its name, folder, kinds and team.
#[derive(Debug, Clone, PartialEq)]
pub struct TeamMember {
    pub name: String,
    pub root: PathBuf,
    pub kind: Vec<crate::registry::RepoKind>,
    pub team: Option<String>,
}

impl TeamMember {
    fn is_wiki(&self) -> bool {
        self.kind.contains(&crate::registry::RepoKind::Wiki)
    }
}

/// The repos a wiki drafts from: the other members on its team, wikis left
/// out. A wiki with no team covers every member that has no team.
pub fn team_repos(wiki: &str, members: &[TeamMember]) -> Vec<(String, PathBuf)> {
    let Some(w) = members.iter().find(|m| m.name == wiki) else { return Vec::new() };
    members
        .iter()
        .filter(|m| m.name != wiki && !m.is_wiki() && m.team == w.team)
        .map(|m| (m.name.clone(), m.root.clone()))
        .collect()
}

/// The wiki that covers `member`: the wiki on its team (the first by name
/// when a team has two).
pub fn wiki_for<'a>(member: &str, members: &'a [TeamMember]) -> Option<&'a TeamMember> {
    let m = members.iter().find(|m| m.name == member)?;
    let mut wikis: Vec<&TeamMember> = members.iter().filter(|w| w.is_wiki() && w.name != member && w.team == m.team).collect();
    wikis.sort_by(|a, b| a.name.cmp(&b.name));
    wikis.into_iter().next()
}

// --- A repo removed from the team: the pages that still cite it. -----------

/// The wiki pages that cite `repo`: each page with the citations that name
/// it (its `sources:`), and the repo's own Repo Map page. Read from the index.
pub fn citing_pages(db: &Db, repo: &str) -> Result<Vec<(String, Vec<String>)>> {
    let leaf = crate::workspace::member_leaf(repo);
    let own = repo_page(repo);
    let mut out = Vec::new();
    for page in db.page_paths()?.into_iter().filter(|p| p.ends_with(".md")) {
        let Some(meta) = db.page_meta(&page)? else { continue };
        let cites: Vec<String> = meta
            .sources
            .iter()
            .filter(|s| {
                matches!(crate::drift::classify(s), crate::drift::Citation::Code(c) if c.repo.eq_ignore_ascii_case(leaf))
            })
            .map(|s| s.trim().trim_matches('"').to_string())
            .collect();
        if !cites.is_empty() || page.eq_ignore_ascii_case(&own) {
            out.push((page, cites));
        }
    }
    Ok(out)
}

/// Record that a repo left the team, when pages still cite it, so the Team
/// screen lists them for a person to rewrite or retire ([`findings`]).
/// Nothing is edited. Returns how many pages cite it; `None` when none do.
pub fn record_removed_repo(db: &mut Db, repo: &str) -> Result<Option<usize>> {
    let pages = citing_pages(db, repo)?;
    if pages.is_empty() {
        return Ok(None);
    }
    let mut repos = removed_repos(db);
    if !repos.iter().any(|r| r == repo) {
        repos.push(repo.to_string());
        db.store_removed_repos(&serde_json::to_string(&repos).map_err(|e| Error::Other(e.to_string()))?)?;
    }
    Ok(Some(pages.len()))
}

/// Why a proposal could not be applied.
#[derive(Debug, PartialEq)]
pub enum ApplyError {
    /// The page changed after Ken proposed; the proposal is out of date.
    Changed,
    Io(String),
}

/// Write a proposal, only while the page still reads as `base`: a change
/// made against an older page would undo whatever a person wrote since. A
/// ruling to append goes onto the log as it is now, whatever was added since.
/// `wiki` is the repo whose Review holds the card; `p.root` wins when set.
pub fn apply(wiki: &Path, p: &Proposal) -> std::result::Result<(), ApplyError> {
    let root = p.root.as_deref().map(Path::new).unwrap_or(wiki);
    let path = root.join(&p.page);
    let now = fs::read_to_string(&path).unwrap_or_default();
    if let Some(r) = &p.append {
        let log = if now.trim().is_empty() { "# Decisions\n".to_string() } else { now };
        let text = crate::ingest::decisions_entry(&log, &r.ruling, r.decider.as_deref(), &r.date, &r.note);
        return write_page(&path, &text).map_err(|e| ApplyError::Io(e.to_string()));
    }
    if now.replace("\r\n", "\n") != p.base.replace("\r\n", "\n") {
        return Err(ApplyError::Changed);
    }
    write_page(&path, &p.proposed).map_err(|e| ApplyError::Io(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gather_reads_readmes_docs_layout_and_an_extra_folder_within_budget() {
        let repo = tempfile::tempdir().unwrap();
        fs::write(repo.path().join("README.md"), "# Game\nA tactics game.\n").unwrap();
        fs::create_dir_all(repo.path().join("docs")).unwrap();
        fs::write(repo.path().join("docs/save.md"), "Saves are region files.\n").unwrap();
        fs::create_dir_all(repo.path().join("src/world")).unwrap();
        fs::create_dir_all(repo.path().join("node_modules/x")).unwrap();
        let extra = tempfile::tempdir().unwrap();
        fs::write(extra.path().join("Confluence-Home.txt"), "Our roadmap: ship in Q4.\n").unwrap();

        let s = gather(&[("game".into(), repo.path().to_path_buf())], Some(extra.path()));
        let labels: Vec<&str> = s.iter().map(|s| s.label.as_str()).collect();
        assert!(labels.contains(&"game:README.md"), "{labels:?}");
        assert!(labels.contains(&"game:docs/save.md"));
        let layout = s.iter().find(|s| s.label == "game:(layout)").unwrap();
        assert!(layout.text.contains("src/") && layout.text.contains("  world/") && !layout.text.contains("node_modules"));
        assert!(s.iter().any(|s| s.label.ends_with(":Confluence-Home.txt") && s.text.contains("Q4")));
    }

    #[test]
    fn a_draft_pins_its_code_citations_to_the_commit_it_read() {
        let heads: HashMap<String, String> = [("app".to_string(), "0a1b2c3d4e5f".to_string())].into();
        let page = "---\ntitle: Arch\nsources:\n  - app:README.md\n  - \"app:src/main.rs:12\"\n  - app:(layout)\n  - app@1234abcd:old.rs\n  - other:x.md\n  - \"[[Save]]\"\nlens: arch\n---\n# Arch\nSee `app:README.md`.\n";
        let pinned = pin_sources(page, &heads);
        assert_eq!(
            pinned,
            "---\ntitle: Arch\nsources:\n  - app@0a1b2c3d4e5f:README.md\n  - \"app@0a1b2c3d4e5f:src/main.rs:12\"\n  - app:(layout)\n  - app@1234abcd:old.rs\n  - other:x.md\n  - \"[[Save]]\"\nlens: arch\n---\n# Arch\nSee `app:README.md`.\n",
            "only sources: entries of known repos, and never the text"
        );
        assert_eq!(pin_sources(&pinned, &heads), pinned, "pinning twice changes nothing");

        // A pin the draft left empty is Ken's: the commit of each cited repo.
        let heads: HashMap<String, String> = [("app".to_string(), "0a1b2c3d4e5f".to_string()), ("tools".to_string(), "99aa".to_string())].into();
        let one = "---\ntitle: Arch\npin:          # the commit the sources were read at\nsources:\n  - app:README.md\n---\n# Arch\n";
        assert!(pin_sources(one, &heads).contains("\npin: 0a1b2c3d4e5f\nsources:"), "{}", pin_sources(one, &heads));
        let two = "---\ntitle: Systems\npin: {{commit}}\nsources:\n  - tools:src/a.ts\n  - app:README.md\n---\n# S\n";
        assert!(pin_sources(two, &heads).contains("\npin: app@0a1b2c3d4e5f, tools@99aa\n"));
        let set = "---\npin: 1234\nsources:\n  - app:README.md\n---\n";
        assert!(pin_sources(set, &heads).contains("\npin: 1234\n"), "a pin already set stays");
    }

    /// A fact with no source is left out: the prompt says so, and what a
    /// model writes anyway is taken out of the page.
    #[test]
    fn unknowns_are_left_out_of_a_draft() {
        let p = prompt("Reference/Systems.md", "the systems", None, &[], "2026-10-06");
        assert!(!p.contains("write `(not in the sources yet)`") && p.contains("leave the fact out") && p.contains("`To fill: …`"), "{p}");
        assert!(p.contains("write the checkout's answer and cite it; do not write that the sources disagree"));
        let reply = "---\ntitle: Systems\npin: (not in the sources yet)\n---\n# Systems\n\n| system | does | code |\n|---|---|---|\n\
            | Quests | (not in the sources yet) | (not in the sources yet) |\n| Rifts | Runs a rift. | (not in the sources yet) |\n\n\
            - Owner: (not in the sources yet)\n\nA long paragraph about the rift runs that says the timer is (not in the sources yet) and the rest is plain words.\n\
            To fill: what each system does.\n";
        let page = finish(reply).unwrap();
        assert!(!page.contains("not in the sources"), "{page}");
        assert!(page.contains("\npin:\n") && !page.contains("| Quests |") && page.contains("| Rifts | Runs a rift. |  |"), "{page}");
        assert!(!page.contains("Owner:") && page.contains("says the timer is  and the rest") && page.contains("To fill: what each system does."), "{page}");
    }

    #[test]
    fn only_missing_or_untouched_template_pages_are_drafted() {
        assert!(may_draft(None));
        assert!(may_draft(Some("---\ntitle: \"Team\"\n---\n| {{role}} | {{name}} |\n")));
        assert!(!may_draft(Some("---\ntitle: Team\n---\nAna leads.\n")));
    }

    #[test]
    fn a_draft_is_marked_draft_and_never_verified() {
        let page = finish("```\n---\ntitle: Project\nstatus: current\nverified: 2026-09-24\nsources:\n  - game:README.md\n---\n# Project\n```").unwrap();
        assert!(page.contains("status: draft") && !page.contains("verified:") && page.contains("game:README.md"));
        assert!(finish("Here you go").is_err());
    }

    #[test]
    fn a_code_repo_gets_its_layers_page_and_a_reference_repo_a_dependency_page() {
        use crate::registry::RepoKind;
        let pages = |kinds: &[RepoKind]| kind_pages("team/Game", kinds).into_iter().map(|(p, b, _)| (p, b)).collect::<Vec<_>>();
        let arch = ("Conventions/Architecture-Game.md".to_string(), "Templates/Repo-Architecture.md");
        assert_eq!(pages(&[RepoKind::Code]), vec![arch]);
        assert_eq!(pages(&[RepoKind::Reference]), vec![("Platform/Game.md".to_string(), "Templates/Dependency.md")]);
        assert_eq!(pages(&[RepoKind::Code, RepoKind::Reference]).len(), 1, "a reference repo never gets an architecture page");
        assert!(pages(&[RepoKind::Team]).is_empty() && pages(&[]).is_empty());

        // The blank, its repo named, is the template while the page is missing.
        let wiki = tempfile::tempdir().unwrap();
        let blank = blank_for(wiki.path(), "Templates/Repo-Architecture.md", "team/Game");
        assert!(blank.contains("title: \"Game Architecture\"") && !blank.contains("{{Repo}}") && blank.contains("{{layer name}}"));
        let dep = blank_for(wiki.path(), "Templates/Dependency.md", "Engine");
        assert!(dep.contains("# Engine\n") && dep.contains("[[{{Repo}} Architecture]]"), "a code repo's placeholder stays");
        let sources = vec![Source { label: "Game:README.md".into(), text: "A game.".into() }];
        let mut report = DraftReport::default();
        let mut sent = String::new();
        let mut generate = |p: &str| {
            sent = p.to_string();
            Ok("---\ntitle: Game Architecture\n---\n# Game Architecture\n".to_string())
        };
        let page = "Conventions/Architecture-Game.md";
        draft_page(wiki.path(), page, "layers", Some(&blank), &sources, "2026-10-06", &mut report, &mut generate, &PathCheck::default())
            .unwrap();
        assert!(sent.contains("TEMPLATE (fill it in") && sent.contains("# Game Architecture"));
        assert!(!sent.contains("Drafted by Ken on"), "no byline");
        assert_eq!(report.drafted, vec![page]);
    }

    #[test]
    fn a_wiki_is_drafted_around_what_a_person_wrote() {
        let wiki = tempfile::tempdir().unwrap();
        fs::create_dir_all(wiki.path().join("Current")).unwrap();
        fs::write(wiki.path().join("Current/Team.md"), "---\ntitle: Team\n---\nAna leads; Ben reviews.\n").unwrap();
        fs::write(wiki.path().join("Current/Project.md"), "---\ntitle: \"Project\"\n---\n{{one paragraph}}\n").unwrap();
        let sources = vec![Source { label: "game:README.md".into(), text: "A tactics game.".into() }];
        let mut db = Db::open_in_memory().unwrap();
        let mut prompts: Vec<String> = Vec::new();
        let report = draft(wiki.path(), &mut db, &sources, "2026-09-24", 5, |p| {
            prompts.push(p.to_string());
            Ok("---\ntitle: Drafted\nsources:\n  - game:README.md\n---\n# Drafted\nDrafted by Ken.\n".into())
        })
        .unwrap();
        assert_eq!(report.kept, vec!["Current/Team.md"]);
        let others: Vec<&str> = PAGES.iter().map(|(p, _)| *p).filter(|p| *p != "Current/Team.md").collect();
        assert_eq!(report.drafted, others);
        assert!(prompts[0].contains("{{one paragraph}}"), "the template page's own text is the template");
        assert!(prompts.iter().all(|p| p.contains("=== game:README.md ===")));
        assert_eq!(fs::read_to_string(wiki.path().join("Current/Team.md")).unwrap(), "---\ntitle: Team\n---\nAna leads; Ben reviews.\n");
        assert!(fs::read_to_string(wiki.path().join("Conventions/Architecture.md")).unwrap().contains("status: draft"));
        let kept = last_draft(&db).unwrap();
        assert!(kept.title.starts_with("First wiki drafted") && kept.report.kept == vec!["Current/Team.md"]);
        assert!(db.open_review_item_of_kind(REVIEW_KIND).unwrap().is_none(), "no Review item");
        let found = findings(wiki.path(), &db).unwrap();
        assert!(found.iter().any(|f| f.kind == "draft" && f.path == "Conventions/Architecture.md"), "{found:?}");
    }

    #[test]
    fn the_draft_reads_which_folders_call_which_from_the_code() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for (p, t) in [
            ("app/api/users.py", "from app.services.users import find\n\ndef route():\n    return find()\n"),
            ("app/api/teams.py", "from app.services.users import find\nfrom app.services import teams\n"),
            ("app/services/users.py", "from app.db.repo import load\n\ndef find():\n    return load()\n"),
            ("app/services/teams.py", "def list_teams():\n    return []\n"),
            ("app/services/__init__.py", ""),
            ("app/db/repo.py", "from app.services.teams import list_teams\n\ndef load():\n    return 1\n"),
            ("README.md", "# App\n"),
        ] {
            fs::create_dir_all(root.join(p).parent().unwrap()).unwrap();
            fs::write(root.join(p), t).unwrap();
        }
        let git = |args: &[&str]| assert!(Command::new("git").args(args).current_dir(root).output().unwrap().status.success());
        git(&["init", "-q"]);
        git(&["add", "-A"]);
        let deps = folder_imports(root).expect("imports between folders");
        assert_eq!(
            deps,
            "app/api -> app/services (3)\napp/db -> app/services (1)\napp/services -> app/db (1)\n\n\
             import each other (a -> b / b -> a):\napp/db <-> app/services (1 / 1)\n\n\
             most imported files (how many files import each):\napp/services/users.py (2)"
        );
        let labels: Vec<String> = gather_repo("app", root, &[]).into_iter().map(|s| s.label).collect();
        assert!(labels.contains(&"app:(imports between folders, from the code)".to_string()), "{labels:?}");
    }

    #[test]
    fn release_notes_read_what_landed_since_the_newest_tag() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .current_dir(root)
                .output()
                .unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        };
        git(&["init", "-q"]);
        let commit = |msg: &str| {
            fs::write(root.join("log.txt"), msg).unwrap();
            git(&["add", "-A"]);
            git(&["commit", "-q", "-m", msg]);
        };
        commit("first cut");
        let (label, log) = git_unreleased(root).unwrap();
        assert_eq!(label, "(recent changes, no tags)");
        assert!(log.ends_with("first cut"));

        git(&["tag", "v1"]);
        commit("add the Dallas market");
        commit("fix the brief export");
        let (label, log) = git_unreleased(root).unwrap();
        assert_eq!(label, "(changes since v1)");
        let subjects: Vec<&str> = log.lines().map(|l| l.split_once(' ').unwrap().1).collect();
        assert_eq!(subjects, vec!["fix the brief export", "add the Dallas market"]);
    }

    #[test]
    fn what_the_draft_could_not_fill_is_named_for_a_person() {
        let wiki = tempfile::tempdir().unwrap();
        crate::wikinew::create(wiki.path(), "Payments", &[], "2026-09-28").unwrap();
        let left = placeholders_left(wiki.path());
        assert!(left.contains(&"START-HERE.md: {{what a newcomer loses before finding it}}".to_string()), "{left:?}");
        assert!(left.contains(&"CLAUDE.md: {{a trap that costs time on this team, and the page that holds it}}".to_string()));
        assert!(!left.iter().any(|l| l.contains("{{Domain}}")), "Ken names the Domain section itself");
        assert!(!left.iter().any(|l| l.starts_with("Templates/")), "{left:?}");
        // Every page's `updated:` is dated; a date in a table or a count is a person's.
        for (page, _) in crate::wikinew::TEMPLATE.iter().filter(|(p, _)| p.ends_with(".md") && !p.starts_with("Templates/")) {
            let text = fs::read_to_string(wiki.path().join(page)).unwrap();
            assert!(!text.lines().any(|l| l.starts_with("updated:") && l.contains("{{")), "{page}");
        }
    }

    /// The reply of a check that finds every statement of the page borne
    /// out by its source; None for any other prompt.
    fn all_supported(prompt: &str) -> Option<String> {
        if !prompt.starts_with("You are checking one drafted page") {
            return None;
        }
        let ids: Vec<&str> = prompt.lines().filter_map(|l| l.split_once(" (line ").map(|(id, _)| id)).filter(|id| id.starts_with('S')).collect();
        Some(format!("[{}]", ids.iter().map(|id| format!("{{\"id\": \"{id}\", \"verdict\": \"supported\"}}")).collect::<Vec<_>>().join(", ")))
    }

    fn page(title: &str) -> String {
        format!("---\ntitle: {title}\nsources:\n  - x\n---\n# {title}\nDrafted by Ken.\n")
    }

    fn repo(dir: &Path, name: &str, readme: &str) -> (String, PathBuf) {
        let root = dir.join(name);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("README.md"), readme).unwrap();
        (name.to_string(), root)
    }

    #[test]
    fn a_team_wiki_is_drafted_repo_by_repo_then_from_the_repo_pages() {
        let d = tempfile::tempdir().unwrap();
        let wiki = d.path().join("Wiki");
        fs::create_dir_all(&wiki).unwrap();
        let repos = vec![repo(d.path(), "Game", "# Game\nThe client.\n"), repo(d.path(), "Tools", "# Tools\nThe editor.\n")];
        let mut db = Db::open_in_memory().unwrap();
        let mut prompts: Vec<String> = Vec::new();
        let report = draft_team(&wiki, "Wiki", &mut db, &repos, None, "2026-09-25", 5, |p| {
            prompts.push(p.to_string());
            Ok(page("Drafted"))
        })
        .unwrap();
        assert_eq!(&report.drafted[..3], &["Repo-Map/Game.md", "Repo-Map/Tools.md", "Repo-Map/Index.md"]);
        assert!(report.drafted.contains(&"Conventions/Architecture.md".to_string()));
        // Pass one reads one repo each.
        assert!(prompts[0].contains("=== Game:README.md ===") && !prompts[0].contains("Tools:README.md"));
        assert!(prompts[1].contains("=== Tools:README.md ===") && !prompts[1].contains("Game:README.md"));
        // Pass two reads the repo pages, and of the raw repos only the brief
        // sections that serve the page: the README's opening says what the
        // project is, so Project gets it and the architecture page does not.
        let team = &prompts[2];
        assert!(team.contains("=== Wiki:Repo-Map/Game.md:7 ===") && team.contains("=== Wiki:Repo-Map/Tools.md:7 ==="), "{team}");
        let arch = prompts.iter().find(|p| p.contains("`Conventions/Architecture.md`")).unwrap();
        assert!(team.contains("=== Game:README.md ===") && !arch.contains("=== Game:README.md ==="));
        let index = fs::read_to_string(wiki.join("Repo-Map/Index.md")).unwrap();
        assert!(index.contains("| [[Game]] | (not said yet) |") && index.contains("| [[Tools]] |"));
        // Every team page knows what each page owns, and its own share.
        assert!(team.contains("=== Wiki:(pages of this wiki, and the facts each owns) ===") && team.contains("[[Vocabulary]] (Reference/Vocabulary.md): the words that differ"));
        assert!(team.contains("It owns what the product is, who it is for"), "{team}");
    }

    /// One fact, one page: a team page reads only the Repo Map sections its
    /// words fit, never every repo page whole.
    #[test]
    fn a_team_page_reads_only_the_repo_page_sections_it_needs() {
        let d = tempfile::tempdir().unwrap();
        let wiki = d.path().join("Wiki");
        let game = repo(d.path(), "Game", "# Game\n");
        let page = "---\ntitle: Game\nsources:\n  - Game:README.md\n---\n# Game\n\nThe client.\n\n\
            ## How it is built and run\n\nRun `./gradlew build`, then the server task. `Game:build.gradle.kts`\n\n\
            ## Who owns it\n\nAna owns the repo; ask her. Owners are in CODEOWNERS. `Game:CODEOWNERS`\n";
        write(&wiki, &[("Repo-Map/Game.md", page)]);
        let repos = vec![game];
        let labels = |p: &str| -> Vec<String> {
            page_sources(p, &wiki, "Wiki", &repos, &[]).into_iter().map(|s| s.label).filter(|l| l.starts_with("Wiki:Repo-Map")).collect()
        };
        assert_eq!(labels("Reference/Build-and-Run.md"), vec!["Wiki:Repo-Map/Game.md:10"]);
        assert_eq!(labels("Current/Who-Does-What.md"), vec!["Wiki:Repo-Map/Game.md:14"]);
        assert_eq!(labels("Current/Project.md"), vec!["Wiki:Repo-Map/Game.md:6"], "the opening fits no page: Project and Architecture");
        assert!(labels("Reference/Vocabulary.md").is_empty());
        assert!(labels(START_HERE).is_empty());
    }

    #[test]
    fn every_repo_gets_its_own_budget_however_many_there_are() {
        let d = tempfile::tempdir().unwrap();
        let big = "word ".repeat(40_000);
        let repos: Vec<(String, PathBuf)> = (0..20).map(|i| repo(d.path(), &format!("r{i}"), &big)).collect();
        let s = gather(&repos, None);
        assert!(s.iter().any(|s| s.label == "r19:README.md"), "the last repo is still read");
    }

    #[test]
    fn an_added_repo_gets_its_page_and_kept_pages_get_proposals() {
        let d = tempfile::tempdir().unwrap();
        let wiki = d.path().join("Wiki");
        fs::create_dir_all(wiki.join("Repo-Map")).unwrap();
        fs::create_dir_all(wiki.join("Current")).unwrap();
        fs::create_dir_all(wiki.join("Conventions")).unwrap();
        let index = "---\ntitle: Repo Map\n---\n| repo | what it is for |\n|---|---|\n| [[Game]] | The client. |\n";
        fs::write(wiki.join("Repo-Map/Index.md"), index).unwrap();
        let arch = "---\ntitle: Architecture\n---\nGame calls the server.\n";
        fs::write(wiki.join("Conventions/Architecture.md"), arch).unwrap();
        fs::write(wiki.join("Current/Who-Does-What.md"), "---\ntitle: Who\n---\nAna owns Game.\n").unwrap();
        fs::write(wiki.join("Current/Project.md"), "---\ntitle: Project\n---\nA game.\n").unwrap();
        let game = repo(d.path(), "Game", "# Game\n");
        let tools = repo(d.path(), "Tools", "# Tools\nThe level editor.\n");
        let mut db = Db::open_in_memory().unwrap();
        let report = draft_added(&wiki, "Wiki", &mut db, &[tools.clone()], &[game, tools], "2026-09-25", 9, |p| {
            Ok(if p.contains("`Repo-Map/Tools.md`") {
                page("Tools")
            } else if p.contains("`Conventions/Architecture.md`") {
                "---\ntitle: Architecture\n---\nGame calls the server. Tools writes levels the Game loads.\n".into()
            } else if p.contains("`Reference/Vocabulary.md`") {
                page("Vocabulary")
            } else {
                "NO CHANGE".into()
            })
        })
        .unwrap();
        assert!(report.drafted.contains(&"Repo-Map/Tools.md".to_string()), "{report:?}");
        assert!(report.drafted.contains(&"Reference/Vocabulary.md".to_string()), "a missing page is drafted whole");
        assert_eq!(report.proposed, vec!["Repo-Map/Index.md", "Conventions/Architecture.md"]);
        assert_eq!(report.kept, vec!["Current/Who-Does-What.md", "Current/Project.md"]);
        assert_eq!(fs::read_to_string(wiki.join("Conventions/Architecture.md")).unwrap(), arch, "a kept page is never written");

        let items = db.list_open_review_items().unwrap();
        let mut props: Vec<Proposal> = items
            .iter()
            .filter(|i| i.kind == PROPOSAL_KIND)
            .map(|i| serde_json::from_str(i.payload.as_deref().unwrap()).unwrap())
            .collect();
        props.sort_by(|a, b| b.page.cmp(&a.page)); // Repo-Map/Index.md, then Conventions/…
        assert_eq!(props.len(), 2);
        assert!(props[0].proposed.ends_with("| [[Game]] | The client. |\n| [[Tools]] | (not said yet) |\n"));

        // Applied while the page reads as it did; refused once it changed.
        apply(&wiki, &props[1]).unwrap();
        assert!(fs::read_to_string(wiki.join("Conventions/Architecture.md")).unwrap().contains("Tools writes levels"));
        fs::write(wiki.join("Repo-Map/Index.md"), format!("{index}| [[Server]] | added by hand |\n")).unwrap();
        assert_eq!(apply(&wiki, &props[0]), Err(ApplyError::Changed));
    }

    #[test]
    fn an_update_reply_of_no_change_or_the_same_page_proposes_nothing() {
        let page = "---\ntitle: A\n---\nText.\n";
        assert_eq!(finish_update("NO CHANGE", page).unwrap(), None);
        assert_eq!(finish_update(page, page).unwrap(), None);
        assert!(finish_update("Sure, here it is", page).is_err());
        assert!(finish_update("---\ntitle: A\n---\nText and more.", page).unwrap().is_some());
    }
    #[test]
    fn a_removed_repo_lists_the_pages_that_cite_it() {
        let d = tempfile::tempdir().unwrap();
        let wiki = d.path().join("Wiki");
        fs::create_dir_all(wiki.join("Platform")).unwrap();
        fs::create_dir_all(wiki.join("Repo-Map")).unwrap();
        fs::write(wiki.join("Platform/Save.md"), "---\nsources:\n  - Tools:src/save.rs:3\n  - Game:src/x.rs\n---\n# Save\n").unwrap();
        fs::write(wiki.join("Platform/Combat.md"), "---\nsources:\n  - Game:src/hit.rs\n---\n# Combat\n").unwrap();
        fs::write(wiki.join("Repo-Map/Tools.md"), "---\ntitle: Tools\n---\n# Tools\n").unwrap();
        let project = crate::project::Project::create(&wiki, "Wiki").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        crate::scan::scan(&project, &mut db).unwrap();

        let mut pages = citing_pages(&db, "Tools").unwrap();
        pages.sort();
        assert_eq!(
            pages,
            vec![
                ("Platform/Save.md".to_string(), vec!["Tools:src/save.rs:3".to_string()]),
                ("Repo-Map/Tools.md".to_string(), vec![]),
            ]
        );
        assert_eq!(record_removed_repo(&mut db, "Tools").unwrap(), Some(2));
        assert_eq!(record_removed_repo(&mut db, "Tools").unwrap(), Some(2));
        assert_eq!(removed_repos(&db), vec!["Tools"]);
        let found = findings(&wiki, &db).unwrap();
        let gone = found.iter().find(|f| f.kind == "repo-removed").unwrap();
        assert!(gone.title.starts_with("Tools left the team: 2 pages") && gone.detail.contains("Platform/Save.md"), "{gone:?}");
        assert_eq!(record_removed_repo(&mut db, "Nobody").unwrap(), None);
    }

    #[test]
    fn a_wiki_covers_its_teams_repos_and_a_repo_finds_its_teams_wiki() {
        use crate::registry::RepoKind;
        let m = |name: &str, kind: RepoKind, team: Option<&str>| TeamMember {
            name: name.into(),
            root: PathBuf::from(name),
            kind: vec![kind],
            team: team.map(String::from),
        };
        let members = vec![
            m("Realms-Wiki", RepoKind::Wiki, Some("Realms")),
            m("Game", RepoKind::Code, Some("Realms")),
            m("Server", RepoKind::Code, Some("Realms")),
            m("Ops-Wiki", RepoKind::Wiki, Some("Ops")),
            m("Infra", RepoKind::Code, Some("Ops")),
            m("Scratch", RepoKind::Code, None),
        ];
        let names = |v: Vec<(String, PathBuf)>| v.into_iter().map(|(n, _)| n).collect::<Vec<_>>();
        assert_eq!(names(team_repos("Realms-Wiki", &members)), vec!["Game", "Server"]);
        assert_eq!(names(team_repos("Ops-Wiki", &members)), vec!["Infra"]);
        assert_eq!(wiki_for("Server", &members).map(|w| w.name.as_str()), Some("Realms-Wiki"));
        assert_eq!(wiki_for("Scratch", &members), None, "no team wiki, no update");
    }

    /// Run git in `root` as a test identity; panics when git fails.
    fn git_in(root: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn write(root: &Path, files: &[(&str, &str)]) {
        for (p, t) in files {
            fs::create_dir_all(root.join(p).parent().unwrap()).unwrap();
            fs::write(root.join(p), t).unwrap();
        }
    }

    /// Item 1: a brief is read by section, and each page gets the sections
    /// that serve it, however deep in the brief they are.
    #[test]
    fn a_brief_is_read_by_section_and_each_page_gets_its_sections() {
        let filler = "Weapons are swords and bows.\n".repeat(400); // ~11,600 characters
        let claude = format!(
            "# Game\n\nA tactics game for Hytale.\n\n## Weapon Types\n\n{filler}\n## Build & Run\n\n```\n## not a heading\n./gradlew build\n```\n\n\
             ## Key Architectural Patterns\n\nSystems may call services; services never call systems.\n\n\
             ### Wiring order\n\nInstallers run in order.\n\n## Common Gotchas\n\nEnergy is called Stamina in the engine.\n\n\
             ## Team\n\nAna decides balance.\n"
        );
        let sections = brief_sections(&claude);
        let headings: Vec<&str> = sections.iter().map(|s| s.heading.as_str()).collect();
        assert_eq!(headings, vec!["", "Weapon Types", "Build & Run", "Key Architectural Patterns", "Common Gotchas", "Team"]);
        assert!(sections[3].text.contains("### Wiring order"), "a ### stays inside its section");
        assert!(sections[2].text.contains("## not a heading"), "never split inside a code fence");
        let topics: Vec<Topic> = headings.iter().map(|h| topic_of(h)).collect();
        assert_eq!(topics, vec![Topic::Project, Topic::Other, Topic::Build, Topic::Architecture, Topic::Traps, Topic::People]);
        assert_eq!(topic_of("Project Structure"), Topic::Architecture);
        assert_eq!(topic_of("Who decides"), Topic::People);
        assert_eq!(topic_of("Wholesale prices"), Topic::Other, "a whole word, not a prefix of one");

        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("Game");
        write(&root, &[("CLAUDE.md", &claude)]);
        let line_of = |h: &str| claude.lines().position(|l| l == h).unwrap() + 1;
        let arch = format!("Game:CLAUDE.md:{}", line_of("## Key Architectural Patterns"));

        // The repo page: building, structure and what it is first, then the rest.
        let labels: Vec<String> = gather_repo("Game", &root, &[]).into_iter().map(|s| s.label).collect();
        let at = |l: &str| labels.iter().position(|x| x == l).unwrap_or_else(|| panic!("{l} in {labels:?}"));
        let build = format!("Game:CLAUDE.md:{}", line_of("## Build & Run"));
        let weapons = format!("Game:CLAUDE.md:{}", line_of("## Weapon Types"));
        assert!(at(&build) < at(&weapons) && at(&arch) < at(&weapons), "{labels:?}");
        assert!(labels.contains(&"Game:(headings of its briefs, with their lines)".to_string()));

        // A team page: only what serves it.
        let wiki = d.path().join("Wiki");
        let repos = vec![("Game".to_string(), root.clone())];
        let of = |page: &str| -> Vec<Source> { page_sources(page, &wiki, "Wiki", &repos, &[]) };
        let architecture = of("Conventions/Architecture.md");
        assert!(architecture.iter().any(|s| s.label == arch && s.text.contains("services never call systems")), "{architecture:?}");
        assert!(!architecture.iter().any(|s| s.text.contains("Ana decides")));
        assert!(of(START_HERE).iter().any(|s| s.text.contains("called Stamina")), "a gotcha is a trap");
        let vocabulary = of("Reference/Vocabulary.md");
        let line = line_of("Energy is called Stamina in the engine.");
        assert!(vocabulary.iter().any(|s| s.text.contains(&format!("Game:CLAUDE.md:{line}: Energy is called Stamina"))), "{vocabulary:?}");
        assert!(of("Current/Team.md").iter().any(|s| s.text.contains("Ana decides")));
        assert!(of("Current/Project.md").iter().any(|s| s.label == "Game:CLAUDE.md" && s.text.contains("tactics game")));

        // A repo that needs less than an equal share leaves the rest to the others.
        assert_eq!(fair_shares(&[40_000, 1_000, 8_000, 500], 30_000), vec![20_500, 1_000, 8_000, 500]);
        assert_eq!(fair_shares(&[40_000, 40_000], 30_000), vec![15_000, 15_000]);
    }

    /// Item 2: the build files are read, and a README once whatever its case.
    #[test]
    fn the_build_files_are_read_and_a_readme_once() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        let gradle = format!("plugins {{ java }}\n{}tasks.register(\"runServer\") {{\n}}\n", "// setting\n".repeat(80));
        write(
            root,
            &[
                ("README.md", "# Tools\nThe authoring suite.\n"),
                ("package.json", r#"{"name": "sr-tools", "scripts": {"start": "pnpm --filter shell tauri dev", "test": "vitest"}}"#),
                ("build.gradle.kts", &gradle),
                ("settings.gradle.kts", "rootProject.name = \"mod\"\n"),
                ("Cargo.toml", "[workspace]\nmembers = [\"crates/*\"]\n\n[dependencies]\nserde = \"1\"\n\n[[bin]]\nname = \"sr-jar\"\n"),
                ("pyproject.toml", "[project]\nname = \"x\"\n\n[project.scripts]\nsr-log = \"tools.log:main\"\n"),
                ("Makefile", "CC = cc\n# build it\nbuild: deps\n\tcargo build --release\n\ntest:\n\tcargo test\n.PHONY: build\n"),
                (".ken/gates.json", "{\"gates\": {\"G2\": {\"tests\": \"./gradlew build --offline\"}}}"),
                ("INSTALL.md", "# Install\nRun tool.cmd.\n"),
            ],
        );
        assert_eq!(top_files(root, &["README.md", "readme.md", "Readme.md"]).len(), 1, "one file, read once");
        let s = gather_repo("tools", root, &[]);
        let text = |label: &str| s.iter().find(|x| x.label == label).map(|x| x.text.clone()).unwrap_or_else(|| panic!("{label}"));
        assert_eq!(s.iter().filter(|x| x.label.to_lowercase().starts_with("tools:readme")).count(), 1);
        assert!(text("tools:package.json").contains("start: pnpm --filter shell tauri dev"));
        let g = text("tools:build.gradle.kts");
        assert!(g.contains("plugins { java }") && g.contains("82: tasks.register(\"runServer\")"), "{g}");
        assert!(text("tools:settings.gradle.kts").contains("rootProject.name"));
        let cargo = text("tools:Cargo.toml");
        assert!(cargo.contains("[workspace]") && cargo.contains("[[bin]]") && !cargo.contains("serde"), "{cargo}");
        let py = text("tools:pyproject.toml");
        assert!(py.contains("sr-log") && !py.contains("name = \"x\""));
        let make = text("tools:Makefile");
        assert!(make.contains("build: deps\n\tcargo build --release\n") && make.contains("test:") && !make.contains("CC = cc"), "{make}");
        assert!(text("tools:.ken/gates.json").contains("--offline"));
        assert!(text("tools:INSTALL.md").contains("tool.cmd"));
        assert!(text("tools:(layout)").contains(".ken/"), "the gates folder is in the layout");
    }

    /// Item 3: the paths a page names are checked against what git tracks,
    /// and the page goes back once, with where the code has each now.
    #[test]
    fn paths_a_page_names_are_checked_and_corrected_once() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("game");
        write(
            &root,
            &[
                ("README.md", "# Game\n"),
                (".gitignore", "build/\n"),
                ("src/main/java/dev/app/services/combat/CombatCalculator.java", "class CombatCalculator {}\n"),
                ("src/main/java/dev/app/utils/SrLog.java", "class SrLog {}\n"),
                ("src/main/resources/data/equipment/WeaponBases.json", "{}\n"),
            ],
        );
        git_in(&root, &["init", "-q"]);
        git_in(&root, &["add", "-A"]);
        let check = PathCheck::of(&[("game".to_string(), root.clone())]);
        let page = "---\ntitle: Game\nsources:\n  - game:README.md\n  - game:docs/OLD.md\n  - game:(layout)\n---\n# Game\n\
            Damage maths is `utils/CombatCalculator.java` (`utils/CombatCalculator`); configs are `data/*.json` and `data/equipment/`.\n\
            The calculator is `services/combat/CombatCalculator.java`, the class `services/combat/CombatCalculator` \
            (`game:README.md:3`). The repo is `Stingbro/game` on branch `claude/brave-otter`; the jar is \
            `build/libs/game.jar`; `gone/Nothing.java` was removed; see `src/.../SrLog.java`.\n\
            ```\nsrc/never/Checked.java\n```\n";
        let missing = check.missing(page, Some("game"));
        let paths: Vec<&str> = missing.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, vec!["docs/OLD.md", "utils/CombatCalculator.java", "utils/CombatCalculator", "data/*.json", "gone/Nothing.java"]);
        assert_eq!(missing[0].repo.as_deref(), Some("game"));
        let calculator = "game:src/main/java/dev/app/services/combat/CombatCalculator.java";
        assert_eq!(missing[1].now, vec![calculator]);
        assert_eq!(missing[2].now, vec![calculator], "a class named without its extension");
        assert_eq!(missing[3].now, vec!["game:src/main/resources/data/"], "a pattern's folder");
        assert!(missing[4].now.is_empty());

        let calls: std::cell::RefCell<Vec<String>> = Default::default();
        let mut generate = |p: &str| -> Result<String> {
            calls.borrow_mut().push(p.to_string());
            Ok("---\ntitle: Game\n---\n# Game\nStill names `gone/Nothing.java`.\n".into())
        };
        let mut report = DraftReport::default();
        let fixed = check_paths("Repo-Map/game.md", finish(page).unwrap(), &check, &mut report, &mut generate);
        let first = calls.borrow()[0].clone();
        assert_eq!(calls.borrow().len(), 1, "one correction call, never a loop");
        // A path with one match is rewritten in place; the call gets the rest.
        let full = "src/main/java/dev/app/services/combat/CombatCalculator.java";
        assert!(first.contains(&format!("Damage maths is `{full}` (`{full}`)")), "{first}");
        assert!(!first.contains("- path `utils/CombatCalculator"), "{first}");
        assert!(first.contains("- path `data/*.json`: not there; a file or folder of that name is at `game:src/main/resources/data/`"), "{first}");
        assert!(first.contains("- path `gone/Nothing.java`: no repo of the team has a file or folder of that name"));
        assert!(fixed.contains("status: draft") && fixed.contains("Still names"));
        assert_eq!(
            report.corrected,
            vec![
                format!("Repo-Map/game.md: `utils/CombatCalculator.java` -> `game:{full}` (rewritten, one match)"),
                format!("Repo-Map/game.md: `utils/CombatCalculator` -> `game:{full}` (rewritten, one match)"),
                "Repo-Map/game.md: sent back once for paths `docs/OLD.md`, `data/*.json`, `gone/Nothing.java`".to_string(),
            ]
        );
        // Only a path with one match: rewritten, bare and cited, and no call.
        let one = "---\ntitle: Game\nsources:\n  - game:log/SrLog.java\n---\nLogging is `log/SrLog.java`.\n".to_string();
        let fixed = check_paths("x.md", one, &check, &mut report, &mut generate);
        assert_eq!(calls.borrow().len(), 1, "a single match costs no call");
        assert!(fixed.contains("  - game:src/main/java/dev/app/utils/SrLog.java\n"), "{fixed}");
        assert!(fixed.contains("Logging is `src/main/java/dev/app/utils/SrLog.java`."), "{fixed}");
        // A brace list is one path per name in it, and a name is checked.
        let braces = "---\ntitle: G\n---\nBases are `src/main/resources/data/equipment/{WeaponBases,ArmourBases}.json`.\n";
        let paths: Vec<String> = check.missing(braces, None).into_iter().map(|m| m.path).collect();
        assert_eq!(paths, vec!["src/main/resources/data/equipment/ArmourBases.json"]);
        let clean = "---\ntitle: Game\n---\nSee `src/main/resources/data/equipment/WeaponBases.json`.\n".to_string();
        assert_eq!(check_paths("x.md", clean.clone(), &check, &mut report, &mut generate), clean);
        assert_eq!(calls.borrow().len(), 1, "a clean page costs no call");
    }

    /// A name written as code must be in the code, and a version must be the
    /// one the build file pins; both go into the page's one correction call.
    #[test]
    fn names_and_versions_a_page_states_are_checked_against_the_checkout() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("game");
        write(
            &root,
            &[
                ("gradle.properties", "# 0.7.0-pre.2 -> 0.7.0-pre.3 on 2026-09-17\nhytale_version = 0.7.0-pre.5\nversion = 0.0.15\n"),
                ("src/test/java/dev/app/ArchitectureRulesTest.java", "class ArchitectureRulesTest {\n  void freezesGetInstance() {}\n}\n"),
                ("src/test/java/dev/app/ItemNameLineTest.java", "// replaces OneNameLineComposerTest\nclass ItemNameLineTest {}\n"),
                ("src/main/resources/data/combat/Causes.json", "{\"WeaponModMultiplier\": 1.17}\n"),
            ],
        );
        git_in(&root, &["init", "-q"]);
        git_in(&root, &["add", "-A"]);
        let check = PathCheck::of(&[("game".to_string(), root.clone())]);
        let page = "---\ntitle: Game\nsources:\n  - game:gradle.properties\n---\n\
            `gradle.properties` pins the engine at 0.7.0-pre.3 (`hytale_version`).\n\
            `ArchitectureRulesTest` freezes `getInstance()` via `freezesGetInstance`; `OneNameLineComposerTest` guards names.\n\
            `WeaponModMultiplier` is 1.17; energy is `Stamina` in `JSON`. Releases 0.0.14 and 0.0.15 shipped.\n\
            ```\nOldGoneTest 0.7.0-pre.1\n```\n";
        let found = check.check("Platform/Index.md", page, None);
        assert!(found.paths.is_empty(), "{:?}", found.paths);
        // `getInstance` is inside `freezesGetInstance`; a comment naming a test is not the test.
        assert_eq!(found.names, vec!["OneNameLineComposerTest"]);
        let v: Vec<(&str, &str)> = found.versions.iter().map(|v| (v.written.as_str(), v.pin.value.as_str())).collect();
        assert_eq!(v, vec![("0.7.0-pre.3", "0.7.0-pre.5")], "history in a release list is not a pin");
        let p = correction_prompt("Platform/Index.md", page, &found);
        assert!(p.contains("- name `OneNameLineComposerTest`: written as code, and no repo defines it"), "{p}");
        assert!(p.contains("- version `0.7.0-pre.3`: `game:gradle.properties` has `hytale_version = 0.7.0-pre.5`"), "{p}");
        assert_eq!(
            found_list(&found),
            "names `OneNameLineComposerTest`; versions `0.7.0-pre.3` (game:gradle.properties hytale_version = 0.7.0-pre.5)"
        );
    }

    /// Item 4: the import map names folders below the source root, so a
    /// Java package tree is not one folder, and a cycle shows.
    #[test]
    fn the_import_map_names_folders_from_the_source_root() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        let b = "src/main/java/dev/app";
        write(
            root,
            &[
                (&format!("{b}/App.java"), "package dev.app;\nimport dev.app.systems.combat.Hit;\nclass App {}\n"),
                (&format!("{b}/services/combat/Calc.java"), "package dev.app.services.combat;\nimport dev.app.systems.combat.Hit;\nclass Calc {}\n"),
                (&format!("{b}/systems/combat/Hit.java"), "package dev.app.systems.combat;\nimport dev.app.services.combat.Calc;\nclass Hit {}\n"),
                (&format!("{b}/systems/spawn/Spawn.java"), "package dev.app.systems.spawn;\nimport dev.app.services.loot.Loot;\nclass Spawn {}\n"),
                (&format!("{b}/services/loot/Loot.java"), "package dev.app.services.loot;\nclass Loot {}\n"),
            ],
        );
        git_in(root, &["init", "-q"]);
        git_in(root, &["add", "-A"]);
        let map = folder_imports(root).expect("an import map");
        assert!(map.contains(&format!("{b}/services/combat -> {b}/systems/combat (1)")), "{map}");
        assert!(map.contains(&format!("{b} -> {b}/systems/combat (1)")), "a file at the root is its own folder: {map}");
        assert!(map.contains(&format!("{b}/services <-> {b}/systems (1 / 2)")), "one level down: {map}");
        assert!(map.contains(&format!("{b}/services/combat <-> {b}/systems/combat (1 / 1)")), "two levels down: {map}");
        assert!(!map.contains("src/main/java/dev ->"), "never one folder for the whole tree: {map}");
    }

    /// A workspace package imported by its name, from a script or a Svelte
    /// component, counts as an import of its folder.
    #[test]
    fn the_import_map_resolves_workspace_packages_by_name() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        write(
            root,
            &[
                ("package.json", r#"{"name": "tools", "private": true}"#),
                ("packages/schema/package.json", r#"{"name": "@t/schema"}"#),
                ("packages/schema/src/index.ts", "export const x = 1;\n"),
                ("packages/schema/src/types.ts", "export type T = number;\n"),
                ("packages/ui/package.json", r#"{"name": "@t/ui"}"#),
                ("packages/ui/src/lib/index.ts", "import { x } from '@t/schema';\n"),
                ("apps/shell/src/routes/+page.svelte", "<script lang=\"ts\">\nimport { x } from '@t/schema';\nimport Button from '@t/ui';\n</script>\n"),
                ("apps/shell/src/lib/load.ts", "import type { T } from '@t/schema/types';\nimport './helper';\n"),
                ("apps/shell/src/lib/helper.ts", "export {};\n"),
                ("crates/sr-jar/Cargo.toml", "[package]\nname = \"sr-jar\"\n"),
                ("crates/sr-jar/src/lib.rs", "pub fn read() {}\n"),
                ("apps/shell/src-tauri/src/main.rs", "use sr_jar::read;\nfn main() {}\n"),
            ],
        );
        git_in(root, &["init", "-q"]);
        git_in(root, &["add", "-A"]);
        let map = folder_imports(root).expect("an import map");
        for edge in [
            "apps/shell/src/routes -> packages/schema/src (1)",
            "apps/shell/src/lib -> packages/schema/src (1)",
            "apps/shell/src/routes -> packages/ui/src/lib (1)",
            "packages/ui/src/lib -> packages/schema/src (1)",
            "apps/shell/src-tauri/src -> crates/sr-jar/src (1)",
        ] {
            assert!(map.contains(edge), "{edge}: {map}");
        }
        assert!(map.contains("packages/schema/src/index.ts (2)"), "the entry is the hub: {map}");
    }

    /// Item 5: only version tags count, all of them, and a changelog is split
    /// by version.
    #[test]
    fn releases_read_version_tags_only_and_each_versions_notes() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        git_in(root, &["init", "-q"]);
        let commit = |msg: &str| {
            fs::write(root.join("log.txt"), msg).unwrap();
            git_in(root, &["add", "-A"]);
            git_in(root, &["commit", "-q", "-m", msg]);
        };
        commit("first cut");
        for i in 1..42 {
            git_in(root, &["tag", &format!("v0.0.{i}")]);
        }
        commit("ship 0.0.42");
        git_in(root, &["tag", "v0.0.42"]);
        commit("add duels");
        git_in(root, &["tag", "archive/ui-faction-2026-09-30"]);
        commit("fix the crossbow");
        let (label, log) = git_unreleased(root).unwrap();
        assert_eq!(label, "(changes since v0.0.42)", "the newest version tag, never the newer archive tag");
        assert!(log.contains("add duels") && log.contains("fix the crossbow") && !log.contains("ship 0.0.42"));
        // A busy range keeps every day, each cut short with a count.
        let busy: String = (0..300).map(|i| format!("2026-10-05 change {i} on the busy day\n")).chain(["2026-09-27 the first change after the tag\n".to_string()]).collect();
        let clipped = clip_by_day(busy.trim_end(), 2_000);
        assert!(clipped.len() <= 2_000 && clipped.contains("2026-09-27 the first change after the tag"), "{clipped}");
        assert!(clipped.contains("2026-10-05 (… ") && clipped.contains(" more that day)"), "{clipped}");
        let tags = git_tags(root).unwrap();
        assert_eq!(tags.lines().count(), 42, "every version tag");
        assert!(!tags.contains("archive/"));

        let changelog = "# Changelog\n\nIntro.\n\n## [Unreleased]\n\n- duels\n\n## 0.0.15 — 2026-09-25\n\n### Added\n\n- realms\n\n## v0.0.14\n\n- polygon zones\n";
        let v = version_sections(changelog);
        let heads: Vec<&str> = v.iter().map(|s| s.heading.as_str()).collect();
        assert_eq!(heads, vec!["[Unreleased]", "0.0.15 — 2026-09-25", "v0.0.14"]);
        assert!(v[1].text.contains("### Added") && v[1].text.contains("realms") && !v[1].text.contains("polygon"));
        write(root, &[("CHANGELOG.md", changelog)]);
        let notes = page_sources("Work/Releases.md", root, "Wiki", &[("game".to_string(), root.to_path_buf())], &[]);
        let labels: Vec<&str> = notes.iter().map(|s| s.label.as_str()).collect();
        assert!(labels.contains(&"game:CHANGELOG.md:5") && labels.contains(&"game:CHANGELOG.md:9") && labels.contains(&"game:CHANGELOG.md:15"), "{labels:?}");
        assert!(labels.contains(&"game:(version tags, newest first)") && labels.contains(&"game:(changes since v0.0.42)"));
    }

    /// Item 6: the layout shows the dot-folders that matter, every entry of a
    /// big folder, and folders further down.
    #[test]
    fn the_layout_shows_meaningful_dot_folders_and_deeper_data() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        let mut files: Vec<(String, String)> = (0..20).map(|i| (format!("packages/p{i:02}/package.json"), "{}".to_string())).collect();
        files.push((".ken/gates.json".into(), "{}".into()));
        files.push((".github/workflows/ci.yml".into(), "on: push\n".into()));
        files.push((".claude/settings.json".into(), "{}".into()));
        files.push(("src/main/resources/data/mobs/t1/Skeleton.json".into(), "{}".into()));
        let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        write(root, &refs);
        git_in(root, &["init", "-q"]);
        git_in(root, &["add", "-A"]);
        let text = layout(root, &ls_files(root).unwrap());
        assert!(text.contains(".ken/\n  gates.json") && text.contains(".github/\n  workflows/"), "{text}");
        assert!(!text.contains(".claude"), "{text}");
        assert!(text.contains("  p19/"), "no 15-entry cap: {text}");
        assert!(text.contains("src/main/resources/data/mobs/\n") && text.contains("src/main/resources/data/mobs/t1/\n"), "{text}");
    }

    /// Item 7: git adds only recent hands, by email, without bots, in UTF-8.
    #[test]
    fn recent_contributors_are_people_by_email_without_bots() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        git_in(root, &["init", "-q"]);
        let mut n = 0;
        let mut commit = |who: &str, file: &str| {
            n += 1;
            write(root, &[(file, &n.to_string())]);
            git_in(root, &["add", "-A"]);
            git_in(root, &["commit", "-q", "--author", who, "-m", &format!("change {n}")]);
        };
        commit("Zoë Ångström <zoe@example.com>", "data/spawns/a.json");
        commit("Zoë Ångström <zoe@example.com>", "data/spawns/b.json");
        commit("Chris <14892384+Stingbro@users.noreply.github.com>", "data/spawns/c.json");
        commit("Stingbro <99+stingbro@users.noreply.github.com>", "src/x.rs");
        commit("Chris <14892384+Stingbro@users.noreply.github.com>", "src/y.rs");
        commit("dependabot[bot] <49699333+dependabot[bot]@users.noreply.github.com>", "src/z.rs");
        commit("Claude <noreply@anthropic.com>", "src/w.rs");
        commit("Hytale Sync Bot <sync@hytale.example>", "src/v.rs");
        commit("Cursor Agent <cursoragent@cursor.com>", "README.md");
        let rows = recent_contributors(root, &[]).unwrap();
        assert!(rows.contains("data/: Zoë Ångström (2), Chris (1)"), "UTF-8 names, most first: {rows}");
        assert!(rows.contains("src/: Chris (2)\n"), "one person per GitHub id, under the name used most: {rows}");
        assert!(!rows.contains("(top-level files)"), "a bot's only folder has no row: {rows}");
        for bot in ["dependabot", "Claude", "Sync Bot", "Cursor"] {
            assert!(!rows.contains(bot), "{bot}: {rows}");
        }
        assert!(is_bot("github-actions[bot]", "41898282+github-actions[bot]@users.noreply.github.com"));
        assert!(is_bot("Copilot", "x@y") && !is_bot("Abbot Smith", "abbot@example.com"));

        let p = prompt("Current/Team.md", "the team", None, &[], "2026-10-06");
        assert!(p.contains("Never write a role nobody stated") && p.contains("Never speculate whether two names are one person"));
        write(root, &[("people/ana.md", "# Ana\nDecides balance.\n"), ("people/PERSON.md", "{{name}}\n"), ("CODEOWNERS", "data/ @ana\n")]);
        let s = page_sources("Current/Who-Does-What.md", root, "Wiki", &[("game".to_string(), root.to_path_buf())], &[]);
        let labels: Vec<&str> = s.iter().map(|x| x.label.as_str()).collect();
        assert!(labels.contains(&"game:CODEOWNERS") && labels.contains(&"game:people/ana.md"), "{labels:?}");
        assert!(labels.contains(&"game:(recent contributors by folder, last 90 days, from git)"));
        assert!(!labels.iter().any(|l| l.ends_with("PERSON.md")), "the template to copy is not a person");
    }

    /// One person who commits as three identities is counted once, under the
    /// name the roster gives them, and a roster person is never a bot.
    #[test]
    fn recent_contributors_count_a_roster_person_once() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        git_in(root, &["init", "-q"]);
        let mut n = 0;
        let mut commit = |who: &str, file: &str| {
            n += 1;
            write(root, &[(file, &n.to_string())]);
            git_in(root, &["add", "-A"]);
            git_in(root, &["commit", "-q", "--author", who, "-m", &format!("change {n}")]);
        };
        commit("Chris <14892384+Stingbro@users.noreply.github.com>", "src/a.rs");
        commit("Stingbro <noname@example.com>", "src/b.rs");
        commit("AlpahSignalAI <alpha@example.com>", "src/c.rs");
        commit("Ana Ruiz <ana@example.com>", "src/d.rs");
        commit("Mabel Bot <mabel@example.com>", "docs/e.md");
        commit("Hytale Sync Bot <sync@hytale.example>", "docs/f.md");

        // No roster: as before, one person per address.
        let rows = recent_contributors(root, &[]).unwrap();
        assert!(rows.contains("src/: AlpahSignalAI (1), Ana Ruiz (1), Chris (1)"), "{rows}");
        assert!(!rows.contains("docs/"), "{rows}");

        write(
            root,
            &[
                (
                    "people/chris.md",
                    "---
id: chris
name: Chris Rowe
emails: [14892384+Stingbro@users.noreply.github.com, noname@example.com, alpha@example.com]
aliases: [Stingbro, AlpahSignalAI]
---
",
                ),
                ("people/mabel.md", "---
id: mabel
name: Mabel Bot
---
"),
            ],
        );
        let roster = crate::people::roster(root);
        assert_eq!(roster.len(), 2);
        let rows = recent_contributors(root, &roster).unwrap();
        assert!(rows.contains("src/: Chris Rowe (3), Ana Ruiz (1)
"), "three identities, one person: {rows}");
        assert!(!rows.contains("AlpahSignalAI") && !rows.contains("Stingbro"), "{rows}");
        assert!(rows.contains("docs/: Mabel Bot (1)
"), "the roster beats the bot list: {rows}");
        assert!(!rows.contains("Sync Bot"), "a bot not on the roster stays out: {rows}");
    }

    /// Someone who owns one area shows up one level down, behind those who
    /// commit everywhere; and Team and Who Does What get the whole roster.
    #[test]
    fn an_area_owner_shows_one_level_down_and_the_roster_is_listed() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        git_in(root, &["init", "-q"]);
        let mut n = 0;
        let mut commit = |who: &str, file: &str| {
            n += 1;
            write(root, &[(file, &n.to_string())]);
            git_in(root, &["add", "-A"]);
            git_in(root, &["commit", "-q", "--author", who, "-m", &format!("change {n}")]);
        };
        for i in 0..4 {
            commit("Chris <chris@example.com>", &format!("src/main/java/dev/app/A{i}.java"));
            commit("Stingbro <sting@example.com>", &format!("src/main/java/dev/app/B{i}.java"));
            commit("AlpahSignalAI <alpha@example.com>", &format!("src/main/resources/Server/Item/C{i}.json"));
        }
        commit("Anjewist <andrew@example.com>", "src/main/resources/data/spawns/zone.json");
        let rows = recent_contributors(root, &[]).unwrap();
        assert!(rows.starts_with("src/: AlpahSignalAI (4), Chris (4), Stingbro (4)\n"), "{rows}");
        assert!(rows.contains("src/main/resources/data/spawns/: Anjewist (1)\n"), "{rows}");
        assert!(!rows.contains("src/main/java/dev/: "), "nobody new there: {rows}");
        assert_eq!(second_level("src/main/resources/Server/Item/x.json").as_deref(), Some("src/main/resources/Server/Item/"));
        assert_eq!(second_level("packages/schema/src/index.ts").as_deref(), Some("packages/schema/"));
        assert_eq!(second_level("README.md"), None);

        write(root, &[("people/andrew.md", "---\nid: andrew\nname: Andrew\naliases: [Anjewist]\n---\n"), ("people/chris.md", "---\nid: chris\nname: Chris\n---\n")]);
        let roster = crate::people::roster(root);
        let s = page_sources("Current/Team.md", root, "Wiki", &[("game".to_string(), root.to_path_buf())], &roster);
        let listed = s.iter().find(|x| x.label == "Wiki:(the team's roster, from people/)").expect("the roster");
        assert_eq!(listed.text, "- Andrew (also Anjewist)\n- Chris");
        assert!(PAGES.iter().find(|(p, _)| *p == "Current/Who-Does-What.md").unwrap().1.contains("every person on the team's roster"));
    }

    /// Item 8: the index and START-HERE say what a repo is for once its page
    /// does, where set-up had nothing.
    #[test]
    fn a_repo_with_no_description_takes_its_purpose_from_its_page() {
        let tools_page = "---\ntitle: Tools\nsources:\n  - Tools:CLAUDE.md\n---\n\n# Tools\n\nDrafted by Ken on 2026-10-06 from the sources listed; not yet verified by a person.\n\n\
            ## What it is for\n\nIn the team's own words, this repo is \"the authoring suite\": the desktop tools the team uses. \
            It is a pnpm workspace. `Tools:CLAUDE.md`\n";
        assert_eq!(
            page_purpose(tools_page).as_deref(),
            Some("In the team's own words, this repo is \"the authoring suite\": the desktop tools the team uses.")
        );
        assert_eq!(page_purpose("---\ntitle: X\n---\n# X\nDrafted by Ken.\n"), None);

        let d = tempfile::tempdir().unwrap();
        let wiki = d.path().join("Wiki");
        let covered = vec![crate::wikinew::Covered { name: "Tools".into(), ..Default::default() }];
        crate::wikinew::create(&wiki, "Realms", &covered, "2026-10-06").unwrap();
        assert!(fs::read_to_string(wiki.join("START-HERE.md")).unwrap().contains("| `Tools` | (not said yet) |"));
        let repos = vec![repo(d.path(), "Tools", "# Tools\n")];
        // A git checkout, so a citation of it is one the check can read.
        git_in(&repos[0].1, &["init", "-q"]);
        git_in(&repos[0].1, &["add", "-A"]);
        let mut db = Db::open_in_memory().unwrap();
        draft_team(&wiki, "Wiki", &mut db, &repos, None, "2026-10-06", 5, |p| {
            Ok(all_supported(p).unwrap_or_else(|| if p.contains("`Repo-Map/Tools.md`") { tools_page.to_string() } else { page("Drafted") }))
        })
        .unwrap();
        let index = fs::read_to_string(wiki.join("Repo-Map/Index.md")).unwrap();
        assert!(index.contains("| [[Tools]] | In the team's own words, this repo is \"the authoring suite\""), "{index}");
        let start = fs::read_to_string(wiki.join("START-HERE.md")).unwrap();
        assert!(start.contains("| `Tools` | In the team's own words") && !start.contains("(not said yet)"), "{start}");
    }

    /// A long section is split at its `###` headings and then at blank lines,
    /// never cut, each part at its own line; and every part of a brief goes
    /// to at least one page and at most three, a part no topic claims included.
    #[test]
    fn every_part_of_a_brief_reaches_one_page_and_long_sections_are_split() {
        let gotchas: String = (0..6).map(|i| format!("### Trap {i}\n\n{}", "Never edit the generated manifest by hand.\n\n".repeat(60))).collect();
        let rules = format!("## Binding rules\n\n{}", "A rule a change must follow, with its reason.\n\n".repeat(400));
        let brief = format!(
            "# Game\n\nA tactics game.\n\n## Common Gotchas\n\n{gotchas}{rules}## Four repos\n\n\
             ### Where the base game's assets are\n\nThe engine's assets are in the platform jar.\n\n## Team\n\nAna decides balance.\n"
        );
        let parts = brief_sections(&brief);
        assert!(parts.iter().all(|s| s.text.len() <= SECTION_MAX), "nothing over the cap");
        assert_eq!(parts.iter().map(|s| s.text.as_str()).collect::<String>(), brief, "nothing cut, nothing repeated");
        for s in &parts {
            assert_eq!(brief.lines().nth(s.line - 1), s.text.lines().next(), "part at line {} starts there", s.line);
        }
        let trap = parts.iter().find(|s| s.heading == "Trap 3").unwrap();
        assert_eq!(trap.parent, "Common Gotchas");
        assert!(parts.iter().filter(|s| s.heading == "Binding rules").count() >= 2, "a long run of paragraphs is split at blank lines");

        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("Game");
        write(&root, &[("CLAUDE.md", &brief)]);
        let repos = vec![("Game".to_string(), root.clone())];
        let wiki = d.path().join("Wiki");
        let mut reached: HashMap<String, Vec<&str>> = HashMap::new();
        for page in PAGES.iter().map(|(p, _)| *p).chain([START_HERE]) {
            for s in page_sources(page, &wiki, "Wiki", &repos, &[]) {
                if s.label.starts_with("Game:CLAUDE.md") {
                    reached.entry(s.label).or_default().push(page);
                }
            }
        }
        for s in &parts {
            let label = part_label("Game", "CLAUDE.md", s.line);
            let pages = reached.get(&label).cloned().unwrap_or_default();
            assert!((1..=PART_PAGES).contains(&pages.len()), "{label} ({}) went to {pages:?}", s.heading);
        }
        assert_eq!(reached[&part_label("Game", "CLAUDE.md", trap.line)], vec![START_HERE]);
        let team = parts.iter().find(|s| s.heading == "Team").unwrap();
        assert_eq!(reached[&part_label("Game", "CLAUDE.md", team.line)], vec!["Current/Team.md"]);
    }

    /// START-HERE's Traps rows are drafted from the briefs' gotchas and
    /// spliced in; the rest of the page, set-up's repo table included, stays.
    #[test]
    fn start_here_gets_its_traps_rows_and_keeps_its_table() {
        let reply = "Here are the rows:\n| trap | what happens | page |\n|---|---|---|\n\
            | Base game assets live in the jar | A search of the repo finds nothing `Game:CLAUDE.md:7` | [[Platform]] |\n\
            | {{a placeholder}} | x | y |\n| Energy is Stamina | Searching energy finds nothing `Game:CLAUDE.md:9` | [[Vocabulary]] |\n";
        let rows = traps_rows(reply);
        assert_eq!(rows.len(), 2, "{rows:?}");

        let d = tempfile::tempdir().unwrap();
        let wiki = d.path().join("Wiki");
        let covered = vec![crate::wikinew::Covered { name: "Game".into(), ..Default::default() }];
        crate::wikinew::create(&wiki, "Realms", &covered, "2026-10-06").unwrap();
        let before = fs::read_to_string(wiki.join(START_HERE)).unwrap();
        let root = d.path().join("Game");
        write(&root, &[("CLAUDE.md", "# Game\n\nA game.\n\n## Common Gotchas\n\n- The base game's assets live in the server jar.\n- Energy is Stamina in code.\n")]);
        git_in(&root, &["init", "-q"]);
        git_in(&root, &["add", "-A"]);
        let repos = vec![("Game".to_string(), root)];
        let mut db = Db::open_in_memory().unwrap();
        let mut asked: Vec<String> = Vec::new();
        let report = draft_team(&wiki, "Wiki", &mut db, &repos, None, "2026-10-06", 5, |p| {
            asked.push(p.to_string());
            Ok(all_supported(p).unwrap_or_else(|| if p.contains("the Traps table") { reply.to_string() } else { page("Drafted") }))
        })
        .unwrap();
        let traps = asked.iter().find(|p| p.contains("the Traps table")).expect("a traps call");
        assert!(traps.contains("server jar") && traps.contains("[[Vocabulary]] (Reference/Vocabulary.md)"), "{traps}");
        assert!(report.drafted.contains(&START_HERE.to_string()), "{report:?}");
        let after = fs::read_to_string(wiki.join(START_HERE)).unwrap();
        assert!(after.contains(&rows[0]) && after.contains(&rows[1]) && !after.contains("{{the trap that costs"), "{after}");
        assert_eq!(after.replace(&format!("{}\n{}\n", rows[0], rows[1]), ""), before.lines().filter(|l| !l.starts_with(TRAPS_PLACEHOLDER)).map(|l| format!("{l}\n")).collect::<String>(), "nothing else changes");
        // Filled once, kept after: a second draft asks nothing for it.
        let mut again = 0;
        draft_team(&wiki, "Wiki", &mut db, &repos, None, "2026-10-06", 6, |p| {
            again += usize::from(p.contains("the Traps table"));
            Ok(page("Drafted"))
        })
        .unwrap();
        assert_eq!(again, 0);
    }

    /// One fact, one page is the drafting's rule, not the reading's: a part
    /// goes to every page it fits as well, the people pages read who decides
    /// wherever a brief says it, a repo's other docs reach the team pages,
    /// and a repo's layers page reads the parts about layers first.
    #[test]
    fn a_part_reaches_every_page_it_fits() {
        let d = tempfile::tempdir().unwrap();
        let filler = "The server boots from the jar and loads the world.\n\n".repeat(1_400); // ~73,000 characters
        let brief = format!(
            "# Game\n\nA tactics game.\n\n## Build and run\n\n{filler}## Running the server\n\n\
             Paths no lane touches without Chris's yes: `run/`, `gradle.properties`.\n\n\
             ## Registries, wiring and config keys\n\nEvery installer registers its systems and its config keys in setup(), in order.\n\
             Config defaults are read once at start-up by each installer.\n"
        );
        let root = d.path().join("Game");
        write(
            &root,
            &[
                ("CLAUDE.md", &brief),
                (".ken/gates.json", "{\"protected\": [\"run/\"], \"_protected_note\": \"Paths no lane touches without Chris's yes.\"}"),
                ("docs/CODE-STANDARDS.md", "# Code standards\n\n## Style\n\nLint with the formatter; helpers live in `utils/`. Follow the style conventions.\n"),
            ],
        );
        let briefs = vec![("CLAUDE.md".to_string(), brief_sections(&brief))];
        let routed = route_parts("Game", &briefs);
        let line_of = |h: &str| brief.lines().position(|l| l == h).unwrap() + 1;
        let wiring = format!("Game:CLAUDE.md:{}", line_of("## Registries, wiring and config keys"));
        let pages_of = |label: &str| -> Vec<&str> { routed.iter().filter(|(_, v)| v.iter().any(|s| s.label == label)).map(|(p, _)| *p).collect() };
        let wired = pages_of(&wiring);
        assert!(wired.contains(&"Conventions/Registries.md") && wired.contains(&"Reference/Config-Map.md"), "{wired:?}");
        assert!(wired.len() <= PART_PAGES);

        // Who decides reaches Team though it sits in a build section that
        // fills Build and Run.
        let repos = vec![("Game".to_string(), root.clone())];
        let wiki = d.path().join("Wiki");
        let team = page_sources("Current/Team.md", &wiki, "Wiki", &repos, &[]);
        let who = team.iter().find(|s| s.label == "Game:(lines of its briefs about who decides)").expect("the decider lines");
        assert!(who.text.contains(&format!("Game:CLAUDE.md:{}: Paths no lane touches without Chris's yes", line_of("Paths no lane touches without Chris's yes: `run/`, `gradle.properties`."))), "{}", who.text);
        assert!(team.iter().any(|s| s.label == "Game:.ken/gates.json"), "the protected paths");
        let code = page_sources("Conventions/Code.md", &wiki, "Wiki", &repos, &[]);
        assert!(code.iter().any(|s| s.label.starts_with("Game:docs/CODE-STANDARDS.md") && s.text.contains("helpers live in")), "{code:?}");

        // The layers page: the parts about layers before what the code registers.
        let map = vec![Source { label: format!("Game:{REGISTERS}"), text: "GamePlugin.java:5 setup(): 3 statements".into() }];
        let layers: Vec<String> = layers_sources("Game", &root, &map).into_iter().map(|s| s.label).collect();
        let at = |l: &str| layers.iter().position(|x| x == l).unwrap_or_else(|| panic!("{l} in {layers:?}"));
        assert!(at(&wiring) < at(&format!("Game:{REGISTERS}")), "{layers:?}");
    }

    /// A pin a draft wrote as `""` is empty: Ken fills it.
    #[test]
    fn an_empty_pin_is_filled() {
        let heads: HashMap<String, String> = [("app".to_string(), "0a1b2c3d4e5f".to_string())].into();
        for empty in ["pin: \"\"   # the commit each repo's sources were read at", "pin: ''", "pin:"] {
            let page = format!("---\ntitle: V\n{empty}\nsources:\n  - app:README.md\n---\n# V\n");
            assert!(pin_sources(&page, &heads).contains("\npin: 0a1b2c3d4e5f\n"), "{empty}: {}", pin_sources(&page, &heads));
        }
    }

    /// The name check finds a name inside a longer identifier, never
    /// rewrites a path to the wiki's own page, and leaves alone what it
    /// cannot judge, saying so; and a correction may say a thing is gone
    /// only when Ken's search finds nothing.
    #[test]
    fn the_check_is_precise_and_says_what_it_skipped() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("game");
        write(
            &root,
            &[
                ("gradle.properties", "hytale_version = 0.7.0-pre.5\n"),
                ("src/commands/ResetCommand.java", "class ResetCommand {\n  String NAME = \"reset_kill_stats\";\n}\n"),
            ],
        );
        git_in(&root, &["init", "-q"]);
        git_in(&root, &["add", "-A"]);
        let wiki = d.path().join("Wiki");
        write(&wiki, &[("Reference/Vocabulary.md", "# Vocabulary\n")]);
        git_in(&wiki, &["init", "-q"]);
        git_in(&wiki, &["add", "-A"]);
        let check = PathCheck::of(&[("game".to_string(), root.clone())]).with_library("Wiki", &wiki);
        let page = "---\ntitle: V\nchanged_by: \"\"\n---\n# V\n\
            The ids say `kill_stats`; `changed_by` is set by a person; add `MyComponent` to `myConfig`.\n\
            There is no `stopServer` task. The manifest takes `>=0.7.0-pre.1 <0.8.0`.\n\
            The brief names the word map at `sr-docs/Engine/Reference/Vocabulary.md`.\n\
            The docs vault's `_meta/ticketcheck.py` runs at G2; tickets are `tickets/SR-NNN.md`.\n";
        let found = check.check("Reference/Vocabulary.md", page, None);
        assert!(found.names.is_empty(), "{:?}", found.names);
        assert!(found.versions.is_empty(), "{:?}", found.versions);
        let paths: Vec<(&str, &[String])> = found.paths.iter().map(|m| (m.path.as_str(), m.now.as_slice())).collect();
        assert_eq!(paths, vec![("sr-docs/Engine/Reference/Vocabulary.md", &[][..])], "never the wiki's own page");
        for skipped in ["`changed_by` (a key of the page's front matter)", "`MyComponent` (an example name)", "`myConfig` (an example name)", "`stopServer` (on a line that says it does not exist)", "`0.8.0` (a range's bound)", "`_meta/ticketcheck.py` (in a repo Ken did not read)", "`tickets/SR-NNN.md` (a pattern)"] {
            assert!(found.skipped.contains(&skipped.to_string()), "{skipped}: {:?}", found.skipped);
        }
        let (text, _) = rewrite_single_matches(page, &found.paths);
        assert_eq!(text, page, "a path is never rewritten to the wiki itself");

        // A correction that says `kill_stats` is gone is put back: Ken's
        // search finds it; one that says `Gone` is gone stands.
        let before = "# V\n\n| word | code's word |\n|---|---|\n| Chronicle | `kill_stats` `game:x:1` |\n| Old | `Gone` |\n";
        let fixed = "# V\n\n| word | code's word |\n|---|---|\n| Chronicle | `kill_stats` is gone: no repo has it now |\n| Old | `GoneThing` is gone: no repo has it now |\n";
        let (out, undone) = guard_absences(before, fixed, &check);
        assert!(out.contains("| Chronicle | `kill_stats` `game:x:1` |") && out.contains("`GoneThing` is gone"), "{out}");
        assert_eq!(undone.len(), 1);
        assert!(undone[0].2.contains("git grep -F kill_stats") && undone[0].2.contains("game:src/commands/ResetCommand.java:2"), "{:?}", undone);
    }
}

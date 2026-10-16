//! A new knowledge base, laid down at set-up from Ken's own template
//! (`templates/wiki`, bundled so set-up works offline and every workspace
//! starts from the same version). One repo holds the library, the decisions log, the
//! tickets, `log.md` and the settings in `.ken/knowledge.json`.
//!
//! Only the facts Ken knows are filled in: the workspace's name, the repo's own
//! name, the ticket key, the repos it covers with what each is for, and
//! `updated:` dates. Every other `{{…}}` stays for a person. The folder
//! becomes a git repo with one commit; publishing it (a remote, a push) is a
//! person's step.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

macro_rules! template {
    ($($path:literal),* $(,)?) => {
        &[$(($path, include_str!(concat!("../templates/wiki/", $path)))),*]
    };
}

/// Every file of the template: (path in the knowledge base, text).
pub const TEMPLATE: &[(&str, &str)] = template![
    ".ken/knowledge.json",
    "CLAUDE.md",
    "START-HERE.md",
    "log.md",
    "Current/Index.md",
    "Current/Project.md",
    "Current/RAID.md",
    "Decisions/DECISIONS.md",
    "Design/Index.md",
    "Reference/Index.md",
    "Reference/Platform/.gitkeep",
    "Reference/Vocabulary.md",
    "Research/Index.md",
    "Research/Ingestion/Index.md",
    "Research/Ingestion/Ingested/.gitkeep",
    "Research/Ingestion/Raw/.gitkeep",
    "Rules/Index.md",
    "Templates/Index.md",
    "Templates/Design-Note.md",
    "Templates/Feature-Status.md",
    "Templates/Finding.md",
    "Templates/How-to.md",
    "Templates/Ingested-note.md",
    "Templates/Platform.md",
    "Templates/Roadmap.md",
    "Templates/Rule.md",
    "Templates/Ticket.md",
    "tickets/README.md",
];

/// Where the settings live in a knowledge base.
pub const KNOWLEDGE_FILE: &str = ".ken/knowledge.json";

/// A repo the new knowledge base covers: its name in the workspace and what it
/// is for, and, when set-up knows them, its kinds and folder.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Covered {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub kind: Vec<crate::registry::RepoKind>,
    #[serde(default)]
    pub path: Option<std::path::PathBuf>,
}

/// Pages that are templates for people to copy keep their placeholders,
/// dates included. The blanks all live in `Templates/`; its index is a page.
pub(crate) fn is_copy_template(path: &str) -> bool {
    path.starts_with("Templates/") && path != "Templates/Index.md"
}

/// A ticket key from the workspace's name: the initials of its words when it
/// has two or more (`Shattered-Realms` → SR), else its first six letters and
/// digits; in upper case, TEAM when it has none.
pub fn ticket_key(name: &str) -> String {
    let words: Vec<&str> = name.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let k: String = if words.len() >= 2 {
        words.iter().filter_map(|w| w.chars().next()).collect()
    } else {
        name.chars().filter(|c| c.is_ascii_alphanumeric()).take(6).collect()
    };
    let k = k.to_uppercase();
    if k.is_empty() {
        "TEAM".into()
    } else {
        k
    }
}

fn one_line(s: &str) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ").replace('|', "/");
    if s.is_empty() {
        "(not said yet)".into()
    } else {
        s
    }
}

/// What git says about a repo: the branch it is on (None on no branch), its
/// HEAD, its `origin` URL.
fn repo_facts(dir: &Path) -> (Option<String>, Option<String>, Option<String>) {
    let git = |args: &[&str]| crate::git::output(dir, args).filter(|s| !s.is_empty());
    let branch = git(&["rev-parse", "--abbrev-ref", "HEAD"]).filter(|b| b != "HEAD");
    (branch, git(&["rev-parse", "HEAD"]), crate::setup::origin_url(dir))
}

/// The `repos` entries for `.ken/knowledge.json`: this knowledge base, then
/// each covered repo with its kind, the branch it is on, and for a reference
/// repo the commit read and its `origin`.
fn knowledge_repos(wiki: &str, repos: &[Covered]) -> String {
    use crate::registry::RepoKind;
    use serde_json::{json, Value};
    let q = |s: &str| serde_json::to_string(s).unwrap_or_default();
    let mut entries = vec![format!("    {}: {}", q(wiki), json!({ "kind": "wiki", "base": "main" }))];
    for r in repos.iter().filter(|r| r.name != wiki) {
        let kinds: Vec<Value> = r.kind.iter().map(|k| serde_json::to_value(k).unwrap_or(Value::Null)).collect();
        let kind = match kinds.as_slice() {
            [] => json!("code"),
            [one] => one.clone(),
            _ => Value::Array(kinds),
        };
        let (branch, head, origin) = r.path.as_deref().map(repo_facts).unwrap_or_default();
        let mut entry = serde_json::Map::new();
        entry.insert("kind".into(), kind);
        entry.insert("base".into(), json!(branch.unwrap_or_else(|| "main".into())));
        if r.kind.contains(&RepoKind::Reference) {
            if let Some(h) = head {
                entry.insert("pin".into(), json!(h));
            }
            if let Some(o) = origin {
                entry.insert("upstream".into(), json!(o));
            }
        }
        entries.push(format!("    {}: {}", q(&r.name), Value::Object(entry)));
    }
    entries.join(",\n")
}

/// One template file with what Ken knows filled in.
pub fn fill(path: &str, text: &str, workspace: &str, wiki: &str, repos: &[Covered], today: &str) -> String {
    let key = ticket_key(workspace);
    let mut t = text.replace("{{KEY}}", &key);
    if path == KNOWLEDGE_FILE {
        // The one `repos` line of the template becomes every repo's entry.
        let line = t.lines().find(|l| l.trim_start().starts_with("\"{{wiki_repo}}\"")).map(str::to_string);
        if let Some(line) = line {
            t = t.replace(&line, &knowledge_repos(wiki, repos));
        }
        // Inside JSON strings: escaped, the quotes the template already has.
        let inner = |s: &str| {
            let quoted = serde_json::to_string(s).unwrap_or_default();
            quoted.strip_prefix('"').and_then(|q| q.strip_suffix('"')).unwrap_or(&quoted).to_string()
        };
        return t.replace("{{workspace_name}}", &inner(workspace)).replace("{{wiki_repo}}", &inner(wiki));
    }
    t = t.replace("{{workspace_name}}", workspace).replace("{{wiki_repo}}", wiki);
    if !is_copy_template(path) {
        t = t
            .lines()
            .map(|l| if l.trim_start().starts_with("updated:") { l.replacen("{{date}}", today, 1) } else { l.to_string() })
            .collect::<Vec<_>>()
            .join("\n")
            + if text.ends_with('\n') { "\n" } else { "" };
    }
    if path == "START-HERE.md" {
        // The repo table: this knowledge base, then each repo it covers.
        let mut rows = format!("| `{wiki}` | this repo · the knowledge base |\n");
        for r in repos.iter().filter(|r| r.name != wiki) {
            rows.push_str(&format!("| `{}` | {} |\n", r.name, one_line(&r.description)));
        }
        let mut out = String::new();
        let mut replaced = false;
        for line in t.lines() {
            if line.starts_with(&format!("| `{wiki}` |")) {
                if !replaced {
                    out.push_str(&rows);
                    replaced = true;
                }
                continue;
            }
            out.push_str(line);
            out.push('\n');
        }
        t = out;
    }
    t
}

fn git(dir: &Path, args: &[&str]) -> Result<()> {
    crate::git::run(dir, args).map_err(Error::Other)
}

/// Lay a new knowledge base down at `dir` (missing or empty) for the
/// workspace named `workspace`, covering `repos`, then make it a git repo
/// with one commit. Refuses a folder that already holds anything, so it
/// never writes over a person's files.
pub fn create(dir: &Path, workspace: &str, repos: &[Covered], today: &str) -> Result<()> {
    if dir.exists() && fs::read_dir(dir).map_err(|e| Error::io(dir, e))?.next().is_some() {
        return Err(Error::Other(format!("{} is not empty; pick a new folder for the knowledge base", dir.display())));
    }
    let wiki = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "wiki".into());
    for (path, text) in TEMPLATE {
        let dest = dir.join(path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        // LF, whatever the checkout Ken was built from used.
        let text = text.replace("\r\n", "\n");
        fs::write(&dest, fill(path, &text, workspace, &wiki, repos, today)).map_err(|e| Error::io(&dest, e))?;
    }
    let paths: Vec<&str> = TEMPLATE.iter().map(|(path, _)| *path).collect();
    git_commit_new(dir, &paths, &format!("Start the {workspace} knowledge base from Ken's template"))
}

/// Overlay Ken's structure on an existing wiki at `dir` (decision 32, step
/// 2): every template file it lacks is laid down with what Ken knows filled
/// in, and nothing it has is written, moved or deleted. The folder becomes a
/// git repo when it is not one, and the files Ken added are committed by
/// name, never anything of the person's (decision 36). Returns the files
/// added. The pages it already has are mapped into the sections afterwards,
/// after a dry run (`wikimap`).
pub fn overlay(dir: &Path, name: &str, repos: &[Covered], today: &str) -> Result<Vec<String>> {
    if !dir.is_dir() {
        return Err(Error::Other(format!("{} is not a folder", dir.display())));
    }
    let wiki = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "wiki".into());
    let mut added = Vec::new();
    for (path, text) in TEMPLATE {
        let dest = dir.join(path);
        if dest.exists() {
            continue;
        }
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        let text = text.replace("\r\n", "\n");
        fs::write(&dest, fill(path, &text, name, &wiki, repos, today)).map_err(|e| Error::io(&dest, e))?;
        added.push(path.to_string());
    }
    if !dir.join(".git").exists() {
        git(dir, &["init", "-q"])?;
    }
    if !added.is_empty() {
        if let Err(e) = crate::git::commit_paths(dir, &added, &format!("Lay Ken's structure over the {name} knowledge base")) {
            eprintln!("warning: the files Ken added to {} are not committed: {e}", dir.display());
        }
    }
    Ok(added)
}

/// A template file a knowledge base lacks, or one Ken owns whose text is
/// not the bundled template's any more.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateGap {
    pub path: String,
    /// `missing` or `out-of-date`.
    pub state: String,
    /// For an out-of-date file, the lines that differ: `- ` the file's,
    /// `+ ` the template's.
    pub diff: Vec<String>,
}

/// Files whose text is Ken's to keep current: the blanks in `Templates/`
/// and the brief. Every other page is a person's once laid down, so only
/// its absence is reported.
fn ken_owned(path: &str) -> bool {
    is_copy_template(path) || path == "CLAUDE.md"
}

/// Running set-up again on a knowledge base (decision 4): the template files
/// it lacks, and the ones Ken owns that differ from the bundled template, as
/// a diff. Reads only; nothing is overwritten. `name` fills the brief as
/// set-up would.
pub fn template_gaps(dir: &Path, name: &str, today: &str) -> Vec<TemplateGap> {
    let wiki = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "wiki".into());
    let mut out = Vec::new();
    for (path, text) in TEMPLATE {
        if path.ends_with(".gitkeep") {
            continue;
        }
        let dest = dir.join(path);
        let Ok(have) = fs::read_to_string(&dest) else {
            out.push(TemplateGap { path: path.to_string(), state: "missing".into(), diff: Vec::new() });
            continue;
        };
        if !ken_owned(path) {
            continue;
        }
        let want = fill(path, &text.replace("\r\n", "\n"), name, &wiki, &[], today);
        let have = have.replace("\r\n", "\n");
        if have.trim_end() != want.trim_end() {
            out.push(TemplateGap { path: path.to_string(), state: "out-of-date".into(), diff: line_diff(&have, &want) });
        }
    }
    out
}

/// The lines only one side has, in order: `- ` lines of `have`, `+ ` lines
/// of `want`. At most 40 lines.
fn line_diff(have: &str, want: &str) -> Vec<String> {
    let mine: std::collections::HashSet<&str> = have.lines().collect();
    let theirs: std::collections::HashSet<&str> = want.lines().collect();
    have.lines()
        .filter(|l| !theirs.contains(l))
        .map(|l| format!("- {l}"))
        .chain(want.lines().filter(|l| !mine.contains(l)).map(|l| format!("+ {l}")))
        .take(40)
        .collect()
}

/// Lay down the template files `dir` lacks from the gaps a person chose,
/// never overwriting; commit them by name. Returns the files written.
pub fn add_missing(dir: &Path, name: &str, paths: &[String], today: &str) -> Result<Vec<String>> {
    let wiki = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "wiki".into());
    let mut added = Vec::new();
    for (path, text) in TEMPLATE.iter().filter(|(p, _)| paths.iter().any(|x| x == p)) {
        let dest = dir.join(path);
        if dest.exists() {
            continue;
        }
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        fs::write(&dest, fill(path, &text.replace("\r\n", "\n"), name, &wiki, &[], today)).map_err(|e| Error::io(&dest, e))?;
        added.push(path.to_string());
    }
    if !added.is_empty() {
        crate::git::commit_paths(dir, &added, "Add the template files the knowledge base lacked").map_err(Error::Other)?;
    }
    Ok(added)
}

/// `git init` in `dir` and one commit of exactly `paths`, the files Ken laid
/// down there, added by name (decision 36: never `git add -A`). The person's
/// own git identity when they have one; Ken's otherwise, so a machine with no
/// identity set can still make the first commit.
pub(crate) fn git_commit_new(dir: &Path, paths: &[&str], msg: &str) -> Result<()> {
    git(dir, &["init", "-q"])?;
    // A path the person's global ignore rules leave out stays out, as it
    // would have with `git add -A`.
    let mut check = vec!["check-ignore", "--"];
    check.extend_from_slice(paths);
    let ignored = crate::git::output(dir, &check).unwrap_or_default();
    let ignored: Vec<&str> = ignored.lines().map(str::trim).collect();
    let mut add = vec!["add", "--"];
    add.extend(paths.iter().filter(|p| !ignored.contains(p)));
    git(dir, &add)?;
    if git(dir, &["commit", "-q", "-m", msg]).is_err() {
        git(dir, &["-c", "user.name=Ken", "-c", "user.email=ken@localhost", "commit", "-q", "-m", msg])?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn repos() -> Vec<Covered> {
        vec![
            Covered { name: "Game".into(), description: "The game client.\nBuilt nightly.".into(), ..Default::default() },
            Covered { name: "Tools".into(), description: String::new(), ..Default::default() },
        ]
    }

    fn template(path: &str) -> &'static str {
        TEMPLATE.iter().find(|(p, _)| *p == path).unwrap().1
    }

    #[test]
    fn start_here_lists_this_knowledge_base_and_its_repos() {
        let t = fill("START-HERE.md", template("START-HERE.md"), "Realms", "Realms-Wiki", &repos(), "2026-09-25");
        assert!(t.contains("The Realms knowledge base"));
        assert!(t.contains(
            "| `Realms-Wiki` | this repo · the knowledge base |\n| `Game` | The game client. Built nightly. |\n| `Tools` | (not said yet) |"
        ));
        assert!(!t.contains("{{wiki_repo}}"));
    }

    #[test]
    fn the_knowledge_file_names_every_repo_and_is_json() {
        let t = fill(KNOWLEDGE_FILE, template(KNOWLEDGE_FILE), "Shattered Realms", "SR-Wiki", &repos(), "2026-09-25");
        let v: serde_json::Value = serde_json::from_str(&t).unwrap();
        assert_eq!(v["workspace"], "Shattered Realms");
        assert!(v.get("team").is_none(), "no team (decision 35)");
        assert_eq!(v["key"], "SR");
        assert_eq!(v["repos"]["SR-Wiki"]["kind"], "wiki");
        assert_eq!(v["repos"]["Game"]["kind"], "code");
        assert!(v["repos"]["Tools"].is_object());
    }

    #[test]
    fn ticket_keys_come_from_the_workspaces_initials() {
        assert_eq!(ticket_key("Shattered-Realms"), "SR");
        assert_eq!(ticket_key("ken"), "KEN");
        assert_eq!(ticket_key("--"), "TEAM");
    }

    /// Every file under `templates/<dir>`, as a path inside it, sorted.
    pub(crate) fn files_under(dir: &str) -> Vec<String> {
        fn walk(root: &Path, at: &Path, out: &mut Vec<String>) {
            for e in fs::read_dir(at).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(root, &p, out);
                } else {
                    out.push(p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"));
                }
            }
        }
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("templates").join(dir);
        let mut out = Vec::new();
        walk(&root, &root, &mut out);
        out.sort();
        out
    }

    #[test]
    fn the_template_list_is_exactly_the_bundled_folder() {
        let mut listed: Vec<String> = TEMPLATE.iter().map(|(p, _)| p.to_string()).collect();
        listed.sort();
        assert_eq!(listed, files_under("wiki"), "TEMPLATE and templates/wiki differ");
        for (path, text) in TEMPLATE {
            assert!(!text.contains('\r'), "{path} has a CR");
        }
    }

    #[test]
    fn updated_is_dated_and_copy_templates_keep_placeholders() {
        let text = "---\nupdated: {{date}}\n---\n";
        assert_eq!(fill("Current/Project.md", text, "T", "W", &[], "2026-09-25"), "---\nupdated: 2026-09-25\n---\n");
        assert_eq!(fill("Templates/How-to.md", text, "T", "W", &[], "2026-09-25"), text, "a template to copy keeps its placeholders");
    }

    #[test]
    fn create_lays_the_template_down_as_a_repo_and_refuses_a_full_folder() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("Realms-Wiki");
        create(&dir, "Realms", &repos(), "2026-09-25").unwrap();
        for (path, _) in TEMPLATE {
            assert!(dir.join(path).exists(), "{path}");
        }
        assert!(dir.join(".git").exists());
        assert!(fs::read_to_string(dir.join("CLAUDE.md")).unwrap().starts_with("# Realms-Wiki"));
        assert!(create(&dir, "Realms", &repos(), "2026-09-25").is_err(), "never writes over a folder with files");
    }

    #[test]
    fn running_set_up_again_reports_gaps_and_never_overwrites() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("Realms-Wiki");
        create(&dir, "Realms", &[], "2026-09-25").unwrap();
        assert!(template_gaps(&dir, "Realms", "2026-09-25").is_empty(), "fresh from the template");

        fs::remove_file(dir.join("Templates/Rule.md")).unwrap();
        fs::write(dir.join("Templates/How-to.md"), "# An older blank\n").unwrap();
        fs::write(dir.join("START-HERE.md"), "# Ours now\n").unwrap();
        let gaps = template_gaps(&dir, "Realms", "2026-09-25");
        let state = |p: &str| gaps.iter().find(|g| g.path == p).map(|g| g.state.clone());
        assert_eq!(state("Templates/Rule.md").as_deref(), Some("missing"));
        assert_eq!(state("Templates/How-to.md").as_deref(), Some("out-of-date"));
        assert_eq!(state("START-HERE.md"), None, "a page a person owns is theirs to change");
        let howto = gaps.iter().find(|g| g.path == "Templates/How-to.md").unwrap();
        assert!(howto.diff.contains(&"- # An older blank".to_string()), "{:?}", howto.diff);

        let added = add_missing(&dir, "Realms", &["Templates/Rule.md".into(), "Templates/How-to.md".into()], "2026-09-25").unwrap();
        assert_eq!(added, vec!["Templates/Rule.md"], "an out-of-date file is reported, never overwritten");
        assert_eq!(fs::read_to_string(dir.join("Templates/How-to.md")).unwrap(), "# An older blank\n");
    }
}

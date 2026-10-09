//! A new team wiki, laid down at set-up from the method's own template
//! (Ways-of-Working `templates/wiki`, bundled into Ken so set-up works
//! offline and every team starts from the same version).
//!
//! Only the facts Ken knows are filled in: the team's name, the wiki's own
//! name, the team's repos with what each is for, and `updated:` dates. Every
//! other `{{…}}` is left for a person or the first-wiki draft, and no
//! `verified:` date is ever stamped: only a person reads a page against its
//! sources. The folder becomes a git repo with one commit; publishing it
//! (a remote, a push) is a person's step.

use std::fs;
use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Which copy of the method's template is bundled.
pub const TEMPLATE_SOURCE: &str = "Stingbro/Ways-of-Working templates/wiki @ 15afe07";

macro_rules! template {
    ($($path:literal),* $(,)?) => {
        &[$(($path, include_str!(concat!("../templates/wiki/", $path)))),*]
    };
}

/// Every file of the template: (path in the wiki, text).
pub const TEMPLATE: &[(&str, &str)] = template![
    "CLAUDE.md",
    "START-HERE.md",
    "_meta/README.md",
    "Conventions/Index.md",
    "Conventions/Architecture.md",
    "Conventions/Code.md",
    "Conventions/Data-and-Config.md",
    "Conventions/Registries.md",
    "Conventions/Testing.md",
    "Current/Index.md",
    "Current/Feature-Status.md",
    "Current/Project.md",
    "Current/Roadmap.md",
    "Current/Team.md",
    "Current/Who-Does-What.md",
    "Design/Index.md",
    "Design/Principles.md",
    "Platform/Index.md",
    "Reference/Index.md",
    "Reference/Build-and-Run.md",
    "Reference/Config-Map.md",
    "Reference/How-tos.md",
    "Reference/How-tos/.gitkeep",
    "Reference/Systems.md",
    "Reference/Systems/.gitkeep",
    "Reference/Vocabulary.md",
    "Research/Index.md",
    "Research/Findings/.gitkeep",
    "Research/Ingestion/Index.md",
    "Research/Ingestion/Ingested/.gitkeep",
    "Research/Ingestion/Raw/.gitkeep",
    "Templates/Index.md",
    "Templates/Convention.md",
    "Templates/Dependency.md",
    "Templates/Design-Note.md",
    "Templates/Finding.md",
    "Templates/How-to.md",
    "Templates/Incident.md",
    "Templates/Ingested-note.md",
    "Templates/Repo-Architecture.md",
    "Templates/Rule.md",
    "Templates/System.md",
    "Ways-of-Working/Ways-of-Working.md",
    "Ways-of-Working/Agents/Index.md",
    "Ways-of-Working/Lifecycle.md",
    "Ways-of-Working/Research-Ladder.md",
    "Ways-of-Working/Rules.md",
    "Ways-of-Working/Rules/write-for-the-altitude-like-teammates-talking.md",
    "Work/Index.md",
    "Work/Incidents.md",
    "Work/Incidents/.gitkeep",
    "Work/Releases.md",
];

/// A repo the new wiki covers: its name in the workspace and what it is for,
/// and, when set-up knows them, its kinds and folder (for the team manifest:
/// on 2026-10-06 its code and reference entries stayed `{{placeholders}}`
/// though Ken had registered all four repos).
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

fn one_line(s: &str) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ").replace('|', "/");
    if s.is_empty() {
        "(not said yet)".into()
    } else {
        s
    }
}

/// One template file with what Ken knows filled in.
pub fn fill(path: &str, text: &str, team: &str, wiki: &str, repos: &[Covered], today: &str) -> String {
    // The section on how the system works is named `Domain` until the team
    // names it after its own subject.
    let mut t = text.replace("{{team_name}}", team).replace("{{wiki_repo}}", wiki).replace("{{Domain}}", "Domain");
    if !is_copy_template(path) {
        // `updated:` only; a `verified:` date is a person's.
        t = t
            .lines()
            .map(|l| {
                if l.trim_start().starts_with("updated:") {
                    l.replacen("{{date}}", today, 1)
                } else {
                    l.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
            + if text.ends_with('\n') { "\n" } else { "" };
        // A page with frontmatter but no date would read as unverified for
        // ever from the first sweep; it was written today.
        let head_end = t.strip_prefix("---\n").and_then(|rest| rest.find("\n---").map(|i| i + 4));
        if let Some(end) = head_end {
            if !t[..end].lines().any(|l| l.trim_start().starts_with("updated:")) {
                t.insert_str(4, &format!("updated: {today}\n"));
            }
        }
    }
    if path == "START-HERE.md" {
        // The repo table: this wiki, then each of the team's repos.
        let mut rows = format!("| `{wiki}` | this repo · the library |\n");
        for r in repos {
            rows.push_str(&format!("| `{}` | {} |\n", r.name, one_line(&r.description)));
        }
        let mut out = String::new();
        let mut replaced = false;
        for line in t.lines() {
            let placeholder_row = line.starts_with("| `{{") || (line.starts_with(&format!("| `{wiki}`")) && !replaced);
            if placeholder_row {
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
    let mut cmd = Command::new("git");
    let out = crate::proc::quiet(&mut cmd)
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| Error::Other(format!("git could not run: {e}")))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(Error::Other(format!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim())))
    }
}

/// Lay a new wiki down at `dir` (missing or empty) for `team`, covering
/// `repos`, then make it a git repo with one commit. Refuses a folder that
/// already holds anything, so it can never write over a person's files.
pub fn create(dir: &Path, team: &str, repos: &[Covered], today: &str) -> Result<()> {
    create_with(dir, team, repos, today, false)
}

/// [`create`], and with `team_parts` the team template's folders too
/// (tickets, decisions, ideas, people, the team manifest): a team with no
/// team repo keeps its tickets in its docs repo, as one repo of both kinds.
pub fn create_with(dir: &Path, team: &str, repos: &[Covered], today: &str, team_parts: bool) -> Result<()> {
    if dir.exists() && fs::read_dir(dir).map_err(|e| Error::io(dir, e))?.next().is_some() {
        return Err(Error::Other(format!("{} is not empty; pick a new folder for the wiki", dir.display())));
    }
    let wiki = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "wiki".into());
    for (path, text) in TEMPLATE {
        let dest = dir.join(path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        // LF, whatever the checkout Ken was built from used.
        let text = text.replace("\r\n", "\n");
        fs::write(&dest, fill(path, &text, team, &wiki, repos, today)).map_err(|e| Error::io(&dest, e))?;
    }
    if team_parts {
        for (path, text) in crate::teamnew::TEMPLATE {
            let dest = dir.join(path);
            if dest.exists() {
                continue;
            }
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
            }
            let text = text.replace("\r\n", "\n");
            let filled = crate::teamnew::fill(path, &text, team, &wiki, Some(&wiki), repos);
            fs::write(&dest, filled).map_err(|e| Error::io(&dest, e))?;
        }
    }
    git_commit_all(dir, &format!("Start the {team} wiki from the Ways-of-Working template"))
}

/// `git init` in `dir` and one commit of everything in it. The person's own
/// git identity when they have one; Ken's otherwise, so a machine with no
/// identity set can still make the first commit.
pub(crate) fn git_commit_all(dir: &Path, msg: &str) -> Result<()> {
    git(dir, &["init", "-q"])?;
    git(dir, &["add", "-A"])?;
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

    #[test]
    fn start_here_lists_this_wiki_and_the_teams_repos() {
        let text = TEMPLATE.iter().find(|(p, _)| *p == "START-HERE.md").unwrap().1;
        let t = fill("START-HERE.md", text, "Realms", "Realms-Wiki", &repos(), "2026-09-25");
        assert!(t.contains("The Realms library: the traps"));
        assert!(t.contains("| `Realms-Wiki` | this repo · the library |\n| `Game` | The game client. Built nightly. |\n| `Tools` | (not said yet) |"));
        assert!(!t.contains("{{team_repo}}") && !t.contains("{{code_repo}}"));
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
    fn updated_is_dated_and_verified_never_is() {
        let undated = fill("Ways-of-Working/Agents/Index.md", "---
title: \"Agents\"
---
# Agents
", "T", "W", &[], "2026-09-28");
        assert_eq!(undated, "---
updated: 2026-09-28
title: \"Agents\"
---
# Agents
", "an undated page is dated today");
        let copy = "---
title: \"{{Area}}\"
---
";
        assert_eq!(fill("Templates/How-to.md", copy, "T", "W", &[], "2026-09-28"), copy, "a copy template is left as it is");
        let text = "---\nupdated: {{date}}\nverified: {{date}}      # a person\n---\n";
        let t = fill("Current/Project.md", text, "T", "W", &[], "2026-09-25");
        assert_eq!(t, "---\nupdated: 2026-09-25\nverified: {{date}}      # a person\n---\n");
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
}

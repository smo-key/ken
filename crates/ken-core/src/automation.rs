//! Automations: `.ken/automations/<slug>.md` — YAML frontmatter + a
//! plain-language agent prompt body. Generic trigger→agent rules; external
//! reach comes only from the MCP servers the user configured for `claude`.
//! Parsing is defensive (one bad file never hides the rest) and rewriting
//! preserves fields this version doesn't know about.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::project::Project;
use crate::{Error, Result};

pub const AUTOMATIONS_DIR: &str = ".ken/automations";

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Automation {
    pub slug: String,
    pub name: String,
    pub globs: Vec<String>,
    pub prompt: String,
    pub auto_apply: bool,
    pub enabled: bool,
    #[serde(skip)]
    pub(crate) extra: serde_yaml::Mapping,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Frontmatter {
    name: String,
    #[serde(default)]
    globs: Vec<String>,
    #[serde(default)]
    auto_apply: bool,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(flatten)]
    extra: serde_yaml::Mapping,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationError {
    pub slug: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum AutomationEntry {
    Ok { automation: Automation },
    Broken { error: AutomationError },
}

pub fn automations_dir(root: &Path) -> PathBuf {
    root.join(AUTOMATIONS_DIR)
}

pub fn automation_path(root: &Path, slug: &str) -> PathBuf {
    automations_dir(root).join(format!("{slug}.md"))
}

/// Which of `changed` a rule's globs match — the file list handed to the prompt.
pub fn triggers(a: &Automation, changed: &[String]) -> Vec<String> {
    changed
        .iter()
        .filter(|p| a.globs.iter().any(|g| glob_match(g, p)))
        .cloned()
        .collect()
}

/// Phase-1 (auto_apply=false): research + write a PROPOSAL, change nothing else.
pub fn proposal_prompt(a: &Automation, matched: &[String], staging: &Path) -> String {
    let files = if matched.is_empty() {
        "(no specific files — inspect the folders your patterns cover)".to_string()
    } else {
        matched.iter().map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n")
    };
    format!(
        r###"You are Ken running the automation "{name}".

## What to do

{prompt}

## Files that triggered this run

{files}

## THIS IS A PROPOSAL RUN — do not act outside the project

- Research whatever you need by READING files and using read-only tools.
- Do NOT create, edit, send, or delete anything outside this project folder.
  Do NOT call any tool that changes an external system (issue trackers, chat,
  email, calendars, etc.). This run only PLANS.
- Write a single markdown proposal to `PROPOSAL_FILE={proposal}` containing:
  1. A short summary of what you found.
  2. A section "## Proposed actions" listing each external action you intend,
     one bullet per action, concrete and self-contained (exactly what a later
     run must do). If no external action is warranted, say so plainly.
- Write ONLY that proposal file. Do not modify any other file.

PROPOSAL_FILE={proposal}
"###,
        name = a.name,
        prompt = a.prompt,
        files = files,
        proposal = staging.join("proposal.md").display(),
    )
}

/// Phase-2: execute exactly the approved actions (MCP tools available).
pub fn apply_prompt(a: &Automation, proposal: &str) -> String {
    format!(
        r#"You are Ken carrying out the approved actions for the automation "{name}".

The user has reviewed and APPROVED the plan below. Execute exactly these actions
using the tools available to you (including any MCP servers configured in the
user's Claude setup). Do only what the approved plan says — nothing more. When
done, reply with a one-line confirmation of what you did.

## Approved plan

{proposal}
"#,
        name = a.name,
        proposal = proposal,
    )
}

/// auto_apply=true: one session researches AND acts.
pub fn direct_prompt(a: &Automation, matched: &[String]) -> String {
    let files = if matched.is_empty() {
        String::from("(no specific files — inspect the folders your patterns cover)")
    } else {
        matched.iter().map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n")
    };
    format!(
        r#"You are Ken running the automation "{name}".

## What to do

{prompt}

## Files that triggered this run

{files}

Research what you need, then carry out the actions directly using the tools
available to you (including any MCP servers configured in the user's Claude
setup). When done, reply with a one-line confirmation.
"#,
        name = a.name, prompt = a.prompt, files = files,
    )
}

pub fn list(project: &Project) -> Result<Vec<AutomationEntry>> {
    let dir = automations_dir(&project.root);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut names: Vec<_> = fs::read_dir(&dir)
        .map_err(|e| Error::io(&dir, e))?
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
        .map(|e| e.path())
        .collect();
    names.sort();
    let mut out = Vec::new();
    for path in names {
        let slug = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        match load(&path, &slug) {
            Ok(a) => out.push(AutomationEntry::Ok { automation: a }),
            Err(e) => out.push(AutomationEntry::Broken {
                error: AutomationError { slug, reason: e.to_string() },
            }),
        }
    }
    Ok(out)
}

/// Just the valid automations (the engine's trigger path ignores broken ones).
pub fn list_ok(project: &Project) -> Result<Vec<Automation>> {
    Ok(list(project)?
        .into_iter()
        .filter_map(|e| match e {
            AutomationEntry::Ok { automation } => Some(automation),
            _ => None,
        })
        .collect())
}

pub fn load_slug(project: &Project, slug: &str) -> Result<Automation> {
    load(&automation_path(&project.root, slug), slug)
}

fn load(path: &Path, slug: &str) -> Result<Automation> {
    let raw = fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
    let (fm_str, body) = split_frontmatter(&raw)
        .ok_or_else(|| Error::Other("missing frontmatter — the file must start with a --- block".into()))?;
    let fm: Frontmatter = serde_yaml::from_str(fm_str)
        .map_err(|e| Error::Other(format!("frontmatter problem: {e}")))?;
    let a = Automation {
        slug: slug.to_string(),
        name: fm.name,
        globs: fm.globs,
        prompt: body.trim().to_string(),
        auto_apply: fm.auto_apply,
        enabled: fm.enabled,
        extra: fm.extra,
    };
    validate(&a)?;
    Ok(a)
}

pub fn save(project: &Project, a: &Automation) -> Result<()> {
    validate(a)?;
    let fm = Frontmatter {
        name: a.name.clone(),
        globs: a.globs.clone(),
        auto_apply: a.auto_apply,
        enabled: a.enabled,
        extra: a.extra.clone(),
    };
    let yaml = serde_yaml::to_string(&fm).map_err(|e| Error::Other(e.to_string()))?;
    let dir = automations_dir(&project.root);
    fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
    let path = automation_path(&project.root, &a.slug);
    fs::write(&path, format!("---\n{yaml}---\n\n{}\n", a.prompt.trim()))
        .map_err(|e| Error::io(&path, e))
}

pub fn delete(project: &Project, slug: &str) -> Result<()> {
    let path = automation_path(&project.root, slug);
    fs::remove_file(&path).map_err(|e| Error::io(&path, e))
}

pub fn validate(a: &Automation) -> Result<()> {
    let fail = |r: &str| Err(Error::Other(format!("automation '{}': {r}", a.slug)));
    if a.slug.trim().is_empty() || a.slug.contains('/') || a.slug.starts_with('.') {
        return fail("the file name must be a simple slug");
    }
    if a.name.trim().is_empty() {
        return fail("name can't be empty");
    }
    if a.globs.is_empty() || a.globs.iter().all(|g| g.trim().is_empty()) {
        return fail("add at least one file pattern to watch");
    }
    if a.prompt.trim().is_empty() {
        return fail("the prompt can't be empty — say what Ken should do");
    }
    Ok(())
}

/// Same frontmatter splitter as recipes (kept local to avoid a cross-module dep).
fn split_frontmatter(raw: &str) -> Option<(&str, &str)> {
    let rest = raw.strip_prefix("---")?;
    let rest = rest.strip_prefix('\n').or_else(|| rest.strip_prefix("\r\n"))?;
    let end = rest.find("\n---")?;
    Some((&rest[..end + 1], {
        let after = &rest[end + 4..];
        after.strip_prefix('\n').unwrap_or(after)
    }))
}

/// Match a project-relative path against a glob. `**` crosses `/`; `*` and `?`
/// do not. Case-sensitive. Pure so it's unit-tested without a filesystem.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let p: Vec<&str> = pattern.split('/').collect();
    let s: Vec<&str> = path.split('/').collect();
    seg_match(&p, &s)
}

fn seg_match(p: &[&str], s: &[&str]) -> bool {
    match p.first() {
        None => s.is_empty(),
        Some(&"**") => {
            // `**` consumes zero or more path segments.
            if seg_match(&p[1..], s) {
                return true;
            }
            !s.is_empty() && seg_match(p, &s[1..])
        }
        Some(seg) => {
            if s.is_empty() {
                return false;
            }
            wildcard_match(seg, s[0]) && seg_match(&p[1..], &s[1..])
        }
    }
}

/// `*` (any run, no `/`) and `?` (one char) within a single path segment.
fn wildcard_match(pat: &str, text: &str) -> bool {
    let pat: Vec<char> = pat.chars().collect();
    let text: Vec<char> = text.chars().collect();
    fn go(pat: &[char], text: &[char]) -> bool {
        match pat.first() {
            None => text.is_empty(),
            Some('*') => go(&pat[1..], text) || (!text.is_empty() && go(pat, &text[1..])),
            Some('?') => !text.is_empty() && go(&pat[1..], &text[1..]),
            Some(&c) => !text.is_empty() && text[0] == c && go(&pat[1..], &text[1..]),
        }
    }
    go(&pat, &text)
}

#[cfg(test)]
mod glob_tests {
    use super::glob_match;
    #[test]
    fn star_does_not_cross_slash() {
        assert!(glob_match("Recordings/*.md", "Recordings/a.md"));
        assert!(!glob_match("Recordings/*.md", "Recordings/sub/a.md"));
    }
    #[test]
    fn doublestar_crosses_slash() {
        assert!(glob_match("Recordings/**/*.md", "Recordings/sub/a.md"));
        assert!(glob_match("**/*.md", "a/b/c.md"));
        assert!(!glob_match("**/*.md", "a/b/c.txt"));
    }
    #[test]
    fn question_matches_single_non_slash() {
        assert!(glob_match("a?.md", "ab.md"));
        assert!(!glob_match("a?.md", "a/b.md"));
    }
    #[test]
    fn literal_and_case_sensitive() {
        assert!(glob_match("Notes/People.md", "Notes/People.md"));
        assert!(!glob_match("Notes/People.md", "notes/people.md"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Project;
    use tempfile::tempdir;

    fn project() -> (tempfile::TempDir, Project) {
        let d = tempdir().unwrap();
        let p = Project::create(d.path(), "T").unwrap();
        (d, p)
    }

    fn sample() -> Automation {
        Automation {
            slug: "weekly-jira".into(),
            name: "Weekly Jira from recordings".into(),
            globs: vec!["Recordings/*.md".into()],
            prompt: "Summarize the recording and propose Jira tasks.".into(),
            auto_apply: false,
            enabled: true,
            extra: serde_yaml::Mapping::new(),
        }
    }

    #[test]
    fn save_load_roundtrip_and_default_auto_apply_false() {
        let (_d, p) = project();
        save(&p, &sample()).unwrap();
        let loaded = load_slug(&p, "weekly-jira").unwrap();
        assert_eq!(loaded, sample());
        assert!(!loaded.auto_apply);
        assert!(loaded.enabled);
    }

    #[test]
    fn triggers_returns_matched_paths_only() {
        let a = sample();
        let hits = triggers(&a, &[
            "Recordings/2026-07-13 14.02 Recording.md".into(),
            "Notes/other.md".into(),
        ]);
        assert_eq!(hits, vec!["Recordings/2026-07-13 14.02 Recording.md".to_string()]);
    }

    #[test]
    fn list_ok_skips_broken_files() {
        let (_d, p) = project();
        save(&p, &sample()).unwrap();
        std::fs::write(automation_path(&p.root, "broken"), "no frontmatter").unwrap();
        let ok = list_ok(&p).unwrap();
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].slug, "weekly-jira");
    }

    #[test]
    fn validate_rejects_empty_globs_and_prompt() {
        let mut a = sample();
        a.globs = vec![];
        assert!(validate(&a).is_err());
        a = sample();
        a.prompt = "  ".into();
        assert!(validate(&a).is_err());
    }

    #[test]
    fn proposal_prompt_names_files_staging_and_forbids_external_writes() {
        let a = sample();
        let matched = vec!["Recordings/a.md".to_string()];
        let staging = std::path::Path::new("/tmp/stg");
        let p = proposal_prompt(&a, &matched, staging);
        assert!(p.contains("Recordings/a.md"));
        assert!(p.contains("PROPOSAL_FILE=/tmp/stg/proposal.md"));
        assert!(p.to_lowercase().contains("do not") || p.to_lowercase().contains("must not"));
        assert!(p.contains(&a.prompt));
    }

    #[test]
    fn apply_prompt_embeds_the_approved_proposal() {
        let a = sample();
        let p = apply_prompt(&a, "## Proposed actions\n- Create issue X");
        assert!(p.contains("Create issue X"));
        assert!(p.to_lowercase().contains("execute"));
    }

    #[test]
    fn direct_prompt_used_when_auto_apply() {
        let a = sample();
        let p = direct_prompt(&a, &["Recordings/a.md".into()]);
        assert!(p.contains("Recordings/a.md"));
        assert!(p.contains(&a.prompt));
    }
}

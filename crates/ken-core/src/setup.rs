//! Set-up for Ken on its own: Folder, Team, Repos, Index. The scan reads a
//! code folder and proposes one row per repo (kind, team, index state, with
//! the evidence for each), and the ignore entries it would add; nothing is
//! written until a person confirms, and Ken reads nothing until then. Scan
//! again reports what moved instead of overwriting.
//!
//! What the scan reads, cheaply and without a model: folder grouping, `.git`
//! and its `origin` URL (read from `.git/config`, no process), repo markers,
//! the signs of a library (a decisions log, a vault, the method's sections),
//! the signs of a team repo (tickets, decisions, ideas), a README that names
//! a sibling, and `git worktree list` for second checkouts.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::registry::{IndexState, Registry, RepoKind};
use crate::workspace::{self, Workspace};
use crate::Result;

/// One repo the scan found, as a row a person can change before Confirm.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoRow {
    /// Folder relative to the code folder (`Game` or `SR/Game`).
    pub member: String,
    pub include: bool,
    pub kind: Vec<RepoKind>,
    pub team: Option<String>,
    pub index: IndexState,
    /// Why the scan proposed what it did, one fact per entry.
    pub evidence: Vec<String>,
    pub remote: Option<String>,
    /// Already a Ken project (it has `.ken/project.json`).
    pub existing: bool,
    pub has_git: bool,
    /// The repo's folder. Always set when repos are picked one by one.
    #[serde(default)]
    pub path: Option<PathBuf>,
    /// What the repo is and how to use it, first proposed from its README;
    /// a person edits it. Kept in the registry and read by the first-wiki
    /// draft.
    #[serde(default)]
    pub description: String,
}

/// One ignore line the scan would add to the code folder's `.kenignore`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IgnoreRow {
    pub pattern: String,
    /// `off` (a bare line) or `search` (a `~` line).
    pub state: IndexState,
    pub reason: String,
    pub evidence: String,
    pub ticked: bool,
    /// Shown so a person sees it, never a choice (secrets are built in).
    pub fixed: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Proposal {
    pub folder: PathBuf,
    pub repos: usize,
    pub without_git: usize,
    pub rows: Vec<RepoRow>,
    pub teams: Vec<String>,
    pub ignores: Vec<IgnoreRow>,
    /// The folder already has a Ken workspace: Confirm updates it.
    pub existing_workspace: bool,
}

/// What a second scan found that the confirmed set-up does not have.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "change")]
pub enum Moved {
    NewFolder { member: String, evidence: Vec<String> },
    Gone { member: String },
    NewWorktree { pattern: String, evidence: String },
}

/// What a person took out, so it is not proposed twice
/// (`.ken-workspace/setup.json`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Declined {
    #[serde(default)]
    pub members: Vec<String>,
    #[serde(default)]
    pub ignores: Vec<String>,
}

fn declined_path(parent: &Path) -> PathBuf {
    parent.join(workspace::CONFIG_DIR).join("setup.json")
}

pub fn declined(parent: &Path) -> Declined {
    fs::read_to_string(declined_path(parent))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// The `origin` URL from `.git/config`, if any.
pub fn origin_url(repo: &Path) -> Option<String> {
    let cfg = fs::read_to_string(repo.join(".git").join("config")).ok()?;
    let mut in_origin = false;
    for line in cfg.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_origin = t == "[remote \"origin\"]";
            continue;
        }
        if in_origin {
            if let Some(url) = t.strip_prefix("url").map(str::trim).and_then(|r| r.strip_prefix('=')) {
                return Some(url.trim().to_string());
            }
        }
    }
    None
}

fn exists_ci(dir: &Path, rel: &str) -> bool {
    dir.join(rel).exists()
        || fs::read_dir(dir).ok().is_some_and(|mut it| {
            it.any(|e| e.ok().is_some_and(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(rel)))
        })
}

/// A repo's kinds from what is in it, with the evidence.
pub fn detect(dir: &Path) -> (Vec<RepoKind>, Vec<String>) {
    let mut kind = Vec::new();
    let mut ev = Vec::new();
    let library: Vec<&str> = [
        ("_meta/DECISIONS.md", "_meta/DECISIONS.md"),
        ("DECISIONS.md", "a decisions log"),
        (".obsidian", "an Obsidian vault"),
        ("Research/Ingestion", "Research/Ingestion"),
        ("Ways-of-Working", "a Ways-of-Working section"),
        ("Current", "a Current section"),
        ("Reference/Vocabulary.md", "a Vocabulary page"),
        // Where libraries laid down before the 2026-10 template kept it.
        ("Vocabulary.md", "a Vocabulary page"),
        ("START-HERE.md", "START-HERE.md"),
    ]
    .into_iter()
    .filter(|(p, _)| dir.join(p).exists())
    .map(|(_, e)| e)
    .collect();
    // A team repo says so with its manifest. A folder name is not evidence:
    // a code repo can keep a `tickets/README.md` pointing at where tickets
    // moved, and that one file made it a team repo read in full.
    let team: Vec<&str> = if dir.join(".wright").join("team.json").is_file() { vec![".wright/team.json"] } else { Vec::new() };
    let mut code: Vec<String> = crate::profiler::REPO_MARKERS
        .iter()
        .copied()
        .filter(|m| *m != ".git" && exists_ci(dir, m))
        .map(str::to_string)
        .chain(
            fs::read_dir(dir)
                .ok()
                .into_iter()
                .flatten()
                .flatten()
                .any(|e| e.file_name().to_string_lossy().to_lowercase().ends_with(".sln"))
                .then(|| "*.sln".to_string()),
        )
        .collect();
    // A monorepo keeps its packages a level or two down
    // (`services/api/pyproject.toml`, `apps/web/package.json`). Only in a
    // repo: a folder without git whose children hold markers is a group of
    // repos, each detected on its own.
    if code.is_empty() && dir.join(".git").exists() {
        code = nested_markers(dir, 2);
    }
    if !team.is_empty() {
        kind.push(RepoKind::Team);
        ev.push(team.join(", "));
    }
    if library.len() >= 1 {
        kind.push(RepoKind::Wiki);
        ev.push(library.join(", "));
    }
    if !code.is_empty() {
        kind.push(RepoKind::Code);
        ev.push(code.join(", "));
    }
    (kind, ev)
}

/// Code markers in the folders below `dir`, at most `depth` levels down, as
/// the relative paths that prove it (at most three). Skips hidden folders
/// and the ones dependencies and builds fill.
fn nested_markers(dir: &Path, depth: usize) -> Vec<String> {
    const SKIP: [&str; 7] = ["node_modules", "vendor", "target", "dist", "build", "venv", "__pycache__"];
    let mut found = Vec::new();
    let mut level: Vec<(PathBuf, String)> = vec![(dir.to_path_buf(), String::new())];
    for _ in 0..depth {
        let mut next = Vec::new();
        for (path, rel) in &level {
            let Ok(entries) = fs::read_dir(path) else { continue };
            let mut dirs: Vec<_> = entries
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| !n.starts_with('.') && !SKIP.contains(&n.to_lowercase().as_str()))
                .collect();
            dirs.sort();
            for name in dirs {
                let child = path.join(&name);
                let child_rel = if rel.is_empty() { name } else { format!("{rel}/{name}") };
                for m in crate::profiler::REPO_MARKERS.iter().filter(|m| **m != ".git") {
                    if exists_ci(&child, m) && found.len() < 3 {
                        found.push(format!("{child_rel}/{m}"));
                    }
                }
                next.push((child, child_rel));
            }
        }
        if !found.is_empty() {
            break;
        }
        level = next;
    }
    found
}

/// The owner in a git remote URL (`github.com/owner/repo`, `git@host:owner/repo`).
pub fn remote_owner(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let rest = rest.split_once('@').map_or(rest, |(_, r)| r);
    let path = rest.split_once([':', '/']).map(|(_, p)| p)?;
    let owner = path.trim_start_matches('/').split('/').next()?;
    (!owner.is_empty()).then(|| owner.to_string())
}

fn readme_text(dir: &Path) -> String {
    ["README.md", "START-HERE.md", "CLAUDE.md", "readme.md"]
        .iter()
        .filter_map(|f| fs::read_to_string(dir.join(f)).ok())
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase()
}

fn names_word(haystack: &str, word: &str) -> bool {
    let w = word.to_lowercase();
    w.len() >= 4
        && haystack.match_indices(&w).any(|(i, _)| {
            let before = haystack[..i].chars().next_back();
            let after = haystack[i + w.len()..].chars().next();
            !before.is_some_and(|c| c.is_alphanumeric()) && !after.is_some_and(|c| c.is_alphanumeric())
        })
}

/// A team name from a docs repo's folder name: `Realms-Docs` → `Realms`.
fn team_from_docs(leaf: &str) -> String {
    let lower = leaf.to_lowercase();
    for suffix in ["-docs", "_docs", " docs", "-wiki", "_wiki", "-knowledge"] {
        if lower.ends_with(suffix) {
            return leaf[..leaf.len() - suffix.len()].to_string();
        }
    }
    leaf.to_string()
}

/// Second checkouts of the repos found: every `git worktree list` path under
/// the folder other than the repo itself, as a folder relative to it.
fn worktrees(parent: &Path, repo: &Path) -> Vec<(String, String)> {
    let mut cmd = std::process::Command::new("git");
    let Ok(out) = crate::proc::quiet(&mut cmd).args(["worktree", "list", "--porcelain"]).current_dir(repo).output() else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let canon = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let (me, root) = (canon(repo), canon(parent));
    let mut found = Vec::new();
    let mut path: Option<PathBuf> = None;
    for line in text.lines().chain(std::iter::once("")) {
        if let Some(p) = line.strip_prefix("worktree ") {
            path = Some(PathBuf::from(p));
        } else if line.is_empty() {
            if let Some(p) = path.take().map(|p| canon(&p)) {
                if p != me {
                    if let Ok(rel) = p.strip_prefix(&root) {
                        let rel = rel.to_string_lossy().replace('\\', "/");
                        let leaf = repo.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                        found.push((rel, format!("a second checkout of {leaf}")));
                    }
                }
            }
        }
    }
    found
}

/// Scan `parent` and propose the set-up. Reads only; writes nothing.
pub fn propose(parent: &Path) -> Result<Proposal> {
    let candidates = workspace::discover_candidates(parent)?;
    let declined = declined(parent);
    let existing_ws = Workspace::open(parent).ok();
    let mut rows: Vec<RepoRow> = Vec::new();
    for c in candidates {
        let dir = parent.join(&c.name);
        let has_git = dir.join(".git").is_dir();
        let (kind, mut evidence) = detect(&dir);
        if !has_git && kind.is_empty() {
            evidence.push("no git · documents".into());
        }
        let remote = has_git.then(|| origin_url(&dir)).flatten();
        if let Some(r) = &remote {
            evidence.push(format!("remote {r}"));
        }
        let index = if !has_git && kind.is_empty() && c.file_count == 0 && c.markers.is_empty() {
            IndexState::Off
        } else {
            IndexState::for_kind(&kind)
        };
        let member_of_ws = existing_ws.as_ref().is_some_and(|w| w.config.members.contains(&c.name));
        rows.push(RepoRow {
            include: !declined.members.contains(&c.name) && (member_of_ws || existing_ws.is_none()) && index != IndexState::Off,
            team: workspace::member_group(&c.name).map(str::to_string),
            member: c.name,
            kind,
            index,
            evidence,
            remote,
            existing: c.existing,
            has_git,
            description: readme_summary(&dir),
            path: Some(dir),
        });
    }

    suggest_teams(&mut rows, |r| parent.join(&r.member));
    let rows = rows;

    // Ignore entries: second checkouts, and the secrets that are built in.
    let already = workspace::ignore_rules(parent);
    let mut ignores: Vec<IgnoreRow> = Vec::new();
    for r in rows.iter().filter(|r| r.has_git) {
        for (rel, evidence) in worktrees(parent, &parent.join(&r.member)) {
            let pattern = workspace::folder_line(&rel);
            if workspace::is_ignored_folder(&already, &rel) || ignores.iter().any(|i| i.pattern == pattern) {
                continue;
            }
            ignores.push(IgnoreRow {
                ticked: !declined.ignores.iter().any(|d| d.trim_start_matches('/') == pattern.trim_start_matches('/')),
                pattern,
                state: IndexState::Off,
                reason: "Git worktree".into(),
                evidence,
                fixed: false,
            });
        }
    }
    ignores.push(secrets_row());

    let mut teams: Vec<String> = rows.iter().filter_map(|r| r.team.clone()).collect();
    teams.sort();
    teams.dedup();
    Ok(Proposal {
        folder: parent.to_path_buf(),
        repos: rows.iter().filter(|r| r.has_git).count(),
        without_git: rows.iter().filter(|r| !r.has_git).count(),
        rows,
        teams,
        ignores,
        existing_workspace: existing_ws.is_some(),
    })
}

/// The first plain paragraph of a repo's README, one line, for its
/// proposed description; of its CLAUDE.md or AGENTS.md when it has no
/// README that says one (Shattered-Realms-Tools has none, and on 2026-10-06
/// its row read "(not said yet)" though its CLAUDE.md opens with what it is).
/// Empty when none of them says.
pub fn readme_summary(dir: &Path) -> String {
    ["README.md", "readme.md", "README", "START-HERE.md", "CLAUDE.md", "AGENTS.md"]
        .iter()
        .filter_map(|f| fs::read_to_string(dir.join(f)).ok())
        .map(|t| first_paragraph(&t))
        .find(|s| !s.is_empty())
        .unwrap_or_default()
}

/// A Markdown file's first plain paragraph as one line, at most 220 characters.
fn first_paragraph(text: &str) -> String {
    let para: Vec<&str> = text
        .lines()
        .map(str::trim)
        .skip_while(|l| l.is_empty() || l.starts_with('#') || l.starts_with("---") || l.starts_with('!') || l.starts_with('['))
        .take_while(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    let line = para.join(" ").replace("**", "");
    if line.chars().count() > 220 {
        format!("{}…", line.chars().take(219).collect::<String>())
    } else {
        line
    }
}

fn secrets_row() -> IgnoreRow {
    IgnoreRow {
        pattern: ".env* *.pem *.key id_rsa* secrets.* credentials.json".into(),
        state: IndexState::Off,
        reason: "Secrets".into(),
        evidence: "built in: never read, and no line brings them back".into(),
        ticked: true,
        fixed: true,
    }
}

/// A docs repo whose README names another row proposes them as one team.
fn suggest_teams(rows: &mut [RepoRow], dir_of: impl Fn(&RepoRow) -> PathBuf) {
    let texts: Vec<String> = rows.iter().map(|r| readme_text(&dir_of(r))).collect();
    for i in 0..rows.len() {
        if !rows[i].kind.contains(&RepoKind::Wiki) {
            continue;
        }
        let team = rows[i].team.clone().unwrap_or_else(|| team_from_docs(workspace::member_leaf(&rows[i].member)));
        let named: Vec<usize> = (0..rows.len())
            .filter(|j| *j != i && names_word(&texts[i], workspace::member_leaf(&rows[*j].member)))
            .collect();
        if named.is_empty() {
            continue;
        }
        rows[i].team.get_or_insert(team.clone());
        for j in named {
            if rows[j].team.is_none() {
                rows[j].team = Some(team.clone());
                let by = workspace::member_leaf(&rows[i].member).to_string();
                rows[j].evidence.push(format!("{by}'s README names it"));
            }
        }
    }
}

/// Write the confirmed set-up: the workspace manifest (members, teams as
/// groups), each repo's kind, team and index state in the registry at
/// `base`, the ticked ignore lines under a dated comment, and what was
/// declined. The only step that writes.
/// A repo Ken leaves untouched: code or reference, and not also team or
/// wiki. It gains no file from Ken; its project config lives in app data.
pub fn leaves_untouched(kind: &[RepoKind]) -> bool {
    !kind.is_empty() && kind.iter().all(|k| matches!(k, RepoKind::Code | RepoKind::Reference))
}

/// Give each untouched repo among `rows` its project in app data before the
/// workspace adopts it, so opening the workspace finds that one instead of
/// writing `.ken/project.json` into the repo.
fn place_untouched(rows: &[&RepoRow], folder: impl Fn(&RepoRow) -> Option<PathBuf>) -> Result<()> {
    for r in rows.iter().filter(|r| leaves_untouched(&r.kind)) {
        if let Some(dir) = folder(r) {
            crate::project::Project::create_outside(&dir, workspace::member_leaf(&r.member))?;
        }
    }
    Ok(())
}

pub fn confirm(base: &Path, parent: &Path, name: &str, rows: &[RepoRow], ignores: &[IgnoreRow], today: &str) -> Result<Workspace> {
    let chosen: Vec<&RepoRow> = rows.iter().filter(|r| r.include && r.index != IndexState::Off).collect();
    let members: Vec<String> = chosen.iter().map(|r| r.member.clone()).collect();
    place_untouched(&chosen, |r| Some(parent.join(&r.member)))?;

    // Ignore lines first, so the workspace opens with them in force.
    let fresh: Vec<&IgnoreRow> = {
        let already = workspace::ignore_rules(parent);
        ignores
            .iter()
            .filter(|i| i.ticked && !i.fixed)
            .filter(|i| !workspace::is_ignored_folder(&already, i.pattern.trim_end_matches('/')))
            .collect()
    };
    if !fresh.is_empty() {
        let path = workspace::ignore_path(parent);
        let mut text = fs::read_to_string(&path).unwrap_or_default();
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&format!("\n# Ken proposed these at set-up on {today}. Edit or delete freely.\n"));
        for i in fresh {
            // gitignore syntax: `#` starts a comment only at the start of a
            // line, so the reason goes on its own line above the pattern.
            let line = if i.state == IndexState::Search { format!("~{}", i.pattern) } else { i.pattern.clone() };
            text.push_str(&format!("# {}: {}\n{line}\n", i.reason, i.evidence));
        }
        fs::write(&path, text).map_err(|e| crate::Error::io(&path, e))?;
    }

    let mut ws = match Workspace::open(parent) {
        Ok(mut ws) => {
            for m in &members {
                if !ws.config.members.contains(m) {
                    ws.config.members.push(m.clone());
                }
            }
            ws.save()?;
            Workspace::open(parent)?
        }
        Err(_) => Workspace::create(parent, name, &members)?,
    };

    // Teams become groups, except where the team is already a group folder.
    let derived: Vec<String> = ws.config.derived_groups().into_iter().map(|g| g.name).collect();
    let mut teams: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for r in &chosen {
        if let Some(t) = &r.team {
            teams.entry(t.clone()).or_default().push(r.member.clone());
        }
    }
    for (team, ms) in teams {
        if !derived.iter().any(|d| d.eq_ignore_ascii_case(&team)) {
            ws.config.set_group(&team, &ms)?;
        }
    }
    ws.save()?;

    let mut reg = Registry::load(base)?;
    for m in &ws.members {
        let workspace::MemberStatus::Ok(p) = &m.status else { continue };
        let Some(row) = chosen.iter().find(|r| r.member == m.name) else { continue };
        reg.add(p);
        reg.set_kind(p.config.id, row.kind.clone(), row.team.clone());
        reg.set_index(p.config.id, Some(row.index));
        reg.set_description(p.config.id, &row.description);
    }
    reg.save(base)?;

    let mut d = declined(parent);
    // Only what a person took out; a folder with nothing to index was never
    // offered, so it is not a refusal.
    for r in rows.iter().filter(|r| !r.include && r.index != IndexState::Off) {
        if !d.members.contains(&r.member) {
            d.members.push(r.member.clone());
        }
    }
    for i in ignores.iter().filter(|i| !i.ticked && !i.fixed) {
        if !d.ignores.contains(&i.pattern) {
            d.ignores.push(i.pattern.clone());
        }
    }
    let path = declined_path(parent);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| crate::Error::io(dir, e))?;
    }
    let json = serde_json::to_string_pretty(&d).map_err(|e| crate::Error::Other(e.to_string()))?;
    fs::write(&path, json + "\n").map_err(|e| crate::Error::io(&path, e))?;

    Workspace::open(parent)
}

/// A row for one folder a person picked. A linked worktree comes back
/// unticked and off, named as a second checkout.
fn row_for(dir: &Path, member: String) -> RepoRow {
    let has_git = dir.join(".git").is_dir();
    let (kind, mut evidence) = detect(dir);
    if !has_git && kind.is_empty() {
        evidence.push("no git · documents".into());
    }
    let remote = has_git.then(|| origin_url(dir)).flatten();
    if let Some(r) = &remote {
        evidence.push(format!("remote {r}"));
    }
    let worktree = workspace::is_git_worktree(dir);
    if worktree {
        evidence.insert(0, "a second checkout (git worktree) of another repo".into());
    }
    let index = if worktree { IndexState::Off } else { IndexState::for_kind(&kind) };
    RepoRow {
        include: !worktree,
        team: None,
        member,
        kind,
        index,
        evidence,
        remote,
        existing: crate::project::existing_config(dir).is_some(),
        has_git,
        description: readme_summary(dir),
        path: Some(dir.to_path_buf()),
    }
}

/// The canonical form of `p` without Windows' verbatim prefix (`\\?\C:\…`),
/// so a folder reads as a person would type it wherever it is shown or stored.
pub fn plain_canonical(p: &Path) -> PathBuf {
    let c = fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let s = c.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    match s.strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => c,
    }
}

fn is_repo_like(dir: &Path) -> bool {
    dir.join(".git").exists() || !detect(dir).0.is_empty()
}

/// Propose rows for repos a person picked one by one, wherever they are.
/// A picked folder that is not a repo itself but holds repos (a group
/// folder) stands for each repo inside it. Names are the folder names,
/// made unique with `-2`, `-3`; `taken` holds names already in use (the
/// open workspace's members, when adding). Reads only.
pub fn propose_repos(picked: &[PathBuf], taken: &[String]) -> Result<Proposal> {
    let canon = |p: &Path| plain_canonical(p);
    let mut dirs: Vec<PathBuf> = Vec::new();
    for p in picked {
        let children: Vec<PathBuf> = if is_repo_like(p) {
            Vec::new()
        } else {
            let mut c: Vec<PathBuf> = fs::read_dir(p)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .filter(|c| c.is_dir() && is_repo_like(c))
                .filter(|c| !c.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')))
                .collect();
            c.sort();
            c
        };
        let add = if children.is_empty() { vec![p.clone()] } else { children };
        for d in add {
            let d = canon(&d);
            if !dirs.contains(&d) {
                dirs.push(d);
            }
        }
    }
    let mut used: Vec<String> = taken.to_vec();
    let mut rows: Vec<RepoRow> = Vec::new();
    for d in &dirs {
        let leaf = d.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "repo".into());
        let leaf = leaf.trim_start_matches('.').to_string();
        let mut name = leaf.clone();
        let mut n = 2;
        while used.iter().any(|u| u.eq_ignore_ascii_case(&name)) {
            name = format!("{leaf}-{n}");
            n += 1;
        }
        used.push(name.clone());
        rows.push(row_for(d, name));
    }
    suggest_teams(&mut rows, |r| r.path.clone().unwrap_or_default());
    let mut teams: Vec<String> = rows.iter().filter_map(|r| r.team.clone()).collect();
    teams.sort();
    teams.dedup();
    Ok(Proposal {
        folder: PathBuf::new(),
        repos: rows.iter().filter(|r| r.has_git).count(),
        without_git: rows.iter().filter(|r| !r.has_git).count(),
        rows,
        teams,
        ignores: vec![secrets_row()],
        existing_workspace: false,
    })
}

/// Create a team's wiki at `dir` from the bundled template (see
/// [`crate::wikinew`]), covering `repos`, and return its set-up row: a wiki
/// on `team`, read for entities. `taken` are names already in use.
pub fn create_wiki(dir: &Path, team: &str, repos: &[crate::wikinew::Covered], taken: &[String], today: &str) -> Result<RepoRow> {
    create_wiki_with(dir, team, repos, taken, today, false)
}

/// [`create_wiki`]; with `holds_team` the wiki is also the team repo (the
/// team has no team repo of its own): it gets the team template's folders and
/// both kinds, so tickets, decisions and people live in the docs repo.
pub fn create_wiki_with(
    dir: &Path,
    team: &str,
    repos: &[crate::wikinew::Covered],
    taken: &[String],
    today: &str,
    holds_team: bool,
) -> Result<RepoRow> {
    crate::wikinew::create_with(dir, team, repos, today, holds_team)?;
    let mut row = propose_repos(&[dir.to_path_buf()], taken)?
        .rows
        .into_iter()
        .next()
        .ok_or_else(|| crate::Error::Other(format!("{} could not be read back", dir.display())))?;
    row.include = true;
    row.kind = if holds_team { vec![RepoKind::Team, RepoKind::Wiki] } else { vec![RepoKind::Wiki] };
    row.team = Some(team.to_string());
    row.index = IndexState::Entities;
    row.description = format!("The {team} team's wiki: what is true now, how the team works, and what lives where in each repo.");
    row.evidence.insert(0, "created at set-up from the Ways-of-Working template".into());
    Ok(row)
}

/// Create a team's team repo at `dir` from the bundled template (see
/// [`crate::teamnew`]) and return its set-up row: a team repo on `team`,
/// read for entities. `wiki` is the team wiki's name, when it has one;
/// `repos` the team's other repos, named in its manifest.
pub fn create_team_repo(
    dir: &Path,
    team: &str,
    wiki: Option<&str>,
    repos: &[crate::wikinew::Covered],
    taken: &[String],
) -> Result<RepoRow> {
    crate::teamnew::create(dir, team, wiki, repos)?;
    let mut row = propose_repos(&[dir.to_path_buf()], taken)?
        .rows
        .into_iter()
        .next()
        .ok_or_else(|| crate::Error::Other(format!("{} could not be read back", dir.display())))?;
    row.include = true;
    row.kind = vec![RepoKind::Team];
    row.team = Some(team.to_string());
    row.index = IndexState::Entities;
    row.description = format!("The {team} team's tickets, decisions, ideas and people.");
    row.evidence.insert(0, "created at set-up from the Ways-of-Working template".into());
    Ok(row)
}

/// Where a workspace of picked repos lives: Ken's app data.
pub fn workspaces_dir(base: &Path) -> PathBuf {
    base.join("workspaces")
}

/// Write the confirmed set-up of picked repos into the workspace at `root`
/// (new under [`workspaces_dir`], or the open one when adding): the manifest
/// with each member's folder and the teams as groups, and each repo's kind,
/// team, index state and description in the registry at `base`. Nothing is
/// written into a shared parent folder. The only step that writes.
pub fn confirm_repos(base: &Path, root: &Path, name: &str, rows: &[RepoRow]) -> Result<Workspace> {
    let chosen: Vec<&RepoRow> =
        rows.iter().filter(|r| r.include && r.index != IndexState::Off && r.path.is_some()).collect();
    let members: Vec<(String, PathBuf)> =
        chosen.iter().filter_map(|r| r.path.clone().map(|p| (r.member.clone(), p))).collect();
    place_untouched(&chosen, |r| r.path.clone())?;
    let mut ws = Workspace::create_at(root, name, &members)?;

    let mut teams: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for r in &chosen {
        if let Some(t) = &r.team {
            teams.entry(t.clone()).or_default().push(r.member.clone());
        }
    }
    for (team, ms) in teams {
        let mut all = ws.config.group(&team).map(|g| g.members.clone()).unwrap_or_default();
        for m in ms {
            if !all.contains(&m) {
                all.push(m);
            }
        }
        ws.config.set_group(&team, &all)?;
    }
    ws.save()?;

    let mut reg = Registry::load(base)?;
    for m in &ws.members {
        let workspace::MemberStatus::Ok(p) = &m.status else { continue };
        let Some(row) = chosen.iter().find(|r| r.member == m.name) else { continue };
        reg.add(p);
        reg.set_kind(p.config.id, row.kind.clone(), row.team.clone());
        reg.set_index(p.config.id, Some(row.index));
        reg.set_description(p.config.id, &row.description);
    }
    reg.save(base)?;
    Workspace::open(root)
}

/// Scan again and report what moved since set-up: folders that are new (and
/// were not declined), members gone from disk, and new second checkouts.
/// Writes nothing.
pub fn rescan(parent: &Path) -> Result<Vec<Moved>> {
    let ws = Workspace::open(parent)?;
    let proposal = propose(parent)?;
    let declined = declined(parent);
    let mut out = Vec::new();
    for r in &proposal.rows {
        if !ws.config.members.contains(&r.member) && !declined.members.contains(&r.member) && r.index != IndexState::Off {
            out.push(Moved::NewFolder { member: r.member.clone(), evidence: r.evidence.clone() });
        }
    }
    for m in &ws.members {
        if matches!(m.status, workspace::MemberStatus::Missing) {
            out.push(Moved::Gone { member: m.name.clone() });
        }
    }
    for i in proposal.ignores.iter().filter(|i| !i.fixed && i.ticked) {
        out.push(Moved::NewWorktree { pattern: i.pattern.clone(), evidence: i.evidence.clone() });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_docs_repo_with_no_team_repo_holds_the_tickets_too() {
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("Realms-Wiki");
        let row = create_wiki_with(&dir, "Realms", &[], &[], "2026-10-06", true).unwrap();
        assert_eq!(row.kind, vec![RepoKind::Team, RepoKind::Wiki]);
        for f in ["tickets/TICKET.md", "decisions/DECISIONS.md", "people/README.md", ".wright/team.json", "Current/Project.md"] {
            assert!(dir.join(f).exists(), "{f}");
        }
        // One entry of both kinds, not the docs repo twice (2026-10-06).
        let manifest: serde_json::Value = serde_json::from_str(&fs::read_to_string(dir.join(".wright/team.json")).unwrap()).unwrap();
        assert_eq!(manifest["repos"]["Realms-Wiki"]["kind"], serde_json::json!(["team", "wiki"]));
        let text = fs::read_to_string(dir.join(".wright/team.json")).unwrap();
        assert_eq!(text.matches("\"Realms-Wiki\": {").count(), 1, "{text}");
        let alone = create_wiki(&parent.path().join("Other-Wiki"), "Other", &[], &[], "2026-10-06").unwrap();
        assert_eq!(alone.kind, vec![RepoKind::Wiki]);
        assert!(!parent.path().join("Other-Wiki/tickets").exists());
    }

    #[test]
    fn a_repo_with_no_readme_is_described_from_its_brief() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("CLAUDE.md"), "# ShatterdRealmsTools\n\nThe authoring suite. pnpm workspace.\n\n## Repos\n").unwrap();
        assert_eq!(readme_summary(d.path()), "The authoring suite. pnpm workspace.");
        fs::write(d.path().join("README.md"), "# Tools\n\n![badge](x.svg)\n").unwrap();
        assert_eq!(readme_summary(d.path()), "The authoring suite. pnpm workspace.", "a README that says nothing is passed over");
        fs::write(d.path().join("README.md"), "# Tools\n\nThe level editor.\n").unwrap();
        assert_eq!(readme_summary(d.path()), "The level editor.");
    }

    #[test]
    fn a_stray_tickets_folder_does_not_make_a_team_repo() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("tickets")).unwrap();
        fs::write(d.path().join("tickets/README.md"), "Tickets moved to the docs repo.").unwrap();
        fs::write(d.path().join("build.gradle.kts"), "").unwrap();
        let (kind, _) = detect(d.path());
        assert_eq!(kind, vec![RepoKind::Code]);
        fs::create_dir_all(d.path().join(".wright")).unwrap();
        fs::write(d.path().join(".wright/team.json"), "{}").unwrap();
        assert!(detect(d.path()).0.contains(&RepoKind::Team));
    }
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// A code folder like the report's: a docs repo whose README names the
    /// game, the game with a worktree beside it, a group folder of tools, a
    /// folder of documents with no git, and an empty folder.
    fn code_folder() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        fs::create_dir_all(p.join("Realms-Docs/_meta")).unwrap();
        fs::create_dir_all(p.join("Realms-Docs/.obsidian")).unwrap();
        fs::write(p.join("Realms-Docs/_meta/DECISIONS.md"), "# Decisions\n").unwrap();
        fs::write(p.join("Realms-Docs/README.md"), "Docs for Realms-Game and its tools.\n").unwrap();
        fs::create_dir_all(p.join("Realms-Docs/.git")).unwrap();

        let game = p.join("Realms-Game");
        fs::create_dir_all(&game).unwrap();
        git(&game, &["init", "-q", "-b", "main"]);
        git(&game, &["config", "user.email", "t@t"]);
        git(&game, &["config", "user.name", "t"]);
        git(&game, &["remote", "add", "origin", "git@github.com:Stingbro/Realms-Game.git"]);
        fs::write(game.join("Cargo.toml"), "[package]\n").unwrap();
        git(&game, &["add", "-A"]);
        git(&game, &["commit", "-q", "-m", "init"]);
        git(&game, &["worktree", "add", "-q", "../Realms-Game-u7", "-b", "u7"]);

        fs::create_dir_all(p.join("SR/tools/.git")).unwrap();
        fs::write(p.join("SR/tools/package.json"), "{}").unwrap();
        fs::create_dir_all(p.join("Personal")).unwrap();
        fs::write(p.join("Personal/notes.md"), "notes\n").unwrap();
        fs::create_dir_all(p.join("Empty")).unwrap();
        d
    }

    #[test]
    fn the_scan_proposes_kinds_teams_states_and_worktrees_with_evidence() {
        let d = code_folder();
        let p = propose(d.path()).unwrap();
        let row = |m: &str| p.rows.iter().find(|r| r.member == m).unwrap_or_else(|| panic!("{m}: {:?}", p.rows));

        let docs = row("Realms-Docs");
        assert_eq!(docs.kind, vec![RepoKind::Wiki]);
        assert_eq!(docs.index, IndexState::Entities);
        assert_eq!(docs.team.as_deref(), Some("Realms"));
        assert!(docs.evidence.iter().any(|e| e.contains("DECISIONS")), "{:?}", docs.evidence);

        let game = row("Realms-Game");
        assert_eq!((game.kind.clone(), game.index), (vec![RepoKind::Code], IndexState::Search));
        assert_eq!(game.team.as_deref(), Some("Realms"), "named by the docs README");
        assert_eq!(game.remote.as_deref(), Some("git@github.com:Stingbro/Realms-Game.git"));

        assert_eq!(row("SR/tools").team.as_deref(), Some("SR"), "the group folder is the team");
        let personal = row("Personal");
        assert!(personal.kind.is_empty() && personal.index == IndexState::Entities && !personal.has_git);
        assert!(!p.rows.iter().any(|r| r.member == "Realms-Game-u7"), "a worktree is never a row");
        assert_eq!(row("Empty").index, IndexState::Off);

        let wt = p.ignores.iter().find(|i| i.reason == "Git worktree").expect("worktree proposed");
        assert_eq!(wt.pattern, "/Realms-Game-u7/", "anchored: that folder, not any of the name");
        assert!(wt.ticked && !wt.fixed);
        assert!(p.ignores.iter().any(|i| i.reason == "Secrets" && i.fixed));
        assert_eq!(p.teams, vec!["Realms", "SR"]);
        assert!(!d.path().join(".kenignore").exists(), "propose writes nothing");
        assert!(!d.path().join(".ken-workspace").exists());
    }

    #[test]
    fn confirm_writes_the_manifest_registry_and_ignores_and_rescan_reports_what_moved() {
        let d = code_folder();
        let base = tempfile::tempdir().unwrap();
        let mut p = propose(d.path()).unwrap();
        // A person takes Personal out and reads the game's docs for entities.
        p.rows.iter_mut().find(|r| r.member == "Personal").unwrap().include = false;
        p.rows.iter_mut().find(|r| r.member == "Realms-Game").unwrap().index = IndexState::Entities;
        let ws = confirm(base.path(), d.path(), "Code", &p.rows, &p.ignores, "2026-09-24").unwrap();

        let mut members = ws.config.members.clone();
        members.sort();
        assert_eq!(members, vec!["Realms-Docs", "Realms-Game", "SR/tools"]);
        assert_eq!(ws.config.group("Realms").map(|g| g.members.len()), Some(2));
        assert!(ws.config.group("SR").is_none(), "SR is already a group folder");

        let reg = Registry::load(base.path()).unwrap();
        let game = reg.entry_at(&d.path().join("Realms-Game")).unwrap();
        assert_eq!(game.kind, vec![RepoKind::Code]);
        assert_eq!(game.team.as_deref(), Some("Realms"));
        assert_eq!(game.index, Some(IndexState::Entities), "a choice other than the kind's is kept");
        let docs = reg.entry_at(&d.path().join("Realms-Docs")).unwrap();
        assert_eq!(docs.index, None, "the kind's own state is not stored twice");

        let ignore = fs::read_to_string(d.path().join(".kenignore")).unwrap();
        assert!(ignore.contains("# Ken proposed these at set-up on 2026-09-24"));
        assert!(ignore.contains("/Realms-Game-u7/"));
        assert!(!ignore.contains("credentials.json"), "built-ins are not written");
        assert_eq!(declined(d.path()).members, vec!["Personal"]);

        // Scan again: nothing new, since Personal was declined and the
        // worktree is now ignored. A new folder is reported, not added.
        assert_eq!(rescan(d.path()).unwrap(), Vec::<Moved>::new());
        fs::create_dir_all(d.path().join("NewGame/.git")).unwrap();
        fs::write(d.path().join("NewGame/go.mod"), "module x\n").unwrap();
        let moved = rescan(d.path()).unwrap();
        assert!(matches!(&moved[..], [Moved::NewFolder { member, .. }] if member == "NewGame"), "{moved:?}");
        assert!(!Workspace::open(d.path()).unwrap().config.members.contains(&"NewGame".to_string()));
    }

    /// Repos picked one by one from anywhere: a group folder stands for the
    /// repos in it, a worktree comes back off, names stay unique, the docs
    /// README still proposes the team, and Confirm writes the workspace in
    /// app data with each member's own folder and nothing in a parent.
    #[test]
    fn repos_picked_one_by_one_make_a_workspace_in_app_data() {
        let d = code_folder();
        let p = d.path();
        let elsewhere = tempfile::tempdir().unwrap();
        fs::create_dir_all(elsewhere.path().join("Personal")).unwrap();
        fs::write(elsewhere.path().join("Personal/notes.md"), "notes\n").unwrap();
        let picked = vec![
            p.join("Realms-Docs"),
            p.join("Realms-Game"),
            p.join("Realms-Game-u7"),
            p.join("SR"), // a group folder: stands for SR/tools
            elsewhere.path().join("Personal"),
        ];
        let prop = propose_repos(&picked, &["tools".into()]).unwrap();
        let row = |m: &str| {
            prop.rows.iter().find(|r| r.member == m).unwrap_or_else(|| panic!("{m}: {:?}", prop.rows))
        };
        assert_eq!(row("Realms-Game").team.as_deref(), Some("Realms"), "named by the docs README");
        assert_eq!(row("Realms-Docs").description, "Docs for Realms-Game and its tools.");
        let wt = row("Realms-Game-u7");
        assert!(!wt.include && wt.index == IndexState::Off, "a worktree is off");
        assert!(prop.rows.iter().any(|r| r.member == "tools-2"), "SR expanded, name kept unique");

        let base = tempfile::tempdir().unwrap();
        let root = workspaces_dir(base.path()).join("code");
        let mut rows = prop.rows.clone();
        rows.iter_mut().find(|r| r.member == "Realms-Game").unwrap().description = "The game: Rust, saves by region.".into();
        let ws = confirm_repos(base.path(), &root, "Code", &rows).unwrap();
        assert!(root.join(".ken-workspace/workspace.json").exists());
        assert!(!p.join(".ken-workspace").exists() && !p.join(".kenignore").exists(), "nothing in a parent");
        // A code repo gains no file from Ken; the wiki keeps its own config.
        assert_eq!(row("Realms-Game").kind, vec![RepoKind::Code]);
        assert!(!p.join("Realms-Game/.ken").exists(), "a code repo gets no .ken/");
        let game = crate::project::Project::open(&p.join("Realms-Game")).unwrap();
        assert!(game.is_outside());
        assert!(p.join("Realms-Docs/.ken/project.json").exists());
        let canon = |x: std::path::PathBuf| plain_canonical(&x);
        assert!(!canon(p.to_path_buf()).to_string_lossy().starts_with(r"\\?\"), "no verbatim prefix");
        assert_eq!(ws.member_root("Personal"), canon(elsewhere.path().join("Personal")));
        assert!(ws.members.iter().all(|m| matches!(m.status, workspace::MemberStatus::Ok(_))));
        assert!(!ws.config.members.contains(&"Realms-Game-u7".to_string()));
        assert_eq!(ws.config.group("Realms").map(|g| g.members.len()), Some(2));
        let reg = Registry::load(base.path()).unwrap();
        let game = reg.entry_at(&p.join("Realms-Game")).unwrap();
        assert_eq!(game.description.as_deref(), Some("The game: Rust, saves by region."));

        // Reopened from app data, members still resolve to their folders.
        let again = Workspace::open(&root).unwrap();
        assert_eq!(again.member_root("Realms-Docs"), canon(p.join("Realms-Docs")));
    }

    #[test]
    fn a_monorepo_with_packages_below_the_root_is_code() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join("services/api")).unwrap();
        fs::write(root.join("services/api/pyproject.toml"), "[project]\n").unwrap();
        fs::create_dir_all(root.join("apps/web")).unwrap();
        fs::write(root.join("apps/web/package.json"), "{}").unwrap();
        // A dependency folder's markers prove nothing.
        fs::create_dir_all(root.join("docs/node_modules/x")).unwrap();
        fs::write(root.join("docs/node_modules/x/package.json"), "{}").unwrap();
        let (kind, evidence) = detect(root);
        assert_eq!(kind, vec![RepoKind::Code]);
        assert_eq!(evidence, vec!["apps/web/package.json, services/api/pyproject.toml"]);

        let docs = tempfile::tempdir().unwrap();
        fs::create_dir_all(docs.path().join("notes/node_modules/y")).unwrap();
        fs::write(docs.path().join("notes/node_modules/y/package.json"), "{}").unwrap();
        assert!(detect(docs.path()).0.is_empty(), "only a dependency folder has a marker");
    }

    #[test]
    fn remote_owners_and_docs_team_names() {
        assert_eq!(remote_owner("git@github.com:Stingbro/ken.git").as_deref(), Some("Stingbro"));
        assert_eq!(remote_owner("https://github.com/smo-key/ken.git").as_deref(), Some("smo-key"));
        assert_eq!(team_from_docs("Shattered-Realms-Docs"), "Shattered-Realms");
        assert_eq!(team_from_docs("sr-docs"), "sr");
        assert!(names_word("docs for realms-game and", "Realms-Game"));
        assert!(!names_word("gamers", "game"), "short and partial words do not count");
    }
    #[test]
    fn a_new_wiki_joins_set_up_as_the_teams_wiki() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("Realms-Wiki");
        let covered = vec![crate::wikinew::Covered { name: "Game".into(), description: "The client.".into(), ..Default::default() }];
        let row = create_wiki(&dir, "Realms", &covered, &["Game".into()], "2026-09-25").unwrap();
        assert_eq!(row.member, "Realms-Wiki");
        assert_eq!(row.kind, vec![RepoKind::Wiki]);
        assert_eq!(row.team.as_deref(), Some("Realms"));
        assert_eq!(row.index, IndexState::Entities);
        assert!(row.include && row.has_git);
        assert!(fs::read_to_string(dir.join("START-HERE.md")).unwrap().contains("| `Game` | The client. |"));
    }
}

//! A new team repo, laid down at set-up from the method's own template
//! (Ways-of-Working `templates/team`, bundled into Ken like the wiki's): the
//! tickets, the decisions log, the Ideas list, people, the method and the
//! team's overlay.
//!
//! Only what Ken knows is filled in: the team's name, its ticket key, the
//! names of the team repo and the wiki, and the repos set-up registered with
//! their kinds. Every other `{{…}}` is left for a person. The folder becomes a git repo with one commit; publishing it is a
//! person's step, as for the wiki.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Error, Result};

/// Which copy of the method's template is bundled.
pub const TEMPLATE_SOURCE: &str = "Stingbro/Ways-of-Working templates/team @ 15afe07";

macro_rules! template {
    ($($path:literal),* $(,)?) => {
        &[$(($path, include_str!(concat!("../templates/team/", $path)))),*]
    };
}

/// Every file of the template: (path in the repo, text).
pub const TEMPLATE: &[(&str, &str)] = template![
    ".wright/team.json",
    "README.md",
    "decisions/Cited-in-Code.md",
    "decisions/DECISIONS.md",
    "escalations/ESCALATION.md",
    "escalations/README.md",
    "ideas/IDEA.md",
    "ideas/README.md",
    "method/README.md",
    "people/PERSON.md",
    "people/README.md",
    "runs/README.md",
    "team/README.md",
    "tickets/TICKET.md",
];

/// A ticket key from the team's name: the initials of its words when it has
/// two or more (`Shattered-Realms` → SR), else its first six letters and
/// digits; in upper case, TEAM when it has none. The team can change it in
/// team.json. Six letters made SHATTE on 2026-10-06, a second ticket series
/// beside the team's real `SR-1174`.
pub fn ticket_key(team: &str) -> String {
    let words: Vec<&str> = team.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let k: String = if words.len() >= 2 {
        words.iter().filter_map(|w| w.chars().next()).collect()
    } else {
        team.chars().filter(|c| c.is_ascii_alphanumeric()).take(6).collect()
    };
    let k = k.to_uppercase();
    if k.is_empty() {
        "TEAM".into()
    } else {
        k
    }
}

/// The span of the JSON object or array that opens at byte `open`, to just
/// past its closing bracket; strings are skipped whole, so a `{{…}}` inside
/// a key never counts.
fn json_span(text: &str, open: usize) -> Option<usize> {
    let (mut depth, mut in_str, mut esc) = (0usize, false, false);
    for (i, c) in text.bytes().enumerate().skip(open) {
        if in_str {
            match c {
                _ if esc => esc = false,
                b'\\' => esc = true,
                b'"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'{' | b'[' => depth += 1,
            b'}' | b']' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// The entries of the JSON object at `text[open..end]`, as (key, the value's
/// text as written).
fn json_entries(text: &str, open: usize, end: usize) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut i = open + 1;
    let b = text.as_bytes();
    while i < end {
        // The key: the next string.
        let Some(k0) = text[i..end].find('"').map(|j| i + j) else { break };
        let Some(k1) = text[k0 + 1..end].find('"').map(|j| k0 + 1 + j) else { break };
        let key = text[k0 + 1..k1].to_string();
        let Some(colon) = text[k1..end].find(':').map(|j| k1 + j) else { break };
        let mut v = colon + 1;
        while v < end && b[v].is_ascii_whitespace() {
            v += 1;
        }
        let v_end = match b.get(v) {
            Some(b'{') | Some(b'[') => json_span(text, v).unwrap_or(end),
            Some(b'"') => text[v + 1..end].find('"').map_or(end, |j| v + 2 + j),
            _ => text[v..end].find([',', '}']).map_or(end, |j| v + j),
        };
        out.push((key, text[v..v_end].to_string()));
        i = v_end;
    }
    out
}

/// What git says about a repo Ken registered: its branch (None when it is on
/// no branch), its HEAD, its `origin` URL.
fn repo_facts(dir: &Path) -> (Option<String>, Option<String>, Option<String>) {
    let git = |args: &[&str]| {
        let mut cmd = std::process::Command::new("git");
        let out = crate::proc::quiet(&mut cmd).args(args).current_dir(dir).output().ok()?;
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (out.status.success() && !s.is_empty()).then_some(s)
    };
    let branch = git(&["rev-parse", "--abbrev-ref", "HEAD"]).filter(|b| b != "HEAD");
    (branch, git(&["rev-parse", "HEAD"]), crate::setup::origin_url(dir))
}

/// One repo's entry for team.json's `repos`, indented as the template is.
fn repo_entry(name: &str, fields: &[(&str, serde_json::Value)]) -> String {
    let q = |s: &str| serde_json::to_string(s).unwrap_or_default();
    let body: Vec<String> = fields.iter().map(|(k, v)| format!("      {}: {}", q(k), v)).collect();
    format!("    {}: {{\n{}\n    }}", q(name), body.join(",\n"))
}

/// team.json's `repos` with what Ken knows, one entry per repo: the team repo,
/// as `["team", "wiki"]` when it is the wiki too (on 2026-10-06 the docs repo
/// was written twice, as team then as wiki, and a JSON object keeps one);
/// the wiki; and each repo set-up registered, under its own name, with its
/// kind, the branch it is on (`main` when Ken cannot tell), and for a
/// reference repo its commit and `origin`. The template's sample code and
/// reference entries stay, placeholders and all, only while Ken knows no
/// repo of that kind.
fn fill_repos(text: &str, team_repo: &str, wiki: Option<&str>, repos: &[crate::wikinew::Covered]) -> String {
    use crate::registry::RepoKind;
    use serde_json::{json, Value};
    let Some(key_at) = text.find("\"repos\"") else { return text.to_string() };
    let Some(open) = text[key_at..].find('{').map(|i| key_at + i) else { return text.to_string() };
    let Some(end) = json_span(text, open) else { return text.to_string() };
    let sample = json_entries(text, open, end);
    let sample_of = |key: &str| sample.iter().find(|(k, _)| k == key).map(|(k, v)| format!("    \"{k}\": {v}"));
    let mut entries: Vec<String> = Vec::new();
    let both = wiki == Some(team_repo);
    let team_kind = if both { json!(["team", "wiki"]) } else { json!("team") };
    entries.push(repo_entry(team_repo, &[("kind", team_kind), ("base", json!("main"))]));
    match wiki {
        Some(w) if !both => entries.push(repo_entry(w, &[("kind", json!("wiki")), ("base", json!("main"))])),
        Some(_) => {}
        None => entries.extend(sample_of("{{wiki_repo}}")),
    }
    let kind_name = |k: &RepoKind| serde_json::to_value(k).unwrap_or(Value::Null);
    for r in repos.iter().filter(|r| r.name != team_repo && Some(r.name.as_str()) != wiki) {
        let kinds: Vec<Value> = r.kind.iter().map(kind_name).collect();
        let kind = match kinds.as_slice() {
            [] => json!("{{code or reference}}"),
            [one] => one.clone(),
            _ => Value::Array(kinds),
        };
        let (branch, head, origin) = r.path.as_deref().map(repo_facts).unwrap_or_default();
        let mut fields = vec![("kind", kind), ("base", json!(branch.unwrap_or_else(|| "main".into())))];
        if r.kind.contains(&RepoKind::Reference) {
            fields.extend(head.map(|h| ("pin", json!(h))));
            fields.extend(origin.map(|o| ("upstream", json!(o))));
        }
        entries.push(repo_entry(&r.name, &fields));
    }
    for (sample_key, kind) in [("{{code_repo}}", RepoKind::Code), ("{{reference_repo}}", RepoKind::Reference)] {
        if !repos.iter().any(|r| r.kind.contains(&kind)) {
            entries.extend(sample_of(sample_key));
        }
    }
    format!("{}{{\n{}\n  }}{}", &text[..open], entries.join(",\n"), &text[end..])
}

/// One template file with what Ken knows filled in. Copy templates (a
/// ticket, a person, an idea, an escalation) keep their placeholders, the
/// key aside.
/// `repos` are the team's other repos as set-up registered them.
pub fn fill(
    path: &str,
    text: &str,
    team: &str,
    team_repo: &str,
    wiki: Option<&str>,
    repos: &[crate::wikinew::Covered],
) -> String {
    let key = ticket_key(team);
    let text = if path == ".wright/team.json" { fill_repos(text, team_repo, wiki, repos) } else { text.to_string() };
    let mut t = text.replace("{{KEY}}", &key);
    let is_copy = ["TICKET.md", "PERSON.md", "IDEA.md", "ESCALATION.md"].iter().any(|c| path.ends_with(c));
    if !is_copy {
        t = t.replace("{{team_name}}", team).replace("{{team_repo}}", team_repo);
        if let Some(w) = wiki {
            t = t.replace("{{wiki_repo}}", w);
        }
    }
    t
}

/// Lay a new team repo down at `dir` (missing or empty) for `team`, then
/// make it a git repo with one commit. Refuses a folder that already holds
/// anything, so it never writes over a person's files. `repos` are the
/// team's other repos, for team.json.
pub fn create(dir: &Path, team: &str, wiki: Option<&str>, repos: &[crate::wikinew::Covered]) -> Result<()> {
    if dir.exists() && fs::read_dir(dir).map_err(|e| Error::io(dir, e))?.next().is_some() {
        return Err(Error::Other(format!("{} is not empty; pick a new folder for the team repo", dir.display())));
    }
    let name = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "team".into());
    for (path, text) in TEMPLATE {
        let dest = dir.join(path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        let text = text.replace("\r\n", "\n");
        fs::write(&dest, fill(path, &text, team, &name, wiki, repos)).map_err(|e| Error::io(&dest, e))?;
    }
    crate::wikinew::git_commit_all(dir, &format!("Start the {team} team repo from the Ways-of-Working template"))
}

// --- The rulings the code cites (`decisions/Cited-in-Code.md`), generated
// from the code repos' tracked files and checked against the log. ---------

/// The generated page, in the team repo.
pub const CITED_IN_CODE: &str = "decisions/Cited-in-Code.md";

/// What rebuilds the page, as the page says.
const CITED_BY: &str = "Ken's wiki draft";

/// The ruling ids `line` cites: `D-nnn` (three digits or more) standing
/// alone, or a link to one, `DECISIONS.md#d-nnn`.
fn ruling_ids(line: &str) -> Vec<String> {
    let b = line.as_bytes();
    let mut out = Vec::new();
    for i in 0..b.len().saturating_sub(2) {
        let id_start = match b[i] {
            b'D' => i == 0 || !b[i - 1].is_ascii_alphanumeric(),
            b'd' => i > 0 && b[i - 1] == b'#',
            _ => false,
        };
        if !id_start || b[i + 1] != b'-' {
            continue;
        }
        let digits: String = line[i + 2..].chars().take_while(|c| c.is_ascii_digit()).collect();
        let after = b.get(i + 2 + digits.len());
        if digits.len() >= 3 && !after.is_some_and(|c| c.is_ascii_alphanumeric()) {
            out.push(format!("D-{digits}"));
        }
    }
    out.dedup();
    out
}

/// Each ruling id `root`'s tracked files cite: (id, `repo:path:line`, the
/// line as written).
fn citations(name: &str, root: &Path) -> Vec<(String, String, String)> {
    let mut cmd = Command::new("git");
    let out = crate::proc::quiet(&mut cmd)
        .args(["-c", "core.quotepath=off", "grep", "-n", "-I", "-i", "-E", "d-[0-9]{3}"])
        .current_dir(root)
        .output();
    let Ok(out) = out else { return Vec::new() };
    let mut found = Vec::new();
    for hit in String::from_utf8_lossy(&out.stdout).lines() {
        let mut parts = hit.splitn(3, ':');
        let (Some(path), Some(line), Some(text)) = (parts.next(), parts.next(), parts.next()) else { continue };
        for id in ruling_ids(text) {
            found.push((id, format!("{name}:{path}:{line}"), text.trim().to_string()));
        }
    }
    found
}

/// The decisions log's entries: id and the rest of its first line.
fn log_entries(log: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut in_fence = false;
    for line in log.lines().map(str::trim) {
        if line.starts_with("```") {
            in_fence = !in_fence; // the format example, not an entry
            continue;
        }
        if in_fence {
            continue;
        }
        if let Some(id) = ruling_ids(line).into_iter().next().filter(|id| line.starts_with(id.as_str())) {
            let rest = line[id.len()..].trim_start_matches([' ', '-', '·', '—']).trim();
            out.push((id, rest.to_string()));
        }
    }
    out
}

/// A table cell: one line, no pipes, at most 200 characters.
fn cell(s: &str) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ").replace('|', "\\|");
    if s.chars().count() > 200 {
        format!("{}…", s.chars().take(199).collect::<String>())
    } else {
        s
    }
}

/// `decisions/Cited-in-Code.md` for `code`, the team's code repos, against
/// `log` (the text of `DECISIONS.md`): every ruling id their tracked files
/// cite, split by whether the log has it. None with no code repo to read.
pub fn cited_in_code(code: &[(String, PathBuf)], log: &str, today: &str) -> Option<String> {
    if code.is_empty() {
        return None;
    }
    let template = TEMPLATE.iter().find(|(p, _)| *p == CITED_IN_CODE)?.1.replace("\r\n", "\n");
    let entries = log_entries(log);
    let mut cited: Vec<(String, String, String)> = code.iter().flat_map(|(n, r)| citations(n, r)).collect();
    cited.sort();
    let (mut missing, mut logged) = (String::new(), String::new());
    for (id, at, text) in &cited {
        match entries.iter().find(|(e, _)| e == id) {
            Some((_, ruling)) => logged.push_str(&format!("| {id} | `{at}` | {} |\n", cell(ruling))),
            None => missing.push_str(&format!("| {id} | `{at}` | {} |\n", cell(text))),
        }
    }
    let names: Vec<&str> = code.iter().map(|(n, _)| n.as_str()).collect();
    let heads: Vec<String> = code
        .iter()
        .map(|(n, r)| {
            let head = repo_facts(r).1.unwrap_or_default();
            format!("{n}@{}", &head[..head.len().min(12)])
        })
        .collect();
    let searched: Vec<String> = names.iter().map(|n| format!("{n} (every tracked file)")).collect();
    let mut page = String::new();
    for line in template.split_inclusive('\n') {
        if line.starts_with("| {{D-nnn}} |") {
            page.push_str(if line.contains("{{the comment line") { &missing } else { &logged });
        } else {
            page.push_str(line);
        }
    }
    Some(
        page.replace("{{code repos, each with the path searched}}", &searched.join(" · "))
            .replace("{{repo@sha for each code repo, from one run}}", &heads.join(" · "))
            .replace("{{the command that rebuilds this page}}", CITED_BY)
            .replace("{{repos}}", &names.join(", "))
            .replace("{{repo@sha}}", &heads.join(" · "))
            .replace("{{command}}", CITED_BY)
            .replace("{{date}}", today),
    )
}

/// Rebuild `team`'s `decisions/Cited-in-Code.md` from `code` (see
/// [`cited_in_code`]). A page that no longer says `generated: true` is a
/// person's, and is left alone. Whether it was written.
pub fn write_cited_in_code(team: &Path, code: &[(String, PathBuf)], today: &str) -> Result<bool> {
    let path = team.join(CITED_IN_CODE);
    let existing = fs::read_to_string(&path).ok();
    if existing.as_deref().is_some_and(|t| !t.lines().any(|l| l.trim() == "generated: true")) {
        return Ok(false);
    }
    let log = fs::read_to_string(team.join("decisions/DECISIONS.md")).unwrap_or_default();
    let Some(page) = cited_in_code(code, &log, today) else { return Ok(false) };
    if existing.as_deref() == Some(page.as_str()) {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
    }
    fs::write(&path, page).map_err(|e| Error::io(&path, e))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::registry::RepoKind;
    use crate::wikinew::Covered;
    use std::path::PathBuf;

    #[test]
    fn the_key_comes_from_the_team_name() {
        assert_eq!(ticket_key("AUR"), "AUR");
        assert_eq!(ticket_key("Shattered-Realms"), "SR", "the initials of a name of several words");
        assert_eq!(ticket_key("Realms team"), "RT");
        assert_eq!(ticket_key("Payments"), "PAYMEN", "one word: its first six letters");
        assert_eq!(ticket_key("—"), "TEAM");
    }

    fn team_json(team: &str, team_repo: &str, wiki: Option<&str>, repos: &[Covered]) -> serde_json::Value {
        let json = TEMPLATE.iter().find(|(p, _)| *p == ".wright/team.json").unwrap().1.replace("\r\n", "\n");
        let t = fill(".wright/team.json", &json, team, team_repo, wiki, repos);
        serde_json::from_str(&t).unwrap_or_else(|e| panic!("{e}\n{t}"))
    }

    #[test]
    fn fill_names_the_team_and_repos_and_keeps_copy_templates() {
        let readme = TEMPLATE.iter().find(|(p, _)| *p == "README.md").unwrap().1;
        assert!(fill("README.md", readme, "AUR", "AUR-Team", Some("AUR-Wiki"), &[]).starts_with("# AUR\n"));
        let json = TEMPLATE.iter().find(|(p, _)| *p == ".wright/team.json").unwrap().1;
        let t = fill(".wright/team.json", json, "AUR", "AUR-Team", Some("AUR-Wiki"), &[]);
        assert!(t.contains("\"team\": \"AUR\"") && t.contains("\"key\": \"AUR\"") && t.contains("\"AUR-Team\": {") && t.contains("\"AUR-Wiki\": {"));
        let ticket = TEMPLATE.iter().find(|(p, _)| *p == "tickets/TICKET.md").unwrap().1;
        let t = fill("tickets/TICKET.md", ticket, "AUR", "AUR-Team", None, &[]);
        assert!(t.contains("id: AUR-001") && t.contains("{{type}}"), "a copy template keeps its placeholders");
    }

    /// The 2026-10-06 dry run: the docs repo is the team repo too, three
    /// repos registered (two code, one reference).
    #[test]
    fn team_json_names_each_registered_repo_once_with_its_kind() {
        let d = tempfile::tempdir().unwrap();
        let mod_dir = d.path().join("Shattered-Realms");
        fs::create_dir_all(&mod_dir).unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .current_dir(&mod_dir)
                .output()
                .unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        };
        git(&["init", "-q", "-b", "dev"]);
        git(&["commit", "-q", "--allow-empty", "-m", "first"]);
        let covered = |name: &str, kind: RepoKind, path: Option<PathBuf>| Covered {
            name: name.into(),
            description: String::new(),
            kind: vec![kind],
            path,
        };
        let repos = vec![
            covered("Shattered-Realms", RepoKind::Code, Some(mod_dir.clone())),
            covered("Shattered-Realms-Tools", RepoKind::Code, None),
            covered("hytale-shared-source", RepoKind::Reference, None),
        ];
        let wiki = "Shattered-Realms-Wiki";
        let v = team_json("Shattered-Realms", wiki, Some(wiki), &repos);
        assert_eq!(v["key"], "SR");
        let r = v["repos"].as_object().unwrap();
        let mut names: Vec<&String> = r.keys().collect();
        names.sort();
        assert_eq!(names, vec!["Shattered-Realms", "Shattered-Realms-Tools", "Shattered-Realms-Wiki", "hytale-shared-source"]);
        assert_eq!(r[wiki]["kind"], serde_json::json!(["team", "wiki"]), "one entry of both kinds, not two");
        assert_eq!(r["Shattered-Realms"]["kind"], "code");
        assert_eq!(r["Shattered-Realms"]["base"], "dev", "the branch the checkout is on");
        assert_eq!(r["Shattered-Realms-Tools"]["base"], "main", "main when Ken cannot tell");
        assert_eq!(r["hytale-shared-source"]["kind"], "reference");
        let text = serde_json::to_string(&v["repos"]).unwrap();
        assert!(!text.contains("{{"), "no sample entry is left once each kind is known: {text}");

        // Nothing registered: the samples stay for a person to fill.
        let v = team_json("AUR", "AUR-Team", Some("AUR-Wiki"), &[]);
        let r = v["repos"].as_object().unwrap();
        assert!(r.contains_key("{{code_repo}}") && r.contains_key("{{reference_repo}}"));
        assert_eq!(r["AUR-Team"]["kind"], "team");
        assert_eq!(r["AUR-Wiki"]["kind"], "wiki");
    }

    #[test]
    fn the_rulings_the_code_cites_are_listed_against_the_log() {
        assert_eq!(ruling_ids("// D-012: saves are region files; see DECISIONS.md#d-007"), vec!["D-012", "D-007"]);
        assert!(ruling_ids("HD-123 · D-12 · D-0123a · grid-d-100").is_empty(), "not an id: a word, two digits, a letter after, no link");

        let d = tempfile::tempdir().unwrap();
        let game = d.path().join("Game");
        fs::create_dir_all(game.join("src")).unwrap();
        fs::write(game.join("src/save.rs"), "fn save() {}\n// D-012: worlds save as region files | always\n").unwrap();
        fs::write(game.join("src/combat.rs"), "// Stamina, not energy (D-031).\n").unwrap();
        fs::write(game.join("notes.txt"), "D-099 untracked, never read\n").unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .current_dir(&game)
                .output()
                .unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        };
        git(&["init", "-q"]);
        git(&["add", "src"]);
        git(&["commit", "-q", "-m", "first"]);
        let log = "```\nD-001 - 2026-01-01 - topic - THE RULING.\n```\n\n## Log\n\nD-012 - 2026-09-01 - saves - Worlds save as region files.\n";
        let code = vec![("Game".to_string(), game.clone())];
        let page = cited_in_code(&code, log, "2026-10-06").unwrap();
        assert!(page.contains("| D-031 | `Game:src/combat.rs:1` | // Stamina, not energy (D-031). |\n"), "{page}");
        assert!(page.contains("| D-012 | `Game:src/save.rs:2` | 2026-09-01 - saves - Worlds save as region files. |\n"), "{page}");
        assert!(!page.contains("D-099") && !page.contains("D-001 |"), "untracked files and the format example are not read");
        assert!(page.contains("updated: 2026-10-06") && page.contains("generated: true") && !page.contains("{{"), "{page}");
        let not_in_log = &page[page.find("## Not in the Log").unwrap()..page.find("## In the Log").unwrap()];
        assert!(not_in_log.contains("D-031") && !not_in_log.contains("D-012"));
        assert_eq!(cited_in_code(&[], log, "2026-10-06"), None, "no code repo, no page");

        // Written while it is generated; a page a person took over is theirs.
        let team = d.path().join("Team");
        fs::create_dir_all(team.join("decisions")).unwrap();
        fs::write(team.join("decisions/DECISIONS.md"), log).unwrap();
        assert!(write_cited_in_code(&team, &code, "2026-10-06").unwrap());
        assert!(!write_cited_in_code(&team, &code, "2026-10-06").unwrap(), "unchanged");
        fs::write(team.join(CITED_IN_CODE), "# Cited\n\nKept by hand.\n").unwrap();
        assert!(!write_cited_in_code(&team, &code, "2026-10-07").unwrap());
        assert_eq!(fs::read_to_string(team.join(CITED_IN_CODE)).unwrap(), "# Cited\n\nKept by hand.\n");
    }

    #[test]
    fn the_template_list_is_exactly_the_bundled_folder() {
        let mut listed: Vec<String> = TEMPLATE.iter().map(|(p, _)| p.to_string()).collect();
        listed.sort();
        assert_eq!(listed, crate::wikinew::tests::files_under("team"), "TEMPLATE and templates/team differ");
        for (path, text) in TEMPLATE {
            assert!(!text.contains('\r'), "{path} has a CR");
        }
    }

    #[test]
    fn create_lays_the_template_down_as_a_repo_and_refuses_a_full_folder() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("AUR-Team");
        create(&dir, "AUR", Some("AUR-Wiki"), &[]).unwrap();
        for (path, _) in TEMPLATE {
            assert!(dir.join(path).exists(), "{path}");
        }
        assert!(dir.join(".git").exists());
        assert!(create(&dir, "AUR", None, &[]).is_err(), "never writes over a folder with files");
    }
}

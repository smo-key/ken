//! The repo analysis (knowledge-core decisions 17 and 19): when a source is
//! added, one Claude call over its file list proposes its role rules (for a
//! code or reference repo) and a few controls (for every source). What it
//! proposes waits for a person (Chris, 2026-10-14): one Inbox card in the
//! knowledge base's index ([`KIND`], [`file`]) shows each rule and control.
//! Accept ([`accept`]) writes them into the knowledge base's
//! `.ken/knowledge.json` as one commit naming the file; Dismiss drops them.
//! Nothing applies until accepted. Once in the file, a person edits them in
//! Settings › Workspace, and they are applied without a model at index time
//! (`coderole`, `controls`).
//!
//! Claude reads the prompt and answers with no tools; the local model is
//! never asked. When Claude is not there, or the call fails, nothing waits
//! and the built-in core roles apply; the next time a source is added the
//! analysis is tried again. A source whose analysis is in the file, or was
//! proposed once (waiting, accepted or dismissed), is not analysed again by
//! itself, so a person's edits and a person's no both stay. A person can ask
//! for it in Settings › Workspace ("Analyse again", Chris, 2026-10-14):
//! [`forget`] clears the mark and [`analyze_again`] files a fresh card, which
//! replaces one still waiting for the source.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::controls::Control;
use crate::db::{Db, ReviewItemRow};
use crate::knowledge_settings::Settings;
use crate::registry::RepoKind;
use crate::{Error, Result};

/// The Review kind of a repo analysis waiting for a person.
pub const KIND: &str = "analysis";

/// Most files listed in the prompt; a bigger repo is sampled evenly across
/// its tree, so every folder shows.
pub const MAX_FILES: usize = 600;
/// Most entries the walk reads before it stops.
const WALK_LIMIT: usize = 20_000;
/// Most pages whose title is read for the prompt.
const MAX_TITLES: usize = 200;
/// Most role rules kept from one reply.
pub const MAX_ROLES: usize = 30;

/// What the analysis proposes for one source.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Analysis {
    pub roles: Vec<(String, String)>,
    pub controls: Vec<Control>,
}

/// Whether a source of these kinds gets role rules: code does, a knowledge
/// base's pages do not.
pub fn wants_roles(kinds: &[RepoKind]) -> bool {
    kinds.iter().any(|k| matches!(k, RepoKind::Code | RepoKind::Reference))
}

/// The files the analysis reads: the code and the Markdown pages Ken would
/// index (the repo's `.gitignore`, the built-in rules: no tests, archives,
/// generated files or templates), sorted, sampled down to [`MAX_FILES`].
pub fn file_list(root: &Path) -> Vec<String> {
    let built_in = crate::kenignore::built_in_rule_sets();
    let mut files: Vec<String> = ignore::WalkBuilder::new(root)
        .hidden(true)
        .parents(false)
        .git_global(false)
        .filter_entry(|e| e.file_name() != crate::project::CONFIG_DIR && e.file_name() != "node_modules")
        .build()
        .flatten()
        .take(WALK_LIMIT)
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .filter_map(|e| e.path().strip_prefix(root).ok().map(|p| p.to_string_lossy().replace('\\', "/")))
        .filter(|rel| crate::kenignore::classify(rel, false, &[&built_in]) != crate::kenignore::Tier::Ignored)
        .filter(|rel| crate::chunker::is_markdown(rel) || crate::contenttype::of(rel) == crate::contenttype::ContentType::Code)
        .collect();
    files.sort();
    sample(files, MAX_FILES)
}

/// At most `n` of `files`, taken evenly from the sorted list.
fn sample(files: Vec<String>, n: usize) -> Vec<String> {
    if files.len() <= n || n == 0 {
        return files;
    }
    let step = files.len() as f64 / n as f64;
    (0..n).filter_map(|i| files.get((i as f64 * step) as usize).cloned()).collect()
}

/// The prompt: the source, what to propose, and its files, each page with
/// its title when one is known.
pub fn prompt(name: &str, kinds: &[RepoKind], files: &[String], titles: &[(String, String)]) -> String {
    let kinds: Vec<&str> = kinds
        .iter()
        .map(|k| match k {
            RepoKind::Wiki => "wiki",
            RepoKind::Code => "code",
            RepoKind::Reference => "reference",
        })
        .collect();
    let mut p = format!(
        "You are setting up search for one source of a project's knowledge base: the repo \"{name}\" (kind: {}). \
         Below is a list of its files.\n\n",
        if kinds.is_empty() { "not said".to_string() } else { kinds.join(", ") }
    );
    if wants_roles_names(&kinds) {
        let core: Vec<&str> = crate::coderole::core_roles().collect();
        p.push_str(&format!(
            "1. Roles. Search labels each code file with its role in the repo. It already reads these roles from folder and \
             file names: {}. Propose rules for the roles this repo has that those names miss or get wrong, each a \
             gitignore-style pattern relative to the repo root and a role of one or two lowercase words \
             (for example \"src/net/**\": \"protocol\"). At most {MAX_ROLES} rules, and none when the built-in roles fit.\n",
            core.join(", ")
        ));
    } else {
        p.push_str("1. Roles. This source is not code: propose no roles.\n");
    }
    p.push_str(&format!(
        "2. Controls. Propose up to {} questions a person on the team would really ask that this repo answers, each \
         with the one file from the list that must come back for it. Pick questions with one clear answer file, in the \
         words a person would type rather than the file's name.\n\n\
         Reply with JSON only, in this shape:\n\
         {{\"roles\": {{\"pattern\": \"role\"}}, \"controls\": [{{\"question\": \"...\", \"expect\": \"path/from/the/list\"}}]}}\n\n\
         Files:\n",
        crate::controls::MAX_PROPOSED
    ));
    for f in files {
        match titles.iter().find(|(p, _)| p == f) {
            Some((_, t)) => p.push_str(&format!("{f} | {t}\n")),
            None => p.push_str(&format!("{f}\n")),
        }
    }
    p
}

fn wants_roles_names(kinds: &[&str]) -> bool {
    kinds.iter().any(|k| matches!(*k, "code" | "reference"))
}

/// What a reply proposes: the JSON object in it, read leniently. A role
/// rule needs a pattern and a short role; a control needs a question and a
/// file that is in `files`. Anything else is left out.
pub fn parse(reply: &str, files: &[String], roles_wanted: bool) -> Analysis {
    let (Some(start), Some(end)) = (reply.find('{'), reply.rfind('}')) else {
        return Analysis::default();
    };
    let Ok(v) = serde_json::from_str::<Value>(reply.get(start..=end).unwrap_or("")) else {
        return Analysis::default();
    };
    let mut out = Analysis::default();
    if roles_wanted {
        if let Some(roles) = v.get("roles").and_then(|r| r.as_object()) {
            for (pattern, role) in roles {
                let pattern = pattern.trim();
                let role = role.as_str().map(crate::coderole::normalize_role).unwrap_or_default();
                if pattern.is_empty() || pattern.len() > 200 || role.is_empty() || role.len() > 40 {
                    continue;
                }
                out.roles.push((pattern.to_string(), role));
                if out.roles.len() == MAX_ROLES {
                    break;
                }
            }
        }
    }
    if let Some(list) = v.get("controls").and_then(|c| c.as_array()) {
        for c in list {
            let question = c.get("question").and_then(|q| q.as_str()).map(str::trim).unwrap_or("");
            let expect = c.get("expect").and_then(|e| e.as_str()).map(|e| e.trim().trim_start_matches("./")).unwrap_or("");
            let Some(file) = files.iter().find(|f| f.eq_ignore_ascii_case(expect)) else { continue };
            if question.is_empty() || question.len() > 300 {
                continue;
            }
            out.controls.push(Control { question: question.to_string(), expect: file.clone(), source: None });
            if out.controls.len() == crate::controls::MAX_PROPOSED {
                break;
            }
        }
    }
    out
}

/// The titles of the first [`MAX_TITLES`] pages in `files`, read from the
/// head of each.
fn page_titles(root: &Path, files: &[String]) -> Vec<(String, String)> {
    files
        .iter()
        .filter(|f| crate::chunker::is_markdown(f))
        .take(MAX_TITLES)
        .filter_map(|f| {
            let text = std::fs::read_to_string(root.join(f)).ok()?;
            let head: String = text.chars().take(4000).collect();
            crate::chunker::page_title(&head).map(|t| (f.clone(), t))
        })
        .collect()
}

/// Analyse the source at `root` with `generate` (one Claude call).
pub fn analyze(root: &Path, name: &str, kinds: &[RepoKind], generate: &mut dyn FnMut(&str) -> Result<String>) -> Result<Analysis> {
    let files = file_list(root);
    if files.is_empty() {
        return Ok(Analysis::default());
    }
    let titles = page_titles(root, &files);
    let reply = generate(&prompt(name, kinds, &files, &titles))?;
    Ok(parse(&reply, &files, wants_roles(kinds)))
}

/// One role rule as the Inbox card shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleRule {
    pub pattern: String,
    pub role: String,
}

/// What waits in the Inbox for one source: the card's payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Proposal {
    /// The source's name, as the settings file's `repos` and `controls`
    /// name it.
    pub source: String,
    /// Whether the source is the knowledge base itself.
    #[serde(default)]
    pub own: bool,
    #[serde(default)]
    pub roles: Vec<RoleRule>,
    #[serde(default)]
    pub controls: Vec<Control>,
}

impl Proposal {
    fn role_pairs(&self) -> Vec<(String, String)> {
        self.roles.iter().map(|r| (r.pattern.clone(), r.role.clone())).collect()
    }
}

/// The settings of the source at `root` when it is due an analysis: it has
/// a knowledge base to keep the settings in, and no rules or controls of its
/// own there yet. None for a code repo that keeps its own settings file:
/// Ken writes nothing in a code repo (decision 36).
pub fn due(root: &Path) -> Option<Settings> {
    due_of(crate::knowledge_settings::for_repo(root))
}

/// [`due`] with the settings already found.
pub fn due_of(settings: Option<Settings>) -> Option<Settings> {
    settings.filter(|s| !s.analyzed() && writable(s))
}

/// Whether what the analysis proposes for a source with these settings could
/// ever be written: not when they are a code repo's own settings file, since
/// Ken writes nothing in a code repo (decision 36). "Analyse again" is
/// offered only where this holds.
pub fn writable(settings: &Settings) -> bool {
    !settings.own || crate::registry::is_library(&settings.kb_root)
}

/// The proposal a Review item holds, when it is an analysis.
pub fn proposal_of(it: &ReviewItemRow) -> Option<Proposal> {
    (it.kind == KIND).then(|| it.payload.as_deref().and_then(|p| serde_json::from_str(p).ok())).flatten()
}

/// Whether the source `name` was proposed before in this knowledge base's
/// index: waiting, accepted or dismissed.
pub fn proposed_before(db: &Db, name: &str) -> Result<bool> {
    Ok(db.review_items_of_kind(KIND, 500)?.iter().filter_map(proposal_of).any(|p| p.source.eq_ignore_ascii_case(name)))
}

/// The analyses waiting for a person, newest first.
pub fn open(db: &Db) -> Result<Vec<(ReviewItemRow, Proposal)>> {
    Ok(db.list_open_review_items()?.into_iter().filter_map(|it| proposal_of(&it).map(|p| (it, p))).collect())
}

/// The ids of the analyses waiting for the source `name`.
pub fn waiting_for(db: &Db, name: &str) -> Result<Vec<i64>> {
    Ok(open(db)?.into_iter().filter(|(_, p)| p.source.eq_ignore_ascii_case(name)).map(|(it, _)| it.id).collect())
}

/// Clear the mark that keeps the source `name` from being analysed again,
/// when a person asks for a fresh analysis (Chris, 2026-10-14): its cards
/// that are done, accepted or dismissed, leave the knowledge base's index
/// `db`. A card still waiting stays until the fresh one replaces it
/// ([`analyze_again`]). Returns how many went.
pub fn forget(db: &mut Db, name: &str) -> Result<usize> {
    let done: Vec<i64> = db
        .review_items_of_kind(KIND, 500)?
        .iter()
        .filter(|it| it.status != "open")
        .filter(|it| proposal_of(it).is_some_and(|p| p.source.eq_ignore_ascii_case(name)))
        .map(|it| it.id)
        .collect();
    for id in &done {
        db.delete_review_item(*id)?;
    }
    Ok(done.len())
}

/// The card's text: each rule and each control, one a line.
fn card_body(p: &Proposal) -> String {
    let mut s = String::new();
    if !p.roles.is_empty() {
        s.push_str("Role rules (pattern = role):\n");
        s.extend(p.roles.iter().map(|r| format!("- {} = {}\n", r.pattern, r.role)));
    }
    if !p.controls.is_empty() {
        if !s.is_empty() {
            s.push('\n');
        }
        s.push_str("Controls (the question, then the file that must come back near the top):\n");
        s.extend(p.controls.iter().map(|c| format!("- {} | {}\n", c.question, c.expect)));
    }
    s.push_str(&format!(
        "\nAccept writes them into {} as one commit; Dismiss drops them. Nothing applies until you accept.",
        crate::wikinew::KNOWLEDGE_FILE
    ));
    s
}

/// File what the analysis proposed for the source as one Inbox card in the
/// knowledge base's index `db`. Returns its id; None when it proposed
/// nothing.
pub fn file(db: &mut Db, settings: &Settings, analysis: &Analysis, now: i64) -> Result<Option<i64>> {
    if analysis.roles.is_empty() && analysis.controls.is_empty() {
        return Ok(None);
    }
    let p = Proposal {
        source: settings.name.clone(),
        own: settings.own,
        roles: analysis.roles.iter().map(|(pattern, role)| RoleRule { pattern: pattern.clone(), role: role.clone() }).collect(),
        controls: analysis.controls.clone(),
    };
    let title = match (p.roles.len(), p.controls.len()) {
        (0, c) => format!("{c} controls proposed for {}", p.source),
        (r, 0) => format!("{r} role rules proposed for {}", p.source),
        (r, c) => format!("{r} role rules and {c} controls proposed for {}", p.source),
    };
    let payload = serde_json::to_string(&p).map_err(|e| Error::Other(e.to_string()))?;
    Ok(Some(db.insert_review_item(KIND, &title, &card_body(&p), crate::wikinew::KNOWLEDGE_FILE, Some(&payload), now)?))
}

/// Analyse the source at `root` and file what it proposes for a person
/// ([`file`]), when it is due ([`due_of`]) and was not proposed before in
/// the knowledge base's index `kb_db`. No call is made when there is nothing
/// to do. Returns the card's id.
pub fn analyze_and_propose(
    root: &Path,
    settings: Option<Settings>,
    kinds: &[RepoKind],
    kb_db: &mut Db,
    generate: &mut dyn FnMut(&str) -> Result<String>,
    now: i64,
) -> Result<Option<i64>> {
    let Some(settings) = due_of(settings) else { return Ok(None) };
    if proposed_before(kb_db, &settings.name)? {
        return Ok(None);
    }
    let analysis = analyze(root, &settings.name, kinds, generate)?;
    file(kb_db, &settings, &analysis, now)
}

/// Analyse the source at `root` again because a person asked ("Analyse
/// again" in Settings › Workspace, Chris, 2026-10-14): one call whatever the
/// file or the index already holds for it, its proposal filed as a new card
/// ([`file`]) that replaces a card still waiting for the same source. Accept
/// is the usual one, writing only what the file has none of for the source
/// yet. Nothing is asked for settings that cannot be written ([`writable`]).
/// Returns the new card's id; None when it proposed nothing, and a waiting
/// card then stays.
pub fn analyze_again(
    root: &Path,
    settings: Option<Settings>,
    kinds: &[RepoKind],
    kb_db: &mut Db,
    generate: &mut dyn FnMut(&str) -> Result<String>,
    now: i64,
) -> Result<Option<i64>> {
    let Some(settings) = settings.filter(writable) else { return Ok(None) };
    let waiting = waiting_for(kb_db, &settings.name)?;
    let analysis = analyze(root, &settings.name, kinds, generate)?;
    let id = file(kb_db, &settings, &analysis, now)?;
    if id.is_some() {
        for old in waiting {
            kb_db.delete_review_item(old)?;
        }
    }
    Ok(id)
}

/// Accept the analysis waiting as item `id` in the knowledge base at
/// `kb_root`: its rules and controls are written into `.ken/knowledge.json`
/// (only where the file has none for the source yet) as one commit naming
/// the file (`knowledge_settings::accept_proposals`), and the card is done.
/// Returns the source's name and what was written.
pub fn accept(kb_root: &Path, db: &mut Db, id: i64, now: i64) -> Result<(String, crate::knowledge_settings::Accepted)> {
    let (it, p) = open(db)?.into_iter().find(|(it, _)| it.id == id).ok_or_else(|| Error::Other("no repo analysis waits with that id".into()))?;
    let accepted = crate::knowledge_settings::accept_proposals(kb_root, &p.source, p.own, &p.role_pairs(), &p.controls)?;
    db.resolve_review_item(it.id, now)?;
    Ok((p.source, accepted))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_keeps_what_holds() {
        let files = vec!["src/net/Packet.java".to_string(), "Design/Saving.md".to_string()];
        let reply = "Here it is:\n```json\n{\"roles\": {\"src/net/**\": \"Network Protocol\", \"\": \"x\", \"src/a/**\": \"\"},\n \
                     \"controls\": [{\"question\": \"how are packets framed\", \"expect\": \"./src/net/packet.java\"},\n \
                     {\"question\": \"made up\", \"expect\": \"src/Nowhere.java\"}, {\"question\": \"\", \"expect\": \"Design/Saving.md\"}]}\n```";
        let a = parse(reply, &files, true);
        assert_eq!(a.roles, vec![("src/net/**".to_string(), "network protocol".to_string())]);
        assert_eq!(a.controls, vec![Control { question: "how are packets framed".into(), expect: "src/net/Packet.java".into(), source: None }]);
        assert!(parse(reply, &files, false).roles.is_empty(), "a knowledge base gets no roles");
        assert_eq!(parse("no json here", &files, true), Analysis::default());
    }

    #[test]
    fn the_prompt_lists_files_with_titles_and_asks_for_roles_only_of_code() {
        let files = vec!["src/Save.java".to_string(), "Design/Saving.md".to_string()];
        let titles = vec![("Design/Saving.md".to_string(), "How worlds save".to_string())];
        let p = prompt("Game", &[RepoKind::Code], &files, &titles);
        assert!(p.contains("\"Game\" (kind: code)") && p.contains("service, ") && p.contains("Design/Saving.md | How worlds save\n"), "{p}");
        let p = prompt("Wiki", &[RepoKind::Wiki], &files, &[]);
        assert!(p.contains("propose no roles"), "{p}");
    }

    #[test]
    fn the_file_list_skips_tests_and_what_is_not_code_or_pages() {
        let d = tempfile::tempdir().unwrap();
        for f in ["src/Save.java", "src/test/SaveTest.java", "Design/Saving.md", "assets/logo.png", "build.zip"] {
            let p = d.path().join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "x").unwrap();
        }
        assert_eq!(file_list(d.path()), vec!["Design/Saving.md".to_string(), "src/Save.java".to_string()]);
        assert_eq!(sample((0..10).map(|i| i.to_string()).collect(), 5), vec!["0", "2", "4", "6", "8"]);
    }

    #[test]
    fn with_no_settings_nothing_is_asked() {
        let d = tempfile::tempdir().unwrap();
        let mut db = Db::open_in_memory().unwrap();
        let mut never = |_: &str| -> Result<String> { panic!("no knowledge base to save into") };
        assert_eq!(analyze_and_propose(d.path(), None, &[RepoKind::Code], &mut db, &mut never, 1).unwrap(), None);
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git").args(args).current_dir(dir).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    /// The analysis waits as an Inbox card (Chris, 2026-10-14): nothing is
    /// written until a person accepts, Accept is one commit naming the file,
    /// and a source proposed once is not asked about again.
    #[test]
    fn a_proposal_waits_and_applies_only_when_accepted() {
        let d = tempfile::tempdir().unwrap();
        let (wiki, game) = (d.path().join("Wiki"), d.path().join("Game"));
        std::fs::create_dir_all(wiki.join(crate::ingest::RAW)).unwrap();
        std::fs::write(wiki.join(crate::ingest::RAW).join(".keep"), "").unwrap();
        std::fs::create_dir_all(wiki.join(".ken")).unwrap();
        let file = wiki.join(crate::wikinew::KNOWLEDGE_FILE);
        std::fs::write(&file, "{ \"repos\": { \"Game\": { \"kind\": \"code\" }, \"Tools\": { \"kind\": \"code\" } } }\n").unwrap();
        git(&wiki, &["init", "-q", "-b", "main"]);
        for (k, v) in [("user.email", "t@t"), ("user.name", "t"), ("core.autocrlf", "false")] {
            git(&wiki, &["config", k, v]);
        }
        git(&wiki, &["add", "-A"]);
        git(&wiki, &["commit", "-q", "-m", "start"]);
        std::fs::create_dir_all(game.join("src/net")).unwrap();
        std::fs::write(game.join("src/net/Packet.java"), "class Packet {}").unwrap();
        let before = std::fs::read_to_string(&file).unwrap();

        let mut db = Db::open_in_memory().unwrap();
        let settings = || crate::knowledge_settings::among(&game, "Game", &[wiki.clone()]);
        let mut asked = 0;
        let mut claude = |_: &str| -> Result<String> {
            asked += 1;
            Ok(r#"{"roles": {"src/net/**": "protocol"}, "controls": [{"question": "how are packets framed", "expect": "src/net/Packet.java"}]}"#.into())
        };
        let id = analyze_and_propose(&game, settings(), &[RepoKind::Code], &mut db, &mut claude, 1).unwrap().expect("a card");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), before, "nothing applies until accepted");
        let waiting = open(&db).unwrap();
        assert_eq!(waiting.len(), 1);
        let (card, p) = &waiting[0];
        assert_eq!(card.title, "1 role rules and 1 controls proposed for Game");
        assert!(card.body.contains("- src/net/** = protocol") && card.body.contains("- how are packets framed | src/net/Packet.java"), "{}", card.body);
        assert_eq!(p.roles, vec![RoleRule { pattern: "src/net/**".into(), role: "protocol".into() }]);

        assert_eq!(analyze_and_propose(&game, settings(), &[RepoKind::Code], &mut db, &mut claude, 2).unwrap(), None, "it waits already");
        assert_eq!(asked, 1, "no second call");

        let (source, accepted) = accept(&wiki, &mut db, id, 3).unwrap();
        assert_eq!(source, "Game");
        assert!(accepted.written && accepted.commit.is_some());
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(v["repos"]["Game"]["roles"]["src/net/**"], "protocol");
        assert_eq!(v["controls"][0]["source"], "Game");
        let shown = git(&wiki, &["show", "--name-only", "--format=%s", "HEAD"]);
        assert!(shown.starts_with("settings: accept the repo analysis of Game into .ken/knowledge.json"), "{shown}");
        assert!(shown.lines().any(|l| l == crate::wikinew::KNOWLEDGE_FILE), "{shown}");
        assert!(open(&db).unwrap().is_empty(), "the card is done");
        assert!(due_of(settings()).is_none(), "accepted: in the file now");

        // Dismissed: dropped, and not asked about again.
        let tools = d.path().join("Tools");
        std::fs::create_dir_all(tools.join("cli")).unwrap();
        std::fs::write(tools.join("cli/main.rs"), "fn main() {}").unwrap();
        let tools_settings = || crate::knowledge_settings::among(&tools, "Tools", &[wiki.clone()]);
        let mut claude = |_: &str| -> Result<String> { Ok(r#"{"roles": {"cli/**": "command"}, "controls": []}"#.into()) };
        let id = analyze_and_propose(&tools, tools_settings(), &[RepoKind::Code], &mut db, &mut claude, 4).unwrap().expect("a card");
        db.resolve_review_item(id, 5).unwrap();
        let mut never = |_: &str| -> Result<String> { panic!("dismissed once, not asked again") };
        assert_eq!(analyze_and_propose(&tools, tools_settings(), &[RepoKind::Code], &mut db, &mut never, 6).unwrap(), None);
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert!(v["repos"]["Tools"].get("roles").is_none(), "a dismissed proposal is never written");
    }

    /// "Analyse again" (Chris, 2026-10-14): a person clears a dismissed
    /// source's mark and asks once; the fresh card replaces one still
    /// waiting, and a code repo's own settings file is never asked about.
    #[test]
    fn a_person_can_ask_for_the_analysis_again() {
        let d = tempfile::tempdir().unwrap();
        let (wiki, game) = (d.path().join("Wiki"), d.path().join("Game"));
        std::fs::create_dir_all(wiki.join(crate::ingest::RAW)).unwrap();
        std::fs::write(wiki.join(crate::ingest::RAW).join(".keep"), "").unwrap();
        std::fs::create_dir_all(wiki.join(".ken")).unwrap();
        let file_path = wiki.join(crate::wikinew::KNOWLEDGE_FILE);
        std::fs::write(&file_path, "{ \"repos\": { \"Game\": { \"kind\": \"code\" } } }\n").unwrap();
        git(&wiki, &["init", "-q", "-b", "main"]);
        for (k, v) in [("user.email", "t@t"), ("user.name", "t"), ("core.autocrlf", "false")] {
            git(&wiki, &["config", k, v]);
        }
        git(&wiki, &["add", "-A"]);
        git(&wiki, &["commit", "-q", "-m", "start"]);
        std::fs::create_dir_all(game.join("src/net")).unwrap();
        std::fs::write(game.join("src/net/Packet.java"), "class Packet {}").unwrap();
        std::fs::write(game.join("src/Main.java"), "class Main {}").unwrap();

        let mut db = Db::open_in_memory().unwrap();
        let settings = || crate::knowledge_settings::among(&game, "Game", &[wiki.clone()]);
        let reply = |role: &str| format!(r#"{{"roles": {{"src/net/**": "{role}"}}, "controls": [{{"question": "where do packets start", "expect": "src/net/Packet.java"}}]}}"#);
        let asked = std::cell::Cell::new(0);
        let mut claude = |_: &str| -> Result<String> {
            asked.set(asked.get() + 1);
            Ok(reply("protocol"))
        };

        // Dismissed once: the source is not asked about by itself again.
        let first = analyze_and_propose(&game, settings(), &[RepoKind::Code], &mut db, &mut claude, 1).unwrap().expect("a card");
        db.resolve_review_item(first, 2).unwrap();
        assert_eq!(analyze_and_propose(&game, settings(), &[RepoKind::Code], &mut db, &mut claude, 3).unwrap(), None);
        assert_eq!(asked.get(), 1);

        // The person asks: the mark is cleared, and one call files a card.
        assert_eq!(forget(&mut db, "game").unwrap(), 1, "the dismissed card goes, matched ignoring case");
        assert!(!proposed_before(&db, "Game").unwrap());
        assert!(db.get_review_item(first).unwrap().is_none());
        let second = analyze_again(&game, settings(), &[RepoKind::Code], &mut db, &mut claude, 4).unwrap().expect("a fresh card");
        assert_eq!(asked.get(), 2, "one call");
        assert_eq!(waiting_for(&db, "Game").unwrap(), vec![second]);

        // Asked again while that card waits: the new card replaces it.
        let mut claude = |_: &str| -> Result<String> { Ok(reply("network")) };
        let third = analyze_again(&game, settings(), &[RepoKind::Code], &mut db, &mut claude, 5).unwrap().expect("a fresh card");
        assert_eq!(waiting_for(&db, "Game").unwrap(), vec![third], "the waiting card is replaced");
        assert!(db.get_review_item(second).unwrap().is_none());
        assert_eq!(forget(&mut db, "Game").unwrap(), 0, "a waiting card is not the mark");

        // A fresh analysis that proposes nothing leaves the waiting card.
        let mut empty = |_: &str| -> Result<String> { Ok(r#"{"roles": {}, "controls": []}"#.into()) };
        assert_eq!(analyze_again(&game, settings(), &[RepoKind::Code], &mut db, &mut empty, 6).unwrap(), None);
        assert_eq!(waiting_for(&db, "Game").unwrap(), vec![third]);

        // Accepted, then the person empties the roles and asks again: the
        // usual Accept fills them, and leaves the controls already there.
        accept(&wiki, &mut db, third, 7).unwrap();
        crate::knowledge_settings::set_role_lines(&wiki, "Game", &[]).unwrap();
        let mut claude = |_: &str| -> Result<String> { Ok(reply("wire")) };
        let fourth = analyze_again(&game, settings(), &[RepoKind::Code], &mut db, &mut claude, 8).unwrap().expect("asked whatever the file holds");
        let (_, accepted) = accept(&wiki, &mut db, fourth, 9).unwrap();
        assert!(accepted.written);
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&file_path).unwrap()).unwrap();
        assert_eq!(v["repos"]["Game"]["roles"]["src/net/**"], "wire");
        assert_eq!(v["controls"].as_array().map(Vec::len), Some(1), "the controls in the file stay as they were");

        // A code repo that keeps its own settings file: nothing is asked.
        let own = d.path().join("Own");
        std::fs::create_dir_all(own.join(".ken")).unwrap();
        std::fs::write(own.join(crate::wikinew::KNOWLEDGE_FILE), "{}\n").unwrap();
        let own_settings = Settings { kb_root: own.clone(), name: "Own".into(), own: true, value: serde_json::json!({}) };
        assert!(!writable(&own_settings));
        let mut never = |_: &str| -> Result<String> { panic!("Ken writes nothing in a code repo") };
        assert_eq!(analyze_again(&own, Some(own_settings), &[RepoKind::Code], &mut db, &mut never, 10).unwrap(), None);
    }
}

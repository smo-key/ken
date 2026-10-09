//! An end-to-end evaluation run on a real folder of repos, through the same
//! ken-core calls the app makes: set up a team with a new wiki, index,
//! embed, extract entities, draft the first wiki with Claude, build the
//! workspace graph, ask curated questions, change a file and check drift,
//! and exercise `.kenignore`. One phase per run, so each can be checked.
//!
//! ```text
//! set KEN_DATA_DIR=<scratch>\data      (required; never the app's own data)
//! set KEN_EVAL_TEAM=Payments             (the team; its new wiki is <team>-Wiki)
//! set KEN_EVAL_EXTRA=Notes               (optional: a notes folder for the draft)
//! cargo run --release -p ken-core --example eval_run -- <phase> <parent> [args]
//! ```
//!
//! Phases: setup · index · embed · wiki · extract [minutes] · kg · ask
//! <questions.tsv> · kb-ask <kb-questions.tsv> · supersede · chat <questions.tsv> ·
//! ingest <file> · pass · transcribe <file> · drift · drift-change <member>
//! <file> · ignore <member> · model <member> · sql <member> <query> · status.
//! Each prints a Markdown report on stdout.
//!
//! Knowledge-base questions (`kb-ask`, and `chat` with `KEN_EVAL_FORMAT=kb`)
//! are `question<TAB>answer<TAB>key facts<TAB>wiki pages<TAB>category`, the
//! pages as `path:lines` in the knowledge base (`KEN_EVAL_KB`, a member's
//! folder name, required) separated by `;`. Files the run keeps go
//! under `KEN_EVAL_OUT` (default `<parent>/../eval-out`). `KEN_EVAL_MEMBERS`
//! (comma-separated folder names) keeps `index` and `embed` to those members.
//!
//! `supersede` runs the supersede pass on the knowledge base (`KEN_EVAL_KB`):
//! one Claude call per cluster of rulings (`KEN_EVAL_MODEL`, default `opus`;
//! `KEN_EVAL_WORKERS` calls at a time, default 3), the pairs stored in its
//! index and written to `supersede.tsv`. `KEN_EVAL_DRY=1` only clusters, with
//! no call; `KEN_SUPERSEDE_MIN`, `_NEIGHBOURS` and `_MAX` tune the clusters.
//!
//! It refuses to run without `KEN_DATA_DIR`, so it can never write into the
//! app's own data.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ken_core::db::Db;
use ken_core::federation::{self, FederationLlm};
use ken_core::local_llm::{self, Priority};
use ken_core::project::Project;
use ken_core::registry::{IndexState, Registry};
use ken_core::routing::{self, MemberDbHandle, MemberInfo};
use ken_core::runner::CancelToken;
use ken_core::workspace::{MemberStatus, Workspace};
use ken_core::workspace_kg_db::WorkspaceKgDb;
use ken_core::{assistant, drift, engine, knowledge_model, scan, setup, wikidraft, wikinew, Error, Result};

/// The team's name (`KEN_EVAL_TEAM`, default `Team`); its new wiki is
/// `<team>-Wiki` beside the repos.
fn team() -> String {
    std::env::var("KEN_EVAL_TEAM").unwrap_or_else(|_| "Team".into())
}

fn wiki_name() -> String {
    format!("{}-Wiki", team())
}

/// A notes folder handed to the first draft as extra material
/// (`KEN_EVAL_EXTRA`, a folder name beside the repos), if any.
fn extra_folder() -> Option<String> {
    std::env::var("KEN_EVAL_EXTRA").ok().filter(|s| !s.is_empty())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: eval_run <phase> <parent> [args]");
        std::process::exit(2);
    }
    let Some(base) = std::env::var_os("KEN_DATA_DIR").map(PathBuf::from) else {
        eprintln!("set KEN_DATA_DIR to a scratch folder first");
        std::process::exit(2);
    };
    let parent = PathBuf::from(&args[1]);
    local_llm::init(base.clone());
    let started = Instant::now();
    let out = match args[0].as_str() {
        "setup" => phase_setup(&base, &parent),
        "index" => phase_index(&base, &parent),
        "embed" => phase_embed(&base, &parent),
        "wiki" => phase_wiki(&base, &parent),
        "extract" => phase_extract(&base, &parent, args.get(2).and_then(|m| m.parse().ok()).unwrap_or(30)),
        "map" => phase_map(&base, &parent),
        "add-repo" => phase_add_repo(&base, &parent, &args[2]),
        "sync" => phase_sync(&base, &parent, &args[2]),
        "ingest" => phase_ingest(&base, &parent, &args[2]),
        "pass" => phase_pass(&base, &parent),
        "transcribe" => phase_transcribe(&base, &args[2]),
        "chat" => phase_chat(&base, &parent, Path::new(args.get(2).map(String::as_str).unwrap_or("questions.tsv"))),
        "kg" => phase_kg(&base, &parent),
        "ask" => phase_ask(&base, &parent, Path::new(args.get(2).map(String::as_str).unwrap_or("questions.tsv"))),
        "kb-ask" => phase_kb_ask(&base, &parent, Path::new(args.get(2).map(String::as_str).unwrap_or("kb-questions.tsv"))),
        "supersede" => phase_supersede(&base, &parent),
        "look" => phase_look(&base, &parent, Path::new(args.get(2).map(String::as_str).unwrap_or("questions.tsv"))),
        "drift" => phase_drift(&base, &parent),
        "drift-change" => phase_drift_change(&base, &parent, &args[2], &args[3]),
        "ignore" => phase_ignore(&base, &parent, &args[2]),
        "status" => phase_status(&base, &parent),
        "sql" => phase_sql(&base, &parent, &args[2], &args[3]),
        "model" => phase_model(&base, &parent, &args[2]),
        "imports" => {
            let t = Instant::now();
            let deps = wikidraft::folder_imports(&parent.join(&args[2]));
            println!("# Imports between folders in `{}`\n\n```\n{}\n```\n\n_{:.1}s_", args[2], deps.as_deref().unwrap_or("(none)"), t.elapsed().as_secs_f64());
            Ok(())
        }
        other => Err(Error::Other(format!("unknown phase {other}"))),
    };
    match out {
        Ok(()) => println!("\n_{} took {:.1}s_", args[0], started.elapsed().as_secs_f64()),
        Err(e) => {
            eprintln!("FAILED: {e}");
            std::process::exit(1);
        }
    }
}

fn today() -> String {
    // YYYY-MM-DD as the app writes it, in UTC: no time-zone crate needed.
    let secs = engine::now_epoch();
    let days = secs.div_euclid(86_400);
    // Civil-from-days (Howard Hinnant), UTC — close enough for a report.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{:02}-{:02}", if m <= 2 { y + 1 } else { y }, m, d)
}

fn members(parent: &Path) -> Result<Vec<(String, Project)>> {
    let ws = Workspace::open(parent)?;
    Ok(ws
        .members
        .into_iter()
        .filter_map(|m| match m.status {
            MemberStatus::Ok(p) => Some((m.name, p)),
            _ => None,
        })
        .collect())
}

/// Whether `index` and `embed` take this member: every one, unless
/// `KEN_EVAL_MEMBERS` names some (a member added later, without walking the
/// big repos again).
fn selected(name: &str) -> bool {
    let only = std::env::var("KEN_EVAL_MEMBERS").unwrap_or_default();
    let names: Vec<&str> = only.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
    names.is_empty() || names.iter().any(|n| n.eq_ignore_ascii_case(name))
}

fn short(s: &str, n: usize) -> String {
    let one = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > n {
        one.chars().take(n).collect::<String>() + "…"
    } else {
        one
    }
}

// ---------------------------------------------------------------- setup

fn phase_setup(base: &Path, parent: &Path) -> Result<()> {
    let prop = setup::propose(parent)?;
    println!("# Setup proposal for `{}`\n", parent.display());
    println!("{} repos, {} folders without git, teams proposed: {:?}\n", prop.repos, prop.without_git, prop.teams);
    println!("| member | include | kind | index | git | evidence | description |\n|---|---|---|---|---|---|---|");
    for r in &prop.rows {
        println!(
            "| {} | {} | {:?} | {:?} | {} | {} | {} |",
            r.member,
            r.include,
            r.kind,
            r.index,
            r.has_git,
            short(&r.evidence.join("; "), 90),
            short(&r.description, 70)
        );
    }
    println!("\n**Ignores proposed**\n");
    for i in &prop.ignores {
        println!(
            "- `{}` {:?} ticked={} fixed={} — {} ({})",
            i.pattern,
            i.state,
            i.ticked,
            i.fixed,
            i.reason,
            short(&i.evidence, 80)
        );
    }

    let mut rows = prop.rows.clone();
    for r in rows.iter_mut().filter(|r| r.include) {
        r.team = Some(team());
    }
    let covered: Vec<wikinew::Covered> = rows
        .iter()
        .filter(|r| r.include)
        .map(|r| wikinew::Covered {
            name: r.member.clone(),
            description: r.description.clone(),
            kind: r.kind.clone(),
            path: r.path.clone(),
        })
        .collect();
    let taken: Vec<String> = rows.iter().map(|r| r.member.clone()).collect();
    let mut wiki = setup::create_wiki(&parent.join(wiki_name()), &team(), &covered, &taken, &today())?;
    wiki.team = Some(team());
    println!("\n**New wiki row**: {} {:?} {:?}", wiki.member, wiki.kind, wiki.index);
    rows.push(wiki);

    let ws = setup::confirm(base, parent, &team(), &rows, &prop.ignores, &today())?;
    println!("\n## Confirmed workspace `{}`\n", ws.config.name);
    let reg = Registry::load(base)?;
    for m in &ws.members {
        let root = parent.join(&m.name);
        let e = reg.entry_at(&root);
        let where_config = if root.join(".ken").join("project.json").exists() { "in repo (.ken/)" } else { "app data (outside)" };
        println!(
            "- **{}** kind={:?} index={:?} team={:?} config={}",
            m.name,
            e.map(|e| e.kind.clone()).unwrap_or_default(),
            e.map(|e| e.index),
            e.and_then(|e| e.team.clone()),
            where_config
        );
    }
    let ki = parent.join(".kenignore");
    println!("\n**`{}`**\n\n```\n{}\n```", ki.display(), std::fs::read_to_string(&ki).unwrap_or_else(|_| "(none written)".into()));
    Ok(())
}

// ---------------------------------------------------------------- index

fn tier_counts(base: &Path, id: uuid::Uuid) -> BTreeMap<i64, i64> {
    let conn = rusqlite::Connection::open(ken_core::db::db_path(base, id)).unwrap();
    let mut stmt = conn.prepare("SELECT tier, COUNT(*) FROM files GROUP BY tier").unwrap();
    stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect()
}

fn phase_index(base: &Path, parent: &Path) -> Result<()> {
    println!("# Index\n\n| member | kind | files in index | full / search-only | failed | chunks | queued for extraction | time |\n|---|---|---|---|---|---|---|---|");
    let reg = Registry::load(base)?;
    for (name, p) in members(parent)?.into_iter().filter(|(n, _)| selected(n)) {
        let t = Instant::now();
        let mut db = Db::open(base, p.config.id)?;
        let _ = db.refresh_stored_kinds();
        let stats = scan::scan(&p, &mut db)?;
        let _ = db.backfill_page_meta();
        let _ = db.backfill_page_links();
        let _ = db.backfill_code_map();
        let _ = db.backfill_extractions();
        let tiers = tier_counts(base, p.config.id);
        println!(
            "| {} | {:?} | {} | {} / {} | {} | {} | {} | {:.1}s |",
            name,
            reg.entry_at(&p.root).map(|e| e.kind.clone()).unwrap_or_default(),
            db.file_count()?,
            tiers.get(&0).copied().unwrap_or(0),
            tiers.get(&1).copied().unwrap_or(0),
            stats.failed,
            db.chunk_count()?,
            db.extractable_file_count()?,
            t.elapsed().as_secs_f64()
        );
    }
    Ok(())
}

// ---------------------------------------------------------------- embed

fn phase_embed(base: &Path, parent: &Path) -> Result<()> {
    let mut emb = ken_core::embedder::installed_embedding_model()
        .ok_or_else(|| Error::Other("no embedding model installed under KEN_DATA_DIR/whisper".into()))?;
    println!("# Meaning index ({}, {} dims)\n\n| member | chunks | embedded | time |\n|---|---|---|---|", emb.model_id(), emb.dim());
    for (name, p) in members(parent)?.into_iter().filter(|(n, _)| selected(n)) {
        let t = Instant::now();
        let mut db = Db::open(base, p.config.id)?;
        let mut last = 0usize;
        let built = engine::rebuild_semantic_index(&p, &mut db, emb.as_mut(), &CancelToken::new(), |done, total| {
            if total > 0 && done * 10 / total > last {
                last = done * 10 / total;
                eprintln!("  {name}: {done}/{total}");
            }
        })?;
        println!("| {} | {} | {} | {:.1}s |", name, db.chunk_count()?, built, t.elapsed().as_secs_f64());
    }
    Ok(())
}

// ---------------------------------------------------------------- wiki

fn team_members(base: &Path, parent: &Path) -> Result<Vec<wikidraft::TeamMember>> {
    let reg = Registry::load(base)?;
    Ok(members(parent)?
        .into_iter()
        .map(|(name, p)| {
            let e = reg.entry_at(&p.root);
            wikidraft::TeamMember {
                name,
                root: p.root.clone(),
                kind: e.map(|e| e.kind.clone()).unwrap_or_default(),
                team: e.and_then(|e| e.team.clone()),
            }
        })
        .collect())
}

fn claude(root: &Path) -> Result<impl FnMut(&str) -> Result<String>> {
    let bin = ken_core::runner::discover_claude().ok_or_else(|| Error::Other("claude CLI not found".into()))?;
    let root = root.to_path_buf();
    Ok(move |prompt: &str| {
        let t = Instant::now();
        let out = assistant::oneshot(&bin, &root, prompt, Duration::from_secs(600), &CancelToken::new())?;
        eprintln!("  claude: {} chars in, {:.0}s", prompt.len(), t.elapsed().as_secs_f64());
        match out {
            assistant::OneshotOutcome::Completed(text) => Ok(text),
            other => Err(Error::Other(format!("claude: {other:?}"))),
        }
    })
}

fn phase_wiki(base: &Path, parent: &Path) -> Result<()> {
    let wiki = Project::open(&parent.join(wiki_name()))?;
    let team = team_members(base, parent)?;
    let repos = wikidraft::team_repos(&wiki_name(), &team);
    println!("# First wiki draft\n\nSources: {:?}\n", repos.iter().map(|r| &r.0).collect::<Vec<_>>());
    // A folder a person would hand over as extra material, such as the
    // team's own notes vault when setup counted it as a wiki (team_repos
    // leaves wikis out).
    let extra = extra_folder().map(|f| parent.join(f)).filter(|d| d.is_dir() && !repos.iter().any(|r| &r.1 == d));
    let mut db = Db::open(base, wiki.config.id)?;
    let generate = claude(&wiki.root)?;
    let report = wikidraft::draft_team(
        &wiki.root,
        &wiki_name(),
        &mut db,
        &repos,
        extra.as_deref(),
        &today(),
        engine::now_epoch(),
        generate,
    )?;
    println!("- drafted: {:?}\n- kept: {:?}\n- failed: {:?}\n- proposed: {:?}\n- still to fill: {:?}\n- sources used: {}", report.drafted, report.kept, report.failed, report.proposed, report.to_fill, report.sources.len());
    // What the check against the checkout rewrote or sent back, page by page.
    println!("- corrected: {}", report.corrected.len());
    for c in &report.corrected {
        println!("  - {c}");
    }
    // What the check of each page against its sources did, then every change.
    println!("- verified: {}", report.verified.len());
    for v in &report.verified {
        println!("  - {v}");
    }
    println!("- changes: {}", report.changes.len());
    for c in &report.changes {
        let one = |s: &str| s.replace('\n', " ");
        println!("  - {} {}: `{}` -> `{}` ({})", c.page, c.kind, one(&c.before), one(&c.after), one(&c.evidence));
    }
    let stats = scan::scan(&wiki, &mut db)?;
    println!("- wiki rescanned: {} added, {} updated", stats.added, stats.updated);
    Ok(())
}

// ---------------------------------------------------------------- extract

/// Ken's chat on each question (or `KEN_EVAL_ONLY`), headless: the chat's
/// guide and Ken's MCP server over this evaluation's data, started in the
/// team wiki (or the member `KEN_EVAL_START`) with every member readable.
/// Found when an expected file is in the answer (it cites `ken://<id>/<path>`).
///
/// Each answer is kept whole, with every tool call and what it returned, in
/// `<KEN_EVAL_OUT>/<KEN_EVAL_LABEL>/` (label default `chat-full`, or
/// `chat-mcp` with `KEN_EVAL_MCP_ONLY=1`): `qNN.answer.md`, `qNN.tools.jsonl`
/// (one call a line: tool, input, files it returned or read),
/// `qNN.events.jsonl` (the CLI's own stream) and `qNN.summary.json` (tools
/// used, which knowledge-base pages came back, were read or cited, and the
/// time). `KEN_EVAL_FORMAT=kb` reads the knowledge-base question file.
fn phase_chat(base: &Path, parent: &Path, questions: &Path) -> Result<()> {
    let kb = kb_format();
    let qs = read_questions(questions, kb)?;
    let only = only();
    let binary = ken_core::runner::discover_claude().ok_or_else(|| Error::Other("Claude Code not found".into()))?;
    let mcp = std::env::current_exe().map_err(|e| Error::Other(e.to_string()))?.parent().and_then(|d| d.parent()).map(|d| d.join("ken-mcp.exe")).filter(|p| p.exists())
        .ok_or_else(|| Error::Other("ken-mcp.exe not built next to the harness".into()))?;
    let start = std::env::var("KEN_EVAL_START").ok().filter(|s| !s.is_empty()).unwrap_or_else(wiki_name);
    let wiki = Project::open(&parent.join(&start))?;
    let cfg = base.join("chat-mcp").join("eval.json");
    std::fs::create_dir_all(cfg.parent().unwrap()).map_err(|e| Error::Other(e.to_string()))?;
    let config = serde_json::json!({"mcpServers": {"ken": {"command": mcp, "args": ["--project", wiki.root], "env": {"KEN_DATA_DIR": base}}}});
    std::fs::write(&cfg, config.to_string()).map_err(|e| Error::Other(e.to_string()))?;
    let ms = members(parent)?;
    let dirs: Vec<PathBuf> = ms.iter().map(|(_, p)| p.root.clone()).collect();
    let cite: Vec<(String, uuid::Uuid)> = ms.iter().map(|(n, p)| (n.clone(), p.config.id)).collect();
    let folders = Folders::new(&ms);
    // KEN_EVAL_MCP_ONLY=1: Ken's MCP tools alone, no file tools, to
    // measure what Ken's own search answers.
    let look = std::env::var("KEN_EVAL_MCP_ONLY").map_or(true, |v| v != "1");
    let label = std::env::var("KEN_EVAL_LABEL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| if look { "chat-full".into() } else { "chat-mcp".into() });
    let out = out_root(parent).join(&label);
    std::fs::create_dir_all(&out).map_err(|e| Error::io(&out, e))?;
    let write = |name: String, body: &str| -> Result<()> {
        let path = out.join(name);
        std::fs::write(&path, body).map_err(|e| Error::io(&path, e))
    };
    let kb_prefix = format!("{}/", kb_member().to_lowercase());
    let ken_wiki = format!("{}/", wiki_name().to_lowercase());
    let (mut asked, mut found, mut not_there) = (0, 0, 0);
    let (mut kb_used, mut exp_read, mut exp_cited) = (0, 0, 0);
    println!("# Ken's chat\n\nstarted in `{start}`, MCP `{}`, file tools {look}, kept in `{}`\n", mcp.display(), out.display());
    for q in &qs {
        if !only.is_empty() && !only.contains(&q.n) {
            continue;
        }
        let n = q.n;
        let t = Instant::now();
        // As the app sends a workspace turn: how to link each repo's files.
        let prompt = match ken_core::chat::build_cite_preamble(&cite) {
            Some(c) => format!("{c}\n\n{}", q.text),
            None => q.text.clone(),
        };
        let (outcome, events) =
            assistant::chat_oneshot_traced(&binary, &wiki.root, &dirs, &cfg, &prompt, Duration::from_secs(300), &CancelToken::new(), look)?;
        let secs = t.elapsed().as_secs_f64();
        let status = match &outcome {
            assistant::OneshotOutcome::Completed(_) => "completed".to_string(),
            other => format!("{other:?}"),
        };
        let answer = match outcome {
            assistant::OneshotOutcome::Completed(t) => t,
            other => format!("(no answer: {other:?})"),
        };
        // A ken:// citation names the repo by id: read it as `<repo>/<path>`,
        // the form the expected answers use, so a right citation counts.
        let mut cited = answer.clone();
        for (name, id) in &cite {
            cited = cited.replace(&format!("ken://{id}/"), &format!("{name}/"));
        }
        let lower = cited.to_lowercase().replace("%20", " ");
        let hit = q.expects.iter().any(|e| lower.contains(e));
        let missing = !hit && (lower.contains("isn't there") || lower.contains("not there") || lower.contains("could not find") || lower.contains("couldn't find"));
        asked += 1;
        found += usize::from(hit);
        not_there += usize::from(missing);

        // What the turn did: each call, and the files it returned or read.
        let calls = tool_calls(&events, &folders, &start);
        let mut tools: BTreeMap<String, usize> = BTreeMap::new();
        let (mut returned, mut read): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
        for c in &calls {
            *tools.entry(c["name"].as_str().unwrap_or("?").to_string()).or_default() += 1;
            for f in c["returned"].as_array().into_iter().flatten().filter_map(|f| f.as_str()) {
                if !returned.iter().any(|r| r == f) {
                    returned.push(f.to_string());
                }
            }
            if let Some(f) = c["read"].as_str() {
                if !read.iter().any(|r| r == f) {
                    read.push(f.to_string());
                }
            }
        }
        let cited_files = folders.files_in(&answer);
        let under = |files: &[String], prefix: &str| -> Vec<String> { files.iter().filter(|f| f.to_lowercase().starts_with(prefix)).cloned().collect() };
        let expected = |files: &[String]| files.iter().any(|f| q.expects.iter().any(|e| f.to_lowercase().contains(e)));
        let (kb_returned, kb_read, kb_cited) = (under(&returned, &kb_prefix), under(&read, &kb_prefix), under(&cited_files, &kb_prefix));
        let used = !kb_read.is_empty() || !kb_cited.is_empty();
        let (e_read, e_cited) = (expected(&read), expected(&cited_files) || hit);
        kb_used += usize::from(used);
        exp_read += usize::from(e_read);
        exp_cited += usize::from(e_cited);
        let result = events.iter().rev().find(|e| e["type"] == "result");
        let summary = serde_json::json!({
            "n": n,
            "category": q.category,
            "question": q.text,
            "outcome": status,
            "seconds": (secs * 10.0).round() / 10.0,
            "duration_ms": result.and_then(|r| r["duration_ms"].as_u64()),
            "num_turns": result.and_then(|r| r["num_turns"].as_u64()),
            "cost_usd": result.and_then(|r| r["total_cost_usd"].as_f64()),
            "file_tools": look,
            "calls": calls.len(),
            "tools": tools,
            "expected": q.expects,
            "expected_returned": expected(&returned),
            "expected_read": e_read,
            "expected_cited": e_cited,
            "kb_used": used,
            "kb_returned": kb_returned,
            "kb_read": kb_read,
            "kb_cited": kb_cited,
            "ken_wiki_read": under(&read, &ken_wiki),
            "other_read": read.iter().filter(|f| { let l = f.to_lowercase(); !l.starts_with(&kb_prefix) && !l.starts_with(&ken_wiki) }).collect::<Vec<_>>(),
            "cited": cited_files,
            "answer_chars": answer.chars().count(),
        });
        write(format!("q{n:02}.answer.md"), &answer)?;
        write(format!("q{n:02}.tools.jsonl"), &calls.iter().map(|c| c.to_string() + "\n").collect::<String>())?;
        write(format!("q{n:02}.events.jsonl"), &events.iter().map(|e| e.to_string() + "\n").collect::<String>())?;
        write(format!("q{n:02}.summary.json"), &(serde_json::to_string_pretty(&summary).unwrap_or_default() + "\n"))?;

        let verdict = if kb {
            format!(
                "wiki {} · expected page read {} · cited {}",
                if used { "used" } else { "NOT USED" },
                if e_read { "yes" } else { "no" },
                if e_cited { "yes" } else { "no" }
            )
        } else if hit {
            "FOUND".to_string()
        } else if missing {
            "SAID NOT THERE".to_string()
        } else {
            "MISSED".to_string()
        };
        let used_tools: Vec<String> = tools.iter().map(|(t, c)| format!("{} ×{c}", t.trim_start_matches("mcp__ken__"))).collect();
        println!(
            "## Q{n}. {}\n\n{verdict} · {secs:.0}s · {status} · expected `{}`\n\ntools: {}\n\n{}\n\n_full answer: `q{n:02}.answer.md`_\n",
            q.text,
            q.expect_raw,
            if used_tools.is_empty() { "none".to_string() } else { used_tools.join(", ") },
            short(&answer, 700)
        );
    }
    if kb {
        println!("## Score\n\n{asked} asked: the knowledge base read or cited in {kb_used}; the expected page read in {exp_read}, cited in {exp_cited}; {not_there} said it was not there.");
    } else {
        println!("## Score\n\n{found} of {asked} answers name an expected file; {not_there} said it was not there.");
    }
    Ok(())
}

// ------------------------------------------------------- knowledge base

/// The knowledge base the KB questions are about: a member's folder name
/// (`KEN_EVAL_KB`); empty when unset, which the phases that need it refuse.
fn kb_member() -> String {
    std::env::var("KEN_EVAL_KB").unwrap_or_default().trim().to_string()
}

/// Whether `chat` reads the knowledge-base question file (`KEN_EVAL_FORMAT=kb`).
fn kb_format() -> bool {
    std::env::var("KEN_EVAL_FORMAT").is_ok_and(|v| v.eq_ignore_ascii_case("kb"))
}

/// `KEN_EVAL_DUMP=<file>`: each question's per-member hits as one JSON line
/// (member, whether it is the knowledge base or a reference repo, and each
/// hit's path, score and band), so a merge policy can be tried on the same
/// lists without searching again.
fn dump_hits(label: &str, n: usize, expects: &[String], plan: &routing::RoutePlan, hits: &[routing::MemberHits]) {
    let Some(path) = std::env::var_os("KEN_EVAL_DUMP") else { return };
    let members: Vec<serde_json::Value> = hits
        .iter()
        .map(|m| {
            let kinds = ken_core::registry::entry_of(m.project_id).map(|(_, k)| k).unwrap_or_default();
            serde_json::json!({
                "member": m.member_name,
                "order": plan.targets.iter().position(|t| *t == m.project_id),
                "kb": routing::is_knowledge_base(&kinds),
                "reference": kinds.contains(&ken_core::registry::RepoKind::Reference),
                "hits": m.hits.iter().map(|h| serde_json::json!({"path": h.path, "score": h.score, "band": h.page.as_ref().map_or(1, |p| p.band)})).collect::<Vec<_>>(),
            })
        })
        .collect();
    let line = serde_json::json!({"label": label, "n": n, "expects": expects, "reason": format!("{:?}", plan.reason), "platform": plan.platform, "members": members});
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{line}");
    }
}

/// The question numbers in `KEN_EVAL_ONLY` (e.g. `9,10,15`); empty for all.
fn only() -> Vec<usize> {
    std::env::var("KEN_EVAL_ONLY").unwrap_or_default().split(',').filter_map(|n| n.trim().parse().ok()).collect()
}

/// Where a run keeps its files: `KEN_EVAL_OUT`, else `eval-out` beside the
/// evaluation's folder.
fn out_root(parent: &Path) -> PathBuf {
    match std::env::var_os("KEN_EVAL_OUT") {
        Some(o) if !o.is_empty() => PathBuf::from(o),
        _ => parent.parent().unwrap_or(parent).join("eval-out"),
    }
}

/// One question, numbered as the file lists them (comment and blank lines
/// not counted).
struct Question {
    n: usize,
    text: String,
    /// A right source contains one of these, as lowercase `member/path`.
    expects: Vec<String>,
    /// The expected column as written: paths, or the wiki pages.
    expect_raw: String,
    category: String,
}

/// The pages in a knowledge-base question's wiki column, `path:lines`
/// separated by `;`, without their lines. `-` or `none` is no page (a
/// question the knowledge base does not answer).
fn wiki_pages(column: &str) -> Vec<String> {
    column
        .split(';')
        .map(str::trim)
        .filter(|p| !p.is_empty() && *p != "-" && !p.eq_ignore_ascii_case("none"))
        .map(|p| match p.rsplit_once(':') {
            Some((path, lines)) if !lines.is_empty() && lines.chars().all(|c| c.is_ascii_digit() || c == '-' || c == ',') => path.trim().to_string(),
            _ => p.to_string(),
        })
        .collect()
}

/// A question file: `question<TAB>expected|paths`, or with `kb` the
/// knowledge-base format, whose expected files are its wiki pages in
/// [`kb_member`].
fn read_questions(path: &Path, kb: bool) -> Result<Vec<Question>> {
    let text = std::fs::read_to_string(path).map_err(|e| Error::Other(format!("{}: {e}", path.display())))?;
    let member = kb_member();
    Ok(text
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .enumerate()
        .map(|(i, line)| {
            let cols: Vec<&str> = line.split('\t').collect();
            if kb {
                let wiki = cols.get(3).copied().unwrap_or("");
                Question {
                    n: i + 1,
                    text: cols[0].trim().to_string(),
                    expects: wiki_pages(wiki).iter().map(|p| format!("{member}/{p}").to_lowercase()).collect(),
                    expect_raw: wiki.trim().to_string(),
                    category: cols.get(4).map(|c| c.trim().to_string()).unwrap_or_default(),
                }
            } else {
                let expect = cols.get(1).copied().unwrap_or("");
                Question {
                    n: i + 1,
                    text: cols[0].to_string(),
                    expects: expect.split('|').filter(|e| !e.is_empty()).map(|e| e.to_lowercase()).collect(),
                    expect_raw: expect.to_string(),
                    category: String::new(),
                }
            }
        })
        .collect())
}

/// `ken://<project id>/`, as Ken's tools address a file.
fn ken_address_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"ken://([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})/").unwrap())
}

/// A file path at the start of the rest of a line: up to the first
/// extension that a delimiter or the line's end follows, so a page name
/// with spaces (`Feature Status.md#L12`) comes whole.
fn path_head(rest: &str) -> Option<String> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r#"^(.+?\.[A-Za-z0-9]{1,8})(?:[:#)\]\s,;'"`>|*]|$)"#).unwrap());
    re.captures(rest).map(|c| c[1].replace("%20", " "))
}

/// The workspace's members as a chat turn names them: by id in a ken://
/// address, by folder or project name in a tool's `project`, by folder in a
/// file tool's absolute path.
struct Folders {
    /// (folder name, project name, id, root with `/` and lowercase).
    all: Vec<(String, String, uuid::Uuid, String)>,
}

impl Folders {
    fn new(ms: &[(String, Project)]) -> Folders {
        Folders {
            all: ms
                .iter()
                .map(|(n, p)| {
                    let root = p.root.to_string_lossy().replace('\\', "/").trim_end_matches('/').to_ascii_lowercase();
                    (n.clone(), p.config.name.clone(), p.config.id, root)
                })
                .collect(),
        }
    }

    /// The folder name for a member's folder name, project name or id.
    fn named(&self, project: &str) -> Option<&str> {
        let p = project.trim();
        self.all
            .iter()
            .find(|(n, name, id, _)| n.eq_ignore_ascii_case(p) || name.eq_ignore_ascii_case(p) || id.to_string().eq_ignore_ascii_case(p))
            .map(|(n, ..)| n.as_str())
    }

    /// Every file `text` names, as `member/path`, first mention first: ken://
    /// addresses, absolute paths under a member, and `member/path` written out.
    fn files_in(&self, text: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut push = |f: String| {
            if !out.contains(&f) {
                out.push(f);
            }
        };
        for c in ken_address_re().captures_iter(text) {
            let rest = text[c.get(0).unwrap().end()..].lines().next().unwrap_or("");
            if let (Some(name), Some(path)) = (self.named(&c[1]), path_head(rest)) {
                push(format!("{name}/{path}"));
            }
        }
        // Byte offsets agree: an ASCII-only lowercase keeps every length.
        let flat = text.replace('\\', "/");
        let lower = flat.to_ascii_lowercase();
        for (folder, name, _, root) in &self.all {
            let needles = [format!("{root}/"), format!("{}/", folder.to_ascii_lowercase()), format!("{}/", name.to_ascii_lowercase())];
            for needle in needles.iter() {
                let mut from = 0;
                while let Some(i) = lower[from..].find(needle.as_str()) {
                    let at = from + i;
                    let start = at + needle.len();
                    // A name only where a path could begin, not inside a longer one.
                    let clean = at == 0 || !lower.as_bytes()[at - 1].is_ascii_alphanumeric() && !matches!(lower.as_bytes()[at - 1], b'-' | b'_' | b'.');
                    if clean || needle.starts_with(root.as_str()) {
                        if let Some(path) = path_head(flat[start..].lines().next().unwrap_or("")) {
                            push(format!("{folder}/{path}"));
                        }
                    }
                    from = start;
                }
            }
        }
        out
    }
}

/// The text of a tool result's content: a string, or text blocks.
fn result_text(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(blocks) => blocks.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

/// Every tool call in a chat turn's events, in order, each with the input
/// it was given (query, path, project), whether it failed, the files its
/// result named (`returned`) and the file it read (`read`, for
/// read_document and Read). `start` is the member a read_document without a
/// project reads from.
fn tool_calls(events: &[serde_json::Value], folders: &Folders, start: &str) -> Vec<serde_json::Value> {
    let mut results: HashMap<String, (bool, String)> = HashMap::new();
    for e in events.iter().filter(|e| e["type"] == "user") {
        for b in e["message"]["content"].as_array().into_iter().flatten().filter(|b| b["type"] == "tool_result") {
            let id = b["tool_use_id"].as_str().unwrap_or_default().to_string();
            results.insert(id, (b["is_error"].as_bool().unwrap_or(false), result_text(&b["content"])));
        }
    }
    let mut calls = Vec::new();
    for e in events.iter().filter(|e| e["type"] == "assistant") {
        for b in e["message"]["content"].as_array().into_iter().flatten().filter(|b| b["type"] == "tool_use") {
            let name = b["name"].as_str().unwrap_or_default();
            let input = &b["input"];
            let field = |keys: &[&str]| keys.iter().find_map(|k| input[*k].as_str().map(str::to_string));
            let path = field(&["path", "file_path", "folder"]);
            let project = field(&["project"]);
            let (is_error, text) = results.get(b["id"].as_str().unwrap_or_default()).cloned().unwrap_or((false, String::new()));
            let read = match (name, &path) {
                (n, Some(p)) if n.ends_with("read_document") => {
                    if p.starts_with("ken://") {
                        folders.files_in(p).into_iter().next()
                    } else {
                        let member = project.as_deref().and_then(|pr| folders.named(pr)).unwrap_or(start);
                        Some(format!("{member}/{}", p.split('#').next().unwrap_or(p).trim_start_matches('/')))
                    }
                }
                ("Read", Some(p)) => folders.files_in(p).into_iter().next(),
                _ => None,
            };
            calls.push(serde_json::json!({
                "seq": calls.len() + 1,
                "name": name,
                "input": input,
                "query": field(&["query", "pattern"]),
                "path": path,
                "project": project,
                "subagent": e["parent_tool_use_id"].as_str(),
                "is_error": is_error,
                "result_chars": text.chars().count(),
                "returned": folders.files_in(&text),
                "read": read,
            }));
        }
    }
    calls
}

/// Where each knowledge-base question's wiki page ranks, as Claude's tools
/// would show it: in hybrid search of the knowledge base alone (what
/// semantic_search on it returns, 20 deep), in keyword search of it, and in
/// route_query's merge across the routed members (its default 10, then 30
/// deep). Pages are counted once, at their best chunk. Writes
/// `<KEN_EVAL_OUT>/kb-ask.tsv` for the grading script.
fn phase_kb_ask(base: &Path, parent: &Path, questions: &Path) -> Result<()> {
    const KB_DEPTH: usize = 20;
    const ROUTE_LIMIT: usize = 10;
    const ROUTE_DEEP: usize = 30;
    let qs = read_questions(questions, true)?;
    let mut emb = ken_core::embedder::installed_embedding_model();
    let model = emb.as_ref().map(|e| e.model_id());
    let kg = WorkspaceKgDb::open(parent).ok();
    let ms: Vec<(String, Project, Db)> = members(parent)?
        .into_iter()
        .filter_map(|(n, p)| Db::open_read_only(base, p.config.id).ok().map(|db| (n, p, db)))
        .collect();
    let kb = kb_member();
    let Some((_, _, kb_db)) = ms.iter().find(|(n, _, _)| n.eq_ignore_ascii_case(&kb)) else {
        return Err(Error::Other(format!("knowledge base {kb:?} is not a member of this workspace: set KEN_EVAL_KB to its folder name, then add, index and embed it")));
    };
    // As route_query: members by their project name, each searched with the
    // question's vector only when its index was built by the same model.
    let info: Vec<MemberInfo> = ms
        .iter()
        .map(|(_, p, db)| MemberInfo { project_id: p.config.id, name: p.config.name.clone(), index_ready: db.vec_available(), last_activity: 0, knowledge_base: ken_core::registry::entry_of(p.config.id).is_some_and(|(_, k)| ken_core::routing::is_knowledge_base(&k)) })
        .collect();
    let shared = routing::workspace_vocabulary(ms.iter().map(|(_, _, db)| db));
    let types = ken_core::contenttype::parse_filter(std::env::var("KEN_EVAL_TYPES").ok().as_deref());
    let pages = |paths: Vec<String>| -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for p in paths {
            if !out.contains(&p) {
                out.push(p);
            }
        }
        out
    };
    let rank_in = |list: &[String], expects: &[String]| list.iter().position(|p| expects.iter().any(|e| p.to_lowercase().contains(e))).map(|r| r + 1);
    let show = |r: Option<usize>| r.map_or("—".to_string(), |r| r.to_string());

    let mut rows = vec!["n\tcategory\tkb_hybrid\tkb_keyword\troute\troute30\tkb_routed\troute_top5".to_string()];
    let mut by_cat: BTreeMap<String, (usize, usize, usize, usize)> = BTreeMap::new();
    println!("# Knowledge-base questions: where the wiki page ranks\n\nknowledge base `{kb}` · meaning model {:?}\n", model);
    for q in &qs {
        let qv = emb.as_mut().and_then(|e| e.embed_query(&q.text).ok());
        let fits = |db: &Db| model.is_some() && db.embed_model().ok().flatten() == model;
        let kb_list = |v: Option<&[f32]>| -> Vec<String> {
            pages(routing::search_member_of(kb_db, &q.text, v, KB_DEPTH, &types).unwrap_or_default().into_iter().map(|h| format!("{kb}/{}", h.path)).collect())
        };
        let kb_hybrid = kb_list(qv.as_deref().filter(|_| fits(kb_db)));
        let kb_keyword = kb_list(None);
        let plan = routing::plan_route(&q.text, &info, kg.as_ref());
        let route = |limit: usize| -> Vec<String> {
            let hits: Vec<routing::MemberHits> = plan
                .targets
                .iter()
                .filter_map(|id| ms.iter().find(|(_, p, _)| p.config.id == *id))
                .filter(|(_, _, db)| db.vec_available())
                .map(|(n, p, db)| routing::MemberHits {
                    project_id: p.config.id,
                    member_name: n.clone(),
                    status: routing::MemberStatus::Searched,
                    hits: routing::search_member_of_with(db, &q.text, qv.as_deref().filter(|_| fits(db)), limit, &types, Some(&shared)).unwrap_or_default(),
                })
                .collect();
            if limit == ROUTE_LIMIT {
                dump_hits("kb", q.n, &q.expects, &plan, &hits);
            }
            pages(routing::merge_routed(&plan, &hits, limit).results.iter().map(|r| format!("{}/{}", r.member_name, r.path)).collect())
        };
        let routed = route(ROUTE_LIMIT);
        let deep = route(ROUTE_DEEP);
        let kb_routed = plan.targets.iter().any(|id| ms.iter().any(|(n, p, _)| p.config.id == *id && n.eq_ignore_ascii_case(&kb)));
        let (rh, rk, rr, rd) = (rank_in(&kb_hybrid, &q.expects), rank_in(&kb_keyword, &q.expects), rank_in(&routed, &q.expects), rank_in(&deep, &q.expects));
        if !q.expects.is_empty() {
            let c = by_cat.entry(q.category.clone()).or_default();
            c.0 += 1;
            c.1 += usize::from(rh.is_some_and(|r| r <= 5));
            c.2 += usize::from(rr.is_some_and(|r| r <= 5));
            c.3 += usize::from(rr.is_some());
        }
        let targets: Vec<&str> = plan.targets.iter().filter_map(|id| ms.iter().find(|(_, p, _)| p.config.id == *id)).map(|(n, _, _)| n.as_str()).collect();
        println!(
            "## Q{}. [{}] {}\n\nexpected `{}` · knowledge base hybrid {} · keyword {} · route_query {} (30 deep: {}) · routed to {:?}\n",
            q.n,
            q.category,
            q.text,
            q.expect_raw,
            show(rh),
            show(rk),
            show(rr),
            show(rd),
            targets
        );
        for (i, p) in routed.iter().take(5).enumerate() {
            println!("{}. `{p}`", i + 1);
        }
        println!();
        let opt = |r: Option<usize>| if q.expects.is_empty() { "n/a".to_string() } else { r.map_or(String::new(), |r| r.to_string()) };
        rows.push(format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            q.n,
            q.category,
            opt(rh),
            opt(rk),
            opt(rr),
            opt(rd),
            kb_routed,
            routed.iter().take(5).cloned().collect::<Vec<_>>().join(" ; ")
        ));
    }
    println!("## By category (questions with a wiki page)\n\n| category | questions | kb hybrid top 5 | route_query top 5 | route_query top 10 |\n|---|---|---|---|---|");
    for (cat, (n, h5, r5, r10)) in &by_cat {
        println!("| {cat} | {n} | {h5} | {r5} | {r10} |");
    }
    let out = out_root(parent);
    std::fs::create_dir_all(&out).map_err(|e| Error::io(&out, e))?;
    let path = out.join("kb-ask.tsv");
    std::fs::write(&path, rows.join("\n") + "\n").map_err(|e| Error::io(&path, e))?;
    println!("\nwritten: `{}`", path.display());
    Ok(())
}

// ---------------------------------------------------------------- supersede

/// The supersede pass on the knowledge base, as the app would run it: its
/// decisions log's clusters, one Claude call each, the pairs stored in its
/// index. Prints the cost (clusters, calls, prompt size) and every pair.
fn phase_supersede(base: &Path, parent: &Path) -> Result<()> {
    use ken_core::supersede;
    let kb = kb_member();
    let Some((_, project)) = members(parent)?.into_iter().find(|(n, _)| n.eq_ignore_ascii_case(&kb)) else {
        return Err(Error::Other(format!("knowledge base {kb:?} is not a member of this workspace: set KEN_EVAL_KB to its folder name")));
    };
    let db = Db::open(base, project.config.id)?;
    let roles = ken_core::authority::Roles::of_repo(&project.root);
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let mut tuning = supersede::Tuning::default();
    if let Some(v) = env("KEN_SUPERSEDE_MIN").and_then(|v| v.parse().ok()) {
        tuning.min_score = v;
    }
    if let Some(v) = env("KEN_SUPERSEDE_NEIGHBOURS").and_then(|v| v.parse().ok()) {
        tuning.neighbours = v;
    }
    if let Some(v) = env("KEN_SUPERSEDE_MAX").and_then(|v| v.parse().ok()) {
        tuning.max_cluster = v;
    }
    let dry = env("KEN_EVAL_DRY").is_some_and(|v| v != "0");
    let model = env("KEN_EVAL_MODEL").unwrap_or_else(|| "opus".into());
    let workers: usize = env("KEN_EVAL_WORKERS").and_then(|v| v.parse().ok()).unwrap_or(3);
    let bin = ken_core::runner::discover_claude().ok_or_else(|| Error::Other("claude CLI not found".into()))?;
    // An empty folder to answer from: the prompt holds everything, and the
    // repo's own CLAUDE.md would only add to every call.
    let root = out_root(parent).join("supersede-cwd");
    std::fs::create_dir_all(&root).map_err(|e| Error::io(&root, e))?;
    let judge = |prompt: &str| -> Result<String> {
        let t = Instant::now();
        let out = assistant::oneshot_answer(&bin, &root, prompt, Some(&model), Duration::from_secs(600), &CancelToken::new())?;
        eprintln!("  claude: {} chars in, {:.0}s", prompt.len(), t.elapsed().as_secs_f64());
        match out {
            assistant::OneshotOutcome::Completed(text) => Ok(text),
            other => Err(Error::Other(format!("claude: {other:?}"))),
        }
    };
    let t = Instant::now();
    let reports = supersede::run(&db, &roles, &tuning, (!dry).then_some(&judge as &supersede::Judge<'_>), workers)?;
    println!(
        "# Supersede pass on `{kb}`{}\n\ntuning: min score {}, {} neighbours, at most {} entries a cluster · model `{model}` · {workers} at a time\n",
        if dry { " (dry run: no calls)" } else { "" },
        tuning.min_score,
        tuning.neighbours,
        tuning.max_cluster
    );
    let mut rows = vec!["log\tearlier\tlater\trelation\tpoint\tevidence\tearlier_line\tlater_line\tsource".to_string()];
    for r in &reports {
        let mut sizes: BTreeMap<usize, usize> = BTreeMap::new();
        for c in &r.clusters {
            *sizes.entry(c.len()).or_default() += 1;
        }
        let in_clusters: usize = r.clusters.iter().map(Vec::len).sum();
        println!(
            "## `{}`\n\n| entries | with a vector | in a cluster | clusters | sizes | calls | failed | prompt characters | marked in the log | pairs |\n|---|---|---|---|---|---|---|---|---|---|\n| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            r.log,
            r.entries,
            r.vectors,
            in_clusters,
            r.clusters.len(),
            sizes.iter().map(|(s, n)| format!("{n}×{s}")).collect::<Vec<_>>().join(" "),
            r.calls,
            r.failed,
            r.prompt_chars,
            r.marked,
            r.pairs.len()
        );
        println!("### Clusters\n");
        for c in &r.clusters {
            println!("- {}", c.join(", "));
        }
        println!("\n### Pairs\n\n| earlier | later | relation | point | source | evidence |\n|---|---|---|---|---|---|");
        for s in &r.pairs {
            println!("| {} (L{}) | {} (L{}) | {} | {} | {} | {} |", s.earlier, s.earlier_line, s.later, s.later_line, s.relation, s.point, s.source, short(&s.evidence, 140).replace('|', "/"));
            rows.push(format!("{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}", s.log, s.earlier, s.later, s.relation, s.point, s.evidence.replace(['\t', '\n'], " "), s.earlier_line, s.later_line, s.source));
        }
        println!();
    }
    if reports.is_empty() {
        println!("No decisions log with dated entries in `{kb}`.");
    }
    if !dry {
        let out = out_root(parent);
        std::fs::create_dir_all(&out).map_err(|e| Error::io(&out, e))?;
        let path = out.join("supersede.tsv");
        std::fs::write(&path, rows.join("\n") + "\n").map_err(|e| Error::io(&path, e))?;
        println!("written: `{}` · {:.0}s", path.display(), t.elapsed().as_secs_f64());
    }
    Ok(())
}

/// A file through the team wiki's inbox as the app runs it: copied into
/// Research/Ingestion/Raw/, read into a dated note written against what the
/// wiki says, then what follows from it proposed (pages, new pages, rulings,
/// tickets). Prints the takeaways and every proposal.
fn phase_ingest(base: &Path, parent: &Path, file: &str) -> Result<()> {
    let wiki = Project::open(&parent.join(wiki_name()))?;
    let mut db = Db::open(base, wiki.config.id)?;
    let from = Path::new(file);
    let name = from.file_name().ok_or_else(|| Error::Other("no file name".into()))?.to_string_lossy().to_string();
    let raw = format!("{}/{name}", ken_core::ingest::RAW);
    std::fs::create_dir_all(wiki.root.join(ken_core::ingest::RAW)).map_err(|e| Error::Other(e.to_string()))?;
    std::fs::copy(from, wiki.root.join(&raw)).map_err(|e| Error::Other(format!("{file}: {e}")))?;
    scan::scan(&wiki, &mut db)?;
    println!("# Ingest `{name}` into {}\n", wiki_name());
    let context = ken_core::ingest::wiki_context(&db, &ken_core::extract::extract(&wiki.root.join(&raw))?.text);
    let pages: Vec<&str> = context.lines().filter_map(|l| l.strip_prefix("=== ").and_then(|l| l.strip_suffix(" ==="))).collect();
    println!("wiki passages given to the note: {pages:?}\n");
    let t = Instant::now();
    let placement = ken_core::ingest::ingest_one(&wiki.root, &mut db, &raw, &today(), engine::now_epoch(), claude(&wiki.root)?)?;
    let note = std::fs::read_to_string(wiki.root.join(&placement.note)).map_err(|e| Error::Other(e.to_string()))?;
    println!("note: `{}` ({:.0}s)\n", placement.note, t.elapsed().as_secs_f64());
    let tk = ken_core::ingest::takeaways(&note);
    println!("## Takeaways\n\n**{}** · {} · {:?}\n\n### Overturns\n{}\n\n### Contradictions\n{}\n\n### Rulings\n{}\n\n### Actions\n{}\n",
        tk.title, tk.kind, tk.present, tk.overturns,
        tk.contradictions.iter().map(|c| format!("- {c}")).collect::<Vec<_>>().join("\n"),
        tk.rulings.iter().map(|c| format!("- {c}")).collect::<Vec<_>>().join("\n"),
        tk.actions.iter().map(|c| format!("- {c}")).collect::<Vec<_>>().join("\n"));
    let t = Instant::now();
    let me = ken_core::day::git_me();
    let ws = parent.join(ken_core::workspace::CONFIG_DIR);
    let targets = ken_core::ingest::Targets { team_repo: None, workspace: Some(&ws), me: &me, stamp: "" };
    let fu = ken_core::ingest::follow_ups(&wiki.root, &mut db, &placement, &note, &today(), engine::now_epoch(), &targets, claude(&wiki.root)?)?;
    println!("## Written ({:.0}s)\n", t.elapsed().as_secs_f64());
    for w in &fu.written {
        println!("- {:?} `{}`: {}", w.kind, w.path, short(&w.label, 120));
    }
    println!("\nheld: {:?}\nrulings waiting: {}\ntickets: {:?}\nlisted on the card: {}\n", fu.held, fu.rulings, fu.tickets, fu.listed.len());
    for p in ken_core::ingest::proposals_from(&db, &placement.note)? {
        println!("- **{}**: {}", p.title, short(&p.body, 200));
    }
    Ok(())
}

/// The ingest pass as the app runs it (`ingest::run_pass`), over whatever
/// waits in the team wiki's Raw/: a recording transcribed first with the
/// installed Whisper model, a source that fails recorded and passed over,
/// and a CLI failure stopping the pass. Prints what each source came to, so
/// a bad first file that blocks the rest shows here.
fn phase_pass(base: &Path, parent: &Path) -> Result<()> {
    let wiki = Project::open(&parent.join(wiki_name()))?;
    let mut db = Db::open(base, wiki.config.id)?;
    scan::scan(&wiki, &mut db)?;
    let waiting = ken_core::ingest::waiting_new(&wiki.root, &db)?;
    println!("# Ingest pass over {}\n\n{} waiting: {:?}\n", wiki_name(), waiting.len(), waiting);
    let me = ken_core::day::git_me();
    let ws = parent.join(ken_core::workspace::CONFIG_DIR);
    let targets = ken_core::ingest::Targets { team_repo: None, workspace: Some(&ws), me: &me, stamp: "" };
    let model = ken_core::model::selected_model_path(base, ken_core::model::ModelCategory::Transcription);
    let ffmpeg = ken_core::transcript::discover_ffmpeg();
    let transcriber = ken_core::ingest::Transcriber { model: model.as_deref(), ffmpeg: ffmpeg.as_deref() };
    let bin = ken_core::runner::discover_claude().ok_or_else(|| Error::Other("claude CLI not found".into()))?;
    let root = wiki.root.clone();
    let call = |prompt: &str| {
        let t = Instant::now();
        let out = ken_core::ingest::call_result(assistant::oneshot(&bin, &root, prompt, Duration::from_secs(600), &CancelToken::new()));
        eprintln!("  claude: {} chars in, {:.0}s, {}", prompt.len(), t.elapsed().as_secs_f64(), if out.is_ok() { "ok" } else { "failed" });
        out
    };
    let on_phase: std::sync::Arc<dyn Fn(&str, ken_core::ingest::PassPhase) + Send + Sync> =
        std::sync::Arc::new(|raw, phase| eprintln!("  {raw}: {phase:?}"));
    let t = Instant::now();
    let report = ken_core::ingest::run_pass(&wiki.root, &mut db, &targets, &today(), engine::now_epoch, transcriber, call, &on_phase)?;
    println!("| source | came to |\n|---|---|");
    for p in &report.read {
        println!("| {} | read: `{}` |", p.raw, p.note);
    }
    for (raw, why) in &report.failed {
        println!("| {raw} | failed: {} |", short(why, 160));
    }
    for (raw, why) in &report.waiting {
        println!("| {raw} | waiting: {} |", short(why, 160));
    }
    if let Some(why) = &report.stopped {
        println!("\n**Stopped**: {}", short(why, 300));
    }
    println!(
        "\n{} read, {} failed, {} waiting in {:.0}s",
        report.read.len(),
        report.failed.len(),
        report.waiting.len(),
        t.elapsed().as_secs_f64()
    );
    Ok(())
}

/// Transcription on this machine: decode one recording's audio (in-process,
/// else ffmpeg) and run the installed Whisper model on it. Prints how long
/// each step took and the first lines heard.
fn phase_transcribe(base: &Path, file: &str) -> Result<()> {
    let path = Path::new(file);
    let model = ken_core::model::selected_model_path(base, ken_core::model::ModelCategory::Transcription)
        .ok_or_else(|| Error::Other("no transcription model under KEN_DATA_DIR/whisper".into()))?;
    let ffmpeg = ken_core::transcript::discover_ffmpeg();
    println!(
        "# Transcribe `{file}`\n\nmodel `{}`, ffmpeg {}, decoded in process: {}\n",
        model.display(),
        ffmpeg.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "not found".into()),
        ken_core::transcript::decodes_in_process(path)
    );
    let t = Instant::now();
    let samples = ken_core::transcript::decode_audio(path, ffmpeg.as_deref())?;
    println!("- audio: {:.1} s of 16 kHz samples, decoded in {:.1}s", samples.len() as f64 / 16_000.0, t.elapsed().as_secs_f64());
    let t = Instant::now();
    let cues = ken_core::transcript::transcribe(&model, &samples)?;
    println!("- transcript: {} cues in {:.1}s\n", cues.len(), t.elapsed().as_secs_f64());
    for c in cues.iter().take(12) {
        println!("    [{}] {}", ken_core::transcript::format_timestamp(c.start), short(&c.text, 120));
    }
    Ok(())
}

/// Wiki sync as the app runs it, against a bare remote in the evaluation's
/// own folder (`<parent>/../remotes/<member>.git`), never a real one: commit
/// all and push, then a teammate's pushed change pulled back.
fn phase_sync(base: &Path, parent: &Path, member: &str) -> Result<()> {
    use ken_core::sync;
    let p = Project::open(&parent.join(member))?;
    let kind = Registry::load(base)?.entry_at(&p.root).map(|e| e.kind.clone()).unwrap_or_default();
    let area = parent.parent().unwrap_or(parent);
    let remote = area.join("remotes").join(format!("{member}.git"));
    let git = |dir: &Path, args: &[&str]| -> std::result::Result<String, String> {
        let o = std::process::Command::new("git")
            .args(["-c", "user.name=Ken eval", "-c", "user.email=ken-eval@example.invalid"])
            .args(args)
            .current_dir(dir)
            .output()
            .map_err(|e| e.to_string())?;
        if o.status.success() { Ok(String::from_utf8_lossy(&o.stdout).trim().to_string()) } else { Err(String::from_utf8_lossy(&o.stderr).trim().to_string()) }
    };
    let (origin, branch) = sync::remote_and_branch(&p.root);
    if origin.is_none() {
        std::fs::create_dir_all(remote.parent().unwrap()).map_err(|e| Error::Other(e.to_string()))?;
        git(area, &["init", "-q", "--bare", &remote.to_string_lossy()]).map_err(Error::Other)?;
        git(&p.root, &["remote", "add", "origin", &remote.to_string_lossy()]).map_err(Error::Other)?;
    }
    let url = git(&p.root, &["remote", "get-url", "origin"]).map_err(Error::Other)?.replace('\\', "/");
    if !url.to_lowercase().starts_with(&area.to_string_lossy().replace('\\', "/").to_lowercase()) {
        return Err(Error::Other(format!("origin `{url}` is outside the evaluation folder; not pushing")));
    }
    let branch = branch.unwrap_or_else(|| "main".into());
    println!("# Sync `{member}`\n\nsync on by default for its kind {kind:?}: {}\nremote: `{url}` branch `{branch}`\n", sync::sync_auto(&p, &kind));
    sync::ensure_excludes(&p.root)?;
    // First push sets the upstream, as a person does once.
    let committed = sync::commit_all(&p.root)?;
    let first = git(&p.root, &["push", "-q", "-u", "origin", &branch]);
    println!("commit: {committed} · first push: {first:?}");
    let tracked = git(&remote, &["ls-tree", "-r", "--name-only", &branch]).unwrap_or_default();
    println!(
        "on the remote: {} files · .ken/project.json {} · staging {}",
        tracked.lines().count(),
        if tracked.lines().any(|l| l == ".ken/project.json") { "committed" } else { "MISSING" },
        if tracked.lines().any(|l| l.starts_with(".ken/.staging")) { "LEAKED" } else { "kept out" }
    );

    // A local edit, synced.
    let page = p.root.join("Current/Project.md");
    let mut text = std::fs::read_to_string(&page).map_err(|e| Error::Other(e.to_string()))?;
    text.push_str("\nNote from the evaluation's sync step.\n");
    std::fs::write(&page, &text).map_err(|e| Error::Other(e.to_string()))?;
    let committed = sync::commit_all(&p.root)?;
    println!("local edit: committed {committed}, push {:?}", sync::push(&p.root)?.eq_name());
    let subject = git(&remote, &["log", "-1", "--format=%s", &branch]).unwrap_or_default();
    println!("remote's newest commit: {subject:?}");

    // A teammate's change, pulled.
    let mate = area.join("remotes").join(format!("{member}-teammate"));
    let _ = std::fs::remove_dir_all(&mate);
    git(area, &["clone", "-q", &remote.to_string_lossy(), &mate.to_string_lossy()]).map_err(Error::Other)?;
    std::fs::write(mate.join("Current/Teammate.md"), "# Teammate\n\nAdded on another machine.\n").map_err(|e| Error::Other(e.to_string()))?;
    git(&mate, &["add", "-A"]).map_err(Error::Other)?;
    git(&mate, &["commit", "-q", "-m", "a teammate's page"]).map_err(Error::Other)?;
    git(&mate, &["push", "-q"]).map_err(Error::Other)?;
    let pulled = sync::pull(&p.root, |_| false)?;
    println!("pull: {} · teammate's page here: {}", match pulled {
        sync::PullOutcome::Clean => "clean".to_string(),
        sync::PullOutcome::Conflicts(c) => format!("{} conflicts", c.len()),
        sync::PullOutcome::Failed(e) => format!("failed: {e}"),
    }, p.root.join("Current/Teammate.md").exists());
    Ok(())
}

trait PushName {
    fn eq_name(&self) -> String;
}
impl PushName for ken_core::sync::PushOutcome {
    fn eq_name(&self) -> String {
        match self {
            ken_core::sync::PushOutcome::Pushed => "pushed".into(),
            ken_core::sync::PushOutcome::NoRemote => "no remote".into(),
            ken_core::sync::PushOutcome::Failed(e) => format!("failed: {e}"),
        }
    }
}

/// A repo joins the team later: `folder` (beside the others) is ticked at
/// set-up, confirmed onto the team, indexed, and the wiki gets its Repo Map
/// page drafted and proposed changes for the pages a person keeps.
fn phase_add_repo(base: &Path, parent: &Path, folder: &str) -> Result<()> {
    let prop = setup::propose(parent)?;
    let mut rows = prop.rows.clone();
    let Some(row) = rows.iter_mut().find(|r| r.member == folder) else {
        return Err(Error::Other(format!("{folder} is not a candidate beside the others")));
    };
    row.include = true;
    row.team = Some(team());
    println!("# `{folder}` joins the team\n\nproposed as {:?} {:?}", row.kind, row.index);
    setup::confirm(base, parent, &team(), &rows, &prop.ignores, &today())?;
    let added = Project::open(&parent.join(folder))?;
    let mut adb = Db::open(base, added.config.id)?;
    let stats = scan::scan(&added, &mut adb)?;
    println!("indexed: {} files", stats.added + stats.updated);

    let wiki = Project::open(&parent.join(wiki_name()))?;
    let all = wikidraft::team_repos(&wiki_name(), &team_members(base, parent)?);
    println!("team repos now: {:?}\n", all.iter().map(|r| &r.0).collect::<Vec<_>>());
    let mut db = Db::open(base, wiki.config.id)?;
    let report = wikidraft::draft_added(
        &wiki.root,
        &wiki_name(),
        &mut db,
        &[(folder.to_string(), added.root.clone())],
        &all,
        &today(),
        engine::now_epoch(),
        claude(&wiki.root)?,
    )?;
    println!("- drafted: {:?}\n- proposed: {:?}\n- kept (no change): {:?}\n- failed: {:?}", report.drafted, report.proposed, report.kept, report.failed);
    let conn = rusqlite::Connection::open(ken_core::db::db_path(base, wiki.config.id)).map_err(|e| Error::Other(e.to_string()))?;
    let mut stmt = conn
        .prepare("SELECT kind, title, body FROM review_items WHERE status != 'resolved' ORDER BY id DESC LIMIT 6")
        .map_err(|e| Error::Other(e.to_string()))?;
    let cards: Vec<(String, String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .map_err(|e| Error::Other(e.to_string()))?
        .flatten()
        .collect();
    for (kind, title, body) in cards {
        println!("\n### card ({kind}): {title}\n\n{}", short(&body, 700));
    }
    Ok(())
}

/// Each member's knowledge map built by Claude, as the app does now: the
/// members whose files are read for entities (search-only repos have none).
fn phase_map(base: &Path, parent: &Path) -> Result<()> {
    let binary = ken_core::runner::discover_claude().ok_or_else(|| Error::Other("Claude Code not found".into()))?;
    println!("# Knowledge map (Claude)\n\n| member | files read | entities | edges | still waiting | seconds |\n|---|---|---|---|---|---|");
    for (name, p) in members(parent)? {
        let mut db = Db::open(base, p.config.id)?;
        let files = db.entity_tier_paths()?.len();
        if files == 0 {
            continue;
        }
        let t = Instant::now();
        match knowledge_model::build_knowledge_model(&binary, &p, &mut db, &today(), &CancelToken::new()) {
            Ok(_) => {
                let (ents, edges) = db.list_entities_with_edges()?;
                let waiting = db.waiting_extractions()?.len();
                println!("| {name} | {files} | {} | {} | {waiting} | {:.0} |", ents.len(), edges.len(), t.elapsed().as_secs_f64());
            }
            Err(e) => println!("| {name} | {files} | failed: {e} | | | {:.0} |", t.elapsed().as_secs_f64()),
        }
    }
    Ok(())
}

fn phase_extract(base: &Path, parent: &Path, minutes: u64) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(minutes * 60);
    let generate = |p: &str| local_llm::generate_json(p, Priority::Background);
    println!("# Entity extraction (local model, budget {minutes} min)\n\n| member | done / extractable | entities | edges | seconds per file |\n|---|---|---|---|---|");
    // The team's roster, so a teammate named like a bot is kept, as in the app.
    let roots: Vec<PathBuf> = members(parent)?.into_iter().map(|(_, p)| p.root).collect();
    let roster = ken_core::people::roster_of(roots.iter().map(PathBuf::as_path));
    for (name, p) in members(parent)? {
        let mut db = Db::open(base, p.config.id)?;
        if db.extractable_file_count()? == 0 {
            continue;
        }
        let _ = db.requeue_errored_extractions();
        let mut files = 0;
        let t = Instant::now();
        while Instant::now() < deadline {
            match knowledge_model::process_next_pending_for(&mut db, &today(), engine::now_epoch(), &generate, "", &roster) {
                Ok(Some(path)) => {
                    files += 1;
                    eprintln!("  {name}: {path} ({:.0}s avg)", t.elapsed().as_secs_f64() / files as f64);
                }
                Ok(None) => break,
                Err(e) => {
                    eprintln!("  {name}: error {e}");
                    files += 1;
                }
            }
        }
        let (done, total) = db.extraction_coverage()?;
        let (ents, edges) = db.list_entities_with_edges()?;
        let per = if files > 0 { t.elapsed().as_secs_f64() / files as f64 } else { 0.0 };
        println!("| {} | {} / {} | {} | {} | {:.1} |", name, done, total, ents.len(), edges.len(), per);
    }
    Ok(())
}

// ---------------------------------------------------------------- kg

/// As the app does it: the batched judgements (same thing? how related?)
/// on Claude; no local generation.
struct LocalLlm {
    claude: Option<PathBuf>,
    root: PathBuf,
}

impl FederationLlm for LocalLlm {
    // No local generation (the app's policy): summaries fall back to the
    // longest member summary.
    fn complete(&self, _prompt: &str) -> Result<String> {
        Err(Error::Other("no local generation".into()))
    }

    fn judge(&self, prompt: &str) -> Result<String> {
        let Some(bin) = &self.claude else { return self.complete(prompt) };
        match assistant::oneshot(bin, &self.root, prompt, Duration::from_secs(300), &CancelToken::new())? {
            assistant::OneshotOutcome::Completed(text) => Ok(text),
            other => Err(Error::Other(format!("claude: {other:?}"))),
        }
    }
}

fn kg_members(base: &Path, parent: &Path) -> Result<Vec<(String, uuid::Uuid, Db)>> {
    let reg = Registry::load(base)?;
    let mut out: Vec<(String, uuid::Uuid, Db)> = members(parent)?
        .into_iter()
        .filter(|(_, p)| reg.entry_at(&p.root).is_some_and(|e| e.index.unwrap_or_else(|| IndexState::for_kind(&e.kind)) == IndexState::Entities))
        .filter_map(|(n, p)| Db::open_read_only(base, p.config.id).ok().map(|db| (n, p.config.id, db)))
        .collect();
    out.sort_by_key(|m| m.1);
    Ok(out)
}

fn phase_kg(base: &Path, parent: &Path) -> Result<()> {
    let ms = kg_members(base, parent)?;
    let fed: Vec<federation::Member> = ms.iter().map(|(_, id, db)| federation::Member { project_id: *id, db }).collect();
    let mut kg = WorkspaceKgDb::open(parent)?;
    let llm = LocalLlm { claude: ken_core::runner::discover_claude(), root: parent.to_path_buf() };
    let roots: Vec<PathBuf> = members(parent)?.into_iter().map(|(_, p)| p.root).collect();
    let roster = ken_core::people::roster_of(roots.iter().map(PathBuf::as_path));
    let rep = federation::build_workspace_kg(&mut kg, &fed, &roster, Some(&llm), engine::now_epoch(), &CancelToken::new())?;
    let names: HashMap<String, &str> = ms.iter().map(|(n, id, _)| (id.to_string(), n.as_str())).collect();
    println!("# Workspace knowledge graph\n\nMembers: {:?}\n\n{rep:#?}\n", ms.iter().map(|m| &m.0).collect::<Vec<_>>());

    // Clusters: global entities that merged local entities from more than
    // one member, or several local names into one.
    let mut merged = Vec::new();
    let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();
    for g in kg.list_global_entities()? {
        *by_kind.entry(g.kind.clone()).or_default() += 1;
        let links = kg.list_links_for_global(g.id)?;
        let projects: std::collections::BTreeSet<_> = links.iter().map(|l| l.project_id.clone()).collect();
        let locals: std::collections::BTreeSet<_> = links.iter().map(|l| l.local_name.clone()).collect();
        if projects.len() > 1 || locals.len() > 1 {
            merged.push((g, links, projects.len()));
        }
    }
    println!("Global entities by kind: {by_kind:?}\n\n## Merged clusters ({})\n", merged.len());
    merged.sort_by_key(|m| std::cmp::Reverse(m.1.len()));
    for (g, links, np) in merged.iter().take(40) {
        let locals: Vec<String> = links
            .iter()
            .map(|l| format!("{} ({})", l.local_name, names.get(&l.project_id).copied().unwrap_or("?")))
            .collect();
        println!("- **{}** [{}] across {np} members ← {}", g.name, g.kind, locals.join(", "));
    }
    Ok(())
}

// ---------------------------------------------------------------- ask

/// `questions.tsv`: `question<TAB>expected path fragment[|another]`.
/// "Ask Ken to look" on each question (or the numbers in `KEN_EVAL_ONLY`,
/// e.g. `9,10,15`): the read-only fallback searches the members itself.
/// Found when a source it cites is an expected file.
fn phase_look(base: &Path, parent: &Path, questions: &Path) -> Result<()> {
    let text = std::fs::read_to_string(questions).map_err(|e| Error::Other(format!("{}: {e}", questions.display())))?;
    let only: Vec<usize> =
        std::env::var("KEN_EVAL_ONLY").unwrap_or_default().split(',').filter_map(|n| n.trim().parse().ok()).collect();
    let binary = ken_core::runner::discover_claude().ok_or_else(|| Error::Other("Claude Code not found".into()))?;
    let folders: Vec<(String, uuid::Uuid, PathBuf)> =
        members(parent)?.into_iter().map(|(n, p)| (n, p.config.id, p.root.clone())).collect();
    let dirs: Vec<PathBuf> = folders.iter().map(|(_, _, r)| r.clone()).collect();
    // The leads the app hands the look: what routed search ranked.
    let mut emb = ken_core::embedder::installed_embedding_model();
    let kg = WorkspaceKgDb::open(parent).ok();
    let ms: Vec<(String, Project, Db)> = members(parent)?
        .into_iter()
        .filter_map(|(n, p)| Db::open_read_only(base, p.config.id).ok().map(|db| (n, p, db)))
        .collect();
    let info: Vec<MemberInfo> = ms
        .iter()
        .map(|(n, p, db)| MemberInfo { project_id: p.config.id, name: n.clone(), index_ready: db.vec_available(), last_activity: 0, knowledge_base: ken_core::registry::entry_of(p.config.id).is_some_and(|(_, k)| ken_core::routing::is_knowledge_base(&k)) })
        .collect();
    let (mut asked, mut found, mut named_only, mut not_there) = (0, 0, 0, 0);
    println!("# Ask Ken to look\n");
    for (qi, line) in text.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')).enumerate() {
        let n = qi + 1;
        if !only.is_empty() && !only.contains(&n) {
            continue;
        }
        let (q, expect) = line.split_once('\t').unwrap_or((line, ""));
        let expects: Vec<String> = expect.split('|').filter(|e| !e.is_empty()).map(|e| e.to_lowercase()).collect();
        let t = Instant::now();
        let plan = routing::plan_route(q, &info, kg.as_ref());
        let query_vec = emb.as_mut().and_then(|e| e.embed_query(q).ok());
        let member_hits: Vec<routing::MemberHits> = plan
            .targets
            .iter()
            .filter_map(|id| ms.iter().find(|(_, p, _)| p.config.id == *id))
            .map(|(n, p, db)| routing::MemberHits {
                project_id: p.config.id,
                member_name: n.clone(),
                status: routing::MemberStatus::Searched,
                hits: routing::search_member(db, q, query_vec.as_deref(), 8).unwrap_or_default(),
            })
            .collect();
        let leads: Vec<String> =
            routing::merge_routed(&plan, &member_hits, 8).results.iter().map(|r| format!("{}/{}", r.member_name, r.path)).collect();
        let prompt = assistant::look_prompt(q, &folders, &leads);
        let outcome = assistant::look(&binary, &parent.join(&folders[0].0), &dirs, &prompt, Duration::from_secs(300), &CancelToken::new())?;
        let answer = match outcome {
            assistant::OneshotOutcome::Completed(t) => t,
            other => format!("(no answer: {other:?})"),
        };
        let parsed = ken_core::digest::parse_digest(&answer);
        // A cited ken://<id>/<path> as `member/path`, to match against.
        let cited: Vec<String> = parsed
            .sources
            .iter()
            .map(|s| {
                let rest = s.trim_start_matches("ken://");
                let (id, path) = rest.split_once('/').unwrap_or((rest, ""));
                let name = folders.iter().find(|(_, fid, _)| fid.to_string() == id).map_or(id, |(n, _, _)| n.as_str());
                format!("{name}/{}", path.split('#').next().unwrap_or(path))
            })
            .collect();
        let hit = cited.iter().any(|c| expects.iter().any(|e| c.to_lowercase().contains(e)));
        // Right file named in the answer, but not opened and cited.
        let named = !hit && expects.iter().any(|e| parsed.body.to_lowercase().contains(e));
        let says_missing = parsed.body.to_lowercase().contains("not there") || parsed.body.to_lowercase().contains("could not find");
        asked += 1;
        found += usize::from(hit);
        named_only += usize::from(named);
        not_there += usize::from(says_missing && !hit);
        println!(
            "## Q{n}. {q}\n\n{} · {:.0}s · expected `{expect}`\n\n{}\n\ncited: {cited:?}\n",
            if hit {
                "FOUND"
            } else if named {
                "NAMED, NOT CITED"
            } else if says_missing {
                "SAID NOT THERE"
            } else {
                "MISSED"
            },
            t.elapsed().as_secs_f64(),
            short(&parsed.body, 600)
        );
    }
    println!(
        "## Score\n\n{found} of {asked} cited an expected file; {named_only} more named it without citing it; {not_there} said it was not there."
    );
    Ok(())
}

fn phase_ask(base: &Path, parent: &Path, questions: &Path) -> Result<()> {
    let text = std::fs::read_to_string(questions).map_err(|e| Error::Other(format!("{}: {e}", questions.display())))?;
    let mut emb = ken_core::embedder::installed_embedding_model();
    let kg = WorkspaceKgDb::open(parent).ok();
    let ms: Vec<(String, Project, Db)> = members(parent)?
        .into_iter()
        .filter_map(|(n, p)| Db::open_read_only(base, p.config.id).ok().map(|db| (n, p, db)))
        .collect();
    let info: Vec<MemberInfo> = ms
        .iter()
        .map(|(n, p, db)| MemberInfo { project_id: p.config.id, name: n.clone(), index_ready: db.vec_available(), last_activity: 0, knowledge_base: ken_core::registry::entry_of(p.config.id).is_some_and(|(_, k)| ken_core::routing::is_knowledge_base(&k)) })
        .collect();
    let handles: Vec<MemberDbHandle> = ms
        .iter()
        .map(|(n, p, db)| MemberDbHandle { project_id: p.config.id, name: n, db, index_ready: db.vec_available() })
        .collect();

    let mut scores = (0usize, 0usize, 0usize, 0usize, 0usize);
    println!("# Curated questions\n");
    for (qi, line) in text.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')).enumerate() {
        let (q, expect) = line.split_once('\t').unwrap_or((line, ""));
        let expects: Vec<&str> = expect.split('|').filter(|e| !e.is_empty()).collect();
        let plan = routing::plan_route(q, &info, kg.as_ref());
        let targets: Vec<&str> = plan.targets.iter().filter_map(|id| info.iter().find(|m| m.project_id == *id)).map(|m| m.name.as_str()).collect();

        // Hybrid (as the app with meaning search on) and keyword-only.
        let t = Instant::now();
        // As the app's routed search: each target searched (kept to the kinds
        // in KEN_EVAL_TYPES, if set), then merged by relevance.
        let types = ken_core::contenttype::parse_filter(std::env::var("KEN_EVAL_TYPES").ok().as_deref());
        let query_vec = emb.as_mut().and_then(|e| e.embed_query(q).ok());
        let shared = routing::workspace_vocabulary(handles.iter().map(|h| h.db));
        let search = |qv: Option<&[f32]>| -> Vec<routing::MemberHits> {
            plan.targets
                .iter()
                .filter_map(|id| handles.iter().find(|h| h.project_id == *id))
                .map(|h| routing::MemberHits {
                    project_id: h.project_id,
                    member_name: h.name.to_string(),
                    status: routing::MemberStatus::Searched,
                    hits: routing::search_member_of_with(h.db, q, qv, 8, &types, Some(&shared)).unwrap_or_default(),
                })
                .collect()
        };
        let hybrid_members = search(query_vec.as_deref());
        let hybrid = routing::merge_routed(&plan, &hybrid_members, 8);
        let ms_h = t.elapsed().as_millis();
        // Keyword only, merged across repos the same way as hybrid, so the two
        // columns compare like with like.
        let keyword_members = search(None);
        let keyword = routing::merge_routed(&plan, &keyword_members, 8);
        let expect_list: Vec<String> = expects.iter().map(|e| e.to_string()).collect();
        dump_hits("hybrid", qi + 1, &expect_list, &plan, &hybrid_members);
        dump_hits("keyword", qi + 1, &expect_list, &plan, &keyword_members);

        let rank_of = |paths: &[String]| paths.iter().position(|p| expects.iter().any(|e| p.contains(e)));
        let hybrid_paths: Vec<String> = hybrid.results.iter().map(|r| format!("{}/{}", r.member_name, r.path)).collect();
        let kw_paths: Vec<String> = keyword.results.iter().map(|r| format!("{}/{}", r.member_name, r.path)).collect();
        let hr = rank_of(&hybrid_paths);
        let kr = rank_of(&kw_paths);
        scores.0 += 1;
        if hr == Some(0) {
            scores.1 += 1;
        }
        if hr.is_some_and(|r| r < 5) {
            scores.2 += 1;
        }
        if kr == Some(0) {
            scores.3 += 1;
        }
        if kr.is_some_and(|r| r < 5) {
            scores.4 += 1;
        }
        println!(
            "## Q{}. {q}\n\nRoute: {:?} → {:?} · expected `{expect}` · hybrid rank {} · keyword rank {} · {ms_h} ms\n",
            qi + 1,
            match &plan.reason {
                routing::RouteReason::KgEntities(ids) => format!("graph ({} entities)", ids.len()),
                other => format!("{other:?}"),
            },
            targets,
            hr.map_or("—".to_string(), |r| (r + 1).to_string()),
            kr.map_or("—".to_string(), |r| (r + 1).to_string()),
        );
        for (i, r) in hybrid.results.iter().take(6).enumerate() {
            println!("{}. `{}/{}`{} ({:?}) — {}", i + 1, r.member_name, r.path, r.line.map(|l| format!(":{l}")).unwrap_or_default(), r.source, short(&r.snippet, 110));
        }
        println!();
        // KEN_EVAL_VERBOSE: the keyword list too, and each member's own hits
        // with the score they merged by, to see why a file ranks where it does.
        if std::env::var_os("KEN_EVAL_VERBOSE").is_some() {
            println!("Keyword:\n");
            for (i, r) in keyword.results.iter().take(6).enumerate() {
                println!("{}. `{}/{}`{} ({:?})", i + 1, r.member_name, r.path, r.line.map(|l| format!(":{l}")).unwrap_or_default(), r.source);
            }
            for (label, mh) in [("hybrid", &hybrid_members), ("keyword", &keyword_members)] {
                println!("\nPer member, {label}:\n");
                for m in mh {
                    let row: Vec<String> = m.hits.iter().map(|h| format!("{:.2} {} ({}c)", h.score, h.path, h.snippet.len())).collect();
                    println!("- {}: {}", m.member_name, row.join(" · "));
                }
            }
            println!();
        }
    }
    let (n, h1, h5, k1, k5) = scores;
    println!("## Score over {n} questions\n\n| | hit@1 | hit@5 |\n|---|---|---|\n| hybrid (keyword + meaning, routed) | {h1} | {h5} |\n| keyword only | {k1} | {k5} |");
    Ok(())
}

struct MemberHitsLite {
    member: String,
    paths: Vec<String>,
}

// ---------------------------------------------------------------- drift

fn print_drift(label: &str, run: &drift::DriftRun, took: Duration) {
    println!(
        "### {label}\n\nexit {} · pages {} · code citations {} · cross-refs {} · business pages {} · measured {} · reused {} · {:.1}s",
        run.exit_code,
        run.pages_examined,
        run.code_citations,
        run.cross_references,
        run.business_pages,
        run.measured,
        run.reused,
        took.as_secs_f64()
    );
    if let Some(v) = &run.void_reason {
        println!("\nvoid: {v}");
    }
    println!("\nbranches: {:?}\n\nunmeasured: {:?}\n", run.branches, run.unmeasured);
    for m in &run.mismatches {
        println!("- {:?} **{}** `{}` — {}", m.severity, m.subject, m.citation, short(&m.detail, 140));
    }
    for (p, v) in &run.aged {
        println!("- aged **{p}** verified {v:?}");
    }
    println!();
}

fn phase_drift(base: &Path, parent: &Path) -> Result<()> {
    let wiki = Project::open(&parent.join(wiki_name()))?;
    let mut db = Db::open(base, wiki.config.id)?;
    scan::scan(&wiki, &mut db)?;
    println!("# Drift\n");
    for label in ["first sweep", "second sweep, nothing changed"] {
        let t = Instant::now();
        let run = drift::sweep(&wiki, &db, engine::now_epoch())?;
        print_drift(label, &run, t.elapsed());
    }
    Ok(())
}

/// Change `file` in `member` (append a line, commit), then sweep again.
fn phase_drift_change(base: &Path, parent: &Path, member: &str, file: &str) -> Result<()> {
    let repo = parent.join(member);
    let path = repo.join(file);
    let mut text = std::fs::read_to_string(&path).map_err(|e| Error::Other(format!("{}: {e}", path.display())))?;
    // A real change, not a comment (drift clears comment-only changes).
    text.push_str(if file.ends_with(".md") {
        "\nDeploys now need two approvals (changed by the Ken evaluation run).\n"
    } else {
        "\nKEN_EVAL_CHANGE = 1\n"
    });
    std::fs::write(&path, text).map_err(|e| Error::Other(e.to_string()))?;
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["-c", "user.name=Ken eval", "-c", "user.email=ken-eval@example.invalid"])
            .args(args)
            .output()
    };
    let _ = git(&["add", file]);
    let c = git(&["commit", "-q", "-m", "eval: change a cited file"]).map_err(|e| Error::Other(e.to_string()))?;
    println!("# Drift after changing `{member}/{file}`\n\ncommit: {}\n", String::from_utf8_lossy(&c.stderr).trim());
    // Drift measures what is merged upstream, so the change is pushed, as a
    // teammate's merge would land. Only to a remote that is a folder in the
    // evaluation's own area, never to a real one.
    let origin = git(&["remote", "get-url", "origin"]).map(|o| String::from_utf8_lossy(&o.stdout).trim().replace('\\', "/")).unwrap_or_default();
    let area = parent.parent().unwrap_or(parent).to_string_lossy().replace('\\', "/");
    if !origin.is_empty() && origin.to_lowercase().starts_with(&area.to_lowercase()) {
        let p = git(&["push", "-q", "origin", "HEAD"]).map_err(|e| Error::Other(e.to_string()))?;
        println!("pushed to `{origin}`: {}\n", if p.status.success() { "ok".into() } else { String::from_utf8_lossy(&p.stderr).trim().to_string() });
    } else {
        println!("not pushed: `{origin}` is outside `{area}`, so drift will not see this change until it is merged there\n");
    }
    let wiki = Project::open(&parent.join(wiki_name()))?;
    let db = Db::open(base, wiki.config.id)?;
    let t = Instant::now();
    let run = drift::sweep(&wiki, &db, engine::now_epoch())?;
    print_drift("sweep after the change", &run, t.elapsed());
    Ok(())
}

// ---------------------------------------------------------------- ignore

fn phase_ignore(base: &Path, parent: &Path, member: &str) -> Result<()> {
    let p = Project::open(&parent.join(member))?;
    let mut db = Db::open(base, p.config.id)?;
    let before = tier_counts(base, p.config.id);
    let files_before = db.file_count()?;
    // Pick the two biggest top-level folders to demote.
    let conn = rusqlite::Connection::open(ken_core::db::db_path(base, p.config.id)).map_err(|e| Error::Other(e.to_string()))?;
    let mut stmt = conn
        .prepare("SELECT substr(rel_path, 1, instr(rel_path, '/') - 1) AS top, COUNT(*) c FROM files WHERE instr(rel_path, '/') > 0 GROUP BY top ORDER BY c DESC LIMIT 2")
        .map_err(|e| Error::Other(e.to_string()))?;
    let tops: Vec<(String, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(|e| Error::Other(e.to_string()))?
        .filter_map(|r| r.ok())
        .collect();
    let (ignore, search) = (&tops[0], &tops[1]);
    let ki = p.root.join(".kenignore");
    let old = std::fs::read_to_string(&ki).ok();
    let body = format!("# Ken evaluation\n{}/\n~{}/\n", ignore.0, search.0);
    std::fs::write(&ki, &body).map_err(|e| Error::Other(e.to_string()))?;
    let t = Instant::now();
    let stats = scan::scan(&p, &mut db)?;
    let after = tier_counts(base, p.config.id);
    let conn2 = rusqlite::Connection::open(ken_core::db::db_path(base, p.config.id)).map_err(|e| Error::Other(e.to_string()))?;
    let mut left = conn2
        .prepare("SELECT rel_path, status FROM files WHERE rel_path LIKE ?1 ORDER BY rel_path LIMIT 8")
        .map_err(|e| Error::Other(e.to_string()))?;
    let left: Vec<(String, String)> = left
        .query_map([format!("{}/%", ignore.0)], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(|e| Error::Other(e.to_string()))?
        .flatten()
        .collect();
    println!("still indexed under `{}/` after ignoring it: {left:?}\n", ignore.0);
    println!(
        "# `.kenignore` in `{member}`\n\nWrote:\n```\n{body}```\n`{}/` had {} files, `{}/` had {}.\n\n| | files | full | search-only |\n|---|---|---|---|\n| before | {files_before} | {} | {} |\n| after | {} | {} | {} |\n\nrescan: {} removed, {} updated, {:.1}s",
        ignore.0,
        ignore.1,
        search.0,
        search.1,
        before.get(&0).copied().unwrap_or(0),
        before.get(&1).copied().unwrap_or(0),
        db.file_count()?,
        after.get(&0).copied().unwrap_or(0),
        after.get(&1).copied().unwrap_or(0),
        stats.removed,
        stats.updated,
        t.elapsed().as_secs_f64()
    );
    // A pattern in the workspace's own .kenignore, at the parent: does it
    // reach files inside a member?
    let pk = parent.join(".kenignore");
    let pold = std::fs::read_to_string(&pk).unwrap_or_default();
    std::fs::write(&pk, format!("{pold}\n# Ken evaluation: inside a member\n{member}/{}/\n", search.0)).map_err(|e| Error::Other(e.to_string()))?;
    let _ = std::fs::remove_file(&ki);
    let stats2 = scan::scan(&p, &mut db)?;
    let still: i64 = conn
        .query_row("SELECT COUNT(*) FROM files WHERE rel_path LIKE ?1", [format!("{}/%", search.0)], |r| r.get(0))
        .unwrap_or(-1);
    println!(
        "\nParent `.kenignore` line `{member}/{}/` (member `.kenignore` removed): {} files under `{}/` still indexed after rescan ({} removed, {} added).",
        search.0, still, search.0, stats2.removed, stats2.added
    );
    std::fs::write(&pk, pold).map_err(|e| Error::Other(e.to_string()))?;
    match old {
        Some(o) => std::fs::write(&ki, o).map_err(|e| Error::Other(e.to_string()))?,
        None => {
            let _ = std::fs::remove_file(&ki);
        }
    }
    scan::scan(&p, &mut db)?;
    Ok(())
}

// ---------------------------------------------------------------- model

/// The knowledge model built by Claude over the whole member (the app's
/// "refresh knowledge model"), instead of file by file on the local model.
fn phase_model(base: &Path, parent: &Path, member: &str) -> Result<()> {
    let p = Project::open(&parent.join(member))?;
    let mut db = Db::open(base, p.config.id)?;
    let bin = ken_core::runner::discover_claude().ok_or_else(|| Error::Other("claude CLI not found".into()))?;
    let t = Instant::now();
    let counts = knowledge_model::build_knowledge_model(&bin, &p, &mut db, &today(), &CancelToken::new())?;
    let (ents, edges) = db.list_entities_with_edges()?;
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    for e in &ents {
        *kinds.entry(e.kind.clone()).or_default() += 1;
    }
    println!(
        "| {member} | {} entities · {} edges · {} events | stored {} / {} | {kinds:?} | {:.0}s |",
        counts.entities,
        counts.edges,
        counts.events,
        ents.len(),
        edges.len(),
        t.elapsed().as_secs_f64()
    );
    Ok(())
}

// ---------------------------------------------------------------- sql

/// A read-only query against one member's index, rows tab-separated.
fn phase_sql(base: &Path, parent: &Path, member: &str, query: &str) -> Result<()> {
    let p = Project::open(&parent.join(member))?;
    let conn = rusqlite::Connection::open_with_flags(ken_core::db::db_path(base, p.config.id), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| Error::Other(e.to_string()))?;
    let mut stmt = conn.prepare(query).map_err(|e| Error::Other(e.to_string()))?;
    let n = stmt.column_count();
    let rows = stmt
        .query_map([], |r| {
            Ok((0..n)
                .map(|i| r.get::<_, rusqlite::types::Value>(i).map(|v| format!("{v:?}")).unwrap_or_default())
                .collect::<Vec<_>>()
                .join("	"))
        })
        .map_err(|e| Error::Other(e.to_string()))?;
    for row in rows.flatten() {
        println!("{row}");
    }
    Ok(())
}

// ---------------------------------------------------------------- status

fn phase_status(base: &Path, parent: &Path) -> Result<()> {
    println!("# Status\n\n| member | files | chunks | meaning index | extracted | entities |\n|---|---|---|---|---|---|");
    for (name, p) in members(parent)? {
        let db = Db::open_read_only(base, p.config.id)?;
        let (done, total) = db.extraction_coverage()?;
        println!(
            "| {} | {} | {} | {} | {}/{} | {} |",
            name,
            db.file_count()?,
            db.chunk_count()?,
            db.vec_available(),
            done,
            total,
            db.list_entities_with_edges()?.0.len()
        );
    }
    Ok(())
}

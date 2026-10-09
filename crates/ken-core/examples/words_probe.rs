//! What the keyword index finds for a list of questions, on one repo, with
//! no embedder: the before-and-after check for changes to what goes into
//! the index (split names, JSON key outlines, new text formats, the code
//! map).
//!
//! ```text
//! cargo run --release -p ken-core --no-default-features --example words_probe -- index <repo> <index.db>
//! cargo run --release -p ken-core --no-default-features --example words_probe -- ask <index.db> <questions.tsv> [repo-name]
//! ```
//!
//! `index` scans `<repo>` into `<index.db>` (an existing index is brought up
//! to date, as the app does on open). `<repo>` gains a `.ken/project.json`,
//! so point it at a copy. `ask` reads `question<TAB>expected|expected…`
//! lines and prints where the first expected file ranks among the keyword
//! hits; an expected path names the repo first (`repo-name/…`) or is a path
//! tail. Paths in other repos are skipped.

use std::path::Path;
use std::time::Instant;

use ken_core::db::Db;
use ken_core::project::Project;
use ken_core::{codemap, routing, scan};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("index") => index(Path::new(&args[1]), Path::new(&args[2])),
        Some("ask") => ask(Path::new(&args[1]), Path::new(&args[2]), args.get(3).map(String::as_str).unwrap_or("repo")),
        _ => eprintln!("usage: words_probe index <repo> <index.db> | ask <index.db> <questions.tsv> [repo-name]"),
    }
}

fn index(repo: &Path, db_path: &Path) {
    let project = Project::create(repo, "probe").unwrap();
    let t = Instant::now();
    let mut db = Db::open_at(db_path).unwrap();
    let kinds = db.refresh_stored_kinds().unwrap();
    println!("open + upgrade: {:.1} s (kinds refreshed: {kinds})", t.elapsed().as_secs_f64());
    let t = Instant::now();
    let stats = scan::scan(&project, &mut db).unwrap();
    println!(
        "scan: {:.1} s, added {} updated {} unchanged {} failed {}",
        t.elapsed().as_secs_f64(),
        stats.added,
        stats.updated,
        stats.unchanged,
        stats.failed
    );
}

fn ask(db_path: &Path, tsv: &Path, repo: &str) {
    let db = Db::open_at(db_path).unwrap();
    let text = std::fs::read_to_string(tsv).unwrap();
    let prefix = format!("{repo}/");
    let files: Vec<String> = db.list_files().unwrap().into_iter().map(|f| f.rel_path).collect();
    let (mut asked, mut at1, mut at5, mut at30) = (0, 0, 0, 0);
    println!("| # | rank | question | first hit |\n|---|---|---|---|");
    for (n, line) in text.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()).enumerate() {
        let Some((q, expected)) = line.split_once('\t') else { continue };
        // Expected paths in this repo: `repo/…` with the repo taken off, or
        // a bare tail some file here ends with. Anything else is another
        // repo's, not asked here.
        let wanted: Vec<&str> = expected
            .split('|')
            .filter_map(|e| match e.strip_prefix(&prefix) {
                Some(rest) => Some(rest),
                None if files.iter().any(|f| f.contains(e)) => Some(e),
                None => None,
            })
            .collect();
        if wanted.is_empty() {
            continue;
        }
        asked += 1;
        let hits = routing::search_member(&db, q, None, 30).unwrap();
        let rank = hits.iter().position(|h| wanted.iter().any(|w| h.path == *w || h.path.ends_with(&format!("/{w}")) || h.path.contains(w)));
        if rank == Some(0) {
            at1 += 1;
        }
        if rank.is_some_and(|r| r < 5) {
            at5 += 1;
        }
        if rank.is_some() {
            at30 += 1;
        }
        let first = hits.first().map(|h| h.path.as_str()).unwrap_or("");
        println!("| {} | {} | {} | {} |", n + 1, rank.map(|r| (r + 1).to_string()).unwrap_or("—".into()), q, first);
    }
    println!("\n{asked} asked: hit@1 {at1}, hit@5 {at5}, hit@30 {at30}\n");
    for name in ["StashTab", "MAX_ROWS", "WeaponBases", "PartyMemberChip"] {
        let defs = codemap::definitions(&db, name).unwrap();
        println!("definitions of {name}: {:?}", defs.iter().map(|d| format!("{}:{} {}", d.path, d.line, d.kind)).collect::<Vec<_>>());
    }
    for path in [
        "src/main/resources/data/equipment/WeaponBases.json",
        "src/main/resources/Common/UI/Custom/Party/PartyMemberChip.ui",
        "build.gradle.kts",
        "src/main/java/dev/hytalemodding/models/enums/stash/StashTab.java",
    ] {
        let row = db.get_file(path).unwrap();
        println!("{path}: {:?}", row.map(|r| (r.kind, r.status)));
    }
}

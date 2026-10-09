//! How ken-core's index scales with repo size: a first scan, an unchanged
//! rescan, a rescan after every file's modified time moved (a sync client,
//! a checkout), a whole-repo re-tier to searchable only (a code kind), and
//! searches, the link report and index health on the result.
//!
//!   cargo run --release -p ken-core --no-default-features --example scale_bench -- <work-dir> [sizes…]
//!
//! Each size N generates, under <work-dir>/repo-N, N Rust-like files of about
//! 120 lines in folders of 100 and a wiki of N/10 pages with frontmatter,
//! aliases and links. Nothing outside <work-dir> is touched. Timings are wall
//! clock on this machine, one run each; read them as orders of magnitude.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use ken_core::db::Db;
use ken_core::project::Project;
use ken_core::scan;

fn code_file(i: usize) -> String {
    let mut s = format!("//! Module {i}: handles region {i} of the world save.\n\nuse std::collections::HashMap;\n\n");
    for f in 0..12 {
        s.push_str(&format!(
            "/// Loads chunk {f} for region {i}.\npub fn load_{i}_{f}(map: &mut HashMap<u32, Vec<u8>>, key: u32) -> Option<usize> {{\n    let entry = map.entry(key).or_default();\n    entry.push({f} as u8);\n    if entry.len() > 64 {{\n        return Some(entry.len());\n    }}\n    None\n}}\n\n"
        ));
    }
    s
}

fn wiki_page(i: usize, pages: usize) -> String {
    let next = (i + 1) % pages;
    let far = (i * 7 + 3) % pages;
    format!(
        "---\ntitle: \"Page {i}\"\naliases: [\"topic {i}\", \"area {i}\"]\nstatus: current\nverified: 2026-09-01\nsources:\n  - game:src/m{}/f{i}.rs:3\n---\n\n# Page {i}\n\nThis page explains region {i} and how saves are written. See [[page-{next}]] and [[page-{far}]], and [[missing-{i}]].\n\n## Details\n\nRegion {i} loads its chunks in order; a save writes each chunk once. The loader keeps a map of keys to bytes and flushes when a chunk passes 64 bytes.\n\n## Decisions\n\nWe keep one file per region so a crash loses one region, not the world.\n",
        i / 100
    )
}

fn generate(root: &Path, n: usize) {
    for i in 0..n {
        let dir = root.join("src").join(format!("m{}", i / 100));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("f{i}.rs")), code_file(i)).unwrap();
    }
    let pages = (n / 10).max(1);
    let wiki = root.join("Platform");
    fs::create_dir_all(&wiki).unwrap();
    for i in 0..pages {
        fs::write(wiki.join(format!("page-{i}.md")), wiki_page(i, pages)).unwrap();
    }
    fs::create_dir_all(root.join("Reference")).unwrap();
    fs::write(
        root.join("Reference/Vocabulary.md"),
        "# Vocabulary\n\n| our word | the code's or platform's word | shown in |\n|---|---|---|\n| shard | region | src/ |\n",
    )
    .unwrap();
    fs::write(root.join("START-HERE.md"), "# Start here\n\nRead this first.\n").unwrap();
}

fn walk(root: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(root).unwrap().flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn ms(d: Duration) -> String {
    format!("{:.0} ms", d.as_secs_f64() * 1000.0)
}

fn time<T>(f: impl FnOnce() -> T) -> (T, Duration) {
    let t = Instant::now();
    let v = f();
    (v, t.elapsed())
}

fn count(conn: &rusqlite::Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap_or(-1)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let work = PathBuf::from(args.first().expect("usage: scale_bench <work-dir> [sizes…]"));
    let sizes: Vec<usize> = if args.len() > 1 {
        args[1..].iter().map(|s| s.parse().expect("sizes are numbers")).collect()
    } else {
        vec![1_000, 10_000, 50_000]
    };
    fs::create_dir_all(&work).unwrap();
    println!("| files (code + wiki) | first scan | rescan, unchanged | rescan, dates moved | one page saved (watcher) | re-tier to search-only | search, median | link report | index health | DB size | chunks |");
    println!("|---|---|---|---|---|---|---|---|---|---|---|");
    for n in sizes {
        let root = work.join(format!("repo-{n}"));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(&root).unwrap();
        let (_, gen) = time(|| generate(&root, n));
        let project = Project::create(&root, "bench").unwrap();
        let db_path = work.join(format!("bench-{n}.db"));
        let _ = fs::remove_file(&db_path);
        let mut db = Db::open_at(&db_path).unwrap();

        let (stats, first) = time(|| scan::scan(&project, &mut db).unwrap());
        let (_, again) = time(|| scan::scan(&project, &mut db).unwrap());

        // Every file's modified time moves, its bytes do not.
        let mut files = Vec::new();
        walk(&root, &mut files);
        let later = SystemTime::now() + Duration::from_secs(3600);
        for f in &files {
            let _ = fs::File::options().write(true).open(f).and_then(|h| h.set_modified(later));
        }
        let (touched_stats, touched) = time(|| scan::scan(&project, &mut db).unwrap());

        // One page saved, as the watcher sees it: only that path rescanned.
        let page = root.join("Platform/page-0.md");
        fs::write(&page, wiki_page(0, (n / 10).max(1)).replace("how saves are written", "how saves are written and read")).unwrap();
        let (_, saved) = time(|| scan::scan_paths(&project, &mut db, &["Platform/page-0.md".to_string()]).unwrap());

        // Searches (each builds the vocabulary from the index, as the app does).
        let queries = ["region", "shard", "save chunk", "flushes when a chunk", "crash loses", "load map key", "topic 42", "page"];
        let mut lat: Vec<Duration> = queries
            .iter()
            .map(|q| time(|| ken_core::routing::search_member(&db, q, None, 30).unwrap()).1)
            .collect();
        lat.sort();
        let median = lat[lat.len() / 2];
        for q in queries {
            let (hits, full) = time(|| ken_core::routing::search_member(&db, q, None, 30).unwrap());
            let (_, fts) = time(|| db.search_chunks_fts(q, 30).unwrap());
            eprintln!("  n={n} {q:?}: search {} (keyword index alone {}), {} hits", ms(full), ms(fts), hits.len());
        }

        let (report, links) = time(|| ken_core::links::report(&db).unwrap());
        let (_, health) = time(|| db.index_health().unwrap());

        // A code kind: the whole repo becomes searchable only.
        fs::write(root.join(".kenignore"), "~*\n").unwrap();
        let (_, retier) = time(|| scan::scan(&project, &mut db).unwrap());
        fs::remove_file(root.join(".kenignore")).unwrap();

        drop(db);
        let size = fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0)
            + fs::metadata(db_path.with_extension("db-wal")).map(|m| m.len()).unwrap_or(0);
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let chunks = count(&conn, "SELECT COUNT(*) FROM chunks");
        let total = stats.added;
        println!(
            "| {total} | {} | {} | {} | {} | {} | {} | {} | {} | {:.0} MB | {chunks} |",
            ms(first),
            ms(again),
            ms(touched),
            ms(saved),
            ms(retier),
            ms(median),
            ms(links),
            ms(health),
            size as f64 / 1_048_576.0,
        );
        eprintln!(
            "  n={n}: generated in {}, dates-moved rescan re-read {} files, link report {} name links / {} missing",
            ms(gen),
            touched_stats.updated,
            report.name_links,
            report.missing_names.len()
        );
    }
}

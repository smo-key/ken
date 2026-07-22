//! Manual probe against a real index DB — run with:
//!   KEN_PROBE_BASE="$HOME/Library/Application Support/ken/index" \
//!   KEN_PROBE_ID=<uuid> KEN_PROBE_QUERY="..." \
//!   cargo test -p ken-core --test qa_probe -- --ignored --nocapture

use std::time::Instant;

#[test]
#[ignore]
fn probe_real_index() {
    let base = std::path::PathBuf::from(std::env::var("KEN_PROBE_BASE").expect("KEN_PROBE_BASE"));
    let id: uuid::Uuid = std::env::var("KEN_PROBE_ID")
        .expect("KEN_PROBE_ID")
        .parse()
        .expect("valid uuid");
    let query = std::env::var("KEN_PROBE_QUERY").expect("KEN_PROBE_QUERY");

    let db = ken_core::db::Db::open_read_only(&base, id).expect("open db");

    let t = Instant::now();
    let hits = db.search(&query, 8).expect("search");
    let search_ms = t.elapsed().as_secs_f64() * 1000.0;

    let tokens = ken_core::db::significant_tokens(&query);

    let t = Instant::now();
    let excerpts: Vec<Option<String>> = hits
        .iter()
        .map(|h| db.excerpt(&h.rel_path, &tokens, 350))
        .collect();
    let excerpt_ms = t.elapsed().as_secs_f64() * 1000.0;

    println!("search: {search_ms:.1} ms, {} hits", hits.len());
    println!(
        "excerpts: {excerpt_ms:.1} ms, {}/{} non-fallback",
        excerpts.iter().flatten().count(),
        hits.len()
    );
    for (h, ex) in hits.iter().zip(&excerpts) {
        println!(
            "  [{:>8.2}] {}  => {}",
            h.rank,
            h.rel_path,
            ex.as_ref()
                .map(|e| e.chars().take(90).collect::<String>())
                .unwrap_or_else(|| "(fallback snippet)".into())
        );
    }
}

//! Time the searches on an index already built (say the one `scale_bench`
//! leaves behind), without rebuilding it:
//!
//!   cargo run --release -p ken-core --no-default-features --example search_probe -- <index.db>
//!
//! Each query runs three times; the fastest is printed, so a cold page
//! cache does not decide the number.

use std::time::{Duration, Instant};

use ken_core::db::Db;

fn best<T>(mut f: impl FnMut() -> T) -> (T, Duration) {
    let mut out = None;
    let mut fastest = Duration::MAX;
    for _ in 0..3 {
        let t = Instant::now();
        let v = f();
        fastest = fastest.min(t.elapsed());
        out = Some(v);
    }
    (out.unwrap(), fastest)
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: search_probe <index.db>");
    let db = Db::open_at(std::path::Path::new(&path)).unwrap();
    let queries = ["region", "shard", "save chunk", "flushes when a chunk", "crash loses", "load map key", "topic 42", "page"];
    println!("| query | search (hybrid, keyword) | sections index | files index | hits |");
    println!("|---|---|---|---|---|");
    for q in queries {
        let (hits, full) = best(|| ken_core::routing::search_member(&db, q, None, 30).unwrap());
        let (_, chunks) = best(|| db.search_chunks_fts(q, 30).unwrap());
        let (_, files) = best(|| db.search(q, 30).unwrap());
        let top: Vec<String> = hits.iter().take(3).map(|h| h.path.clone()).collect();
        println!(
            "| {q} | {:.0} ms | {:.0} ms | {:.0} ms | {} ({}) |",
            full.as_secs_f64() * 1000.0,
            chunks.as_secs_f64() * 1000.0,
            files.as_secs_f64() * 1000.0,
            hits.len(),
            top.join(", ")
        );
    }
}

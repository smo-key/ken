//! Embedding throughput, one chunk per call against many per call, on real
//! chunks from an index, and whether the two give the same vectors.
//!
//! ```text
//! set KEN_DATA_DIR=<scratch>\data
//! cargo run --release -p ken-core --example embed_bench -- <index.db> [chunks]
//! ```

use std::time::Instant;

use ken_core::embedder::Embedder;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let db = &args[0];
    let n: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(512);
    let base = std::path::PathBuf::from(std::env::var_os("KEN_DATA_DIR").expect("set KEN_DATA_DIR"));
    ken_core::local_llm::init(base);
    let mut emb = ken_core::embedder::installed_embedding_model().expect("no embedding model");
    let conn = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mut stmt = conn.prepare("SELECT text FROM chunks ORDER BY id LIMIT ?1").unwrap();
    let texts: Vec<String> = stmt.query_map([n as i64], |r| r.get(0)).unwrap().flatten().collect();
    println!("{} chunks, model {}", texts.len(), emb.model_id());

    let t = Instant::now();
    let one: Vec<Vec<f32>> = texts.iter().map(|t| emb.embed(std::slice::from_ref(t)).unwrap().remove(0)).collect();
    let single = t.elapsed().as_secs_f64();

    let t = Instant::now();
    let mut many: Vec<Vec<f32>> = Vec::new();
    for batch in texts.chunks(128) {
        many.extend(emb.embed(batch).unwrap());
    }
    let packed = t.elapsed().as_secs_f64();

    let cos: Vec<f32> = one.iter().zip(&many).map(|(a, b)| a.iter().zip(b).map(|(x, y)| x * y).sum()).collect();
    let min = cos.iter().cloned().fold(f32::MAX, f32::min);
    let mean = cos.iter().sum::<f32>() / cos.len() as f32;
    println!("| path | seconds | chunks per second |\n|---|---|---|");
    println!("| one per call | {single:.1} | {:.1} |", texts.len() as f64 / single);
    println!("| 128 per call, packed | {packed:.1} | {:.1} |", texts.len() as f64 / packed);
    println!("\nsame vectors: cosine mean {mean:.5}, min {min:.5}");
}

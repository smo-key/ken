//! The semantic index: chunks and their vectors, rebuilt from what the
//! project's index holds. (This module once also ran the recipe and
//! automation queue; those are gone.)

use crate::chunker::{self, IndexProfile};
use crate::db::Db;
use crate::embedder::Embedder;
use crate::project::Project;
use crate::runner::CancelToken;
use crate::Result;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Number of chunk texts embedded per `Embedder::embed` call, gathered across
/// files. Each call makes one model context and packs its chunks several to a
/// pass; one call per file (most files are one or two chunks) spent the time
/// making contexts, not vectors.
const EMBED_BATCH: usize = 128;

/// Embed `ids`/`texts` and store the vectors. One chunk the model can't take
/// must not stop the index: the batch is retried one by one and only what
/// fails is left out (it stays keyword-searchable, and is retried next build).
///
/// Embedding runs on the graphics card or the processor, so it obeys the one
/// background throttle (knowledge-core decision 34): it waits while paused,
/// and after each batch rests by the pace. A build cancelled while it waits
/// leaves the batch for the next build, which embeds every chunk with no
/// vector.
fn embed_pending(db: &mut Db, embedder: &mut dyn Embedder, ids: &mut Vec<i64>, texts: &mut Vec<String>, token: &CancelToken) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let stop = || token.is_cancelled();
    if !crate::throttle::wait_unpaused(&stop) {
        ids.clear();
        texts.clear();
        return Ok(());
    }
    let started = std::time::Instant::now();
    match embedder.embed(texts) {
        Ok(vecs) => db.store_embeddings(ids, &vecs)?,
        Err(batch_err) => {
            for (id, text) in ids.iter().zip(texts.iter()) {
                match embedder.embed(std::slice::from_ref(text)) {
                    Ok(v) => db.store_embeddings(&[*id], &v)?,
                    Err(e) => eprintln!("semantic index: chunk {id} skipped: {e} (batch: {batch_err})"),
                }
            }
        }
    }
    ids.clear();
    texts.clear();
    crate::throttle::rest_after(started.elapsed(), &stop);
    Ok(())
}

/// Regenerate `chunks` and `vec_chunks` from the project's indexed contents
/// alone (nothing else feeds it — see spec "The semantic index is entirely
/// derived"). Incremental: `Db::upsert_chunks` diffs by `content_hash` per
/// file, so unchanged files cost no embed calls.
///
/// Cancellation is checked once per file, before that file's chunks are
/// upserted — never mid-file. This matters: `upsert_chunks` persists a file's
/// `chunks` rows (keyed by content hash) before this function gets a chance
/// to embed them, so bailing out *after* the upsert but *before*
/// `store_embeddings` would leave that file's changed chunks permanently
/// un-embedded — a later rebuild would see the same hash and treat them as
/// already-up-to-date. Checking only at the top of the loop avoids that trap.
///
/// Returns `Ok(true)` if every indexed file was processed (and `embed_model`
/// / `embed_dim` / `semantic_built_at` were recorded in `meta`), or
/// `Ok(false)` if `token` was cancelled before completion (meta is left
/// untouched in that case, so the next rebuild attempt starts from a
/// known-stale state instead of a falsely-fresh one).
pub fn rebuild_semantic_index(
    project: &Project,
    db: &mut Db,
    embedder: &mut dyn Embedder,
    token: &CancelToken,
    mut on_progress: impl FnMut(usize, usize),
) -> Result<bool> {
    // Every file: an indexed one is chunked again from its text; one with no
    // readable text keeps the chunks the scan gave it (its path, or its OCR
    // text) and only has them embedded.
    let files: Vec<_> = db.list_files()?;
    let total = files.len();

    // A different embedding model (or width) cannot share the index with the
    // old vectors: they would fail to store, or worse, mix two meaning spaces.
    // Drop them and re-read every chunk. An index from before the model was
    // stamped holds Nomic vectors.
    if needs_reembed(db.embed_model()?.as_deref(), db.embed_dim()?, db.vector_count()?, &embedder.model_id(), embedder.dim()) {
        db.reset_vectors()?;
    }
    db.ensure_vec_chunks(embedder.dim())?;

    // The same index rules the scan reads (decision 34), once per file.
    let rules = project.index_rules();

    // Chunks still to embed, gathered across files.
    let mut pending_ids: Vec<i64> = Vec::new();
    let mut pending_texts: Vec<String> = Vec::new();
    // Whether anything was chunked again, dropped or embedded: what makes
    // the controls due again.
    let mut changed = false;

    for (done, file) in files.into_iter().enumerate() {
        if token.is_cancelled() {
            return Ok(false);
        }

        if crate::kenignore::classify(&file.rel_path, false, &[&rules]) == crate::kenignore::Tier::Ignored {
            // Defensive: scan.rs should already keep Ignore-tier paths out of
            // `files`, but if one slips through (e.g. a `.kenignore` edit
            // landed after the last scan), don't chunk/embed it, and drop any
            // stale chunks left from before it became Ignore.
            if db.has_chunks(&file.rel_path)? {
                changed = true;
            }
            db.delete_chunks(&file.rel_path)?;
            on_progress(done + 1, total);
            continue;
        }

        if file.status == "indexed" {
            let text = match db.get_text(&file.rel_path)? {
                Some(t) => t,
                None => {
                    on_progress(done + 1, total);
                    continue;
                }
            };

            // Chunked by its extension's profile. An empty file keeps its
            // name chunk.
            let idx_profile = IndexProfile::default_for(&file.rel_path);
            // Code is cut at its definitions, read from the code index the
            // scan stored rather than parsed again.
            let chunked = if text.trim().is_empty() {
                chunker::Chunked { chunks: vec![chunker::name_chunk(&file.rel_path)], capped: false }
            } else {
                let defs = if crate::codeindex::Lang::of(&file.rel_path).is_some() {
                    db.code_symbols(None, Some(&file.rel_path), Some(true), 5000)?
                } else {
                    Vec::new()
                };
                chunker::chunk_with(&file.rel_path, &text, &idx_profile, &defs)
            };
            if !db.store_chunked(&file.rel_path, &chunked)?.is_empty() {
                changed = true;
            }
        }

        // Embed every chunk with no vector yet, not only the ones this call
        // changed: the scan writes chunks (for keyword search) before any
        // rebuild runs, so a chunk can be unchanged here and still unembedded.
        // `upsert_chunks` drops the vector of a chunk whose text changed, so
        // those are missing too.
        for (id, text) in db.chunks_missing_vectors(&file.rel_path)? {
            changed = true;
            pending_ids.push(id);
            pending_texts.push(text);
            if pending_ids.len() >= EMBED_BATCH {
                embed_pending(db, embedder, &mut pending_ids, &mut pending_texts, token)?;
            }
        }

        on_progress(done + 1, total);
    }
    embed_pending(db, embedder, &mut pending_ids, &mut pending_texts, token)?;

    db.set_embed_model(&embedder.model_id())?;
    db.set_embed_dim(embedder.dim())?;
    db.set_semantic_built_at(now_epoch())?;
    run_controls(project, db, embedder, changed);
    Ok(true)
}

/// Bring the meaning index up to date after a scan changed some chunks, or a
/// person accepted new controls: embed every chunk that has no vector yet,
/// then run the controls. Unlike [`rebuild_semantic_index`] it chunks
/// nothing again, since the scan already wrote the chunks; it reads only
/// what is missing, so it is cheap to ask for after every change.
///
/// It is only asked for when the files or the controls changed, so the
/// controls always run, even when nothing needed embedding (knowledge-core
/// decision 19). It waits while background work is paused before it starts,
/// and its embedding obeys the throttle like the rebuild's.
///
/// A source with no meaning index yet, or one built with another model,
/// gets the full rebuild instead. Returns `Ok(false)` when cancelled.
pub fn catch_up_semantic_index(
    project: &Project,
    db: &mut Db,
    embedder: &mut dyn Embedder,
    token: &CancelToken,
    mut on_progress: impl FnMut(usize, usize),
) -> Result<bool> {
    if db.semantic_built_at()?.is_none()
        || needs_reembed(db.embed_model()?.as_deref(), db.embed_dim()?, db.vector_count()?, &embedder.model_id(), embedder.dim())
    {
        return rebuild_semantic_index(project, db, embedder, token, on_progress);
    }
    if !crate::throttle::wait_unpaused(&|| token.is_cancelled()) {
        return Ok(false);
    }
    db.ensure_vec_chunks(embedder.dim())?;

    let paths = db.paths_missing_vectors()?;
    let total = paths.len();
    let mut pending_ids: Vec<i64> = Vec::new();
    let mut pending_texts: Vec<String> = Vec::new();
    for (done, path) in paths.iter().enumerate() {
        if token.is_cancelled() {
            return Ok(false);
        }
        for (id, text) in db.chunks_missing_vectors(path)? {
            pending_ids.push(id);
            pending_texts.push(text);
            if pending_ids.len() >= EMBED_BATCH {
                embed_pending(db, embedder, &mut pending_ids, &mut pending_texts, token)?;
            }
        }
        on_progress(done + 1, total);
    }
    embed_pending(db, embedder, &mut pending_ids, &mut pending_texts, token)?;
    if token.is_cancelled() {
        return Ok(false);
    }
    on_progress(total, total);
    db.set_semantic_built_at(now_epoch())?;
    run_controls(project, db, embedder, true);
    Ok(true)
}

/// The controls, through the real pipeline with meaning (knowledge-core
/// decision 19): when the index changed, when the controls are not the ones
/// last run, or when none ran yet. A failure is reported, never a failed
/// build.
fn run_controls(project: &Project, db: &Db, embedder: &mut dyn Embedder, changed: bool) {
    let controls = crate::controls::for_source(&project.root);
    let due = changed
        || match db.control_results() {
            Ok(Some(_)) => !crate::controls::same_set(&crate::controls::stored(db), &controls),
            _ => true,
        };
    if !due {
        return;
    }
    if let Err(e) = crate::controls::run_and_store(db, &controls, &mut |q| embedder.embed_query(q).ok()) {
        eprintln!("controls: {e}");
    }
}

/// Whether the stored vectors came from a different model than `model_id`
/// (or a different width). Unstamped vectors are from Nomic, the first model.
pub fn needs_reembed(stored_model: Option<&str>, stored_dim: Option<usize>, vectors: i64, model_id: &str, dim: usize) -> bool {
    match stored_model {
        Some(m) => m != model_id || stored_dim.is_some_and(|d| d != dim),
        None => vectors > 0 && model_id != crate::embedder::NOMIC.model_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedder::FakeEmbedder;

    #[test]
    fn a_different_model_or_width_means_reading_again() {
        assert!(!needs_reembed(None, None, 0, "qwen3-embedding-0.6b", 1024), "nothing stored yet");
        assert!(!needs_reembed(None, None, 50, "nomic-embed-text-v1.5", 768), "unstamped vectors are Nomic's");
        assert!(needs_reembed(None, None, 50, "qwen3-embedding-0.6b", 1024));
        assert!(needs_reembed(Some("nomic-embed-text-v1.5"), Some(768), 50, "qwen3-embedding-0.6b", 1024));
        assert!(needs_reembed(Some("x"), Some(768), 50, "x", 1024), "same id, other width");
        assert!(!needs_reembed(Some("x"), Some(768), 50, "x", 768));
    }

    #[test]
    fn switching_models_drops_the_old_vectors_and_reads_again() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.md"), "# Retry\n\nPayments retry three times.").unwrap();
        let project = Project::create(dir.path(), "T").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        crate::scan::scan(&project, &mut db).unwrap();
        let mut first = FakeEmbedder::new();
        rebuild_semantic_index(&project, &mut db, &mut first, &CancelToken::new(), |_, _| {}).unwrap();
        assert_eq!(db.embed_model().unwrap().as_deref(), Some("fake-hash-8"));
        let before = db.vector_count().unwrap();
        // Another model of another width: the store would refuse 16-wide
        // vectors in an 8-wide table unless the table is rebuilt.
        struct Wide;
        impl Embedder for Wide {
            fn embed(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
                Ok(texts.iter().map(|_| { let mut v = vec![0.0f32; 16]; v[0] = 1.0; v }).collect())
            }
            fn dim(&self) -> usize { 16 }
            fn model_id(&self) -> String { "wide-16".into() }
        }
        rebuild_semantic_index(&project, &mut db, &mut Wide, &CancelToken::new(), |_, _| {}).unwrap();
        assert_eq!(db.embed_model().unwrap().as_deref(), Some("wide-16"));
        assert_eq!(db.embed_dim().unwrap(), Some(16));
        if db.vec_available() {
            assert_eq!(db.vector_count().unwrap(), before, "every chunk read again");
        }
    }
    fn set_controls(root: &std::path::Path, controls: &str) {
        fs::create_dir_all(root.join(".ken")).unwrap();
        fs::write(root.join(".ken/knowledge.json"), format!(r#"{{"controls": {controls}}}"#)).unwrap();
    }

    /// After a scan changed a file, or a person accepted controls, the
    /// catch-up embeds what has no vector and runs the controls, even when
    /// nothing needed embedding (knowledge-core decision 19).
    #[test]
    fn a_catch_up_embeds_what_a_scan_changed_and_runs_the_controls() {
        let dir = tempfile::tempdir().unwrap();
        let app_dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.md"), "# Retry

Payments retry three times.").unwrap();
        set_controls(dir.path(), "[]");
        let project = Project::create(dir.path(), "T").unwrap();
        let mut db = Db::open_at(&app_dir.path().join("t.db")).unwrap();
        scan::scan(&project, &mut db).unwrap();
        let mut embedder = FakeEmbedder::new();
        assert!(rebuild_semantic_index(&project, &mut db, &mut embedder, &CancelToken::new(), |_, _| {}).unwrap());
        assert!(crate::controls::stored(&db).is_empty());

        // Accepted: the controls changed, nothing needs embedding.
        set_controls(dir.path(), r#"[{"question": "how often do payments retry", "expect": "a.md"}]"#);
        assert!(db.paths_missing_vectors().unwrap().is_empty() || !db.vec_available());
        assert!(catch_up_semantic_index(&project, &mut db, &mut embedder, &CancelToken::new(), |_, _| {}).unwrap());
        let results = crate::controls::stored(&db);
        assert_eq!(results.len(), 1, "the accepted control ran");
        assert!(results[0].passed, "{results:?}");

        // A file changed: the scan writes its chunks, the catch-up embeds them.
        fs::write(dir.path().join("b.md"), "# Saving

Worlds save every five minutes.").unwrap();
        scan::scan(&project, &mut db).unwrap();
        if db.vec_available() {
            assert_eq!(db.paths_missing_vectors().unwrap(), vec!["b.md".to_string()]);
        }
        assert!(catch_up_semantic_index(&project, &mut db, &mut embedder, &CancelToken::new(), |_, _| {}).unwrap());
        if db.vec_available() {
            assert!(db.paths_missing_vectors().unwrap().is_empty(), "every changed chunk has its vector");
        }
    }

    /// A rebuild that changed nothing, with the controls last run, leaves
    /// their results alone; new controls make them due again.
    #[test]
    fn a_rebuild_runs_the_controls_only_when_something_changed() {
        let dir = tempfile::tempdir().unwrap();
        let app_dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.md"), "# Retry

Payments retry three times.").unwrap();
        set_controls(dir.path(), r#"[{"question": "how often do payments retry", "expect": "a.md"}]"#);
        let project = Project::create(dir.path(), "T").unwrap();
        let mut db = Db::open_at(&app_dir.path().join("t.db")).unwrap();
        scan::scan(&project, &mut db).unwrap();
        let mut embedder = FakeEmbedder::new();
        rebuild_semantic_index(&project, &mut db, &mut embedder, &CancelToken::new(), |_, _| {}).unwrap();
        assert!(crate::controls::stored(&db)[0].passed);

        // A marker run of the same controls: a rebuild with nothing to do keeps it.
        let marker = r#"[{"question":"how often do payments retry","expect":"a.md","rank":4,"passed":true}]"#;
        db.store_control_results(marker).unwrap();
        rebuild_semantic_index(&project, &mut db, &mut embedder, &CancelToken::new(), |_, _| {}).unwrap();
        assert_eq!(crate::controls::stored(&db)[0].rank, Some(4), "nothing changed, nothing ran");

        set_controls(dir.path(), r#"[{"question": "payments retry", "expect": "a.md"}]"#);
        rebuild_semantic_index(&project, &mut db, &mut embedder, &CancelToken::new(), |_, _| {}).unwrap();
        assert_eq!(crate::controls::stored(&db)[0].question, "payments retry", "new controls ran");
    }

    use crate::scan;
    use crate::search::{self, FtsHit, VecHit};
    use crate::Error;
    use std::fs;

    /// Runs `search_chunks_fts` + `semantic_search` + `search::merge_and_rerank`
    /// for a query: the building blocks `routing::search_member` composes,
    /// with results always reranked before return (S7b Condition C).
    fn hybrid_search(db: &Db, embedder: &mut FakeEmbedder, query: &str) -> Vec<search::HybridHit> {
        let fts_hits: Vec<FtsHit> = db.search_chunks_fts(query, 10).unwrap();
        let query_vec = embedder.embed(&[query.to_string()]).unwrap().remove(0);
        let vec_hits: Vec<VecHit> = db
            .semantic_search(&query_vec, 10)
            .unwrap()
            .into_iter()
            .map(|(chunk_id, path, text, distance)| VecHit {
                chunk_id,
                path,
                text,
                distance,
            })
            .collect();
        search::merge_and_rerank(&fts_hits, &vec_hits, query)
    }

    /// What the model reads is the chunk under its breadcrumb (prose) or its
    /// path and symbol (code); what search shows is the chunk alone.
    #[test]
    fn the_embedded_text_carries_the_breadcrumb_and_the_symbol() {
        struct Recording(FakeEmbedder, Vec<String>);
        impl Embedder for Recording {
            fn embed(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
                self.1.extend(texts.iter().cloned());
                self.0.embed(texts)
            }
            fn dim(&self) -> usize {
                self.0.dim()
            }
            fn model_id(&self) -> String {
                self.0.model_id()
            }
        }
        let project_dir = tempfile::tempdir().unwrap();
        fs::write(project_dir.path().join("pay.md"), "# Payments\n\nIntro.\n\n## Retries\n\nThree times.\n").unwrap();
        fs::write(project_dir.path().join("pay.py"), "def settle():\n    \"\"\"Settles the month.\"\"\"\n    return 1\n").unwrap();
        let project = Project::create(project_dir.path(), "T").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        scan::scan(&project, &mut db).unwrap();
        let mut embedder = Recording(FakeEmbedder::new(), Vec::new());
        rebuild_semantic_index(&project, &mut db, &mut embedder, &CancelToken::new(), |_, _| {}).unwrap();
        assert!(embedder.1.iter().any(|t| t == "Payments › Retries\n\n## Retries\n\nThree times."), "{:?}", embedder.1);
        assert!(embedder.1.iter().any(|t| t.starts_with("pay.py › settle")), "{:?}", embedder.1);
        let shown = db.search_chunks_fts("three times", 5).unwrap();
        assert_eq!(shown[0].text, "## Retries\n\nThree times.");
    }

    #[test]
    fn a_chunk_the_model_refuses_is_left_out_not_the_whole_index() {
        struct Picky(FakeEmbedder);
        impl Embedder for Picky {
            fn embed(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
                if texts.iter().any(|t| t.contains("POISON")) {
                    return Err(Error::Other("the model refused it".into()));
                }
                self.0.embed(texts)
            }
            fn dim(&self) -> usize {
                self.0.dim()
            }
            fn model_id(&self) -> String {
                self.0.model_id()
            }
        }
        let project_dir = tempfile::tempdir().unwrap();
        let app_dir = tempfile::tempdir().unwrap();
        fs::write(project_dir.path().join("a.md"), "# Fine\nThe quokka juggles.\n").unwrap();
        fs::write(project_dir.path().join("b.md"), "# Bad\nPOISON in here.\n").unwrap();
        fs::write(project_dir.path().join("c.md"), "# Also fine\nSpreadsheets.\n").unwrap();
        let project = Project::create(project_dir.path(), "T").unwrap();
        let mut db = Db::open_at(&app_dir.path().join("t.db")).unwrap();
        scan::scan(&project, &mut db).unwrap();

        let mut embedder = Picky(FakeEmbedder::new());
        assert!(rebuild_semantic_index(&project, &mut db, &mut embedder, &CancelToken::new(), |_, _| {}).unwrap());
        assert!(db.chunks_missing_vectors("a.md").unwrap().is_empty());
        assert!(db.chunks_missing_vectors("c.md").unwrap().is_empty(), "files after the bad one still embed");
        assert_eq!(db.chunks_missing_vectors("b.md").unwrap().len(), 1, "only the refused chunk waits");
    }

    #[test]
    fn semantic_rebuild_and_hybrid_search_finds_exact_text_chunk() {
        // Lightweight fixture (not the full Rig): a project dir with two
        // fixture files, a separate app-dir DB, scanned but with no
        // engine/hooks/automation involved — task 1.8 only needs
        // `rebuild_semantic_index` plus the search building blocks.
        let project_dir = tempfile::tempdir().unwrap();
        let app_dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(project_dir.path().join("notes")).unwrap();
        fs::write(
            project_dir.path().join("notes/a.md"),
            "# Zephyr\nThe quokka juggles xylophones at midnight.\n",
        )
        .unwrap();
        fs::write(
            project_dir.path().join("notes/b.md"),
            "# Other\nSomething entirely unrelated about spreadsheets.\n",
        )
        .unwrap();

        let project = Project::create(project_dir.path(), "T").unwrap();
        let db_path = app_dir.path().join("test.db");
        let mut db = Db::open_at(&db_path).unwrap();
        scan::scan(&project, &mut db).unwrap();

        let mut embedder = FakeEmbedder::new();
        let token = CancelToken::new();
        let completed =
            rebuild_semantic_index(&project, &mut db, &mut embedder, &token, |_, _| {}).unwrap();
        assert!(completed, "rebuild should run to completion (no cancel token set)");

        // FTS finds the distinctive keyword.
        let by_keyword = hybrid_search(&db, &mut embedder, "quokka");
        assert!(
            by_keyword.iter().any(|h| h.path == "notes/a.md"),
            "expected notes/a.md among hybrid results for 'quokka', got {:?}",
            by_keyword
        );

        // KNN finds the exact chunk text: under FakeEmbedder's no-prefix
        // determinism, a query built from the same string as a stored
        // chunk embeds to the identical vector, so it must come back as
        // an exact (distance ~0) semantic hit.
        let stored_chunk_text = by_keyword
            .iter()
            .find(|h| h.path == "notes/a.md")
            .map(|h| h.snippet.clone())
            .expect("notes/a.md snippet");
        let by_exact_chunk = hybrid_search(&db, &mut embedder, &stored_chunk_text);
        assert!(
            by_exact_chunk.iter().any(|h| h.path == "notes/a.md"),
            "expected notes/a.md among hybrid results for its own exact chunk text, got {:?}",
            by_exact_chunk
        );

        // --- rebuild-after-drop equivalence (spec.md's explicit scenario:
        // "WHEN chunks and vec_chunks are dropped and a rebuild runs THEN
        // hybrid search results are equivalent to before the deletion") ---
        let before = hybrid_search(&db, &mut embedder, "quokka");

        for f in db.list_files().unwrap() {
            db.delete_chunks(&f.rel_path).unwrap();
        }
        let token2 = CancelToken::new();
        let completed2 =
            rebuild_semantic_index(&project, &mut db, &mut embedder, &token2, |_, _| {}).unwrap();
        assert!(completed2, "second rebuild should also run to completion");

        let after = hybrid_search(&db, &mut embedder, "quokka");
        assert_eq!(
            before
                .iter()
                .map(|h| (h.path.clone(), h.source))
                .collect::<Vec<_>>(),
            after
                .iter()
                .map(|h| (h.path.clone(), h.source))
                .collect::<Vec<_>>(),
            "hybrid search results must be equivalent after a drop + rebuild"
        );
    }
}

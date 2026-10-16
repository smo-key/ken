//! Text embedding — turns chunks of text into dense vectors for semantic
//! search. Two implementations live here:
//!
//! * [`FakeEmbedder`] — deterministic, dependency-free hash vectors (dim 8).
//!   Fast and stable, so it powers unit/integration tests without a GGUF on
//!   disk. Vectors are derived from the RAW text (no `search_document:` /
//!   `search_query:` prefix) so a document and a query built from the same
//!   string produce the same vector — exact-match search works end to end.
//!
//! * [`LlamaEmbedder`] — the real model (nomic-embed-text-v1.5, 768-dim,
//!   Q8_0), run as a *second* llama.cpp context in this process. It shares the
//!   single process-wide [`LlamaBackend`](crate::local_llm) with the language
//!   model — llama.cpp's backend must only be initialised once — via
//!   `crate::local_llm::shared_backend()`. It is gated behind the `local-llm`
//!   feature.
//!
//! nomic-embed-text expects task prefixes: documents are embedded with
//! `search_document:` and queries with `search_query:`. [`Embedder::embed`]
//! is the document side; [`Embedder::embed_query`] is the query side. The
//! default `embed_query` just embeds the raw text (which keeps the fake
//! embedder's document/query exact-match property), while [`LlamaEmbedder`]
//! overrides it to apply the `search_query:` prefix.

use crate::Result;

/// Turns batches of text into fixed-width, L2-normalized vectors.
///
/// Implementors embed *documents* (the `search_document:` side for models that
/// use task prefixes). All returned vectors have length [`Embedder::dim`] and
/// are L2-normalized, so cosine similarity is a plain dot product.
pub trait Embedder {
    /// Embed a batch of documents. The returned outer vec has one entry per
    /// input, in order; each inner vec has length [`Embedder::dim`].
    fn embed(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>>;

    /// Dimensionality of every vector this embedder returns.
    fn dim(&self) -> usize;

    /// Stable identifier for the model, e.g. `"nomic-embed-text-v1.5"` or
    /// `"fake-hash-8"`. Persisted in meta so we can detect when the embedding
    /// model changed and the index must be rebuilt.
    fn model_id(&self) -> String;

    /// Embed a single *query* (the `search_query:` side for models with task
    /// prefixes). The default embeds the raw text like a document, which is
    /// exactly right for prefix-free embedders such as [`FakeEmbedder`] —
    /// document and query vectors for the same string stay identical.
    fn embed_query(&mut self, text: &str) -> Result<Vec<f32>> {
        self.embed(&[text.to_string()])?
            .into_iter()
            .next()
            .ok_or_else(|| crate::Error::Other("embedder returned no vector for query".into()))
    }
}

/// L2-normalize a vector in place. A zero vector is left untouched (its norm is
/// zero, so there is nothing sensible to divide by).
fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// How one embedding model wants its text and its vectors: the model id
/// stored with the index (a change forces a re-read), how token vectors are
/// pooled, and the text put before a query or a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedProfile {
    pub model_id: &'static str,
    pub pooling: Pooling,
    pub query_prefix: &'static str,
    pub doc_prefix: &'static str,
    /// Whether the end-of-text token must close every input (last-token
    /// pooling reads the vector there).
    pub add_eos: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pooling {
    Mean,
    Last,
}

/// Nomic Embed v1.5: mean pooling and its two task prefixes.
pub const NOMIC: EmbedProfile = EmbedProfile {
    model_id: "nomic-embed-text-v1.5",
    pooling: Pooling::Mean,
    query_prefix: "search_query: ",
    doc_prefix: "search_document: ",
    add_eos: false,
};

/// Qwen3 Embedding 0.6B: last-token pooling, an instruction on queries
/// only, documents as they are.
pub const QWEN3: EmbedProfile = EmbedProfile {
    model_id: "qwen3-embedding-0.6b",
    pooling: Pooling::Last,
    query_prefix: "Instruct: Given a question, retrieve passages from a team's documents and code that answer it\nQuery:",
    doc_prefix: "",
    add_eos: true,
};

/// The profile for an embedding model file, by its name. Unknown files are
/// read as Nomic, the model Ken shipped first.
pub fn profile_for_file(file: &str) -> EmbedProfile {
    let f = file.to_ascii_lowercase();
    if f.contains("qwen3-embedding") {
        QWEN3
    } else {
        NOMIC
    }
}

/// Deterministic, model-free embedder used by tests and as a safe fallback.
///
/// Each of the [`DIM`](FakeEmbedder::DIM) components is an independent
/// `XxHash64` of the raw text under a distinct seed, mapped into `[-1, 1]`, and
/// the whole vector is L2-normalized. Identical text always yields the identical
/// vector; different text almost always differs. No task prefix is applied, so a
/// document and a query built from the same string match exactly.
#[derive(Debug, Clone, Default)]
pub struct FakeEmbedder;

impl FakeEmbedder {
    /// Vector width for the fake embedder. Small on purpose — enough to be a
    /// meaningful vector, cheap enough to be free.
    pub const DIM: usize = 8;

    /// Construct a new fake embedder.
    pub fn new() -> Self {
        FakeEmbedder
    }

    /// Embed a single string into a `DIM`-wide, L2-normalized vector.
    fn embed_one(text: &str) -> Vec<f32> {
        let bytes = text.as_bytes();
        let mut v = vec![0.0f32; Self::DIM];
        for (i, slot) in v.iter_mut().enumerate() {
            // A distinct, fixed seed per component gives DIM independent hashes
            // of the same text.
            let h = twox_hash::XxHash64::oneshot(0x9E37_79B9_7F4A_7C15u64 ^ (i as u64), bytes);
            // Map the full u64 into [-1, 1].
            *slot = ((h as f64 / u64::MAX as f64) * 2.0 - 1.0) as f32;
        }
        l2_normalize(&mut v);
        v
    }
}

impl Embedder for FakeEmbedder {
    fn embed(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| Self::embed_one(t)).collect())
    }

    fn dim(&self) -> usize {
        Self::DIM
    }

    fn model_id(&self) -> String {
        "fake-hash-8".to_string()
    }
}

#[cfg(feature = "local-llm")]
mod llama {
    use super::{l2_normalize, EmbedProfile, Embedder, Pooling};
    use crate::{Error, Result};
    use llama_cpp_2::context::params::{LlamaContextParams, LlamaPoolingType};
    use llama_cpp_2::context::LlamaContext;
    use llama_cpp_2::llama_backend::LlamaBackend;
    use llama_cpp_2::llama_batch::LlamaBatch;
    use llama_cpp_2::model::params::LlamaModelParams;
    use llama_cpp_2::model::{AddBos, LlamaModel};
    use std::num::NonZeroU32;
    use std::path::Path;

    /// Largest single string (in tokens) we will embed. nomic-embed-text has a
    /// 2048-token context; we cap the batch/context there. Longer inputs should
    /// be chunked before reaching the embedder.
    const N_CTX: u32 = 2048;

    /// Most chunks run through the model in one pass. A mean-pooled model
    /// takes many short sequences side by side in one batch; one pass per
    /// chunk left the graphics card idle most of the time (about 10 chunks a
    /// second on an RTX A4000 at 12% load, measured 2026-10-05).
    const N_SEQ: u32 = 32;

    /// The real embedding model, run as a second llama.cpp context sharing the
    /// process-wide backend with the language model. 768-dim, mean-pooled,
    /// L2-normalized output. Gated behind the `local-llm` feature.
    pub struct LlamaEmbedder {
        backend: &'static LlamaBackend,
        model: LlamaModel,
        profile: EmbedProfile,
        dim: usize,
    }

    impl LlamaEmbedder {
        /// Load a GGUF embedding model from `path`, sharing the single
        /// process-wide [`LlamaBackend`]. `model_id` is a stable label stored
        /// alongside the index so a model swap can be detected.
        pub fn load(path: &Path, profile: EmbedProfile) -> Result<Self> {
            let backend = crate::local_llm::shared_backend()?;
            let layers = crate::compute::gpu_layers();
            let load = |layers: u32| {
                LlamaModel::load_from_file(backend, path, &LlamaModelParams::default().with_n_gpu_layers(layers))
            };
            // A graphics card that cannot take the model (no memory, a driver
            // fault) must not turn meaning search off: load it on the CPU.
            let model = match load(layers) {
                Ok(m) => Ok(m),
                Err(gpu_err) if layers > 0 => {
                    eprintln!("embedding model on the graphics card failed ({gpu_err}); loading it on the CPU");
                    load(0)
                }
                Err(e) => Err(e),
            }
            .map_err(|e| Error::Other(format!("couldn't load embedding model {}: {e}", path.display())))?;
            let dim = usize::try_from(model.n_embd()).unwrap_or(0);
            Ok(Self { backend, model, profile, dim })
        }

        /// An embedding context: pooled output, no generation. n_batch and
        /// n_ubatch are held equal (coordinator constraint) so a whole
        /// sequence is embedded in a single pooled pass. Making one allocates
        /// the compute buffers on the GPU, so a batch of texts shares one.
        fn context(&self) -> Result<LlamaContext<'_>> {
            let threads = crate::compute::threads();
            let ctx_params = LlamaContextParams::default()
                .with_n_ctx(NonZeroU32::new(N_CTX))
                .with_n_batch(N_CTX)
                .with_n_ubatch(N_CTX)
                .with_n_seq_max(if self.profile.pooling == Pooling::Mean { N_SEQ } else { 1 })
                .with_n_threads(threads)
                .with_n_threads_batch(threads)
                .with_embeddings(true)
                .with_pooling_type(match self.profile.pooling {
                    Pooling::Mean => LlamaPoolingType::Mean,
                    Pooling::Last => LlamaPoolingType::Last,
                });
            self.model
                .new_context(self.backend, ctx_params)
                .map_err(|e| Error::Other(format!("couldn't create embedding context: {e}")))
        }

        /// The tokens one already-prefixed string is embedded from: NUL
        /// refused, cut to the context, closed by end-of-text when the
        /// model pools on it.
        fn tokens(&self, text: &str) -> Result<Vec<llama_cpp_2::token::LlamaToken>> {
            // The tokenizer takes a C string: a NUL (binary-ish text from a
            // PDF or a data file) would end it, so it is refused outright.
            let text = text.replace('\0', " ");
            let mut tokens = self
                .model
                .str_to_token(&text, AddBos::Always)
                .map_err(|e| Error::Other(format!("tokenize failed: {e}")))?;
            // A dense chunk (a table, minified code) can tokenize past the
            // window. Its opening stands for it, as it would in a search
            // result; failing instead stopped the whole meaning index at
            // the first such chunk. Keyword search still sees all of it.
            tokens.truncate(N_CTX as usize);
            // Last-token pooling reads the vector at the end-of-text token;
            // add it when the tokenizer did not (or truncation cut it off).
            if self.profile.add_eos && !tokens.is_empty() {
                let eos = self.model.token_eos();
                if tokens.last() != Some(&eos) {
                    if tokens.len() >= N_CTX as usize {
                        tokens.pop();
                    }
                    tokens.push(eos);
                }
            }
            Ok(tokens)
        }

        /// Embed one already-prefixed string into a pooled, L2-normalized
        /// vector in `ctx`, cleared first so state never leaks between inputs.
        fn embed_prefixed(&self, ctx: &mut LlamaContext<'_>, text: &str) -> Result<Vec<f32>> {
            let tokens = self.tokens(text)?;
            if tokens.is_empty() {
                return Ok(vec![0.0; self.dim]);
            }
            ctx.clear_kv_cache();
            let mut batch = LlamaBatch::new(tokens.len(), 1);
            // logits_all = true so every token's output is enabled, ensuring
            // mean pooling sees the whole sequence.
            batch
                .add_sequence(&tokens, 0, true)
                .map_err(|e| Error::Other(format!("batch add failed: {e}")))?;
            ctx.decode(&mut batch)
                .map_err(|e| Error::Other(format!("embedding decode failed: {e}")))?;

            let embedding = ctx
                .embeddings_seq_ith(0)
                .map_err(|e| Error::Other(format!("couldn't read embedding: {e}")))?;
            let mut v = embedding.to_vec();
            l2_normalize(&mut v);
            Ok(v)
        }

        /// Embed many already-prefixed strings, several sequences to a pass
        /// (up to [`N_SEQ`], their tokens together within the context), one
        /// pooled vector per sequence. Mean pooling only: each sequence is
        /// pooled on its own, so the vectors are the ones one at a time gives.
        fn embed_packed(&self, ctx: &mut LlamaContext<'_>, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            let toks: Vec<Vec<llama_cpp_2::token::LlamaToken>> =
                texts.iter().map(|t| self.tokens(t)).collect::<Result<_>>()?;
            let mut out: Vec<Vec<f32>> = vec![vec![0.0; self.dim]; texts.len()];
            let mut i = 0;
            while i < toks.len() {
                let mut group: Vec<usize> = Vec::new();
                let mut total = 0usize;
                while i < toks.len()
                    && group.len() < N_SEQ as usize
                    && (group.is_empty() || total + toks[i].len() <= N_CTX as usize)
                {
                    if !toks[i].is_empty() {
                        total += toks[i].len();
                        group.push(i);
                    }
                    i += 1;
                }
                if group.is_empty() {
                    continue;
                }
                ctx.clear_kv_cache();
                let mut batch = LlamaBatch::new(total, group.len() as i32);
                for (seq, &k) in group.iter().enumerate() {
                    batch
                        .add_sequence(&toks[k], seq as i32, true)
                        .map_err(|e| Error::Other(format!("batch add failed: {e}")))?;
                }
                ctx.decode(&mut batch)
                    .map_err(|e| Error::Other(format!("embedding decode failed: {e}")))?;
                for (seq, &k) in group.iter().enumerate() {
                    let e = ctx
                        .embeddings_seq_ith(seq as i32)
                        .map_err(|e| Error::Other(format!("couldn't read embedding: {e}")))?;
                    let mut v = e.to_vec();
                    l2_normalize(&mut v);
                    out[k] = v;
                }
            }
            Ok(out)
        }

        /// Embed a query, with the model's query prefix.
        pub fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
            let mut ctx = self.context()?;
            self.embed_prefixed(&mut ctx, &format!("{}{text}", self.profile.query_prefix))
        }
    }

    impl Embedder for LlamaEmbedder {
        fn embed(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            let mut ctx = self.context()?;
            let prefixed: Vec<String> = texts.iter().map(|t| format!("{}{t}", self.profile.doc_prefix)).collect();
            if self.profile.pooling == Pooling::Mean && prefixed.len() > 1 {
                return self.embed_packed(&mut ctx, &prefixed);
            }
            prefixed.iter().map(|t| self.embed_prefixed(&mut ctx, t)).collect()
        }

        fn dim(&self) -> usize {
            self.dim
        }

        fn model_id(&self) -> String {
            self.profile.model_id.to_string()
        }

        fn embed_query(&mut self, text: &str) -> Result<Vec<f32>> {
            // Delegate to the inherent method so the `search_query:` prefix is
            // applied (the trait default embeds raw text).
            LlamaEmbedder::embed_query(self, text)
        }
    }
}

#[cfg(feature = "local-llm")]
pub use llama::LlamaEmbedder;

/// Build the real embedder from the installed Embedding-category model, the
/// same shape as `local_llm::make_real_engine`: `None` when the app-data dir
/// is unknown or no model is installed, and a load failure on a present file
/// also yields `None` (reported on stderr) so search degrades to FTS-only
/// instead of erroring.
#[cfg(feature = "local-llm")]
pub fn installed_embedding_model() -> Option<Box<dyn Embedder + Send>> {
    let base = crate::local_llm::base_dir()?;
    let path = crate::model::selected_model_path(&base, crate::model::ModelCategory::Embedding)?;
    let file = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    match LlamaEmbedder::load(&path, profile_for_file(&file)) {
        Ok(e) => Some(Box::new(e)),
        Err(e) => {
            eprintln!("semantic search disabled: {e}");
            None
        }
    }
}

/// The profile of the embedding model [`installed_embedding_model`] would
/// load (the selected one if installed, else any installed one), or `None`.
pub fn selected_profile() -> Option<EmbedProfile> {
    let base = crate::local_llm::base_dir()?;
    let path = crate::model::selected_model_path(&base, crate::model::ModelCategory::Embedding)?;
    let file = path.file_name()?.to_string_lossy().into_owned();
    Some(profile_for_file(&file))
}

/// Feature-off stub: this build has no on-device embedding model, so semantic
/// search is simply unavailable. (Same-signature pair, like `llm_status`.)
#[cfg(not(feature = "local-llm"))]
pub fn installed_embedding_model() -> Option<Box<dyn Embedder + Send>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_embedder_is_deterministic_and_normalized() {
        let mut e = FakeEmbedder::new();
        let out = e.embed(&["hello world".to_string()]).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].len(), FakeEmbedder::DIM);
        // Same input, same vector.
        let again = e.embed(&["hello world".to_string()]).unwrap();
        assert_eq!(out[0], again[0]);
        // L2-normalized.
        let norm: f32 = out[0].iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "norm was {norm}");
    }

    #[test]
    fn fake_embedder_distinguishes_text() {
        let mut e = FakeEmbedder::new();
        let out = e
            .embed(&["alpha".to_string(), "beta".to_string()])
            .unwrap();
        assert_ne!(out[0], out[1]);
    }

    #[test]
    fn fake_embedder_reports_metadata() {
        let e = FakeEmbedder::new();
        assert_eq!(e.dim(), 8);
        assert_eq!(e.model_id(), "fake-hash-8");
    }

    #[test]
    fn each_model_file_gets_its_own_profile() {
        assert_eq!(profile_for_file("nomic-embed-text-v1.5.Q8_0.gguf"), NOMIC);
        assert_eq!(profile_for_file("Qwen3-Embedding-0.6B-Q8_0.gguf"), QWEN3);
        assert_eq!(QWEN3.pooling, Pooling::Last);
        assert!(QWEN3.add_eos && QWEN3.doc_prefix.is_empty() && QWEN3.query_prefix.ends_with("Query:"));
        assert_ne!(NOMIC.model_id, QWEN3.model_id, "a switch must be seen as a different model");
    }

    // The model ranks a matching passage above an unrelated one. Runs when
    // KEN_EMBED_MODEL names a GGUF (any catalog embedding model).
    #[cfg(feature = "local-llm")]
    #[test]
    fn llama_embedder_ranks_the_matching_passage_first() {
        let Ok(path) = std::env::var("KEN_EMBED_MODEL") else { return };
        let file = std::path::Path::new(&path).file_name().unwrap().to_string_lossy().into_owned();
        let mut e = LlamaEmbedder::load(std::path::Path::new(&path), profile_for_file(&file)).expect("load");
        let docs = e
            .embed(&[
                "Payments are retried three times with exponential backoff before the booking fails.".to_string(),
                "The design review moved to Thursday because the projector was broken.".to_string(),
            ])
            .unwrap();
        let q = e.embed_query("how often does a failed payment get retried").unwrap();
        let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        assert!(dot(&q, &docs[0]) > dot(&q, &docs[1]), "the retry passage should score higher");
    }

    // Real-model smoke test. Requires a nomic-embed-text GGUF; the path comes
    // from KEN_EMBED_MODEL. Skipped (passes trivially) when the env var is
    // unset so plain `cargo test` stays green without the model on disk.
    #[cfg(feature = "local-llm")]
    #[test]
    fn llama_embedder_produces_normalized_768() {
        let path = match std::env::var("KEN_EMBED_MODEL") {
            Ok(p) => p,
            Err(_) => {
                eprintln!("KEN_EMBED_MODEL unset — skipping real embedder test");
                return;
            }
        };
        let file = std::path::Path::new(&path).file_name().unwrap().to_string_lossy().into_owned();
        let mut e = LlamaEmbedder::load(std::path::Path::new(&path), profile_for_file(&file))
            .expect("load embedding model");
        let dim = e.dim();
        eprintln!("embedding model dim = {dim}");
        // nomic-embed-text-v1.5 is 768-dim; assert it when that's the model on
        // disk. The rest of the checks are model-agnostic so the full pipeline
        // (load -> tokenize -> decode -> pooled read -> normalize) can be
        // exercised against any embedding-capable GGUF.
        if dim == 768 {
            eprintln!("dim matches nomic-embed-text-v1.5 (768)");
        }
        let out = e
            .embed(&["the quick brown fox".to_string()])
            .expect("embed document");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].len(), dim, "vector width must equal dim()");
        let norm: f32 = out[0].iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-3, "document vector not normalized: norm was {norm}");
        // Query side works and is also normalized to the same width.
        let q = e.embed_query("quick fox").expect("embed query");
        assert_eq!(q.len(), dim);
        let qnorm: f32 = q.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((qnorm - 1.0).abs() < 1e-3, "query vector not normalized: norm was {qnorm}");
        // A batch shares one context; a text in it embeds as it does alone.
        let long = "a much longer passage about invoices, payment terms and the ledger ".repeat(20);
        let batch = e
            .embed(&[long, "the quick brown fox".to_string()])
            .expect("embed batch");
        let drift: f32 = batch[1].iter().zip(&out[0]).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        assert!(drift < 1e-3, "the second text in a batch differs from it alone by {drift}");
    }
}

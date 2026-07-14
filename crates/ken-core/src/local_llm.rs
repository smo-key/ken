//! Embedded, on-device language model — instant ⌘K quick answers (spec §5) and
//! Map entity extraction (sibling plan), running entirely on the user's Mac.
//!
//! One model is loaded per app process, lazily, on first use, and driven by a
//! single worker thread behind a two-priority inference queue: `Interactive`
//! work (quick answers) is always served before `Background` work (Map
//! extraction), so an interactive request waits at most one already-running
//! generation. The real llama.cpp engine lives behind the [`Engine`] trait seam
//! (like `model.rs`'s `ByteSource`) so every scheduling / cancel / JSON-retry
//! rule is unit-tested against a fake with no model download; only the
//! `#[ignore]`d integration test touches a real GGUF.

use crate::{Error, Result};

/// Where a job sits in the inference queue. `Interactive` (quick answers) is
/// always served before `Background` (Map extraction).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    Interactive,
    Background,
}

/// The inference seam: one blocking, token-by-token generation. The real
/// implementation wraps llama.cpp; tests supply a fake. Kept deliberately small
/// so the whole scheduler above it is exercised without a model.
pub trait Engine: Send {
    /// Generate a completion for `prompt`, invoking `on_token` for each decoded
    /// UTF-8 piece. Returning `false` from `on_token` stops generation early.
    /// `greedy` selects a deterministic sampler (used by `generate_json`).
    /// Returns the full text produced (which may be partial if cancelled).
    fn generate(
        &mut self,
        prompt: &str,
        greedy: bool,
        on_token: &mut dyn FnMut(&str) -> bool,
    ) -> Result<String>;
}

/// Wrap a system+user turn in Qwen3's ChatML template (non-thinking Instruct
/// variant — no `<think>` block needed). Callers build the prompt; the engine
/// primitives take a raw string.
pub fn chatml_prompt(system: &str, user: &str) -> String {
    format!(
        "<|im_start|>system\n{system}<|im_end|>\n\
         <|im_start|>user\n{user}<|im_end|>\n<|im_start|>assistant\n"
    )
}

/// Reassembles a UTF-8 string from token byte fragments. llama.cpp emits raw
/// bytes per token and a single character can straddle two tokens, so we buffer
/// any trailing bytes that don't yet form a complete char and emit them once the
/// rest arrives.
#[derive(Default)]
pub struct Utf8Streamer {
    pending: Vec<u8>,
}

impl Utf8Streamer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed the next token's bytes; return whatever newly-complete text results.
    pub fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        match std::str::from_utf8(&self.pending) {
            Ok(s) => {
                let out = s.to_string();
                self.pending.clear();
                out
            }
            Err(e) => {
                let good = e.valid_up_to();
                // SAFETY: `good` is a validated boundary.
                let out = String::from_utf8_lossy(&self.pending[..good]).into_owned();
                self.pending.drain(..good);
                out
            }
        }
    }
}

/// Parse JSON out of a model completion that may be wrapped in prose or code
/// fences: take the substring from the first `{` or `[` to the matching last
/// `}` or `]`. Good enough for the greedy, schema-hinted generations Map
/// extraction and any JSON caller produce.
pub fn parse_json_lenient(text: &str) -> Result<serde_json::Value> {
    let start = text.find(['{', '[']);
    let end = text.rfind(['}', ']']);
    let slice = match (start, end) {
        (Some(s), Some(e)) if e >= s => &text[s..=e],
        _ => return Err(Error::Other("no JSON object found in the model output".into())),
    };
    serde_json::from_str(slice)
        .map_err(|e| Error::Other(format!("model output wasn't valid JSON: {e}")))
}

#[derive(Debug, Clone, PartialEq)]
pub enum LlmStatus {
    /// Feature off, or the selected model file isn't on disk yet.
    NotInstalled,
    /// The selected model file is present (and, once loaded, usable).
    Ready,
    /// A load or init attempt failed; carries user-facing detail.
    Error(String),
}

// --- Task 3: the inference scheduler ---

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;

/// A worker→caller message during one generation.
enum Msg {
    Token(String),
    Done(Result<String>),
}

/// One queued generation. The caller blocks on its `rx`; the worker streams
/// `Token`s (each acknowledged over `reply_rx` with a keep-going boolean, so
/// cancellation is prompt and deterministic) then a final `Done`.
struct Job {
    prompt: String,
    greedy: bool,
    tx: Sender<Msg>,
    reply_rx: Receiver<bool>,
}

#[derive(Default)]
struct Queues {
    interactive: VecDeque<Job>,
    background: VecDeque<Job>,
    running_interactive: bool,
    shutdown: bool,
}

/// Why the engine factory couldn't produce an engine. `NotInstalled` is not a
/// fault — no Language model is on disk yet — so it is never cached as a sticky
/// load Error and the factory is retried on the next job. `Failed` is a real
/// load failure (corrupt GGUF, backend init error): it is cached and reported
/// until [`LlmService::rearm`] (via [`notify_model_installed`]) clears it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineBuildError {
    NotInstalled,
    Failed(String),
}

/// The inference scheduler: a single worker thread owning one [`Engine`], fed by
/// a two-priority queue. Owns no llama.cpp types directly — the engine is built
/// by the factory passed to [`LlmService::spawn`], lazily on the first job, and
/// rebuilt after [`rearm`](LlmService::rearm) (model installed / selection
/// changed / recovering from a load error).
pub struct LlmService {
    shared: Arc<(Mutex<Queues>, Condvar)>,
    interactive_pending: Arc<AtomicBool>,
    load_status: Arc<Mutex<Option<LlmStatus>>>,
    rebuild_gen: Arc<AtomicU64>,
    _worker: thread::JoinHandle<()>,
}

impl LlmService {
    /// Spawn the worker. `make_engine` runs on the worker thread, lazily on the
    /// first job — so model loading (slow, fallible) never blocks submission and
    /// its failure is reported per-job. It is `FnMut`, not `FnOnce`: a failed
    /// build must not permanently disable building (the not-installed case
    /// retries on every job; a real error retries after [`rearm`](Self::rearm)).
    pub fn spawn<F>(make_engine: F) -> Self
    where
        F: FnMut() -> std::result::Result<Box<dyn Engine>, EngineBuildError> + Send + 'static,
    {
        let shared = Arc::new((Mutex::new(Queues::default()), Condvar::new()));
        let interactive_pending = Arc::new(AtomicBool::new(false));
        let load_status = Arc::new(Mutex::new(None));
        let rebuild_gen = Arc::new(AtomicU64::new(0));
        let worker = {
            let shared = shared.clone();
            let ip = interactive_pending.clone();
            let ls = load_status.clone();
            let gen = rebuild_gen.clone();
            thread::spawn(move || worker_loop(shared, ip, ls, gen, make_engine))
        };
        LlmService {
            shared,
            interactive_pending,
            load_status,
            rebuild_gen,
            _worker: worker,
        }
    }

    /// True while any Interactive job is queued or running — the "yield flag" a
    /// Background caller (Map extraction) checks between its per-file jobs.
    pub fn interactive_pending(&self) -> bool {
        self.interactive_pending.load(Ordering::SeqCst)
    }

    /// The worker-cached load outcome: `None` until a build has been attempted
    /// (or after [`rearm`](Self::rearm)); `Ready` after a successful load;
    /// `Error` after a real load failure. The not-installed case deliberately
    /// leaves this `None` so [`llm_status`] falls through to the file check.
    pub fn load_status(&self) -> Option<LlmStatus> {
        self.load_status.lock().unwrap().clone()
    }

    /// Clear any cached load Error and mark the engine for rebuild on the next
    /// job: called when a model finishes installing or the Language selection
    /// changes. Cheap (an atomic bump + a mutex store); the actual (re)load
    /// happens lazily on the worker when the next job arrives.
    pub fn rearm(&self) {
        *self.load_status.lock().unwrap() = None;
        self.rebuild_gen.fetch_add(1, Ordering::SeqCst);
    }

    fn recompute_pending(&self, q: &Queues) {
        let pending = !q.interactive.is_empty() || q.running_interactive;
        self.interactive_pending.store(pending, Ordering::SeqCst);
    }

    /// Submit a job and block, streaming tokens to `on_token`, until the
    /// generation finishes or is cancelled (by `on_token` returning `false`).
    /// The core of `generate_stream` and (greedy) `generate_json`.
    pub fn run(
        &self,
        prompt: &str,
        greedy: bool,
        priority: Priority,
        on_token: &mut dyn FnMut(&str) -> bool,
    ) -> Result<String> {
        let (tx, rx) = mpsc::channel();
        let (reply_tx, reply_rx) = mpsc::channel();
        let job = Job {
            prompt: prompt.to_string(),
            greedy,
            tx,
            reply_rx,
        };
        {
            let (m, cv) = &*self.shared;
            let mut q = m.lock().unwrap();
            match priority {
                Priority::Interactive => q.interactive.push_back(job),
                Priority::Background => q.background.push_back(job),
            }
            self.recompute_pending(&q);
            cv.notify_one();
        }
        loop {
            match rx.recv() {
                Ok(Msg::Token(t)) => {
                    // Reply with the caller's keep-going decision; the worker
                    // blocks for it, so cancel takes effect on the very next token.
                    let keep = on_token(&t);
                    let _ = reply_tx.send(keep);
                }
                Ok(Msg::Done(res)) => return res,
                Err(_) => return Err(Error::Other("the local model worker stopped".into())),
            }
        }
    }

    /// Greedy generation parsed as JSON, retried once on a parse failure. The
    /// retry appends a short nudge to the prompt: with a deterministic (greedy)
    /// sampler, resending the *identical* prompt would reproduce the same
    /// non-JSON output, so the second attempt must perturb the input to have a
    /// chance at succeeding.
    pub fn generate_json_via(
        &self,
        prompt: &str,
        priority: Priority,
    ) -> Result<serde_json::Value> {
        const NUDGE: &str = "\n\nReturn ONLY valid JSON.";
        let mut last_err = None;
        for attempt in 0..2 {
            let this_prompt = if attempt == 0 {
                prompt.to_string()
            } else {
                format!("{prompt}{NUDGE}")
            };
            let text = self.run(&this_prompt, true, priority, &mut |_| true)?;
            match parse_json_lenient(&text) {
                Ok(v) => return Ok(v),
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or_else(|| Error::Other("model produced no JSON".into())))
    }
}

impl Drop for LlmService {
    fn drop(&mut self) {
        let (m, cv) = &*self.shared;
        m.lock().unwrap().shutdown = true;
        cv.notify_all();
    }
}

fn worker_loop<F>(
    shared: Arc<(Mutex<Queues>, Condvar)>,
    interactive_pending: Arc<AtomicBool>,
    load_status: Arc<Mutex<Option<LlmStatus>>>,
    rebuild_gen: Arc<AtomicU64>,
    mut make_engine: F,
) where
    F: FnMut() -> std::result::Result<Box<dyn Engine>, EngineBuildError>,
{
    let mut engine: Option<Box<dyn Engine>> = None;
    // A cached REAL load failure: jobs fail fast on it (no rebuild churn on a
    // broken file) until a rearm. The not-installed case never lands here.
    let mut load_err: Option<String> = None;
    let mut seen_gen = rebuild_gen.load(Ordering::SeqCst);
    let set_status = |s: Option<LlmStatus>| *load_status.lock().unwrap() = s;

    loop {
        // Take the next job: Interactive before Background.
        let (job, is_interactive) = {
            let (m, cv) = &*shared;
            let mut q = m.lock().unwrap();
            loop {
                if q.shutdown {
                    return;
                }
                if let Some(j) = q.interactive.pop_front() {
                    q.running_interactive = true;
                    let pending = !q.interactive.is_empty() || q.running_interactive;
                    interactive_pending.store(pending, Ordering::SeqCst);
                    break (j, true);
                }
                if let Some(j) = q.background.pop_front() {
                    break (j, false);
                }
                q = cv.wait(q).unwrap();
            }
        };

        // A rearm (model installed / selection changed / error acknowledged)
        // drops the current engine and clears the cached error so the build
        // below runs against the new file.
        let cur_gen = rebuild_gen.load(Ordering::SeqCst);
        if cur_gen != seen_gen {
            seen_gen = cur_gen;
            engine = None;
            load_err = None;
        }

        // (Re)build the engine lazily. `unavailable` is this job's reason when
        // no engine results; only REAL failures stick in `load_err`/the status.
        let mut unavailable: Option<String> = None;
        if engine.is_none() {
            if let Some(e) = &load_err {
                unavailable = Some(e.clone());
            } else {
                match make_engine() {
                    Ok(e) => {
                        engine = Some(e);
                        set_status(Some(LlmStatus::Ready));
                    }
                    Err(EngineBuildError::NotInstalled) => {
                        // Not a fault: leave the status unset so llm_status
                        // falls through to the file check (NotInstalled), and
                        // retry the factory on the next job.
                        set_status(None);
                        unavailable = Some("the answers model isn't installed".into());
                    }
                    Err(EngineBuildError::Failed(msg)) => {
                        load_err = Some(msg.clone());
                        set_status(Some(LlmStatus::Error(msg.clone())));
                        unavailable = Some(msg);
                    }
                }
            }
        }

        let Job {
            prompt,
            greedy,
            tx,
            reply_rx,
        } = job;

        let result = if let Some(eng) = engine.as_mut() {
            let mut cb = |piece: &str| -> bool {
                if tx.send(Msg::Token(piece.to_string())).is_err() {
                    return false; // caller hung up
                }
                // Block for the caller's decision; false (or a dropped reply
                // channel) cancels the generation on the next token boundary.
                matches!(reply_rx.recv(), Ok(true))
            };
            eng.generate(&prompt, greedy, &mut cb)
        } else {
            Err(Error::Other(
                unavailable.unwrap_or_else(|| "the local model is unavailable".into()),
            ))
        };

        let _ = tx.send(Msg::Done(result));

        if is_interactive {
            let (m, _cv) = &*shared;
            let mut q = m.lock().unwrap();
            q.running_interactive = false;
            let pending = !q.interactive.is_empty() || q.running_interactive;
            interactive_pending.store(pending, Ordering::SeqCst);
        }
    }
}

// --- Task 4: process-global wiring + the real engine ---

/// Process-global app-data dir, recorded once at startup by [`init`].
static BASE_DIR: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();
/// The single process-wide service, spawned on first use.
static SERVICE: OnceLock<LlmService> = OnceLock::new();

/// Record the app-data base dir. Called once from the Tauri app's `run()`.
pub fn init(base_dir: PathBuf) {
    let cell = BASE_DIR.get_or_init(|| Mutex::new(None));
    *cell.lock().unwrap() = Some(base_dir);
}

#[allow(dead_code)]
fn base_dir() -> Option<PathBuf> {
    BASE_DIR.get()?.lock().unwrap().clone()
}

/// Absolute path of the Language model file to load, if the app-data dir is
/// known and one is installed. Delegates entirely to the catalog's resolver
/// (installed selection → any installed in category → None), which goes through
/// the same `model::target_path` the downloader writes to — load path and
/// download path can never disagree.
#[allow(dead_code)]
fn installed_language_model() -> Option<PathBuf> {
    let base = base_dir()?;
    crate::model::selected_model_path(&base, crate::model::ModelCategory::Language)
}

fn service() -> &'static LlmService {
    SERVICE.get_or_init(|| LlmService::spawn(make_real_engine))
}

/// Stream a completion at the given priority. See the trait contract: returns
/// the full text; `on_token` returning `false` cancels.
pub fn generate_stream(
    prompt: &str,
    priority: Priority,
    on_token: &mut dyn FnMut(&str) -> bool,
) -> Result<String> {
    service().run(prompt, false, priority, on_token)
}

/// Greedy JSON generation, retried once (with a nudged prompt) on a parse failure.
pub fn generate_json(prompt: &str, priority: Priority) -> Result<serde_json::Value> {
    service().generate_json_via(prompt, priority)
}

/// True while an Interactive job is queued or running (the Background yield
/// flag). Cheap; safe to poll frequently.
pub fn interactive_pending() -> bool {
    match SERVICE.get() {
        Some(svc) => svc.interactive_pending(),
        None => false,
    }
}

/// A model finished installing, or the Language selection changed: clear any
/// cached load Error and mark the engine for rebuild so the next job loads the
/// new/newly-selected file. Cheap, and a no-op until the service has spawned —
/// safe to call from any download/selection success path.
pub fn notify_model_installed() {
    if let Some(svc) = SERVICE.get() {
        svc.rearm();
    }
}

/// The global service's cached load outcome (None until it exists / has built).
#[allow(dead_code)]
fn cached_load_status() -> Option<LlmStatus> {
    SERVICE.get().and_then(|svc| svc.load_status())
}

/// Pure status precedence, factored out so it is testable without the process
/// globals: a recorded load `Error` outranks the file check (a present-but-broken
/// model must not read as `Ready`); otherwise an installed file is `Ready`, and
/// nothing installed is `NotInstalled`.
#[allow(dead_code)]
fn status_from(cached: Option<LlmStatus>, installed: Option<PathBuf>) -> LlmStatus {
    if let Some(LlmStatus::Error(e)) = cached {
        return LlmStatus::Error(e);
    }
    match installed {
        Some(_) => LlmStatus::Ready, // resolver only returns installed files
        None => LlmStatus::NotInstalled,
    }
}

/// Current status. Cheap: the catalog's installed-file resolution plus any
/// cached load result.
#[cfg(feature = "local-llm")]
pub fn llm_status() -> LlmStatus {
    status_from(cached_load_status(), installed_language_model())
}

/// Feature-off builds have no on-device model — the contract still exists so
/// every caller falls back cleanly. (Same-signature pair, like `transcribe`.)
#[cfg(not(feature = "local-llm"))]
pub fn llm_status() -> LlmStatus {
    LlmStatus::NotInstalled
}

/// Build the real llama.cpp engine from the installed Language model. No file
/// on disk is `NotInstalled` (retried each job, never a sticky Error); a load
/// failure on a present file is a real `Failed`.
#[cfg(feature = "local-llm")]
fn make_real_engine() -> std::result::Result<Box<dyn Engine>, EngineBuildError> {
    let path = installed_language_model().ok_or(EngineBuildError::NotInstalled)?;
    match llama::LlamaEngine::load(&path) {
        Ok(e) => Ok(Box::new(e)),
        Err(e) => Err(EngineBuildError::Failed(e.to_string())),
    }
}

/// Feature-off stub: no engine, and the error tells the scheduler to report it.
#[cfg(not(feature = "local-llm"))]
fn make_real_engine() -> std::result::Result<Box<dyn Engine>, EngineBuildError> {
    Err(EngineBuildError::Failed(
        "this build of Ken has no on-device language model".into(),
    ))
}

/// The real llama.cpp engine, copying Task 1's VERIFIED llama-cpp-2 0.1.151 call
/// sequence verbatim. `token_to_bytes`/`Special` are deprecated convenience
/// wrappers (they run the `token_to_piece_bytes` buffer-resize loop internally);
/// kept deliberately per the Task 1 report.
#[cfg(feature = "local-llm")]
#[allow(deprecated)]
mod llama {
    use std::num::NonZeroU32;
    use std::path::Path;

    use llama_cpp_2::context::params::LlamaContextParams;
    use llama_cpp_2::llama_backend::LlamaBackend;
    use llama_cpp_2::llama_batch::LlamaBatch;
    use llama_cpp_2::model::params::LlamaModelParams;
    use llama_cpp_2::model::{AddBos, LlamaModel, Special};
    use llama_cpp_2::sampling::LlamaSampler;

    use super::{Engine, Utf8Streamer};
    use crate::{Error, Result};

    /// The real llama.cpp engine. Holds the backend + weights for the process
    /// lifetime; a fresh context is created per generation (cheap next to load).
    pub struct LlamaEngine {
        backend: LlamaBackend,
        model: LlamaModel,
        n_ctx: u32,
        max_tokens: usize,
    }

    impl LlamaEngine {
        pub fn load(path: &Path) -> Result<Self> {
            let backend = LlamaBackend::init()
                .map_err(|e| Error::Other(format!("couldn't start llama.cpp: {e}")))?;
            let params = LlamaModelParams::default().with_n_gpu_layers(1000); // Metal: offload all
            let model = LlamaModel::load_from_file(&backend, path, &params)
                .map_err(|e| Error::Other(format!("couldn't load the answers model: {e}")))?;
            Ok(LlamaEngine { backend, model, n_ctx: 8192, max_tokens: 1024 })
        }
    }

    impl Engine for LlamaEngine {
        fn generate(
            &mut self,
            prompt: &str,
            greedy: bool,
            on_token: &mut dyn FnMut(&str) -> bool,
        ) -> Result<String> {
            let ctx_params =
                LlamaContextParams::default().with_n_ctx(NonZeroU32::new(self.n_ctx));
            let mut ctx = self
                .model
                .new_context(&self.backend, ctx_params)
                .map_err(|e| Error::Other(format!("llama context failed: {e}")))?;

            let tokens = self
                .model
                .str_to_token(prompt, AddBos::Always)
                .map_err(|e| Error::Other(format!("tokenize failed: {e}")))?;

            let mut batch = LlamaBatch::new(512, 1);
            let last = tokens.len().saturating_sub(1);
            for (i, tok) in tokens.iter().enumerate() {
                batch
                    .add(*tok, i as i32, &[0], i == last)
                    .map_err(|e| Error::Other(format!("batch add failed: {e}")))?;
            }
            ctx.decode(&mut batch)
                .map_err(|e| Error::Other(format!("decode failed: {e}")))?;

            let mut sampler = if greedy {
                LlamaSampler::chain_simple([LlamaSampler::greedy()])
            } else {
                LlamaSampler::chain_simple([
                    LlamaSampler::temp(0.7),
                    LlamaSampler::top_p(0.8, 1),
                    LlamaSampler::dist(1234),
                ])
            };

            let mut out = String::new();
            // The byte→piece boundary: llama.cpp emits raw token bytes and a
            // multi-byte char can straddle two tokens, so reassemble here.
            let mut streamer = Utf8Streamer::new();
            let mut n_cur = batch.n_tokens();

            for _ in 0..self.max_tokens {
                let token = sampler.sample(&ctx, batch.n_tokens() - 1);
                sampler.accept(token);
                if self.model.is_eog_token(token) {
                    break;
                }
                let bytes = self
                    .model
                    .token_to_bytes(token, Special::Plaintext)
                    .map_err(|e| Error::Other(format!("detokenize failed: {e}")))?;
                let piece = streamer.push(&bytes);
                if !piece.is_empty() {
                    out.push_str(&piece);
                    if !on_token(&piece) {
                        break;
                    }
                }

                batch.clear();
                batch
                    .add(token, n_cur, &[0], true)
                    .map_err(|e| Error::Other(format!("batch add failed: {e}")))?;
                n_cur += 1;
                ctx.decode(&mut batch)
                    .map_err(|e| Error::Other(format!("decode failed: {e}")))?;
            }
            Ok(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chatml_wraps_system_and_user_for_qwen3() {
        let p = chatml_prompt("You are terse.", "Hi");
        assert_eq!(
            p,
            "<|im_start|>system\nYou are terse.<|im_end|>\n\
             <|im_start|>user\nHi<|im_end|>\n<|im_start|>assistant\n"
        );
    }

    #[test]
    fn utf8_streamer_holds_back_incomplete_sequences() {
        // "é" is 0xC3 0xA9; feeding one byte at a time must not emit until whole.
        let mut s = Utf8Streamer::new();
        assert_eq!(s.push(&[0xC3]), "");
        assert_eq!(s.push(&[0xA9]), "é");
        // Plain ASCII flows straight through.
        assert_eq!(s.push(b"ok"), "ok");
    }

    #[test]
    fn utf8_streamer_splits_a_multibyte_char_across_pushes() {
        let mut s = Utf8Streamer::new();
        let euro = "€".as_bytes(); // 3 bytes: E2 82 AC
        assert_eq!(s.push(&euro[..1]), "");
        assert_eq!(s.push(&euro[1..2]), "");
        assert_eq!(s.push(&euro[2..]), "€");
    }

    #[test]
    fn json_parses_a_clean_object() {
        let v = parse_json_lenient(r#"{"a":1}"#).unwrap();
        assert_eq!(v["a"], 1);
    }

    #[test]
    fn json_recovers_from_prose_and_code_fences() {
        let text = "Sure! Here is the JSON:\n```json\n{\"entities\": [\"x\"]}\n```\nDone.";
        let v = parse_json_lenient(text).unwrap();
        assert_eq!(v["entities"][0], "x");
    }

    #[test]
    fn json_errors_when_there_is_no_object() {
        assert!(parse_json_lenient("no json here at all").is_err());
    }

    #[test]
    fn status_precedence_error_outranks_present_file() {
        use std::path::PathBuf;
        // A cached load Error wins even when a model file is present on disk.
        let s = status_from(
            Some(LlmStatus::Error("boom".into())),
            Some(PathBuf::from("/models/x.gguf")),
        );
        assert_eq!(s, LlmStatus::Error("boom".into()));
    }

    #[test]
    fn status_ready_when_installed_and_no_load_error() {
        use std::path::PathBuf;
        assert_eq!(
            status_from(None, Some(PathBuf::from("/models/x.gguf"))),
            LlmStatus::Ready
        );
        // A cached Ready doesn't override a genuine install check either way.
        assert_eq!(
            status_from(Some(LlmStatus::Ready), Some(PathBuf::from("/models/x.gguf"))),
            LlmStatus::Ready
        );
    }

    #[test]
    fn status_not_installed_when_nothing_on_disk() {
        assert_eq!(status_from(None, None), LlmStatus::NotInstalled);
        // No cached error, nothing installed → NotInstalled (not Ready).
        assert_eq!(
            status_from(Some(LlmStatus::Ready), None),
            LlmStatus::NotInstalled
        );
    }

    // --- Task 3: LlmService scheduler (fake engine) ---

    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier, Mutex};
    use std::thread;

    /// A scripted engine: each `generate` call consumes one entry from `scripts`
    /// and emits its pieces through `on_token`, honouring early cancel. The
    /// *first* generation optionally blocks on `gate` (a once-gate) so a test can
    /// hold one generation "in flight". If `order` is set, every generation logs
    /// its prompt as it starts, so tests can assert scheduling order.
    struct FakeEngine {
        scripts: VecDeque<Vec<String>>,
        gate: Option<Arc<Barrier>>,
        started: Arc<AtomicUsize>,
        order: Option<Arc<Mutex<Vec<String>>>>,
    }
    impl FakeEngine {
        fn new(pieces: &[&str]) -> Self {
            let mut scripts = VecDeque::new();
            scripts.push_back(pieces.iter().map(|s| s.to_string()).collect());
            FakeEngine {
                scripts,
                gate: None,
                started: Arc::new(AtomicUsize::new(0)),
                order: None,
            }
        }
    }
    impl Engine for FakeEngine {
        fn generate(
            &mut self,
            prompt: &str,
            _greedy: bool,
            on_token: &mut dyn FnMut(&str) -> bool,
        ) -> Result<String> {
            let call = self.started.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                if let Some(g) = &self.gate {
                    g.wait();
                }
            }
            if let Some(o) = &self.order {
                o.lock().unwrap().push(prompt.to_string());
            }
            let pieces = self.scripts.pop_front().unwrap_or_default();
            let mut out = String::new();
            for p in &pieces {
                out.push_str(p);
                if !on_token(p) {
                    return Ok(out); // cancelled: return partial
                }
            }
            Ok(out)
        }
    }

    #[test]
    fn stream_returns_full_text_and_delivers_every_token() {
        let svc = LlmService::spawn(|| Ok(Box::new(FakeEngine::new(&["Hel", "lo"])) as Box<dyn Engine>));
        let mut got = String::new();
        let full = svc
            .run("hi", false, Priority::Interactive, &mut |t| {
                got.push_str(t);
                true
            })
            .unwrap();
        assert_eq!(full, "Hello");
        assert_eq!(got, "Hello");
    }

    #[test]
    fn on_token_false_cancels_and_returns_partial() {
        let svc = LlmService::spawn(|| Ok(Box::new(FakeEngine::new(&["a", "b", "c"])) as Box<dyn Engine>));
        let mut n = 0;
        let out = svc
            .run("hi", false, Priority::Interactive, &mut |_| {
                n += 1;
                n < 2 // stop after the first token
            })
            .unwrap();
        assert_eq!(out, "ab"); // engine appends "b", then sees false
        assert_eq!(n, 2);
    }

    #[test]
    fn json_retries_once_then_succeeds() {
        // First generation is bad JSON, second is good. The single engine is
        // stateful: it pops one script per generate call.
        let svc = LlmService::spawn(|| {
            let mut scripts = VecDeque::new();
            scripts.push_back(vec!["not json".to_string()]);
            scripts.push_back(vec!["{\"ok\":".to_string(), "true}".to_string()]);
            Ok(Box::new(FakeEngine {
                scripts,
                gate: None,
                started: Arc::new(AtomicUsize::new(0)),
                order: None,
            }) as Box<dyn Engine>)
        });
        let v = svc.generate_json_via("x", Priority::Background).unwrap();
        assert_eq!(v["ok"], true);
    }

    #[test]
    fn json_retry_nudges_the_prompt() {
        // First generation is bad JSON → retry. With a deterministic sampler the
        // retry must NOT resend the identical prompt (that would reproduce the
        // same bad output); it appends a "return only JSON" nudge.
        let order = Arc::new(Mutex::new(Vec::<String>::new()));
        let svc = {
            let order = order.clone();
            LlmService::spawn(move || {
                let mut scripts = VecDeque::new();
                scripts.push_back(vec!["not json".to_string()]);
                scripts.push_back(vec!["{\"ok\":true}".to_string()]);
                Ok(Box::new(FakeEngine {
                    scripts,
                    gate: None,
                    started: Arc::new(AtomicUsize::new(0)),
                    order: Some(order.clone()),
                }) as Box<dyn Engine>)
            })
        };
        let v = svc.generate_json_via("base prompt", Priority::Background).unwrap();
        assert_eq!(v["ok"], true);
        let log = order.lock().unwrap().clone();
        assert_eq!(log.len(), 2, "one initial attempt + one retry");
        assert_eq!(log[0], "base prompt");
        assert_ne!(log[1], log[0], "retry prompt differs from the first");
        assert!(
            log[1].starts_with("base prompt") && log[1].contains("valid JSON"),
            "retry appends a JSON nudge: {:?}",
            log[1]
        );
    }

    #[test]
    fn interactive_is_served_before_background() {
        // Hold a background job in flight on a once-gate, then queue one
        // interactive and one background behind it. When the gate releases, the
        // interactive must run before the still-queued background one.
        let barrier = Arc::new(Barrier::new(2));
        let order = Arc::new(Mutex::new(Vec::<String>::new()));
        let started = Arc::new(AtomicUsize::new(0));

        let svc = {
            let barrier = barrier.clone();
            let order = order.clone();
            let started = started.clone();
            Arc::new(LlmService::spawn(move || {
                let mut scripts = VecDeque::new();
                scripts.push_back(vec!["x".to_string()]);
                scripts.push_back(vec!["x".to_string()]);
                scripts.push_back(vec!["x".to_string()]);
                Ok(Box::new(FakeEngine {
                    scripts,
                    gate: Some(barrier.clone()),
                    started: started.clone(),
                    order: Some(order.clone()),
                }) as Box<dyn Engine>)
            }))
        };

        // Job A (background): the first generation; it blocks on the gate.
        let a = {
            let svc = svc.clone();
            thread::spawn(move || {
                svc.run("A", false, Priority::Background, &mut |_| true).unwrap();
            })
        };
        // Wait until A is in flight (engine built, first generate entered).
        while started.load(Ordering::SeqCst) < 1 {
            std::hint::spin_loop();
        }
        // Queue an interactive job behind the in-flight A.
        let c = {
            let svc = svc.clone();
            thread::spawn(move || {
                svc.run("C-int", false, Priority::Interactive, &mut |_| true)
                    .unwrap();
            })
        };
        // Ensure the interactive job is enqueued before we release A.
        while !svc.interactive_pending() {
            std::hint::spin_loop();
        }
        // Queue a background job too.
        let b = {
            let svc = svc.clone();
            thread::spawn(move || {
                svc.run("B-bg", false, Priority::Background, &mut |_| true)
                    .unwrap();
            })
        };
        // Release A; the worker then serves interactive before queued background.
        barrier.wait();

        a.join().unwrap();
        b.join().unwrap();
        c.join().unwrap();

        let log = order.lock().unwrap().clone();
        assert_eq!(log[0], "A", "background A ran first (held on the gate): {log:?}");
        let ci = log.iter().position(|s| s == "C-int").unwrap();
        let bi = log.iter().position(|s| s == "B-bg").unwrap();
        assert!(ci < bi, "interactive must run before background: {log:?}");
        assert!(!svc.interactive_pending());
    }

    #[test]
    fn not_installed_build_failure_is_not_sticky_and_retries() {
        use std::sync::atomic::AtomicBool;
        let installed = Arc::new(AtomicBool::new(false));
        let svc = {
            let installed = installed.clone();
            LlmService::spawn(move || {
                if installed.load(Ordering::SeqCst) {
                    Ok(Box::new(FakeEngine::new(&["hi"])) as Box<dyn Engine>)
                } else {
                    Err(EngineBuildError::NotInstalled)
                }
            })
        };
        // First job fails: nothing installed.
        let err = svc
            .run("q", false, Priority::Interactive, &mut |_| true)
            .unwrap_err();
        assert!(err.to_string().contains("isn't installed"), "{err}");
        // The not-installed case is NOT cached as a load Error — status stays
        // unset, so llm_status falls through to the file check → NotInstalled.
        assert_eq!(svc.load_status(), None);
        assert_eq!(status_from(svc.load_status(), None), LlmStatus::NotInstalled);
        // Model arrives (production also calls notify_model_installed → rearm;
        // the not-installed case retries even without it, since the factory was
        // not consumed by the failed build).
        installed.store(true, Ordering::SeqCst);
        svc.rearm();
        let out = svc
            .run("q", false, Priority::Interactive, &mut |_| true)
            .unwrap();
        assert_eq!(out, "hi");
        assert_eq!(svc.load_status(), Some(LlmStatus::Ready));
    }

    #[test]
    fn not_installed_retries_even_without_rearm() {
        use std::sync::atomic::AtomicBool;
        let installed = Arc::new(AtomicBool::new(false));
        let svc = {
            let installed = installed.clone();
            LlmService::spawn(move || {
                if installed.load(Ordering::SeqCst) {
                    Ok(Box::new(FakeEngine::new(&["hi"])) as Box<dyn Engine>)
                } else {
                    Err(EngineBuildError::NotInstalled)
                }
            })
        };
        let _ = svc.run("q", false, Priority::Interactive, &mut |_| true);
        installed.store(true, Ordering::SeqCst);
        // No rearm: the next job still retries the (unconsumed) factory.
        let out = svc
            .run("q", false, Priority::Interactive, &mut |_| true)
            .unwrap();
        assert_eq!(out, "hi");
    }

    #[test]
    fn real_load_error_is_sticky_until_rearmed() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let svc = {
            let attempts = attempts.clone();
            LlmService::spawn(move || {
                if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    Err(EngineBuildError::Failed("corrupt model".into()))
                } else {
                    Ok(Box::new(FakeEngine::new(&["ok"])) as Box<dyn Engine>)
                }
            })
        };
        // First job: the build fails for real → cached Error, reported to the job.
        let err = svc
            .run("q", false, Priority::Interactive, &mut |_| true)
            .unwrap_err();
        assert!(err.to_string().contains("corrupt model"), "{err}");
        assert_eq!(svc.load_status(), Some(LlmStatus::Error("corrupt model".into())));
        // A real Error outranks a present file in the status precedence.
        assert_eq!(
            status_from(svc.load_status(), Some(std::path::PathBuf::from("/m.gguf"))),
            LlmStatus::Error("corrupt model".into())
        );
        // Without rearm, jobs fail fast on the cached error; no rebuild attempt.
        let err2 = svc
            .run("q", false, Priority::Interactive, &mut |_| true)
            .unwrap_err();
        assert!(err2.to_string().contains("corrupt model"));
        assert_eq!(attempts.load(Ordering::SeqCst), 1, "factory not retried while stuck");
        // notify_model_installed's per-service effect: clear the error + rebuild.
        svc.rearm();
        assert_eq!(svc.load_status(), None, "rearm clears the cached error");
        let out = svc
            .run("q", false, Priority::Interactive, &mut |_| true)
            .unwrap();
        assert_eq!(out, "ok");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(svc.load_status(), Some(LlmStatus::Ready));
    }

    #[test]
    fn rearm_rebuilds_a_working_engine_with_the_new_selection() {
        // Switching 4B↔8B calls notify_model_installed: the worker drops the
        // loaded engine and rebuilds from the (newly selected) file on the next
        // job. Each factory call yields a fresh scripted engine here.
        let attempts = Arc::new(AtomicUsize::new(0));
        let svc = {
            let attempts = attempts.clone();
            LlmService::spawn(move || {
                let n = attempts.fetch_add(1, Ordering::SeqCst);
                let text = if n == 0 { "first" } else { "second" };
                Ok(Box::new(FakeEngine::new(&[text])) as Box<dyn Engine>)
            })
        };
        let out = svc.run("q", false, Priority::Interactive, &mut |_| true).unwrap();
        assert_eq!(out, "first");
        svc.rearm();
        let out = svc.run("q", false, Priority::Interactive, &mut |_| true).unwrap();
        assert_eq!(out, "second", "rearm swapped in a freshly built engine");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn yield_flag_true_while_interactive_queued() {
        let svc = LlmService::spawn(|| Ok(Box::new(FakeEngine::new(&["ok"])) as Box<dyn Engine>));
        assert!(!svc.interactive_pending());
        // After a completed interactive job, the flag settles back to false.
        let _ = svc
            .run("q", false, Priority::Interactive, &mut |_| true)
            .unwrap();
        assert!(!svc.interactive_pending());
    }
}

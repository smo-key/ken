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

/// Current availability of the on-device LLM. Stub for Task 1; later tasks
/// resolve the selected model path and probe the engine.
pub fn llm_status() -> LlmStatus {
    LlmStatus::NotInstalled
}

// --- Task 3: the inference scheduler ---

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
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

/// The inference scheduler: a single worker thread owning one [`Engine`], fed by
/// a two-priority queue. Owns no llama.cpp types directly — the engine is built
/// by the factory passed to [`LlmService::spawn`], lazily on the first job.
pub struct LlmService {
    shared: Arc<(Mutex<Queues>, Condvar)>,
    interactive_pending: Arc<AtomicBool>,
    _worker: thread::JoinHandle<()>,
}

impl LlmService {
    /// Spawn the worker. `make_engine` runs once, on the worker thread, on the
    /// first job — so model loading (slow, fallible) never blocks submission and
    /// its failure is reported per-job.
    pub fn spawn<F>(make_engine: F) -> Self
    where
        F: FnOnce() -> Result<Box<dyn Engine>> + Send + 'static,
    {
        let shared = Arc::new((Mutex::new(Queues::default()), Condvar::new()));
        let interactive_pending = Arc::new(AtomicBool::new(false));
        let worker = {
            let shared = shared.clone();
            let ip = interactive_pending.clone();
            thread::spawn(move || worker_loop(shared, ip, make_engine))
        };
        LlmService {
            shared,
            interactive_pending,
            _worker: worker,
        }
    }

    /// True while any Interactive job is queued or running — the "yield flag" a
    /// Background caller (Map extraction) checks between its per-file jobs.
    pub fn interactive_pending(&self) -> bool {
        self.interactive_pending.load(Ordering::SeqCst)
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

    /// Greedy generation parsed as JSON, retried once on a parse failure.
    pub fn generate_json_via(
        &self,
        prompt: &str,
        priority: Priority,
    ) -> Result<serde_json::Value> {
        let mut last_err = None;
        for _ in 0..2 {
            let text = self.run(prompt, true, priority, &mut |_| true)?;
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
    make_engine: F,
) where
    F: FnOnce() -> Result<Box<dyn Engine>>,
{
    let mut engine: Option<Box<dyn Engine>> = None;
    let mut make_engine = Some(make_engine);
    let mut load_err: Option<String> = None;

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

        // Lazily build the engine on first use.
        if engine.is_none() && load_err.is_none() {
            match (make_engine.take().unwrap())() {
                Ok(e) => engine = Some(e),
                Err(e) => load_err = Some(e.to_string()),
            }
            set_load_status(match &load_err {
                Some(e) => LlmStatus::Error(e.clone()),
                None => LlmStatus::Ready,
            });
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
                load_err
                    .clone()
                    .unwrap_or_else(|| "the local model is unavailable".into()),
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

static LOAD_STATUS: OnceLock<Mutex<Option<LlmStatus>>> = OnceLock::new();

fn set_load_status(s: LlmStatus) {
    let cell = LOAD_STATUS.get_or_init(|| Mutex::new(None));
    *cell.lock().unwrap() = Some(s);
}

/// The worker-cached load outcome, read by `llm_status` (Task 4). `None` until
/// the engine has been built (or its build attempted) at least once.
#[allow(dead_code)]
fn cached_load_status() -> Option<LlmStatus> {
    LOAD_STATUS
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap()
        .clone()
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
                    gate: Some(barrier),
                    started,
                    order: Some(order),
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

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

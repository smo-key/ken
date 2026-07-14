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
}

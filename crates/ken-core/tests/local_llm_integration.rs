//! End-to-end smoke test for the real llama.cpp engine. Ignored by default —
//! run by hand with a model on disk:
//!
//!   KEN_TEST_LLM_MODEL=/path/Qwen3-4B-Instruct-2507-Q4_K_M.gguf \
//!     cargo test -p ken-core --features local-llm --test local_llm_integration \
//!     -- --ignored --nocapture
//!
//! It doubles as the compile-probe that pins the llama-cpp-2 0.1.151 API the
//! production `LlamaEngine` copies.
#![cfg(feature = "local-llm")]

use std::num::NonZeroU32;
use std::path::PathBuf;

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;

#[test]
#[ignore = "needs a real GGUF via KEN_TEST_LLM_MODEL"]
// token_to_bytes/Special are deprecated wrappers; kept deliberately — token_to_bytes
// already does the token_to_piece_bytes buffer-resize loop internally. Task 4 decides
// whether to inline.
#[allow(deprecated)]
fn loads_a_real_model_and_generates_a_few_tokens() {
    let Ok(path) = std::env::var("KEN_TEST_LLM_MODEL") else {
        eprintln!("set KEN_TEST_LLM_MODEL to run this");
        return;
    };
    let path = PathBuf::from(path);

    let backend = LlamaBackend::init().expect("backend init");
    let model_params = LlamaModelParams::default().with_n_gpu_layers(1000); // Metal: offload all
    let model =
        LlamaModel::load_from_file(&backend, &path, &model_params).expect("load model");

    let ctx_params = LlamaContextParams::default().with_n_ctx(NonZeroU32::new(4096));
    let mut ctx = model.new_context(&backend, ctx_params).expect("context");

    let prompt = "<|im_start|>user\nSay hello in three words.<|im_end|>\n<|im_start|>assistant\n";
    let tokens = model.str_to_token(prompt, AddBos::Always).expect("tokenize");

    let mut batch = LlamaBatch::new(512, 1);
    let last = tokens.len() - 1;
    for (i, tok) in tokens.iter().enumerate() {
        batch.add(*tok, i as i32, &[0], i == last).expect("batch add");
    }
    ctx.decode(&mut batch).expect("decode prompt");

    let mut sampler = LlamaSampler::chain_simple([LlamaSampler::greedy()]);
    let mut produced = String::new();
    let mut n_cur = batch.n_tokens();

    for _ in 0..16 {
        let token = sampler.sample(&ctx, batch.n_tokens() - 1);
        sampler.accept(token);
        if model.is_eog_token(token) {
            break;
        }
        // token_to_bytes so a multi-byte piece split across tokens is handled by
        // the caller's Utf8Streamer; here just check it is non-empty.
        let bytes = model
            .token_to_bytes(token, llama_cpp_2::model::Special::Plaintext)
            .expect("token to bytes");
        produced.push_str(&String::from_utf8_lossy(&bytes));

        batch.clear();
        batch.add(token, n_cur, &[0], true).expect("batch add gen");
        n_cur += 1;
        ctx.decode(&mut batch).expect("decode gen");
    }

    assert!(!produced.trim().is_empty(), "model produced no text");

    // --- Regression for the batch-overflow bug (a single backend/model init;
    // llama's backend is global and can only be initialized once per process,
    // so this lives in the same test rather than a second one). A prompt longer
    // than 512 tokens must decode: the old `LlamaBatch::new(512, 1)` returned
    // `InsufficientSpace` on the 513th `add`. The production fix sizes the batch
    // to the token count and lifts `n_batch` to the context window.
    let filler = "The quarterly review covered billing, vendors, and rollout. "
        .repeat(200);
    let long_prompt = format!(
        "<|im_start|>user\nSummarize in three words. Context:\n{filler}<|im_end|>\n<|im_start|>assistant\n"
    );
    let long_tokens = model.str_to_token(&long_prompt, AddBos::Always).expect("tokenize long");
    assert!(
        long_tokens.len() > 512,
        "prompt should exceed the old cap (got {} tokens)",
        long_tokens.len()
    );

    // n_batch must cover the whole prompt for a single decode (default is 2048).
    let n_ctx = 8192u32;
    let long_ctx_params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(n_ctx))
        .with_n_batch(n_ctx);
    let mut long_ctx = model.new_context(&backend, long_ctx_params).expect("long context");

    // Batch sized to the actual prompt length — the fix.
    let mut long_batch = LlamaBatch::new(long_tokens.len().max(1), 1);
    let long_last = long_tokens.len() - 1;
    for (i, tok) in long_tokens.iter().enumerate() {
        long_batch
            .add(*tok, i as i32, &[0], i == long_last)
            .expect("batch add must not overflow for a long prompt");
    }
    long_ctx.decode(&mut long_batch).expect("decode long prompt");

    let mut long_sampler = LlamaSampler::chain_simple([LlamaSampler::greedy()]);
    let mut long_produced = String::new();
    let mut long_n_cur = long_batch.n_tokens();
    for _ in 0..16 {
        let token = long_sampler.sample(&long_ctx, long_batch.n_tokens() - 1);
        long_sampler.accept(token);
        if model.is_eog_token(token) {
            break;
        }
        let bytes = model
            .token_to_bytes(token, llama_cpp_2::model::Special::Plaintext)
            .expect("token to bytes");
        long_produced.push_str(&String::from_utf8_lossy(&bytes));
        long_batch.clear();
        long_batch.add(token, long_n_cur, &[0], true).expect("batch add gen");
        long_n_cur += 1;
        long_ctx.decode(&mut long_batch).expect("decode gen");
    }
    eprintln!(
        "[long-prompt] {} tokens in → produced {:?}",
        long_tokens.len(), long_produced
    );
    assert!(
        !long_produced.trim().is_empty(),
        "model produced no text for a long prompt"
    );
}

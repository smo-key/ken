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
}

//! Repro/verification harness for the quit-time SIGABRT in ggml's Metal
//! backend (ggml_metal_rsets_free asserts no residency sets remain, but the
//! app's process-lifetime LLM service never drops its engine).
//!
//! Run with the real app-data dir; loads the installed Language model through
//! the exact same service path as the app, then exits:
//!
//!   cargo run --release -p ken-core --example llm_exit_repro <app-data-dir>            # plain exit(): expect SIGABRT (134)
//!   cargo run --release -p ken-core --example llm_exit_repro <app-data-dir> --hard-exit # libc::_exit(0): expect clean 0
//!
//! Not a #[test] because the interesting behavior IS the process exit status.

fn main() {
    let mut args = std::env::args().skip(1);
    let base = args.next().expect("usage: llm_exit_repro <app-data-dir> [--hard-exit]");
    let hard_exit = args.next().as_deref() == Some("--hard-exit");

    ken_core::local_llm::init(base.into());
    let mut out = String::new();
    let text = ken_core::local_llm::generate_stream(
        "Reply with the single word: ok",
        ken_core::local_llm::Priority::Interactive,
        &mut |tok| {
            out.push_str(tok);
            out.len() < 40 // stop early; we only need the engine resident
        },
    )
    .expect("generation failed — is a Language model installed?");
    println!("generated: {text:?}");
    println!("engine resident; exiting ({})", if hard_exit { "_exit" } else { "exit" });

    if hard_exit {
        unsafe { libc::_exit(0) };
    }
    // Plain return: libc exit() runs C++ static destructors, including ggml's
    // Metal device teardown, while the service's engine is still resident.
}

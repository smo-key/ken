//! macOS capture backends (cpal microphone, ScreenCaptureKit system audio) and
//! TCC permission probes. Placeholder for a later wave-4 Record batch — this
//! pure-logic batch adds no platform capture code. Kept as an (empty) module so
//! the `#[cfg(target_os = "macos")] pub mod mac;` declaration in `mod.rs`
//! compiles on macOS.

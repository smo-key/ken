//! On-device meeting recorder: capture the microphone ("Me") and/or macOS
//! system audio ("Them"), write each active source to a 16 kHz mono WAV, then
//! (on stop) transcribe with Whisper and merge the channels into one labeled
//! markdown transcript. Everything in this file is pure and hardware-free —
//! the platform capture backends live in `mac.rs` behind the `CaptureSource`
//! seam, so the state machine, meter math, resampling, and merge are all tested
//! with synthesized samples.

use std::path::Path;
use std::time::Duration;

use serde::Serialize;

use crate::{transcript, Error, Result};

#[cfg(target_os = "macos")]
pub mod mac;

/// The two capture channels. `Mic` is labeled "Me", `System` is "Them".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Mic,
    System,
}

/// Everything downstream runs at this rate; Whisper expects 16 kHz mono.
pub const TARGET_RATE: u32 = 16_000;

/// Whether a macOS privacy permission is granted, so the UI can guide the user.
/// `Unsupported` covers non-macOS / pre-13 system audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionStatus {
    Granted,
    Denied,
    NotDetermined,
    Unsupported,
}

/// System Settings deep links (macOS) for the inline permission guidance.
pub const MIC_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone";
pub const SCREEN_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&Source::Mic).unwrap(), "\"mic\"");
        assert_eq!(serde_json::to_string(&Source::System).unwrap(), "\"system\"");
    }
}

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

/// Root-mean-square level of a sample block, in [0, 1] for normalized audio —
/// what the live meter shows. Empty block reads as silence.
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
    (sum / samples.len() as f64).sqrt() as f32
}

/// Fold interleaved N-channel frames down to mono by averaging each frame.
/// `channels == 0` is treated as mono. A trailing partial frame (shouldn't
/// happen from a well-formed callback) is averaged over what's present.
pub fn downmix_to_mono(interleaved: &[f32], channels: u16) -> Vec<f32> {
    let ch = channels.max(1) as usize;
    if ch == 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks(ch)
        .map(|frame| frame.iter().copied().sum::<f32>() / frame.len() as f32)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&Source::Mic).unwrap(), "\"mic\"");
        assert_eq!(serde_json::to_string(&Source::System).unwrap(), "\"system\"");
    }

    #[test]
    fn rms_of_silence_and_full_scale() {
        assert_eq!(rms(&[]), 0.0);
        assert_eq!(rms(&[0.0, 0.0, 0.0]), 0.0);
        // A constant ±1 signal has RMS 1.
        assert!((rms(&[1.0, -1.0, 1.0, -1.0]) - 1.0).abs() < 1e-6);
        // Half-scale square wave → 0.5.
        assert!((rms(&[0.5, -0.5, 0.5, -0.5]) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn downmix_averages_channels() {
        // Mono passes through.
        assert_eq!(downmix_to_mono(&[0.2, -0.4, 0.6], 1), vec![0.2, -0.4, 0.6]);
        // Stereo interleaved L,R -> average per frame.
        assert_eq!(downmix_to_mono(&[1.0, 0.0, -1.0, 1.0], 2), vec![0.5, 0.0]);
        // channels=0 is treated as mono (defensive).
        assert_eq!(downmix_to_mono(&[0.3], 0), vec![0.3]);
    }
}

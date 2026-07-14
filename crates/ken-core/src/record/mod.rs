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

/// Stateful linear-interpolation resampler. Deliberately simple (linear, not
/// sinc): speech feeding Whisper doesn't need band-limited quality, it needs to
/// be dependency-free and testable. Carries a pending-input tail across
/// `process` calls so chunk boundaries don't glitch. `step` input samples are
/// advanced per output sample; non-integer ratios are fine.
pub struct LinearResampler {
    step: f64,
    pos: f64,       // read position within `buf`, in input samples
    buf: Vec<f32>,  // input not yet fully consumed
}

impl LinearResampler {
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        let step = if out_rate == 0 { 1.0 } else { in_rate as f64 / out_rate as f64 };
        LinearResampler { step, pos: 0.0, buf: Vec::new() }
    }

    /// Consume a native-rate chunk, emit as many 16 kHz samples as are fully
    /// bracketed by available input. The final partial sample waits for the
    /// next chunk (one-sample latency) or `finish`.
    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        self.buf.extend_from_slice(input);
        let mut out = Vec::new();
        while (self.pos as usize) + 1 < self.buf.len() {
            let i = self.pos as usize;
            let frac = self.pos - i as f64;
            let s = self.buf[i] as f64 * (1.0 - frac) + self.buf[i + 1] as f64 * frac;
            out.push(s as f32);
            self.pos += self.step;
        }
        let drop = self.pos as usize;
        if drop > 0 {
            self.buf.drain(0..drop);
            self.pos -= drop as f64;
        }
        out
    }

    /// Flush the trailing sample by holding the last input value, so the tail of
    /// a recording isn't lost. Idempotent-ish: call once at stop.
    pub fn finish(&mut self) -> Vec<f32> {
        if self.buf.is_empty() {
            return Vec::new();
        }
        let last = *self.buf.last().unwrap();
        self.buf.push(last); // gives the loop one more bracketed pair
        let out = self.process(&[]);
        self.buf.clear();
        self.pos = 0.0;
        out
    }
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

    #[test]
    fn resampler_identity_when_rates_match() {
        let mut r = LinearResampler::new(16_000, 16_000);
        let mut out = r.process(&[0.0, 0.25, 0.5, 0.75, 1.0]);
        out.extend(r.finish());
        // Same rate: output tracks input closely and preserves length.
        assert_eq!(out.len(), 5);
        assert!((out[0] - 0.0).abs() < 1e-6);
        assert!((out[4] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn resampler_downsamples_by_integer_ratio() {
        // 48k -> 16k is exactly 3:1: a length-9 block yields ~3 samples.
        let mut r = LinearResampler::new(48_000, 16_000);
        let input: Vec<f32> = (0..9).map(|i| i as f32).collect();
        let mut out = r.process(&input);
        out.extend(r.finish());
        assert_eq!(out.len(), 3);
        // Picks indices 0, 3, 6.
        assert!((out[0] - 0.0).abs() < 1e-6);
        assert!((out[1] - 3.0).abs() < 1e-6);
        assert!((out[2] - 6.0).abs() < 1e-6);
    }

    #[test]
    fn resampler_preserves_dc_across_chunks() {
        // A constant signal must stay constant even when split across calls
        // (state carries the pending tail between chunks). 44.1k -> 16k.
        let mut r = LinearResampler::new(44_100, 16_000);
        let mut out = Vec::new();
        for _ in 0..10 {
            out.extend(r.process(&vec![0.7f32; 441]));
        }
        out.extend(r.finish());
        assert!(!out.is_empty());
        for s in &out {
            assert!((s - 0.7).abs() < 1e-4, "DC drifted: {s}");
        }
        // ~10 * 441 input @44.1k = 0.1s -> ~1600 output samples (±a few).
        assert!((out.len() as i64 - 1600).abs() < 8, "len {}", out.len());
    }
}

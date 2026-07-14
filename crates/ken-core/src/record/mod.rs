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

/// Create a 16 kHz / mono / 16-bit-PCM WAV writer at `path`. The caller writes
/// `i16` samples (via `f32_to_i16`) and calls `finalize`.
pub fn create_wav(
    path: &Path,
) -> Result<hound::WavWriter<std::io::BufWriter<std::fs::File>>> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: TARGET_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    hound::WavWriter::create(path, spec)
        .map_err(|e| Error::Other(format!("couldn't create recording file: {e}")))
}

/// Normalize a float sample to 16-bit PCM, clamped to avoid wrap on overshoot.
pub fn f32_to_i16(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * 32767.0) as i16
}

/// Read a 16-bit PCM WAV back to `f32` in [-1, 1] — the form Whisper wants.
pub fn read_wav_f32(path: &Path) -> Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(path)
        .map_err(|e| Error::Other(format!("couldn't open recording file: {e}")))?;
    reader
        .samples::<i16>()
        .map(|s| s.map(|v| v as f32 / 32768.0).map_err(|e| Error::Other(e.to_string())))
        .collect()
}

/// The base name (no extension) shared by a recording's transcript and WAVs.
pub fn recording_stem(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> String {
    format!("{y:04}-{mo:02}-{d:02} {h:02}.{mi:02} Recording")
}

/// `m:ss`, or `h:mm:ss` past an hour — the readable duration for the header.
pub fn dur_hms(d: Duration) -> String {
    let secs = d.as_secs();
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// A non-colliding `<stem>.<ext>` within `dir`, appending " 2", " 3", … before
/// the extension when needed.
pub fn unique_name(dir: &Path, stem: &str, ext: &str) -> String {
    let mut name = format!("{stem}.{ext}");
    let mut n = 2;
    while dir.join(&name).exists() {
        name = format!("{stem} {n}.{ext}");
        n += 1;
    }
    name
}

/// The transcript document's metadata header, ending in a `---` rule.
pub fn metadata_header(
    y: i32,
    mo: u32,
    d: u32,
    h: u32,
    mi: u32,
    duration: Duration,
    mic: bool,
    system: bool,
) -> String {
    let sources = match (mic, system) {
        (true, true) => "Me (microphone), Them (system audio)",
        (true, false) => "Me (microphone)",
        (false, true) => "Them (system audio)",
        (false, false) => "none",
    };
    format!(
        "# Recording — {y:04}-{mo:02}-{d:02} {h:02}.{mi:02}\n\n\
         - Date: {y:04}-{mo:02}-{d:02} {h:02}:{mi:02}\n\
         - Duration: {}\n\
         - Sources: {sources}\n\n---\n",
        dur_hms(duration)
    )
}

/// A live audio source. The backend owns its own capture thread (cpal `Stream`
/// is `!Send`), calling `sink` with device-native interleaved f32 frames plus
/// the device's sample rate and channel count. `stop` ends capture. Behind this
/// seam the session's per-callback work (`ingest_frames`) is tested with a fake.
pub trait CaptureSource: Send {
    fn start(&mut self, sink: Box<dyn FnMut(&[f32], u32, u16) + Send>) -> Result<()>;
    fn stop(&mut self);
}

/// The per-callback core: downmix one device block to mono, resample it to
/// 16 kHz through the source's running resampler, and return the 16 kHz samples
/// (ready to write to the WAV) plus this block's meter level. Hardware-free.
pub fn ingest_frames(
    resampler: &mut LinearResampler,
    interleaved: &[f32],
    channels: u16,
) -> (Vec<f32>, f32) {
    let mono = downmix_to_mono(interleaved, channels);
    let out = resampler.process(&mono);
    let level = rms(&out);
    (out, level)
}

/// Body used when the storage choice is "audio" (WAVs kept, no transcription).
pub const AUDIO_ONLY_NOTE: &str =
    "_This recording's audio was saved without a transcript._\n";

/// Concatenate the metadata header (ends with `---\n`) and the transcript body.
pub fn build_document(header: &str, body: &str) -> String {
    format!("{header}\n{body}")
}

/// One channel's timed cues with an optional speaker label (`None` suppresses
/// labels for a single-source recording).
#[derive(Debug, Clone, Copy)]
pub struct LabeledChannel<'a> {
    pub label: Option<&'a str>,
    pub cues: &'a [transcript::Cue],
}

#[derive(Debug, Clone)]
struct Turn {
    label: Option<String>,
    start: Duration,
    text: String,
}

/// Flatten every channel's cues into turns, dropping blank text and trimming.
/// A stable sort by start keeps earlier channels ahead on ties (Me before Them,
/// given the caller passes Me first).
fn ordered_turns(channels: &[LabeledChannel]) -> Vec<Turn> {
    let mut turns: Vec<Turn> = Vec::new();
    for ch in channels {
        for c in ch.cues {
            let text = c.text.trim();
            if text.is_empty() {
                continue;
            }
            turns.push(Turn {
                label: ch.label.map(str::to_string),
                start: c.start,
                text: text.to_string(),
            });
        }
    }
    turns.sort_by(|a, b| a.start.cmp(&b.start));
    turns
}

/// Join back-to-back turns from the SAME labeled speaker into one paragraph.
/// Only labeled speakers coalesce; unlabeled (single-source) turns stay per-cue
/// so a solo transcript keeps its natural line breaks.
fn coalesce(turns: Vec<Turn>) -> Vec<Turn> {
    let mut out: Vec<Turn> = Vec::new();
    for t in turns {
        if let Some(last) = out.last_mut() {
            if t.label.is_some() && last.label == t.label {
                last.text.push(' ');
                last.text.push_str(&t.text);
                continue;
            }
        }
        out.push(t);
    }
    out
}

fn stamp(d: Duration) -> String {
    dur_hms(d)
}

/// Merge one or two labeled, timestamped channels into a single markdown
/// transcript. Turns are ordered by start; a labeled turn is
/// `**Label** [m:ss] text`, an unlabeled turn is `[m:ss] text`; turns are
/// separated by a blank line and the doc ends with a newline. Empty input →
/// empty string.
pub fn merge_transcript(channels: &[LabeledChannel]) -> String {
    let turns = coalesce(ordered_turns(channels));
    if turns.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for (i, t) in turns.iter().enumerate() {
        if i > 0 {
            out.push_str("\n\n");
        }
        match &t.label {
            Some(label) => out.push_str(&format!("**{label}** [{}] {}", stamp(t.start), t.text)),
            None => out.push_str(&format!("[{}] {}", stamp(t.start), t.text)),
        }
    }
    out.push('\n');
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Idle,
    Recording,
    Paused,
    Stopped,
}

/// The recorder's timing/phase bookkeeping — pure, so pause accounting is
/// tested without any audio device. `now_ms` is a monotonic millisecond clock
/// supplied by the caller (Tauri uses `Instant`).
#[derive(Debug, Clone)]
pub struct RecorderState {
    pub phase: Phase,
    pub mic: bool,
    pub system: bool,
    banked_ms: u64,
    segment_start: Option<u64>,
}

impl RecorderState {
    pub fn new() -> Self {
        RecorderState { phase: Phase::Idle, mic: false, system: false, banked_ms: 0, segment_start: None }
    }

    pub fn start(&mut self, now_ms: u64, mic: bool, system: bool) {
        self.phase = Phase::Recording;
        self.mic = mic;
        self.system = system;
        self.banked_ms = 0;
        self.segment_start = Some(now_ms);
    }

    pub fn pause(&mut self, now_ms: u64) {
        if self.phase == Phase::Recording {
            if let Some(s) = self.segment_start.take() {
                self.banked_ms += now_ms.saturating_sub(s);
            }
            self.phase = Phase::Paused;
        }
    }

    pub fn resume(&mut self, now_ms: u64) {
        if self.phase == Phase::Paused {
            self.segment_start = Some(now_ms);
            self.phase = Phase::Recording;
        }
    }

    pub fn stop(&mut self, now_ms: u64) {
        if self.phase == Phase::Recording {
            if let Some(s) = self.segment_start.take() {
                self.banked_ms += now_ms.saturating_sub(s);
            }
        }
        self.segment_start = None;
        self.phase = Phase::Stopped;
    }

    pub fn elapsed_ms(&self, now_ms: u64) -> u64 {
        self.banked_ms + self.segment_start.map(|s| now_ms.saturating_sub(s)).unwrap_or(0)
    }

    pub fn is_capturing(&self) -> bool {
        self.phase == Phase::Recording
    }
}

impl Default for RecorderState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_excludes_paused_time() {
        let mut r = RecorderState::new();
        assert_eq!(r.phase, Phase::Idle);
        r.start(1_000, true, false);
        assert_eq!(r.phase, Phase::Recording);
        assert_eq!(r.elapsed_ms(3_000), 2_000); // ran 1s..3s
        r.pause(3_000);
        assert_eq!(r.phase, Phase::Paused);
        // Time passes while paused; elapsed is frozen at 2s.
        assert_eq!(r.elapsed_ms(10_000), 2_000);
        r.resume(10_000);
        assert_eq!(r.elapsed_ms(11_000), 3_000); // 2s banked + 1s live
        r.stop(12_000);
        assert_eq!(r.phase, Phase::Stopped);
        assert_eq!(r.elapsed_ms(99_999), 4_000); // banked at stop, frozen
    }

    #[test]
    fn is_capturing_only_while_recording() {
        let mut r = RecorderState::new();
        assert!(!r.is_capturing());
        r.start(0, true, true);
        assert!(r.is_capturing());
        r.pause(500);
        assert!(!r.is_capturing());
    }

    fn cue(start_ms: u64, end_ms: u64, text: &str) -> transcript::Cue {
        transcript::Cue {
            start: Duration::from_millis(start_ms),
            end: Duration::from_millis(end_ms),
            text: text.into(),
        }
    }

    #[test]
    fn merge_interleaves_two_channels_by_start_and_labels() {
        let me = vec![cue(0, 2000, "hi there"), cue(6000, 8000, "sounds good")];
        let them = vec![cue(2500, 4000, "thanks for joining")];
        let md = merge_transcript(&[
            LabeledChannel { label: Some("Me"), cues: &me },
            LabeledChannel { label: Some("Them"), cues: &them },
        ]);
        let expected = "\
**Me** [0:00] hi there

**Them** [0:02] thanks for joining

**Me** [0:06] sounds good
";
        assert_eq!(md, expected);
    }

    #[test]
    fn merge_breaks_ties_me_before_them() {
        let me = vec![cue(1000, 2000, "first")];
        let them = vec![cue(1000, 2000, "second")];
        let md = merge_transcript(&[
            LabeledChannel { label: Some("Me"), cues: &me },
            LabeledChannel { label: Some("Them"), cues: &them },
        ]);
        assert_eq!(md, "**Me** [0:01] first\n\n**Them** [0:01] second\n");
    }

    #[test]
    fn merge_coalesces_consecutive_same_speaker_turns() {
        let me = vec![cue(0, 1000, "one"), cue(1000, 2000, "two"), cue(2000, 3000, "three")];
        let them = vec![cue(1000, 2000, "interject")];
        let md = merge_transcript(&[
            LabeledChannel { label: Some("Me"), cues: &me },
            LabeledChannel { label: Some("Them"), cues: &them },
        ]);
        // Ordered by start (stable, Me pushed before Them): one@0(Me),
        // two@1(Me), interject@1(Them), three@2(Me). Adjacent Me turns
        // "one"+"two" coalesce; Them breaks the run; Me "three" is its own turn.
        let expected = "\
**Me** [0:00] one two

**Them** [0:01] interject

**Me** [0:02] three
";
        assert_eq!(md, expected);
    }

    #[test]
    fn merge_single_channel_has_no_labels_but_keeps_timestamps() {
        let only = vec![cue(0, 2000, "solo line"), cue(4000, 5000, "next line")];
        let md = merge_transcript(&[LabeledChannel { label: None, cues: &only }]);
        // No labels; single-source turns are NOT coalesced (readability per cue).
        assert_eq!(md, "[0:00] solo line\n\n[0:04] next line\n");
    }

    #[test]
    fn merge_skips_empty_channel_and_blank_cues() {
        let me = vec![cue(0, 1000, "  hello  "), cue(1000, 2000, "   ")];
        let them: Vec<transcript::Cue> = vec![];
        let md = merge_transcript(&[
            LabeledChannel { label: Some("Me"), cues: &me },
            LabeledChannel { label: Some("Them"), cues: &them },
        ]);
        // Blank cue dropped; text trimmed; empty channel contributes nothing.
        assert_eq!(md, "**Me** [0:00] hello\n");
    }

    #[test]
    fn merge_of_nothing_is_empty() {
        assert_eq!(merge_transcript(&[]), "");
        let empty: Vec<transcript::Cue> = vec![];
        assert_eq!(
            merge_transcript(&[LabeledChannel { label: Some("Me"), cues: &empty }]),
            ""
        );
    }

    #[test]
    fn build_document_joins_header_and_body() {
        let header = metadata_header(2026, 7, 14, 14, 2, Duration::from_secs(5), true, false);
        let doc = build_document(&header, "[0:00] hello\n");
        assert!(doc.contains("# Recording — 2026-07-14 14.02"));
        assert!(doc.trim_end().ends_with("[0:00] hello"));
        // Audio-only note stands in for an absent transcript.
        let audio = build_document(&header, AUDIO_ONLY_NOTE);
        assert!(audio.contains("audio was saved without a transcript"));
    }

    /// A hardware-free source: on `start` it hands the sink one synthesized
    /// stereo block at 48 kHz, then reports stopped.
    struct FakeSource {
        started: bool,
    }
    impl CaptureSource for FakeSource {
        fn start(&mut self, mut sink: Box<dyn FnMut(&[f32], u32, u16) + Send>) -> Result<()> {
            self.started = true;
            // 480 stereo frames (10ms @48k) of a constant 0.5 in both channels.
            let block: Vec<f32> = std::iter::repeat(0.5).take(480 * 2).collect();
            sink(&block, 48_000, 2);
            Ok(())
        }
        fn stop(&mut self) {
            self.started = false;
        }
    }

    #[test]
    fn ingest_pipeline_downmixes_resamples_and_meters() {
        // Drive one callback through the same helper the live session uses.
        let mut resampler = LinearResampler::new(48_000, TARGET_RATE);
        let stereo: Vec<f32> = std::iter::repeat(0.5).take(480 * 2).collect();
        let (out, level) = ingest_frames(&mut resampler, &stereo, 2);
        // 480 frames @48k -> ~160 samples @16k.
        assert!((out.len() as i64 - 160).abs() <= 2, "len {}", out.len());
        // Constant 0.5 survives downmix + resample.
        for s in &out {
            assert!((s - 0.5).abs() < 1e-3);
        }
        // RMS of a constant 0.5 signal is 0.5.
        assert!((level - 0.5).abs() < 1e-3);
    }

    #[test]
    fn fake_source_feeds_the_sink() {
        use std::sync::{Arc, Mutex};
        let mut src = FakeSource { started: false };
        let seen = Arc::new(Mutex::new(Vec::<(usize, u32, u16)>::new()));
        let seen2 = seen.clone();
        src.start(Box::new(move |data, rate, ch| {
            seen2.lock().unwrap().push((data.len(), rate, ch));
        }))
        .unwrap();
        assert_eq!(seen.lock().unwrap().as_slice(), &[(480 * 2, 48_000, 2)]);
        src.stop();
    }

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

    #[test]
    fn wav_round_trips_16k_mono() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.wav");
        let samples: Vec<f32> = vec![0.0, 0.5, -0.5, 1.0, -1.0, 0.25];
        {
            let mut w = create_wav(&path).unwrap();
            for s in &samples {
                w.write_sample(f32_to_i16(*s)).unwrap();
            }
            w.finalize().unwrap();
        }
        let back = read_wav_f32(&path).unwrap();
        assert_eq!(back.len(), samples.len());
        for (a, b) in samples.iter().zip(back.iter()) {
            assert!((a - b).abs() < 1e-3, "{a} vs {b}");
        }
    }

    #[test]
    fn stem_and_duration_formatting() {
        assert_eq!(recording_stem(2026, 7, 14, 14, 2), "2026-07-14 14.02 Recording");
        assert_eq!(dur_hms(Duration::from_secs(9)), "0:09");
        assert_eq!(dur_hms(Duration::from_secs(754)), "12:34");
        assert_eq!(dur_hms(Duration::from_secs(3_661)), "1:01:01");
    }

    #[test]
    fn unique_name_avoids_collisions() {
        let dir = tempfile::tempdir().unwrap();
        let stem = "2026-07-14 14.02 Recording";
        assert_eq!(unique_name(dir.path(), stem, "md"), format!("{stem}.md"));
        std::fs::write(dir.path().join(format!("{stem}.md")), b"x").unwrap();
        assert_eq!(unique_name(dir.path(), stem, "md"), format!("{stem} 2.md"));
    }

    #[test]
    fn metadata_header_lists_both_sources() {
        let h = metadata_header(2026, 7, 14, 14, 2, Duration::from_secs(754), true, true);
        assert!(h.starts_with("# Recording — 2026-07-14 14.02\n"));
        assert!(h.contains("- Date: 2026-07-14 14:02"));
        assert!(h.contains("- Duration: 12:34"));
        assert!(h.contains("- Sources: Me (microphone), Them (system audio)"));
        assert!(h.trim_end().ends_with("---"));
        // Single source names only that source.
        let mic = metadata_header(2026, 7, 14, 9, 5, Duration::from_secs(30), true, false);
        assert!(mic.contains("- Sources: Me (microphone)"));
        assert!(!mic.contains("Them"));
    }
}

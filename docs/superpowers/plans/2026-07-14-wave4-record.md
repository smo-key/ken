# Wave 4 — Record Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a **Record** screen to Ken that captures the user's microphone ("Me") and/or macOS system audio ("Them") to 16 kHz mono WAV, then on stop transcribes each channel with the existing on-device Whisper path and merges them by segment start time into one labeled markdown transcript written into the project under `Recordings/` — where the normal scan/index picks it up (searchable, automation-eligible).

**Architecture:** Pure, hardware-free logic lives in a new `ken-core::record` module (recorder state machine, RMS meter math, downmix, a stateful linear resampler, WAV read/write wrappers, filename + metadata formatting, and the two-channel segment→markdown merge) — all unit-tested with synthesized samples behind a `CaptureSource` trait seam. The two real capture backends (cpal microphone, ScreenCaptureKit system audio) and the TCC permission probes are `#[cfg(target_os = "macos")]` shells over that core, verified by hand. Tauri commands + throttled events in `src-tauri/src/lib.rs` own a `RecordSession` that wires backends → downmix/resample → WAV writers → transcription → project write. The UI is `src/screens/RecordScreen.svelte` plus small `src/record/` components, following Paper & Ink (`.impeccable.md`).

**Tech Stack:** Rust (ken-core + Tauri 2), Svelte 5 runes. Reuses `transcript::transcribe` (whisper-rs behind the default-on `whisper` feature) and `scan::refresh_path` for indexing. New crates pinned below.

---

## Global Constraints

**Pinned crates** (add to `crates/ken-core/Cargo.toml`):

- `cpal = "0.18"` — microphone capture + input-device enumeration (cross-platform; used on macOS here). Real newest stable 0.18.1 (2026-06-07).
- `hound = "3.5"` — WAV encode/decode (16 kHz mono, 16-bit PCM). Newest 3.5.1.
- macOS-only, under `[target.'cfg(target_os = "macos")'.dependencies]`:
  - `screencapturekit = "8.0.0"` — **the pinned SCK choice.** Safe Rust bindings for Apple ScreenCaptureKit, audio-only capture on macOS 13+ (`with_captures_audio(true)`). Newest stable 8.0.0 (2026-06-19). Chosen over `objc2-screen-capture-kit` (raw, unsafe, hand-rolled delegates) and `cidre` (large, less idiomatic) for its ready-made `SCStreamOutputTrait` audio path and minimal deps (`apple-cf`/`apple-metal`). **The crate's API churns across majors (6.x→7.x→8.x within weeks); the first SCK task is a compile-probe that confirms exact symbol names against `cargo doc` before any real capture code.**
  - `objc2 = "0.6"`, `objc2-av-foundation = "0.3"` — microphone TCC authorization status/request (`AVCaptureDevice` audio media type). Exact symbol paths confirmed in the permissions compile-probe task.

**Resampling decision:** a **hand-written stateful linear-interpolation resampler in ken-core** (`LinearResampler`), NOT `rubato`. Rationale: device rate → 16 kHz for speech feeding Whisper (which is robust to mild resampling artifacts) does not need band-limited sinc quality; a linear resampler is ~30 lines, adds zero dependency, handles non-integer ratios (44 100 → 16 000), and — the deciding factor — becomes **testable ken-core logic** (synthesized-sample unit tests) rather than a thin untested shell over a crate.

**Audio format:** every active source records **16 kHz, mono, 16-bit PCM WAV**. Device callbacks deliver native-rate interleaved f32 (or i16→f32); the session downmixes to mono then resamples to 16 kHz before writing. On stop the WAV is read back to `f32` ([-1, 1]) and handed to `transcript::transcribe`.

**Tauri event names** (all throttled/emitted from `src-tauri`):

- `record-level` — `{ source: "mic" | "system", rms: f32 }`, throttled to ~10 Hz per source.
- `record-state` — `{ phase, elapsedMs, mic, system }` on every start/pause/resume/stop.
- `record-transcribing` — `{}` when stop begins transcription.
- `record-saved` — `{ relPath: string }` when the transcript doc is written + indexed.
- `record-error` — `{ message: string, canRetry: bool }`.

**File naming** (into project `Recordings/`):

- Transcript: `YYYY-MM-DD HH.MM Recording.md` (24-hour clock; minute-precision; `HH.MM` uses a dot because `:` is illegal on some filesystems). Collisions in the same minute get ` 2`, ` 3`, … before the extension.
- WAV, two sources: `YYYY-MM-DD HH.MM Recording - Me.wav` and `… - Them.wav`.
- WAV, single source: `YYYY-MM-DD HH.MM Recording.wav`.
- Markdown metadata header: `# Recording — YYYY-MM-DD HH.MM`, then a bullet list (Date, Duration `m:ss`/`h:mm:ss`, Sources), then `---`, then the transcript body.

**Storage choice** (chosen at/just-before stop, `"transcript" | "audio" | "both"`):

- `transcript` — transcribe → write `.md` → delete the temp WAVs **only after a successful transcription**.
- `audio` — keep the WAVs (moved into `Recordings/`), write a `.md` with the metadata header and a line noting audio was kept without a transcript; **no transcription performed**.
- `both` — transcribe → write `.md` → keep the WAVs alongside.
- **Failure rule:** a transcription failure keeps the audio regardless of choice and emits `record-error` with `canRetry: true`; nothing is deleted.

**Speaker labels:** two active sources → **Me** (mic) / **Them** (system audio). A single active source produces **no labels**. Merge orders turns by segment start; ties keep Me before Them.

**Permission strings / deep links** (macOS):

- `NSMicrophoneUsageDescription` in a new `src-tauri/Info.plist` (Tauri v2 injects it in `tauri dev` and merges it on bundle). Value: `"Ken records your microphone only while you press Record, to make a private on-device transcript."`
- Screen Recording has no usage-description key; it is a TCC prompt triggered on first capture. Detected with `CGPreflightScreenCaptureAccess()` / requested with `CGRequestScreenCaptureAccess()` (CoreGraphics).
- Deep links (opened via the existing `tauri-plugin-opener`): `x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone` and `…?Privacy_ScreenCapture`.

**Constraints on capture threading:** cpal `Stream` is `!Send` on macOS/CoreAudio. Each backend therefore runs its stream on a **dedicated thread** that builds + plays the stream and parks on a stop channel; the session stores only a `Sender<()>` stop signal, never the `Stream`.

---

## File Structure

```
crates/ken-core/
  Cargo.toml                      (edit: add cpal, hound; macos: screencapturekit, objc2, objc2-av-foundation)
  src/lib.rs                      (edit: `pub mod record;`)
  src/record/mod.rs               (new: pure logic + CaptureSource trait + PermissionStatus + URLs)
  src/record/mac.rs               (new: #[cfg(macos)] cpal MicSource, SCK SystemAudioSource, permission probes)
src-tauri/
  Info.plist                      (new: NSMicrophoneUsageDescription)
  Cargo.toml                      (edit: add chrono already present; no new deps needed)
  src/lib.rs                      (edit: RecordSession, commands, events, registration; AppState field)
src/
  lib/api.ts                      (edit: record commands + event listeners + DTOs)
  lib/app.svelte.ts               (edit: Screen union += "record")
  lib/record.svelte.ts            (new: RecordStore — live state, meters, elapsed, actions)
  shell/NavRail.svelte            (edit: Record nav item, Mic icon)
  shell/Shell.svelte              (edit: lazy RecordScreen pane)
  screens/RecordScreen.svelte     (new: Paper & Ink Record surface)
  record/LevelMeter.svelte        (new: quiet RMS bar)
  record/PermissionNotice.svelte  (new: inline "grant access" guidance with Open Settings)
```

Manual verification uses the project's `verify` skill (build/launch/observe). Hardware- and TCC-dependent steps cannot be automated and get explicit click-by-click checks.

---

## Tasks

### Task 1 — Scaffold the `record` module + `Source`/`PermissionStatus` types

- [ ] Add the pure module skeleton and register it. No capture code yet.

**Files:** `crates/ken-core/src/lib.rs` (edit), `crates/ken-core/src/record/mod.rs` (new)

Edit `crates/ken-core/src/lib.rs` — add in the `pub mod` list (alphabetical, after `pub mod project;`):

```rust
pub mod record;
```

Create `crates/ken-core/src/record/mod.rs`:

```rust
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
```

**Commands:**

```
cargo test -p ken-core record::
```

Expected: `test record::tests::source_serializes_lowercase ... ok`, build succeeds.

**Interfaces:** Produces `ken_core::record::{Source, PermissionStatus, TARGET_RATE, MIC_SETTINGS_URL, SCREEN_SETTINGS_URL}`.

**Commit:** `feat(record): scaffold ken-core record module`

---

### Task 2 — RMS meter math (TDD)

- [ ] Add `rms()` and its test.

**Files:** `crates/ken-core/src/record/mod.rs` (edit)

Add the failing test first (inside `mod tests`):

```rust
    #[test]
    fn rms_of_silence_and_full_scale() {
        assert_eq!(rms(&[]), 0.0);
        assert_eq!(rms(&[0.0, 0.0, 0.0]), 0.0);
        // A constant ±1 signal has RMS 1.
        assert!((rms(&[1.0, -1.0, 1.0, -1.0]) - 1.0).abs() < 1e-6);
        // Half-scale square wave → 0.5.
        assert!((rms(&[0.5, -0.5, 0.5, -0.5]) - 0.5).abs() < 1e-6);
    }
```

Run `cargo test -p ken-core record::rms` → **fails to compile** (`rms` undefined). Then add, above `mod tests`:

```rust
/// Root-mean-square level of a sample block, in [0, 1] for normalized audio —
/// what the live meter shows. Empty block reads as silence.
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
    (sum / samples.len() as f64).sqrt() as f32
}
```

**Commands:** `cargo test -p ken-core record::rms` → `ok`.

**Interfaces:** Produces `pub fn rms(&[f32]) -> f32`.

**Commit:** `feat(record): rms meter math`

---

### Task 3 — Downmix to mono (TDD)

- [ ] Add `downmix_to_mono()` and test.

**Files:** `crates/ken-core/src/record/mod.rs` (edit)

Failing test:

```rust
    #[test]
    fn downmix_averages_channels() {
        // Mono passes through.
        assert_eq!(downmix_to_mono(&[0.2, -0.4, 0.6], 1), vec![0.2, -0.4, 0.6]);
        // Stereo interleaved L,R -> average per frame.
        assert_eq!(downmix_to_mono(&[1.0, 0.0, -1.0, 1.0], 2), vec![0.5, 0.0]);
        // channels=0 is treated as mono (defensive).
        assert_eq!(downmix_to_mono(&[0.3], 0), vec![0.3]);
    }
```

Run → fails. Implement:

```rust
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
```

**Commands:** `cargo test -p ken-core record::downmix` → `ok`.

**Interfaces:** Produces `pub fn downmix_to_mono(&[f32], u16) -> Vec<f32>`.

**Commit:** `feat(record): mono downmix`

---

### Task 4 — Stateful linear resampler (TDD)

- [ ] Add `LinearResampler` (carries state across callback chunks) and tests.

**Files:** `crates/ken-core/src/record/mod.rs` (edit)

Failing tests:

```rust
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
```

Run → fails. Implement:

```rust
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
```

**Commands:** `cargo test -p ken-core record::resampler` → 3 tests `ok`.

**Interfaces:** Produces `LinearResampler::{new, process, finish}`.

**Commit:** `feat(record): stateful linear resampler`

---

### Task 5 — WAV write/read wrappers (TDD, round-trip)

- [ ] Add `hound` dep; add `create_wav`, `f32_to_i16`, `read_wav_f32`; round-trip test.

**Files:** `crates/ken-core/Cargo.toml` (edit), `crates/ken-core/src/record/mod.rs` (edit)

`Cargo.toml` — add under `[dependencies]`:

```toml
cpal = "0.18"
hound = "3.5"
```

(add cpal now too so later tasks don't re-touch the manifest; it's cross-platform and compiles on macOS.)

Failing test (needs `tempfile`, already a dev-dep):

```rust
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
```

Run → fails. Implement (above `mod tests`):

```rust
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
```

**Commands:** `cargo test -p ken-core record::wav` → `ok`.

**Interfaces:** Produces `create_wav`, `f32_to_i16`, `read_wav_f32`.

**Commit:** `feat(record): 16k mono WAV read/write`

---

### Task 6 — Filenames + metadata header (TDD)

- [ ] Add `recording_stem`, `unique_name`, `dur_hms`, `metadata_header`; tests.

**Files:** `crates/ken-core/src/record/mod.rs` (edit)

Failing tests:

```rust
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
```

Run → fails. Implement:

```rust
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
```

**Commands:** `cargo test -p ken-core record::` (runs stem/unique/header) → all `ok`.

**Interfaces:** Produces `recording_stem`, `dur_hms`, `unique_name`, `metadata_header`.

**Commit:** `feat(record): recording filenames and metadata header`

---

### Task 7 — Recorder state machine with pause accounting (TDD)

- [ ] Add `Phase` + `RecorderState`; tests for elapsed under pause/resume/stop.

**Files:** `crates/ken-core/src/record/mod.rs` (edit)

Failing tests:

```rust
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
```

Run → fails. Implement:

```rust
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
```

**Commands:** `cargo test -p ken-core record::` (elapsed/is_capturing) → `ok`.

**Interfaces:** Produces `Phase`, `RecorderState::{new, start, pause, resume, stop, elapsed_ms, is_capturing}`.

**Commit:** `feat(record): recorder state machine with pause accounting`

---

### Task 8 — Two-channel transcript merge (TDD — the core deliverable)

- [ ] Add `LabeledChannel` + `merge_transcript`; thorough tests (interleave, overlap, single-channel, empty channel, label suppression, coalescing).

**Files:** `crates/ken-core/src/record/mod.rs` (edit)

Failing tests — write all of these first:

```rust
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
```

Run → fails. Implement (above `mod tests`):

```rust
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
```

**Commands:** `cargo test -p ken-core record::merge` → all merge tests `ok`.

**Interfaces:** Produces `LabeledChannel<'a>`, `pub fn merge_transcript(&[LabeledChannel]) -> String`.

**Commit:** `feat(record): two-channel labeled transcript merge`

---

### Task 9 — Assemble the full document (TDD)

- [ ] Add `build_document(header, body)` joining header + transcript (or an audio-only note).

**Files:** `crates/ken-core/src/record/mod.rs` (edit)

Failing test:

```rust
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
```

Run → fails. Implement:

```rust
/// Body used when the storage choice is "audio" (WAVs kept, no transcription).
pub const AUDIO_ONLY_NOTE: &str =
    "_This recording's audio was saved without a transcript._\n";

/// Concatenate the metadata header (ends with `---\n`) and the transcript body.
pub fn build_document(header: &str, body: &str) -> String {
    format!("{header}\n{body}")
}
```

**Commands:** `cargo test -p ken-core record::build_document` → `ok`.

**Interfaces:** Produces `AUDIO_ONLY_NOTE`, `build_document`.

**Commit:** `feat(record): assemble transcript document`

---

### Task 10 — `CaptureSource` seam + hardware-free ingest pipeline (TDD)

- [ ] Add the `CaptureSource` trait and `ingest_frames`; test the pipeline with a `FakeSource` that emits synthesized frames — proving the state machine + downmix + resample + meter all compose with no hardware.

**Files:** `crates/ken-core/src/record/mod.rs` (edit)

Failing test:

```rust
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
```

Run → fails. Implement:

```rust
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
```

**Commands:** `cargo test -p ken-core record::` → ingest + fake-source tests `ok`; then run the whole module: `cargo test -p ken-core record` → all green.

**Interfaces:** Produces `trait CaptureSource { fn start(Box<dyn FnMut(&[f32],u32,u16)+Send>) -> Result<()>; fn stop(); }` and `pub fn ingest_frames(&mut LinearResampler, &[f32], u16) -> (Vec<f32>, f32)`.

**Commit:** `feat(record): capture-source seam and ingest pipeline`

---

### Task 11 — macOS: microphone device enumeration (compile-probe + manual verify)

- [ ] Create `mac.rs`; add cpal-based `list_input_devices()`. cpal is already a dep. No new manifest change.

**Files:** `crates/ken-core/src/record/mac.rs` (new)

Create `crates/ken-core/src/record/mac.rs`:

```rust
//! macOS capture backends and TCC permission probes. Everything here is a thin
//! shell over the pure `record` core; it is verified by hand (audio hardware
//! and TCC prompts can't be unit-tested).

use std::sync::mpsc::{self, Sender};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::record::{ingest_frames, CaptureSource, LinearResampler};
use crate::{Error, Result};

/// (id, display name) for each available input device. cpal identifies devices
/// by name on CoreAudio, so id == name here.
pub fn list_input_devices() -> Vec<(String, String)> {
    let host = cpal::default_host();
    let mut out = Vec::new();
    if let Ok(devices) = host.input_devices() {
        for d in devices {
            if let Ok(name) = d.name() {
                out.push((name.clone(), name));
            }
        }
    }
    out
}
```

Add to `crates/ken-core/src/record/mod.rs` (already has `#[cfg(target_os = "macos")] pub mod mac;` from Task 1).

**Commands:**

```
cargo build -p ken-core
cargo clippy -p ken-core
```

Expected: compiles clean.

**Manual verification** (needs the app; deferred until Task 17 wires a command — for now just confirm it compiles). Later, in the running app, the device picker must list at least the built-in mic (e.g. "MacBook Pro Microphone").

**Interfaces:** Produces `mac::list_input_devices() -> Vec<(String, String)>`.

**Commit:** `feat(record): cpal input-device enumeration (macos)`

---

### Task 12 — macOS: microphone capture backend (code + manual verify)

- [ ] Add `MicSource` running its stream on a dedicated thread (cpal `Stream` is `!Send`), feeding the sink.

**Files:** `crates/ken-core/src/record/mac.rs` (edit)

Append:

```rust
/// Microphone capture via cpal. The cpal `Stream` is `!Send` on CoreAudio, so
/// the stream is built and played on its OWN thread which then parks until a
/// stop signal arrives; only the `Sender` lives in the struct.
pub struct MicSource {
    device_name: Option<String>,
    stop_tx: Option<Sender<()>>,
}

impl MicSource {
    pub fn new(device_name: Option<String>) -> Self {
        MicSource { device_name, stop_tx: None }
    }
}

impl CaptureSource for MicSource {
    fn start(&mut self, sink: Box<dyn FnMut(&[f32], u32, u16) + Send>) -> Result<()> {
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();
        let device_name = self.device_name.clone();

        std::thread::spawn(move || {
            // A resampler per source, created once the device rate is known.
            let build = || -> Result<(cpal::Stream, ())> {
                let host = cpal::default_host();
                let device = match &device_name {
                    Some(n) => host
                        .input_devices()
                        .ok()
                        .and_then(|mut it| it.find(|d| d.name().ok().as_deref() == Some(n.as_str())))
                        .ok_or_else(|| Error::Other("that microphone isn't available".into()))?,
                    None => host
                        .default_input_device()
                        .ok_or_else(|| Error::Other("no microphone found".into()))?,
                };
                let supported = device
                    .default_input_config()
                    .map_err(|e| Error::Other(format!("microphone config error: {e}")))?;
                let rate = supported.sample_rate().0;
                let channels = supported.channels();
                let fmt = supported.sample_format();
                let config: cpal::StreamConfig = supported.into();

                let mut resampler = LinearResampler::new(rate, crate::record::TARGET_RATE);
                let mut sink = sink;
                let err_fn = |e| eprintln!("microphone stream error: {e}");

                let stream = match fmt {
                    cpal::SampleFormat::F32 => device.build_input_stream(
                        &config,
                        move |data: &[f32], _| {
                            let (samples, level) = ingest_frames(&mut resampler, data, channels);
                            sink(&samples, rate, channels);
                            let _ = level; // meter is derived by the session's wrapper sink
                        },
                        err_fn,
                        None,
                    ),
                    cpal::SampleFormat::I16 => device.build_input_stream(
                        &config,
                        move |data: &[i16], _| {
                            let f: Vec<f32> = data.iter().map(|&s| s as f32 / 32768.0).collect();
                            let (samples, _l) = ingest_frames(&mut resampler, &f, channels);
                            sink(&samples, rate, channels);
                        },
                        err_fn,
                        None,
                    ),
                    cpal::SampleFormat::U16 => device.build_input_stream(
                        &config,
                        move |data: &[u16], _| {
                            let f: Vec<f32> =
                                data.iter().map(|&s| (s as f32 - 32768.0) / 32768.0).collect();
                            let (samples, _l) = ingest_frames(&mut resampler, &f, channels);
                            sink(&samples, rate, channels);
                        },
                        err_fn,
                        None,
                    ),
                    other => return Err(Error::Other(format!("unsupported audio format: {other:?}"))),
                }
                .map_err(|e| Error::Other(format!("couldn't open the microphone: {e}")))?;

                stream
                    .play()
                    .map_err(|e| Error::Other(format!("couldn't start the microphone: {e}")))?;
                Ok((stream, ()))
            };

            match build() {
                Ok((stream, _)) => {
                    let _ = ready_tx.send(Ok(()));
                    let _ = stop_rx.recv(); // park until stop
                    drop(stream); // ends capture on this thread
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            }
        });

        // Surface a build/permission error synchronously to the caller.
        match ready_rx.recv() {
            Ok(Ok(())) => {
                self.stop_tx = Some(stop_tx);
                Ok(())
            }
            Ok(Err(e)) => Err(e),
            Err(_) => Err(Error::Other("microphone thread failed to start".into())),
        }
    }

    fn stop(&mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
    }
}
```

> **Note for the implementer:** the session (Task 17) wraps the raw `sink` so it also (a) writes samples to the WAV writer and (b) computes/throttles the meter. `ingest_frames` here already downmixes+resamples; the session's wrapper sink receives 16 kHz mono samples and just writes + meters them. Keep `ingest_frames` in ONE place — either here or in the wrapper, not both. **Decision:** do the downmix/resample in the session wrapper, and have `MicSource` pass device-native frames straight through (call `sink(data_as_f32, rate, channels)` with NO `ingest_frames`). Adjust the code above so each format arm converts to f32 and calls `sink(&f32_frames, rate, channels)` directly; the session owns the resampler. This keeps per-source resampler state with the session that also owns the writer.

Re-verify the note is applied: `MicSource` arms should be, e.g. for F32: `move |data: &[f32], _| sink(data, rate, channels)`; for I16/U16 convert then `sink(&f, rate, channels)`. Remove the `LinearResampler`/`ingest_frames` usage from `mac.rs`.

**Commands:** `cargo build -p ken-core && cargo clippy -p ken-core` → clean.

**Manual verification** (after Task 17 wiring; record it here as the acceptance for the mic path):
1. `pnpm tauri dev` (per `verify` skill), open a project, go to Record.
2. Toggle **Me** on, press Record. Grant the mic prompt the first time.
3. Speak; the **Me** meter must move. Stop with storage **audio**.
4. In `Recordings/`, the `… Recording.wav` opens in QuickLook and plays back your voice at normal pitch (confirms correct 16 kHz mono, no chipmunk/slow artifacts → resampler correct).

**Interfaces:** Produces `mac::MicSource::new(Option<String>)` implementing `CaptureSource`.

**Commit:** `feat(record): cpal microphone capture backend (macos)`

---

### Task 13 — macOS: ScreenCaptureKit compile-probe

- [ ] Add the `screencapturekit`/`objc2` deps; write a minimal call that lists shareable content, to pin the exact v8 API surface BEFORE writing capture code.

**Files:** `crates/ken-core/Cargo.toml` (edit), `crates/ken-core/src/record/mac.rs` (edit)

`Cargo.toml` — add at the end:

```toml
[target.'cfg(target_os = "macos")'.dependencies]
screencapturekit = "8.0.0"
objc2 = "0.6"
objc2-av-foundation = "0.3"
```

`mac.rs` — add a probe function and, temporarily, a `#[test]`-free `pub fn` that the implementer calls from a scratch `cargo run` or an `#[ignore]`d test to confirm symbols compile:

```rust
/// Compile-probe: confirm the pinned ScreenCaptureKit v8 API. This lists
/// shareable displays synchronously. Its ONLY purpose is to lock the exact
/// module paths / method names before the capture backend is written — the
/// crate's API changes across majors. Delete or fold into SystemAudioSource
/// once Task 14 lands.
#[allow(dead_code)]
pub fn sck_probe() -> Result<usize> {
    use screencapturekit::shareable_content::SCShareableContent;
    // NOTE: confirm the exact constructor/getter names against
    // `cargo doc -p screencapturekit --open` for 8.0.0 before relying on this.
    let content = SCShareableContent::get()
        .map_err(|e| Error::Other(format!("ScreenCaptureKit unavailable: {e}")))?;
    Ok(content.displays().len())
}
```

**Commands:**

```
cargo doc -p screencapturekit --no-deps
cargo build -p ken-core
```

Expected: `screencapturekit` compiles and links; `sck_probe` compiles. If a symbol name differs (e.g. `SCShareableContent::get_with_completion_handler`, `.displays()` vs `.get_displays()`), FIX the call to match the generated docs and note the confirmed names inline. This step's deliverable is a compiling probe + a comment recording the real symbol names for Task 14.

**Manual verification:** run the probe once (add a throwaway `#[test] #[ignore] fn probe() { assert!(sck_probe().unwrap() >= 1); }` and `cargo test -p ken-core probe -- --ignored --nocapture`). It should print/return ≥1 display (grant Screen Recording if prompted). Remove the throwaway test after.

**Interfaces:** Produces `mac::sck_probe()` (temporary) and, more importantly, confirmed SCK 8.0.0 symbol names recorded in comments.

**Commit:** `chore(record): screencapturekit compile-probe (macos)`

---

### Task 14 — macOS: system-audio capture backend (code + manual verify)

- [ ] Add `SystemAudioSource` — audio-only SCK stream, delegate pushes PCM to the sink. Uses the symbol names confirmed in Task 13.

**Files:** `crates/ken-core/src/record/mac.rs` (edit)

Append (adjust exact symbol names to the Task-13 findings; this is the intended shape):

```rust
use std::sync::{Arc, Mutex};

use screencapturekit::prelude::{SCStreamOutputTrait, SCStreamOutputType};
use screencapturekit::shareable_content::SCShareableContent;
use screencapturekit::stream::configuration::SCStreamConfiguration;
use screencapturekit::stream::content_filter::SCContentFilter;
use screencapturekit::stream::SCStream;

/// Receives audio sample buffers from SCK and forwards mono-ish f32 frames to
/// the session sink. SCK audio arrives as an AudioBufferList inside the
/// CMSampleBuffer; extract the interleaved f32 via the crate's audio-buffer
/// accessor (confirmed in Task 13).
struct AudioHandler {
    sink: Arc<Mutex<Box<dyn FnMut(&[f32], u32, u16) + Send>>>,
    sample_rate: u32,
    channels: u16,
}

impl SCStreamOutputTrait for AudioHandler {
    fn did_output_sample_buffer(
        &self,
        sample: screencapturekit::cm::CMSampleBuffer,
        of_type: SCStreamOutputType,
    ) {
        if of_type != SCStreamOutputType::Audio {
            return;
        }
        // Extract interleaved f32 PCM from the buffer's AudioBufferList. Use the
        // exact accessor confirmed in Task 13 (e.g. `sample.audio_buffer_list()`
        // / `CMSampleBufferExt`); collect channel 0..N interleaved into `frames`.
        if let Ok(frames) = extract_audio_f32(&sample) {
            if let Ok(mut sink) = self.sink.lock() {
                sink(&frames, self.sample_rate, self.channels);
            }
        }
    }
}

/// System audio capture via ScreenCaptureKit (macOS 13+). Captures audio only
/// (no frames rendered). Requires Screen Recording TCC permission.
pub struct SystemAudioSource {
    stream: Option<SCStream>,
}

impl SystemAudioSource {
    pub fn new() -> Self {
        SystemAudioSource { stream: None }
    }
}

impl CaptureSource for SystemAudioSource {
    fn start(&mut self, sink: Box<dyn FnMut(&[f32], u32, u16) + Send>) -> Result<()> {
        let content = SCShareableContent::get()
            .map_err(|e| Error::Other(format!("ScreenCaptureKit unavailable: {e}")))?;
        let display = content
            .displays()
            .into_iter()
            .next()
            .ok_or_else(|| Error::Other("no display to attach audio capture to".into()))?;

        // Audio-only: attach to a display filter but enable audio and keep the
        // video path minimal. Confirm builder names in Task 13.
        let sample_rate = 48_000u32;
        let channels = 2u16;
        let config = SCStreamConfiguration::new()
            .set_captures_audio(true)
            .map_err(|e| Error::Other(e.to_string()))?
            .set_sample_rate(sample_rate)
            .map_err(|e| Error::Other(e.to_string()))?
            .set_channel_count(channels)
            .map_err(|e| Error::Other(e.to_string()))?;

        let filter = SCContentFilter::new().with_display_excluding_windows(&display, &[]);

        let mut stream = SCStream::new(&filter, &config);
        stream.add_output_handler(
            AudioHandler {
                sink: Arc::new(Mutex::new(sink)),
                sample_rate,
                channels,
            },
            SCStreamOutputType::Audio,
        );
        stream
            .start_capture()
            .map_err(|e| Error::Other(format!("couldn't start system audio: {e}")))?;
        self.stream = Some(stream);
        Ok(())
    }

    fn stop(&mut self) {
        if let Some(stream) = self.stream.take() {
            let _ = stream.stop_capture();
        }
    }
}

/// Pull interleaved f32 PCM out of an SCK audio CMSampleBuffer. Implement using
/// the exact audio-buffer accessor confirmed in Task 13 (AudioBufferList →
/// per-channel f32 slices → interleave or take channel 0). Returns the frames
/// at the stream's configured rate/channel count.
fn extract_audio_f32(sample: &screencapturekit::cm::CMSampleBuffer) -> Result<Vec<f32>> {
    // Placeholder body to be replaced with the confirmed accessor in Task 13.
    // Must return interleaved f32 samples for the block, or an Err on malformed
    // buffers (which are then skipped).
    let _ = sample;
    Err(Error::Other("extract_audio_f32 not yet implemented".into()))
}
```

> **Implementer:** replace `extract_audio_f32`'s body and the exact SCK builder/handler symbol names with what Task 13's `cargo doc` confirmed. The AudioBufferList → f32 extraction is the crux; the crate exposes an audio-buffer accessor on the sample buffer (search the docs for `audio`, `AudioBuffer`, `CMSampleBufferExt`). Keep the function returning **interleaved f32 at the stream's rate/channels** so the session's downmix+resample handles the rest uniformly with the mic path.

**Commands:** `cargo build -p ken-core && cargo clippy -p ken-core` → clean (once `extract_audio_f32` is implemented).

**Manual verification** (after Task 17):
1. Play audio (a YouTube video / music) on the Mac.
2. In Record, toggle **Them** on, press Record. Grant Screen Recording TCC the first time (System Settings → Privacy & Security → Screen Recording → enable Ken → relaunch if macOS requires it).
3. The **Them** meter must move in time with the playing audio.
4. Stop with storage **audio**; the `… - Them.wav` (or `… Recording.wav` if solo) plays back the captured system audio.

**Interfaces:** Produces `mac::SystemAudioSource::new()` implementing `CaptureSource`.

**Commit:** `feat(record): screencapturekit system-audio capture (macos)`

---

### Task 15 — macOS: permission probes (code + manual verify)

- [ ] Add `mic_permission()`, `request_mic()`, `screen_permission()`, `request_screen()` returning/using `PermissionStatus`.

**Files:** `crates/ken-core/src/record/mac.rs` (edit)

Append (confirm objc2-av-foundation symbol paths against `cargo doc -p objc2-av-foundation`):

```rust
use crate::record::PermissionStatus;

// Screen Recording TCC lives in CoreGraphics (linked transitively by SCK).
extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

/// Current Screen Recording authorization — no prompt.
pub fn screen_permission() -> PermissionStatus {
    if unsafe { CGPreflightScreenCaptureAccess() } {
        PermissionStatus::Granted
    } else {
        // Preflight can't distinguish "denied" from "not yet asked"; treat as
        // NotDetermined so the UI offers "Allow", falling back to the Settings
        // deep link if the OS won't prompt again.
        PermissionStatus::NotDetermined
    }
}

/// Trigger the Screen Recording prompt (first time only; later a no-op that
/// returns the current state). Returns true if now granted.
pub fn request_screen() -> bool {
    unsafe { CGRequestScreenCaptureAccess() }
}

/// Microphone authorization via AVFoundation. Confirm the exact enum/method
/// names in Task 13's doc pass.
pub fn mic_permission() -> PermissionStatus {
    use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};
    let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(AVMediaTypeAudio) };
    match status {
        AVAuthorizationStatus::Authorized => PermissionStatus::Granted,
        AVAuthorizationStatus::NotDetermined => PermissionStatus::NotDetermined,
        _ => PermissionStatus::Denied,
    }
}

/// Ask for microphone access (async prompt). Fire-and-forget: the UI re-reads
/// `mic_permission()` after the user responds. Uses AVFoundation's
/// requestAccess completion handler.
pub fn request_mic() {
    use objc2_av_foundation::{AVCaptureDevice, AVMediaTypeAudio};
    unsafe {
        AVCaptureDevice::requestAccessForMediaType_completionHandler(
            AVMediaTypeAudio,
            &block2::StackBlock::new(|_granted: bool| {}),
        );
    }
}
```

> **Implementer:** the exact objc2-av-foundation call shapes (feature flags, `Retained<>`, `MainThreadMarker`, the block crate) must match the pinned versions — confirm via `cargo doc`. If `block2` isn't already available, add `block2 = "0.6"` under the macOS target deps. Keep the four public fns' signatures stable; only the bodies adapt.

**Commands:** `cargo build -p ken-core && cargo clippy -p ken-core` → clean.

**Manual verification:**
1. Reset TCC for a clean test: `tccutil reset Microphone dev.smokey.ken` and `tccutil reset ScreenCapture dev.smokey.ken`.
2. Launch; on Record, both notices should show "not yet granted".
3. Press Record with Me → mic prompt appears; grant → notice clears, meter works.
4. Toggle Them → screen prompt appears; grant → notice clears.
5. Deny a permission, reopen Record: the notice must show the **Open Settings** deep link, and clicking it opens the correct pane.

**Interfaces:** Produces `mac::{mic_permission, request_mic, screen_permission, request_screen}`.

**Commit:** `feat(record): microphone + screen-recording permission probes (macos)`

---

### Task 16 — Tauri: Info.plist microphone usage string + opener capability

- [ ] Add `src-tauri/Info.plist`; confirm `opener:default` already permits opening the settings URLs.

**Files:** `src-tauri/Info.plist` (new)

Create `src-tauri/Info.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>NSMicrophoneUsageDescription</key>
    <string>Ken records your microphone only while you press Record, to make a private on-device transcript.</string>
</dict>
</plist>
```

Tauri v2 injects this in `tauri dev` and merges it on bundle. No `tauri.conf.json` change needed. Screen Recording needs no usage-description key.

`src-tauri/capabilities/default.json` already grants `opener:default`, which permits `openUrl`. No capability change needed (verify by opening a settings URL in Task 17's manual check).

**Commands:**

```
pnpm tauri dev
```

Expected: app launches; no plist parse error in the Tauri log.

**Manual verification:** `codesign -d --entitlements - "$(...)"` is not needed; instead, after Task 17, pressing Record with Me shows the system mic prompt containing the usage string above (proves the plist is applied).

**Commit:** `feat(record): NSMicrophoneUsageDescription plist`

---

### Task 17 — Tauri: RecordSession, commands, and events

- [ ] Add the session struct + all `record_*` commands + throttled events; register them; add the `AppState` field.

**Files:** `src-tauri/src/lib.rs` (edit)

**17a — AppState field.** In `struct AppState` (around line 92) add:

```rust
    /// The single in-progress recording, if any (app-global: one recorder at a
    /// time). Holds its own state/writers behind its mutex.
    record: Arc<Mutex<Option<RecordSession>>>,
```

Initialize it where `AppState` is constructed (search for `model_downloads: Arc::new(Mutex::new(` in the setup and add `record: Arc::new(Mutex::new(None)),`).

**17b — DTOs + session.** Add near the other DTOs (e.g. after the model-download DTOs ~line 1516):

```rust
use ken_core::record::{self, CaptureSource, LinearResampler, RecorderState, Source};

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct AudioDeviceDto {
    id: String,
    name: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct RecordPermissionsDto {
    mic: record::PermissionStatus,
    screen: record::PermissionStatus,
    mic_settings_url: String,
    screen_settings_url: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct RecordLevel {
    source: Source,
    rms: f32,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct RecordStateDto {
    phase: record::Phase,
    elapsed_ms: u64,
    mic: bool,
    system: bool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct RecordSaved {
    rel_path: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct RecordErrorDto {
    message: String,
    can_retry: bool,
}

/// One live recording. Owns the phase/timing state, the per-source WAV writers
/// and resamplers, the meter throttles, and the capture backends. All mutation
/// happens under `AppState.record`'s mutex except the audio callbacks, which
/// write through `Arc<Mutex<..>>` channel state created at start.
struct RecordSession {
    state: RecorderState,
    started: Instant,
    tmp_dir: PathBuf,
    project_root: PathBuf,
    // Per-source capture backends (stopped on finish).
    mic: Option<Box<dyn CaptureSource>>,
    system: Option<Box<dyn CaptureSource>>,
    // Shared writer/meter state each callback appends to.
    mic_chan: Option<Arc<Mutex<ChannelWriter>>>,
    system_chan: Option<Arc<Mutex<ChannelWriter>>>,
}

/// A single source's WAV writer + resampler + meter throttle. The audio
/// callback locks this, downmixes/resamples the device block, writes 16 kHz
/// samples, and (throttled) emits a level event.
struct ChannelWriter {
    writer: hound::WavWriter<std::io::BufWriter<std::fs::File>>,
    resampler: LinearResampler,
    path: PathBuf,
    paused: bool,
    last_emit: Instant,
    app: AppHandle,
    source: Source,
}

impl ChannelWriter {
    fn feed(&mut self, device: &[f32], rate: u32, channels: u16) {
        if self.paused {
            return;
        }
        // Rebuild the resampler if the device rate differs from its assumption.
        // (Created with the true rate at start; here we trust `rate`.)
        let mono = record::downmix_to_mono(device, channels);
        let samples = self.resampler.process(&mono);
        let level = record::rms(&samples);
        for s in &samples {
            let _ = self.writer.write_sample(record::f32_to_i16(*s));
        }
        // ~10 Hz meter.
        if self.last_emit.elapsed().as_millis() >= 100 {
            self.last_emit = Instant::now();
            let _ = self.app.emit("record-level", RecordLevel { source: self.source, rms: level });
        }
    }
}
```

> Note the resampler rate: create each `ChannelWriter.resampler` lazily on the first callback (when `rate` is known) or, simpler, have the backend report its device rate up front. **Decision:** create the resampler on the first `feed` call for that channel: store `Option<LinearResampler>` and `resampler.get_or_insert_with(|| LinearResampler::new(rate, record::TARGET_RATE))`. Update the struct/field accordingly.

**17c — Commands.** Add these `#[tauri::command]`s (bodies complete):

```rust
#[tauri::command]
fn record_input_devices() -> CmdResult<Vec<AudioDeviceDto>> {
    #[cfg(target_os = "macos")]
    {
        Ok(record::mac::list_input_devices()
            .into_iter()
            .map(|(id, name)| AudioDeviceDto { id, name })
            .collect())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(Vec::new())
    }
}

#[tauri::command]
fn record_permissions() -> CmdResult<RecordPermissionsDto> {
    #[cfg(target_os = "macos")]
    let (mic, screen) = (record::mac::mic_permission(), record::mac::screen_permission());
    #[cfg(not(target_os = "macos"))]
    let (mic, screen) = (
        record::PermissionStatus::Unsupported,
        record::PermissionStatus::Unsupported,
    );
    Ok(RecordPermissionsDto {
        mic,
        screen,
        mic_settings_url: record::MIC_SETTINGS_URL.into(),
        screen_settings_url: record::SCREEN_SETTINGS_URL.into(),
    })
}

#[tauri::command]
fn record_request_permission(kind: String) -> CmdResult<()> {
    #[cfg(target_os = "macos")]
    match kind.as_str() {
        "mic" => record::mac::request_mic(),
        "screen" => {
            let _ = record::mac::request_screen();
        }
        _ => return Err("unknown permission".into()),
    }
    let _ = kind;
    Ok(())
}

#[tauri::command]
fn record_start(
    app: AppHandle,
    state: State<SharedState>,
    mic: bool,
    system: bool,
    device_id: Option<String>,
) -> CmdResult<()> {
    if !mic && !system {
        return Err("Pick at least one source to record.".into());
    }
    let (root, base) = {
        let guard = state.lock().unwrap();
        let active = guard.active.as_ref().ok_or("Open a project first.")?;
        (active.project.root.clone(), guard.base_dir.clone())
    };
    let mut slot = state.lock().unwrap().record.clone();
    // Guard against a second recording.
    {
        let existing = slot.lock().unwrap();
        if existing.is_some() {
            return Err("A recording is already in progress.".into());
        }
    }
    let id = uuid::Uuid::new_v4();
    let tmp_dir = base.join("record").join(id.to_string());
    std::fs::create_dir_all(&tmp_dir).map_err(err)?;

    let now = Instant::now();
    let mut sess = RecordSession {
        state: RecorderState::new(),
        started: now,
        tmp_dir: tmp_dir.clone(),
        project_root: root,
        mic: None,
        system: None,
        mic_chan: None,
        system_chan: None,
    };
    sess.state.start(0, mic, system);

    // Build each requested channel's writer + backend.
    #[cfg(target_os = "macos")]
    {
        if mic {
            let path = tmp_dir.join("me.wav");
            let writer = record::create_wav(&path).map_err(err)?;
            let chan = Arc::new(Mutex::new(ChannelWriter::new(writer, path, app.clone(), Source::Mic)));
            let chan2 = chan.clone();
            let mut src = Box::new(record::mac::MicSource::new(device_id.clone()));
            src.start(Box::new(move |data, rate, ch| chan2.lock().unwrap().feed(data, rate, ch)))
                .map_err(err)?;
            sess.mic = Some(src);
            sess.mic_chan = Some(chan);
        }
        if system {
            let path = tmp_dir.join("them.wav");
            let writer = record::create_wav(&path).map_err(err)?;
            let chan = Arc::new(Mutex::new(ChannelWriter::new(writer, path, app.clone(), Source::System)));
            let chan2 = chan.clone();
            let mut src = Box::new(record::mac::SystemAudioSource::new());
            src.start(Box::new(move |data, rate, ch| chan2.lock().unwrap().feed(data, rate, ch)))
                .map_err(err)?;
            sess.system = Some(src);
            sess.system_chan = Some(chan);
        }
    }

    let dto = RecordStateDto { phase: sess.state.phase, elapsed_ms: 0, mic, system };
    *slot.lock().unwrap() = Some(sess);
    let _ = app.emit("record-state", dto);
    Ok(())
}

#[tauri::command]
fn record_pause(app: AppHandle, state: State<SharedState>) -> CmdResult<()> {
    with_session(&state, |sess| {
        let now = sess.started.elapsed().as_millis() as u64;
        sess.state.pause(now);
        set_paused(sess, true);
        Ok(state_dto(sess))
    })
    .map(|dto| {
        let _ = app.emit("record-state", dto);
    })
}

#[tauri::command]
fn record_resume(app: AppHandle, state: State<SharedState>) -> CmdResult<()> {
    with_session(&state, |sess| {
        let now = sess.started.elapsed().as_millis() as u64;
        sess.state.resume(now);
        set_paused(sess, false);
        Ok(state_dto(sess))
    })
    .map(|dto| {
        let _ = app.emit("record-state", dto);
    })
}

#[tauri::command]
fn record_cancel(app: AppHandle, state: State<SharedState>) -> CmdResult<()> {
    let slot = { state.lock().unwrap().record.clone() };
    let sess = slot.lock().unwrap().take();
    if let Some(mut sess) = sess {
        stop_backends(&mut sess);
        let _ = std::fs::remove_dir_all(&sess.tmp_dir);
    }
    let _ = app.emit("record-state", RecordStateDto { phase: record::Phase::Idle, elapsed_ms: 0, mic: false, system: false });
    Ok(())
}

#[tauri::command]
fn record_stop(app: AppHandle, state: State<SharedState>, storage: String) -> CmdResult<()> {
    let slot = { state.lock().unwrap().record.clone() };
    let mut sess = slot.lock().unwrap().take().ok_or("No recording to stop.")?;
    let now = sess.started.elapsed().as_millis() as u64;
    sess.state.stop(now);
    let duration = Duration::from_millis(sess.state.elapsed_ms(now));
    stop_backends(&mut sess); // finalizes WAV writers

    let _ = app.emit("record-transcribing", ());
    // Do the slow transcription + write off the command thread.
    let root = sess.project_root.clone();
    let base = { state.lock().unwrap().base_dir.clone() };
    let mic_wav = sess.mic_chan.as_ref().map(|c| c.lock().unwrap().path.clone());
    let system_wav = sess.system_chan.as_ref().map(|c| c.lock().unwrap().path.clone());
    let mic_on = sess.state.mic;
    let system_on = sess.state.system;
    let tmp_dir = sess.tmp_dir.clone();
    let inner = state.inner().clone();

    std::thread::spawn(move || {
        let outcome = finish_recording(
            &app, &inner, &root, &base, &tmp_dir, &storage, mic_on, system_on,
            mic_wav, system_wav, duration,
        );
        if let Err(e) = outcome {
            let _ = app.emit("record-error", RecordErrorDto { message: e, can_retry: true });
        }
    });
    Ok(())
}
```

Helpers (complete):

```rust
impl ChannelWriter {
    fn new(
        writer: hound::WavWriter<std::io::BufWriter<std::fs::File>>,
        path: PathBuf,
        app: AppHandle,
        source: Source,
    ) -> Self {
        ChannelWriter {
            writer,
            resampler: LinearResampler::new(16_000, 16_000), // replaced on first feed
            resampler_ready: false,
            path,
            paused: false,
            last_emit: Instant::now(),
            app,
            source,
        }
    }
}

fn with_session<T>(
    state: &State<SharedState>,
    f: impl FnOnce(&mut RecordSession) -> CmdResult<T>,
) -> CmdResult<T> {
    let slot = { state.lock().unwrap().record.clone() };
    let mut guard = slot.lock().unwrap();
    let sess = guard.as_mut().ok_or("No recording in progress.")?;
    f(sess)
}

fn state_dto(sess: &RecordSession) -> RecordStateDto {
    let now = sess.started.elapsed().as_millis() as u64;
    RecordStateDto {
        phase: sess.state.phase,
        elapsed_ms: sess.state.elapsed_ms(now),
        mic: sess.state.mic,
        system: sess.state.system,
    }
}

fn set_paused(sess: &mut RecordSession, paused: bool) {
    if let Some(c) = &sess.mic_chan {
        c.lock().unwrap().paused = paused;
    }
    if let Some(c) = &sess.system_chan {
        c.lock().unwrap().paused = paused;
    }
}

fn stop_backends(sess: &mut RecordSession) {
    if let Some(mut s) = sess.mic.take() {
        s.stop();
    }
    if let Some(mut s) = sess.system.take() {
        s.stop();
    }
    // Finalize writers by dropping the Arc's inner writer via a flush call.
    for chan in [sess.mic_chan.take(), sess.system_chan.take()].into_iter().flatten() {
        if let Ok(c) = Arc::try_unwrap(chan) {
            let _ = c.into_inner().unwrap().writer.finalize();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn finish_recording(
    app: &AppHandle,
    state: &SharedState,
    root: &Path,
    _base: &Path,
    tmp_dir: &Path,
    storage: &str,
    mic_on: bool,
    system_on: bool,
    mic_wav: Option<PathBuf>,
    system_wav: Option<PathBuf>,
    duration: Duration,
) -> CmdResult<()> {
    use chrono::{Datelike, Local, Timelike};
    let now = Local::now();
    let (y, mo, d, h, mi) = (
        now.year(), now.month(), now.day(), now.hour(), now.minute(),
    );
    let recordings = root.join("Recordings");
    std::fs::create_dir_all(&recordings).map_err(err)?;
    let stem = record::recording_stem(y, mo, d, h, mi);
    let md_name = record::unique_name(&recordings, &stem, "md");
    let md_stem = md_name.trim_end_matches(".md").to_string();

    let header = record::metadata_header(y, mo, d, h, mi, duration, mic_on, system_on);
    let single = mic_on ^ system_on;

    let body = if storage == "audio" {
        record::AUDIO_ONLY_NOTE.to_string()
    } else {
        // Transcribe each present channel.
        let model = transcript::model_path(_base);
        if !model.is_file() {
            return Err("Download a transcription model in Settings to make transcripts.".into());
        }
        let mut me_cues = Vec::new();
        let mut them_cues = Vec::new();
        if let Some(p) = &mic_wav {
            let samples = record::read_wav_f32(p).map_err(err)?;
            me_cues = transcript::transcribe(&model, &samples).map_err(err)?;
        }
        if let Some(p) = &system_wav {
            let samples = record::read_wav_f32(p).map_err(err)?;
            them_cues = transcript::transcribe(&model, &samples).map_err(err)?;
        }
        let channels: Vec<record::LabeledChannel> = if single {
            vec![record::LabeledChannel {
                label: None,
                cues: if mic_on { &me_cues } else { &them_cues },
            }]
        } else {
            vec![
                record::LabeledChannel { label: Some("Me"), cues: &me_cues },
                record::LabeledChannel { label: Some("Them"), cues: &them_cues },
            ]
        };
        record::merge_transcript(&channels)
    };

    let doc = record::build_document(&header, &body);
    let md_rel = format!("Recordings/{md_name}");
    std::fs::write(recordings.join(&md_name), &doc).map_err(err)?;

    // Place WAVs per storage choice.
    let keep_audio = storage == "audio" || storage == "both";
    let place = |src: &Option<PathBuf>, suffix: &str| -> CmdResult<Option<String>> {
        let Some(src) = src else { return Ok(None) };
        if !keep_audio {
            let _ = std::fs::remove_file(src); // transcript-only: delete after success
            return Ok(None);
        }
        let wav_stem = if single { md_stem.clone() } else { format!("{md_stem} - {suffix}") };
        let name = record::unique_name(&recordings, &wav_stem, "wav");
        std::fs::rename(src, recordings.join(&name)).map_err(err)?;
        Ok(Some(format!("Recordings/{name}")))
    };
    let mic_rel = place(&mic_wav, "Me")?;
    let sys_rel = place(&system_wav, "Them")?;
    let _ = std::fs::remove_dir_all(tmp_dir);

    // Index the new files so they're searchable + automation-eligible.
    {
        let mut guard = state.lock().unwrap();
        if let Some(active) = guard.active.as_mut() {
            let _ = scan::refresh_path(&active.project, &mut active.db, &md_rel);
            for rel in [mic_rel, sys_rel].into_iter().flatten() {
                let _ = scan::refresh_path(&active.project, &mut active.db, &rel);
            }
        }
    }
    let _ = app.emit("index-updated", ScanStats::default());
    let _ = app.emit("record-saved", RecordSaved { rel_path: md_rel });
    let _ = app.emit(
        "record-state",
        RecordStateDto { phase: record::Phase::Idle, elapsed_ms: 0, mic: false, system: false },
    );
    Ok(())
}
```

Update `ChannelWriter` to add `resampler_ready: bool` and create the resampler on first feed:

```rust
    fn feed(&mut self, device: &[f32], rate: u32, channels: u16) {
        if self.paused {
            return;
        }
        if !self.resampler_ready {
            self.resampler = LinearResampler::new(rate, record::TARGET_RATE);
            self.resampler_ready = true;
        }
        let mono = record::downmix_to_mono(device, channels);
        let samples = self.resampler.process(&mono);
        let level = record::rms(&samples);
        for s in &samples {
            let _ = self.writer.write_sample(record::f32_to_i16(*s));
        }
        if self.last_emit.elapsed().as_millis() >= 100 {
            self.last_emit = Instant::now();
            let _ = self.app.emit("record-level", RecordLevel { source: self.source, rms: level });
        }
    }
```

Add a `record_retry` command that re-runs `finish_recording` reading WAVs still present in a kept `record/<id>` dir — **only needed if audio was retained**. Simpler v1: on transcription failure the audio was already moved to `Recordings/` (finish_recording writes the `.md` last; restructure so on the transcript path, WAVs are placed BEFORE transcription so a failure leaves them on disk). **Decision:** in `finish_recording`, when `storage != "audio"`, still place the WAVs into `Recordings/` first (as "both") if transcription then fails — but that violates transcript-only "delete after success". Resolve: for transcript-only, keep WAVs in `tmp_dir` until transcription succeeds; on failure, leave `tmp_dir` intact and emit `record-error{can_retry:true}`; `record_retry` re-reads `tmp_dir`. Implement `record_retry(recording_id)` accordingly, or (v1-simpler) have the error path MOVE the tmp WAVs into `Recordings/` and tell the user their audio was saved and they can re-transcribe by... — this is fiddly.

**Decision (final, simplest correct):** On any transcription failure, MOVE both WAVs into `Recordings/` (labeled) and write the `.md` with `AUDIO_ONLY_NOTE` plus an error line, then emit `record-error{can_retry:false}` with a message "Saved the audio; transcription failed: <e>. Re-run transcription from the file's context menu later." This keeps audio (the hard requirement), needs no retry-state machine, and the file lands indexed. Note the retry affordance as a known simplification. Implement this in the `finish_recording` error branch. Update the `record_stop` spawn to not emit a second error.

**17d — Registration.** In `tauri::generate_handler![ … ]` (ends ~line 3483) add before the closing `]`:

```rust
            record_input_devices,
            record_permissions,
            record_request_permission,
            record_start,
            record_pause,
            record_resume,
            record_stop,
            record_cancel,
```

**Commands:**

```
cargo build -p ken-app
cargo clippy -p ken-app
```

Expected: compiles clean.

**Manual verification:** deferred to Tasks 12/14/15 (they depend on this wiring) and Task 20 (the UI).

**Interfaces:** Produces commands `record_input_devices`, `record_permissions`, `record_request_permission`, `record_start`, `record_pause`, `record_resume`, `record_stop`, `record_cancel`; events `record-level`, `record-state`, `record-transcribing`, `record-saved`, `record-error`.

**Commit:** `feat(record): tauri session, commands, and events`

---

### Task 18 — Frontend API bindings

- [ ] Add DTOs, command wrappers, and event listeners to `src/lib/api.ts`.

**Files:** `src/lib/api.ts` (edit)

Add interfaces (after `ModelDownloadError`):

```ts
export type RecordPhase = "idle" | "recording" | "paused" | "stopped";
export type PermissionStatus = "granted" | "denied" | "notDetermined" | "unsupported";
export type RecordSourceName = "mic" | "system";
export type RecordStorage = "transcript" | "audio" | "both";

export interface AudioDevice {
  id: string;
  name: string;
}

export interface RecordPermissions {
  mic: PermissionStatus;
  screen: PermissionStatus;
  micSettingsUrl: string;
  screenSettingsUrl: string;
}

export interface RecordLevelEvent {
  source: RecordSourceName;
  rms: number;
}

export interface RecordStateEvent {
  phase: RecordPhase;
  elapsedMs: number;
  mic: boolean;
  system: boolean;
}

export interface RecordSavedEvent {
  relPath: string;
}

export interface RecordErrorEvent {
  message: string;
  canRetry: boolean;
}
```

Add to the `api` object (before the closing `}`):

```ts
  recordInputDevices: () => invoke<AudioDevice[]>("record_input_devices"),
  recordPermissions: () => invoke<RecordPermissions>("record_permissions"),
  recordRequestPermission: (kind: "mic" | "screen") =>
    invoke<void>("record_request_permission", { kind }),
  recordStart: (mic: boolean, system: boolean, deviceId: string | null) =>
    invoke<void>("record_start", { mic, system, deviceId }),
  recordPause: () => invoke<void>("record_pause"),
  recordResume: () => invoke<void>("record_resume"),
  recordStop: (storage: RecordStorage) => invoke<void>("record_stop", { storage }),
  recordCancel: () => invoke<void>("record_cancel"),

  onRecordLevel: (fn: (ev: RecordLevelEvent) => void): Promise<UnlistenFn> =>
    listen<RecordLevelEvent>("record-level", (e) => fn(e.payload)),
  onRecordState: (fn: (ev: RecordStateEvent) => void): Promise<UnlistenFn> =>
    listen<RecordStateEvent>("record-state", (e) => fn(e.payload)),
  onRecordTranscribing: (fn: () => void): Promise<UnlistenFn> =>
    listen<null>("record-transcribing", () => fn()),
  onRecordSaved: (fn: (ev: RecordSavedEvent) => void): Promise<UnlistenFn> =>
    listen<RecordSavedEvent>("record-saved", (e) => fn(e.payload)),
  onRecordError: (fn: (ev: RecordErrorEvent) => void): Promise<UnlistenFn> =>
    listen<RecordErrorEvent>("record-error", (e) => fn(e.payload)),
```

Opening the settings deep links reuses the opener plugin. Add at the top imports:

```ts
import { openUrl } from "@tauri-apps/plugin-opener";
```

and expose:

```ts
  openSettingsUrl: (url: string) => openUrl(url),
```

(Confirm `@tauri-apps/plugin-opener` is a JS dependency; it is used by the app already for external opens — if the JS package isn't installed, `pnpm add @tauri-apps/plugin-opener`.)

**Commands:**

```
pnpm exec tsc --noEmit
```

Expected: no type errors.

**Interfaces:** Produces `api.record*` methods + `api.on Record*` listeners + `api.openSettingsUrl`.

**Commit:** `feat(record): frontend api bindings`

---

### Task 19 — Nav rail entry, Screen union, lazy pane

- [ ] Add `"record"` to the `Screen` union, a Mic nav item, and the lazy pane.

**Files:** `src/lib/app.svelte.ts` (edit), `src/shell/NavRail.svelte` (edit), `src/shell/Shell.svelte` (edit)

`app.svelte.ts` — extend the union (line 48-55):

```ts
export type Screen =
  | "home"
  | "files"
  | "review"
  | "ingests"
  | "map"
  | "record"
  | "timeline"
  | "settings";
```

`NavRail.svelte` — add the import and the item (place Record after Timeline, before Settings' auto-margin, to sit with the working surfaces):

```ts
  import Mic from "@lucide/svelte/icons/mic";
```

Add to the `items` array after the `timeline` entry:

```ts
    { key: "record", icon: Mic, label: "Record" },
```

`Shell.svelte` — import and add the pane:

```ts
  import RecordScreen from "../screens/RecordScreen.svelte";
```

Add after the timeline pane block:

```svelte
      {#if visited.has("record")}
        <div class="pane" hidden={app.screen !== "record"}><RecordScreen /></div>
      {/if}
```

**Commands:**

```
pnpm exec tsc --noEmit
```

Expected: no errors (RecordScreen exists after Task 20 — do Task 20's file first or stub it). **Order note:** create `RecordScreen.svelte` (Task 20) before compiling this, or add a temporary stub.

**Commit:** `feat(record): nav entry and lazy pane`

---

### Task 20 — RecordScreen + components (Paper & Ink)

- [ ] Build the Record surface and its store. Follow `.impeccable.md`: warm paper, serif lead heading, clay accent only for the live record state, amber for "waiting on you" permission notices, quiet motion, plain copy.

**Files:** `src/lib/record.svelte.ts` (new), `src/record/LevelMeter.svelte` (new), `src/record/PermissionNotice.svelte` (new), `src/screens/RecordScreen.svelte` (new)

**20a — Store** `src/lib/record.svelte.ts`:

```ts
// Live recording state (Svelte 5 runes). One recorder at a time.
import { api, type AudioDevice, type PermissionStatus, type RecordPhase } from "./api";

class RecordStore {
  phase = $state<RecordPhase>("idle");
  elapsedMs = $state(0);
  micOn = $state(true);
  systemOn = $state(false);
  devices = $state<AudioDevice[]>([]);
  deviceId = $state<string | null>(null);
  micLevel = $state(0);
  systemLevel = $state(0);
  micPerm = $state<PermissionStatus>("notDetermined");
  screenPerm = $state<PermissionStatus>("notDetermined");
  micSettingsUrl = $state("");
  screenSettingsUrl = $state("");
  storage = $state<"transcript" | "audio" | "both">("both");
  transcribing = $state(false);
  savedPath = $state<string | null>(null);
  error = $state<string | null>(null);

  private clock: ReturnType<typeof setInterval> | null = null;
  private baseElapsed = 0;
  private baseAt = 0;

  get recording() {
    return this.phase === "recording" || this.phase === "paused";
  }

  async init() {
    this.devices = await api.recordInputDevices().catch(() => []);
    if (!this.deviceId && this.devices[0]) this.deviceId = this.devices[0].id;
    await this.refreshPermissions();
    await api.onRecordLevel((ev) => {
      if (ev.source === "mic") this.micLevel = ev.rms;
      else this.systemLevel = ev.rms;
    });
    await api.onRecordState((ev) => {
      this.phase = ev.phase;
      this.micOn = ev.mic || this.micOn;
      this.systemOn = ev.system || this.systemOn;
      this.baseElapsed = ev.elapsedMs;
      this.baseAt = performance.now();
      this.elapsedMs = ev.elapsedMs;
      if (ev.phase === "recording") this.startClock();
      else this.stopClock();
      if (ev.phase === "idle") {
        this.micLevel = 0;
        this.systemLevel = 0;
      }
    });
    await api.onRecordTranscribing(() => {
      this.transcribing = true;
    });
    await api.onRecordSaved((ev) => {
      this.transcribing = false;
      this.savedPath = ev.relPath;
    });
    await api.onRecordError((ev) => {
      this.transcribing = false;
      this.error = ev.message;
    });
  }

  async refreshPermissions() {
    const p = await api.recordPermissions().catch(() => null);
    if (!p) return;
    this.micPerm = p.mic;
    this.screenPerm = p.screen;
    this.micSettingsUrl = p.micSettingsUrl;
    this.screenSettingsUrl = p.screenSettingsUrl;
  }

  private startClock() {
    this.stopClock();
    this.clock = setInterval(() => {
      this.elapsedMs = this.baseElapsed + (performance.now() - this.baseAt);
    }, 200);
  }
  private stopClock() {
    if (this.clock) clearInterval(this.clock);
    this.clock = null;
  }

  async start() {
    this.error = null;
    this.savedPath = null;
    await api.recordStart(this.micOn, this.systemOn, this.deviceId).catch((e) => {
      this.error = String(e);
    });
    await this.refreshPermissions();
  }
  async pause() {
    await api.recordPause();
  }
  async resume() {
    await api.recordResume();
  }
  async stop() {
    await api.recordStop(this.storage);
  }
  async cancel() {
    await api.recordCancel();
  }
  async requestMic() {
    await api.recordRequestPermission("mic");
    setTimeout(() => void this.refreshPermissions(), 800);
  }
  async requestScreen() {
    await api.recordRequestPermission("screen");
    setTimeout(() => void this.refreshPermissions(), 800);
  }
}

export const record = new RecordStore();
```

**20b — LevelMeter** `src/record/LevelMeter.svelte`:

```svelte
<script lang="ts">
  let { level = 0, active = false }: { level: number; active: boolean } = $props();
  // Map RMS (~0..0.5 typical speech) to a 0..100% bar, gently compressed.
  const pct = $derived(Math.min(100, Math.round(Math.sqrt(level) * 140)));
</script>

<div class="meter" class:active>
  <div class="fill" style:width="{pct}%"></div>
</div>

<style>
  .meter {
    height: 6px;
    border-radius: 3px;
    background: var(--sunken);
    overflow: hidden;
  }
  .fill {
    height: 100%;
    background: var(--ink-tertiary);
    transition: width 90ms linear;
  }
  .meter.active .fill {
    background: var(--accent);
  }
</style>
```

**20c — PermissionNotice** `src/record/PermissionNotice.svelte`:

```svelte
<script lang="ts">
  import { api, type PermissionStatus } from "../lib/api";

  let {
    status,
    label,
    settingsUrl,
    onRequest,
  }: {
    status: PermissionStatus;
    label: string;
    settingsUrl: string;
    onRequest: () => void;
  } = $props();
</script>

{#if status !== "granted" && status !== "unsupported"}
  <div class="notice">
    <span class="text">
      {#if status === "notDetermined"}
        Ken needs your permission to use the {label}.
      {:else}
        {label} access is turned off for Ken.
      {/if}
    </span>
    {#if status === "notDetermined"}
      <button class="link" onclick={onRequest}>Allow</button>
    {:else}
      <button class="link" onclick={() => void api.openSettingsUrl(settingsUrl)}>
        Open Settings
      </button>
    {/if}
  </div>
{/if}

<style>
  .notice {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 9px 12px;
    border-radius: 9px;
    font-size: 12.5px;
    line-height: 1.5;
    color: var(--needs-input-text);
    background: color-mix(in srgb, var(--needs-input) 10%, transparent);
    border: 1px solid color-mix(in srgb, var(--needs-input) 26%, transparent);
  }
  .text {
    flex: 1;
  }
  .link {
    border: none;
    background: none;
    color: var(--needs-input-text);
    font-weight: 600;
    font-size: 12.5px;
    text-decoration: underline;
    cursor: pointer;
  }
</style>
```

**20d — RecordScreen** `src/screens/RecordScreen.svelte`:

```svelte
<script lang="ts">
  import { onMount } from "svelte";
  import { app } from "../lib/app.svelte";
  import { record } from "../lib/record.svelte";
  import LevelMeter from "../record/LevelMeter.svelte";
  import PermissionNotice from "../record/PermissionNotice.svelte";
  import Mic from "@lucide/svelte/icons/mic";
  import Speaker from "@lucide/svelte/icons/volume-2";

  onMount(() => void record.init());

  function clock(ms: number): string {
    const s = Math.floor(ms / 1000);
    const m = Math.floor(s / 60);
    const rem = s % 60;
    return `${m}:${rem.toString().padStart(2, "0")}`;
  }

  const canStart = $derived(record.micOn || record.systemOn);

  function openSaved() {
    if (record.savedPath) app.openInFiles(record.savedPath);
  }
</script>

<div class="screen">
  <div class="inner">
    <header>
      <h1>Record</h1>
      <p class="lead">
        Capture a meeting or a call. Ken keeps a private transcript — nothing
        you say leaves your Mac.
      </p>
    </header>

    <section class="sources">
      <label class="source" class:on={record.micOn}>
        <input type="checkbox" bind:checked={record.micOn} disabled={record.recording} />
        <span class="src-icon"><Mic size={15} strokeWidth={1.75} /></span>
        <span class="src-body">
          <span class="src-name">Me</span>
          <span class="src-sub">Your microphone</span>
          <LevelMeter level={record.micLevel} active={record.recording && record.micOn} />
        </span>
      </label>

      <label class="source" class:on={record.systemOn}>
        <input type="checkbox" bind:checked={record.systemOn} disabled={record.recording} />
        <span class="src-icon"><Speaker size={15} strokeWidth={1.75} /></span>
        <span class="src-body">
          <span class="src-name">Them</span>
          <span class="src-sub">System audio — the other side of a call</span>
          <LevelMeter level={record.systemLevel} active={record.recording && record.systemOn} />
        </span>
      </label>
    </section>

    {#if record.micOn}
      <PermissionNotice
        status={record.micPerm}
        label="microphone"
        settingsUrl={record.micSettingsUrl}
        onRequest={() => void record.requestMic()}
      />
    {/if}
    {#if record.systemOn}
      <PermissionNotice
        status={record.screenPerm}
        label="system audio (Screen Recording)"
        settingsUrl={record.screenSettingsUrl}
        onRequest={() => void record.requestScreen()}
      />
    {/if}

    {#if record.micOn && record.devices.length > 1 && !record.recording}
      <div class="device">
        <span class="device-label">Microphone</span>
        <select bind:value={record.deviceId}>
          {#each record.devices as d (d.id)}
            <option value={d.id}>{d.name}</option>
          {/each}
        </select>
      </div>
    {/if}

    <section class="controls">
      <div class="clock" class:live={record.phase === "recording"}>
        {clock(record.elapsedMs)}
      </div>

      {#if !record.recording}
        <button class="rec" disabled={!canStart} onclick={() => void record.start()}>
          <span class="rec-dot"></span> Record
        </button>
      {:else}
        {#if record.phase === "recording"}
          <button class="btn" onclick={() => void record.pause()}>Pause</button>
        {:else}
          <button class="btn" onclick={() => void record.resume()}>Resume</button>
        {/if}
        <button class="btn btn-primary" onclick={() => void record.stop()}>Stop &amp; save</button>
        <button class="btn btn-quiet" onclick={() => void record.cancel()}>Discard</button>
      {/if}
    </section>

    {#if record.recording}
      <div class="storage">
        <span class="storage-label">Keep</span>
        <div class="seg">
          <button class:sel={record.storage === "transcript"} onclick={() => (record.storage = "transcript")}>Transcript</button>
          <button class:sel={record.storage === "audio"} onclick={() => (record.storage = "audio")}>Audio</button>
          <button class:sel={record.storage === "both"} onclick={() => (record.storage = "both")}>Both</button>
        </div>
      </div>
    {/if}

    {#if record.transcribing}
      <p class="status">Transcribing on your Mac…</p>
    {/if}
    {#if record.savedPath}
      <p class="status done">
        Saved. <button class="link" onclick={openSaved}>Open transcript</button>
      </p>
    {/if}
    {#if record.error}
      <p class="status err">{record.error}</p>
    {/if}
  </div>
</div>

<style>
  .screen {
    flex: 1;
    min-width: 0;
    overflow-y: auto;
    padding: 40px;
  }
  .inner {
    max-width: 560px;
    margin: 0 auto;
    display: flex;
    flex-direction: column;
    gap: 20px;
  }
  h1 {
    margin: 0;
    font-family: var(--font-serif);
    font-size: 28px;
    font-weight: 500;
  }
  .lead {
    margin: 6px 0 0;
    font-family: var(--font-serif);
    font-size: 15px;
    line-height: 1.6;
    color: var(--ink-secondary);
  }
  .sources {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .source {
    display: flex;
    align-items: flex-start;
    gap: 12px;
    padding: 14px 16px;
    border: 1px solid var(--border);
    border-radius: var(--radius-card);
    background: var(--surface);
    cursor: pointer;
  }
  .source.on {
    border-color: color-mix(in srgb, var(--accent) 40%, var(--border));
  }
  .source input {
    margin-top: 2px;
  }
  .src-icon {
    color: var(--ink-tertiary);
    margin-top: 1px;
  }
  .src-body {
    display: flex;
    flex-direction: column;
    gap: 4px;
    flex: 1;
    min-width: 0;
  }
  .src-name {
    font-weight: 600;
    font-size: 13.5px;
  }
  .src-sub {
    font-size: 12px;
    color: var(--ink-tertiary);
  }
  .device {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 12.5px;
  }
  .device-label {
    color: var(--ink-tertiary);
  }
  .device select {
    flex: 1;
    padding: 6px 8px;
    border-radius: 8px;
    border: 1px solid var(--border);
    background: var(--surface);
    font-size: 12.5px;
  }
  .controls {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .clock {
    font-family: var(--font-mono);
    font-size: 22px;
    color: var(--ink-secondary);
    min-width: 72px;
  }
  .clock.live {
    color: var(--accent-deep);
  }
  .rec {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    padding: 10px 18px;
    border-radius: 22px;
    border: none;
    background: var(--accent);
    color: var(--surface);
    font-size: 13.5px;
    font-weight: 600;
    cursor: pointer;
  }
  .rec:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .rec-dot {
    width: 9px;
    height: 9px;
    border-radius: 5px;
    background: var(--surface);
  }
  .btn {
    padding: 9px 15px;
    border-radius: 20px;
    border: 1px solid var(--border);
    background: var(--surface);
    font-size: 13px;
    cursor: pointer;
  }
  .btn-primary {
    background: var(--accent);
    color: var(--surface);
    border-color: transparent;
    font-weight: 600;
  }
  .btn-quiet {
    color: var(--ink-tertiary);
  }
  .storage {
    display: flex;
    align-items: center;
    gap: 12px;
    font-size: 12.5px;
  }
  .storage-label {
    color: var(--ink-tertiary);
  }
  .seg {
    display: inline-flex;
    border: 1px solid var(--border);
    border-radius: 8px;
    overflow: hidden;
  }
  .seg button {
    padding: 6px 12px;
    border: none;
    background: var(--surface);
    font-size: 12.5px;
    cursor: pointer;
    color: var(--ink-secondary);
  }
  .seg button.sel {
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    color: var(--accent-deep);
    font-weight: 600;
  }
  .status {
    font-size: 12.5px;
    color: var(--ink-secondary);
  }
  .status.done {
    color: var(--healthy);
  }
  .status.err {
    color: var(--danger);
  }
  .link {
    border: none;
    background: none;
    color: var(--accent);
    font-weight: 600;
    text-decoration: underline;
    cursor: pointer;
    font-size: 12.5px;
  }
</style>
```

**Commands:**

```
pnpm exec tsc --noEmit
pnpm build
```

Expected: type-check + build succeed.

**Manual verification** (the full acceptance, per the `verify` skill):
1. Launch, open a project, click **Record** in the nav (Mic icon present).
2. Heading reads as editorial serif on warm paper; only the record button / live clock use the clay accent; permission notices use amber.
3. Me on / Them off, Record → mic prompt, grant → clock ticks, Me meter moves as you speak, Them meter still.
4. Pause → clock freezes + meters idle; Resume → continues.
5. Stop & save with **Both** → "Transcribing…" then "Saved. Open transcript" → click opens `Recordings/<date> Recording.md` with a metadata header and unlabeled `[m:ss]` lines; a `.wav` sits beside it.
6. Repeat with Me + Them both on and audio playing → transcript shows **Me**/**Them** turns interleaved by time.
7. Storage **Transcript** → after save, only the `.md` exists (WAVs gone). Storage **Audio** → `.wav` + a `.md` noting audio-only.
8. Deny mic in Settings, return → notice shows Open Settings, clicking opens the Microphone pane.

**Commit:** `feat(record): Record screen and components`

---

### Task 21 — Full-suite green + self-review

- [ ] Run the whole Rust suite and a clean build; verify nothing else regressed.

**Commands:**

```
cargo test -p ken-core
cargo build -p ken-app
pnpm exec tsc --noEmit
pnpm build
```

Expected: all ken-core tests pass (including the whole `record` module); Tauri + frontend build clean.

**Self-review against spec §6:**
- [ ] Mic via cpal with device picker — Tasks 11, 20.
- [ ] System audio via ScreenCaptureKit audio-only, macOS 13+, Screen Recording TCC — Tasks 13-15.
- [ ] Either source independent, both together — `record_start(mic, system, …)`.
- [ ] Each active source → own 16 kHz mono WAV in a temp workspace — `ChannelWriter` + `tmp_dir`.
- [ ] Live UI: device picker, toggles, level meters (rms events), elapsed clock, pause/resume, stop — Task 20.
- [ ] Permission guidance inline + deep links — Tasks 15, 20.
- [ ] On stop: each channel Whisper-transcribed, merged by start time, Me/Them labels; single-source unlabeled — Tasks 8, 17.
- [ ] Storage choice transcript/audio/both; transcript-only deletes WAVs only after success; failure keeps audio + surfaces error — Task 17 `finish_recording`.
- [ ] Output into `Recordings/` `YYYY-MM-DD HH.MM Recording.md` + metadata header + one `.wav` per channel; normal scan/index picks it up — Tasks 6, 17.

**Commit:** `test(record): full-suite green + spec self-review`

---

## Notes / known limitations (surface in UI copy or a follow-up)

- **Retry UX:** v1 handles transcription failure by saving the audio into `Recordings/` and writing an audio-only `.md` with an error line (no in-session retry button). Re-transcription is a future affordance.
- **System audio needs a display:** SCK attaches audio capture to a display filter; a headless Mac is out of scope (matches spec's macOS-only posture).
- **SCK / objc2 API churn:** the exact symbol names for SCK 8.0.0 audio extraction and objc2-av-foundation authorization are confirmed at implementation time in Tasks 13/15 against `cargo doc`; the plan pins versions and structures those as compile-probes precisely because these crates change across majors.

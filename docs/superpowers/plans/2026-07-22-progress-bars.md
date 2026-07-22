# Inline Progress Bars (Hydration + Transcription) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Determinate, inline progress bars for cloud-file hydration downloads and video/recording transcription, mirroring the existing model-download progress pattern.

**Architecture:** Rust emits two new named Tauri events — `hydration-progress` (allocated-vs-logical bytes sampled during the existing 2s cloud poll loop) and `transcript-progress` (whisper-rs native progress callback, phased `extracting`/`transcribing`) — throttled with the existing `ProgressThrottle`. The frontend adds a shared `ProgressBar.svelte`, typed listeners in `api.ts`, and wires bars into `VideoPreview`, `EditorPane` (via `PreviewLoading`), and `RecordScreen`.

**Tech Stack:** Rust (Tauri 2, whisper-rs 0.16, workspace at repo root), Svelte 5 runes, TypeScript, vitest, svelte-check.

**Spec:** `docs/superpowers/specs/2026-07-22-progress-bars-design.md`

## Global Constraints

- Repo root: `/Users/arthur.pachachura/git/ken`. Cargo workspace at root (`cargo test -p ken-core` from root). Frontend: `npm test` (vitest), `npm run check` (svelte-check).
- The model-download flow (`model-download-progress` event, `download_to`, `ModelDownloadDialog` behavior) must not change behavior — only its bar markup is extracted into the shared component.
- Event names are exactly `hydration-progress` and `transcript-progress`; payloads serialize `camelCase` (`#[serde(rename_all = "camelCase")]`), matching every other event in `lib.rs`.
- `ken-core` must keep compiling **without** the `whisper` feature (the fallback `transcribe` stub must gain the same new signature).
- Follow the codebase comment style: comments state constraints/why, not what-the-next-line-does.
- Existing public functions keep working: add `_with_progress` variants; old names delegate with a no-op callback.
- Commit after each task with a conventional-commit message ending in the Claude co-author trailer.

---

### Task 1: `cloud.rs` — hydration progress plumbing

**Files:**
- Modify: `crates/ken-core/src/cloud.rs` (constants at :19, `poll_until_hydrated` at :109, `hydrate`/`hydrate_with_deadline` at :143-159, tests module at :161)

**Interfaces:**
- Produces: `pub const DEFAULT_DEADLINE: Duration` (was private), `pub fn hydrate_with_progress(path: &Path, deadline: Duration, on_progress: impl FnMut(u64, u64)) -> crate::Result<()>` — `on_progress(allocated_or_final, logical_total)`; fires one sample per poll tick while downloading and a final `(total, total)` when the bytes land. Task 3 consumes both.

- [ ] **Step 1: Write the failing tests**

Append to the existing `mod tests` in `crates/ken-core/src/cloud.rs`:

```rust
#[test]
fn hydration_sample_reports_allocated_vs_logical_and_skips_missing_files() {
    // A real file we just wrote is fully allocated: the sample must be
    // (total, total)-ish — allocated is block-rounded, so we clamp to len.
    let dir = std::env::temp_dir().join(format!("ken-hydration-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("sample.bin");
    std::fs::write(&f, vec![7u8; 10_000]).unwrap();
    let (got, total) = hydration_sample(&f).expect("sample for an existing file");
    assert_eq!(total, 10_000);
    assert_eq!(got, 10_000, "allocated bytes are clamped to the logical size");
    assert!(hydration_sample(&dir.join("missing.bin")).is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hydrate_with_progress_emits_a_final_full_sample_for_a_local_file() {
    // A plain local file hydrates on the first probe; the callback still gets
    // the terminal (total, total) so UIs can treat 100% as "done".
    let dir = std::env::temp_dir().join(format!("ken-hydrate-prog-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("local.txt");
    std::fs::write(&f, b"already here").unwrap();
    let mut samples: Vec<(u64, u64)> = Vec::new();
    hydrate_with_progress(&f, Duration::from_secs(1), |d, t| samples.push((d, t))).unwrap();
    assert_eq!(samples.last().copied(), Some((12, 12)));
    let _ = std::fs::remove_dir_all(&dir);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ken-core cloud:: 2>&1 | tail -20`
Expected: compile error — `hydration_sample` and `hydrate_with_progress` not found.

- [ ] **Step 3: Implement**

In `crates/ken-core/src/cloud.rs`:

1. Make the deadline public (line 19): change `const DEFAULT_DEADLINE` to `pub const DEFAULT_DEADLINE` (keep the doc comment).

2. After `hydrate_with_deadline` (line ~159), add:

```rust
/// [`hydrate_with_deadline`] that also reports download progress. The provider
/// owns the transfer, so bytes can't be counted as they're read — instead each
/// poll tick samples how much of the file is *allocated* on disk against its
/// logical size (macOS reports a dataless file's full `len()` up front). The
/// terminal `(total, total)` sample fires once the bytes have fully landed.
pub fn hydrate_with_progress(
    path: &Path,
    deadline: Duration,
    mut on_progress: impl FnMut(u64, u64),
) -> crate::Result<()> {
    let started = Instant::now();
    poll_until_hydrated(
        path,
        deadline,
        || {
            let attempt = probe(path);
            match &attempt {
                Attempt::Downloading => {
                    if let Some((got, total)) = hydration_sample(path) {
                        on_progress(got, total);
                    }
                }
                Attempt::Ready => {
                    if let Ok(m) = std::fs::metadata(path) {
                        on_progress(m.len(), m.len());
                    }
                }
                Attempt::Fatal(_) => {}
            }
            attempt
        },
        || started.elapsed(),
        std::thread::sleep,
    )
}

/// One (allocated, logical) size sample for a file mid-hydration, or `None`
/// when it can't be read or has no known size. Allocation is block-granular,
/// so it's clamped to the logical size to never overshoot 100%.
fn hydration_sample(path: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let total = meta.len();
    if total == 0 {
        return None;
    }
    Some((allocated_bytes(&meta).min(total), total))
}

#[cfg(unix)]
fn allocated_bytes(meta: &Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.blocks().saturating_mul(512)
}

#[cfg(not(unix))]
fn allocated_bytes(_meta: &Metadata) -> u64 {
    0
}
```

(Confirm `Metadata`, `Instant`, `Duration` are already imported at the top of the file; they are used by existing code — add `use` lines only if missing.)

3. Change `hydrate_with_deadline` to delegate so there is one poll loop:

```rust
pub fn hydrate_with_deadline(path: &Path, deadline: Duration) -> crate::Result<()> {
    hydrate_with_progress(path, deadline, |_, _| {})
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ken-core cloud:: 2>&1 | tail -20`
Expected: all `cloud::` tests PASS, including the two new ones and all pre-existing hydration tests.

- [ ] **Step 5: Commit**

```bash
git add crates/ken-core/src/cloud.rs
git commit -m "feat(cloud): hydration progress sampling in the poll loop

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: `transcript.rs` — transcription progress callback

**Files:**
- Modify: `crates/ken-core/src/transcript.rs` (whisper `transcribe` at :499, stub at :546, `generate_vtt` at :554, `generate_and_cache` at :566, tests module at :581)

**Interfaces:**
- Produces (consumed by Task 4):
  - `pub enum TranscriptPhase { Extracting, Transcribing(u8) }` (derives `Debug, Clone, Copy, PartialEq, Eq`)
  - `pub type ProgressFn = std::sync::Arc<dyn Fn(TranscriptPhase) + Send + Sync>;`
  - `pub fn noop_progress() -> ProgressFn`
  - `pub fn transcribe_with_progress(model: &Path, samples: &[f32], on_progress: ProgressFn) -> Result<Vec<Cue>>`
  - `pub fn generate_and_cache_with_progress(ffmpeg: &Path, model: &Path, project_root: &Path, video_rel: &str, on_progress: ProgressFn) -> Result<PathBuf>`
  - `pub fn scale_channel_pct(idx: usize, count: usize, pct: u8) -> u8`

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `crates/ken-core/src/transcript.rs`:

```rust
#[test]
fn channel_percentages_scale_into_one_overall_bar() {
    // Two channels: channel 0 covers 0–50, channel 1 covers 50–100.
    assert_eq!(scale_channel_pct(0, 2, 0), 0);
    assert_eq!(scale_channel_pct(0, 2, 100), 50);
    assert_eq!(scale_channel_pct(1, 2, 0), 50);
    assert_eq!(scale_channel_pct(1, 2, 100), 100);
    // One channel passes through; degenerate inputs stay in range.
    assert_eq!(scale_channel_pct(0, 1, 37), 37);
    assert_eq!(scale_channel_pct(0, 0, 50), 0);
    assert_eq!(scale_channel_pct(3, 2, 100), 100);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ken-core transcript:: 2>&1 | tail -20`
Expected: compile error — `scale_channel_pct` not found.

- [ ] **Step 3: Implement**

In `crates/ken-core/src/transcript.rs`, in the `// ---------- ffmpeg + Whisper (thin, untested shells) ----------` section (before `transcribe` at :499), add:

```rust
/// Progress from the transcription pipeline: a quick unmeterable ffmpeg
/// extraction, then Whisper's native 0–100 percent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptPhase {
    Extracting,
    Transcribing(u8),
}

/// Shared, thread-safe progress sink. `Arc` because whisper-rs demands a
/// `'static` callback, and the same closure is observed from the caller side.
pub type ProgressFn = std::sync::Arc<dyn Fn(TranscriptPhase) + Send + Sync>;

/// The do-nothing sink for callers that don't surface progress.
pub fn noop_progress() -> ProgressFn {
    std::sync::Arc::new(|_| {})
}

/// Map one channel's 0–100 into an overall percent across `count` sequential
/// channels (a two-channel recording transcribes Me then Them; the bar must
/// not jump back to 0 in between).
pub fn scale_channel_pct(idx: usize, count: usize, pct: u8) -> u8 {
    if count == 0 {
        return 0;
    }
    (((idx.min(count - 1) * 100) + pct.min(100) as usize) / count).min(100) as u8
}
```

Rename the whisper-feature `transcribe` (at :499) to `transcribe_with_progress`, adding the parameter and callback registration. The full new pair:

```rust
#[cfg(feature = "whisper")]
pub fn transcribe_with_progress(
    model: &Path,
    samples: &[f32],
    on_progress: ProgressFn,
) -> Result<Vec<Cue>> {
    use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

    // whisper-rs 0.16 accepts any `AsRef<Path>`, so the model path passes through
    // directly (no UTF-8 round-trip needed).
    let ctx = WhisperContext::new_with_params(model, WhisperContextParameters::default())
        .map_err(|e| Error::Other(format!("couldn't load the Whisper model: {e}")))?;
    let mut state = ctx
        .create_state()
        .map_err(|e| Error::Other(format!("Whisper init failed: {e}")))?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(Some("en"));
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    {
        let cb = on_progress.clone();
        params.set_progress_callback_safe(move |pct: i32| {
            cb(TranscriptPhase::Transcribing(pct.clamp(0, 100) as u8));
        });
    }

    state
        .full(params, samples)
        .map_err(|e| Error::Other(format!("transcription failed: {e}")))?;
    // ... keep the existing segment-iteration body from the old `transcribe`
    // unchanged from here down (cues loop + Ok(cues)).
```

> If `set_progress_callback_safe` doesn't exist under that exact name in whisper-rs 0.16, check `cargo doc`/the crate source for the safe progress-callback setter (`set_progress_callback_safe` is the documented safe variant; the raw `set_progress_callback` takes a C fn pointer and must NOT be used with a closure). Adapt the registration call only — the `ProgressFn` type stays.

Update the non-whisper stub (at :546) to the same shape, and add back a `transcribe` wrapper used by tests/back-compat:

```rust
#[cfg(not(feature = "whisper"))]
pub fn transcribe_with_progress(
    _model: &Path,
    _samples: &[f32],
    _on_progress: ProgressFn,
) -> Result<Vec<Cue>> {
    Err(Error::Other(
        "This build of Ken was compiled without on-device transcription support.".into(),
    ))
}

/// [`transcribe_with_progress`] without a progress sink.
pub fn transcribe(model: &Path, samples: &[f32]) -> Result<Vec<Cue>> {
    transcribe_with_progress(model, samples, noop_progress())
}
```

Replace `generate_vtt` and `generate_and_cache` bodies with delegating pairs:

```rust
/// Full pipeline: video → ffmpeg audio → Whisper → WebVTT. Blocking; call from
/// a worker thread.
pub fn generate_vtt(ffmpeg: &Path, model: &Path, video: &Path) -> Result<String> {
    generate_vtt_with_progress(ffmpeg, model, video, noop_progress())
}

/// [`generate_vtt`] reporting phase progress: `Extracting` before ffmpeg runs,
/// then Whisper's percent stream.
pub fn generate_vtt_with_progress(
    ffmpeg: &Path,
    model: &Path,
    video: &Path,
    on_progress: ProgressFn,
) -> Result<String> {
    on_progress(TranscriptPhase::Extracting);
    let samples = extract_audio_f32(ffmpeg, video)?;
    if samples.is_empty() {
        return Err(Error::Other("this video has no audio to transcribe".into()));
    }
    let cues = transcribe_with_progress(model, &samples, on_progress)?;
    Ok(emit_webvtt(&cues))
}

/// Generate a transcript and store it in the project's cache, returning the
/// path written. The caller re-indexes the video so its transcript becomes
/// searchable and the `index-updated` event fires.
pub fn generate_and_cache(
    ffmpeg: &Path,
    model: &Path,
    project_root: &Path,
    video_rel: &str,
) -> Result<PathBuf> {
    generate_and_cache_with_progress(ffmpeg, model, project_root, video_rel, noop_progress())
}

/// [`generate_and_cache`] with a progress sink (see [`TranscriptPhase`]).
pub fn generate_and_cache_with_progress(
    ffmpeg: &Path,
    model: &Path,
    project_root: &Path,
    video_rel: &str,
    on_progress: ProgressFn,
) -> Result<PathBuf> {
    let video_abs = project_root.join(video_rel);
    let vtt = generate_vtt_with_progress(ffmpeg, model, &video_abs, on_progress)?;
    let dir = cache_dir(project_root);
    std::fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
    let out = dir.join(cache_name(video_rel));
    std::fs::write(&out, vtt).map_err(|e| Error::io(&out, e))?;
    Ok(out)
}
```

- [ ] **Step 4: Run tests and both feature builds**

Run: `cargo test -p ken-core transcript:: 2>&1 | tail -10`
Expected: PASS including `channel_percentages_scale_into_one_overall_bar`.

Run: `cargo check -p ken-core --features whisper 2>&1 | tail -5` and `cargo check -p ken-core 2>&1 | tail -5`
Expected: both compile cleanly (the whisper build validates the callback registration; the plain build validates the stub).

- [ ] **Step 5: Commit**

```bash
git add crates/ken-core/src/transcript.rs
git commit -m "feat(transcript): phase/percent progress callback through the pipeline

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: `lib.rs` — emit `hydration-progress`

**Files:**
- Modify: `src-tauri/src/lib.rs` (`hydrate_file` at :839-850, `background_hydrate_worker` hydrate call at :589; put the new struct + helper near the cloud commands ~:829)

**Interfaces:**
- Consumes: `cloud::hydrate_with_progress`, `cloud::DEFAULT_DEADLINE` (Task 1), `model::ProgressThrottle` (existing).
- Produces: Tauri event `hydration-progress` with camelCase payload `{ relPath: string, downloaded: number, total: number }`. Terminal sample has `downloaded == total`. Consumed by Task 6.

- [ ] **Step 1: Add the payload struct and emitting helper**

Near `is_cloud_only` (~line 829), add:

```rust
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct HydrationProgress {
    rel_path: String,
    downloaded: u64,
    total: u64,
}

/// Hydrate a placeholder while streaming `hydration-progress` events, throttled
/// exactly like model downloads. The terminal `(total, total)` sample always
/// goes out so the UI can treat 100% as "the bytes are here".
fn hydrate_emitting(
    app: &AppHandle,
    rel_path: &str,
    abs: &Path,
    deadline: Duration,
) -> ken_core::Result<()> {
    let mut throttle = model::ProgressThrottle::new();
    let start = Instant::now();
    let app = app.clone();
    let rel = rel_path.to_string();
    cloud::hydrate_with_progress(abs, deadline, move |downloaded, total| {
        let now_ms = start.elapsed().as_millis() as u64;
        let done = total > 0 && downloaded >= total;
        if done || throttle.should_emit(downloaded, total, now_ms) {
            let _ = app.emit(
                "hydration-progress",
                HydrationProgress { rel_path: rel.clone(), downloaded, total },
            );
        }
    })
}
```

(`cloud` and `model` are already in scope in `lib.rs` — check the existing `use` lines; `Path` may need adding to an existing import.)

- [ ] **Step 2: Wire both hydration call sites**

In `hydrate_file` (:846-850), replace:

```rust
    let abs = resolve_path(&state, &rel_path)?;
    let path = abs.clone();
    tauri::async_runtime::spawn_blocking(move || ken_core::cloud::hydrate(&path))
        .await
        .map_err(err)?
        .map_err(err)?;
```

with:

```rust
    let abs = resolve_path(&state, &rel_path)?;
    let path = abs.clone();
    let progress_app = app.clone();
    let progress_rel = rel_path.clone();
    tauri::async_runtime::spawn_blocking(move || {
        hydrate_emitting(&progress_app, &progress_rel, &path, cloud::DEFAULT_DEADLINE)
    })
    .await
    .map_err(err)?
    .map_err(err)?;
```

In `background_hydrate_worker` (:589), replace:

```rust
            match cloud::hydrate_with_deadline(&abs, BG_HYDRATE_DEADLINE) {
```

with:

```rust
            match hydrate_emitting(&app, &rel, &abs, BG_HYDRATE_DEADLINE) {
```

- [ ] **Step 3: Compile**

Run: `cargo check -p ken 2>&1 | tail -5` (the `src-tauri` package — confirm its name with `grep '^name' src-tauri/Cargo.toml` and substitute if different)
Expected: clean check, no warnings about unused `hydrate_with_deadline` breaking the build (it's still used by `cloud.rs` tests/API).

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/lib.rs
git commit -m "feat(cloud): emit hydration-progress from on-demand and background hydration

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: `lib.rs` — emit `transcript-progress`

**Files:**
- Modify: `src-tauri/src/lib.rs` (`spawn_transcription` at :2374-2410, `finish_recording` transcribe closure at :1580-1608; put the struct + helper just above `TranscriptDto` ~:2240)

**Interfaces:**
- Consumes: `transcript::{TranscriptPhase, ProgressFn, generate_and_cache_with_progress, transcribe_with_progress, scale_channel_pct}` (Task 2).
- Produces: Tauri event `transcript-progress` with camelCase payload `{ relPath: string, phase: "extracting" | "transcribing", pct: number | null }`. Consumed by Tasks 6 and 7.

- [ ] **Step 1: Add the payload struct and helpers**

Above `TranscriptDto` (~:2240):

```rust
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct TranscriptProgress {
    rel_path: String,
    /// "extracting" | "transcribing"
    phase: String,
    /// 0–100; present only while transcribing.
    pct: Option<u8>,
}

fn emit_transcript_phase(app: &AppHandle, rel: &str, phase: transcript::TranscriptPhase) {
    let (phase, pct) = match phase {
        transcript::TranscriptPhase::Extracting => ("extracting", None),
        transcript::TranscriptPhase::Transcribing(p) => ("transcribing", Some(p)),
    };
    let _ = app.emit(
        "transcript-progress",
        TranscriptProgress { rel_path: rel.to_string(), phase: phase.into(), pct },
    );
}

/// A `ProgressFn` that forwards to `transcript-progress`, dropping repeated
/// percents (Whisper's callback re-fires the same value between segments).
fn transcript_progress_sink(app: AppHandle, rel: String) -> transcript::ProgressFn {
    let last = std::sync::atomic::AtomicI32::new(-1);
    std::sync::Arc::new(move |phase| {
        if let transcript::TranscriptPhase::Transcribing(p) = phase {
            if last.swap(p as i32, Ordering::Relaxed) == p as i32 {
                return;
            }
        }
        emit_transcript_phase(&app, &rel, phase);
    })
}
```

- [ ] **Step 2: Wire `spawn_transcription`**

In the thread body (:2386-2388), replace:

```rust
        let result =
            transcript::generate_and_cache(&job.ffmpeg, &job.model, &job.root, &job.rel_path);
```

with:

```rust
        let on_progress = transcript_progress_sink(app.clone(), job.rel_path.clone());
        let result = transcript::generate_and_cache_with_progress(
            &job.ffmpeg, &job.model, &job.root, &job.rel_path, on_progress,
        );
```

- [ ] **Step 3: Wire the recording path**

In `finish_recording`'s `transcribe_all` closure (:1582-1608), replace the two direct `transcript::transcribe(&model, &samples)` calls so a two-channel recording renders one continuous 0–100 bar (channel 0 → 0–50, channel 1 → 50–100). Replace the closure body's channel section:

```rust
        let transcribe_all = || -> CmdResult<String> {
            if !model.is_file() {
                return Err("Download a transcription model in Settings to make transcripts.".into());
            }
            let channel_count =
                mic_moved.is_some() as usize + sys_moved.is_some() as usize;
            // One continuous bar across sequential channels: Me fills the first
            // half, Them the second (or the whole bar when only one exists).
            let scaled_sink = |idx: usize| -> transcript::ProgressFn {
                let app = app.clone();
                let rel = md_rel.clone();
                let last = std::sync::atomic::AtomicI32::new(-1);
                std::sync::Arc::new(move |phase| {
                    if let transcript::TranscriptPhase::Transcribing(p) = phase {
                        let overall = transcript::scale_channel_pct(idx, channel_count, p);
                        if last.swap(overall as i32, Ordering::Relaxed) == overall as i32 {
                            return;
                        }
                        emit_transcript_phase(
                            &app,
                            &rel,
                            transcript::TranscriptPhase::Transcribing(overall),
                        );
                    }
                })
            };
            let mut me_cues = Vec::new();
            let mut them_cues = Vec::new();
            let mut idx = 0usize;
            if let Some((p, _)) = &mic_moved {
                let samples = record::read_wav_f32(p).map_err(err)?;
                me_cues = transcript::transcribe_with_progress(&model, &samples, scaled_sink(idx))
                    .map_err(err)?;
                idx += 1;
            }
            if let Some((p, _)) = &sys_moved {
                let samples = record::read_wav_f32(p).map_err(err)?;
                them_cues =
                    transcript::transcribe_with_progress(&model, &samples, scaled_sink(idx))
                        .map_err(err)?;
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
            Ok(record::merge_transcript(&channels))
        };
```

Notes: `md_rel` (`format!("Recordings/{md_name}")`) is defined just above the closure — keep the definition order; `Ordering` is already imported for the atomic stop flags. The `single` fallback logic is unchanged from the current code — only the two transcribe calls and the sink are new.

- [ ] **Step 4: Compile + core tests**

Run: `cargo check -p ken 2>&1 | tail -5` (substitute the actual src-tauri package name) and `cargo test -p ken-core 2>&1 | tail -5`
Expected: clean check; all ken-core tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/lib.rs
git commit -m "feat(transcript): emit transcript-progress from video and recording paths

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 5: Shared `ProgressBar.svelte` + refactors

**Files:**
- Create: `src/lib/ProgressBar.svelte`
- Modify: `src/files/previews/ModelDownloadDialog.svelte` (bar markup :104-110, styles :173-184)
- Modify: `src/files/previews/PreviewLoading.svelte`

**Interfaces:**
- Produces: `ProgressBar` props `{ pct: number | null; label?: string }` — `pct: null` renders an indeterminate sweep. `PreviewLoading` gains optional prop `progress?: { pct: number | null; note?: string } | null`. Consumed by Tasks 6 and 7.

- [ ] **Step 1: Create `src/lib/ProgressBar.svelte`**

```svelte
<script lang="ts">
  // The app's one determinate progress bar (extracted from the model download
  // dialog so hydration and transcription reuse it). `pct: null` renders an
  // indeterminate sweep for work whose extent isn't known yet.
  let { pct, label }: { pct: number | null; label?: string } = $props();
  const clamped = $derived(
    pct === null ? null : Math.max(0, Math.min(100, Math.round(pct))),
  );
</script>

<div class="progress">
  <div
    class="bar"
    class:indeterminate={clamped === null}
    role="progressbar"
    aria-valuenow={clamped ?? undefined}
    aria-valuemin={0}
    aria-valuemax={100}
  >
    <div class="fill" style:width={clamped === null ? "40%" : `${clamped}%`}></div>
  </div>
  {#if label}
    <p class="status">{label}</p>
  {/if}
</div>

<style>
  .progress {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .bar {
    height: 6px;
    border-radius: 4px;
    background: var(--sunken);
    overflow: hidden;
  }
  .fill {
    height: 100%;
    background: var(--accent);
    border-radius: 4px;
    transition: width 0.2s ease;
  }
  .bar.indeterminate .fill {
    animation: sweep 1.2s ease-in-out infinite;
  }
  @keyframes sweep {
    0% {
      transform: translateX(-100%);
    }
    100% {
      transform: translateX(350%);
    }
  }
  .status {
    margin: 0;
    font-size: 12px;
    color: var(--ink-tertiary);
  }
</style>
```

- [ ] **Step 2: Refactor `ModelDownloadDialog.svelte` to use it**

Add the import: `import ProgressBar from "../../lib/ProgressBar.svelte";`

Replace lines 104-110 (`.bar` div + `.status` line):

```svelte
  {#if phase === "downloading"}
    <ProgressBar
      {pct}
      label={pct >= 100 ? "Installing…" : `Downloading… ${pct}%`}
    />
  {:else if phase === "error" && message}
    <p class="status error">{message}</p>
  {/if}
```

Delete the now-unused `.bar` and `.fill` style rules (:173-184). Keep `.status` / `.status.error` — the error branch still uses them.

- [ ] **Step 3: Add the `progress` prop to `PreviewLoading.svelte`**

```svelte
<script lang="ts">
  import ProgressBar from "../../lib/ProgressBar.svelte";
  // The one loading/downloading state every file view shares: a single spinner
  // centered in its container, one primary line, and an optional quieter second
  // line. `progress` swaps in a real bar once the work's extent is known.
  let {
    label,
    detail,
    progress = null,
  }: {
    label: string;
    detail?: string;
    progress?: { pct: number | null; note?: string } | null;
  } = $props();
</script>
```

In the markup, after the `detail` block:

```svelte
  {#if progress}
    <div class="pb">
      <ProgressBar pct={progress.pct} label={progress.note} />
    </div>
  {/if}
```

And in styles:

```css
  .pb {
    width: min(260px, 100%);
  }
```

- [ ] **Step 4: Type-check and test**

Run: `npm run check 2>&1 | tail -5` and `npm test 2>&1 | tail -5`
Expected: 0 errors; existing vitest suite passes.

- [ ] **Step 5: Commit**

```bash
git add src/lib/ProgressBar.svelte src/files/previews/ModelDownloadDialog.svelte src/files/previews/PreviewLoading.svelte
git commit -m "refactor(ui): shared ProgressBar; PreviewLoading learns determinate progress

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 6: `api.ts` listeners + `VideoPreview` + `EditorPane` wiring

**Files:**
- Modify: `src/lib/api.ts` (types after `ModelDownloadError` :404-408, listeners after `onModelDownloadError` :707-710)
- Modify: `src/files/previews/VideoPreview.svelte`
- Modify: `src/files/EditorPane.svelte`

**Interfaces:**
- Consumes: events from Tasks 3-4, `ProgressBar`/`PreviewLoading.progress` from Task 5.
- Produces: `api.onTranscriptProgress`, `api.onHydrationProgress`, exported types `TranscriptProgress`, `HydrationProgress` (consumed by Task 7).

- [ ] **Step 1: api.ts types + listeners**

After the `ModelDownloadError` interface:

```ts
/** Payload of the `transcript-progress` event. */
export interface TranscriptProgress {
  relPath: string;
  phase: "extracting" | "transcribing";
  /** 0–100; present only while transcribing. */
  pct: number | null;
}

/** Payload of the `hydration-progress` event. */
export interface HydrationProgress {
  relPath: string;
  downloaded: number;
  total: number;
}
```

After `onModelDownloadError` in the `api` object:

```ts
  onTranscriptProgress: (
    fn: (ev: TranscriptProgress) => void,
  ): Promise<UnlistenFn> =>
    listen<TranscriptProgress>("transcript-progress", (e) => fn(e.payload)),
  onHydrationProgress: (
    fn: (ev: HydrationProgress) => void,
  ): Promise<UnlistenFn> =>
    listen<HydrationProgress>("hydration-progress", (e) => fn(e.payload)),
```

- [ ] **Step 2: VideoPreview — percent bar while generating**

In `src/files/previews/VideoPreview.svelte`:

Script additions (near the other state, ~:27):

```ts
  import ProgressBar from "../../lib/ProgressBar.svelte";
  // Live generation progress for THIS video; null until the first event lands.
  let genPhase = $state<"extracting" | "transcribing" | null>(null);
  let genPct = $state<number | null>(null);
```

Reset both in the `$effect` that runs on `relPath` change (after `missingModel = null;`):

```ts
    genPhase = null;
    genPct = null;
```

In `onMount`, subscribe alongside the existing `onIndexUpdated` listener (keep both unlistens in the cleanup):

```ts
    let unlistenProgress: UnlistenFn | undefined;
    api
      .onTranscriptProgress((ev) => {
        if (ev.relPath !== relPath) return;
        genPhase = ev.phase;
        genPct = ev.pct;
      })
      .then((u) => (unlistenProgress = u));
```

and in the returned cleanup add `unlistenProgress?.();`.

Also set `genPhase = null; genPct = null;` at the top of `generate()` so a retry starts fresh.

Replace the `status === "generating"` branch (:200-204):

```svelte
      {:else if status === "generating"}
        {#if genPhase === "transcribing" && genPct !== null}
          <div class="note">
            <p class="lead">Transcribing this video…</p>
            <ProgressBar pct={genPct} label={`Transcribing… ${genPct}%`} />
            <p>The transcript and captions will appear here when it finishes.</p>
          </div>
        {:else}
          <PreviewLoading
            label={genPhase === "extracting"
              ? "Preparing audio…"
              : "Transcribing this video…"}
            detail="On-device transcription is running in the background. The transcript and captions will appear here when it finishes."
          />
        {/if}
```

- [ ] **Step 3: EditorPane — byte bar during cloud download**

In `src/files/EditorPane.svelte`:

Script additions:

```ts
  import type { HydrationProgress } from "../lib/api";
  // Live byte progress for THIS file's cloud pull (on-demand or the background
  // worker happening to fetch the open file).
  let hydration = $state<HydrationProgress | null>(null);

  function fmtMb(n: number): string {
    return `${Math.max(1, Math.round(n / (1024 * 1024)))} MB`;
  }
```

Subscribe in `onMount` next to the existing listener (and unlisten in `onDestroy` — follow the existing `unlisten` pattern, e.g. keep a second `unlistenHydration` variable):

```ts
    unlistenHydration = await api.onHydrationProgress((ev) => {
      if (ev.relPath === relPath) hydration = ev;
    });
```

Reset `hydration = null;` at the top of `load()` (next to `cloudError = null;`).

Update the downloading branch (~:306):

```svelte
    <PreviewLoading
      label="Downloading from the cloud"
      detail="..."  <!-- keep the existing detail text exactly as-is -->
      progress={hydration && hydration.total > 0
        ? {
            pct: Math.round((hydration.downloaded / hydration.total) * 100),
            note: `${fmtMb(hydration.downloaded)} / ${fmtMb(hydration.total)}`,
          }
        : { pct: null }}
    />
```

(Read the current branch first and keep its exact `label`/`detail` strings; only add the `progress` prop.)

- [ ] **Step 4: Type-check and test**

Run: `npm run check 2>&1 | tail -5` and `npm test 2>&1 | tail -5`
Expected: 0 errors; suite passes.

- [ ] **Step 5: Commit**

```bash
git add src/lib/api.ts src/files/previews/VideoPreview.svelte src/files/EditorPane.svelte
git commit -m "feat(ui): live progress bars for video transcription and cloud downloads

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 7: Recording transcription bar

**Files:**
- Modify: `src/lib/record.svelte.ts`
- Modify: `src/screens/RecordScreen.svelte` (:133-135)

**Interfaces:**
- Consumes: `api.onTranscriptProgress` (Task 6), `ProgressBar` (Task 5).
- Produces: `record.transcribePct: number | null`.

- [ ] **Step 1: Track percent in the store**

In `src/lib/record.svelte.ts`, next to `transcribing`:

```ts
  transcribing = $state(false);
  /** 0–100 while the post-stop transcription runs; null until the first sample. */
  transcribePct = $state<number | null>(null);
```

In `init()`, after the `onRecordTranscribing` subscription:

```ts
    await api.onTranscriptProgress((ev) => {
      // Recording docs land under Recordings/; that prefix keeps a concurrent
      // video transcription from driving this bar.
      if (this.transcribing && ev.relPath.startsWith("Recordings/")) {
        this.transcribePct = ev.pct;
      }
    });
```

Reset points: inside the `onRecordTranscribing` handler add `this.transcribePct = null;` (before setting `transcribing`), and in both `onRecordSaved` and `onRecordError` handlers add `this.transcribePct = null;` next to `this.transcribing = false;`.

- [ ] **Step 2: Show the bar in RecordScreen**

In `src/screens/RecordScreen.svelte`, import `ProgressBar` (`import ProgressBar from "../lib/ProgressBar.svelte";`) and replace :133-135:

```svelte
    {#if record.transcribing}
      <div class="transcribe">
        <ProgressBar
          pct={record.transcribePct}
          label={record.transcribePct === null
            ? "Transcribing on your Mac…"
            : `Transcribing on your Mac… ${record.transcribePct}%`}
        />
      </div>
    {/if}
```

Add a style scoped like the neighboring `.status` rules:

```css
  .transcribe {
    width: min(320px, 100%);
    margin: 8px auto 0;
  }
```

(Read the surrounding styles first and match the screen's actual layout — center or left-align consistently with the `.status` lines it replaces.)

- [ ] **Step 3: Type-check and test**

Run: `npm run check 2>&1 | tail -5` and `npm test 2>&1 | tail -5`
Expected: 0 errors; suite passes.

- [ ] **Step 4: Commit**

```bash
git add src/lib/record.svelte.ts src/screens/RecordScreen.svelte
git commit -m "feat(record): percent bar during post-stop transcription

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 8: Full verification

**Files:** none (verification only)

- [ ] **Step 1: Full automated pass**

Run from repo root:

```bash
cargo test -p ken-core 2>&1 | tail -5
cargo check --workspace 2>&1 | tail -5
npm run check 2>&1 | tail -5
npm test 2>&1 | tail -5
```

Expected: all green. (Note: `cargo check --workspace` compiles src-tauri too; the whisper feature is enabled per the app's default features — do not change feature flags.)

- [ ] **Step 2: Live app verification**

Use the project's `verify` skill (build + launch the Tauri app). Verify:
1. Open a video without a transcript → Generate transcript → "Preparing audio…" then a moving percent bar → finished transcript appears.
2. Model download dialog still shows its bar and completes (no regression from the refactor).
3. If a cloud-only file is available: open it → "Downloading from the cloud" shows a byte bar. If no cloud placeholder exists in the test environment, note that this path was verified by compilation + the cloud.rs unit tests only.
4. Record a short clip → stop → "Transcribing on your Mac… N%" advances.

- [ ] **Step 3: Report**

Report actual command output and what was observed in the app; do not claim success without it.

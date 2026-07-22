# Progress Bars for Downloads and Transcript Processing — Design

**Date:** 2026-07-22
**Status:** Approved

## Goal

Show determinate progress bars, inline where the user is waiting, for:

1. **Cloud file hydration** — downloading cloud-offline files (on-demand open and background worker).
2. **Transcript processing** — video/audio transcription (manual from the video player, auto-on-index queue, and screen-recording transcription).

Model downloads already have an end-to-end percent bar (`download_to` → `model-download-progress` → `ModelDownloadDialog.svelte`) and are unchanged, except that their bar markup is extracted into a shared component.

**Placement decision:** inline only. No global/aggregate status UI. Background queue work stays invisible except when the affected file is on screen.

## Approach

Extend the existing, proven model-download pattern: named Tauri events emitted from Rust with throttling, typed listeners in `src/lib/api.ts`, and a percent bar in the relevant Svelte component. No polling, no generic progress bus, no changes to the working model-download flow beyond componentizing its bar.

## 1. Rust core — `crates/ken-core/src/transcript.rs`

- Add an `on_progress` callback parameter threaded through `generate_vtt` → `transcribe`, and through `generate_and_cache`. Signature style follows `model::download_to`'s `on_progress` (generic `FnMut`).
- Progress is reported as two phases:
  - `Extracting` — ffmpeg audio extraction; quick and unmeterable, no percent.
  - `Transcribing(pct)` — whisper inference, 0–100.
- In `transcribe`, register whisper-rs's `set_progress_callback_safe` to forward whisper's native percent to the callback. `set_print_progress(false)` stays (that only controls stderr printing).
- Callers that don't care pass a no-op closure.

## 2. Tauri layer — `src-tauri/src/lib.rs`

### New event: `transcript-progress`

Payload:

```rust
struct TranscriptProgress {
    rel_path: String,              // key the UI matches on; for recordings, the recording's rel path
    phase: String,                 // "extracting" | "transcribing"
    pct: Option<u8>,               // Some(0..=100) only for "transcribing"
}
```

Emitted from all three transcription paths:

- `spawn_transcription` (auto-on-index queue),
- `generate_transcript` (manual, from the video player),
- the recording-finish transcription path.

Emission is de-duplicated on percent change (whisper's callback already fires at ~1% granularity, so no time throttle needed).

### New event: `hydration-progress`

Payload:

```rust
struct HydrationProgress {
    rel_path: String,
    downloaded: u64,
    total: u64,
}
```

- `download_placeholder` (on-demand hydration) and the background hydration worker chunk the byte read and emit this event, throttled via the existing `ProgressThrottle` (1% / 250ms).
- Both paths always emit; the frontend only renders progress when that file is currently on screen. This keeps the Rust side ignorant of UI state.

Existing error events are unchanged (`transcript-error`, coarse hydration status strings on failure).

## 3. Frontend

### Shared component: `ProgressBar.svelte`

- Extracted from `ModelDownloadDialog.svelte`'s bar markup (`role="progressbar"`, `.fill` width, label).
- Props: `pct` (number | null — null renders an indeterminate/pulsing state), `label`.
- `ModelDownloadDialog.svelte` is refactored to use it with no behavior change.

### `src/lib/api.ts`

- Payload types `TranscriptProgress`, `HydrationProgress`.
- Listeners `onTranscriptProgress`, `onHydrationProgress`, following the existing `onModelDownloadProgress` pattern.

### Wiring

- **`VideoPreview.svelte`** — while status is `generating`, subscribe to `transcript-progress` filtered to its `rel_path`. Show "Preparing audio…" (indeterminate) during `extracting`, then the percent bar during `transcribing`. Falls back to the current indeterminate message if no events arrive (e.g., older cached state).
- **`RecordScreen.svelte` / `record.svelte.ts`** — `record.svelte.ts` tracks `transcribePct` from `transcript-progress`; the "Transcribing on your Mac…" indeterminate text becomes a percent bar.
- **Hydration** — `PreviewLoading.svelte` gains an optional `progress` prop (pct + byte label). Preview panes that wait on a cloud-offline file listen for `hydration-progress` matching their `rel_path` and swap spinner → byte bar (e.g., "Downloading… 4.2 MB / 18 MB").

## 4. Error handling

Unchanged. Failures continue to surface through `transcript-error`, `record-error`, and existing hydration status strings. Progress UI resets to its prior state on error or completion (completion is signaled by the existing `index-updated` / status flips).

## 5. Testing

- **ken-core unit tests:** progress callback plumbing — `generate_vtt`/`generate_and_cache` invoke the callback with `Extracting` before any `Transcribing`, percentages are monotonic and clamped to 0–100, no-op callback path compiles and runs. Use the existing fake-source/test patterns in `transcript.rs` tests.
- **lib.rs:** event de-dup on unchanged percent (unit-testable helper, mirroring `should_emit`).
- **Frontend:** `ProgressBar.svelte` render states (determinate, indeterminate) per existing component test patterns; listener wiring covered by existing api.ts conventions.
- **End-to-end:** manual verification via the `verify` skill — open a video, trigger transcription, observe the bar; hydrate a cloud-offline file and observe the byte bar.

## Out of scope

- Global/aggregate progress UI (TitleBar/HomeStatus).
- Reworking the model-download event flow.
- Progress for the ffmpeg extraction phase beyond an indeterminate label.
- App-updater downloads (no updater download code exists in the app today).

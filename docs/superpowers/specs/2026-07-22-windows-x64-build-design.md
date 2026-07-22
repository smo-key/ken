# Windows x64 Build Support — Design

Date: 2026-07-22
Status: Approved

## Goal

Ken builds for `x86_64-pc-windows-msvc`: the Rust workspace compiles, and
`pnpm tauri build` produces the NSIS installer plus signed updater artifacts
on a `windows-latest` GitHub runner. Verification is a green Windows CI job.

## Background

- `release.yml` already contains a `windows-latest` matrix leg, but no release
  has ever been tagged, so the Windows path has never run.
- `crates/ken-core/Cargo.toml` enables the `metal` feature on `whisper-rs` and
  `llama-cpp-2` unconditionally. Metal is macOS-only; this breaks (or silently
  mis-configures) non-mac builds.
- whisper-rs and llama-cpp-2 each bundle a ggml; the linker resolves shared
  ggml symbols from whisper's copy. Both crates must therefore enable the
  same backend on each platform, or llama silently loses its GPU backend.
- Ubuntu CI currently fails in `alsa-sys` (cpal needs `libasound2-dev`),
  masking any other cross-platform breakage.
- Much of ken-core is already platform-aware: `cloud.rs` has Windows
  placeholder-attribute handling; OCR and record are cfg-gated to macOS with
  non-mac fallbacks.

## Decisions

- **Windows GPU backend: Vulkan** (user choice) — `vulkan` feature on both
  whisper-rs 0.16 and llama-cpp-2 0.1.151 (both verified to expose it).
  Building ggml's Vulkan backend needs the Vulkan SDK (`glslc`) on the
  build machine; at runtime `vulkan-1.dll` ships with GPU drivers.
- **Scope: CI-verified compile + bundle** — a Windows CI job that compiles the
  workspace and produces the NSIS installer. Rust tests remain Ubuntu-only:
  the ken-core runner tests drive a fake `claude` shell script and curl hook
  callbacks, which are not Windows-portable; porting the test harness is out
  of scope.

## Changes

### 1. `crates/ken-core/Cargo.toml` — target-gated backends

Move `whisper-rs` and `llama-cpp-2` into target-specific dependency tables,
still `optional = true` behind the existing `whisper` / `local-llm` features:

- `cfg(target_os = "macos")` → `features = ["metal"]` (unchanged)
- `cfg(windows)` → `features = ["vulkan"]` on both crates
- `cfg(not(any(target_os = "macos", windows)))` → no backend feature (CPU),
  which is what Linux CI exercises

Preserve the existing comment explaining the shared-ggml linking constraint.

### 2. `ci.yml` — Windows job + Ubuntu fix

- Add `libasound2-dev` to the Ubuntu apt install list (fixes current red CI).
- New `build-windows` job on `windows-latest`:
  - Vulkan SDK via `humbletim/setup-vulkan-sdk` (pinned, with SDK version)
  - pnpm 11 / node 24 / stable Rust / `swatinem/rust-cache`
  - `pnpm install`, build `ken-mcp` sidecar into `src-tauri/binaries/` with
    the target-triple name (same recipe as release.yml)
  - `pnpm tauri build` producing the NSIS bundle; pass
    `TAURI_SIGNING_PRIVATE_KEY(_PASSWORD)` secrets since
    `createUpdaterArtifacts: true` makes the build fail without them
- Runs on the same push/PR triggers as the existing job.

### 3. `release.yml` — make the Windows leg real

Add the same Vulkan SDK setup step to the Windows matrix leg (conditioned on
`runner.os == 'Windows'`). Everything else (sidecar naming, NSIS upload via
tauri-action, `ken-mcp-<triple>.exe` asset) already exists.

### 4. Fix what the Windows build surfaces

Any compile errors revealed by the first Windows CI run (e.g. stray
`std::os::unix` usage) are fixed iteratively until the job is green.

## Error handling / risks

- Vulkan SDK action failure or SDK version drift → pin the SDK version.
- ggml Vulkan build needs cmake + MSVC — both preinstalled on windows-latest.
- Fork PRs lack signing secrets; acceptable for a personal repo (the Windows
  job would fail there, same as release would).

## Testing / verification

- Green `build-windows` CI job on a draft PR from this branch, with the NSIS
  installer produced in the tauri build step (evidence in the job log).
- Existing Ubuntu job returns to green (ALSA fix), keeping test coverage.

# Windows x64 Build Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ken (workspace + Tauri NSIS bundle) builds for `x86_64-pc-windows-msvc`, verified by a green `windows-latest` CI job.

**Architecture:** Target-gate the ggml GPU backend features per platform in `ken-core` (Metal on macOS, Vulkan on Windows, CPU elsewhere), then add a Windows build job to CI and a Vulkan SDK setup step to the release workflow. Verification is CI on a draft PR, iterated until green.

**Tech Stack:** Cargo target-specific dependencies, GitHub Actions (`windows-latest`), LunarG Vulkan SDK, tauri-action/NSIS.

**Spec:** `docs/superpowers/specs/2026-07-22-windows-x64-build-design.md`

## Global Constraints

- whisper-rs stays at `0.16`, llama-cpp-2 stays pinned at `=0.1.151`.
- whisper-rs and llama-cpp-2 MUST enable the same ggml backend feature on each platform (shared-ggml linking: the linker resolves ggml symbols from whisper's copy — see the comment in `crates/ken-core/Cargo.toml`). Preserve that comment.
- macOS behavior must not change: `metal` on both crates.
- Rust tests stay Ubuntu-only (ken-core runner tests drive a fake `claude` shell script + curl; not Windows-portable).
- Workflow steps that run on all matrix legs must keep `shell: bash` where release.yml already uses it.
- This machine is macOS: Windows compilation can only be verified via CI. Do not claim success without a green `windows-latest` job.

---

### Task 1: Target-gate GPU backend features in ken-core

**Files:**
- Modify: `crates/ken-core/Cargo.toml` (deps at lines ~38–54, features at ~56–59, existing macOS target table at ~61)

**Interfaces:**
- Produces: features `whisper` / `local-llm` unchanged in name and default; `dep:whisper-rs` / `dep:llama-cpp-2` now resolve per-target.

- [ ] **Step 1: Move the two optional deps into target tables**

In `crates/ken-core/Cargo.toml`, delete the `whisper-rs = ...` and `llama-cpp-2 = ...` lines from `[dependencies]` (keep their doc comments, moving them as shown) and add, directly above the existing `[target.'cfg(target_os = "macos")'.dependencies]` section:

```toml
# On-device speech-to-text (whisper.cpp) and embedded LLM (llama.cpp) for
# quick answers and Map entity extraction. Both bundle ggml and build it from
# source (cmake + C/C++ toolchain), so each is gated behind a default feature:
# everything else in ken-core still builds with `--no-default-features`.
#
# The GPU backend feature is target-specific, and it is load-bearing that BOTH
# crates enable the SAME backend on each platform: whisper-rs and llama-cpp-2
# each bundle a ggml, and the linker resolves the shared ggml symbols from
# whisper's copy. A mismatch silently strips the GPU backend from llama.cpp
# (all layers land on CPU despite n_gpu_layers).
#   macOS   -> metal
#   Windows -> vulkan (needs the Vulkan SDK's glslc at build time; at runtime
#              vulkan-1.dll ships with GPU drivers)
#   other   -> CPU-only (what Linux CI exercises)
[target.'cfg(windows)'.dependencies]
whisper-rs = { version = "0.16", features = ["vulkan"], optional = true }
llama-cpp-2 = { version = "=0.1.151", default-features = false, features = ["vulkan"], optional = true }

[target.'cfg(not(any(target_os = "macos", windows)))'.dependencies]
whisper-rs = { version = "0.16", optional = true }
llama-cpp-2 = { version = "=0.1.151", default-features = false, optional = true }
```

Then add to the EXISTING `[target.'cfg(target_os = "macos")'.dependencies]` table (keep the screencapturekit/objc2 entries as-is), at its top:

```toml
whisper-rs = { version = "0.16", features = ["metal"], optional = true }
llama-cpp-2 = { version = "=0.1.151", default-features = false, features = ["metal"], optional = true }
```

The `[features]` section stays exactly:

```toml
[features]
default = ["whisper", "local-llm"]
whisper = ["dep:whisper-rs"]
local-llm = ["dep:llama-cpp-2"]
```

- [ ] **Step 2: Verify the macOS build still resolves metal**

Run: `cargo tree -p ken-core -e features -i whisper-rs | head -20`
Expected: `whisper-rs` present with `metal` feature listed.

Run: `cargo check -p ken-core`
Expected: finishes without errors (first run rebuilds whisper/llama via cmake; takes minutes).

- [ ] **Step 3: Verify the feature gate still works**

Run: `cargo check -p ken-core --no-default-features`
Expected: success, and much faster (no whisper/llama compile).

- [ ] **Step 4: Commit**

```bash
git add crates/ken-core/Cargo.toml Cargo.lock
git commit -m "build(core): target-gate ggml backends — metal on macOS, vulkan on Windows"
```

---

### Task 2: CI — fix Ubuntu ALSA dep and add Windows build job

**Files:**
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: Task 1's per-target features (Windows job builds with `vulkan`).
- Produces: job named `build-windows` whose green run is the project's Windows-build proof.

- [ ] **Step 1: Add `libasound2-dev` to the Ubuntu apt list**

In the `Install Tauri system dependencies` step of the `test` job, add one line to the `apt-get install` list (cpal → alsa-sys needs it; this is why main CI is currently red):

```yaml
            libasound2-dev \
```

- [ ] **Step 2: Add the Windows job**

Append to `jobs:` in `.github/workflows/ci.yml`:

```yaml
  # Proves Ken builds for x86_64-pc-windows-msvc: workspace compile with the
  # vulkan ggml backend, plus the NSIS installer via `tauri build`. Rust tests
  # stay on the Ubuntu job — the ken-core runner tests drive a fake `claude`
  # shell script and curl hook callbacks, which are not Windows-portable.
  build-windows:
    runs-on: windows-latest
    steps:
      - uses: actions/checkout@v4

      # ggml's Vulkan backend compiles its shaders at build time with glslc,
      # which ships in the LunarG SDK (same install recipe as llama.cpp's CI).
      - name: Install Vulkan SDK
        shell: pwsh
        env:
          VULKAN_VERSION: "1.4.309.0"
        run: |
          curl.exe -o "$env:RUNNER_TEMP\VulkanSDK-Installer.exe" -L "https://sdk.lunarg.com/sdk/download/$env:VULKAN_VERSION/windows/VulkanSDK-$env:VULKAN_VERSION-Installer.exe"
          & "$env:RUNNER_TEMP\VulkanSDK-Installer.exe" --accept-licenses --default-answer --confirm-command install | Out-Null
          Add-Content $env:GITHUB_ENV "VULKAN_SDK=C:\VulkanSDK\$env:VULKAN_VERSION"
          Add-Content $env:GITHUB_PATH "C:\VulkanSDK\$env:VULKAN_VERSION\Bin"

      - uses: pnpm/action-setup@v4
        with:
          version: 11

      - uses: actions/setup-node@v4
        with:
          node-version: 24
          cache: pnpm

      - uses: dtolnay/rust-toolchain@stable

      - uses: swatinem/rust-cache@v2

      - name: Install frontend dependencies
        run: pnpm install

      - name: Build ken-mcp sidecar
        shell: bash
        run: |
          cargo build --release -p ken-mcp
          mkdir -p src-tauri/binaries
          triple=$(rustc -vV | sed -n 's/^host: //p')
          cp target/release/ken-mcp.exe "src-tauri/binaries/ken-mcp-$triple.exe"

      # createUpdaterArtifacts is enabled in tauri.conf.json, so `tauri build`
      # needs the updater signing key even in CI.
      - name: Build app bundle (NSIS)
        env:
          TAURI_SIGNING_PRIVATE_KEY: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY }}
          TAURI_SIGNING_PRIVATE_KEY_PASSWORD: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY_PASSWORD }}
        run: pnpm tauri build
```

- [ ] **Step 3: Sanity-check the YAML**

Run: `python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/ci.yml'))" && echo OK`
Expected: `OK`

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: add windows-latest build job (vulkan) and fix Ubuntu ALSA dep"
```

---

### Task 3: Release workflow — Vulkan SDK step and Linux ALSA dep

**Files:**
- Modify: `.github/workflows/release.yml`

**Interfaces:**
- Consumes: same Vulkan install recipe as Task 2 (keep the two steps textually identical so they stay in sync).

- [ ] **Step 1: Add `libasound2-dev` to the Linux apt list**

Same one-line addition as Task 2 Step 1, in the `Install Tauri system dependencies (Linux)` step.

- [ ] **Step 2: Add the Vulkan SDK step to the Windows matrix leg**

Insert after the Linux dependencies step, before the pnpm setup:

```yaml
      # ggml's Vulkan backend compiles its shaders at build time with glslc,
      # which ships in the LunarG SDK (same install recipe as llama.cpp's CI).
      - name: Install Vulkan SDK (Windows)
        if: runner.os == 'Windows'
        shell: pwsh
        env:
          VULKAN_VERSION: "1.4.309.0"
        run: |
          curl.exe -o "$env:RUNNER_TEMP\VulkanSDK-Installer.exe" -L "https://sdk.lunarg.com/sdk/download/$env:VULKAN_VERSION/windows/VulkanSDK-$env:VULKAN_VERSION-Installer.exe"
          & "$env:RUNNER_TEMP\VulkanSDK-Installer.exe" --accept-licenses --default-answer --confirm-command install | Out-Null
          Add-Content $env:GITHUB_ENV "VULKAN_SDK=C:\VulkanSDK\$env:VULKAN_VERSION"
          Add-Content $env:GITHUB_PATH "C:\VulkanSDK\$env:VULKAN_VERSION\Bin"
```

- [ ] **Step 3: Sanity-check the YAML**

Run: `python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/release.yml'))" && echo OK`
Expected: `OK`

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "release: install Vulkan SDK on Windows leg, add Linux ALSA dep"
```

---

### Task 4: Push, open draft PR, iterate to green

**Files:**
- None planned; fixes for whatever the Windows compiler surfaces (likely candidates: `std::os::unix` imports, path handling, `#[cfg]` gaps in `src-tauri/src/lib.rs` or `crates/ken-core`).

**Interfaces:**
- Consumes: `build-windows` job from Task 2.

- [ ] **Step 1: Push the branch and open a draft PR**

```bash
git push -u origin worktree-windows-x64-build
gh pr create --draft --title "Windows x64 build support" --body "Target-gates ggml backends (metal/vulkan/CPU), adds a windows-latest CI job, and makes the release Windows leg real. Verification: green build-windows job.

🤖 Generated with [Claude Code](https://claude.com/claude-code)"
```

- [ ] **Step 2: Watch the `build-windows` job**

Run: `gh run watch $(gh run list --branch worktree-windows-x64-build --workflow=ci.yml --limit 1 --json databaseId --jq '.[0].databaseId') --exit-status`
Expected: exit 0 with `build-windows` green. The `tauri build` step log must show an `.exe` NSIS bundle under `target\release\bundle\nsis\`.

- [ ] **Step 3: If the Windows job fails, fix and repeat**

For each failure: read the failing step's log (`gh run view <id> --log-failed`), fix the root cause (Vulkan SDK URL/version, cfg-gating a unix-only code path, missing `shell: bash`), commit with a message naming the actual error, push, and re-run Step 2. Do not weaken the build (e.g. disabling `whisper`/`local-llm` on Windows) without user sign-off — that changes the approved design.

- [ ] **Step 4: Confirm Ubuntu job is green too**

Expected: the `test` job passes with the ALSA fix (it was the only failure on main's last runs).

- [ ] **Step 5: Mark PR ready**

```bash
gh pr ready
```

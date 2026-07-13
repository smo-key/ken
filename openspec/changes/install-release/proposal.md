# Proposal: install-release

## Why

Ken exists to serve non-technical teams, but today the only way to get it is
to clone the repo and build a Rust + Svelte workspace by hand. This change
(10 of 10 in the build order) closes the loop: a one-line installer, an
automated release pipeline that turns a version tag into signed-ready
platform bundles on GitHub Releases, and continuous integration so main
stays releasable. The design doc (§4.11) calls for exactly this: detect
OS/arch, download the latest release, put the app and `ken-mcp` in place,
and guide the user toward Claude Code if it's missing.

## What Changes

- **`install.sh`** at the repo root, runnable as
  `curl -fsSL https://raw.githubusercontent.com/smo-key/ken/main/install.sh | sh`:
  - Detects OS (macOS / Linux) and CPU (Apple Silicon / Intel / arm64 /
    x86_64). On Windows it prints a friendly pointer to the releases page
    and exits cleanly.
  - Downloads the latest GitHub Release: on macOS the `.app` bundle
    tarball (`Ken-<target-triple>.app.tar.gz`) into `/Applications`; on
    Linux the AppImage (`Ken-<target-triple>.AppImage`) into
    `~/.local/bin/ken`.
  - Installs the `ken-mcp` binary asset (`ken-mcp-<target-triple>`) to
    `~/.local/bin/ken-mcp` and warns politely if `~/.local/bin` isn't on
    PATH.
  - If no release (or no asset for this platform) exists yet, falls back
    to building from source: checks for git / Rust / Node / pnpm with
    plain-language install guidance, uses the current checkout or clones
    fresh, runs `pnpm install && pnpm tauri build`, installs the produced
    bundle, and builds `ken-mcp` with `cargo build --release -p ken-mcp`.
  - Checks whether the `claude` CLI is installed; if not, explains that
    Ken's AI features need it (`npm install -g @anthropic-ai/claude-code`,
    then run `claude` once to log in) and that everything else works
    without it.
  - Supports a `KEN_INSTALL_PREFIX` override so the script can be tested
    into a temporary directory.
- **`.github/workflows/release.yml`**: on tags `v*`, builds with
  `tauri-apps/tauri-action` on a matrix — macOS arm64 (macos-14), macOS
  x64 (macos-15-intel), Linux (ubuntu-22.04), Windows (windows-latest) — and
  publishes a draft GitHub Release with the platform bundles. Each job
  additionally builds `ken-mcp` in release mode and uploads it as a
  release asset named `ken-mcp-<target-triple>`; macOS jobs upload the
  app-bundle tarball and the Linux job the AppImage under the
  deterministic names `install.sh` expects.
- **`.github/workflows/ci.yml`**: on push / PR to main, runs
  `cargo test --workspace`, `pnpm install`, `pnpm test`, `pnpm check`,
  and `pnpm build` on ubuntu-22.04 with the Tauri v2 system dependencies.
  Rust tests need no real Claude — the runner tests use a fake `claude`
  script (bash + curl, both present on runners).
- **README**: an Install section with the one-liner, a plain "or download
  from the releases page" link for non-technical users, and the Claude
  Code prerequisite note for AI features.

## Capabilities

### New Capabilities
- `install-distribution`: the one-line installer (download path, source
  fallback, `ken-mcp` on PATH, Claude Code guidance) and the release / CI
  pipelines that produce what it installs.

### Modified Capabilities

_None — nothing in the app, ken-core, or ken-mcp changes; this change is
packaging and automation around the existing binaries._

## Impact

- New files: `install.sh`, `.github/workflows/release.yml`,
  `.github/workflows/ci.yml`.
- README gains an Install section.
- No code changes in `crates/`, `src-tauri/`, or `src/`.
- External surface: GitHub Releases on `smo-key/ken` become the
  distribution channel; asset names form a small contract between
  `release.yml` and `install.sh`.

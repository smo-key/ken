# Tasks: install-release

## 1. Installer

- [x] 1.1 `install.sh` (POSIX sh): OS/arch detection (macOS arm64/x64, Linux x64/arm64, Windows → friendly releases-page pointer + exit 0), plain-language messaging throughout
- [x] 1.2 Release download path: fetch `Ken-<triple>.app.tar.gz` (macOS → /Applications) or `Ken-<triple>.AppImage` (Linux → ~/.local/bin/ken) and `ken-mcp-<triple>` (→ ~/.local/bin/ken-mcp, chmod +x) from `releases/latest/download/`; PATH warning for ~/.local/bin; `KEN_INSTALL_PREFIX` and `KEN_DOWNLOAD_BASE` overrides for testing
- [x] 1.3 Source fallback on 404: prerequisite checks (git/cargo/node/pnpm) with one-line install guidance, in-place checkout detection vs fresh clone, `pnpm install && pnpm tauri build`, install bundle + `cargo build --release -p ken-mcp`; non-404 network errors abort with a retry message instead
- [x] 1.4 Claude Code check: `command -v claude`; if missing print npm install command + one-time `claude` login note, AI-features-only caveat; never a failure

## 2. Release workflow

- [x] 2.1 `.github/workflows/release.yml`: tags `v*`, matrix macos-14 (aarch64-apple-darwin) / macos-15-intel (x86_64-apple-darwin; macos-13 is retired) / ubuntu-22.04 / windows-latest; pnpm/action-setup + Node 24 + Rust stable + swatinem/rust-cache; Ubuntu Tauri v2 system deps; `tauri-apps/tauri-action@v0` publishing a draft release
- [x] 2.2 Per-job asset steps: `cargo build --release -p ken-mcp`, upload as `ken-mcp-<target-triple>` (`.exe` on Windows); macOS jobs tar the `.app` and upload `Ken-<triple>.app.tar.gz`; Linux uploads `Ken-<triple>.AppImage` — attached to tauri-action's release id, delete-then-upload for re-runs

## 3. CI workflow

- [x] 3.1 `.github/workflows/ci.yml`: push/PR to main, ubuntu-22.04, Tauri v2 system deps, Rust stable + cache, pnpm + Node 24; `cargo test --workspace`, `pnpm test`, `pnpm check`, `pnpm build` (fake-claude tests need only bash + curl)

## 4. README

- [x] 4.1 Install section: one-liner, releases-page link for hand-downloads (and Windows), Claude Code prerequisite note for AI features

## 5. Verification

- [x] 5.1 `bash -n install.sh` and shellcheck clean; workflows YAML-parse; `cargo build --release -p ken-mcp` succeeds
- [x] 5.2 Simulated installer runs against a temp `KEN_INSTALL_PREFIX`: fake-release download path (local HTTP server) installs app + binaries; 404 → source-fallback branch reaches the prerequisite/build decision point; Windows and missing-prereq messaging checked

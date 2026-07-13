# Design: install-release

## Context

All nine feature changes land runnable from a dev checkout; distribution is
the missing last mile. The audience is non-technical (design §1), so the
installer must speak plainly, and the release pipeline must produce assets
whose names the installer can predict. The repo is a Cargo workspace
(`ken-core`, `ken-mcp`, `src-tauri`) with a pnpm/Vite frontend; `pnpm tauri
build` produces the platform bundles, `cargo build --release -p ken-mcp`
the MCP sidecar.

## Goals / Non-Goals

**Goals:**
- One command that gets a working Ken + `ken-mcp` onto macOS and Linux,
  with a graceful source-build fallback while no release exists yet.
- Tag-driven releases: push `v*`, get a draft GitHub Release with bundles
  for macOS (arm64 + x64), Linux, and Windows, plus per-platform
  `ken-mcp` binaries.
- CI that keeps main releasable (Rust tests, frontend tests, type check,
  production build).
- Testability: the installer can be pointed at a temp prefix and a fake
  download server; CI never needs a real Claude.

**Non-Goals:**
- Code signing / notarization (unsigned bundles in v1; macOS users may
  need to approve the app once in System Settings — documented, not
  automated).
- Auto-update (Tauri updater) — later change; the asset naming leaves room
  for it.
- A Windows installer script — Windows users get the `.msi`/`.exe` from
  the releases page; `install.sh` points them there.
- Homebrew / apt / winget packaging.

## Decisions

1. **Asset-name contract.** The workflow uploads deterministic,
   target-triple-suffixed names and `install.sh` downloads exactly these:
   - `Ken-aarch64-apple-darwin.app.tar.gz`, `Ken-x86_64-apple-darwin.app.tar.gz`
   - `Ken-x86_64-unknown-linux-gnu.AppImage`
   - `ken-mcp-<target-triple>` (plus `.exe` on Windows)
   tauri-action's own uploads (`.dmg`, `.msi`, NSIS `.exe`, `.deb`,
   `.rpm`) stay on the release for people downloading by hand; the
   installer never depends on tauri-action's naming (which encodes
   version/arch differently per platform and has changed across
   versions). The explicit uploads are done by a small `gh api` step
   against the release id that tauri-action outputs, so they attach to
   the same draft release. Re-runs delete a same-named asset first.
2. **`releases/latest/download/<asset>` over the JSON API.** The installer
   fetches `https://github.com/smo-key/ken/releases/latest/download/<asset>`
   directly: no jq dependency, no API rate limits, and a plain 404 both
   when no release exists and when this platform has no asset — one
   fallback path handles both. Overridable via `KEN_DOWNLOAD_BASE` for
   testing against a local HTTP server. (Note: `latest` only resolves
   after the draft release is published — publishing the draft is the
   release act.)
3. **POSIX sh, not bash.** The one-liner pipes to `sh`, which is dash on
   Debian/Ubuntu. The script uses only POSIX constructs (`command -v`,
   `case`, no arrays), passes `bash -n` and shellcheck, and sets
   `set -eu`.
4. **Install locations.** macOS app → `/Applications/Ken.app` (per-user
   fallback not needed; `/Applications` is admin-group writable, and the
   script says what to do if not). Linux AppImage → `~/.local/bin/ken`,
   `ken-mcp` → `~/.local/bin/ken-mcp` on both platforms (XDG-conventional,
   no sudo). `KEN_INSTALL_PREFIX=<dir>` reroutes to `<dir>/Applications`
   and `<dir>/bin` so tests never touch the real system.
5. **Source fallback shape.** Triggered only by download 404 (network
   errors abort with a clear message instead — building from source
   because Wi-Fi dropped would be hostile). Prerequisites are checked
   up front with one plain-language line each (what it is, where to get
   it) and the script exits before doing any work if something is
   missing. If the current directory is already a Ken checkout
   (`src-tauri/tauri.conf.json` present and `package.json` says "ken") it
   builds in place; otherwise it clones to a temp dir. Installs the same
   artifacts the release path would.
6. **Release matrix without cross-compiling.** macos-14 is natively
   arm64 and macos-15-intel natively x64, so no `--target` flags or extra
   rustup targets: `pnpm tauri build` and `cargo build --release -p
   ken-mcp` both produce native binaries at predictable
   `target/release/...` paths. The target triple for asset naming is read
   from `rustc -vV` at runtime rather than hard-coded per matrix leg.
7. **Draft releases.** tauri-action publishes the release as a draft; a
   human reviews the assets and hits Publish. That keeps a broken matrix
   leg from shipping a half-release, at the cost of one manual click.
8. **CI scope.** One ubuntu-22.04 job: Tauri v2 system deps
   (webkit2gtk-4.1 et al. — needed to compile `src-tauri` in
   `cargo test --workspace`), Rust stable + `swatinem/rust-cache`, pnpm
   via `pnpm/action-setup` + Node 24, then
   `cargo test --workspace`, `pnpm test`, `pnpm check`, `pnpm build`.
   The ken-core runner tests spawn a fake `claude` shell script and POST
   hook callbacks with `curl`; both exist on GitHub runners, so CI needs
   no Anthropic credentials.
9. **Claude Code check is advice, not a gate.** The installer ends by
   checking `command -v claude` and, when missing, prints the npm install
   command and the one-time login step, stating plainly that only AI
   features need it. It never installs Node packages on the user's
   behalf.

## Risks / Trade-offs

- **Unsigned macOS app**: Gatekeeper may warn on first launch of a
  releases-page download. The installer's curl download avoids the
  quarantine attribute, but the README wording stays honest that the
  build is unsigned. Signing is deferred deliberately.
- **Asset-name drift**: if tauri.conf.json's `productName` changes, the
  tarball step still works (it globs the produced `.app`), but
  `install.sh` looks for `Ken-*` names — the contract is documented at
  the top of both files.
- **`releases/latest` with only drafts** returns 404, so the installer
  falls back to source until the first release is published — acceptable
  and explicitly messaged ("No packaged release yet").
- **Two macOS legs, one tauri-action release**: tauri-action serializes
  release creation by tag; both jobs attach to the same draft. Its own
  generically-named artifacts may collide across arches, but nothing the
  installer uses does; the explicit uploads are triple-suffixed.

## Open Questions

_None blocking. Signing identities and an auto-update channel are future
changes._

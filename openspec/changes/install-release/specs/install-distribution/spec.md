# install-distribution

## ADDED Requirements

### Requirement: One-line install on macOS and Linux
The repo SHALL ship an `install.sh` at its root, runnable as
`curl -fsSL https://raw.githubusercontent.com/smo-key/ken/main/install.sh | sh`,
that detects the operating system and CPU architecture, downloads the
matching assets from the latest GitHub Release of `smo-key/ken`, installs
the Ken app (macOS: `.app` bundle into `/Applications`; Linux: AppImage as
`~/.local/bin/ken`), and installs the `ken-mcp` binary as
`~/.local/bin/ken-mcp` with execute permission. All output SHALL be plain
language suitable for non-technical users. The script SHALL be valid POSIX
shell.

#### Scenario: Fresh install on Apple Silicon
- **WHEN** the one-liner runs on an arm64 Mac and a published release
  exists
- **THEN** `Ken.app` is installed into `/Applications`, `ken-mcp` is
  installed to `~/.local/bin/ken-mcp` and is executable, and the script
  reports where each landed and how to open the app

#### Scenario: Fresh install on Linux
- **WHEN** the one-liner runs on an x86_64 Linux machine and a published
  release exists
- **THEN** the AppImage is installed as `~/.local/bin/ken` (executable)
  alongside `~/.local/bin/ken-mcp`, and the script says how to launch Ken

#### Scenario: Windows gets a friendly pointer
- **WHEN** the script runs on Windows (Git Bash, MSYS, Cygwin, or WSL
  detection aside — any `uname` reporting a Windows environment)
- **THEN** it prints a friendly note pointing to the GitHub releases page
  for the Windows installer and exits with status 0

#### Scenario: bin directory not on PATH
- **WHEN** installation succeeds but `~/.local/bin` is not on the user's
  PATH
- **THEN** the script politely explains how to add it, and does not treat
  this as a failure

#### Scenario: Testable without touching the system
- **WHEN** `KEN_INSTALL_PREFIX=<dir>` is set
- **THEN** the app installs under `<dir>/Applications` and binaries under
  `<dir>/bin`, and nothing is written to `/Applications` or
  `~/.local/bin`

### Requirement: Source-build fallback when no release exists
`install.sh` SHALL fall back to building from source when the release
download returns 404 (no release published yet, or no asset for this
platform): it SHALL check for git, Rust (cargo), Node.js, and pnpm — naming
anything missing with one plain-language line on how to install it — then
build with `pnpm install && pnpm tauri build`, install the produced
bundle, build `ken-mcp` with `cargo build --release -p ken-mcp`, and
install it. If run from inside a Ken checkout it SHALL build in place;
otherwise it SHALL clone the repo to a temporary directory. Network
failures other than 404 SHALL abort with a clear message rather than
silently switching to a source build.

#### Scenario: No release published yet
- **WHEN** the download of the app asset returns HTTP 404
- **THEN** the script says there is no packaged release yet, checks the
  build prerequisites, and proceeds to build from source

#### Scenario: Missing prerequisite stops before work starts
- **WHEN** the fallback is entered and pnpm is not installed
- **THEN** the script lists what is missing with install guidance (e.g.
  "pnpm — install with: npm install -g pnpm") and exits with a non-zero
  status without cloning or building anything

#### Scenario: Network failure is not a build trigger
- **WHEN** the download fails for a reason other than 404 (e.g. DNS
  failure, connection reset)
- **THEN** the script reports the connection problem and asks the user to
  retry, and does not start a source build

### Requirement: Claude Code guidance
After installing, `install.sh` SHALL check whether the `claude` CLI is on
PATH. If it is missing, the script SHALL print that Ken's AI features need
Claude Code — install with `npm install -g @anthropic-ai/claude-code` and
run `claude` once to log in — and that everything else in Ken works
without it. A missing `claude` SHALL NOT fail the install.

#### Scenario: claude missing is advice, not an error
- **WHEN** the install completes on a machine without the `claude` CLI
- **THEN** the script exits 0 and its final output includes the install
  command, the one-time login step, and the reassurance that only AI
  features need it

### Requirement: Tag-driven release pipeline
A GitHub Actions workflow (`.github/workflows/release.yml`) SHALL trigger
on tags matching `v*` and, via `tauri-apps/tauri-action`, build platform
bundles on a matrix of macOS arm64 (macos-14), macOS x64 (macos-15-intel),
Linux (ubuntu-22.04), and Windows (windows-latest), publishing them to a
draft GitHub Release. Each matrix job SHALL additionally run
`cargo build --release -p ken-mcp` and upload the resulting binary to the
same release as `ken-mcp-<target-triple>`. The macOS jobs SHALL upload the
app bundle as `Ken-<target-triple>.app.tar.gz` and the Linux job the
AppImage as `Ken-<target-triple>.AppImage`, matching exactly the names
`install.sh` downloads.

#### Scenario: Tagging a version produces a draft release
- **WHEN** a `v0.2.0` tag is pushed
- **THEN** a draft GitHub Release exists containing macOS (arm64 and x64)
  app bundles, a Linux AppImage, Windows installers, and four
  `ken-mcp-<target-triple>` binaries

#### Scenario: Installer and workflow agree on names
- **WHEN** the release assets are published
- **THEN** every asset name `install.sh` constructs for a supported
  platform exists on the release

### Requirement: Continuous integration on main
A GitHub Actions workflow (`.github/workflows/ci.yml`) SHALL run on pushes
and pull requests to main, on ubuntu-22.04 with the Tauri v2 system
dependencies installed, and SHALL run `cargo test --workspace`,
`pnpm install`, `pnpm test`, `pnpm check`, and `pnpm build`. The Rust
tests SHALL pass without a real Claude installation (the runner tests use
a fake `claude` script needing only bash and curl).

#### Scenario: CI validates a pull request
- **WHEN** a pull request against main is opened
- **THEN** the workflow runs Rust workspace tests, frontend tests, the
  Svelte type check, and a production frontend build, and fails the check
  if any step fails

### Requirement: README install instructions
The README SHALL contain an Install section presenting the one-line
install command, a plain link to the GitHub releases page for people who
prefer downloading by hand (including Windows users), and a note that
Ken's AI features require the Claude Code CLI while everything else works
without it.

#### Scenario: A non-technical user finds their path
- **WHEN** a user reads the README's Install section
- **THEN** they see the copy-paste one-liner, a releases-page link as the
  no-terminal alternative, and the Claude Code prerequisite explained in
  plain words

# Auto-update for Ken — Design

Date: 2026-07-22
Status: Approved

## Goal

Ken updates itself. When a new release is published, running apps download it in
the background and show a chip in the title bar (left of Chats): "Downloading
update…" while in flight, "Update ready — Restart" when installed. Clicking the
ready chip relaunches into the new version. The app never restarts on its own.

The one-line installer (`install.sh`) and tag-driven release pipeline
(`.github/workflows/release.yml`) already exist and are out of scope except
where the updater work touches them.

## Components

### 1. Update pipeline (infra)

- Updater signing keypair generated with `pnpm tauri signer generate`.
  - Public key: committed in `tauri.conf.json` under `plugins.updater.pubkey`.
  - Private key + password: GitHub Actions secrets `TAURI_SIGNING_PRIVATE_KEY`
    and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` (added manually by Arthur;
    documented in the release workflow header comment).
- `tauri.conf.json`:
  - `bundle.createUpdaterArtifacts: true`
  - `plugins.updater.endpoints: ["https://github.com/smo-key/ken/releases/latest/download/latest.json"]`
- `release.yml`: pass the two signing secrets as env to the `tauri-action`
  step. tauri-action then produces `.app.tar.gz` + `.sig` updater artifacts and
  generates and uploads `latest.json` to the release. The explicitly named
  `install.sh` assets (`Ken-<triple>.app.tar.gz`, `ken-mcp-<triple>`) keep
  their existing names and upload path.

### 2. In-app updater

- Crates: `tauri-plugin-updater`, `tauri-plugin-process`. JS:
  `@tauri-apps/plugin-updater`, `@tauri-apps/plugin-process`. Capability
  entries in `src-tauri/capabilities/default.json`: `updater:default`,
  `process:allow-restart`.
- New store `src/lib/updater.svelte.ts` (Svelte 5 runes, same style as
  `app.svelte.ts` / `chats.svelte.ts`):
  - State machine: `idle → checking → downloading → ready`, plus `error`
    (silent — logged, retried on the next cycle; no chip shown).
  - Checks ~10s after launch, then every 4 hours.
  - On update found: `update.downloadAndInstall(progressCallback)` immediately
    (auto-download). Progress tracked for the chip tooltip.
  - `restart()`: calls `relaunch()` from plugin-process. Only ever invoked by
    the user clicking the chip.
  - Exposes `state`, `version` (target version), `progress` (0–1 or null).
- Dev builds skip checks entirely (`import.meta.env.DEV`). For end-to-end
  testing, temporarily point `plugins.updater.endpoints` at a local server
  serving a hand-made `latest.json` (the JS `check()` API has no runtime
  endpoint override).

### 3. Title-bar chip (`src/shell/TitleBar.svelte`)

- Rendered immediately left of the Chats button. Hidden when `idle`/`checking`/
  `error`.
- `downloading`: non-interactive chip, "Downloading update…", subtle pulse
  (reuse the existing `pulse` keyframes), tooltip shows percent.
- `ready`: clickable accent chip, "Update ready — Restart", tooltip
  "Restart Ken to update to v<version>". Click → `updater.restart()`.
- Visual family: same height/radius/typography as the Chats chip.

### 4. ken-mcp freshness

- Bundle `ken-mcp` into Ken.app as a Tauri sidecar: `bundle.externalBin:
  ["binaries/ken-mcp"]`; build step copies the compiled `ken-mcp` to
  `src-tauri/binaries/ken-mcp-<target-triple>` before `tauri build` (release
  workflow + Makefile).
- On startup (release builds only), Rust compares the bundled sidecar's bytes
  (hash) to `~/.local/bin/ken-mcp`; if missing or different, copy over and
  `chmod +x`. Idempotent; failures are logged, never fatal.
- install.sh continues to install ken-mcp on first install; this keeps it
  fresh across auto-updates.

## Error handling

- Update check/download failures: log via existing logging, stay silent in UI,
  retry next 4-hour cycle.
- Signature mismatch: plugin rejects the artifact; treated as any other error.
- ken-mcp copy failure (e.g. permissions): log warning, continue startup.

## Testing

- Unit tests for the updater store state machine with the plugin API mocked
  (Vitest, alongside existing `*.test.ts`).
- Rust unit test for the sidecar-refresh compare/copy logic against a temp dir.
- Manual E2E: build with a test keypair, serve a hand-made `latest.json` +
  artifact locally, point `KEN_UPDATE_URL` at it, verify chip states and
  relaunch in the real app.

## Out of scope

- Windows/Linux updater verification (config is cross-platform; only macOS is
  verified).
- Delta updates, release-notes UI, update channels.

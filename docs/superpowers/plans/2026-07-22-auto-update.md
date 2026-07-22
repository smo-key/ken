# Auto-update Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ken auto-downloads signed updates from GitHub Releases and shows a title-bar chip ("Downloading update…" → "Update ready — Restart") left of Chats; clicking the ready chip relaunches into the new version. The bundled ken-mcp sidecar refreshes `~/.local/bin/ken-mcp` on launch.

**Architecture:** `tauri-plugin-updater` + `tauri-plugin-process` (frontend-driven). Pure state-machine controller in `src/lib/updater.ts` (unit-tested, plugin API injected), thin Svelte-5-runes wrapper in `src/lib/updater.svelte.ts`, chip in `src/shell/TitleBar.svelte`. Release pipeline signs updater artifacts and publishes `latest.json`. ken-mcp is bundled as a Tauri sidecar (`externalBin`) and copied to `~/.local/bin` by Rust at startup when its bytes differ.

**Tech Stack:** Tauri 2, tauri-plugin-updater 2, tauri-plugin-process 2, Svelte 5 runes, Vitest 4, GitHub Actions (tauri-action).

## Global Constraints

- Spec: `docs/superpowers/specs/2026-07-22-auto-update-design.md`.
- The app must NEVER restart on its own; only the user clicking the ready chip triggers relaunch.
- Update errors are silent in the UI (no chip), logged to console, retried next cycle.
- Update checks are skipped entirely in dev builds (`import.meta.env.DEV`).
- Asset-name contract with install.sh must not change: `Ken-<triple>.app.tar.gz`, `Ken-<triple>.AppImage`, `ken-mcp-<triple>[.exe]`.
- Endpoint: `https://github.com/smo-key/ken/releases/latest/download/latest.json`.
- ken-mcp refresh: release builds only, never fatal on failure.
- Run frontend tests with `pnpm test`, Rust tests with `cargo test`.
- Repo package manager is pnpm.

---

### Task 1: Updater state-machine controller (pure TS)

**Files:**
- Create: `src/lib/updater.ts`
- Test: `src/lib/updater.test.ts`

**Interfaces:**
- Produces (consumed by Tasks 2 and 3):

```ts
export type UpdaterPhase = "idle" | "checking" | "downloading" | "ready" | "error";
export interface UpdaterSnapshot {
  phase: UpdaterPhase;
  version: string | null;   // target version once known
  progress: number | null;  // 0..1 while downloading, else null
}
export interface UpdateHandle {
  version: string;
  downloadAndInstall(
    onProgress: (event: DownloadEvent) => void,
  ): Promise<void>;
}
export type DownloadEvent =
  | { event: "Started"; data: { contentLength?: number } }
  | { event: "Progress"; data: { chunkLength: number } }
  | { event: "Finished" };
export class UpdateController {
  constructor(deps: {
    check: () => Promise<UpdateHandle | null>;
    onChange: (snapshot: UpdaterSnapshot) => void;
  });
  readonly snapshot: UpdaterSnapshot;
  runCheck(): Promise<void>; // resolves when the cycle ends (ready/idle/error)
}
```

- [ ] **Step 1: Write the failing tests**

```ts
// src/lib/updater.test.ts
import { describe, expect, it } from "vitest";
import {
  UpdateController,
  type DownloadEvent,
  type UpdaterSnapshot,
} from "./updater";

function collector() {
  const snapshots: UpdaterSnapshot[] = [];
  return { snapshots, onChange: (s: UpdaterSnapshot) => snapshots.push(s) };
}

describe("UpdateController", () => {
  it("stays idle when no update is available", async () => {
    const c = collector();
    const ctrl = new UpdateController({
      check: async () => null,
      onChange: c.onChange,
    });
    await ctrl.runCheck();
    expect(ctrl.snapshot).toEqual({ phase: "idle", version: null, progress: null });
    expect(c.snapshots.map((s) => s.phase)).toEqual(["checking", "idle"]);
  });

  it("downloads a found update and lands in ready", async () => {
    const c = collector();
    const ctrl = new UpdateController({
      check: async () => ({
        version: "0.2.0",
        async downloadAndInstall(onProgress: (e: DownloadEvent) => void) {
          onProgress({ event: "Started", data: { contentLength: 100 } });
          onProgress({ event: "Progress", data: { chunkLength: 50 } });
          onProgress({ event: "Progress", data: { chunkLength: 50 } });
          onProgress({ event: "Finished" });
        },
      }),
      onChange: c.onChange,
    });
    await ctrl.runCheck();
    expect(ctrl.snapshot).toEqual({ phase: "ready", version: "0.2.0", progress: null });
    // Progress reached 1.0 mid-download.
    const dl = c.snapshots.filter((s) => s.phase === "downloading");
    expect(dl[0].progress).toBe(0);
    expect(dl.at(-1)?.progress).toBe(1);
  });

  it("reports null progress when content length is unknown", async () => {
    const c = collector();
    const ctrl = new UpdateController({
      check: async () => ({
        version: "0.2.0",
        async downloadAndInstall(onProgress: (e: DownloadEvent) => void) {
          onProgress({ event: "Started", data: {} });
          onProgress({ event: "Progress", data: { chunkLength: 10 } });
          onProgress({ event: "Finished" });
        },
      }),
      onChange: c.onChange,
    });
    await ctrl.runCheck();
    const dl = c.snapshots.filter((s) => s.phase === "downloading");
    expect(dl.every((s) => s.progress === null)).toBe(true);
    expect(ctrl.snapshot.phase).toBe("ready");
  });

  it("moves to error when check throws, so the next cycle can retry", async () => {
    const ctrl = new UpdateController({
      check: async () => {
        throw new Error("network down");
      },
      onChange: () => {},
    });
    await ctrl.runCheck(); // must not reject
    expect(ctrl.snapshot).toEqual({ phase: "error", version: null, progress: null });
  });

  it("moves to error when download throws", async () => {
    const ctrl = new UpdateController({
      check: async () => ({
        version: "0.2.0",
        async downloadAndInstall() {
          throw new Error("disk full");
        },
      }),
      onChange: () => {},
    });
    await ctrl.runCheck();
    expect(ctrl.snapshot.phase).toBe("error");
  });

  it("does not re-check once ready (installed update must not be clobbered)", async () => {
    let checks = 0;
    const ctrl = new UpdateController({
      check: async () => {
        checks++;
        return {
          version: "0.2.0",
          async downloadAndInstall() {},
        };
      },
      onChange: () => {},
    });
    await ctrl.runCheck();
    await ctrl.runCheck();
    expect(checks).toBe(1);
    expect(ctrl.snapshot.phase).toBe("ready");
  });

  it("retries after an error cycle", async () => {
    let checks = 0;
    const ctrl = new UpdateController({
      check: async () => {
        checks++;
        if (checks === 1) throw new Error("flaky");
        return null;
      },
      onChange: () => {},
    });
    await ctrl.runCheck();
    expect(ctrl.snapshot.phase).toBe("error");
    await ctrl.runCheck();
    expect(ctrl.snapshot.phase).toBe("idle");
    expect(checks).toBe(2);
  });
});
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `pnpm vitest run src/lib/updater.test.ts`
Expected: FAIL — cannot resolve `./updater` / `UpdateController is not defined`.

- [ ] **Step 3: Implement the controller**

```ts
// src/lib/updater.ts
// Auto-update state machine, kept free of Tauri imports so it can be
// unit-tested. The Svelte wiring (updater.svelte.ts) injects the real
// plugin-updater API.

export type UpdaterPhase = "idle" | "checking" | "downloading" | "ready" | "error";

export interface UpdaterSnapshot {
  phase: UpdaterPhase;
  version: string | null;
  progress: number | null;
}

export type DownloadEvent =
  | { event: "Started"; data: { contentLength?: number } }
  | { event: "Progress"; data: { chunkLength: number } }
  | { event: "Finished" };

export interface UpdateHandle {
  version: string;
  downloadAndInstall(onProgress: (event: DownloadEvent) => void): Promise<void>;
}

interface Deps {
  check: () => Promise<UpdateHandle | null>;
  onChange: (snapshot: UpdaterSnapshot) => void;
}

export class UpdateController {
  #deps: Deps;
  #snapshot: UpdaterSnapshot = { phase: "idle", version: null, progress: null };

  constructor(deps: Deps) {
    this.#deps = deps;
  }

  get snapshot(): UpdaterSnapshot {
    return this.#snapshot;
  }

  #set(next: UpdaterSnapshot) {
    this.#snapshot = next;
    this.#deps.onChange(next);
  }

  async runCheck(): Promise<void> {
    // Once an update is installed on disk, another downloadAndInstall
    // could clobber it mid-restart; stay put until the user relaunches.
    if (this.#snapshot.phase === "ready" || this.#snapshot.phase === "downloading") {
      return;
    }
    this.#set({ phase: "checking", version: null, progress: null });
    let update: UpdateHandle | null;
    try {
      update = await this.#deps.check();
    } catch (err) {
      console.warn("update check failed", err);
      this.#set({ phase: "error", version: null, progress: null });
      return;
    }
    if (!update) {
      this.#set({ phase: "idle", version: null, progress: null });
      return;
    }
    const version = update.version;
    let total: number | null = null;
    let received = 0;
    this.#set({ phase: "downloading", version, progress: null });
    try {
      await update.downloadAndInstall((event) => {
        if (event.event === "Started") {
          total = event.data.contentLength ?? null;
          this.#set({
            phase: "downloading",
            version,
            progress: total ? 0 : null,
          });
        } else if (event.event === "Progress") {
          received += event.data.chunkLength;
          this.#set({
            phase: "downloading",
            version,
            progress: total ? Math.min(received / total, 1) : null,
          });
        }
      });
    } catch (err) {
      console.warn("update download failed", err);
      this.#set({ phase: "error", version: null, progress: null });
      return;
    }
    this.#set({ phase: "ready", version, progress: null });
  }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `pnpm vitest run src/lib/updater.test.ts`
Expected: 7 passed.

- [ ] **Step 5: Run the full frontend suite to check for regressions**

Run: `pnpm test`
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add src/lib/updater.ts src/lib/updater.test.ts
git commit -m "feat(update): updater state-machine controller"
```

---

### Task 2: Plugin wiring — Rust plugins, capabilities, npm deps, Svelte store

**Files:**
- Modify: `src-tauri/Cargo.toml` (dependencies section, after `tauri-plugin-opener`)
- Modify: `src-tauri/src/lib.rs` (the `tauri::Builder::default()` chain, ~line 4827)
- Modify: `src-tauri/capabilities/default.json`
- Modify: `package.json` (via pnpm add)
- Create: `src/lib/updater.svelte.ts`

**Interfaces:**
- Consumes: `UpdateController`, `UpdaterSnapshot`, `UpdateHandle`, `DownloadEvent` from `src/lib/updater.ts` (Task 1).
- Produces (consumed by Task 3):

```ts
// src/lib/updater.svelte.ts
export const updater: {
  readonly phase: UpdaterPhase;
  readonly version: string | null;
  readonly progress: number | null; // 0..1 or null
  start(): void;    // begins the delayed-first-check + 4h interval; no-op in dev or if already started
  restart(): Promise<void>; // relaunches the app; only call from the chip click
};
```

- [ ] **Step 1: Add Rust plugin dependencies**

In `src-tauri/Cargo.toml`, after the `tauri-plugin-opener = "2"` line add:

```toml
tauri-plugin-updater = "2"
tauri-plugin-process = "2"
```

- [ ] **Step 2: Register the plugins**

In `src-tauri/src/lib.rs`, in the builder chain:

```rust
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
```

- [ ] **Step 3: Grant capabilities**

In `src-tauri/capabilities/default.json`, extend `permissions`:

```json
  "permissions": [
    "core:default",
    "core:window:allow-start-dragging",
    "dialog:default",
    "opener:default",
    "updater:default",
    "process:allow-restart"
  ]
```

- [ ] **Step 4: Add JS packages**

Run: `pnpm add @tauri-apps/plugin-updater @tauri-apps/plugin-process`

- [ ] **Step 5: Verify Rust side compiles**

Run: `cargo check -p ken-app`
Expected: success (warnings ok). Note: the updater plugin requires `plugins.updater` config to exist before the app RUNS, but `cargo check` only compiles — config lands in Task 4.

- [ ] **Step 6: Write the Svelte store**

```ts
// src/lib/updater.svelte.ts
// Auto-update wiring (Svelte 5 runes). Checks ~10s after launch and every
// 4 hours; downloads silently; the TitleBar chip surfaces downloading/ready.
// Restart only ever happens from the user clicking the chip.
import { check } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { UpdateController, type UpdaterSnapshot } from "./updater";

const FIRST_CHECK_MS = 10_000;
const CHECK_EVERY_MS = 4 * 60 * 60 * 1000;

let snapshot = $state<UpdaterSnapshot>({
  phase: "idle",
  version: null,
  progress: null,
});
let started = false;

const controller = new UpdateController({
  check: async () => {
    const update = await check();
    if (!update) return null;
    return {
      version: update.version,
      downloadAndInstall: (onProgress) => update.downloadAndInstall(onProgress),
    };
  },
  onChange: (s) => (snapshot = s),
});

export const updater = {
  get phase() {
    return snapshot.phase;
  },
  get version() {
    return snapshot.version;
  },
  get progress() {
    return snapshot.progress;
  },
  start() {
    if (started || import.meta.env.DEV) return;
    started = true;
    setTimeout(() => void controller.runCheck(), FIRST_CHECK_MS);
    setInterval(() => void controller.runCheck(), CHECK_EVERY_MS);
  },
  async restart() {
    await relaunch();
  },
};
```

- [ ] **Step 7: Verify frontend compiles and tests pass**

Run: `pnpm check 2>/dev/null || npx svelte-check --threshold error; pnpm test`
(If the repo has no `check` script, `npx vite build` is an acceptable compile check.)
Expected: no new errors; tests pass.

- [ ] **Step 8: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/src/lib.rs src-tauri/capabilities/default.json package.json pnpm-lock.yaml src/lib/updater.svelte.ts
git commit -m "feat(update): wire tauri updater + process plugins and svelte store"
```

---

### Task 3: Title-bar chip

**Files:**
- Modify: `src/shell/TitleBar.svelte`

**Interfaces:**
- Consumes: `updater` from `src/lib/updater.svelte` (Task 2).

- [ ] **Step 1: Add the chip markup**

In `src/shell/TitleBar.svelte`:

Add to the imports in `<script>`:

```ts
  import { updater } from "../lib/updater.svelte";
  import RefreshCw from "@lucide/svelte/icons/refresh-cw";
```

Add in `<script>` (after the `syncTitle` derived):

```ts
  const updateTitle = $derived.by(() => {
    if (updater.phase === "downloading") {
      return updater.progress === null
        ? "Downloading update…"
        : `Downloading update… ${Math.round(updater.progress * 100)}%`;
    }
    return `Restart Ken to update to v${updater.version}`;
  });
```

Insert directly BEFORE the `<button class="chats"...>` element:

```svelte
  {#if updater.phase === "downloading"}
    <span class="update downloading" title={updateTitle}>
      <RefreshCw size={13} strokeWidth={1.75} aria-hidden="true" />
      Downloading update…
    </span>
  {:else if updater.phase === "ready"}
    <button class="update ready" title={updateTitle} onclick={() => updater.restart()}>
      <RefreshCw size={13} strokeWidth={1.75} aria-hidden="true" />
      Update ready — Restart
    </button>
  {/if}
```

Add to `<style>` (after the `.chats` rules; reuses the existing `pulse` keyframes defined for `.dot.busy`):

```css
  .update {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 7px;
    height: 28px;
    padding: 0 11px;
    border-radius: 8px;
    font-size: 12.5px;
    font-weight: 600;
  }
  .update.downloading {
    border: 1px solid var(--border-strong);
    background: var(--surface);
    color: var(--ink-tertiary);
  }
  .update.downloading :global(svg) {
    animation: pulse 1.2s ease-in-out infinite;
  }
  .update.ready {
    border: 1px solid color-mix(in srgb, var(--accent) 35%, transparent);
    background: color-mix(in srgb, var(--accent) 8%, transparent);
    color: var(--accent-deep);
    cursor: pointer;
  }
  .update.ready:hover {
    background: color-mix(in srgb, var(--accent) 16%, transparent);
  }
```

- [ ] **Step 2: Start the updater from the shell**

The chip only renders when the store leaves idle, and the store only runs
once `start()` is called. Call it where TitleBar mounts:

```ts
  // in TitleBar.svelte <script>, top level (runs once per app since the
  // title bar mounts once):
  updater.start();
```

- [ ] **Step 3: Verify compile + visual check in dev**

Run: `pnpm test` and launch per the `verify` skill (`make dev`).
Expected: tests pass; app runs; NO chip visible (dev skips checks — correct).
To eyeball the chip states without a real update, temporarily hardcode
`updater.phase` reads by editing the `{#if}` to `{#if true}` in a scratch run,
screenshot both states, then revert. (Do not commit the hack.)

- [ ] **Step 4: Commit**

```bash
git add src/shell/TitleBar.svelte
git commit -m "feat(update): title-bar update chip (downloading / restart)"
```

---

### Task 4: Updater config, signing keys, release workflow

**Files:**
- Modify: `src-tauri/tauri.conf.json`
- Modify: `.github/workflows/release.yml`

**Interfaces:**
- Consumes: nothing from other tasks (independent of Tasks 1–3 at build level; the running app needs this config for the plugin not to error).
- Produces: `plugins.updater.pubkey` consumed by the runtime; signed artifacts + `latest.json` consumed by the endpoint.

- [ ] **Step 1: Generate the signing keypair**

```bash
mkdir -p ~/.tauri
pnpm tauri signer generate -w ~/.tauri/ken-updater.key --password ""
```

This writes `~/.tauri/ken-updater.key` (private) and `~/.tauri/ken-updater.key.pub` (public). NEVER commit or print the private key file's contents into the repo. Read the `.pub` file's contents for the next step.

- [ ] **Step 2: Add updater config to `src-tauri/tauri.conf.json`**

Add `createUpdaterArtifacts` inside the existing `bundle` object, and a top-level `plugins` key (`<PUBKEY>` = contents of `~/.tauri/ken-updater.key.pub`):

```json
  "bundle": {
    "active": true,
    "createUpdaterArtifacts": true,
    "targets": "all",
    "icon": ["..."]
  },
  "plugins": {
    "updater": {
      "pubkey": "<PUBKEY>",
      "endpoints": [
        "https://github.com/smo-key/ken/releases/latest/download/latest.json"
      ]
    }
  }
```

(Keep every existing bundle field; only add the new keys.)

- [ ] **Step 3: Pass signing secrets to tauri-action in `.github/workflows/release.yml`**

In the "Build app bundles and create draft release" step, extend `env`:

```yaml
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          TAURI_SIGNING_PRIVATE_KEY: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY }}
          TAURI_SIGNING_PRIVATE_KEY_PASSWORD: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY_PASSWORD }}
```

Also update the workflow's header comment to document the new contract:

```yaml
# Release pipeline: push a v* tag -> draft GitHub Release with platform
# bundles (via tauri-action) plus explicitly named assets that install.sh
# downloads. Asset-name contract with install.sh:
#   Ken-<target-triple>.app.tar.gz   (macOS app bundle, tarred)
#   Ken-<target-triple>.AppImage     (Linux)
#   ken-mcp-<target-triple>[.exe]    (all platforms)
# Auto-update contract: tauri-action also uploads signed updater artifacts
# and latest.json (endpoint in tauri.conf.json). Requires repo secrets
# TAURI_SIGNING_PRIVATE_KEY and TAURI_SIGNING_PRIVATE_KEY_PASSWORD
# (generated with: pnpm tauri signer generate).
```

- [ ] **Step 4: Sanity-check the config**

Run: `pnpm tauri build --debug --bundles app 2>&1 | tail -5` — or, cheaper, `npx tauri info >/dev/null && python3 -c "import json; json.load(open('src-tauri/tauri.conf.json'))"`
Expected: JSON parses; build (if run) succeeds and produces `Ken.app.tar.gz` + `.sig` next to the bundle.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/tauri.conf.json .github/workflows/release.yml
git commit -m "feat(update): signed updater artifacts + latest.json in release pipeline"
```

- [ ] **Step 6: Tell Arthur about the secrets (in the final report)**

The final summary MUST include: add repo secrets on GitHub (`smo-key/ken` → Settings → Secrets → Actions):
- `TAURI_SIGNING_PRIVATE_KEY` = contents of `~/.tauri/ken-updater.key`
- `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` = empty string
Or via CLI: `gh secret set TAURI_SIGNING_PRIVATE_KEY < ~/.tauri/ken-updater.key && gh secret set TAURI_SIGNING_PRIVATE_KEY_PASSWORD --body ""`

---

### Task 5: ken-mcp sidecar bundling + startup refresh

**Files:**
- Modify: `src-tauri/tauri.conf.json` (bundle.externalBin)
- Modify: `Makefile` (dev/build pre-step)
- Modify: `.github/workflows/release.yml` (build ken-mcp BEFORE tauri-action)
- Modify: `.gitignore`
- Modify: `src-tauri/src/lib.rs` (setup hook + refresh fn + unit test)

**Interfaces:**
- Consumes: nothing from other tasks.
- Produces: `refresh_ken_mcp_from(bundled: &Path, dest: &Path) -> std::io::Result<bool>` (returns whether a copy happened) — internal to lib.rs, used by the setup hook and its test.

- [ ] **Step 1: Declare the sidecar**

In `src-tauri/tauri.conf.json` `bundle` object add:

```json
    "externalBin": ["binaries/ken-mcp"]
```

- [ ] **Step 2: Provide the binary locally (dev/build) and ignore it in git**

Append to `.gitignore`:

```
src-tauri/binaries/
```

In `Makefile`, add a target and hook it into `dev` and `build`:

```make
mcp-sidecar: ## Build ken-mcp and stage it for Tauri bundling (externalBin)
	cargo build --release -p ken-mcp
	mkdir -p src-tauri/binaries
	cp target/release/ken-mcp "src-tauri/binaries/ken-mcp-$$(rustc -vV | sed -n 's/^host: //p')"

dev: require-pnpm mcp-sidecar ## Run the Tauri dev server (app window + hot reload)
	@lsof -ti :$(VITE_PORT) | xargs kill -9 2>/dev/null || true
	$(PNPM) tauri dev

build: require-pnpm mcp-sidecar ## Produce a release bundle
	$(PNPM) tauri build
```

(Keep existing `.PHONY` list updated: add `mcp-sidecar`.)

- [ ] **Step 3: Stage the sidecar in CI before tauri-action**

In `.github/workflows/release.yml`, MOVE the "Build ken-mcp" step to BEFORE the tauri-action step and extend it to stage the sidecar:

```yaml
      - name: Build ken-mcp
        shell: bash
        run: |
          cargo build --release -p ken-mcp
          mkdir -p src-tauri/binaries
          triple=$(rustc -vV | sed -n 's/^host: //p')
          if [ "$RUNNER_OS" = "Windows" ]; then
            cp target/release/ken-mcp.exe "src-tauri/binaries/ken-mcp-$triple.exe"
          else
            cp target/release/ken-mcp "src-tauri/binaries/ken-mcp-$triple"
          fi
```

Delete the old post-tauri "Build ken-mcp" step (the later upload step keeps using `target/release/ken-mcp`, which still exists).

- [ ] **Step 4: Write the failing Rust test for the refresh logic**

In `src-tauri/src/lib.rs` (append to the existing `#[cfg(test)] mod tests` if one exists, else create one at the bottom):

```rust
#[cfg(test)]
mod ken_mcp_refresh_tests {
    use super::refresh_ken_mcp_from;

    #[test]
    fn copies_when_dest_missing_or_stale_and_skips_when_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let bundled = dir.path().join("bundled-ken-mcp");
        let dest = dir.path().join("bin").join("ken-mcp");
        std::fs::write(&bundled, b"v2 binary").unwrap();

        // Missing dest -> copied (and parent dir created).
        assert!(refresh_ken_mcp_from(&bundled, &dest).unwrap());
        assert_eq!(std::fs::read(&dest).unwrap(), b"v2 binary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dest).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "must be executable");
        }

        // Same bytes -> no copy.
        assert!(!refresh_ken_mcp_from(&bundled, &dest).unwrap());

        // Stale dest -> copied.
        std::fs::write(&dest, b"v1 binary").unwrap();
        assert!(refresh_ken_mcp_from(&bundled, &dest).unwrap());
        assert_eq!(std::fs::read(&dest).unwrap(), b"v2 binary");
    }
}
```

Run: `cargo test -p ken-app ken_mcp_refresh`
Expected: FAIL — `refresh_ken_mcp_from` not found.

- [ ] **Step 5: Implement refresh + setup hook**

In `src-tauri/src/lib.rs` add (near the other free functions):

```rust
/// Copy the bundled ken-mcp sidecar over `dest` when the bytes differ, so
/// `~/.local/bin/ken-mcp` (installed by install.sh) stays in sync across
/// auto-updates. Returns whether a copy happened.
fn refresh_ken_mcp_from(bundled: &std::path::Path, dest: &std::path::Path) -> std::io::Result<bool> {
    let new_bytes = std::fs::read(bundled)?;
    if let Ok(old_bytes) = std::fs::read(dest) {
        if old_bytes == new_bytes {
            return Ok(false);
        }
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Write to a temp name then rename, so a running ken-mcp keeps its
    // old inode and the swap is atomic.
    let tmp = dest.with_extension("tmp");
    std::fs::write(&tmp, &new_bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&tmp, dest)?;
    Ok(true)
}

/// Locate the bundled sidecar (next to the app executable) and refresh the
/// user's copy. Release builds only; failures are logged, never fatal.
fn refresh_ken_mcp() {
    if cfg!(debug_assertions) {
        return;
    }
    let Some(home) = std::env::var_os("HOME") else { return };
    let dest = std::path::PathBuf::from(home).join(".local/bin/ken-mcp");
    let bundled = match std::env::current_exe() {
        Ok(exe) => exe.parent().map(|d| d.join("ken-mcp")),
        Err(_) => None,
    };
    let Some(bundled) = bundled.filter(|p| p.exists()) else { return };
    match refresh_ken_mcp_from(&bundled, &dest) {
        Ok(true) => eprintln!("ken-mcp refreshed at {}", dest.display()),
        Ok(false) => {}
        Err(err) => eprintln!("warning: could not refresh ken-mcp: {err}"),
    }
}
```

And in the builder chain add a setup hook (before `.invoke_handler`):

```rust
        .setup(|_app| {
            std::thread::spawn(refresh_ken_mcp);
            Ok(())
        })
```

(If the builder already has a `.setup`, add the `std::thread::spawn(refresh_ken_mcp);` line inside it instead.)

- [ ] **Step 6: Run the tests**

Run: `cargo test -p ken-app ken_mcp_refresh`
Expected: PASS.
Then: `make mcp-sidecar && cargo check -p ken-app`
Expected: success; `src-tauri/binaries/ken-mcp-<triple>` exists.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/tauri.conf.json src-tauri/src/lib.rs Makefile .gitignore .github/workflows/release.yml
git commit -m "feat(update): bundle ken-mcp sidecar and refresh ~/.local/bin on launch"
```

---

### Task 6: Full verification

**Files:** none new.

- [ ] **Step 1: Full test suites**

Run: `pnpm test && cargo test`
Expected: all pass.

- [ ] **Step 2: Dev app launch**

Per the `verify` skill: `make dev`, confirm the app boots, the title bar renders normally, and no update chip appears (dev builds skip checks).

- [ ] **Step 3: Release build smoke test**

Run: `make build` (needs the sidecar staged — the Makefile does it).
Expected: bundle succeeds; `target/release/bundle/macos/Ken.app/Contents/MacOS/ken-mcp` exists; updater artifacts `Ken.app.tar.gz` + `Ken.app.tar.gz.sig` produced (createUpdaterArtifacts). If signing fails because no key is configured locally, set `TAURI_SIGNING_PRIVATE_KEY=$(cat ~/.tauri/ken-updater.key)` for the build.

- [ ] **Step 4: Commit any fixes and report**

Report must include: the GitHub-secrets setup command (Task 4 Step 6), and that end-to-end update flow (chip → restart) can only be fully exercised once two signed releases exist; a local E2E recipe is to serve a hand-made `latest.json` + the built `.tar.gz`/`.sig` on localhost and temporarily point `plugins.updater.endpoints` at it.

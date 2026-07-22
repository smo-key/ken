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

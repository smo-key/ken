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
    // The first `downloading` snapshot is emitted by the Started event below,
    // so its progress reflects the real state (0 when the size is known, null
    // otherwise) rather than a placeholder.
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

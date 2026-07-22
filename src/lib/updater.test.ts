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

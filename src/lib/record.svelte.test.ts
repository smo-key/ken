import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ModelProgress, ModelStatus, RecordStateEvent, TranscriptProgress } from "./api";

// Captured event listeners registered by the store's `init()`.
const listeners: {
  state?: (ev: RecordStateEvent) => void;
  transcribing?: () => void;
  saved?: (ev: { relPath: string }) => void;
  modelProgress?: (ev: ModelProgress) => void;
  transcriptProgress?: (ev: TranscriptProgress) => void;
} = {};

// A resolved, installed transcription-model status by default; individual tests
// override `installed` to exercise the not-ready gate.
const modelStatus: ModelStatus = {
  id: "whisper-base",
  name: "Whisper Base",
  installed: true,
  sizeBytes: 100,
  expectedBytes: 100,
  recommended: true,
  category: "transcription",
  tier: "recommended",
  blurb: "",
  selected: true,
};

vi.mock("./api", () => ({
  api: {
    recordInputDevices: vi.fn(async () => []),
    recordSupport: vi.fn(async () => ({ mic: true, system: true, reason: null })),
    recordPermissions: vi.fn(async () => ({
      mic: "granted",
      screen: "granted",
      micSettingsUrl: "",
      screenSettingsUrl: "",
    })),
    modelStatus: vi.fn(async () => modelStatus),
    recordStart: vi.fn(async () => {}),
    onModelDownloadProgress: vi.fn(async (cb: (ev: ModelProgress) => void) => {
      listeners.modelProgress = cb;
    }),
    onRecordLevel: vi.fn(async () => {}),
    onRecordState: vi.fn(async (cb: (ev: RecordStateEvent) => void) => {
      listeners.state = cb;
    }),
    onRecordTranscribing: vi.fn(async (cb: () => void) => {
      listeners.transcribing = cb;
    }),
    onTranscriptProgress: vi.fn(async (cb: (ev: TranscriptProgress) => void) => {
      listeners.transcriptProgress = cb;
    }),
    onRecordSaved: vi.fn(async (cb: (ev: { relPath: string }) => void) => {
      listeners.saved = cb;
    }),
    onRecordError: vi.fn(async () => {}),
  },
}));

import { isRecordingPath, record, recordClock } from "./record.svelte";
import { api } from "./api";

// Drive the elapsed clock deterministically: fake setInterval/clearInterval and
// control performance.now() directly so we can prove the interval ticks while
// recording and is stopped once transcription begins.
let now = 0;

function enterRecording(elapsedMs: number) {
  now = 0;
  listeners.state?.({ phase: "recording", elapsedMs, mic: true, system: false });
}

describe("record store — transcribing", () => {
  beforeEach(async () => {
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] });
    now = 0;
    vi.spyOn(performance, "now").mockImplementation(() => now);
    await record.init();
    record.storage = "both";
    record.transcribing = false;
    record.elapsedMs = 0;
  });

  afterEach(() => {
    vi.restoreAllMocks();
    vi.useRealTimers();
  });

  it("ticks the elapsed clock while recording", () => {
    enterRecording(1000);
    now = 5000;
    vi.advanceTimersByTime(200);
    expect(record.elapsedMs).toBe(6000); // baseElapsed 1000 + 5000 elapsed
  });

  it("stops the clock and shows transcribing on stop (transcript/both)", () => {
    enterRecording(1000);
    now = 5000;
    vi.advanceTimersByTime(200);
    expect(record.elapsedMs).toBe(6000);

    listeners.transcribing?.();
    expect(record.transcribing).toBe(true);

    // Clock is frozen: further time advancement does not move elapsedMs.
    now = 30000;
    vi.advanceTimersByTime(5000);
    expect(record.elapsedMs).toBe(6000);
  });

  it("audio-only path stops the clock but does not flash transcribing", () => {
    record.storage = "audio";
    enterRecording(1000);
    now = 5000;
    vi.advanceTimersByTime(200);
    expect(record.elapsedMs).toBe(6000);

    listeners.transcribing?.();
    expect(record.transcribing).toBe(false); // no "Transcribing…" flash for audio

    now = 30000;
    vi.advanceTimersByTime(5000);
    expect(record.elapsedMs).toBe(6000); // clock still frozen at true duration
  });

  it("tracks percent from Recordings/ progress and resets on saved", () => {
    listeners.transcribing?.();
    expect(record.transcribing).toBe(true);
    expect(record.transcribePct).toBeNull(); // null until the first sample

    listeners.transcriptProgress?.({ relPath: "Recordings/take.md", phase: "transcribing", pct: 42 });
    expect(record.transcribePct).toBe(42);

    // A concurrent video transcription must not drive this bar.
    listeners.transcriptProgress?.({ relPath: "Videos/clip.md", phase: "transcribing", pct: 99 });
    expect(record.transcribePct).toBe(42);

    listeners.saved?.({ relPath: "Recordings/take.md" });
    expect(record.transcribing).toBe(false);
    expect(record.transcribePct).toBeNull();
  });
});

describe("record store — transcription-model gate", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    // Leave the shared status installed so other suites start ready.
    modelStatus.installed = true;
  });

  it("reads model status on init and reports readiness", async () => {
    modelStatus.installed = true;
    await record.init();
    expect(record.modelStatus?.installed).toBe(true);
    expect(record.modelReady).toBe(true);
  });

  it("refuses to start and does not call recordStart when the model is missing", async () => {
    modelStatus.installed = false;
    await record.init();
    vi.mocked(api.recordStart).mockClear();

    expect(record.modelReady).toBe(false);
    await record.start();

    expect(api.recordStart).not.toHaveBeenCalled();
    expect(record.error).toBeTruthy();
  });

  it("starts recording once the model is installed", async () => {
    modelStatus.installed = true;
    await record.init();
    vi.mocked(api.recordStart).mockClear();

    await record.start("ATT");

    expect(api.recordStart).toHaveBeenCalledTimes(1);
    // The take is for the team: its transcript goes to that wiki's Raw/.
    expect(vi.mocked(api.recordStart).mock.calls[0][3]).toBe("ATT");
    expect(record.error).toBeNull();
  });

  it("clears the gate when a download completes (progress hits 100%)", async () => {
    modelStatus.installed = false;
    await record.init();
    expect(record.modelReady).toBe(false);

    // The install lands, then the terminal 100% progress sample fires.
    modelStatus.installed = true;
    listeners.modelProgress?.({ id: "whisper-base", downloaded: 100, total: 100 });
    // Let refreshModelStatus() resolve.
    await Promise.resolve();
    await Promise.resolve();

    expect(record.modelReady).toBe(true);
  });
});

describe("record store — what the machine can capture", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    vi.mocked(api.recordSupport).mockResolvedValue({ mic: true, system: true, reason: null });
  });

  it("turns system audio off where it cannot be captured", async () => {
    vi.mocked(api.recordSupport).mockResolvedValue({ mic: true, system: false, reason: "No loopback device" });
    listeners.state?.({ phase: "idle", elapsedMs: 0, mic: false, system: false });
    record.systemOn = true;
    await record.init();
    expect(record.systemSupported).toBe(false);
    expect(record.systemOn).toBe(false);
    expect(record.support?.reason).toBe("No loopback device");
  });

  it("keeps the transcript only by default", async () => {
    vi.resetModules();
    const { record: fresh } = await import("./record.svelte");
    expect(fresh.storage).toBe("transcript");
  });
});

describe("isRecordingPath", () => {
  it("knows a recording from a video", () => {
    expect(isRecordingPath("Recordings/take.md")).toBe(true);
    expect(isRecordingPath("Research/Ingestion/Raw/2026-10-01 14.02 Recording.md")).toBe(true);
    expect(isRecordingPath("Videos/clip.md")).toBe(false);
    expect(isRecordingPath("Research/Ingestion/Raw/2026-10-01 14.02 Note - plan.md")).toBe(false);
  });
});

describe("recordClock", () => {
  it("reads minutes and seconds", () => {
    expect(recordClock(0)).toBe("0:00");
    expect(recordClock(192_000)).toBe("3:12");
    expect(recordClock(3_725_000)).toBe("62:05");
  });
});

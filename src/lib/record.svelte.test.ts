import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { RecordStateEvent } from "./api";

// Captured event listeners registered by the store's `init()`.
const listeners: {
  state?: (ev: RecordStateEvent) => void;
  transcribing?: () => void;
  saved?: (ev: { relPath: string }) => void;
} = {};

vi.mock("./api", () => ({
  api: {
    recordInputDevices: vi.fn(async () => []),
    recordPermissions: vi.fn(async () => ({
      mic: "granted",
      screen: "granted",
      micSettingsUrl: "",
      screenSettingsUrl: "",
    })),
    onRecordLevel: vi.fn(async () => {}),
    onRecordState: vi.fn(async (cb: (ev: RecordStateEvent) => void) => {
      listeners.state = cb;
    }),
    onRecordTranscribing: vi.fn(async (cb: () => void) => {
      listeners.transcribing = cb;
    }),
    onRecordSaved: vi.fn(async (cb: (ev: { relPath: string }) => void) => {
      listeners.saved = cb;
    }),
    onRecordError: vi.fn(async () => {}),
  },
}));

import { record } from "./record.svelte";

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
});

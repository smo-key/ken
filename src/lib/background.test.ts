import { describe, expect, it } from "vitest";
import { buildLine, jobLine, jobPct, kindLabel, paceLine, PACES, waitLine } from "./background";

describe("background status panel", () => {
  it("offers the three paces in order", () => {
    expect(PACES.map((p) => p.value)).toEqual(["gentle", "normal", "fast"]);
  });

  it("shows the pace as data, with the rest in its tooltip", () => {
    const gentle = paceLine({ setting: { pace: "gentle", paused: false }, callsLastHour: 8, callsPerHour: 20 });
    expect(gentle.text).toBe("20/h · 8 used");
    expect(gentle.tip).toContain("The default: a call every 10 seconds");
    expect(gentle.tip).toContain("not counted");
    const paused = paceLine({ setting: { pace: "gentle", paused: true }, callsLastHour: 8, callsPerHour: 20 });
    expect(paused.text).toBe("paused · 8 used");
    expect(paused.tip).toContain("all wait");
  });

  it("reads a job with and without progress", () => {
    expect(jobLine({ kind: "meaning", label: "Game", done: 40, total: 100 })).toBe("Meaning search · Game · 40 of 100");
    expect(jobLine({ kind: "checks", label: "Wiki", done: 0, total: 0 })).toBe("Checks · Wiki");
    expect(jobPct({ kind: "meaning", label: "Game", done: 1, total: 4 })).toBe(25);
    expect(jobPct({ kind: "checks", label: "Wiki", done: 0, total: 0 })).toBeNull();
    expect(kindLabel("unknown")).toBe("unknown");
  });

  it("reads a job that waits for its turn as waiting", () => {
    const job = { kind: "ingest", label: "Wiki: note.md, reading", done: 0, total: 0, waiting: true };
    expect(waitLine(job, true)).toBe("Ingest · Wiki: note.md, reading · paused");
    expect(waitLine(job, false)).toBe("Ingest · Wiki: note.md, reading · waiting for its turn");
  });

  it("sums up the build queue", () => {
    expect(buildLine({ total: 60, done: 12, queued: 45, reading: ["Game: overview"], failed: [["Game: a.md", "timed out"]], atOnce: 1 })).toBe(
      "12 of 60 read · 1 in the inbox · 45 queued · 1 failed",
    );
    expect(buildLine({ total: 3, done: 3, queued: 0, reading: [], failed: [], atOnce: 1 })).toBe("3 of 3 read");
  });
});

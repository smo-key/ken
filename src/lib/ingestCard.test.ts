import { describe, it, expect } from "vitest";
import { canReadNow, ingestBusy, pageName, rawActions, rawStateLabel, stagingLine, writtenLine } from "./ingestCard";
import type { RawSource } from "./api";

describe("writtenLine", () => {
  it("counts pages, ideas and escalations, then what waits", () => {
    expect(writtenLine(["edit", "page", "escalation"], 3)).toBe("2 pages, 1 escalation written · 3 wait");
    expect(writtenLine(["edit", "idea", "idea"], 1)).toBe("1 page, 2 ideas written · 1 waits");
  });
  it("names the items on my day", () => {
    expect(writtenLine(["edit", "task"], 0)).toBe("1 page written, 1 on my day");
    expect(writtenLine(["task"], 2)).toBe("1 on my day · 2 wait");
  });
  it("says when nothing was written", () => {
    expect(writtenLine([], 0)).toBe("nothing written");
    expect(writtenLine([], 4)).toBe("nothing written · 4 wait");
  });
});

describe("pageName", () => {
  it("reads a path as sections", () => {
    expect(pageName("Current/Project.md")).toBe("Current › Project");
    expect(pageName("ideas/I-031.md")).toBe("ideas › I-031");
    expect(pageName("Vocabulary.md")).toBe("Vocabulary");
  });
});

describe("stagingLine", () => {
  it("says none or how many were held", () => {
    expect(stagingLine(0)).toBe("Every page edit went through staging; none was held.");
    expect(stagingLine(2)).toBe("2 held, below.");
  });
});

describe("Raw rows", () => {
  it("say where each source is", () => {
    expect(rawStateLabel("queued")).toBe("in the queue");
    expect(rawStateLabel("transcribing")).toBe("transcribing");
    expect(rawStateLabel("waiting")).toBe("waiting for a transcript");
    expect(rawStateLabel("failed")).toBe("could not read");
  });
  it("offer Try again and Remove only where they make sense", () => {
    expect(rawActions("failed")).toEqual(["retry", "remove"]);
    expect(rawActions("waiting")).toEqual(["retry", "remove"]);
    expect(rawActions("queued")).toEqual(["remove"]);
    expect(rawActions("reading")).toEqual([]);
    expect(rawActions("transcribing")).toEqual([]);
    expect(rawActions("read")).toEqual([]);
  });
});

describe("Read now", () => {
  const raw = (state: RawSource["state"]) => ({ state }) as RawSource;
  it("shows when something is queued and no pass runs", () => {
    expect(canReadNow({ running: false, raw: [raw("queued")] })).toBe(true);
    expect(canReadNow({ running: true, raw: [raw("queued")] })).toBe(false);
    expect(canReadNow({ running: false, raw: [raw("failed")] })).toBe(false);
    expect(canReadNow(null)).toBe(false);
  });
  it("keeps the screen reading while anything moves", () => {
    expect(ingestBusy({ running: false, raw: [raw("transcribing")] })).toBe(true);
    expect(ingestBusy({ running: false, raw: [raw("waiting"), raw("read")] })).toBe(false);
  });
});

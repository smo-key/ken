import { describe, it, expect } from "vitest";
import { canReadNow, heldTitle, ingestBusy, pageName, proposalShort, raidDetail, rawActions, rawStatus } from "./ingestCard";
import type { RawSource } from "./api";

describe("pageName", () => {
  it("reads a path as sections", () => {
    expect(pageName("Current/Project.md")).toBe("Current › Project");
    expect(pageName("tickets/SR-031.md")).toBe("tickets › SR-031");
    expect(pageName("Vocabulary.md")).toBe("Vocabulary");
  });
});

describe("an Inbox card", () => {
  it("names a held edit by its page, never its path", () => {
    expect(heldTitle({ title: "Current › Project", page: "Current/Project.md" })).toBe("Current › Project");
    expect(heldTitle({ title: "Held: Current/Project.md from 2026-10-15-meeting", page: "Current/Project.md" })).toBe("Current › Project");
    expect(heldTitle({ title: "Held: retire Design/Old.md from 2026-10-15-meeting", page: "Design/Old.md" })).toBe("Retire Design › Old");
  });

  it("shows a line that is not the title: who, or what ingest wrote", () => {
    const raid = JSON.stringify({ raid: { text: "A crash mid-write loses `saves/world.json`." } });
    expect(proposalShort({ kind: "risk", payload: raid, who: "Ana" })).toBe("A crash mid-write loses `saves/world.json`.");
    expect(proposalShort({ kind: "ruling", payload: null, who: "Ana and Ben" })).toBe("Ana and Ben");
    expect(proposalShort({ kind: "page", payload: null, who: null })).toBe("");
  });
});

describe("Raw rows", () => {
  it("say where each source is", () => {
    expect(rawStatus({ state: "queued", detail: null })).toBe("Queued");
    expect(rawStatus({ state: "transcribing", detail: "transcribing 40%" })).toBe("Transcribing 40%");
    expect(rawStatus({ state: "transcribing", detail: null })).toBe("Transcribing");
    expect(rawStatus({ state: "waiting", detail: "no whisper model" })).toBe("Waits for a transcript");
    expect(rawStatus({ state: "failed", detail: "bad file" })).toBe("Couldn't read");
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

describe("raidDetail", () => {
  it("reads the text ingest wrote for a risk or issue", () => {
    expect(raidDetail(JSON.stringify({ raid: { kind: "risk", title: "Friday may slip", text: " The build waits on QA. " } }))).toBe(
      "The build waits on QA.",
    );
  });

  it("is empty for anything else", () => {
    expect(raidDetail(JSON.stringify({ append: { ruling: "x" } }))).toBe("");
    expect(raidDetail(null)).toBe("");
    expect(raidDetail("not json")).toBe("");
  });
});

import { describe, expect, it } from "vitest";
import {
  bannerCounts,
  bannerLine,
  buildDiffRows,
  conflictCopyPayload,
  conflictPayload,
  numericId,
  proposalPayload,
} from "./conflicts";
import type { ConflictItem } from "./api";

function item(partial: Partial<ConflictItem>): ConflictItem {
  return {
    id: "item-1",
    kind: "conflict",
    title: "t",
    body: "b",
    when: 0,
    sourceRef: "Decisions.md",
    payload: null,
    ...partial,
  };
}

describe("conflict payload parsing", () => {
  it("parses a conflict payload and rejects the other kind", () => {
    const payload = JSON.stringify({
      path: "Decisions.md",
      ours: "mine",
      theirs: "theirs",
      draft: null,
      draftStatus: "pending",
    });
    const conflict = item({ id: "item-4", kind: "conflict", payload });
    expect(conflictPayload(conflict)?.path).toBe("Decisions.md");
    expect(conflictPayload(conflict)?.draftStatus).toBe("pending");
    expect(conflictPayload(item({ kind: "conflict-copy", payload }))).toBeNull();
    expect(conflictPayload(item({ kind: "conflict", payload: null }))).toBeNull();
    expect(conflictPayload(item({ kind: "conflict", payload: "{oops" }))).toBeNull();
  });

  it("parses a conflicted-copy payload", () => {
    const payload = JSON.stringify({ copyPath: "notes (conflicted copy).md", originalPath: "notes.md" });
    const copy = item({ id: "item-5", kind: "conflict-copy", payload });
    expect(conflictCopyPayload(copy)?.originalPath).toBe("notes.md");
    expect(conflictCopyPayload(item({ kind: "conflict", payload }))).toBeNull();
  });

  it("parses a page proposal and rejects a malformed one", () => {
    expect(proposalPayload(JSON.stringify({ page: "a.md", base: "x", proposed: "y" }))?.page).toBe("a.md");
    expect(proposalPayload(JSON.stringify({ page: "a.md" }))).toBeNull();
    expect(proposalPayload("not json")).toBeNull();
    expect(proposalPayload(null)).toBeNull();
  });
});

describe("numericId", () => {
  it("parses the row id out of kind-prefixed ids", () => {
    expect(numericId("item-12")).toBe(12);
    expect(numericId("stored-3")).toBe(3);
    expect(numericId("item-12@6f1c2a9e-0000-4000-8000-000000000000")).toBe(12);
  });
});

describe("buildDiffRows", () => {
  it("marks added and removed lines and keeps changed context", () => {
    const rows = buildDiffRows("one\ntwo\nthree\n", "one\nTWO\nthree\n");
    expect(rows).toContainEqual({ type: "del", text: "two" });
    expect(rows).toContainEqual({ type: "add", text: "TWO" });
    expect(rows).toContainEqual({ type: "ctx", text: "one" });
    expect(rows).toContainEqual({ type: "ctx", text: "three" });
    expect(rows.some((r) => r.type === "gap")).toBe(false);
  });

  it("collapses long unchanged runs into a gap marker", () => {
    const lines = Array.from({ length: 30 }, (_, i) => `line ${i}`);
    const changed = [...lines];
    changed[15] = "line 15 EDITED";
    const rows = buildDiffRows(lines.join("\n") + "\n", changed.join("\n") + "\n", 3);
    expect(rows.filter((r) => r.type === "gap").length).toBe(2);
    expect(rows).toContainEqual({ type: "del", text: "line 15" });
    expect(rows).toContainEqual({ type: "add", text: "line 15 EDITED" });
    expect(rows).toContainEqual({ type: "ctx", text: "line 14" });
    expect(rows).toContainEqual({ type: "ctx", text: "line 16" });
  });

  it("returns no add/del rows for identical text", () => {
    const rows = buildDiffRows("same\ntext\n", "same\ntext\n");
    expect(rows.some((r) => r.type === "add" || r.type === "del")).toBe(false);
  });
});

describe("the Files banner", () => {
  it("says nothing with nothing to say", () => {
    expect(bannerLine(null)).toBe("");
    expect(bannerLine({ conflicts: [], conflictedCopies: 0 })).toBe("");
  });

  it("counts both kinds", () => {
    const banner = {
      conflicts: [item({ id: "item-1" }), item({ id: "item-2", kind: "conflict-copy" as const })],
      conflictedCopies: 1,
    };
    expect(bannerCounts(banner)).toEqual({ conflicts: 1, copies: 1 });
    expect(bannerLine(banner)).toBe("1 file changed in two places · 1 conflicted copy");
  });

  it("trusts the copy count when the copies are not listed", () => {
    const banner = { conflicts: [item({}), item({ id: "item-2" })], conflictedCopies: 3 };
    expect(bannerLine(banner)).toBe("2 files changed in two places · 3 conflicted copies");
  });
});

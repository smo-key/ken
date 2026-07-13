import { describe, expect, it } from "vitest";
import {
  actionsFor,
  buildDiffRows,
  conflictCopyPayload,
  conflictPayload,
  dotFor,
  numericId,
  sourceLabel,
} from "./review.svelte";
import type { InboxItem } from "./api";

function item(partial: Partial<InboxItem>): InboxItem {
  return {
    id: "run-1",
    kind: "approval",
    title: "t",
    body: "b",
    when: 0,
    sourceRef: "people",
    payload: null,
    ...partial,
  };
}

describe("actionsFor", () => {
  it("gives every kind at least one action, primary first", () => {
    expect(actionsFor("approval")).toEqual(["approve", "discard"]);
    expect(actionsFor("stale")).toEqual(["run-now"]);
    expect(actionsFor("failed-file")).toEqual(["open-files"]);
    expect(actionsFor("broken-recipe")).toEqual(["open-ingests"]);
    expect(actionsFor("stored")).toEqual(["mark-done"]);
    expect(actionsFor("conflict")).toEqual([
      "accept-draft",
      "keep-mine",
      "take-theirs",
      "edit-manually",
    ]);
    expect(actionsFor("conflict-copy")).toEqual([
      "keep-copy",
      "keep-original",
      "open-both",
    ]);
  });
});

describe("conflict payload parsing", () => {
  it("parses a conflict payload and rejects mismatched kinds", () => {
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
    // Wrong kind or missing/broken payload → null, never a throw.
    expect(conflictPayload(item({ kind: "stored", payload }))).toBeNull();
    expect(conflictPayload(item({ kind: "conflict", payload: null }))).toBeNull();
    expect(conflictPayload(item({ kind: "conflict", payload: "{oops" }))).toBeNull();
  });

  it("parses a conflicted-copy payload", () => {
    const payload = JSON.stringify({
      copyPath: "notes (conflicted copy).md",
      originalPath: "notes.md",
    });
    const copy = item({ id: "item-5", kind: "conflict-copy", payload });
    expect(conflictCopyPayload(copy)?.originalPath).toBe("notes.md");
    expect(conflictCopyPayload(item({ kind: "conflict", payload }))).toBeNull();
  });
});

describe("numericId", () => {
  it("parses the row id out of kind-prefixed ids", () => {
    expect(numericId(item({ id: "run-12" }))).toBe(12);
    expect(numericId(item({ id: "item-3", kind: "stored" }))).toBe(3);
  });
});

describe("sourceLabel", () => {
  it("shortens paths to the file name and keeps slugs", () => {
    expect(sourceLabel(item({ sourceRef: "notes/x.pdf" }))).toBe("x.pdf");
    expect(sourceLabel(item({ sourceRef: "people" }))).toBe("people");
  });
});

describe("buildDiffRows", () => {
  it("marks added and removed lines and keeps changed context", () => {
    const a = "one\ntwo\nthree\n";
    const b = "one\nTWO\nthree\n";
    const rows = buildDiffRows(a, b);
    expect(rows).toContainEqual({ type: "del", text: "two" });
    expect(rows).toContainEqual({ type: "add", text: "TWO" });
    expect(rows).toContainEqual({ type: "ctx", text: "one" });
    expect(rows).toContainEqual({ type: "ctx", text: "three" });
    // No gap: the unchanged runs are shorter than the context window.
    expect(rows.some((r) => r.type === "gap")).toBe(false);
  });

  it("collapses long unchanged runs into a gap marker", () => {
    const lines = Array.from({ length: 30 }, (_, i) => `line ${i}`);
    const a = lines.join("\n") + "\n";
    const changed = [...lines];
    changed[15] = "line 15 EDITED";
    const b = changed.join("\n") + "\n";
    const rows = buildDiffRows(a, b, 3);
    const gaps = rows.filter((r) => r.type === "gap");
    // A gap before and after the single change.
    expect(gaps.length).toBe(2);
    expect(rows).toContainEqual({ type: "del", text: "line 15" });
    expect(rows).toContainEqual({ type: "add", text: "line 15 EDITED" });
    // Context lines immediately around the change survive.
    expect(rows).toContainEqual({ type: "ctx", text: "line 14" });
    expect(rows).toContainEqual({ type: "ctx", text: "line 16" });
  });

  it("returns no add/del rows for identical text", () => {
    const rows = buildDiffRows("same\ntext\n", "same\ntext\n");
    expect(rows.some((r) => r.type === "add" || r.type === "del")).toBe(false);
  });
});

describe("dotFor", () => {
  it("maps kinds to Paper & Ink vars", () => {
    expect(dotFor("approval")).toBe("var(--accent)");
    expect(dotFor("stored")).toBe("var(--needs-input)");
    expect(dotFor("failed-file")).toBe("var(--danger)");
    expect(dotFor("broken-recipe")).toBe("var(--danger)");
    expect(dotFor("conflict")).toBe("var(--danger)");
    expect(dotFor("conflict-copy")).toBe("var(--danger)");
    expect(dotFor("stale")).toBe("var(--ink-tertiary)");
  });
});

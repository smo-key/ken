import { describe, expect, it } from "vitest";
import { actionsFor, dotFor, numericId, sourceLabel } from "./review.svelte";
import type { InboxItem } from "./api";

function item(partial: Partial<InboxItem>): InboxItem {
  return {
    id: "run-1",
    kind: "approval",
    title: "t",
    body: "b",
    when: 0,
    sourceRef: "people",
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

describe("dotFor", () => {
  it("maps kinds to Paper & Ink vars", () => {
    expect(dotFor("approval")).toBe("var(--accent)");
    expect(dotFor("stored")).toBe("var(--needs-input)");
    expect(dotFor("failed-file")).toBe("var(--danger)");
    expect(dotFor("broken-recipe")).toBe("var(--danger)");
    expect(dotFor("stale")).toBe("var(--ink-tertiary)");
  });
});

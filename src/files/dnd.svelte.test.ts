import { afterEach, describe, expect, it } from "vitest";
import { canDrop, drag, parentOf } from "./dnd.svelte";

afterEach(() => {
  drag.reset();
  drag.fromKind = "file";
});

describe("drag-and-drop guards", () => {
  it("parentOf handles nested and top-level paths", () => {
    expect(parentOf("a/b/c.md")).toBe("a/b");
    expect(parentOf("c.md")).toBe("");
  });

  it("refuses drops when nothing is dragged", () => {
    expect(canDrop("Meetings")).toBe(false);
  });

  it("refuses a no-op drop into the current parent", () => {
    drag.from = "Meetings/notes.md";
    expect(canDrop("Meetings")).toBe(false);
    expect(canDrop("")).toBe(true);
  });

  it("lets a folder move to a different parent", () => {
    drag.from = "Meetings";
    drag.fromKind = "folder";
    expect(canDrop("Research")).toBe(true);
  });

  it("refuses dropping a folder into itself or its own subtree", () => {
    drag.from = "Meetings";
    drag.fromKind = "folder";
    expect(canDrop("Meetings")).toBe(false);
    expect(canDrop("Meetings/2026")).toBe(false);
    // A sibling merely sharing the name prefix is fine.
    expect(canDrop("Meetings Archive")).toBe(true);
  });
});

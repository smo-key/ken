import { describe, expect, it } from "vitest";
import {
  buildFolderTree,
  folderTriState,
  isExcluded,
  toggleFolder,
} from "./folderTree";

describe("watched-folders tree", () => {
  const folders = [
    { relPath: "Meetings" },
    { relPath: "Meetings/2026" },
    { relPath: "Meetings/2026/Q1" },
    { relPath: "Research" },
    { relPath: "Research/Data" },
  ];

  it("builds a roots-first nested tree", () => {
    const tree = buildFolderTree(folders);
    expect(tree.map((n) => n.relPath)).toEqual(["Meetings", "Research"]);
    const meetings = tree[0];
    expect(meetings.name).toBe("Meetings");
    expect(meetings.children.map((n) => n.relPath)).toEqual(["Meetings/2026"]);
    expect(meetings.children[0].children.map((n) => n.relPath)).toEqual(["Meetings/2026/Q1"]);
  });

  it("treats exclusion as prefix-based (self or ancestor)", () => {
    const ex = new Set(["Meetings/2026"]);
    expect(isExcluded("Meetings/2026", ex)).toBe(true);
    expect(isExcluded("Meetings/2026/Q1", ex)).toBe(true); // descendant of an excluded folder
    expect(isExcluded("Meetings", ex)).toBe(false);
    expect(isExcluded("Research", ex)).toBe(false);
  });

  it("computes tri-state: checked / unchecked / indeterminate", () => {
    const ex = new Set(["Meetings/2026"]);
    // Meetings itself isn't excluded, but a descendant is → indeterminate.
    expect(folderTriState("Meetings", ex)).toBe("indeterminate");
    // The excluded folder and everything under it → unchecked.
    expect(folderTriState("Meetings/2026", ex)).toBe("unchecked");
    expect(folderTriState("Meetings/2026/Q1", ex)).toBe("unchecked");
    // A clean subtree → checked.
    expect(folderTriState("Research", ex)).toBe("checked");
  });

  it("excludes a whole subtree by adding the folder (prefix model)", () => {
    const next = toggleFolder("Meetings", false, []);
    expect(next).toEqual(["Meetings"]);
  });

  it("re-includes a folder by dropping it and any excluded descendants", () => {
    const next = toggleFolder("Meetings", true, ["Meetings", "Meetings/2026", "Research/Data"]);
    expect(next.sort()).toEqual(["Research/Data"]);
  });
});

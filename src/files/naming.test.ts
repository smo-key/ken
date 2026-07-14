import { describe, expect, it } from "vitest";
import { dedupedDocName, siblingNames, validateName } from "./naming";

describe("sibling listing", () => {
  const paths = ["a.md", "Meetings/notes.md", "Meetings/2026", "Meetings/2026/deep.md", "Research"];
  it("lists direct children of the root", () => {
    expect(siblingNames(paths, "").sort()).toEqual(["Meetings", "Research", "a.md"]);
  });
  it("lists direct children of a folder, not grandchildren", () => {
    expect(siblingNames(paths, "Meetings").sort()).toEqual(["2026", "notes.md"]);
  });
});

describe("name validation", () => {
  it("rejects empty and whitespace-only names", () => {
    expect(validateName("", [])).toBe("Give it a name.");
    expect(validateName("   ", [])).toBe("Give it a name.");
  });
  it("rejects slashes", () => {
    expect(validateName("a/b.md", [])).toBe("Names can't contain “/”.");
  });
  it("rejects duplicate sibling names, case-insensitively", () => {
    expect(validateName("Notes.md", ["notes.md"])).toBe("“Notes.md” already exists here.");
  });
  it("accepts a fresh name", () => {
    expect(validateName("Plan.md", ["notes.md"])).toBeNull();
  });
});

describe("default document name", () => {
  it("starts at Untitled.md", () => {
    expect(dedupedDocName([])).toBe("Untitled.md");
  });
  it("counts up past collisions (space + counter, per spec)", () => {
    expect(dedupedDocName(["Untitled.md"])).toBe("Untitled 2.md");
    expect(dedupedDocName(["Untitled.md", "Untitled 2.md"])).toBe("Untitled 3.md");
  });
});

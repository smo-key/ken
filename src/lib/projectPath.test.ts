import { describe, expect, it } from "vitest";
import { composeSingleOutput, toProjectRelative } from "./projectPath";

describe("toProjectRelative", () => {
  const root = "/Users/me/project";

  it("converts an inside-project folder to a relative path", () => {
    const r = toProjectRelative(root, "/Users/me/project/knowledge/people");
    expect(r).toEqual({ ok: true, rel: "knowledge/people" });
  });

  it("returns empty relative for the project root itself", () => {
    expect(toProjectRelative(root, "/Users/me/project")).toEqual({
      ok: true,
      rel: "",
    });
  });

  it("ignores trailing slashes on both root and chosen path", () => {
    expect(toProjectRelative(root + "/", "/Users/me/project/notes/")).toEqual({
      ok: true,
      rel: "notes",
    });
  });

  it("rejects a folder outside the project", () => {
    const r = toProjectRelative(root, "/Users/me/other/notes");
    expect(r.ok).toBe(false);
  });

  it("rejects a sibling whose path shares the root as a string prefix", () => {
    // "/Users/me/project-2" starts with the root string but is NOT inside it.
    const r = toProjectRelative(root, "/Users/me/project-2/notes");
    expect(r.ok).toBe(false);
  });

  it("rejects an escaping parent path", () => {
    expect(toProjectRelative(root, "/Users/me").ok).toBe(false);
  });

  it("treats backslash separators defensively (Windows)", () => {
    const win = "C:\\Users\\me\\project";
    const r = toProjectRelative(win, "C:\\Users\\me\\project\\knowledge\\people");
    expect(r).toEqual({ ok: true, rel: "knowledge/people" });
  });
});

describe("composeSingleOutput", () => {
  it("joins a relative folder and a bare filename", () => {
    expect(composeSingleOutput("knowledge", "People.md")).toEqual({
      ok: true,
      rel: "knowledge/People.md",
    });
  });

  it("defaults a missing extension to .md", () => {
    expect(composeSingleOutput("knowledge", "People")).toEqual({
      ok: true,
      rel: "knowledge/People.md",
    });
  });

  it("places the file at the root when the folder is empty", () => {
    expect(composeSingleOutput("", "People.md")).toEqual({
      ok: true,
      rel: "People.md",
    });
  });

  it("ignores a trailing slash on the folder", () => {
    expect(composeSingleOutput("knowledge/", "People.md")).toEqual({
      ok: true,
      rel: "knowledge/People.md",
    });
  });

  it("rejects a name that contains a path separator", () => {
    expect(composeSingleOutput("knowledge", "sub/People.md").ok).toBe(false);
    expect(composeSingleOutput("knowledge", "sub\\People.md").ok).toBe(false);
  });

  it("rejects an empty name", () => {
    expect(composeSingleOutput("knowledge", "   ").ok).toBe(false);
  });
});

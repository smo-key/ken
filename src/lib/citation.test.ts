import { describe, expect, it } from "vitest";
import { inMember, lineTarget, parseCitation } from "./citation";

describe("citations", () => {
  it("reads a path with a line, a range, a heading, or a colon line", () => {
    expect(parseCitation("Platform/Save.md#L12")).toEqual({ projectId: null, path: "Platform/Save.md", line: 12, anchor: undefined });
    expect(parseCitation("src/save.rs#L10-L20")?.line).toBe(10);
    expect(parseCitation("Platform/Save.md#save-format")).toMatchObject({ path: "Platform/Save.md", anchor: "save-format" });
    expect(parseCitation("src/save.rs:42")).toMatchObject({ path: "src/save.rs", line: 42 });
    expect(parseCitation("./notes/a%20b.md")).toMatchObject({ path: "notes/a b.md" });
  });

  it("reads a ken:// address with its member and line", () => {
    expect(parseCitation("ken://1234-abcd/src/save.rs#L7")).toEqual({
      projectId: "1234-abcd",
      path: "src/save.rs",
      line: 7,
      anchor: undefined,
    });
    expect(parseCitation("ken://1234/")).toBeNull();
  });

  it("sends a link that starts with another repo's name to that repo", () => {
    const members = [
      { id: "wiki", name: "ATT-Wiki" },
      { id: "docs", name: "Project Documents" },
      { id: "code", name: "att-opmodel" },
    ];
    const c = inMember(parseCitation("Project%20Documents/07%20Concepts/Market%20Cluster.md#L18")!, members, "wiki");
    expect(c).toEqual({ projectId: "docs", path: "07 Concepts/Market Cluster.md", line: 18, anchor: undefined });
    const own = parseCitation("Current/Project.md#L37")!;
    expect(inMember(own, members, "wiki")).toEqual(own);
    const addressed = parseCitation("ken://code/BE_PACK/app/auth.py#L3")!;
    expect(inMember(addressed, members, "wiki")).toEqual(addressed);
  });

  it("leaves web links and in-page anchors alone", () => {
    for (const href of ["https://example.com/a.md", "mailto:a@b.c", "#top", "//cdn/x.js", ""]) {
      expect(parseCitation(href)).toBeNull();
    }
  });

  it("finds what to scroll to for a source line", () => {
    const md = "# Save\n\nIntro.\n\n## Format\n\n- Saves are **region** files, one per region.\n";
    expect(lineTarget(md, 5)).toEqual({ heading: "Format" });
    expect(lineTarget(md, 7)).toEqual({ heading: "Format", phrase: "Saves are region files, one per region." });
    expect(lineTarget(md, 3)).toEqual({ heading: "Save", phrase: "Intro." });
  });
});

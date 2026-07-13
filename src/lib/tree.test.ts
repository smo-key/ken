import { describe, expect, it } from "vitest";
import { buildTree } from "./tree";
import type { FileRow, FolderInfo } from "./api";

const file = (relPath: string): FileRow => ({
  relPath,
  kind: "md",
  size: 10,
  mtime: 0,
  status: "indexed",
  error: null,
});

describe("buildTree", () => {
  it("nests files under folders and sorts folders first", () => {
    const files = [file("zeta.md"), file("notes/b.md"), file("notes/a.md")];
    const folders: FolderInfo[] = [{ relPath: "notes", excluded: false }];
    const tree = buildTree(files, folders);

    expect(tree.map((n) => n.name)).toEqual(["notes", "zeta.md"]);
    expect(tree[0].children.map((n) => n.name)).toEqual(["a.md", "b.md"]);
  });

  it("creates missing intermediate folders from file paths", () => {
    const tree = buildTree([file("a/b/c.md")], []);
    expect(tree[0].name).toBe("a");
    expect(tree[0].children[0].name).toBe("b");
    expect(tree[0].children[0].children[0].file?.relPath).toBe("a/b/c.md");
  });

  it("keeps excluded folders visible with their flag", () => {
    const tree = buildTree([], [{ relPath: "archive", excluded: true }]);
    expect(tree[0].excluded).toBe(true);
    expect(tree[0].file).toBeUndefined();
  });

  it("handles empty input", () => {
    expect(buildTree([], [])).toEqual([]);
  });
});

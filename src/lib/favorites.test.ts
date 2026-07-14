import { describe, expect, it } from "vitest";
import {
  addFavorite,
  pruneFavorites,
  removeFavorite,
  renameFavorite,
  renameFavoritesForMove,
  type Favorite,
} from "./favorites";

const file = (path: string): Favorite => ({ path, kind: "file" });

describe("favorites helpers", () => {
  it("adds uniquely by path", () => {
    let list = addFavorite([], file("a.md"));
    list = addFavorite(list, file("a.md"));
    expect(list).toHaveLength(1);
    list = addFavorite(list, { path: "notes", kind: "folder" });
    expect(list.map((f) => f.path)).toEqual(["a.md", "notes"]);
  });

  it("removes by path", () => {
    const list = [file("a.md"), file("b.md")];
    expect(removeFavorite(list, "a.md").map((f) => f.path)).toEqual(["b.md"]);
    expect(removeFavorite(list, "missing.md")).toHaveLength(2);
  });

  it("prunes favorites whose path disappeared", () => {
    const list = [file("a.md"), file("gone.md"), { path: "notes", kind: "folder" as const }];
    const kept = pruneFavorites(list, new Set(["a.md", "notes"]));
    expect(kept.map((f) => f.path)).toEqual(["a.md", "notes"]);
  });

  it("rewrites a moved favorite's path", () => {
    const list = [file("a.md"), file("b.md")];
    const moved = renameFavorite(list, "a.md", "sub/a.md");
    expect(moved.map((f) => f.path)).toEqual(["sub/a.md", "b.md"]);
  });
});

describe("renameFavoritesForMove", () => {
  it("renames exact and prefixed favorite paths on a folder move", () => {
    const list = [
      { path: "Meetings", kind: "folder" as const },
      { path: "Meetings/notes.md", kind: "file" as const },
      { path: "Research", kind: "folder" as const },
    ];
    const out = renameFavoritesForMove(list, "Meetings", "Archive/Meetings");
    expect(out.map((f) => f.path)).toEqual([
      "Archive/Meetings",
      "Archive/Meetings/notes.md",
      "Research",
    ]);
  });
});

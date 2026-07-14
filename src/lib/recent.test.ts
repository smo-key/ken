import { beforeEach, describe, expect, it } from "vitest";
import {
  RECENT_LIMIT,
  fallbackRecents,
  homeRecents,
  loadRecents,
  recentlyOpened,
  recordRecent,
  saveRecents,
  type IndexedFile,
} from "./recent";

const file = (relPath: string, mtime: number): IndexedFile => ({
  relPath,
  kind: "md",
  mtime,
});

const tree: IndexedFile[] = [
  file("notes/a.md", 100),
  file("notes/b.md", 300),
  file("c.md", 200),
];

describe("recordRecent", () => {
  it("puts the newest open at the front", () => {
    const list = recordRecent(recordRecent([], "a.md", 10), "b.md", 20);
    expect(list.map((e) => e.path)).toEqual(["b.md", "a.md"]);
  });

  it("dedupes by path, keeping the latest timestamp", () => {
    let list = recordRecent([], "a.md", 10);
    list = recordRecent(list, "b.md", 20);
    list = recordRecent(list, "a.md", 30);
    expect(list.map((e) => e.path)).toEqual(["a.md", "b.md"]);
    expect(list[0].at).toBe(30);
  });

  it("caps the stored history", () => {
    let list: ReturnType<typeof recordRecent> = [];
    for (let i = 0; i < 40; i++) list = recordRecent(list, `f${i}.md`, i);
    expect(list.length).toBeLessThanOrEqual(20);
    expect(list[0].path).toBe("f39.md");
  });
});

describe("recentlyOpened", () => {
  const history = [
    { path: "notes/b.md", at: 500 },
    { path: "gone.md", at: 400 },
    { path: "notes/a.md", at: 300 },
  ];

  it("keeps history order and drops files no longer in the index", () => {
    const rows = recentlyOpened(history, tree);
    expect(rows.map((r) => r.relPath)).toEqual(["notes/b.md", "notes/a.md"]);
  });

  it("timestamps rows with when they were opened, not the file's mtime", () => {
    expect(recentlyOpened(history, tree)[0].at).toBe(500);
  });

  it("caps the list", () => {
    const many = Array.from({ length: 10 }, (_, i) => ({
      path: `f${i}.md`,
      at: 100 - i,
    }));
    const files = many.map((e) => file(e.path, 1));
    expect(recentlyOpened(many, files).length).toBe(RECENT_LIMIT);
    expect(recentlyOpened(many, files, 2).length).toBe(2);
  });
});

describe("fallbackRecents", () => {
  it("orders by mtime, newest first, and timestamps rows with mtime", () => {
    const rows = fallbackRecents(tree);
    expect(rows.map((r) => r.relPath)).toEqual(["notes/b.md", "c.md", "notes/a.md"]);
    expect(rows[0].at).toBe(300);
  });

  it("caps the list", () => {
    const files = Array.from({ length: 12 }, (_, i) => file(`f${i}.md`, i));
    expect(fallbackRecents(files).length).toBe(RECENT_LIMIT);
  });
});

describe("homeRecents", () => {
  it("prefers the user's own open history", () => {
    const rows = homeRecents([{ path: "notes/a.md", at: 900 }], tree);
    expect(rows.map((r) => r.relPath)).toEqual(["notes/a.md"]);
  });

  it("falls back to most-recently-modified files on a fresh project", () => {
    expect(homeRecents([], tree)[0].relPath).toBe("notes/b.md");
  });

  it("falls back when every remembered file has left the index", () => {
    expect(homeRecents([{ path: "gone.md", at: 900 }], tree)[0].relPath).toBe(
      "notes/b.md",
    );
  });

  it("has nothing to show for an empty index", () => {
    expect(homeRecents([{ path: "gone.md", at: 900 }], [])).toEqual([]);
  });
});

describe("recents persistence", () => {
  beforeEach(() => localStorage.clear());

  it("defaults to empty when nothing is stored", () => {
    expect(loadRecents("p1")).toEqual([]);
  });

  it("round-trips per project", () => {
    saveRecents("p1", [{ path: "a.md", at: 10 }]);
    expect(loadRecents("p1")).toEqual([{ path: "a.md", at: 10 }]);
    expect(loadRecents("p2")).toEqual([]);
  });

  it("ignores corrupt or malformed stored state", () => {
    localStorage.setItem("ken.files.recent.p1", "{not json");
    expect(loadRecents("p1")).toEqual([]);
    localStorage.setItem("ken.files.recent.p1", '[{"path":1},{"path":"a.md","at":3}]');
    expect(loadRecents("p1")).toEqual([{ path: "a.md", at: 3 }]);
  });
});

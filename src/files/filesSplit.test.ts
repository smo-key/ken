import { describe, expect, it } from "vitest";
import { repoFor, sideOf, splitRepos } from "./filesSplit";

const repos = [{ id: "kb" }, { id: "game" }, { id: "tools" }];

describe("Files: Wiki and Sources", () => {
  it("puts the knowledge base under Wiki and every other repo under Sources", () => {
    const split = splitRepos(repos, "kb");
    expect(split.wiki).toEqual({ id: "kb" });
    expect(split.sources.map((r) => r.id)).toEqual(["game", "tools"]);
    expect(splitRepos(repos, null)).toEqual({ wiki: null, sources: repos });
  });

  it("knows the side of the repo on show and which repo a side opens", () => {
    expect(sideOf("kb", "kb")).toBe("wiki");
    expect(sideOf("game", "kb")).toBe("sources");
    expect(sideOf(null, "kb")).toBe("sources");
    const split = splitRepos(repos, "kb");
    expect(repoFor("wiki", split, null)).toBe("kb");
    expect(repoFor("sources", split, "tools")).toBe("tools");
    expect(repoFor("sources", split, "gone")).toBe("game");
    expect(repoFor("sources", splitRepos([{ id: "kb" }], "kb"), null)).toBeNull();
  });
});

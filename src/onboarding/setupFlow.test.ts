import { describe, expect, it } from "vitest";
import type { SetupRepoRow } from "../lib/api";
import {
  commonParent,
  coveredRepos,
  defaultTeamName,
  defaultWikiChoice,
  indexSummary,
  mergeRows,
  newWikiPath,
  onOneTeam,
  renamedWikiChoice,
  suggestedTeams,
  teamWikis,
  toggleKind,
  wikiChoicesReady,
} from "./setupFlow";

const row = (member: string, over: Partial<SetupRepoRow> = {}): SetupRepoRow => ({
  member,
  include: true,
  kind: [],
  team: "Realms",
  index: "entities",
  evidence: [],
  remote: null,
  existing: false,
  hasGit: true,
  path: `C:/Code/${member}`,
  description: "",
  ...over,
});

describe("setup flow", () => {
  it("keeps a person's edits when the picked set is proposed again", () => {
    const old = [row("docs", { description: "Our wiki", team: "Mine", include: false })];
    const fresh = [row("docs", { description: "From the README" }), row("game")];
    const merged = mergeRows(old, fresh);
    expect(merged.map((r) => [r.member, r.description, r.team, r.include])).toEqual([
      ["docs", "Our wiki", "Mine", false],
      ["game", "", "Realms", true],
    ]);
    expect(suggestedTeams(merged)).toEqual(["Realms"]);
  });

  it("toggles a kind and keeps the fixed order", () => {
    expect(toggleKind(["code"], "wiki")).toEqual(["wiki", "code"]);
    expect(toggleKind(["wiki", "code"], "wiki")).toEqual(["code"]);
  });

  it("puts every included repo on the one team", () => {
    const rows = [row("a", { team: null }), row("b", { team: "Old" }), row("c", { team: null, include: false })];
    expect(onOneTeam(rows, " AUR ").map((r) => r.team)).toEqual(["AUR", "AUR", null]);
    expect(onOneTeam(rows, "  ").map((r) => r.team)).toEqual([null, null, null]);
  });

  it("names the team from a team or wiki repo, else the folder the repos share", () => {
    const aur = [
      row("agentic-ui-reviewer", { path: "C:\\ken-eval\\AUR\\agentic-ui-reviewer" }),
      row("AUR-Team", { path: "C:\\ken-eval\\AUR\\AUR-Team" }),
    ];
    expect(defaultTeamName(aur)).toBe("AUR");
    expect(commonParent(aur)).toBe("C:\\ken-eval\\AUR");
    const code = [row("game", { path: "/home/me/realms/game" }), row("docs", { path: "/home/me/realms/docs" })];
    expect(defaultTeamName(code)).toBe("realms");
    expect(defaultTeamName([row("steam", { path: "C:\\steam" })])).toBe("");
    expect(commonParent([row("a", { path: "C:\\x\\a" }), row("b", { path: "D:\\y\\b" })])).toBeNull();
  });

  it("counts excluded and off rows as not indexed", () => {
    const rows = [
      row("docs"),
      row("game", { index: "search" }),
      row("tools", { index: "search", include: false }),
      row("empty", { index: "off", include: false }),
    ];
    expect(indexSummary(rows)).toEqual({ entities: 1, search: 1, off: 2 });
  });

  it("a team uses its wiki repo, or a new one is ticked", () => {
    const rows = [row("docs", { kind: ["wiki"] }), row("game", { kind: ["code"], description: "The client" }), row("ops", { team: "Ops" })];
    expect(teamWikis(rows, "Realms").map((r) => r.member)).toEqual(["docs"]);
    expect(defaultWikiChoice(rows, "Realms")).toEqual({ mode: "existing", member: "docs" });
    expect(defaultWikiChoice(rows, "Ops")).toEqual({ mode: "new", parent: null, name: "Ops-Wiki" });
    expect(defaultWikiChoice(rows, "Ops", "C:/Code")).toEqual({ mode: "new", parent: "C:/Code", name: "Ops-Wiki" });
    expect(coveredRepos(rows, "Realms")).toEqual([{ name: "game", description: "The client", kind: ["code"], path: "C:/Code/game" }]);
  });

  it("a new wiki needs a folder and a name", () => {
    expect(newWikiPath("C:\\Code\\", "Realms-Wiki")).toBe("C:\\Code\\Realms-Wiki");
    expect(newWikiPath("/home/me/code", " Realms-Wiki ")).toBe("/home/me/code/Realms-Wiki");
    expect(wikiChoicesReady({ A: { mode: "new", parent: null, name: "W" } })).toBe(false);
    expect(wikiChoicesReady({ A: { mode: "new", parent: "C:/x", name: "W" }, B: { mode: "none" } })).toBe(true);
  });

  it("renaming the team renames a new wiki still named after it", () => {
    expect(renamedWikiChoice({ mode: "new", parent: "C:/x", name: "AU-Wiki" }, "AU", "AUR")).toEqual({ mode: "new", parent: "C:/x", name: "AUR-Wiki" });
    expect(renamedWikiChoice({ mode: "new", parent: null, name: "Docs" }, "AU", "AUR")).toEqual({ mode: "new", parent: null, name: "Docs" });
    expect(renamedWikiChoice({ mode: "existing", member: "docs" }, "AU", "AUR")).toEqual({ mode: "existing", member: "docs" });
  });
});

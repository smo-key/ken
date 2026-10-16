import { describe, expect, it } from "vitest";
import type { SetupRepoRow } from "../lib/api";
import {
  commonParent,
  confirmBlocked,
  coveredRepos,
  defaultKb,
  kbReady,
  mergeRows,
  newWikiPath,
  renamedKb,
  rowEvidence,
  rowTip,
  sourceSummary,
  STEPS,
  toggleKind,
  withKnowledgeBase,
} from "./setupFlow";

const row = (member: string, over: Partial<SetupRepoRow> = {}): SetupRepoRow => ({
  member,
  include: true,
  kind: [],
  evidence: [],
  remote: null,
  existing: false,
  hasGit: true,
  path: `C:/Code/${member}`,
  description: "",
  ...over,
});

describe("setup flow", () => {
  it("is three steps: the name, the knowledge base, the sources", () => {
    expect(STEPS.map((s) => s.step)).toEqual(["name", "base", "sources"]);
  });

  it("keeps a person's edits when the picked set is proposed again", () => {
    const old = [row("docs", { description: "Our wiki", include: false, kind: ["wiki"] })];
    const fresh = [row("docs", { description: "From the README" }), row("game")];
    const merged = mergeRows(old, fresh);
    expect(merged.map((r) => [r.member, r.description, r.include, r.kind])).toEqual([
      ["docs", "Our wiki", false, ["wiki"]],
      ["game", "", true, []],
    ]);
  });

  it("toggles a kind and keeps the fixed order", () => {
    expect(toggleKind(["code"], "wiki")).toEqual(["wiki", "code"]);
    expect(toggleKind(["wiki", "code"], "wiki")).toEqual(["code"]);
  });

  it("counts a source as indexed or left out, nothing between", () => {
    expect(sourceSummary([row("a"), row("b", { include: false }), row("c")])).toEqual({ indexed: 2, left: 1 });
  });

  it("names a new knowledge base after the workspace and follows a rename", () => {
    const kb = defaultKb("Realms", "C:/Code");
    expect(kb).toEqual({ mode: "new", parent: "C:/Code", name: "Realms-Wiki" });
    expect(renamedKb(kb, "Realms", "AUR")).toEqual({ mode: "new", parent: "C:/Code", name: "AUR-Wiki" });
    expect(renamedKb({ mode: "new", parent: null, name: "Docs" }, "Realms", "AUR")).toEqual({ mode: "new", parent: null, name: "Docs" });
    expect(renamedKb({ mode: "existing", path: "C:/w", map: true }, "Realms", "AUR")).toEqual({ mode: "existing", path: "C:/w", map: true });
  });

  it("step 2 is ready with a folder for a new repo, or an existing wiki picked", () => {
    expect(kbReady({ mode: "new", parent: null, name: "W" })).toBe(false);
    expect(kbReady({ mode: "new", parent: "C:/x", name: " " })).toBe(false);
    expect(kbReady({ mode: "new", parent: "C:/x", name: "W" })).toBe(true);
    expect(kbReady({ mode: "existing", path: null, map: true })).toBe(false);
    expect(kbReady({ mode: "existing", path: "C:/Docs", map: false })).toBe(true);
    expect(newWikiPath("C:\\Code\\", "Realms-Wiki")).toBe("C:\\Code\\Realms-Wiki");
    expect(newWikiPath("/home/me/code", " Realms-Wiki ")).toBe("/home/me/code/Realms-Wiki");
  });

  it("lists the sources on the knowledge base and lets its own row replace a source", () => {
    const rows = [row("docs"), row("game", { kind: ["code"], description: "The client" }), row("old", { include: false })];
    expect(coveredRepos(rows, "C:/Code/docs")).toEqual([{ name: "game", description: "The client", kind: ["code"], path: "C:/Code/game" }]);
    const kb = row("docs", { kind: ["wiki"] });
    expect(withKnowledgeBase(rows, kb).map((r) => [r.member, r.kind])).toEqual([
      ["game", ["code"]],
      ["old", []],
      ["docs", ["wiki"]],
    ]);
    expect(commonParent(rows)).toBe("C:/Code");
  });
});

describe("a source's row", () => {
  it("shows the kind evidence and keeps the paths for the name's tooltip", () => {
    const r = row("tiny-game", { path: "C:/Code/tiny-game", evidence: ["mostly source: 2 of 3 tracked files (.py)", "remote C:/remotes/tiny-game.git"] });
    expect(rowEvidence(r)).toBe("mostly source: 2 of 3 tracked files (.py)");
    expect(rowTip(r)).toBe("C:/Code/tiny-game\nremote C:/remotes/tiny-game.git");
  });

  it("waits for a source to index before Confirm", () => {
    expect(confirmBlocked([])).toBe("No source to index");
    expect(confirmBlocked([row("a", { include: false })])).toBe("No source to index");
    expect(confirmBlocked([row("a")])).toBeNull();
  });
});

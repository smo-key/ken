import { describe, expect, it } from "vitest";
import type { RaidItem, RaidRegister } from "./api";
import { GROUPS, counts, highlightMatches, matches, statusTone, viewOf } from "./log";

function item(kind: string, id: string, title: string, status: string, more: Partial<RaidItem> = {}): RaidItem {
  return { id, title, status, owner: null, date: "", closed: false, kind, repo: null, path: null, line: null, ...more };
}

// Each group newest first, as `raid_register` returns them.
const register: RaidRegister = {
  risks: [
    item("risk", "R-002", "Saves may corrupt", "closed", { closed: true, date: "2026-10-03", path: "Current/RAID.md", line: 9 }),
    item("risk", "R-001", "Friday may slip", "open", { owner: "Ben", date: "2026-10-01", path: "Current/RAID.md", line: 3 }),
  ],
  actions: [item("ticket", "SR-101", "Fix saves", "in-progress", { owner: "Ben", date: "2026-10-04", path: "tickets/SR-101.md" })],
  issues: [item("issue", "I-001", "The build is red", "resolved", { owner: "Ana", date: "2026-10-04" })],
  decisions: [
    item("ruling", "D-002", "Regions save apart.", "live", { owner: "Ana", date: "2026-10-02", path: "Decisions/DECISIONS.md" }),
    item("ruling", "D-001", "Worlds save whole.", "superseded by D-002", { closed: true, date: "2026-09-01" }),
  ],
  milestones: [
    item("release", "v0.3.0", "Alpha", "released", { repo: "game", date: "2026-10-08" }),
    item("roadmap", "", "Combat", "at risk", { date: "2026-11-01", path: "Current/Roadmap.md", line: 7 }),
  ],
};

describe("viewOf", () => {
  it("lists the RAID groups, then the milestones", () => {
    expect(GROUPS.map((g) => g.label)).toEqual(["Risks", "Actions", "Issues", "Decisions", "Milestones"]);
  });

  it("keeps each group's order and folds the closed items apart", () => {
    const v = viewOf(register, "");
    const risks = v.find((g) => g.key === "risks")!;
    expect(risks.open.map((i) => i.id)).toEqual(["R-001"]);
    expect(risks.closed.map((i) => i.id)).toEqual(["R-002"]);
    const decisions = v.find((g) => g.key === "decisions")!;
    expect(decisions.closed.map((i) => i.status)).toEqual(["superseded by D-002"]);
    expect(decisions.closedLabel).toBe("superseded or refined");
  });

  it("searches every group, closed items included", () => {
    const v = viewOf(register, "save");
    expect(v.flatMap((g) => [...g.open, ...g.closed]).map((i) => i.id)).toEqual(["R-002", "SR-101", "D-002", "D-001"]);
    expect(counts(register, "save")).toEqual({ shown: 4, total: 8 });
    expect(counts(register, "")).toEqual({ shown: 8, total: 8 });
  });
});

describe("matches", () => {
  it("reads the id, title, status, owner, date, repo and file", () => {
    const [closed, open] = register.risks;
    expect(matches(open, "ben")).toBe(true);
    expect(matches(open, "r-001")).toBe(true);
    expect(matches(closed, "CLOSED")).toBe(true);
    expect(matches(register.milestones[0], "game")).toBe(true);
    expect(matches(register.actions[0], "tickets/")).toBe(true);
    expect(matches(register.issues[0], "2026-10")).toBe(true);
    expect(matches(open, "nothing like it")).toBe(false);
    expect(matches(open, "  ")).toBe(true);
  });
});

describe("statusTone", () => {
  it("asks for attention on what needs watching", () => {
    expect(statusTone(register.risks[1])).toBe("attn");
    expect(statusTone(register.milestones[1])).toBe("attn");
    expect(statusTone(item("ticket", "SR-1", "x", "blocked"))).toBe("attn");
  });

  it("is calm for what settled, and muted for what folded", () => {
    expect(statusTone(register.issues[0])).toBe("ok");
    expect(statusTone(register.decisions[0])).toBe("ok");
    expect(statusTone(register.risks[0])).toBe("muted");
    expect(statusTone(register.actions[0])).toBe("plain");
  });
});

describe("highlightMatches", () => {
  it("marks case-insensitive matches", () => {
    expect(highlightMatches("The Cutover slipped", "cutover")).toBe("The <mark>Cutover</mark> slipped");
  });

  it("escapes HTML and never executes it", () => {
    expect(highlightMatches("<img src=x> & cutover", "cutover")).toBe("&lt;img src=x&gt; &amp; <mark>cutover</mark>");
    // A query matching escaped-looking text stays escaped around the mark.
    expect(highlightMatches("<b>bold</b>", "bold")).toBe("&lt;b&gt;<mark>bold</mark>&lt;/b&gt;");
  });

  it("empty query just escapes", () => {
    expect(highlightMatches("a < b", "  ")).toBe("a &lt; b");
  });

  it("marks every occurrence", () => {
    expect(highlightMatches("go go go", "go")).toBe("<mark>go</mark> <mark>go</mark> <mark>go</mark>");
  });
});

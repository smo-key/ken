import { describe, expect, it } from "vitest";
import { findingLabel, kindlessRepos, orderFindings, proposedKinds } from "./team";
import type { RepoKind, TeamFinding } from "./api";

const finding = (kind: string, title = kind): TeamFinding => ({ kind, title, detail: "", path: null, projectId: null });

describe("findingLabel", () => {
  it("names each kind plainly", () => {
    expect(findingLabel("drift")).toBe("drift");
    expect(findingLabel("aged")).toBe("unverified");
    expect(findingLabel("links")).toBe("links");
    expect(findingLabel("wiki-draft")).toBe("first draft");
    expect(findingLabel("other")).toBe("other");
    expect(findingLabel("")).toBe("finding");
  });
});

describe("orderFindings", () => {
  it("puts drift first and keeps the order within a kind", () => {
    const list = [finding("links", "l"), finding("aged", "a1"), finding("drift", "d"), finding("x", "x"), finding("aged", "a2")];
    expect(orderFindings(list).map((f) => f.title)).toEqual(["d", "a1", "a2", "l", "x"]);
  });
});

describe("kindlessRepos", () => {
  it("lists the repos with no kind that are still there", () => {
    const repos = [
      { id: "a", kind: [] as RepoKind[], available: true },
      { id: "b", kind: ["code"] as RepoKind[], available: true },
      { id: "c", kind: [] as RepoKind[], available: false },
    ];
    expect(kindlessRepos(repos).map((r) => r.id)).toEqual(["a"]);
  });
});

describe("proposedKinds", () => {
  it("matches a proposal row by folder, then by name", () => {
    const repos = [
      { id: "1", name: "ATT-Wiki", path: "C:\\ken-eval\\ATT\\ATT-Wiki" },
      { id: "2", name: "att-opmodel", path: "C:/elsewhere/att-opmodel" },
      { id: "3", name: "notes", path: "C:/notes" },
    ];
    const rows = [
      { member: "ATT-Wiki", path: "C:/ken-eval/ATT/ATT-Wiki/", kind: ["wiki"] as RepoKind[] },
      { member: "att-opmodel", path: null, kind: ["code"] as RepoKind[] },
      { member: "notes", path: "C:/notes", kind: [] as RepoKind[] },
    ];
    expect(proposedKinds(repos, rows)).toEqual({ "1": ["wiki"], "2": ["code"] });
  });
});

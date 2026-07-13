import { describe, expect, it } from "vitest";
import type { EntityEdge, EntityRow } from "./api";
import { highlightMatches, layoutMap, stableHash } from "./knowledge";

function entity(id: number, name: string): EntityRow {
  return { id, kind: "topic", name, summary: "", sources: [] };
}

function edge(id: number, a: number, b: number): EntityEdge {
  return { id, a, b, label: "" };
}

const entities = [
  entity(1, "Billing cutover"),
  entity(2, "Priya N."),
  entity(3, "LangdonSoft"),
  entity(4, "Decision #41"),
  entity(5, "Contract renewal"),
];
// 1 is the hub (degree 3); 4 connects only to 2; 5 is unconnected.
const edges = [edge(10, 1, 2), edge(11, 1, 3), edge(12, 1, 4), edge(13, 2, 4)];

describe("layoutMap", () => {
  it("is deterministic and centers the highest-degree entity", () => {
    const a = layoutMap(entities, edges);
    const b = layoutMap(entities, edges);
    expect(a).toEqual(b);
    expect(a.get(1)).toEqual({ id: 1, xPct: 50, yPct: 46, ring: "primary" });
    // Neighbors of the hub ring it; everyone gets a position.
    expect(a.get(2)?.ring).toBe("inner");
    expect(a.get(3)?.ring).toBe("inner");
    expect(a.get(4)?.ring).toBe("inner");
    expect(a.size).toBe(entities.length);
  });

  it("puts degree-0 entities in the clamped peripheral band", () => {
    const nodes = layoutMap(entities, edges);
    const lone = nodes.get(5)!;
    expect(lone.ring).toBe("unconnected");
    // Outside the outer ring's reach, inside the clamps.
    const dx = Math.abs(lone.xPct - 50);
    const dy = Math.abs(lone.yPct - 46);
    expect(Math.max(dx / 46, dy / 41)).toBeGreaterThan(0.9);
    expect(lone.xPct).toBeGreaterThanOrEqual(6);
    expect(lone.xPct).toBeLessThanOrEqual(94);
    expect(lone.yPct).toBeGreaterThanOrEqual(8);
    expect(lone.yPct).toBeLessThanOrEqual(88);
  });

  it("handles no edges (no primary) and empty input", () => {
    const nodes = layoutMap(entities, []);
    for (const n of nodes.values()) expect(n.ring).toBe("unconnected");
    expect(layoutMap([], []).size).toBe(0);
  });

  it("distinct ring nodes never collide", () => {
    const seen = new Set<string>();
    for (const n of layoutMap(entities, edges).values()) {
      const key = `${n.xPct.toFixed(2)},${n.yPct.toFixed(2)}`;
      expect(seen.has(key)).toBe(false);
      seen.add(key);
    }
  });
});

describe("stableHash", () => {
  it("is stable and spreads names", () => {
    expect(stableHash("Priya N.")).toBe(stableHash("Priya N."));
    expect(stableHash("Priya N.")).not.toBe(stableHash("LangdonSoft"));
  });
});

describe("highlightMatches", () => {
  it("marks case-insensitive matches", () => {
    expect(highlightMatches("The Cutover slipped", "cutover")).toBe(
      "The <mark>Cutover</mark> slipped",
    );
  });

  it("escapes HTML and never executes it", () => {
    expect(highlightMatches("<img src=x> & cutover", "cutover")).toBe(
      "&lt;img src=x&gt; &amp; <mark>cutover</mark>",
    );
    // A query matching escaped-looking text stays escaped around the mark.
    expect(highlightMatches("<b>bold</b>", "bold")).toBe(
      "&lt;b&gt;<mark>bold</mark>&lt;/b&gt;",
    );
  });

  it("empty query just escapes", () => {
    expect(highlightMatches("a < b", "  ")).toBe("a &lt; b");
  });

  it("marks every occurrence", () => {
    expect(highlightMatches("go go go", "go")).toBe(
      "<mark>go</mark> <mark>go</mark> <mark>go</mark>",
    );
  });
});

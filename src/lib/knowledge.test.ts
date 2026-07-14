import { describe, expect, it } from "vitest";
import type { EntityEdge, EntityRow } from "./api";
import {
  computeMapView,
  highlightMatches,
  layoutMap,
  type MapViewInput,
  stableHash,
} from "./knowledge";

function entity(
  id: number,
  name: string,
  kind: EntityRow["kind"] = "topic",
  summary = "",
): EntityRow {
  return { id, kind, name, summary, sources: [] };
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

describe("computeMapView", () => {
  // A small graph: 1 is the hub, 2/3/4 hang off it, 5 is a lone person,
  // 6 an organization connected only to 4.
  const ents = [
    entity(1, "Billing cutover", "topic", "The migration project."),
    entity(2, "Priya N.", "person", "Owns the cutover."),
    entity(3, "Contract", "topic"),
    entity(4, "Decision #41", "decision"),
    entity(5, "Lone person", "person"),
    entity(6, "LangdonSoft", "organization"),
  ];
  const es = [edge(10, 1, 2), edge(11, 1, 3), edge(12, 1, 4), edge(13, 4, 6)];

  const base: MapViewInput = {
    entities: ents,
    edges: es,
    query: "",
    kinds: [],
    selected: null,
    hovered: null,
    showAllLabels: false,
    prominentCount: 2,
  };

  it("idle: labels only the most-connected, everyone visible, none dimmed", () => {
    const v = computeMapView(base);
    // Hub (deg 3) and its neighbor Decision #41 (deg 2) are the top two.
    expect(v.get(1)?.labeled).toBe(true);
    expect(v.get(4)?.labeled).toBe(true);
    // A leaf is a plain tagged dot until you interact.
    expect(v.get(3)?.labeled).toBe(false);
    for (const nv of v.values()) {
      expect(nv.visible).toBe(true);
      expect(nv.dimmed).toBe(false);
    }
  });

  it("showAllLabels labels every visible node when idle", () => {
    const v = computeMapView({ ...base, showAllLabels: true });
    for (const nv of v.values()) expect(nv.labeled).toBe(true);
  });

  it("kind filter hides non-matching kinds entirely", () => {
    const v = computeMapView({ ...base, kinds: ["person"] });
    expect(v.get(2)?.visible).toBe(true);
    expect(v.get(5)?.visible).toBe(true);
    // Non-person kinds drop out of the graph.
    expect(v.get(1)?.visible).toBe(false);
    expect(v.get(4)?.visible).toBe(false);
  });

  it("search: matches are flagged and labeled, neighbors stay visible, rest dim", () => {
    const v = computeMapView({ ...base, query: "priya" });
    expect(v.get(2)?.matched).toBe(true);
    expect(v.get(2)?.labeled).toBe(true);
    expect(v.get(2)?.dimmed).toBe(false);
    // Priya's neighbour (the hub) is in focus, not dimmed.
    expect(v.get(1)?.dimmed).toBe(false);
    // Unrelated nodes dim but remain for context.
    expect(v.get(5)?.matched).toBe(false);
    expect(v.get(5)?.dimmed).toBe(true);
    expect(v.get(5)?.visible).toBe(true);
  });

  it("search matches summaries too", () => {
    const v = computeMapView({ ...base, query: "migration" });
    expect(v.get(1)?.matched).toBe(true);
  });

  it("selection: highlights the node and its neighbors, dims the rest", () => {
    const v = computeMapView({ ...base, selected: 1 });
    expect(v.get(1)?.dimmed).toBe(false);
    expect(v.get(1)?.labeled).toBe(true);
    // Direct neighbours of the hub stay lit and labelled.
    for (const id of [2, 3, 4]) {
      expect(v.get(id)?.dimmed).toBe(false);
      expect(v.get(id)?.labeled).toBe(true);
    }
    // Two hops away → dimmed and unlabelled.
    expect(v.get(6)?.dimmed).toBe(true);
    expect(v.get(6)?.labeled).toBe(false);
    // The lone node is dimmed too.
    expect(v.get(5)?.dimmed).toBe(true);
  });

  it("kind filter composes with selection (filtered-out nodes never show)", () => {
    const v = computeMapView({ ...base, selected: 1, kinds: ["person", "topic"] });
    // Decision #41 is a neighbour but filtered out by kind.
    expect(v.get(4)?.visible).toBe(false);
    // A person neighbour still shows.
    expect(v.get(2)?.visible).toBe(true);
    expect(v.get(2)?.dimmed).toBe(false);
  });

  it("hover labels the hovered node and its neighbors when idle", () => {
    const v = computeMapView({ ...base, hovered: 3 });
    expect(v.get(3)?.labeled).toBe(true);
    expect(v.get(1)?.labeled).toBe(true);
    // A non-neighbour leaf stays a dot.
    expect(v.get(5)?.labeled).toBe(false);
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

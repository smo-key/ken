// Pure helpers behind the knowledge views: the deterministic map
// layout (no physics, no stored positions) and safe match highlighting
// for the timeline search.
import type { EntityEdge, EntityRow } from "./api";

export type MapRing = "primary" | "inner" | "outer" | "unconnected";

export interface MapNode {
  id: number;
  /** Position as percentages of the canvas. */
  xPct: number;
  yPct: number;
  ring: MapRing;
}

/** FNV-1a — a stable, dependency-free hash for layout seeding. */
export function stableHash(s: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}

const CX = 50;
const CY = 46;

/**
 * Deterministic layout: the highest-degree entity centered, its
 * neighbors on an inner ellipse, other connected entities on an outer
 * ellipse, degree-0 entities on a clamped peripheral band. Within each
 * ring nodes are ordered and angle-jittered by a stable hash of their
 * name, so the same model always draws the same picture.
 */
export function layoutMap(
  entities: EntityRow[],
  edges: EntityEdge[],
): Map<number, MapNode> {
  const out = new Map<number, MapNode>();
  if (entities.length === 0) return out;

  const ids = new Set(entities.map((e) => e.id));
  const degree = new Map<number, number>();
  for (const e of entities) degree.set(e.id, 0);
  for (const edge of edges) {
    if (!ids.has(edge.a) || !ids.has(edge.b)) continue;
    degree.set(edge.a, (degree.get(edge.a) ?? 0) + 1);
    degree.set(edge.b, (degree.get(edge.b) ?? 0) + 1);
  }

  // Primary: highest degree, name as the deterministic tiebreak. With
  // no edges at all there is no center — everything is unconnected.
  const byDegree = [...entities].sort(
    (x, y) =>
      (degree.get(y.id) ?? 0) - (degree.get(x.id) ?? 0) ||
      x.name.localeCompare(y.name),
  );
  const primary = (degree.get(byDegree[0].id) ?? 0) > 0 ? byDegree[0] : null;

  const neighborIds = new Set<number>();
  if (primary) {
    for (const edge of edges) {
      if (edge.a === primary.id) neighborIds.add(edge.b);
      if (edge.b === primary.id) neighborIds.add(edge.a);
    }
  }

  const inner: EntityRow[] = [];
  const outer: EntityRow[] = [];
  const unconnected: EntityRow[] = [];
  for (const e of entities) {
    if (primary && e.id === primary.id) continue;
    if ((degree.get(e.id) ?? 0) === 0) unconnected.push(e);
    else if (neighborIds.has(e.id)) inner.push(e);
    else outer.push(e);
  }

  if (primary) {
    out.set(primary.id, { id: primary.id, xPct: CX, yPct: CY, ring: "primary" });
  }
  placeRing(out, inner, "inner", 24, 20);
  placeRing(out, outer, "outer", 38, 32);
  placeRing(out, unconnected, "unconnected", 46, 41);
  return out;
}

/** Even spacing around an ellipse, hash-ordered and hash-jittered. */
function placeRing(
  out: Map<number, MapNode>,
  nodes: EntityRow[],
  ring: MapRing,
  rx: number,
  ry: number,
): void {
  if (nodes.length === 0) return;
  const sorted = [...nodes].sort(
    (a, b) => stableHash(a.name) - stableHash(b.name) || a.name.localeCompare(b.name),
  );
  const step = (2 * Math.PI) / sorted.length;
  for (let i = 0; i < sorted.length; i++) {
    const node = sorted[i];
    // A small per-name jitter inside the slot keeps rings organic
    // without ever colliding two slots.
    const jitter = ((stableHash(node.name) % 1000) / 1000 - 0.5) * step * 0.5;
    const angle = i * step + jitter;
    const x = clamp(CX + rx * Math.cos(angle), 6, 94);
    const y = clamp(CY + ry * Math.sin(angle), 8, 88);
    out.set(node.id, { id: node.id, xPct: x, yPct: y, ring });
  }
}

function clamp(v: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, v));
}

/**
 * Escape-then-mark: HTML in `text` is escaped, and the only tags in the
 * result are our own <mark>s around case-insensitive matches of `query`.
 */
export function highlightMatches(text: string, query: string): string {
  const esc = (s: string) =>
    s.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;");
  const q = query.trim();
  if (!q) return esc(text);
  const lower = text.toLowerCase();
  const ql = q.toLowerCase();
  let out = "";
  let i = 0;
  for (;;) {
    const at = lower.indexOf(ql, i);
    if (at === -1) break;
    out +=
      esc(text.slice(i, at)) +
      "<mark>" +
      esc(text.slice(at, at + q.length)) +
      "</mark>";
    i = at + q.length;
  }
  return out + esc(text.slice(i));
}

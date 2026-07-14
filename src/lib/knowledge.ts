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

export type EntityKind = EntityRow["kind"];

/** Per-node display state the Map renders from — the declutter decision. */
export interface NodeView {
  /** Rendered at all. A kind filter is the only thing that hides a node. */
  visible: boolean;
  /** Faded to background: present for context but out of the current focus. */
  dimmed: boolean;
  /** Show the text label; otherwise the node is a bare tagged dot. */
  labeled: boolean;
  /** Matches the active search query (drives the highlight ring). */
  matched: boolean;
}

export interface MapViewInput {
  entities: EntityRow[];
  edges: EntityEdge[];
  /** Free-text search over name + summary; empty means no search. */
  query: string;
  /** Kinds to keep; empty array means all kinds. */
  kinds: EntityKind[];
  /** The selected node id, or null. */
  selected: number | null;
  /** The hovered node id, or null. */
  hovered: number | null;
  /** Zoomed in far enough to afford every label. */
  showAllLabels: boolean;
  /** How many most-connected nodes carry a label at rest. */
  prominentCount: number;
}

/** Undirected adjacency over entities present in the model. */
export function adjacency(
  entities: EntityRow[],
  edges: EntityEdge[],
): Map<number, Set<number>> {
  const ids = new Set(entities.map((e) => e.id));
  const adj = new Map<number, Set<number>>();
  for (const e of entities) adj.set(e.id, new Set());
  for (const edge of edges) {
    if (!ids.has(edge.a) || !ids.has(edge.b)) continue;
    adj.get(edge.a)!.add(edge.b);
    adj.get(edge.b)!.add(edge.a);
  }
  return adj;
}

/**
 * The declutter / focus / filter decision for every node, as one pure
 * function so it can be tested and memoized apart from rendering. It runs
 * on search/selection/hover/filter changes only — never on pan or zoom,
 * which merely transform the already-laid-out view.
 *
 * Precedence: a kind filter hides non-matching nodes outright. Otherwise
 * a *focus* — an active selection, else an active search — lights its
 * node(s) and direct neighbors and dims everything else. With no focus,
 * labels are budgeted to the most-connected nodes plus whatever the
 * pointer is hovering (or every node once zoomed in).
 */
export function computeMapView(input: MapViewInput): Map<number, NodeView> {
  const { entities, edges, query, kinds, selected, hovered, showAllLabels } =
    input;
  const adj = adjacency(entities, edges);
  const kindSet = new Set(kinds);
  const kindOk = (k: EntityKind) => kindSet.size === 0 || kindSet.has(k);

  const q = query.trim().toLowerCase();
  const matched = new Set<number>();
  if (q) {
    for (const e of entities) {
      if (!kindOk(e.kind)) continue;
      if (
        e.name.toLowerCase().includes(q) ||
        e.summary.toLowerCase().includes(q)
      ) {
        matched.add(e.id);
      }
    }
  }

  // At-rest labels go to the highest-degree visible nodes (name tiebreak),
  // so the graph reads as a few anchors amid a field of dots.
  const prominent = new Set(
    [...entities]
      .filter((e) => kindOk(e.kind) && (adj.get(e.id)?.size ?? 0) > 0)
      .sort(
        (a, b) =>
          (adj.get(b.id)?.size ?? 0) - (adj.get(a.id)?.size ?? 0) ||
          a.name.localeCompare(b.name),
      )
      .slice(0, Math.max(0, input.prominentCount))
      .map((e) => e.id),
  );

  // The focus roots: the selection wins over a search.
  let focusRoots: number[] | null = null;
  if (selected !== null) focusRoots = [selected];
  else if (q) focusRoots = [...matched];

  const expand = (roots: number[]) => {
    const set = new Set<number>();
    for (const r of roots) {
      set.add(r);
      for (const n of adj.get(r) ?? []) set.add(n);
    }
    return set;
  };
  const focusSet = focusRoots ? expand(focusRoots) : null;
  const hoverSet = hovered !== null ? expand([hovered]) : null;

  const out = new Map<number, NodeView>();
  for (const e of entities) {
    if (!kindOk(e.kind)) {
      out.set(e.id, {
        visible: false,
        dimmed: false,
        labeled: false,
        matched: false,
      });
      continue;
    }
    let dimmed = false;
    let labeled: boolean;
    if (focusSet) {
      const inFocus = focusSet.has(e.id);
      dimmed = !inFocus;
      labeled = inFocus;
    } else {
      labeled =
        showAllLabels || prominent.has(e.id) || (hoverSet?.has(e.id) ?? false);
    }
    out.set(e.id, { visible: true, dimmed, labeled, matched: matched.has(e.id) });
  }
  return out;
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

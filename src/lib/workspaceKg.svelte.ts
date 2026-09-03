// Workspace knowledge-graph state behind the Map screen's "Workspace" mode
// (federated-kg task 3.3): whether the `federatedKg` flag is on, the
// overview (counts + per-member staleness), a rebuild's live progress, and
// the explored subgraph the wiki-style Map view accumulates as the user
// searches and follows links.
//
// There is no "list every global entity + edge" command (only
// `workspace_kg_overview` for counts, `workspace_kg_entity(id)` for one
// wiki page, and `workspace_kg_search(query)` capped at 50 hits) — see the
// final report for this task. So unlike the per-project Map, which loads
// its whole model up front, the workspace graph starts empty and grows:
// a search seeds nodes, opening one fetches its full wiki page and adds
// stub nodes + real edges for every out-/back-link, and opening a stub
// upgrades it to a full node the same way.
import {
  api,
  type WorkspaceKgEntity,
  type WorkspaceKgOverview,
  type WorkspaceKgSearchHit,
  type WorkspaceKgState,
} from "./api";
import type { MapEdgeInput, MapEntity } from "./knowledge";
import { stableHash } from "./knowledge";

/** Stable hue (0-359) for a member project id — "member-hue nodes (stable
 *  hue per project_id)" (federated-kg task 3.3). Reuses the same FNV-1a hash
 *  the per-project Map layout already relies on for determinism. */
export function memberHue(projectId: string): number {
  return stableHash(projectId) % 360;
}

/** A CSS color for a member project id, at a lightness/saturation chosen to
 *  read against both the light and dark node backgrounds. */
export function memberColor(projectId: string): string {
  return `hsl(${memberHue(projectId)} 60% 50%)`;
}

class WorkspaceKgStore {
  /** Whether the `federatedKg` flag resolves on — gates whether the Map
   *  screen's Project/Workspace toggle even appears. */
  enabled = $state(false);
  overview = $state<WorkspaceKgOverview | null>(null);
  buildState = $state<WorkspaceKgState | null>(null);

  searchQuery = $state("");
  searchHits = $state<WorkspaceKgSearchHit[]>([]);
  searching = $state(false);

  selectedId = $state<number | null>(null);
  selectedEntity = $state<WorkspaceKgEntity | null>(null);
  selectedLoading = $state(false);
  selectedError = $state<string | null>(null);

  /** The explored subgraph, keyed by global entity id / edge id. A node
   *  discovered only as a link target (not yet opened) is a "stub": kind
   *  `"other"`, empty summary, until `select()` fetches its real page. */
  nodes = $state<Map<number, MapEntity>>(new Map());
  edges = $state<Map<number, MapEdgeInput>>(new Map());

  /** Distinct member project ids each fully-opened entity's doc pointers
   *  touch — the closest honest proxy for `entity_links` membership
   *  available from the wiki payload (no command surfaces the federation
   *  mapping directly). Keyed by global entity id; absent for stubs. */
  private memberIdsCache = new Map<number, string[]>();

  private initDone = false;

  /** Call once (Map screen mount): resolve the flag and subscribe to
   *  build progress. Cheap even if the user never opens Workspace mode —
   *  `workspace_kg_overview` (the first real read) is fetched lazily by
   *  the caller only when the toggle is actually switched. */
  async init() {
    if (this.initDone) return;
    this.initDone = true;
    this.enabled = await this.checkEnabled();
    await api.onWorkspaceKgState((ev) => {
      this.buildState = ev;
      if (ev.state === "ready") {
        // Entity/edge ids can shift across a rebuild (full re-merge, design
        // D3) — the accumulated explored graph would otherwise point at
        // stale ids, so start the explorer over.
        this.resetGraph();
        void this.refreshOverview();
      } else if (ev.state === "unavailable") {
        void this.refreshOverview();
      }
    });
  }

  private async checkEnabled(): Promise<boolean> {
    const features = await api.listFeatures().catch(() => []);
    return features.find((f) => f.name === "federatedKg")?.effective ?? false;
  }

  async refreshOverview() {
    if (!this.enabled) return;
    this.overview = await api.workspaceKgOverview().catch(() => null);
  }

  resetGraph() {
    this.nodes = new Map();
    this.edges = new Map();
    this.memberIdsCache.clear();
    this.searchHits = [];
    this.selectedId = null;
    this.selectedEntity = null;
    this.selectedError = null;
  }

  async rebuild() {
    await api.rebuildWorkspaceKg();
  }

  /** Search global entities and seed the graph with the hits (each carries
   *  kind + summary, so hits render as full nodes right away, not stubs). */
  async search(query: string) {
    this.searchQuery = query;
    const q = query.trim();
    if (!q) {
      this.searchHits = [];
      return;
    }
    this.searching = true;
    try {
      const hits = await api.workspaceKgSearch(q);
      this.searchHits = hits;
      const next = new Map(this.nodes);
      for (const hit of hits) {
        next.set(hit.id, {
          id: hit.id,
          kind: hit.kind,
          name: hit.name,
          summary: hit.summary,
        });
      }
      this.nodes = next;
    } catch {
      this.searchHits = [];
    } finally {
      this.searching = false;
    }
  }

  /** Open one global entity's wiki page: fetch the full payload, upgrade
   *  its node to full, and grow the graph with a stub node + real edge for
   *  every out-/back-link so exploring outward is one click per hop. */
  async select(id: number) {
    this.selectedId = id;
    this.selectedEntity = null;
    this.selectedError = null;
    this.selectedLoading = true;
    try {
      const entity = await api.workspaceKgEntity(id);
      this.selectedEntity = entity;
      this.memberIdsCache.set(
        entity.id,
        [...new Set(entity.pointers.map((p) => p.projectId))],
      );

      const nextNodes = new Map(this.nodes);
      nextNodes.set(entity.id, {
        id: entity.id,
        kind: entity.kind,
        name: entity.name,
        summary: entity.summary,
      });
      const nextEdges = new Map(this.edges);
      for (const link of entity.outLinks) {
        if (!nextNodes.has(link.otherId)) {
          nextNodes.set(link.otherId, {
            id: link.otherId,
            kind: "other",
            name: link.otherName,
            summary: "",
          });
        }
        nextEdges.set(link.id, {
          id: link.id,
          a: entity.id,
          b: link.otherId,
          label: link.relation,
        });
      }
      for (const link of entity.backLinks) {
        if (!nextNodes.has(link.otherId)) {
          nextNodes.set(link.otherId, {
            id: link.otherId,
            kind: "other",
            name: link.otherName,
            summary: "",
          });
        }
        nextEdges.set(link.id, {
          id: link.id,
          a: link.otherId,
          b: entity.id,
          label: link.relation,
        });
      }
      this.nodes = nextNodes;
      this.edges = nextEdges;
    } catch (e) {
      this.selectedError = String(e);
    } finally {
      this.selectedLoading = false;
    }
  }

  deselect() {
    this.selectedId = null;
    this.selectedEntity = null;
    this.selectedError = null;
  }

  /** Multi-member badge input: distinct member project ids linked to a
   *  fully-opened entity, or `[]` for a stub / not-yet-opened node. */
  memberIdsOf(id: number): string[] {
    return this.memberIdsCache.get(id) ?? [];
  }
}

export const workspaceKg = new WorkspaceKgStore();

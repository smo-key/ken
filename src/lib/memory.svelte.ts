// ken-memory task 4.1/4.2 frontend store: the `kenMemory` flag, live
// distillation state, and the pending approval-card roster. Mirrors
// `workspaceKg.svelte.ts`'s shape (flag-gated `init()`, an event
// subscription that updates `$state`, thin wrappers over `api` calls).
import { api, type DistillCandidate } from "./api";

export type MemoryPhase = "idle" | "planning" | "distilling" | "ready" | "error";

class MemoryStore {
  /** Whether the `kenMemory` flag resolves on — gates the Settings "Memory"
   *  section (including the "Distill journal" trigger) entirely. */
  enabled = $state(false);

  /** Live distillation lifecycle (design D6: planning → distilling →
   *  ready | error), driven by the `memory-state` event. `"idle"` before
   *  any run this session. */
  phase = $state<MemoryPhase>("idle");
  errorReason = $state<string | null>(null);

  /** Pending approval-card roster from the last `ready` event. Trimmed
   *  locally on `resolve()` — `resolve_distill_candidate` mutates the file/
   *  dismissed-slug set on the backend but never re-emits `memory-state`,
   *  so there's no server signal to wait for; removing the resolved slug
   *  from this list IS the honest reflection of what just happened. */
  candidates = $state<DistillCandidate[]>([]);

  /** Slug currently being approved/dismissed, for per-card busy state. */
  resolvingSlug = $state<string | null>(null);

  private initDone = false;

  /** Call once (Settings screen mount): resolve the flag and subscribe to
   *  distillation progress. Cheap even if the user never opens Settings'
   *  Memory section — nothing here starts a distillation run on its own. */
  async init() {
    if (this.initDone) return;
    this.initDone = true;
    this.enabled = await this.checkEnabled();
    await api.onMemoryState((ev) => {
      this.phase = ev.state;
      if (ev.state === "ready") {
        this.candidates = ev.candidates;
        this.errorReason = null;
      } else if (ev.state === "error") {
        this.errorReason = ev.reason;
      }
    });
  }

  private async checkEnabled(): Promise<boolean> {
    const features = await api.listFeatures().catch(() => []);
    return features.find((f) => f.name === "kenMemory")?.effective ?? false;
  }

  /** Kick off a distillation pass ("Distill journal" button). Progress
   *  arrives via the `memory-state` subscription above. */
  async distill() {
    if (!this.enabled) return;
    await api.distillJournal();
  }

  /** Approve or dismiss one pending candidate. */
  async resolve(slug: string, approve: boolean) {
    this.resolvingSlug = slug;
    try {
      await api.resolveDistillCandidate(slug, approve);
      this.candidates = this.candidates.filter((c) => c.slug !== slug);
    } finally {
      this.resolvingSlug = null;
    }
  }
}

export const memory = new MemoryStore();

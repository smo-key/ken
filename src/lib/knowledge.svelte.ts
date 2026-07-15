// Knowledge-model state shared by the Map and Timeline screens: the stored
// model, whether a manual Deep rebuild is running, incremental coverage, and
// whether the local model is available to keep extracting.
import { api, type KnowledgeModel } from "./api";

class KnowledgeStore {
  model = $state<KnowledgeModel | null>(null);
  building = $state(false);
  error = $state<string | null>(null);
  /** Optimistic until claude_doctor answers (Deep rebuild needs Claude). */
  claudeFound = $state(true);
  private initDone = false;

  /** No entities yet AND no deep build has ever stamped the model. */
  get empty(): boolean {
    return (
      this.model === null ||
      (this.model.entities.length === 0 && this.model.builtAt === null)
    );
  }

  /**
   * "N of M files analyzed" (plus any terminal failures), or null when fully
   * caught up with no failures / nothing indexed. When every extractable file
   * has been accounted for (analyzed + failed >= total) but some failed, we
   * still surface the line so the failures aren't silently hidden.
   */
  get coverage(): { analyzed: number; total: number; failed: number } | null {
    const m = this.model;
    if (!m || m.total === 0) return null;
    const failed = m.failed ?? 0;
    // Nothing left to analyze and nothing failed → fully caught up, hide.
    if (m.analyzed >= m.total && failed === 0) return null;
    return { analyzed: m.analyzed, total: m.total, failed };
  }

  /** The local model can't extract right now — show the plain notice. */
  get llmPaused(): boolean {
    return (
      this.model?.llmStatus === "notInstalled" ||
      this.model?.llmStatus === "error"
    );
  }

  get llmNotice(): string {
    if (this.model?.llmStatus === "error") {
      return "Ken's on-device model hit a snag — mapping is paused. Open Settings to check the model.";
    }
    // notInstalled
    return "Ken maps your project on your Mac. Choose the on-device model in Settings to begin.";
  }

  /** Call on screen mount: subscribe once, re-read every visit. */
  async visit() {
    if (!this.initDone) {
      this.initDone = true;
      await api.onKnowledgeModelState((ev) => {
        if (ev.state === "building") {
          this.building = true;
          this.error = null;
        } else if (ev.state === "ready") {
          this.building = false;
          this.error = null;
          void this.load();
        } else if (ev.state === "idle") {
          this.building = false;
          this.error = null;
        } else {
          this.building = false;
          this.error = ev.detail ?? "The rebuild didn't finish.";
        }
      });
      // Incremental merges land continuously; throttle the reload so a burst
      // of merged files triggers at most one refetch per ~600ms.
      await api.onKnowledgeUpdated(() => this.throttledLoad());
      this.claudeFound =
        (await api.claudeDoctor().catch(() => null))?.found ?? false;
    }
    await this.load();
  }

  private loadTimer: ReturnType<typeof setTimeout> | undefined;
  private throttledLoad() {
    if (this.loadTimer) return;
    this.loadTimer = setTimeout(() => {
      this.loadTimer = undefined;
      void this.load();
    }, 600);
  }

  async load() {
    if (!(await api.currentProject().catch(() => null))) return;
    this.model = await api.knowledgeModel().catch(() => null);
    if (this.model?.building) this.building = true;
  }

  /** Manual Deep rebuild (Claude, whole-model replace). */
  async refresh() {
    this.error = null;
    this.building = true;
    try {
      await api.refreshKnowledgeModel();
    } catch (e) {
      this.building = false;
      this.error = String(e);
    }
  }
}

export const knowledge = new KnowledgeStore();

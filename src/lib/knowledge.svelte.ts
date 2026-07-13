// Knowledge-model state shared by the Map and Timeline screens: the
// stored model, whether a rebuild is running, and whether Claude is
// around to build one.
import { api, type KnowledgeModel } from "./api";

class KnowledgeStore {
  model = $state<KnowledgeModel | null>(null);
  building = $state(false);
  error = $state<string | null>(null);
  /** Optimistic until claude_doctor answers. */
  claudeFound = $state(true);
  private initDone = false;

  /** No model has ever been built for this project. */
  get empty(): boolean {
    return this.model === null || this.model.builtAt === null;
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
        } else {
          this.building = false;
          this.error = ev.detail ?? "The refresh didn't finish.";
        }
      });
      this.claudeFound =
        (await api.claudeDoctor().catch(() => null))?.found ?? false;
    }
    await this.load();
  }

  async load() {
    if (!(await api.currentProject().catch(() => null))) return;
    this.model = await api.knowledgeModel().catch(() => null);
  }

  /** Rebuild the model (the Refresh buttons on both screens). */
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

// Automations state: the list, the selected detail, and per-slug live run
// events. Fed by list/get commands + the shared ingest-run-changed event,
// filtered to kind==="automation" (ingest-kind events route to ingests.svelte).
import { api, type Automation, type AutomationDetail, type IngestEvent } from "./api";

const TERMINAL = new Set(["fresh", "failed", "discarded", "cancelled"]);

/** Only automation-kind events belong to this store; ingests route elsewhere. */
export function routesToAutomation(ev: IngestEvent): boolean {
  return ev.kind === "automation";
}

class AutomationsStore {
  list = $state<Automation[]>([]);
  selected = $state<string | null>(null);
  detail = $state<AutomationDetail | null>(null);
  /** Latest live event per slug (running/queued/waiting before the DB catches up). */
  live = $state<Record<string, IngestEvent>>({});

  /** The raw live event for a slug (activity/elapsed/eta), or null. */
  liveEvent(slug: string): IngestEvent | null {
    return this.live[slug] ?? null;
  }

  async init() {
    await api.onIngestRunChanged((ev) => void this.onEvent(ev));
    await this.refresh();
  }

  private async onEvent(ev: IngestEvent) {
    if (!routesToAutomation(ev)) return;
    this.live = { ...this.live, [ev.slug]: ev };
    if (TERMINAL.has(ev.status)) {
      await this.refresh();
      if (this.selected === ev.slug) await this.select(ev.slug);
      const { [ev.slug]: _gone, ...rest } = this.live;
      this.live = rest;
    }
  }

  async refresh() {
    if (!(await hasProject())) return;
    this.list = await api.listAutomations();
  }

  async select(slug: string | null) {
    this.selected = slug;
    this.detail = slug ? await api.getAutomation(slug).catch(() => null) : null;
  }

  async run(slug: string) {
    await api.runAutomation(slug);
  }

  async remove(slug: string) {
    await api.deleteAutomation(slug);
    if (this.selected === slug) await this.select(null);
    await this.refresh();
  }
}

async function hasProject(): Promise<boolean> {
  return (await api.currentProject().catch(() => null)) !== null;
}

export const automations = new AutomationsStore();

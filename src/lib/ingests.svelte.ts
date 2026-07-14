// Ingests state: summaries, selected detail, live run status, pending
// approvals. Fed by list/get commands + the ingest-run-changed event.
import {
  api,
  type ClaudeDoctor,
  type IngestDetail,
  type IngestEvent,
  type IngestSummary,
  type LiveStatus,
  type RunRow,
} from "./api";

/** Run statuses that mean "the run is over". */
const TERMINAL: LiveStatus[] = ["fresh", "failed", "discarded", "cancelled", "pending_approval"];

/** Only ingest-kind events belong to this store; automations route elsewhere. */
export function routesToIngest(ev: IngestEvent): boolean {
  return ev.kind === "ingest";
}

class IngestsStore {
  summaries = $state<IngestSummary[]>([]);
  selected = $state<string | null>(null);
  detail = $state<IngestDetail | null>(null);
  /** Latest live event per slug (running/blocked flashes before DB catches up). */
  live = $state<Record<string, IngestEvent>>({});
  pending = $state<RunRow[]>([]);
  doctor = $state<ClaudeDoctor | null>(null);

  get waitingOnYou(): RunRow[] {
    return this.pending;
  }

  liveStatus(slug: string): LiveStatus | null {
    return this.live[slug]?.status ?? null;
  }

  /** The raw live event for a slug (activity/elapsed/eta), or null. */
  liveEvent(slug: string): IngestEvent | null {
    return this.live[slug] ?? null;
  }

  async init() {
    await api.onIngestRunChanged((ev) => void this.onEvent(ev));
    await this.refresh();
    this.doctor = await api.claudeDoctor().catch(() => null);
  }

  private async onEvent(ev: IngestEvent) {
    // Automation events share the channel but belong to the automations store.
    if (!routesToIngest(ev)) return;
    // queued/waiting/running are non-terminal: keep the transient marker so the
    // detail pane can render the live caption; only TERMINAL drops it + refreshes.
    this.live = { ...this.live, [ev.slug]: ev };
    if (TERMINAL.includes(ev.status)) {
      await this.refresh();
      if (this.selected === ev.slug) await this.select(ev.slug);
      // Terminal state is now in the DB; drop the transient marker.
      const { [ev.slug]: _gone, ...rest } = this.live;
      this.live = rest;
    }
  }

  async refresh() {
    if (!(await hasProject())) return;
    this.summaries = await api.listIngests();
    this.pending = await api.pendingApprovals();
  }

  async select(slug: string | null) {
    this.selected = slug;
    this.detail = slug ? await api.getIngest(slug).catch(() => null) : null;
  }

  async run(slug: string) {
    await api.runIngest(slug, true);
  }

  async cancel(slug: string) {
    await api.cancelRun(slug);
  }

  async delete(slug: string) {
    await api.deleteIngest(slug);
    if (this.selected === slug) await this.select(null);
    await this.refresh();
  }

  async approve(runId: number) {
    await api.approveRun(runId);
    await this.refresh();
    if (this.selected) await this.select(this.selected);
  }

  async discard(runId: number) {
    await api.discardRun(runId);
    await this.refresh();
    if (this.selected) await this.select(this.selected);
  }
}

async function hasProject(): Promise<boolean> {
  return (await api.currentProject().catch(() => null)) !== null;
}

export const ingests = new IngestsStore();

/** Plain-language caption for an ingest's current state. */
export function statusCaption(
  s: IngestSummary,
  live: LiveStatus | null,
  now = Date.now(),
): { label: string; tone: "healthy" | "busy" | "attention" | "danger" | "muted" } {
  if (s.entry.kind === "broken") {
    return { label: "recipe has a problem", tone: "danger" };
  }
  if (live === "running") return { label: "running…", tone: "busy" };
  if (live === "blocked") return { label: "blocked on you", tone: "attention" };
  const run = s.lastRun;
  if (!run) return { label: "never run", tone: "muted" };
  const ago = agoShort(run.startedAt, now);
  switch (run.status) {
    case "running":
      return { label: "running…", tone: "busy" };
    case "pending_approval":
      return { label: "waiting for your approval", tone: "attention" };
    case "blocked":
      return { label: "blocked on you", tone: "attention" };
    case "failed":
      return { label: `failed · ${ago}`, tone: "danger" };
    case "cancelled":
      return { label: `cancelled · ${ago}`, tone: "muted" };
    case "discarded":
      return { label: `discarded · ${ago}`, tone: "muted" };
    default:
      return s.stale
        ? { label: `stale · last run ${ago}`, tone: "attention" }
        : { label: `fresh · ${ago}`, tone: "healthy" };
  }
}

function agoShort(epochSeconds: number, nowMs: number): string {
  const s = Math.max(0, Math.floor(nowMs / 1000) - epochSeconds);
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86400)}d`;
}

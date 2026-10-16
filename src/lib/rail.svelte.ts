// The rail's counts that come from the backend: sources with something
// waiting on Ingest, and what the knowledge base's checks found. Read on a
// change of workspace and after the events that move them; Ingest and the
// workspace settings screen hand in what they read.
import { api } from "./api";
import { app } from "./app.svelte";
import { coalesce, singleFlight } from "./refresh";

/** Scans fire in bursts; read the counts once, after this quiet spell. */
const DEBOUNCE_MS = 1500;
/** The findings come from checks that run now and then; look again this
 *  often in case one ran with the settings screen closed. */
const FINDINGS_EVERY_MS = 5 * 60 * 1000;

class RailStore {
  ingestWaiting = $state(0);
  findings = $state(0);

  private subscribed = false;
  private pendingFindings = false;

  async subscribe() {
    if (this.subscribed) return;
    this.subscribed = true;
    // An ingest, a drift run or the scan's lint files its findings and then
    // writes files: the index event is the one that follows.
    await api.onIndexUpdated(() => this.queue(true));
    await api.onReviewChanged(() => this.queue(true));
    setInterval(() => void this.refreshFindings(), FINDINGS_EVERY_MS);
    $effect.root(() => {
      $effect(() => {
        void app.workspace?.id;
        void this.refreshIngest();
        void this.refreshFindings();
      });
    });
  }

  private queue(findings: boolean) {
    this.pendingFindings ||= findings;
    this.queueRead();
  }

  private readonly queueRead = coalesce(() => {
    void this.refreshIngest();
    if (this.pendingFindings) void this.refreshFindings();
    this.pendingFindings = false;
  }, DEBOUNCE_MS);

  /** Optional reads: a failure keeps the last count. One of each at a time
   *  (see `singleFlight`). */
  readonly refreshIngest = singleFlight(async () => {
    if (!app.workspace) return;
    const ws = app.workspace.id;
    const o = await api.ingestOverview().catch(() => null);
    if (o && ws === app.workspace?.id) this.setIngest(o.waiting ?? 0);
  });

  readonly refreshFindings = singleFlight(async () => {
    if (!app.workspace) return;
    const ws = app.workspace.id;
    const found = await api.knowledgeFindings().catch(() => null);
    if (found && ws === app.workspace?.id) this.setFindings(found.length);
  });

  setIngest(n: number) {
    this.ingestWaiting = n;
  }

  setFindings(n: number) {
    this.findings = n;
  }
}

export const rail = new RailStore();

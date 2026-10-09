// The rail's counts that come from the backend: sources with something
// waiting on Ingest, and what the wiki's checks found on Team (Team adds
// the unread team inbox messages itself). Read on a change of team and
// after the events that move them; Ingest and Team hand in what they read.
import { api } from "./api";
import { app } from "./app.svelte";
import { scope } from "./scope.svelte";
import { coalesce, singleFlight } from "./refresh";

/** Scans fire in bursts; read the counts once, after this quiet spell. */
const DEBOUNCE_MS = 1500;
/** Team's findings come from a sweep that runs now and then; look again
 *  this often in case one ran with Team closed. */
const FINDINGS_EVERY_MS = 5 * 60 * 1000;

class RailStore {
  ingestWaiting = $state(0);
  findings = $state(0);

  private subscribed = false;
  private pendingFindings = false;

  async subscribe() {
    if (this.subscribed) return;
    this.subscribed = true;
    await api.onIndexUpdated(() => this.queue(false));
    await api.onReviewChanged(() => this.queue(true));
    setInterval(() => void this.refreshFindings(), FINDINGS_EVERY_MS);
    $effect.root(() => {
      $effect(() => {
        void scope.team;
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
    const team = scope.team;
    const o = await api.ingestOverview(team).catch(() => null);
    if (o && team === scope.team) this.setIngest(o.waiting ?? 0);
  });

  readonly refreshFindings = singleFlight(async () => {
    if (!app.workspace) return;
    const team = scope.team;
    const o = await api.teamOverview(team).catch(() => null);
    if (o && team === scope.team) this.setFindings(o.findings?.length ?? 0);
  });

  setIngest(n: number) {
    this.ingestWaiting = n;
  }

  setFindings(n: number) {
    this.findings = n;
  }
}

export const rail = new RailStore();

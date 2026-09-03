// ken-pipeline task 4.1 frontend store: the `kenPipeline` flag, the run
// queue (via `pipeline_runs` + the `pipeline-runs` event), the daily
// digest, pipeline-board filters, and every kickoff/advance/block/unblock/
// sign-off/idea/artifact mutation. Mirrors `families.svelte.ts`/
// `memory.svelte.ts`'s shape: flag-gated `init()`, an event subscription
// that updates `$state`, thin wrappers over `api` calls.
//
// Board DATA (tasks/pipelines/pipelineFields/pipelineLaneCounts/blocked) is
// deliberately NOT duplicated here — `pipeline_board`/`board_get`/
// `board-state` all return the identical `BoardStateDto` shape (task 2.1's
// "byte-identical computation, not byte-identical payload"), so
// `tasksStore.board` (already live via the existing `board-state`
// subscription) is the one source of truth. This store only adds what
// `board-state` doesn't carry: the run ledger, the digest, and pipeline
// definition validation issues (`pipeline_list_defs`'s `issues`, not part
// of `BoardStateDto.pipelines`).
import {
  api,
  type Pipeline,
  type PipelineAdvanceDto,
  type PipelineAdvanceOutcome,
  type PipelineArtifactManifestDto,
  type PipelineBlockRequest,
  type PipelineDefDto,
  type PipelineDigestDto,
  type PipelineIdeaOutcome,
  type PipelineKickoffOutcome,
  type PipelineLane,
  type PipelineRunQueue,
  type PipelineSignoffDecision,
  type PipelineSignoffDto,
  type PipelineUnblockRequest,
  type Task,
  type TicketFields,
} from "./api";
import { tasksStore } from "./tasks.svelte";

const EMPTY_QUEUE: PipelineRunQueue = { running: [], queued: [], blocked: [], stale: [], waitingHuman: [] };

/** Block-state filter for the pipeline board's own filter row (task 4.5).
 *  Mirrors `ken_core::tasks::BlockedFilter`'s four meaningful states plus
 *  `""` for "don't care" (matching `TaskFilters`'s `""`-means-unset
 *  convention elsewhere in this codebase). */
export type PipelineBlockFilterMode = "" | "blocked" | "notBlocked" | "newlyUnblocked" | "byTicket";

export interface PipelineFilters {
  project: string;
  lane: string;
  model: string;
  assignee: string;
  blockMode: PipelineBlockFilterMode;
  /** Only read when `blockMode === "byTicket"`. */
  blockByTicketId: string;
}

function emptyPipelineFilters(): PipelineFilters {
  return { project: "", lane: "", model: "", assignee: "", blockMode: "", blockByTicketId: "" };
}

/** A placeholder project symbol (initials), rendered top-left on every card
 *  per D12. **Judgment call / deferred, flagged rather than invented**: the
 *  real per-project `symbol` (ken-core task 1.17, `ProjectConfig.extra`)
 *  has no Tauri command exposing it to the frontend yet — `ProjectInfo` in
 *  `api.ts` carries `id`/`name`/`root`/`excluded`/`ingestRunner` only, and
 *  no `WorkspaceOverview`/`ProjectInfo` field round-trips `symbol` or
 *  `color`. Rather than inventing a new command (out of this session's
 *  touch scope — `src-tauri` is read-only here), this derives a stable
 *  2-character placeholder from the project name so the "readable at a
 *  glance" requirement still has *something* to render; swap this for the
 *  real field once a command exposes it. */
export function projectSymbol(project: string): string {
  const t = project.trim();
  if (!t) return "?";
  const words = t.split(/[\s_-]+/).filter(Boolean);
  if (words.length >= 2) return (words[0][0] + words[1][0]).toUpperCase();
  return t.slice(0, 2).toUpperCase();
}

/** Client-side mirror of `pipeline::matches_block_filter` (ken-core, read-
 *  only this session) — evaluated over the same `TicketFields` the board
 *  already carries, so filtering the pipeline board needs no extra round
 *  trip. "Blocked" means *carries block evidence* (`blockedBy`/
 *  `blockReason`), not "sits in the blocked lane" — see the Rust doc
 *  comment this mirrors for why those can disagree on a hand-edited file. */
export function matchesBlockFilter(fields: TicketFields, mode: PipelineBlockFilterMode, byTicketId: string): boolean {
  const blocked = fields.blockedBy.length > 0 || fields.blockReason !== null;
  switch (mode) {
    case "":
      return true;
    case "blocked":
      return blocked;
    case "notBlocked":
      return !blocked;
    case "newlyUnblocked":
      return !blocked && fields.returnLane !== null;
    case "byTicket":
      return fields.blockedBy.some((b) => b.toLowerCase() === byTicketId.trim().toLowerCase());
  }
}

/** One idea ticket plus the lane/pipeline it was resolved against — the
 *  Ideas view's (task 4.12, D16) unit of display. */
export interface IdeaEntry {
  task: Task;
  lane: PipelineLane;
  pipeline: Pipeline;
}

class PipelineStore {
  /** Whether the `kenPipeline` flag resolves on (requires `workspace` +
   *  `kenTasks` — enforced server-side, `listFeatures`'s `effective`
   *  already folds the dependency in). Gates the Tasks tab's Pipeline view
   *  entirely; flag off ⇒ the classic board renders exactly as before
   *  (spec: "Flag-scoped activation"). */
  enabled = $state(false);

  /** Every loaded pipeline definition plus `validate_pipeline`'s findings
   *  — `BoardStateDto.pipelines` doesn't carry `issues`, only
   *  `pipeline_list_defs` does, so a bad definition file (dangling
   *  `on_pass`, two blocked lanes, …) doesn't get silently swallowed by
   *  the board view (task 4's "pipelineIssues ... must be visible, not
   *  swallowed"). */
  defs = $state<PipelineDefDto[]>([]);

  runs = $state<PipelineRunQueue>(EMPTY_QUEUE);
  runTrayOpen = $state(false);

  digest = $state<PipelineDigestDto | null>(null);
  digestLoading = $state(false);
  digestError = $state<string | null>(null);
  digestOpen = $state(false);

  filters = $state<PipelineFilters>(emptyPipelineFilters());

  /** Ideas sub-tab toggle (task 4.12, D16) — a filtered view WITHIN the
   *  existing Pipeline tab rather than a new nav-rail screen, same
   *  precedent as the Main/Daily/Pipeline segmented control `TasksScreen`
   *  already uses one level up. `false` = the normal lanes board. */
  showIdeas = $state(false);

  /** Kickoff confirmation dialog state (task 4.6) — the only path that can
   *  ever start a lane's agent. `outcome` is the last `pipelineKickoff`
   *  response; the dialog renders from it directly rather than a second
   *  derived shape. */
  kickoffTicketId = $state<string | null>(null);
  kickoffOutcome = $state<PipelineKickoffOutcome | null>(null);
  kickoffBusy = $state(false);
  kickoffError = $state<string | null>(null);

  /** Block/unblock dialog target (task 4.4). `null` = closed. */
  blockTicketId = $state<string | null>(null);
  blockBusy = $state(false);
  blockError = $state<string | null>(null);

  /** Sign-off dialog target (task 4.8, D11 — human lane only). */
  signoffTicketId = $state<string | null>(null);
  signoffBusy = $state(false);
  signoffError = $state<string | null>(null);

  /** Artifact viewer target (task 4.10). */
  artifactsTicketId = $state<string | null>(null);
  artifactsManifest = $state<PipelineArtifactManifestDto | null>(null);
  artifactsLoading = $state(false);
  artifactsError = $state<string | null>(null);

  /** Generic "report outcome" (pass/fail) busy state, keyed by ticket id —
   *  the manual stand-in for an external agent's `pipeline_advance` call
   *  while ken-mcp's pull runner isn't something this UI drives directly. */
  advancingId = $state<string | null>(null);
  advanceError = $state<string | null>(null);

  private initDone = false;
  private unlistenRuns: (() => void) | null = null;

  async init() {
    if (this.initDone) return;
    this.initDone = true;
    this.enabled = await this.checkEnabled();
    if (!this.enabled) return;
    this.unlistenRuns = await api.onPipelineRuns((q) => {
      this.runs = q;
    });
    await Promise.all([this.refreshRuns(), this.refreshDefs()]);
  }

  private async checkEnabled(): Promise<boolean> {
    const features = await api.listFeatures().catch(() => []);
    return features.find((f) => f.name === "kenPipeline")?.effective ?? false;
  }

  async refreshDefs() {
    if (!this.enabled) return;
    this.defs = await api.pipelineListDefs().catch(() => this.defs);
  }

  async refreshRuns() {
    if (!this.enabled) return;
    this.runs = await api.pipelineRuns().catch(() => this.runs);
  }

  async refreshDigest(day?: string) {
    if (!this.enabled) return;
    this.digestLoading = true;
    this.digestError = null;
    try {
      this.digest = await api.pipelineDigest(day);
    } catch (e) {
      this.digestError = String(e);
    } finally {
      this.digestLoading = false;
    }
  }

  // ── Board data (reads through `tasksStore.board` — see file header) ────

  get pipelines() {
    return tasksStore.board.pipelines;
  }

  fieldsFor(taskId: string): TicketFields | null {
    return tasksStore.board.pipelineFields[taskId] ?? null;
  }

  laneCountsFor(pipelineId: string): Record<string, number> {
    return tasksStore.board.pipelineLaneCounts[pipelineId] ?? {};
  }

  /** Every pipeline ticket on the board (`task.lane !== null`) — the
   *  universe the pipeline view's filter chips (project/model/assignee)
   *  are built from, mirroring `tasksStore.projects`'s "distinct values
   *  present" shape. */
  get pipelineTasks(): Task[] {
    return tasksStore.board.tasks.filter((t) => t.lane !== null);
  }

  get projects(): string[] {
    return [...new Set(this.pipelineTasks.map((t) => t.project).filter((p) => p.length > 0))].sort((a, b) =>
      a.localeCompare(b),
    );
  }

  get models(): string[] {
    const out = new Set<string>();
    for (const t of this.pipelineTasks) {
      const f = this.fieldsFor(t.id);
      if (f?.model) out.add(f.model);
    }
    for (const p of this.pipelines) for (const l of p.lanes) if (l.model) out.add(l.model);
    return [...out].sort((a, b) => a.localeCompare(b));
  }

  private matchesFilters(task: Task, fields: TicketFields): boolean {
    const f = this.filters;
    if (f.project && task.project.toLowerCase() !== f.project.toLowerCase()) return false;
    if (f.lane && task.lane?.toLowerCase() !== f.lane.toLowerCase()) return false;
    const effectiveModel = fields.model ?? "";
    if (f.model && effectiveModel.toLowerCase() !== f.model.toLowerCase()) return false;
    if (f.assignee && task.assignee.toLowerCase() !== f.assignee.toLowerCase()) return false;
    if (!matchesBlockFilter(fields, f.blockMode, f.blockByTicketId)) return false;
    return true;
  }

  clearFilters() {
    this.filters = emptyPipelineFilters();
  }

  get filtersActive(): boolean {
    const f = this.filters;
    return !!(f.project || f.lane || f.model || f.assignee || f.blockMode);
  }

  /** Tickets for one lane of one pipeline, filtered — the pipeline board's
   *  own per-column list. Scoped by BOTH `pipeline.id` and `lane.id`
   *  because lane ids are only unique within a pipeline (two loaded
   *  pipelines could both declare a `todo` lane). */
  tasksForLane(pipelineId: string, laneId: string): Task[] {
    return this.pipelineTasks.filter((t) => {
      const fields = this.fieldsFor(t.id);
      if (!fields || fields.pipeline?.toLowerCase() !== pipelineId.toLowerCase()) return false;
      if (t.lane?.toLowerCase() !== laneId.toLowerCase()) return false;
      return this.matchesFilters(t, fields);
    });
  }

  // ── Ideas surface (task 4.12, D16) ──────────────────────────────────────

  /** The lane a generated idea auto-lands in, resolved from the pipeline's
   *  OWN definition rather than assumed by the view. **Finding**: this
   *  task's brief asked to check whether `Lane.generative` is a better
   *  signal than the literal id — it is not. Per ken-core's own doc
   *  comment ("D7: this lane files new ideas back into the ideas lane"),
   *  `generative` marks the lane that PRODUCES ideas (the Documentation
   *  lane in the shipped default pipeline), not the lane ideas land IN.
   *  The backend has no other structural marker for the landing lane
   *  either: `compose_idea_ticket` (crates/ken-core/src/pipeline.rs, read-
   *  only this session) hardcodes `const IDEA_LANE: &str = "ideas"` and
   *  refuses to build a ticket at all when `pipeline.lane("ideas")` is
   *  absent. So resolving by id here mirrors the backend's own convention
   *  — a well-known reserved lane id, the same way `blocked_lane()`/
   *  `human_lane()` are resolved by a boolean flag instead — rather than
   *  inventing a new one. `!lane.blocked` is a defensive sanity check
   *  matching D5 (a pipeline may declare at most one `blocked: true` lane,
   *  and the ideas lane is never meant to be it). */
  ideaLaneFor(pipeline: Pipeline): PipelineLane | null {
    const lane = pipeline.lanes.find((l) => l.id.toLowerCase() === "ideas");
    return lane && !lane.blocked ? lane : null;
  }

  /** Every idea ticket across every loaded pipeline, unfiltered — the true
   *  total the toolbar badge counts (task 4.12: "an unobtrusive count so
   *  ideas are discoverable without nagging" needs the real number, not
   *  whatever the board's own filters currently narrow it to). */
  get allIdeas(): IdeaEntry[] {
    const out: IdeaEntry[] = [];
    for (const pipeline of this.pipelines) {
      const lane = this.ideaLaneFor(pipeline);
      if (!lane) continue;
      for (const task of this.pipelineTasks) {
        const fields = this.fieldsFor(task.id);
        if (!fields || fields.pipeline?.toLowerCase() !== pipeline.id.toLowerCase()) continue;
        if (task.lane?.toLowerCase() !== lane.id.toLowerCase()) continue;
        out.push({ task, lane, pipeline });
      }
    }
    return out;
  }

  get ideaCount(): number {
    return this.allIdeas.length;
  }

  /** The Ideas view's own list: `allIdeas` narrowed by the project filter
   *  only, newest first. The board toolbar's other filters (lane/model/
   *  assignee/block state) describe work in progress, which an inert idea
   *  never has — D7: "nothing an idea does can start a run" — so they're
   *  not meaningful here and are deliberately not applied. */
  get ideasForView(): IdeaEntry[] {
    const project = this.filters.project.trim().toLowerCase();
    return this.allIdeas
      .filter((e) => !project || e.task.project.toLowerCase() === project)
      .sort((a, b) => b.task.created.localeCompare(a.task.created));
  }

  /** Promote = advance the idea along the lane's own `on_pass` edge (D16:
   *  "the lane's existing `on_pass` edge, so this needs no new transition
   *  machinery" — `ideas → backlog` in the shipped default). Thin wrapper
   *  over `advance()` so the Ideas view's intent reads clearly at the call
   *  site; it is the ONLY thing that moves an idea off this lane. */
  async promoteIdea(ticketId: string, report: string): Promise<PipelineAdvanceDto> {
    return this.advance(ticketId, "pass", report);
  }

  // ── Kickoff (task 4.6, D3) ──────────────────────────────────────────────

  /** Ask whether `ticketId` may start. Always the FIRST call
   *  (`confirmed: false`) — a `queued`/`refused` verdict resolves
   *  immediately; a `needsConfirm` verdict opens the dialog and waits for
   *  `confirmKickoff()`. Never skipped: this is the only path in this UI
   *  that can start a lane's agent, and it always goes through the
   *  backend's `admit()`. */
  async openKickoff(ticketId: string) {
    this.kickoffTicketId = ticketId;
    this.kickoffOutcome = null;
    this.kickoffError = null;
    this.kickoffBusy = true;
    try {
      const outcome = await api.pipelineKickoff(ticketId, false);
      this.kickoffOutcome = outcome;
      if (outcome.kind !== "needsConfirm") {
        // `queued` or `refused` — nothing to confirm, the verdict IS the
        // answer. Leave the dialog open just long enough to show it (the
        // component reads `kickoffOutcome.kind` to decide what to render),
        // and refresh the live run queue for a `queued` verdict.
        if (outcome.kind === "queued") await this.refreshRuns();
      }
    } catch (e) {
      this.kickoffError = String(e);
    } finally {
      this.kickoffBusy = false;
    }
  }

  /** Accept the confirmation dialog — re-calls `pipeline_kickoff` with
   *  `confirmed: true`. Only meaningful while `kickoffOutcome.kind ===
   *  "needsConfirm"`. */
  async confirmKickoff() {
    if (!this.kickoffTicketId || this.kickoffOutcome?.kind !== "needsConfirm") return;
    this.kickoffBusy = true;
    this.kickoffError = null;
    try {
      const outcome = await api.pipelineKickoff(this.kickoffTicketId, true);
      this.kickoffOutcome = outcome;
      await this.refreshRuns();
    } catch (e) {
      this.kickoffError = String(e);
    } finally {
      this.kickoffBusy = false;
    }
  }

  closeKickoff() {
    this.kickoffTicketId = null;
    this.kickoffOutcome = null;
    this.kickoffError = null;
    this.kickoffBusy = false;
  }

  // ── Advance (report an outcome — task 5.4's manual stand-in) ────────────

  /** Record `outcome` for `ticketId` — resolves `on_pass`/`on_fail`, applies
   *  bounce accounting (a cap breach blocks the ticket instead, D4), and
   *  closes whichever run is open. Used both for a holding lane's plain
   *  "move forward" action (no agent involved, `on_pass` only) and for
   *  manually recording an agent lane's result when driving the board by
   *  hand (D6: manual kickoff ships before auto-transitions — there is no
   *  Ken-side process to detect completion in v1's `mcp` pull runner). */
  async advance(ticketId: string, outcome: PipelineAdvanceOutcome, report: string, artifacts?: string[]): Promise<PipelineAdvanceDto> {
    this.advancingId = ticketId;
    this.advanceError = null;
    try {
      const dto = await api.pipelineAdvance(ticketId, outcome, report, artifacts);
      await this.refreshRuns();
      return dto;
    } catch (e) {
      this.advanceError = String(e);
      throw e;
    } finally {
      this.advancingId = null;
    }
  }

  async cancelRun(runId: string) {
    await api.pipelineCancelRun(runId);
    await this.refreshRuns();
  }

  // ── Block / unblock (task 4.4, D5) ──────────────────────────────────────

  openBlockDialog(ticketId: string) {
    this.blockTicketId = ticketId;
    this.blockError = null;
  }

  closeBlockDialog() {
    this.blockTicketId = null;
    this.blockError = null;
    this.blockBusy = false;
  }

  async block(ticketId: string, request: PipelineBlockRequest) {
    this.blockBusy = true;
    this.blockError = null;
    try {
      const task = await api.pipelineBlock(ticketId, request);
      this.closeBlockDialog();
      return task;
    } catch (e) {
      this.blockError = String(e);
      throw e;
    } finally {
      this.blockBusy = false;
    }
  }

  async unblock(ticketId: string, request: PipelineUnblockRequest) {
    this.blockBusy = true;
    this.blockError = null;
    try {
      const task = await api.pipelineUnblock(ticketId, request);
      this.closeBlockDialog();
      return task;
    } catch (e) {
      this.blockError = String(e);
      throw e;
    } finally {
      this.blockBusy = false;
    }
  }

  // ── Sign-off (task 4.8, D11) ─────────────────────────────────────────────

  openSignoff(ticketId: string) {
    this.signoffTicketId = ticketId;
    this.signoffError = null;
  }

  closeSignoff() {
    this.signoffTicketId = null;
    this.signoffError = null;
    this.signoffBusy = false;
  }

  /** Does NOT auto-close the dialog on success — `accept with comments`
   *  must show the resulting child ticket (task 4.8: "shown in the
   *  confirmation") before the dialog goes away, so `SignoffDialog.svelte`
   *  closes itself once the human has seen the result. */
  async signoff(ticketId: string, decision: PipelineSignoffDecision, comment?: string): Promise<PipelineSignoffDto> {
    this.signoffBusy = true;
    this.signoffError = null;
    try {
      const dto = await api.pipelineSignoff(ticketId, decision, comment);
      await this.refreshRuns();
      return dto;
    } catch (e) {
      this.signoffError = String(e);
      throw e;
    } finally {
      this.signoffBusy = false;
    }
  }

  // ── Ideas (D7 — normally the documentation lane's own action) ──────────

  async proposeIdea(ticketId: string, title: string, body: string): Promise<PipelineIdeaOutcome> {
    return api.pipelineProposeIdea(ticketId, title, body);
  }

  // ── Artifacts (task 4.10, D9) ────────────────────────────────────────────

  async openArtifacts(ticketId: string) {
    this.artifactsTicketId = ticketId;
    this.artifactsError = null;
    this.artifactsLoading = true;
    try {
      this.artifactsManifest = await api.pipelineArtifacts(ticketId);
    } catch (e) {
      this.artifactsError = String(e);
    } finally {
      this.artifactsLoading = false;
    }
  }

  closeArtifacts() {
    this.artifactsTicketId = null;
    this.artifactsManifest = null;
    this.artifactsError = null;
  }

  /** Lazily create the folder + manifest on first use, or append `filename`
   *  to an already-existing manifest (idempotent — `pipeline_register_
   *  artifact` itself dedupes). Callers that want the viewer to reflect the
   *  write should re-call `openArtifacts` afterwards; this only returns the
   *  fresh manifest, it doesn't assume it's the one currently open. */
  async registerArtifact(ticketId: string, filename: string): Promise<PipelineArtifactManifestDto> {
    return api.pipelineRegisterArtifact(ticketId, filename);
  }

  async pruneArtifacts(ticketId: string) {
    await api.pipelinePruneArtifacts(ticketId);
    if (this.artifactsTicketId === ticketId) this.artifactsManifest = null;
  }

  /** Settings-independent teardown for tests; production code never calls
   *  this (the store lives for the app's lifetime). */
  dispose() {
    this.unlistenRuns?.();
    this.unlistenRuns = null;
  }
}

export const pipelineStore = new PipelineStore();

// Your day: the team's tickets assigned to me, escalations addressed to me,
// my tasks, the team digest and the team's index health. Refreshes on `day-changed`, the team-digest
// events, `index-updated` (those two coalesced), a change of team, and a new
// local day: at midnight, or on wake or focus when the timer ran late.
import {
  api,
  memberLeaf,
  type DayState,
  type DayTask,
  type DayTaskInput,
  type DayTaskPatch,
  type IndexHealth,
  type TeamDigest,
} from "./api";
import { app } from "./app.svelte";
import { scope } from "./scope.svelte";
import { localToday, orderTasks } from "./day";
import { coalesce, singleFlight } from "./refresh";
import { toast } from "./toast.svelte";

/** What the task panel shows: a new task (optionally pre-linked) or one
 *  existing task by id. */
export type DayPanel =
  | { mode: "new"; links: string[] }
  /** `task`: the copy it was opened with, for a task not in today's list
   *  (done earlier, or recurring on another day); `ticket`: where to read
   *  it again from. */
  | { mode: "edit"; id: string; task?: DayTask; ticket?: TicketRef };

export type TicketRef = { projectId: string; ticketId: string };

/** How long `day-changed` and `index-updated` wait for more of the same
 *  before reading again. */
export const REFRESH_DEBOUNCE_MS = 250;

const PATCH_KEYS = ["title", "target", "description", "repeat", "links", "state"] as const;

function same(a: unknown, b: unknown): boolean {
  return a === b || JSON.stringify(a) === JSON.stringify(b);
}

class DayStore {
  state = $state<DayState | null>(null);
  loadError = $state<string | null>(null);

  digest = $state<TeamDigest | null>(null);
  generating = $state(false);
  digestError = $state<string | null>(null);
  /** Optimistic until claude_doctor answers. */
  claudeFound = $state(true);

  health = $state<IndexHealth | null>(null);

  today = $state(localToday());
  panel = $state<DayPanel | null>(null);
  /** The inbox task an Accept/Not mine/Reply is running for, by id. */
  busy = $state<string | null>(null);

  private initDone = false;
  private midnightTimer: ReturnType<typeof setTimeout> | undefined;
  private pendingState = false;
  private pendingHealth = false;

  /** Other, in display order. */
  get tasks(): DayTask[] {
    return orderTasks(this.state?.tasks ?? [], this.today);
  }

  get tickets() {
    return this.state?.tickets ?? [];
  }

  /** Open escalations addressed to me, from every team repo. */
  get escalations() {
    return this.state?.escalations ?? [];
  }

  /** The task open in the panel, when editing: today's copy, else the one
   *  it was opened with. */
  get panelTask(): DayTask | null {
    const p = this.panel;
    if (!p || p.mode !== "edit") return null;
    return this.state?.tasks.find((t) => t.id === p.id) ?? p.task ?? null;
  }

  private get team(): string | null {
    return scope.team;
  }

  async init() {
    if (this.initDone) return;
    this.initDone = true;
    await api.onDayChanged(() => this.queueRefresh({ state: true }));
    // Whoever started it (the morning schedule, opening the workspace, the
    // button), a digest for this team shows as being written.
    await api.onTeamDigestGenerating((team) => {
      if (team === this.team) {
        this.generating = true;
        this.digestError = null;
      }
    });
    await api.onTeamDigestUpdated(() => {
      // The event carries a digest but not which team it was for; read
      // ours back rather than show another team's.
      void this.refreshDigest();
    });
    await api.onTeamDigestError((team, message) => {
      if (team !== this.team) return;
      this.generating = false;
      this.digestError = message;
    });
    await api.onIndexUpdated(() => this.queueRefresh({ state: true, health: true }));
    // A map build reads the files waiting for it: the count on Home drops.
    await api.onKnowledgeModelState((ev) => {
      if (ev.state !== "building") this.queueRefresh({ health: true });
    });
    // A change of team (or workspace) reads everything again.
    $effect.root(() => {
      $effect(() => {
        void scope.team;
        void app.workspace?.id;
        void this.refresh();
      });
    });
    this.scheduleMidnight();
    // Sleep pauses the midnight timer; look at the date again on wake.
    if (typeof document !== "undefined") {
      document.addEventListener("visibilitychange", () => {
        if (document.visibilityState === "visible") this.checkDay();
      });
    }
    if (typeof window !== "undefined") window.addEventListener("focus", () => this.checkDay());
    this.claudeFound = (await api.claudeDoctor().catch(() => null))?.found ?? false;
  }

  private scheduleMidnight() {
    clearTimeout(this.midnightTimer);
    const now = new Date();
    const next = new Date(now.getFullYear(), now.getMonth(), now.getDate() + 1, 0, 0, 5);
    this.midnightTimer = setTimeout(() => this.checkDay(), next.getTime() - now.getTime());
  }

  /** A new local day: done tasks leave the list, recurring ones come back. */
  checkDay() {
    const now = localToday();
    if (now === this.today) return;
    this.today = now;
    void this.refresh();
    this.scheduleMidnight();
  }

  /** Coalesce bursts of events into one read, after a quiet spell. */
  private queueRefresh(what: { state?: boolean; health?: boolean }) {
    if (what.state) this.pendingState = true;
    if (what.health) this.pendingHealth = true;
    this.queueRead();
  }

  private readonly queueRead = coalesce(() => {
    const state = this.pendingState;
    const health = this.pendingHealth;
    this.pendingState = this.pendingHealth = false;
    if (state) void this.refreshState();
    if (health) void this.refreshHealth();
  }, REFRESH_DEBOUNCE_MS);

  async refresh() {
    await Promise.all([this.refreshState(), this.refreshDigest(), this.refreshHealth()]);
  }

  /** Read Your day again; see `singleFlight` for how calls during a read
   *  are handled. */
  readonly refreshState = singleFlight(async () => {
    if (!app.workspace) {
      this.state = null;
      return;
    }
    const team = this.team;
    const ws = app.workspace.id;
    try {
      const next = await api.dayState(team);
      if (team !== this.team || ws !== app.workspace?.id) return;
      this.state = next;
      this.loadError = null;
      this.today = localToday();
      void this.resolvePanel();
    } catch (e) {
      if (team === this.team && ws === app.workspace?.id) this.loadError = String(e);
    }
  });

  readonly refreshDigest = singleFlight(async () => {
    if (!app.workspace) {
      this.digest = null;
      return;
    }
    const team = this.team;
    const ws = app.workspace.id;
    const [d, writing] = await Promise.all([
      api.teamDigest(team).catch(() => null),
      api.teamDigestWriting(team).catch(() => false),
    ]);
    if (team !== this.team || ws !== app.workspace?.id) return;
    this.digest = d;
    this.generating = writing;
  });

  readonly refreshHealth = singleFlight(async () => {
    if (!app.workspace) {
      this.health = null;
      return;
    }
    const team = this.team;
    const ws = app.workspace.id;
    const h = await api.indexHealth(team).catch(() => null);
    if (team === this.team && ws === app.workspace?.id) this.health = h;
  });

  /** "Write it now". */
  async writeDigest() {
    this.digestError = null;
    this.generating = true;
    try {
      await api.refreshTeamDigest(this.team);
    } catch (e) {
      this.generating = false;
      this.digestError = String(e);
      toast.error("Could not write the digest", e);
    }
  }

  // ── Tasks ───────────────────────────────────────────────────────────

  openNew(links: string[] = []) {
    this.panel = { mode: "new", links };
    app.screen = "home";
  }

  /** Open a task's panel. From outside today's list, pass the task and
   *  the ticket it was listed under. */
  openTask(id: string, from?: { task: DayTask; ticket?: TicketRef }) {
    this.panel = { mode: "edit", id, task: from?.task, ticket: from?.ticket };
  }

  closePanel() {
    this.panel = null;
  }

  /** Put a task's new copy wherever it shows: today's list (added there
   *  only with `add`) and the panel. */
  private replace(task: DayTask, add = false) {
    const p = this.panel;
    if (p?.mode === "edit" && p.id === task.id && p.task) this.panel = { ...p, task };
    if (!this.state) return;
    const at = this.state.tasks.findIndex((t) => t.id === task.id);
    if (at === -1 && !add) return;
    const tasks = [...this.state.tasks];
    if (at === -1) tasks.push(task);
    else tasks[at] = task;
    this.state = { ...this.state, tasks };
  }

  async create(input: DayTaskInput): Promise<DayTask> {
    const task = await api.dayTaskCreate(input);
    this.replace(task, true);
    return task;
  }

  /** Patch a task, showing the change at once and taking the backend's copy
   *  back (it computes the next target of a recurring task). */
  async update(id: string, patch: DayTaskPatch): Promise<DayTask | null> {
    const before = this.find(id);
    let local: DayTask | undefined;
    if (before) {
      local = { ...before };
      if (patch.title !== undefined) local.title = patch.title;
      if (patch.target !== undefined) local.target = patch.target;
      if (patch.description !== undefined) local.description = patch.description;
      if (patch.repeat !== undefined) local.repeat = patch.repeat;
      if (patch.links !== undefined) local.links = patch.links;
      if (patch.state !== undefined) {
        local.state = patch.state;
        // Keep a just-done task in today's list until the backend answers.
        if (patch.state === "done") local.updated = localToday();
      }
      this.replace(local);
    }
    try {
      const task = await api.dayTaskUpdate(id, patch);
      this.replace(task);
      return task;
    } catch (e) {
      if (before && local) this.rollback(id, patch, before, local);
      throw e;
    }
  }

  /** Today's copy of a task, else the panel's. */
  private find(id: string): DayTask | undefined {
    const p = this.panel;
    return this.state?.tasks.find((t) => t.id === id) ?? (p?.mode === "edit" && p.id === id ? p.task : undefined);
  }

  /** Undo a failed patch: only the fields it set, and only those still
   *  holding the optimistic value (a newer edit or read wins). */
  private rollback(id: string, patch: DayTaskPatch, before: DayTask, local: DayTask) {
    const current = this.find(id);
    if (!current) return;
    const next: DayTask = { ...current };
    let changed = false;
    for (const k of PATCH_KEYS) {
      if (patch[k] === undefined || !same(current[k], local[k])) continue;
      (next as unknown as Record<string, unknown>)[k] = before[k];
      if (k === "state" && same(current.updated, local.updated)) next.updated = before.updated;
      changed = true;
    }
    if (changed) this.replace(next);
  }

  async toggle(task: DayTask) {
    await this.update(task.id, { state: task.state === "done" ? "open" : "done" });
  }

  async remove(id: string) {
    await api.dayTaskDelete(id);
    if (this.state) this.state = { ...this.state, tasks: this.state.tasks.filter((t) => t.id !== id) };
    if (this.panel?.mode === "edit" && this.panel.id === id) this.panel = null;
  }

  /** The panel is open on a task that is not in today's list: read it
   *  again from its ticket, or close the panel when it is gone. */
  async resolvePanel() {
    const p = this.panel;
    if (!p || p.mode !== "edit" || this.state?.tasks.some((t) => t.id === p.id)) return;
    if (!p.ticket) {
      if (!p.task) this.panel = null;
      return;
    }
    const list = await api.ticketTasks(p.ticket.projectId, p.ticket.ticketId).catch(() => null);
    if (this.panel !== p || list === null) return;
    const task = list.find((t) => t.id === p.id);
    this.panel = task ? { ...p, task } : null;
  }

  // ── A task from a teammate, not yet accepted ────────────────────────

  private async inboxAction(task: DayTask, run: (familyId: string, itemId: string) => Promise<unknown>) {
    if (!task.inbox) return;
    this.busy = task.id;
    try {
      const out = await run(task.inbox.familyId, task.inbox.itemId);
      // Accept answers with the new task; show it before the read lands.
      if (out && typeof out === "object" && "id" in out && this.state) {
        this.state = { ...this.state, tasks: this.state.tasks.filter((t) => t.id !== task.id) };
        this.replace(out as DayTask, true);
      }
      await this.refreshState();
    } finally {
      this.busy = null;
    }
  }

  accept(task: DayTask) {
    return this.inboxAction(task, (f, i) => api.familyAcceptTask(f, i));
  }

  notMine(task: DayTask) {
    return this.inboxAction(task, (f, i) => api.familySetItemStatus(f, i, "archived"));
  }

  reply(task: DayTask, note: string) {
    return this.inboxAction(task, (f, i) => api.familyPushBack(f, i, note));
  }
}

export const day = new DayStore();

/** The repo name for a project id, as the workspace names it. */
export function repoName(projectId: string): string {
  const m = app.workspace?.members.find((x) => x.projectId === projectId);
  if (m) return memberLeaf(m.name);
  return app.registry.find((e) => e.id === projectId)?.name ?? projectId;
}

/** Every workspace member's name, full and leaf, for reading `<repo>/<ID>`
 *  ticket links. */
export function memberNames(): string[] {
  return (app.workspace?.members ?? []).flatMap((m) => [m.name, memberLeaf(m.name)]);
}

/** A ticket link for a ticket in a repo: `<repo>/<ID>` in a workspace, the
 *  bare id otherwise. */
export function ticketLink(projectId: string, ticketId: string): string {
  const m = app.workspace?.members.find((x) => x.projectId === projectId);
  return m ? `${m.name}/${ticketId}` : ticketId;
}

type Where = { line?: number; anchor?: string };

/** Open a file in a repo of the workspace: focus that repo, then Files,
 *  at a line or heading when given. */
export async function openInRepo(projectId: string, relPath: string, where: Where = {}) {
  if (projectId !== app.focused) await app.focusMember(projectId);
  if (where.line || where.anchor) app.openAt(relPath, where);
  else app.openInFiles(relPath);
}

/** Open a member-relative path (`repo/path/to/file.md`), as the team digest
 *  cites them. The longest member name (full or leaf) that prefixes the
 *  path wins; with none, the path opens in the focused repo. */
export async function openMemberPath(path: string, where: Where = {}) {
  const lower = path.toLowerCase();
  const candidates = (app.workspace?.members ?? []).flatMap((m) =>
    m.projectId ? [{ id: m.projectId, name: m.name }, { id: m.projectId, name: memberLeaf(m.name) }] : [],
  );
  const hit = candidates
    .sort((a, b) => b.name.length - a.name.length)
    .find((c) => lower.startsWith(c.name.toLowerCase() + "/"));
  if (hit) await openInRepo(hit.id, path.slice(hit.name.length + 1), where);
  else if (where.line || where.anchor) app.openAt(path, where);
  else app.openInFiles(path);
}

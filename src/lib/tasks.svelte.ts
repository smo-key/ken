// ken-tasks task 4.2/4.3/4.4 frontend store: the `kenTasks` flag, the live
// board (via `board_get` + the `board-state` event), client-side filtering,
// goal CRUD, the daily-board proposal/rollover rituals, and the task-file
// "open in the normal document view" resolution. Mirrors `memory.svelte.ts`/
// `workspaceKg.svelte.ts`'s shape: flag-gated `init()`, an event
// subscription that updates `$state`, thin wrappers over `api` calls.
//
// Filtering is done client-side against the already-live `board.tasks`
// list rather than round-tripping through `task_list` on every filter
// change — `board_get`/`board-state` already carry the full task set, and
// `matchesFilters` below mirrors `ken_core::tasks::matches` exactly (case-
// insensitive text comparisons, tag-any-match, unrecognized status matches
// no column). `api.taskList` still exists as a faithful wrapper (task 4.1
// asks for all 13 command wrappers) even though this store doesn't call it.
import {
  api,
  type BoardStateDto,
  type DailyCandidate,
  type Goal,
  type GoalPatch,
  type Rollover,
  type Task,
  type TaskKind,
  type TaskPatch,
  type TaskStatus,
} from "./api";
import { app } from "./app.svelte";
import { families } from "./families.svelte";

// ken-pipeline task 4.2: a third view, "pipeline" — added here (rather than
// a new top-level nav-rail screen) because the pipeline board extends the
// Phase 7 Kanban (precedent this change follows) and is gated by its own
// `kenPipeline` flag exactly the way Main/Daily are gated by `kenTasks` —
// same tab strip, same "flag off ⇒ renders exactly as today" contract.
export type TaskView = "board" | "daily" | "pipeline";
export type DailyPlanPhase = "idle" | "planning" | "ready" | "error";

// ken-pipeline task 4.1: `BoardStateDto` gained four fields (pipelines,
// pipelineFields, pipelineLaneCounts, blocked) — always present, empty when
// `kenPipeline` is off or the board has no pipeline tickets (src-tauri's own
// "byte-identical computation, not byte-identical payload" note on
// `board_state_dto`). `pipeline.svelte.ts` reads this same store's `board`
// rather than keeping a second live copy.
const EMPTY_BOARD: BoardStateDto = {
  tasks: [],
  goals: [],
  needsAttention: [],
  progress: {},
  pipelines: [],
  pipelineFields: {},
  pipelineLaneCounts: {},
  blocked: [],
};

export interface TaskFilters {
  project: string;
  tag: string;
  /** `""` = no filter, `"__unassigned__"` = claimable pool, else an exact
   *  (case-insensitive) assignee name — mirrors `AssigneeFilter`'s three
   *  states without needing the tagged-union shape for a plain text input. */
  assignee: string;
  kind: "" | TaskKind;
  goal: string;
  /** ken-families task 4.3: `""` = no filter, else a family id — matched
   *  via `families.familyForTask` since no `Task` field names its family
   *  directly (see that function's doc comment). */
  family: string;
  /** ken-pipeline task 4.5 (OPEN-7): hide pipeline tickets from the classic
   *  Main/Daily Kanban. Pipeline tickets project onto the classic board by
   *  design (D2/`maps_to`) — this is purely a "keep the classic board
   *  clean" viewing preference, never a data change. */
  hidePipeline: boolean;
}

function emptyFilters(): TaskFilters {
  return { project: "", tag: "", assignee: "", kind: "", goal: "", family: "", hidePipeline: false };
}

class TasksStore {
  /** Whether the `kenTasks` flag resolves on — gates the sidebar tab
   *  entirely (proposal: "Off ⇒ no tab, no tools, no folders"). */
  enabled = $state(false);

  board = $state<BoardStateDto>(EMPTY_BOARD);
  loading = $state(false);
  loadError = $state<string | null>(null);

  view = $state<TaskView>("board");
  groupByGoal = $state(false);
  attentionOpen = $state(false);

  filters = $state<TaskFilters>(emptyFilters());

  /** Live daily-plan lifecycle, driven by `daily-plan-state`. */
  dailyPhase = $state<DailyPlanPhase>("idle");
  dailyError = $state<string | null>(null);
  dailyCandidates = $state<DailyCandidate[]>([]);
  resolvingKey = $state<string | null>(null);

  /** Unresolved daily tasks from before today (design D5's rollover
   *  ritual). Refreshed on init and after each resolution. */
  rolloverCandidates = $state<Task[]>([]);
  resolvingRolloverId = $state<string | null>(null);

  goalDialogOpen = $state(false);
  editingGoal = $state<Goal | null>(null);

  private initDone = false;
  private unlistenBoard: (() => void) | null = null;
  private unlistenDaily: (() => void) | null = null;

  /** Call once (Tasks tab mount): resolve the flag, subscribe to live
   *  board/daily-plan events, and do the initial load. Cheap even if the
   *  user never opens the tab this session — nothing here runs unless
   *  called. */
  async init() {
    if (this.initDone) return;
    this.initDone = true;
    this.enabled = await this.checkEnabled();
    if (!this.enabled) return;
    this.unlistenBoard = await api.onBoardState((dto) => {
      this.board = dto;
    });
    this.unlistenDaily = await api.onDailyPlanState((ev) => {
      if (ev.state === "planning") {
        this.dailyPhase = "planning";
        this.dailyError = null;
      } else if (ev.state === "ready") {
        this.dailyPhase = "ready";
        this.dailyCandidates = ev.candidates;
        this.dailyError = null;
      } else {
        this.dailyPhase = "error";
        this.dailyError = ev.reason;
      }
    });
    await this.refresh();
    await this.refreshRollover();
  }

  private async checkEnabled(): Promise<boolean> {
    const features = await api.listFeatures().catch(() => []);
    return features.find((f) => f.name === "kenTasks")?.effective ?? false;
  }

  async refresh() {
    if (!this.enabled) return;
    this.loading = true;
    this.loadError = null;
    try {
      this.board = await api.boardGet();
    } catch (e) {
      this.loadError = String(e);
    } finally {
      this.loading = false;
    }
  }

  async refreshRollover() {
    if (!this.enabled) return;
    this.rolloverCandidates = await api.dailyRolloverCandidates().catch(() => []);
  }

  // ── Filtering (client-side, mirrors `ken_core::tasks::matches`) ────────

  /** Distinct project names present on the board, for the project filter's
   *  suggestions. */
  get projects(): string[] {
    return [...new Set(this.board.tasks.map((t) => t.project).filter((p) => p.length > 0))].sort(
      (a, b) => a.localeCompare(b),
    );
  }

  private matchesFilters(task: Task): boolean {
    const f = this.filters;
    if (f.project && task.project.toLowerCase() !== f.project.toLowerCase()) return false;
    if (f.tag && !task.tags.some((t) => t.toLowerCase() === f.tag.toLowerCase())) return false;
    if (f.assignee === "__unassigned__") {
      if (task.assignee.trim() !== "") return false;
    } else if (f.assignee && task.assignee.toLowerCase() !== f.assignee.toLowerCase()) {
      return false;
    }
    if (f.kind && task.kind !== f.kind) return false;
    if (f.goal && (task.goal ?? "").toLowerCase() !== f.goal.toLowerCase()) return false;
    if (f.family && families.familyForTask(task)?.connection.familyId !== f.family) return false;
    if (f.hidePipeline && task.lane !== null) return false;
    return true;
  }

  clearFilters() {
    this.filters = emptyFilters();
  }

  get filtersActive(): boolean {
    const f = this.filters;
    return !!(f.project || f.tag || f.assignee || f.kind || f.goal || f.family || f.hidePipeline);
  }

  /** Filtered tasks for one board (`"main"` | `"daily"`), still unsorted by
   *  status — `TasksScreen.svelte`'s `kanban` snippet groups these into the
   *  five status columns (`backlog` leftmost, D7's intake column) itself, so
   *  it can reuse the same grouping for both the flat board and each
   *  group-by-goal bucket. A task with an unrecognized status never lands
   *  in a column (no `TaskStatus` value equals `null`) — it surfaces only
   *  in the needs-attention tray. */
  tasksForBoard(board: "main" | "daily"): Task[] {
    return this.board.tasks.filter((t) => t.board === board && this.matchesFilters(t));
  }

  progressFor(goalId: string): { done: number; total: number } {
    return this.board.progress[goalId] ?? { done: 0, total: 0 };
  }

  goalTitle(goalId: string): string {
    return this.board.goals.find((g) => g.id === goalId)?.title ?? goalId;
  }

  // ── Mutations ────────────────────────────────────────────────────────

  async createTask(title: string, fields?: TaskPatch, projectId?: string) {
    return api.taskCreate(title, undefined, fields, projectId);
  }

  /** Drag-drop → status-only patch (task 4.3: never a whole-task patch). */
  async setStatus(id: string, status: TaskStatus) {
    return api.taskUpdate(id, { status });
  }

  async archiveTask(id: string) {
    return api.taskArchive(id);
  }

  async createGoal(title: string, body?: string) {
    return api.goalCreate(title, body);
  }

  async updateGoal(id: string, patch: GoalPatch) {
    return api.goalUpdate(id, patch);
  }

  openGoalDialog(goal: Goal | null) {
    this.editingGoal = goal;
    this.goalDialogOpen = true;
  }

  closeGoalDialog() {
    this.goalDialogOpen = false;
    this.editingGoal = null;
  }

  // ── Daily board ──────────────────────────────────────────────────────

  async planDaily() {
    this.dailyPhase = "planning";
    this.dailyError = null;
    await api.planDailyTasks();
  }

  async resolveDailyCandidate(key: string, approve: boolean, projectId?: string) {
    this.resolvingKey = key;
    try {
      await api.resolveDailyCandidate(key, approve, projectId);
      this.dailyCandidates = this.dailyCandidates.filter((c) => c.key !== key);
    } finally {
      this.resolvingKey = null;
    }
  }

  async resolveRollover(id: string, choice: Rollover) {
    this.resolvingRolloverId = id;
    try {
      await api.resolveDailyRollover(id, choice);
      this.rolloverCandidates = this.rolloverCandidates.filter((t) => t.id !== id);
    } finally {
      this.resolvingRolloverId = null;
    }
  }

  // ── Open a task's file in the normal document view (task 4.3) ─────────

  /** Why `task` can't be opened right now, or `null` if it can — the same
   *  honest-disabled-state pattern `kenAddress.ts`'s `unopenableReason` uses
   *  for `ken://workspace/...` addresses.
   *
   *  Workspace-home tasks hit the exact same design collision documented in
   *  `kenAddress.ts`: the `.ken-workspace/` pseudo-member is never a
   *  workspace-manifest member, so there is no `focus_project`/`read_file`
   *  path that can open a file inside it today. Per-repo tasks resolve by
   *  matching `task.project` (the display name a per-repo task defaults to
   *  when it omits its own `project:` key — design D2) against an open
   *  workspace member's name; a task whose explicit `project:` frontmatter
   *  was hand-set to something else won't match and falls back to this same
   *  honest-disabled state rather than guessing. */
  openReason(task: Task): string | null {
    if (task.home === "workspace") {
      return "Workspace tasks can't be opened as files yet — see the note in src/lib/kenAddress.ts.";
    }
    // ken-families task 4.3: a family board task's "project" is just the
    // family's display name, not an open workspace member — matching it
    // against `app.members` would either miss (correct, but a misleading
    // "open it" message) or false-positive on a same-named repo, so this
    // is called out on its own rather than falling into the per-repo
    // branch below.
    if (families.familyForTask(task)) {
      return "Family board tasks can't be opened as files yet.";
    }
    const member = this.memberFor(task);
    if (!member?.id) {
      return `No open project named "${task.project}" — open it to view this task's file.`;
    }
    return null;
  }

  private memberFor(task: Task) {
    return app.members.find((m) => m.id && m.name.toLowerCase() === task.project.toLowerCase());
  }

  /** Open a per-repo task's underlying file in the normal document view:
   *  focus its owning member if needed, then open `.ken/tasks/<file>`
   *  (`Task::address_rel_path`'s own relative-path formula, reconstructed
   *  here without a backend round trip). No-ops (silently) when
   *  `openReason` already says it can't be done — callers should check that
   *  first to render a disabled state instead of a dead click. */
  async openFile(task: Task) {
    if (this.openReason(task)) return;
    const member = this.memberFor(task)!;
    if (member.id !== app.focused) await app.focusMember(member.id!);
    const fileName = task.path.split(/[\\/]/).pop();
    if (!fileName) return;
    app.openInFiles(`.ken/tasks/${fileName}`);
  }

  /** Settings-independent teardown for tests; production code never calls
   *  this (the store lives for the app's lifetime). */
  dispose() {
    this.unlistenBoard?.();
    this.unlistenDaily?.();
    this.unlistenBoard = null;
    this.unlistenDaily = null;
  }
}

export const tasksStore = new TasksStore();

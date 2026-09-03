<script lang="ts">
  // ken-tasks tasks 4.2-4.5: the Tasks tab — Kanban board (main + daily),
  // filters, goal grouping with derived n/m progress, goal CRUD, the
  // needs-attention tray, drag-drop status changes, daily proposals +
  // rollover, and archiving done cards.
  import { onMount } from "svelte";
  import { tasksStore } from "../lib/tasks.svelte";
  import { pipelineStore } from "../lib/pipeline.svelte";
  import { families } from "../lib/families.svelte";
  import { app } from "../lib/app.svelte";
  import type { Task, TaskStatus } from "../lib/api";
  import TaskCard from "../tasks/TaskCard.svelte";
  import GoalDialog from "../tasks/GoalDialog.svelte";
  import NeedsAttentionTray from "../tasks/NeedsAttentionTray.svelte";
  import DailyPlanCards from "../tasks/DailyPlanCards.svelte";
  import RolloverPrompt from "../tasks/RolloverPrompt.svelte";
  import PipelineBoard from "../pipeline/PipelineBoard.svelte";
  import Plus from "@lucide/svelte/icons/plus";
  import TriangleAlert from "@lucide/svelte/icons/triangle-alert";
  import Target from "@lucide/svelte/icons/target";
  import X from "@lucide/svelte/icons/x";

  onMount(() => {
    void tasksStore.init();
    void families.init();
    // ken-pipeline task 4: cheap even if the user never opens the Pipeline
    // tab — resolves the `kenPipeline` flag and nothing else. The tab
    // itself only renders once `pipelineStore.enabled` (task 4's "flag off
    // ⇒ the Phase 7 board renders exactly as today").
    void pipelineStore.init();
  });

  // ken-families task 4.3: per-family filter chip. Only families attached
  // to the currently open workspace can possibly have tasks on this board
  // (`task_homes_scan` merges exactly those, see families.svelte.ts's
  // `attachedTo` doc comment) — listing anything else would be a filter
  // option that can never match a task.
  const familyOptions = $derived(families.attachedTo(app.workspace?.id));

  const COLUMNS: { status: TaskStatus; label: string }[] = [
    { status: "backlog", label: "Backlog" },
    { status: "todo", label: "Todo" },
    { status: "doing", label: "Doing" },
    { status: "review", label: "Review" },
    { status: "done", label: "Done" },
  ];

  let dragTaskId = $state<string | null>(null);
  let dragOverStatus = $state<TaskStatus | null>(null);
  let dragError = $state<string | null>(null);

  let addingIn = $state<TaskStatus | null>(null);
  let newTitle = $state("");

  // Autofocus the quick-add input on mount (Svelte action, matching
  // `InlineNameRow.svelte`'s pattern — avoids the a11y-autofocus lint that a
  // plain `autofocus` attribute would trip).
  function focusOnMount(el: HTMLInputElement) {
    el.focus();
  }

  function board() {
    return tasksStore.view === "daily" ? "daily" : "main";
  }

  function onDragStart(id: string) {
    dragTaskId = id;
    dragError = null;
  }
  function onDragEnd() {
    dragTaskId = null;
    dragOverStatus = null;
  }
  function onColDragOver(e: DragEvent, status: TaskStatus) {
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = "move";
    dragOverStatus = status;
  }
  function onColDragLeave(status: TaskStatus) {
    if (dragOverStatus === status) dragOverStatus = null;
  }
  async function onColDrop(e: DragEvent, status: TaskStatus) {
    e.preventDefault();
    dragOverStatus = null;
    const id = dragTaskId;
    dragTaskId = null;
    if (!id) return;
    const task = tasksStore.board.tasks.find((t) => t.id === id);
    if (!task || task.status === status) return;
    if (families.familyForTask(task)) {
      // ken-families task 4.3: `TaskCard.svelte`'s `dragReason` already
      // stops the drag from starting on a family task; this is the same
      // guard applied again at the drop site in case a drag somehow
      // reaches here anyway (see `TaskCard.svelte` for why this write
      // isn't safe yet — no command commits+pushes a family status patch).
      dragError = "Family task status changes aren't synced yet — dragging is disabled for these cards.";
      return;
    }
    try {
      // task 4.3: status-only patch — never a whole-task patch.
      await tasksStore.setStatus(id, status);
    } catch (err) {
      dragError = String(err);
    }
  }

  function beginAdd(status: TaskStatus) {
    addingIn = status;
    newTitle = "";
  }
  async function submitAdd(status: TaskStatus) {
    const title = newTitle.trim();
    addingIn = null;
    if (!title) return;
    try {
      await tasksStore.createTask(title, {
        status,
        board: board(),
        project: tasksStore.filters.project || undefined,
        goal: tasksStore.groupByGoal ? undefined : tasksStore.filters.goal || undefined,
      });
    } catch (err) {
      dragError = String(err);
    }
  }

  /** Goal buckets for group-by-goal mode: every active goal that has at
   *  least one task on this board, plus a synthetic "no goal" bucket last. */
  function goalBuckets(tasksList: Task[]): { id: string | null; title: string }[] {
    const ids = new Set(tasksList.map((t) => t.goal).filter((g): g is string => !!g));
    const buckets: { id: string | null; title: string }[] = tasksStore.board.goals
      .filter((g) => ids.has(g.id))
      .map((g) => ({ id: g.id, title: g.title }));
    // A `goal:` id with no matching goal file still needs a home here so its
    // tasks aren't silently dropped from the grouped view — it also shows in
    // the needs-attention tray.
    for (const id of ids) {
      if (!buckets.some((b) => b.id === id)) buckets.push({ id, title: id });
    }
    if (tasksList.some((t) => !t.goal)) buckets.push({ id: null, title: "No goal" });
    return buckets;
  }
</script>

{#snippet kanban(tasksList: Task[])}
  <div class="board">
    {#each COLUMNS as col (col.status)}
      {@const colTasks = tasksList.filter((t) => t.status === col.status)}
      <div
        class="column"
        role="list"
        aria-label={col.label}
        class:drop-target={dragOverStatus === col.status}
        ondragover={(e) => onColDragOver(e, col.status)}
        ondragleave={() => onColDragLeave(col.status)}
        ondrop={(e) => onColDrop(e, col.status)}
      >
        <div class="col-head">
          <span class="col-title">{col.label}</span>
          <span class="col-count">{colTasks.length}</span>
          {#if col.status === "backlog"}
            <button class="col-add" title="Add task" onclick={() => beginAdd(col.status)}>
              <Plus size={13} strokeWidth={2} />
            </button>
          {/if}
        </div>
        {#if addingIn === col.status}
          <input
            class="add-input"
            placeholder="Task title…"
            bind:value={newTitle}
            use:focusOnMount
            onkeydown={(e) => {
              if (e.key === "Enter") void submitAdd(col.status);
              else if (e.key === "Escape") addingIn = null;
            }}
            onblur={() => void submitAdd(col.status)}
          />
        {/if}
        <div class="col-body">
          {#each colTasks as task (task.id)}
            <TaskCard {task} {onDragStart} {onDragEnd} />
          {/each}
        </div>
      </div>
    {/each}
  </div>
{/snippet}

<div class="screen">
  {#if !tasksStore.enabled}
    <div class="empty-state">
      <p>Ken's task board is off — turn on <span class="mono">kenTasks</span> in Settings → Features to use it.</p>
    </div>
  {:else}
    <div class="main">
      <div class="header">
        <div class="header-top">
          <h1>Tasks</h1>
          <div class="seg" role="tablist" aria-label="Board">
            <button class:on={tasksStore.view === "board"} onclick={() => (tasksStore.view = "board")}>
              Main
            </button>
            <button class:on={tasksStore.view === "daily"} onclick={() => (tasksStore.view = "daily")}>
              Daily
            </button>
            {#if pipelineStore.enabled}
              <button class:on={tasksStore.view === "pipeline"} onclick={() => (tasksStore.view = "pipeline")}>
                Pipeline
              </button>
            {/if}
          </div>
          {#if tasksStore.view !== "pipeline"}
            <label class="check">
              <input type="checkbox" bind:checked={tasksStore.groupByGoal} />
              Group by goal
            </label>
            <button class="btn btn-small btn-ghost" onclick={() => tasksStore.openGoalDialog(null)}>
              <Target size={13} strokeWidth={1.75} /> New goal
            </button>
          {/if}
          <button
            class="btn btn-small attention-toggle"
            class:has-items={tasksStore.board.needsAttention.length > 0}
            onclick={() => (tasksStore.attentionOpen = !tasksStore.attentionOpen)}
          >
            <TriangleAlert size={13} strokeWidth={1.75} />
            {tasksStore.board.needsAttention.length}
          </button>
        </div>

        {#if tasksStore.view !== "pipeline"}
          <div class="filters">
            <select bind:value={tasksStore.filters.project}>
              <option value="">All projects</option>
              {#each tasksStore.projects as p (p)}
                <option value={p}>{p}</option>
              {/each}
            </select>
            <input class="filter-text" placeholder="Tag" bind:value={tasksStore.filters.tag} />
            <input class="filter-text" placeholder="Assignee" bind:value={tasksStore.filters.assignee} />
            <select bind:value={tasksStore.filters.kind}>
              <option value="">Any kind</option>
              <option value="human">Human</option>
              <option value="ai">AI</option>
            </select>
            <select bind:value={tasksStore.filters.goal}>
              <option value="">Any goal</option>
              {#each tasksStore.board.goals as g (g.id)}
                <option value={g.id}>{g.title}</option>
              {/each}
            </select>
            {#if familyOptions.length > 0}
              <select bind:value={tasksStore.filters.family}>
                <option value="">All families</option>
                {#each familyOptions as dto (dto.connection.familyId)}
                  <option value={dto.connection.familyId}>{dto.connection.name}</option>
                {/each}
              </select>
            {/if}
            {#if pipelineStore.enabled}
              <label class="check" title="Pipeline tickets still project onto this column by design (D2) — this only hides them from view.">
                <input type="checkbox" bind:checked={tasksStore.filters.hidePipeline} />
                Hide pipeline tickets
              </label>
            {/if}
            {#if tasksStore.filtersActive}
              <button class="clear-filters" onclick={() => tasksStore.clearFilters()}>
                <X size={12} strokeWidth={2} /> Clear
              </button>
            {/if}
          </div>
        {/if}

        {#if dragError}
          <div class="drag-error">
            <span>{dragError}</span>
            <button onclick={() => (dragError = null)}><X size={12} strokeWidth={2} /></button>
          </div>
        {/if}
      </div>

      {#if tasksStore.view === "pipeline"}
        <PipelineBoard />
      {:else}
        <div class="content">
          {#if tasksStore.view === "daily"}
            <DailyPlanCards />
            <RolloverPrompt />
          {/if}

          {#if tasksStore.loading && tasksStore.board.tasks.length === 0}
            <p class="note">Loading the board…</p>
          {:else if tasksStore.loadError}
            <p class="note warn">Couldn't load the board: {tasksStore.loadError}</p>
          {:else if tasksStore.groupByGoal}
            {#each goalBuckets(tasksStore.tasksForBoard(board())) as bucket (bucket.id ?? "none")}
              {@const bucketTasks = tasksStore
                .tasksForBoard(board())
                .filter((t) => (bucket.id === null ? !t.goal : t.goal === bucket.id))}
              <div class="goal-group">
                <div class="goal-group-head">
                  <Target size={13} strokeWidth={1.75} />
                  <span class="goal-group-title">{bucket.title}</span>
                  {#if bucket.id !== null}
                    {@const progress = tasksStore.progressFor(bucket.id)}
                    <span class="goal-progress">{progress.done}/{progress.total}</span>
                  {/if}
                </div>
                {@render kanban(bucketTasks)}
              </div>
            {/each}
          {:else}
            {@render kanban(tasksStore.tasksForBoard(board()))}
          {/if}
        </div>
      {/if}
    </div>

    {#if tasksStore.attentionOpen}
      <NeedsAttentionTray />
    {/if}

    {#if tasksStore.goalDialogOpen}
      <GoalDialog goal={tasksStore.editingGoal} close={() => tasksStore.closeGoalDialog()} />
    {/if}
  {/if}
</div>

<style>
  .screen {
    flex: 1;
    min-width: 0;
    display: flex;
    min-height: 0;
  }
  .empty-state {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--ink-tertiary);
    font-size: 13px;
  }
  .main {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
  .header {
    flex: none;
    padding: 16px 20px 10px;
    border-bottom: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .header-top {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  h1 {
    margin: 0;
    font-family: var(--font-serif);
    font-size: 20px;
    font-weight: 500;
  }
  .seg {
    display: flex;
    border: 1px solid var(--border-strong);
    border-radius: 8px;
    overflow: hidden;
  }
  .seg button {
    font-size: 12px;
    font-weight: 500;
    padding: 6px 12px;
    border: none;
    background: var(--surface);
    color: var(--ink-secondary);
  }
  .seg button.on {
    background: color-mix(in srgb, var(--accent) 10%, transparent);
    color: var(--accent-deep);
    font-weight: 600;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 12px;
    color: var(--ink-secondary);
  }
  .attention-toggle {
    margin-left: auto;
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }
  .attention-toggle.has-items {
    color: var(--needs-input-text);
    border-color: color-mix(in srgb, var(--needs-input) 45%, var(--border-strong));
  }
  .filters {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
  .filters select,
  .filter-text {
    font-family: inherit;
    font-size: 12px;
    padding: 6px 9px;
    border-radius: 7px;
    border: 1px solid var(--border-strong);
    background: var(--surface);
    color: var(--ink);
  }
  .filter-text {
    width: 110px;
  }
  .clear-filters {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    font-size: 11.5px;
    color: var(--ink-tertiary);
    border: none;
    background: transparent;
    padding: 4px 6px;
  }
  .clear-filters:hover {
    color: var(--ink);
  }
  .drag-error {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 12px;
    color: var(--danger);
    background: color-mix(in srgb, var(--danger) 7%, transparent);
    border: 1px solid color-mix(in srgb, var(--danger) 25%, transparent);
    border-radius: 8px;
    padding: 6px 10px;
  }
  .drag-error button {
    margin-left: auto;
    border: none;
    background: transparent;
    color: inherit;
  }
  .content {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: 16px 20px 20px;
  }
  .note {
    font-size: 12.5px;
    color: var(--ink-tertiary);
  }
  .note.warn {
    color: var(--danger);
  }
  .board {
    display: flex;
    gap: 12px;
    align-items: flex-start;
  }
  .column {
    width: 240px;
    flex: none;
    display: flex;
    flex-direction: column;
    gap: 8px;
    border-radius: var(--radius-card);
    background: var(--sunken-2);
    border: 1px solid var(--border);
    padding: 10px;
    min-height: 120px;
  }
  .column.drop-target {
    border-color: var(--accent);
    background: color-mix(in srgb, var(--accent) 6%, var(--sunken-2));
  }
  .col-head {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .col-title {
    font-size: 12px;
    font-weight: 700;
    color: var(--ink-secondary);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .col-count {
    font-size: 11px;
    color: var(--ink-tertiary);
  }
  .col-add {
    margin-left: auto;
    border: none;
    background: transparent;
    color: var(--ink-tertiary);
    border-radius: 5px;
    width: 20px;
    height: 20px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
  }
  .col-add:hover {
    background: var(--sunken);
    color: var(--ink);
  }
  .add-input {
    font-family: inherit;
    font-size: 12.5px;
    padding: 7px 9px;
    border-radius: 7px;
    border: 1px solid var(--accent);
    background: var(--surface);
    color: var(--ink);
  }
  .col-body {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .goal-group {
    margin-bottom: 20px;
  }
  .goal-group-head {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-bottom: 8px;
    color: var(--accent-deep);
  }
  .goal-group-title {
    font-size: 13px;
    font-weight: 600;
  }
  .goal-progress {
    font-size: 11px;
    font-weight: 600;
    color: var(--ink-tertiary);
    background: var(--sunken);
    border-radius: 999px;
    padding: 1px 8px;
  }
</style>

<script lang="ts">
  // ken-tasks task 4.2/4.3/4.5: one Kanban card — title, project, tags,
  // assignee, kind badge, goal chip; draggable between columns; click opens
  // the task's file in the normal document view; an archive action on done
  // cards (task 4.5).
  import type { Task } from "../lib/api";
  import { tasksStore } from "../lib/tasks.svelte";
  import { families } from "../lib/families.svelte";
  import Bot from "@lucide/svelte/icons/bot";
  import UserRound from "@lucide/svelte/icons/user-round";
  import Target from "@lucide/svelte/icons/target";
  import Archive from "@lucide/svelte/icons/archive";
  import Users from "@lucide/svelte/icons/users";

  let {
    task,
    onDragStart,
    onDragEnd,
  }: {
    task: Task;
    onDragStart: (id: string) => void;
    onDragEnd: () => void;
  } = $props();

  const openReason = $derived(tasksStore.openReason(task));
  // ken-families task 4.3: the family marker chip. `familyForTask` is the
  // real per-task signal (see its doc comment in families.svelte.ts) —
  // there's no invented field here.
  const familyDto = $derived(families.familyForTask(task));
  // Drag-drop status writes go through `task_update`, which for a family
  // board task would leave an UNCOMMITTED change in the family clone (no
  // command in this build's touch-boundary routes a status-only patch
  // through `GitTransport::commit_paths` the way `family_accept_task` does
  // for acceptance) — and S8 recorded that an uncommitted change makes the
  // next `git pull --rebase` refuse outright ("You have unstaged
  // changes"), which would silently halt sync for the whole connection.
  // Deferred, not invented: dragging a family task is disabled here until
  // a `family_task_set_status`-shaped command exists to commit + push the
  // change like every other family write in this build.
  const dragReason = $derived(
    familyDto
      ? `Dragging family tasks is disabled for now — status changes aren't wired to sync yet (see the comment in TaskCard.svelte).`
      : null,
  );
  let archiving = $state(false);

  async function archive() {
    archiving = true;
    try {
      await tasksStore.archiveTask(task.id);
    } finally {
      archiving = false;
    }
  }
</script>

<div
  class="card"
  class:disabled-link={!!openReason}
  class:no-drag={!!dragReason}
  draggable={!dragReason}
  role="button"
  tabindex="0"
  title={dragReason ?? openReason ?? "Open task file"}
  ondragstart={(e) => {
    if (dragReason) {
      e.preventDefault();
      return;
    }
    onDragStart(task.id);
    if (e.dataTransfer) {
      e.dataTransfer.effectAllowed = "move";
      e.dataTransfer.setData("text/plain", task.id);
    }
  }}
  ondragend={onDragEnd}
  onclick={() => void tasksStore.openFile(task)}
  onkeydown={(e) => {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      void tasksStore.openFile(task);
    }
  }}
>
  <div class="row title-row">
    <span class="kind-badge" class:ai={task.kind === "ai"} title={task.kind === "ai" ? "AI task" : "Human task"}>
      {#if task.kind === "ai"}<Bot size={11} strokeWidth={2} />{:else}<UserRound size={11} strokeWidth={2} />{/if}
    </span>
    <span class="title">{task.title || "Untitled task"}</span>
    {#if familyDto}
      <span class="family-badge" title="From the '{familyDto.connection.name}' family board">
        <Users size={11} strokeWidth={1.75} />
      </span>
    {/if}
  </div>

  <div class="row meta-row">
    {#if task.project}
      <span class="chip project">{task.project}</span>
    {/if}
    {#if task.goal}
      <span class="chip goal" title="Goal: {tasksStore.goalTitle(task.goal)}">
        <Target size={10} strokeWidth={2} />{tasksStore.goalTitle(task.goal)}
      </span>
    {/if}
    {#if task.assignee}
      <span class="chip assignee">{task.assignee}</span>
    {/if}
  </div>

  {#if task.tags.length > 0}
    <div class="row tag-row">
      {#each task.tags as tag (tag)}
        <span class="chip tag">{tag}</span>
      {/each}
    </div>
  {/if}

  {#if task.status === "done"}
    <div class="row archive-row">
      <button
        class="archive-btn"
        disabled={archiving}
        onclick={(e) => {
          e.stopPropagation();
          void archive();
        }}
        title="Archive this task"
      >
        <Archive size={12} strokeWidth={1.75} />{archiving ? "Archiving…" : "Archive"}
      </button>
    </div>
  {/if}
</div>

<style>
  .card {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 10px 11px;
    border-radius: var(--radius-control);
    border: 1px solid var(--border);
    background: var(--surface);
    box-shadow: var(--shadow-card);
    cursor: pointer;
    text-align: left;
  }
  .card:hover {
    border-color: var(--border-strong);
  }
  .card.disabled-link {
    cursor: default;
  }
  /* ken-families task 4.3: family tasks aren't draggable yet (see the
     `dragReason` comment above) — `grab` would promise a drag that
     `ondragstart` then refuses, so this shows the disabled cursor instead. */
  .card.no-drag {
    cursor: default;
  }
  .family-badge {
    flex: none;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 16px;
    height: 16px;
    border-radius: 5px;
    background: color-mix(in srgb, var(--file-doc) 14%, transparent);
    color: var(--file-doc);
    margin-left: auto;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
  }
  .title-row {
    gap: 7px;
  }
  .kind-badge {
    flex: none;
    width: 17px;
    height: 17px;
    border-radius: 5px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    background: var(--sunken);
    color: var(--ink-tertiary);
  }
  .kind-badge.ai {
    background: color-mix(in srgb, var(--accent) 16%, transparent);
    color: var(--accent-deep);
  }
  .title {
    font-size: 13px;
    font-weight: 600;
    color: var(--ink);
    line-height: 1.35;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    font-size: 10.5px;
    font-weight: 500;
    padding: 2px 7px;
    border-radius: 999px;
    background: var(--sunken);
    color: var(--ink-secondary);
    white-space: nowrap;
  }
  .chip.project {
    background: color-mix(in srgb, var(--file-doc) 14%, transparent);
    color: var(--file-doc);
  }
  .chip.goal {
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    color: var(--accent-deep);
  }
  .chip.tag {
    font-family: var(--font-mono);
  }
  .archive-row {
    justify-content: flex-end;
  }
  .archive-btn {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: 10.5px;
    font-weight: 500;
    padding: 3px 8px;
    border-radius: 6px;
    border: 1px solid var(--border);
    background: var(--paper);
    color: var(--ink-tertiary);
  }
  .archive-btn:hover {
    background: var(--sunken);
    color: var(--ink);
  }
</style>

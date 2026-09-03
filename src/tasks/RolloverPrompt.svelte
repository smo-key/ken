<script lang="ts">
  // ken-tasks task 4.4: the new-day rollover prompt (design D5) — one
  // independent roll/promote/archive choice per unfinished daily task.
  // `tasksStore.rolloverCandidates` is purely derived server-side
  // (`daily_rollover_candidates`), so this panel simply disappears once
  // every stale task has been resolved — no dismiss-all needed.
  import { tasksStore } from "../lib/tasks.svelte";
  import RotateCw from "@lucide/svelte/icons/rotate-cw";
</script>

{#if tasksStore.rolloverCandidates.length > 0}
  <div class="panel">
    <div class="panel-title">
      <RotateCw size={14} strokeWidth={1.75} />
      <span>Yesterday's daily tasks</span>
    </div>
    <p class="note">
      These are still unfinished. Roll each forward, promote it to the main
      board, or archive it — nothing happens automatically.
    </p>
    {#each tasksStore.rolloverCandidates as t (t.id)}
      <div class="row">
        <span class="title">{t.title || "Untitled task"}</span>
        <div class="actions">
          <button
            class="btn btn-small"
            disabled={tasksStore.resolvingRolloverId === t.id}
            onclick={() => void tasksStore.resolveRollover(t.id, "roll")}
          >
            Roll forward
          </button>
          <button
            class="btn btn-small"
            disabled={tasksStore.resolvingRolloverId === t.id}
            onclick={() => void tasksStore.resolveRollover(t.id, "promote")}
          >
            Promote to main
          </button>
          <button
            class="btn btn-small btn-ghost"
            disabled={tasksStore.resolvingRolloverId === t.id}
            onclick={() => void tasksStore.resolveRollover(t.id, "archive")}
          >
            Archive
          </button>
        </div>
      </div>
    {/each}
  </div>
{/if}

<style>
  .panel {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 14px;
    border: 1px solid color-mix(in srgb, var(--needs-input) 40%, var(--border));
    border-radius: var(--radius-card);
    background: color-mix(in srgb, var(--needs-input) 6%, var(--surface));
    margin-bottom: 14px;
  }
  .panel-title {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 13px;
    font-weight: 600;
    color: var(--needs-input-text);
  }
  .note {
    font-size: 12px;
    color: var(--ink-tertiary);
    margin: 0;
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    flex-wrap: wrap;
    padding: 8px 10px;
    border-radius: 8px;
    background: var(--surface);
    border: 1px solid var(--border);
  }
  .title {
    font-size: 12.5px;
    font-weight: 600;
    color: var(--ink);
  }
  .actions {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
  }
</style>

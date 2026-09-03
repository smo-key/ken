<script lang="ts">
  // ken-tasks task 4.4: "Plan my day" trigger + drafted-candidate approval
  // cards, same propose/approve pattern as memory promotion (Settings'
  // Memory section / `memory.svelte.ts`). Population is on-request only —
  // this component never calls `planDaily()` on its own.
  import { tasksStore } from "../lib/tasks.svelte";
  import Sparkles from "@lucide/svelte/icons/sparkles";
</script>

<div class="panel">
  <div class="panel-title">
    <Sparkles size={14} strokeWidth={1.75} />
    <span>Plan my day</span>
  </div>
  <p class="note">
    Ken reads the recent journal and project activity and drafts a few
    day-sized items. Nothing is created until you approve a card below.
  </p>

  {#if tasksStore.dailyPhase === "planning"}
    <div class="row">
      <span class="mini-spinner" aria-hidden="true"></span>
      <span class="soft">Drafting today's candidates…</span>
    </div>
  {/if}

  {#if tasksStore.dailyPhase === "error" && tasksStore.dailyError}
    <p class="note warn">Planning failed: {tasksStore.dailyError}</p>
  {/if}

  <div class="row">
    <button
      class="btn btn-small"
      onclick={() => void tasksStore.planDaily()}
      disabled={tasksStore.dailyPhase === "planning"}
    >
      {tasksStore.dailyPhase === "planning" ? "Planning…" : "Plan my day"}
    </button>
  </div>

  {#each tasksStore.dailyCandidates as c (c.key)}
    <div class="card">
      <div class="card-title">{c.title}</div>
      {#if c.body}
        <p class="body">{c.body}</p>
      {/if}
      <div class="row">
        {#if c.project}<span class="chip">{c.project}</span>{/if}
        {#each c.tags as tag (tag)}<span class="chip mono">{tag}</span>{/each}
      </div>
      <div class="row">
        <button
          class="btn btn-small"
          onclick={() => void tasksStore.resolveDailyCandidate(c.key, true)}
          disabled={tasksStore.resolvingKey === c.key}
        >
          Approve
        </button>
        <button
          class="btn btn-small btn-ghost"
          onclick={() => void tasksStore.resolveDailyCandidate(c.key, false)}
          disabled={tasksStore.resolvingKey === c.key}
        >
          Dismiss
        </button>
      </div>
    </div>
  {/each}
</div>

<style>
  .panel {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 14px;
    border: 1px solid var(--border);
    border-radius: var(--radius-card);
    background: var(--surface);
    box-shadow: var(--shadow-card);
    margin-bottom: 14px;
  }
  .panel-title {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 13px;
    font-weight: 600;
    color: var(--ink);
  }
  .note {
    font-size: 12px;
    color: var(--ink-tertiary);
    margin: 0;
  }
  .note.warn {
    color: var(--danger);
  }
  .row {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
  .soft {
    font-size: 12px;
    color: var(--ink-tertiary);
  }
  .mini-spinner {
    width: 12px;
    height: 12px;
    border-radius: 50%;
    border: 2px solid var(--border-strong);
    border-top-color: var(--accent);
    animation: spin 0.7s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  .card {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 10px 12px;
    border: 1px solid var(--border);
    border-radius: 9px;
    background: var(--paper);
  }
  .card-title {
    font-size: 13px;
    font-weight: 600;
    color: var(--ink);
  }
  .body {
    margin: 0;
    font-size: 12px;
    color: var(--ink-secondary);
    white-space: pre-wrap;
  }
  .chip {
    font-size: 10.5px;
    font-weight: 500;
    padding: 2px 7px;
    border-radius: 999px;
    background: var(--sunken);
    color: var(--ink-secondary);
  }
  .chip.mono {
    font-family: var(--font-mono);
  }
</style>

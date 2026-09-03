<script lang="ts">
  // ken-pipeline task 4.7: the run tray — running / queued / waiting-on-
  // human / stale, each derived straight from `pipeline_runs`/
  // `pipeline-runs` (D13: the ledger plus the board, nothing else), with a
  // cancel action and a per-pipeline cap indicator ("N/cap running").
  import { pipelineStore } from "../lib/pipeline.svelte";
  import { tasksStore } from "../lib/tasks.svelte";
  import type { PipelineRunRecord } from "../lib/api";
  import Play from "@lucide/svelte/icons/play";
  import Hourglass from "@lucide/svelte/icons/hourglass";
  import History from "@lucide/svelte/icons/history";
  import X from "@lucide/svelte/icons/x";

  let cancellingId = $state<string | null>(null);

  function ticketTitle(id: string): string {
    return tasksStore.board.tasks.find((t) => t.id === id)?.title || id;
  }

  async function cancel(runId: string) {
    cancellingId = runId;
    try {
      await pipelineStore.cancelRun(runId);
    } finally {
      cancellingId = null;
    }
  }

  /** "N/cap running" per pipeline — `RunRecord` doesn't carry a pipeline
   *  display name, only its id, so this looks it up from the already-live
   *  `tasksStore.board.pipelines`. */
  const capRows = $derived(
    pipelineStore.pipelines.map((p) => ({
      id: p.id,
      name: p.name,
      cap: p.concurrencyCap,
      running: pipelineStore.runs.running.filter((r) => r.pipeline.toLowerCase() === p.id.toLowerCase()).length,
    })),
  );
</script>

{#snippet runRow(r: PipelineRunRecord, stale: boolean)}
  <div class="run-row" class:stale>
    <div class="run-main">
      <span class="run-title">{ticketTitle(r.ticket)}</span>
      <span class="run-meta">{r.lane} · {r.agent || "—"} · {r.model || "—"}</span>
    </div>
    {#if stale}<span class="stale-badge" title="No live process observed since Ken last restarted">stale</span>{/if}
    <button class="cancel-btn" disabled={cancellingId === r.id} onclick={() => cancel(r.id)} title="Cancel this run">
      <X size={11} strokeWidth={2} />
    </button>
  </div>
{/snippet}

<div class="tray">
  <div class="tray-head">
    <span>Runs</span>
    <button class="close-btn" onclick={() => (pipelineStore.runTrayOpen = false)} aria-label="Close"><X size={13} strokeWidth={2} /></button>
  </div>

  {#if capRows.length > 0}
    <div class="caps">
      {#each capRows as row (row.id)}
        <span class="cap-chip" class:full={row.running >= row.cap}>{row.name}: {row.running}/{row.cap}</span>
      {/each}
    </div>
  {/if}

  <section>
    <h3><Play size={12} strokeWidth={2} />Running ({pipelineStore.runs.running.length})</h3>
    {#each pipelineStore.runs.running as r (r.id)}
      {@render runRow(r, false)}
    {:else}
      <p class="empty">Nothing running.</p>
    {/each}
  </section>

  <section>
    <h3><Hourglass size={12} strokeWidth={2} />Queued ({pipelineStore.runs.queued.length})</h3>
    <p class="hint">Behind the concurrency cap — starts automatically once a slot frees up.</p>
    {#each pipelineStore.runs.queued as r (r.id)}
      {@render runRow(r, false)}
    {:else}
      <p class="empty">Nothing queued.</p>
    {/each}
  </section>

  <section>
    <h3>Waiting on you ({pipelineStore.runs.waitingHuman.length})</h3>
    {#each pipelineStore.runs.waitingHuman as ticketId (ticketId)}
      <div class="run-row">
        <div class="run-main"><span class="run-title">{ticketTitle(ticketId)}</span></div>
        <button class="btn btn-small" onclick={() => pipelineStore.openKickoff(ticketId)}>Review</button>
      </div>
    {:else}
      <p class="empty">Nothing waiting on a confirmation.</p>
    {/each}
  </section>

  {#if pipelineStore.runs.stale.length > 0}
    <section>
      <h3><History size={12} strokeWidth={2} />Stale ({pipelineStore.runs.stale.length})</h3>
      <p class="hint">Recorded `running` on disk with no live process observed since restart — not silently passed (D13).</p>
      {#each pipelineStore.runs.stale as r (r.id)}
        {@render runRow(r, true)}
      {/each}
    </section>
  {/if}
</div>

<style>
  .tray {
    width: 280px;
    flex: none;
    border-left: 1px solid var(--border);
    padding: 14px;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .tray-head {
    display: flex;
    align-items: center;
    font-size: 13px;
    font-weight: 600;
    color: var(--ink);
  }
  .close-btn {
    margin-left: auto;
    border: none;
    background: transparent;
    color: var(--ink-tertiary);
  }
  .caps {
    display: flex;
    flex-wrap: wrap;
    gap: 5px;
  }
  .cap-chip {
    font-size: 10.5px;
    font-weight: 600;
    padding: 2px 8px;
    border-radius: 999px;
    background: var(--sunken);
    color: var(--ink-secondary);
  }
  .cap-chip.full {
    background: color-mix(in srgb, var(--needs-input) 16%, transparent);
    color: var(--needs-input-text);
  }
  h3 {
    display: flex;
    align-items: center;
    gap: 5px;
    margin: 0 0 6px;
    font-size: 11.5px;
    font-weight: 700;
    color: var(--ink-secondary);
    text-transform: uppercase;
    letter-spacing: 0.03em;
  }
  .hint {
    margin: -3px 0 6px;
    font-size: 10.5px;
    color: var(--ink-tertiary);
  }
  .empty {
    margin: 0;
    font-size: 11.5px;
    color: var(--ink-tertiary);
  }
  .run-row {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 8px;
    border-radius: 7px;
    border: 1px solid var(--border);
    background: var(--surface);
    margin-bottom: 5px;
  }
  .run-row.stale {
    border-color: color-mix(in srgb, var(--needs-input) 40%, var(--border-strong));
  }
  .run-main {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
    flex: 1;
  }
  .run-title {
    font-size: 12px;
    font-weight: 600;
    color: var(--ink);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .run-meta {
    font-size: 10.5px;
    color: var(--ink-tertiary);
  }
  .stale-badge {
    flex: none;
    font-size: 9.5px;
    font-weight: 700;
    color: var(--needs-input-text);
    background: color-mix(in srgb, var(--needs-input) 16%, transparent);
    border-radius: 999px;
    padding: 1px 6px;
  }
  .cancel-btn {
    flex: none;
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
  .cancel-btn:hover {
    background: var(--sunken);
    color: var(--danger);
  }
</style>

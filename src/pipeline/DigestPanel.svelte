<script lang="ts">
  // ken-pipeline task 4.11: the daily digest panel — `pipeline_digest`,
  // rendered group-by-group in spec order (awaiting review, then newly
  // unblocked, then blocked oldest-first, then moved-today, new ideas,
  // stale runs). The "unblocked overnight" group gets a per-ticket start
  // action that still opens the kickoff confirmation dialog — an unblock
  // never starts an agent by itself (D5), and this UI never invents a
  // shortcut around that.
  import { pipelineStore } from "../lib/pipeline.svelte";
  import SquareCheck from "@lucide/svelte/icons/square-check";
  import Sparkles from "@lucide/svelte/icons/sparkles";
  import X from "@lucide/svelte/icons/x";
  import RefreshCw from "@lucide/svelte/icons/refresh-cw";

  $effect(() => {
    if (pipelineStore.digestOpen && !pipelineStore.digest && !pipelineStore.digestLoading) {
      void pipelineStore.refreshDigest();
    }
  });
</script>

<div class="tray">
  <div class="tray-head">
    <span>Daily digest</span>
    <button class="icon-btn" onclick={() => void pipelineStore.refreshDigest()} title="Refresh" disabled={pipelineStore.digestLoading}>
      <RefreshCw size={12} strokeWidth={2} />
    </button>
    <button class="close-btn" onclick={() => (pipelineStore.digestOpen = false)} aria-label="Close"><X size={13} strokeWidth={2} /></button>
  </div>

  {#if pipelineStore.digestLoading}
    <p class="note">Loading…</p>
  {:else if pipelineStore.digestError}
    <p class="note warn">{pipelineStore.digestError}</p>
  {:else if pipelineStore.digest}
    {@const d = pipelineStore.digest}

    <section>
      <h3><SquareCheck size={12} strokeWidth={2} />Awaiting your review ({d.awaitingReview.length})</h3>
      {#each d.awaitingReview as e (e.ticketId)}
        <div class="row">
          <span class="title">{e.title}</span>
          <span class="sub">{e.runCount} run(s) · updated {e.updated}</span>
        </div>
      {:else}
        <p class="empty">Nothing waiting on you.</p>
      {/each}
    </section>

    <section class="prominent">
      <h3>Unblocked overnight ({d.newlyUnblocked.length})</h3>
      {#each d.newlyUnblocked as e (e.ticketId)}
        <div class="row">
          <span class="title">{e.title}</span>
          <span class="sub">returns to {e.returnLane}</span>
          <button class="btn btn-small" onclick={() => pipelineStore.openKickoff(e.ticketId)}>Start</button>
        </div>
      {:else}
        <p class="empty">Nothing freed up overnight.</p>
      {/each}
    </section>

    <section>
      <h3>Blocked, oldest first ({d.blocked.length})</h3>
      {#each d.blocked as e (e.ticketId)}
        <div class="row">
          <span class="title">{e.title}</span>
          <span class="sub">
            {e.blockReason ?? "dependency"}
            {#if e.rootBlockers.length > 0}· root: {e.rootBlockers.join(", ")}{/if}
            {#if e.blockedAt}· since {e.blockedAt}{/if}
          </span>
        </div>
      {:else}
        <p class="empty">Nothing blocked.</p>
      {/each}
    </section>

    <section>
      <h3>Moved today ({d.movedToday.length})</h3>
      {#each d.movedToday as e (e.ticketId)}
        <div class="row">
          <span class="title">{e.title}</span>
          <span class="sub">now in {e.lane}</span>
        </div>
      {:else}
        <p class="empty">Nothing moved today.</p>
      {/each}
    </section>

    <section>
      <h3><Sparkles size={12} strokeWidth={2} />New ideas ({d.newIdeas.length})</h3>
      {#each d.newIdeas as e (e.ticketId)}
        <div class="row">
          <span class="title">{e.title}</span>
          {#if e.spawnedBy}<span class="sub">from {e.spawnedBy}</span>{/if}
        </div>
      {:else}
        <p class="empty">No new ideas today.</p>
      {/each}
    </section>

    {#if d.staleRuns.length > 0}
      <section>
        <h3>Stale runs ({d.staleRuns.length})</h3>
        {#each d.staleRuns as r (r.id)}
          <div class="row"><span class="title">{r.ticket}</span><span class="sub">{r.lane}</span></div>
        {/each}
      </section>
    {/if}
  {/if}
</div>

<style>
  .tray {
    width: 300px;
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
    gap: 6px;
    font-size: 13px;
    font-weight: 600;
    color: var(--ink);
  }
  .icon-btn,
  .close-btn {
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
  .close-btn {
    margin-left: auto;
  }
  .icon-btn:hover,
  .close-btn:hover {
    background: var(--sunken);
    color: var(--ink);
  }
  .note {
    font-size: 12px;
    color: var(--ink-tertiary);
  }
  .note.warn {
    color: var(--danger);
  }
  section {
    display: flex;
    flex-direction: column;
    gap: 5px;
  }
  section.prominent {
    padding: 8px 9px;
    border-radius: 9px;
    background: color-mix(in srgb, var(--accent) 8%, transparent);
    border: 1px solid color-mix(in srgb, var(--accent) 25%, transparent);
  }
  h3 {
    display: flex;
    align-items: center;
    gap: 5px;
    margin: 0 0 2px;
    font-size: 11.5px;
    font-weight: 700;
    color: var(--ink-secondary);
    text-transform: uppercase;
    letter-spacing: 0.03em;
  }
  .row {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 6px 8px;
    border-radius: 7px;
    background: var(--surface);
    border: 1px solid var(--border);
  }
  .title {
    font-size: 12px;
    font-weight: 600;
    color: var(--ink);
  }
  .sub {
    font-size: 10.5px;
    color: var(--ink-tertiary);
  }
  .empty {
    margin: 0;
    font-size: 11px;
    color: var(--ink-tertiary);
  }
  .row .btn {
    align-self: flex-start;
    margin-top: 2px;
  }
</style>

<script lang="ts">
  // The one status panel for background work (knowledge-core decision 34):
  // the one throttle (Gentle, Normal, Fast, and Pause) over everything Ken
  // does on its own, then what is running with its progress, what is queued,
  // and what failed with its reason. The library's build queue (decision 33)
  // is started, stopped and widened here too.
  import { onMount } from "svelte";
  import { api, type BackgroundStatus, type Pace } from "../lib/api";
  import { buildLine, jobLine, jobPct, kindLabel, paceLine, PACES, waitLine } from "../lib/background";
  import { toast } from "../lib/toast.svelte";
  import ProgressBar from "../lib/ProgressBar.svelte";
  import EmptyState from "../lib/ui/EmptyState.svelte";

  let status = $state<BackgroundStatus | null>(null);
  let busy = $state(false);

  async function refresh() {
    try {
      status = await api.backgroundStatus();
    } catch {
      status = null;
    }
  }

  onMount(() => {
    void refresh();
    const t = window.setInterval(() => void refresh(), 3000);
    return () => window.clearInterval(t);
  });

  async function act(what: string, f: () => Promise<unknown>) {
    busy = true;
    try {
      await f();
    } catch (e) {
      toast.error(what, e);
    } finally {
      busy = false;
      await refresh();
    }
  }

  const setting = $derived(status?.throttle.setting ?? { pace: "gentle" as Pace, paused: false });
  // Work that waits for its turn (paused, the gap) is not running: it is
  // listed with what waits.
  const working = $derived((status?.running ?? []).filter((j) => !j.waiting));
  const waitingJobs = $derived((status?.running ?? []).filter((j) => j.waiting));

  function setPace(pace: Pace) {
    void act("Could not set the pace", () => api.backgroundSet(pace, setting.paused));
  }

  function togglePause() {
    void act("Could not pause", () => api.backgroundSet(setting.pace, !setting.paused));
  }
</script>

<div class="panel">
  <div class="head">
    <span class="title">Background work</span>
    <button class="btn btn-small" class:btn-primary={setting.paused} disabled={busy} onclick={togglePause}>
      {setting.paused ? "Resume" : "Pause"}
    </button>
  </div>
  <div class="paces" role="radiogroup" aria-label="Pace of background work">
    {#each PACES as p (p.value)}
      <button
        class="seg"
        class:on={setting.pace === p.value}
        role="radio"
        aria-checked={setting.pace === p.value}
        disabled={busy}
        title={p.note}
        onclick={() => setPace(p.value)}>{p.label}</button
      >
    {/each}
  </div>
  {#if status}
    {@const pace = paceLine(status.throttle)}
    <p class="note" title={pace.tip}>{pace.text}</p>
  {/if}

  <div class="section">Running</div>
  {#each working as job, i (i)}
    <div class="row">
      <span>{jobLine(job)}</span>
      {#if jobPct(job) !== null}<ProgressBar pct={jobPct(job)} />{/if}
    </div>
  {:else}
    <EmptyState />
  {/each}

  <div class="section">Queued</div>
  {#each waitingJobs as job, i (i)}
    <div class="row"><span>{waitLine(job, setting.paused)}</span></div>
  {/each}
  {#each status?.queued ?? [] as q, i (i)}
    <div class="row"><span>{kindLabel(q.kind)} · {q.label} · {q.count}</span></div>
  {:else}
    {#if waitingJobs.length === 0}<EmptyState />{/if}
  {/each}

  <div class="section">Building the library</div>
  {#if status?.build}
    <p class="note">{buildLine(status.build)}</p>
    {#each status.build.reading as r (r)}<div class="row"><span>In the inbox · {r}</span></div>{/each}
    <div class="actions">
      <label class="at-once">
        At once
        <select
          value={status.build.atOnce}
          disabled={busy}
          onchange={(e) => void act("Could not change it", () => api.buildSetAtOnce(Number((e.currentTarget as HTMLSelectElement).value)))}
        >
          {#each [1, 2, 3, 5] as n (n)}<option value={n}>{n}</option>{/each}
        </select>
      </label>
      {#if status.build.queued > 0}
        <button class="btn btn-small btn-ghost" disabled={busy} onclick={() => void act("Could not stop", () => api.buildStop())}>Stop</button>
      {:else}
        <button class="btn btn-small" disabled={busy} onclick={() => void act("Could not start", () => api.buildStart())}>Build again</button>
      {/if}
    </div>
  {:else}
    <div class="actions">
      <button class="btn btn-small btn-primary" disabled={busy} onclick={() => void act("Could not start building", () => api.buildStart())}>
        Build the library
      </button>
    </div>
  {/if}

  <div class="section">Failed</div>
  {#each status?.failed ?? [] as f, i (i)}
    <div class="row fail">
      <span>{kindLabel(f.kind)} · {f.label}</span>
      <span class="why">{f.reason}</span>
    </div>
  {:else}
    <EmptyState />
  {/each}
</div>

<style>
  .panel {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .title {
    font-weight: 600;
    font-size: 13.5px;
  }
  .paces {
    display: inline-flex;
    gap: 2px;
    padding: 2px;
    border: 1px solid var(--border);
    border-radius: 8px;
    align-self: flex-start;
  }
  .seg {
    padding: 3px 12px;
    border: none;
    border-radius: 6px;
    background: transparent;
    color: var(--ink-secondary);
    font-size: 12.5px;
    cursor: pointer;
  }
  .seg.on {
    background: var(--accent);
    color: var(--paper);
  }
  .note {
    margin: 0;
    font-size: 12.5px;
    line-height: 1.5;
    color: var(--ink-secondary);
  }
  .section {
    margin-top: 6px;
    font-size: 11.5px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--ink-tertiary);
  }
  .row {
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: 12.5px;
    color: var(--ink);
  }
  .fail .why {
    color: var(--ink-secondary);
    font-size: 12px;
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .at-once {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 12.5px;
    color: var(--ink-secondary);
  }
</style>

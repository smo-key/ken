<script lang="ts">
  import { api, type RunRow } from "../lib/api";
  import { automations } from "../lib/automations.svelte";
  import { liveCaption, countdownFrom, type Tone } from "../lib/liveRun";
  import { timeAgo } from "../lib/format";
  import AutomationForm from "./AutomationForm.svelte";
  import Pencil from "@lucide/svelte/icons/pencil";
  import Play from "@lucide/svelte/icons/play";
  import Trash2 from "@lucide/svelte/icons/trash-2";
  import ContextMenu, { openContextMenu } from "../lib/ui/ContextMenu.svelte";
  import ConfirmMenu, { openConfirm } from "../lib/ui/ConfirmMenu.svelte";

  let formOpen = $state(false);
  let editingSlug = $state<string | null>(null);

  const detail = $derived(automations.detail);
  const liveForSelected = $derived(
    automations.selected ? automations.liveEvent(automations.selected)?.status ?? null : null,
  );
  const isRunning = $derived(
    liveForSelected === "running" || liveForSelected === "blocked",
  );

  // A local clock so the elapsed timer and queued countdown advance smoothly
  // between the (coarser) server events. It only ticks while something is live.
  let nowTick = $state(Date.now());
  $effect(() => {
    const ticking = Object.values(automations.live).some(
      (e) => e.status === "running" || e.status === "queued",
    );
    if (!ticking) return;
    nowTick = Date.now();
    const id = setInterval(() => (nowTick = Date.now()), 1000);
    return () => clearInterval(id);
  });

  // Per-run anchors so ticking is stable across re-renders.
  const seenEvent = new Map<string, unknown>();
  const startMsByRun = new Map<string, number>();
  const deadlineByRun = new Map<string, number>();

  type LiveDisplay =
    | { kind: "running"; head: string; path: string | null; timer: string; tone: Tone }
    | { kind: "queued"; label: string; count: string; tone: Tone }
    | { kind: "plain"; label: string; tone: Tone };

  /** Split "Read notes/a.md" into a verb head and a mono file-path tail. */
  function splitActivity(act: string): { head: string; path: string | null } {
    const sp = act.indexOf(" ");
    if (sp > 0) {
      const rest = act.slice(sp + 1);
      if (/[\/.]/.test(rest)) return { head: act.slice(0, sp), path: rest };
    }
    return { head: act, path: null };
  }

  /** Live caption for a slug, with a locally-ticking timer/countdown. */
  function liveDisplay(slug: string): LiveDisplay | null {
    const ev = automations.liveEvent(slug);
    if (!ev) return null;
    const isNew = seenEvent.get(slug) !== ev;
    if (isNew) seenEvent.set(slug, ev);

    if (ev.status === "running") {
      const key = `${slug}:${ev.runId}`;
      if ((isNew && ev.elapsedSecs != null) || !startMsByRun.has(key)) {
        startMsByRun.set(key, Date.now() - (ev.elapsedSecs ?? 0) * 1000);
      }
      const start = startMsByRun.get(key)!;
      const elapsed = Math.max(0, Math.floor((nowTick - start) / 1000));
      const act = ev.activity?.trim();
      const { head, path } = act ? splitActivity(act) : { head: "running…", path: null };
      return { kind: "running", head, path, timer: `${elapsed}s`, tone: "busy" };
    }
    if (ev.status === "queued") {
      const key = `${slug}:${ev.runId}`;
      if (isNew || !deadlineByRun.has(key)) {
        deadlineByRun.set(key, Date.now() + (ev.etaSecs ?? 0) * 1000);
      }
      return {
        kind: "queued",
        label: "queued — starts in",
        count: countdownFrom(deadlineByRun.get(key)!, nowTick),
        tone: "attention",
      };
    }
    const c = liveCaption(ev);
    return { kind: "plain", label: c.label, tone: c.tone };
  }

  /** Static caption for a list row when nothing is live. */
  function staticCaption(a: { enabled: boolean; autoApply: boolean }): {
    label: string;
    tone: Tone;
  } {
    if (!a.enabled) return { label: "paused", tone: "muted" };
    return a.autoApply
      ? { label: "automatic", tone: "healthy" }
      : { label: "ask first", tone: "muted" };
  }

  function toneColor(tone: string): string {
    switch (tone) {
      case "healthy": return "var(--healthy)";
      case "busy": return "var(--accent)";
      case "attention": return "var(--needs-input)";
      case "danger": return "var(--danger)";
      default: return "var(--ink-tertiary)";
    }
  }

  function dotFor(run: RunRow): string {
    switch (run.status) {
      case "fresh": return "var(--healthy)";
      case "running": return "var(--accent)";
      case "pending_approval":
      case "blocked": return "var(--needs-input)";
      case "failed": return "var(--danger)";
      default: return "var(--ink-tertiary)";
    }
  }

  function runLine(run: RunRow): string {
    const when = timeAgo(run.startedAt);
    switch (run.status) {
      case "running": return `${when} — running…`;
      case "pending_approval": return `${when} — ${run.summary ?? "waiting for your approval"}`;
      case "fresh": return `${when} — ${run.summary ?? "completed"}`;
      case "failed": return `${when} — failed`;
      case "cancelled": return `${when} — cancelled`;
      case "discarded": return `${when} — discarded`;
      default: return `${when} — ${run.status}`;
    }
  }

  function startCreate() {
    editingSlug = null;
    formOpen = true;
  }

  function openEdit(slug: string) {
    editingSlug = slug;
    formOpen = true;
  }

  function cancelRun(slug: string) {
    void api.cancelRun(slug, "automation");
  }

  function rowMenu(e: MouseEvent, slug: string, name: string) {
    e.preventDefault();
    const x = e.clientX;
    const y = e.clientY;
    openContextMenu(x, y, [
      { label: "Edit", icon: Pencil, onSelect: () => openEdit(slug) },
      { label: "Run now", icon: Play, onSelect: () => void automations.run(slug) },
      "separator",
      {
        label: "Delete…",
        icon: Trash2,
        danger: true,
        onSelect: () =>
          openConfirm(x, y, {
            title: `Delete “${name}”?`,
            body: "Removes this automation from Ken. Anything it already did stays as it is.",
            confirmLabel: "Delete automation",
            onConfirm: () => void automations.remove(slug),
          }),
      },
    ]);
  }
</script>

<div class="screen">
  <!-- list pane -->
  <div class="list">
    <div class="list-head">
      Automations
      <button class="plus" title="New automation" onclick={startCreate}>+</button>
    </div>

    {#each automations.list as a (a.slug)}
      {@const live = liveDisplay(a.slug)}
      {@const cap = staticCaption(a)}
      {@const tone = live ? live.tone : cap.tone}
      <button
        class="row"
        class:active={automations.selected === a.slug}
        onclick={() => automations.select(a.slug)}
        oncontextmenu={(e) => rowMenu(e, a.slug, a.name)}
      >
        <span class="row-top">
          {a.name}
          <span class="dot" style:background={toneColor(tone)}></span>
        </span>
        {#if live?.kind === "running"}
          <span class="row-caption live">
            <span class="act">{live.head}</span>{#if live.path}<span class="mono path"> {live.path}</span>{/if}<span class="timer"> · {live.timer}</span>
          </span>
        {:else if live?.kind === "queued"}
          <span class="row-caption live">
            <span class="act">{live.label}</span><span class="timer"> {live.count}</span>
          </span>
        {:else if live?.kind === "plain"}
          <span class="row-caption live"><span class="act">{live.label}</span></span>
        {:else}
          <span class="row-caption">{cap.label}</span>
        {/if}
      </button>
    {/each}

    {#if automations.list.length === 0}
      <div class="empty-list">
        Automations watch for new files and do something with them — draft a
        summary, file a task. New files matching your patterns kick them off.
      </div>
    {/if}

    <div class="list-foot">
      <button class="foot-link accent" onclick={startCreate}>+ New automation</button>
    </div>
  </div>

  <!-- detail pane -->
  <div class="detail">
    {#if detail}
      {@const a = detail.automation}
      {@const liveSel = liveDisplay(a.slug)}
      <div class="detail-inner">
        <div class="detail-head">
          <h1>{a.name}</h1>
          <span class="spacer"></span>
          <button class="btn btn-small" onclick={() => openEdit(a.slug)}>Edit</button>
          {#if isRunning}
            <button class="btn btn-small" onclick={() => cancelRun(a.slug)}>Cancel run</button>
          {:else}
            <button class="btn btn-small btn-primary" onclick={() => automations.run(a.slug)}>
              Run now
            </button>
          {/if}
        </div>

        {#if liveSel}
          <div class="live-line" style:color={toneColor(liveSel.tone)}>
            <span class="live-dot" style:background={toneColor(liveSel.tone)}></span>
            {#if liveSel.kind === "running"}
              <span class="act">{liveSel.head}</span>{#if liveSel.path}<span class="mono path"> {liveSel.path}</span>{/if}<span class="timer"> · {liveSel.timer}</span>
            {:else if liveSel.kind === "queued"}
              <span class="act">{liveSel.label}</span><span class="timer"> {liveSel.count}</span>
            {:else}
              <span class="act">{liveSel.label}</span>
            {/if}
          </div>
        {/if}

        {#if !a.enabled}
          <p class="desc">Paused — new files won't trigger it, but you can still run it by hand.</p>
        {/if}

        <div class="card">
          <div class="card-label">Watches</div>
          <div class="chips">
            {#each a.globs as g (g)}
              <span class="chip mono">{g}</span>
            {/each}
          </div>
          <div class="card-note">New files matching these patterns start a run.</div>
        </div>

        <div class="card">
          <div class="card-label">What Ken does</div>
          <div class="instruction">{a.prompt}</div>
        </div>

        <div class="card">
          <div class="card-label">Acting</div>
          <div class="card-note">
            {a.autoApply
              ? "Automatic — Ken carries out the actions directly when it runs."
              : "Ask first — Ken writes a plan and waits for your approval before doing anything outside your files."}
          </div>
        </div>

        <div class="card runs">
          <div class="card-label">Recent runs</div>
          {#if detail.runs.length === 0}
            <div class="card-note">No runs yet.</div>
          {/if}
          {#each detail.runs as run (run.id)}
            <div class="run-row">
              <span class="dot" style:background={dotFor(run)}></span>
              <span class="run-text">
                {runLine(run)}
                {#if run.error}
                  <span class="run-error">{run.error}</span>
                {/if}
              </span>
            </div>
          {/each}
        </div>
      </div>
    {:else}
      <div class="empty">
        <p>Select an automation, or create one.</p>
        <p class="hint">
          Automations watch for new files and do something with them — draft a
          summary, file a task. New files matching your patterns kick them off.
        </p>
        <button class="btn btn-primary" onclick={startCreate}>New automation</button>
      </div>
    {/if}
  </div>
</div>

{#if formOpen}
  <AutomationForm
    slug={editingSlug}
    close={(saved) => {
      formOpen = false;
      void automations.refresh();
      if (saved) void automations.select(saved);
    }}
  />
{/if}

<ContextMenu />
<ConfirmMenu />

<style>
  .screen {
    flex: 1;
    min-width: 0;
    display: flex;
    min-height: 0;
  }
  .list {
    flex: 0 1 272px;
    min-width: 220px;
    border-right: 1px solid var(--border);
    background: var(--sunken-2);
    display: flex;
    flex-direction: column;
    overflow-y: auto;
    padding: 12px 8px;
  }
  .list-head {
    display: flex;
    align-items: center;
    padding: 2px 10px 8px;
    font-size: 11px;
    font-weight: 700;
    color: var(--ink-tertiary);
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }
  .plus {
    margin-left: auto;
    font-size: 14px;
    color: var(--accent);
    font-weight: 600;
    border: none;
    background: none;
  }
  .row {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 9px 10px;
    border-radius: 8px;
    border: none;
    background: transparent;
    text-align: left;
    width: 100%;
  }
  .row:hover {
    background: var(--sunken);
  }
  .row.active {
    background: color-mix(in srgb, var(--accent) 10%, transparent);
  }
  .row.active .row-top {
    color: var(--accent-deep);
  }
  .row-top {
    display: flex;
    align-items: center;
    gap: 7px;
    font-size: 12.5px;
    font-weight: 600;
    color: var(--ink);
  }
  .row-top .dot {
    margin-left: auto;
  }
  .dot {
    width: 6px;
    height: 6px;
    border-radius: 3px;
    flex: none;
  }
  .row-caption {
    font-size: 11.5px;
    color: var(--ink-tertiary);
  }
  .empty-list {
    padding: 10px;
    font-size: 12px;
    color: var(--ink-tertiary);
    line-height: 1.6;
  }
  .list-foot {
    margin-top: auto;
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding-top: 12px;
  }
  .foot-link {
    padding: 7px 10px;
    border-radius: 8px;
    font-size: 12.5px;
    color: var(--ink-secondary);
    border: none;
    background: none;
    text-align: left;
  }
  .foot-link:hover {
    background: var(--sunken);
  }
  .foot-link.accent {
    color: var(--accent);
    font-weight: 600;
  }

  .detail {
    flex: 1;
    min-width: 0;
    overflow-y: auto;
    padding: 32px 40px;
  }
  .detail-inner {
    max-width: 680px;
    margin: 0 auto;
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .detail-head {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
  }
  h1 {
    margin: 0;
    font-family: var(--font-serif);
    font-size: 24px;
    font-weight: 500;
  }
  .spacer {
    flex: 1;
  }
  .live-line {
    display: flex;
    align-items: baseline;
    gap: 7px;
    margin: -4px 0 2px;
    font-size: 12px;
    line-height: 1.5;
  }
  .live-line .live-dot {
    width: 6px;
    height: 6px;
    border-radius: 3px;
    align-self: center;
    flex: none;
  }
  .act,
  .path {
    color: var(--ink-secondary);
  }
  .mono {
    font-family: var(--font-mono);
  }
  .timer {
    color: var(--ink-tertiary);
  }
  .desc {
    margin: -6px 0 0;
    font-size: 13px;
    color: var(--ink-secondary);
  }
  .card {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-card);
    padding: 18px 20px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .card-label {
    display: flex;
    align-items: center;
    font-size: 11px;
    font-weight: 700;
    color: var(--ink-tertiary);
    letter-spacing: 0.07em;
    text-transform: uppercase;
  }
  .chips {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    font-size: 12px;
    border: 1px solid var(--border);
    border-radius: 7px;
    padding: 5px 11px;
    background: var(--paper);
  }
  .card-note {
    font-size: 11.5px;
    color: var(--ink-tertiary);
    line-height: 1.6;
  }
  .instruction {
    font-size: 13.5px;
    line-height: 1.7;
    background: var(--paper);
    border-radius: 9px;
    padding: 12px 15px;
    white-space: pre-wrap;
  }
  .runs .run-row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 0;
    border-top: 1px solid var(--sunken);
    font-size: 12.5px;
  }
  .run-text {
    flex: 1;
    min-width: 0;
    line-height: 1.5;
  }
  .run-error {
    display: block;
    color: var(--danger);
    font-size: 11.5px;
    white-space: pre-wrap;
    max-height: 80px;
    overflow-y: auto;
  }
  .empty {
    height: 100%;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 8px;
  }
  .empty p {
    margin: 0;
    color: var(--ink-secondary);
    font-size: 14px;
  }
  .empty .hint {
    color: var(--ink-tertiary);
    font-size: 12.5px;
    max-width: 380px;
    text-align: center;
    line-height: 1.6;
  }
</style>

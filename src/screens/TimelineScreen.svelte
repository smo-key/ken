<script lang="ts">
  import { onMount } from "svelte";
  import { app } from "../lib/app.svelte";
  import { knowledge } from "../lib/knowledge.svelte";
  import { highlightMatches } from "../lib/knowledge";
  import { timeAgo } from "../lib/format";

  onMount(() => void knowledge.visit());

  let query = $state("");
  let category = $state<string | null>(null);
  let asOfOpen = $state(false);
  let asOf = $state("");

  const model = $derived(knowledge.model);
  const allEvents = $derived(model?.events ?? []);

  /** Distinct categories, count-ordered, at most 6. */
  const categories = $derived.by(() => {
    const counts = new Map<string, number>();
    for (const ev of allEvents) {
      counts.set(ev.category, (counts.get(ev.category) ?? 0) + 1);
    }
    return [...counts.entries()]
      .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))
      .slice(0, 6)
      .map(([name]) => name);
  });

  const filtered = $derived.by(() => {
    const q = query.trim().toLowerCase();
    return allEvents.filter(
      (ev) =>
        (!asOf || ev.date <= asOf) &&
        (!category || ev.category === category) &&
        (!q || ev.text.toLowerCase().includes(q)),
    );
  });

  const MONTHS = ["JAN", "FEB", "MAR", "APR", "MAY", "JUN",
    "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"];

  function dateLabel(date: string): string {
    const [y, m, d] = date.split("-").map(Number);
    const month = MONTHS[(m ?? 1) - 1] ?? "—";
    const label = `${month} ${d ?? "?"}`;
    return y === new Date().getFullYear() ? label : `${label}, ${y}`;
  }

  function pillStyle(cat: string): string {
    switch (cat) {
      case "decision":
      case "decisions":
        return "background:rgba(90,122,94,0.12);color:var(--healthy-text)";
      case "people":
      case "person":
        return "background:rgba(138,90,68,0.1);color:var(--accent-deep)";
      case "vendor":
        return "background:rgba(168,116,44,0.12);color:var(--needs-input-text)";
      default:
        return "background:var(--sunken);color:var(--ink-secondary)";
    }
  }

  function chipLabel(path: string): string {
    return path.split("/").pop() ?? path;
  }

  function toggleAsOf() {
    asOfOpen = !asOfOpen;
    if (!asOfOpen) asOf = "";
  }
</script>

<div class="screen">
  <div class="inner">
    <div class="head">
      <h1>Timeline</h1>
      {#if model?.builtAt}
        <span class="when">built {timeAgo(model.builtAt)}</span>
      {/if}
      <span class="spacer"></span>
      {#if !knowledge.empty}
        <button class="asof" class:active={asOfOpen} onclick={toggleAsOf}>
          View as of… ◷
        </button>
      {/if}
      <button
        class="btn btn-small"
        disabled={knowledge.building}
        onclick={() => void knowledge.refresh()}
      >
        {knowledge.building ? "Rebuilding…" : "Refresh"}
      </button>
    </div>

    {#if knowledge.error}
      <div class="error">Last refresh didn't finish — {knowledge.error}</div>
    {/if}

    {#if knowledge.empty}
      <div class="empty-card">
        <h2>Ken hasn't mapped this project yet</h2>
        <p>
          One pass over your documents turns dated decisions and changes
          into a timeline you can search and rewind.
        </p>
        {#if knowledge.building}
          <p class="pulse">Ken is mapping the project…</p>
        {:else if knowledge.claudeFound}
          <button class="btn btn-primary" onclick={() => void knowledge.refresh()}>
            Build the timeline
          </button>
        {:else}
          <p class="note">
            The timeline needs Claude Code — install with
            <span class="mono">npm i -g @anthropic-ai/claude-code</span>, then
            log in once with <span class="mono">claude</span>.
          </p>
        {/if}
      </div>
    {:else}
      {#if asOfOpen}
        <div class="asof-row">
          <label for="asof-date">Show what was known on or before</label>
          <input id="asof-date" type="date" bind:value={asOf} />
        </div>
      {/if}

      <div class="search">
        <span class="lens" aria-hidden="true"></span>
        <input
          bind:value={query}
          placeholder="Search events…"
          spellcheck="false"
        />
        <span class="count">{filtered.length} of {allEvents.length} events</span>
      </div>

      <div class="chips">
        <button
          class="cat-chip"
          class:on={category === null}
          onclick={() => (category = null)}
        >
          All
        </button>
        {#each categories as cat (cat)}
          <button
            class="cat-chip"
            class:on={category === cat}
            onclick={() => (category = category === cat ? null : cat)}
          >
            {cat}
          </button>
        {/each}
      </div>

      {#if filtered.length === 0}
        <div class="no-match">No events match.</div>
      {:else}
        <div class="timeline">
          <div class="line"></div>
          {#each filtered as ev, i (ev.id)}
            <div class="event">
              <span class="dot" class:newest={i === 0}></span>
              <div class="meta">
                <span class="date" class:newest-date={i === 0}>{dateLabel(ev.date)}</span>
                <span class="pill" style={pillStyle(ev.category)}>{ev.category}</span>
              </div>
              <div class="body" class:dim={i !== 0}>
                {@html highlightMatches(ev.text, query)}
                {#if ev.source}
                  <button
                    class="src mono"
                    title={ev.source}
                    onclick={() => app.openInFiles(ev.source)}
                  >
                    {chipLabel(ev.source)}
                  </button>
                {/if}
              </div>
            </div>
          {/each}
        </div>
      {/if}
    {/if}
  </div>
</div>

<style>
  .screen {
    flex: 1;
    min-width: 0;
    overflow-y: auto;
    padding: 36px 44px;
  }
  .inner {
    max-width: 760px;
    margin: 0 auto;
    display: flex;
    flex-direction: column;
    gap: 16px;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  h1 {
    margin: 0;
    font-family: var(--font-serif);
    font-size: 28px;
    font-weight: 500;
    letter-spacing: -0.01em;
  }
  .when {
    font-size: 12px;
    color: var(--ink-tertiary);
  }
  .spacer {
    flex: 1;
  }
  .error {
    font-size: 12.5px;
    color: var(--danger);
  }

  .asof {
    font-size: 12px;
    color: var(--accent);
    font-weight: 600;
    border: 1px solid rgba(138, 90, 68, 0.35);
    border-radius: 7px;
    padding: 5px 12px;
    background: rgba(138, 90, 68, 0.06);
  }
  .asof:hover,
  .asof.active {
    background: rgba(138, 90, 68, 0.12);
  }
  .asof-row {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 12.5px;
    color: var(--ink-secondary);
  }
  .asof-row input {
    font-family: inherit;
    font-size: 12.5px;
    color: var(--ink);
    border: 1px solid var(--border-strong);
    border-radius: 7px;
    padding: 4px 8px;
    background: var(--surface);
  }

  .search {
    height: 38px;
    border: 1px solid var(--border-strong);
    border-radius: 9px;
    background: var(--surface);
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 0 14px;
    box-shadow: var(--shadow-control);
  }
  .lens {
    width: 12px;
    height: 12px;
    border: 1.5px solid var(--ink-tertiary);
    border-radius: 50%;
    position: relative;
    flex: none;
  }
  .lens::after {
    content: "";
    position: absolute;
    width: 1.5px;
    height: 5px;
    background: var(--ink-tertiary);
    transform: rotate(-45deg);
    top: 9px;
    left: 11px;
  }
  .search input {
    flex: 1;
    min-width: 0;
    border: none;
    outline: none;
    background: transparent;
    font-family: inherit;
    font-size: 13px;
    color: var(--ink);
  }
  .count {
    font-size: 11.5px;
    color: var(--ink-tertiary);
    white-space: nowrap;
  }

  .chips {
    display: flex;
    gap: 7px;
    flex-wrap: wrap;
  }
  .cat-chip {
    font-size: 11.5px;
    font-weight: 600;
    padding: 4px 12px;
    border-radius: 12px;
    border: 1px solid var(--border-strong);
    background: var(--surface);
    color: var(--ink-secondary);
  }
  .cat-chip:hover {
    background: var(--paper);
  }
  .cat-chip.on {
    background: var(--ink);
    border-color: var(--ink);
    color: var(--paper);
  }

  .no-match {
    font-size: 13px;
    color: var(--ink-tertiary);
    padding: 12px 0;
  }

  .timeline {
    position: relative;
    padding-left: 26px;
    display: flex;
    flex-direction: column;
  }
  .line {
    position: absolute;
    left: 7px;
    top: 8px;
    bottom: 8px;
    width: 2px;
    background: var(--border);
    border-radius: 1px;
  }
  .event {
    position: relative;
    padding: 0 0 22px;
  }
  .event:last-child {
    padding-bottom: 0;
  }
  .dot {
    position: absolute;
    left: -25px;
    top: 4px;
    width: 12px;
    height: 12px;
    border-radius: 6px;
    background: var(--surface);
    border: 2.5px solid var(--paper);
    box-shadow: 0 0 0 1.5px var(--border-strong);
  }
  .dot.newest {
    background: var(--accent);
    box-shadow: 0 0 0 1.5px var(--accent);
  }
  .meta {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .date {
    font-size: 11.5px;
    font-weight: 700;
    color: var(--ink-tertiary);
    letter-spacing: 0.05em;
  }
  .newest-date {
    color: var(--accent-deep);
  }
  .pill {
    font-size: 10px;
    font-weight: 600;
    padding: 1px 7px;
    border-radius: 8px;
  }
  .body {
    font-size: 13.5px;
    line-height: 1.65;
    margin-top: 3px;
  }
  .body.dim {
    color: var(--ink-secondary);
  }
  .body :global(mark) {
    background: var(--match-highlight);
    border-radius: 3px;
    padding: 0 3px;
    color: inherit;
  }
  .src {
    font-size: 11px;
    color: var(--ink-secondary);
    border: 1px solid var(--border);
    border-radius: 5px;
    padding: 1px 6px;
    background: var(--surface);
    margin-left: 4px;
  }
  .src:hover {
    border-color: var(--border-strong);
    color: var(--ink);
  }

  .empty-card {
    max-width: 420px;
    margin: 48px auto 0;
    text-align: center;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-card);
    padding: 28px 32px;
    box-shadow: var(--shadow-card);
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 12px;
  }
  .empty-card h2 {
    margin: 0;
    font-family: var(--font-serif);
    font-size: 19px;
    font-weight: 500;
  }
  .empty-card p {
    margin: 0;
    font-size: 13.5px;
    color: var(--ink-secondary);
  }
  .note {
    font-size: 12.5px;
  }
  .pulse {
    color: var(--accent);
    animation: pulse 1.6s ease-in-out infinite;
  }
  @keyframes pulse {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.55; }
  }
</style>

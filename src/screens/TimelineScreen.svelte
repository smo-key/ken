<script lang="ts">
  import { onMount } from "svelte";
  import { app } from "../lib/app.svelte";
  import { knowledge } from "../lib/knowledge.svelte";
  import { highlightMatches } from "../lib/knowledge";
  import { partitionTimeline } from "../lib/timeline";
  import Search from "@lucide/svelte/icons/search";
  import ChevronRight from "@lucide/svelte/icons/chevron-right";

  onMount(() => void knowledge.visit());

  let query = $state("");
  let category = $state<string | null>(null);
  /** User's manual collapse state for the future group; default collapsed. */
  let futureOpen = $state(false);

  /** yyyy-mm-dd for "now"; the real clock stays out of the pure partition. */
  const today = new Date().toLocaleDateString("en-CA");

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
        (!category || ev.category === category) &&
        (!q || ev.text.toLowerCase().includes(q)),
    );
  });

  const groups = $derived(partitionTimeline(filtered, today));

  // A live search or category filter must be able to surface future matches,
  // so it force-expands the future group regardless of the manual toggle.
  const filtering = $derived(query.trim() !== "" || category !== null);
  const showFuture = $derived(futureOpen || filtering);

  const MONTHS = ["JAN", "FEB", "MAR", "APR", "MAY", "JUN",
    "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"];

  function todayLabel(): string {
    const [y, m, d] = today.split("-").map(Number);
    return `${MONTHS[(m ?? 1) - 1] ?? "—"} ${d ?? "?"}, ${y}`;
  }

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
        return "background:color-mix(in srgb, var(--healthy) 12%, transparent);color:var(--healthy-text)";
      case "people":
      case "person":
        return "background:color-mix(in srgb, var(--accent) 10%, transparent);color:var(--accent-deep)";
      case "vendor":
        return "background:color-mix(in srgb, var(--needs-input) 12%, transparent);color:var(--needs-input-text)";
      default:
        return "background:var(--sunken);color:var(--ink-secondary)";
    }
  }

  function chipLabel(path: string): string {
    return path.split("/").pop() ?? path;
  }
</script>

<div class="screen">
  <div class="inner">
    <div class="head">
      <h1>Timeline</h1>
    </div>

    {#if knowledge.error}
      <div class="error">Last refresh didn't finish — {knowledge.error}</div>
    {/if}

    {#if knowledge.empty}
      <div class="empty-card">
        <h2>
          {knowledge.building
            ? "Ken is mapping this project…"
            : "Ken hasn't mapped this project yet"}
        </h2>
        <p>
          One pass over your documents turns dated decisions and changes
          into a timeline you can search and rewind.
        </p>
        {#if knowledge.building}
          <p class="pulse">
            This runs on its own after a project opens — a few minutes.
          </p>
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
      <div class="search">
        <Search class="lens" size={14} strokeWidth={1.75} aria-hidden="true" />
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
        {#snippet eventRow(ev: (typeof filtered)[number])}
          <div class="event">
            <span class="dot" class:on-today={ev.date === today}></span>
            <div class="meta">
              <span class="date">{dateLabel(ev.date)}</span>
              <span class="pill" style={pillStyle(ev.category)}>{ev.category}</span>
            </div>
            <div class="body">
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
        {/snippet}

        <div class="timeline">
          <div class="line"></div>

          {#if groups.future.length > 0}
            <button
              class="collapse"
              aria-expanded={showFuture}
              onclick={() => (futureOpen = !showFuture)}
            >
              <ChevronRight class="chev" size={14} strokeWidth={2} aria-hidden="true" />
              <span>
                {groups.future.length} upcoming event{groups.future.length === 1 ? "" : "s"}
                {#if filtering}<span class="hint">(matching)</span>{/if}
              </span>
            </button>
            {#if showFuture}
              {#each groups.future as ev (ev.id)}
                {@render eventRow(ev)}
              {/each}
            {/if}
          {/if}

          {#if groups.hasDated}
            <div class="event today-marker">
              <span class="dot today"></span>
              <div class="meta">
                <span class="date today-label">Today — {todayLabel()}</span>
              </div>
            </div>
          {/if}

          {#each groups.visible as ev (ev.id)}
            {@render eventRow(ev)}
          {/each}

          {#each groups.undated as ev (ev.id)}
            {@render eventRow(ev)}
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
  }
  h1 {
    margin: 0;
    font-family: var(--font-serif);
    font-size: 28px;
    font-weight: 500;
    letter-spacing: -0.01em;
  }
  .error {
    font-size: 12.5px;
    color: var(--danger);
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
  .search :global(.lens) {
    color: var(--ink-tertiary);
    flex: none;
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
  .dot.on-today {
    background: var(--accent);
    box-shadow: 0 0 0 1.5px var(--accent);
  }
  /* The "now" marker: a filled accent dot with a soft halo to read as the
     current moment rather than another dated entry. */
  .dot.today {
    background: var(--accent);
    border-color: var(--paper);
    box-shadow: 0 0 0 1.5px var(--accent), 0 0 0 5px color-mix(in srgb, var(--accent) 18%, transparent);
  }
  .today-marker {
    padding-bottom: 22px;
  }
  .today-label {
    color: var(--accent-deep);
  }

  .collapse {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    align-self: flex-start;
    margin-bottom: 22px;
    padding: 3px 4px;
    background: transparent;
    border: none;
    font-family: inherit;
    font-size: 12px;
    font-weight: 600;
    color: var(--ink-secondary);
    cursor: pointer;
  }
  .collapse:hover {
    color: var(--ink);
  }
  .collapse :global(.chev) {
    transition: transform 0.12s ease;
    flex: none;
  }
  .collapse[aria-expanded="true"] :global(.chev) {
    transform: rotate(90deg);
  }
  .hint {
    color: var(--ink-tertiary);
    font-weight: 500;
    margin-left: 3px;
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

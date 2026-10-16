<script lang="ts">
  // The Log (knowledge-core decision 37): the project's RAID register, not a
  // feed of every update. Risks and issues from Current/RAID.md, actions
  // (open tickets with an assignee), decisions (the decisions log's rulings),
  // and milestones (milestone tickets, the roadmap, releases). Each item
  // shows its status, owner and date and opens its source; closed items fold
  // away under their group. Read from the workspace's knowledge base, no
  // model. Ingests, drift, lint and commits stay in the Inbox, Settings and
  // log.md.
  import { untrack } from "svelte";
  import { api, type RaidItem, type RaidLog } from "../lib/api";
  import { app } from "../lib/app.svelte";
  import { counts, highlightMatches, statusTone, viewOf } from "../lib/log";
  import { renderTitle } from "../lib/markdown";
  import EmptyState from "../lib/ui/EmptyState.svelte";
  import ScreenHeader from "../lib/ui/ScreenHeader.svelte";
  import Search from "@lucide/svelte/icons/search";
  import RefreshCw from "@lucide/svelte/icons/refresh-cw";

  let log = $state<RaidLog | null>(null);
  let error = $state<string | null>(null);
  let loading = $state(false);
  let query = $state("");

  async function load() {
    loading = true;
    try {
      log = await api.raidRegister();
      error = null;
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  // Read again each time the screen is shown, and when the focused repo
  // changes while it is.
  $effect(() => {
    if (app.screen !== "log") return;
    void app.focused;
    untrack(() => void load());
  });

  const groups = $derived(log ? viewOf(log.register, query) : []);
  const count = $derived(log ? counts(log.register, query) : { shown: 0, total: 0 });
  const searching = $derived(query.trim() !== "");

  const MONTHS = ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"];

  function dateLabel(date: string): string {
    const [y, m, d] = date.split("-").map(Number);
    const label = `${MONTHS[(m ?? 1) - 1] ?? "—"} ${d ?? "?"}`;
    return y === new Date().getFullYear() ? label : `${label}, ${y}`;
  }

  function fileName(path: string): string {
    return path.split("/").pop() ?? path;
  }

  /** Open an item's source in Files, in the repo the register was read from. */
  async function open(it: RaidItem) {
    const projectId = log?.projectId;
    if (!it.path || !projectId) return;
    if (projectId !== app.focused && app.workspace?.members.some((m) => m.projectId === projectId)) {
      await app.focusMember(projectId);
    }
    app.openAt(it.path, { line: it.line ?? undefined });
  }
</script>

{#snippet row(it: RaidItem, milestone: boolean)}
  <div class="item" class:closed={it.closed}>
    {#if it.id}<span class="id mono">{@html highlightMatches(it.id, query)}</span>{/if}
    <div class="main">
      <div class="title">{@html renderTitle(it.title, (s) => highlightMatches(s, query))}</div>
      <div class="meta">
        <span class="pill {statusTone(it)}">{it.status}</span>
        {#if it.owner}<span class="owner">{it.owner}</span>{/if}
        {#if it.date}<span class="date">{dateLabel(it.date)}</span>{/if}
        {#if milestone}<span class="from">{it.kind}</span>{/if}
        {#if it.repo}<span class="repo">{it.repo}</span>{/if}
        {#if it.path}
          <button class="src mono" title={it.path} onclick={() => void open(it)}>{fileName(it.path)}</button>
        {/if}
      </div>
    </div>
  </div>
{/snippet}

<div class="screen">
  <div class="inner">
    <ScreenHeader title="Log" sub={log?.name} count={log && count.total > 0 ? count : null}>
      {#snippet actions()}
        <button class="refresh" title="Read again" aria-label="Read again" disabled={loading} onclick={() => void load()}>
          <RefreshCw size={14} strokeWidth={1.75} aria-hidden="true" />
        </button>
      {/snippet}
    </ScreenHeader>

    {#if error}
      <div class="error">The log could not be read: {error}</div>
    {/if}

    {#if log && count.total === 0}
      <EmptyState title="Nothing in the log yet" />
    {:else if log}
      <div class="search">
        <Search class="lens" size={14} strokeWidth={1.75} aria-hidden="true" />
        <input bind:value={query} placeholder="Search the log…" spellcheck="false" />
      </div>

      {#each groups as g (g.key)}
        <section class="group">
          <div class="ghead">
            <h2>{g.label}</h2>
            <span class="gcount">{g.open.length}</span>
          </div>
          {#if g.open.length === 0}
            <EmptyState />
          {/if}
          {#each g.open as it, i (`${g.key}-${i}`)}
            {@render row(it, g.key === "milestones")}
          {/each}
          {#if g.closed.length > 0}
            <details class="folded" open={searching}>
              <summary>{g.closed.length} {g.closedLabel}</summary>
              {#each g.closed as it, i (`${g.key}-c${i}`)}
                {@render row(it, g.key === "milestones")}
              {/each}
            </details>
          {/if}
        </section>
      {/each}
    {/if}
  </div>
</div>

<style>
  .screen {
    flex: 1;
    min-width: 0;
    overflow-y: auto;
    padding: 24px clamp(24px, 4vw, 56px) 48px;
  }
  .inner {
    max-width: 760px;
    margin: 0 auto;
    display: flex;
    flex-direction: column;
    gap: 16px;
  }
  .refresh {
    display: inline-flex;
    padding: 5px;
    border-radius: 6px;
    border: 1px solid var(--border);
    background: var(--surface);
    color: var(--ink-secondary);
  }
  .refresh:hover:not(:disabled) {
    color: var(--ink);
    border-color: var(--border-strong);
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
  .group {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .ghead {
    display: flex;
    align-items: baseline;
    gap: 8px;
    padding-bottom: 6px;
    border-bottom: 1px solid var(--border);
    margin-bottom: 4px;
  }
  .ghead h2 {
    margin: 0;
    font-family: var(--font-serif);
    font-size: 19px;
    font-weight: 500;
  }
  .gcount {
    font-size: 11.5px;
    font-weight: 600;
    color: var(--ink-tertiary);
  }
  .item {
    display: flex;
    gap: 12px;
    padding: 8px 0;
  }
  .item.closed .title {
    color: var(--ink-secondary);
  }
  .id {
    flex: none;
    min-width: 58px;
    font-size: 11.5px;
    color: var(--ink-tertiary);
    padding-top: 2px;
  }
  .main {
    flex: 1;
    min-width: 0;
  }
  .title {
    font-size: 13.5px;
    line-height: 1.5;
    overflow-wrap: anywhere;
  }
  .title :global(mark),
  .id :global(mark) {
    background: var(--match-highlight);
    border-radius: 3px;
    padding: 0 3px;
    color: inherit;
  }
  .meta {
    display: flex;
    align-items: baseline;
    flex-wrap: wrap;
    gap: 8px;
    margin-top: 3px;
    font-size: 11px;
    color: var(--ink-tertiary);
  }
  .pill {
    font-size: 10px;
    font-weight: 600;
    padding: 1px 7px;
    border-radius: 8px;
    background: var(--sunken);
    color: var(--ink-secondary);
  }
  .pill.attn {
    background: color-mix(in srgb, var(--needs-input) 12%, transparent);
    color: var(--needs-input-text);
  }
  .pill.ok {
    background: color-mix(in srgb, var(--healthy) 12%, transparent);
    color: var(--healthy-text);
  }
  .pill.muted {
    background: transparent;
    border: 1px solid var(--border);
    color: var(--ink-tertiary);
  }
  .owner,
  .repo {
    color: var(--ink-secondary);
  }
  .src {
    font-size: 11px;
    color: var(--ink-secondary);
    border: 1px solid var(--border);
    border-radius: 5px;
    padding: 1px 6px;
    background: var(--surface);
  }
  .src:hover {
    border-color: var(--border-strong);
    color: var(--ink);
  }

  .folded summary {
    cursor: pointer;
    font-size: 12px;
    color: var(--ink-tertiary);
    padding: 6px 0;
  }
  .folded summary:hover {
    color: var(--ink-secondary);
  }
</style>

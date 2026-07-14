<script lang="ts">
  // The home footer: a few at-a-glance stats and where team-sync stands. All
  // numbers are derived live from `app.files`; the counting/copy lives in
  // ./homeStatus so it can be unit-tested.
  import { app } from "../lib/app.svelte";
  import { timeAgo } from "../lib/format";
  import { cloudPresentation, fileStats, syncPresentation } from "./homeStatus";

  const stats = $derived(fileStats(app.files));
  const cloud = $derived(cloudPresentation(stats.cloudOnly, app.backgroundIndex));
  const sync = $derived(syncPresentation(app.syncState, app.syncDetail));
</script>

<div class="footer">
  {#if app.scanning}
    <p class="first-run">
      Ken is reading <strong>{app.project?.name}</strong> for the first time —
      search lights up as files are indexed.
    </p>
  {:else}
    <div class="stats">
      <div class="tile">
        <span class="num">{stats.total}</span>
        <span class="lbl">{stats.total === 1 ? "file" : "files"}</span>
      </div>
      <div class="tile">
        <span class="num">{stats.searchable}</span>
        <span class="lbl">searchable</span>
      </div>
      {#if cloud.show}
        <div class="tile" class:indexing={cloud.active}>
          <span class="num">{cloud.count}</span>
          <span class="lbl">{cloud.label}</span>
        </div>
      {/if}
      {#if stats.unreadable > 0}
        <div class="tile">
          <span class="num">{stats.unreadable}</span>
          <span class="lbl">unreadable</span>
        </div>
      {/if}
    </div>

    <div class="sync">
      <span class="dot {sync.tone}" title={sync.label}></span>
      <span class="sync-label">{sync.label}</span>
      {#if app.lastScanAt}
        <span class="updated">
          Updated {timeAgo(Math.floor(app.lastScanAt / 1000))}
        </span>
      {/if}
    </div>
  {/if}
</div>

<style>
  .footer {
    padding: 16px 18px;
    background: var(--sunken-2);
    border: 1px solid var(--border);
    border-radius: 10px;
  }
  .first-run {
    margin: 0;
    font-size: 13px;
    line-height: 1.6;
    color: var(--ink-secondary);
  }
  .first-run strong {
    color: var(--ink);
  }

  .stats {
    display: flex;
    flex-wrap: wrap;
    gap: 28px;
  }
  .tile {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .num {
    font-family: var(--font-serif);
    font-size: 22px;
    font-weight: 500;
    line-height: 1.1;
    color: var(--ink);
  }
  .lbl {
    font-size: 11.5px;
    color: var(--ink-tertiary);
  }
  /* Cloud files being pulled down read as live work, not a static backlog. */
  .tile.indexing .num {
    color: var(--accent);
  }
  .tile.indexing .lbl {
    color: var(--accent);
    animation: indexing-pulse 1.4s ease-in-out infinite;
  }
  @keyframes indexing-pulse {
    50% {
      opacity: 0.5;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .tile.indexing .lbl {
      animation: none;
    }
  }

  .sync {
    display: flex;
    align-items: center;
    gap: 9px;
    margin-top: 16px;
    padding-top: 14px;
    border-top: 1px solid var(--border);
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 4px;
    flex: none;
  }
  .dot.healthy {
    background: var(--healthy);
  }
  .dot.progress {
    background: var(--accent);
    animation: sync-pulse 1.2s ease-in-out infinite;
  }
  .dot.attention {
    background: var(--needs-input);
  }
  .dot.muted {
    background: var(--ink-tertiary);
    opacity: 0.5;
  }
  @keyframes sync-pulse {
    50% {
      opacity: 0.35;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .dot.progress {
      animation: none;
    }
  }
  .sync-label {
    font-size: 13px;
    color: var(--ink-secondary);
  }
  .updated {
    margin-left: auto;
    font-size: 11.5px;
    color: var(--ink-tertiary);
  }
</style>

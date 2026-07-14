<script lang="ts">
  // "Pick up where you left off": the user's own recently opened files, or —
  // on a project with no history yet — whatever changed most recently.
  import { app } from "../lib/app.svelte";
  import type { RecentFile } from "../lib/recent";
  import { timeAgo } from "../lib/format";
  import FileGlyph from "../files/FileGlyph.svelte";

  // `fromHistory` is false on a project with no open-history yet, where `rows`
  // are the most recently *modified* files — label that honestly.
  let { rows, fromHistory }: { rows: RecentFile[]; fromHistory: boolean } =
    $props();

  const name = (relPath: string) => relPath.split("/").pop() || relPath;
  const dir = (relPath: string) =>
    relPath.includes("/") ? relPath.slice(0, relPath.lastIndexOf("/")) : "";
</script>

<div class="overline">
  {fromHistory ? "Pick up where you left off" : "Recently changed"}
</div>
<div class="group">
  {#each rows as row (row.relPath)}
    <button
      class="row"
      title={row.relPath}
      onclick={() => app.openInFiles(row.relPath)}
    >
      <FileGlyph kind={row.kind} />
      <span class="rbody">
        <span class="rname">{name(row.relPath)}</span>
        {#if dir(row.relPath)}
          <span class="rdir mono">{dir(row.relPath)}</span>
        {/if}
      </span>
      <span class="rtime">{timeAgo(row.at)}</span>
    </button>
  {/each}
</div>

<style>
  /* Typography lives on the global `.overline`; only the spacing is local. */
  .overline {
    margin-bottom: 8px;
  }
  .group {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-card);
    box-shadow: var(--shadow-card);
    overflow: hidden;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 12px;
    width: 100%;
    padding: 10px 16px;
    border: none;
    background: transparent;
    text-align: left;
  }
  .row + .row {
    border-top: 1px solid var(--border);
  }
  /* Same row tint as the search hits above it and the ⌘K palette. */
  .row:hover,
  .row:focus-visible {
    background: color-mix(in srgb, var(--accent) 7%, transparent);
  }
  .rbody {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .rname {
    font-size: 13px;
    color: var(--ink);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .rdir {
    font-size: 11.5px;
    color: var(--ink-tertiary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .rtime {
    flex: none;
    font-size: 11.5px;
    color: var(--ink-tertiary);
  }
</style>

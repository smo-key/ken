<script lang="ts">
  import { app } from "../lib/app.svelte";
  import { buildTree } from "../lib/tree";
  import { timeAgo } from "../lib/format";
  import TreeNodeRow from "./TreeNodeRow.svelte";

  const tree = $derived(buildTree(app.files, app.folders));
</script>

<div class="tree">
  <div class="tree-head">
    Files
    <button class="manage" onclick={() => (app.screen = "settings")}>Manage</button>
  </div>
  <div class="nodes">
    {#each tree as node (node.relPath)}
      <TreeNodeRow {node} depth={0} />
    {/each}
    {#if tree.length === 0}
      <div class="empty">
        {app.scanning ? "Reading your folder…" : "This folder is empty."}
      </div>
    {/if}
  </div>
  <div class="foot">
    <span class="dot" class:busy={app.scanning}></span>
    {#if app.scanning}
      Indexing…
    {:else if app.lastScanAt}
      Watching · updated {timeAgo(Math.floor(app.lastScanAt / 1000))}
    {:else}
      Watching
    {/if}
  </div>
</div>

<style>
  .tree {
    flex: 0 1 264px;
    min-width: 190px;
    border-right: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    padding: 12px 8px 10px;
    overflow-y: auto;
    overflow-x: hidden;
    font-size: 13px;
  }
  .tree-head {
    display: flex;
    align-items: center;
    padding: 2px 10px 8px;
    font-size: 11px;
    font-weight: 700;
    color: var(--ink-tertiary);
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }
  .manage {
    margin-left: auto;
    font-size: 11.5px;
    color: var(--accent);
    font-weight: 600;
    text-transform: none;
    letter-spacing: 0;
    border: none;
    background: none;
    padding: 0;
  }
  .nodes {
    flex: 1;
  }
  .empty {
    padding: 8px 10px;
    color: var(--ink-tertiary);
    font-size: 12.5px;
  }
  .foot {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 10px 2px;
    font-size: 12px;
    color: var(--ink-secondary);
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 4px;
    background: var(--healthy);
    flex: none;
  }
  .dot.busy {
    background: var(--accent);
    animation: pulse 1.2s ease-in-out infinite;
  }
  @keyframes pulse {
    50% {
      opacity: 0.35;
    }
  }
</style>

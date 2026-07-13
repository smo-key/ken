<script lang="ts">
  import { app } from "../lib/app.svelte";
  import type { TreeNode } from "../lib/tree";
  import FileGlyph from "./FileGlyph.svelte";
  import TreeNodeRow from "./TreeNodeRow.svelte";

  let { node, depth }: { node: TreeNode; depth: number } = $props();
  let open = $state(depth < 1);

  const isFolder = $derived(node.file === undefined);
</script>

{#if isFolder}
  <button
    class="row folder"
    class:excluded={node.excluded}
    style:padding-left={`${8 + depth * 18}px`}
    onclick={() => (open = !open)}
  >
    <span class="chev">{open ? "▾" : "▸"}</span>
    <span class="folder-icon" aria-hidden="true"><span class="tab"></span></span>
    <span class="name">{node.name}</span>
    {#if node.excluded}
      <span class="tag">excluded</span>
    {/if}
  </button>
  {#if open && !node.excluded}
    {#each node.children as child (child.relPath)}
      <TreeNodeRow node={child} depth={depth + 1} />
    {/each}
  {/if}
{:else}
  <button
    class="row file"
    class:selected={app.openFile === node.relPath}
    class:failed={node.file?.status === "failed"}
    style:padding-left={`${8 + depth * 18 + 10}px`}
    onclick={() => (app.openFile = node.relPath)}
    title={node.file?.status === "failed"
      ? `Not indexed — ${node.file.error ?? "unknown reason"}`
      : node.relPath}
  >
    <FileGlyph kind={node.file?.kind ?? "binary"} size="sm" />
    <span class="name">{node.name}</span>
    {#if node.file?.status === "failed"}
      <span class="fail-dot" title={node.file.error ?? "not indexed"}></span>
    {/if}
  </button>
{/if}

<style>
  .row {
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 4px 8px;
    border-radius: 6px;
    width: 100%;
    border: none;
    background: transparent;
    font-size: 13px;
    color: var(--ink);
    text-align: left;
    cursor: pointer;
  }
  .row:hover {
    background: var(--sunken);
  }
  .row.selected {
    background: rgba(138, 90, 68, 0.1);
    color: var(--accent-deep);
    font-weight: 600;
  }
  .row.excluded {
    color: var(--ink-tertiary);
    opacity: 0.65;
  }
  .chev {
    width: 10px;
    color: var(--ink-tertiary);
    font-size: 9px;
    flex: none;
  }
  .folder-icon {
    position: relative;
    width: 15px;
    height: 11px;
    flex: none;
    background: #c2a878;
    border-radius: 2px;
  }
  .folder-icon .tab {
    position: absolute;
    top: -3px;
    left: 0;
    width: 7px;
    height: 4px;
    background: inherit;
    border-radius: 2px 2px 0 0;
  }
  .excluded .folder-icon {
    background: var(--border-strong);
  }
  .name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .tag {
    margin-left: auto;
    font-size: 10px;
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 0 5px;
    flex: none;
  }
  .fail-dot {
    margin-left: auto;
    width: 6px;
    height: 6px;
    border-radius: 3px;
    background: var(--danger);
    flex: none;
  }
</style>

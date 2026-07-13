<script lang="ts">
  import ChevronRight from "@lucide/svelte/icons/chevron-right";
  import ChevronDown from "@lucide/svelte/icons/chevron-down";
  import Star from "@lucide/svelte/icons/star";
  import StarOff from "@lucide/svelte/icons/star-off";
  import ExternalLink from "@lucide/svelte/icons/external-link";
  import SquareArrowOutUpRight from "@lucide/svelte/icons/square-arrow-out-up-right";
  import { app } from "../lib/app.svelte";
  import { api } from "../lib/api";
  import type { TreeNode } from "../lib/tree";
  import { openContextMenu, type MenuEntry } from "../lib/ui/ContextMenu.svelte";
  import { canDrop, drag, parentOf } from "./dnd.svelte";
  import FileGlyph from "./FileGlyph.svelte";
  import TreeNodeRow from "./TreeNodeRow.svelte";

  let { node, depth }: { node: TreeNode; depth: number } = $props();
  let open = $state(depth < 1);

  const isFolder = $derived(node.file === undefined);
  const isDropTarget = $derived(drag.over === node.relPath);

  // Auto-expand when a reveal request targets this folder or something inside it.
  $effect(() => {
    app.revealNonce; // re-run each reveal, even if the target is unchanged
    const t = app.revealTarget;
    if (isFolder && t && (t === node.relPath || t.startsWith(node.relPath + "/"))) {
      open = true;
    }
  });

  function rowMenu(e: MouseEvent) {
    e.preventDefault();
    const fav = app.isFavorite(node.relPath);
    const kind = isFolder ? "folder" : "file";
    const items: MenuEntry[] = [
      {
        label: isFolder ? "Expand" : "Open",
        icon: SquareArrowOutUpRight,
        onSelect: () =>
          isFolder ? (open = true) : app.openTab(node.relPath, true),
      },
      {
        label: "Open in default app",
        icon: ExternalLink,
        onSelect: () => void api.openExternal(node.relPath),
      },
      "separator",
      fav
        ? {
            label: "Remove from favorites",
            icon: StarOff,
            onSelect: () => app.removeFavorite(node.relPath),
          }
        : {
            label: "Add to favorites",
            icon: Star,
            onSelect: () => app.toggleFavorite(node.relPath, kind),
          },
    ];
    openContextMenu(e.clientX, e.clientY, items);
  }

  async function doMove(from: string, folder: string) {
    const base = from.split("/").pop()!;
    const to = folder ? `${folder}/${base}` : base;
    drag.error = null;
    try {
      await app.moveFile(from, to);
    } catch (e) {
      drag.error = String(e);
    }
    drag.reset();
  }

  // Folder drop-target handlers.
  const droppable = $derived(isFolder && !node.excluded);
  function onDragOver(e: DragEvent) {
    if (!droppable || !canDrop(node.relPath)) return;
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = "move";
    drag.over = node.relPath;
  }
  function onDragLeave() {
    if (drag.over === node.relPath) drag.over = null;
  }
  function onDrop(e: DragEvent) {
    if (!droppable || !canDrop(node.relPath)) return;
    e.preventDefault();
    const from = drag.from;
    drag.over = null;
    if (from) void doMove(from, node.relPath);
  }
</script>

{#if isFolder}
  <button
    class="row folder"
    class:excluded={node.excluded}
    class:drop-target={isDropTarget}
    style:padding-left={`${8 + depth * 18}px`}
    onclick={() => (open = !open)}
    oncontextmenu={rowMenu}
    ondragover={onDragOver}
    ondragleave={onDragLeave}
    ondrop={onDrop}
  >
    <span class="chev">
      {#if open}<ChevronDown size={14} strokeWidth={1.75} />{:else}<ChevronRight size={14} strokeWidth={1.75} />{/if}
    </span>
    <FileGlyph kind={open ? "folder-open" : "folder"} size="sm" />
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
    draggable="true"
    onclick={() => app.openTab(node.relPath, false)}
    ondblclick={() => app.makeTabPersistent(node.relPath)}
    oncontextmenu={rowMenu}
    ondragstart={(e) => {
      drag.from = node.relPath;
      if (e.dataTransfer) {
        e.dataTransfer.effectAllowed = "move";
        e.dataTransfer.setData("text/plain", node.relPath);
      }
    }}
    ondragend={() => drag.reset()}
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
    border: 1px solid transparent;
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
  .row.drop-target {
    background: rgba(138, 90, 68, 0.12);
    border-color: var(--accent);
  }
  .chev {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 14px;
    color: var(--ink-tertiary);
    flex: none;
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

<script lang="ts">
  import SlidersHorizontal from "@lucide/svelte/icons/sliders-horizontal";
  import StarOff from "@lucide/svelte/icons/star-off";
  import Eye from "@lucide/svelte/icons/eye";
  import X from "@lucide/svelte/icons/x";
  import { app } from "../lib/app.svelte";
  import { buildTree } from "../lib/tree";
  import {
    openContextMenu,
    type MenuEntry,
  } from "../lib/ui/ContextMenu.svelte";
  import ContextMenu from "../lib/ui/ContextMenu.svelte";
  import { canDrop, drag } from "./dnd.svelte";
  import FileGlyph from "./FileGlyph.svelte";
  import TreeNodeRow from "./TreeNodeRow.svelte";

  const tree = $derived(buildTree(app.files, app.folders));

  function glyphKind(path: string, kind: "file" | "folder"): string {
    if (kind === "folder") return "folder";
    return app.files.find((f) => f.relPath === path)?.kind ?? "binary";
  }

  function openFavorite(path: string, kind: "file" | "folder") {
    if (kind === "file") app.openTab(path, false);
    app.reveal(path);
  }

  function favMenu(e: MouseEvent, path: string, kind: "file" | "folder") {
    e.preventDefault();
    const items: MenuEntry[] = [
      {
        label: "Reveal in files",
        icon: Eye,
        onSelect: () => app.reveal(path),
      },
      "separator",
      {
        label: "Remove from favorites",
        icon: StarOff,
        danger: true,
        onSelect: () => app.removeFavorite(path),
      },
    ];
    openContextMenu(e.clientX, e.clientY, items);
  }

  // Root drop zone — only the empty area of the tree (not rows).
  function onRootDragOver(e: DragEvent) {
    if (e.target !== e.currentTarget || !canDrop("")) return;
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = "move";
    drag.over = "";
  }
  function onRootDragLeave(e: DragEvent) {
    if (e.target === e.currentTarget && drag.over === "") drag.over = null;
  }
  async function onRootDrop(e: DragEvent) {
    if (e.target !== e.currentTarget || !canDrop("")) return;
    e.preventDefault();
    const from = drag.from;
    drag.over = null;
    if (!from) return;
    const base = from.split("/").pop()!;
    drag.error = null;
    try {
      await app.moveFile(from, base);
    } catch (err) {
      drag.error = String(err);
    }
    drag.reset();
  }
</script>

<div class="tree">
  {#if app.favorites.length > 0}
    <div class="tree-head">Favorites</div>
    <div class="favorites">
      {#each app.favorites as fav (fav.path)}
        <button
          class="row fav"
          title={fav.path}
          onclick={() => openFavorite(fav.path, fav.kind)}
          oncontextmenu={(e) => favMenu(e, fav.path, fav.kind)}
        >
          <FileGlyph kind={glyphKind(fav.path, fav.kind)} size="sm" />
          <span class="name">{fav.path.split("/").pop()}</span>
        </button>
      {/each}
    </div>
  {/if}

  <div class="tree-head">
    Files
    <button
      class="icon-btn"
      data-tooltip="Manage folders"
      aria-label="Manage folders"
      onclick={() => (app.screen = "settings")}
    >
      <SlidersHorizontal size={15} strokeWidth={1.75} />
    </button>
  </div>

  {#if drag.error}
    <div class="move-error">
      <span class="etext">{drag.error}</span>
      <button class="edismiss" aria-label="Dismiss" onclick={() => (drag.error = null)}>
        <X size={13} strokeWidth={1.75} />
      </button>
    </div>
  {/if}

  <div
    class="nodes"
    class:drop-root={drag.over === ""}
    role="tree"
    tabindex="-1"
    ondragover={onRootDragOver}
    ondragleave={onRootDragLeave}
    ondrop={onRootDrop}
  >
    {#each tree as node (node.relPath)}
      <TreeNodeRow {node} depth={0} />
    {/each}
    {#if tree.length === 0}
      <div class="empty">
        {app.scanning ? "Reading your folder…" : "This folder is empty."}
      </div>
    {/if}
  </div>
</div>

<ContextMenu />

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
  .favorites {
    margin-bottom: 10px;
  }
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
  .name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .icon-btn {
    position: relative;
    margin-left: auto;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 24px;
    border-radius: 6px;
    border: none;
    background: transparent;
    color: var(--ink-tertiary);
  }
  .icon-btn:hover {
    background: var(--sunken);
    color: var(--ink);
  }
  .icon-btn[data-tooltip]::after {
    content: attr(data-tooltip);
    position: absolute;
    top: calc(100% + 6px);
    left: 50%;
    transform: translateX(-50%);
    background: var(--ink);
    color: var(--surface);
    font-size: 11px;
    font-weight: 500;
    letter-spacing: 0;
    text-transform: none;
    padding: 3px 7px;
    border-radius: 6px;
    white-space: nowrap;
    pointer-events: none;
    opacity: 0;
    transition: opacity 0.12s ease;
    z-index: 50;
  }
  .icon-btn[data-tooltip]:hover::after {
    opacity: 1;
  }
  .nodes {
    flex: 1;
    border-radius: 8px;
  }
  .nodes.drop-root {
    background: rgba(138, 90, 68, 0.06);
    box-shadow: inset 0 0 0 1px var(--accent);
  }
  .empty {
    padding: 8px 10px;
    color: var(--ink-tertiary);
    font-size: 12.5px;
  }
  .move-error {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 0 6px 8px;
    padding: 7px 10px;
    background: rgba(163, 77, 63, 0.08);
    border: 1px solid rgba(163, 77, 63, 0.25);
    border-radius: 8px;
    font-size: 12px;
    color: var(--danger);
  }
  .etext {
    flex: 1;
    line-height: 1.4;
  }
  .edismiss {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border: none;
    background: transparent;
    color: var(--danger);
    padding: 2px;
    flex: none;
  }
</style>

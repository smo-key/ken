<script lang="ts">
  import Upload from "@lucide/svelte/icons/upload";
  import CheckCheck from "@lucide/svelte/icons/check-check";
  import StarOff from "@lucide/svelte/icons/star-off";
  import Eye from "@lucide/svelte/icons/eye";
  import X from "@lucide/svelte/icons/x";
  import FilePlus from "@lucide/svelte/icons/file-plus";
  import FolderPlus from "@lucide/svelte/icons/folder-plus";
  import { app } from "../lib/app.svelte";
  import { imports } from "../lib/imports.svelte";
  import { showMarkAllViewed, showUnreadFilter } from "./filesHeader";
  import { buildTree } from "../lib/tree";
  import {
    openContextMenu,
    type MenuEntry,
  } from "../lib/ui/ContextMenu.svelte";
  import ContextMenu from "../lib/ui/ContextMenu.svelte";
  import { canDrop, drag } from "./dnd.svelte";
  import { treeEdit } from "./treeEdit.svelte";
  import FileGlyph from "./FileGlyph.svelte";
  import InlineNameRow from "./InlineNameRow.svelte";
  import TreeNodeRow from "./TreeNodeRow.svelte";

  let { width }: { width: number } = $props();

  const unreadOnly = $derived(app.filesFilter === "unread");
  // In unread-only view, buildTree gets just the unread files and synthesizes
  // their ancestor folders (folders=[]), so nothing but the changed files and
  // the path to them shows.
  const shownFiles = $derived(
    unreadOnly ? app.files.filter((f) => app.isUnread(f.relPath)) : app.files,
  );
  const tree = $derived(
    buildTree(shownFiles, unreadOnly ? [] : app.folders),
  );

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

  // Right-click on the tree's empty/root area (not a row — rows own their menus).
  function onRootMenu(e: MouseEvent) {
    if (e.target !== e.currentTarget) return;
    e.preventDefault();
    openContextMenu(e.clientX, e.clientY, [
      {
        label: "New document",
        icon: FilePlus,
        onSelect: () => treeEdit.beginCreate("new-document", ""),
      },
      {
        label: "New folder",
        icon: FolderPlus,
        onSelect: () => treeEdit.beginCreate("new-folder", ""),
      },
    ]);
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

<div class="tree" style:width="{width}px">
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

  <div class="tree-head files-head">
    <span class="ttl">Files</span>
    <div class="head-actions">
      {#if showUnreadFilter(app.unread.length, app.filesFilter)}
        <div class="filter" role="tablist" aria-label="Filter files">
          <button
            class="seg"
            class:on={app.filesFilter === "all"}
            role="tab"
            aria-selected={app.filesFilter === "all"}
            onclick={() => (app.filesFilter = "all")}
          >All</button>
          <button
            class="seg"
            class:on={app.filesFilter === "unread"}
            role="tab"
            aria-selected={app.filesFilter === "unread"}
            onclick={() => (app.filesFilter = "unread")}
          >Unread{#if app.unread.length > 0}&nbsp;·&nbsp;{app.unread.length}{/if}</button>
        </div>
      {/if}
      {#if showMarkAllViewed(app.unread.length)}
        <button
          class="icon-btn"
          data-tooltip="Mark every changed file as viewed"
          aria-label="Mark all as viewed"
          onclick={() => void app.markAllSeen()}
        >
          <CheckCheck size={15} strokeWidth={1.75} />
        </button>
      {/if}
      <button
        class="icon-btn"
        data-tooltip="Import file"
        aria-label="Import file"
        disabled={imports.importing}
        onclick={() => void imports.startImport()}
      >
        <Upload size={15} strokeWidth={1.75} />
      </button>
    </div>
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
    oncontextmenu={onRootMenu}
    ondragover={onRootDragOver}
    ondragleave={onRootDragLeave}
    ondrop={onRootDrop}
  >
    {#if (treeEdit.mode === "new-document" || treeEdit.mode === "new-folder") && treeEdit.target === ""}
      <InlineNameRow indent={8} />
    {/if}
    {#each tree as node (node.relPath)}
      <TreeNodeRow {node} depth={0} expandAll={unreadOnly} />
    {/each}
    {#if tree.length === 0}
      <div class="empty">
        {#if unreadOnly}
          Nothing new — every file is up to date.
        {:else}
          {app.scanning ? "Reading your folder…" : "This folder is empty."}
        {/if}
      </div>
    {/if}
  </div>
</div>

<ContextMenu />

<style>
  .tree {
    /* Width comes from the resizable, persisted sidebar preference. */
    flex: none;
    border-right: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    /* No top padding: sticky offsets resolve against this scroll container's
       padding box, so any padding here would park .files-head that far below the
       top of the scrollport and let rows scroll through the gap above it. The
       resting inset rides on the first row instead. */
    padding: 0 8px 10px;
    overflow-y: auto;
    overflow-x: hidden;
    font-size: 13px;
  }
  .tree > :first-child {
    padding-top: 12px;
  }
  .tree-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 2px 10px 8px;
    font-size: 11px;
    font-weight: 700;
    color: var(--ink-tertiary);
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }
  /* The Files header stays visually pinned: sticky flush to the top of the
     scrolling .tree column (which carries no top padding, so there is nowhere
     for rows to show above it) so it never scrolls away with Favorites and the
     tree. A solid ground covers content sliding under it. */
  .files-head {
    position: sticky;
    top: 0;
    z-index: 5;
    background: var(--paper);
    justify-content: space-between;
    /* extend the ground to the column edges under the sticky row */
    margin: 0 -8px;
    padding-left: 18px;
    padding-right: 12px;
  }
  .files-head .ttl {
    flex: none;
  }
  .head-actions {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    /* header uppercases its text; keep control labels normal-case */
    text-transform: none;
    letter-spacing: 0;
    font-weight: 500;
  }
  .filter {
    display: inline-flex;
    border: 1px solid var(--border);
    border-radius: var(--radius-control);
    overflow: hidden;
  }
  .filter .seg {
    padding: 2px 8px;
    border: none;
    background: var(--surface);
    color: var(--ink-secondary);
    font-size: 11px;
    font-weight: 500;
  }
  .filter .seg:hover {
    background: var(--sunken);
  }
  .filter .seg.on {
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    color: var(--accent-deep);
    font-weight: 600;
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
    /* These buttons live at the right edge of a column that clips overflow-x, so
       the bubble hangs from their right edge and wraps inside the narrowest
       sidebar (190px) rather than running off it. */
    right: 0;
    width: max-content;
    max-width: 160px;
    background: var(--ink);
    color: var(--surface);
    font-size: 11px;
    font-weight: 500;
    letter-spacing: 0;
    line-height: 1.35;
    text-transform: none;
    padding: 3px 7px;
    border-radius: 6px;
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
    background: color-mix(in srgb, var(--accent) 6%, transparent);
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
    background: color-mix(in srgb, var(--danger) 8%, transparent);
    border: 1px solid color-mix(in srgb, var(--danger) 25%, transparent);
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

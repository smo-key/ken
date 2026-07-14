<script lang="ts">
  import Pin from "@lucide/svelte/icons/pin";
  import PinOff from "@lucide/svelte/icons/pin-off";
  import X from "@lucide/svelte/icons/x";
  import Upload from "@lucide/svelte/icons/upload";
  import CheckCheck from "@lucide/svelte/icons/check-check";
  import { open as openDialog } from "@tauri-apps/plugin-dialog";
  import { app } from "../lib/app.svelte";
  import { api, type ImportDto } from "../lib/api";
  import {
    openContextMenu,
    type MenuEntry,
  } from "../lib/ui/ContextMenu.svelte";
  import FileTree from "../files/FileTree.svelte";
  import SidebarResizer from "../files/SidebarResizer.svelte";
  import EditorPane from "../files/EditorPane.svelte";
  import ImportDialog from "../files/ImportDialog.svelte";
  import FileGlyph from "../files/FileGlyph.svelte";
  import { clampSidebarWidth } from "../lib/sidebar";

  let windowWidth = $state(window.innerWidth);

  // The file staged for import while the ImportDialog is open; null otherwise.
  let staged = $state<ImportDto | null>(null);
  let importing = $state(false);
  let importError = $state<string | null>(null);

  // Pick a file, copy it into staging, then open the placement dialog. The AI's
  // folder decision and the preview both resolve inside the dialog.
  async function startImport() {
    if (importing || staged) return;
    importing = true;
    importError = null;
    try {
      const chosen = await openDialog({ directory: false });
      if (typeof chosen !== "string") return; // cancelled
      staged = await api.importBegin(chosen);
    } catch (e) {
      importError = String(e);
    } finally {
      importing = false;
    }
  }

  // Shrinking the window narrows the sidebar for as long as it has to, but the
  // stored preference is left alone so the width comes back when it grows.
  const sidebarWidth = $derived(clampSidebarWidth(app.sidebarWidth, windowWidth));

  function glyphKind(path: string): string {
    return app.files.find((f) => f.relPath === path)?.kind ?? "binary";
  }

  function basename(path: string): string {
    return path.split("/").pop() ?? path;
  }

  function tabMenu(e: MouseEvent, path: string, pinned: boolean) {
    e.preventDefault();
    const items: MenuEntry[] = [
      pinned
        ? { label: "Unpin", icon: PinOff, onSelect: () => app.setTabPinned(path, false) }
        : { label: "Pin", icon: Pin, onSelect: () => app.setTabPinned(path, true) },
      "separator",
      { label: "Close", icon: X, onSelect: () => app.closeTab(path) },
      { label: "Close others", onSelect: () => app.closeOtherTabs(path) },
    ];
    openContextMenu(e.clientX, e.clientY, items);
  }

  // Vertical wheel scrolls the strip horizontally when it overflows.
  function onWheel(e: WheelEvent) {
    const el = e.currentTarget as HTMLElement;
    if (el.scrollWidth <= el.clientWidth) return;
    if (Math.abs(e.deltaY) > Math.abs(e.deltaX)) {
      el.scrollLeft += e.deltaY;
      e.preventDefault();
    }
  }
</script>

<svelte:window bind:innerWidth={windowWidth} />

<div class="files">
  <FileTree width={sidebarWidth} />
  <SidebarResizer width={sidebarWidth} {windowWidth} />
  <div class="content">
    <div class="toolbar">
      <button
        class="import-btn"
        onclick={startImport}
        disabled={importing}
        title="Copy a file into this project"
      >
        <Upload size={14} strokeWidth={1.75} />
        <span>{importing ? "Opening…" : "Import file"}</span>
      </button>
      {#if importError}
        <span class="import-error">{importError}</span>
      {/if}
      <!-- Unread controls sit to the right; only meaningful when something is
           actually unread, so they stay out of the way otherwise. -->
      <div class="unread-controls">
        {#if app.unread.length > 0 || app.filesFilter === "unread"}
          <div class="filter" role="tablist" aria-label="Filter files">
            <button
              class="seg"
              class:on={app.filesFilter === "all"}
              role="tab"
              aria-selected={app.filesFilter === "all"}
              onclick={() => (app.filesFilter = "all")}
            >
              All
            </button>
            <button
              class="seg"
              class:on={app.filesFilter === "unread"}
              role="tab"
              aria-selected={app.filesFilter === "unread"}
              onclick={() => (app.filesFilter = "unread")}
            >
              Unread{#if app.unread.length > 0}&nbsp;·&nbsp;{app.unread.length}{/if}
            </button>
          </div>
          {#if app.unread.length > 0}
            <button
              class="mark-all"
              title="Mark every changed file as viewed"
              onclick={() => void app.markAllSeen()}
            >
              <CheckCheck size={14} strokeWidth={1.75} />
              <span>Mark all as viewed</span>
            </button>
          {/if}
        {/if}
      </div>
    </div>
    {#if app.fileTabs.length > 0}
      <div class="tabstrip" onwheel={onWheel}>
        {#each app.fileTabs as tab (tab.path)}
          <div
            class="tab"
            class:active={app.activeTab === tab.path}
            class:preview={tab.preview}
            class:pinned={tab.pinned}
            role="tab"
            tabindex="0"
            aria-selected={app.activeTab === tab.path}
            title={tab.path}
            onclick={() => app.activateTab(tab.path)}
            ondblclick={() => app.makeTabPersistent(tab.path)}
            onauxclick={(e) => {
              if (e.button === 1) {
                e.preventDefault();
                app.closeTab(tab.path);
              }
            }}
            oncontextmenu={(e) => tabMenu(e, tab.path, tab.pinned)}
            onkeydown={(e) => {
              if (e.key === "Enter" || e.key === " ") app.activateTab(tab.path);
            }}
          >
            {#if tab.pinned}
              <Pin size={12} strokeWidth={1.75} class="pin-mark" />
            {/if}
            <FileGlyph kind={glyphKind(tab.path)} size="sm" />
            {#if !tab.pinned}
              <span class="tab-title">{basename(tab.path)}</span>
            {/if}
            <button
              class="tab-close"
              aria-label="Close tab"
              onclick={(e) => {
                e.stopPropagation();
                app.closeTab(tab.path);
              }}
            >
              <X size={13} strokeWidth={1.75} />
            </button>
          </div>
        {/each}
      </div>
    {/if}

    {#if app.activeTab}
      {#key app.activeTab}
        <EditorPane relPath={app.activeTab} />
      {/key}
    {:else}
      <div class="empty">
        <p>Select a file to read or edit it.</p>
        <p class="hint">Markdown and text open in the editor; Word, Excel, PDF and images preview right here.</p>
      </div>
    {/if}
  </div>
</div>

{#if staged}
  <ImportDialog {staged} close={() => (staged = null)} />
{/if}

<style>
  .files {
    flex: 1;
    min-width: 0;
    display: flex;
    min-height: 0;
  }
  .content {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    min-height: 0;
    background: var(--surface);
  }
  .toolbar {
    display: flex;
    align-items: center;
    gap: 10px;
    flex: none;
    padding: 8px 12px;
    border-bottom: 1px solid var(--border);
    background: var(--paper);
  }
  .import-btn {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 5px 10px;
    border-radius: var(--radius-control);
    border: 1px solid var(--border);
    background: var(--surface);
    color: var(--ink);
    font-size: 12.5px;
    font-weight: 500;
  }
  .import-btn:hover:not(:disabled) {
    background: var(--sunken);
  }
  .import-btn:disabled {
    opacity: 0.6;
  }
  .import-error {
    font-size: 12px;
    color: var(--danger);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .unread-controls {
    margin-left: auto;
    display: flex;
    align-items: center;
    gap: 8px;
    flex: none;
  }
  .filter {
    display: inline-flex;
    border: 1px solid var(--border);
    border-radius: var(--radius-control);
    overflow: hidden;
  }
  .seg {
    padding: 4px 10px;
    border: none;
    background: var(--surface);
    color: var(--ink-secondary);
    font-size: 12px;
    font-weight: 500;
  }
  .seg:hover {
    background: var(--sunken);
  }
  .seg.on {
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    color: var(--accent-deep);
    font-weight: 600;
  }
  .mark-all {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 5px 10px;
    border-radius: var(--radius-control);
    border: 1px solid var(--border);
    background: var(--surface);
    color: var(--ink);
    font-size: 12.5px;
    font-weight: 500;
  }
  .mark-all:hover {
    background: var(--sunken);
  }
  .tabstrip {
    display: flex;
    align-items: stretch;
    gap: 2px;
    flex: none;
    padding: 6px 8px 0;
    border-bottom: 1px solid var(--border);
    background: var(--paper);
    overflow-x: auto;
    overflow-y: hidden;
    scrollbar-width: none;
  }
  .tabstrip::-webkit-scrollbar {
    display: none;
  }
  .tab {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    flex: none;
    max-width: 200px;
    padding: 6px 8px 6px 10px;
    border-radius: 8px 8px 0 0;
    border: 1px solid transparent;
    border-bottom: none;
    background: transparent;
    color: var(--ink-secondary);
    font-size: 12.5px;
    cursor: pointer;
    user-select: none;
  }
  .tab:hover {
    background: var(--sunken);
  }
  .tab.active {
    background: var(--surface);
    border-color: var(--border);
    color: var(--accent-deep);
    font-weight: 600;
  }
  .tab.preview .tab-title {
    font-style: italic;
  }
  .tab.pinned {
    padding-right: 8px;
  }
  .tab-title {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .tab :global(.pin-mark) {
    color: var(--accent);
    flex: none;
  }
  .tab-close {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 16px;
    height: 16px;
    border-radius: 4px;
    border: none;
    background: transparent;
    color: var(--ink-tertiary);
    padding: 0;
    flex: none;
    opacity: 0;
    transition: opacity 0.1s ease;
  }
  .tab:hover .tab-close,
  .tab.active .tab-close {
    opacity: 1;
  }
  .tab-close:hover {
    background: var(--border);
    color: var(--ink);
  }
  .empty {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 4px;
  }
  .empty p {
    margin: 0;
    color: var(--ink-secondary);
    font-size: 14px;
  }
  .empty .hint {
    color: var(--ink-tertiary);
    font-size: 12.5px;
  }
</style>

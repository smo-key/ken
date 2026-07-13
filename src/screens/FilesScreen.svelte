<script lang="ts">
  import Pin from "@lucide/svelte/icons/pin";
  import PinOff from "@lucide/svelte/icons/pin-off";
  import X from "@lucide/svelte/icons/x";
  import { app } from "../lib/app.svelte";
  import {
    openContextMenu,
    type MenuEntry,
  } from "../lib/ui/ContextMenu.svelte";
  import FileTree from "../files/FileTree.svelte";
  import EditorPane from "../files/EditorPane.svelte";
  import FileGlyph from "../files/FileGlyph.svelte";

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

<div class="files">
  <FileTree />
  <div class="content">
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

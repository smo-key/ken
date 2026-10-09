<script lang="ts">
  import ArrowRightToLine from "@lucide/svelte/icons/arrow-right-to-line";
  import CircleX from "@lucide/svelte/icons/circle-x";
  import Copy from "@lucide/svelte/icons/copy";
  import CopyX from "@lucide/svelte/icons/copy-x";
  import ExternalLink from "@lucide/svelte/icons/external-link";
  import Eye from "@lucide/svelte/icons/eye";
  import FolderOpen from "@lucide/svelte/icons/folder-open";
  import Pin from "@lucide/svelte/icons/pin";
  import PinOff from "@lucide/svelte/icons/pin-off";
  import X from "@lucide/svelte/icons/x";
  import { api } from "../lib/api";
  import { app } from "../lib/app.svelte";
  import { imports } from "../lib/imports.svelte";
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
  import TicketColumn from "../files/TicketColumn.svelte";
  import { ticketIdForPath } from "../lib/day";
  import { scope } from "../lib/scope.svelte";
  import { conflicts } from "../lib/conflicts.svelte";
  import { bannerLine } from "../lib/conflicts";
  import ConflictsPanel from "../files/ConflictsPanel.svelte";

  // Sync conflicts and conflicted copies in the team's repos: a line at the
  // top of Files that opens the conflict view in place of the open file.
  const banner = $derived(bannerLine(conflicts.banner));

  // A ticket file (`tickets/<ID>.md`) in one of the team's repos gets the
  // column of my tasks for it beside the file.
  const ticket = $derived.by(() => {
    const id = ticketIdForPath(app.activeTab);
    const projectId = app.focused;
    if (!id || !projectId || !app.workspace) return null;
    if (!scope.teamProjectIds.includes(projectId)) return null;
    return { id, projectId };
  });

  let windowWidth = $state(window.innerWidth);

  // Shrinking the window narrows the sidebar for as long as it has to, but the
  // stored preference is left alone so the width comes back when it grows.
  const sidebarWidth = $derived(clampSidebarWidth(app.sidebarWidth, windowWidth));

  function glyphKind(path: string): string {
    return app.files.find((f) => f.relPath === path)?.kind ?? "binary";
  }

  function basename(path: string): string {
    return path.split("/").pop() ?? path;
  }

  // Pinned tabs survive "close to the right", so the item only earns its keep
  // when there's an unpinned tab past this one.
  function hasClosableRight(path: string): boolean {
    const idx = app.fileTabs.findIndex((t) => t.path === path);
    return idx >= 0 && app.fileTabs.slice(idx + 1).some((t) => !t.pinned);
  }

  function tabMenu(e: MouseEvent, path: string, pinned: boolean) {
    e.preventDefault();
    const items: MenuEntry[] = [
      pinned
        ? { label: "Unpin", icon: PinOff, onSelect: () => app.setTabPinned(path, false) }
        : { label: "Pin", icon: Pin, onSelect: () => app.setTabPinned(path, true) },
      "separator",
      {
        label: "Open in default app",
        icon: ExternalLink,
        onSelect: () => void api.openExternal(path),
      },
      {
        label: "Open containing folder",
        icon: FolderOpen,
        onSelect: () => void api.revealInFolder(path),
      },
      { label: "Reveal in files", icon: Eye, onSelect: () => app.reveal(path) },
      {
        label: "Copy path",
        icon: Copy,
        onSelect: () => void navigator.clipboard.writeText(path),
      },
      "separator",
      { label: "Close", icon: X, onSelect: () => app.closeTab(path) },
      { label: "Close others", icon: CopyX, onSelect: () => app.closeOtherTabs(path) },
      {
        label: "Close to the right",
        icon: ArrowRightToLine,
        disabled: !hasClosableRight(path),
        onSelect: () => app.closeTabsToRight(path),
      },
      { label: "Close all", icon: CircleX, onSelect: () => app.closeAllTabs() },
    ];
    openContextMenu(e.clientX, e.clientY, items);
  }

  // Tabs are narrow and elide long names, so a hover bubble spells out the file.
  // It lives outside the tabstrip because that strip clips overflow-y.
  let tip = $state<{ name: string; path: string; left: number; top: number } | null>(null);
  let tipEl = $state<HTMLElement | null>(null);
  let tipTimer: number | undefined;

  function armTip(e: PointerEvent, path: string) {
    const el = e.currentTarget as HTMLElement;
    clearTimeout(tipTimer);
    tipTimer = window.setTimeout(() => {
      const r = el.getBoundingClientRect();
      tip = { name: basename(path), path, left: r.left, top: r.bottom + 6 };
    }, 500);
  }

  function hideTip() {
    clearTimeout(tipTimer);
    tip = null;
  }

  // Fixed positioning can overhang the viewport, and the bubble's real width
  // isn't known until it renders, so clamp from the measured box.
  $effect(() => {
    const el = tipEl;
    const left = tip?.left;
    if (!el || left === undefined) return;
    el.style.left = `${Math.max(8, Math.min(left, window.innerWidth - el.offsetWidth - 8))}px`;
  });

  $effect(() => () => clearTimeout(tipTimer));

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
    {#if banner}
      <div class="banner" role="status">
        <span class="bdot"></span>
        <span class="btext">{banner}</span>
        {#if conflicts.open}
          <button class="btn btn-small btn-ghost" onclick={() => conflicts.close()}>Back to the file</button>
        {:else}
          <button class="btn btn-small" onclick={() => void conflicts.show()}>Resolve</button>
        {/if}
      </div>
    {/if}
    {#if conflicts.open}
      <ConflictsPanel />
    {/if}
    <!-- Kept mounted under the conflict view, so the open file and its
         unsaved edits are there on the way back. -->
    <div class="body" class:gone={conflicts.open}>
    {#if app.fileTabs.length > 0}
      <div class="tabstrip" onwheel={onWheel} onscroll={hideTip}>
        {#each app.fileTabs as tab (tab.path)}
          <div
            class="tab"
            class:active={app.activeTab === tab.path}
            class:preview={tab.preview}
            class:pinned={tab.pinned}
            role="tab"
            tabindex="0"
            aria-selected={app.activeTab === tab.path}
            onpointerenter={(e) => armTip(e, tab.path)}
            onpointerleave={hideTip}
            onclick={() => {
              hideTip();
              app.activateTab(tab.path);
            }}
            ondblclick={() => app.makeTabPersistent(tab.path)}
            onauxclick={(e) => {
              if (e.button === 1) {
                e.preventDefault();
                hideTip();
                app.closeTab(tab.path);
              }
            }}
            oncontextmenu={(e) => {
              hideTip();
              tabMenu(e, tab.path, tab.pinned);
            }}
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
                hideTip();
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
      <div class="open">
        {#key `${app.focused}:${app.activeTab}`}
          <EditorPane relPath={app.activeTab} />
        {/key}
        {#if ticket}
          <TicketColumn projectId={ticket.projectId} ticketId={ticket.id} />
        {/if}
      </div>
    {:else}
      <div class="empty">
        <p>Select a file to read or edit it.</p>
        <p class="hint">Markdown and text open in the editor; Word, Excel, PDF and images preview right here.</p>
      </div>
    {/if}
    </div>
  </div>
</div>

{#if tip}
  <div
    class="tab-tip"
    role="tooltip"
    bind:this={tipEl}
    style="left: {tip.left}px; top: {tip.top}px"
  >
    <span class="tip-name">{tip.name}</span>
    <span class="tip-path">{tip.path}</span>
  </div>
{/if}

{#if imports.staged}
  <ImportDialog staged={imports.staged} close={() => imports.close()} />
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
  .open {
    flex: 1;
    min-height: 0;
    display: flex;
  }
  .body {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .body.gone {
    display: none;
  }
  .banner {
    display: flex;
    align-items: center;
    gap: 10px;
    flex: none;
    padding: 7px 14px;
    border-bottom: 1px solid color-mix(in srgb, var(--danger) 25%, var(--border));
    background: color-mix(in srgb, var(--danger) 6%, var(--paper));
    font-size: 12.5px;
  }
  .bdot {
    width: 7px;
    height: 7px;
    border-radius: 4px;
    background: var(--danger);
    flex: none;
  }
  .btext {
    flex: 1;
    min-width: 0;
    color: var(--ink);
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
  .tab-tip {
    position: fixed;
    display: flex;
    flex-direction: column;
    gap: 1px;
    width: max-content;
    max-width: 360px;
    padding: 4px 8px;
    background: var(--ink);
    border-radius: 6px;
    line-height: 1.35;
    pointer-events: none;
    z-index: 50;
  }
  .tip-name {
    color: var(--surface);
    font-size: 12px;
    font-weight: 550;
  }
  /* The bubble is light-on-dark, so the muted line dims against the bubble's own
     text rather than using --ink-tertiary, which would vanish into the fill. */
  .tip-path {
    color: color-mix(in srgb, var(--surface) 62%, transparent);
    font-size: 11px;
    overflow-wrap: anywhere;
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

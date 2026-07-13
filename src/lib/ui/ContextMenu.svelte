<script module lang="ts">
  import type { Component } from "svelte";

  /** A Lucide (or compatible) icon component. */
  export type IconComponent = Component<{
    size?: number | string;
    strokeWidth?: number | string;
    color?: string;
    class?: string;
  }>;

  export interface MenuItem {
    label: string;
    icon?: IconComponent;
    danger?: boolean;
    disabled?: boolean;
    onSelect: () => void;
  }

  export type MenuEntry = MenuItem | "separator";

  interface OpenState {
    x: number;
    y: number;
    items: MenuEntry[];
  }

  let current = $state<OpenState | null>(null);
  // Only the most-recently-mounted instance renders, so mounting <ContextMenu/>
  // in several screens never double-draws. Ownership falls back to another live
  // instance when the owner unmounts, so menus keep working across screens.
  let seq = 0;
  let owner = $state(0);
  const mounted = new Set<number>();

  /** Open the shared context menu at viewport coords with the given items. */
  export function openContextMenu(x: number, y: number, items: MenuEntry[]) {
    current = { x, y, items };
  }

  export function closeContextMenu() {
    current = null;
  }
</script>

<script lang="ts">
  import { onDestroy, onMount } from "svelte";

  const myId = ++seq;
  onMount(() => {
    mounted.add(myId);
    owner = myId;
  });
  onDestroy(() => {
    mounted.delete(myId);
    if (owner === myId) owner = mounted.size ? Math.max(...mounted) : 0;
  });

  let menuEl = $state<HTMLDivElement | null>(null);
  let pos = $state({ x: 0, y: 0 });

  const visible = $derived(current !== null && owner === myId);

  // Position on open, clamped to the viewport once the menu has measured.
  $effect(() => {
    if (!current || !menuEl) return;
    const rect = menuEl.getBoundingClientRect();
    const pad = 8;
    let x = current.x;
    let y = current.y;
    if (x + rect.width > window.innerWidth - pad) x = window.innerWidth - rect.width - pad;
    if (y + rect.height > window.innerHeight - pad) y = window.innerHeight - rect.height - pad;
    pos = { x: Math.max(pad, x), y: Math.max(pad, y) };
  });

  // Dismiss on click-away, Escape, or scroll while open.
  $effect(() => {
    if (!visible) return;
    const onDown = (e: PointerEvent) => {
      if (menuEl && !menuEl.contains(e.target as Node)) closeContextMenu();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") closeContextMenu();
    };
    const onScroll = () => closeContextMenu();
    window.addEventListener("pointerdown", onDown, true);
    window.addEventListener("keydown", onKey);
    window.addEventListener("scroll", onScroll, true);
    window.addEventListener("resize", onScroll);
    return () => {
      window.removeEventListener("pointerdown", onDown, true);
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("scroll", onScroll, true);
      window.removeEventListener("resize", onScroll);
    };
  });

  function select(item: MenuItem) {
    if (item.disabled) return;
    closeContextMenu();
    item.onSelect();
  }
</script>

{#if visible && current}
  <div
    class="cm"
    role="menu"
    tabindex="-1"
    bind:this={menuEl}
    style:left={`${pos.x}px`}
    style:top={`${pos.y}px`}
    oncontextmenu={(e) => e.preventDefault()}
  >
    {#each current.items as entry, i (i)}
      {#if entry === "separator"}
        <div class="cm-sep" role="separator"></div>
      {:else}
        {@const Icon = entry.icon}
        <button
          class="cm-item"
          class:danger={entry.danger}
          role="menuitem"
          disabled={entry.disabled}
          onclick={() => select(entry)}
        >
          {#if Icon}
            <span class="cm-icon"><Icon size={15} strokeWidth={1.75} /></span>
          {/if}
          <span class="cm-label">{entry.label}</span>
        </button>
      {/if}
    {/each}
  </div>
{/if}

<style>
  .cm {
    position: fixed;
    z-index: 1000;
    min-width: 180px;
    max-width: 280px;
    padding: 5px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-overlay);
    box-shadow: var(--shadow-overlay);
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .cm-item {
    display: flex;
    align-items: center;
    gap: 9px;
    width: 100%;
    padding: 6px 10px;
    border: none;
    background: transparent;
    border-radius: 8px;
    font-size: 13px;
    color: var(--ink);
    text-align: left;
  }
  .cm-item:hover:not(:disabled) {
    background: var(--sunken);
  }
  .cm-item:disabled {
    color: var(--ink-tertiary);
    cursor: default;
  }
  .cm-item.danger {
    color: var(--danger);
  }
  .cm-item.danger:hover:not(:disabled) {
    background: color-mix(in srgb, var(--danger) 8%, transparent);
  }
  .cm-icon {
    display: inline-flex;
    align-items: center;
    color: var(--ink-tertiary);
    flex: none;
  }
  .cm-item.danger .cm-icon {
    color: var(--danger);
  }
  .cm-label {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .cm-sep {
    height: 1px;
    background: var(--border);
    margin: 4px 6px;
  }
</style>

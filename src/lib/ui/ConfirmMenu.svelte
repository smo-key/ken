<script module lang="ts">
  export interface ConfirmOptions {
    title: string;
    body: string;
    confirmLabel: string;
    onConfirm: () => void;
  }

  interface OpenState extends ConfirmOptions {
    x: number;
    y: number;
  }

  let current = $state<OpenState | null>(null);
  // Mirror ContextMenu's most-recently-mounted owner guard so several screens
  // can each mount <ConfirmMenu/> without double-drawing.
  let seq = 0;
  let owner = $state(0);
  const mounted = new Set<number>();

  /** Open a small Paper & Ink confirm popover at viewport coords. */
  export function openConfirm(x: number, y: number, opts: ConfirmOptions) {
    current = { x, y, ...opts };
  }

  export function closeConfirm() {
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

  // Position on open, clamped to the viewport once measured.
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
      if (menuEl && !menuEl.contains(e.target as Node)) closeConfirm();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") closeConfirm();
    };
    const onScroll = () => closeConfirm();
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

  function confirm() {
    const fn = current?.onConfirm;
    closeConfirm();
    fn?.();
  }
</script>

{#if visible && current}
  <div
    class="confirm"
    role="dialog"
    aria-label={current.title}
    bind:this={menuEl}
    style:left={`${pos.x}px`}
    style:top={`${pos.y}px`}
  >
    <div class="title">{current.title}</div>
    <div class="body">{current.body}</div>
    <div class="actions">
      <button class="btn btn-small" onclick={closeConfirm}>Cancel</button>
      <button class="btn btn-small danger" onclick={confirm}>{current.confirmLabel}</button>
    </div>
  </div>
{/if}

<style>
  .confirm {
    position: fixed;
    z-index: 1001;
    width: 250px;
    padding: 14px 15px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-overlay);
    box-shadow: var(--shadow-overlay);
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .title {
    font-size: 13px;
    font-weight: 600;
    color: var(--ink);
  }
  .body {
    font-size: 12px;
    line-height: 1.5;
    color: var(--ink-secondary);
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 2px;
  }
  .btn.danger {
    border-color: color-mix(in srgb, var(--danger) 45%, transparent);
    background: color-mix(in srgb, var(--danger) 10%, transparent);
    color: var(--danger);
    font-weight: 600;
  }
  .btn.danger:hover {
    background: color-mix(in srgb, var(--danger) 16%, transparent);
  }
</style>

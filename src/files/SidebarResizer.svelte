<script lang="ts">
  import { app } from "../lib/app.svelte";
  import {
    DEFAULT_SIDEBAR_WIDTH,
    MIN_SIDEBAR_WIDTH,
    maxSidebarWidth,
  } from "../lib/sidebar";

  // `width` is the *rendered* sidebar width (already clamped to this window),
  // which is what a drag or an arrow key has to start from.
  let { width, windowWidth }: { width: number; windowWidth: number } = $props();

  let dragging = $state(false);
  let startX = 0;
  let startWidth = 0;

  const STEP = 16;
  const COARSE_STEP = 64;

  function onPointerDown(e: PointerEvent) {
    if (e.button !== 0) return;
    dragging = true;
    startX = e.clientX;
    startWidth = width;
    // Capture keeps the drag alive when the pointer outruns the divider or
    // leaves the window entirely.
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    // Stop the tree rows underneath from starting a text selection.
    e.preventDefault();
  }

  function onPointerMove(e: PointerEvent) {
    if (!dragging) return;
    app.setSidebarWidth(startWidth + (e.clientX - startX), windowWidth);
  }

  function endDrag(e: PointerEvent) {
    if (!dragging) return;
    dragging = false;
    const el = e.currentTarget as HTMLElement;
    if (el.hasPointerCapture(e.pointerId)) el.releasePointerCapture(e.pointerId);
    app.commitSidebarWidth();
  }

  function onKeydown(e: KeyboardEvent) {
    const step = e.shiftKey ? COARSE_STEP : STEP;
    let next: number | null = null;
    if (e.key === "ArrowLeft") next = width - step;
    else if (e.key === "ArrowRight") next = width + step;
    else if (e.key === "Home") next = MIN_SIDEBAR_WIDTH;
    else if (e.key === "End") next = maxSidebarWidth(windowWidth);
    else if (e.key === "Enter") next = DEFAULT_SIDEBAR_WIDTH;
    if (next === null) return;
    e.preventDefault();
    app.setSidebarWidth(next, windowWidth);
    app.commitSidebarWidth();
  }

  function reset() {
    app.setSidebarWidth(DEFAULT_SIDEBAR_WIDTH, windowWidth);
    app.commitSidebarWidth();
  }

  // Suppress selection app-wide (not just on the divider) and hold the resize
  // cursor even where the pointer strays during a drag.
  $effect(() => {
    const body = document.body;
    body.style.userSelect = dragging ? "none" : "";
    body.style.cursor = dragging ? "col-resize" : "";
    return () => {
      body.style.userSelect = "";
      body.style.cursor = "";
    };
  });
</script>

<!-- A focusable separator is the ARIA window-splitter pattern; the linter's role
     table treats every separator as decorative and so flags the tabindex. -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
<div
  class="resizer"
  class:dragging
  role="separator"
  tabindex="0"
  aria-orientation="vertical"
  aria-label="Resize sidebar"
  aria-valuenow={width}
  aria-valuemin={MIN_SIDEBAR_WIDTH}
  aria-valuemax={maxSidebarWidth(windowWidth)}
  aria-valuetext="{width} pixels"
  onpointerdown={onPointerDown}
  onpointermove={onPointerMove}
  onpointerup={endDrag}
  onpointercancel={endDrag}
  ondblclick={reset}
  onkeydown={onKeydown}
></div>

<style>
  /* Sits between the tree and the editor, straddling the tree's right border
     with negative margins so widening the hit area doesn't shift the layout. */
  .resizer {
    flex: none;
    position: relative;
    z-index: 2;
    width: 9px;
    margin: 0 -5px 0 -4px;
    cursor: col-resize;
    touch-action: none;
  }
  /* The visible hairline — transparent at rest, so the tree's own border is
     all you see until you go looking for the divider. */
  .resizer::after {
    content: "";
    position: absolute;
    top: 0;
    bottom: 0;
    left: 4px;
    width: 1px;
    background: transparent;
    transition: background 0.12s ease;
  }
  .resizer:hover::after {
    background: var(--ink-tertiary);
  }
  .resizer:focus-visible::after,
  .resizer.dragging::after {
    background: var(--accent);
  }
  .resizer:focus-visible {
    outline: none;
  }
</style>

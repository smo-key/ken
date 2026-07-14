<script lang="ts">
  import { app } from "../lib/app.svelte";
  import {
    DEFAULT_CHAT_WIDTH,
    MIN_CHAT_WIDTH,
    maxChatWidth,
  } from "../lib/chatWidth";

  // `width` is the *rendered* drawer width (already clamped to this window),
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
    // Stop the content underneath from starting a text selection.
    e.preventDefault();
  }

  function onPointerMove(e: PointerEvent) {
    if (!dragging) return;
    // The drawer sits on the right and grows toward the left, so dragging the
    // handle left (a negative clientX delta) has to *widen* it — inverted from
    // the Files sidebar, whose handle is on its right edge.
    app.setChatWidth(startWidth + (startX - e.clientX), windowWidth);
  }

  function endDrag(e: PointerEvent) {
    if (!dragging) return;
    dragging = false;
    const el = e.currentTarget as HTMLElement;
    if (el.hasPointerCapture(e.pointerId)) el.releasePointerCapture(e.pointerId);
    app.commitChatWidth();
  }

  function onKeydown(e: KeyboardEvent) {
    const step = e.shiftKey ? COARSE_STEP : STEP;
    let next: number | null = null;
    // Left grows the drawer, right shrinks it — the handle moves with the key.
    if (e.key === "ArrowLeft") next = width + step;
    else if (e.key === "ArrowRight") next = width - step;
    else if (e.key === "Home") next = MIN_CHAT_WIDTH;
    else if (e.key === "End") next = maxChatWidth(windowWidth);
    else if (e.key === "Enter") next = DEFAULT_CHAT_WIDTH;
    if (next === null) return;
    e.preventDefault();
    app.setChatWidth(next, windowWidth);
    app.commitChatWidth();
  }

  function reset() {
    app.setChatWidth(DEFAULT_CHAT_WIDTH, windowWidth);
    app.commitChatWidth();
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
  aria-label="Resize chat"
  aria-valuenow={width}
  aria-valuemin={MIN_CHAT_WIDTH}
  aria-valuemax={maxChatWidth(windowWidth)}
  aria-valuetext="{width} pixels"
  onpointerdown={onPointerDown}
  onpointermove={onPointerMove}
  onpointerup={endDrag}
  onpointercancel={endDrag}
  ondblclick={reset}
  onkeydown={onKeydown}
></div>

<style>
  /* Straddles the drawer's left border from inside it, with negative margin so
     widening the hit area doesn't shift the drawer's own layout. */
  .resizer {
    position: absolute;
    top: 0;
    bottom: 0;
    left: 0;
    z-index: 3;
    width: 9px;
    margin-left: -5px;
    cursor: col-resize;
    touch-action: none;
  }
  /* The visible hairline — transparent at rest, so the drawer's own border is
     all you see until you go looking for the divider. */
  .resizer::after {
    content: "";
    position: absolute;
    top: 0;
    bottom: 0;
    left: 5px;
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

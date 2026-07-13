<script lang="ts">
  import { onMount } from "svelte";
  import { app } from "../lib/app.svelte";
  import { knowledge } from "../lib/knowledge.svelte";
  import { layoutMap, type MapNode } from "../lib/knowledge";
  import { timeAgo } from "../lib/format";
  import type { EntityRow } from "../lib/api";

  onMount(() => void knowledge.visit());

  // Fixed-size world in px: nodes are placed inside it and the whole
  // layer is panned with a translate. layoutMap gives % coordinates,
  // which we scale into this px space.
  const WORLD_W = 1600;
  const WORLD_H = 1200;

  const model = $derived(knowledge.model);
  const nodes = $derived<Map<number, MapNode>>(
    model ? layoutMap(model.entities, model.edges) : new Map(),
  );
  const drawnEdges = $derived(
    (model?.edges ?? []).flatMap((e) => {
      const a = nodes.get(e.a);
      const b = nodes.get(e.b);
      return a && b ? [{ id: e.id, a, b, label: e.label }] : [];
    }),
  );
  const hasUnconnected = $derived(
    [...nodes.values()].some((n) => n.ring === "unconnected"),
  );

  let selected = $state<number | null>(null);
  const selectedEntity = $derived<EntityRow | null>(
    selected !== null
      ? (model?.entities.find((e) => e.id === selected) ?? null)
      : null,
  );

  // Pan state. `panning` disables the world's transform transition so a
  // drag tracks the pointer 1:1; focus animations run with it off.
  let panX = $state(0);
  let panY = $state(0);
  let panning = $state(false);
  let canvasW = $state(0);
  let canvasH = $state(0);

  // Centre the world in the viewport once, when we first know its size.
  let centred = false;
  $effect(() => {
    if (!centred && canvasW > 0 && canvasH > 0) {
      centred = true;
      panX = (canvasW - WORLD_W) / 2;
      panY = (canvasH - WORLD_H) / 2;
    }
  });

  const worldX = (pct: number) => (pct / 100) * WORLD_W;
  const worldY = (pct: number) => (pct / 100) * WORLD_H;

  // Background drag → pan. A small movement threshold tells a drag apart
  // from a click (which deselects).
  const DRAG_THRESHOLD = 4;
  let dragPointer: number | null = null;
  let startX = 0;
  let startY = 0;
  let startPanX = 0;
  let startPanY = 0;
  let moved = false;

  function onPointerDown(e: PointerEvent) {
    dragPointer = e.pointerId;
    startX = e.clientX;
    startY = e.clientY;
    startPanX = panX;
    startPanY = panY;
    moved = false;
    panning = true;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }

  function onPointerMove(e: PointerEvent) {
    if (dragPointer === null) return;
    const dx = e.clientX - startX;
    const dy = e.clientY - startY;
    if (!moved && Math.hypot(dx, dy) < DRAG_THRESHOLD) return;
    moved = true;
    panX = startPanX + dx;
    panY = startPanY + dy;
  }

  function onPointerUp(e: PointerEvent) {
    if (dragPointer === null) return;
    const wasDrag = moved;
    dragPointer = null;
    panning = false;
    (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    // A click on the background (no drag) deselects.
    if (!wasDrag) selected = null;
  }

  // Click a node → select it and glide the pan so it sits centred.
  function focusNode(entity: EntityRow) {
    const node = nodes.get(entity.id);
    if (!node) return;
    selected = entity.id;
    panning = false;
    panX = canvasW / 2 - worldX(node.xPct);
    panY = canvasH / 2 - worldY(node.yPct);
  }

  function nodeTitle(entity: EntityRow): string {
    const where = entity.sources[0] ? ` — ${entity.sources[0]}` : "";
    return `${entity.summary || entity.name}${where}`;
  }

  function chipLabel(path: string): string {
    return path.split("/").pop() ?? path;
  }
</script>

<div class="screen">
  {#if knowledge.empty}
    <div class="empty">
      {#if knowledge.error}
        <div class="error">Last refresh didn't finish — {knowledge.error}</div>
      {/if}
      <div class="empty-card">
        <h2>Ken hasn't mapped this project yet</h2>
        <p>
          One pass over your documents finds the people, organizations,
          topics, and decisions — and how they connect.
        </p>
        {#if knowledge.building}
          <p class="pulse">Ken is mapping the project…</p>
        {:else if knowledge.claudeFound}
          <button class="btn btn-primary" onclick={() => void knowledge.refresh()}>
            Refresh map
          </button>
        {:else}
          <p class="note">
            The map needs Claude Code — install with
            <span class="mono">npm i -g @anthropic-ai/claude-code</span>, then
            log in once with <span class="mono">claude</span>.
          </p>
        {/if}
      </div>
    </div>
  {:else if model}
    <div
      class="canvas"
      class:grabbing={panning}
      role="application"
      aria-label="Knowledge map"
      bind:clientWidth={canvasW}
      bind:clientHeight={canvasH}
      onpointerdown={onPointerDown}
      onpointermove={onPointerMove}
      onpointerup={onPointerUp}
      onpointercancel={onPointerUp}
    >
      <div
        class="world"
        style:width="{WORLD_W}px"
        style:height="{WORLD_H}px"
        style:transform="translate({panX}px, {panY}px)"
        style:transition={panning ? "none" : "transform 0.4s cubic-bezier(0.4, 0, 0.2, 1)"}
      >
        <svg
          width={WORLD_W}
          height={WORLD_H}
          viewBox="0 0 {WORLD_W} {WORLD_H}"
          aria-hidden="true"
        >
          {#each drawnEdges as edge (edge.id)}
            <line
              x1={worldX(edge.a.xPct)}
              y1={worldY(edge.a.yPct)}
              x2={worldX(edge.b.xPct)}
              y2={worldY(edge.b.yPct)}
            >
              {#if edge.label}<title>{edge.label}</title>{/if}
            </line>
          {/each}
        </svg>
        {#each model.entities as entity (entity.id)}
          {@const node = nodes.get(entity.id)}
          {#if node}
            <button
              class="node {node.ring}"
              class:mono-label={entity.kind === "decision"}
              class:selected={selected === entity.id}
              style:left="{worldX(node.xPct)}px"
              style:top="{worldY(node.yPct)}px"
              title={nodeTitle(entity)}
              onpointerdown={(e) => e.stopPropagation()}
              onclick={(e) => {
                e.stopPropagation();
                focusNode(entity);
              }}
            >
              {entity.name}
            </button>
          {/if}
        {/each}
      </div>

      <div class="controls">
        {#if model.builtAt}
          <span class="when">built {timeAgo(model.builtAt)}</span>
        {/if}
        <button
          class="btn btn-small"
          disabled={knowledge.building}
          onclick={() => void knowledge.refresh()}
        >
          {knowledge.building ? "Mapping…" : "Refresh map"}
        </button>
      </div>

      {#if knowledge.error}
        <div class="error-overlay">Last refresh didn't finish — {knowledge.error}</div>
      {/if}

      {#if hasUnconnected}
        <div class="legend">Dashed = mentioned but unconnected</div>
      {/if}

      {#if selectedEntity}
        <div class="detail">
          <div class="detail-name">{selectedEntity.name}</div>
          {#if selectedEntity.summary}
            <p class="detail-summary">{selectedEntity.summary}</p>
          {/if}
          {#if selectedEntity.sources.length > 0}
            <div class="detail-sources">
              {#each selectedEntity.sources as source (source)}
                <button
                  class="src-chip mono"
                  title={source}
                  onclick={() => app.openInFiles(source)}
                >
                  {chipLabel(source)}
                </button>
              {/each}
            </div>
          {/if}
        </div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .screen {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }

  .canvas {
    flex: 1;
    min-height: 0;
    position: relative;
    overflow: hidden;
    background: var(--surface);
    cursor: grab;
    touch-action: none;
  }
  .canvas.grabbing {
    cursor: grabbing;
  }

  .world {
    position: absolute;
    top: 0;
    left: 0;
    will-change: transform;
  }
  svg {
    position: absolute;
    top: 0;
    left: 0;
    pointer-events: none;
  }
  line {
    stroke: var(--border-strong);
    stroke-width: 1.5;
  }

  .node {
    position: absolute;
    transform: translate(-50%, -50%);
    border-radius: 9px;
    padding: 7px 14px;
    font-size: 12.5px;
    font-weight: 600;
    background: var(--surface);
    border: 1px solid var(--border-strong);
    color: var(--ink);
    white-space: nowrap;
    max-width: 220px;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .node:hover {
    z-index: 2;
    box-shadow: var(--shadow-card);
    transform: translate(-50%, -50%) translateY(-1px);
  }
  .node.primary {
    background: var(--ink);
    color: var(--paper);
    border-color: var(--ink);
    padding: 8px 16px;
    font-size: 13px;
    box-shadow: 0 2px 8px rgba(33, 30, 25, 0.2);
  }
  .node.unconnected {
    background: color-mix(in srgb, var(--needs-input) 8%, transparent);
    border: 1px dashed color-mix(in srgb, var(--needs-input) 50%, transparent);
    color: var(--needs-input-text);
    font-weight: 500;
  }
  .node.mono-label {
    font-family: var(--font-mono);
    font-size: 11.5px;
    font-weight: 500;
  }
  .node.selected {
    z-index: 3;
    box-shadow: 0 0 0 2px var(--accent), var(--shadow-card);
  }

  .controls {
    position: absolute;
    top: 16px;
    right: 16px;
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 7px 12px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-card);
    box-shadow: var(--shadow-card);
  }
  .when {
    font-size: 12px;
    color: var(--ink-tertiary);
    white-space: nowrap;
  }

  .error-overlay {
    position: absolute;
    top: 16px;
    left: 16px;
    max-width: 420px;
    font-size: 12.5px;
    color: var(--danger);
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 6px 12px;
    box-shadow: var(--shadow-card);
  }

  .legend {
    position: absolute;
    left: 16px;
    bottom: 14px;
    font-size: 11.5px;
    color: var(--ink-tertiary);
  }

  .detail {
    position: absolute;
    left: 16px;
    bottom: 36px;
    max-width: 320px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-card);
    box-shadow: var(--shadow-card);
    padding: 14px 16px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .detail-name {
    font-family: var(--font-serif);
    font-size: 15px;
    font-weight: 500;
    color: var(--ink);
  }
  .detail-summary {
    margin: 0;
    font-size: 12.5px;
    line-height: 1.55;
    color: var(--ink-secondary);
  }
  .detail-sources {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .src-chip {
    font-size: 11px;
    color: var(--ink-secondary);
    border: 1px solid var(--border);
    border-radius: 5px;
    padding: 2px 7px;
    background: var(--paper);
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .src-chip:hover {
    border-color: var(--border-strong);
    color: var(--ink);
  }

  .empty {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 12px;
    background: var(--surface);
  }
  .error {
    font-size: 12.5px;
    color: var(--danger);
  }
  .empty-card {
    max-width: 420px;
    text-align: center;
    background: var(--paper);
    border: 1px solid var(--border);
    border-radius: var(--radius-card);
    padding: 28px 32px;
    box-shadow: var(--shadow-card);
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 12px;
  }
  .empty-card h2 {
    margin: 0;
    font-family: var(--font-serif);
    font-size: 19px;
    font-weight: 500;
  }
  .empty-card p {
    margin: 0;
    font-size: 13.5px;
    color: var(--ink-secondary);
  }
  .note {
    font-size: 12.5px;
  }
  .pulse {
    color: var(--accent);
    animation: pulse 1.6s ease-in-out infinite;
  }
  @keyframes pulse {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.55; }
  }
</style>

<script lang="ts">
  import { onMount } from "svelte";
  import { app } from "../lib/app.svelte";
  import { knowledge } from "../lib/knowledge.svelte";
  import {
    computeMapView,
    layoutMap,
    type EntityKind,
    type MapNode,
    type NodeView,
  } from "../lib/knowledge";
  import type { EntityRow } from "../lib/api";

  onMount(() => void knowledge.visit());

  // Fixed-size world in px: nodes are placed inside it and the whole
  // layer is panned+scaled with a single transform. layoutMap gives %
  // coordinates, which we scale into this px space. Layout is memoized
  // (depends only on the model), so pan/zoom/search never relayout.
  const WORLD_W = 1600;
  const WORLD_H = 1200;

  // Label budget for the at-rest graph — the most-connected anchors.
  const PROMINENT_LABELS = 16;
  // Past this scale the graph is sparse enough on screen to show every
  // label without clutter.
  const LABEL_ALL_SCALE = 1.9;
  const MIN_SCALE = 0.3;
  const MAX_SCALE = 4;

  const KINDS: { kind: EntityKind; label: string }[] = [
    { kind: "person", label: "People" },
    { kind: "organization", label: "Orgs" },
    { kind: "topic", label: "Topics" },
    { kind: "decision", label: "Decisions" },
    { kind: "other", label: "Other" },
  ];

  const model = $derived(knowledge.model);
  const nodes = $derived<Map<number, MapNode>>(
    model ? layoutMap(model.entities, model.edges) : new Map(),
  );

  let selected = $state<number | null>(null);
  let hovered = $state<number | null>(null);
  let rawQuery = $state("");
  let query = $state(""); // debounced copy that drives the view
  let activeKinds = $state<Set<EntityKind>>(new Set());

  // Pan + zoom. A single transform: translate(pan) then scale. `panning`
  // disables the transition so a drag tracks the pointer 1:1.
  let panX = $state(0);
  let panY = $state(0);
  let scale = $state(1);
  let panning = $state(false);
  let canvasW = $state(0);
  let canvasH = $state(0);

  // Light debounce: the search is client-side over the in-memory model,
  // so this is only to avoid re-deriving on every keystroke burst.
  let debounceTimer: ReturnType<typeof setTimeout> | undefined;
  function onQueryInput() {
    clearTimeout(debounceTimer);
    debounceTimer = setTimeout(() => (query = rawQuery), 120);
  }

  const view = $derived<Map<number, NodeView>>(
    model
      ? computeMapView({
          entities: model.entities,
          edges: model.edges,
          query,
          kinds: [...activeKinds],
          selected,
          hovered,
          showAllLabels: scale >= LABEL_ALL_SCALE,
          prominentCount: PROMINENT_LABELS,
        })
      : new Map(),
  );

  const drawnEdges = $derived(
    (model?.edges ?? []).flatMap((e) => {
      const a = nodes.get(e.a);
      const b = nodes.get(e.b);
      const va = view.get(e.a);
      const vb = view.get(e.b);
      if (!a || !b || !va || !vb || !va.visible || !vb.visible) return [];
      // An edge is lit when both ends are in focus (neither dimmed);
      // otherwise it recedes with the background.
      const active = !va.dimmed && !vb.dimmed && (selected !== null || query.trim() !== "");
      return [{ id: e.id, a, b, label: e.label, active, dimmed: va.dimmed || vb.dimmed }];
    }),
  );

  const selectedEntity = $derived<EntityRow | null>(
    selected !== null
      ? (model?.entities.find((e) => e.id === selected) ?? null)
      : null,
  );

  // The selected node's relationships, ready for the detail panel.
  const connections = $derived(
    selected === null || !model
      ? []
      : model.edges.flatMap((e) => {
          const otherId = e.a === selected ? e.b : e.b === selected ? e.a : null;
          if (otherId === null) return [];
          const other = model.entities.find((x) => x.id === otherId);
          return other ? [{ other, label: e.label }] : [];
        }),
  );

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

  const clamp = (v: number, lo: number, hi: number) =>
    Math.min(hi, Math.max(lo, v));

  function toggleKind(kind: EntityKind) {
    const next = new Set(activeKinds);
    if (next.has(kind)) next.delete(kind);
    else next.add(kind);
    activeKinds = next;
  }

  // Zoom toward a canvas-space anchor, keeping the world point under it
  // fixed. Trackpad pinch arrives as a ctrlKey wheel; a plain wheel zooms
  // too (deeper investigation is the point of this screen).
  function zoomAt(canvasPx: number, canvasPy: number, factor: number) {
    const next = clamp(scale * factor, MIN_SCALE, MAX_SCALE);
    if (next === scale) return;
    const wx = (canvasPx - panX) / scale;
    const wy = (canvasPy - panY) / scale;
    panning = true; // no transition — zoom should feel direct
    scale = next;
    panX = canvasPx - wx * next;
    panY = canvasPy - wy * next;
  }

  function onWheel(e: WheelEvent) {
    e.preventDefault();
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const px = e.clientX - rect.left;
    const py = e.clientY - rect.top;
    // Pinch (ctrlKey) is finer-grained than a mouse wheel notch.
    const step = e.ctrlKey ? 0.01 : 0.0015;
    zoomAt(px, py, Math.exp(-e.deltaY * step));
  }

  function zoomButton(factor: number) {
    zoomAt(canvasW / 2, canvasH / 2, factor);
  }

  // Fit every visible node into view with padding — the reset control.
  function zoomToFit() {
    const pts = [...nodes.values()]
      .filter((n) => view.get(n.id)?.visible ?? true)
      .map((n) => ({ x: worldX(n.xPct), y: worldY(n.yPct) }));
    panning = false;
    if (pts.length === 0 || canvasW === 0) {
      scale = 1;
      panX = (canvasW - WORLD_W) / 2;
      panY = (canvasH - WORLD_H) / 2;
      return;
    }
    const pad = 120;
    let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
    for (const p of pts) {
      minX = Math.min(minX, p.x); maxX = Math.max(maxX, p.x);
      minY = Math.min(minY, p.y); maxY = Math.max(maxY, p.y);
    }
    const w = maxX - minX + pad * 2;
    const h = maxY - minY + pad * 2;
    const next = clamp(Math.min(canvasW / w, canvasH / h), MIN_SCALE, MAX_SCALE);
    scale = next;
    panX = canvasW / 2 - ((minX + maxX) / 2) * next;
    panY = canvasH / 2 - ((minY + maxY) / 2) * next;
  }

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

  // Click a node → select it and glide the pan so it sits centred (scale
  // preserved). The transition runs because panning is off here.
  function focusNode(entity: EntityRow) {
    const node = nodes.get(entity.id);
    if (!node) return;
    selected = entity.id;
    panning = false;
    panX = canvasW / 2 - worldX(node.xPct) * scale;
    panY = canvasH / 2 - worldY(node.yPct) * scale;
  }

  function nodeTitle(entity: EntityRow): string {
    const where = entity.sources[0] ? ` — ${entity.sources[0]}` : "";
    return `${entity.summary || entity.name}${where}`;
  }

  function chipLabel(path: string): string {
    return path.split("/").pop() ?? path;
  }

  function kindLabel(kind: EntityKind): string {
    return KINDS.find((k) => k.kind === kind)?.label ?? kind;
  }
</script>

<div class="screen">
  {#if knowledge.empty}
    <div class="empty">
      {#if knowledge.error}
        <div class="error">Last refresh didn't finish — {knowledge.error}</div>
      {/if}
      <div class="empty-card">
        <h2>
          {knowledge.building
            ? "Ken is mapping this project…"
            : "Ken hasn't mapped this project yet"}
        </h2>
        <p>
          One pass over your documents finds the people, organizations,
          topics, and decisions — and how they connect.
        </p>
        {#if knowledge.building}
          <p class="pulse">
            This runs on its own after a project opens — a few minutes.
          </p>
        {:else if knowledge.claudeFound}
          <p class="note">
            Ken maps this automatically once indexing settles — and keeps
            the map current as your documents change.
          </p>
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
      onwheel={onWheel}
    >
      <div
        class="world"
        style:width="{WORLD_W}px"
        style:height="{WORLD_H}px"
        style:transform="translate({panX}px, {panY}px) scale({scale})"
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
              class:active={edge.active}
              class:dim={edge.dimmed}
              x1={worldX(edge.a.xPct)}
              y1={worldY(edge.a.yPct)}
              x2={worldX(edge.b.xPct)}
              y2={worldY(edge.b.yPct)}
              vector-effect="non-scaling-stroke"
            >
              {#if edge.label}<title>{edge.label}</title>{/if}
            </line>
          {/each}
        </svg>
        {#each model.entities as entity (entity.id)}
          {@const node = nodes.get(entity.id)}
          {@const nv = view.get(entity.id)}
          {#if node && nv && nv.visible}
            <button
              class="node kind-{entity.kind} {node.ring}"
              class:labeled={nv.labeled}
              class:dot={!nv.labeled}
              class:dimmed={nv.dimmed}
              class:matched={nv.matched}
              class:selected={selected === entity.id}
              style:left="{worldX(node.xPct)}px"
              style:top="{worldY(node.yPct)}px"
              title={nodeTitle(entity)}
              onpointerdown={(e) => e.stopPropagation()}
              onpointerenter={() => (hovered = entity.id)}
              onpointerleave={() => { if (hovered === entity.id) hovered = null; }}
              onclick={(e) => {
                e.stopPropagation();
                focusNode(entity);
              }}
            >
              <span class="tag" aria-hidden="true"></span>
              {#if nv.labeled}<span class="node-name">{entity.name}</span>{/if}
            </button>
          {/if}
        {/each}
      </div>

      <div class="toolbar">
        <input
          class="search"
          type="search"
          placeholder="Search the map…"
          aria-label="Search the map"
          bind:value={rawQuery}
          oninput={onQueryInput}
        />
        <div class="filters" role="group" aria-label="Filter by type">
          {#each KINDS as k (k.kind)}
            <button
              class="chip kind-{k.kind}"
              class:on={activeKinds.has(k.kind)}
              aria-pressed={activeKinds.has(k.kind)}
              onclick={() => toggleKind(k.kind)}
            >
              <span class="tag" aria-hidden="true"></span>{k.label}
            </button>
          {/each}
        </div>
      </div>

      <div class="zoom">
        <button class="btn btn-small" title="Zoom in" aria-label="Zoom in" onclick={() => zoomButton(1.25)}>+</button>
        <button class="btn btn-small" title="Zoom out" aria-label="Zoom out" onclick={() => zoomButton(0.8)}>−</button>
        <button class="btn btn-small" title="Fit to view" aria-label="Fit to view" onclick={zoomToFit}>Fit</button>
      </div>

      {#if knowledge.building}
        <div class="status-overlay pulse">Mapping this project…</div>
      {:else if knowledge.error}
        <div class="status-overlay error">Last map update didn't finish — {knowledge.error}</div>
      {/if}

      {#if selectedEntity}
        <div class="detail">
          <div class="detail-head">
            <span class="tag kind-{selectedEntity.kind}" aria-hidden="true"></span>
            <span class="detail-name">{selectedEntity.name}</span>
            <span class="detail-kind kind-{selectedEntity.kind}">{kindLabel(selectedEntity.kind)}</span>
          </div>
          {#if selectedEntity.summary}
            <p class="detail-summary">{selectedEntity.summary}</p>
          {/if}
          {#if connections.length > 0}
            <div class="detail-section">
              <div class="detail-label">Connected to</div>
              <ul class="conn-list">
                {#each connections as c (c.other.id)}
                  <li>
                    <button class="conn" onclick={() => focusNode(c.other)}>
                      <span class="tag kind-{c.other.kind}" aria-hidden="true"></span>
                      <span class="conn-name">{c.other.name}</span>
                      {#if c.label}<span class="conn-label">{c.label}</span>{/if}
                    </button>
                  </li>
                {/each}
              </ul>
            </div>
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
    opacity: 0.5;
  }
  line.dim {
    opacity: 0.12;
  }
  line.active {
    stroke: var(--accent);
    stroke-width: 2;
    opacity: 0.9;
  }

  /* Per-kind colour: each kind binds --k to one existing token family,
     reused by the node tag, the badge, and the filter chip so the
     encoding is identical everywhere. No new tokens, no literals. */
  .kind-person { --k: var(--accent); }
  .kind-organization { --k: var(--healthy); }
  .kind-topic { --k: var(--file-doc); }
  .kind-decision { --k: var(--needs-input); }
  .kind-other { --k: var(--ink-tertiary); }

  .node {
    position: absolute;
    transform: translate(-50%, -50%);
    display: flex;
    align-items: center;
    gap: 7px;
    border-radius: 9px;
    background: var(--surface);
    border: 1px solid var(--border-strong);
    color: var(--ink);
    white-space: nowrap;
    transition: opacity 0.2s ease;
  }
  .node .tag {
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--k);
  }
  .node.labeled {
    padding: 7px 12px;
    font-size: 12.5px;
    font-weight: 600;
    max-width: 220px;
  }
  .node.labeled .node-name {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  /* Undecorated nodes collapse to just their kind dot until zoom/hover. */
  .node.dot {
    padding: 5px;
    border-color: color-mix(in srgb, var(--k) 45%, var(--border));
  }
  .node.dimmed {
    opacity: 0.28;
  }
  .node:hover {
    z-index: 2;
    box-shadow: var(--shadow-card);
    opacity: 1;
    transform: translate(-50%, -50%) translateY(-1px);
  }
  .node.matched {
    box-shadow: 0 0 0 2px color-mix(in srgb, var(--needs-input) 70%, transparent);
  }
  .node.selected {
    z-index: 3;
    box-shadow: 0 0 0 2px var(--accent), var(--shadow-card);
  }

  .toolbar {
    position: absolute;
    top: 16px;
    right: 16px;
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 8px;
    max-width: min(60%, 360px);
  }
  .search {
    width: 240px;
    max-width: 100%;
    padding: 7px 12px;
    font-size: 12.5px;
    color: var(--ink);
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-control);
    box-shadow: var(--shadow-card);
  }
  .search:focus {
    outline: none;
    border-color: var(--accent-deep);
  }
  .filters {
    display: flex;
    flex-wrap: wrap;
    justify-content: flex-end;
    gap: 6px;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 4px 9px;
    font-size: 11.5px;
    font-weight: 500;
    color: var(--ink-secondary);
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 999px;
    box-shadow: var(--shadow-card);
  }
  .chip .tag {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--k);
  }
  .chip:hover {
    border-color: var(--border-strong);
    color: var(--ink);
  }
  .chip.on {
    color: var(--ink);
    border-color: color-mix(in srgb, var(--k) 55%, var(--border));
    background: color-mix(in srgb, var(--k) 12%, var(--surface));
  }

  .zoom {
    position: absolute;
    right: 16px;
    bottom: 16px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .zoom .btn {
    width: 32px;
    justify-content: center;
  }

  .status-overlay {
    position: absolute;
    top: 16px;
    left: 16px;
    font-size: 12.5px;
    color: var(--accent);
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 6px 12px;
    box-shadow: var(--shadow-card);
    max-width: 420px;
  }
  .status-overlay.error {
    color: var(--danger);
  }

  .detail {
    position: absolute;
    left: 16px;
    bottom: 16px;
    max-width: 320px;
    max-height: calc(100% - 32px);
    overflow-y: auto;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-card);
    box-shadow: var(--shadow-card);
    padding: 14px 16px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .detail-head {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .detail-head .tag {
    flex: none;
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: var(--k);
  }
  .detail-name {
    font-family: var(--font-serif);
    font-size: 15px;
    font-weight: 500;
    color: var(--ink);
    flex: 1;
    min-width: 0;
  }
  .detail-kind {
    flex: none;
    font-size: 10.5px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.03em;
    padding: 2px 7px;
    border-radius: 999px;
    color: color-mix(in srgb, var(--k) 75%, var(--ink));
    background: color-mix(in srgb, var(--k) 14%, transparent);
  }
  .detail-section {
    display: flex;
    flex-direction: column;
    gap: 5px;
  }
  .detail-label {
    font-size: 10.5px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--ink-tertiary);
  }
  .conn-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .conn {
    display: flex;
    align-items: center;
    gap: 7px;
    width: 100%;
    padding: 3px 6px;
    border-radius: 6px;
    font-size: 12px;
    color: var(--ink-secondary);
    text-align: left;
  }
  .conn:hover {
    background: var(--sunken);
    color: var(--ink);
  }
  .conn .tag {
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--k);
  }
  .conn-name {
    color: var(--ink);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .conn-label {
    color: var(--ink-tertiary);
    font-size: 11px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
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

<script lang="ts">
  import { onMount } from "svelte";
  import { app } from "../lib/app.svelte";
  import { knowledge } from "../lib/knowledge.svelte";
  import { layoutMap, type MapNode } from "../lib/knowledge";
  import { timeAgo } from "../lib/format";
  import type { EntityRow } from "../lib/api";

  onMount(() => void knowledge.visit());

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

  function open(entity: EntityRow) {
    const source = entity.sources[0];
    if (source) app.openInFiles(source);
  }

  function nodeTitle(entity: EntityRow): string {
    const where = entity.sources[0] ? ` — ${entity.sources[0]}` : "";
    return `${entity.summary || entity.name}${where}`;
  }
</script>

<div class="screen">
  <div class="head">
    <h1>Map</h1>
    {#if model?.builtAt}
      <span class="when">built {timeAgo(model.builtAt)}</span>
    {/if}
    <span class="spacer"></span>
    <button
      class="btn"
      disabled={knowledge.building}
      onclick={() => void knowledge.refresh()}
    >
      {knowledge.building ? "Mapping…" : "Refresh map"}
    </button>
  </div>

  {#if knowledge.error}
    <div class="error">Last refresh didn't finish — {knowledge.error}</div>
  {/if}

  {#if knowledge.empty}
    <div class="empty">
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
    <div class="canvas">
      <svg viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true">
        {#each drawnEdges as edge (edge.id)}
          <line
            x1={edge.a.xPct}
            y1={edge.a.yPct}
            x2={edge.b.xPct}
            y2={edge.b.yPct}
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
            style:left="{node.xPct}%"
            style:top="{node.yPct}%"
            title={nodeTitle(entity)}
            onclick={() => open(entity)}
          >
            {entity.name}
          </button>
        {/if}
      {/each}
      {#if hasUnconnected}
        <div class="legend">Dashed = mentioned but unconnected</div>
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
  .head {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 24px 44px 16px;
  }
  h1 {
    margin: 0;
    font-family: var(--font-serif);
    font-size: 28px;
    font-weight: 500;
    letter-spacing: -0.01em;
  }
  .when {
    font-size: 12px;
    color: var(--ink-tertiary);
  }
  .spacer {
    flex: 1;
  }
  .error {
    margin: 0 44px 10px;
    font-size: 12.5px;
    color: var(--danger);
  }

  .canvas {
    flex: 1;
    min-height: 0;
    position: relative;
    overflow: hidden;
    background: var(--surface);
    border-top: 1px solid var(--border);
  }
  svg {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
  }
  line {
    stroke: var(--border-strong);
    stroke-width: 1.5;
    vector-effect: non-scaling-stroke;
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
    background: rgba(168, 116, 44, 0.08);
    border: 1px dashed rgba(168, 116, 44, 0.5);
    color: var(--needs-input-text);
    font-weight: 500;
  }
  .node.mono-label {
    font-family: var(--font-mono);
    font-size: 11.5px;
    font-weight: 500;
  }

  .legend {
    position: absolute;
    left: 16px;
    bottom: 14px;
    font-size: 11.5px;
    color: var(--ink-tertiary);
  }

  .empty {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    background: var(--surface);
    border-top: 1px solid var(--border);
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

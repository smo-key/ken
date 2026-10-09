<script lang="ts">
  import type { ToolCard } from "../lib/api";

  let { card }: { card: ToolCard } = $props();

  // The result is folded away until asked for; a failure opens by itself.
  let open = $state(false);
  $effect(() => {
    if (card.status === "error") open = true;
  });
  const hasResult = $derived(!!card.result && card.result.trim().length > 0);
</script>

<div class="card" class:error={card.status === "error"}>
  <button
    class="head"
    onclick={() => (open = !open)}
    disabled={!hasResult}
    aria-expanded={hasResult ? open : undefined}
  >
    <span class="state {card.status}" aria-label={card.status}>
      {#if card.status === "running"}<span class="spin"></span>
      {:else if card.status === "error"}!
      {:else}✓{/if}
    </span>
    <span class="summary mono">{card.summary}</span>
    {#if hasResult}<span class="chev" class:open>›</span>{/if}
  </button>
  {#if open && hasResult}
    <pre class="result mono">{card.result}</pre>
  {/if}
</div>

<style>
  .card {
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--surface);
    overflow: hidden;
  }
  .card.error {
    border-color: var(--danger);
  }
  .head {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 6px 10px;
    background: none;
    border: none;
    text-align: left;
    color: var(--ink-secondary, var(--ink));
    font-size: 11.5px;
    cursor: pointer;
  }
  .head:disabled {
    cursor: default;
  }
  .head:not(:disabled):hover {
    background: var(--sunken);
  }
  .state {
    flex: none;
    width: 14px;
    text-align: center;
    font-size: 11px;
    color: var(--ink-tertiary);
  }
  .state.error {
    color: var(--danger);
    font-weight: 600;
  }
  .spin {
    display: inline-block;
    width: 9px;
    height: 9px;
    border: 1.5px solid var(--border-strong);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  .summary {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .chev {
    flex: none;
    color: var(--ink-tertiary);
    transition: transform 0.15s;
  }
  .chev.open {
    transform: rotate(90deg);
  }
  .result {
    margin: 0;
    padding: 8px 10px;
    border-top: 1px solid var(--border);
    background: var(--sunken);
    font-size: 11px;
    line-height: 1.5;
    color: var(--ink-tertiary);
    white-space: pre-wrap;
    word-break: break-word;
    max-height: 180px;
    overflow: auto;
  }
  @media (prefers-reduced-motion: reduce) {
    .spin {
      animation: none;
    }
  }
</style>

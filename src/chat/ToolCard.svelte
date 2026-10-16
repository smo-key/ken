<script lang="ts">
  import type { ToolCard } from "../lib/api";
  import { cardHits } from "./toolCard";

  let { card }: { card: ToolCard } = $props();

  // The result is folded away until asked for; a failure opens by itself.
  let open = $state(false);
  $effect(() => {
    if (card.status === "error") open = true;
  });
  const hasResult = $derived(!!card.result && card.result.trim().length > 0);
  // A search's hits, listed while the result is folded: each place, and a
  // code hit's symbol beside it.
  const hits = $derived(cardHits(card.result));
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
  {:else if hits.length > 0}
    <ul class="hits">
      {#each hits as hit, i (i)}
        <li class="mono">
          <span class="place">{hit.place}</span>
          {#if hit.symbol}<span class="symbol" title="The definition this hit holds">{hit.symbol}</span>{/if}
        </li>
      {/each}
    </ul>
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
  .hits {
    margin: 0;
    padding: 4px 10px 6px 32px;
    list-style: none;
    font-size: 11px;
    line-height: 1.5;
    color: var(--ink-tertiary);
  }
  .hits li {
    display: flex;
    gap: 6px;
    min-width: 0;
  }
  .place {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .symbol {
    flex: none;
    color: var(--ink-secondary, var(--ink));
  }
  @media (prefers-reduced-motion: reduce) {
    .spin {
      animation: none;
    }
  }
</style>

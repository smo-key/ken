<script lang="ts">
  import ChevronDown from "@lucide/svelte/icons/chevron-down";
  import ChevronUp from "@lucide/svelte/icons/chevron-up";
  import X from "@lucide/svelte/icons/x";
  import { find } from "../lib/find.svelte";

  let input = $state<HTMLInputElement | null>(null);

  // ⌘F on an already-open bar (or the magnifier button) re-focuses and selects,
  // so a second ⌘F means "search for something else", not "nothing happened".
  $effect(() => {
    void find.focusToken;
    input?.focus();
    input?.select();
  });

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      if (e.shiftKey) find.previous();
      else find.next();
    } else if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      find.close();
    }
  }
</script>

<div class="find">
  <div class="row">
    <input
      bind:this={input}
      class="query"
      type="text"
      placeholder="Find in document"
      spellcheck="false"
      value={find.query}
      oninput={(e) => find.setQuery(e.currentTarget.value)}
      onkeydown={onKeydown}
    />
    <span class="counter" class:empty={find.total === 0}>{find.counter}</span>
    <button
      class="step"
      aria-label="Previous match"
      title="Previous match (⇧⏎)"
      disabled={find.total === 0}
      onclick={() => find.previous()}
    >
      <ChevronUp size={13} strokeWidth={1.75} />
    </button>
    <button
      class="step"
      aria-label="Next match"
      title="Next match (⏎)"
      disabled={find.total === 0}
      onclick={() => find.next()}
    >
      <ChevronDown size={13} strokeWidth={1.75} />
    </button>
    <button class="step" aria-label="Close find" title="Close (Esc)" onclick={() => find.close()}>
      <X size={13} strokeWidth={1.75} />
    </button>
  </div>
  {#if find.note}
    <div class="note">{find.note}</div>
  {/if}
</div>

<style>
  .find {
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 2px;
  }
  .query {
    width: 150px;
    border: none;
    outline: none;
    background: transparent;
    color: var(--ink);
    font-size: 12px;
    padding: 3px 6px;
  }
  .query::placeholder {
    color: var(--ink-tertiary);
  }
  .counter {
    font-size: 11px;
    color: var(--ink-secondary);
    white-space: nowrap;
    padding: 0 4px;
    font-variant-numeric: tabular-nums;
  }
  .counter.empty {
    color: var(--ink-tertiary);
  }
  .step {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    border: none;
    border-radius: 6px;
    background: transparent;
    color: var(--ink-secondary);
    flex: none;
  }
  .step:hover:not(:disabled) {
    background: var(--sunken);
    color: var(--ink);
  }
  .step:disabled {
    color: var(--ink-tertiary);
    opacity: 0.5;
  }
  .note {
    max-width: 260px;
    padding: 0 6px 2px;
    font-size: 10.5px;
    line-height: 1.4;
    color: var(--ink-tertiary);
  }
</style>

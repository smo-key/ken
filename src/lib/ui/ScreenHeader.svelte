<script lang="ts">
  // The one screen header (the copy rule, docs/design/design-tokens.md): the
  // screen's title, beside it at most a count or an id (never a sentence or a
  // path), and on the right the screen's actions and its count, "4 of 4".
  // No lede goes under it.
  import type { Snippet } from "svelte";

  interface Props {
    title: string;
    /** A count or an id: "3 waiting", the repo's name. */
    sub?: string | null;
    /** Rows shown of all rows, read as "4 of 4". */
    count?: { shown: number; total: number } | null;
    /** The screen's own actions, right-aligned before the count. */
    actions?: Snippet;
  }

  let { title, sub = null, count = null, actions }: Props = $props();
</script>

<header class="screen-head">
  <h1>{title}</h1>
  {#if sub}<span class="sub">{sub}</span>{/if}
  {#if actions || count}
    <div class="right">
      {@render actions?.()}
      {#if count}<span class="count">{count.shown} of {count.total}</span>{/if}
    </div>
  {/if}
</header>

<style>
  .screen-head {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 4px 10px;
    min-width: 0;
  }
  h1 {
    margin: 0;
    min-width: 0;
    font-family: var(--font-serif);
    font-size: 28px;
    font-weight: 500;
    line-height: 1.15;
    letter-spacing: -0.01em;
  }
  .sub {
    font-size: 12.5px;
    color: var(--ink-3);
    white-space: nowrap;
  }
  .right {
    margin-left: auto;
    align-self: center;
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .count {
    font-size: 12.5px;
    color: var(--ink-3);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
</style>

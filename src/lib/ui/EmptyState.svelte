<script lang="ts">
  // The one empty state (the copy rule, docs/design/design-tokens.md): a
  // short title, or "none" inside a list, and at most one action. Never a
  // paragraph and never a how-to.
  import type { Snippet } from "svelte";

  interface Props {
    /** What is empty, as a short noun phrase ("No file open"). Absent: the
     *  list's "none". */
    title?: string;
    /** On a card ground, as in Home's rail. */
    card?: boolean;
    /** Centred in the space it is given, as an empty pane. */
    center?: boolean;
    action?: Snippet;
  }

  let { title, card = false, center = false, action }: Props = $props();
</script>

{#if title}
  <div class="k-empty" class:card class:center>
    <span class="head">{title}</span>
    {#if action}<div class="act">{@render action()}</div>{/if}
  </div>
{:else}
  <p class="none">none</p>
{/if}

<style>
  .card {
    background: var(--surface);
    border: 1px solid var(--line);
  }
  .center {
    flex: 1;
    align-items: center;
    justify-content: center;
  }
  .act {
    display: flex;
    gap: 8px;
    margin-top: 6px;
  }
  .none {
    margin: 0;
    padding: 6px 0;
    font-size: 13px;
    color: var(--ink-3);
  }
</style>

<script lang="ts">
  // A proposed change to a wiki page a person keeps (a held edit, a ticket):
  // the page as it is against the page with the change. Formatted by
  // default, block by block as the page reads (`blockDiff`), an added block
  // green and a removed one red and struck; the raw lines are one click
  // away. Applying writes it; the card's own buttons do that.
  import { buildDiffRows, proposalPayload, type DiffRow } from "../lib/conflicts";
  import { buildBlockRows, type BlockRow } from "../lib/blockDiff";
  import { renderBody } from "../lib/markdown";

  let { payload }: { payload: string | null } = $props();

  let raw = $state(false);
  const p = $derived(proposalPayload(payload));
  const rows = $derived<DiffRow[]>(p && raw ? buildDiffRows(p.base, p.proposed) : []);
  const blocks = $derived<BlockRow[]>(p && !raw ? buildBlockRows(p.base, p.proposed) : []);

  /** A link inside the diff goes nowhere: the page is not written yet. */
  function inert(e: MouseEvent) {
    if ((e.target as HTMLElement).closest("a")) e.preventDefault();
  }
</script>

{#if p}
  <div class="diff">
    <div class="diff-head">
      <span class="side del-side" title={p.page}>− now</span>
      <span class="side add-side">+ with the change</span>
      <span class="grow"></span>
      <button class="view" aria-pressed={raw} onclick={() => (raw = !raw)}>{raw ? "Formatted" : "Raw lines"}</button>
    </div>
    {#if raw}
      <div class="diff-body mono">
        {#each rows as row, i (i)}
          {#if row.type === "gap"}
            <div class="line gap">⋯ {row.count} unchanged {row.count === 1 ? "line" : "lines"}</div>
          {:else}
            <div class="line {row.type}">
              <span class="gutter">{row.type === "add" ? "+" : row.type === "del" ? "−" : " "}</span>
              <span class="text">{row.text === "" ? " " : row.text}</span>
            </div>
          {/if}
        {/each}
      </div>
    {:else}
      <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
      <div class="diff-body blocks" onclick={inert}>
        {#each blocks as row, i (i)}
          {#if row.type === "gap"}
            <div class="line gap">⋯ {row.count} unchanged {row.count === 1 ? "line" : "lines"}</div>
          {:else if row.frontmatter}
            <pre class="block front {row.type}">{row.text}</pre>
          {:else}
            <div class="block md {row.type}">{@html renderBody(row.text)}</div>
          {/if}
        {/each}
      </div>
    {/if}
  </div>
{/if}

<style>
  .diff {
    border: 1px solid var(--sunken);
    border-radius: 9px;
    overflow: hidden;
    background: var(--surface);
    margin: 10px 0;
  }
  .diff-head {
    display: flex;
    align-items: center;
    gap: 16px;
    flex-wrap: wrap;
    padding: 8px 12px;
    border-bottom: 1px solid var(--sunken);
    background: var(--sunken-2);
    font-size: 11px;
    font-weight: 700;
    letter-spacing: 0.03em;
  }
  .del-side {
    color: var(--danger);
  }
  .add-side {
    color: var(--healthy-text);
  }
  .grow {
    flex: 1;
  }
  .view {
    border: none;
    background: transparent;
    padding: 0;
    font: inherit;
    font-weight: 600;
    letter-spacing: 0;
    color: var(--ink-tertiary);
    cursor: pointer;
  }
  .view:hover {
    color: var(--ink);
  }
  .diff-body {
    max-height: 360px;
    overflow: auto;
    font-size: 12px;
    line-height: 1.55;
    padding: 4px 0;
  }
  .line {
    display: flex;
    gap: 8px;
    padding: 0 12px;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .line .gutter {
    flex: none;
    width: 0.9em;
    text-align: center;
    color: var(--ink-tertiary);
    user-select: none;
  }
  .line .text {
    flex: 1;
    min-width: 0;
  }
  .line.del {
    background: color-mix(in srgb, var(--danger) 9%, transparent);
  }
  .line.del .gutter {
    color: var(--danger);
  }
  .line.add {
    background: color-mix(in srgb, var(--healthy) 10%, transparent);
  }
  .line.add .gutter {
    color: var(--healthy-text);
  }
  .line.gap {
    padding: 3px 12px;
    color: var(--ink-tertiary);
    font-size: 11px;
    background: var(--sunken-2);
  }
  /* The formatted view: each block as the page draws it, a removed one
     struck and an added one marked, at the block's left edge too. */
  .blocks {
    font-size: 13.5px;
    line-height: 1.6;
    color: var(--ink-2);
    padding: 6px 0;
  }
  .block {
    padding: 4px 12px 4px 14px;
    border-left: 3px solid transparent;
    overflow-wrap: anywhere;
  }
  .block.add {
    background: color-mix(in srgb, var(--healthy) 10%, transparent);
    border-left-color: var(--healthy);
  }
  .block.del {
    background: color-mix(in srgb, var(--danger) 8%, transparent);
    border-left-color: var(--danger);
    text-decoration: line-through;
    text-decoration-color: color-mix(in srgb, var(--danger) 60%, transparent);
    color: var(--ink-tertiary);
  }
  .block.front {
    margin: 0;
    font-family: var(--font-mono);
    font-size: 11.5px;
    white-space: pre-wrap;
  }
  .block.md :global(:is(p, ul, ol, table, pre, blockquote, h1, h2, h3, h4)) {
    margin: 0;
  }
  .block.md :global(:is(h1, h2, h3, h4)) {
    font-family: var(--font-serif);
    font-weight: 500;
    color: inherit;
  }
  .block.md :global(h1) {
    font-size: 19px;
  }
  .block.md :global(h2) {
    font-size: 17px;
  }
  .block.md :global(:is(h3, h4)) {
    font-size: 15px;
  }
  .block.md :global(:is(ul, ol)) {
    padding-left: 20px;
  }
  .block.md :global(code) {
    font-family: var(--font-mono);
    font-size: 0.88em;
  }
  .block.md :global(pre) {
    font-family: var(--font-mono);
    font-size: 12px;
    white-space: pre-wrap;
  }
  .block.md :global(table) {
    border-collapse: collapse;
    font-size: 12.5px;
  }
  .block.md :global(:is(th, td)) {
    border: 1px solid var(--line);
    padding: 3px 8px;
    text-align: left;
  }
  .block.md :global(a) {
    color: var(--accent-ink);
    text-decoration: underline;
    text-underline-offset: 2px;
  }
  .block.md :global(blockquote) {
    padding-left: 10px;
    border-left: 2px solid var(--line);
  }
</style>

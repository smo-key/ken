<script lang="ts">
  // A proposed change to a wiki page a person keeps (a held edit, a ticket):
  // the page as it is against the page with the change, as a line diff.
  // Applying writes it; the card's own buttons do that.
  import { buildDiffRows, proposalPayload, type DiffRow } from "../lib/conflicts";

  let { payload }: { payload: string | null } = $props();

  const p = $derived(proposalPayload(payload));
  const rows = $derived<DiffRow[]>(p ? buildDiffRows(p.base, p.proposed) : []);
</script>

{#if p}
  <div class="diff">
    <div class="diff-head">
      <span class="side del-side">− {p.page} now</span>
      <span class="side add-side">+ with the change</span>
    </div>
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
</style>

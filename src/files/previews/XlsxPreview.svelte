<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "../../lib/api";

  let { relPath }: { relPath: string } = $props();

  let sheets = $state<{ name: string; html: string }[]>([]);
  let active = $state(0);
  let error = $state<string | null>(null);

  onMount(async () => {
    try {
      const XLSX = await import("xlsx");
      const bytes = await api.readFileBytes(relPath);
      const wb = XLSX.read(new Uint8Array(bytes), { type: "array" });
      sheets = wb.SheetNames.map((name) => ({
        name,
        html: XLSX.utils.sheet_to_html(wb.Sheets[name], { header: "", footer: "" }),
      }));
    } catch (e) {
      error = `Couldn't render this workbook — ${e}. Try “Open in default app”.`;
    }
  });
</script>

<div class="pane">
  {#if error}
    <div class="note error">{error}</div>
  {:else if sheets.length === 0}
    <div class="note">Rendering workbook…</div>
  {:else}
    {#if sheets.length > 1}
      <div class="tabs">
        {#each sheets as sheet, i (sheet.name)}
          <button class:active={active === i} onclick={() => (active = i)}>
            {sheet.name}
          </button>
        {/each}
      </div>
    {/if}
    <div class="grid">{@html sheets[active].html}</div>
  {/if}
</div>

<style>
  .pane {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .tabs {
    display: flex;
    gap: 4px;
    padding: 10px 20px 0;
    border-bottom: 1px solid var(--sunken);
    flex: none;
  }
  .tabs button {
    font-size: 12px;
    font-weight: 600;
    padding: 6px 12px;
    border-radius: 8px 8px 0 0;
    border: 1px solid transparent;
    background: transparent;
    color: var(--ink-secondary);
  }
  .tabs button.active {
    background: var(--paper);
    border-color: var(--border);
    border-bottom-color: var(--paper);
    color: var(--accent-deep);
  }
  .grid {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: 16px 20px 60px;
  }
  .grid :global(table) {
    border-collapse: collapse;
    font-size: 12.5px;
  }
  .grid :global(td) {
    border: 1px solid var(--border);
    padding: 5px 10px;
    white-space: nowrap;
    font-family: var(--font-mono);
    font-size: 12px;
  }
  .grid :global(tr:first-child td) {
    font-weight: 600;
    background: var(--paper);
    font-family: var(--font-sans);
  }
  .note {
    text-align: center;
    color: var(--ink-tertiary);
    font-size: 13px;
    padding: 20px;
  }
  .note.error {
    color: var(--danger);
  }
</style>

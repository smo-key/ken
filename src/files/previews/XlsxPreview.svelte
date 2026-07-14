<script lang="ts">
  import { onMount } from "svelte";
  import type { WorkBook } from "xlsx";
  import { api } from "../../lib/api";
  import { findCellMatches, type CellMatch } from "../../lib/find";
  import {
    clearHighlights,
    highlightMatches,
    scrollMarkIntoView,
    setCurrent,
  } from "../../lib/find-dom";
  import { find, type FindAdapter } from "../../lib/find.svelte";
  import PreviewLoading from "./PreviewLoading.svelte";

  let { relPath }: { relPath: string } = $props();

  let sheets = $state<{ name: string; html: string }[]>([]);
  let active = $state(0);
  let error = $state<string | null>(null);

  let grid = $state<HTMLDivElement | null>(null);
  let matches = $state<CellMatch[]>([]);
  let marks: HTMLElement[] = [];

  // Cell values, one grid per sheet — built once, on the first search, because a
  // 50-sheet workbook shouldn't pay for a search nobody ran. findCellMatches caps
  // how many cells it will scan.
  let workbook: WorkBook | null = null;
  let xlsx: typeof import("xlsx") | null = null;
  let values: string[][][] | null = null;

  function cellValues(): string[][][] {
    if (values) return values;
    if (!workbook || !xlsx) return [];
    values = workbook.SheetNames.map((name) =>
      (
        xlsx!.utils.sheet_to_json(workbook!.Sheets[name], {
          header: 1,
          raw: false,
          defval: "",
          blankrows: true,
        }) as unknown[][]
      ).map((row) => row.map((cell) => String(cell ?? ""))),
    );
    return values;
  }

  // The rendered table is replaced wholesale when the sheet changes, so the
  // highlights are (re)painted here — after the swap, and after any index move.
  $effect(() => {
    void active;
    void matches;
    const el = grid;
    if (!el) return;
    const query = find.open ? find.query : "";
    marks = highlightMatches(el, query);
    const m = matches[find.index];
    if (m && m.sheet === active) {
      scrollMarkIntoView(setCurrent(marks, m.occurrence));
    }
    return () => clearHighlights(el);
  });

  $effect(() => {
    const adapter: FindAdapter = {
      search(query) {
        const result = findCellMatches(cellValues(), query);
        matches = result.matches;
        return { total: result.matches.length, capped: result.capped };
      },
      reveal(index) {
        // Stepping onto a hit in another sheet selects that sheet; the effect
        // above repaints and scrolls once the new table is in the DOM.
        const m = matches[index];
        if (m) active = m.sheet;
      },
      clear() {
        matches = [];
      },
    };
    find.register(adapter);
    return () => find.unregister(adapter);
  });

  let strip = $state<HTMLDivElement | null>(null);
  let tabEls = $state<(HTMLButtonElement | null)[]>([]);

  // Keep the selected sheet reachable when the strip has scrolled past it (mouse or keyboard).
  // "nearest" on both axes so bringing a tab into view can't scroll the surrounding pane vertically.
  $effect(() => {
    tabEls[active]?.scrollIntoView({ block: "nearest", inline: "nearest" });
  });

  // A vertical wheel over a horizontal tab bar is expected to scroll it sideways; the listener is
  // registered manually because Svelte marks `onwheel` passive, which forbids preventDefault().
  $effect(() => {
    const el = strip;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      if (e.deltaX !== 0 || el.scrollWidth <= el.clientWidth) return;
      e.preventDefault();
      el.scrollLeft += e.deltaY;
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  });

  onMount(async () => {
    try {
      const XLSX = await import("xlsx");
      const bytes = await api.readFileBytes(relPath);
      const wb = XLSX.read(new Uint8Array(bytes), { type: "array" });
      xlsx = XLSX;
      workbook = wb;
      sheets = wb.SheetNames.map((name) => ({
        name,
        html: XLSX.utils.sheet_to_html(wb.Sheets[name], { header: "", footer: "" }),
      }));
      find.refresh(); // the workbook landed after the user already typed a query
    } catch (e) {
      error = `Couldn't render this workbook — ${e}. Try “Open in default app”.`;
    }
  });
</script>

<div class="pane">
  {#if error}
    <div class="note error">{error}</div>
  {:else if sheets.length === 0}
    <PreviewLoading label="Rendering workbook…" />
  {:else}
    {#if sheets.length > 1}
      <div class="tabs" bind:this={strip}>
        {#each sheets as sheet, i (sheet.name)}
          <button
            bind:this={tabEls[i]}
            class:active={active === i}
            onclick={() => (active = i)}
          >
            {sheet.name}
          </button>
        {/each}
      </div>
    {/if}
    <div class="grid" bind:this={grid}>{@html sheets[active].html}</div>
  {/if}
</div>

<style>
  .pane {
    flex: 1;
    /* min-width:0 keeps a wide tab strip from stretching the pane past the preview area. */
    min-width: 0;
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
    overflow-x: auto;
    overflow-y: hidden;
    /* Same quiet treatment as the file tabstrip in FilesScreen: scroll, but no visible bar. */
    scrollbar-width: none;
  }
  .tabs::-webkit-scrollbar {
    display: none;
  }
  .tabs button {
    /* Tabs keep their natural width instead of squashing once the strip overflows. */
    flex: none;
    white-space: nowrap;
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

<script lang="ts">
  import { tick } from "svelte";
  import { parseDelimited, serializeDelimited } from "../lib/csv";

  let {
    initial,
    delimiter,
    onchange,
  }: {
    initial: string;
    delimiter: string;
    onchange: (text: string) => void;
  } = $props();

  // Parse into a rectangular grid; guarantee at least one editable cell so an
  // empty file still opens as a usable grid rather than a blank pane.
  function toGrid(text: string): string[][] {
    const parsed = parseDelimited(text, delimiter);
    const width = Math.max(1, ...parsed.map((r) => r.length));
    const grid = parsed.map((r) => {
      const row = r.slice();
      while (row.length < width) row.push("");
      return row;
    });
    if (grid.length === 0) return [Array(width).fill("")];
    return grid;
  }

  let rows = $state<string[][]>(toGrid(initial));
  const cols = $derived(rows[0]?.length ?? 0);

  let gridEl = $state<HTMLDivElement | undefined>();

  function emit() {
    onchange(serializeDelimited(rows, delimiter));
  }

  function setCell(r: number, c: number, value: string) {
    rows[r][c] = value;
    emit();
  }

  function addRow() {
    rows.push(Array(cols).fill(""));
    emit();
  }

  function addColumn() {
    for (const row of rows) row.push("");
    emit();
  }

  function deleteRow(r: number) {
    if (rows.length <= 1) return; // keep the header row
    rows.splice(r, 1);
    emit();
  }

  function deleteColumn(c: number) {
    if (cols <= 1) return;
    for (const row of rows) row.splice(c, 1);
    emit();
  }

  async function focusCell(r: number, c: number) {
    await tick();
    const el = gridEl?.querySelector<HTMLInputElement>(
      `input[data-r="${r}"][data-c="${c}"]`,
    );
    el?.focus();
    el?.select();
  }

  function onKeydown(e: KeyboardEvent, r: number, c: number) {
    if (e.key === "Enter") {
      e.preventDefault();
      if (e.shiftKey) {
        if (r > 0) void focusCell(r - 1, c);
      } else {
        if (r === rows.length - 1) addRow();
        void focusCell(r + 1, c);
      }
    } else if (e.key === "ArrowDown" && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      void focusCell(rows.length - 1, c);
    } else if (e.key === "ArrowUp" && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      void focusCell(0, c);
    }
  }
</script>

<div class="csv">
  <div class="toolbar">
    <button class="chip" onclick={addRow}>+ Row</button>
    <button class="chip" onclick={addColumn}>+ Column</button>
    <span class="dims">
      {rows.length - 1} row{rows.length - 1 === 1 ? "" : "s"} · {cols} column{cols ===
      1
        ? ""
        : "s"}
    </span>
  </div>

  <div class="grid-scroll" bind:this={gridEl}>
    <table>
      <thead>
        <tr>
          <th class="corner" aria-hidden="true"></th>
          {#each rows[0] as _cell, c (c)}
            <th class="head-cell">
              <input
                class="cell head"
                data-r="0"
                data-c={c}
                value={rows[0][c]}
                oninput={(e) => setCell(0, c, e.currentTarget.value)}
                onkeydown={(e) => onKeydown(e, 0, c)}
                spellcheck="false"
              />
              <button
                class="del col-del"
                title="Delete column"
                aria-label="Delete column"
                onclick={() => deleteColumn(c)}
                disabled={cols <= 1}>×</button
              >
            </th>
          {/each}
        </tr>
      </thead>
      <tbody>
        {#each rows.slice(1) as row, i (i)}
          {@const r = i + 1}
          <tr>
            <td class="rownum">
              <span class="num">{r}</span>
              <button
                class="del row-del"
                title="Delete row"
                aria-label="Delete row"
                onclick={() => deleteRow(r)}>×</button
              >
            </td>
            {#each row as _cell, c (c)}
              <td>
                <input
                  class="cell"
                  data-r={r}
                  data-c={c}
                  value={rows[r][c]}
                  oninput={(e) => setCell(r, c, e.currentTarget.value)}
                  onkeydown={(e) => onKeydown(e, r, c)}
                  spellcheck="false"
                />
              </td>
            {/each}
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
</div>

<style>
  .csv {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .toolbar {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 20px;
    border-bottom: 1px solid var(--sunken);
    flex: none;
  }
  .chip {
    border: 1px solid var(--border-strong);
    background: var(--surface);
    color: var(--ink-secondary);
    font-size: 12px;
    font-weight: 600;
    padding: 4px 10px;
    border-radius: 7px;
    box-shadow: var(--shadow-control);
  }
  .chip:hover {
    background: var(--paper);
    color: var(--ink);
  }
  .dims {
    font-size: 11.5px;
    color: var(--ink-tertiary);
    margin-left: 4px;
  }
  .grid-scroll {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: 0 0 60px;
  }
  table {
    border-collapse: separate;
    border-spacing: 0;
    font-size: 12.5px;
  }
  th,
  td {
    border-right: 1px solid var(--border);
    border-bottom: 1px solid var(--border);
    padding: 0;
    margin: 0;
  }
  /* Sticky header row */
  thead th {
    position: sticky;
    top: 0;
    z-index: 2;
    background: var(--paper);
    border-top: 1px solid var(--border);
  }
  .corner {
    position: sticky;
    left: 0;
    z-index: 3;
    width: 44px;
    min-width: 44px;
    background: var(--paper);
    border-left: 1px solid var(--border);
  }
  .head-cell {
    position: relative;
  }
  .rownum {
    position: sticky;
    left: 0;
    z-index: 1;
    width: 44px;
    min-width: 44px;
    background: var(--sunken-2);
    border-left: 1px solid var(--border);
    text-align: center;
    color: var(--ink-tertiary);
    font-family: var(--font-mono);
    font-size: 11px;
    vertical-align: middle;
    position: relative;
  }
  .rownum .num {
    display: inline-block;
    padding: 0 4px;
  }
  .cell {
    display: block;
    width: 160px;
    min-width: 96px;
    box-sizing: border-box;
    border: none;
    outline: none;
    background: transparent;
    color: var(--ink);
    font-family: var(--font-mono);
    font-size: 12px;
    padding: 6px 10px;
  }
  .cell:focus {
    background: var(--surface);
    box-shadow: inset 0 0 0 2px var(--accent);
  }
  .cell.head {
    font-family: var(--font-sans);
    font-weight: 600;
    color: var(--ink);
    padding-right: 22px;
  }
  /* Delete affordances — subtle until hovered */
  .del {
    border: none;
    background: none;
    color: var(--ink-tertiary);
    font-size: 13px;
    line-height: 1;
    padding: 2px 4px;
    border-radius: 4px;
    opacity: 0;
    transition: opacity 0.1s;
  }
  .del:hover {
    color: var(--danger);
    background: rgba(163, 77, 63, 0.1);
  }
  .del:disabled {
    display: none;
  }
  .col-del {
    position: absolute;
    top: 50%;
    right: 4px;
    transform: translateY(-50%);
  }
  .head-cell:hover .del,
  .rownum:hover .del {
    opacity: 1;
  }
  .row-del {
    position: absolute;
    top: 50%;
    right: 2px;
    transform: translateY(-50%);
  }
</style>

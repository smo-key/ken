<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "../../lib/api";
  import { renderMarkdown } from "../../lib/markdown";
  import { parseNotebook, type IpynbCell } from "../../lib/ipynb";

  let { relPath }: { relPath: string } = $props();

  let cells = $state<IpynbCell[] | null>(null);
  let error = $state<string | null>(null);

  onMount(async () => {
    try {
      const text = await api.readFile(relPath);
      cells = parseNotebook(text).cells;
    } catch (e) {
      error = `Couldn't render this notebook — ${e}. Try “Open in default app”.`;
    }
  });
</script>

<div class="scroll">
  {#if error}
    <div class="note error">{error}</div>
  {:else if cells === null}
    <div class="note">Rendering notebook…</div>
  {:else if cells.length === 0}
    <div class="note">This notebook has no cells.</div>
  {:else}
    <div class="nb">
      {#each cells as cell, i (i)}
        {#if cell.type === "markdown"}
          <div class="md">{@html renderMarkdown(cell.source)}</div>
        {:else if cell.type === "code"}
          <div class="code-cell">
            <div class="code-row">
              <span class="prompt"
                >In [{cell.executionCount ?? " "}]:</span
              >
              <pre class="src"><code>{cell.source}</code></pre>
            </div>
            {#each cell.outputs as out, j (j)}
              {#if out.kind === "stream"}
                <pre class="out stream">{out.text}</pre>
              {:else if out.kind === "text"}
                <pre class="out">{out.text}</pre>
              {:else if out.kind === "image"}
                <img
                  class="out-img"
                  src={`data:${out.mime};base64,${out.data}`}
                  alt="cell output"
                />
              {:else if out.kind === "error"}
                <pre class="out err">{out.traceback ||
                    `${out.ename}: ${out.evalue}`}</pre>
              {/if}
            {/each}
          </div>
        {:else if cell.source.trim()}
          <pre class="out raw">{cell.source}</pre>
        {/if}
      {/each}
    </div>
  {/if}
</div>

<style>
  .scroll {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }
  .nb {
    max-width: 820px;
    margin: 0 auto;
    padding: 32px clamp(20px, 5%, 48px) 80px;
    display: flex;
    flex-direction: column;
    gap: 18px;
  }
  .md {
    font-size: 14.5px;
    line-height: 1.75;
  }
  .md :global(h1),
  .md :global(h2),
  .md :global(h3) {
    font-family: var(--font-serif);
    font-weight: 500;
    letter-spacing: -0.01em;
  }
  .md :global(pre) {
    background: var(--paper);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 12px 14px;
    overflow-x: auto;
    font-size: 12.5px;
  }
  .md :global(table) {
    border-collapse: collapse;
  }
  .md :global(td),
  .md :global(th) {
    border: 1px solid var(--border);
    padding: 5px 10px;
    font-size: 13px;
  }
  .code-cell {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .code-row {
    display: flex;
    gap: 10px;
    align-items: flex-start;
  }
  .prompt {
    flex: none;
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--needs-input-text);
    padding-top: 12px;
    white-space: nowrap;
    user-select: none;
  }
  .src {
    flex: 1;
    min-width: 0;
    margin: 0;
    padding: 10px 14px;
    background: var(--paper);
    border: 1px solid var(--border);
    border-radius: 8px;
    overflow-x: auto;
  }
  .src code {
    font-family: var(--font-mono);
    font-size: 12.5px;
    line-height: 1.6;
    white-space: pre;
  }
  .out {
    margin: 0 0 0 60px;
    padding: 8px 14px;
    background: var(--sunken-2);
    border: 1px solid var(--border);
    border-radius: 8px;
    font-family: var(--font-mono);
    font-size: 12px;
    line-height: 1.6;
    white-space: pre-wrap;
    word-break: break-word;
    overflow-x: auto;
  }
  .out.err {
    background: color-mix(in srgb, var(--danger) 7%, transparent);
    border-color: color-mix(in srgb, var(--danger) 25%, transparent);
    color: var(--danger);
  }
  .out.raw {
    margin-left: 0;
  }
  .out-img {
    margin-left: 60px;
    max-width: 100%;
    border-radius: 8px;
    box-shadow: var(--shadow-card);
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

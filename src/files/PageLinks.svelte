<script lang="ts">
  // A page's links both ways, under a Markdown page (knowledge-layer step
  // 10): the pages it reaches, and the pages that reach it, resolved by name
  // or alias and by path, from the links index.
  import { api } from "../lib/api";
  import { app } from "../lib/app.svelte";

  let { relPath, refresh = 0 }: { relPath: string; refresh?: number } = $props();

  let outgoing = $state<string[]>([]);
  let incoming = $state<string[]>([]);

  $effect(() => {
    void refresh;
    const path = relPath;
    api.pageLinks(path).then(
      (l) => {
        if (path !== relPath) return;
        outgoing = l.outgoing;
        incoming = l.incoming;
      },
      () => {
        outgoing = [];
        incoming = [];
      },
    );
  });

  const name = (p: string) => (p.split("/").pop() ?? p).replace(/\.md$/, "");
</script>

{#if outgoing.length + incoming.length > 0}
  <div class="links" aria-label="Page links">
    {#if incoming.length > 0}
      <div class="group">
        <span class="label">Linked from</span>
        {#each incoming as p}
          <button class="page" title={p} onclick={() => app.openInFiles(p)}>{name(p)}</button>
        {/each}
      </div>
    {/if}
    {#if outgoing.length > 0}
      <div class="group">
        <span class="label">Links to</span>
        {#each outgoing as p}
          <button class="page" title={p} onclick={() => app.openInFiles(p)}>{name(p)}</button>
        {/each}
      </div>
    {/if}
  </div>
{/if}

<style>
  .links {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 8px 16px;
    border-top: 1px solid var(--border);
    font-size: 12px;
    flex: none;
  }
  .group {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: baseline;
  }
  .label {
    color: var(--ink-tertiary);
    min-width: 76px;
  }
  .page {
    background: none;
    border: none;
    padding: 0;
    color: var(--accent);
    cursor: pointer;
    font-size: 12px;
  }
  .page:hover {
    text-decoration: underline;
  }
</style>

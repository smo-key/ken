<script lang="ts">
  // A page's links both ways, under a Markdown page (knowledge-layer step
  // 10): the pages it reaches, and the pages that reach it, resolved by name
  // or alias and by path, from the links index. Then what else the project
  // graph relates it to (knowledge-core decision 21): what it cites, the
  // note that changed it, the rulings and commits around it, pages on the
  // same topic. Plain links are not said twice.
  import { api, type RelatedNeighbour } from "../lib/api";
  import { app } from "../lib/app.svelte";

  let { relPath, refresh = 0 }: { relPath: string; refresh?: number } = $props();

  let outgoing = $state<string[]>([]);
  let incoming = $state<string[]>([]);
  let related = $state<RelatedNeighbour[]>([]);

  $effect(() => {
    void refresh;
    const path = relPath;
    api.graphRelated(path).then(
      (r) => {
        if (path !== relPath) return;
        related = r.filter((n) => n.edge !== "link");
      },
      () => {
        related = [];
      },
    );
  });

  /** How a neighbour joins the page, in a word or two. */
  function joins(n: RelatedNeighbour): string {
    if (n.hops > 1) return n.edge === "topic" ? "same topic" : "near";
    switch (n.edge) {
      case "cites":
        return n.outgoing ? "cites" : "cited by";
      case "changed":
        return n.outgoing ? "changed" : "changed by";
      case "supersedes":
        return n.outgoing ? "supersedes" : "superseded by";
      case "refines":
        return n.outgoing ? "refines" : "refined by";
      case "code-cites":
        return "cited in code";
      default:
        return n.edge;
    }
  }

  /** Open a neighbour that is a file, in the repo that holds it. */
  async function openNeighbour(n: RelatedNeighbour) {
    if (!n.path) return;
    if (n.projectId && n.projectId !== app.focused && app.workspace?.members.some((m) => m.projectId === n.projectId)) {
      await app.focusMember(n.projectId);
    }
    app.openInFiles(n.path);
  }

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

{#if outgoing.length + incoming.length + related.length > 0}
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
    {#if related.length > 0}
      <div class="group">
        <span class="label">Related</span>
        {#each related as n (n.key)}
          {#if n.path}
            <button class="page" title={`${joins(n)} · ${n.key}${n.detail ? ` · ${n.detail}` : ""}`} onclick={() => void openNeighbour(n)}>{n.label}</button>
          {:else}
            <span class="node" title={`${joins(n)} · ${n.key}${n.detail ? ` · ${n.detail}` : ""}`}>{n.label}</span>
          {/if}
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
  .node {
    color: var(--ink-secondary);
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

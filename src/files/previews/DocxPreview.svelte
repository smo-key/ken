<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "../../lib/api";

  let { relPath }: { relPath: string } = $props();

  let html = $state<string | null>(null);
  let error = $state<string | null>(null);

  onMount(async () => {
    try {
      const mammoth = await import("mammoth");
      const bytes = await api.readFileBytes(relPath);
      const result = await mammoth.convertToHtml({ arrayBuffer: bytes });
      html = result.value;
    } catch (e) {
      error = `Couldn't render this document — ${e}. Try “Open in default app”.`;
    }
  });
</script>

<div class="scroll">
  {#if error}
    <div class="note error">{error}</div>
  {:else if html === null}
    <div class="note">Rendering document…</div>
  {:else}
    <div class="doc">{@html html}</div>
  {/if}
</div>

<style>
  .scroll {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }
  .doc {
    max-width: 720px;
    margin: 0 auto;
    padding: 32px clamp(20px, 5%, 48px) 80px;
    font-size: 14.5px;
    line-height: 1.75;
  }
  .doc :global(h1),
  .doc :global(h2),
  .doc :global(h3) {
    font-family: var(--font-serif);
    font-weight: 500;
    letter-spacing: -0.01em;
  }
  .doc :global(table) {
    border-collapse: collapse;
    width: 100%;
  }
  .doc :global(td),
  .doc :global(th) {
    border: 1px solid var(--border);
    padding: 6px 10px;
    font-size: 13px;
  }
  .doc :global(img) {
    max-width: 100%;
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

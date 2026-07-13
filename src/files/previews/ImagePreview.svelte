<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { api } from "../../lib/api";

  let { relPath }: { relPath: string } = $props();

  let url = $state<string | null>(null);
  let error = $state<string | null>(null);

  onMount(async () => {
    try {
      const bytes = await api.readFileBytes(relPath);
      url = URL.createObjectURL(new Blob([bytes]));
    } catch (e) {
      error = `Couldn't load this image — ${e}.`;
    }
  });

  onDestroy(() => {
    if (url) URL.revokeObjectURL(url);
  });
</script>

<div class="scroll">
  {#if error}
    <div class="note error">{error}</div>
  {:else if url}
    <img src={url} alt={relPath} />
  {:else}
    <div class="note">Loading image…</div>
  {/if}
</div>

<style>
  .scroll {
    flex: 1;
    min-height: 0;
    overflow: auto;
    display: flex;
    align-items: center;
    justify-content: center;
    background: var(--sunken);
    padding: 24px;
  }
  img {
    max-width: 100%;
    max-height: 100%;
    border-radius: 6px;
    box-shadow: var(--shadow-card);
  }
  .note {
    color: var(--ink-tertiary);
    font-size: 13px;
  }
  .note.error {
    color: var(--danger);
  }
</style>

<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { api } from "../../lib/api";

  let { relPath }: { relPath: string } = $props();

  let host: HTMLDivElement;
  let error = $state<string | null>(null);
  let loading = $state(true);
  let cancelled = false;

  onMount(async () => {
    try {
      const pdfjs = await import("pdfjs-dist");
      pdfjs.GlobalWorkerOptions.workerSrc = new URL(
        "pdfjs-dist/build/pdf.worker.min.mjs",
        import.meta.url,
      ).toString();

      const bytes = await api.readFileBytes(relPath);
      const doc = await pdfjs.getDocument({ data: new Uint8Array(bytes) }).promise;
      loading = false;

      for (let i = 1; i <= doc.numPages && !cancelled; i++) {
        const page = await doc.getPage(i);
        const scale = 1.4;
        const viewport = page.getViewport({ scale });
        const canvas = document.createElement("canvas");
        const ratio = window.devicePixelRatio || 1;
        canvas.width = viewport.width * ratio;
        canvas.height = viewport.height * ratio;
        canvas.style.width = `${viewport.width}px`;
        const ctx = canvas.getContext("2d")!;
        ctx.scale(ratio, ratio);
        host.appendChild(canvas);
        await page.render({ canvasContext: ctx, viewport, canvas }).promise;
      }
    } catch (e) {
      loading = false;
      error = `Couldn't render this PDF — ${e}. Try “Open in default app”.`;
    }
  });

  onDestroy(() => {
    cancelled = true;
  });
</script>

<div class="scroll">
  {#if loading}
    <div class="note">Rendering PDF…</div>
  {/if}
  {#if error}
    <div class="note error">{error}</div>
  {/if}
  <div class="pages" bind:this={host}></div>
</div>

<style>
  .scroll {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    background: var(--sunken);
    padding: 24px;
  }
  .pages {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 16px;
  }
  .pages :global(canvas) {
    max-width: 100%;
    box-shadow: var(--shadow-card);
    border-radius: 4px;
    background: white;
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

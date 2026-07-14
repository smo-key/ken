<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "../../lib/api";
  import { registerDomFind } from "../../lib/find-dom.svelte";
  import { applyExtentsToHtml, parseExtents } from "./docx";
  import PreviewLoading from "./PreviewLoading.svelte";

  let { relPath }: { relPath: string } = $props();

  let html = $state<string | null>(null);
  let error = $state<string | null>(null);
  let doc = $state<HTMLDivElement | null>(null);

  registerDomFind(() => doc, { deps: () => html });

  onMount(async () => {
    try {
      const mammoth = await import("mammoth");
      const bytes = await api.readFileBytes(relPath);
      const result = await mammoth.convertToHtml({ arrayBuffer: bytes });
      // mammoth inlines images at intrinsic resolution and drops Word's layout
      // size; re-apply the display extents so inline avatars stay small. The zip
      // read is best-effort — any failure leaves mammoth's html (CSS-bounded).
      html = await sizeImages(bytes, result.value);
    } catch (e) {
      error = `Couldn't render this document — ${e}. Try “Open in default app”.`;
    }
  });

  async function sizeImages(bytes: ArrayBuffer, rendered: string): Promise<string> {
    try {
      const JSZip = (await import("jszip")).default;
      const zip = await JSZip.loadAsync(bytes);
      const documentXml = await zip.file("word/document.xml")?.async("string");
      if (!documentXml) return rendered;
      return applyExtentsToHtml(rendered, parseExtents(documentXml));
    } catch {
      return rendered;
    }
  }
</script>

<div class="scroll">
  {#if error}
    <div class="note error">{error}</div>
  {:else if html === null}
    <PreviewLoading label="Rendering document…" />
  {:else}
    <div class="doc" bind:this={doc}>{@html html}</div>
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
    /* Honour the width/height attributes we stamp from Word's layout extents,
       but never let an image overflow the pane; height:auto keeps aspect ratio
       when max-width clamps a genuinely large figure. */
    max-width: 100%;
    height: auto;
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

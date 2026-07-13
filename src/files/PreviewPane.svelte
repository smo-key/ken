<script lang="ts">
  import type { FileRow } from "../lib/api";
  import PdfPreview from "./previews/PdfPreview.svelte";
  import DocxPreview from "./previews/DocxPreview.svelte";
  import XlsxPreview from "./previews/XlsxPreview.svelte";
  import ImagePreview from "./previews/ImagePreview.svelte";
  import FallbackPreview from "./previews/FallbackPreview.svelte";

  let { relPath, kind, meta }: { relPath: string; kind: string; meta: FileRow } =
    $props();
</script>

{#if kind === "pdf"}
  <PdfPreview {relPath} />
{:else if kind === "docx"}
  <DocxPreview {relPath} />
{:else if kind === "xlsx"}
  <XlsxPreview {relPath} />
{:else if kind === "image"}
  <ImagePreview {relPath} />
{:else}
  <FallbackPreview {relPath} {meta} />
{/if}

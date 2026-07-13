<script lang="ts">
  import type { FileRow } from "../lib/api";
  import PdfPreview from "./previews/PdfPreview.svelte";
  import DocxPreview from "./previews/DocxPreview.svelte";
  import XlsxPreview from "./previews/XlsxPreview.svelte";
  import ImagePreview from "./previews/ImagePreview.svelte";
  import IpynbPreview from "./previews/IpynbPreview.svelte";
  import PptxPreview from "./previews/PptxPreview.svelte";
  import FallbackPreview from "./previews/FallbackPreview.svelte";

  let { relPath, kind, meta }: { relPath: string; kind: string; meta: FileRow } =
    $props();

  // Some formats are routed by extension because the backend kind is coarse
  // (e.g. .ipynb indexes as "binary").
  const ext = $derived(relPath.split(".").pop()?.toLowerCase() ?? "");
</script>

{#if ext === "ipynb"}
  <IpynbPreview {relPath} />
{:else if kind === "pdf"}
  <PdfPreview {relPath} />
{:else if kind === "docx"}
  <DocxPreview {relPath} />
{:else if kind === "xlsx"}
  <XlsxPreview {relPath} />
{:else if kind === "pptx"}
  <PptxPreview {relPath} />
{:else if kind === "image"}
  <ImagePreview {relPath} />
{:else}
  <FallbackPreview {relPath} {meta} />
{/if}

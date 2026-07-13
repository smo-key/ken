<script lang="ts">
  import type { Component } from "svelte";
  import File from "@lucide/svelte/icons/file";
  import FileText from "@lucide/svelte/icons/file-text";
  import FileCode from "@lucide/svelte/icons/file-code";
  import FileSpreadsheet from "@lucide/svelte/icons/file-spreadsheet";
  import FileType from "@lucide/svelte/icons/file-type";
  import Presentation from "@lucide/svelte/icons/presentation";
  import ImageIcon from "@lucide/svelte/icons/image";
  import NotebookText from "@lucide/svelte/icons/notebook-text";
  import Folder from "@lucide/svelte/icons/folder";
  import FolderOpen from "@lucide/svelte/icons/folder-open";
  import { glyphFor } from "../lib/format";

  let { kind, size = "md" }: { kind: string; size?: "sm" | "md" } = $props();

  type IconComponent = Component<{
    size?: number | string;
    strokeWidth?: number | string;
    color?: string;
  }>;

  const ICONS: Record<string, IconComponent> = {
    md: FileText,
    txt: FileText,
    code: FileCode,
    docx: FileText,
    xlsx: FileSpreadsheet,
    csv: FileSpreadsheet,
    pptx: Presentation,
    pdf: FileType,
    image: ImageIcon,
    ipynb: NotebookText,
    binary: File,
    folder: Folder,
    "folder-open": FolderOpen,
  };

  const isFolder = $derived(kind === "folder" || kind === "folder-open");
  const Icon = $derived(ICONS[kind] ?? File);
  const color = $derived(isFolder ? "#c2a878" : glyphFor(kind).color);
  const px = $derived(size === "sm" ? 16 : 20);
</script>

<span class="glyph" style:color>
  <Icon size={px} strokeWidth={1.75} />
</span>

<style>
  .glyph {
    flex: none;
    display: inline-flex;
    align-items: center;
    justify-content: center;
  }
</style>

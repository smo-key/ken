<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { Crepe } from "@milkdown/crepe";
  import "@milkdown/crepe/theme/common/style.css";
  import "@milkdown/crepe/theme/frame.css";

  let {
    initial,
    onchange,
  }: { initial: string; onchange: (markdown: string) => void } = $props();

  let host: HTMLDivElement;
  let crepe: Crepe | undefined;

  onMount(async () => {
    crepe = new Crepe({
      root: host,
      defaultValue: initial,
    });
    crepe.on((listener) => {
      listener.markdownUpdated((_ctx, markdown, prev) => {
        if (markdown !== prev) onchange(markdown);
      });
    });
    await crepe.create();
  });

  onDestroy(() => {
    void crepe?.destroy();
  });
</script>

<div class="editor-scroll">
  <div class="measure" bind:this={host}></div>
</div>

<style>
  .editor-scroll {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }
  .measure {
    max-width: 720px;
    margin: 0 auto;
    padding: 24px clamp(20px, 5%, 48px) 80px;
  }

  /* Paper & Ink over Crepe's frame theme */
  .measure :global(.milkdown) {
    --crepe-color-background: var(--surface);
    --crepe-color-on-background: var(--ink);
    --crepe-color-surface: var(--surface);
    --crepe-color-surface-low: var(--paper);
    --crepe-color-on-surface: var(--ink);
    --crepe-color-on-surface-variant: var(--ink-secondary);
    --crepe-color-outline: var(--border-strong);
    --crepe-color-primary: var(--accent);
    --crepe-color-secondary: var(--sunken);
    --crepe-color-on-secondary: var(--ink);
    --crepe-color-inverse: var(--ink);
    --crepe-color-on-inverse: var(--paper);
    --crepe-color-inline-code: var(--accent-deep);
    --crepe-color-error: var(--danger);
    --crepe-color-hover: var(--sunken);
    --crepe-color-selected: #e8dccb;
    --crepe-color-inline-area: var(--sunken);
    --crepe-font-title: var(--font-serif);
    --crepe-font-default: var(--font-sans);
    --crepe-font-code: var(--font-mono);
    --crepe-shadow-1: var(--shadow-card);
    --crepe-shadow-2: var(--shadow-overlay);
    background: transparent;
  }
  .measure :global(.milkdown .ProseMirror) {
    padding: 0;
    font-size: 14.5px;
    line-height: 1.75;
  }
  .measure :global(.milkdown h1),
  .measure :global(.milkdown h2),
  .measure :global(.milkdown h3) {
    font-family: var(--font-serif);
    font-weight: 500;
    letter-spacing: -0.01em;
  }
</style>

<script lang="ts">
  import { onDestroy, onMount, tick } from "svelte";
  import { Crepe } from "@milkdown/crepe";
  import { editorViewCtx } from "@milkdown/kit/core";
  // Namespace import: Svelte reserves the `$` prefix, so `$prose` can only be
  // reached as a property.
  import * as milkdown from "@milkdown/kit/utils";
  import { Plugin, PluginKey } from "@milkdown/kit/prose/state";
  import type { Node as ProseNode } from "@milkdown/kit/prose/model";
  import { Decoration, DecorationSet, type EditorView } from "@milkdown/kit/prose/view";
  import "@milkdown/crepe/theme/common/style.css";
  import "@milkdown/crepe/theme/frame.css";
  import { CURRENT_CLASS, MARK_CLASS } from "../lib/find-dom";
  import { MATCH_CAP, findTextMatches } from "../lib/find";
  import { find, type FindAdapter } from "../lib/find.svelte";

  let {
    initial,
    onchange,
  }: { initial: string; onchange: (markdown: string) => void } = $props();

  let host: HTMLDivElement;
  let crepe: Crepe | undefined;
  let view = $state<EditorView | null>(null);

  // Milkdown owns this DOM: wrapping hits in <mark> would make ProseMirror
  // reconcile markup it never produced, and could corrupt the document. Find is
  // therefore a ProseMirror decoration plugin — highlights live in the view
  // layer, the document and its selection are never touched, and the
  // decoration-only transactions stay out of history and out of autosave.
  interface Hit {
    from: number;
    to: number;
  }
  interface FindState {
    query: string;
    current: number;
    hits: Hit[];
    decorations: DecorationSet;
  }

  const findKey = new PluginKey<FindState>("ken-find");
  const EMPTY: FindState = {
    query: "",
    current: 0,
    hits: [],
    decorations: DecorationSet.empty,
  };

  function collect(doc: ProseNode, query: string): Hit[] {
    const hits: Hit[] = [];
    if (!query.trim()) return hits;
    doc.descendants((node, pos) => {
      if (hits.length >= MATCH_CAP) return false;
      if (!node.isText || !node.text) return true;
      const { starts } = findTextMatches(
        node.text,
        query,
        MATCH_CAP - hits.length,
      );
      for (const start of starts) {
        hits.push({ from: pos + start, to: pos + start + query.length });
      }
      return true;
    });
    return hits;
  }

  function build(doc: ProseNode, query: string, current: number): FindState {
    const hits = collect(doc, query);
    const decorations = DecorationSet.create(
      doc,
      hits.map((hit, i) =>
        Decoration.inline(hit.from, hit.to, {
          class: i === current ? `${MARK_CLASS} ${CURRENT_CLASS}` : MARK_CLASS,
        }),
      ),
    );
    return { query, current, hits, decorations };
  }

  const findPlugin = milkdown.$prose(
    () =>
      new Plugin<FindState>({
        key: findKey,
        state: {
          init: () => EMPTY,
          apply(tr, previous) {
            const meta = tr.getMeta(findKey) as
              | { query: string; current: number }
              | undefined;
            if (meta) return build(tr.doc, meta.query, meta.current);
            if (tr.docChanged && previous.query) {
              return build(tr.doc, previous.query, previous.current);
            }
            return previous;
          },
        },
        props: {
          decorations: (state) => findKey.getState(state)?.decorations,
        },
      }),
  );

  /** Decoration-only transaction: no doc change, no history entry, no save. */
  function paint(query: string, current: number): number {
    if (!view) return 0;
    const tr = view.state.tr.setMeta(findKey, { query, current });
    tr.setMeta("addToHistory", false);
    view.dispatch(tr);
    return findKey.getState(view.state)?.hits.length ?? 0;
  }

  $effect(() => {
    if (!view) return;
    const adapter: FindAdapter = {
      search(query) {
        const total = paint(query, 0);
        return { total, capped: total >= MATCH_CAP };
      },
      async reveal(index) {
        paint(find.query, index);
        await tick();
        host
          ?.querySelector(`.${CURRENT_CLASS}`)
          ?.scrollIntoView({ block: "center", inline: "nearest" });
      },
      clear() {
        paint("", 0);
      },
    };
    find.register(adapter);
    return () => find.unregister(adapter);
  });

  onMount(async () => {
    crepe = new Crepe({
      root: host,
      defaultValue: initial,
      // Disable LaTeX/math so `$…$` (e.g. two dollar amounts in one sentence)
      // stays literal text instead of being parsed as an inline math node.
      features: {
        [Crepe.Feature.Latex]: false,
      },
    });
    crepe.on((listener) => {
      listener.markdownUpdated((_ctx, markdown, prev) => {
        if (markdown !== prev) onchange(markdown);
      });
    });
    crepe.editor.use(findPlugin);
    await crepe.create();
    crepe.editor.action((ctx) => {
      view = ctx.get(editorViewCtx);
    });
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
    --crepe-color-selected: var(--selection-bg);
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
    line-height: 1.85;
  }
  .measure :global(.milkdown .ProseMirror p) {
    line-height: 1.85;
  }
  .measure :global(.milkdown h1),
  .measure :global(.milkdown h2),
  .measure :global(.milkdown h3),
  .measure :global(.milkdown h4) {
    font-family: var(--font-serif);
    font-weight: 500;
    letter-spacing: -0.01em;
    /* A touch more breathing room after headings. */
    margin-bottom: 0.55em;
  }
  /* Slightly smaller than Crepe's defaults, keeping the scale gentle. */
  .measure :global(.milkdown h1) {
    font-size: 1.7em;
  }
  .measure :global(.milkdown h2) {
    font-size: 1.35em;
  }
  .measure :global(.milkdown h3) {
    font-size: 1.15em;
  }
  .measure :global(.milkdown h4) {
    font-size: 1.02em;
  }
</style>

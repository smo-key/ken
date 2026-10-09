<script lang="ts">
  import { onDestroy, onMount, tick, untrack } from "svelte";
  import { Crepe } from "@milkdown/crepe";
  import { editorViewCtx, remarkStringifyOptionsCtx } from "@milkdown/kit/core";
  import type { Ctx } from "@milkdown/kit/ctx";
  // Namespace import: Svelte reserves the `$` prefix, so `$prose` can only be
  // reached as a property.
  import * as milkdown from "@milkdown/kit/utils";
  import {
    Plugin,
    PluginKey,
    TextSelection,
    type Transaction,
  } from "@milkdown/kit/prose/state";
  import { Fragment, type Node as ProseNode, type Schema } from "@milkdown/kit/prose/model";
  import { uploadConfig, type Uploader } from "@milkdown/kit/plugin/upload";
  import { Decoration, DecorationSet, type EditorView } from "@milkdown/kit/prose/view";
  import "@milkdown/crepe/theme/common/style.css";
  import "@milkdown/crepe/theme/frame.css";
  import {
    blockquoteSchema,
    paragraphSchema,
  } from "@milkdown/kit/preset/commonmark";
  import {
    addGithubAlertMenuGroup,
    githubAlertPlugins,
  } from "./markdown/githubAlert";
  import "./markdown/githubAlert.css";
  import { disableHeadingDowngrade, headingBackspace } from "./markdown/headingBackspace";
  import {
    mermaidPreviewToggleText,
    renderMermaidPreview,
  } from "./markdown/mermaid";
  import {
    slashShortcutInputRule,
    slashShortcutKeys,
  } from "./markdown/slashShortcuts";
  import { anchorLinkPlugin, findHeadingForTarget, headingSlug } from "./markdown/anchors";
  import { lineTarget } from "../lib/citation";
  import type { Reveal } from "../lib/app.svelte";
  import { headingLinkPlugin } from "./markdown/headingLink";
  import { wikiLinkPlugin, type LinkClick } from "./markdown/wikiLinks";
  import { tableContextMenu } from "./markdown/tableMenu";
  import { tableFullWidthPlugins } from "./markdown/tableFullWidth";
  import { addTocMenuItem, tocPlugins } from "./markdown/toc";
  import { tableLayoutPlugin } from "./markdown/tableLayout";
  import {
    configureListDelimiter,
    listStylePlugins,
  } from "./markdown/listStyles";
  import ImageLightbox from "./markdown/ImageLightbox.svelte";
  import { lightbox } from "./markdown/lightbox.svelte";
  import { CURRENT_CLASS, MARK_CLASS } from "../lib/find-dom";
  import { MATCH_CAP, findTextMatches } from "../lib/find";
  import { find, type FindAdapter } from "../lib/find.svelte";
  import { drag } from "./dnd.svelte";
  import { hasFiles } from "./externalDrop";
  import {
    attachmentName,
    docDir,
    encodeHref,
    isImageFile,
    isImagePath,
    relativeTo,
    resolveSrc,
    type AttachmentAdapter,
  } from "./markdown/attachments";

  let {
    initial,
    onchange,
    relPath,
    attachments,
    reveal = null,
    onlink,
    isRepo = () => false,
  }: {
    initial: string;
    onchange: (markdown: string) => void;
    /** The document's project-relative path; links are written relative to it. */
    relPath?: string;
    /** Storage for pasted/dropped files. Without it (and `relPath`), Crepe's
     *  stock behaviour applies. */
    attachments?: AttachmentAdapter;
    reveal?: Reveal | null;
    /** A link was clicked: follow it and say whether it was followed. */
    onlink?: (link: LinkClick) => boolean;
    /** Whether a name in code is one of the workspace's repos. */
    isRepo?: (name: string) => boolean;
  } = $props();

  // A clicked citation: scroll to its heading, or to the block its source
  // line became, and flash it so the eye lands there.
  $effect(() => {
    const r = reveal;
    const v = view;
    if (!r || !v) return;
    void r.nonce;
    untrack(() => revealIn(v, r));
  });

  function revealIn(v: EditorView, r: Reveal) {
    const doc = v.state.doc;
    let pos: number | undefined = r.anchor ? findHeadingForTarget(doc, r.anchor)?.pos : undefined;
    if (pos === undefined && r.line) {
      const t = lineTarget(initial, r.line);
      if (t.phrase) {
        const phrase = t.phrase;
        doc.descendants((node, p) => {
          if (pos !== undefined) return false;
          if (node.isTextblock && node.textContent.includes(phrase)) {
            pos = p;
            return false;
          }
          return true;
        });
      }
      if (pos === undefined && t.heading) pos = findHeadingForTarget(doc, headingSlug(t.heading))?.pos;
    }
    if (pos === undefined) return;
    const dom = v.nodeDOM(pos);
    if (!(dom instanceof HTMLElement)) return;
    dom.scrollIntoView({ block: "center", behavior: "smooth" });
    dom.classList.add("ken-reveal");
    setTimeout(() => dom.classList.remove("ken-reveal"), 1600);
  }

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

  /**
   * Replace the current block with `blockquote(paragraph)` and put the cursor
   * inside it. Crepe's stock Quote item clears the block and then *wraps* it,
   * which silently does nothing whenever `findWrapping` cannot fit a blockquote
   * around the current block range; replacing works from any textblock.
   */
  function insertQuote(ctx: Ctx) {
    const editorView = ctx.get(editorViewCtx);
    const { state } = editorView;
    // Svelte reserves the `$` prefix, so the resolved position cannot be
    // destructured as `$from` here.
    const pos = state.selection.$from;
    const depth = pos.depth;
    const blockquote = blockquoteSchema.type(ctx);
    const paragraph = paragraphSchema.type(ctx);
    const node = blockquote.createAndFill(null, paragraph.create());
    if (!node) return;
    const start = pos.before(depth);
    const tr = state.tr.replaceWith(start, pos.after(depth), node);
    tr.setSelection(TextSelection.near(tr.doc.resolve(start + 2)));
    editorView.dispatch(tr.scrollIntoView());
    editorView.focus();
  }

  // --- Lightbox ------------------------------------------------------------
  // Clicks are delegated from the scroll container rather than wired into the
  // node views: Crepe owns those, and a click handler there would fight its
  // image-block selection and resize handling.
  let pointerStart: { x: number; y: number } | null = null;

  function notePointer(event: PointerEvent) {
    pointerStart = { x: event.clientX, y: event.clientY };
  }

  /** A click that dragged was a selection or a resize, not a request to zoom. */
  function wasDrag(event: MouseEvent): boolean {
    if (!pointerStart) return false;
    return (
      Math.abs(event.clientX - pointerStart.x) > 4 ||
      Math.abs(event.clientY - pointerStart.y) > 4
    );
  }

  function maybeOpenLightbox(event: MouseEvent) {
    if (event.defaultPrevented || event.button !== 0 || wasDrag(event)) return;
    const target = event.target;
    if (!(target instanceof Element)) return;
    // Never steal a click from a control: the caption and resize affordances
    // sit right on top of the image.
    if (target.closest("button, input, .image-resize-handle, .operation")) return;

    const diagram = target.closest(".mermaid-preview");
    const svg = diagram?.querySelector("svg");
    if (svg) {
      event.preventDefault();
      // A clone: the live SVG belongs to the preview and is re-rendered.
      lightbox.show({ kind: "svg", svg: svg.outerHTML });
      return;
    }

    // Crepe's image block layers a wrapper over the picture, so a plain click
    // often lands on the wrapper rather than the `<img>`; resolve either to
    // the block's image.
    const block = target.closest(".milkdown-image-block, .image-wrapper");
    const image =
      target instanceof HTMLImageElement ? target : block?.querySelector("img");
    if (image instanceof HTMLImageElement && (image.currentSrc || image.src)) {
      event.preventDefault();
      lightbox.show({
        kind: "img",
        src: image.currentSrc || image.src,
        alt: image.alt,
      });
    }
  }

  // --- Attachments ---------------------------------------------------------
  // A pasted or dropped file is saved beside the document and linked with a
  // relative, percent-encoded path, so the .md stays portable and renders on
  // reload. Crepe's stock uploader writes `blob:` URLs that die with the page
  // and silently drops anything that isn't an image.

  /** Save `file` beside the document; the href to it, relative to the doc. */
  async function saveBeside(adapter: AttachmentAdapter, docPath: string, file: File) {
    const name = attachmentName(file, docPath, new Date());
    const named = name === file.name ? file : new File([file], name, { type: file.type });
    const saved = await adapter.save(named);
    return { saved, href: encodeHref(relativeTo(docDir(docPath), saved)) };
  }

  /** An image node (Crepe's block image when present), else a paragraph with
   *  the file name linked. Blocks, not bare text: plugin-upload splices the
   *  result in with `replaceWith`, which needs whole nodes. */
  function attachmentNode(schema: Schema, href: string, target: string, image: boolean) {
    if (image) {
      const imageType = schema.nodes["image-block"] ?? schema.nodes.image;
      return imageType?.createAndFill({ src: href }) ?? null;
    }
    const label = target.slice(target.lastIndexOf("/") + 1);
    const link = schema.marks.link.create({ href });
    return schema.nodes.paragraph.create(null, schema.text(label, [link]));
  }

  /** Save `files` beside the document; the nodes to insert for those saved. */
  async function importFiles(files: File[], schema: Schema): Promise<ProseNode[]> {
    const adapter = attachments;
    const docPath = relPath;
    if (!adapter || docPath === undefined) return [];
    // One failed save must not sink the rest, and a rejection would leave
    // plugin-upload's "Upload in progress" placeholder in the doc for good.
    const settled = await Promise.allSettled(
      files.map(async (file) => {
        const { saved, href } = await saveBeside(adapter, docPath, file);
        return attachmentNode(schema, href, saved, isImageFile(file));
      }),
    );
    const nodes: ProseNode[] = [];
    for (const result of settled) {
      if (result.status === "fulfilled" && result.value) nodes.push(result.value);
      else if (result.status === "rejected") console.error("attachment save failed", result.reason);
    }
    return nodes;
  }

  const uploader: Uploader = (files, schema) => importFiles(Array.from(files), schema);

  // --- Drops around the text -------------------------------------------------
  // ProseMirror only listens on its own element, so a file dropped in the
  // margins or below the last line would otherwise reach nobody (and the
  // window guard would refuse it). The whole pane accepts OS files: inside the
  // text the uploader handles them (and cancels the event); anywhere else they
  // land at the nearest document position.
  let fileDragOver = $state(false);

  function canImport(e: DragEvent): boolean {
    return hasFiles(e.dataTransfer) && !!attachments && relPath !== undefined && !!view;
  }

  function onPaneDragOver(e: DragEvent) {
    if (!canImport(e)) return;
    e.preventDefault();
    e.dataTransfer!.dropEffect = "copy";
    fileDragOver = true;
  }

  function onPaneDragLeave(e: DragEvent) {
    const pane = e.currentTarget as HTMLElement;
    if (!pane.contains(e.relatedTarget as Node | null)) fileDragOver = false;
  }

  /** Nearest document position to a point, clamped into the text column;
   *  the end of the document when even that finds nothing. */
  function nearestPos(editorView: EditorView, x: number, y: number): number {
    const rect = editorView.dom.getBoundingClientRect();
    const left = Math.min(Math.max(x, rect.left + 1), rect.right - 1);
    const top = Math.min(Math.max(y, rect.top + 1), rect.bottom - 1);
    return editorView.posAtCoords({ left, top })?.pos ?? editorView.state.doc.content.size;
  }

  async function onPaneDrop(e: DragEvent) {
    fileDragOver = false;
    if (e.defaultPrevented || !canImport(e) || !view) return;
    e.preventDefault();
    const files = Array.from(e.dataTransfer!.files);
    const at = nearestPos(view, e.clientX, e.clientY);
    const nodes = await importFiles(files, view.state.schema);
    if (!view || nodes.length === 0) return;
    // The document may have changed during the save; keep the spot in range.
    const pos = Math.min(at, view.state.doc.content.size);
    view.dispatch(insertDropped(view.state.tr, pos, nodes, false).scrollIntoView());
    view.focus();
  }

  /** The image block's own Upload button: same save, relative src back. */
  async function uploadFromImageBlock(file: File): Promise<string> {
    if (!attachments || relPath === undefined) return URL.createObjectURL(file);
    return (await saveBeside(attachments, relPath, file)).href;
  }

  // Asset URLs for relative image srcs, per project path. Only successes are
  // kept, so an image that appears on disk later still gets another try.
  const resolved = new Map<string, string>();

  /** Display URL for an image src: a document-relative path becomes a URL the
   *  webview can load; anything else (http, data:, …) passes through. */
  function proxyImageSrc(src: string): string | Promise<string> {
    const adapter = attachments;
    if (!adapter || relPath === undefined) return src;
    const target = resolveSrc(relPath, src);
    if (!target) return src;
    const hit = resolved.get(target);
    if (hit) return hit;
    // The image component leaves the src unset if this rejects, so a failure
    // degrades to the original src instead.
    return Promise.resolve()
      .then(() => adapter.resolve(target))
      .then((url) => {
        resolved.set(target, url);
        return url;
      })
      .catch(() => src);
  }

  /**
   * Insert dropped nodes without leaving debris: a single link goes inline at
   * the drop point (unless `inline` is off, for drops in the margins); blocks
   * replace an empty paragraph, or land before or after the paragraph they
   * were dropped in, rather than splitting it.
   */
  function insertDropped(
    tr: Transaction,
    pos: number,
    nodes: ProseNode[],
    inline = true,
  ): Transaction {
    const target = tr.doc.resolve(pos);
    const parent = target.parent;
    const link = tr.doc.type.schema.marks.link;
    const only = nodes.length === 1 ? nodes[0] : null;
    // Not inside a code block: it takes no marks, so the link stays a block.
    if (inline && only?.type.name === "paragraph" && parent.inlineContent && parent.type.allowsMarkType(link)) {
      return tr.insert(pos, only.content);
    }
    const content = Fragment.from(nodes);
    if (nodes.every((n) => n.isBlock) && parent.isTextblock && target.depth > 0) {
      if (parent.content.size === 0) {
        return tr.replaceWith(target.before(), target.after(), content);
      }
      return tr.insert(target.parentOffset === 0 ? target.before() : target.after(), content);
    }
    return tr.replaceWith(pos, pos, content);
  }

  /**
   * A file dragged in from Ken's own tree arrives as plain text (its path).
   * Insert an image or a link to the existing file instead, relative to the
   * document; nothing is copied. Drag state is left alone: the tree row's
   * `dragend` resets it.
   */
  const treeDropPlugin = milkdown.$prose(
    () =>
      new Plugin({
        key: new PluginKey("ken-tree-drop"),
        props: {
          // A DOM handler rather than `handleDrop`: Crepe's drop indicator
          // claims every drop in `handleDrop` and would paste the path text.
          handleDOMEvents: {
            drop(view, event) {
              const from = drag.from;
              if (!from || drag.fromKind !== "file" || relPath === undefined) return false;
              if (event.dataTransfer?.files.length) return false;
              const at = view.posAtCoords({ left: event.clientX, top: event.clientY });
              if (!at) return false;
              const href = encodeHref(relativeTo(docDir(relPath), from));
              const node = attachmentNode(view.state.schema, href, from, isImagePath(from));
              if (!node) return false;
              event.preventDefault();
              view.dispatch(insertDropped(view.state.tr, at.pos, [node]).scrollIntoView());
              view.focus();
              return true;
            },
          },
        },
      }),
  );

  // Frontmatter is not Markdown the editor knows: it showed as a rule and a
  // heading and saved back mangled, losing a page's aliases. It is set aside
  // here and put back on every save.
  const front = initial.match(/^---\r?\n[\s\S]*?\r?\n---[ \t]*(\r?\n|$)/)?.[0] ?? "";
  // The serializer escapes `[` in text, which turns every [[wiki link]] into
  // \[\[text]]: put them back.
  const unescapeWikiLinks = (md: string) => md.replace(/\\\[\\\[([^\n]+?)\\?\]\\?\]/g, "[[$1]]");

  onMount(async () => {
    crepe = new Crepe({
      root: host,
      defaultValue: initial.slice(front.length),
      // Disable LaTeX/math so `$…$` (e.g. two dollar amounts in one sentence)
      // stays literal text instead of being parsed as an inline math node.
      features: {
        [Crepe.Feature.Latex]: false,
      },
      featureConfigs: {
        [Crepe.Feature.CodeMirror]: {
          renderPreview: renderMermaidPreview,
          // Only has an effect where a preview exists, i.e. mermaid fences.
          previewOnlyByDefault: true,
          previewToggleText: mermaidPreviewToggleText,
        },
        [Crepe.Feature.BlockEdit]: {
          listGroup: { taskList: { label: "Checklist" } },
          buildMenu: (builder) => {
            const quote = builder
              .build()
              .find((group) => group.key === "text")
              ?.items.find((item) => item.key === "quote");
            if (quote) quote.onRun = insertQuote;
            addTocMenuItem(builder);
            addGithubAlertMenuGroup(builder);
          },
        },
        // Covers inline images too: Crepe hands the same config to both.
        [Crepe.Feature.ImageBlock]: {
          onUpload: uploadFromImageBlock,
          proxyDomURL: proxyImageSrc,
        },
      },
    });
    // After construction: Crepe installs its own (blob: URL) uploader in its
    // constructor, and the last update wins. `enableHtmlFileUploader` stays
    // off, so an image copied from a web page still pastes as HTML.
    if (attachments && relPath !== undefined) {
      crepe.editor.config((ctx) => {
        ctx.update(uploadConfig.key, (prev) => ({ ...prev, uploader }));
      });
    }
    crepe.on((listener) => {
      listener.markdownUpdated((_ctx, markdown, prev) => {
        if (markdown !== prev) onchange(front + unescapeWikiLinks(markdown));
      });
    });
    crepe.editor.use(findPlugin);
    crepe.editor.use(treeDropPlugin);
    crepe.editor.use(githubAlertPlugins);
    crepe.editor.use(headingBackspace);
    crepe.editor.use(slashShortcutInputRule);
    crepe.editor.use(slashShortcutKeys);
    crepe.editor.use(anchorLinkPlugin);
    crepe.editor.use(headingLinkPlugin);
    crepe.editor.use(wikiLinkPlugin((link) => onlink?.(link) ?? false, (name) => isRepo(name)));
    // After the GFM preset: the table schema extension replaces the preset's
    // own `table` node, so it has to be registered last.
    crepe.editor.use(tableFullWidthPlugins);
    // Same reason: the bullet-list schema extension replaces the commonmark
    // preset's own `bullet_list` node.
    crepe.editor.use(tocPlugins);
    // After the commonmark preset: it extends the preset's `ordered_list`.
    crepe.editor.use(listStylePlugins);
    crepe.editor.config(configureListDelimiter);
    crepe.editor.use(tableLayoutPlugin);
    crepe.editor.use(tableContextMenu);
    crepe.editor.config(disableHeadingDowngrade);
    // House style is `- ` bullets; remark-stringify would otherwise write `*`.
    crepe.editor.config((ctx) => {
      ctx.update(remarkStringifyOptionsCtx, (opts) => ({
        ...opts,
        bullet: "-" as const,
      }));
    });
    await crepe.create();
    crepe.editor.action((ctx) => {
      view = ctx.get(editorViewCtx);
    });
  });

  onDestroy(() => {
    void crepe?.destroy();
  });
</script>

<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="editor-scroll"
  class:file-drop={fileDragOver}
  onpointerdown={notePointer}
  onclick={maybeOpenLightbox}
  ondragover={onPaneDragOver}
  ondragleave={onPaneDragLeave}
  ondrop={onPaneDrop}
>
  <div class="measure" bind:this={host}></div>
</div>
<ImageLightbox />

<style>
  /* A clicked citation's block, flashed so the eye lands on it. */
  :global(.milkdown .ken-reveal) {
    background: color-mix(in srgb, var(--accent) 16%, transparent);
    border-radius: 4px;
    transition: background 1.2s ease-out;
  }
  .editor-scroll {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    /* Makes the pane a query container so a table can size itself against the
       pane rather than the 720px text column (`100cqw` below). The pane's own
       width comes from the flex parent, so inline-size containment is safe. */
    container-type: inline-size;
  }
  /* Files dragged in from the OS: the whole pane takes them. Same accent
     treatment as a file-tree drop target. */
  .editor-scroll.file-drop {
    background: color-mix(in srgb, var(--accent) 5%, transparent);
    outline: 1px solid var(--accent);
    outline-offset: -1px;
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
    /* Code in ink, links in the accent: the two never look alike. */
    --crepe-color-inline-code: var(--ink-2);
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

  /* --- Links -------------------------------------------------------------- */
  /* Every link in the document reads as one, the table of contents included:
     accent ink and an underline. Crepe's reset says as much already, but the
     app-wide `a` rule (no underline) and the TOC's own styling both pull
     against it, so Ken states it once here, with the app's deeper hover. */
  .measure :global(.milkdown .ProseMirror a) {
    color: var(--accent);
    text-decoration: underline;
  }
  .measure :global(.milkdown .ProseMirror a:hover) {
    color: var(--accent-deep);
  }

  /* --- Crepe controls ----------------------------------------------------- */
  /* Crepe draws every button glyph in `--crepe-color-outline`, the same token
     it uses for hairlines. Mapped to Ken's `--border-strong` that is right for
     the borders but leaves the icons all but invisible, worst of all in dark
     mode. The token stays a border colour; the glyphs are recoloured one rule
     at a time to the secondary ink, rising to full ink on hover. Active and
     primary states keep Crepe's accent. */
  .measure :global(.milkdown .milkdown-block-handle .operation-item svg),
  .measure :global(.milkdown .milkdown-toolbar .toolbar-item svg),
  .measure :global(.milkdown .milkdown-slash-menu .menu-groups .menu-group li svg),
  .measure
    :global(.milkdown .milkdown-code-block .tools .language-button .expand-icon svg),
  .measure :global(.milkdown .milkdown-link-preview > .link-preview > .link-icon > svg),
  .measure :global(.milkdown .milkdown-link-preview > .link-preview > .button > svg),
  .measure :global(.milkdown .milkdown-link-edit > .link-edit > .button > svg),
  .measure :global(.milkdown .milkdown-image-block .image-edit .image-icon svg),
  .measure
    :global(.milkdown .milkdown-image-inline .empty-image-inline .image-icon svg) {
    color: var(--ink-secondary);
    fill: var(--ink-secondary);
  }
  .measure :global(.milkdown .milkdown-image-block .image-edit .image-icon) {
    color: var(--ink-secondary);
  }
  .measure :global(.milkdown .milkdown-block-handle .operation-item:hover svg),
  .measure
    :global(.milkdown .milkdown-toolbar .toolbar-item:not(.active):hover svg),
  .measure
    :global(.milkdown .milkdown-slash-menu .menu-groups .menu-group li:hover svg),
  .measure
    :global(.milkdown .milkdown-slash-menu .menu-groups .menu-group li.active svg),
  .measure
    :global(
      .milkdown .milkdown-code-block .tools .language-button:hover .expand-icon svg
    ),
  .measure
    :global(.milkdown .milkdown-link-preview > .link-preview > .link-icon:hover > svg),
  .measure
    :global(.milkdown .milkdown-link-preview > .link-preview > .button:hover > svg),
  .measure :global(.milkdown .milkdown-link-edit > .link-edit > .button:hover > svg) {
    color: var(--ink);
    fill: var(--ink);
  }
  /* Crepe paints its floating popups in `--crepe-color-surface`, which is
     Ken's `--surface` — the pane's own colour — so a menu read as a hole in
     the page rather than something above it. Each overlay gets its own
     surface, a step of ink mixed into the pane (darker on paper, lighter in
     dark mode), a real edge, and a hover a further step on so rows stay
     distinguishable. Scoped to the overlays: in-document blocks (code, table
     cells) keep the page surface. */
  .measure :global(.milkdown .milkdown-slash-menu),
  .measure :global(.milkdown .milkdown-toolbar),
  .measure :global(.milkdown .milkdown-link-preview),
  .measure :global(.milkdown .milkdown-link-edit),
  .measure :global(.milkdown .milkdown-code-block .list-wrapper),
  .measure :global(.milkdown .milkdown-table-block .cell-handle .button-group) {
    --crepe-color-surface: color-mix(in srgb, var(--ink) 7%, var(--surface));
    --crepe-color-surface-low: var(--crepe-color-surface);
    --crepe-color-hover: color-mix(in srgb, var(--ink) 14%, var(--surface));
  }
  .measure :global(.milkdown .milkdown-slash-menu),
  .measure :global(.milkdown .milkdown-toolbar),
  .measure :global(.milkdown .milkdown-link-preview > .link-preview),
  .measure :global(.milkdown .milkdown-link-edit > .link-edit),
  .measure :global(.milkdown .milkdown-code-block .list-wrapper),
  .measure :global(.milkdown .milkdown-table-block .cell-handle .button-group) {
    border: 1px solid var(--border-strong);
  }

  /* The rules above outrank Crepe's own, so its accent for an active format
     (bold, italic…) is restated. */
  .measure :global(.milkdown .milkdown-toolbar .toolbar-item.active svg) {
    color: var(--accent);
    fill: var(--accent);
  }
  /* The same token colours the text caret in Crepe's small inputs (code
     language search, image URL) and the virtual cursor beside marks. */
  .measure :global(.milkdown .milkdown-code-block .tools input),
  .measure :global(.milkdown .milkdown-image-block input),
  .measure :global(.milkdown .milkdown-image-inline input) {
    caret-color: var(--ink);
  }
  .measure :global(.milkdown .ProseMirror-focused) {
    --prosemirror-virtual-cursor-color: var(--ink);
  }

  /* --- Mermaid ------------------------------------------------------------ */
  /* In diagram mode (preview only: the CodeMirror host is hidden) the toolbar
     is a distraction, so it only fades in while the block is hovered or has
     focus. In code mode it stays put, as it always has. */
  .measure
    :global(.milkdown .milkdown-code-block:has(.codemirror-host.hidden) .tools) {
    opacity: 0;
    transition: opacity 120ms ease;
  }
  .measure
    :global(
      .milkdown .milkdown-code-block:has(.codemirror-host.hidden):hover .tools
    ),
  .measure
    :global(
      .milkdown
        .milkdown-code-block:has(.codemirror-host.hidden):focus-within
        .tools
    ) {
    opacity: 1;
  }
  .measure :global(.milkdown .mermaid-preview) {
    display: flex;
    justify-content: center;
  }
  .measure :global(.milkdown .mermaid-preview svg) {
    max-width: 100%;
    height: auto;
    background: transparent;
  }
  /* Mermaid sizes its label boxes itself and renders the text in a
     foreignObject; the editor's generous paragraph line-height would overflow
     those boxes and clip descenders. */
  .measure :global(.milkdown .mermaid-preview foreignObject p),
  .measure :global(.milkdown .mermaid-preview foreignObject div),
  .measure :global(.milkdown .mermaid-preview foreignObject span) {
    margin: 0;
    line-height: inherit;
  }
  .measure :global(.milkdown .mermaid-error) {
    padding: 8px 12px;
    border-left: 3px solid var(--danger);
    color: var(--danger);
    font-family: var(--font-mono);
    font-size: 12.5px;
  }

  /* --- Tables ------------------------------------------------------------- */
  /* A table never scrolls sideways and never pushes the pane wider: it shrinks
     to the space it is given and its cells wrap. By default that space is the
     text column. The hover toggle (and the right-click menu) switch a table
     into full-width mode, where it fills the pane's gutters up to 1200px and
     centres on the text column — `left`/`translateX` rather than auto margins,
     so it stays centred even when it is wider than its containing block. The
     choice is stored in the markdown as an HTML comment (see
     markdown/tableFullWidth.ts) and arrives here as `data-full-width`. */
  .measure :global(.milkdown .milkdown-table-block) {
    position: relative;
    /* Shrink-to-fit, capped at the text column. */
    width: fit-content;
    max-width: 100%;
    /* Tables need more air than a paragraph to read as their own block. */
    margin: 1.5em 0;
  }
  .measure :global(.milkdown .milkdown-table-block[data-full-width="true"]) {
    width: min(calc(100cqw - 2 * clamp(20px, 5cqw, 48px)), 1200px);
    max-width: min(calc(100cqw - 2 * clamp(20px, 5cqw, 48px)), 1200px);
    left: 50%;
    transform: translateX(-50%);
  }
  .measure :global(.milkdown .milkdown-table-block .table-wrapper) {
    /* Deliberately not `auto`: a table that outgrows its cap wraps its text
       rather than hiding columns behind a scrollbar. */
    overflow: visible;
  }
  /* ProseMirror's stylesheet pins tables to `width: 100%; table-layout: fixed`,
     which divides the column evenly however long the headings are. `auto` plus
     a `max-width` lets the browser size columns to their content and then
     squeeze them back inside the cap.

     That auto layout is only the starting point a table is painted with:
     markdown/tableLayout.ts then measures each column's content and writes
     exact widths to a `<colgroup>`, switching the table to
     `table-layout: fixed` inline so they are honoured to the pixel. See that
     file for why the browser's own auto layout cannot be left to do it. */
  .measure :global(.milkdown .milkdown-table-block table.children) {
    width: auto;
    min-width: 0;
    max-width: 100%;
    table-layout: auto;
  }
  .measure
    :global(
      .milkdown .milkdown-table-block[data-full-width="true"] table.children
    ) {
    width: 100%;
  }

  /* Crepe's cell borders are the outline colour at 20% opacity, which all but
     disappears on paper. Ken draws the full hairline instead, with a heavier
     rule under the header row. Long cells wrap; `anywhere` is what lets a
     single unbroken token (a URL, an id) give way rather than spill out of a
     column that tableLayout.ts has had to make narrower than it. */
  .measure :global(.milkdown .milkdown-table-block th),
  .measure :global(.milkdown .milkdown-table-block td) {
    border: 1px solid var(--border-strong);
    /* Roomy on a wide pane; tighter on a narrow one, where the padding of a
       many-columned table is what would stop it fitting at all. */
    padding: 6px clamp(7px, 1.2cqw, 16px);
    white-space: normal;
    overflow-wrap: anywhere;
    word-break: normal;
  }
  .measure :global(.milkdown .milkdown-table-block th) {
    background: var(--sunken);
    border-bottom-color: var(--ink-tertiary);
  }

  /* Hover-only, like the mermaid toolbar: the width toggle sits just above the
     table's top-right corner so it never covers a cell. */
  .measure :global(.milkdown .milkdown-table-block .ken-table-width-toggle) {
    position: absolute;
    top: -12px;
    right: 0;
    z-index: 40;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 24px;
    padding: 0;
    border: 1px solid var(--border);
    border-radius: 7px;
    background: var(--surface);
    color: var(--ink-secondary);
    box-shadow: var(--shadow-card);
    opacity: 0;
    transition: opacity 120ms ease;
  }
  .measure
    :global(.milkdown .milkdown-table-block:hover .ken-table-width-toggle),
  .measure
    :global(
      .milkdown .milkdown-table-block .ken-table-width-toggle:focus-visible
    ) {
    opacity: 1;
  }
  .measure
    :global(.milkdown .milkdown-table-block .ken-table-width-toggle:hover) {
    color: var(--ink);
    border-color: var(--ink-tertiary);
  }

  /* Crepe draws the row/column handles in `--crepe-color-outline`, which maps
     to Ken's hairline `--border-strong` and left them all but invisible against
     the paper. Give the pills a readable glyph and an edge so they read as
     controls; everything else in the table block is Crepe's. */
  .measure :global(.milkdown .milkdown-table-block .cell-handle),
  .measure :global(.milkdown .milkdown-table-block .line-handle .add-button) {
    color: var(--ink-secondary);
    border: 1px solid var(--border-strong);
  }
  .measure :global(.milkdown .milkdown-table-block .cell-handle svg),
  .measure
    :global(.milkdown .milkdown-table-block .line-handle .add-button svg) {
    fill: var(--ink-secondary);
  }
  .measure :global(.milkdown .milkdown-table-block .cell-handle:hover),
  .measure
    :global(.milkdown .milkdown-table-block .line-handle .add-button:hover) {
    border-color: var(--ink-tertiary);
  }
  .measure
    :global(.milkdown .milkdown-table-block .cell-handle .button-group svg) {
    fill: var(--ink-secondary);
  }

  /* --- GitHub-flavored markdown ------------------------------------------ */
  .measure :global(.milkdown .ProseMirror del) {
    color: var(--ink-secondary);
  }
  /* A ticked task reads as done. */
  .measure :global(.milkdown .list-item:has(> .label-wrapper .label.checked)) {
    color: var(--ink-secondary);
  }
  /* Crepe draws bullet dots and ordinals in its outline colour, which reads as
     chrome; a list marker belongs to the text it marks, so it follows the item's
     own ink (the drawn bullets below use `currentcolor`). Task checkboxes are
     controls; see below. */
  .measure :global(.milkdown .milkdown-list-item-block li .label-wrapper) {
    color: inherit;
  }
  /* Task checkboxes are controls, so they keep a control's colour rather than
     the text's — but Crepe's outline colour is a hairline, too faint to read
     as a box, so they take the same secondary ink as Crepe's other buttons. */
  .measure
    :global(
      .milkdown .milkdown-list-item-block li .label-wrapper .label.checked svg
    ),
  .measure
    :global(
      .milkdown .milkdown-list-item-block li .label-wrapper .label.unchecked svg
    ) {
    fill: var(--ink-secondary);
  }
  /* Crepe sets an ordinal in a fixed 32px box at the editor's inherited
     14.5px, while its paragraphs are 16px on a 1.85 line — so "1." sat smaller
     than its text and a few pixels high. The marker's box mirrors the
     paragraph's instead (Crepe's `p`: 16px, `padding: 4px 0`; Ken's line
     height), which puts ordinals on the text's baseline and centres bullets
     and task checkboxes on its first line (Crepe's own 32px box left the
     checkbox 2px high of it). */
  .measure
    :global(
      .milkdown .milkdown-list-item-block li .label-wrapper:has(> .label.ordered)
    ),
  .measure
    :global(
      .milkdown .milkdown-list-item-block li .label-wrapper:has(> .label.bullet)
    ),
  .measure
    :global(
      .milkdown .milkdown-list-item-block li .label-wrapper:has(> .label.checked)
    ),
  .measure
    :global(
      .milkdown
        .milkdown-list-item-block
        li
        .label-wrapper:has(> .label.unchecked)
    ) {
    height: auto;
    align-items: flex-start;
  }
  .measure :global(.milkdown .milkdown-list-item-block li .label.ordered),
  .measure :global(.milkdown .milkdown-list-item-block li .label.bullet),
  .measure :global(.milkdown .milkdown-list-item-block li .label.checked),
  .measure :global(.milkdown .milkdown-list-item-block li .label.unchecked) {
    box-sizing: border-box;
    height: calc(1.85em + 8px);
    padding: 4px 0;
    font-size: 16px;
    line-height: 1.85;
  }
  .measure :global(.milkdown .milkdown-list-item-block li .label.ordered) {
    /* "10." and up are wider than the 24px gutter; end-aligned flex lets
       them run left into the indent rather than wrap or push the text. */
    display: flex;
    justify-content: flex-end;
    white-space: nowrap;
  }
  /* Bullets alternate with depth, as a word processor's do: disc, open
     circle, square, then round again. Crepe's node view doesn't know an
     item's depth, so it is counted here in `ul` ancestors only — the first
     bullets under a numbered item are discs, not circles. Each extra `ul` in
     a selector also raises its specificity, so the deepest match wins. Task
     items (`.checked`/`.unchecked`) aren't `.bullet`, and the TOC hides its
     labels, so neither is touched. Crepe's SVG dot is replaced by a drawn
     one; both follow the item's ink. */
  .measure :global(.milkdown .milkdown-list-item-block li .label.bullet),
  .measure :global(.milkdown .milkdown-list-item-block li .label.checked),
  .measure :global(.milkdown .milkdown-list-item-block li .label.unchecked) {
    display: flex;
    align-items: center;
    justify-content: center;
  }
  /* The checkbox glyph keeps its own size inside the taller box. */
  .measure :global(.milkdown .milkdown-list-item-block li .label.checked svg),
  .measure :global(.milkdown .milkdown-list-item-block li .label.unchecked svg) {
    flex: none;
  }
  .measure :global(.milkdown .milkdown-list-item-block li .label.bullet svg) {
    display: none;
  }
  .measure :global(.milkdown ul .milkdown-list-item-block li .label.bullet::before),
  .measure
    :global(.milkdown ul ul ul ul .milkdown-list-item-block li .label.bullet::before) {
    content: "";
    box-sizing: border-box;
    width: 6px;
    height: 6px;
    border: none;
    border-radius: 50%;
    background: currentcolor;
  }
  .measure
    :global(.milkdown ul ul .milkdown-list-item-block li .label.bullet::before),
  .measure
    :global(
      .milkdown ul ul ul ul ul .milkdown-list-item-block li .label.bullet::before
    ) {
    border: 1.25px solid currentcolor;
    border-radius: 50%;
    background: transparent;
  }
  .measure
    :global(.milkdown ul ul ul .milkdown-list-item-block li .label.bullet::before),
  .measure
    :global(
      .milkdown ul ul ul ul ul ul .milkdown-list-item-block li .label.bullet::before
    ) {
    /* A square reads heavier than a disc of the same width. */
    width: 5px;
    height: 5px;
    border: none;
    border-radius: 0;
    background: currentcolor;
  }
  /* A seventh level and deeper is a disc again. */
  .measure
    :global(
      .milkdown
        ul
        ul
        ul
        ul
        ul
        ul
        ul
        .milkdown-list-item-block
        li
        .label.bullet::before
    ) {
    width: 6px;
    height: 6px;
    border: none;
    border-radius: 50%;
    background: currentcolor;
  }
  /* Footnote definitions read as an aside, not as body copy. */
  .measure :global(.milkdown dl[data-type="footnote_definition"]) {
    border-top: 1px solid var(--border);
    margin-top: 24px;
    padding-top: 12px;
    font-size: 13px;
    color: var(--ink-secondary);
  }

  /* A wider scale than Crepe's, so the outline of a document reads at a glance.
     Line height tightens as the type grows, and the space above each heading
     grows with it, so nothing crowds. */
  .measure :global(.milkdown h1) {
    font-size: 2.25em;
    line-height: 1.18;
    margin-top: 0.9em;
  }
  .measure :global(.milkdown h2) {
    font-size: 1.75em;
    line-height: 1.24;
    margin-top: 1.1em;
  }
  .measure :global(.milkdown h3) {
    font-size: 1.45em;
    line-height: 1.3;
    margin-top: 1.2em;
  }
  .measure :global(.milkdown h4) {
    font-size: 1.25em;
    line-height: 1.36;
    margin-top: 1.3em;
  }
  .measure :global(.milkdown h5) {
    font-size: 1.1em;
    line-height: 1.42;
    margin-top: 1.4em;
  }
  .measure :global(.milkdown h6) {
    font-size: 1em;
    line-height: 1.5;
    margin-top: 1.5em;
    color: var(--ink-secondary);
  }
  /* The document's own title has nothing to separate itself from. */
  .measure :global(.milkdown .ProseMirror > h1:first-child),
  .measure :global(.milkdown .ProseMirror > h2:first-child),
  .measure :global(.milkdown .ProseMirror > h3:first-child) {
    margin-top: 0;
  }

  /* --- Heading links ------------------------------------------------------ */
  /* A hover-only "copy link" button in the text column's left gutter. It is a
     ProseMirror widget, so it is never part of the document.

     Crepe's block drag/insert handle already sits in that gutter, from 82px to
     16px left of the text, and appears on the same hover — so the button goes
     further out still, leaving both clickable. How far out is what the gutter
     allows: `(100cqw - 100%) / 2` is the distance from the text column's edge
     to the pane's, against `.editor-scroll` as the query container. Wide
     panes give the full 110px, which clears Crepe's handle; narrower ones
     slide the button in towards the text until it is flush with the pane
     edge, where it still clears the text (and sits above Crepe's handle,
     which is itself clipped at those widths). */
  .measure :global(.milkdown h1),
  .measure :global(.milkdown h2),
  .measure :global(.milkdown h3),
  .measure :global(.milkdown h4),
  .measure :global(.milkdown h5),
  .measure :global(.milkdown h6) {
    position: relative;
  }
  /* Table of contents (markdown/toc.ts). On disk it is a plain nested bullet
     list of anchor links; here it reads as a section of the document itself:
     a heading that matches the document's own H1, then an indented index of
     links flush with the text column. Nothing about the document changes —
     the caption is a `::before`, not a node. */
  .measure :global(.milkdown ul.ken-toc) {
    margin: 1.5em 0;
    padding: 0;
    list-style: none;
    font-family: var(--font-sans);
    user-select: none;
  }
  /* Styled as `.milkdown h1` is, so it reads as the document's own heading. */
  .measure :global(.milkdown ul.ken-toc::before) {
    content: "Table of Contents";
    display: block;
    font-family: var(--font-serif);
    font-weight: 500;
    letter-spacing: -0.01em;
    font-size: 2.25em;
    line-height: 1.18;
    margin-bottom: 0.55em;
    color: var(--ink);
  }
  .measure :global(.milkdown ul.ken-toc ul) {
    margin: 0;
    padding-left: 1.75em;
    list-style: none;
  }
  /* Crepe's list-item node view gives every item a paragraph's worth of air
     and a bullet in a fixed-width gutter; a table of contents wants to read as
     a compact index of plain links instead, starting at the text column's own
     left edge. */
  .measure :global(.milkdown ul.ken-toc li.list-item) {
    margin: 0;
    gap: 0;
  }
  .measure :global(.milkdown ul.ken-toc .label-wrapper) {
    width: 0;
    height: auto;
  }
  .measure :global(.milkdown ul.ken-toc .milkdown-icon.label) {
    display: none;
  }
  .measure :global(.milkdown ul.ken-toc li p) {
    margin: 0;
    padding: 0.35em 0;
    line-height: 1.6;
  }
  /* An entry for an `h1` carries the weight its heading does. The decoration
     covers the item's nested list too, so those links take it back. */
  .measure :global(.milkdown ul.ken-toc .ken-toc-h1 a) {
    font-weight: 600;
  }
  .measure :global(.milkdown ul.ken-toc .ken-toc-h1 ul a) {
    font-weight: 400;
  }
  /* The entries sit a touch below body size; the caption keeps the document's
     own scale, so `2.25em` above resolves against it and matches a real H1. */
  .measure :global(.milkdown ul.ken-toc a) {
    font-size: 0.95em;
    cursor: pointer;
  }

  .measure :global(.milkdown .ken-heading-link) {
    position: absolute;
    left: calc(-1 * clamp(28px, (100cqw - 100%) / 2 - 4px, 110px));
    /* Centred on the heading's first line, whatever its size. A button does
       not inherit the font size by default, so `em` would otherwise resolve
       against the UA's 13.3px rather than the heading's type. */
    font-size: inherit;
    top: 0.62em;
    transform: translateY(-50%);
    z-index: 5;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    padding: 0;
    border: none;
    border-radius: 6px;
    background: transparent;
    box-shadow: none;
    color: var(--ink-secondary);
    opacity: 0;
    transition: opacity 120ms ease;
    user-select: none;
    line-height: 1;
  }
  .measure :global(.milkdown h1:hover .ken-heading-link),
  .measure :global(.milkdown h2:hover .ken-heading-link),
  .measure :global(.milkdown h3:hover .ken-heading-link),
  .measure :global(.milkdown h4:hover .ken-heading-link),
  .measure :global(.milkdown h5:hover .ken-heading-link),
  .measure :global(.milkdown h6:hover .ken-heading-link),
  .measure :global(.milkdown .ken-heading-link:focus-visible) {
    opacity: 1;
  }
  .measure :global(.milkdown .ken-heading-link:hover) {
    color: var(--ink);
    background: var(--sunken);
  }
  .measure :global(.milkdown .ken-heading-link.copied) {
    opacity: 1;
    color: var(--accent-deep);
  }

  /* --- Lightbox affordance ------------------------------------------------ */
  .measure :global(.milkdown .mermaid-preview),
  .measure :global(.milkdown .milkdown-image-block img),
  .measure :global(.milkdown .milkdown-image-block .image-wrapper) {
    cursor: zoom-in;
  }
</style>

<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import type { PageViewport, PDFDocumentProxy } from "pdfjs-dist";
  import type { TextItem } from "pdfjs-dist/types/src/display/api";
  import { api } from "../../lib/api";
  import { MATCH_CAP, findTextMatches } from "../../lib/find";
  import { find, type FindAdapter } from "../../lib/find.svelte";
  import PreviewLoading from "./PreviewLoading.svelte";

  let { relPath }: { relPath: string } = $props();

  let host: HTMLDivElement;
  let error = $state<string | null>(null);
  let loading = $state(true);
  let cancelled = false;

  interface Page {
    number: number;
    overlay: HTMLDivElement;
    wrapper: HTMLDivElement;
    viewport: PageViewport;
    /** Text runs, fetched the first time someone searches this document. */
    items?: TextItem[];
  }

  interface Hit {
    page: number; // index into `pages`
    item: number;
    start: number;
  }

  let doc: PDFDocumentProxy | null = null;
  let pages: Page[] = [];
  let hits: Hit[] = [];
  let boxes: HTMLElement[] = [];

  // A PDF is rendered to canvas, so a hit has to be drawn as a box over the
  // page. Text is read per page, and only for the first TEXT_PAGE_CAP pages —
  // pulling the text layer out of a 900-page scan would stall the UI.
  const TEXT_PAGE_CAP = 200;

  async function loadText(): Promise<void> {
    if (!doc) return;
    for (const page of pages.slice(0, TEXT_PAGE_CAP)) {
      if (page.items || cancelled) continue;
      const content = await doc.getPage(page.number).then((p) => p.getTextContent());
      page.items = content.items.filter(
        (item): item is TextItem => "str" in item,
      );
    }
  }

  function clearBoxes() {
    for (const box of boxes) box.remove();
    boxes = [];
  }

  /**
   * Where a run of characters sits on the page. pdf.js gives the position and
   * width of a whole text run, not of each glyph, so the offset inside the run
   * is interpolated by character count — exact for the monospaced and near
   * enough for everything else to land the highlight on the right words.
   */
  function draw(hit: Hit, queryLength: number, current: boolean): HTMLElement | null {
    const page = pages[hit.page];
    const item = page.items?.[hit.item];
    if (!item || !item.str.length) return null;

    const [, , c, d, e, f] = mul(
      page.viewport.transform as unknown as number[],
      item.transform,
    );
    const height = Math.hypot(c, d) || item.height;
    const width = item.width * page.viewport.scale;
    const left = e + (width * hit.start) / item.str.length;
    const boxWidth = Math.max((width * queryLength) / item.str.length, 2);

    // Positioned in percentages, not pixels: a narrow pane scales the canvas
    // down (max-width: 100%) and the boxes have to scale with it.
    const pw = page.viewport.width;
    const ph = page.viewport.height;
    const box = document.createElement("div");
    box.className = current ? "pdf-hit current" : "pdf-hit";
    box.style.left = `${(left / pw) * 100}%`;
    box.style.top = `${((f - height) / ph) * 100}%`;
    box.style.width = `${(boxWidth / pw) * 100}%`;
    box.style.height = `${((height * 1.15) / ph) * 100}%`;
    page.overlay.appendChild(box);
    boxes.push(box);
    return box;
  }

  /** 2D matrix multiply — pdf.js's Util.transform, without importing the module. */
  function mul(a: number[], b: number[]): number[] {
    return [
      a[0] * b[0] + a[2] * b[1],
      a[1] * b[0] + a[3] * b[1],
      a[0] * b[2] + a[2] * b[3],
      a[1] * b[2] + a[3] * b[3],
      a[0] * b[4] + a[2] * b[5] + a[4],
      a[1] * b[4] + a[3] * b[5] + a[5],
    ];
  }

  // Bumped on every search so a slow one (loadText awaits pdf.js) that resolves
  // after the user has already typed more can't overwrite the newer result's
  // boxes. The controller drops the stale total; this keeps the drawn boxes and
  // `hits` in step with the query the reader can actually see.
  let searchGen = 0;

  $effect(() => {
    const adapter: FindAdapter = {
      async search(query) {
        const gen = ++searchGen;
        if (!query.trim()) {
          clearBoxes();
          hits = [];
          return { total: 0 };
        }
        await loadText();
        if (gen !== searchGen) return { total: 0 }; // superseded; leave shared state alone

        // A match split across two text runs ("in-" / "voice") isn't found; the
        // runs are what pdf.js gives us, and stitching them would misplace the box.
        const found: Hit[] = [];
        for (let p = 0; p < pages.length && found.length < MATCH_CAP; p++) {
          const items = pages[p].items ?? [];
          for (let i = 0; i < items.length && found.length < MATCH_CAP; i++) {
            for (const start of findTextMatches(
              items[i].str,
              query,
              MATCH_CAP - found.length,
            ).starts) {
              found.push({ page: p, item: i, start });
            }
          }
        }

        // Commit in one synchronous step: concurrent searches each build their
        // own list, so an older one can never mix its hits into a newer count or
        // leave orphaned boxes on the page.
        clearBoxes();
        hits = found;
        for (const hit of found) draw(hit, query.length, false);
        return {
          total: found.length,
          capped: found.length >= MATCH_CAP,
          note:
            pages.length > TEXT_PAGE_CAP
              ? `Searching the first ${TEXT_PAGE_CAP} pages of ${pages.length}.`
              : undefined,
        };
      },
      reveal(index, opts) {
        boxes.forEach((box, i) => box.classList.toggle("current", i === index));
        if (opts?.scroll !== false) {
          boxes[index]?.scrollIntoView({ block: "center", inline: "nearest" });
        }
      },
      clear() {
        hits = [];
        clearBoxes();
      },
    };
    find.register(adapter);
    return () => {
      clearBoxes();
      find.unregister(adapter);
    };
  });

  onMount(async () => {
    try {
      const pdfjs = await import("pdfjs-dist");
      pdfjs.GlobalWorkerOptions.workerSrc = new URL(
        "pdfjs-dist/build/pdf.worker.min.mjs",
        import.meta.url,
      ).toString();

      const bytes = await api.readFileBytes(relPath);
      doc = await pdfjs.getDocument({ data: new Uint8Array(bytes) }).promise;
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

        // Canvas plus an overlay the find boxes are drawn into, in page pixels.
        const wrapper = document.createElement("div");
        wrapper.className = "page-box";
        wrapper.style.width = `${viewport.width}px`;
        const overlay = document.createElement("div");
        overlay.className = "page-overlay";
        wrapper.append(canvas, overlay);
        host.appendChild(wrapper);
        pages.push({ number: i, wrapper, overlay, viewport });

        await page.render({ canvasContext: ctx, viewport, canvas }).promise;
      }
      find.refresh(); // pages arrived after the user already typed a query
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
    <PreviewLoading label="Rendering PDF…" />
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
  .pages :global(.page-box) {
    position: relative;
    max-width: 100%;
  }
  .pages :global(.page-overlay) {
    position: absolute;
    inset: 0;
    pointer-events: none;
  }
  .pages :global(.pdf-hit) {
    position: absolute;
    background: color-mix(in srgb, var(--needs-input) 45%, transparent);
    border-radius: 2px;
    mix-blend-mode: multiply;
  }
  .pages :global(.pdf-hit.current) {
    background: color-mix(in srgb, var(--accent) 50%, transparent);
    box-shadow: 0 0 0 1px var(--accent);
  }
  .pages :global(canvas) {
    display: block;
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

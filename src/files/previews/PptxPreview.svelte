<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "../../lib/api";
  import { registerDomFind } from "../../lib/find-dom.svelte";
  import { mimeForExtension } from "../../lib/mime";
  import {
    parseRels,
    parseSlide,
    parseSlideSize,
    resolvePath,
    slidePathsInOrder,
    type Shape,
    type SlideSize,
  } from "./pptx";
  import PreviewLoading from "./PreviewLoading.svelte";

  let { relPath }: { relPath: string } = $props();

  interface RenderShape extends Shape {
    /** Data-URI resolved from the shape's embedId, for image shapes. */
    imgSrc?: string;
  }
  interface RenderSlide {
    number: number;
    shapes: RenderShape[];
    /** True once any shape carries real coordinates → absolute layout. */
    positioned: boolean;
  }

  // A runaway deck shouldn't lock the pane; we render the first slides and note
  // the cut. Real presentations rarely approach this.
  const MAX_SLIDES = 300;

  let slides = $state<RenderSlide[]>([]);
  let size = $state<SlideSize>({ width: 1280, height: 720 });
  let loading = $state(true);
  let truncated = $state(false);
  let error = $state<string | null>(null);
  let deck = $state<HTMLDivElement | null>(null);

  registerDomFind(() => deck, { deps: () => slides.length });

  function defaultSizePt(shape: RenderShape): number {
    return shape.isTitle ? 40 : 18;
  }

  /** Points → CSS px (72pt/inch, 96px/inch) within the slide's native frame. */
  function ptToPx(pt: number): number {
    return pt * (96 / 72);
  }

  function runStyle(
    run: RenderShape["paragraphs"][number]["runs"][number],
    shape: RenderShape,
  ): string {
    const parts = [`font-size:${ptToPx(run.sizePt ?? defaultSizePt(shape))}px`];
    if (run.bold) parts.push("font-weight:700");
    if (run.italic) parts.push("font-style:italic");
    if (run.underline) parts.push("text-decoration:underline");
    if (run.color) parts.push(`color:${run.color}`);
    if (run.font) parts.push(`font-family:'${run.font}',sans-serif`);
    return parts.join(";");
  }

  function boxStyle(shape: RenderShape): string {
    if (shape.x === null || shape.y === null) return "";
    const parts = [
      "position:absolute",
      `left:${shape.x}px`,
      `top:${shape.y}px`,
    ];
    if (shape.w !== null) parts.push(`width:${shape.w}px`);
    if (shape.h !== null) parts.push(`height:${shape.h}px`);
    parts.push(
      `justify-content:${shape.anchor === "center" ? "center" : shape.anchor === "bottom" ? "flex-end" : "flex-start"}`,
    );
    if (shape.fill) parts.push(`background:${shape.fill}`);
    if (shape.geom === "ellipse") parts.push("border-radius:50%");
    else if (shape.geom === "roundRect") parts.push("border-radius:8%");
    return parts.join(";");
  }

  /** Set --scale so a native-sized slide fills the responsive wrapper width. */
  function fitScale(node: HTMLElement, nativeWidth: number) {
    const apply = () =>
      node.style.setProperty("--scale", String(node.clientWidth / nativeWidth));
    const ro = new ResizeObserver(apply);
    ro.observe(node);
    apply();
    return { destroy: () => ro.disconnect() };
  }

  onMount(async () => {
    try {
      const JSZip = (await import("jszip")).default;
      const bytes = await api.readFileBytes(relPath);
      const zip = await JSZip.loadAsync(bytes);

      const presXml = await zip.file("ppt/presentation.xml")?.async("string");
      const presRels = await zip
        .file("ppt/_rels/presentation.xml.rels")
        ?.async("string");
      size = parseSlideSize(presXml);

      let slidePaths = slidePathsInOrder(presXml, presRels);
      // Bare decks (no presentation part) still enumerate slide files directly.
      if (slidePaths.length === 0) {
        slidePaths = Object.keys(zip.files)
          .filter((n) => /^ppt\/slides\/slide\d+\.xml$/.test(n))
          .sort((a, b) => {
            const na = Number(a.match(/slide(\d+)\.xml/)?.[1] ?? 0);
            const nb = Number(b.match(/slide(\d+)\.xml/)?.[1] ?? 0);
            return na - nb;
          });
      }

      if (slidePaths.length > MAX_SLIDES) {
        slidePaths = slidePaths.slice(0, MAX_SLIDES);
        truncated = true;
      }

      // Same media bytes are often reused across slides; decode each once.
      const mediaCache = new Map<string, string | null>();
      async function dataUri(path: string): Promise<string | null> {
        if (mediaCache.has(path)) return mediaCache.get(path)!;
        const ext = path.split(".").pop() ?? "";
        const mime = mimeForExtension(ext);
        let uri: string | null = null;
        if (mime) {
          const b64 = await zip.file(path)?.async("base64");
          if (b64) uri = `data:${mime};base64,${b64}`;
        }
        mediaCache.set(path, uri);
        return uri;
      }

      for (let i = 0; i < slidePaths.length; i++) {
        const path = slidePaths[i];
        const xml = await zip.file(path)?.async("string");
        if (!xml) continue;

        const { shapes } = parseSlide(xml);
        const dir = path.slice(0, path.lastIndexOf("/"));
        const name = path.slice(path.lastIndexOf("/") + 1);
        const relsXml = await zip
          .file(`${dir}/_rels/${name}.rels`)
          ?.async("string");
        const rels = relsXml ? parseRels(relsXml) : null;

        const render: RenderShape[] = [];
        for (const shape of shapes) {
          const rs: RenderShape = { ...shape };
          if (shape.kind === "image" && shape.embedId && rels) {
            const target = rels.get(shape.embedId);
            if (target) {
              const src = await dataUri(resolvePath(dir, target));
              if (src) rs.imgSrc = src;
            }
          }
          // Drop images we couldn't decode (e.g. emf/wmf) so no broken box shows.
          if (shape.kind === "image" && !rs.imgSrc) continue;
          render.push(rs);
        }

        slides.push({
          number: i + 1,
          shapes: render,
          positioned: render.some((s) => s.x !== null),
        });
        // Yield between slides so a large deck streams in without freezing.
        await new Promise((r) => setTimeout(r));
      }
    } catch (e) {
      error = `Couldn't render this deck — ${e}. Try “Open in default app”.`;
    } finally {
      loading = false;
    }
  });
</script>

<div class="scroll">
  {#if error}
    <div class="note error">{error}</div>
  {:else if slides.length === 0}
    {#if loading}
      <PreviewLoading label="Rendering deck…" />
    {:else}
      <div class="note">No slides found in this deck.</div>
    {/if}
  {:else}
    <div class="deck" bind:this={deck}>
      {#each slides as slide (slide.number)}
        <figure class="slide-wrap" style="aspect-ratio:{size.width}/{size.height}">
          <div
            class="stage"
            class:flow={!slide.positioned}
            style="width:{size.width}px;height:{size.height}px"
            use:fitScale={size.width}
          >
            {#each slide.shapes as shape, si (si)}
              <div class="shape" style={boxStyle(shape)}>
                {#if shape.imgSrc}
                  <img src={shape.imgSrc} alt="" />
                {:else}
                  {#each shape.paragraphs as para, pi (pi)}
                    <p
                      class="para"
                      class:bullet={para.bullet}
                      style="text-align:{para.align ??
                        (shape.isTitle ? 'center' : 'left')};padding-left:{para.level *
                        24}px"
                    >
                      {#each para.runs as run, ri (ri)}<span
                          style={runStyle(run, shape)}>{run.text}</span
                        >{/each}
                    </p>
                  {/each}
                {/if}
              </div>
            {/each}
          </div>
          <figcaption class="badge">{slide.number}</figcaption>
        </figure>
      {/each}
      {#if truncated}
        <div class="note">
          Showing the first {MAX_SLIDES} slides of a larger deck.
        </div>
      {:else if loading}
        <div class="note">Rendering more slides…</div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .scroll {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    background: var(--sunken);
  }
  .deck {
    max-width: 900px;
    margin: 0 auto;
    padding: 28px clamp(16px, 4%, 40px) 80px;
    display: flex;
    flex-direction: column;
    gap: 22px;
  }
  .slide-wrap {
    position: relative;
    margin: 0;
    width: 100%;
    background: #fff;
    border: 1px solid var(--border);
    border-radius: var(--radius-card);
    box-shadow: var(--shadow-card);
    overflow: hidden;
  }
  /* Native-sized slide frame, scaled to the wrapper via --scale so DOM text
     stays crisp (vector) at any zoom. Content keeps its authored colours. */
  .stage {
    position: absolute;
    top: 0;
    left: 0;
    transform-origin: top left;
    transform: scale(var(--scale, 1));
    color: #1a1a1a;
    font-family: var(--font-sans, system-ui, sans-serif);
  }
  .stage.flow {
    box-sizing: border-box;
    padding: 6%;
    display: flex;
    flex-direction: column;
    justify-content: center;
    gap: 2%;
  }
  .shape {
    display: flex;
    flex-direction: column;
    box-sizing: border-box;
    overflow: hidden;
  }
  .stage.flow .shape {
    position: static !important;
    width: 100% !important;
    height: auto !important;
  }
  .shape img {
    width: 100%;
    height: 100%;
    object-fit: contain;
  }
  /* Auto-flow shapes have no fixed height, so let the image size to its box. */
  .stage.flow .shape img {
    height: auto;
    max-height: 60%;
    object-position: left;
  }
  .para {
    margin: 0;
    line-height: 1.25;
    white-space: pre-wrap;
    word-break: break-word;
  }
  .para.bullet {
    position: relative;
    padding-left: 1.1em;
  }
  .para.bullet::before {
    content: "•";
    position: absolute;
    left: 0.15em;
  }
  .badge {
    position: absolute;
    top: 8px;
    right: 10px;
    font-family: var(--font-mono);
    font-size: 10.5px;
    color: var(--ink-tertiary);
    background: var(--paper);
    border: 1px solid var(--border);
    border-radius: 5px;
    padding: 1px 6px;
    z-index: 1;
  }
  .note {
    text-align: center;
    color: var(--ink-tertiary);
    font-size: 13px;
    padding: 40px 20px;
  }
  .note.error {
    color: var(--danger);
  }
</style>

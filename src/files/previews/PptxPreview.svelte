<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { api } from "../../lib/api";
  import { registerDomFind } from "../../lib/find-dom.svelte";
  import { mimeForExtension } from "../../lib/mime";
  import {
    parseBackground,
    parseMasterTextStyles,
    parsePlaceholders,
    parseRels,
    parseSlide,
    parseSlideSize,
    parseTheme,
    resolvePath,
    slidePathsInOrder,
    type MasterTextStyles,
    type Placeholder,
    type Shape,
    type SlideSize,
    type ThemeContext,
  } from "./pptx";
  import { extractZip, type PartFiles } from "./pptx.worker";
  import PreviewLoading from "./PreviewLoading.svelte";

  let { relPath }: { relPath: string } = $props();

  interface RenderShape extends Shape {
    /** Object URL resolved from the shape's embedId, for image shapes. */
    imgSrc?: string;
    /** An image we can't decode (EMF/WMF) → render a labelled placeholder. */
    imgUndecodable?: boolean;
    /** Object URL for a picture (blip) shape fill → CSS background-image. */
    fillImageUrl?: string;
  }
  interface RenderBackground {
    color?: string;
    gradient?: string;
    imageUrl?: string;
  }
  interface RenderSlide {
    number: number;
    shapes: RenderShape[];
    background?: RenderBackground;
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

  // Non-reactive working state: cancellation flag, the live worker, decoded media
  // (path → object URL), and the URLs to revoke on teardown.
  let cancelled = false;
  let worker: Worker | null = null;
  const mediaMap = new Map<string, { url?: string }>();
  const objectUrls: string[] = [];

  function defaultSizePt(shape: RenderShape): number {
    return shape.defaultSizePt ?? (shape.isTitle ? 40 : 18);
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

  /** SVG stroke-dasharray (px, relative to line width) for a dash family. */
  function dashArray(shape: RenderShape): string | undefined {
    const line = shape.line;
    if (!line?.dash) return undefined;
    const w = Math.max(line.width, 1);
    if (line.dash === "dot") return `${w} ${w * 2}`;
    if (line.dash === "dashDot") return `${w * 4} ${w * 2} ${w} ${w * 2}`;
    return `${w * 4} ${w * 2}`;
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
    // Shapes drawn as an SVG path (custGeom / presets) paint their own fill,
    // outline and — via a drop-shadow filter — their shadow; a plain autoshape
    // box gets CSS fill/gradient/picture, border and box-shadow instead.
    const isPath = !!shape.geomPath;
    if (!isPath && shape.kind === "shape") {
      if (shape.fillImageUrl)
        parts.push(`background-image:url(${shape.fillImageUrl})`, "background-size:cover");
      else if (shape.gradient) parts.push(`background-image:${shape.gradient}`);
      else if (shape.fill) parts.push(`background:${shape.fill}`);
      if (shape.line) {
        const style =
          shape.line.dash === "dot" ? "dotted" : shape.line.dash ? "dashed" : "solid";
        parts.push(`border:${shape.line.width}px ${style} ${shape.line.color}`);
      }
      if (shape.geom === "ellipse") parts.push("border-radius:50%");
      else if (shape.geom === "roundRect") parts.push("border-radius:8%");
    }
    if (shape.shadow) {
      parts.push(isPath ? `filter:drop-shadow(${shape.shadow})` : `box-shadow:${shape.shadow}`);
    }
    const tf: string[] = [];
    if (shape.rot) tf.push(`rotate(${shape.rot}deg)`);
    if (shape.flipH) tf.push("scaleX(-1)");
    if (shape.flipV) tf.push("scaleY(-1)");
    if (tf.length) parts.push(`transform:${tf.join(" ")}`);
    return parts.join(";");
  }

  function stageStyle(slide: RenderSlide): string {
    const parts = [`width:${size.width}px`, `height:${size.height}px`];
    const bg = slide.background;
    if (bg?.color) parts.push(`background-color:${bg.color}`);
    if (bg?.imageUrl)
      parts.push(`background-image:url(${bg.imageUrl})`, "background-size:cover");
    else if (bg?.gradient) parts.push(`background-image:${bg.gradient}`);
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

  // --- Media (built as worker/main-thread streams entries in) -----------------

  function ingestMedia(path: string, buffer: ArrayBuffer) {
    if (cancelled) return;
    const ext = path.split(".").pop()?.toLowerCase() ?? "";
    const mime = mimeForExtension(ext);
    let url: string | undefined;
    if (mime) {
      url = URL.createObjectURL(new Blob([buffer], { type: mime }));
      objectUrls.push(url);
    }
    // No mime (EMF/WMF, …): still record the path so an image referencing it is
    // shown as a placeholder rather than silently dropped.
    mediaMap.set(path, { url });
  }

  /** Resolve an embed id, against a rels map + base dir, to its decoded media. */
  function mediaByEmbed(
    embedId: string | undefined,
    rels: Map<string, string> | null,
    dir: string,
  ): { url?: string } | undefined {
    const target = embedId ? rels?.get(embedId) : undefined;
    if (!target) return undefined;
    return mediaMap.get(resolvePath(dir, target));
  }

  // --- Off-thread unzip, with a main-thread fallback --------------------------

  function collectFiles(bytes: ArrayBuffer): Promise<PartFiles> {
    return new Promise((resolve, reject) => {
      let w: Worker;
      try {
        w = new Worker(new URL("./pptx.worker.ts", import.meta.url), {
          type: "module",
        });
      } catch {
        // Worker unavailable → do the unzip inline, yielding between media.
        collectOnMainThread(bytes).then(resolve, reject);
        return;
      }
      worker = w;
      let files: PartFiles | null = null;
      let fellBack = false;
      const fallback = () => {
        if (fellBack) return;
        fellBack = true;
        w.terminate();
        collectOnMainThread(bytes).then(resolve, reject);
      };
      w.onmessage = (e: MessageEvent) => {
        if (cancelled) {
          w.terminate();
          resolve({});
          return;
        }
        const msg = e.data;
        if (msg.type === "parts") files = msg.files;
        else if (msg.type === "media") ingestMedia(msg.path, msg.buffer);
        else if (msg.type === "done") {
          w.terminate();
          resolve(files ?? {});
        } else if (msg.type === "error") fallback();
      };
      w.onerror = fallback;
      w.postMessage({ bytes }, [bytes]);
    });
  }

  async function collectOnMainThread(bytes: ArrayBuffer): Promise<PartFiles> {
    let files: PartFiles = {};
    await extractZip(bytes, {
      onParts: (f) => {
        files = f;
      },
      onMedia: async (path, buffer) => {
        ingestMedia(path, buffer);
        // Yield so a big deck's inflate doesn't monopolise the main thread.
        await new Promise((r) => setTimeout(r));
      },
      shouldCancel: () => cancelled,
    });
    return files;
  }

  // --- Turn the collected parts into rendered slides, one at a time -----------

  function dirOf(path: string): string {
    return path.slice(0, path.lastIndexOf("/"));
  }

  function relsFor(files: PartFiles, path: string): Map<string, string> | null {
    const name = path.slice(path.lastIndexOf("/") + 1);
    const xml = files[`${dirOf(path)}/_rels/${name}.rels`];
    return xml ? parseRels(xml) : null;
  }

  /** First relationship target whose path mentions `substr`, resolved to a path. */
  function relTarget(
    rels: Map<string, string> | null,
    baseDir: string,
    substr: string,
  ): string | undefined {
    if (!rels) return undefined;
    for (const target of rels.values()) {
      if (target.includes(substr)) return resolvePath(baseDir, target);
    }
    return undefined;
  }

  function toRenderBg(
    bg: ReturnType<typeof parseBackground>,
    rels: Map<string, string> | null,
    dir: string,
  ): RenderBackground | undefined {
    if (!bg) return undefined;
    const out: RenderBackground = { color: bg.color, gradient: bg.gradient };
    if (bg.imageEmbedId) out.imageUrl = mediaByEmbed(bg.imageEmbedId, rels, dir)?.url;
    return out.color || out.gradient || out.imageUrl ? out : undefined;
  }

  /** Memoise a per-part computation keyed by the part's path. */
  function memo<T>(cache: Map<string, T>, key: string | undefined, make: () => T): T | undefined {
    if (!key) return undefined;
    let v = cache.get(key);
    if (v === undefined) {
      v = make();
      cache.set(key, v);
    }
    return v;
  }

  async function renderBundle(files: PartFiles) {
    const presXml = files["ppt/presentation.xml"];
    const presRels = files["ppt/_rels/presentation.xml.rels"];
    size = parseSlideSize(presXml);

    let slidePaths = slidePathsInOrder(presXml, presRels);
    // Bare decks (no presentation part) still enumerate slide files directly.
    if (slidePaths.length === 0) {
      slidePaths = Object.keys(files)
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

    // Caches keyed by part path: layout/master parts recur across many slides.
    const themeCache = new Map<string, ThemeContext>();
    const layoutPhCache = new Map<string, Placeholder[]>();
    const masterPhCache = new Map<string, Placeholder[]>();
    const textStyleCache = new Map<string, MasterTextStyles>();

    for (let i = 0; i < slidePaths.length; i++) {
      if (cancelled) return;
      const path = slidePaths[i];
      const xml = files[path];
      if (!xml) continue;
      const slideDir = dirOf(path);
      const slideRels = relsFor(files, path);

      // Resolve the slide → layout → master → theme chain from relationships.
      const layoutPath = relTarget(slideRels, slideDir, "slideLayout");
      const layoutXml = layoutPath ? files[layoutPath] : undefined;
      const layoutRels = layoutPath ? relsFor(files, layoutPath) : null;
      const layoutDir = layoutPath ? dirOf(layoutPath) : "";
      const masterPath = relTarget(layoutRels, layoutDir, "slideMaster");
      const masterXml = masterPath ? files[masterPath] : undefined;
      const masterRels = masterPath ? relsFor(files, masterPath) : null;
      const masterDir = masterPath ? dirOf(masterPath) : "";
      const themePath = relTarget(masterRels, masterDir, "theme");
      const themeXml = themePath ? files[themePath] : undefined;

      const theme =
        memo(themeCache, masterPath ?? "∅", () => parseTheme(themeXml, masterXml)) ??
        parseTheme(themeXml, masterXml);
      const inherit = {
        layout: memo(layoutPhCache, layoutPath, () => parsePlaceholders(layoutXml)),
        master: memo(masterPhCache, masterPath, () => parsePlaceholders(masterXml)),
        textStyles: memo(textStyleCache, masterPath, () => parseMasterTextStyles(masterXml)),
      };

      const parsed = parseSlide(xml, theme, inherit);

      const render: RenderShape[] = [];
      for (const shape of parsed.shapes) {
        const rs: RenderShape = { ...shape };
        if (shape.kind === "image") {
          const m = mediaByEmbed(shape.embedId, slideRels, slideDir);
          if (m?.url) rs.imgSrc = m.url;
          else if (m) rs.imgUndecodable = true; // known media, no decoder
          else continue; // unresolved reference → nothing to show
        }
        if (shape.fillImageEmbedId) {
          rs.fillImageUrl = mediaByEmbed(shape.fillImageEmbedId, slideRels, slideDir)?.url;
        }
        render.push(rs);
      }

      // Background: the slide's own fill, else the layout's, else the master's.
      const background =
        toRenderBg(parsed.background, slideRels, slideDir) ??
        toRenderBg(parseBackground(layoutXml, theme), layoutRels, layoutDir) ??
        toRenderBg(parseBackground(masterXml, theme), masterRels, masterDir);

      if (cancelled) return;
      slides.push({
        number: i + 1,
        shapes: render,
        background,
        positioned: render.some((s) => s.x !== null),
      });
      // Yield between slides so a large deck streams in without freezing.
      await new Promise((r) => setTimeout(r));
    }
  }

  onMount(() => {
    (async () => {
      try {
        const raw = await api.readFileBytes(relPath);
        if (cancelled) return;
        // Normalise to a fresh, fully-owned ArrayBuffer we can transfer to the
        // worker (Tauri may hand back either an ArrayBuffer or a number[]).
        const u8 = new Uint8Array(raw as ArrayBuffer);
        const files = await collectFiles(u8.buffer as ArrayBuffer);
        if (cancelled) return;
        await renderBundle(files);
      } catch (e) {
        if (!cancelled) {
          error = `Couldn't render this deck — ${e}. Try “Open in default app”.`;
        }
      } finally {
        if (!cancelled) loading = false;
      }
    })();
  });

  onDestroy(() => {
    cancelled = true;
    worker?.terminate();
    worker = null;
    for (const url of objectUrls) URL.revokeObjectURL(url);
    objectUrls.length = 0;
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
            style={stageStyle(slide)}
            use:fitScale={size.width}
          >
            {#each slide.shapes as shape, si (si)}
              <div class="shape" style={boxStyle(shape)}>
                {#if shape.geomPath}
                  <svg
                    class="geom"
                    viewBox="0 0 {shape.pathW} {shape.pathH}"
                    preserveAspectRatio="none"
                  >
                    <path
                      d={shape.geomPath}
                      fill={shape.fill ?? "none"}
                      stroke={shape.line?.color ?? "none"}
                      stroke-width={shape.line ? shape.line.width : 0}
                      stroke-dasharray={dashArray(shape) ?? ""}
                      vector-effect="non-scaling-stroke"
                    />
                  </svg>
                {/if}
                {#if shape.kind === "image"}
                  {#if shape.imgSrc}
                    <img src={shape.imgSrc} alt="" />
                  {:else if shape.imgUndecodable}
                    <div class="ph">Vector image (EMF/WMF)</div>
                  {/if}
                {:else if shape.kind === "placeholder"}
                  <div class="ph">{shape.placeholder}</div>
                {:else if shape.kind === "table" && shape.table}
                  <table class="tbl">
                    <colgroup>
                      {#each shape.table.colWidths as cw, ci (ci)}<col
                          style="width:{cw}px"
                        />{/each}
                    </colgroup>
                    <tbody>
                      {#each shape.table.rows as row, ri (ri)}
                        <tr style={row.height ? `height:${row.height}px` : ""}>
                          {#each row.cells as cell, ci (ci)}
                            {#if !cell.hMerge && !cell.vMerge}
                              <td
                                colspan={cell.gridSpan}
                                rowspan={cell.rowSpan}
                                style="{cell.fill
                                  ? `background:${cell.fill};`
                                  : ''}vertical-align:{cell.anchor === 'center'
                                  ? 'middle'
                                  : cell.anchor === 'bottom'
                                    ? 'bottom'
                                    : 'top'}"
                              >
                                {#each cell.paragraphs as para, pi (pi)}
                                  <p class="para" style="text-align:{para.align ?? 'left'}">
                                    {#each para.runs as run, rii (rii)}<span
                                        style={runStyle(run, shape)}>{run.text}</span
                                      >{/each}
                                  </p>
                                {/each}
                              </td>
                            {/if}
                          {/each}
                        </tr>
                      {/each}
                    </tbody>
                  </table>
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
    background-repeat: no-repeat;
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
  /* Labelled stand-in for content we don't render (charts, SmartArt, EMF/WMF). */
  .ph {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    text-align: center;
    padding: 8px;
    font-size: 13px;
    color: #6b6b6b;
    background: repeating-linear-gradient(
      45deg,
      #f4f4f4,
      #f4f4f4 8px,
      #ececec 8px,
      #ececec 16px
    );
    border: 1px dashed #c4c4c4;
    box-sizing: border-box;
  }
  /* SVG geometry sits behind the shape's own text, filling its box. */
  .geom {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    display: block;
    overflow: visible;
    pointer-events: none;
  }
  .tbl {
    width: 100%;
    height: 100%;
    border-collapse: collapse;
    table-layout: fixed;
    font-size: 14px;
  }
  .tbl td {
    border: 1px solid #bbb;
    padding: 2px 6px;
    overflow: hidden;
    word-break: break-word;
  }
  .para {
    margin: 0;
    line-height: 1.25;
    white-space: pre-wrap;
    word-break: break-word;
    position: relative;
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

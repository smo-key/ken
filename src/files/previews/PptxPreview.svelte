<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "../../lib/api";

  let { relPath }: { relPath: string } = $props();

  interface Slide {
    number: number;
    /** Shapes → paragraphs of text (non-empty lines only). */
    blocks: string[][];
    /** Data-URI images embedded on the slide. */
    images: string[];
  }

  let slides = $state<Slide[] | null>(null);
  let error = $state<string | null>(null);

  const IMG_MIME: Record<string, string> = {
    png: "image/png",
    jpg: "image/jpeg",
    jpeg: "image/jpeg",
    gif: "image/gif",
    bmp: "image/bmp",
    webp: "image/webp",
    svg: "image/svg+xml",
    emf: "image/emf",
    wmf: "image/wmf",
  };

  const parser = new DOMParser();

  /** Resolve a rels Target (possibly `../media/x`) against a base directory. */
  function resolvePath(baseDir: string, target: string): string {
    const parts = baseDir.split("/").filter(Boolean);
    for (const seg of target.split("/")) {
      if (seg === "..") parts.pop();
      else if (seg !== "." && seg !== "") parts.push(seg);
    }
    return parts.join("/");
  }

  function parseRels(xml: string): Map<string, string> {
    const map = new Map<string, string>();
    const doc = parser.parseFromString(xml, "application/xml");
    for (const rel of Array.from(doc.getElementsByTagName("Relationship"))) {
      const id = rel.getAttribute("Id");
      const target = rel.getAttribute("Target");
      if (id && target) map.set(id, target);
    }
    return map;
  }

  function extractBlocks(xml: string): string[][] {
    const doc = parser.parseFromString(xml, "application/xml");
    const shapes = Array.from(doc.getElementsByTagName("p:sp"));
    const scopes = shapes.length ? shapes : [doc.documentElement];
    const blocks: string[][] = [];
    for (const scope of scopes) {
      const lines: string[] = [];
      for (const p of Array.from(scope.getElementsByTagName("a:p"))) {
        const line = Array.from(p.getElementsByTagName("a:t"))
          .map((t) => t.textContent ?? "")
          .join("");
        if (line.trim()) lines.push(line);
      }
      if (lines.length) blocks.push(lines);
    }
    return blocks;
  }

  onMount(async () => {
    try {
      const JSZip = (await import("jszip")).default;
      const bytes = await api.readFileBytes(relPath);
      const zip = await JSZip.loadAsync(bytes);

      // Slide order: presentation.xml lists sldId (r:id) → rels maps to a file.
      let slidePaths: string[] = [];
      const presXml = await zip.file("ppt/presentation.xml")?.async("string");
      const presRelsXml = await zip
        .file("ppt/_rels/presentation.xml.rels")
        ?.async("string");
      if (presXml && presRelsXml) {
        const rels = parseRels(presRelsXml);
        const doc = parser.parseFromString(presXml, "application/xml");
        for (const s of Array.from(doc.getElementsByTagName("p:sldId"))) {
          const rid =
            s.getAttribute("r:id") ??
            s.getAttributeNS(
              "http://schemas.openxmlformats.org/officeDocument/2006/relationships",
              "id",
            );
          const target = rid ? rels.get(rid) : undefined;
          if (target) slidePaths.push(resolvePath("ppt", target));
        }
      }
      // Fallback: enumerate slide files numerically.
      if (slidePaths.length === 0) {
        slidePaths = Object.keys(zip.files)
          .filter((n) => /^ppt\/slides\/slide\d+\.xml$/.test(n))
          .sort((a, b) => {
            const na = Number(a.match(/slide(\d+)\.xml/)?.[1] ?? 0);
            const nb = Number(b.match(/slide(\d+)\.xml/)?.[1] ?? 0);
            return na - nb;
          });
      }

      const out: Slide[] = [];
      for (let i = 0; i < slidePaths.length; i++) {
        const path = slidePaths[i];
        const xml = await zip.file(path)?.async("string");
        if (!xml) continue;
        const blocks = extractBlocks(xml);

        // Images: slide rels map embed ids → media files.
        const images: string[] = [];
        try {
          const dir = path.slice(0, path.lastIndexOf("/"));
          const name = path.slice(path.lastIndexOf("/") + 1);
          const relsXml = await zip
            .file(`${dir}/_rels/${name}.rels`)
            ?.async("string");
          if (relsXml) {
            const rels = parseRels(relsXml);
            const doc = parser.parseFromString(xml, "application/xml");
            const embeds = new Set<string>();
            for (const b of Array.from(doc.getElementsByTagName("a:blip"))) {
              const id =
                b.getAttribute("r:embed") ??
                b.getAttributeNS(
                  "http://schemas.openxmlformats.org/officeDocument/2006/relationships",
                  "embed",
                );
              if (id) embeds.add(id);
            }
            for (const id of embeds) {
              const target = rels.get(id);
              if (!target) continue;
              const mediaPath = resolvePath(dir, target);
              const ext = mediaPath.split(".").pop()?.toLowerCase() ?? "";
              const mime = IMG_MIME[ext];
              // Skip vector metafiles browsers can't display inline.
              if (!mime || mime === "image/emf" || mime === "image/wmf") continue;
              const b64 = await zip.file(mediaPath)?.async("base64");
              if (b64) images.push(`data:${mime};base64,${b64}`);
            }
          }
        } catch {
          /* images are best-effort; text still renders */
        }

        out.push({ number: i + 1, blocks, images });
      }

      slides = out;
    } catch (e) {
      error = `Couldn't render this deck — ${e}. Try “Open in default app”.`;
    }
  });
</script>

<div class="scroll">
  {#if error}
    <div class="note error">{error}</div>
  {:else if slides === null}
    <div class="note">Rendering deck…</div>
  {:else if slides.length === 0}
    <div class="note">No slides found in this deck.</div>
  {:else}
    <div class="deck">
      {#each slides as slide (slide.number)}
        <div class="slide">
          <span class="badge">{slide.number}</span>
          <div class="content">
            {#each slide.blocks as block, bi (bi)}
              <div class="block">
                {#each block as line, li (li)}
                  {#if bi === 0 && li === 0}
                    <div class="title">{line}</div>
                  {:else}
                    <div class="line">{line}</div>
                  {/if}
                {/each}
              </div>
            {/each}
            {#if slide.images.length}
              <div class="images">
                {#each slide.images as src, ii (ii)}
                  <img {src} alt="slide visual" />
                {/each}
              </div>
            {/if}
            {#if slide.blocks.length === 0 && slide.images.length === 0}
              <div class="empty">Slide {slide.number}</div>
            {/if}
          </div>
        </div>
      {/each}
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
    max-width: 780px;
    margin: 0 auto;
    padding: 28px clamp(16px, 4%, 40px) 80px;
    display: flex;
    flex-direction: column;
    gap: 20px;
  }
  .slide {
    position: relative;
    aspect-ratio: 16 / 9;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-card);
    box-shadow: var(--shadow-card);
    overflow: hidden;
  }
  .badge {
    position: absolute;
    top: 10px;
    right: 12px;
    font-family: var(--font-mono);
    font-size: 10.5px;
    color: var(--ink-tertiary);
    background: var(--paper);
    border: 1px solid var(--border);
    border-radius: 5px;
    padding: 1px 6px;
    z-index: 1;
  }
  .content {
    height: 100%;
    box-sizing: border-box;
    padding: clamp(20px, 5%, 40px);
    display: flex;
    flex-direction: column;
    gap: 12px;
    overflow: auto;
  }
  .block {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .title {
    font-family: var(--font-serif);
    font-weight: 500;
    font-size: clamp(20px, 3.4vw, 30px);
    letter-spacing: -0.01em;
    line-height: 1.2;
    color: var(--ink);
  }
  .line {
    font-size: 14.5px;
    line-height: 1.5;
    color: var(--ink-secondary);
  }
  .images {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
    margin-top: auto;
  }
  .images img {
    max-width: 100%;
    max-height: 220px;
    border-radius: 6px;
    border: 1px solid var(--border);
  }
  .empty {
    margin: auto;
    color: var(--ink-tertiary);
    font-size: 13px;
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

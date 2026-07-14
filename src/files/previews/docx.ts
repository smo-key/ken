// Pure OOXML parsing for the .docx previewer's image sizing. mammoth inlines
// every embedded image as a base64 data URI at its *intrinsic* resolution and
// drops the size Word laid it out at, so a small avatar stored high-res renders
// huge. Word records the intended display size on each drawing as <wp:extent>;
// this module extracts those (in document order) and stamps them back onto the
// <img> elements mammoth emitted. Kept free of Svelte/jszip/mammoth so it runs
// under vitest with only a DOMParser (happy-dom).

// OOXML measures in EMU: 914400 per inch, and CSS is 96px per inch, so one CSS
// pixel is exactly 9525 EMU.
const EMU_PER_PX = 9525;

export function emuToPx(emu: number): number {
  // Rounded because the result becomes an integer width/height attribute.
  return Math.round(emu / EMU_PER_PX);
}

export interface ImageExtent {
  /** r:embed relationship id of the picture, for diagnostics/correlation. */
  embedId: string | null;
  width: number;
  height: number;
}

const parser = new DOMParser();

// happy-dom silently drops prefixed *attributes* (r:embed) when parsing
// namespaced XML, so relationship ids would vanish in tests. Rewrite the colon
// in attribute names to an underscore before parsing — applied uniformly so the
// webview and happy-dom agree — while leaving element tag names and xmlns
// declarations (needed for namespace resolution) untouched.
function normalizeNsAttrs(xml: string): string {
  return xml.replace(
    /(\s)([A-Za-z][\w-]*):([A-Za-z][\w-]*)=/g,
    (m, sp, prefix, local) => (prefix === "xmlns" ? m : `${sp}${prefix}_${local}=`),
  );
}

function relAttr(el: Element, local: string): string | null {
  return (
    el.getAttribute(`r_${local}`) ??
    Array.from(el.attributes).find((a) => a.name.endsWith(`_${local}`))?.value ??
    null
  );
}

/**
 * Extract the display extent of every embedded picture in `word/document.xml`,
 * in document order. Only drawings that carry both a <wp:extent> and an
 * <a:blip r:embed> are included: those are exactly the ones mammoth turns into
 * an <img>, so the returned order stays aligned with mammoth's image stream.
 * Charts, shapes and linked (non-embedded) drawings are skipped so they never
 * consume a slot and shift later images.
 */
export function parseExtents(documentXml: string): ImageExtent[] {
  const dom = parser.parseFromString(normalizeNsAttrs(documentXml), "application/xml");
  const out: ImageExtent[] = [];
  for (const drawing of Array.from(dom.getElementsByTagName("w:drawing"))) {
    const blip = drawing.getElementsByTagName("a:blip")[0];
    const embedId = blip ? relAttr(blip, "embed") : null;
    if (!embedId) continue; // no embedded picture → mammoth emits no <img>
    const extent = drawing.getElementsByTagName("wp:extent")[0];
    const cx = Number(extent?.getAttribute("cx"));
    const cy = Number(extent?.getAttribute("cy"));
    if (!cx || !cy) continue;
    out.push({ embedId, width: emuToPx(cx), height: emuToPx(cy) });
  }
  return out;
}

/**
 * Stamp each extent onto the mammoth-emitted <img> at the same position.
 * Correlation is positional (see report): mammoth does not surface the embed id
 * in its output, and positional order also handles the same image reused at
 * different sizes. Images with no matching extent are left untouched and fall
 * back to the component's `max-width: 100%` bound. The paired CSS
 * `height: auto` keeps the width attribute authoritative while preserving
 * aspect ratio, so a genuinely large figure is still capped at the pane width.
 */
export function applyExtentsToHtml(html: string, extents: ImageExtent[]): string {
  if (extents.length === 0) return html;
  const dom = parser.parseFromString(html, "text/html");
  const imgs = dom.querySelectorAll("img");
  for (let i = 0; i < imgs.length && i < extents.length; i++) {
    imgs[i].setAttribute("width", String(extents[i].width));
    imgs[i].setAttribute("height", String(extents[i].height));
  }
  return dom.body.innerHTML;
}

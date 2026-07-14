// Pure OOXML parsing for the .pptx previewer. A .pptx is a zip of DrawingML
// XML; the component owns the zip/media I/O, this module owns turning slide XML
// into a positioned, styled model the view can lay out absolutely. Kept free of
// Svelte/jszip so it runs under vitest with only a DOMParser (happy-dom).

// OOXML measures in EMU: 914400 per inch, and CSS is 96px per inch, so one CSS
// pixel is exactly 9525 EMU. We render each slide at these native px then scale
// the whole slide with a CSS transform, which keeps DOM text crisp at any size.
const EMU_PER_PX = 9525;

export function emuToPx(emu: number): number {
  return emu / EMU_PER_PX;
}

export interface SlideSize {
  width: number;
  height: number;
}

export interface TextRun {
  text: string;
  bold?: boolean;
  italic?: boolean;
  underline?: boolean;
  color?: string;
  sizePt?: number;
  font?: string;
}

export interface Paragraph {
  runs: TextRun[];
  align?: "left" | "center" | "right" | "justify";
  level: number;
  bullet: boolean;
}

export type ShapeKind = "text" | "image" | "shape";

export interface Shape {
  kind: ShapeKind;
  // Native px, or null when the shape carries no xfrm (bare decks, inherited
  // placeholders) — the view then flows it instead of absolute-positioning it.
  x: number | null;
  y: number | null;
  w: number | null;
  h: number | null;
  paragraphs: Paragraph[];
  /** Relationship id of an embedded picture; caller resolves it to a data URI. */
  embedId?: string;
  /** Solid fill colour of an autoshape box, when present. */
  fill?: string;
  geom?: "rect" | "ellipse" | "roundRect";
  isTitle: boolean;
  anchor: "top" | "center" | "bottom";
}

export interface ParsedSlide {
  shapes: Shape[];
}

const parser = new DOMParser();

// happy-dom silently drops prefixed *attributes* (r:embed, r:id) when parsing
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

function parseXml(xml: string): Document {
  return parser.parseFromString(normalizeNsAttrs(xml), "application/xml");
}

/** Read a relationships-namespace attribute (r:embed → r_embed after normalize). */
function relAttr(el: Element, local: string): string | null {
  return (
    el.getAttribute(`r_${local}`) ??
    Array.from(el.attributes).find((a) => a.name.endsWith(`_${local}`))?.value ??
    null
  );
}

function firstChildTag(parent: Element, tag: string): Element | null {
  return parent.getElementsByTagName(tag)[0] ?? null;
}

export function parseSlideSize(presentationXml: string | undefined): SlideSize {
  // 13.333in × 7.5in is the modern 16:9 default; used when there is no
  // presentation part or it omits the size.
  const fallback: SlideSize = { width: 1280, height: 720 };
  if (!presentationXml) return fallback;
  const sz = firstChildTag(parseXml(presentationXml).documentElement, "p:sldSz");
  const cx = Number(sz?.getAttribute("cx"));
  const cy = Number(sz?.getAttribute("cy"));
  if (!cx || !cy) return fallback;
  return { width: emuToPx(cx), height: emuToPx(cy) };
}

/** Resolve a rels Target (possibly `../media/x`) against a base directory. */
export function resolvePath(baseDir: string, target: string): string {
  const parts = baseDir.split("/").filter(Boolean);
  for (const seg of target.split("/")) {
    if (seg === "..") parts.pop();
    else if (seg !== "." && seg !== "") parts.push(seg);
  }
  return parts.join("/");
}

export function parseRels(xml: string): Map<string, string> {
  const map = new Map<string, string>();
  for (const rel of Array.from(
    parseXml(xml).getElementsByTagName("Relationship"),
  )) {
    const id = rel.getAttribute("Id");
    const target = rel.getAttribute("Target");
    if (id && target) map.set(id, target);
  }
  return map;
}

/**
 * Slide file paths in presentation (reading) order. PowerPoint stores order in
 * presentation.xml's sldIdLst, not in filenames, so we follow r:id → rels.
 */
export function slidePathsInOrder(
  presentationXml: string | undefined,
  presentationRelsXml: string | undefined,
): string[] {
  if (!presentationXml || !presentationRelsXml) return [];
  const rels = parseRels(presentationRelsXml);
  const doc = parseXml(presentationXml);
  const paths: string[] = [];
  for (const s of Array.from(doc.getElementsByTagName("p:sldId"))) {
    const rid = relAttr(s, "id");
    const target = rid ? rels.get(rid) : undefined;
    if (target) paths.push(resolvePath("ppt", target));
  }
  return paths;
}

function xfrmOf(sp: Element): Pick<Shape, "x" | "y" | "w" | "h"> {
  const xfrm = firstChildTag(sp, "a:xfrm");
  const off = xfrm && firstChildTag(xfrm, "a:off");
  const ext = xfrm && firstChildTag(xfrm, "a:ext");
  if (!off || !ext) return { x: null, y: null, w: null, h: null };
  return {
    x: emuToPx(Number(off.getAttribute("x") ?? 0)),
    y: emuToPx(Number(off.getAttribute("y") ?? 0)),
    w: emuToPx(Number(ext.getAttribute("cx") ?? 0)),
    h: emuToPx(Number(ext.getAttribute("cy") ?? 0)),
  };
}

export interface ThemeContext {
  colors: Record<string, string>;
}

// --- Color helpers (sRGB <-> HSL for lumMod/lumOff) ---------------------------

function clamp01(n: number): number {
  return n < 0 ? 0 : n > 1 ? 1 : n;
}

function rgbToHsl(r: number, g: number, b: number): [number, number, number] {
  r /= 255; g /= 255; b /= 255;
  const max = Math.max(r, g, b), min = Math.min(r, g, b);
  const l = (max + min) / 2;
  let h = 0, s = 0;
  if (max !== min) {
    const d = max - min;
    s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
    if (max === r) h = (g - b) / d + (g < b ? 6 : 0);
    else if (max === g) h = (b - r) / d + 2;
    else h = (r - g) / d + 4;
    h /= 6;
  }
  return [h, s, l];
}

function hslToRgb(h: number, s: number, l: number): [number, number, number] {
  if (s === 0) {
    const v = Math.round(l * 255);
    return [v, v, v];
  }
  const hue = (p: number, q: number, t: number) => {
    if (t < 0) t += 1;
    if (t > 1) t -= 1;
    if (t < 1 / 6) return p + (q - p) * 6 * t;
    if (t < 1 / 2) return q;
    if (t < 2 / 3) return p + (q - p) * (2 / 3 - t) * 6;
    return p;
  };
  const q = l < 0.5 ? l * (1 + s) : l + s - l * s;
  const p = 2 * l - q;
  return [
    Math.round(hue(p, q, h + 1 / 3) * 255),
    Math.round(hue(p, q, h) * 255),
    Math.round(hue(p, q, h - 1 / 3) * 255),
  ];
}

function hex2(n: number): string {
  return Math.max(0, Math.min(255, Math.round(n))).toString(16).toUpperCase().padStart(2, "0");
}

/**
 * Apply DrawingML colour-transform child elements (in document order) to a base
 * RGB. Returns the final CSS colour string (#RRGGBB, or rgba(...) if alpha set).
 * frac = val/100000 (val is in thousandths). See Global Constraints for formulas.
 */
function applyColorTransforms(clr: Element, rgb: [number, number, number]): string {
  let [r, g, b] = rgb;
  let alpha = 1;
  for (const t of Array.from(clr.children)) {
    const frac = Number(t.getAttribute("val")) / 100000;
    switch (t.tagName) {
      case "a:shade":
        r *= frac; g *= frac; b *= frac;
        break;
      case "a:tint":
        r = r * frac + 255 * (1 - frac);
        g = g * frac + 255 * (1 - frac);
        b = b * frac + 255 * (1 - frac);
        break;
      case "a:lumMod": {
        const [h, s, l] = rgbToHsl(r, g, b);
        [r, g, b] = hslToRgb(h, s, clamp01(l * frac));
        break;
      }
      case "a:lumOff": {
        const [h, s, l] = rgbToHsl(r, g, b);
        [r, g, b] = hslToRgb(h, s, clamp01(l + frac));
        break;
      }
      case "a:alpha":
        alpha = frac;
        break;
      // a:satMod, a:hueMod, a:gamma, etc.: unsupported → ignored (v1).
    }
  }
  if (alpha < 1) {
    return `rgba(${Math.round(r)},${Math.round(g)},${Math.round(b)},${+alpha.toFixed(3)})`;
  }
  return `#${hex2(r)}${hex2(g)}${hex2(b)}`;
}

/** Hex ("RRGGBB") for a single scheme slot element (dk1/lt1/accentN/...). */
function schemeSlotHex(slot: Element | null): string | undefined {
  if (!slot) return undefined;
  const srgb = firstChildTag(slot, "a:srgbClr");
  if (srgb) return srgb.getAttribute("val")?.toUpperCase() ?? undefined;
  const sys = firstChildTag(slot, "a:sysClr");
  if (sys) return sys.getAttribute("lastClr")?.toUpperCase() ?? undefined;
  return undefined;
}

export function parseTheme(
  themeXml: string | undefined,
  masterXml: string | undefined,
): ThemeContext {
  const colors: Record<string, string> = {};
  if (!themeXml) return { colors };
  const scheme = firstChildTag(parseXml(themeXml).documentElement, "a:clrScheme");
  if (!scheme) return { colors };
  for (const slot of Array.from(scheme.children)) {
    const name = slot.tagName.replace(/^a:/, ""); // dk1, lt1, accent1, ...
    const hex = schemeSlotHex(slot);
    if (hex) colors[name] = hex;
  }
  // clrMap maps document names (bg1/tx1/bg2/tx2/accentN/...) onto scheme slots.
  if (masterXml) {
    const clrMap = firstChildTag(parseXml(masterXml).documentElement, "p:clrMap");
    if (clrMap) {
      for (const attr of Array.from(clrMap.attributes)) {
        const target = colors[attr.value];
        if (target) colors[attr.name] = target;
      }
    }
  }
  return { colors };
}

/** First DIRECT child with the given tag (unlike firstChildTag's descendant search). */
function directChild(parent: Element, tag: string): Element | null {
  for (const c of Array.from(parent.children)) if (c.tagName === tag) return c;
  return null;
}

/**
 * Resolve a solid colour off `clrParent` (a spPr, ln, rPr, tcPr, or a bare
 * a:solidFill) to a CSS colour, applying colour transforms. schemeClr needs the
 * theme. Replaces the old solidFillColor.
 *
 * IMPORTANT: uses DIRECT-child lookup, not firstChildTag's descendant search —
 * otherwise a shape with `<a:noFill/>` but an outlined `<a:ln><a:solidFill>` would
 * wrongly report the line's colour as its fill (these text-less shapes are now
 * rendered, not dropped, so the bug would be visible).
 */
export function resolveColor(
  clrParent: Element | null,
  theme?: ThemeContext,
): string | undefined {
  if (!clrParent) return undefined;
  // An explicit noFill on this element means "no colour" (e.g. spPr or ln).
  if (directChild(clrParent, "a:noFill")) return undefined;
  // clrParent may BE the solidFill (bare-fill callers) or CONTAIN one directly.
  const fill = clrParent.tagName === "a:solidFill"
    ? clrParent
    : directChild(clrParent, "a:solidFill");
  if (!fill) return undefined;
  const srgb = directChild(fill, "a:srgbClr");
  if (srgb) {
    const val = srgb.getAttribute("val");
    if (!val) return undefined;
    return applyColorTransforms(srgb, [
      parseInt(val.slice(0, 2), 16),
      parseInt(val.slice(2, 4), 16),
      parseInt(val.slice(4, 6), 16),
    ]);
  }
  const scheme = directChild(fill, "a:schemeClr");
  if (scheme) {
    const name = scheme.getAttribute("val");
    const hex = name ? theme?.colors[name] : undefined;
    if (!hex) return undefined;
    return applyColorTransforms(scheme, [
      parseInt(hex.slice(0, 2), 16),
      parseInt(hex.slice(2, 4), 16),
      parseInt(hex.slice(4, 6), 16),
    ]);
  }
  return undefined;
}

const ALIGN: Record<string, Paragraph["align"]> = {
  l: "left",
  ctr: "center",
  r: "right",
  just: "justify",
};

function parseRun(r: Element, theme?: ThemeContext): TextRun {
  const text = Array.from(r.getElementsByTagName("a:t"))
    .map((t) => t.textContent ?? "")
    .join("");
  const rPr = firstChildTag(r, "a:rPr");
  const run: TextRun = { text };
  if (rPr) {
    if (rPr.getAttribute("b") === "1") run.bold = true;
    if (rPr.getAttribute("i") === "1") run.italic = true;
    const u = rPr.getAttribute("u");
    if (u && u !== "none") run.underline = true;
    const sz = Number(rPr.getAttribute("sz"));
    if (sz) run.sizePt = sz / 100; // sz is in hundredths of a point
    const color = resolveColor(rPr, theme);
    if (color) run.color = color;
    const font = firstChildTag(rPr, "a:latin")?.getAttribute("typeface");
    if (font) run.font = font;
  }
  return run;
}

function parseParagraph(p: Element, theme?: ThemeContext): Paragraph {
  const runs: TextRun[] = [];
  // a:r runs and a:fld fields (slide numbers, dates) both carry a:t text.
  for (const child of Array.from(p.children)) {
    const tag = child.tagName;
    if (tag === "a:r" || tag === "a:fld") runs.push(parseRun(child, theme));
    else if (tag === "a:br") runs.push({ text: "\n" });
  }
  const pPr = firstChildTag(p, "a:pPr");
  const level = Number(pPr?.getAttribute("lvl") ?? 0) || 0;
  const align = pPr ? ALIGN[pPr.getAttribute("algn") ?? ""] : undefined;
  // Explicit bullet markup, or any indented list line, gets a bullet; a:buNone
  // suppresses it. Titles are handled by the shape, never bulleted here.
  const hasBullet = !!(
    pPr &&
    (pPr.getElementsByTagName("a:buChar").length ||
      pPr.getElementsByTagName("a:buAutoNum").length)
  );
  const noBullet = !!pPr && pPr.getElementsByTagName("a:buNone").length > 0;
  const bullet = !noBullet && (hasBullet || level > 0);
  return { runs, align, level, bullet };
}

function paragraphsOf(sp: Element, theme?: ThemeContext): Paragraph[] {
  const body = firstChildTag(sp, "p:txBody");
  if (!body) return [];
  return Array.from(body.getElementsByTagName("a:p"))
    .map((p) => parseParagraph(p, theme))
    .filter((p) => p.runs.some((r) => r.text.trim().length > 0));
}

function placeholderType(sp: Element): string | null {
  return firstChildTag(sp, "p:ph")?.getAttribute("type") ?? null;
}

const GEOM: Record<string, Shape["geom"]> = {
  ellipse: "ellipse",
  roundRect: "roundRect",
};

function parseSp(sp: Element, theme?: ThemeContext): Shape | null {
  const paragraphs = paragraphsOf(sp, theme);
  const spPr = firstChildTag(sp, "p:spPr");
  const fill = resolveColor(spPr, theme);
  const prst = spPr
    ? firstChildTag(spPr, "a:prstGeom")?.getAttribute("prst")
    : null;
  const geom: Shape["geom"] | undefined = prst
    ? (GEOM[prst] ?? "rect")
    : undefined;
  // Drop shapes that carry neither text nor any visible box.
  if (paragraphs.length === 0 && !fill) return null;

  const ph = placeholderType(sp);
  const isTitle = ph === "title" || ph === "ctrTitle";
  const anchorAttr = firstChildTag(sp, "a:bodyPr")?.getAttribute("anchor");
  const anchor: Shape["anchor"] =
    anchorAttr === "ctr" ? "center" : anchorAttr === "b" ? "bottom" : "top";

  return {
    kind: "shape",
    ...xfrmOf(sp),
    paragraphs,
    fill,
    geom,
    isTitle,
    anchor,
  };
}

function parsePic(pic: Element): Shape | null {
  const embedId = relAttr(firstChildTag(pic, "a:blip") ?? pic, "embed");
  return {
    kind: "image",
    ...xfrmOf(pic),
    paragraphs: [],
    embedId: embedId ?? undefined,
    isTitle: false,
    anchor: "top",
  };
}

export function parseSlide(slideXml: string, theme?: ThemeContext): ParsedSlide {
  const doc = parseXml(slideXml);
  const tree =
    doc.getElementsByTagName("p:spTree")[0] ?? doc.documentElement;
  const shapes: Shape[] = [];
  // Walk direct children in authored order so z-order is preserved. Group
  // shapes (p:grpSp) are flattened one level; nested groups are uncommon.
  const walk = (parent: Element) => {
    for (const el of Array.from(parent.children)) {
      if (el.tagName === "p:sp") {
        const s = parseSp(el, theme);
        if (s) shapes.push(s);
      } else if (el.tagName === "p:pic") {
        const s = parsePic(el);
        if (s) shapes.push(s);
      } else if (el.tagName === "p:grpSp") {
        walk(el);
      }
    }
  };
  walk(tree);
  return { shapes };
}

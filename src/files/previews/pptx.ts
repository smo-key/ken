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

export type ShapeKind =
  | "text"
  | "image"
  | "shape"
  | "custgeom"
  | "table"
  | "placeholder";

/** Resolved outline: CSS colour + width in px, plus optional dash pattern. */
export interface ShapeLine {
  color: string;
  width: number;
  /** Preset dash style (a:prstDash) mapped to a coarse family, when dashed. */
  dash?: "dash" | "dot" | "dashDot";
}

export interface TableCell {
  paragraphs: Paragraph[];
  /** Solid cell fill colour, when present. */
  fill?: string;
  gridSpan: number;
  rowSpan: number;
  /** Continuation of a horizontal span → the view skips it. */
  hMerge: boolean;
  /** Continuation of a vertical span → the view skips it. */
  vMerge: boolean;
  anchor: "top" | "center" | "bottom";
}

export interface TableRow {
  /** Native px, or null when the row omits an explicit height. */
  height: number | null;
  cells: TableCell[];
}

export interface Table {
  colWidths: number[];
  rows: TableRow[];
}

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
  /** CSS gradient string (a:gradFill), when the shape/background is gradient-filled. */
  gradient?: string;
  /** Relationship id of a picture fill (a:blipFill on spPr) → background-image. */
  fillImageEmbedId?: string;
  geom?: "rect" | "ellipse" | "roundRect";
  /** CSS box-shadow / drop-shadow value resolved from a:effectLst/a:outerShdw. */
  shadow?: string;
  /** Inherited default text size (pt) from the placeholder's master text style. */
  defaultSizePt?: number;
  /** Label for an unsupported embedded object (chart, SmartArt, OLE) placeholder. */
  placeholder?: string;
  /** Resolved outline (a:ln), when present. */
  line?: ShapeLine;
  /** custGeom: SVG path string in path space (view scales it to the box). */
  geomPath?: string;
  /** custGeom: path-space viewBox width. */
  pathW?: number;
  /** custGeom: path-space viewBox height. */
  pathH?: number;
  /** Parsed table model for a p:graphicFrame table shape. */
  table?: Table;
  isTitle: boolean;
  anchor: "top" | "center" | "bottom";
  /** Clockwise rotation in degrees (from a:xfrm rot, 60000ths of a degree). */
  rot?: number;
  /** Horizontal / vertical flip flags (from a:xfrm flipH/flipV). */
  flipH?: boolean;
  flipV?: boolean;
}

/** A resolved slide background fill: solid colour, CSS gradient, or picture. */
export interface SlideBackground {
  color?: string;
  gradient?: string;
  /** Relationship id of a background picture fill; caller resolves to a URI. */
  imageEmbedId?: string;
}

export interface ParsedSlide {
  shapes: Shape[];
  /** The slide's own background fill (p:cSld/p:bg), when it declares one. */
  background?: SlideBackground;
}

/** A placeholder's inheritable geometry/anchor, read from a layout or master. */
export interface Placeholder {
  type: string | null;
  idx: number | null;
  box: Pick<Shape, "x" | "y" | "w" | "h"> | null;
  anchor?: Shape["anchor"];
}

/** Default text sizes (pt) by placeholder family, read from a master's txStyles. */
export interface MasterTextStyles {
  title?: number;
  body?: number;
  other?: number;
}

/** Inheritance context threaded from the slide's layout + master into parseSlide. */
export interface InheritContext {
  layout?: Placeholder[];
  master?: Placeholder[];
  textStyles?: MasterTextStyles;
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

/** Per-shape rotation/flip, read off its own a:xfrm and passed through verbatim. */
function rotFlipOf(el: Element): Pick<Shape, "rot" | "flipH" | "flipV"> {
  const xfrm = firstChildTag(el, "a:xfrm");
  if (!xfrm) return {};
  const out: Pick<Shape, "rot" | "flipH" | "flipV"> = {};
  const rot = Number(xfrm.getAttribute("rot"));
  if (rot) out.rot = rot / 60000; // 60000ths of a degree → degrees
  if (xfrm.getAttribute("flipH") === "1") out.flipH = true;
  if (xfrm.getAttribute("flipV") === "1") out.flipV = true;
  return out;
}

// A group's a:xfrm maps child-space coords into slide space via an affine
// slide = s*child + b (per axis). Contexts compose so nested groups stack.
interface Ctx { sx: number; sy: number; bx: number; by: number; }
const IDENTITY: Ctx = { sx: 1, sy: 1, bx: 0, by: 0 };

/** Read a group's own a:xfrm (under p:grpSpPr) and compose it onto the parent ctx. */
function groupCtx(grp: Element, parent: Ctx): Ctx {
  const grpPr = firstChildTag(grp, "p:grpSpPr") ?? grp;
  const xfrm = firstChildTag(grpPr, "a:xfrm");
  if (!xfrm) return parent;
  const off = firstChildTag(xfrm, "a:off");
  const ext = firstChildTag(xfrm, "a:ext");
  const chOff = firstChildTag(xfrm, "a:chOff");
  const chExt = firstChildTag(xfrm, "a:chExt");
  if (!off || !ext || !chOff || !chExt) return parent;
  const num = (el: Element, a: string) => Number(el.getAttribute(a) ?? 0);
  const chcx = num(chExt, "cx"), chcy = num(chExt, "cy");
  const sx = chcx ? num(ext, "cx") / chcx : 1;
  const sy = chcy ? num(ext, "cy") / chcy : 1;
  // local (px): slide = s * child + b, with b = off - chOff*s
  const local: Ctx = {
    sx, sy,
    bx: emuToPx(num(off, "x") - num(chOff, "x") * sx),
    by: emuToPx(num(off, "y") - num(chOff, "y") * sy),
  };
  // compose parent ∘ local: s = ps*ls, b = ps*lb + pb
  return {
    sx: parent.sx * local.sx,
    sy: parent.sy * local.sy,
    bx: parent.sx * local.bx + parent.bx,
    by: parent.sy * local.by + parent.by,
  };
}

/** Apply a ctx to an already-EMU→px shape box. Identity is a no-op. */
function applyCtx(
  ctx: Ctx,
  box: Pick<Shape, "x" | "y" | "w" | "h">,
): Pick<Shape, "x" | "y" | "w" | "h"> {
  if (box.x === null || box.y === null) return box; // unpositioned stays unpositioned
  return {
    x: ctx.bx + ctx.sx * box.x,
    y: ctx.by + ctx.sy * box.y,
    w: box.w === null ? null : box.w * ctx.sx,
    h: box.h === null ? null : box.h * ctx.sy,
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
      case "a:satMod": {
        const [h, s, l] = rgbToHsl(r, g, b);
        [r, g, b] = hslToRgb(h, clamp01(s * frac), l);
        break;
      }
      case "a:hueMod": {
        const [h, s, l] = rgbToHsl(r, g, b);
        // hue is 0..1 (a full turn); hueMod multiplies the angle, wrapping.
        [r, g, b] = hslToRgb((h * frac) % 1, s, l);
        break;
      }
      case "a:alpha":
        alpha = frac;
        break;
      // a:gamma/a:invGamma and other rare transforms: still ignored.
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
/** Resolve a single colour element (a:srgbClr | a:schemeClr | a:sysClr | a:prstClr)
 *  to CSS, applying its colour transforms. Returns undefined if unresolvable. */
function resolveClrEl(el: Element, theme?: ThemeContext): string | undefined {
  if (el.tagName === "a:srgbClr") {
    const val = el.getAttribute("val");
    if (!val) return undefined;
    return applyColorTransforms(el, [
      parseInt(val.slice(0, 2), 16),
      parseInt(val.slice(2, 4), 16),
      parseInt(val.slice(4, 6), 16),
    ]);
  }
  if (el.tagName === "a:schemeClr") {
    const name = el.getAttribute("val");
    const hex = name ? theme?.colors[name] : undefined;
    if (!hex) return undefined;
    return applyColorTransforms(el, [
      parseInt(hex.slice(0, 2), 16),
      parseInt(hex.slice(2, 4), 16),
      parseInt(hex.slice(4, 6), 16),
    ]);
  }
  if (el.tagName === "a:sysClr") {
    const last = el.getAttribute("lastClr");
    if (!last) return undefined;
    return applyColorTransforms(el, [
      parseInt(last.slice(0, 2), 16),
      parseInt(last.slice(2, 4), 16),
      parseInt(last.slice(4, 6), 16),
    ]);
  }
  return undefined;
}

/** First colour child (srgb/scheme/sys) of an element, if any. */
function firstClrChild(parent: Element): Element | null {
  for (const c of Array.from(parent.children)) {
    if (
      c.tagName === "a:srgbClr" ||
      c.tagName === "a:schemeClr" ||
      c.tagName === "a:sysClr" ||
      c.tagName === "a:prstClr"
    ) {
      return c;
    }
  }
  return null;
}

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
  const clr = firstClrChild(fill);
  return clr ? resolveClrEl(clr, theme) : undefined;
}

/**
 * Resolve an a:gradFill element to a CSS gradient string, or undefined if it has
 * no usable stops. `fillParent` may BE the a:gradFill or CONTAIN one directly.
 * Linear gradients map a:lin ang (60000ths deg, clockwise from 3 o'clock) to the
 * CSS convention (deg from 12 o'clock) via +90; radial paths become radial-gradient.
 */
export function resolveGradient(
  fillParent: Element | null,
  theme?: ThemeContext,
): string | undefined {
  if (!fillParent) return undefined;
  const grad = fillParent.tagName === "a:gradFill"
    ? fillParent
    : directChild(fillParent, "a:gradFill");
  if (!grad) return undefined;
  const gsLst = directChild(grad, "a:gsLst");
  if (!gsLst) return undefined;
  const stops: string[] = [];
  for (const gs of Array.from(gsLst.children)) {
    if (gs.tagName !== "a:gs") continue;
    const clr = firstClrChild(gs);
    const css = clr ? resolveClrEl(clr, theme) : undefined;
    if (!css) continue;
    const pos = Number(gs.getAttribute("pos")) / 1000; // thousandths of a % → %
    stops.push(`${css} ${Number.isFinite(pos) ? pos : 0}%`);
  }
  if (stops.length < 2) return undefined;
  const path = directChild(grad, "a:path");
  if (path) {
    // Radial / rectangular: approximate all as a centred radial gradient.
    return `radial-gradient(circle, ${stops.join(", ")})`;
  }
  const lin = directChild(grad, "a:lin");
  const ang = lin ? Number(lin.getAttribute("ang")) / 60000 : 90;
  const cssAng = (((Number.isFinite(ang) ? ang : 90) + 90) % 360 + 360) % 360;
  return `linear-gradient(${cssAng}deg, ${stops.join(", ")})`;
}

/** Resolve a picture fill (a:blipFill) to its embed relationship id, if any. */
function blipEmbedId(fillParent: Element): string | null {
  const blipFill = fillParent.tagName === "a:blipFill"
    ? fillParent
    : directChild(fillParent, "a:blipFill");
  if (!blipFill) return null;
  const blip = directChild(blipFill, "a:blip");
  return blip ? relAttr(blip, "embed") : null;
}

/**
 * Resolve a slide/layout/master background (p:cSld/p:bg) to a fill. Accepts the
 * part's XML (any of the three levels) and returns the first p:bg it finds, or
 * undefined. Callers chain slide → layout → master for inheritance.
 */
export function parseBackground(
  xml: string | undefined,
  theme?: ThemeContext,
): SlideBackground | undefined {
  if (!xml) return undefined;
  const bg = parseXml(xml).getElementsByTagName("p:bg")[0];
  if (!bg) return undefined;
  return backgroundFromEl(bg, theme);
}

function backgroundFromEl(bg: Element, theme?: ThemeContext): SlideBackground | undefined {
  const bgPr = directChild(bg, "p:bgPr");
  if (bgPr) {
    if (directChild(bgPr, "a:noFill")) return undefined;
    const gradient = resolveGradient(bgPr, theme);
    if (gradient) return { gradient };
    const color = resolveColor(bgPr, theme);
    if (color) return { color };
    const imageEmbedId = blipEmbedId(bgPr);
    if (imageEmbedId) return { imageEmbedId };
    return undefined;
  }
  // p:bgRef references a theme fill by idx; we can at least honour its colour.
  const bgRef = directChild(bg, "p:bgRef");
  if (bgRef) {
    const clr = firstClrChild(bgRef);
    const color = clr ? resolveClrEl(clr, theme) : undefined;
    if (color) return { color };
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

function paragraphsOf(el: Element, theme?: ThemeContext): Paragraph[] {
  // Shapes carry p:txBody; table cells carry a:txBody.
  const body = firstChildTag(el, "p:txBody") ?? firstChildTag(el, "a:txBody");
  if (!body) return [];
  return Array.from(body.getElementsByTagName("a:p"))
    .map((p) => parseParagraph(p, theme))
    .filter((p) => p.runs.some((r) => r.text.trim().length > 0));
}

function placeholderEl(sp: Element): Element | null {
  return firstChildTag(sp, "p:ph");
}

function placeholderType(sp: Element): string | null {
  return placeholderEl(sp)?.getAttribute("type") ?? null;
}

function placeholderIdx(sp: Element): number | null {
  const idx = placeholderEl(sp)?.getAttribute("idx");
  return idx != null ? Number(idx) : null;
}

/** Normalize placeholder types so title/ctrTitle (and the body family) match. */
function phFamily(type: string | null): "title" | "body" | "other" {
  if (type === "title" || type === "ctrTitle") return "title";
  if (
    type == null ||
    type === "body" ||
    type === "subTitle" ||
    type === "obj"
  ) {
    return "body";
  }
  return "other";
}

function bodyAnchor(el: Element): Shape["anchor"] | undefined {
  const a = firstChildTag(el, "a:bodyPr")?.getAttribute("anchor");
  return a === "ctr" ? "center" : a === "b" ? "bottom" : a === "t" ? "top" : undefined;
}

/**
 * Read a layout/master part's placeholders (type/idx + geometry + anchor) so a
 * slide shape without its own xfrm can inherit them. Walks the spTree directly.
 */
export function parsePlaceholders(xml: string | undefined): Placeholder[] {
  if (!xml) return [];
  const doc = parseXml(xml);
  const tree = doc.getElementsByTagName("p:spTree")[0] ?? doc.documentElement;
  const out: Placeholder[] = [];
  for (const sp of Array.from(tree.getElementsByTagName("p:sp"))) {
    const ph = placeholderEl(sp);
    if (!ph) continue;
    const box = xfrmOf(sp);
    out.push({
      type: ph.getAttribute("type"),
      idx: ph.getAttribute("idx") != null ? Number(ph.getAttribute("idx")) : null,
      box: box.x === null ? null : box,
      anchor: bodyAnchor(sp),
    });
  }
  return out;
}

/** Best placeholder match for (type, idx): idx wins, else same family. */
function matchPlaceholder(
  list: Placeholder[] | undefined,
  type: string | null,
  idx: number | null,
): Placeholder | undefined {
  if (!list?.length) return undefined;
  if (idx != null) {
    const byIdx = list.find((p) => p.idx === idx);
    if (byIdx) return byIdx;
  }
  const fam = phFamily(type);
  return list.find((p) => phFamily(p.type) === fam);
}

/** Default text sizes (pt) per placeholder family from a master's p:txStyles. */
export function parseMasterTextStyles(xml: string | undefined): MasterTextStyles {
  const out: MasterTextStyles = {};
  if (!xml) return out;
  const styles = parseXml(xml).getElementsByTagName("p:txStyles")[0];
  if (!styles) return out;
  const lvl1Size = (styleTag: string): number | undefined => {
    const style = firstChildTag(styles, styleTag);
    const lvl1 = style && firstChildTag(style, "a:lvl1pPr");
    const sz = lvl1 && firstChildTag(lvl1, "a:defRPr")?.getAttribute("sz");
    return sz ? Number(sz) / 100 : undefined;
  };
  const title = lvl1Size("p:titleStyle");
  const body = lvl1Size("p:bodyStyle");
  const other = lvl1Size("p:otherStyle");
  if (title) out.title = title;
  if (body) out.body = body;
  if (other) out.other = other;
  return out;
}

const GEOM: Record<string, Shape["geom"]> = {
  ellipse: "ellipse",
  roundRect: "roundRect",
};

// Common preset geometries drawn as an SVG path in a normalized 100×100 box
// (preserveAspectRatio="none" stretches it to the shape). rect/ellipse/roundRect
// stay CSS (crisper, cheaper) and are intentionally absent here.
const PRESET_PATHS: Record<string, string> = {
  triangle: "M50 0 L100 100 L0 100 Z",
  rtTriangle: "M0 0 L0 100 L100 100 Z",
  diamond: "M50 0 L100 50 L50 100 L0 50 Z",
  parallelogram: "M25 0 L100 0 L75 100 L0 100 Z",
  trapezoid: "M25 0 L75 0 L100 100 L0 100 Z",
  pentagon: "M50 0 L100 38 L82 100 L18 100 L0 38 Z",
  hexagon: "M25 0 L75 0 L100 50 L75 100 L25 100 L0 50 Z",
  heptagon: "M50 0 L90 20 L100 60 L75 100 L25 100 L0 60 L10 20 Z",
  octagon: "M30 0 L70 0 L100 30 L100 70 L70 100 L30 100 L0 70 L0 30 Z",
  chevron: "M0 0 L75 0 L100 50 L75 100 L0 100 L25 50 Z",
  homePlate: "M0 0 L75 0 L100 50 L75 100 L0 100 Z",
  rightArrow: "M0 30 L60 30 L60 10 L100 50 L60 90 L60 70 L0 70 Z",
  leftArrow: "M100 30 L40 30 L40 10 L0 50 L40 90 L40 70 L100 70 Z",
  upArrow: "M30 100 L30 40 L10 40 L50 0 L90 40 L70 40 L70 100 Z",
  downArrow: "M30 0 L30 60 L10 60 L50 100 L90 60 L70 60 L70 0 Z",
  leftRightArrow: "M0 50 L25 25 L25 40 L75 40 L75 25 L100 50 L75 75 L75 60 L25 60 L25 75 Z",
  plus: "M35 0 L65 0 L65 35 L100 35 L100 65 L65 65 L65 100 L35 100 L35 65 L0 65 L0 35 L35 35 Z",
  star5:
    "M50 0 L61 35 L98 35 L68 57 L79 91 L50 70 L21 91 L32 57 L2 35 L39 35 Z",
  line: "M0 0 L100 100",
  straightConnector1: "M0 0 L100 100",
  bentConnector2: "M0 0 L100 0 L100 100",
  bentConnector3: "M0 0 L50 0 L50 100 L100 100",
};

/** SVG path for a preset that has no crisp CSS form, or null to keep CSS/box. */
function presetPath(prst: string | null | undefined): string | null {
  return prst ? (PRESET_PATHS[prst] ?? null) : null;
}

/** Map an a:prstDash value onto a coarse dash family the view can render. */
function dashFamily(val: string | null): ShapeLine["dash"] {
  if (!val || val === "solid") return undefined;
  if (val.includes("Dot") && val.includes("ash")) return "dashDot";
  if (val.toLowerCase().includes("dot")) return "dot";
  return "dash"; // dash, lgDash, sysDash, ...
}

/** a:ln → resolved outline (px width + CSS colour + dash), or undefined. */
function parseLine(spPr: Element | null, theme?: ThemeContext): ShapeLine | undefined {
  const ln = spPr && directChild(spPr, "a:ln");
  if (!ln) return undefined;
  if (directChild(ln, "a:noFill")) return undefined;
  const color = resolveColor(ln, theme);
  if (!color) return undefined;
  const w = Number(ln.getAttribute("w"));
  const dash = dashFamily(directChild(ln, "a:prstDash")?.getAttribute("val") ?? null);
  const line: ShapeLine = { color, width: w ? emuToPx(w) : 1 };
  if (dash) line.dash = dash;
  return line;
}

/** a:effectLst/a:outerShdw → a CSS box-shadow/drop-shadow value (px), or undefined. */
function parseShadow(spPr: Element | null, theme?: ThemeContext): string | undefined {
  const fx = spPr && directChild(spPr, "a:effectLst");
  const shdw = fx && directChild(fx, "a:outerShdw");
  if (!shdw) return undefined;
  const clr = firstClrChild(shdw);
  const color = (clr ? resolveClrEl(clr, theme) : undefined) ?? "rgba(0,0,0,0.4)";
  const dist = emuToPx(Number(shdw.getAttribute("dist")) || 0);
  const blur = emuToPx(Number(shdw.getAttribute("blurRad")) || 0);
  const dir = (Number(shdw.getAttribute("dir")) || 0) / 60000; // deg, clockwise
  const rad = (dir * Math.PI) / 180;
  const dx = Math.round(Math.cos(rad) * dist);
  const dy = Math.round(Math.sin(rad) * dist);
  return `${dx}px ${dy}px ${Math.round(blur)}px ${color}`;
}

/** a:custGeom → { d, w, h } SVG path in path space, or null if unusable. */
function parseCustGeom(spPr: Element): { d: string; w: number; h: number } | null {
  const geom = firstChildTag(spPr, "a:custGeom");
  const path = geom && firstChildTag(geom, "a:path");
  if (!path) return null;
  // Each a:path contour may declare its OWN w/h coordinate space. We take the
  // first path's w/h as the viewBox and normalize every later contour's coords
  // into that space (scale by firstW/thisW, firstH/thisH) so all contours
  // concatenate into one correct d — degrading to null only when NOTHING
  // drawable results. A later path with a zero w/h is skipped.
  const w = Number(path.getAttribute("w")) || 0;
  const h = Number(path.getAttribute("h")) || 0;
  const pathLst = path.parentElement;
  const paths = pathLst
    ? Array.from(pathLst.children).filter((c) => c.tagName === "a:path")
    : [path];
  const cmds: string[] = [];
  for (const p of paths) {
    const pw = Number(p.getAttribute("w")) || 0;
    const ph = Number(p.getAttribute("h")) || 0;
    // Skip a contour whose own space is unusable while the first's isn't; the
    // first path's zero w/h is caught by the nothing-drawable degrade below.
    if (p !== path && (!pw || !ph)) continue;
    const sx = p === path || pw === w ? 1 : w / pw;
    const sy = p === path || ph === h ? 1 : h / ph;
    const xOf = (el: Element | null) => Number(el?.getAttribute("x") ?? 0) * sx;
    const yOf = (el: Element | null) => Number(el?.getAttribute("y") ?? 0) * sy;
    const pt = (el: Element | null) => `${xOf(el)} ${yOf(el)}`;
    // Track the current + subpath-start points so a:arcTo (which gives radii and
    // sweep angles, not an endpoint) can be turned into an absolute SVG arc.
    let curX = 0, curY = 0, startX = 0, startY = 0;
    for (const seg of Array.from(p.children)) {
      const pts = Array.from(seg.getElementsByTagName("a:pt"));
      switch (seg.tagName) {
        case "a:moveTo":
          cmds.push(`M ${pt(pts[0])}`);
          curX = startX = xOf(pts[0]); curY = startY = yOf(pts[0]);
          break;
        case "a:lnTo":
          cmds.push(`L ${pt(pts[0])}`);
          curX = xOf(pts[0]); curY = yOf(pts[0]);
          break;
        case "a:cubicBezTo":
          cmds.push(`C ${pt(pts[0])} ${pt(pts[1])} ${pt(pts[2])}`);
          curX = xOf(pts[2]); curY = yOf(pts[2]);
          break;
        case "a:quadBezTo":
          cmds.push(`Q ${pt(pts[0])} ${pt(pts[1])}`);
          curX = xOf(pts[1]); curY = yOf(pts[1]);
          break;
        case "a:arcTo": {
          const wR = (Number(seg.getAttribute("wR")) || 0) * sx;
          const hR = (Number(seg.getAttribute("hR")) || 0) * sy;
          const st = ((Number(seg.getAttribute("stAng")) || 0) / 60000) * (Math.PI / 180);
          const sw = ((Number(seg.getAttribute("swAng")) || 0) / 60000) * (Math.PI / 180);
          // The current point sits at angle st on the ellipse; back out the centre.
          const cx = curX - wR * Math.cos(st);
          const cy = curY - hR * Math.sin(st);
          const ex = cx + wR * Math.cos(st + sw);
          const ey = cy + hR * Math.sin(st + sw);
          const large = Math.abs(sw) > Math.PI ? 1 : 0;
          const sweep = sw > 0 ? 1 : 0; // OOXML +ve = clockwise = SVG sweep 1 (y-down)
          cmds.push(`A ${wR} ${hR} 0 ${large} ${sweep} ${ex} ${ey}`);
          curX = ex; curY = ey;
          break;
        }
        case "a:close":
          cmds.push("Z");
          curX = startX; curY = startY;
          break;
      }
    }
  }
  if (!cmds.length || !w || !h) return null;
  return { d: cmds.join(" "), w, h };
}

function parseSp(
  sp: Element,
  theme?: ThemeContext,
  ctx: Ctx = IDENTITY,
  inherit?: InheritContext,
): Shape | null {
  const paragraphs = paragraphsOf(sp, theme);
  const spPr = firstChildTag(sp, "p:spPr");
  const fill = resolveColor(spPr, theme);
  const gradient = fill ? undefined : resolveGradient(spPr, theme);
  const fillImageEmbedId = spPr ? (blipEmbedId(spPr) ?? undefined) : undefined;
  const line = parseLine(spPr, theme);
  const shadow = parseShadow(spPr, theme);
  const custom = spPr ? parseCustGeom(spPr) : null;
  const prst = spPr
    ? firstChildTag(spPr, "a:prstGeom")?.getAttribute("prst")
    : null;
  const preset = custom ? null : presetPath(prst);

  // Placeholder inheritance: a shape without its own xfrm borrows geometry and
  // anchor from the matching layout/master placeholder (type/idx), which is what
  // keeps titles/bodies in their authored positions instead of naive flow.
  const phEl = placeholderEl(sp);
  const phType = phEl?.getAttribute("type") ?? null;
  const phIdx = placeholderIdx(sp);
  const inherited =
    matchPlaceholder(inherit?.layout, phType, phIdx) ??
    matchPlaceholder(inherit?.master, phType, phIdx);
  const own = xfrmOf(sp);
  const box = own.x === null && inherited?.box ? inherited.box : own;
  const anchor: Shape["anchor"] =
    bodyAnchor(sp) ?? inherited?.anchor ?? "top";
  const defaultSizePt = phEl
    ? inherit?.textStyles?.[phFamily(phType)]
    : undefined;

  // Drop only shapes with nothing to draw.
  if (
    paragraphs.length === 0 &&
    !fill &&
    !gradient &&
    !fillImageEmbedId &&
    !line &&
    !custom &&
    !preset
  ) {
    return null;
  }

  const isTitle = phFamily(phType) === "title";
  const base = {
    ...applyCtx(ctx, box),
    ...rotFlipOf(sp),
    paragraphs,
    fill,
    gradient,
    fillImageEmbedId,
    line,
    shadow,
    isTitle,
    anchor,
    defaultSizePt,
  };

  if (custom) {
    return { kind: "custgeom", ...base, geomPath: custom.d, pathW: custom.w, pathH: custom.h };
  }
  if (preset) {
    // Preset drawn as a stretched SVG path; text still renders over it.
    return { kind: "shape", ...base, geomPath: preset, pathW: 100, pathH: 100 };
  }

  const geom: Shape["geom"] | undefined = prst ? (GEOM[prst] ?? "rect") : undefined;
  return { kind: "shape", ...base, geom };
}

function parsePic(pic: Element, ctx: Ctx = IDENTITY): Shape | null {
  const embedId = relAttr(firstChildTag(pic, "a:blip") ?? pic, "embed");
  return {
    kind: "image",
    ...applyCtx(ctx, xfrmOf(pic)),
    ...rotFlipOf(pic),
    paragraphs: [],
    embedId: embedId ?? undefined,
    isTitle: false,
    anchor: "top",
  };
}

/** p:graphicFrame uses p:xfrm (a:off/a:ext), no chOff/chExt. */
function frameXfrm(frame: Element): Pick<Shape, "x" | "y" | "w" | "h"> {
  const xfrm = firstChildTag(frame, "p:xfrm");
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

function parseTableCell(tc: Element, theme?: ThemeContext): TableCell {
  const tcPr = firstChildTag(tc, "a:tcPr");
  const anchorAttr = tcPr?.getAttribute("anchor");
  return {
    paragraphs: paragraphsOf(tc, theme),
    fill: resolveColor(tcPr, theme),
    gridSpan: Number(tc.getAttribute("gridSpan")) || 1,
    rowSpan: Number(tc.getAttribute("rowSpan")) || 1,
    hMerge: tc.getAttribute("hMerge") === "1",
    vMerge: tc.getAttribute("vMerge") === "1",
    anchor: anchorAttr === "ctr" ? "center" : anchorAttr === "b" ? "bottom" : "top",
  };
}

function parseGraphicFrame(
  frame: Element,
  theme?: ThemeContext,
  ctx: Ctx = IDENTITY,
): Shape | null {
  const tbl = firstChildTag(frame, "a:tbl");
  if (!tbl) {
    // Charts / SmartArt / OLE aren't rendered, but a labelled placeholder box at
    // the right position keeps the layout honest instead of silently dropping it.
    const uri = firstChildTag(frame, "a:graphicData")?.getAttribute("uri") ?? "";
    const label = uri.includes("chart")
      ? "Chart"
      : uri.includes("diagram") || uri.includes("smartArt")
        ? "SmartArt"
        : uri.includes("ole") || uri.includes("oleObject")
          ? "Embedded object"
          : "Embedded content";
    const box = applyCtx(ctx, frameXfrm(frame));
    if (box.x === null) return null; // no geometry → nothing to place
    return {
      kind: "placeholder",
      ...box,
      paragraphs: [],
      isTitle: false,
      anchor: "center",
      placeholder: label,
    };
  }
  const grid = firstChildTag(tbl, "a:tblGrid");
  const colWidths = grid
    ? Array.from(grid.getElementsByTagName("a:gridCol")).map((c) =>
        emuToPx(Number(c.getAttribute("w") ?? 0)))
    : [];
  const rows: TableRow[] = Array.from(tbl.getElementsByTagName("a:tr")).map((tr) => ({
    height: tr.getAttribute("h") ? emuToPx(Number(tr.getAttribute("h"))) : null,
    cells: Array.from(tr.children)
      .filter((c) => c.tagName === "a:tc")
      .map((tc) => parseTableCell(tc, theme)),
  }));
  return {
    kind: "table",
    ...applyCtx(ctx, frameXfrm(frame)),
    paragraphs: [],
    isTitle: false,
    anchor: "top",
    table: { colWidths, rows },
  };
}

export function parseSlide(
  slideXml: string,
  theme?: ThemeContext,
  inherit?: InheritContext,
): ParsedSlide {
  const doc = parseXml(slideXml);
  const tree =
    doc.getElementsByTagName("p:spTree")[0] ?? doc.documentElement;
  const shapes: Shape[] = [];
  // Walk direct children in authored order so z-order is preserved. Group
  // shapes (p:grpSp) carry an a:xfrm that maps their child coordinate space into
  // slide space; we compose that onto the running ctx so nested groups stack.
  const walk = (parent: Element, ctx: Ctx) => {
    for (const el of Array.from(parent.children)) {
      if (el.tagName === "p:sp" || el.tagName === "p:cxnSp") {
        // Connectors (p:cxnSp) carry the same spPr/prstGeom as shapes; the preset
        // path table turns straight/bent connectors into drawable lines.
        const s = parseSp(el, theme, ctx, inherit);
        if (s) shapes.push(s);
      } else if (el.tagName === "p:pic") {
        const s = parsePic(el, ctx);
        if (s) shapes.push(s);
      } else if (el.tagName === "p:graphicFrame") {
        const s = parseGraphicFrame(el, theme, ctx);
        if (s) shapes.push(s);
      } else if (el.tagName === "p:grpSp") {
        walk(el, groupCtx(el, ctx));
      }
    }
  };
  walk(tree, IDENTITY);
  const bg = doc.getElementsByTagName("p:bg")[0];
  const background = bg ? backgroundFromEl(bg, theme) : undefined;
  return background ? { shapes, background } : { shapes };
}

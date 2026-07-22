// Pure OOXML parsing for the .pptx previewer. A .pptx is a zip of DrawingML
// XML; the component owns the zip/media I/O, this module owns turning slide XML
// into a positioned, styled model the view can lay out absolutely. Kept free of
// Svelte/jszip so it runs under vitest with only a DOMParser (happy-dom).
// Colour/theme/font and text-style inheritance live in ./pptx-style.

import {
  applyRPr,
  cssFontStack,
  directChild,
  emuToPx,
  firstChildTag,
  firstClrChild,
  levelStyles,
  parseTheme,
  parseXml,
  phFamily,
  ptToPx,
  relAttr,
  resolveClrEl,
  resolveColor,
  resolveGradient,
  resolveParagraphStyle,
  resolveRunStyle,
  resolveStyleRef,
  resolveTypeface,
  type LevelStyles,
  type StyleChain,
  type TextAlign,
  type TextStyle,
  type ThemeContext,
} from "./pptx-style";

export {
  cssFontStack,
  emuToPx,
  parseTheme,
  ptToPx,
  resolveColor,
  resolveGradient,
  type ThemeContext,
};

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
  /** a:highlight — a background colour behind just this run. */
  highlight?: string;
  /** a:rPr@spc tracking in px (negative tightens); absent when zero. */
  letterSpacingPx?: number;
}

export interface Paragraph {
  runs: TextRun[];
  align?: TextAlign;
  level: number;
  bullet: boolean;
  /**
   * a:lnSpc as a unitless CSS line-height. Percentage line spacing is relative
   * to each run's own size, so it must stay unitless: a paragraph that mixes an
   * 18pt heading run with a 12pt body run gets two different line boxes, as it
   * does in PowerPoint.
   */
  lineHeight?: number;
  /** a:lnSpc given as exact points (a:spcPts) → a fixed px line box. */
  lineHeightPx?: number;
  /** a:spcBef / a:spcAft resolved against the paragraph's own font size. */
  spaceBeforePx?: number;
  spaceAfterPx?: number;
  /** a:marL / a:indent in px; indent is negative for a hanging bullet. */
  marginLeftPx?: number;
  indentPx?: number;
  /** Font size of the line box, which is all an empty paragraph contributes. */
  sizePt?: number;
  /** The bullet glyph or list number to draw, when `bullet` is true. */
  bulletText?: string;
  /** a:buFont for the glyph; bullets routinely use a different family. */
  bulletFont?: string;
  /** a:buClr, when the bullet is coloured independently of the text. */
  bulletColor?: string;
  /** Bullet size in px, resolved from a:buSzPct/a:buSzPts against the run size. */
  bulletSizePx?: number;
}

export type ShapeKind =
  | "text"
  | "image"
  | "shape"
  | "custgeom"
  | "table"
  | "placeholder";

/** Which part a shape came from — decides which rels resolve its media. */
export type ShapeSource = "slide" | "layout" | "master";

/** Resolved outline: CSS colour + width in px, plus optional dash pattern. */
export interface ShapeLine {
  color: string;
  width: number;
  /** Preset dash style (a:prstDash) mapped to a coarse family, when dashed. */
  dash?: "dash" | "dot" | "dashDot";
}

/** Per-edge cell outlines; an absent edge draws nothing at all. */
export interface CellBorders {
  l?: ShapeLine;
  r?: ShapeLine;
  t?: ShapeLine;
  b?: ShapeLine;
  /** a:lnTlToBr / a:lnBlToTr, drawn as an overlaid diagonal. */
  tlToBr?: ShapeLine;
  blToTr?: ShapeLine;
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
  /** a:lnL/R/T/B, plus whatever the table style contributes beneath them. */
  borders?: CellBorders;
  /** a:tcPr marL/marR/marT/marB in px (OOXML defaults when unset). */
  margins?: Insets;
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

/** a:tblPr's banding/emphasis flags — which table-style parts apply. */
interface TableFlags {
  firstRow: boolean;
  lastRow: boolean;
  firstCol: boolean;
  lastCol: boolean;
  bandRow: boolean;
  bandCol: boolean;
}

/** a:bodyPr text insets in px. */
export interface Insets {
  l: number;
  t: number;
  r: number;
  b: number;
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
  /** Resolved a:bodyPr text insets, defaulted per the OOXML spec. */
  insets?: Insets;
  /** Clockwise rotation in degrees (from a:xfrm rot, 60000ths of a degree). */
  rot?: number;
  /** Horizontal / vertical flip flags (from a:xfrm flipH/flipV). */
  flipH?: boolean;
  flipV?: boolean;
  /** a:blipFill/a:stretch → the picture fills its frame instead of letterboxing. */
  stretch?: boolean;
  /** a:srcRect crop, as the fraction of the source inset from each edge. */
  crop?: Insets;
  /** a:tile → the picture repeats at its natural size instead of stretching. */
  tile?: boolean;
  /** roundRect corner radius as a fraction of the shorter side (a:avLst adj). */
  cornerRadius?: number;
  /** a:bodyPr vert — rotated text flow inside the shape. */
  vert?: "vert" | "vert270";
  /** a:normAutofit: the view may measure and shrink the text to fit the box. */
  shrinkToFit?: boolean;
  /** Absent means the slide's own spTree. */
  source?: ShapeSource;
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

/** A placeholder's inheritable geometry/anchor/styling, from a layout or master. */
export interface Placeholder {
  type: string | null;
  idx: number | null;
  box: Pick<Shape, "x" | "y" | "w" | "h"> | null;
  anchor?: Shape["anchor"];
  /** The placeholder's a:lstStyle level overrides, for the inheritance chain. */
  levels?: LevelStyles;
  /** Its a:bodyPr, so insets/autofit inherit when the slide shape omits them. */
  bodyPr?: Element | null;
}

/** A master's p:txStyles: nine level styles per placeholder family. */
export interface MasterTextStyles {
  title: LevelStyles;
  body: LevelStyles;
  other: LevelStyles;
}

/** Inheritance context threaded from the slide's layout + master into parseSlide. */
export interface InheritContext {
  layout?: Placeholder[];
  master?: Placeholder[];
  textStyles?: MasterTextStyles;
  /** presentation.xml's p:defaultTextStyle — the floor for plain text boxes. */
  defaultTextStyle?: LevelStyles;
  /** Layout/master XML, so their own shapes render beneath the slide's. */
  layoutXml?: string;
  masterXml?: string;
  /** ppt/tableStyles.xml, indexed by styleId (see parseTableStyles). */
  tableStyles?: Map<string, Element>;
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

/** presentation.xml's p:defaultTextStyle levels, the floor for plain text boxes. */
export function parseDefaultTextStyle(
  presentationXml: string | undefined,
): LevelStyles {
  if (!presentationXml) return levelStyles(null);
  const doc = parseXml(presentationXml);
  return levelStyles(doc.getElementsByTagName("p:defaultTextStyle")[0] ?? null);
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

// --- Text ---------------------------------------------------------------------

// PowerPoint's "single" line spacing is the font's natural line box, which for
// the Office families is ≈1.2× the point size; a:lnSpc percentages scale that.
const SINGLE_LINE = 1.2;

function runFrom(style: TextStyle, text: string): TextRun {
  const run: TextRun = { text };
  if (style.bold) run.bold = true;
  if (style.italic) run.italic = true;
  if (style.underline) run.underline = true;
  if (style.color) run.color = style.color;
  if (style.sizePt) run.sizePt = style.sizePt;
  if (style.font) run.font = style.font;
  if (style.highlight) run.highlight = style.highlight;
  if (style.spcPt) run.letterSpacingPx = ptToPx(style.spcPt);
  return run;
}

/** 1 → "a", 26 → "z", 27 → "aa" — a:buAutoNum's alphabetic sequence. */
function alphaNum(n: number): string {
  let out = "";
  let v = Math.max(1, n);
  while (v > 0) {
    const r = (v - 1) % 26;
    out = String.fromCharCode(97 + r) + out;
    v = Math.floor((v - 1) / 26);
  }
  return out;
}

const ROMAN: [number, string][] = [
  [1000, "m"], [900, "cm"], [500, "d"], [400, "cd"], [100, "c"], [90, "xc"],
  [50, "l"], [40, "xl"], [10, "x"], [9, "ix"], [5, "v"], [4, "iv"], [1, "i"],
];

function romanNum(n: number): string {
  let v = Math.max(1, Math.min(3999, n));
  let out = "";
  for (const [val, sym] of ROMAN) {
    while (v >= val) {
      out += sym;
      v -= val;
    }
  }
  return out;
}

/**
 * a:buAutoNum's type names are `<sequence><punctuation>`, e.g. alphaLcParenR →
 * "a)". Anything unrecognised degrades to a bare arabic number rather than a
 * missing bullet.
 */
function autoNumText(type: string, n: number): string {
  const body = type.startsWith("alphaLc")
    ? alphaNum(n)
    : type.startsWith("alphaUc")
      ? alphaNum(n).toUpperCase()
      : type.startsWith("romanLc")
        ? romanNum(n)
        : type.startsWith("romanUc")
          ? romanNum(n).toUpperCase()
          : String(n);
  if (type.endsWith("ParenBoth")) return `(${body})`;
  if (type.endsWith("ParenR")) return `${body})`;
  if (type.endsWith("Period")) return `${body}.`;
  if (type.endsWith("Dash")) return `- ${body}`;
  return body;
}

function parseParagraph(p: Element, chain: StyleChain): { para: Paragraph; style: TextStyle } {
  const pPr = directChild(p, "a:pPr");
  const level = Number(pPr?.getAttribute("lvl") ?? 0) || 0;
  const paraStyle = resolveParagraphStyle(chain, level, pPr);

  const runs: TextRun[] = [];
  // a:r runs and a:fld fields (slide numbers, dates) both carry a:t text.
  for (const child of Array.from(p.children)) {
    const tag = child.tagName;
    if (tag === "a:r" || tag === "a:fld") {
      const text = Array.from(child.getElementsByTagName("a:t"))
        .map((t) => t.textContent ?? "")
        .join("");
      runs.push(runFrom(resolveRunStyle(chain, paraStyle, directChild(child, "a:rPr")), text));
    } else if (tag === "a:br") {
      runs.push({ text: "\n" });
    }
  }

  // An empty paragraph still occupies a line; a:endParaRPr sizes it.
  const endStyle = { ...paraStyle };
  if (!runs.length) {
    applyRPr(endStyle, directChild(p, "a:endParaRPr"), chain.theme);
    if (chain.fontScale && endStyle.sizePt) endStyle.sizePt *= chain.fontScale;
  }
  const sizePt = runs.length
    ? runs.reduce((m, r) => Math.max(m, r.sizePt ?? 0), 0) || paraStyle.sizePt
    : endStyle.sizePt;

  const para: Paragraph = {
    runs,
    align: paraStyle.align,
    level,
    // Bullets only exist where the chain says so; titles are never bulleted.
    // An empty paragraph never draws one either — PowerPoint keeps its line box
    // but no glyph, and a bullet-only paragraph would collapse to zero height
    // because the marker is positioned out of flow.
    bullet: paraStyle.bullet === true && runs.some((r) => r.text.length > 0),
  };
  if (sizePt) para.sizePt = sizePt;

  // Percent-based metrics resolve against the paragraph's own size; a:normAutofit
  // shrinks line spacing on top of that.
  const basePx = ptToPx(sizePt ?? 18);
  if (paraStyle.linePts !== undefined) {
    para.lineHeightPx = ptToPx(paraStyle.linePts);
  } else {
    const pct = Math.max(0.1, (paraStyle.linePct ?? 1) - (chain.lnSpcReduction ?? 0));
    para.lineHeight = pct * SINGLE_LINE;
  }

  if (paraStyle.spcBefPts !== undefined) para.spaceBeforePx = ptToPx(paraStyle.spcBefPts);
  else if (paraStyle.spcBefPct !== undefined) para.spaceBeforePx = basePx * paraStyle.spcBefPct;
  if (paraStyle.spcAftPts !== undefined) para.spaceAfterPx = ptToPx(paraStyle.spcAftPts);
  else if (paraStyle.spcAftPct !== undefined) para.spaceAfterPx = basePx * paraStyle.spcAftPct;

  // marL/indent of exactly 0 are meaningful overrides of an inherited hanging
  // indent, so they must survive as 0 rather than be dropped as falsy.
  if (paraStyle.marL !== undefined) para.marginLeftPx = paraStyle.marL;
  if (paraStyle.indent !== undefined) para.indentPx = paraStyle.indent;

  if (para.bullet) {
    if (paraStyle.buChar) para.bulletText = paraStyle.buChar;
    if (paraStyle.buFont) para.bulletFont = paraStyle.buFont;
    if (paraStyle.buClr) para.bulletColor = paraStyle.buClr;
    if (paraStyle.buSzPts !== undefined) para.bulletSizePx = ptToPx(paraStyle.buSzPts);
    else if (paraStyle.buSzPct !== undefined) para.bulletSizePx = basePx * paraStyle.buSzPct;
  }
  return { para, style: paraStyle };
}

function paragraphsOf(el: Element, chain: StyleChain): Paragraph[] {
  // Shapes carry p:txBody; table cells carry a:txBody.
  const body = firstChildTag(el, "p:txBody") ?? firstChildTag(el, "a:txBody");
  if (!body) return [];
  const parsed = Array.from(body.getElementsByTagName("a:p")).map((p) =>
    parseParagraph(p, chain));
  numberAutoLists(parsed);
  return parsed.map((x) => x.para);
}

/**
 * Fill in a:buAutoNum bullet text. Numbering is per level and per text body:
 * a deeper level starts its own count and is discarded once the list returns to
 * a shallower one, and any non-numbered paragraph at a level ends that level's
 * run (so a following numbered paragraph restarts at its own startAt).
 */
function numberAutoLists(parsed: { para: Paragraph; style: TextStyle }[]): void {
  const counters: (number | undefined)[] = [];
  for (const { para, style } of parsed) {
    const lvl = para.level;
    for (let i = lvl + 1; i < counters.length; i++) counters[i] = undefined;
    if (!para.bullet || !style.buAutoNum) {
      counters[lvl] = undefined;
      continue;
    }
    const start = style.buStartAt ?? 1;
    counters[lvl] = counters[lvl] === undefined ? start : (counters[lvl] as number) + 1;
    para.bulletText = autoNumText(style.buAutoNum, counters[lvl] as number);
  }
}

/** True when a text body would draw nothing at all (no glyphs in any run). */
function hasVisibleText(paragraphs: Paragraph[]): boolean {
  return paragraphs.some((p) => p.runs.some((r) => r.text.trim().length > 0));
}

function placeholderEl(sp: Element): Element | null {
  return firstChildTag(sp, "p:ph");
}

function placeholderIdx(sp: Element): number | null {
  const idx = placeholderEl(sp)?.getAttribute("idx");
  return idx != null ? Number(idx) : null;
}

function bodyPrOf(el: Element): Element | null {
  return firstChildTag(el, "a:bodyPr");
}

function bodyAnchor(el: Element): Shape["anchor"] | undefined {
  const a = bodyPrOf(el)?.getAttribute("anchor");
  return a === "ctr" ? "center" : a === "b" ? "bottom" : a === "t" ? "top" : undefined;
}

// OOXML a:bodyPr inset defaults, in EMU.
const DEFAULT_INSETS: Insets = {
  l: emuToPx(91440),
  t: emuToPx(45720),
  r: emuToPx(91440),
  b: emuToPx(45720),
};

/** Text insets, taking each side from the first bodyPr in the chain that sets it. */
function resolveInsets(chain: (Element | null | undefined)[]): Insets {
  const pick = (attr: string, fallback: number): number => {
    for (const bp of chain) {
      const raw = bp?.getAttribute(attr);
      if (raw != null && raw !== "") {
        const n = Number(raw);
        if (Number.isFinite(n)) return emuToPx(n);
      }
    }
    return fallback;
  };
  return {
    l: pick("lIns", DEFAULT_INSETS.l),
    t: pick("tIns", DEFAULT_INSETS.t),
    r: pick("rIns", DEFAULT_INSETS.r),
    b: pick("bIns", DEFAULT_INSETS.b),
  };
}

/**
 * a:normAutofit from the first bodyPr in the chain that carries one. `shrink`
 * records that PowerPoint was in shrink-to-fit mode at all: the stored
 * fontScale was computed for the authored font, so with a substituted (wider)
 * one the view still has to measure and shrink further. Shapes WITHOUT
 * normAutofit are left to overflow, exactly as PowerPoint lets them.
 */
function autofit(chain: (Element | null | undefined)[]): {
  fontScale?: number;
  lnSpcReduction?: number;
  shrink?: boolean;
} {
  for (const bp of chain) {
    const fit = bp && directChild(bp, "a:normAutofit");
    if (!fit) continue;
    const fs = Number(fit.getAttribute("fontScale"));
    const ls = Number(fit.getAttribute("lnSpcReduction"));
    return {
      fontScale: fs ? fs / 100000 : undefined,
      lnSpcReduction: ls ? ls / 100000 : undefined,
      shrink: true,
    };
  }
  return {};
}

/**
 * Read a layout/master part's placeholders (type/idx + geometry + anchor + the
 * a:lstStyle and a:bodyPr a slide shape inherits from). Walks the spTree.
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
      levels: levelStyles(firstChildTag(sp, "a:lstStyle")),
      bodyPr: bodyPrOf(sp),
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

/** A master's p:txStyles, as nine level styles per placeholder family. */
export function parseMasterTextStyles(xml: string | undefined): MasterTextStyles {
  const empty = (): MasterTextStyles => ({
    title: levelStyles(null),
    body: levelStyles(null),
    other: levelStyles(null),
  });
  if (!xml) return empty();
  const styles = parseXml(xml).getElementsByTagName("p:txStyles")[0];
  if (!styles) return empty();
  return {
    title: levelStyles(directChild(styles, "p:titleStyle")),
    body: levelStyles(directChild(styles, "p:bodyStyle")),
    other: levelStyles(directChild(styles, "p:otherStyle")),
  };
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

/** One a:ln element → resolved outline (px width + CSS colour + dash), or undefined. */
function lineFrom(
  ln: Element | null,
  theme?: ThemeContext,
  phClr?: string,
): ShapeLine | undefined {
  if (!ln) return undefined;
  if (directChild(ln, "a:noFill")) return undefined;
  const color = resolveColor(ln, theme, phClr);
  if (!color) return undefined;
  const w = Number(ln.getAttribute("w"));
  const dash = dashFamily(directChild(ln, "a:prstDash")?.getAttribute("val") ?? null);
  const line: ShapeLine = { color, width: w ? emuToPx(w) : 1 };
  if (dash) line.dash = dash;
  return line;
}

/** a:effectLst/a:outerShdw → a CSS box-shadow/drop-shadow value (px), or undefined. */
function shadowFrom(
  fx: Element | null,
  theme?: ThemeContext,
  phClr?: string,
): string | undefined {
  const shdw = fx && directChild(fx, "a:outerShdw");
  if (!shdw) return undefined;
  const clr = firstClrChild(shdw);
  const color = (clr ? resolveClrEl(clr, theme, phClr) : undefined) ?? "rgba(0,0,0,0.4)";
  const dist = emuToPx(Number(shdw.getAttribute("dist")) || 0);
  const blur = emuToPx(Number(shdw.getAttribute("blurRad")) || 0);
  const dir = (Number(shdw.getAttribute("dir")) || 0) / 60000; // deg, clockwise
  const rad = (dir * Math.PI) / 180;
  const dx = Math.round(Math.cos(rad) * dist);
  const dy = Math.round(Math.sin(rad) * dist);
  return `${dx}px ${dy}px ${Math.round(blur)}px ${color}`;
}

/** What a shape's p:style contributes before its own spPr overrides anything. */
interface ThemeStyle {
  fill?: string;
  gradient?: string;
  line?: ShapeLine;
  shadow?: string;
  /** a:fontRef → the shape's default text colour and family. */
  text?: TextStyle;
}

/**
 * Resolve p:style's a:fillRef/a:lnRef/a:effectRef/a:fontRef against the theme's
 * a:fmtScheme. Shapes drawn from a gallery style carry no explicit fill or line
 * at all, so without this they render as nothing.
 */
function parseThemeStyle(sp: Element, theme?: ThemeContext): ThemeStyle | undefined {
  const style = firstChildTag(sp, "p:style");
  if (!style) return undefined;
  const fmt = theme?.format;
  const out: ThemeStyle = {};

  const fillRef = resolveStyleRef(directChild(style, "a:fillRef"), fmt?.fills, theme);
  if (fillRef?.el) {
    out.fill = resolveColor(fillRef.el, theme, fillRef.phClr);
    if (!out.fill) out.gradient = resolveGradient(fillRef.el, theme, fillRef.phClr);
  }
  const lnRef = resolveStyleRef(directChild(style, "a:lnRef"), fmt?.lines, theme);
  if (lnRef?.el) out.line = lineFrom(lnRef.el, theme, lnRef.phClr);
  const fxRef = resolveStyleRef(directChild(style, "a:effectRef"), fmt?.effects, theme);
  if (fxRef?.el) out.shadow = shadowFrom(fxRef.el, theme, fxRef.phClr);

  // a:fontRef indexes the font scheme by name ("major"/"minor"), not by number,
  // and its colour child is the shape's default text colour.
  const fontRef = directChild(style, "a:fontRef");
  if (fontRef) {
    const text: TextStyle = {};
    const clr = firstClrChild(fontRef);
    const color = clr ? resolveClrEl(clr, theme) : undefined;
    if (color) text.color = color;
    const idx = fontRef.getAttribute("idx");
    const font = resolveTypeface(idx === "major" ? "+mj-lt" : "+mn-lt", theme?.fonts);
    if (font && idx !== "none") text.font = font;
    if (text.color || text.font) out.text = text;
  }
  return out;
}

/** True when spPr states a fill of its own (including an explicit a:noFill). */
function hasExplicitFill(spPr: Element | null): boolean {
  if (!spPr) return false;
  for (const c of Array.from(spPr.children)) {
    if (
      c.tagName === "a:noFill" || c.tagName === "a:solidFill" ||
      c.tagName === "a:gradFill" || c.tagName === "a:blipFill" ||
      c.tagName === "a:pattFill" || c.tagName === "a:grpFill"
    ) {
      return true;
    }
  }
  return false;
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

/** Per-part options for one pass over a spTree. */
interface WalkOpts {
  theme?: ThemeContext;
  inherit?: InheritContext;
  source: ShapeSource;
  /** Layout/master pass: placeholder keys the slide fills itself, to skip. */
  slidePhKeys?: Set<string>;
  /** ppt/tableStyles.xml, indexed by styleId, for a:tblPr's tableStyleId. */
  tableStyles?: Map<string, Element>;
}

/** Identity of a placeholder for slide-vs-layout matching. */
function phKey(type: string | null, idx: number | null): string {
  return `${phFamily(type)}#${idx ?? ""}`;
}

/**
 * Build the level-style chain for one text body, lowest precedence first:
 * presentation defaults (plain text boxes) or the master's family txStyles,
 * then the master placeholder's lstStyle, the layout's, and the shape's own.
 */
function chainFor(
  sp: Element,
  phType: string | null,
  isPlaceholder: boolean,
  layoutPh: Placeholder | undefined,
  masterPh: Placeholder | undefined,
  opts: WalkOpts,
  fit: { fontScale?: number; lnSpcReduction?: number },
  base?: TextStyle,
): StyleChain {
  const styles = opts.inherit?.textStyles;
  const sources: LevelStyles[] = [];
  if (isPlaceholder) {
    const fam = phFamily(phType);
    if (styles) sources.push(styles[fam]);
  } else if (opts.inherit?.defaultTextStyle) {
    sources.push(opts.inherit.defaultTextStyle);
  } else if (styles) {
    sources.push(styles.other);
  }
  if (masterPh?.levels) sources.push(masterPh.levels);
  if (layoutPh?.levels) sources.push(layoutPh.levels);
  const own = firstChildTag(sp, "a:lstStyle");
  if (own) sources.push(levelStyles(own));
  return { sources, base, theme: opts.theme, ...fit };
}

function parseSp(sp: Element, ctx: Ctx, opts: WalkOpts): Shape | null {
  const theme = opts.theme;
  const spPr = firstChildTag(sp, "p:spPr");
  const themeStyle = parseThemeStyle(sp, theme);
  // spPr always wins where it speaks — including an explicit a:noFill or an
  // <a:ln><a:noFill/>, which must suppress the p:style reference rather than
  // fall through to it.
  const ownFill = hasExplicitFill(spPr);
  const fill = resolveColor(spPr, theme) ?? (ownFill ? undefined : themeStyle?.fill);
  const gradient = fill
    ? undefined
    : (resolveGradient(spPr, theme) ?? (ownFill ? undefined : themeStyle?.gradient));
  const fillImageEmbedId = spPr ? (blipEmbedId(spPr) ?? undefined) : undefined;
  const ownLn = spPr && directChild(spPr, "a:ln");
  const line = ownLn ? lineFrom(ownLn, theme) : themeStyle?.line;
  const ownFx = spPr && directChild(spPr, "a:effectLst");
  const shadow = ownFx ? shadowFrom(ownFx, theme) : themeStyle?.shadow;
  const custom = spPr ? parseCustGeom(spPr) : null;
  const prst = spPr
    ? firstChildTag(spPr, "a:prstGeom")?.getAttribute("prst")
    : null;
  const preset = custom ? null : presetPath(prst);

  // Placeholder inheritance: a shape without its own xfrm borrows geometry,
  // anchor, insets and text styling from the matching layout/master placeholder
  // (type/idx), which is what keeps titles/bodies in their authored positions.
  const phEl = placeholderEl(sp);
  const phType = phEl?.getAttribute("type") ?? null;
  const phIdx = placeholderIdx(sp);
  // Only a slide shape inherits; a layout/master shape IS the template, so
  // matching it against itself would hand it another placeholder's geometry.
  const fromTemplate = opts.source !== "slide";
  const inheritable = phEl && !fromTemplate;
  const layoutPh = inheritable ? matchPlaceholder(opts.inherit?.layout, phType, phIdx) : undefined;
  const masterPh = inheritable ? matchPlaceholder(opts.inherit?.master, phType, phIdx) : undefined;

  // A layout/master placeholder the slide fills itself is drawn by the slide's
  // own copy; and prompt text ("Click to edit…") is never rendered on a slide.
  if (fromTemplate && phEl && opts.slidePhKeys?.has(phKey(phType, phIdx))) return null;

  const bodyPrChain = [bodyPrOf(sp), layoutPh?.bodyPr, masterPh?.bodyPr];
  const fit = autofit(bodyPrChain);
  const chain = chainFor(sp, phType, !!phEl, layoutPh, masterPh, opts, fit, themeStyle?.text);
  const paragraphs = fromTemplate && phEl ? [] : paragraphsOf(sp, chain);

  const own = xfrmOf(sp);
  const inheritedBox = layoutPh?.box ?? masterPh?.box ?? null;
  const box = own.x === null && inheritedBox ? inheritedBox : own;
  const anchor: Shape["anchor"] =
    bodyAnchor(sp) ?? layoutPh?.anchor ?? masterPh?.anchor ?? "top";
  const defaultSizePt = phEl
    ? resolveParagraphStyle(chain, 0, null).sizePt
    : undefined;

  // Drop shapes with nothing to draw — including a text body whose runs are all
  // whitespace, which would otherwise leave an invisible box swallowing clicks.
  if (
    !hasVisibleText(paragraphs) &&
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
    insets: resolveInsets(bodyPrChain),
    defaultSizePt,
    ...(fit.shrink && paragraphs.length ? { shrinkToFit: true } : {}),
    ...vertOf(bodyPrChain),
    ...(opts.source === "slide" ? {} : { source: opts.source }),
  };

  if (custom) {
    return { kind: "custgeom", ...base, geomPath: custom.d, pathW: custom.w, pathH: custom.h };
  }
  if (preset) {
    // Preset drawn as a stretched SVG path; text still renders over it.
    return { kind: "shape", ...base, geomPath: preset, pathW: 100, pathH: 100 };
  }

  const geom: Shape["geom"] | undefined = prst ? (GEOM[prst] ?? "rect") : undefined;
  if (geom === "roundRect") {
    return { kind: "shape", ...base, geom, cornerRadius: roundRectAdj(spPr) };
  }
  return { kind: "shape", ...base, geom };
}

// roundRect's `adj` guide is the corner radius as thousandths of a percent of
// the shape's SHORTER side; 16667 (=1/6) is the preset's own default.
const ROUND_RECT_DEFAULT_ADJ = 16667;

function roundRectAdj(spPr: Element | null): number {
  const geom = spPr && firstChildTag(spPr, "a:prstGeom");
  const gd = geom
    ? Array.from(geom.getElementsByTagName("a:gd")).find(
        (g) => (g.getAttribute("name") ?? "adj") === "adj",
      )
    : undefined;
  const raw = gd?.getAttribute("fmla")?.replace(/^val\s+/, "") ?? "";
  const val = raw === "" ? NaN : Number(raw);
  // The guide is clamped to [0, 50%] — beyond that the corners would overlap.
  const pct = (Number.isFinite(val) ? val : ROUND_RECT_DEFAULT_ADJ) / 100000;
  return Math.max(0, Math.min(0.5, pct));
}

/** a:bodyPr vert, from the first bodyPr in the chain that sets a rotated flow. */
function vertOf(chain: (Element | null | undefined)[]): { vert?: Shape["vert"] } {
  for (const bp of chain) {
    const v = bp?.getAttribute("vert");
    if (!v || v === "horz") continue;
    // eaVert/mongolianVert/wordArtVert* all stack characters; the two that just
    // rotate the whole run are the ones a CSS writing-mode can reproduce.
    return { vert: v === "vert270" ? "vert270" : "vert" };
  }
  return {};
}

/**
 * a:srcRect → the fraction of the source cropped away at each edge. Attributes
 * are thousandths of a percent and may be negative (padding rather than crop).
 * Returns undefined for the ubiquitous empty `<a:srcRect/>`, which crops nothing.
 */
function parseSrcRect(blipFill: Element | null): Insets | undefined {
  const rect = blipFill && directChild(blipFill, "a:srcRect");
  if (!rect) return undefined;
  const side = (a: string) => {
    const n = Number(rect.getAttribute(a));
    return Number.isFinite(n) ? n / 100000 : 0;
  };
  const crop = { l: side("l"), t: side("t"), r: side("r"), b: side("b") };
  if (!crop.l && !crop.t && !crop.r && !crop.b) return undefined;
  // A crop that leaves nothing of an axis is unrenderable; ignore it.
  if (crop.l + crop.r >= 1 || crop.t + crop.b >= 1) return undefined;
  return crop;
}

function parsePic(pic: Element, ctx: Ctx, opts: WalkOpts): Shape | null {
  const embedId = relAttr(firstChildTag(pic, "a:blip") ?? pic, "embed");
  const blipFill = firstChildTag(pic, "p:blipFill") ?? firstChildTag(pic, "a:blipFill");
  // a:stretch/a:fillRect means "fill the frame", which is what PowerPoint does
  // for every inserted picture — letterboxing it would leave the frame's own
  // background showing through wherever the aspect ratios disagree. a:tile is
  // the other half of that choice: repeat at natural size instead.
  const stretch = !!firstChildTag(pic, "a:stretch");
  const tile = !stretch && !!(blipFill && directChild(blipFill, "a:tile"));
  const crop = parseSrcRect(blipFill);
  return {
    kind: "image",
    ...applyCtx(ctx, xfrmOf(pic)),
    ...rotFlipOf(pic),
    paragraphs: [],
    embedId: embedId ?? undefined,
    isTitle: false,
    anchor: "top",
    ...(stretch ? { stretch: true } : {}),
    ...(tile ? { tile: true } : {}),
    ...(crop ? { crop } : {}),
    ...(opts.source === "slide" ? {} : { source: opts.source }),
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

/** ppt/tableStyles.xml → styleId → its a:tblStyle element. */
export function parseTableStyles(xml: string | undefined): Map<string, Element> {
  const out = new Map<string, Element>();
  if (!xml) return out;
  for (const st of Array.from(parseXml(xml).getElementsByTagName("a:tblStyle"))) {
    const id = st.getAttribute("styleId");
    if (id) out.set(id, st);
  }
  return out;
}

/** OOXML a:tcPr cell margin defaults, in EMU. */
const DEFAULT_CELL_MARGINS: Insets = {
  l: emuToPx(91440),
  t: emuToPx(45720),
  r: emuToPx(91440),
  b: emuToPx(45720),
};

/** What a table style contributes to one cell, beneath its own a:tcPr. */
interface CellStyle {
  fill?: string;
  borders: CellBorders;
  text?: TextStyle;
}

/** Which a:tcBdr edge each cell side takes, given the cell's position. */
type EdgeMap = Partial<Record<keyof CellBorders, string>>;

function applyTablePart(
  acc: CellStyle,
  part: Element | null,
  edges: EdgeMap,
  theme?: ThemeContext,
): void {
  if (!part) return;
  const tcStyle = directChild(part, "a:tcStyle");
  if (tcStyle) {
    const fill = directChild(tcStyle, "a:fill");
    const color = fill ? resolveColor(fill, theme) : undefined;
    if (color) acc.fill = color;
    const bdr = directChild(tcStyle, "a:tcBdr");
    // An empty a:tcBdr (band parts routinely carry one) overrides nothing.
    if (bdr) {
      for (const [side, tag] of Object.entries(edges) as [keyof CellBorders, string][]) {
        const edge = directChild(bdr, tag);
        if (edge) acc.borders[side] = lineFrom(directChild(edge, "a:ln"), theme);
      }
    }
  }
  const tx = directChild(part, "a:tcTxStyle");
  if (tx) {
    const text: TextStyle = { ...acc.text };
    const b = tx.getAttribute("b");
    if (b != null) text.bold = b === "on" || b === "1";
    const i = tx.getAttribute("i");
    if (i != null) text.italic = i === "on" || i === "1";
    const clr = firstClrChild(tx);
    const color = clr ? resolveClrEl(clr, theme) : undefined;
    if (color) text.color = color;
    acc.text = text;
  }
}

/**
 * Resolve the table style for one cell at (row, col). Parts apply in increasing
 * precedence: whole table, then column/row banding, then the emphasised first
 * and last column/row — each only when a:tblPr turned that flag on.
 */
function cellStyleFor(
  style: Element | undefined,
  flags: TableFlags,
  row: number,
  col: number,
  rowCount: number,
  colCount: number,
  theme?: ThemeContext,
): CellStyle {
  const acc: CellStyle = { borders: {} };
  if (!style) return acc;
  const firstR = flags.firstRow && row === 0;
  const lastR = flags.lastRow && row === rowCount - 1;
  const firstC = flags.firstCol && col === 0;
  const lastC = flags.lastCol && col === colCount - 1;
  // Interior edges come from insideH/insideV; the table's outer edges from the
  // matching outer edge of whichever part is being applied.
  const edges: EdgeMap = {
    l: col === 0 ? "a:left" : "a:insideV",
    r: col === colCount - 1 ? "a:right" : "a:insideV",
    t: row === 0 ? "a:top" : "a:insideH",
    b: row === rowCount - 1 ? "a:bottom" : "a:insideH",
  };
  const part = (tag: string) => directChild(style, tag);
  applyTablePart(acc, part("a:wholeTbl"), edges, theme);
  if (flags.bandCol && !firstC && !lastC) {
    const band = (col - (flags.firstCol ? 1 : 0)) % 2 === 0 ? "a:band1V" : "a:band2V";
    applyTablePart(acc, part(band), edges, theme);
  }
  if (flags.bandRow && !firstR && !lastR) {
    const band = (row - (flags.firstRow ? 1 : 0)) % 2 === 0 ? "a:band1H" : "a:band2H";
    applyTablePart(acc, part(band), edges, theme);
  }
  if (firstC) applyTablePart(acc, part("a:firstCol"), edges, theme);
  if (lastC) applyTablePart(acc, part("a:lastCol"), edges, theme);
  if (firstR) applyTablePart(acc, part("a:firstRow"), edges, theme);
  if (lastR) applyTablePart(acc, part("a:lastRow"), edges, theme);
  return acc;
}

/** a:tcPr's own a:lnL/R/T/B (+ diagonals), layered over the table style's. */
function cellBorders(tcPr: Element | null, base: CellBorders, theme?: ThemeContext): CellBorders {
  const out: CellBorders = { ...base };
  if (tcPr) {
    const sides: [keyof CellBorders, string][] = [
      ["l", "a:lnL"], ["r", "a:lnR"], ["t", "a:lnT"], ["b", "a:lnB"],
      ["tlToBr", "a:lnTlToBr"], ["blToTr", "a:lnBlToTr"],
    ];
    for (const [side, tag] of sides) {
      const ln = directChild(tcPr, tag);
      if (ln) out[side] = lineFrom(ln, theme);
    }
  }
  for (const k of Object.keys(out) as (keyof CellBorders)[]) {
    if (!out[k]) delete out[k];
  }
  return out;
}

function parseTableCell(tc: Element, chain: StyleChain, style: CellStyle): TableCell {
  const tcPr = firstChildTag(tc, "a:tcPr");
  const anchorAttr = tcPr?.getAttribute("anchor");
  const margin = (attr: string, fallback: number): number => {
    const raw = tcPr?.getAttribute(attr);
    const n = raw == null || raw === "" ? NaN : Number(raw);
    return Number.isFinite(n) ? emuToPx(n) : fallback;
  };
  const borders = cellBorders(tcPr, style.borders, chain.theme);
  const cell: TableCell = {
    paragraphs: paragraphsOf(tc, style.text ? { ...chain, base: style.text } : chain),
    fill: resolveColor(tcPr, chain.theme) ?? style.fill,
    gridSpan: Number(tc.getAttribute("gridSpan")) || 1,
    rowSpan: Number(tc.getAttribute("rowSpan")) || 1,
    hMerge: tc.getAttribute("hMerge") === "1",
    vMerge: tc.getAttribute("vMerge") === "1",
    anchor: anchorAttr === "ctr" ? "center" : anchorAttr === "b" ? "bottom" : "top",
    margins: {
      l: margin("marL", DEFAULT_CELL_MARGINS.l),
      t: margin("marT", DEFAULT_CELL_MARGINS.t),
      r: margin("marR", DEFAULT_CELL_MARGINS.r),
      b: margin("marB", DEFAULT_CELL_MARGINS.b),
    },
  };
  if (Object.keys(borders).length) cell.borders = borders;
  return cell;
}

function parseGraphicFrame(frame: Element, ctx: Ctx, opts: WalkOpts): Shape | null {
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
  // Table text takes no master txStyles; only the theme (for schemeClr runs).
  const cellChain: StyleChain = { sources: [], theme: opts.theme };
  const tblPr = directChild(tbl, "a:tblPr");
  const flag = (a: string) => tblPr?.getAttribute(a) === "1";
  const flags: TableFlags = {
    firstRow: flag("firstRow"),
    lastRow: flag("lastRow"),
    firstCol: flag("firstCol"),
    lastCol: flag("lastCol"),
    bandRow: flag("bandRow"),
    bandCol: flag("bandCol"),
  };
  const styleId = tblPr
    ? (firstChildTag(tblPr, "a:tableStyleId")?.textContent ?? "").trim()
    : "";
  const tblStyle = styleId ? opts.tableStyles?.get(styleId) : undefined;
  const trs = Array.from(tbl.getElementsByTagName("a:tr"));
  const rows: TableRow[] = trs.map((tr, ri) => {
    const tcs = Array.from(tr.children).filter((c) => c.tagName === "a:tc");
    return {
      height: tr.getAttribute("h") ? emuToPx(Number(tr.getAttribute("h"))) : null,
      cells: tcs.map((tc, ci) =>
        parseTableCell(
          tc,
          cellChain,
          cellStyleFor(tblStyle, flags, ri, ci, trs.length, tcs.length, opts.theme),
        )),
    };
  });
  return {
    kind: "table",
    ...applyCtx(ctx, frameXfrm(frame)),
    paragraphs: [],
    isTitle: false,
    anchor: "top",
    table: { colWidths, rows },
  };
}

/** Walk one part's spTree in authored order, composing group transforms. */
function walkTree(tree: Element, opts: WalkOpts): Shape[] {
  const shapes: Shape[] = [];
  const walk = (parent: Element, ctx: Ctx) => {
    for (const el of Array.from(parent.children)) {
      if (el.tagName === "p:sp" || el.tagName === "p:cxnSp") {
        // Connectors (p:cxnSp) carry the same spPr/prstGeom as shapes; the preset
        // path table turns straight/bent connectors into drawable lines.
        const s = parseSp(el, ctx, opts);
        if (s) shapes.push(s);
      } else if (el.tagName === "p:pic") {
        const s = parsePic(el, ctx, opts);
        if (s) shapes.push(s);
      } else if (el.tagName === "p:graphicFrame") {
        const s = parseGraphicFrame(el, ctx, opts);
        if (s) shapes.push(s);
      } else if (el.tagName === "p:grpSp") {
        walk(el, groupCtx(el, ctx));
      }
    }
  };
  walk(tree, IDENTITY);
  return shapes;
}

function spTreeOf(doc: Document): Element {
  return doc.getElementsByTagName("p:spTree")[0] ?? doc.documentElement;
}

// A deck's slides share a handful of layouts and one or two masters, but
// parseSlide is called per slide, so without this every slide would re-parse the
// same template XML. Bounded because the cache outlives a single deck.
const TEMPLATE_CACHE_MAX = 24;
const templateDocs = new Map<string, Document>();

function parseTemplate(xml: string): Document {
  const hit = templateDocs.get(xml);
  if (hit) return hit;
  const doc = parseXml(xml);
  if (templateDocs.size >= TEMPLATE_CACHE_MAX) {
    templateDocs.delete(templateDocs.keys().next().value as string);
  }
  templateDocs.set(xml, doc);
  return doc;
}

/** Every placeholder the slide itself supplies, so templates don't redraw them. */
function slidePlaceholderKeys(tree: Element): Set<string> {
  const keys = new Set<string>();
  for (const sp of Array.from(tree.getElementsByTagName("p:sp"))) {
    const ph = placeholderEl(sp);
    if (!ph) continue;
    const idx = ph.getAttribute("idx");
    keys.add(phKey(ph.getAttribute("type"), idx != null ? Number(idx) : null));
  }
  return keys;
}

/**
 * p:sld@show="0" — a slide hidden from the show. PowerPoint skips these when
 * presenting and when exporting to PDF, so the previewer must not number or
 * render them either.
 */
export function isHiddenSlide(slideXml: string | undefined): boolean {
  if (!slideXml) return false;
  // Cheap prefilter: the attribute only ever appears on the root p:sld element.
  if (!slideXml.includes("show=")) return false;
  return parseXml(slideXml).documentElement.getAttribute("show") === "0";
}

export function parseSlide(
  slideXml: string,
  theme?: ThemeContext,
  inherit?: InheritContext,
): ParsedSlide {
  const doc = parseXml(slideXml);
  const tree = spTreeOf(doc);
  const slidePhKeys = slidePlaceholderKeys(tree);
  const tableStyles = inherit?.tableStyles;

  // A slide's showMasterSp="0" hides everything the layout contributes; the
  // layout's own flag then gates the master's shapes beneath it.
  const showLayout = doc.documentElement.getAttribute("showMasterSp") !== "0";
  const shapes: Shape[] = [];
  if (showLayout && (inherit?.layoutXml || inherit?.masterXml)) {
    const layoutDoc = inherit.layoutXml ? parseTemplate(inherit.layoutXml) : null;
    const showMaster =
      !layoutDoc || layoutDoc.documentElement.getAttribute("showMasterSp") !== "0";
    if (showMaster && inherit.masterXml) {
      shapes.push(
        ...walkTree(spTreeOf(parseTemplate(inherit.masterXml)), {
          theme, inherit, source: "master", slidePhKeys, tableStyles,
        }),
      );
    }
    if (layoutDoc) {
      shapes.push(
        ...walkTree(spTreeOf(layoutDoc), {
          theme, inherit, source: "layout", slidePhKeys, tableStyles,
        }),
      );
    }
  }
  shapes.push(...walkTree(tree, { theme, inherit, source: "slide", tableStyles }));

  const bg = doc.getElementsByTagName("p:bg")[0];
  const background = bg ? backgroundFromEl(bg, theme) : undefined;
  return background ? { shapes, background } : { shapes };
}

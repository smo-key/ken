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

function solidFillColor(el: Element | null): string | undefined {
  if (!el) return undefined;
  const fill = firstChildTag(el, "a:solidFill");
  const srgb = fill && firstChildTag(fill, "a:srgbClr");
  const val = srgb?.getAttribute("val");
  // Theme colours (a:schemeClr) need the theme part to resolve; we leave those
  // to inherit rather than guess wrong.
  return val ? `#${val}` : undefined;
}

const ALIGN: Record<string, Paragraph["align"]> = {
  l: "left",
  ctr: "center",
  r: "right",
  just: "justify",
};

function parseRun(r: Element): TextRun {
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
    const color = solidFillColor(rPr);
    if (color) run.color = color;
    const font = firstChildTag(rPr, "a:latin")?.getAttribute("typeface");
    if (font) run.font = font;
  }
  return run;
}

function parseParagraph(p: Element): Paragraph {
  const runs: TextRun[] = [];
  // a:r runs and a:fld fields (slide numbers, dates) both carry a:t text.
  for (const child of Array.from(p.children)) {
    const tag = child.tagName;
    if (tag === "a:r" || tag === "a:fld") runs.push(parseRun(child));
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

function paragraphsOf(sp: Element): Paragraph[] {
  const body = firstChildTag(sp, "p:txBody");
  if (!body) return [];
  return Array.from(body.getElementsByTagName("a:p"))
    .map(parseParagraph)
    .filter((p) => p.runs.some((r) => r.text.trim().length > 0));
}

function placeholderType(sp: Element): string | null {
  return firstChildTag(sp, "p:ph")?.getAttribute("type") ?? null;
}

const GEOM: Record<string, Shape["geom"]> = {
  ellipse: "ellipse",
  roundRect: "roundRect",
};

function parseSp(sp: Element): Shape | null {
  const paragraphs = paragraphsOf(sp);
  const spPr = firstChildTag(sp, "p:spPr");
  const fill = solidFillColor(spPr);
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

export function parseSlide(slideXml: string): ParsedSlide {
  const doc = parseXml(slideXml);
  const tree =
    doc.getElementsByTagName("p:spTree")[0] ?? doc.documentElement;
  const shapes: Shape[] = [];
  // Walk direct children in authored order so z-order is preserved. Group
  // shapes (p:grpSp) are flattened one level; nested groups are uncommon.
  const walk = (parent: Element) => {
    for (const el of Array.from(parent.children)) {
      if (el.tagName === "p:sp") {
        const s = parseSp(el);
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

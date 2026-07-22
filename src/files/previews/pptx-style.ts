// DrawingML styling primitives for the .pptx previewer: XML access, colour and
// theme resolution, the theme font scheme, and the run/paragraph style
// inheritance chain. Split out of pptx.ts so that file stays about shapes and
// geometry. Depends only on DOMParser, so vitest (happy-dom) can run it.

// OOXML measures in EMU: 914400 per inch, and CSS is 96px per inch, so one CSS
// pixel is exactly 9525 EMU.
const EMU_PER_PX = 9525;

export function emuToPx(emu: number): number {
  return emu / EMU_PER_PX;
}

/** Points → CSS px (72pt/inch, 96px/inch). */
export function ptToPx(pt: number): number {
  return pt * (96 / 72);
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

export function parseXml(xml: string): Document {
  return parser.parseFromString(normalizeNsAttrs(xml), "application/xml");
}

/** Read a relationships-namespace attribute (r:embed → r_embed after normalize). */
export function relAttr(el: Element, local: string): string | null {
  return (
    el.getAttribute(`r_${local}`) ??
    Array.from(el.attributes).find((a) => a.name.endsWith(`_${local}`))?.value ??
    null
  );
}

export function firstChildTag(parent: Element, tag: string): Element | null {
  return parent.getElementsByTagName(tag)[0] ?? null;
}

/** First DIRECT child with the given tag (unlike firstChildTag's descendant search). */
export function directChild(parent: Element, tag: string): Element | null {
  for (const c of Array.from(parent.children)) if (c.tagName === tag) return c;
  return null;
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

// a:tint / a:shade are defined on *linear* RGB, not on the sRGB byte values.
// Lerping toward white in sRGB (the obvious reading) comes out visibly too
// saturated: accent 0057B8 at tint 40% is CBD1E6 in PowerPoint, but 99BCE3 if
// mixed in sRGB. Verified against PowerPoint's own render of a banded table.
function srgbToLinear(c: number): number {
  const v = c / 255;
  return v <= 0.04045 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
}

function linearToSrgb(v: number): number {
  const c = v <= 0.0031308 ? v * 12.92 : 1.055 * Math.pow(Math.max(0, v), 1 / 2.4) - 0.055;
  return c * 255;
}

function hex2(n: number): string {
  return Math.max(0, Math.min(255, Math.round(n))).toString(16).toUpperCase().padStart(2, "0");
}

/**
 * Apply DrawingML colour-transform child elements (in document order) to a base
 * RGB. Returns the final CSS colour string (#RRGGBB, or rgba(...) if alpha set).
 * frac = val/100000 (val is in thousandths).
 */
function applyColorTransforms(clr: Element, rgb: [number, number, number]): string {
  let [r, g, b] = rgb;
  let alpha = 1;
  for (const t of Array.from(clr.children)) {
    const frac = Number(t.getAttribute("val")) / 100000;
    switch (t.tagName) {
      case "a:shade": {
        const f = (c: number) => linearToSrgb(srgbToLinear(c) * frac);
        r = f(r); g = f(g); b = f(b);
        break;
      }
      case "a:tint": {
        const f = (c: number) => linearToSrgb(srgbToLinear(c) * frac + (1 - frac));
        r = f(r); g = f(g); b = f(b);
        break;
      }
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

/** Theme's a:fontScheme, keyed the way `+mj-lt` / `+mn-ea` style refs address it. */
export interface FontScheme {
  "mj-lt"?: string;
  "mj-ea"?: string;
  "mj-cs"?: string;
  "mn-lt"?: string;
  "mn-ea"?: string;
  "mn-cs"?: string;
}

/**
 * The theme's a:fmtScheme lists, which p:style's a:fillRef/a:lnRef/a:effectRef
 * index into. Kept as raw elements: each ref supplies its own `phClr`, so the
 * same list entry resolves to a different colour per shape.
 */
export interface FormatScheme {
  fills: Element[];
  lines: Element[];
  effects: Element[];
}

export interface ThemeContext {
  colors: Record<string, string>;
  fonts: FontScheme;
  format?: FormatScheme;
}

function readFontGroup(
  scheme: Element | null,
  tag: string,
  prefix: "mj" | "mn",
  out: FontScheme,
): void {
  const grp = scheme && directChild(scheme, tag);
  if (!grp) return;
  for (const [child, slot] of [["a:latin", "lt"], ["a:ea", "ea"], ["a:cs", "cs"]] as const) {
    const face = directChild(grp, child)?.getAttribute("typeface");
    // Empty typeface means "no override" — keep the slot unset so callers fall
    // through to their own default rather than emitting font-family:''.
    if (face) out[`${prefix}-${slot}` as keyof FontScheme] = face;
  }
}

export function parseTheme(
  themeXml: string | undefined,
  masterXml: string | undefined,
): ThemeContext {
  const colors: Record<string, string> = {};
  const fonts: FontScheme = {};
  if (!themeXml) return { colors, fonts };
  const root = parseXml(themeXml).documentElement;
  const fontScheme = firstChildTag(root, "a:fontScheme");
  readFontGroup(fontScheme, "a:majorFont", "mj", fonts);
  readFontGroup(fontScheme, "a:minorFont", "mn", fonts);
  const format = parseFormatScheme(firstChildTag(root, "a:fmtScheme"));
  const scheme = firstChildTag(root, "a:clrScheme");
  if (!scheme) return { colors, fonts, format };
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
  return { colors, fonts, format };
}

function parseFormatScheme(fmt: Element | null): FormatScheme | undefined {
  if (!fmt) return undefined;
  const list = (tag: string): Element[] => {
    const lst = directChild(fmt, tag);
    return lst ? Array.from(lst.children) : [];
  };
  return {
    fills: list("a:fillStyleLst"),
    lines: list("a:lnStyleLst"),
    // Each a:effectStyle wraps the a:effectLst callers actually want.
    effects: list("a:effectStyleLst").map((es) => directChild(es, "a:effectLst") ?? es),
  };
}

/** A CSS colour from resolveClrEl back to a bare "RRGGBB", for phClr substitution. */
function cssToHex6(css: string | undefined): string | undefined {
  if (!css) return undefined;
  if (css.startsWith("#")) return css.slice(1, 7).toUpperCase();
  const m = css.match(/rgba?\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)/);
  return m ? `${hex2(+m[1])}${hex2(+m[2])}${hex2(+m[3])}` : undefined;
}

/** One resolved p:style reference: the fmtScheme entry plus its colour override. */
export interface StyleRef {
  /** The referenced a:solidFill/a:gradFill, a:ln or a:effectLst; null for "none". */
  el: Element | null;
  /** The ref's own colour, substituted wherever the entry says schemeClr="phClr". */
  phClr?: string;
}

/**
 * Resolve an a:fillRef / a:lnRef / a:effectRef against the theme's matching
 * fmtScheme list. `idx` is 1-based, and `idx="0"` explicitly means no fill/line
 * — which is why a missing entry and a zero index both yield `el: null` while
 * still carrying the ref's phClr (a:fontRef needs the colour alone).
 */
export function resolveStyleRef(
  ref: Element | null,
  list: Element[] | undefined,
  theme?: ThemeContext,
): StyleRef | undefined {
  if (!ref) return undefined;
  const clr = firstClrChild(ref);
  const phClr = clr ? cssToHex6(resolveClrEl(clr, theme)) : undefined;
  const idx = Number(ref.getAttribute("idx"));
  if (!Number.isFinite(idx) || idx < 1) return { el: null, phClr };
  return { el: list?.[idx - 1] ?? null, phClr };
}

/**
 * Resolve a typeface reference. `+mn-lt` / `+mj-ea` etc. address the theme's
 * font scheme; anything else is a literal family name.
 */
export function resolveTypeface(
  typeface: string | null | undefined,
  fonts?: FontScheme,
): string | undefined {
  if (!typeface) return undefined;
  if (!typeface.startsWith("+")) return typeface;
  return fonts?.[typeface.slice(1) as keyof FontScheme];
}

// Office's default families are almost never installed outside Windows/Office,
// so a bare `font-family:'Calibri'` silently falls back to the browser default
// and every metric shifts. Map the common ones onto substitutes with similar
// x-height and advance widths that macOS/Linux actually ship.
//
// Carlito (Calibri), Arimo (Arial) and Tinos (Times New Roman) are
// *metric-compatible* clones — identical advance widths, so text wraps exactly
// where PowerPoint wraps rather than a word early or late. All three are bundled
// via src/app.css, so unlike the rest of this table they are always present; the
// authored family still wins ahead of them when the real font is installed.
const FONT_SUBSTITUTES: Record<string, string> = {
  calibri: "Carlito, 'Helvetica Neue', Helvetica, Arial, sans-serif",
  "calibri light": "Carlito, 'Helvetica Neue', Helvetica, Arial, sans-serif",
  aptos: "'Helvetica Neue', Helvetica, Arial, sans-serif",
  "aptos display": "'Helvetica Neue', Helvetica, Arial, sans-serif",
  "aptos narrow": "'Helvetica Neue', Helvetica, Arial, sans-serif",
  corbel: "'Helvetica Neue', Helvetica, Arial, sans-serif",
  candara: "Optima, 'Helvetica Neue', Helvetica, sans-serif",
  tahoma: "Verdana, Geneva, sans-serif",
  "segoe ui": "'Helvetica Neue', Helvetica, Arial, sans-serif",
  "trebuchet ms": "'Helvetica Neue', Helvetica, Arial, sans-serif",
  arial: "Arimo, Helvetica, 'Helvetica Neue', sans-serif",
  "arial narrow": "'Helvetica Neue', Helvetica, sans-serif",
  helvetica: "'Helvetica Neue', Arial, Arimo, sans-serif",
  verdana: "Geneva, 'DejaVu Sans', sans-serif",
  cambria: "Georgia, 'Times New Roman', serif",
  constantia: "Georgia, 'Times New Roman', serif",
  "times new roman": "Tinos, Times, Georgia, serif",
  times: "Tinos, 'Times New Roman', Georgia, serif",
  garamond: "'EB Garamond', Georgia, serif",
  georgia: "'Times New Roman', Times, serif",
  "book antiqua": "Palatino, 'Palatino Linotype', Georgia, serif",
  "courier new": "Courier, ui-monospace, monospace",
  consolas: "'SF Mono', Menlo, ui-monospace, monospace",
  wingdings: "sans-serif",
};

const SERIF_HINT = /serif|roman|georgia|garamond|cambria|book|times|minion|caslon|didot/i;

// Office names a *face* ("ATT Aleck Sans Medium", "Calibri Light"), but macOS and
// fontconfig expose it as a family plus a style, so the full name matches only
// where the platform happens to index full names too. Strip a trailing style word
// so the base family can catch the fallback instead of Helvetica.
const STYLE_SUFFIX =
  /[ -](?:thin|extralight|ultralight|light|book|regular|normal|medium|demi|demibold|semibold|semi ?bold|bold|extrabold|ultrabold|heavy|black|italic|oblique|condensed|cond|narrow|display|text|caption|subhead)$/i;

/** Drop a trailing weight/style word, repeatedly ("Foo Sans SemiBold Italic"). */
function baseFamily(family: string): string | undefined {
  let name = family.trim();
  for (let i = 0; i < 3 && STYLE_SUFFIX.test(name); i++) {
    name = name.replace(STYLE_SUFFIX, "").trim();
  }
  return name && name.toLowerCase() !== family.trim().toLowerCase() ? name : undefined;
}

/**
 * A resolved family name → a CSS font stack with metric-reasonable fallbacks.
 * The authored face comes first (it renders exactly right where installed), then
 * its base family, then the substitutes. Deliberately no synthesized
 * `font-weight` for the suffix: a faux-bold face is wider than the real one and
 * makes the fit worse than plain regular.
 */
export function cssFontStack(family: string | undefined): string | undefined {
  if (!family) return undefined;
  const name = family.trim();
  const base = baseFamily(name);
  const sub =
    FONT_SUBSTITUTES[name.toLowerCase()] ??
    (base ? FONT_SUBSTITUTES[base.toLowerCase()] : undefined);
  const tail = sub ?? (SERIF_HINT.test(name) ? "Georgia, serif" : "Helvetica, Arial, sans-serif");
  // Quote the authored family so multi-word names survive; the substitutes are
  // already written with their own quoting.
  const head = base ? `'${quote(name)}', '${quote(base)}'` : `'${quote(name)}'`;
  return `${head}, ${tail}`;
}

function quote(s: string): string {
  return s.replace(/'/g, "");
}

/** Resolve a single colour element (a:srgbClr | a:schemeClr | a:sysClr | a:prstClr)
 *  to CSS, applying its colour transforms. Returns undefined if unresolvable.
 *  `phClr` (bare "RRGGBB") supplies the placeholder colour a theme fmtScheme
 *  entry refers to as `<a:schemeClr val="phClr"/>`. */
export function resolveClrEl(
  el: Element,
  theme?: ThemeContext,
  phClr?: string,
): string | undefined {
  const fromHex = (hex: string) =>
    applyColorTransforms(el, [
      parseInt(hex.slice(0, 2), 16),
      parseInt(hex.slice(2, 4), 16),
      parseInt(hex.slice(4, 6), 16),
    ]);
  if (el.tagName === "a:srgbClr") {
    const val = el.getAttribute("val");
    return val ? fromHex(val) : undefined;
  }
  if (el.tagName === "a:schemeClr") {
    const name = el.getAttribute("val");
    const hex = name === "phClr" ? phClr : name ? theme?.colors[name] : undefined;
    return hex ? fromHex(hex) : undefined;
  }
  if (el.tagName === "a:sysClr") {
    const last = el.getAttribute("lastClr");
    return last ? fromHex(last) : undefined;
  }
  return undefined;
}

/** First colour child (srgb/scheme/sys) of an element, if any. */
export function firstClrChild(parent: Element): Element | null {
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

/**
 * Resolve a solid colour off `clrParent` (a spPr, ln, rPr, tcPr, or a bare
 * a:solidFill) to a CSS colour, applying colour transforms.
 *
 * IMPORTANT: uses DIRECT-child lookup, not firstChildTag's descendant search —
 * otherwise a shape with `<a:noFill/>` but an outlined `<a:ln><a:solidFill>` would
 * wrongly report the line's colour as its fill (these text-less shapes are now
 * rendered, not dropped, so the bug would be visible).
 */
export function resolveColor(
  clrParent: Element | null,
  theme?: ThemeContext,
  phClr?: string,
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
  return clr ? resolveClrEl(clr, theme, phClr) : undefined;
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
  phClr?: string,
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
    const css = clr ? resolveClrEl(clr, theme, phClr) : undefined;
    if (!css) continue;
    const pos = Number(gs.getAttribute("pos")) / 1000; // thousandths of a % → %
    stops.push(`${css} ${Number.isFinite(pos) ? pos : 0}%`);
  }
  if (stops.length < 2) return undefined;
  if (directChild(grad, "a:path")) {
    // Radial / rectangular: approximate all as a centred radial gradient.
    return `radial-gradient(circle, ${stops.join(", ")})`;
  }
  const lin = directChild(grad, "a:lin");
  const ang = lin ? Number(lin.getAttribute("ang")) / 60000 : 90;
  const cssAng = (((Number.isFinite(ang) ? ang : 90) + 90) % 360 + 360) % 360;
  return `linear-gradient(${cssAng}deg, ${stops.join(", ")})`;
}

// --- The text style inheritance chain ----------------------------------------

export type TextAlign = "left" | "center" | "right" | "justify";

const ALIGN: Record<string, TextAlign> = {
  l: "left",
  ctr: "center",
  r: "right",
  just: "justify",
};

/** Normalize placeholder types so title/ctrTitle (and the body family) match. */
export function phFamily(type: string | null): "title" | "body" | "other" {
  if (type === "title" || type === "ctrTitle") return "title";
  if (type == null || type === "body" || type === "subTitle" || type === "obj") {
    return "body";
  }
  return "other";
}

/**
 * Everything a paragraph + its runs inherit, accumulated down the chain. Sizes
 * stay in points and spacing stays in its authored form (percent or points)
 * until the caller knows the final font size to resolve percentages against.
 */
export interface TextStyle {
  sizePt?: number;
  bold?: boolean;
  italic?: boolean;
  underline?: boolean;
  color?: string;
  font?: string;
  align?: TextAlign;
  /** a:lnSpc as a multiple of single spacing (spcPct) … */
  linePct?: number;
  /** … or as an exact leading in points (spcPts). */
  linePts?: number;
  spcBefPct?: number;
  spcBefPts?: number;
  spcAftPct?: number;
  spcAftPts?: number;
  /** a:highlight — run background colour. */
  highlight?: string;
  /** a:rPr@spc character spacing (tracking) in points; negative tightens. */
  spcPt?: number;
  /** a:marL / a:indent in px. */
  marL?: number;
  indent?: number;
  bullet?: boolean;
  /** a:buChar's glyph, when the bullet is a literal character. */
  buChar?: string;
  /** a:buAutoNum type (arabicPeriod, alphaLcParenR, …) for a numbered list. */
  buAutoNum?: string;
  /** a:buAutoNum@startAt, 1 unless the list restarts at another number. */
  buStartAt?: number;
  /** a:buFont, resolved through the theme font scheme. */
  buFont?: string;
  /** a:buClr. */
  buClr?: string;
  /** a:buSzPct as a fraction of the run size … */
  buSzPct?: number;
  /** … or a:buSzPts as an absolute size in points. */
  buSzPts?: number;
}

function num(el: Element, attr: string): number | undefined {
  const raw = el.getAttribute(attr);
  if (raw == null || raw === "") return undefined;
  const n = Number(raw);
  return Number.isFinite(n) ? n : undefined;
}

/** a:lnSpc/a:spcBef/a:spcAft carry either a:spcPct (thousandths of %) or a:spcPts (100ths pt). */
function spacing(el: Element | null): { pct?: number; pts?: number } {
  if (!el) return {};
  const pct = directChild(el, "a:spcPct");
  if (pct) {
    const v = num(pct, "val");
    if (v !== undefined) return { pct: v / 100000 };
  }
  const pts = directChild(el, "a:spcPts");
  if (pts) {
    const v = num(pts, "val");
    if (v !== undefined) return { pts: v / 100 };
  }
  return {};
}

/** Merge an a:defRPr / a:rPr / a:endParaRPr element's run properties into `acc`. */
export function applyRPr(acc: TextStyle, rPr: Element | null, theme?: ThemeContext): void {
  if (!rPr) return;
  const sz = num(rPr, "sz");
  if (sz) acc.sizePt = sz / 100; // sz is in hundredths of a point
  // Tracking, in hundredths of a point. Office themes routinely tighten display
  // faces by -0.6pt or more; ignoring it makes a title measurably wider than
  // PowerPoint's and wrap a line early.
  const spc = num(rPr, "spc");
  if (spc !== undefined) acc.spcPt = spc / 100;
  // b/i/u are tri-state: an explicit "0" must *clear* an inherited true.
  const b = rPr.getAttribute("b");
  if (b != null) acc.bold = b === "1";
  const i = rPr.getAttribute("i");
  if (i != null) acc.italic = i === "1";
  const u = rPr.getAttribute("u");
  if (u != null) acc.underline = u !== "none";
  const color = resolveColor(rPr, theme);
  if (color) acc.color = color;
  const hl = directChild(rPr, "a:highlight");
  if (hl) {
    const clr = firstClrChild(hl);
    acc.highlight = clr ? resolveClrEl(clr, theme) : undefined;
  }
  const font = resolveTypeface(directChild(rPr, "a:latin")?.getAttribute("typeface"), theme?.fonts);
  if (font) acc.font = font;
}

/** Merge an a:lvlNpPr / a:pPr element (paragraph props + its a:defRPr) into `acc`. */
export function applyPPr(acc: TextStyle, pPr: Element | null, theme?: ThemeContext): void {
  if (!pPr) return;
  const algn = pPr.getAttribute("algn");
  if (algn && ALIGN[algn]) acc.align = ALIGN[algn];
  const marL = num(pPr, "marL");
  if (marL !== undefined) acc.marL = emuToPx(marL);
  const indent = num(pPr, "indent");
  if (indent !== undefined) acc.indent = emuToPx(indent);

  const ln = spacing(directChild(pPr, "a:lnSpc"));
  if (ln.pct !== undefined) { acc.linePct = ln.pct; acc.linePts = undefined; }
  if (ln.pts !== undefined) { acc.linePts = ln.pts; acc.linePct = undefined; }
  const bef = spacing(directChild(pPr, "a:spcBef"));
  if (bef.pct !== undefined) { acc.spcBefPct = bef.pct; acc.spcBefPts = undefined; }
  if (bef.pts !== undefined) { acc.spcBefPts = bef.pts; acc.spcBefPct = undefined; }
  const aft = spacing(directChild(pPr, "a:spcAft"));
  if (aft.pct !== undefined) { acc.spcAftPct = aft.pct; acc.spcAftPts = undefined; }
  if (aft.pts !== undefined) { acc.spcAftPts = aft.pts; acc.spcAftPct = undefined; }

  applyBullet(acc, pPr, theme);
  applyRPr(acc, directChild(pPr, "a:defRPr"), theme);
}

/**
 * Bullet properties off one a:pPr / a:lvlNpPr. The `…Tx` forms ("follow the
 * text") are explicit *clears* of an inherited override, so they must reset the
 * accumulator rather than be ignored; buNone likewise clears the glyph itself.
 */
function applyBullet(acc: TextStyle, pPr: Element, theme?: ThemeContext): void {
  if (directChild(pPr, "a:buClrTx")) acc.buClr = undefined;
  const buClr = directChild(pPr, "a:buClr");
  if (buClr) {
    const clr = firstClrChild(buClr);
    acc.buClr = clr ? resolveClrEl(clr, theme) : undefined;
  }

  if (directChild(pPr, "a:buSzTx")) {
    acc.buSzPct = undefined;
    acc.buSzPts = undefined;
  }
  const szPct = directChild(pPr, "a:buSzPct");
  if (szPct) {
    const v = num(szPct, "val");
    if (v !== undefined) { acc.buSzPct = v / 100000; acc.buSzPts = undefined; }
  }
  const szPts = directChild(pPr, "a:buSzPts");
  if (szPts) {
    const v = num(szPts, "val");
    if (v !== undefined) { acc.buSzPts = v / 100; acc.buSzPct = undefined; }
  }

  if (directChild(pPr, "a:buFontTx")) acc.buFont = undefined;
  const buFont = directChild(pPr, "a:buFont");
  if (buFont) acc.buFont = resolveTypeface(buFont.getAttribute("typeface"), theme?.fonts);

  if (directChild(pPr, "a:buNone")) {
    acc.bullet = false;
    acc.buChar = undefined;
    acc.buAutoNum = undefined;
  }
  const buChar = directChild(pPr, "a:buChar");
  if (buChar) {
    acc.bullet = true;
    acc.buChar = buChar.getAttribute("char") ?? "•";
    acc.buAutoNum = undefined;
  }
  const buAuto = directChild(pPr, "a:buAutoNum");
  if (buAuto) {
    acc.bullet = true;
    acc.buAutoNum = buAuto.getAttribute("type") ?? "arabicPeriod";
    acc.buStartAt = num(buAuto, "startAt") ?? 1;
    acc.buChar = undefined;
  }
}

/**
 * The ordered a:lvlNpPr sources for one text body, lowest precedence first:
 * presentation defaults / master txStyles → master placeholder lstStyle →
 * layout placeholder lstStyle → the shape's own lstStyle. Each entry is that
 * source's nine level elements, indexed 0..8.
 */
export type LevelStyles = (Element | null)[];

export interface StyleChain {
  sources: LevelStyles[];
  /**
   * Shape-level defaults from p:style's a:fontRef (theme font + text colour).
   * Applied after `sources` because a shape's own p:style outranks the
   * presentation/master defaults it would otherwise inherit black text from.
   */
  base?: TextStyle;
  theme?: ThemeContext;
  /** a:normAutofit shrink factors from the shape's own bodyPr. */
  fontScale?: number;
  lnSpcReduction?: number;
  /** The shape is in shrink-to-fit mode at all (a:normAutofit present). */
  shrink?: boolean;
}

/** Pull a:lvl1pPr..a:lvl9pPr off an a:lstStyle / p:titleStyle / p:defaultTextStyle. */
export function levelStyles(container: Element | null | undefined): LevelStyles {
  const out: LevelStyles = [];
  for (let i = 1; i <= 9; i++) {
    out.push(container ? directChild(container, `a:lvl${i}pPr`) : null);
  }
  return out;
}

/**
 * Resolve one paragraph's style: walk the chain's level sources in order, then
 * the paragraph's own a:pPr, then (per run) its a:rPr. Levels are 0-based on the
 * wire and 1-based in the XML tag names.
 */
export function resolveParagraphStyle(
  chain: StyleChain,
  level: number,
  pPr: Element | null,
): TextStyle {
  const acc: TextStyle = {};
  const lvl = Math.max(0, Math.min(8, level));
  for (const src of chain.sources) applyPPr(acc, src[lvl] ?? null, chain.theme);
  if (chain.base) Object.assign(acc, chain.base);
  applyPPr(acc, pPr, chain.theme);
  return acc;
}

/** Layer a run's own a:rPr over the paragraph style. */
export function resolveRunStyle(
  chain: StyleChain,
  paraStyle: TextStyle,
  rPr: Element | null,
): TextStyle {
  const acc: TextStyle = { ...paraStyle };
  applyRPr(acc, rPr, chain.theme);
  if (chain.fontScale && acc.sizePt) acc.sizePt *= chain.fontScale;
  return acc;
}

import { describe, expect, it } from "vitest";
import {
  emuToPx,
  parseRels,
  parseSlide,
  parseSlideSize,
  parseTheme,
  resolveColor,
  resolvePath,
  slidePathsInOrder,
} from "./pptx";

const A = `xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"`;
const P = `xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"`;
const R = `xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"`;

/** Wrap a spTree body in the minimal slide envelope the parser expects. */
function slide(inner: string): string {
  return `<?xml version="1.0"?><p:sld ${A} ${P} ${R}><p:cSld><p:spTree>${inner}</p:spTree></p:cSld></p:sld>`;
}

// --- Real, trimmed theme + master fragments from "Website Options v2.pptx" ---
const THEME1 = `<?xml version="1.0"?><a:theme ${A}><a:themeElements><a:clrScheme name="Office">` +
  `<a:dk1><a:sysClr val="windowText" lastClr="000000"/></a:dk1>` +
  `<a:lt1><a:sysClr val="window" lastClr="FFFFFF"/></a:lt1>` +
  `<a:dk2><a:srgbClr val="44546A"/></a:dk2>` +
  `<a:lt2><a:srgbClr val="E7E6E6"/></a:lt2>` +
  `<a:accent1><a:srgbClr val="4472C4"/></a:accent1>` +
  `<a:accent2><a:srgbClr val="ED7D31"/></a:accent2>` +
  `<a:accent6><a:srgbClr val="70AD47"/></a:accent6>` +
  `</a:clrScheme></a:themeElements></a:theme>`;

const MASTER1 = `<?xml version="1.0"?><p:sldMaster ${P} ${A}><p:clrMap ` +
  `bg1="lt1" tx1="dk1" bg2="lt2" tx2="dk2" accent1="accent1" accent2="accent2" ` +
  `accent3="accent3" accent4="accent4" accent5="accent5" accent6="accent6" ` +
  `hlink="hlink" folHlink="folHlink"/></p:sldMaster>`;

/** Parse a bare <a:solidFill> (or any color-bearing element) for resolveColor tests. */
function colorEl(inner: string): Element {
  const doc = new DOMParser().parseFromString(
    `<root ${A}>${inner}</root>`, "application/xml",
  );
  return doc.documentElement.firstElementChild as Element;
}

describe("parseTheme", () => {
  it("resolves scheme slots (srgbClr and sysClr lastClr) to hex", () => {
    const t = parseTheme(THEME1, MASTER1);
    expect(t.colors.accent1).toBe("4472C4");
    expect(t.colors.dk1).toBe("000000"); // sysClr → lastClr
    expect(t.colors.lt1).toBe("FFFFFF");
  });

  it("maps clrMap names (bg1→lt1, tx1→dk1) onto scheme colors", () => {
    const t = parseTheme(THEME1, MASTER1);
    expect(t.colors.bg1).toBe("FFFFFF"); // bg1 → lt1 → window
    expect(t.colors.tx1).toBe("000000"); // tx1 → dk1 → windowText
  });

  it("returns an empty lookup when theme/master are missing", () => {
    expect(parseTheme(undefined, undefined).colors).toEqual({});
  });
});

describe("resolveColor", () => {
  const t = parseTheme(THEME1, MASTER1);

  it("still reads a plain srgbClr fill", () => {
    expect(resolveColor(colorEl(`<a:solidFill><a:srgbClr val="F2F2F2"/></a:solidFill>`), t))
      .toBe("#F2F2F2");
  });

  it("resolves a bare schemeClr through the theme", () => {
    expect(resolveColor(colorEl(`<a:solidFill><a:schemeClr val="accent1"/></a:solidFill>`), t))
      .toBe("#4472C4");
  });

  it("applies shade (darken) to a scheme color", () => {
    // accent1 4472C4 → each channel * 0.5 → 22 39 62
    expect(resolveColor(colorEl(
      `<a:solidFill><a:schemeClr val="accent1"><a:shade val="50000"/></a:schemeClr></a:solidFill>`), t))
      .toBe("#223962");
  });

  it("applies lumMod+lumOff (light tint) in document order", () => {
    // accent6 70AD47 lumMod 20000 + lumOff 80000 → very light green
    const c = resolveColor(colorEl(
      `<a:solidFill><a:schemeClr val="accent6"><a:lumMod val="20000"/><a:lumOff val="80000"/></a:schemeClr></a:solidFill>`), t);
    expect(c).toMatch(/^#[0-9A-F]{6}$/);
    // luminance pushed high: all channels should be light (>0xC0)
    const [r, g, b] = [1, 3, 5].map((i) => parseInt(c!.slice(i, i + 2), 16));
    expect(Math.min(r, g, b)).toBeGreaterThan(0xC0);
  });

  it("emits rgba when alpha is present", () => {
    expect(resolveColor(colorEl(
      `<a:solidFill><a:srgbClr val="FF0000"><a:alpha val="50000"/></a:srgbClr></a:solidFill>`), t))
      .toBe("rgba(255,0,0,0.5)");
  });

  it("returns undefined for a colorless / noFill element", () => {
    expect(resolveColor(colorEl(`<a:ln><a:noFill/></a:ln>`), t)).toBeUndefined();
  });
});

describe("emuToPx", () => {
  it("uses 9525 EMU per CSS pixel (914400 EMU/inch ÷ 96px/inch)", () => {
    expect(emuToPx(9525)).toBe(1);
    expect(emuToPx(914400)).toBe(96);
    expect(emuToPx(0)).toBe(0);
  });
});

describe("parseSlideSize", () => {
  it("reads sldSz cx/cy and converts EMU to px", () => {
    const xml = `<?xml version="1.0"?><p:presentation ${P}><p:sldSz cx="9144000" cy="6858000"/></p:presentation>`;
    expect(parseSlideSize(xml)).toEqual({ width: 960, height: 720 });
  });

  it("defaults to 16:9 (1280x720) when presentation.xml is missing or sizeless", () => {
    expect(parseSlideSize(undefined)).toEqual({ width: 1280, height: 720 });
    expect(parseSlideSize(`<?xml version="1.0"?><p:presentation ${P}/>`)).toEqual(
      { width: 1280, height: 720 },
    );
  });
});

describe("resolvePath", () => {
  it("resolves ../media targets against a slide's own folder", () => {
    expect(resolvePath("ppt/slides", "../media/image1.png")).toBe(
      "ppt/media/image1.png",
    );
    expect(resolvePath("ppt", "slides/slide1.xml")).toBe("ppt/slides/slide1.xml");
  });
});

describe("parseRels", () => {
  it("maps relationship ids to their targets", () => {
    const xml = `<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Target="slides/slide1.xml"/><Relationship Id="rId2" Target="../media/image1.png"/></Relationships>`;
    const rels = parseRels(xml);
    expect(rels.get("rId1")).toBe("slides/slide1.xml");
    expect(rels.get("rId2")).toBe("../media/image1.png");
  });
});

describe("slidePathsInOrder", () => {
  it("orders slides by the sldId sequence in presentation.xml, not by filename", () => {
    const pres = `<?xml version="1.0"?><p:presentation ${P} ${R}><p:sldIdLst><p:sldId id="256" r:id="rId3"/><p:sldId id="257" r:id="rId2"/></p:sldIdLst></p:presentation>`;
    const rels = `<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId2" Target="slides/slide9.xml"/><Relationship Id="rId3" Target="slides/slide4.xml"/></Relationships>`;
    expect(slidePathsInOrder(pres, rels)).toEqual([
      "ppt/slides/slide4.xml",
      "ppt/slides/slide9.xml",
    ]);
  });

  it("returns an empty list when there is no presentation part to order from", () => {
    expect(slidePathsInOrder(undefined, undefined)).toEqual([]);
  });
});

describe("parseSlide — geometry", () => {
  it("converts a shape's xfrm off/ext into positioned px", () => {
    const xml = slide(
      `<p:sp><p:spPr><a:xfrm><a:off x="914400" y="457200"/><a:ext cx="1828800" cy="914400"/></a:xfrm></p:spPr><p:txBody><a:p><a:r><a:t>Hi</a:t></a:r></a:p></p:txBody></p:sp>`,
    );
    const [shape] = parseSlide(xml).shapes;
    expect(shape.x).toBe(96);
    expect(shape.y).toBe(48);
    expect(shape.w).toBe(192);
    expect(shape.h).toBe(96);
  });

  it("leaves geometry null when a shape has no xfrm (auto-flow fallback)", () => {
    const xml = slide(
      `<p:sp><p:txBody><a:p><a:r><a:t>No coords</a:t></a:r></a:p></p:txBody></p:sp>`,
    );
    const [shape] = parseSlide(xml).shapes;
    expect(shape.x).toBeNull();
    expect(shape.y).toBeNull();
  });
});

describe("parseSlide — text and runs", () => {
  it("captures run styling: bold, italic, size (pt) and srgb color", () => {
    const xml = slide(
      `<p:sp><p:txBody><a:p><a:r><a:rPr b="1" i="1" sz="2400"><a:solidFill><a:srgbClr val="FF0000"/></a:solidFill><a:latin typeface="Arial"/></a:rPr><a:t>Styled</a:t></a:r></a:p></p:txBody></p:sp>`,
    );
    const run = parseSlide(xml).shapes[0].paragraphs[0].runs[0];
    expect(run.text).toBe("Styled");
    expect(run.bold).toBe(true);
    expect(run.italic).toBe(true);
    expect(run.sizePt).toBe(24);
    expect(run.color).toBe("#FF0000");
    expect(run.font).toBe("Arial");
  });

  it("reads paragraph alignment and indent level", () => {
    const xml = slide(
      `<p:sp><p:txBody><a:p><a:pPr algn="ctr" lvl="2"/><a:r><a:t>Centered</a:t></a:r></a:p></p:txBody></p:sp>`,
    );
    const para = parseSlide(xml).shapes[0].paragraphs[0];
    expect(para.align).toBe("center");
    expect(para.level).toBe(2);
  });

  it("marks title placeholders and drops empty paragraphs' shapes", () => {
    const xml = slide(
      `<p:sp><p:nvSpPr><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>Deck title</a:t></a:r></a:p></p:txBody></p:sp><p:sp><p:txBody><a:p><a:r><a:t>   </a:t></a:r></a:p></p:txBody></p:sp>`,
    );
    const shapes = parseSlide(xml).shapes;
    expect(shapes).toHaveLength(1);
    expect(shapes[0].isTitle).toBe(true);
    expect(shapes[0].paragraphs[0].runs[0].text).toBe("Deck title");
  });
});

describe("parseSlide — images and shapes", () => {
  it("extracts a picture's embed id and geometry", () => {
    const xml = slide(
      `<p:pic><p:blipFill><a:blip r:embed="rId5"/></p:blipFill><p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="1905000" cy="1905000"/></a:xfrm></p:spPr></p:pic>`,
    );
    const [shape] = parseSlide(xml).shapes;
    expect(shape.kind).toBe("image");
    expect(shape.embedId).toBe("rId5");
    expect(shape.w).toBe(200);
  });

  it("captures a filled preset shape as a drawable box", () => {
    const xml = slide(
      `<p:sp><p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="952500" cy="952500"/></a:xfrm><a:prstGeom prst="ellipse"/><a:solidFill><a:srgbClr val="00FF00"/></a:solidFill></p:spPr></p:sp>`,
    );
    const [shape] = parseSlide(xml).shapes;
    expect(shape.geom).toBe("ellipse");
    expect(shape.fill).toBe("#00FF00");
  });
});

describe("parseSlide — the repo fixture deck", () => {
  it("still reads text from a bare slide that has no positions at all", () => {
    // Mirrors crates/ken-core/fixtures/.../deck.pptx slide1 (no xfrm, no rels).
    const xml = slide(
      `<p:sp><p:txBody><a:p><a:r><a:t>Migration kickoff deck</a:t></a:r></a:p></p:txBody></p:sp>`,
    );
    const [shape] = parseSlide(xml).shapes;
    expect(shape.x).toBeNull();
    expect(shape.paragraphs[0].runs[0].text).toBe("Migration kickoff deck");
  });
});

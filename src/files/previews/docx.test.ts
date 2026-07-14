import { describe, expect, it } from "vitest";
import { applyExtentsToHtml, emuToPx, parseExtents } from "./docx";

const W = `xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"`;
const WP = `xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"`;
const A = `xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"`;
const R = `xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"`;
const PIC = `xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"`;

/** One inline picture drawing as Word authors it: layout extent + blip embed. */
function drawing(cx: number, cy: number, embed: string): string {
  return `<w:drawing><wp:inline><wp:extent cx="${cx}" cy="${cy}"/><wp:effectExtent l="0" t="0" r="0" b="0"/><a:graphic><a:graphicData><pic:pic><pic:blipFill><a:blip r:embed="${embed}"/></pic:blipFill><pic:spPr><a:xfrm><a:ext cx="${cx}" cy="${cy}"/></a:xfrm></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing>`;
}

function doc(body: string): string {
  return `<?xml version="1.0"?><w:document ${W} ${WP} ${A} ${R} ${PIC}><w:body>${body}</w:body></w:document>`;
}

describe("emuToPx", () => {
  it("uses 9525 EMU per CSS pixel (914400 EMU/inch ÷ 96px/inch)", () => {
    expect(emuToPx(9525)).toBe(1);
    expect(emuToPx(914400)).toBe(96);
    expect(emuToPx(0)).toBe(0);
  });

  it("rounds to whole pixels so width/height attributes stay clean", () => {
    // 190500 EMU = 20px exactly; 200000 EMU ≈ 20.997px → 21.
    expect(emuToPx(190500)).toBe(20);
    expect(emuToPx(200000)).toBe(21);
  });
});

describe("parseExtents", () => {
  it("reads each drawing's wp:extent as px, in document order", () => {
    // A small avatar then a large figure — the sizes Word actually laid out,
    // independent of the images' intrinsic (base64) resolution.
    const xml = doc(
      `<w:p>${drawing(190500, 190500, "rId4")}</w:p><w:p>${drawing(2743200, 2057400, "rId5")}</w:p>`,
    );
    expect(parseExtents(xml)).toEqual([
      { embedId: "rId4", width: 20, height: 20 },
      { embedId: "rId5", width: 288, height: 216 },
    ]);
  });

  it("captures the r:embed relationship id alongside each extent", () => {
    const xml = doc(`<w:p>${drawing(190500, 190500, "rId7")}</w:p>`);
    expect(parseExtents(xml)[0].embedId).toBe("rId7");
  });

  it("skips drawings that carry an extent but no picture blip, keeping order aligned with mammoth's <img> stream", () => {
    // A chart/shape drawing (extent, no <a:blip r:embed>) produces no mammoth
    // <img>, so it must not consume an extent slot or later images shift.
    const chart = `<w:drawing><wp:inline><wp:extent cx="914400" cy="914400"/><a:graphic><a:graphicData/></a:graphic></wp:inline></w:drawing>`;
    const xml = doc(
      `<w:p>${chart}</w:p><w:p>${drawing(190500, 190500, "rId9")}</w:p>`,
    );
    expect(parseExtents(xml)).toEqual([
      { embedId: "rId9", width: 20, height: 20 },
    ]);
  });

  it("returns an empty list for a document with no drawings", () => {
    expect(parseExtents(doc(`<w:p><w:r><w:t>plain text</w:t></w:r></w:p>`))).toEqual(
      [],
    );
  });
});

describe("applyExtentsToHtml", () => {
  it("applies each extent to the mammoth <img> at the same position", () => {
    const html =
      `<p><img src="data:image/png;base64,AAAA" /></p>` +
      `<p><img src="data:image/png;base64,BBBB" /></p>`;
    const out = applyExtentsToHtml(html, [
      { embedId: "rId4", width: 20, height: 20 },
      { embedId: "rId5", width: 288, height: 216 },
    ]);
    const imgs = new DOMParser()
      .parseFromString(out, "text/html")
      .querySelectorAll("img");
    expect(imgs[0].getAttribute("width")).toBe("20");
    expect(imgs[0].getAttribute("height")).toBe("20");
    expect(imgs[1].getAttribute("width")).toBe("288");
    expect(imgs[1].getAttribute("height")).toBe("216");
  });

  it("leaves images without a matching extent untouched (they fall through to CSS max-width)", () => {
    const html = `<p><img src="data:image/png;base64,AAAA" /><img src="data:image/png;base64,BBBB" /></p>`;
    const out = applyExtentsToHtml(html, [
      { embedId: "rId4", width: 20, height: 20 },
    ]);
    const imgs = new DOMParser()
      .parseFromString(out, "text/html")
      .querySelectorAll("img");
    expect(imgs[0].getAttribute("width")).toBe("20");
    expect(imgs[1].hasAttribute("width")).toBe(false);
    expect(imgs[1].hasAttribute("height")).toBe(false);
  });

  it("is a no-op when there are no extents to apply", () => {
    const html = `<p><img src="data:image/png;base64,AAAA" /></p>`;
    expect(applyExtentsToHtml(html, [])).toBe(html);
  });
});

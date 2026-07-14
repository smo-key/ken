import { describe, expect, it } from "vitest";
import { PREVIEW_CAP_BYTES, isPreviewTooLarge, previewFormat } from "./sizeGate";

describe("preview size gate", () => {
  const MB = 1024 * 1024;

  it("recognises the capped formats by kind and extension", () => {
    expect(previewFormat("book.xlsx", "xlsx")).toBe("xlsx");
    expect(previewFormat("memo.docx", "docx")).toBe("docx");
    expect(previewFormat("deck.pptx", "pptx")).toBe("pptx");
    // ipynb indexes as "binary"; it is routed by extension.
    expect(previewFormat("nb.ipynb", "binary")).toBe("ipynb");
  });

  it("does not gate formats that already stream or are cheap", () => {
    expect(previewFormat("photo.png", "image")).toBeNull();
    expect(previewFormat("doc.pdf", "pdf")).toBeNull();
    expect(previewFormat("clip.mp4", "binary")).toBeNull();
    expect(previewFormat("notes.md", "md")).toBeNull();
  });

  it("flags an over-cap workbook and clears an under-cap one", () => {
    expect(isPreviewTooLarge("eo3.xlsx", "xlsx", 148 * MB)).toBe(true); // the reported hang
    expect(isPreviewTooLarge("small.xlsx", "xlsx", 2 * MB)).toBe(false);
    // Exactly at the cap is allowed; one byte over is not.
    expect(isPreviewTooLarge("edge.docx", "docx", PREVIEW_CAP_BYTES)).toBe(false);
    expect(isPreviewTooLarge("edge.docx", "docx", PREVIEW_CAP_BYTES + 1)).toBe(true);
  });

  it("never gates a non-capped format regardless of size", () => {
    expect(isPreviewTooLarge("huge.pdf", "pdf", 500 * MB)).toBe(false);
  });
});

import { describe, expect, it } from "vitest";
import {
  PPTX_CAP_BYTES,
  PREVIEW_CAP_BYTES,
  capForFormat,
  isPreviewTooLarge,
  previewFormat,
} from "./sizeGate";

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

  it("gives pptx its own 50 MB cap without lowering the others", () => {
    expect(PPTX_CAP_BYTES).toBe(50 * MB);
    expect(capForFormat("pptx")).toBe(50 * MB);
    expect(capForFormat("xlsx")).toBe(PREVIEW_CAP_BYTES);
    expect(capForFormat("docx")).toBe(PREVIEW_CAP_BYTES);
    expect(capForFormat("ipynb")).toBe(PREVIEW_CAP_BYTES);
  });

  it("allows a 40 MB deck but gates one over 50 MB", () => {
    expect(isPreviewTooLarge("deck.pptx", "pptx", 40 * MB)).toBe(false);
    // A workbook of the same size is still gated at 15 MB.
    expect(isPreviewTooLarge("book.xlsx", "xlsx", 40 * MB)).toBe(true);
    // Exactly at the pptx cap is allowed; one byte over is not.
    expect(isPreviewTooLarge("edge.pptx", "pptx", PPTX_CAP_BYTES)).toBe(false);
    expect(isPreviewTooLarge("edge.pptx", "pptx", PPTX_CAP_BYTES + 1)).toBe(true);
  });
});

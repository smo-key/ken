// Per-format preview size caps (§11). Office/notebook previews parse the whole
// file synchronously; over a cap they would wedge the webview, so we gate on the
// already-loaded meta.size before any bytes are read. Formats that stream (pdf,
// image, video) or are cheap text are never gated here.

/** Default cap for the heavy office/notebook formats. Tuned against real parse
 *  times: a 15 MB workbook parses in well under a second; 148 MB hangs. */
export const PREVIEW_CAP_BYTES = 15 * 1024 * 1024;

/** pptx gets a higher cap: its unzip + parse now runs off the main thread in a
 *  Web Worker and streams slides in, so a large deck stays responsive where a
 *  same-size synchronous workbook parse would not. */
export const PPTX_CAP_BYTES = 50 * 1024 * 1024;

export type PreviewFormat = "xlsx" | "docx" | "pptx" | "ipynb";

/** Byte cap for a given capped format. pptx is the one exception (50 MB). */
export function capForFormat(format: PreviewFormat): number {
  return format === "pptx" ? PPTX_CAP_BYTES : PREVIEW_CAP_BYTES;
}

/** The capped format for a file, or null if this preview isn't size-gated.
 *  ipynb indexes as "binary", so it is matched by extension; the office kinds
 *  come straight from the backend classifier. */
export function previewFormat(relPath: string, kind: string): PreviewFormat | null {
  const ext = relPath.split(".").pop()?.toLowerCase() ?? "";
  if (ext === "ipynb") return "ipynb";
  if (kind === "xlsx") return "xlsx";
  if (kind === "docx") return "docx";
  if (kind === "pptx") return "pptx";
  return null;
}

/** Whether a preview would exceed its (per-format) cap. False when not gated. */
export function isPreviewTooLarge(relPath: string, kind: string, size: number): boolean {
  const format = previewFormat(relPath, kind);
  return format !== null && size > capForFormat(format);
}

// One home for the extension→MIME map and the base64 encoder the previews
// share. They lived as four hand-copied duplicates and had already drifted —
// the image preview's copy was missing webp/avif/ico, so the very asset that
// inlined cleanly in the HTML preview rendered broken there. A single map means
// a type is added once and every call site gains it.

/**
 * Union of every type the previews resolve: the image formats an <img> or a
 * data-URI can display, plus the web-font formats the HTML preview inlines from
 * @font-face. Keys are lowercase, bare extensions (no dot).
 */
export const MIME_TYPES: Readonly<Record<string, string>> = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
  avif: "image/avif",
  bmp: "image/bmp",
  ico: "image/x-icon",
  svg: "image/svg+xml",
  tiff: "image/tiff",
  woff: "font/woff",
  woff2: "font/woff2",
  ttf: "font/ttf",
  otf: "font/otf",
  eot: "application/vnd.ms-fontobject",
};

/** MIME type for a bare extension, or undefined when it is not one we map. */
export function mimeForExtension(ext: string): string | undefined {
  return MIME_TYPES[ext.toLowerCase()];
}

/** MIME type derived from a path's extension, or undefined when unmapped. */
export function mimeForPath(path: string): string | undefined {
  const dot = path.lastIndexOf(".");
  return dot < 0 ? undefined : mimeForExtension(path.slice(dot + 1));
}

/**
 * Base64 of a byte buffer. Chunked because `String.fromCharCode(...bytes)` on a
 * real image overruns the call's argument limit and throws. Accepts an
 * ArrayBuffer or a typed-array view so every call site passes what it already
 * holds without a copy.
 */
export function base64(input: ArrayBuffer | Uint8Array): string {
  const bytes = input instanceof Uint8Array ? input : new Uint8Array(input);
  const CHUNK = 0x8000;
  let binary = "";
  for (let i = 0; i < bytes.length; i += CHUNK) {
    binary += String.fromCharCode(...bytes.subarray(i, i + CHUNK));
  }
  return btoa(binary);
}

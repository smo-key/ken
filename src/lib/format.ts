// Display helpers: file glyph labels/tints, sizes, relative times.

export interface GlyphStyle {
  label: string;
  bg: string;
  border: string;
  color: string;
  /** Ken-maintained structured docs render in full ink. */
  solid?: boolean;
}

const GLYPHS: Record<string, GlyphStyle> = {
  md: { label: "MD", bg: "var(--paper)", border: "var(--border-strong)", color: "var(--ink-secondary)" },
  txt: { label: "TXT", bg: "var(--paper)", border: "var(--border-strong)", color: "var(--ink-secondary)" },
  code: { label: "SRC", bg: "var(--paper)", border: "var(--border-strong)", color: "var(--ink-secondary)" },
  docx: { label: "DOC", bg: "color-mix(in srgb, var(--file-doc) 8%, transparent)", border: "color-mix(in srgb, var(--file-doc) 30%, transparent)", color: "var(--file-doc)" },
  xlsx: { label: "XLS", bg: "color-mix(in srgb, var(--healthy) 10%, transparent)", border: "color-mix(in srgb, var(--healthy) 35%, transparent)", color: "var(--healthy-text)" },
  pptx: { label: "PPT", bg: "color-mix(in srgb, var(--needs-input) 10%, transparent)", border: "color-mix(in srgb, var(--needs-input) 32%, transparent)", color: "var(--needs-input-text)" },
  pdf: { label: "PDF", bg: "color-mix(in srgb, var(--danger) 9%, transparent)", border: "color-mix(in srgb, var(--danger) 32%, transparent)", color: "var(--danger)" },
  url: { label: "URL", bg: "color-mix(in srgb, var(--accent) 9%, transparent)", border: "color-mix(in srgb, var(--accent) 30%, transparent)", color: "var(--accent)" },
  image: { label: "IMG", bg: "var(--sunken)", border: "var(--border-strong)", color: "var(--ink-secondary)" },
  binary: { label: "BIN", bg: "var(--sunken)", border: "var(--border-strong)", color: "var(--ink-tertiary)" },
};

export function glyphFor(kind: string): GlyphStyle {
  return GLYPHS[kind] ?? GLYPHS.binary;
}

/** Mirrors `FileKind::from_path` (crates/ken-core/src/extract.rs) so hit
 *  sources that don't carry a `kind` field (e.g. `hybrid_search`) can still
 *  pick a glyph. Keep this extension table in lockstep with the Rust match. */
export function kindForPath(path: string): string {
  const dot = path.lastIndexOf(".");
  const ext = dot >= 0 ? path.slice(dot + 1).toLowerCase() : "";
  switch (ext) {
    case "md":
    case "markdown":
      return "md";
    case "txt":
    case "text":
    case "log":
    case "vtt":
      return "txt";
    case "rs":
    case "ts":
    case "js":
    case "jsx":
    case "tsx":
    case "svelte":
    case "py":
    case "rb":
    case "go":
    case "java":
    case "c":
    case "cc":
    case "cpp":
    case "h":
    case "hpp":
    case "cs":
    case "swift":
    case "kt":
    case "sh":
    case "bash":
    case "zsh":
    case "sql":
    case "json":
    case "yaml":
    case "yml":
    case "toml":
    case "ini":
    case "cfg":
    case "html":
    case "htm":
    case "css":
    case "scss":
    case "xml":
    case "csv":
      return "code";
    case "docx":
      return "docx";
    case "xlsx":
    case "xlsm":
      return "xlsx";
    case "pptx":
      return "pptx";
    case "pdf":
      return "pdf";
    case "ipynb":
      return "ipynb";
    case "png":
    case "jpg":
    case "jpeg":
    case "gif":
    case "webp":
    case "heic":
    case "bmp":
    case "tiff":
    case "tif":
    case "svg":
      return "image";
    case "mp4":
    case "mov":
    case "m4v":
    case "webm":
    case "mkv":
    case "avi":
      return "video";
    default:
      return "binary";
  }
}

export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export function timeAgo(epochSeconds: number, nowMs = Date.now()): string {
  const s = Math.max(0, Math.floor(nowMs / 1000) - epochSeconds);
  if (s < 10) return "just now";
  if (s < 60) return `${s} sec ago`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m} min ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h} hr ago`;
  const d = Math.floor(h / 24);
  if (d === 1) return "yesterday";
  if (d < 30) return `${d} days ago`;
  return new Date(epochSeconds * 1000).toLocaleDateString();
}

/** Formats editable by the WYSIWYG/plain editor. */
export function isEditable(kind: string): boolean {
  return kind === "md" || kind === "txt" || kind === "code";
}

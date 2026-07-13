// Pure parsing of Jupyter notebooks (nbformat v4) into a shape the preview can
// render directly. No DOM, no rendering here — just normalization.

export type IpynbOutput =
  | { kind: "stream"; name: string; text: string }
  | { kind: "text"; text: string }
  | { kind: "image"; mime: string; data: string }
  | { kind: "error"; ename: string; evalue: string; traceback: string };

export interface IpynbCell {
  type: "markdown" | "code" | "raw";
  /** Joined source text. */
  source: string;
  /** Code cells only; null when never executed. */
  executionCount: number | null;
  /** Code cells only. */
  outputs: IpynbOutput[];
}

export interface Notebook {
  cells: IpynbCell[];
}

/** nbformat stores multiline text as either a string or an array of lines
 *  (each line keeping its own trailing newline), so array lines join with "". */
export function joinSource(source: unknown): string {
  if (typeof source === "string") return source;
  if (Array.isArray(source)) return source.map((s) => String(s)).join("");
  return "";
}

// Strip ANSI escape sequences (SGR colors, cursor moves) from tracebacks.
const ANSI = /\x1b(?:[@-Z\\-_]|\[[0-?]*[ -/]*[@-~])/g;

export function stripAnsi(text: string): string {
  return text.replace(ANSI, "");
}

const IMAGE_MIMES = ["image/png", "image/jpeg"] as const;

function parseOutput(raw: unknown): IpynbOutput | null {
  if (!raw || typeof raw !== "object") return null;
  const o = raw as Record<string, unknown>;
  switch (o.output_type) {
    case "stream":
      return {
        kind: "stream",
        name: typeof o.name === "string" ? o.name : "stdout",
        text: joinSource(o.text),
      };
    case "error":
      return {
        kind: "error",
        ename: typeof o.ename === "string" ? o.ename : "",
        evalue: typeof o.evalue === "string" ? o.evalue : "",
        traceback: stripAnsi(
          Array.isArray(o.traceback)
            ? o.traceback.map((l) => String(l)).join("\n")
            : joinSource(o.traceback),
        ),
      };
    case "execute_result":
    case "display_data": {
      const data = (o.data ?? {}) as Record<string, unknown>;
      // Prefer a rendered image; fall back to plain text.
      for (const mime of IMAGE_MIMES) {
        if (data[mime] != null) {
          // Base64 payloads may be stored as a split array of lines.
          const b64 = joinSource(data[mime]).replace(/\s+/g, "");
          return { kind: "image", mime, data: b64 };
        }
      }
      if (data["text/plain"] != null) {
        return { kind: "text", text: joinSource(data["text/plain"]) };
      }
      return null;
    }
    default:
      return null;
  }
}

function parseCell(raw: unknown): IpynbCell | null {
  if (!raw || typeof raw !== "object") return null;
  const c = raw as Record<string, unknown>;
  const type =
    c.cell_type === "markdown" || c.cell_type === "code" || c.cell_type === "raw"
      ? c.cell_type
      : null;
  if (!type) return null;

  const outputs =
    type === "code" && Array.isArray(c.outputs)
      ? c.outputs.map(parseOutput).filter((o): o is IpynbOutput => o !== null)
      : [];

  return {
    type,
    source: joinSource(c.source),
    executionCount:
      typeof c.execution_count === "number" ? c.execution_count : null,
    outputs,
  };
}

/**
 * Parse a notebook from JSON text or an already-parsed object. Throws on
 * unusable input (not JSON, or no `cells` array) so callers can show an error.
 */
export function parseNotebook(input: string | unknown): Notebook {
  const nb =
    typeof input === "string" ? (JSON.parse(input) as unknown) : input;
  if (!nb || typeof nb !== "object") {
    throw new Error("Notebook is not an object.");
  }
  const rawCells = (nb as Record<string, unknown>).cells;
  if (!Array.isArray(rawCells)) {
    throw new Error("Notebook has no cells array.");
  }
  const cells = rawCells
    .map(parseCell)
    .filter((c): c is IpynbCell => c !== null);
  return { cells };
}

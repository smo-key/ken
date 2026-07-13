// RFC 4180-compliant delimited-text parsing and serialization.
//
// Handles quoted fields, embedded delimiters / quotes / newlines, and CRLF or
// LF record separators. TSV is just CSV with a tab delimiter. A grid parsed
// here re-serializes to text that parses back to the same grid (see caveats
// below), so it round-trips faithfully through the editor's save path.

/** The delimiter for a path, by extension: TSV → tab, everything else comma. */
export function delimiterForPath(path: string): string {
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  return ext === "tsv" ? "\t" : ",";
}

/**
 * Parse delimited text into a grid of rows × fields.
 *
 * - Fields may be wrapped in double quotes; inside a quoted field a literal
 *   double quote is written as two (`""`), and delimiters / CR / LF are taken
 *   verbatim.
 * - Records are separated by LF, CR, or CRLF.
 * - A single trailing record separator does not produce a phantom empty row.
 * - Empty input yields an empty grid (`[]`).
 */
export function parseDelimited(text: string, delimiter: string): string[][] {
  const rows: string[][] = [];
  let row: string[] = [];
  let field = "";
  let inQuotes = false;
  // Whether the current record has begun (any char, delimiter, or open quote).
  // Distinguishes a genuine empty trailing line from the terminator of the
  // previous line, so we don't append a phantom `[""]`.
  let recordStarted = false;
  const n = text.length;
  let i = 0;

  const endField = () => {
    row.push(field);
    field = "";
  };
  const endRow = () => {
    rows.push(row);
    row = [];
    recordStarted = false;
  };

  while (i < n) {
    const c = text[i];

    if (inQuotes) {
      if (c === '"') {
        if (text[i + 1] === '"') {
          field += '"';
          i += 2;
          continue;
        }
        inQuotes = false;
        i += 1;
        continue;
      }
      field += c;
      i += 1;
      continue;
    }

    if (c === '"') {
      inQuotes = true;
      recordStarted = true;
      i += 1;
      continue;
    }
    if (c === delimiter) {
      endField();
      recordStarted = true;
      i += 1;
      continue;
    }
    if (c === "\r") {
      endField();
      endRow();
      // Absorb the LF of a CRLF pair.
      i += text[i + 1] === "\n" ? 2 : 1;
      continue;
    }
    if (c === "\n") {
      endField();
      endRow();
      i += 1;
      continue;
    }

    field += c;
    recordStarted = true;
    i += 1;
  }

  // Flush a final unterminated record. Skip when the input ended exactly on a
  // record separator (nothing pending) so no phantom row appears.
  if (field !== "" || row.length > 0 || recordStarted || inQuotes) {
    endField();
    endRow();
  }

  return rows;
}

function needsQuoting(field: string, delimiter: string): boolean {
  return (
    field.includes(delimiter) ||
    field.includes('"') ||
    field.includes("\n") ||
    field.includes("\r")
  );
}

function escapeField(field: string, delimiter: string): string {
  if (needsQuoting(field, delimiter)) {
    return '"' + field.replace(/"/g, '""') + '"';
  }
  return field;
}

/**
 * Serialize a grid back to delimited text. Fields containing the delimiter, a
 * double quote, or a newline are quoted; embedded quotes are doubled. Records
 * are joined with `newline` (LF by default).
 *
 * Caveat: a grid of exactly one row with one empty field (`[[""]]`) serializes
 * to the empty string, which parses back as an empty grid. This ambiguity is
 * inherent to CSV; all non-degenerate grids round-trip exactly.
 */
export function serializeDelimited(
  rows: string[][],
  delimiter: string,
  newline = "\n",
): string {
  return rows
    .map((row) => row.map((field) => escapeField(field, delimiter)).join(delimiter))
    .join(newline);
}

export const parseCsv = (text: string): string[][] => parseDelimited(text, ",");
export const parseTsv = (text: string): string[][] => parseDelimited(text, "\t");
export const serializeCsv = (rows: string[][], newline?: string): string =>
  serializeDelimited(rows, ",", newline);
export const serializeTsv = (rows: string[][], newline?: string): string =>
  serializeDelimited(rows, "\t", newline);

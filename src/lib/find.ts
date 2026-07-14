// Pure match-finding for find-in-document: offsets in a string, hits in a cell
// grid, and the index math the counter and next/prev buttons run on.
// Everything here is case-insensitive and literal — the query is what the user
// typed, never a pattern.

/** Marking tens of thousands of hits costs more than it's worth; stop early. */
export const MATCH_CAP = 2000;

/** A 50-sheet workbook is scanned lazily; this bounds the worst case. */
export const CELL_CAP = 200_000;

export interface TextMatches {
  /** Start offsets, ascending, non-overlapping. */
  starts: number[];
  /** True when the cap cut the scan short — the count is a floor, not a total. */
  capped: boolean;
}

/** RegExp-special characters, so the query stays literal — never a pattern. */
function escapeRegExp(literal: string): string {
  return literal.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

export function findTextMatches(
  text: string,
  query: string,
  cap = MATCH_CAP,
): TextMatches {
  const starts: number[] = [];
  if (!query.trim() || !text) return { starts, capped: false };

  // Scan the ORIGINAL text, not a lowercased copy. Lowercasing and returning
  // offsets into that copy corrupts them for every caller that indexes the
  // original string: a character that lengthens under toLowerCase() (Turkish
  // 'İ' U+0130 → 'i̇', two UTF-16 units) shifts every following offset, so a
  // highlight lands on the wrong glyph or runs off the node and crashes
  // splitText. A global `i`-flag regex over the original keeps match.index in
  // original-string coordinates. Simple case folding never changes the matched
  // span's length, so it stays query.length units — callers may keep relying on
  // that. The query is escaped so it is matched literally.
  const re = new RegExp(escapeRegExp(query), "gi");
  for (let m = re.exec(text); m; m = re.exec(text)) {
    if (starts.length === cap) return { starts, capped: true };
    starts.push(m.index);
    // The global regex resumes at index + match length, so hits never overlap:
    // "aa" in "aaaa" is 0 then 2.
  }
  return { starts, capped: false };
}

export interface CellMatch {
  sheet: number;
  row: number;
  col: number;
  /** Ordinal of this hit among all hits in its sheet, row-major. Lines the
      match up with the nth highlight in the rendered table. */
  occurrence: number;
}

export interface CellMatches {
  matches: CellMatch[];
  capped: boolean;
}

export function findCellMatches(
  sheets: string[][][],
  query: string,
  opts: { matchCap?: number; cellCap?: number } = {},
): CellMatches {
  const matchCap = opts.matchCap ?? MATCH_CAP;
  const cellCap = opts.cellCap ?? CELL_CAP;
  const matches: CellMatch[] = [];
  if (!query.trim()) return { matches, capped: false };

  const needle = query.toLowerCase();
  let cells = 0;

  for (let sheet = 0; sheet < sheets.length; sheet++) {
    let occurrence = 0;
    const rows = sheets[sheet];
    for (let row = 0; row < rows.length; row++) {
      for (let col = 0; col < rows[row].length; col++) {
        if (cells >= cellCap) return { matches, capped: true };
        cells++;
        const value = String(rows[row][col] ?? "").toLowerCase();
        let from = 0;
        for (;;) {
          const at = value.indexOf(needle, from);
          if (at < 0) break;
          if (matches.length >= matchCap) return { matches, capped: true };
          matches.push({ sheet, row, col, occurrence });
          occurrence++;
          from = at + needle.length;
        }
      }
    }
  }
  return { matches, capped: false };
}

/** Next/previous with wrap-around; tolerant of an index left stale by a re-search. */
export function stepMatch(index: number, total: number, delta: number): number {
  if (total <= 0) return 0;
  const from = index >= total || index < 0 ? (delta > 0 ? -1 : 0) : index;
  return (((from + delta) % total) + total) % total;
}

export function counterLabel(
  query: string,
  index: number,
  total: number,
  capped: boolean,
): string {
  if (!query.trim()) return "";
  if (total === 0) return "No results";
  return `${index + 1} of ${total}${capped ? "+" : ""}`;
}

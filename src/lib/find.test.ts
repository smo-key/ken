import { describe, expect, it } from "vitest";
import {
  MATCH_CAP,
  counterLabel,
  findCellMatches,
  findTextMatches,
  stepMatch,
} from "./find";

describe("findTextMatches", () => {
  it("finds every occurrence, case-insensitively", () => {
    expect(findTextMatches("Cat cat CAT", "cat").starts).toEqual([0, 4, 8]);
  });

  it("finds nothing for an empty or whitespace-only query", () => {
    expect(findTextMatches("cat", "").starts).toEqual([]);
    expect(findTextMatches("cat", "   ").starts).toEqual([]);
  });

  it("advances past each hit so matches never overlap", () => {
    expect(findTextMatches("aaaa", "aa").starts).toEqual([0, 2]);
  });

  it("treats the query literally, not as a regex", () => {
    expect(findTextMatches("a.b axb", "a.b").starts).toEqual([0]);
  });

  it("caps the number of matches and says so", () => {
    const text = "x".repeat(10);
    const capped = findTextMatches(text, "x", 4);
    expect(capped.starts).toHaveLength(4);
    expect(capped.capped).toBe(true);
    expect(findTextMatches(text, "x", 100).capped).toBe(false);
  });

  it("defaults to the shared cap", () => {
    expect(findTextMatches("y".repeat(MATCH_CAP + 5), "y").starts).toHaveLength(
      MATCH_CAP,
    );
  });

  // Regression: the scanner used to lowercase the haystack and return offsets
  // into that copy. 'İ' (U+0130) becomes two UTF-16 units ('i̇') under
  // toLowerCase(), so every offset after it drifted — landing a highlight on the
  // wrong glyph or, when it ran off the end of the node, crashing splitText with
  // IndexSizeError. Offsets must index the ORIGINAL string.
  it("returns offsets valid in the original string after a length-changing fold", () => {
    const { starts } = findTextMatches("İx", "x");
    expect(starts).toEqual([1]); // was 2 (offset into the lowercased copy)
    expect("İx".slice(starts[0], starts[0] + "x".length)).toBe("x");
  });

  it("keeps every offset aligned when a length-changing char precedes several hits", () => {
    const text = "İ a a a"; // İ, then three 'a's at 2, 4, 6
    const { starts } = findTextMatches(text, "a");
    expect(starts).toEqual([2, 4, 6]);
    for (const start of starts) expect(text[start]).toBe("a");
  });

  it("finds no offsets when the query is absent from a folding string", () => {
    expect(findTextMatches("İstanbul", "zzz").starts).toEqual([]);
  });
});

describe("stepMatch", () => {
  it("moves forward and wraps at the end", () => {
    expect(stepMatch(0, 3, 1)).toBe(1);
    expect(stepMatch(2, 3, 1)).toBe(0);
  });

  it("moves backward and wraps at the start", () => {
    expect(stepMatch(1, 3, -1)).toBe(0);
    expect(stepMatch(0, 3, -1)).toBe(2);
  });

  it("stays at 0 when there is nothing to step through", () => {
    expect(stepMatch(0, 0, 1)).toBe(0);
    expect(stepMatch(0, 0, -1)).toBe(0);
    expect(stepMatch(0, 1, 1)).toBe(0);
  });

  it("clamps an index left stale by a shrinking match list", () => {
    expect(stepMatch(9, 3, 1)).toBe(0);
  });
});

describe("counterLabel", () => {
  it("is blank until there is a query", () => {
    expect(counterLabel("", 0, 0, false)).toBe("");
    expect(counterLabel("  ", 0, 0, false)).toBe("");
  });

  it("counts from one", () => {
    expect(counterLabel("cat", 2, 17, false)).toBe("3 of 17");
  });

  it("says so when a query finds nothing", () => {
    expect(counterLabel("cat", 0, 0, false)).toBe("No results");
  });

  it("marks a capped count", () => {
    expect(counterLabel("cat", 0, 2000, true)).toBe("1 of 2000+");
  });
});

describe("findCellMatches", () => {
  const sheets = [
    [
      ["Name", "City"],
      ["Kate", "Boston"],
    ],
    [["catalog", "cat cat"]],
  ];

  it("locates matches by sheet, row and column", () => {
    const { matches } = findCellMatches(sheets, "kat");
    expect(matches).toEqual([{ sheet: 0, row: 1, col: 0, occurrence: 0 }]);
  });

  it("numbers occurrences within a sheet in row-major order", () => {
    const { matches } = findCellMatches(sheets, "cat");
    expect(matches).toEqual([
      { sheet: 1, row: 0, col: 0, occurrence: 0 },
      { sheet: 1, row: 0, col: 1, occurrence: 1 },
      { sheet: 1, row: 0, col: 1, occurrence: 2 },
    ]);
  });

  it("finds nothing for an empty query", () => {
    expect(findCellMatches(sheets, "").matches).toEqual([]);
  });

  it("stops scanning once the cell budget is spent", () => {
    const big = [Array.from({ length: 10 }, () => ["cat"])];
    const { matches, capped } = findCellMatches(big, "cat", { cellCap: 4 });
    expect(matches).toHaveLength(4);
    expect(capped).toBe(true);
  });

  it("stops at the match cap", () => {
    const big = [Array.from({ length: 10 }, () => ["cat"])];
    const { matches, capped } = findCellMatches(big, "cat", { matchCap: 3 });
    expect(matches).toHaveLength(3);
    expect(capped).toBe(true);
  });
});

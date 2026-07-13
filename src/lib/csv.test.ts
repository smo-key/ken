import { describe, expect, it } from "vitest";
import {
  delimiterForPath,
  parseCsv,
  parseDelimited,
  parseTsv,
  serializeCsv,
  serializeDelimited,
  serializeTsv,
} from "./csv";

describe("delimiterForPath", () => {
  it("picks tab for .tsv, comma otherwise", () => {
    expect(delimiterForPath("data.tsv")).toBe("\t");
    expect(delimiterForPath("data.csv")).toBe(",");
    expect(delimiterForPath("weird.TSV")).toBe("\t");
    expect(delimiterForPath("no-ext")).toBe(",");
  });
});

describe("parseCsv — basics", () => {
  it("parses simple rows", () => {
    expect(parseCsv("a,b,c\n1,2,3")).toEqual([
      ["a", "b", "c"],
      ["1", "2", "3"],
    ]);
  });

  it("returns an empty grid for empty input", () => {
    expect(parseCsv("")).toEqual([]);
  });

  it("does not add a phantom row for a trailing newline", () => {
    expect(parseCsv("a,b\n")).toEqual([["a", "b"]]);
    expect(parseCsv("a,b\r\n")).toEqual([["a", "b"]]);
  });

  it("keeps genuine empty lines as a single empty field", () => {
    expect(parseCsv("a,b\n\nc,d")).toEqual([
      ["a", "b"],
      [""],
      ["c", "d"],
    ]);
  });

  it("preserves empty fields", () => {
    expect(parseCsv("a,,c")).toEqual([["a", "", "c"]]);
    expect(parseCsv(",")).toEqual([["", ""]]);
  });

  it("preserves leading/trailing whitespace", () => {
    expect(parseCsv(" a , b ")).toEqual([[" a ", " b "]]);
  });
});

describe("parseCsv — line endings", () => {
  it("handles LF, CRLF, and bare CR", () => {
    expect(parseCsv("a\nb")).toEqual([["a"], ["b"]]);
    expect(parseCsv("a\r\nb")).toEqual([["a"], ["b"]]);
    expect(parseCsv("a\rb")).toEqual([["a"], ["b"]]);
  });
});

describe("parseCsv — quoting", () => {
  it("parses quoted fields", () => {
    expect(parseCsv('"a","b"')).toEqual([["a", "b"]]);
  });

  it("handles embedded delimiters inside quotes", () => {
    expect(parseCsv('"a,b",c')).toEqual([["a,b", "c"]]);
  });

  it("handles embedded newlines inside quotes", () => {
    expect(parseCsv('"line1\nline2",c')).toEqual([["line1\nline2", "c"]]);
    expect(parseCsv('"line1\r\nline2",c')).toEqual([["line1\r\nline2", "c"]]);
  });

  it("handles escaped quotes (doubled)", () => {
    expect(parseCsv('"a""b"')).toEqual([['a"b']]);
    expect(parseCsv('"say ""hi""",x')).toEqual([['say "hi"', "x"]]);
  });

  it("parses an explicitly empty quoted field", () => {
    expect(parseCsv('"",x')).toEqual([["", "x"]]);
  });

  it("tolerates an unterminated quoted field", () => {
    expect(parseCsv('"unterminated')).toEqual([["unterminated"]]);
  });
});

describe("serializeCsv", () => {
  it("serializes simple grids with LF", () => {
    expect(
      serializeCsv([
        ["a", "b"],
        ["1", "2"],
      ]),
    ).toBe("a,b\n1,2");
  });

  it("quotes fields that need it", () => {
    expect(serializeCsv([["a,b", "c"]])).toBe('"a,b",c');
    expect(serializeCsv([['a"b']])).toBe('"a""b"');
    expect(serializeCsv([["line1\nline2"]])).toBe('"line1\nline2"');
    expect(serializeCsv([["has\rcr"]])).toBe('"has\rcr"');
  });

  it("does not quote fields with plain whitespace", () => {
    expect(serializeCsv([[" a ", "b"]])).toBe(" a ,b");
  });

  it("honors a custom newline", () => {
    expect(serializeCsv([["a"], ["b"]], "\r\n")).toBe("a\r\nb");
  });

  it("serializes an empty grid to empty string", () => {
    expect(serializeCsv([])).toBe("");
  });
});

describe("round-trip fidelity", () => {
  const grids: string[][][] = [
    [
      ["Name", "Email", "Notes"],
      ["Ada", "ada@x.io", "first, programmer"],
      ["Grace", "grace@navy.mil", 'said "hello"'],
      ["Alan", "", "multi\nline\nnote"],
      ["", "", ""],
    ],
    [
      ["a", "b"],
      ["", "d"],
      ["e", ""],
    ],
    [["single"]],
    [
      ["x\r\ny", "tab\tinside"],
      ["comma,here", 'quote"here'],
    ],
  ];

  it("parse(serialize(grid)) === grid for non-degenerate grids", () => {
    for (const grid of grids) {
      expect(parseCsv(serializeCsv(grid))).toEqual(grid);
    }
  });

  it("serialize(parse(text)) is stable across a second round", () => {
    const text =
      'Name,Email,Notes\r\nAda,ada@x.io,"first, programmer"\r\nGrace,,"said ""hi"""\r\n';
    const once = serializeCsv(parseCsv(text));
    const twice = serializeCsv(parseCsv(once));
    expect(twice).toBe(once);
  });
});

describe("TSV", () => {
  it("parses tab-delimited text and leaves commas alone", () => {
    expect(parseTsv("a\tb,c\t d ")).toEqual([["a", "b,c", " d "]]);
  });

  it("round-trips a tab-delimited grid", () => {
    const grid = [
      ["h1", "h2"],
      ["v,1", "v\t2 quoted"],
    ];
    expect(parseTsv(serializeTsv(grid))).toEqual(grid);
  });

  it("quotes a field only when it contains the active delimiter", () => {
    // Comma is fine unquoted in TSV; a tab forces quoting.
    expect(serializeDelimited([["a,b"]], "\t")).toBe("a,b");
    expect(serializeDelimited([["a\tb"]], "\t")).toBe('"a\tb"');
  });
});

describe("parseDelimited symmetry", () => {
  it("comma parse equals parseCsv", () => {
    expect(parseDelimited("x,y", ",")).toEqual(parseCsv("x,y"));
  });
});

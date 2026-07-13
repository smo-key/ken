import { describe, expect, it } from "vitest";
import { joinSource, parseNotebook, stripAnsi } from "./ipynb";

describe("joinSource", () => {
  it("passes strings through and joins arrays with no separator", () => {
    expect(joinSource("# hi\n")).toBe("# hi\n");
    expect(joinSource(["a\n", "b\n", "c"])).toBe("a\nb\nc");
    expect(joinSource(undefined)).toBe("");
    expect(joinSource(null)).toBe("");
  });
});

describe("stripAnsi", () => {
  it("removes SGR color codes but keeps text", () => {
    const colored = "[0;31mValueError[0m: bad";
    expect(stripAnsi(colored)).toBe("ValueError: bad");
  });

  it("leaves plain text (and brackets) untouched", () => {
    expect(stripAnsi("arr[0] = value")).toBe("arr[0] = value");
    expect(stripAnsi("C:\\path")).toBe("C:\\path");
  });
});

describe("parseNotebook", () => {
  it("parses markdown and code cells with array/string sources", () => {
    const nb = {
      cells: [
        { cell_type: "markdown", source: ["# Title\n", "body"] },
        { cell_type: "code", source: "print(1)", execution_count: 4, outputs: [] },
      ],
      nbformat: 4,
    };
    const out = parseNotebook(JSON.stringify(nb));
    expect(out.cells).toHaveLength(2);
    expect(out.cells[0]).toMatchObject({ type: "markdown", source: "# Title\nbody" });
    expect(out.cells[1]).toMatchObject({
      type: "code",
      source: "print(1)",
      executionCount: 4,
    });
  });

  it("accepts an already-parsed object", () => {
    const out = parseNotebook({ cells: [{ cell_type: "raw", source: "x" }] });
    expect(out.cells[0]).toMatchObject({ type: "raw", source: "x" });
  });

  it("normalizes stream output", () => {
    const out = parseNotebook({
      cells: [
        {
          cell_type: "code",
          source: "",
          outputs: [{ output_type: "stream", name: "stdout", text: ["line1\n", "line2"] }],
        },
      ],
    });
    expect(out.cells[0].outputs).toEqual([
      { kind: "stream", name: "stdout", text: "line1\nline2" },
    ]);
  });

  it("normalizes execute_result text/plain", () => {
    const out = parseNotebook({
      cells: [
        {
          cell_type: "code",
          source: "1+1",
          execution_count: 2,
          outputs: [
            {
              output_type: "execute_result",
              execution_count: 2,
              data: { "text/plain": ["2"] },
            },
          ],
        },
      ],
    });
    expect(out.cells[0].outputs).toEqual([{ kind: "text", text: "2" }]);
  });

  it("prefers image/png over text/plain in display_data", () => {
    const out = parseNotebook({
      cells: [
        {
          cell_type: "code",
          source: "plot()",
          outputs: [
            {
              output_type: "display_data",
              data: {
                "text/plain": ["<Figure>"],
                "image/png": "iVBORw0KGgo=\n",
              },
            },
          ],
        },
      ],
    });
    expect(out.cells[0].outputs).toEqual([
      { kind: "image", mime: "image/png", data: "iVBORw0KGgo=" },
    ]);
  });

  it("falls back to image/jpeg", () => {
    const out = parseNotebook({
      cells: [
        {
          cell_type: "code",
          source: "",
          outputs: [
            { output_type: "display_data", data: { "image/jpeg": "/9j/4AAQ" } },
          ],
        },
      ],
    });
    expect(out.cells[0].outputs[0]).toMatchObject({
      kind: "image",
      mime: "image/jpeg",
    });
  });

  it("normalizes error tracebacks and strips ANSI", () => {
    const out = parseNotebook({
      cells: [
        {
          cell_type: "code",
          source: "boom()",
          outputs: [
            {
              output_type: "error",
              ename: "ValueError",
              evalue: "bad",
              traceback: ["[0;31mValueError[0m: bad", "  at line 1"],
            },
          ],
        },
      ],
    });
    expect(out.cells[0].outputs[0]).toEqual({
      kind: "error",
      ename: "ValueError",
      evalue: "bad",
      traceback: "ValueError: bad\n  at line 1",
    });
  });

  it("drops unrenderable outputs (e.g. only text/html)", () => {
    const out = parseNotebook({
      cells: [
        {
          cell_type: "code",
          source: "",
          outputs: [{ output_type: "display_data", data: { "text/html": "<b>x</b>" } }],
        },
      ],
    });
    expect(out.cells[0].outputs).toEqual([]);
  });

  it("leaves execution_count null when unexecuted", () => {
    const out = parseNotebook({
      cells: [{ cell_type: "code", source: "", execution_count: null, outputs: [] }],
    });
    expect(out.cells[0].executionCount).toBeNull();
  });

  it("skips unknown cell types", () => {
    const out = parseNotebook({
      cells: [
        { cell_type: "markdown", source: "keep" },
        { cell_type: "mystery", source: "drop" },
      ],
    });
    expect(out.cells).toHaveLength(1);
  });

  it("throws on non-JSON and on missing cells", () => {
    expect(() => parseNotebook("{not json")).toThrow();
    expect(() => parseNotebook("{}")).toThrow(/cells/);
  });
});

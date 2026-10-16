import { describe, expect, it } from "vitest";
import { buildBlockRows, markdownBlocks } from "./blockDiff";

describe("markdownBlocks", () => {
  it("cuts a page into its frontmatter, headings, paragraphs, tables and fences", () => {
    const page = "---\nstatus: current\n---\n# Vocabulary\nIntro line.\n\n| old | new |\n|---|---|\n| a | b |\n\n```js\nx();\n\ny();\n```\n- one\n- two\n";
    expect(markdownBlocks(page)).toEqual([
      "---\nstatus: current\n---",
      "# Vocabulary",
      "Intro line.",
      "| old | new |\n|---|---|\n| a | b |",
      "```js\nx();\n\ny();\n```",
      "- one\n- two",
    ]);
  });
});

describe("buildBlockRows", () => {
  it("marks a changed block removed and added, and keeps a block of context", () => {
    const a = "# Save\n\nOne file per region.\n\n## Format\n\nJSON.\n";
    const b = "# Save\n\nOne file for the world.\n\n## Format\n\nJSON.\n";
    expect(buildBlockRows(a, b)).toEqual([
      { type: "ctx", text: "# Save", frontmatter: false },
      { type: "del", text: "One file per region.", frontmatter: false },
      { type: "add", text: "One file for the world.", frontmatter: false },
      { type: "ctx", text: "## Format", frontmatter: false },
      { type: "gap", count: 1 },
    ]);
  });

  it("folds long runs of unchanged blocks by their lines", () => {
    const same = "A.\n\nB.\n\nC.\n\n| x |\n|---|\n| 1 |\n";
    const rows = buildBlockRows(`${same}\nOld.\n`, `${same}\nNew.\n`);
    expect(rows[0]).toEqual({ type: "gap", count: 3 });
    expect(rows.slice(1).map((r) => r.type)).toEqual(["ctx", "del", "add"]);
  });

  it("knows the frontmatter, so it is not drawn as Markdown", () => {
    const rows = buildBlockRows("---\nstatus: current\n---\n# A\n", "---\nstatus: superseded\n---\n# A\n");
    expect(rows.filter((r) => r.type !== "gap" && r.frontmatter).map((r) => r.type)).toEqual(["del", "add"]);
  });
});

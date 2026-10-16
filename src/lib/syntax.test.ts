import { describe, expect, it } from "vitest";
import { HIGHLIGHT_MAX, highlightHtml, parserFor } from "./syntax";

/** What the HTML shows, as text: it must match the code character for
 *  character, or the colours drift off the words in the textarea above. */
function shown(html: string): string {
  return html.replace(/<[^>]+>/g, "").replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&amp;/g, "&");
}

describe("syntax", () => {
  it("finds a language by the file's name and colours its tokens", async () => {
    const parser = await parserFor("src/game/main.py");
    expect(parser).not.toBeNull();
    const code = "def area(r):\n    return 3.14 * r * r  # a < b & c\n";
    const html = highlightHtml(code, parser!)!;
    expect(html).toContain('<span class="tok-keyword">def</span>');
    expect(html).toContain('<span class="tok-number">3.14</span>');
    expect(html).toContain('class="tok-comment"');
    expect(html).toContain("&lt; b &amp; c");
    expect(shown(html)).toBe(`${code} `);
  });

  it("keeps each line where the textarea has it", async () => {
    const parser = await parserFor("lib.rs");
    const code = "fn main() {\r\n    let x = \"<y>\";\r\n}";
    const html = highlightHtml(code, parser!)!;
    expect(shown(html)).toBe(code.replace(/\r/g, ""));
    expect(html.split("\n")).toHaveLength(3);
  });

  it("leaves an unknown kind of file, and a large one, plain", async () => {
    expect(await parserFor("notes.unknownext")).toBeNull();
    const parser = await parserFor("app.ts");
    expect(highlightHtml("x".repeat(HIGHLIGHT_MAX + 1), parser!)).toBeNull();
  });
});

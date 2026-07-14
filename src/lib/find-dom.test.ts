import { describe, expect, it } from "vitest";
import { clearHighlights, highlightMatches, setCurrent } from "./find-dom";

function host(html: string): HTMLElement {
  const el = document.createElement("div");
  el.innerHTML = html;
  return el;
}

describe("highlightMatches", () => {
  it("wraps every hit in a mark", () => {
    const el = host("<p>Cat and cat</p>");
    const marks = highlightMatches(el, "cat");
    expect(marks).toHaveLength(2);
    expect(el.querySelectorAll("mark")).toHaveLength(2);
    expect(marks[0].textContent).toBe("Cat"); // the original casing survives
  });

  it("keeps the surrounding text intact", () => {
    const el = host("<p>a cat sat</p>");
    highlightMatches(el, "cat");
    expect(el.textContent).toBe("a cat sat");
  });

  it("crosses element boundaries by marking each text node it hits", () => {
    const el = host("<p>cat</p><div><span>cat</span> cat</div>");
    expect(highlightMatches(el, "cat")).toHaveLength(3);
  });

  it("skips script and style text", () => {
    const el = host("<style>.cat {}</style><p>cat</p>");
    expect(highlightMatches(el, "cat")).toHaveLength(1);
  });

  it("finds nothing for an empty query", () => {
    const el = host("<p>cat</p>");
    expect(highlightMatches(el, "  ")).toHaveLength(0);
    expect(el.querySelector("mark")).toBeNull();
  });

  it("honours the match cap", () => {
    const el = host("<p>cat cat cat cat</p>");
    expect(highlightMatches(el, "cat", { matchCap: 2 })).toHaveLength(2);
  });

  it("highlights a hit sitting after a length-changing character without crashing", () => {
    // 'İ'.toLowerCase() is two UTF-16 units, so the old offset drifted to the end
    // of the node and splitText threw IndexSizeError — the whole document lost its
    // highlights. The mark must land on the real 'x'.
    const el = host("<p>İx</p>");
    const marks = highlightMatches(el, "x");
    expect(marks).toHaveLength(1);
    expect(marks[0].textContent).toBe("x");
    expect(el.textContent).toBe("İx");
  });
});

describe("clearHighlights", () => {
  it("restores the original DOM exactly", () => {
    const original = "<p>a <b>cat</b> and a cat</p>";
    const el = host(original);
    highlightMatches(el, "cat");
    expect(el.innerHTML).not.toBe(original);
    clearHighlights(el);
    expect(el.innerHTML).toBe(original);
    expect(el.querySelector("mark")).toBeNull();
  });

  it("is safe to run when nothing was highlighted", () => {
    const el = host("<p>cat</p>");
    clearHighlights(el);
    expect(el.innerHTML).toBe("<p>cat</p>");
  });
});

describe("setCurrent", () => {
  it("accents exactly one mark", () => {
    const el = host("<p>cat cat cat</p>");
    const marks = highlightMatches(el, "cat");
    setCurrent(marks, 1);
    expect(marks.filter((m) => m.classList.contains("ken-find-current"))).toEqual([
      marks[1],
    ]);
    setCurrent(marks, 2);
    expect(marks[1].classList.contains("ken-find-current")).toBe(false);
    expect(marks[2].classList.contains("ken-find-current")).toBe(true);
  });

  it("ignores an out-of-range index", () => {
    const el = host("<p>cat</p>");
    const marks = highlightMatches(el, "cat");
    setCurrent(marks, 5);
    expect(el.querySelector(".ken-find-current")).toBeNull();
  });
});

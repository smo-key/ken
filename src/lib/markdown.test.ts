import { describe, expect, it } from "vitest";
import { renderSearchSnippet } from "./markdown";

describe("renderSearchSnippet", () => {
  it("escapes plain text with no markup", () => {
    expect(renderSearchSnippet("plain text")).toBe("plain text");
    expect(renderSearchSnippet("a & b < c > d")).toBe(
      "a &amp; b &lt; c &gt; d",
    );
  });

  it("re-allows the backend's <mark> highlight tags", () => {
    expect(renderSearchSnippet("the <mark>quick</mark> fox")).toBe(
      "the <mark>quick</mark> fox",
    );
  });

  // The result is fed to Svelte {@html}; anything but <mark> must stay inert.
  it("keeps an injected <script> escaped", () => {
    expect(
      renderSearchSnippet("<script>alert('xss')</script>"),
    ).toBe("&lt;script&gt;alert('xss')&lt;/script&gt;");
  });

  it("keeps an injected <img onerror> escaped even beside real marks", () => {
    expect(
      renderSearchSnippet('<mark>hit</mark> <img src=x onerror=alert(1)>'),
    ).toBe("<mark>hit</mark> &lt;img src=x onerror=alert(1)&gt;");
  });
});

import { describe, expect, it } from "vitest";
import { pageLinkTarget, plainSnippet, renderBody, renderSearchSnippet, renderTitle } from "./markdown";

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

describe("renderTitle", () => {
  it("shows a code span as code and escapes the rest", () => {
    expect(renderTitle("Ruling: `save()` runs <first>")).toBe('Ruling: <code class="title-code">save()</code> runs &lt;first&gt;');
  });

  it("escapes inside a code span", () => {
    expect(renderTitle("`<img onerror=x>`")).toBe('<code class="title-code">&lt;img onerror=x&gt;</code>');
  });

  it("leaves a lone backtick as it is", () => {
    expect(renderTitle("a ` b")).toBe("a ` b");
  });

  it("passes each plain run through the given mark", () => {
    expect(renderTitle("x `y` z", (s) => `[${s}]`)).toBe('[x ]<code class="title-code">[y]</code>[ z]');
  });
});

describe("renderBody", () => {
  it("renders a quote, inline code and a [[page]] link", () => {
    const html = renderBody("It says `save()`.\n\n> one file\n\nFrom [[2026-10-14-meeting]].");
    expect(html).toContain("<code>save()</code>");
    expect(html).toContain("<blockquote>");
    expect(html).toContain('<a href="#ken-page:2026-10-14-meeting">2026-10-14-meeting</a>');
    expect(html).not.toContain("[[");
  });

  it("shows a link's label and keeps its page for the click", () => {
    const html = renderBody("See [[Current/Project|the project]].");
    expect(html).toContain(">the project</a>");
    const href = /href="([^"]+)"/.exec(html)?.[1] ?? null;
    expect(pageLinkTarget(href)).toBe("Current/Project");
    expect(pageLinkTarget("https://example.com")).toBeNull();
  });
});

describe("plainSnippet", () => {
  it("takes out headings, table pipes and rules, and emphasis", () => {
    expect(plainSnippet("## Pages | page | decides | |---|---| | [[Save Format]] | Why the world")).toBe(
      "Pages · page · decides · Save Format · Why the world",
    );
    expect(plainSnippet("## D-001 · world save **The whole world** saves to `saves/world.json`")).toBe(
      "D-001 · world save The whole world saves to saves/world.json",
    );
  });

  it("keeps the highlight marks", () => {
    expect(plainSnippet("# Why the <mark>world</mark> saves")).toBe("Why the <mark>world</mark> saves");
  });
});

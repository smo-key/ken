import { describe, expect, it } from "vitest";
import {
  SANDBOX,
  buildHtmlDocument,
  cspFor,
  findInHtmlDocument,
  isHtmlPath,
  resolveAssetPath,
  scrubHtml,
  type AssetReader,
} from "./html";

/** A fake project folder: paths are project-relative, exactly like the backend. */
function makeReader(files: Record<string, string | Uint8Array>): AssetReader & {
  asked: string[];
} {
  const asked: string[] = [];
  return {
    asked,
    async text(relPath) {
      asked.push(relPath);
      const value = files[relPath];
      if (value === undefined) throw new Error(`no such file: ${relPath}`);
      return typeof value === "string" ? value : new TextDecoder().decode(value);
    },
    async bytes(relPath) {
      asked.push(relPath);
      const value = files[relPath];
      if (value === undefined) throw new Error(`no such file: ${relPath}`);
      const bytes =
        typeof value === "string" ? new TextEncoder().encode(value) : value;
      return bytes.buffer.slice(
        bytes.byteOffset,
        bytes.byteOffset + bytes.byteLength,
      ) as ArrayBuffer;
    },
  };
}

const PAGE = "notes/page.html";

describe("isHtmlPath", () => {
  it("matches .html and .htm, case-insensitively", () => {
    expect(isHtmlPath("a/b/report.html")).toBe(true);
    expect(isHtmlPath("Report.HTM")).toBe(true);
    expect(isHtmlPath("notes.md")).toBe(false);
    expect(isHtmlPath("html")).toBe(false);
  });
});

describe("resolveAssetPath", () => {
  it("resolves a sibling file against the page's own folder", () => {
    expect(resolveAssetPath("notes", "app.js")).toBe("notes/app.js");
  });

  it("resolves nested and ./-prefixed paths", () => {
    expect(resolveAssetPath("notes", "./assets/logo.png")).toBe(
      "notes/assets/logo.png",
    );
    expect(resolveAssetPath("", "css/site.css")).toBe("css/site.css");
  });

  it("resolves parent-relative paths that stay inside the project", () => {
    expect(resolveAssetPath("notes/deep", "../css/site.css")).toBe(
      "notes/css/site.css",
    );
    expect(resolveAssetPath("notes", "../css/site.css")).toBe("css/site.css");
  });

  it("refuses a path that escapes the project root", () => {
    expect(resolveAssetPath("notes", "../../../etc/passwd")).toBeNull();
    expect(resolveAssetPath("", "../secrets.txt")).toBeNull();
  });

  it("refuses absolute filesystem and site-root paths", () => {
    expect(resolveAssetPath("notes", "/etc/passwd")).toBeNull();
    expect(resolveAssetPath("notes", "file:///etc/passwd")).toBeNull();
  });

  it("refuses anything remote, embedded, or in-document", () => {
    expect(resolveAssetPath("notes", "https://cdn.example/x.js")).toBeNull();
    expect(resolveAssetPath("notes", "//cdn.example/x.js")).toBeNull();
    expect(resolveAssetPath("notes", "data:image/gif;base64,AA")).toBeNull();
    expect(resolveAssetPath("notes", "#top")).toBeNull();
  });

  it("ignores cache-busting queries and fragments, and decodes escapes", () => {
    expect(resolveAssetPath("notes", "site.css?v=2#x")).toBe("notes/site.css");
    expect(resolveAssetPath("notes", "my%20styles.css")).toBe(
      "notes/my styles.css",
    );
  });

  // A percent-encoded separator must normalize to a real one: `%2F` splits the
  // ref into segments and `..%2F` pops a parent, exactly as the literal forms
  // do — otherwise a valid encoded asset never resolves and the frontend's
  // own escape guard is bypassed.
  it("treats an encoded separator the same as a literal one", () => {
    expect(resolveAssetPath("notes/deep", "..%2Fcss%2Fsite.css")).toBe(
      resolveAssetPath("notes/deep", "../css/site.css"),
    );
    expect(resolveAssetPath("notes/deep", "..%2Fcss%2Fsite.css")).toBe(
      "notes/css/site.css",
    );
    expect(resolveAssetPath("notes", "my%20file.css")).toBe("notes/my file.css");
  });

  it("refuses an encoded escape just like its literal form", () => {
    expect(resolveAssetPath("notes", "..%2F..%2Fetc")).toBeNull();
    expect(resolveAssetPath("notes", "../../etc")).toBeNull();
    // An encoded leading separator is an absolute path in disguise.
    expect(resolveAssetPath("notes", "%2Fetc%2Fpasswd")).toBeNull();
  });
});

describe("buildHtmlDocument", () => {
  it("inlines a local stylesheet as a <style>", async () => {
    const reader = makeReader({ "css/site.css": "h1{color:red}" });
    const doc = await buildHtmlDocument(
      `<html><head><link rel="stylesheet" href="../css/site.css"></head><body><h1>Hi</h1></body></html>`,
      PAGE,
      reader,
    );
    expect(doc).toContain("h1{color:red}");
    expect(doc).not.toContain("<link");
    expect(reader.asked).toContain("css/site.css");
  });

  it("inlines a sibling script so the page's own code can run", async () => {
    const reader = makeReader({ "notes/app.js": `document.title="ok"` });
    const doc = await buildHtmlDocument(
      `<body><script src="app.js"></script></body>`,
      PAGE,
      reader,
    );
    expect(doc).toContain(`document.title="ok"`);
    expect(doc).not.toContain('src="app.js"');
  });

  it("turns a nested local image into a data URI", async () => {
    const reader = makeReader({
      "notes/assets/logo.png": new Uint8Array([1, 2, 3]),
    });
    const doc = await buildHtmlDocument(
      `<body><img src="./assets/logo.png"></body>`,
      PAGE,
      reader,
    );
    expect(doc).toContain("data:image/png;base64,");
    expect(doc).not.toContain("assets/logo.png");
  });

  it("follows an @import one level inside an inlined stylesheet", async () => {
    const reader = makeReader({
      "css/site.css": `@import "base.css"; h1{color:red}`,
      "css/base.css": "body{margin:0}",
    });
    const doc = await buildHtmlDocument(
      `<head><link rel="stylesheet" href="../css/site.css"></head><body></body>`,
      PAGE,
      reader,
    );
    expect(doc).toContain("body{margin:0}");
    expect(doc).toContain("h1{color:red}");
    expect(doc).not.toContain("@import");
  });

  it("stops at the @import depth cap instead of recursing forever", async () => {
    const reader = makeReader({
      "css/site.css": `@import "base.css";`,
      "css/base.css": `@import "deeper.css"; body{margin:0}`,
      "css/deeper.css": "p{padding:9px}",
    });
    const doc = await buildHtmlDocument(
      `<head><link rel="stylesheet" href="../css/site.css"></head><body></body>`,
      PAGE,
      reader,
    );
    expect(doc).toContain("body{margin:0}");
    expect(doc).not.toContain("p{padding:9px}");
    expect(doc).not.toContain("@import");
  });

  it("rewrites url() inside CSS relative to the stylesheet's own folder", async () => {
    const reader = makeReader({
      "css/site.css": "body{background:url(../img/bg.png)}",
      "img/bg.png": new Uint8Array([7, 7]),
    });
    const doc = await buildHtmlDocument(
      `<head><link rel="stylesheet" href="../css/site.css"></head><body></body>`,
      PAGE,
      reader,
    );
    expect(doc).toContain("data:image/png;base64,");
    expect(reader.asked).toContain("img/bg.png");
  });

  it("inlines a font referenced from an inline <style>", async () => {
    const reader = makeReader({ "notes/fonts/f.woff2": new Uint8Array([9]) });
    const doc = await buildHtmlDocument(
      `<head><style>@font-face{src:url("fonts/f.woff2")}</style></head><body></body>`,
      PAGE,
      reader,
    );
    expect(doc).toContain("data:font/woff2;base64,");
  });

  it("skips a missing asset and still renders the page", async () => {
    const reader = makeReader({});
    const doc = await buildHtmlDocument(
      `<head><link rel="stylesheet" href="gone.css"></head><body><p>Body text</p><img src="gone.png"><script src="gone.js"></script></body>`,
      PAGE,
      reader,
    );
    expect(doc).toContain("Body text");
    expect(doc).not.toContain("<link");
    expect(doc).not.toContain("gone.png");
    expect(doc).not.toContain("gone.js");
  });

  it("never reads a path that escapes the project root", async () => {
    const reader = makeReader({});
    const doc = await buildHtmlDocument(
      `<body><img src="../../../etc/passwd"><script src="/etc/hosts"></script></body>`,
      PAGE,
      reader,
    );
    expect(reader.asked).toEqual([]);
    expect(doc).not.toContain("passwd");
    expect(doc).not.toContain("/etc/hosts");
  });

  const REMOTE = `<head><link rel="stylesheet" href="https://cdn.example/a.css"></head>
    <body><script src="https://cdn.example/a.js"></script><img src="https://cdn.example/px.gif">
    <style>@import url("https://cdn.example/b.css"); body{background:url(https://cdn.example/bg.png)}</style></body>`;

  it("blocks every remote asset by default", async () => {
    const doc = await buildHtmlDocument(REMOTE, PAGE, makeReader({}));
    expect(doc).not.toContain("cdn.example");
    expect(doc).toContain("default-src 'none'");
    expect(doc).toContain("connect-src 'none'");
    expect(doc).not.toContain("https:");
  });

  it("lets the page reach https once network is allowed", async () => {
    const doc = await buildHtmlDocument(REMOTE, PAGE, makeReader({}), {
      network: true,
    });
    expect(doc).toContain('href="https://cdn.example/a.css"');
    expect(doc).toContain('src="https://cdn.example/a.js"');
    expect(doc).toContain('src="https://cdn.example/px.gif"');
    expect(doc).toContain("@import");
    expect(doc).toContain("url(https://cdn.example/bg.png)");
    expect(doc).toContain("script-src 'unsafe-inline' https:");
    expect(doc).toContain("connect-src https:");
  });

  it("still inlines local assets in network mode", async () => {
    const reader = makeReader({ "notes/app.js": "run()" });
    const doc = await buildHtmlDocument(
      `<body><script src="app.js"></script></body>`,
      PAGE,
      reader,
      { network: true },
    );
    expect(doc).toContain("run()");
  });

  it("keeps outbound links from navigating the frame", async () => {
    const doc = await buildHtmlDocument(
      `<body><a href="https://example.com/x">out</a><a href="#top">top</a></body>`,
      PAGE,
      makeReader({}),
    );
    expect(doc).not.toContain('href="https://example.com/x"');
    expect(doc).toContain("Link disabled in preview: https://example.com/x");
    expect(doc).toContain('href="#top"');
  });

  it("drops framing, redirecting and base-rewriting tags", async () => {
    const doc = await buildHtmlDocument(
      `<head><base href="https://evil.example/"><meta http-equiv="refresh" content="0;url=https://evil.example"></head>
       <body><iframe src="https://evil.example"></iframe><object data="x"></object></body>`,
      PAGE,
      makeReader({}),
    );
    expect(doc).not.toContain("<base");
    expect(doc).not.toContain("http-equiv=\"refresh\"");
    expect(doc).not.toContain("<iframe");
    expect(doc).not.toContain("<object");
    expect(doc).not.toContain("evil.example");
  });
});

describe("the frame's policy", () => {
  it("runs scripts in an opaque origin and never grants same-origin", () => {
    expect(SANDBOX).toContain("allow-scripts");
    expect(SANDBOX).not.toContain("allow-same-origin");
  });

  it("blocks the network by default and only widens to https on request", () => {
    expect(cspFor(false)).toContain("default-src 'none'");
    expect(cspFor(false)).not.toContain("https:");
    expect(cspFor(false)).toContain("img-src data:");
    expect(cspFor(true)).toContain("default-src 'none'");
    expect(cspFor(true)).toContain("https:");
    // Forms never post anywhere, in either mode.
    expect(cspFor(false)).toContain("form-action 'none'");
    expect(cspFor(true)).toContain("form-action 'none'");
  });
});

describe("scrubHtml", () => {
  it("strips javascript: urls and remote resources", () => {
    const out = scrubHtml(
      `<a href="javascript:alert(1)">go</a><img src="https://tracker.example/px.gif"><img src="data:image/gif;base64,AA">`,
    );
    expect(out).not.toContain("javascript:");
    expect(out).not.toContain("tracker.example");
    expect(out).toContain("data:image/gif;base64,AA");
  });

  it("keeps inline styling so the page still reads like itself", () => {
    const out = scrubHtml(
      `<style>h1 { color: red }</style><h1 style="margin:0">Title</h1>`,
    );
    expect(out).toContain("color: red");
    expect(out).toContain('style="margin:0"');
  });

  it("defuses CSS that fetches when the network is off", () => {
    const out = scrubHtml(
      `<style>@import url("https://evil.example/x.css"); body { background: url(https://evil.example/px.png) }</style>`,
    );
    expect(out).not.toContain("@import");
    expect(out).not.toContain("evil.example");
    expect(out).toContain("url()");
  });
});

describe("findInHtmlDocument", () => {
  async function page(opts?: { network?: boolean }) {
    return buildHtmlDocument(
      `<html><head><style>h1{color:red}</style></head>
       <body><h1>Cat report</h1><p>The cat sat.</p><script>run()</script></body></html>`,
      PAGE,
      makeReader({}),
      opts,
    );
  }

  it("counts every hit in the rendered text", async () => {
    expect(findInHtmlDocument(await page(), "cat").total).toBe(2);
  });

  it("marks the hits inside the document it returns", async () => {
    const { doc } = findInHtmlDocument(await page(), "cat");
    expect(doc).toContain('<mark class="ken-find">Cat</mark>');
    expect(doc).toContain('<mark class="ken-find">cat</mark>');
  });

  it("keeps the frame's Content-Security-Policy in both modes", async () => {
    const off = findInHtmlDocument(await page(), "cat").doc;
    expect(off).toContain("Content-Security-Policy");
    expect(off).toContain("default-src 'none'");
    expect(off).not.toContain("https:");

    const on = findInHtmlDocument(await page({ network: true }), "cat").doc;
    expect(on).toContain("Content-Security-Policy");
    expect(on).toContain("connect-src https:");
  });

  it("leaves the page's own scripts and styles intact", async () => {
    const { doc } = findInHtmlDocument(await page(), "cat");
    expect(doc).toContain("run()");
    expect(doc).toContain("h1{color:red}");
  });

  it("gives the document back untouched when nothing is searched", async () => {
    const built = await page();
    const { doc, total } = findInHtmlDocument(built, "");
    expect(total).toBe(0);
    expect(doc).toBe(built);
  });
});

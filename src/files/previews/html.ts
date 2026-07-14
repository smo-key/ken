// Turns an arbitrary HTML document from the shared drive into something safe to
// show. Everything here assumes the file is hostile: it may carry scripts,
// trackers, remote beacons, or links that try to steer the app window.
//
// A page is rarely one file: it points at a stylesheet, a script, a logo in an
// adjacent folder. The frame gets no `file://` base URL (a sandboxed srcdoc has
// an opaque origin and could not resolve one anyway), so anything the page needs
// has to be read through the project API and carried *inside* the document. That
// is what the inline pass below does — and it is also the only reason the page's
// own code can be trusted to run: everything it uses is local, and the frame it
// runs in can neither reach the network nor reach Ken.
import { MATCH_CAP } from "../../lib/find";
import { highlightMatches } from "../../lib/find-dom";
import { base64, mimeForPath } from "../../lib/mime";

/**
 * The frame's privileges. `allow-scripts` WITHOUT `allow-same-origin` is the
 * whole design: the two together would give the page a real origin and let it
 * reach out of the frame into the app — they must never both be set. Scripts
 * therefore run in an opaque origin, with no cookies, no storage, no parent
 * access, no Tauri. Not `allow-top-navigation`, not `allow-popups`, not
 * `allow-forms`: a preview never leaves the pane.
 */
export const SANDBOX = "allow-scripts";

/**
 * Second lock, independent of the sandbox: what the document may fetch.
 * Everything it legitimately needs is already inlined (`data:` / inline text),
 * so the default policy has no network reach at all. The network-allowed mode is
 * a deliberate, per-document choice by the user and widens it to https only.
 */
export function cspFor(network: boolean): string {
  const remote = network ? " https:" : "";
  return [
    `default-src 'none'`,
    `img-src data:${remote}`,
    `font-src data:${remote}`,
    `media-src data:${remote}`,
    `style-src 'unsafe-inline'${remote}`,
    `script-src 'unsafe-inline'${remote}`,
    `connect-src ${network ? "https:" : "'none'"}`,
    // A form submit is a network request with the page's data in it. Blocked in
    // both modes: allowing the network is about *loading* a page, not posting.
    `form-action 'none'`,
  ].join("; ");
}

/** Elements that re-frame the document, redirect it, or rewrite its base. */
const FORBID_TAGS = [
  "iframe", "frame", "frameset", "object", "embed", "applet",
  "base", "meta", "title", "noscript",
  // Media would need whole files inlined as data URIs to work at all; a preview
  // is not a player, and their src attributes are prime beacon material.
  "audio", "video", "source", "track",
];

/** Attributes that reach out to the network, even on otherwise benign tags. */
const FORBID_ATTR = [
  "srcset", "ping", "action", "formaction", "background", "poster", "data",
  "integrity", "crossorigin", "target", "manifest",
];

/**
 * How deep a stylesheet's `@import` chain is followed: the sheet the page links,
 * plus one level of imports inside it. Deeper imports are dropped — the frame
 * cannot fetch them, and a real page's `@import` chain is one level (a base
 * sheet) in practice. It also makes an import cycle terminate by construction.
 */
const CSS_IMPORT_DEPTH = 1;

/** One asset, inlined, is one base64 blob in a srcdoc string: keep it sane. */
const MAX_ASSET_BYTES = 8 * 1024 * 1024;

/** Reads project-relative files. The backend enforces the project-root check and
    downloads cloud placeholders; nothing here ever touches the filesystem. */
export interface AssetReader {
  text(relPath: string): Promise<string>;
  bytes(relPath: string): Promise<ArrayBuffer>;
}

export interface BuildOptions {
  /** The user allowed this one document to reach the network (https only). */
  network?: boolean;
}

export function isHtmlPath(relPath: string): boolean {
  const dot = relPath.lastIndexOf(".");
  if (dot < 0) return false;
  const ext = relPath.slice(dot + 1).toLowerCase();
  return ext === "html" || ext === "htm";
}

/**
 * The whole pipeline: read the page's local assets into it, scrub what is left,
 * and wrap the result in the frame's policy. Ready for an iframe `srcdoc`.
 */
export async function buildHtmlDocument(
  raw: string,
  relPath: string,
  reader: AssetReader,
  opts: BuildOptions = {},
): Promise<string> {
  const network = opts.network ?? false;
  const tpl = parseInert(raw);
  await inlineAssets(tpl.content, dirOf(relPath), reader, network);
  scrubTree(tpl.content, network);
  return wrapDocument(tpl.innerHTML, cspFor(network));
}

/**
 * Structural pass on already-inlined markup: drops re-framing elements, remote
 * resources, and outbound links. It runs after inlining, so anything still
 * pointing at a URL is either remote (a beacon, unless the user allowed the
 * network) or a local asset that could not be read (already skipped).
 */
export function scrubHtml(raw: string, opts: BuildOptions = {}): string {
  const tpl = parseInert(raw);
  scrubTree(tpl.content, opts.network ?? false);
  return tpl.innerHTML;
}

/**
 * Resolve a reference inside the page (`app.js`, `./assets/x.png`,
 * `../css/site.css`) to a project-relative path, or null if it is not a local
 * file we may read: remote, `data:`, an in-document anchor, an absolute path, or
 * anything that climbs out of the project root.
 *
 * Normalizing here is not a second copy of the backend's check — it is what
 * makes `../` work at all, since `project.resolve()` refuses any path with a
 * `..` component in it. A reference that still points above the root after
 * normalization has no project-relative form, so it is refused outright and
 * never reaches the backend; if one ever slipped through, `project.resolve()`
 * would still refuse it.
 */
export function resolveAssetPath(baseDir: string, ref: string): string | null {
  const trimmed = ref.trim();
  if (!trimmed || trimmed.startsWith("#")) return null;
  if (/^[a-z][a-z0-9+.-]*:/i.test(trimmed)) return null; // https:, data:, file:…
  if (trimmed.startsWith("//")) return null; // protocol-relative — remote
  if (trimmed.startsWith("/")) return null; // absolute path — refused

  const path = trimmed.split(/[?#]/)[0];
  if (!path) return null;

  // Decode BEFORE splitting on '/': a percent-encoded separator (`%2F`) has to
  // normalize to a real one, or a token like `..%2Fcss` stays opaque — its `..`
  // never pops a parent and a valid encoded asset silently fails to resolve
  // (and the escape guard below is bypassed). Decode per token, then re-split,
  // so a single token that decodes to several segments is handled correctly.
  const decoded = path
    .split("/")
    .map(decodeSegment)
    .join("/");
  // A decoded leading separator is an absolute path in disguise; refuse it the
  // same as the literal `/…` form caught above.
  if (decoded.startsWith("/")) return null;

  const out: string[] = [];
  for (const seg of [...baseDir.split("/"), ...decoded.split("/")]) {
    if (!seg || seg === ".") continue;
    if (seg === "..") {
      if (out.length === 0) return null; // escapes the project root
      out.pop();
      continue;
    }
    out.push(seg);
  }
  return out.length > 0 ? out.join("/") : null;
}

/**
 * Find inside a preview page. Rebuilding the document with the hits already
 * marked reloads the frame, so it is done once per query — never per step, which
 * would bounce the reader to the top. The page's head (and with it the frame's
 * Content-Security-Policy) is spliced through untouched, whichever mode the
 * document was built in.
 */
export function findInHtmlDocument(
  doc: string,
  query: string,
  cap = MATCH_CAP,
): { doc: string; total: number; capped: boolean } {
  if (!query.trim()) return { doc, total: 0, capped: false };

  const open = doc.indexOf("<body>");
  const close = doc.lastIndexOf("</body>");
  if (open < 0 || close < 0) return { doc, total: 0, capped: false };

  // A <template> is inert: nothing in it loads, runs, or renders while we walk
  // it — the page's own scripts included.
  const tpl = document.createElement("template");
  tpl.innerHTML = doc.slice(open + 6, close);
  const holder = document.createElement("div");
  holder.appendChild(tpl.content);

  const marks = highlightMatches(holder, query, { matchCap: cap });
  if (marks.length === 0) return { doc, total: 0, capped: false };

  const body = FIND_STYLE + holder.innerHTML;
  return {
    doc: doc.slice(0, open + 6) + body + doc.slice(close),
    total: marks.length,
    capped: marks.length >= cap,
  };
}

/** Highlight colour has to travel with the frame: the app's CSS can't reach in. */
const FIND_STYLE =
  "<style>mark.ken-find{background:#f2c98a;color:inherit;border-radius:2px}</style>";

function wrapDocument(body: string, csp: string): string {
  return `<!doctype html><html><head><meta http-equiv="Content-Security-Policy" content="${csp}"></head><body>${body}</body></html>`;
}

/**
 * A <template> is inert: unlike a detached <div>, filling it never kicks off an
 * image load or runs a script, so a hostile page is defused before we touch it.
 * <html>/<head>/<body> are flattened away first — as body content the parser
 * still honours <style> and <script>, so the page keeps everything that matters
 * and loses its head.
 */
function parseInert(raw: string): HTMLTemplateElement {
  const tpl = document.createElement("template");
  tpl.innerHTML = raw.replace(/<\/?(?:html|head|body)\b[^>]*>/gi, " ");
  return tpl;
}

// ---------------------------------------------------------------- inlining

async function inlineAssets(
  root: DocumentFragment,
  baseDir: string,
  reader: AssetReader,
  network: boolean,
): Promise<void> {
  // Snapshot the page's own <style> elements before the <link> pass adds more:
  // those arrive already inlined, and their url()s resolve against the
  // stylesheet's folder, not the page's.
  const styles = Array.from(root.querySelectorAll("style"));

  for (const link of Array.from(root.querySelectorAll("link"))) {
    const rel = (link.getAttribute("rel") ?? "").toLowerCase();
    const href = link.getAttribute("href");
    if (!href || !rel.split(/\s+/).includes("stylesheet")) continue;
    const target = resolveAssetPath(baseDir, href);
    if (!target) continue; // remote or refused — scrubTree decides its fate
    const css = await readText(reader, target);
    if (css === null) {
      link.remove(); // one missing stylesheet must not cost us the page
      continue;
    }
    const style = document.createElement("style");
    style.textContent = await inlineCss(
      css,
      dirOf(target),
      reader,
      network,
      CSS_IMPORT_DEPTH,
    );
    link.replaceWith(style);
  }

  for (const style of styles) {
    style.textContent = await inlineCss(
      style.textContent ?? "",
      baseDir,
      reader,
      network,
      CSS_IMPORT_DEPTH,
    );
  }

  for (const el of Array.from(root.querySelectorAll("[style]"))) {
    const declaration = el.getAttribute("style") ?? "";
    if (!declaration.includes("url(")) continue;
    el.setAttribute(
      "style",
      await inlineCssUrls(declaration, baseDir, reader, network),
    );
  }

  for (const script of Array.from(root.querySelectorAll("script[src]"))) {
    const target = resolveAssetPath(baseDir, script.getAttribute("src") ?? "");
    if (!target) continue;
    const code = await readText(reader, target);
    if (code === null) {
      script.remove();
      continue;
    }
    script.removeAttribute("src");
    script.textContent = code;
  }

  for (const img of Array.from(root.querySelectorAll("img[src]"))) {
    const src = img.getAttribute("src") ?? "";
    if (src.startsWith("data:")) continue;
    const target = resolveAssetPath(baseDir, src);
    if (!target) continue;
    const uri = await readDataUri(reader, target);
    if (uri === null) img.removeAttribute("src");
    else img.setAttribute("src", uri);
  }
}

/** Splices local `@import`s in, then turns local `url()`s into data URIs. */
async function inlineCss(
  css: string,
  baseDir: string,
  reader: AssetReader,
  network: boolean,
  depth: number,
): Promise<string> {
  const IMPORT = /@import\s+(?:url\(\s*(['"]?)([^)'"]*)\1\s*\)|(['"])([^'"]*)\3)[^;]*;?/gi;
  const spliced = await replaceAsync(IMPORT, css, async (m, ...groups) => {
    const ref = groups[1] || groups[3] || "";
    const target = resolveAssetPath(baseDir, ref);
    if (!target) return network && isRemote(ref) ? m : "";
    if (depth <= 0) return ""; // depth cap: see CSS_IMPORT_DEPTH
    const nested = await readText(reader, target);
    if (nested === null) return "";
    return inlineCss(nested, dirOf(target), reader, network, depth - 1);
  });
  return inlineCssUrls(spliced, baseDir, reader, network);
}

async function inlineCssUrls(
  css: string,
  baseDir: string,
  reader: AssetReader,
  network: boolean,
): Promise<string> {
  const URL_REF = /url\(\s*(['"]?)([^)'"]*)\1\s*\)/gi;
  return replaceAsync(URL_REF, css, async (m, ...groups) => {
    const ref = groups[1] ?? "";
    if (!ref || ref.startsWith("data:")) return m;
    const target = resolveAssetPath(baseDir, ref);
    // A remote url() is a beacon in disguise — it identifies the reader to
    // whoever sent the file — unless the user asked for the network.
    if (!target) return network && isRemote(ref) ? m : "url()";
    const uri = await readDataUri(reader, target);
    return uri === null ? "url()" : `url("${uri}")`;
  });
}

/** String.replace with an async replacer: matches first, then rewrite. */
async function replaceAsync(
  pattern: RegExp,
  input: string,
  replacer: (match: string, ...groups: string[]) => Promise<string>,
): Promise<string> {
  const matches = Array.from(input.matchAll(pattern));
  if (matches.length === 0) return input;

  let out = "";
  let at = 0;
  for (const m of matches) {
    out += input.slice(at, m.index) + (await replacer(m[0], ...m.slice(1)));
    at = m.index + m[0].length;
  }
  return out + input.slice(at);
}

// A missing or unreadable asset is a hole in the page, never a failed render:
// the file may be cloud-only, deleted, or simply mistyped in the HTML.
async function readText(
  reader: AssetReader,
  relPath: string,
): Promise<string | null> {
  try {
    return await reader.text(relPath);
  } catch {
    return null;
  }
}

async function readDataUri(
  reader: AssetReader,
  relPath: string,
): Promise<string | null> {
  try {
    const bytes = await reader.bytes(relPath);
    if (bytes.byteLength > MAX_ASSET_BYTES) return null;
    // Unknown types fall back to octet-stream: a data URI still forms, and the
    // browser sniffs it rather than the render failing outright.
    const mime = mimeForPath(relPath) ?? "application/octet-stream";
    return `data:${mime};base64,${base64(bytes)}`;
  } catch {
    return null;
  }
}

function dirOf(relPath: string): string {
  const slash = relPath.lastIndexOf("/");
  return slash < 0 ? "" : relPath.slice(0, slash);
}

function decodeSegment(seg: string): string {
  try {
    return decodeURIComponent(seg);
  } catch {
    return seg;
  }
}

function isRemote(ref: string): boolean {
  return /^(?:https?:)?\/\//i.test(ref.trim());
}

// ---------------------------------------------------------------- scrubbing

function scrubTree(root: DocumentFragment, network: boolean): void {
  for (const el of Array.from(root.querySelectorAll("*"))) {
    const tag = el.tagName.toLowerCase();
    if (FORBID_TAGS.includes(tag)) {
      el.remove();
      continue;
    }

    for (const attr of Array.from(el.attributes)) {
      const name = attr.name.toLowerCase();
      if (
        FORBID_ATTR.includes(name) ||
        /^\s*(?:javascript|vbscript|data:text\/html)/i.test(attr.value)
      ) {
        el.removeAttribute(attr.name);
      }
    }
    // Inline `on…` handlers are left alone on purpose: the page's own <script>
    // runs in this frame, so refusing its onclick would only half-break it
    // without buying any safety the sandbox does not already provide.

    // A stylesheet Ken could not inline is either remote or unreadable.
    if (tag === "link") {
      const href = el.getAttribute("href") ?? "";
      if (!(network && isRemote(href))) el.remove();
      continue;
    }

    // Same for a script still carrying a src: local ones were inlined, so this
    // is a remote fetch.
    if (tag === "script" && el.hasAttribute("src")) {
      if (!(network && isRemote(el.getAttribute("src") ?? ""))) {
        el.remove();
        continue;
      }
    }

    // Only self-contained (data:) resources survive by default — an
    // `<img src="https://…">` in a document from a shared drive is a read
    // receipt for whoever sent it.
    const src = el.getAttribute("src");
    if (src !== null && !src.startsWith("data:") && !(network && isRemote(src))) {
      el.removeAttribute("src");
    }

    // Letting a link navigate the frame would load a remote page in place of the
    // document. Same-document jumps are harmless and stay; for real links the
    // action bar's "Open in default app" is the escape hatch.
    if (tag !== "link") {
      const href = el.getAttribute("href");
      if (href !== null && !href.startsWith("#")) {
        el.removeAttribute("href");
        if (tag === "a") {
          el.setAttribute("title", `Link disabled in preview: ${href}`);
        }
      }
    }

    // Last net over CSS the inline pass left behind (or never saw): @import and
    // url(https://…) both fetch.
    if (tag === "style" && !network) {
      el.textContent = (el.textContent ?? "")
        .replace(/@import[^;}]*;?/gi, "")
        // Written as a function, not a lookahead: a quoted url("data:…") makes
        // an optional-quote pattern backtrack and match anyway, silently
        // deleting the assets the inline pass just embedded.
        .replace(/url\(\s*(['"]?)([^)'"]*)\1\s*\)/gi, (m, _q, ref: string) =>
          ref.startsWith("data:") ? m : "url()",
        );
    }
  }
}

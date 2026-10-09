/**
 * In-document anchor links (`[Setup](#setup)`) for the Markdown editor.
 *
 * Two halves live here: the GitHub-compatible heading slug (pure, unit
 * tested) and a `$prose` plugin that turns a click on `a[href^="#"]` into a
 * smooth scroll to the matching heading instead of a webview navigation.
 *
 * Without this, a `#fragment` link inside the Tauri webview either does
 * nothing useful or replaces the app's own URL — Crepe's link tooltip only
 * edits the href, it never handles the click.
 */
import type { Node as ProseNode } from "@milkdown/kit/prose/model";
import type { EditorView } from "@milkdown/kit/prose/view";
import { Plugin, PluginKey } from "@milkdown/kit/prose/state";
import { $prose } from "@milkdown/kit/utils";

/**
 * Slugify one heading the way GitHub does: lower case, drop punctuation, and
 * turn runs of whitespace into single hyphens. Letters, digits, combining
 * marks, `_` and `-` survive; everything else is dropped.
 */
export function headingSlug(text: string): string {
  return text
    .trim()
    .toLowerCase()
    .replace(/[^\p{L}\p{N}\p{M}\s_-]+/gu, "")
    .replace(/\s+/gu, "-");
}

/**
 * Slugs for a document's headings, in document order, with GitHub's duplicate
 * rule applied: the first `Setup` is `setup`, the next `setup-1`, and so on.
 */
export function headingSlugs(texts: readonly string[]): string[] {
  const seen = new Map<string, number>();
  return texts.map((text) => {
    const base = headingSlug(text);
    const count = seen.get(base) ?? 0;
    seen.set(base, count + 1);
    return count === 0 ? base : `${base}-${count}`;
  });
}

/** Strip a leading `#` and percent-decode, so `#a%20b` matches `a b`. */
export function normalizeAnchorHref(href: string): string | null {
  if (!href.startsWith("#")) return null;
  const raw = href.slice(1);
  if (!raw) return null;
  try {
    return decodeURIComponent(raw);
  } catch {
    return raw;
  }
}

export interface HeadingEntry {
  pos: number;
  /** The heading node's size, so callers can address its end. */
  size: number;
  slug: string;
  /** Milkdown's own heading id attribute, when it has one. */
  id: string;
  text: string;
  level: number;
}

/** Every heading in the document with its position, slug and Milkdown id. */
export function collectHeadings(doc: ProseNode): HeadingEntry[] {
  const found: {
    pos: number;
    size: number;
    text: string;
    id: string;
    level: number;
  }[] = [];
  doc.descendants((node, pos) => {
    if (node.type.name !== "heading") return true;
    found.push({
      pos,
      size: node.nodeSize,
      text: node.textContent,
      id: typeof node.attrs.id === "string" ? node.attrs.id : "",
      level: Number(node.attrs.level) || 1,
    });
    return false;
  });
  const slugs = headingSlugs(found.map((h) => h.text));
  return found.map((h, i) => ({ ...h, slug: slugs[i] ?? "" }));
}

/**
 * The heading a `#target` link points at: Milkdown's own id wins, then the
 * GitHub slug, then a slugified comparison of the raw target (so a link
 * written as `#My Section` still resolves).
 */
export function findHeadingForTarget(
  doc: ProseNode,
  target: string,
): HeadingEntry | undefined {
  const headings = collectHeadings(doc);
  const byId = headings.find((h) => h.id && h.id === target);
  if (byId) return byId;
  const bySlug = headings.find((h) => h.slug === target);
  if (bySlug) return bySlug;
  const loose = headingSlug(target);
  return headings.find((h) => h.slug === loose);
}

/** Scroll a heading into view inside the editor's own scroll container. */
export function revealHeading(view: EditorView, pos: number): boolean {
  const dom = view.nodeDOM(pos);
  if (!(dom instanceof HTMLElement)) return false;
  dom.scrollIntoView({ block: "start", behavior: "smooth" });
  return true;
}

const anchorLinkKey = new PluginKey("ken-anchor-links");

/**
 * Intercept clicks on in-document links. Non-anchor links (`http…`, `mailto:`)
 * are untouched, so their existing behaviour is preserved.
 */
export const anchorLinkPlugin = $prose(
  () =>
    new Plugin({
      key: anchorLinkKey,
      props: {
        handleDOMEvents: {
          click: (view, event) => {
            const target = event.target as HTMLElement | null;
            const anchor = target?.closest?.("a");
            if (!(anchor instanceof HTMLAnchorElement)) return false;
            // `getAttribute` keeps the literal `#slug`; `.href` would have
            // been resolved against the page URL already.
            const raw = anchor.getAttribute("href") ?? "";
            const wanted = normalizeAnchorHref(raw);
            if (wanted === null) return false;
            const heading = findHeadingForTarget(view.state.doc, wanted);
            event.preventDefault();
            if (!heading) return true;
            revealHeading(view, heading.pos);
            return true;
          },
        },
      },
    }),
);

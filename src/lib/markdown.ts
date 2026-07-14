// Assistant-message markdown → sanitized HTML.
import { marked } from "marked";
import DOMPurify from "dompurify";

marked.setOptions({ gfm: true, breaks: true });

export function renderMarkdown(md: string): string {
  const html = marked.parse(md, { async: false }) as string;
  return DOMPurify.sanitize(html, {
    ALLOWED_TAGS: [
      "p", "br", "strong", "em", "del", "code", "pre", "blockquote",
      "ul", "ol", "li", "h1", "h2", "h3", "h4", "a", "table", "thead",
      "tbody", "tr", "th", "td", "hr",
    ],
    ALLOWED_ATTR: ["href"],
  });
}

// Search snippets carry only the backend's <mark> highlight tags. Escape the
// whole string, then re-allow just <mark>/</mark> — the result is fed to Svelte
// {@html}, so every other tag (script, img onerror, …) must stay inert. This is
// the single XSS boundary shared by HomeSearch and SearchOverlay; fix it once.
export function renderSearchSnippet(snippet: string): string {
  return snippet
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll("&lt;mark&gt;", "<mark>")
    .replaceAll("&lt;/mark&gt;", "</mark>");
}

/** A href that points inside the project (relative, no scheme). */
export function isProjectLink(href: string): boolean {
  return !/^[a-z][a-z0-9+.-]*:/i.test(href) && !href.startsWith("//");
}

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

/** A href that points inside the project (relative, no scheme). */
export function isProjectLink(href: string): boolean {
  return !/^[a-z][a-z0-9+.-]*:/i.test(href) && !href.startsWith("//");
}

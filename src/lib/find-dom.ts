// Highlighting for find-in-document over rendered DOM (docx, notebooks, slides,
// spreadsheets, the extracted-text fallback). Hits are wrapped in <mark>, and
// clearing puts the tree back exactly as it was — these surfaces render once and
// then belong to the user, so we must not leave anything behind.
import { MATCH_CAP, findTextMatches } from "./find";

export const MARK_CLASS = "ken-find";
export const CURRENT_CLASS = "ken-find-current";

/** Enough text for any document a person actually reads in a preview pane. */
const TEXT_CAP = 4_000_000;

const SKIP_TAGS = new Set([
  "SCRIPT",
  "STYLE",
  "NOSCRIPT",
  "TEXTAREA",
  "INPUT",
  "CANVAS",
  "MARK",
]);

/**
 * Wraps each hit in the subtree in a <mark>, in document order.
 * A match must sit inside one text node — a query split across two elements
 * ("<b>c</b>at") is not found, which is what every browser's find does too.
 */
export function highlightMatches(
  root: HTMLElement,
  query: string,
  opts: { matchCap?: number } = {},
): HTMLElement[] {
  clearHighlights(root);
  const marks: HTMLElement[] = [];
  if (!query.trim()) return marks;
  const matchCap = opts.matchCap ?? MATCH_CAP;

  const doc = root.ownerDocument;
  const walker = doc.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode(node: Node) {
      const text = node as Text;
      const parent = text.parentElement;
      if (!parent || SKIP_TAGS.has(parent.tagName)) return NodeFilter.FILTER_REJECT;
      return text.data.trim() ? NodeFilter.FILTER_ACCEPT : NodeFilter.FILTER_REJECT;
    },
  });

  // Collected first: splitting a text node mid-walk confuses the walker.
  const targets: Text[] = [];
  let scanned = 0;
  for (let n = walker.nextNode(); n; n = walker.nextNode()) {
    const text = n as Text;
    scanned += text.data.length;
    targets.push(text);
    if (scanned > TEXT_CAP) break;
  }

  for (const node of targets) {
    if (marks.length >= matchCap) break;
    const { starts } = findTextMatches(node.data, query, matchCap - marks.length);
    if (starts.length === 0) continue;

    // Split back-to-front: each split only trims the node's tail, so the
    // offsets of the earlier hits stay valid.
    const inNode: HTMLElement[] = [];
    for (let i = starts.length - 1; i >= 0; i--) {
      const hit = node.splitText(starts[i]);
      hit.splitText(query.length);
      const mark = doc.createElement("mark");
      mark.className = MARK_CLASS;
      hit.replaceWith(mark);
      mark.appendChild(hit);
      inNode.unshift(mark);
    }
    marks.push(...inNode);
  }
  return marks;
}

/** Unwraps every mark we added and re-joins the text nodes we split. */
export function clearHighlights(root: HTMLElement): void {
  const marks = root.querySelectorAll<HTMLElement>(`mark.${MARK_CLASS}`);
  if (marks.length === 0) return;
  for (const mark of marks) {
    const parent = mark.parentNode;
    if (!parent) continue;
    parent.replaceChild(
      mark.ownerDocument.createTextNode(mark.textContent ?? ""),
      mark,
    );
  }
  root.normalize(); // re-merges the halves splitText left behind
}

/** Accents one mark; a stale or out-of-range index simply accents nothing. */
export function setCurrent(marks: HTMLElement[], index: number): HTMLElement | null {
  let current: HTMLElement | null = null;
  for (let i = 0; i < marks.length; i++) {
    const on = i === index;
    marks[i].classList.toggle(CURRENT_CLASS, on);
    if (on) current = marks[i];
  }
  return current;
}

export function scrollMarkIntoView(mark: HTMLElement | null): void {
  mark?.scrollIntoView({ block: "center", inline: "nearest" });
}

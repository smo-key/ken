// A page change as formatted Markdown, block by block: the held edit's
// diff in the Ingest detail and the Inbox. A page is cut into its blocks
// (a heading, a paragraph, a list, a table, a fenced block, the
// frontmatter), the two versions are compared block against block, and
// each block is drawn as Markdown, an added one marked green and a removed
// one red and struck. Long runs of unchanged blocks fold into "N unchanged
// lines", as the line diff does. No runes and no IPC.
import { diffArrays } from "diff";

/** One drawn block of a formatted diff, or a fold of unchanged ones. */
export type BlockRow =
  | { type: "add" | "del" | "ctx"; text: string; frontmatter: boolean }
  | { type: "gap"; count: number };

const FENCE = /^\s*(```|~~~)/;

/** A page's Markdown cut into its blocks: runs of lines between blank
 *  lines, a fenced block whole (blank lines and all), and the frontmatter
 *  at the top as one block. Each block is its lines joined. */
export function markdownBlocks(text: string): string[] {
  const lines = text.replace(/\r\n/g, "\n").split("\n");
  const blocks: string[] = [];
  let i = 0;
  if (lines[0]?.trim() === "---") {
    const end = lines.findIndex((l, k) => k > 0 && l.trim() === "---");
    if (end > 0) {
      blocks.push(lines.slice(0, end + 1).join("\n"));
      i = end + 1;
    }
  }
  let cur: string[] = [];
  const flush = () => {
    if (cur.length > 0) blocks.push(cur.join("\n"));
    cur = [];
  };
  while (i < lines.length) {
    const line = lines[i];
    if (FENCE.test(line)) {
      flush();
      const mark = line.trim().slice(0, 3);
      const fence = [line];
      i++;
      while (i < lines.length) {
        fence.push(lines[i]);
        if (lines[i].trim().startsWith(mark)) break;
        i++;
      }
      blocks.push(fence.join("\n"));
      i++;
      continue;
    }
    if (line.trim() === "") flush();
    // A heading is its own block, even with no blank line after it.
    else if (/^#{1,6}\s/.test(line)) {
      flush();
      blocks.push(line);
    } else cur.push(line);
    i++;
  }
  flush();
  return blocks;
}

const lineCount = (block: string) => block.split("\n").length;

/** The blocks of `a` (the page now) against `b` (with the change): changed
 *  blocks whole, `context` unchanged blocks around each change, and the
 *  rest folded into a gap that counts its lines. */
export function buildBlockRows(a: string, b: string, context = 1): BlockRow[] {
  const before = markdownBlocks(a);
  const after = markdownBlocks(b);
  const frontOf = (blocks: string[]) => (blocks[0]?.startsWith("---") ? blocks[0] : null);
  const fronts = new Set([frontOf(before), frontOf(after)].filter((f): f is string => f !== null));
  const all: { type: "add" | "del" | "ctx"; text: string; frontmatter: boolean }[] = [];
  for (const part of diffArrays(before, after)) {
    const type = part.added ? "add" : part.removed ? "del" : "ctx";
    for (const text of part.value) all.push({ type, text, frontmatter: fronts.has(text) });
  }
  const rows: BlockRow[] = [];
  let i = 0;
  while (i < all.length) {
    if (all[i].type !== "ctx") {
      rows.push(all[i]);
      i++;
      continue;
    }
    let j = i;
    while (j < all.length && all[j].type === "ctx") j++;
    const head = i === 0 ? 0 : context;
    const tail = j === all.length ? 0 : context;
    if (j - i <= head + tail) {
      for (let k = i; k < j; k++) rows.push(all[k]);
    } else {
      for (let k = i; k < i + head; k++) rows.push(all[k]);
      const folded = all.slice(i + head, j - tail).reduce((n, r) => n + lineCount(r.text), 0);
      rows.push({ type: "gap", count: folded });
      for (let k = j - tail; k < j; k++) rows.push(all[k]);
    }
    i = j;
  }
  return rows;
}

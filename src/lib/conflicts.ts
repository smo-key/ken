// Sync conflicts and conflicted copies, the pure part: their payloads, the
// line diff the conflict view and page proposals show, and the line the
// banner at the top of Files says. No runes and no IPC.
import { diffLines } from "diff";
import type {
  ConflictCopyPayload,
  ConflictItem,
  ConflictPayload,
  FilesBanner,
  PageProposalPayload,
} from "./api";

/** Parsed conflict payload, or null when absent or malformed. */
export function conflictPayload(item: Pick<ConflictItem, "kind" | "payload">): ConflictPayload | null {
  if (item.kind !== "conflict" || !item.payload) return null;
  try {
    return JSON.parse(item.payload) as ConflictPayload;
  } catch {
    return null;
  }
}

/** Parsed conflicted-copy payload, or null when absent or malformed. */
export function conflictCopyPayload(item: Pick<ConflictItem, "kind" | "payload">): ConflictCopyPayload | null {
  if (item.kind !== "conflict-copy" || !item.payload) return null;
  try {
    return JSON.parse(item.payload) as ConflictCopyPayload;
  } catch {
    return null;
  }
}

/** A page proposal's payload (the page as it is and with the change), or
 *  null when absent or malformed. */
export function proposalPayload(payload: string | null): PageProposalPayload | null {
  if (!payload) return null;
  try {
    const p = JSON.parse(payload) as PageProposalPayload;
    return typeof p.base === "string" && typeof p.proposed === "string" ? p : null;
  } catch {
    return null;
  }
}

/** The row id inside a kind-prefixed id ("item-12" → 12; "item-12@<repo>",
 *  as the banner names it so two repos' rows never share an id → 12). */
export function numericId(id: string): number {
  const m = /-(\d+)/.exec(id);
  return m ? Number(m[1]) : NaN;
}

/** One rendered row of a collapsed unified diff. */
export type DiffRow =
  | { type: "add" | "del" | "ctx"; text: string }
  | { type: "gap"; count: number };

/**
 * Collapsed unified-diff rows comparing text `a` (the removed side) against
 * text `b` (the added side). Runs of unchanged lines longer than
 * `2 * context` collapse to a single "gap" so the eye lands on changes.
 */
export function buildDiffRows(a: string, b: string, context = 3): DiffRow[] {
  const lines: { type: "add" | "del" | "ctx"; text: string }[] = [];
  for (const part of diffLines(a, b)) {
    const type = part.added ? "add" : part.removed ? "del" : "ctx";
    const chunk = part.value.split("\n");
    // diffLines values keep a trailing newline; drop the empty tail it leaves.
    if (chunk.length > 1 && chunk[chunk.length - 1] === "") chunk.pop();
    for (const text of chunk) lines.push({ type, text });
  }

  const rows: DiffRow[] = [];
  let i = 0;
  while (i < lines.length) {
    if (lines[i].type !== "ctx") {
      rows.push(lines[i]);
      i++;
      continue;
    }
    let j = i;
    while (j < lines.length && lines[j].type === "ctx") j++;
    const run = j - i;
    const head = i === 0 ? 0 : context; // keep context after the prior change
    const tail = j === lines.length ? 0 : context; // and before the next change
    if (run <= head + tail) {
      for (let k = i; k < j; k++) rows.push(lines[k]);
    } else {
      for (let k = i; k < i + head; k++) rows.push(lines[k]);
      rows.push({ type: "gap", count: run - head - tail });
      for (let k = j - tail; k < j; k++) rows.push(lines[k]);
    }
    i = j;
  }
  return rows;
}

/** How many of each the banner counts: two edits to one file, and copies a
 *  shared drive left beside the original. */
export function bannerCounts(banner: FilesBanner | null): { conflicts: number; copies: number } {
  if (!banner) return { conflicts: 0, copies: 0 };
  const conflicts = banner.conflicts.filter((c) => c.kind === "conflict").length;
  const listed = banner.conflicts.filter((c) => c.kind === "conflict-copy").length;
  return { conflicts, copies: Math.max(listed, banner.conflictedCopies) };
}

/** "1 file changed in two places · 2 conflicted copies", or "" for none. */
export function bannerLine(banner: FilesBanner | null): string {
  const { conflicts, copies } = bannerCounts(banner);
  const parts: string[] = [];
  if (conflicts > 0) parts.push(`${conflicts} ${conflicts === 1 ? "file" : "files"} changed in two places`);
  if (copies > 0) parts.push(`${copies} conflicted ${copies === 1 ? "copy" : "copies"}`);
  return parts.join(" · ");
}

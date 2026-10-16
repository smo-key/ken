/**
 * Pure logic for the Log screen, the project's RAID register
 * (knowledge-core decision 37): its groups in order, the search box, open
 * items apart from closed ones (which fold away), each status's tone, and
 * marking matches. The register comes from `api.raidRegister()`, each group
 * newest first.
 */
import type { RaidItem, RaidRegister } from "./api";

export type GroupKey = keyof RaidRegister;

export interface GroupInfo {
  key: GroupKey;
  label: string;
  /** What the folded items are called. */
  closedLabel: string;
}

/** The groups, in the order the Log shows them: the RAID four, then the
 *  milestones. */
export const GROUPS: GroupInfo[] = [
  { key: "risks", label: "Risks", closedLabel: "closed" },
  { key: "actions", label: "Actions", closedLabel: "closed" },
  { key: "issues", label: "Issues", closedLabel: "closed" },
  { key: "decisions", label: "Decisions", closedLabel: "superseded or refined" },
  { key: "milestones", label: "Milestones", closedLabel: "dropped" },
];

/** Whether `item` holds `query` (case aside) in its id, title, status,
 *  owner, date, repo or file. An empty query matches everything. */
export function matches(item: RaidItem, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  return [item.id, item.title, item.status, item.owner ?? "", item.date, item.repo ?? "", item.path ?? ""].some((f) =>
    f.toLowerCase().includes(q),
  );
}

export interface GroupView extends GroupInfo {
  open: RaidItem[];
  closed: RaidItem[];
}

/** Each group's items that match `query`, the open ones apart from the
 *  closed ones. Order is kept. */
export function viewOf(register: RaidRegister, query: string): GroupView[] {
  return GROUPS.map((g) => {
    const items = register[g.key].filter((i) => matches(i, query));
    return { ...g, open: items.filter((i) => !i.closed), closed: items.filter((i) => i.closed) };
  });
}

/** How many items the register holds, and how many match `query`. */
export function counts(register: RaidRegister, query: string): { shown: number; total: number } {
  const all = GROUPS.flatMap((g) => register[g.key]);
  return { shown: all.filter((i) => matches(i, query)).length, total: all.length };
}

/** A status's tone for its pill: `attn` for what needs watching (an open
 *  risk or issue, a blocked action, a milestone at risk), `ok` for what
 *  settled well, `muted` for a folded item, else `plain`. */
export function statusTone(item: RaidItem): "attn" | "ok" | "muted" | "plain" {
  const s = item.status;
  if (item.closed) return "muted";
  if (((item.kind === "risk" || item.kind === "issue") && s === "open") || s === "blocked" || s.includes("at risk")) return "attn";
  if (["mitigated", "resolved", "live", "done", "reached", "released"].includes(s)) return "ok";
  return "plain";
}

/**
 * Escape-then-mark: HTML in `text` is escaped, and the only tags in the
 * result are our own <mark>s around case-insensitive matches of `query`.
 */
export function highlightMatches(text: string, query: string): string {
  const esc = (s: string) => s.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;");
  const q = query.trim();
  if (!q) return esc(text);
  const lower = text.toLowerCase();
  const ql = q.toLowerCase();
  let out = "";
  let i = 0;
  for (;;) {
    const at = lower.indexOf(ql, i);
    if (at === -1) break;
    out += esc(text.slice(i, at)) + "<mark>" + esc(text.slice(at, at + q.length)) + "</mark>";
    i = at + q.length;
  }
  return out + esc(text.slice(i));
}

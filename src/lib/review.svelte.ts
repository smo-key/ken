// Review inbox state: the unified list of everything Ken needs a human
// for, plus recently resolved items. Assembled by the review_inbox
// command; refreshed on run, index, and sync events.
import { diffLines } from "diff";
import {
  api,
  type ConflictCopyPayload,
  type ConflictCopyResolution,
  type ConflictPayload,
  type ConflictResolution,
  type InboxItem,
  type InboxKind,
} from "./api";

/** Actions the detail pane offers, derived from item kind. */
export type InboxAction =
  | "approve"
  | "discard"
  | "run-now"
  | "open-files"
  | "open-ingests"
  | "mark-done"
  | "accept-draft"
  | "keep-mine"
  | "take-theirs"
  | "edit-manually"
  | "keep-copy"
  | "keep-original"
  | "open-both";

export function actionsFor(kind: InboxKind): InboxAction[] {
  switch (kind) {
    case "approval":
      return ["approve", "discard"];
    case "stale":
      return ["run-now"];
    case "failed-file":
      return ["open-files"];
    case "broken-recipe":
      return ["open-ingests"];
    case "stored":
      return ["mark-done"];
    case "conflict":
      return ["accept-draft", "keep-mine", "take-theirs", "edit-manually"];
    case "conflict-copy":
      return ["keep-copy", "keep-original", "open-both"];
  }
}

/** Parsed conflict payload, or null when absent/malformed. */
export function conflictPayload(item: InboxItem): ConflictPayload | null {
  if (item.kind !== "conflict" || !item.payload) return null;
  try {
    return JSON.parse(item.payload) as ConflictPayload;
  } catch {
    return null;
  }
}

/** Parsed conflicted-copy payload, or null when absent/malformed. */
export function conflictCopyPayload(
  item: InboxItem,
): ConflictCopyPayload | null {
  if (item.kind !== "conflict-copy" || !item.payload) return null;
  try {
    return JSON.parse(item.payload) as ConflictCopyPayload;
  } catch {
    return null;
  }
}

/** One rendered row of a collapsed unified diff. */
export type DiffRow =
  | { type: "add" | "del" | "ctx"; text: string }
  | { type: "gap"; count: number };

/**
 * Build collapsed unified-diff rows comparing text `a` (the "−"/removed side)
 * against text `b` (the "+"/added side). Runs of unchanged lines longer than
 * `2 * context` collapse to a single "gap" marker so the eye lands on changes.
 * Pure and dependency-light — safe to unit test.
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

/** Numeric id parsed back out of a kind-prefixed inbox id ("run-12" → 12). */
export function numericId(item: InboxItem): number {
  return Number(item.id.slice(item.id.indexOf("-") + 1));
}

/** List dot color per kind (Paper & Ink vars only). */
export function dotFor(kind: InboxKind): string {
  switch (kind) {
    case "approval":
      return "var(--accent)";
    case "stored":
      return "var(--needs-input)";
    case "conflict":
    case "conflict-copy":
    case "failed-file":
    case "broken-recipe":
      return "var(--danger)";
    case "stale":
      return "var(--ink-tertiary)";
  }
}

/** Short source label for list captions: file name or slug. */
export function sourceLabel(item: InboxItem): string {
  return item.sourceRef.split("/").pop() || item.sourceRef;
}

class ReviewStore {
  items = $state<InboxItem[]>([]);
  done = $state<InboxItem[]>([]);
  selected = $state<string | null>(null);

  /** Open-item count — feeds the nav badge. */
  get count(): number {
    return this.items.length;
  }

  get selectedItem(): InboxItem | null {
    return (
      this.items.find((i) => i.id === this.selected) ??
      this.done.find((i) => i.id === this.selected) ??
      null
    );
  }

  /** Whether the selection lives in the Done section (no actions shown). */
  get selectedIsDone(): boolean {
    return this.done.some((i) => i.id === this.selected);
  }

  async init() {
    await api.onIngestRunChanged(() => void this.refresh());
    await api.onIndexUpdated(() => void this.refresh());
    await api.onReviewChanged(() => void this.refresh());
    await this.refresh();
  }

  async refresh() {
    if (!(await hasProject())) return;
    const inbox = await api.reviewInbox();
    this.items = inbox.items;
    this.done = inbox.done;
    const known = (id: string) =>
      this.items.some((i) => i.id === id) || this.done.some((i) => i.id === id);
    if (this.selected && !known(this.selected)) this.selected = null;
    if (!this.selected && this.items.length > 0) this.selected = this.items[0].id;
  }

  select(id: string) {
    this.selected = id;
  }

  async approve(item: InboxItem) {
    await api.approveRun(numericId(item));
    await this.refresh();
  }

  async discard(item: InboxItem) {
    await api.discardRun(numericId(item));
    await this.refresh();
  }

  async runNow(item: InboxItem) {
    await api.runIngest(item.sourceRef, true);
    await this.refresh();
  }

  async markDone(item: InboxItem) {
    await api.resolveReviewItem(numericId(item));
    await this.refresh();
  }

  /** Resolve a merge conflict; returns the project-relative path written. */
  async resolveConflict(
    item: InboxItem,
    resolution: ConflictResolution,
    content?: string,
  ): Promise<string> {
    const path = await api.resolveConflict(numericId(item), resolution, content);
    await this.refresh();
    return path;
  }

  /** Resolve a conflicted copy; returns the surviving file's path. */
  async resolveConflictCopy(
    item: InboxItem,
    resolution: ConflictCopyResolution,
  ): Promise<string> {
    const path = await api.resolveConflictCopy(numericId(item), resolution);
    await this.refresh();
    return path;
  }
}

async function hasProject(): Promise<boolean> {
  return (await api.currentProject().catch(() => null)) !== null;
}

export const review = new ReviewStore();

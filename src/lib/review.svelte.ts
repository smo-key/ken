// Review inbox state: the unified list of everything Ken needs a human
// for, plus recently resolved items. Assembled by the review_inbox
// command; refreshed on run and index events.
import { api, type InboxItem, type InboxKind } from "./api";

/** Actions the detail pane offers, derived from item kind. */
export type InboxAction =
  | "approve"
  | "discard"
  | "run-now"
  | "open-files"
  | "open-ingests"
  | "mark-done";

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
  }
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
}

async function hasProject(): Promise<boolean> {
  return (await api.currentProject().catch(() => null)) !== null;
}

export const review = new ReviewStore();

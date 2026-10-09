// The Inbox (the Briefing design): one list of everything waiting on you,
// sorted by who it is from. People: tasks and messages a teammate sent through
// the team inbox. Ken: what an ingest waits on (rulings, tickets, held page
// edits), what the wiki's checks found, and questions Ken asked in a chat.
// Sync & files: edit conflicts and files Ken could not read. Home's "Needs
// you" is the top of this list and the sidebar count is its length. Resolving
// an item here resolves it where it came from, so nothing lives in two places.
import {
  api,
  type ConflictItem,
  type DayEscalation,
  type FamilyConnectionDto,
  type FamilyInboxItem,
  type IngestProposal,
  type RawSource,
  type TeamFinding,
} from "./api";
import { app } from "./app.svelte";
import { scope } from "./scope.svelte";
import { families } from "./families.svelte";
import { conflicts } from "./conflicts.svelte";
import { chats } from "./chats.svelte";
import { day } from "./day.svelte";
import { coalesce, singleFlight } from "./refresh";
import { uniqueIds } from "./uniqueIds";

export type InboxGroup = "people" | "ken" | "sync";
/** The colour a kind is drawn in: amber for decisions, red for failures. */
export type InboxTone = "attn" | "danger" | "accent" | "ink";

export type InboxSource =
  | { type: "family"; familyId: string; item: FamilyInboxItem }
  | { type: "proposal"; proposal: IngestProposal; cardId: number; note: string; projectId: string }
  | { type: "finding"; finding: TeamFinding & { itemId?: number | null } }
  | { type: "conflict"; conflict: ConflictItem }
  | { type: "failed"; relPath: string; error: string | null }
  | { type: "escalation"; escalation: DayEscalation }
  | { type: "chat"; chatId: string; title: string };

export interface InboxItem {
  /** Stable across reads, so the selection survives a refresh. */
  id: string;
  group: InboxGroup;
  /** "Task", "Ruling", "Conflict"… as a person names it. */
  kind: string;
  tone: InboxTone;
  /** Who it is from: a teammate, "Ken", "Sync". */
  from: string;
  title: string;
  /** One line of context. */
  short: string;
  /** Seconds since the epoch, when known. */
  when: number | null;
  /** The file or source it is about. */
  where: string | null;
  source: InboxSource;
}

const GROUP_ORDER: Record<InboxGroup, number> = { people: 0, ken: 1, sync: 2 };
const DEBOUNCE_MS = 400;
const INDEX_DEBOUNCE_MS = 3000;

function epoch(s: string | null | undefined): number | null {
  if (!s) return null;
  const t = Date.parse(s);
  return Number.isNaN(t) ? null : Math.floor(t / 1000);
}

function leaf(path: string): string {
  return path.split("/").pop() || path;
}

/** The family items that wait on me: unread or seen tasks and messages. */
export function familyItems(groups: { connection: FamilyConnectionDto; items: FamilyInboxItem[] }[]): InboxItem[] {
  const out: InboxItem[] = [];
  for (const g of groups) {
    const familyId = g.connection.connection.familyId;
    for (const item of g.items) {
      if (item.malformed) continue;
      if (item.status !== "unread" && item.status !== "seen") continue;
      const task = item.kind === "task";
      out.push({
        id: `family:${familyId}:${item.id}`,
        group: "people",
        kind: task ? "Task" : item.kind === "notification" ? "Notice" : "Message",
        tone: task ? "attn" : "accent",
        from: item.from || "A teammate",
        title: item.title || (task ? "A task" : "A message"),
        short: item.body.split("\n").find((l) => l.trim())?.trim() ?? "",
        when: epoch(item.created),
        where: null,
        source: { type: "family", familyId, item },
      });
    }
  }
  return out;
}

/** An escalation addressed to me: a decision someone raised to its owner. */
export function escalationItem(e: DayEscalation): InboxItem {
  return {
    id: `escalation:${e.projectId}:${e.relPath}`,
    group: "people",
    kind: "Escalation",
    tone: "attn",
    from: e.raisedBy || "A teammate",
    title: e.title,
    short: e.blocks ? `Blocks ${e.blocks}` : e.ticket ? `On ${e.ticket}` : "A decision raised to you",
    when: epoch(e.raised),
    where: e.relPath,
    source: { type: "escalation", escalation: e },
  };
}

export function proposalItem(p: IngestProposal, cardId: number, note: string, projectId: string): InboxItem {
  const kind = p.kind === "ruling" ? "Ruling" : p.kind === "ticket" ? "Ticket" : p.kind === "new page" ? "New page" : "Page edit";
  return {
    id: `proposal:${p.id}`,
    group: "ken",
    kind,
    tone: "attn",
    from: p.kind === "ruling" && p.decider ? `Ken, for ${p.decider}` : "Ken",
    title: p.title,
    short: p.kind === "page" || p.kind === "new page" ? `${leaf(p.page)} · held back for you to read` : p.body.split("\n")[0] ?? "",
    when: null,
    where: note,
    source: { type: "proposal", proposal: p, cardId, note, projectId },
  };
}

export function findingItem(f: TeamFinding & { itemId?: number | null }, i: number): InboxItem {
  const k = f.kind;
  const kind =
    k === "broken-link" || k === "links"
      ? "Broken link"
      : k === "missing-page"
        ? "Missing page"
        : k === "ambiguous-name"
          ? "Unclear link"
          : k === "aged"
            ? "Not checked lately"
            : k === "draft" || k === "wiki-draft"
              ? "First draft"
              : k === "proposal"
                ? "Proposal"
                : k === "draft-failed"
                  ? "Couldn't draft"
                  : k === "repo-removed"
                    ? "Repo left the team"
                    : "Out of date";
  return {
    id: `finding:${f.itemId ?? `${k}:${f.projectId ?? ""}:${f.path ?? i}:${f.title}`}`,
    group: "ken",
    kind,
    tone: k === "draft" ? "accent" : "attn",
    from: "Ken",
    title: f.title,
    short: f.detail,
    when: null,
    where: f.path,
    source: { type: "finding", finding: f },
  };
}

export function conflictItem(c: ConflictItem): InboxItem {
  return {
    id: `conflict:${c.projectId ?? ""}:${c.id}`,
    group: "sync",
    kind: c.kind === "conflict-copy" ? "Conflicted copy" : "Conflict",
    tone: "danger",
    from: "Sync",
    title: c.title,
    short: c.body.split("\n")[0] ?? "",
    when: c.when || null,
    where: c.sourceRef,
    source: { type: "conflict", conflict: c },
  };
}

export function failedItem(relPath: string, error: string | null): InboxItem {
  return {
    id: `failed:${relPath}`,
    group: "sync",
    kind: "Couldn't read",
    tone: "danger",
    from: "Ken",
    title: `Ken couldn't read ${leaf(relPath)}`,
    short: error ? `${error}. It is still found by name.` : "It is still found by name.",
    when: null,
    where: relPath,
    source: { type: "failed", relPath, error },
  };
}

/** People first, then Ken, then sync; newest first inside a group. */
export function sortInbox(items: InboxItem[]): InboxItem[] {
  return [...items].sort((a, b) => GROUP_ORDER[a.group] - GROUP_ORDER[b.group] || (b.when ?? 0) - (a.when ?? 0));
}
class InboxStore {
  /** Waiting items from the backend reads (ingest waits, findings). */
  private ken = $state<InboxItem[]>([]);
  /** The wiki's findings, read apart: the read runs git in every repo. */
  private findings = $state<InboxItem[]>([]);
  /** The library's Raw sources from the last read: what Ingest is doing. */
  raw = $state<RawSource[]>([]);
  loading = $state(false);
  /** Items dealt with this session, newest first ("Done · n"). */
  done = $state<InboxItem[]>([]);
  selected = $state<string | null>(null);

  private subscribed = false;

  /** Everything waiting on you, in Inbox order. */
  get items(): InboxItem[] {
    const doneIds = new Set(this.done.map((d) => d.id));
    const people = [...familyItems(families.trayGroups), ...day.escalations.map(escalationItem)];
    const sync = [
      ...conflicts.items.map(conflictItem),
      ...app.failedFiles.map((f) => failedItem(f.relPath, f.error ?? null)),
    ];
    const asks = chats.rows
      .filter((r) => r.status === "needs_input")
      .map(
        (r): InboxItem => ({
          id: `chat:${r.id}`,
          group: "ken",
          kind: "Question",
          tone: "attn",
          from: "Ken, in a chat",
          title: r.title || "Ken asked you something",
          short: "Ken is waiting for your answer before it goes on.",
          when: null,
          where: null,
          source: { type: "chat", chatId: r.id, title: r.title },
        }),
      );
    return uniqueIds(sortInbox([...people, ...this.ken, ...this.findings, ...asks, ...sync])).filter((i) => !doneIds.has(i.id));
  }

  get count(): number {
    return this.items.length;
  }

  countFor(group: InboxGroup | "all"): number {
    return group === "all" ? this.items.length : this.items.filter((i) => i.group === group).length;
  }

  get selectedItem(): InboxItem | null {
    const id = this.selected;
    if (!id) return null;
    return this.items.find((i) => i.id === id) ?? this.done.find((i) => i.id === id) ?? null;
  }

  async subscribe() {
    if (this.subscribed) return;
    this.subscribed = true;
    void families.init();
    await api.onReviewChanged(() => {
      this.queue();
      void this.refreshFindings();
    });
    // Indexing moves a source's state here, at most every few seconds; the
    // findings (git in every repo) are not read for it.
    await api.onIndexUpdated(() => this.queue(INDEX_DEBOUNCE_MS));
    $effect.root(() => {
      $effect(() => {
        void scope.team;
        void app.workspace?.id;
        this.queue(0);
        void this.refreshFindings();
      });
    });
  }

  /** The wiki's findings (broken links, drafts, proposals). */
  readonly refreshFindings = singleFlight(async () => {
    if (!app.workspace) {
      this.findings = [];
      return;
    }
    const team = scope.team;
    const ws = app.workspace.id;
    const o = await api.teamOverview(team).catch(() => null);
    if (!o || team !== scope.team || ws !== app.workspace?.id) return;
    this.findings = (o.findings ?? []).map((f, i) => findingItem(f, i));
  });

  readonly queue = coalesce(() => void this.refresh(), DEBOUNCE_MS);

  /** Read Ken's waiting items: every ingest card's waits, and the wiki's
   *  findings. Optional reads: a failure keeps what was there. One read at
   *  a time (see `singleFlight`). */
  readonly refresh = singleFlight(async () => {
    if (!app.workspace) {
      this.ken = [];
      return;
    }
    const team = scope.team;
    const ws = app.workspace.id;
    this.loading = true;
    try {
      const out: InboxItem[] = [];
      const overview = await api.ingestOverview(team).catch(() => null);
      if (overview && team === scope.team) this.raw = overview.raw;
      if (overview) {
        const cards = [
          ...overview.ingested.filter((s) => s.waiting > 0).map((s) => s.id),
          ...overview.raw.filter((s) => s.waiting > 0 && s.cardId !== null).map((s) => s.cardId as number),
        ];
        const unique = [...new Set(cards)];
        const read = await Promise.all(unique.map((id) => api.ingestCard(team, id).catch(() => null)));
        for (const card of read) {
          if (!card) continue;
          for (const p of card.proposals) out.push(proposalItem(p, card.id, card.note, overview.projectId));
        }
      }
      if (team === scope.team && ws === app.workspace?.id) this.ken = out;
    } finally {
      this.loading = false;
    }
  });

  /** Mark an item dealt with: it leaves the list and shows under Done. */
  resolved(item: InboxItem) {
    this.done = [item, ...this.done.filter((d) => d.id !== item.id)].slice(0, 30);
    this.ken = this.ken.filter((k) => k.id !== item.id);
    this.findings = this.findings.filter((k) => k.id !== item.id);
  }

  select(id: string | null) {
    this.selected = id;
  }

  /** The source Ingest is transcribing or reading now, with how far. */
  get reading(): { name: string; verb: string; pct: number | null } | null {
    const r = this.raw.find((s) => s.state === "transcribing" || s.state === "reading");
    if (!r) return null;
    const m = r.detail?.match(/(\d+)%/);
    return { name: r.name, verb: r.state === "transcribing" ? "Transcribing" : "Reading", pct: m ? Number(m[1]) : null };
  }

  /** Sources still to read. */
  get queued(): number {
    return this.raw.filter((s) => s.state === "queued").length;
  }
}

export const inbox = new InboxStore();

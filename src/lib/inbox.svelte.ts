// The Inbox (the Briefing design): one list of everything waiting on you,
// sorted by who it is from. Ken: what an ingest waits on (rulings, tickets, held page edits), the findings (what a
// source contradicts and leaves open, what the wiki's checks found), the
// repo analyses' proposed role rules and controls, and questions Ken asked
// in a chat.
// Sync & files: edit conflicts and files Ken could not read. Home's "Needs
// you" is the top of this list and the sidebar count is its length. Resolving
// an item here resolves it where it came from, so nothing lives in two places.
import {
  api,
  type AnalysisProposal,
  type ConflictItem,
  type Finding,
  type IngestProposal,
  type RawSource,
} from "./api";
import { app } from "./app.svelte";
import { conflicts } from "./conflicts.svelte";
import { chats } from "./chats.svelte";
import { coalesce, singleFlight } from "./refresh";
import { uniqueIds } from "./uniqueIds";
import { heldTitle, proposalShort } from "./ingestCard";

export type InboxGroup = "ken" | "sync";
/** The colour a kind is drawn in: amber for decisions, red for failures. */
export type InboxTone = "attn" | "danger" | "accent" | "ink";

export type InboxSource =
  | { type: "proposal"; proposal: IngestProposal; cardId: number; note: string; projectId: string }
  | { type: "finding"; finding: Finding }
  | { type: "analysis"; analysis: AnalysisProposal }
  | { type: "conflict"; conflict: ConflictItem }
  | { type: "failed"; relPath: string; error: string | null }
  | { type: "chat"; chatId: string; title: string };

export interface InboxItem {
  /** Stable across reads, so the selection survives a refresh. */
  id: string;
  group: InboxGroup;
  /** "Task", "Ruling", "Conflict"… as a person names it. */
  kind: string;
  tone: InboxTone;
  /** Who it is from: "Ken", "Sync". */
  from: string;
  title: string;
  /** Detail behind the title, as its tooltip (a held edit's path). */
  tip?: string | null;
  /** One line of context, never the title again; often empty. */
  short: string;
  /** Seconds since the epoch, when known. */
  when: number | null;
  /** The file or source it is about. */
  where: string | null;
  source: InboxSource;
}

const GROUP_ORDER: Record<InboxGroup, number> = { ken: 0, sync: 1 };
const DEBOUNCE_MS = 400;
const INDEX_DEBOUNCE_MS = 3000;

function leaf(path: string): string {
  return path.split("/").pop() || path;
}

/** What each kind of proposal is called in the Inbox. */
const PROPOSAL_KINDS: Record<IngestProposal["kind"], string> = {
  ruling: "Ruling",
  risk: "Risk",
  issue: "Issue",
  ticket: "Ticket",
  "new page": "New page",
  page: "Page edit",
};

/** A proposal waiting for a person. `note` is the ingest's note it came
 *  from; empty for one no card holds, then `origin` says where it came from. */
export function proposalItem(p: IngestProposal, cardId: number, note: string, projectId: string, origin: "chat" | "drift" | null = null): InboxItem {
  const kind = PROPOSAL_KINDS[p.kind] ?? "Page edit";
  const held = p.kind === "page" || p.kind === "new page";
  return {
    id: `proposal:${p.id}`,
    group: "ken",
    kind,
    tone: "attn",
    from: origin === "chat" ? "Ken, in a chat" : origin === "drift" ? "Ken, checking drift" : "Ken",
    title: held ? heldTitle(p) : p.title,
    tip: held ? p.page : null,
    short: proposalShort(p),
    when: null,
    where: note || p.page || null,
    source: { type: "proposal", proposal: p, cardId, note, projectId },
  };
}

/** What each kind of finding is called in the Inbox. */
const FINDING_KINDS: Record<string, string> = {
  "broken-link": "Broken link",
  "missing-page": "Missing page",
  "ambiguous-name": "Unclear link",
  orphan: "Orphan page",
  "suggested-page": "Page to write",
  "unknown-ruling": "Unknown ruling",
  supersession: "Superseded ruling",
  contradiction: "Contradiction",
  question: "Open question",
  drift: "Out of date",
  aged: "Not checked",
};

export function findingItem(f: Finding, i: number): InboxItem {
  const k = f.kind;
  const kind = FINDING_KINDS[k] ?? "Out of date";
  return {
    id: `finding:${f.itemId ?? `${k}:${f.projectId ?? ""}:${f.path ?? i}:${f.title}`}`,
    group: "ken",
    kind,
    tone: "attn",
    from: "Ken",
    title: f.title,
    // The title says what it is; the rest (the text the title did not
    // hold, the source's words, the note) is the detail's.
    short: "",
    when: null,
    where: f.path,
    source: { type: "finding", finding: f },
  };
}

/** A repo analysis waiting for a person: the role rules and controls Ken
 *  proposes for a source, applied only once accepted. */
export function analysisItem(a: AnalysisProposal): InboxItem {
  const parts = [
    a.roles.length > 0 ? `${a.roles.length} role ${a.roles.length === 1 ? "rule" : "rules"}` : "",
    a.controls.length > 0 ? `${a.controls.length} ${a.controls.length === 1 ? "control" : "controls"}` : "",
  ].filter((p) => p);
  return {
    id: `analysis:${a.itemId}`,
    group: "ken",
    kind: "Proposal",
    tone: "attn",
    from: "Ken",
    title: `Search settings for ${a.source}`,
    short: parts.join(" · "),
    when: a.createdAt || null,
    where: ".ken/knowledge.json",
    source: { type: "analysis", analysis: a },
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
    short: error ?? "",
    when: null,
    where: relPath,
    source: { type: "failed", relPath, error },
  };
}

/** Ken first, then sync; newest first inside a group. */
export function sortInbox(items: InboxItem[]): InboxItem[] {
  return [...items].sort((a, b) => GROUP_ORDER[a.group] - GROUP_ORDER[b.group] || (b.when ?? 0) - (a.when ?? 0));
}
class InboxStore {
  /** Waiting items from the backend reads (ingest waits, findings). */
  private ken = $state<InboxItem[]>([]);
  /** The knowledge base's findings, read apart from the cards. */
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
          short: "",
          when: null,
          where: null,
          source: { type: "chat", chatId: r.id, title: r.title },
        }),
      );
    return uniqueIds(sortInbox([...this.ken, ...this.findings, ...asks, ...sync])).filter((i) => !doneIds.has(i.id));
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
    await api.onReviewChanged(() => {
      this.queue();
      void this.refreshFindings();
    });
    // Indexing moves a source's state here, at most every few seconds. The
    // findings are read for it too: an ingest, a drift run and the scan's
    // lint file theirs and then write or commit files, which is the event
    // that follows, and none of them says "review-changed".
    await api.onIndexUpdated(() => {
      this.queue(INDEX_DEBOUNCE_MS);
      this.queueFindings(INDEX_DEBOUNCE_MS);
    });
    $effect.root(() => {
      $effect(() => {
        void app.workspace?.id;
        this.queue(0);
        void this.refreshFindings();
      });
    });
  }

  /** The knowledge base's findings (drift, the lint, ingest, supersession). */
  readonly refreshFindings = singleFlight(async () => {
    if (!app.workspace) {
      this.findings = [];
      return;
    }
    const ws = app.workspace.id;
    const found = await api.knowledgeFindings().catch(() => null);
    if (!found || ws !== app.workspace?.id) return;
    this.findings = found.map((f, i) => findingItem(f, i));
  });

  readonly queue = coalesce(() => void this.refresh(), DEBOUNCE_MS);
  private readonly queueFindings = coalesce(() => void this.refreshFindings(), DEBOUNCE_MS);

  /** Read Ken's waiting items: every ingest card's waits, and the wiki's
   *  findings. Optional reads: a failure keeps what was there. One read at
   *  a time (see `singleFlight`). */
  readonly refresh = singleFlight(async () => {
    if (!app.workspace) {
      this.ken = [];
      return;
    }
    const ws = app.workspace.id;
    this.loading = true;
    try {
      const out: InboxItem[] = [];
      const overview = await api.ingestOverview().catch(() => null);
      if (overview && ws === app.workspace?.id) this.raw = overview.raw;
      if (overview) {
        const cards = [
          ...overview.ingested.filter((s) => s.waiting > 0).map((s) => s.id),
          ...overview.raw.filter((s) => s.waiting > 0 && s.cardId !== null).map((s) => s.cardId as number),
        ];
        const unique = [...new Set(cards)];
        const read = await Promise.all(unique.map((id) => api.ingestCard(id).catch(() => null)));
        for (const card of read) {
          if (!card) continue;
          for (const p of card.proposals) out.push(proposalItem(p, card.id, card.note, overview.projectId));
        }
      }
      // Proposals no card holds: rulings proposed in chat, edits drift held.
      const loose = await api.looseProposals().catch(() => []);
      for (const p of loose) out.push(proposalItem(p, 0, "", p.projectId, p.from));
      // The repo analyses that wait for a person (role rules and controls).
      const analyses = await api.analysisProposals().catch(() => []);
      for (const a of analyses) out.push(analysisItem(a));
      if (ws === app.workspace?.id) this.ken = out;
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

<script lang="ts">
  // The Inbox (the Briefing design): every decision in one queue, filtered by
  // who it is from. The list on the left, the item on the right with its
  // actions. Dealing with an item here deals with it where it came from.
  import { app } from "../lib/app.svelte";
  import { api, type IngestProposal } from "../lib/api";
  import { inbox, type InboxGroup, type InboxItem } from "../lib/inbox.svelte";
  import { conflicts } from "../lib/conflicts.svelte";
  import { chats } from "../lib/chats.svelte";
  import { openInRepo, openPageLink } from "../lib/day.svelte";
  import { toast } from "../lib/toast.svelte";
  import { timeAgo } from "../lib/format";
  import { pageLinkTarget, renderBody, renderTitle } from "../lib/markdown";
  import { raidDetail } from "../lib/ingestCard";
  import ProposalDetail from "../ingests/ProposalDetail.svelte";
  import ConflictDetail from "../files/ConflictDetail.svelte";
  import InboxKindIcon from "./InboxKindIcon.svelte";
  import Loading from "../lib/ui/Loading.svelte";
  import EmptyState from "../lib/ui/EmptyState.svelte";
  import ScreenHeader from "../lib/ui/ScreenHeader.svelte";
  import CircleCheck from "@lucide/svelte/icons/circle-check";
  import BellOff from "@lucide/svelte/icons/bell-off";

  type Filter = InboxGroup | "all";
  let filter = $state<Filter>("all");
  const filters: { key: Filter; label: string }[] = [
    { key: "all", label: "All" },
    { key: "ken", label: "Ken" },
    { key: "sync", label: "Sync & files" },
  ];

  const list = $derived(inbox.items.filter((i) => filter === "all" || i.group === filter));
  const sel = $derived(inbox.selectedItem ?? list[0] ?? null);
  const isDone = $derived(!!sel && inbox.done.some((d) => d.id === sel.id));

  let busy = $state(false);
  let doneNote = $state<Record<string, string>>({});

  function when(item: InboxItem): string {
    return item.when ? timeAgo(item.when) : "";
  }

  /** Run an item's action; `note` says what it did, from its result when
   *  it is a function. */
  async function act<T>(item: InboxItem, run: () => Promise<T>, note: string | ((result: T) => string)) {
    busy = true;
    try {
      const result = await run();
      doneNote = { ...doneNote, [item.id]: typeof note === "function" ? note(result) : note };
      inbox.resolved(item);
      inbox.select(item.id);
    } catch (e) {
      toast.error("That did not go through", e);
    } finally {
      busy = false;
    }
  }

  function applyLabel(kind: IngestProposal["kind"]): string {
    if (kind === "page" || kind === "new page") return "Apply";
    if (kind === "ruling") return "Add to the decisions log";
    if (kind === "risk" || kind === "issue") return "Add to the register";
    return "Accept";
  }

  function appliedNote(kind: IngestProposal["kind"]): string {
    if (kind === "ruling" || kind === "risk" || kind === "issue") return "Added";
    if (kind === "ticket") return "Accepted";
    return "Applied";
  }

  /** The actions an item offers: the first is primary, at most three. */
  function actions(item: InboxItem): { label: string; run: () => void; primary?: boolean }[] {
    const s = item.source;
    switch (s.type) {
      case "proposal": {
        const p = s.proposal;
        const apply = { label: applyLabel(p.kind), primary: true, run: () => void act(item, () => api.applyPageProposal(p.id, s.projectId), appliedNote(p.kind)) };
        const discard = { label: "Discard", run: () => void act(item, () => api.resolveReviewItem(p.id, s.projectId), "Discarded") };
        // A ruling proposed in chat or an edit drift held comes from no note.
        if (!s.note) return [apply, discard];
        const note = { label: "Open the note", run: () => void openInRepo(s.projectId, s.note) };
        return [apply, discard, note];
      }
      case "finding": {
        const f = s.finding;
        const out: { label: string; run: () => void; primary?: boolean }[] = [];
        // A supersession is confirmed (Ken writes it into the log) or
        // dismissed; any other finding is marked done once seen to.
        const confirms = f.kind === "supersession";
        if (confirms && f.itemId && f.projectId) {
          out.push({ label: "Confirm", primary: true, run: () => void act(item, () => api.confirmFinding(f.itemId as number, f.projectId), "Confirmed") });
        }
        // An open question left unsettled can go into the RAID register as
        // an issue: one click, one commit, and the finding is done.
        if (f.kind === "question" && f.itemId && f.projectId) {
          const fid = f.itemId;
          const pid = f.projectId;
          out.push({ label: "Raise as an issue", primary: true, run: () => void act(item, () => api.raiseIssue(fid, pid), (id) => `Raised as ${id}`) });
        }
        if (f.path && f.projectId) out.push({ label: "Open the page", primary: out.length === 0, run: () => void openInRepo(f.projectId as string, f.path as string) });
        if (f.itemId && f.projectId) {
          const label = confirms ? "Dismiss" : "Mark as done";
          out.push({ label, run: () => void act(item, () => api.resolveReviewItem(f.itemId as number, f.projectId), confirms ? "Dismissed" : "Marked as done") });
        }
        return out;
      }
      case "analysis": {
        // Nothing applies until accepted; Settings › Workspace edits what is.
        const a = s.analysis;
        return [
          { label: "Accept", primary: true, run: () => void act(item, () => api.acceptAnalysis(a.itemId, a.projectId), "Accepted") },
          { label: "Dismiss", run: () => void act(item, () => api.resolveReviewItem(a.itemId, a.projectId), "Dismissed") },
        ];
      }
      case "conflict":
        return [];
      case "failed":
        return [{ label: "Open in Files", primary: true, run: () => app.openInFiles(s.relPath) }];
      case "chat":
        return [
          {
            label: "Answer in Ask Ken",
            primary: true,
            run: () => {
              chats.open = true;
              void chats.select(s.chatId);
            },
          },
        ];
    }
  }

  async function ignoreFile(item: InboxItem) {
    if (item.source.type !== "failed") return;
    const rel = item.source.relPath;
    await act(item, () => app.ignoreFile(rel), "Ignored");
  }

  function selectItem(id: string) {
    inbox.select(id);
  }

  /** A link in a finding's body: a `[[page]]` opens the page, found from
   *  the finding's note; a web link opens in the browser. */
  function followLink(e: MouseEvent, item: InboxItem) {
    const a = (e.target as HTMLElement).closest("a");
    if (!a || item.source.type !== "finding") return;
    e.preventDefault();
    const f = item.source.finding;
    const href = a.getAttribute("href");
    const page = pageLinkTarget(href);
    if (page && f.projectId) void openPageLink(f.projectId, f.path ?? "", page);
    else if (href && /^https?:/i.test(href)) void api.openWebUrl(href).catch((err) => toast.error("Could not open the link", err));
  }

  const emptyHead = $derived(filter === "all" ? "Nothing waiting on you" : "Nothing here");
</script>

<div class="inbox">
  <div class="listcol">
    <div class="lhead">
      <ScreenHeader title="Inbox" sub="{inbox.count} waiting" />
      <div class="k-tabs" role="tablist">
        {#each filters as f (f.key)}
          <button class:on={filter === f.key} role="tab" aria-selected={filter === f.key} onclick={() => (filter = f.key)}>
            {f.label} · {inbox.countFor(f.key)}
          </button>
        {/each}
      </div>
    </div>
    <div class="rows">
      {#each list as it (it.id)}
        <button class="row" class:sel={sel?.id === it.id} onclick={() => selectItem(it.id)}>
          <InboxKindIcon item={it} />
          <span class="rtext">
            <span class="rtitle" title={it.tip ?? undefined}>{@html renderTitle(it.title)}</span>
            <span class="rmeta">{it.kind} · {it.from}{when(it) ? ` · ${when(it)}` : ""}</span>
          </span>
        </button>
      {/each}
      {#if list.length === 0 && inbox.loading}
        <Loading label="Gathering what waits on you…" lines={3} />
      {:else if list.length === 0}
        <EmptyState title="Nothing here" />
      {/if}
      {#if inbox.done.length > 0}
        <div class="t-label donehead">Done · {inbox.done.length}</div>
        {#each inbox.done as it (it.id)}
          <button class="done" class:sel={sel?.id === it.id} onclick={() => selectItem(it.id)}>
            <CircleCheck size={14} strokeWidth={1.75} aria-hidden="true" />
            <span>{@html renderTitle(it.title)}</span>
          </button>
        {/each}
      {/if}
    </div>
  </div>

  <main class="detail">
    {#if sel}
      <div class="dinner">
        <div class="dkind {sel.tone}"><InboxKindIcon item={sel} bare />{sel.kind}</div>
        <h2 title={sel.tip ?? undefined}>{@html renderTitle(sel.title)}</h2>
        <div class="t-small" title={sel.where ?? undefined}>
          From {sel.from}{sel.source.type === "proposal" && sel.source.proposal.who ? ` · ${sel.source.proposal.who}` : ""}{when(sel) ? ` · ${when(sel)}` : ""}
        </div>

        {#if sel.source.type === "proposal"}
          <!-- The title says what it is; the detail is what it writes: the
               line ingest wrote for a risk or issue, else the change. -->
          {#if sel.source.proposal.kind === "risk" || sel.source.proposal.kind === "issue"}
            {#if raidDetail(sel.source.proposal.payload)}
              <p class="quote">{@html renderTitle(raidDetail(sel.source.proposal.payload))}</p>
            {/if}
          {:else if sel.source.proposal.payload}
            <ProposalDetail payload={sel.source.proposal.payload} />
          {/if}
        {:else if sel.source.type === "finding"}
          {#if sel.source.finding.detail.trim()}
            {@const item = sel}
            <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
            <div class="body md" onclick={(e) => followLink(e, item)}>{@html renderBody(sel.source.finding.detail)}</div>
          {/if}
        {:else if sel.source.type === "analysis"}
          {#if sel.source.analysis.roles.length > 0}
            <div class="t-label">Role rules</div>
            <ul class="plist">
              {#each sel.source.analysis.roles as r (r.pattern)}
                <li><code>{r.pattern}</code> is a <strong>{r.role}</strong></li>
              {/each}
            </ul>
          {/if}
          {#if sel.source.analysis.controls.length > 0}
            <div class="t-label">Controls</div>
            <ul class="plist">
              {#each sel.source.analysis.controls as c, i (i)}
                <li>“{c.question}” → <code>{c.expect}</code></li>
              {/each}
            </ul>
          {/if}
        {:else if sel.source.type === "conflict"}
          <ConflictDetail item={sel.source.conflict} />
        {:else if sel.source.type === "failed"}
          {#if sel.source.error}<p class="body">{sel.source.error}</p>{/if}
        {/if}

        {#if isDone}
          <div class="donebar">
            <CircleCheck size={15} strokeWidth={1.75} aria-hidden="true" />
            {doneNote[sel.id] ?? "Done"}
          </div>
        {:else}
          <div class="acts">
            {#each actions(sel) as a, i}
              <button class="btn" class:btn-primary={a.primary && i === 0} disabled={busy} onclick={a.run}>{a.label}</button>
            {/each}
            <span class="grow"></span>
            {#if sel.source.type === "failed"}
              <button class="btn btn-ghost" disabled={busy} onclick={() => sel && void ignoreFile(sel)}>
                <BellOff size={14} strokeWidth={1.75} aria-hidden="true" /> Ignore this file
              </button>
            {/if}
          </div>
        {/if}
      </div>
    {:else}
      <EmptyState title={emptyHead} />
    {/if}
  </main>
</div>

<style>
  .inbox {
    flex: 1;
    min-width: 0;
    display: flex;
    min-height: 0;
  }
  .listcol {
    width: 360px;
    flex: none;
    border-right: 1px solid var(--line-soft);
    display: flex;
    flex-direction: column;
    min-height: 0;
  }
  .lhead {
    padding: 24px 20px 0;
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .rows {
    flex: 1;
    overflow-y: auto;
    padding: 10px 10px 20px;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .row {
    display: flex;
    gap: 11px;
    padding: 11px 10px;
    border-radius: 9px;
    border: none;
    background: transparent;
    text-align: left;
    color: var(--ink);
  }
  .row:hover {
    background: var(--line-soft);
  }
  .row.sel,
  .done.sel {
    background: var(--accent-soft);
  }
  .rtext {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  /* Two lines at most, cut where a line wraps (between words), with an
     ellipsis; the detail shows the whole title. */
  .rtitle {
    font-size: 13.5px;
    font-weight: 600;
    line-height: 1.35;
    display: -webkit-box;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    overflow: hidden;
  }
  .rmeta {
    font-size: 12px;
    color: var(--ink-3);
  }
  .donehead {
    padding: 16px 10px 6px;
  }
  .done {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 10px;
    border-radius: 8px;
    border: none;
    background: transparent;
    text-align: left;
    color: var(--ink-3);
    font-size: 13px;
  }
  .done :global(svg) {
    color: var(--ok);
    flex: none;
  }
  .done span {
    flex: 1;
    display: -webkit-box;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 1;
    line-clamp: 1;
    overflow: hidden;
  }
  .detail {
    flex: 1;
    min-width: 0;
    overflow-y: auto;
    padding: 32px 40px;
  }
  .dinner {
    max-width: 680px;
    display: flex;
    flex-direction: column;
    gap: 16px;
  }
  .dkind {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 12px;
    font-weight: 600;
    letter-spacing: 0.05em;
    text-transform: uppercase;
  }
  .dkind.attn {
    color: var(--attn-ink);
  }
  .dkind.danger {
    color: var(--danger);
  }
  .dkind.accent {
    color: var(--accent-ink);
  }
  .dkind.ink {
    color: var(--ink-2);
  }
  h2 {
    margin: 0;
    font-family: var(--font-serif);
    font-size: 28px;
    font-weight: 500;
    line-height: 1.2;
  }
  .body {
    margin: 0;
    font-size: 15px;
    line-height: 1.65;
    color: var(--ink-2);
    white-space: pre-wrap;
  }
  /* A finding's body is Markdown: the text the title did not hold, the
     source's words as a quote, a link to the note. */
  .body.md {
    white-space: normal;
  }
  .body.md :global(p) {
    margin: 0 0 10px;
  }
  .body.md :global(p:last-child) {
    margin-bottom: 0;
  }
  .body.md :global(blockquote) {
    margin: 0 0 10px;
    padding-left: 12px;
    border-left: 2px solid var(--line);
    color: var(--ink-2);
  }
  .body.md :global(code) {
    font-family: var(--font-mono);
    font-size: 0.88em;
  }
  .body.md :global(a) {
    color: var(--accent-ink);
    text-decoration: underline;
    text-underline-offset: 2px;
    cursor: pointer;
  }
  /* What ingest wrote for a risk or issue, as a ruling's card quotes it. */
  .quote {
    margin: 0;
    padding-left: 12px;
    border-left: 2px solid var(--line);
    font-size: 14px;
    line-height: 1.6;
    color: var(--ink-2);
  }
  .plist {
    margin: 0;
    padding-left: 20px;
    font-size: 14px;
    line-height: 1.7;
    color: var(--ink-2);
  }
  .acts {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
    padding-top: 4px;
  }
  .grow {
    flex: 1;
  }
  .donebar {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 12px 14px;
    border-radius: 10px;
    background: var(--ok-soft);
    color: var(--ok);
    font-size: 13.5px;
    font-weight: 600;
  }
</style>

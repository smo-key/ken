<script lang="ts">
  // The Inbox (the Briefing design): every decision in one queue, filtered by
  // who it is from. The list on the left, the item on the right with its
  // actions. Dealing with an item here deals with it where it came from.
  import { app } from "../lib/app.svelte";
  import { scope } from "../lib/scope.svelte";
  import { api } from "../lib/api";
  import { inbox, type InboxGroup, type InboxItem } from "../lib/inbox.svelte";
  import { families } from "../lib/families.svelte";
  import { conflicts } from "../lib/conflicts.svelte";
  import { chats } from "../lib/chats.svelte";
  import { openInRepo } from "../lib/day.svelte";
  import { toast } from "../lib/toast.svelte";
  import { timeAgo } from "../lib/format";
  import ProposalDetail from "../ingests/ProposalDetail.svelte";
  import ConflictDetail from "../files/ConflictDetail.svelte";
  import InboxKindIcon from "./InboxKindIcon.svelte";
  import Loading from "../lib/ui/Loading.svelte";
  import CircleCheck from "@lucide/svelte/icons/circle-check";
  import BellOff from "@lucide/svelte/icons/bell-off";

  type Filter = InboxGroup | "all";
  let filter = $state<Filter>("all");
  const filters: { key: Filter; label: string }[] = [
    { key: "all", label: "All" },
    { key: "people", label: "People" },
    { key: "ken", label: "Ken" },
    { key: "sync", label: "Sync & files" },
  ];

  const list = $derived(inbox.items.filter((i) => filter === "all" || i.group === filter));
  const sel = $derived(inbox.selectedItem ?? list[0] ?? null);
  const isDone = $derived(!!sel && inbox.done.some((d) => d.id === sel.id));

  let busy = $state(false);
  let reply = $state("");
  let replying = $state(false);
  /** Answering an escalation: a reply in its thread, or the answer that
   *  resolves it. */
  let escMode = $state<"reply" | "resolve" | null>(null);
  let doneNote = $state<Record<string, string>>({});

  function when(item: InboxItem): string {
    return item.when ? timeAgo(item.when) : "";
  }

  async function act(item: InboxItem, run: () => Promise<unknown>, note: string) {
    busy = true;
    try {
      await run();
      doneNote = { ...doneNote, [item.id]: note };
      inbox.resolved(item);
      inbox.select(item.id);
      replying = false;
      reply = "";
    } catch (e) {
      toast.error("That did not go through", e);
    } finally {
      busy = false;
    }
  }

  /** The actions an item offers: the first is primary, at most three. */
  function actions(item: InboxItem): { label: string; run: () => void; primary?: boolean }[] {
    const s = item.source;
    switch (s.type) {
      case "family": {
        const id = s.item.id;
        if (s.item.kind === "task") {
          return [
            { label: "Accept", primary: true, run: () => void act(item, () => families.acceptTask(s.familyId, id), "Accepted. It is on your list.") },
            { label: "Not mine", run: () => void act(item, () => families.setItemStatus(s.familyId, id, "archived"), "Sent back as not yours.") },
            { label: "Reply", run: () => (replying = true) },
          ];
        }
        return [{ label: "Mark as read", primary: true, run: () => void act(item, () => families.setItemStatus(s.familyId, id, "archived"), "Marked as read.") }];
      }
      case "proposal": {
        const p = s.proposal;
        const apply = { label: p.kind === "page" || p.kind === "new page" ? "Apply" : "Accept", primary: true, run: () => void act(item, () => api.applyPageProposal(p.id, s.projectId), p.kind === "ruling" ? "Accepted. It is in the decisions log." : p.kind === "ticket" ? "Accepted. The ticket is written." : "Applied. The page cites the note.") };
        const discard = { label: "Discard", run: () => void act(item, () => api.resolveReviewItem(p.id, s.projectId), "Discarded.") };
        const note = { label: "Open the note", run: () => void openInRepo(s.projectId, s.note) };
        return p.kind === "ruling" && !p.canAccept ? [note, discard] : [apply, discard, note];
      }
      case "finding": {
        const f = s.finding;
        const out: { label: string; run: () => void; primary?: boolean }[] = [];
        // A proposed page change applies, or is discarded; any other finding
        // is marked done once seen to.
        if (f.kind === "proposal" && f.itemId && f.projectId) {
          out.push({ label: "Apply", primary: true, run: () => void act(item, () => api.applyPageProposal(f.itemId as number, f.projectId), "Applied.") });
        }
        if (f.path && f.projectId) out.push({ label: "Open the page", primary: out.length === 0, run: () => void openInRepo(f.projectId as string, f.path as string) });
        if (f.itemId && f.projectId) {
          const label = f.kind === "proposal" ? "Discard" : "Mark as done";
          out.push({ label, run: () => void act(item, () => api.resolveReviewItem(f.itemId as number, f.projectId), f.kind === "proposal" ? "Discarded." : "Marked as done.") });
        }
        return out;
      }
      case "conflict":
        return [];
      case "escalation": {
        const e = s.escalation;
        return [
          { label: "Resolve", primary: true, run: () => ((escMode = "resolve"), (reply = "")) },
          { label: "Reply", run: () => ((escMode = "reply"), (reply = "")) },
          { label: "Open it", run: () => void openInRepo(e.projectId, e.relPath) },
        ];
      }
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

  async function sendReply(item: InboxItem) {
    const s = item.source;
    if (s.type !== "family" || !reply.trim()) return;
    await act(item, () => families.pushBack(s.familyId, s.item.id, reply.trim()), "Your reply was sent.");
  }

  async function sendEscalation(item: InboxItem) {
    const s = item.source;
    if (s.type !== "escalation" || !reply.trim() || !escMode) return;
    const e = s.escalation;
    const text = reply.trim();
    if (escMode === "resolve") {
      await act(item, () => api.escalationReply(e.projectId, e.relPath, text, true), "Resolved. Your answer is in its thread.");
      escMode = null;
      return;
    }
    // A reply keeps it open: it stays in the list.
    busy = true;
    try {
      await api.escalationReply(e.projectId, e.relPath, text, false);
      toast.show("Your reply is in its thread.");
      reply = "";
      escMode = null;
    } catch (err) {
      toast.error("That did not go through", err);
    } finally {
      busy = false;
    }
  }

  async function ignoreFile(item: InboxItem) {
    if (item.source.type !== "failed") return;
    const rel = item.source.relPath;
    await act(item, () => app.ignoreFile(rel), "Ignored, for you only.");
  }

  function selectItem(id: string) {
    inbox.select(id);
    replying = false;
    escMode = null;
    reply = "";
  }

  const emptyHead = $derived(filter === "all" ? "Nothing waiting on you" : "Nothing here");
</script>

<div class="inbox">
  <div class="listcol">
    <div class="lhead">
      <div class="titlerow">
        <h1>Inbox</h1>
        <span class="t-small">{inbox.count} waiting</span>
      </div>
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
            <span class="rtitle">{it.title}</span>
            <span class="rmeta">{it.kind} · {it.from}{when(it) ? ` · ${when(it)}` : ""}</span>
          </span>
        </button>
      {/each}
      {#if list.length === 0 && inbox.loading}
        <Loading label="Gathering what waits on you…" lines={3} />
      {:else if list.length === 0}
        <p class="none">
          {inbox.done.length > 0 ? "Nothing here. What you dealt with is under Done." : "Nothing here."}
        </p>
      {/if}
      {#if inbox.done.length > 0}
        <div class="t-label donehead">Done · {inbox.done.length}</div>
        {#each inbox.done as it (it.id)}
          <button class="done" class:sel={sel?.id === it.id} onclick={() => selectItem(it.id)}>
            <CircleCheck size={14} strokeWidth={1.75} aria-hidden="true" />
            <span>{it.title}</span>
          </button>
        {/each}
      {/if}
    </div>
  </div>

  <main class="detail">
    {#if sel}
      <div class="dinner">
        <div class="dkind {sel.tone}"><InboxKindIcon item={sel} bare />{sel.kind}</div>
        <h2>{sel.title}</h2>
        <div class="t-small">
          From {sel.from}{when(sel) ? ` · ${when(sel)}` : ""}{sel.where ? ` · ${sel.where.split("/").pop()}` : ""}
        </div>

        {#if sel.source.type === "family"}
          {#if sel.source.item.body.trim()}
            <p class="body">{sel.source.item.body}</p>
          {/if}
          {#if sel.source.item.task?.due}
            <p class="t-small">Target {sel.source.item.task.due}</p>
          {/if}
        {:else if sel.source.type === "proposal"}
          {#if sel.source.proposal.body.trim()}
            <p class="body">{sel.source.proposal.body}</p>
          {/if}
          {#if sel.source.proposal.payload && (sel.source.proposal.kind === "page" || sel.source.proposal.kind === "new page")}
            <ProposalDetail payload={sel.source.proposal.payload} />
          {/if}
          {#if sel.source.proposal.kind === "ruling" && !sel.source.proposal.canAccept}
            <p class="t-small">Only {sel.source.proposal.decider ?? "its decider"} can accept this ruling.</p>
          {/if}
        {:else if sel.source.type === "finding"}
          <p class="body">{sel.source.finding.detail}</p>
        {:else if sel.source.type === "conflict"}
          <ConflictDetail item={sel.source.conflict} />
        {:else if sel.source.type === "failed"}
          <p class="body">
            Ken couldn't read {sel.source.relPath.split("/").pop()}{sel.source.error ? `: ${sel.source.error}` : ""}. It may be a scan
            with no text, or a protected file. It is still found by name.
          </p>
        {:else if sel.source.type === "escalation"}
          <p class="body">
            {sel.source.escalation.raisedBy || "A teammate"} raised this decision to you{sel.source.escalation.ticket ? ` on ${sel.source.escalation.ticket}` : ""}.{sel.source.escalation.blocks ? ` It blocks ${sel.source.escalation.blocks}.` : ""}
            Resolve it with your answer, or reply in its thread to ask something first. Both are written to
            the file in {sel.source.escalation.repo}.
          </p>
        {:else if sel.source.type === "chat"}
          <p class="body">Ken stopped in “{sel.source.title || "a chat"}” to ask you something. Answer it in Ask Ken and it carries on.</p>
        {/if}

        {#if isDone}
          <div class="donebar">
            <CircleCheck size={15} strokeWidth={1.75} aria-hidden="true" />
            {doneNote[sel.id] ?? "Done."}
          </div>
        {:else}
          {#if replying && sel.source.type === "family"}
            <textarea class="textarea" bind:value={reply} rows="3" placeholder="Reply to {sel.from}…"></textarea>
          {/if}
          {#if escMode && sel.source.type === "escalation"}
            <textarea
              class="textarea"
              bind:value={reply}
              rows="3"
              placeholder={escMode === "resolve" ? "Your answer, as the decision…" : `Reply to ${sel.source.escalation.raisedBy || "them"}…`}
            ></textarea>
          {/if}
          <div class="acts">
            {#if replying && sel.source.type === "family"}
              <button class="btn btn-primary" disabled={busy || !reply.trim()} onclick={() => sel && void sendReply(sel)}>Send reply</button>
              <button class="btn btn-ghost" onclick={() => (replying = false)}>Cancel</button>
            {:else if escMode && sel.source.type === "escalation"}
              <button class="btn btn-primary" disabled={busy || !reply.trim()} onclick={() => sel && void sendEscalation(sel)}>
                {escMode === "resolve" ? "Resolve" : "Send reply"}
              </button>
              <button class="btn btn-ghost" onclick={() => (escMode = null)}>Cancel</button>
            {:else}
              {#each actions(sel) as a, i}
                <button class="btn" class:btn-primary={a.primary && i === 0} disabled={busy} onclick={a.run}>{a.label}</button>
              {/each}
            {/if}
            <span class="grow"></span>
            {#if sel.source.type === "failed"}
              <button class="btn btn-ghost" title="Stop showing this file's problems (only for you)" disabled={busy} onclick={() => sel && void ignoreFile(sel)}>
                <BellOff size={14} strokeWidth={1.75} aria-hidden="true" /> Ignore this file
              </button>
            {/if}
          </div>
        {/if}
      </div>
    {:else}
      <div class="k-empty quiet">
        <span class="head">{emptyHead}</span>
        <span class="body">When a teammate sends you something, or Ken needs a decision, it shows up here.</span>
      </div>
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
  .titlerow {
    display: flex;
    align-items: baseline;
    gap: 10px;
  }
  h1 {
    margin: 0;
    font-family: var(--font-serif);
    font-size: 28px;
    font-weight: 500;
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
  .rtitle {
    font-size: 13.5px;
    font-weight: 600;
    line-height: 1.35;
  }
  .rmeta {
    font-size: 12px;
    color: var(--ink-3);
  }
  .none {
    padding: 24px 10px;
    margin: 0;
    font-size: 13.5px;
    color: var(--ink-3);
    line-height: 1.5;
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
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
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
  .quiet {
    max-width: 520px;
  }
</style>

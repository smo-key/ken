<script lang="ts">
  import { chats, SUGGESTED_PROMPTS } from "../lib/chats.svelte";
  import { app } from "../lib/app.svelte";
  import { renderMarkdown } from "../lib/markdown";
  import { inMember, parseCitation } from "../lib/citation";
  import { parseEditProposal } from "../lib/chats.svelte";
  import QuestionCard from "./QuestionCard.svelte";
  import EditReview from "./EditReview.svelte";
  import ToolCardView from "./ToolCard.svelte";
  import { parseToolCard } from "./toolCard";
  import { api } from "../lib/api";
  import { scope } from "../lib/scope.svelte";
  import { toast } from "../lib/toast.svelte";
  import { pickMessage, pickedIds, sendableMessages, type Pick } from "../lib/chatPick";
  import Layers from "@lucide/svelte/icons/layers";

  let scroller = $state<HTMLDivElement | null>(null);

  // Follow the conversation, but only when the user is already near the bottom —
  // don't yank them down while they're reading earlier messages.
  $effect(() => {
    const len = chats.transcript.length;
    const streamed = chats.draft.length;
    void len;
    void streamed;
    const el = scroller;
    if (!el) return;
    const nearBottom =
      el.scrollHeight - el.scrollTop - el.clientHeight < 120;
    if (nearBottom) el.scrollTop = el.scrollHeight;
  });

  // A cited source opens in Files at its line or heading; a ken:// address
  // focuses its workspace member first. Nothing opens unless clicked.
  async function onClick(e: MouseEvent) {
    const a = (e.target as HTMLElement).closest("a");
    if (!a) return;
    e.preventDefault();
    const parsed = parseCitation(a.getAttribute("href") ?? "");
    if (!parsed) return;
    const c = inMember(parsed, app.members, app.focused);
    if (c.projectId && c.projectId !== app.focused) {
      if (c.projectId === "workspace") return;
      await app.focusMember(c.projectId);
    }
    app.openAt(c.path, { line: c.line, anchor: c.anchor });
  }

  const working = $derived(chats.active?.status === "working");

  // Messages picked to send to Ingest: a click on a message's Ingest mark
  // takes or drops it, a shift-click takes every message from the last one.
  let pick = $state<Pick>({ selected: new Set(), anchor: null });
  let sending = $state(false);
  const order = $derived(sendableMessages(chats.transcript).map((m) => m.id));
  const picked = $derived(pickedIds(order, pick.selected));

  $effect(() => {
    void chats.activeId;
    pick = { selected: new Set(), anchor: null };
  });

  function togglePick(e: MouseEvent, id: number) {
    e.stopPropagation();
    pick = pickMessage(order, pick, id, e.shiftKey);
  }

  async function sendPicked() {
    const chatId = chats.activeId;
    const projectId = app.focused;
    if (!chatId || !projectId || picked.length === 0 || sending) return;
    sending = true;
    try {
      await api.ingestAddChat(scope.team, projectId, chatId, picked);
      toast.show(`${picked.length} ${picked.length === 1 ? "message is" : "messages are"} in Ingest. Ken reads them now.`);
      pick = { selected: new Set(), anchor: null };
    } catch (e) {
      toast.error("Could not send the messages to Ingest", e);
    } finally {
      sending = false;
    }
  }
</script>

{#snippet pickMark(id: number)}
  {#if id > 0}
    <button
      class="pick"
      class:on={pick.selected.has(id)}
      title={pick.selected.has(id) ? "Picked for Ingest (shift-click for a range)" : "Pick for Ingest (shift-click for a range)"}
      aria-label="Pick for Ingest"
      aria-pressed={pick.selected.has(id)}
      onclick={(e) => togglePick(e, id)}
    >
      <Layers size={12} strokeWidth={1.75} />
    </button>
  {/if}
{/snippet}

<!-- Delegated clicks on citation links; Enter on a focused link fires the
     same click, so keyboards are covered. -->
<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
<div class="scroll" bind:this={scroller} onclick={onClick} role="log">
  {#if chats.transcript.length === 0}
    <div class="starters">
      <div class="hint">Ask anything about this project's knowledge.</div>
      {#each SUGGESTED_PROMPTS as p (p)}
        <button class="starter" onclick={() => chats.send(p)}>{p}</button>
      {/each}
    </div>
  {/if}

  {#each chats.transcript as msg (msg.id + msg.role + msg.createdAt)}
    {#if msg.role === "user"}
      <div class="said user-said" class:picked={pick.selected.has(msg.id)}>
        {@render pickMark(msg.id)}
        <div class="bubble user" class:pending={"pending" in msg && msg.pending}>{msg.content}</div>
      </div>
    {:else if msg.role === "assistant"}
      <div class="said" class:picked={pick.selected.has(msg.id)}>
        <div class="assistant">
          <span class="mark">K</span>
          <div class="md">{@html renderMarkdown(msg.content)}</div>
        </div>
        {@render pickMark(msg.id)}
      </div>
    {:else if msg.role === "activity"}
      <div class="activity mono">{msg.content}</div>
    {:else if msg.role === "tool"}
      {@const card = parseToolCard(msg.content, working)}
      {#if card}<div class="tool"><ToolCardView {card} /></div>
      {:else}<div class="activity mono">{msg.content}</div>{/if}
    {:else if msg.role === "question"}
      <QuestionCard {msg} />
    {:else if msg.role === "edit"}
      {@const proposal = parseEditProposal(msg.content)}
      {#if proposal}<div class="edit"><EditReview messageId={msg.id} {proposal} /></div>{/if}
    {:else}
      <div class="divider"><span>{msg.content}</span></div>
    {/if}
  {/each}

  {#if chats.draft}
    <div class="assistant streaming" aria-live="polite">
      <span class="mark">K</span>
      <div class="md">{@html renderMarkdown(chats.draft)}</div>
    </div>
  {:else if working}
    <div class="working">
      <span class="pulse"></span>Ken is working…
    </div>
  {/if}

  {#if picked.length > 0}
    <div class="pick-bar">
      <span>{picked.length} {picked.length === 1 ? "message" : "messages"} picked</span>
      <span class="sp"></span>
      <button class="btn btn-small btn-ghost" onclick={() => (pick = { selected: new Set(), anchor: null })}>Cancel</button>
      <button class="btn btn-small btn-primary" disabled={sending} onclick={() => void sendPicked()}>Send to Ingest</button>
    </div>
  {/if}
</div>

<style>
  .scroll {
    /* Single-sourced gutter: the assistant mark (24px) + its row gap (10px), so
       activity/working/divider lines align under the assistant's text. */
    --gutter: 34px;
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 18px 16px;
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .starters {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin-top: 12px;
  }
  .hint {
    font-size: 12.5px;
    color: var(--ink-tertiary);
    margin-bottom: 4px;
  }
  .starter {
    text-align: left;
    font-size: 12.5px;
    padding: 9px 12px;
    border: 1px solid var(--border-strong);
    border-radius: 9px;
    background: var(--surface);
    color: var(--ink);
    line-height: 1.4;
  }
  .starter:hover {
    background: var(--sunken);
  }
  /* A message with its Ingest mark, which shows on hover or once picked. */
  .said {
    display: flex;
    align-items: flex-start;
    gap: 6px;
  }
  .said.user-said {
    justify-content: flex-end;
  }
  .said.picked .bubble.user,
  .said.picked .assistant .md {
    outline: 1.5px solid color-mix(in srgb, var(--accent) 45%, transparent);
    outline-offset: 3px;
    border-radius: 8px;
  }
  .pick {
    flex: none;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    margin-top: 2px;
    border: none;
    border-radius: 6px;
    background: transparent;
    color: var(--ink-tertiary);
    opacity: 0;
  }
  .said:hover .pick,
  .pick:focus-visible,
  .pick.on {
    opacity: 1;
  }
  .pick:hover {
    background: var(--sunken);
    color: var(--ink);
  }
  .pick.on {
    color: var(--accent-deep);
    background: color-mix(in srgb, var(--accent) 12%, transparent);
  }
  .pick-bar {
    position: sticky;
    bottom: 0;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 10px;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-control);
    background: var(--surface);
    box-shadow: var(--shadow-card);
    font-size: 12.5px;
  }
  .pick-bar .sp {
    flex: 1;
  }
  .bubble.user {
    align-self: flex-end;
    max-width: 85%;
    background: var(--sunken);
    border-radius: 12px 12px 4px 12px;
    padding: 9px 13px;
    font-size: 13.5px;
    line-height: 1.55;
    white-space: pre-wrap;
  }
  .bubble.user.pending {
    opacity: 0.6;
  }
  .assistant {
    display: flex;
    gap: 10px;
    max-width: 95%;
  }
  .mark {
    width: 24px;
    height: 24px;
    flex: none;
    border-radius: 6px;
    background: var(--ink);
    color: var(--paper);
    display: flex;
    align-items: center;
    justify-content: center;
    font-family: var(--font-serif);
    font-size: 13px;
  }
  .md {
    font-size: 13.5px;
    line-height: 1.6;
    min-width: 0;
  }
  .md :global(p) {
    margin: 0 0 8px;
  }
  .md :global(p:last-child) {
    margin-bottom: 0;
  }
  .md :global(pre) {
    background: var(--terminal-bg);
    color: var(--terminal-text);
    border-radius: 8px;
    padding: 10px 12px;
    overflow-x: auto;
    font-size: 12px;
  }
  .md :global(code) {
    font-family: var(--font-mono);
    font-size: 12px;
  }
  .md :global(h1),
  .md :global(h2),
  .md :global(h3) {
    font-family: var(--font-serif);
    font-weight: 500;
    margin: 10px 0 6px;
  }
  .md :global(ul),
  .md :global(ol) {
    margin: 0 0 8px;
    padding-left: 20px;
  }
  .md :global(table) {
    border-collapse: collapse;
    font-size: 12px;
  }
  .md :global(td),
  .md :global(th) {
    border: 1px solid var(--border);
    padding: 4px 8px;
  }
  .activity {
    font-size: 11px;
    line-height: 1.5;
    color: var(--ink-tertiary);
    padding-left: var(--gutter);
  }
  .edit,
  .tool {
    margin-left: var(--gutter);
  }
  .streaming .md :global(> :last-child)::after {
    content: "▍";
    margin-left: 1px;
    color: var(--ink-tertiary);
    animation: blink 1s steps(1) infinite;
  }
  @keyframes blink {
    50% {
      opacity: 0;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .streaming .md :global(> :last-child)::after {
      animation: none;
    }
  }
  .divider {
    display: flex;
    align-items: center;
    gap: 10px;
    margin: 2px 0;
    color: var(--ink-tertiary);
    font-size: 11px;
  }
  .divider::before,
  .divider::after {
    content: "";
    flex: 1;
    height: 1px;
    background: var(--border);
  }
  .working {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 12px;
    color: var(--ink-secondary);
    padding-left: var(--gutter);
  }
  .pulse {
    width: 7px;
    height: 7px;
    border-radius: 4px;
    background: var(--accent);
    animation: pulse 1.2s ease-in-out infinite;
  }
  @keyframes pulse {
    50% {
      opacity: 0.35;
    }
  }
</style>

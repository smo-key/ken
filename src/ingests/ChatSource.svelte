<script lang="ts">
  // From a chat, as a way to add a source on Ingest: one of this repo's
  // chats, whole or a few of its messages. Only the person's and Ken's turns
  // go; a click takes or drops a message, a shift-click takes every message
  // from the last one clicked.
  import { onMount } from "svelte";
  import { api, type ChatMessage, type ChatRow } from "../lib/api";
  import { app } from "../lib/app.svelte";
  import { pickMessage, pickedIds, sendableMessages, type Pick } from "../lib/chatPick";
  import { errorText } from "../lib/toast.svelte";
  import Check from "@lucide/svelte/icons/check";

  let { team, onAdded }: { team: string | null; onAdded: (path: string) => void } = $props();

  let rows = $state<ChatRow[]>([]);
  let chatId = $state("");
  let messages = $state<ChatMessage[]>([]);
  let pick = $state<Pick>({ selected: new Set(), anchor: null });
  let busy = $state(false);
  let error = $state<string | null>(null);

  const order = $derived(messages.map((m) => m.id));
  const count = $derived(pickedIds(order, pick.selected).length);

  onMount(async () => {
    rows = (await api.listChats().catch(() => [])).filter((r) => r.kind === "user" && !r.archived);
    chatId = rows[0]?.id ?? "";
  });

  $effect(() => {
    const id = chatId;
    pick = { selected: new Set(), anchor: null };
    if (!id) {
      messages = [];
      return;
    }
    void api
      .chatTranscript(id)
      .then((all) => {
        if (id === chatId) messages = sendableMessages(all);
      })
      .catch(() => {
        if (id === chatId) messages = [];
      });
  });

  function click(e: MouseEvent, id: number) {
    pick = pickMessage(order, pick, id, e.shiftKey);
  }

  function preview(text: string): string {
    const t = text.replace(/\s+/g, " ").trim();
    return t.length > 140 ? `${t.slice(0, 140)}…` : t;
  }

  async function add(whole: boolean) {
    const projectId = app.focused;
    if (!chatId || !projectId || busy) return;
    busy = true;
    error = null;
    try {
      const path = await api.ingestAddChat(team, projectId, chatId, whole ? null : pickedIds(order, pick.selected));
      pick = { selected: new Set(), anchor: null };
      onAdded(path);
    } catch (e) {
      error = errorText(e);
    } finally {
      busy = false;
    }
  }
</script>

<div class="chat-source">
  {#if rows.length === 0}
    <p class="note">No chats in this repo yet.</p>
  {:else}
    <select bind:value={chatId} aria-label="Chat">
      {#each rows as r (r.id)}<option value={r.id}>{r.title}</option>{/each}
    </select>
    <div class="msgs">
      {#each messages as m (m.id)}
        <button class="msg" class:on={pick.selected.has(m.id)} onclick={(e) => click(e, m.id)}>
          <span class="cb" class:on={pick.selected.has(m.id)}>
            {#if pick.selected.has(m.id)}<Check size={10} strokeWidth={2.5} />{/if}
          </span>
          <span class="who">{m.role === "user" ? "Me" : "Ken"}</span>
          <span class="text">{preview(m.content)}</span>
        </button>
      {:else}
        <p class="note">Nothing said in this chat yet.</p>
      {/each}
    </div>
    <p class="note">Shift-click takes every message from the last one clicked.</p>
    <div class="actions">
      <button class="btn btn-small" disabled={busy || messages.length === 0} onclick={() => void add(true)}>Add the whole chat</button>
      <button class="btn btn-small btn-primary" disabled={busy || count === 0} onclick={() => void add(false)}>
        Add {count} {count === 1 ? "message" : "messages"}
      </button>
    </div>
  {/if}
  {#if error}<p class="warn">{error}</p>{/if}
</div>

<style>
  .chat-source {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  select {
    font: inherit;
    font-size: 12.5px;
    padding: 5px 26px 5px 8px;
    border-radius: 7px;
    border: 1px solid var(--border-strong);
    background-color: var(--surface);
    color: var(--ink);
  }
  .msgs {
    display: flex;
    flex-direction: column;
    max-height: 280px;
    overflow-y: auto;
    border: 1px solid var(--border);
    border-radius: 9px;
    background: var(--surface);
  }
  .msg {
    display: flex;
    align-items: flex-start;
    gap: 7px;
    padding: 6px 8px;
    border: none;
    border-bottom: 1px solid var(--sunken);
    background: transparent;
    text-align: left;
    font: inherit;
    font-size: 12px;
    color: var(--ink);
    user-select: none;
  }
  .msg:last-child {
    border-bottom: none;
  }
  .msg:hover {
    background: var(--sunken-2);
  }
  .msg.on {
    background: color-mix(in srgb, var(--accent) 8%, transparent);
  }
  .cb {
    flex: none;
    width: 13px;
    height: 13px;
    margin-top: 2px;
    border-radius: 3px;
    border: 1.5px solid var(--border-strong);
    background: var(--surface);
    display: inline-flex;
    align-items: center;
    justify-content: center;
    color: var(--surface);
  }
  .cb.on {
    background: var(--accent);
    border-color: var(--accent);
  }
  .who {
    flex: none;
    width: 28px;
    font-weight: 600;
    color: var(--ink-secondary);
  }
  .text {
    flex: 1;
    min-width: 0;
    line-height: 1.45;
    color: var(--ink-secondary);
  }
  .actions {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
  }
  .note {
    margin: 0;
    font-size: 11.5px;
    color: var(--ink-tertiary);
  }
  .warn {
    margin: 0;
    font-size: 12px;
    color: var(--danger);
  }
</style>

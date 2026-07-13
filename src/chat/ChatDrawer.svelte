<script lang="ts">
  import { onMount } from "svelte";
  import { chats } from "../lib/chats.svelte";
  import ChatTranscript from "./ChatTranscript.svelte";
  import TerminalView from "./TerminalView.svelte";
  import type { ChatRow } from "../lib/api";

  let winW = $state(window.innerWidth);
  let overflowOpen = $state(false);
  let draft = $state("");

  onMount(() => void chats.init());

  const narrow = $derived(winW < 1140);
  const visibleTabs = $derived(chats.ordered.slice(0, 2));
  const overflow = $derived(chats.ordered.slice(2));
  const inTerminal = $derived(
    chats.terminalId !== null && chats.terminalId === chats.activeId,
  );

  function statusDot(row: ChatRow): string | null {
    switch (row.status) {
      case "working": return "var(--accent)";
      case "needs_input": return "var(--needs-input)";
      case "error": return "var(--danger)";
      default: return null;
    }
  }

  async function submit() {
    const text = draft.trim();
    if (!text || !chats.activeId) return;
    if (text === "/") {
      draft = "";
      await chats.enterTerminal();
      return;
    }
    draft = "";
    await chats.send(text);
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      void submit();
    } else if (e.key === "/" && draft === "") {
      e.preventDefault();
      void chats.enterTerminal();
    }
  }

  async function pickChat(id: string) {
    overflowOpen = false;
    await chats.select(id);
  }
</script>

<svelte:window onresize={() => (winW = window.innerWidth)} />

<div class="drawer" class:overlay={narrow}>
  <div class="tabs">
    {#each visibleTabs as row (row.id)}
      <button
        class="tab"
        class:active={row.id === chats.activeId}
        onclick={() => pickChat(row.id)}
        title={row.title}
      >
        <span class="diamond" class:ingest={row.kind === "ingest"}>◈</span>
        <span class="tab-title">{row.title}</span>
        {#if row.pinned}<span class="pin-mark">・pinned</span>{/if}
        {#if statusDot(row)}
          <span class="dot" style:background={statusDot(row)}></span>
        {/if}
      </button>
    {/each}
    {#if overflow.length > 0}
      <button class="more" onclick={() => (overflowOpen = !overflowOpen)}>
        +{overflow.length} ⌄
      </button>
    {/if}
    <button class="new" title="New chat" onclick={() => chats.newChat()}>+</button>
  </div>

  {#if overflowOpen}
    <div class="overflow-menu">
      {#each overflow as row (row.id)}
        <button class="overflow-row" onclick={() => pickChat(row.id)}>
          <span class="diamond" class:ingest={row.kind === "ingest"}>◈</span>
          <span class="tab-title">{row.title}</span>
          {#if statusDot(row)}
            <span class="dot" style:background={statusDot(row)}></span>
          {/if}
        </button>
      {/each}
    </div>
  {/if}

  <div class="body">
    {#if !chats.activeId}
      <div class="empty">
        <p>Chat with Ken about everything this project knows.</p>
        <button class="btn btn-primary" onclick={() => chats.newChat()}>Start a chat</button>
      </div>
    {:else if inTerminal}
      {#key chats.activeId}
        <TerminalView chatId={chats.activeId} />
      {/key}
    {:else}
      <ChatTranscript />
    {/if}
  </div>

  {#if chats.sendError}
    <div class="send-error">{chats.sendError}</div>
  {/if}

  {#if chats.activeId}
    <div class="foot">
      {#if inTerminal}
        <button class="btn btn-small" onclick={() => chats.exitTerminal()}>
          ← Back to conversation
        </button>
        <span class="foot-note">You're in the real Claude terminal.</span>
      {:else if chats.active?.kind === "ingest"}
        <span class="foot-note">Ingest session — opens in the terminal.</span>
      {:else}
        <div class="reply">
          <textarea
            bind:value={draft}
            onkeydown={onKeydown}
            placeholder="Reply to Ken…"
            rows="1"
          ></textarea>
          <span class="slash-hint mono">/ for terminal</span>
          <button class="send" onclick={submit} aria-label="Send">↑</button>
        </div>
        <div class="chat-actions">
          {#if chats.active}
            <button class="mini" onclick={() => chats.pin(chats.activeId!, !chats.active!.pinned)}>
              {chats.active.pinned ? "Unpin" : "Pin"}
            </button>
            <button class="mini" onclick={() => chats.archive(chats.activeId!)}>Archive</button>
          {/if}
        </div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .drawer {
    flex: 0 0 372px;
    border-left: 1px solid var(--border);
    background: var(--sunken-2);
    display: flex;
    flex-direction: column;
    min-height: 0;
    position: relative;
  }
  .drawer.overlay {
    position: absolute;
    top: 52px;
    right: 0;
    bottom: 0;
    width: 340px;
    z-index: 40;
    box-shadow: var(--shadow-drawer);
  }
  .tabs {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 10px 10px 0;
    overflow: hidden;
    flex: none;
  }
  .tab {
    flex: 1;
    min-width: 0;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 12px;
    padding: 6px 9px;
    border-radius: 8px 8px 0 0;
    border: 1px solid transparent;
    background: transparent;
    color: var(--ink-secondary);
    text-align: left;
  }
  .tab.active {
    font-weight: 600;
    background: var(--paper);
    border-color: var(--border);
    border-bottom-color: var(--paper);
    color: var(--accent-deep);
  }
  .diamond {
    color: var(--accent);
    font-size: 10px;
    flex: none;
  }
  .diamond.ingest {
    color: var(--ink-tertiary);
  }
  .tab-title {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .pin-mark {
    font-size: 10px;
    color: var(--accent);
    flex: none;
  }
  .dot {
    width: 6px;
    height: 6px;
    border-radius: 3px;
    flex: none;
  }
  .more {
    flex: none;
    font-size: 12px;
    padding: 6px;
    color: var(--ink-tertiary);
    border: none;
    background: none;
  }
  .new {
    flex: none;
    font-size: 13px;
    color: var(--accent);
    padding: 6px;
    font-weight: 600;
    border: none;
    background: none;
  }
  .overflow-menu {
    position: absolute;
    top: 42px;
    right: 10px;
    width: 260px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 10px;
    box-shadow: var(--shadow-overlay);
    z-index: 45;
    padding: 4px;
    display: flex;
    flex-direction: column;
  }
  .overflow-row {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 8px 10px;
    font-size: 12.5px;
    border: none;
    background: none;
    border-radius: 7px;
    text-align: left;
  }
  .overflow-row:hover {
    background: var(--sunken);
  }
  .body {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    background: var(--paper);
    border-top: 1px solid var(--border);
  }
  .empty {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 10px;
    padding: 20px;
  }
  .empty p {
    margin: 0;
    font-size: 13px;
    color: var(--ink-secondary);
    text-align: center;
    line-height: 1.6;
  }
  .send-error {
    flex: none;
    margin: 8px 12px 0;
    padding: 8px 12px;
    font-size: 12px;
    line-height: 1.5;
    color: var(--danger);
    background: rgba(163, 77, 63, 0.07);
    border: 1px solid rgba(163, 77, 63, 0.25);
    border-radius: 9px;
  }
  .foot {
    flex: none;
    padding: 12px 12px 12px;
    background: var(--paper);
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .reply {
    border: 1px solid var(--border-strong);
    border-radius: 10px;
    padding: 8px 10px;
    display: flex;
    align-items: flex-end;
    gap: 8px;
    background: var(--surface);
  }
  textarea {
    flex: 1;
    border: none;
    outline: none;
    background: transparent;
    resize: none;
    font-family: inherit;
    font-size: 13px;
    color: var(--ink);
    max-height: 120px;
    line-height: 1.5;
  }
  .slash-hint {
    font-size: 10.5px;
    color: var(--ink-tertiary);
    flex: none;
  }
  .send {
    width: 24px;
    height: 24px;
    border-radius: 7px;
    background: var(--accent);
    color: var(--surface);
    border: none;
    font-size: 12px;
    flex: none;
  }
  .send:hover {
    background: var(--accent-hover);
  }
  .chat-actions {
    display: flex;
    gap: 10px;
  }
  .mini {
    font-size: 11px;
    color: var(--ink-tertiary);
    border: none;
    background: none;
    padding: 0;
  }
  .mini:hover {
    color: var(--accent);
  }
  .foot-note {
    font-size: 11.5px;
    color: var(--ink-tertiary);
  }
</style>

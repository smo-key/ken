<script lang="ts">
  import { onMount } from "svelte";
  import { chats } from "../lib/chats.svelte";
  import ChatTranscript from "./ChatTranscript.svelte";
  import TerminalView from "./TerminalView.svelte";
  import { api, type ChatRow } from "../lib/api";
  import MessageSquare from "@lucide/svelte/icons/message-square";
  import Sparkles from "@lucide/svelte/icons/sparkles";
  import Bot from "@lucide/svelte/icons/bot";
  import ChevronDown from "@lucide/svelte/icons/chevron-down";
  import Plus from "@lucide/svelte/icons/plus";
  import ArrowUp from "@lucide/svelte/icons/arrow-up";
  import ArrowLeft from "@lucide/svelte/icons/arrow-left";
  import Pin from "@lucide/svelte/icons/pin";
  import PinOff from "@lucide/svelte/icons/pin-off";
  import Archive from "@lucide/svelte/icons/archive";
  import X from "@lucide/svelte/icons/x";
  import ContextMenu, { openContextMenu } from "../lib/ui/ContextMenu.svelte";

  function rowMenu(e: MouseEvent, row: ChatRow) {
    e.preventDefault();
    openContextMenu(e.clientX, e.clientY, [
      {
        label: row.pinned ? "Unpin" : "Pin",
        icon: row.pinned ? PinOff : Pin,
        onSelect: () => void chats.pin(row.id, !row.pinned),
      },
      "separator",
      {
        label: "Close",
        icon: Archive,
        onSelect: () => void chats.archive(row.id),
      },
    ]);
  }

  function kindIcon(kind: ChatRow["kind"]) {
    if (kind === "ingest") return Sparkles;
    if (kind === "research") return Bot;
    return MessageSquare;
  }

  async function closeChat(id: string, e: Event) {
    e.stopPropagation();
    await chats.archive(id);
  }

  function onCloseKey(id: string, e: KeyboardEvent) {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      void closeChat(id, e);
    }
  }

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
  const researchLive = $derived(
    chats.active?.kind === "research" &&
      (chats.active.status === "working" ||
        chats.active.status === "needs_input"),
  );

  async function cancelResearch() {
    if (chats.activeId) await api.cancelResearch(chats.activeId);
  }

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
      {@const Icon = kindIcon(row.kind)}
      <button
        class="tab"
        class:active={row.id === chats.activeId}
        onclick={() => pickChat(row.id)}
        oncontextmenu={(e) => rowMenu(e, row)}
        title={row.title}
      >
        <span class="diamond" class:ingest={row.kind === "ingest"}>
          <Icon size={13} strokeWidth={1.75} />
        </span>
        <span class="tab-title">{row.title}</span>
        {#if row.pinned}<span class="pin-mark" title="Pinned"><Pin size={11} strokeWidth={1.75} /></span>{/if}
        {#if statusDot(row)}
          <span class="dot" style:background={statusDot(row)}></span>
        {/if}
        <span
          class="close"
          role="button"
          tabindex="0"
          aria-label="Close chat"
          title="Close chat"
          onclick={(e) => closeChat(row.id, e)}
          onkeydown={(e) => onCloseKey(row.id, e)}
        >
          <X size={12} strokeWidth={2} />
        </span>
      </button>
    {/each}
    {#if overflow.length > 0}
      <button class="more" onclick={() => (overflowOpen = !overflowOpen)}>
        +{overflow.length}
        <ChevronDown size={13} strokeWidth={1.75} />
      </button>
    {/if}
    <button class="new" title="New chat" aria-label="New chat" onclick={() => chats.newChat()}>
      <Plus size={15} strokeWidth={1.75} />
    </button>
  </div>

  {#if overflowOpen}
    <div class="overflow-menu">
      {#each overflow as row (row.id)}
        {@const Icon = kindIcon(row.kind)}
        <button class="overflow-row" onclick={() => pickChat(row.id)} oncontextmenu={(e) => rowMenu(e, row)}>
          <span class="diamond" class:ingest={row.kind === "ingest"}>
            <Icon size={13} strokeWidth={1.75} />
          </span>
          <span class="tab-title">{row.title}</span>
          {#if statusDot(row)}
            <span class="dot" style:background={statusDot(row)}></span>
          {/if}
          <span
            class="close"
            role="button"
            tabindex="0"
            aria-label="Close chat"
            title="Close chat"
            onclick={(e) => closeChat(row.id, e)}
            onkeydown={(e) => onCloseKey(row.id, e)}
          >
            <X size={12} strokeWidth={2} />
          </span>
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
        <button class="btn btn-small back" onclick={() => chats.exitTerminal()}>
          <ArrowLeft size={14} strokeWidth={1.75} /> Back to conversation
        </button>
        <span class="foot-note">You're in the real Claude terminal.</span>
        {#if researchLive}
          <div class="chat-actions">
            <button class="mini" onclick={cancelResearch}>Cancel research</button>
          </div>
        {/if}
      {:else if chats.active?.kind === "ingest" || chats.active?.kind === "research"}
        <span class="foot-note">
          {chats.active.kind === "research"
            ? "Research session — opens in the terminal; you can answer its questions there."
            : "Ingest session — opens in the terminal."}
        </span>
        {#if researchLive}
          <div class="chat-actions">
            <button class="mini" onclick={cancelResearch}>Cancel research</button>
          </div>
        {/if}
      {:else}
        <div class="reply">
          <textarea
            bind:value={draft}
            onkeydown={onKeydown}
            placeholder="Reply to Ken…"
            rows="1"
          ></textarea>
          <span class="slash-hint mono">/ for terminal</span>
          <button class="send" onclick={submit} aria-label="Send">
            <ArrowUp size={14} strokeWidth={2} />
          </button>
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

<ContextMenu />

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
    display: inline-flex;
    align-items: center;
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
    display: inline-flex;
    align-items: center;
    color: var(--accent);
    flex: none;
  }
  .close {
    flex: none;
    margin-left: auto;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 17px;
    height: 17px;
    border-radius: 5px;
    color: var(--ink-tertiary);
    opacity: 0;
    transition: opacity 0.12s ease, background 0.12s ease, color 0.12s ease;
  }
  .tab:hover .close,
  .tab.active .close,
  .overflow-row:hover .close,
  .close:focus-visible {
    opacity: 1;
  }
  .close:hover {
    background: var(--sunken);
    color: var(--ink);
  }
  .dot {
    width: 6px;
    height: 6px;
    border-radius: 3px;
    flex: none;
  }
  .more {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 2px;
    font-size: 12px;
    padding: 6px;
    color: var(--ink-tertiary);
    border: none;
    background: none;
  }
  .new {
    flex: none;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    color: var(--accent);
    padding: 6px;
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
    background: color-mix(in srgb, var(--danger) 7%, transparent);
    border: 1px solid color-mix(in srgb, var(--danger) 25%, transparent);
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
    display: inline-flex;
    align-items: center;
    justify-content: center;
    flex: none;
  }
  .send:hover {
    background: var(--accent-hover);
  }
  .back {
    display: inline-flex;
    align-items: center;
    gap: 5px;
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

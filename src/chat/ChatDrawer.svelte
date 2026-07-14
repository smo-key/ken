<script lang="ts">
  import { onMount } from "svelte";
  import { chats } from "../lib/chats.svelte";
  import ChatTranscript from "./ChatTranscript.svelte";
  import TerminalView from "./TerminalView.svelte";
  import { api, CHAT_MODELS, type ChatRow } from "../lib/api";
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
  import Telescope from "@lucide/svelte/icons/telescope";
  import Terminal from "@lucide/svelte/icons/terminal";
  import X from "@lucide/svelte/icons/x";
  import ContextMenu, { openContextMenu } from "../lib/ui/ContextMenu.svelte";
  import ResearchModal from "../research/ResearchModal.svelte";
  import ChatResizer from "./ChatResizer.svelte";
  import { app } from "../lib/app.svelte";
  import { clampChatWidth } from "../lib/chatWidth";

  // Research runs *are* chats (kind: "research") — they open as a terminal
  // session in this drawer and are cancelled from its foot, so this is where
  // starting one belongs.
  let researchOpen = $state(false);

  // Pin and Archive live here (and in the row/tab context menu) rather than as a
  // standing button row under the composer — they're per-chat actions, so they
  // belong with the other per-item actions.
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
        label: "Archive",
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

  let winW = $state(window.innerWidth);
  let overflowOpen = $state(false);
  let draft = $state("");
  let replyEl = $state<HTMLTextAreaElement | null>(null);

  // Auto-grow the composer: start a few rows tall (CSS min-height) and expand
  // with the draft up to a cap, then scroll. Height is content-driven, so this
  // is a DOM measurement rather than a pure function worth unit-testing.
  const REPLY_MAX_H = 220;
  function growReply() {
    const el = replyEl;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, REPLY_MAX_H)}px`;
  }
  // Re-run on every draft change so both typing and programmatic clears resize.
  $effect(() => {
    draft;
    growReply();
  });

  // Shrinking the window narrows the drawer for as long as it has to, but the
  // stored preference is left alone so the width comes back when it grows.
  const width = $derived(clampChatWidth(app.chatWidth, winW));

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
    draft = "";
    await chats.send(text);
  }

  // Enter sends, Shift+Enter inserts a newline. "/" is an ordinary character now
  // — the terminal is revealed by the explicit Terminal toggle, not a keystroke.
  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      void submit();
    }
  }

  // Only plain chats swap between conversation and terminal; ingest/research
  // sessions are terminal-only and have no conversation to return to.
  const canToggleTerminal = $derived(
    chats.active !== null &&
      chats.active.kind !== "ingest" &&
      chats.active.kind !== "research",
  );

  async function toggleTerminal() {
    if (!chats.activeId || !canToggleTerminal) return;
    if (inTerminal) await chats.exitTerminal();
    else await chats.enterTerminal();
  }

  // Ctrl+` toggles the terminal (VS Code's convention). The visible Terminal
  // button is the primary affordance; this is the discoverable shortcut.
  function onWindowKeydown(e: KeyboardEvent) {
    if (e.ctrlKey && !e.metaKey && !e.altKey && e.key === "`") {
      if (!canToggleTerminal) return;
      e.preventDefault();
      void toggleTerminal();
    }
  }

  async function pickChat(id: string) {
    overflowOpen = false;
    await chats.select(id);
  }
</script>

<svelte:window
  onresize={() => (winW = window.innerWidth)}
  onkeydown={onWindowKeydown}
/>

<div class="drawer" class:overlay={narrow} style:width="{width}px">
  <ChatResizer {width} windowWidth={winW} />
  <div class="tabs">
    {#each visibleTabs as row (row.id)}
      {@const Icon = kindIcon(row.kind)}
      <!-- The tab label and its close control are siblings, not nested buttons:
           a button inside a button is invalid and breaks a11y semantics. -->
      <div class="tab-wrap" class:active={row.id === chats.activeId}>
        <button
          class="tab"
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
        </button>
        <button
          class="close"
          aria-label="Close chat"
          title="Close chat"
          onclick={(e) => closeChat(row.id, e)}
        >
          <X size={12} strokeWidth={2} />
        </button>
      </div>
    {/each}
    {#if overflow.length > 0}
      <button class="more" onclick={() => (overflowOpen = !overflowOpen)}>
        +{overflow.length}
        <ChevronDown size={13} strokeWidth={1.75} />
      </button>
    {/if}
    <button
      class="research"
      title="Start a deep research run"
      onclick={() => (researchOpen = true)}
    >
      <Telescope size={14} strokeWidth={1.75} />
      <span>Research</span>
    </button>
    <button class="new" title="New chat" aria-label="New chat" onclick={() => chats.newChat()}>
      <Plus size={15} strokeWidth={1.75} />
    </button>
  </div>

  {#if overflowOpen}
    <div class="overflow-menu">
      {#each overflow as row (row.id)}
        {@const Icon = kindIcon(row.kind)}
        <div class="overflow-row">
          <button
            class="overflow-main"
            onclick={() => pickChat(row.id)}
            oncontextmenu={(e) => rowMenu(e, row)}
          >
            <span class="diamond" class:ingest={row.kind === "ingest"}>
              <Icon size={13} strokeWidth={1.75} />
            </span>
            <span class="tab-title">{row.title}</span>
            {#if statusDot(row)}
              <span class="dot" style:background={statusDot(row)}></span>
            {/if}
          </button>
          <button
            class="close"
            aria-label="Close chat"
            title="Close chat"
            onclick={(e) => closeChat(row.id, e)}
          >
            <X size={12} strokeWidth={2} />
          </button>
        </div>
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
        <div class="terminal-bar">
          <button
            class="btn btn-small back"
            title="Hide terminal, back to conversation (Ctrl+`)"
            onclick={() => chats.exitTerminal()}
          >
            <ArrowLeft size={14} strokeWidth={1.75} /> Back to conversation
          </button>
          {#if researchLive}
            <button class="mini" onclick={cancelResearch}>Cancel research</button>
          {/if}
        </div>
        <span class="foot-note">You're in the real Claude terminal.</span>
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
            bind:this={replyEl}
            bind:value={draft}
            onkeydown={onKeydown}
            placeholder="Reply to Ken…  (Enter to send, Shift+Enter for a new line)"
            rows="3"
          ></textarea>
          <!-- Composer controls sit in their own row so a model-selector
               dropdown can slot in on the left of this bar later without a
               layout rework. -->
          <div class="composer-bar">
            <button
              class="composer-btn"
              title="Show terminal (Ctrl+`)"
              onclick={toggleTerminal}
            >
              <Terminal size={14} strokeWidth={1.75} />
              <span>Terminal</span>
            </button>
            <!-- Model selector: the four stable tiers plus the CLI default.
                 Values are tier aliases, so the list never needs version
                 upkeep. Applies to the next message/session. -->
            <select
              class="model-select"
              title="Model for this chat (applies to your next message)"
              value={chats.active?.model ?? ""}
              onchange={(e) =>
                chats.setModel(
                  (e.currentTarget as HTMLSelectElement).value || null,
                )}
            >
              {#each CHAT_MODELS as m (m.label)}
                <option value={m.value ?? ""}>{m.label}</option>
              {/each}
            </select>
            <span class="spacer"></span>
            <button class="send" onclick={submit} aria-label="Send message">
              <ArrowUp size={14} strokeWidth={2} />
            </button>
          </div>
        </div>
      {/if}
    </div>
  {/if}
</div>

{#if researchOpen}
  <ResearchModal close={() => (researchOpen = false)} />
{/if}

<ContextMenu />

<style>
  /* Width comes in inline (persisted + clamped to the window); flex-basis auto
     defers to it so the drawer holds that fixed width beside the main content. */
  .drawer {
    flex: 0 0 auto;
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
  .tab-wrap {
    flex: 1;
    min-width: 0;
    display: inline-flex;
    align-items: center;
    border-radius: 8px 8px 0 0;
    border: 1px solid transparent;
  }
  .tab-wrap.active {
    background: var(--paper);
    border-color: var(--border);
    /* Bottom edge matches the panel so the active tab merges into the body. */
    border-bottom-color: var(--paper);
  }
  .tab {
    flex: 1;
    min-width: 0;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 12px;
    padding: 6px 4px 6px 9px;
    border: none;
    background: transparent;
    color: var(--ink-secondary);
    text-align: left;
  }
  .tab-wrap.active .tab {
    font-weight: 600;
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
    margin-right: 5px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 17px;
    height: 17px;
    border: none;
    background: transparent;
    border-radius: 5px;
    color: var(--ink-tertiary);
    opacity: 0;
    transition: opacity 0.12s ease, background 0.12s ease, color 0.12s ease;
  }
  .tab-wrap:hover .close,
  .tab-wrap.active .close,
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
  /* A labelled pill so "Research" reads as a distinct action, not another
     icon-only tab affordance. */
  .research {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-size: 11.5px;
    font-weight: 500;
    padding: 5px 9px;
    border-radius: 7px;
    border: 1px solid var(--border);
    background: var(--surface);
    color: var(--accent-deep);
  }
  .research:hover {
    background: var(--sunken);
    border-color: var(--border-strong);
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
    border-radius: 7px;
  }
  .overflow-row:hover {
    background: var(--sunken);
  }
  .overflow-main {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 8px 4px 8px 10px;
    font-size: 12.5px;
    border: none;
    background: none;
    border-radius: 7px;
    text-align: left;
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
    flex-direction: column;
    gap: 8px;
    background: var(--surface);
  }
  .reply:focus-within {
    border-color: var(--accent);
  }
  textarea {
    width: 100%;
    border: none;
    outline: none;
    background: transparent;
    resize: none;
    font-family: inherit;
    font-size: 13px;
    color: var(--ink);
    /* Starts a few rows tall and auto-grows (JS) up to REPLY_MAX_H, then scrolls. */
    min-height: 60px;
    max-height: 220px;
    line-height: 1.5;
  }
  .composer-bar {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .spacer {
    flex: 1;
  }
  .composer-btn {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-size: 11.5px;
    color: var(--ink-tertiary);
    border: 1px solid var(--border);
    background: transparent;
    padding: 4px 8px;
    border-radius: 7px;
  }
  .composer-btn:hover {
    color: var(--ink);
    background: var(--sunken);
    border-color: var(--border-strong);
  }
  .model-select {
    font-size: 11.5px;
    color: var(--ink-secondary);
    border: 1px solid var(--border);
    background: transparent;
    padding: 4px 6px;
    border-radius: 7px;
    max-width: 110px;
  }
  .model-select:hover {
    border-color: var(--border-strong);
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
  .terminal-bar {
    display: flex;
    align-items: center;
    gap: 10px;
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

<script lang="ts">
  import { chats, SUGGESTED_PROMPTS } from "../lib/chats.svelte";
  import { app } from "../lib/app.svelte";
  import { isProjectLink, renderMarkdown } from "../lib/markdown";

  let scroller = $state<HTMLDivElement | null>(null);

  // Follow the conversation, but only when the user is already near the bottom —
  // don't yank them down while they're reading earlier messages.
  $effect(() => {
    const len = chats.transcript.length;
    void len;
    const el = scroller;
    if (!el) return;
    const nearBottom =
      el.scrollHeight - el.scrollTop - el.clientHeight < 120;
    if (nearBottom) el.scrollTop = el.scrollHeight;
  });

  function onClick(e: MouseEvent) {
    // Project-relative links open in Files.
    const a = (e.target as HTMLElement).closest("a");
    if (a) {
      e.preventDefault();
      const href = a.getAttribute("href") ?? "";
      if (isProjectLink(href)) app.openInFiles(href);
    }
  }

  const working = $derived(chats.active?.status === "working");
</script>

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
      <div class="bubble user" class:pending={"pending" in msg && msg.pending}>{msg.content}</div>
    {:else if msg.role === "assistant"}
      <div class="assistant">
        <span class="mark">K</span>
        <div class="md">{@html renderMarkdown(msg.content)}</div>
      </div>
    {:else if msg.role === "activity"}
      <div class="activity mono">{msg.content}</div>
    {:else}
      <div class="divider"><span>{msg.content}</span></div>
    {/if}
  {/each}

  {#if working}
    <div class="working">
      <span class="pulse"></span>Ken is working…
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

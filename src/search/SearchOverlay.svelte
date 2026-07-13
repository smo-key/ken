<script lang="ts">
  import { onMount } from "svelte";
  import type { UnlistenFn } from "@tauri-apps/api/event";
  import { api, type QuickAnswer, type SearchHit } from "../lib/api";
  import { app } from "../lib/app.svelte";
  import { chats } from "../lib/chats.svelte";
  import { isQuestionQuery } from "../lib/assist";
  import { renderMarkdown } from "../lib/markdown";
  import FileGlyph from "../files/FileGlyph.svelte";
  import Search from "@lucide/svelte/icons/search";

  let query = $state("");
  let hits = $state<SearchHit[]>([]);
  let selected = $state(0);
  let searched = $state(false);
  let input: HTMLInputElement;
  let timer: ReturnType<typeof setTimeout> | undefined;

  // Quick answer: additive, never blocking the instant matches. Answers
  // are cached per query for this overlay's lifetime; stale ones are
  // dropped by comparing the event's query to the live input.
  let answer = $state<QuickAnswer | null>(null);
  let aiAvailable = true;
  const answerCache = new Map<string, QuickAnswer>();
  let aiTimer: ReturnType<typeof setTimeout> | undefined;

  onMount(() => {
    input.focus();
    let unlisten: UnlistenFn | undefined;
    void api
      .onQuickAnswer((qa) => {
        answerCache.set(qa.query, qa);
        if (qa.query === query.trim()) answer = qa;
      })
      .then((un) => (unlisten = un));
    return () => {
      unlisten?.();
      if (aiTimer) clearTimeout(aiTimer);
      if (timer) clearTimeout(timer);
    };
  });

  function onInput() {
    if (timer) clearTimeout(timer);
    timer = setTimeout(run, 120);

    // A new keystroke makes any visible answer stale.
    if (aiTimer) clearTimeout(aiTimer);
    const q = query.trim();
    const cached = answerCache.get(q);
    answer = cached ?? null;
    if (!cached && aiAvailable && isQuestionQuery(q)) {
      aiTimer = setTimeout(() => void ask(q), 800);
    }
  }

  async function ask(q: string) {
    if (q !== query.trim()) return;
    const available = await api.quickAnswer(q).catch(() => false);
    if (!available) aiAvailable = false;
  }

  async function run() {
    const q = query.trim();
    if (!q) {
      hits = [];
      searched = false;
      return;
    }
    hits = await api.search(q, 30);
    searched = true;
    selected = 0;
  }

  function openHit(hit: SearchHit | undefined) {
    if (hit) app.openInFiles(hit.relPath);
  }

  /** ⌘↵ — hand the query to a fresh chat in the drawer. */
  async function continueInChat() {
    const q = query.trim();
    if (!q) return;
    app.searchOpen = false;
    await chats.newChat();
    await chats.send(q);
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      selected = Math.min(selected + 1, hits.length - 1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      selected = Math.max(selected - 1, 0);
    } else if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      void continueInChat();
    } else if (e.key === "Enter") {
      e.preventDefault();
      openHit(hits[selected]);
    }
  }

  // Snippets carry only our <mark> tags; escape everything else.
  function renderSnippet(snippet: string): string {
    const escaped = snippet
      .replaceAll("&", "&amp;")
      .replaceAll("<", "&lt;")
      .replaceAll(">", "&gt;");
    return escaped
      .replaceAll("&lt;mark&gt;", "<mark>")
      .replaceAll("&lt;/mark&gt;", "</mark>");
  }
</script>

<button class="scrim" onclick={() => (app.searchOpen = false)} aria-label="Close search"></button>
<div class="overlay" role="dialog" aria-label="Search project knowledge">
  <div class="query-row">
    <Search class="lens" size={16} strokeWidth={1.75} aria-hidden="true" />
    <input
      bind:this={input}
      bind:value={query}
      oninput={onInput}
      onkeydown={onKeydown}
      placeholder="Search project knowledge…"
      spellcheck="false"
    />
  </div>

  {#if answer}
    <div class="qa">
      <div class="qa-head">Quick answer</div>
      <div class="qa-body">{@html renderMarkdown(answer.body)}</div>
      <div class="qa-foot">
        {#each answer.sources as source (source)}
          <button
            class="qa-chip mono"
            title={source}
            onclick={() => app.openInFiles(source)}
          >
            {source.split("/").pop() || source}
          </button>
        {/each}
        <button class="qa-dig" onclick={continueInChat}>
          ⌘↵ dig deeper in chat
        </button>
      </div>
    </div>
  {/if}

  {#if hits.length > 0}
    <div class="section">Matches</div>
    <div class="results">
      {#each hits as hit, i (hit.relPath)}
        <button
          class="hit"
          class:selected={i === selected}
          onclick={() => openHit(hit)}
          onmouseenter={() => (selected = i)}
        >
          <FileGlyph kind={hit.kind} />
          <span class="hit-body">
            <span class="snippet">{@html renderSnippet(hit.snippet)}</span>
            <span class="path mono">{hit.relPath}</span>
          </span>
          {#if i === selected}
            <span class="enter mono">↵</span>
          {/if}
        </button>
      {/each}
    </div>
  {:else if searched}
    <div class="empty">
      Nothing in this project matches “{query}”. Ken searches file contents and
      names — try fewer or different words.
    </div>
  {/if}

  <div class="foot">
    <span><span class="kbd">↵</span> open</span>
    <span><span class="kbd">esc</span> close</span>
    <span class="continue"><span class="kbd">⌘↵</span> continue in chat</span>
  </div>
</div>

<style>
  .scrim {
    position: absolute;
    inset: 0;
    background: rgba(33, 30, 25, 0.18);
    z-index: 50;
    border: none;
  }
  .overlay {
    position: absolute;
    top: 96px;
    left: 50%;
    transform: translateX(-50%);
    width: min(640px, calc(100vw - 80px));
    max-height: calc(100vh - 180px);
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-overlay);
    box-shadow: var(--shadow-overlay);
    overflow: hidden;
    display: flex;
    flex-direction: column;
    z-index: 51;
  }
  .query-row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 16px 18px;
    border-bottom: 1px solid var(--sunken);
    flex: none;
  }
  .query-row :global(.lens) {
    color: var(--ink-secondary);
    flex: none;
  }
  input {
    flex: 1;
    border: none;
    outline: none;
    background: transparent;
    font-size: 15px;
    color: var(--ink);
    font-family: inherit;
  }
  input::placeholder {
    color: var(--ink-tertiary);
  }
  .section {
    padding: 9px 12px 4px;
    font-size: 11px;
    font-weight: 700;
    color: var(--ink-tertiary);
    letter-spacing: 0.07em;
    text-transform: uppercase;
    flex: none;
  }
  .results {
    overflow-y: auto;
    padding: 0 10px 8px;
  }
  .hit {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 9px 12px;
    border-radius: 9px;
    width: 100%;
    border: none;
    background: transparent;
    text-align: left;
  }
  .hit.selected {
    background: rgba(138, 90, 68, 0.07);
  }
  .hit-body {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .snippet {
    font-size: 13px;
    color: var(--ink);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .snippet :global(mark) {
    background: var(--match-highlight);
    border-radius: 3px;
    padding: 0 2px;
    color: inherit;
  }
  .path {
    font-size: 11.5px;
    color: var(--ink-tertiary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .enter {
    font-size: 11px;
    color: var(--ink-tertiary);
    flex: none;
  }
  .empty {
    padding: 24px 20px;
    font-size: 13px;
    color: var(--ink-secondary);
    line-height: 1.6;
  }
  .foot {
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 12px 18px;
    border-top: 1px solid var(--sunken);
    background: var(--paper);
    font-size: 12px;
    color: var(--ink-secondary);
    flex: none;
  }
  .continue {
    margin-left: auto;
    color: var(--accent);
    font-weight: 600;
  }
  .qa {
    margin: 10px 10px 4px;
    border: 1px solid rgba(138, 90, 68, 0.25);
    background: rgba(138, 90, 68, 0.05);
    border-radius: 10px;
    padding: 13px 15px;
    display: flex;
    flex-direction: column;
    gap: 9px;
    flex: none;
  }
  .qa-head {
    font-size: 11px;
    font-weight: 700;
    color: var(--accent);
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }
  .qa-body {
    font-size: 13.5px;
    line-height: 1.6;
  }
  .qa-body :global(p) {
    margin: 0;
  }
  .qa-foot {
    display: flex;
    gap: 6px;
    align-items: center;
    flex-wrap: wrap;
  }
  .qa-chip {
    font-size: 11px;
    color: var(--ink-secondary);
    border: 1px solid var(--border);
    border-radius: 5px;
    padding: 2px 7px;
    background: var(--surface);
    cursor: pointer;
  }
  .qa-chip:hover {
    border-color: var(--accent);
  }
  .qa-dig {
    margin-left: auto;
    border: none;
    background: transparent;
    padding: 0;
    font-family: inherit;
    font-size: 11.5px;
    color: var(--accent);
    font-weight: 600;
    cursor: pointer;
  }
  .qa-dig:hover {
    text-decoration: underline;
  }
</style>

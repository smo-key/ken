<script lang="ts">
  import { onMount } from "svelte";
  import type { UnlistenFn } from "@tauri-apps/api/event";
  import { api, type QuickAnswer, type SearchHit } from "../lib/api";
  import { app } from "../lib/app.svelte";
  import { chats } from "../lib/chats.svelte";
  import { isQuestionQuery, stripStreamingBody } from "../lib/assist";
  import { renderMarkdown, renderSearchSnippet } from "../lib/markdown";
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
  // Live streaming buffer for the query currently being answered.
  let streaming = $state<{ query: string; text: string } | null>(null);
  // The query for which the on-device model is working but has not yet
  // produced any output — drives the "Thinking…" indicator. Tied to the
  // query string so a stale one never lingers across query changes.
  let thinking = $state<string | null>(null);
  let modelInstalled = $state(true);

  onMount(() => {
    input.focus();
    void api.llmStatus().then((s) => (modelInstalled = s !== "notInstalled"));
    let unlistenFinal: UnlistenFn | undefined;
    let unlistenDelta: UnlistenFn | undefined;
    void api
      .onQuickAnswer((qa) => {
        answerCache.set(qa.query, qa);
        if (thinking === qa.query) thinking = null;
        if (qa.query === query.trim()) {
          answer = qa;
          streaming = null; // final replaces the live buffer
        }
      })
      .then((un) => (unlistenFinal = un));
    void api
      .onQuickAnswerDelta((ev) => {
        if (ev.query !== query.trim()) return; // stale
        // First output for this query — the model is no longer "thinking".
        if (thinking === ev.query) thinking = null;
        if (!streaming || streaming.query !== ev.query) {
          streaming = { query: ev.query, text: ev.delta };
        } else {
          streaming = { query: ev.query, text: streaming.text + ev.delta };
        }
      })
      .then((un) => (unlistenDelta = un));
    return () => {
      unlistenFinal?.();
      unlistenDelta?.();
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
    // Clear the live buffer when the query changes so an old stream doesn't
    // bleed into a new query.
    if (!streaming || streaming.query !== q) streaming = null;
    // The query changed, so any pending "Thinking…" belongs to an old query.
    // A fresh dispatch (below) re-arms it after the debounce.
    if (thinking !== q) thinking = null;
    if (!cached && aiAvailable && isQuestionQuery(q)) {
      aiTimer = setTimeout(() => void ask(q), 800);
    }
  }

  async function ask(q: string) {
    if (q !== query.trim()) return;
    // The debounce elapsed and we're dispatching for the live query: the
    // on-device model is now working with nothing to show yet.
    thinking = q;
    const available = await api.quickAnswer(q).catch(() => false);
    if (!available) {
      aiAvailable = false;
      if (thinking === q) thinking = null;
    }
  }

  // Below this length a query is too trivial to be worth a backend round-trip
  // (and would return firehose results); guard the dispatch entirely.
  const MIN_SEARCH_LEN = 2;

  async function run() {
    const q = query.trim();
    if (q.length < MIN_SEARCH_LEN) {
      hits = [];
      searched = false;
      return;
    }
    const found = await api.search(q, 30);
    // A slower earlier request must not overwrite a newer query's results.
    if (q !== query.trim()) return;
    hits = found;
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
  {:else if streaming && streaming.query === query.trim() && stripStreamingBody(streaming.text)}
    <div class="qa">
      <div class="qa-head">Quick answer</div>
      <div class="qa-body">{@html renderMarkdown(stripStreamingBody(streaming.text))}</div>
    </div>
  {:else if thinking === query.trim()}
    <div class="qa">
      <div class="qa-head">Quick answer</div>
      <div class="qa-body qa-thinking">Thinking…</div>
    </div>
  {:else if !modelInstalled && isQuestionQuery(query.trim())}
    <div class="qa qa-hint">
      <div class="qa-body">
        Instant answers run on your Mac.
        <button class="qa-dig" onclick={() => app.openSettings()}>
          Download the answers model in Settings
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
            <span class="snippet">{@html renderSearchSnippet(hit.snippet)}</span>
            <!-- Show only the basename; full path stays in the tooltip so same-named files in different folders remain distinguishable. -->
            <span class="path mono" title={hit.relPath}>{hit.relPath.split("/").pop() || hit.relPath}</span>
          </span>
          {#if i === selected}
            <span class="enter mono">↵</span>
          {/if}
        </button>
      {/each}
    </div>
  {:else if searched}
    <div class="empty">
      {#if app.scanning}
        Nothing matches “{query}” <em>yet</em> — Ken is still reading your folder.
        Search lights up as files are indexed.
      {:else}
        Nothing in this project matches “{query}”. Ken searches file contents and
        names — try fewer or different words.
      {/if}
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
    background: var(--scrim);
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
    background: color-mix(in srgb, var(--accent) 7%, transparent);
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
    border: 1px solid color-mix(in srgb, var(--accent) 25%, transparent);
    background: color-mix(in srgb, var(--accent) 5%, transparent);
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
  .qa-thinking {
    color: var(--ink-secondary);
    animation: qa-pulse 1.2s ease-in-out infinite;
  }
  @keyframes qa-pulse {
    0%,
    100% {
      opacity: 0.5;
    }
    50% {
      opacity: 1;
    }
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

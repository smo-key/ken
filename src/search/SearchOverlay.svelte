<script lang="ts">
  import { onMount } from "svelte";
  import { api, type SearchHit } from "../lib/api";
  import { app } from "../lib/app.svelte";
  import FileGlyph from "../files/FileGlyph.svelte";

  let query = $state("");
  let hits = $state<SearchHit[]>([]);
  let selected = $state(0);
  let searched = $state(false);
  let input: HTMLInputElement;
  let timer: ReturnType<typeof setTimeout> | undefined;

  onMount(() => input.focus());

  function onInput() {
    if (timer) clearTimeout(timer);
    timer = setTimeout(run, 120);
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

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      selected = Math.min(selected + 1, hits.length - 1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      selected = Math.max(selected - 1, 0);
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
    <span class="lens" aria-hidden="true"></span>
    <input
      bind:this={input}
      bind:value={query}
      oninput={onInput}
      onkeydown={onKeydown}
      placeholder="Search project knowledge…"
      spellcheck="false"
    />
  </div>

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
    <span class="soon">AI answers arrive in an upcoming release</span>
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
  .lens {
    width: 14px;
    height: 14px;
    border: 1.5px solid var(--ink-secondary);
    border-radius: 50%;
    position: relative;
    flex: none;
  }
  .lens::after {
    content: "";
    position: absolute;
    width: 1.5px;
    height: 6px;
    background: var(--ink-secondary);
    transform: rotate(-45deg);
    top: 11px;
    left: 13px;
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
  .soon {
    margin-left: auto;
    color: var(--ink-tertiary);
    font-size: 11.5px;
  }
</style>

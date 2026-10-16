<script lang="ts">
  import { onMount } from "svelte";
  import type { UnlistenFn } from "@tauri-apps/api/event";
  import {
    api,
    type ContentType,
    type KindFilter,
    type QuickAnswer,
    type RoutePlan,
    type RouteSearchResult,
    type RoutedSearchStateEvent,
  } from "../lib/api";
  import { app, forFocused } from "../lib/app.svelte";
  import { scope as sharedScope } from "../lib/scope.svelte";
  const workspaceName = $derived(app.workspace?.name ?? "the workspace");
  import { chats } from "../lib/chats.svelte";
  import { isQuestionQuery, stripStreamingBody } from "../lib/assist";
  import { plainSnippet, renderMarkdown, renderSearchSnippet } from "../lib/markdown";
  import { kindForPath } from "../lib/format";
  import { inMember, parseCitation } from "../lib/citation";
  import FileGlyph from "../files/FileGlyph.svelte";
  import Search from "@lucide/svelte/icons/search";
  import { shortcut } from "../lib/platform";
  import { errorText, toast } from "../lib/toast.svelte";
  import { openKenAddress } from "../lib/kenAddress";
  import EmptyState from "../lib/ui/EmptyState.svelte";

  let query = $state("");
  let selected = $state(0);
  let searched = $state(false);
  let input: HTMLInputElement;
  let timer: ReturnType<typeof setTimeout> | undefined;

  // Two scopes, one search (knowledge-core decisions 15 and 20): "This repo"
  // is a routed search pinned to the focused member, and the workspace scope is a
  // routed search over every source. The toggle is visible only in Workspace
  // mode (`app.workspace` set).
  type Scope = "project" | "all";
  // ONE scope, shared with Home's picker (`scope.svelte.ts`). This used to
  // be local state, which meant the overlay and Home could disagree — set
  // "All projects" on Home and the overlay still showed "This project",
  // because nothing connected them.
  //
  // The legacy single-project path is used only when the scope is the
  // FOCUSED project; a pin on any other project (or a group) goes through
  // routing, which can reach it whether or not it is resident.
  const scope = $derived<Scope>(
    sharedScope.enabled &&
      !(sharedScope.kind === "project" && sharedScope.value === app.focused)
      ? "all"
      : "project",
  );
  /** Narrow the all-projects scope to one member (ken-home-workspace
   *  3.3). `null` = every member. Routed search pins the plan to this id;
   *  same result shape either way. */
  let pinnedMember = $state<string | null>(null);

  /** One results row from `route_search`, whichever scope asked. */
  interface DisplayHit {
    key: string;
    path: string;
    snippet: string;
    /** `"keyword"` | `"semantic"` | `"both"`. */
    source: string;
    projectId: string;
    /** Shown in the workspace scope only: in "This repo" every hit is its own. */
    memberName: string | null;
    /** What the file is for: code, spec, doc, … */
    contentType: ContentType;
    /** One label per hit: `kind · tier or role · date`. */
    label: string | null;
    /** For a code hit, the definition it holds. */
    symbol: string | null;
  }

  let results = $state<DisplayHit[]>([]);
  /** Why the last search failed, shown in place of its results. */
  let searchError = $state<string | null>(null);
  // "Ask Ken to look": a read-only Claude pass over the workspace when the
  // index found nothing that answers, so Ken never says "not there" on the
  // index's word alone.
  let look = $state<{ query: string; state: "looking" | "done" | "error"; body: string; sources: string[] } | null>(null);
  async function askKenToLook() {
    const q = query.trim();
    if (!q) return;
    look = { query: q, state: "looking", body: "", sources: [] };
    try {
      const leads = results.slice(0, 8).map((h) => (h.memberName ? `${h.memberName}/${h.path}` : h.path));
      const found = await api.lookFor(q, leads);
      if (look?.query === q) look = { query: q, state: "done", body: found.body, sources: found.sources };
    } catch (e) {
      if (look?.query === q) look = { query: q, state: "error", body: String(e), sources: [] };
    }
  }
  async function openCited(source: string) {
    const parsed = parseCitation(source);
    if (!parsed) return app.openInFiles(source);
    const c = inMember(parsed, app.members, app.focused);
    if (c.projectId && c.projectId !== app.focused && c.projectId !== "workspace") {
      await app.focusMember(c.projectId);
    }
    app.openAt(c.path, { line: c.line, anchor: c.anchor });
  }
  /** What a short result list may be missing: a source still building its
   *  meaning index, one that could not be searched, a slow one, and the
   *  reference repos held back. A building or slow source is searched and
   *  reported, never dropped (decision 20). */
  let coverageNotes = $state<string[]>([]);
  let routedPlan = $state<RoutePlan | null>(null);
  let routedProgress = $state<RoutedSearchStateEvent | null>(null);

  const planLine = $derived.by(() => {
    if (!routedPlan) return null;
    const n = routedPlan.targets.length;
    const projects = `${n} source${n === 1 ? "" : "s"}`;
    if (routedPlan.reason === "named") return `Narrowed to ${projects}`;
    return `Searched ${projects}`;
  });

  function coverage(res: RouteSearchResult): string[] {
    const notes: string[] = [];
    for (const s of res.memberStatus) {
      if (s.status === "unavailable") {
        notes.push(`${s.memberName}: not searched, its index did not open`);
      } else if (s.status === "building") {
        const pct = s.chunks ? Math.floor(((s.embedded ?? 0) * 100) / s.chunks) : 0;
        notes.push(`${s.memberName}: keyword only, meaning search ${pct}% built`);
      }
      if (s.slow) notes.push(`${s.memberName}: slow, ${(s.millis / 1000).toFixed(1)} s`);
    }
    const held = res.plan.reserve.filter((id) => !res.memberStatus.some((s) => s.projectId === id));
    if (held.length) {
      notes.push(`${held.length} reference source${held.length === 1 ? "" : "s"} held back`);
    }
    return notes;
  }

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
    // Warm the on-device model as the overlay opens so the (slow) first-time
    // load is paid before the user's first question, not during it. No-op when
    // no local model is installed.
    void api.warmLlm().catch(() => {});
    let unlistenFinal: UnlistenFn | undefined;
    let unlistenDelta: UnlistenFn | undefined;
    let unlistenRouted: UnlistenFn | undefined;
    void api
      .onQuickAnswer((qa) => {
        if (!forFocused(qa.project_id)) return;
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
        if (!forFocused(ev.project_id)) return;
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
    void api
      .onRoutedSearchState((ev) => {
        routedProgress = ev;
      })
      .then((un) => (unlistenRouted = un));
    return () => {
      unlistenFinal?.();
      unlistenDelta?.();
      unlistenRouted?.();
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
      aiTimer = setTimeout(() => void ask(q), 250);
    }
  }

  /** A quick answer's source: a `ken://` address opens in the repo it came
   *  from (the answer reads every source), a bare path in the focused one. */
  async function openSource(source: string) {
    if (!source.startsWith("ken://")) {
      app.openInFiles(source);
      return;
    }
    const opened = await openKenAddress(source);
    if (!opened.ok) toast.error(opened.reason);
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
      results = [];
      coverageNotes = [];
      routedPlan = null;
      searched = false;
      searchError = null;
      return;
    }
    try {
      await search(q);
    } catch (e) {
      if (q !== query.trim()) return; // a newer query took over
      searchError = errorText(e);
      results = [];
      coverageNotes = [];
      routedPlan = null;
      searched = true;
    }
  }

  async function search(q: string) {
    // The workspace scope is every source unless narrowed: the shared scope store
    // wins when it is (a group, or a project chosen on Home), and
    // `pinnedMember` is the overlay's own narrowing for one query. "This
    // repo" is the focused member alone.
    const all = scope === "all" && !!app.workspace;
    routedProgress = null;
    const res = all
      ? await api.routeSearch(q, 30, pinnedMember ?? sharedScope.projectId, sharedScope.groupName, kinds)
      : await api.routeSearch(q, 30, app.focused, null, kinds);
    // A slower earlier request must not overwrite a newer query's results.
    if (q !== query.trim()) return;
    searchError = null;
    routedPlan = res.plan;
    coverageNotes = coverage(res);
    results = res.results.map((h) => ({
      key: `${h.projectId}:${h.chunkId}`,
      path: h.path,
      snippet: h.snippet,
      source: h.source,
      projectId: h.projectId,
      memberName: all ? h.memberName : null,
      contentType: h.contentType,
      label: h.label,
      symbol: h.symbol,
    }));
    searched = true;
    selected = 0;
  }

  // What kind of file to keep: all, or one kind. Every hit shows its kind.
  let kinds = $state<KindFilter>(null);
  const KINDS: { v: KindFilter; label: string }[] = [
    { v: null, label: "Any kind" },
    { v: "code", label: "Code" },
    { v: "test", label: "Tests" },
    { v: "spec", label: "Specs" },
    { v: "doc", label: "Docs" },
    { v: "meeting", label: "Meetings" },
    { v: "ticket", label: "Tickets" },
    { v: "decision", label: "Decisions" },
    { v: "note", label: "Notes" },
    { v: "config", label: "Config" },
    { v: "data", label: "Data" },
    { v: "design", label: "Design" },
  ];
  function setScope(next: Scope) {
    if (scope === next) return;
    // Writes through to the shared store, so Home reflects it too.
    if (next === "project") {
      pinnedMember = null;
      sharedScope.set("project", app.focused ?? null);
    } else {
      // The broad scope is the whole workspace.
      sharedScope.setWorkspaceScope();
    }
    void run();
  }

  function setPinnedMember(id: string | null) {
    if (pinnedMember === id) return;
    pinnedMember = id;
    void run();
  }

  async function openHit(hit: DisplayHit | undefined) {
    if (!hit) return;
    // A hit from an unfocused member: switch focus first (workspace task
    // 4.4 / kg-routing spec: "selection resolves ... switching member focus
    // as needed"), then open.
    if (hit.projectId && hit.projectId !== app.focused) {
      await app.focusMember(hit.projectId);
    }
    app.openInFiles(hit.path);
  }

  /** Mod+Enter — hand the query to a fresh chat in the drawer. */
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
      selected = Math.min(selected + 1, results.length - 1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      selected = Math.max(selected - 1, 0);
    } else if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      void continueInChat();
    } else if (e.key === "Enter") {
      e.preventDefault();
      void openHit(results[selected]);
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
      placeholder={scope === "all" ? `Search ${workspaceName}…` : "Search this source…"}
      spellcheck="false"
    />
  </div>

  {#if app.workspace}
    <div class="scope-row">
      <button
        class="scope-btn"
        class:active={scope === "project"}
        onclick={() => setScope("project")}
      >
        This source
      </button>
      <button class="scope-btn" class:active={scope === "all"} onclick={() => setScope("all")}>
        {workspaceName}
      </button>
      {#if scope === "all"}
        <!-- Narrowing within all-projects: pins routing to one member,
             including one that is dormant. -->
        <select
          class="member-pin"
          value={pinnedMember ?? ""}
          onchange={(e) => setPinnedMember(e.currentTarget.value || null)}
          aria-label="Source"
          title="Narrow to one source"
        >
          <option value="">Everywhere</option>
          <!-- Dormant members are pinnable: routing opens their index by
               id without activating them. Only unresolvable ones are out. -->
          {#each app.workspace.members.filter((m) => m.projectId && (m.status === "active" || m.status === "dormant")) as m (m.projectId)}
            <option value={m.projectId}>{m.name}</option>
          {/each}
        </select>
      {/if}
    </div>
  {/if}
  <div class="scope-row" role="group" aria-label="What kind of file">
    <select
      class="kind-select"
      aria-label="What kind of file"
      value={kinds ?? ""}
      onchange={(e) => {
        kinds = (e.currentTarget as HTMLSelectElement).value || null;
        void run();
      }}
    >
      {#each KINDS as k (k.label)}
        <option value={k.v ?? ""}>{k.label}</option>
      {/each}
    </select>
  </div>

  {#if answer}
    <div class="qa">
      <div class="qa-head">Quick answer</div>
      <div class="qa-body">{@html renderMarkdown(answer.body)}</div>
      <div class="qa-foot">
        {#each answer.sources as source}
          <button
            class="qa-chip mono"
            title={source}
            onclick={() => void openSource(source)}
          >
            {source.split("/").pop() || source}
          </button>
        {/each}
        <button class="qa-dig" onclick={continueInChat}>
          {shortcut("mod+enter")} dig deeper in chat
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
      <div class="qa-body">Claude Code not found</div>
    </div>
  {/if}

  {#if scope === "all" && searched && routedProgress && routedProgress.state === "searching"}
    <div class="scope-info">
      <span class="progress">searching {routedProgress.done}/{routedProgress.total}…</span>
    </div>
  {/if}
  {#if searched}
    {#each coverageNotes as note}
      <div class="coverage-note">{note}</div>
    {/each}
  {/if}

  {#if results.length > 0}
    <!-- How many sources were searched is the count's tooltip. -->
    <div class="section" title={scope === "all" ? (planLine ?? undefined) : undefined}>Matches · {results.length}</div>
    <div class="results">
      {#each results as hit, i (hit.key)}
        <button
          class="hit"
          class:selected={i === selected}
          onclick={() => openHit(hit)}
          onmouseenter={() => (selected = i)}
        >
          <FileGlyph kind={kindForPath(hit.path)} />
          <span class="hit-body">
            <span class="snippet">{@html renderSearchSnippet(kindForPath(hit.path) === "code" ? hit.snippet : plainSnippet(hit.snippet))}</span>
            <span class="meta">
              <!-- Show only the basename; full path stays in the tooltip so same-named files in different folders remain distinguishable. -->
              <span class="path mono" title={hit.path}>{hit.path.split("/").pop() || hit.path}</span>
              {#if hit.symbol}
                <span class="symbol mono">{hit.symbol}</span>
              {/if}
              {#if hit.label || hit.contentType}
                <span class="tag tag-kind">{hit.label ?? hit.contentType}</span>
              {/if}
              {#if hit.memberName}
                <span class="tag tag-member" title={hit.path}>{hit.memberName}</span>
              {/if}
              {#if hit.source === "semantic" || hit.source === "both"}
                <span class="by-meaning" title="Found by meaning" aria-label="Found by meaning"></span>
              {/if}
            </span>
          </span>
          {#if i === selected}
            <span class="enter mono">↵</span>
          {/if}
        </button>
      {/each}
    </div>
  {/if}

  {#if look && look.query === query.trim()}
    <div class="qa">
      <div class="qa-head">Ken looked</div>
      {#if look.state === "looking"}
        <div class="qa-body qa-thinking">Looking through the sources itself…</div>
      {:else}
        <div class="qa-body">{@html renderMarkdown(look.body)}</div>
        {#if look.sources.length}
          <div class="qa-foot">
            {#each look.sources as source}
              <button class="qa-chip mono" title={source} onclick={() => openCited(source)}>
                {source.split("/").pop()?.split("#")[0] || source}
              </button>
            {/each}
          </div>
        {/if}
      {/if}
    </div>
  {:else if searched && !searchError && results.length < 3 && query.trim().length >= 3}
    <div class="look-row">
      <button class="qa-dig" onclick={askKenToLook}>Ask Ken to look</button>
    </div>
  {/if}

  {#if searchError}
    <div class="empty error">Search failed: {searchError}</div>
  {:else if results.length === 0 && searched}
    <div class="empty"><EmptyState title="No matches" /></div>
  {/if}

  <div class="foot">
    <span><span class="kbd">↵</span> open</span>
    <span><span class="kbd">esc</span> close</span>
    <span class="continue"><span class="kbd">{shortcut("mod+enter")}</span> continue in chat</span>
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
  .scope-row {
    display: flex;
    gap: 6px;
    padding: 8px 14px 0;
    flex: none;
  }
  .scope-btn {
    border: 1px solid var(--border);
    background: var(--surface);
    border-radius: 999px;
    padding: 3px 10px;
    font-size: 11.5px;
    font-weight: 600;
    color: var(--ink-secondary);
  }
  .member-pin {
    margin-left: 4px;
    background-color: var(--surface);
    background-position: right 5px center;
    background-size: 11px;
    border: 1px solid var(--border);
    border-radius: var(--radius-control, 6px);
    padding: 2px 20px 2px 6px;
    font: inherit;
    font-size: 12px;
    color: var(--ink-secondary);
    max-width: 160px;
  }
  .scope-btn.active {
    background: var(--accent);
    border-color: var(--accent-deep);
    color: var(--surface);
  }
  .scope-info {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 14px 0;
    flex: none;
    font-size: 11.5px;
    color: var(--ink-secondary);
  }
  .progress {
    margin-left: auto;
    flex: none;
    color: var(--ink-tertiary);
  }
  .coverage-note {
    padding: 3px 14px 0;
    font-size: 11px;
    color: var(--ink-tertiary);
    flex: none;
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
  .meta {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
  }
  .path {
    font-size: 11.5px;
    color: var(--ink-tertiary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .symbol {
    font-size: 11.5px;
    color: var(--ink-secondary, var(--ink));
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .tag {
    flex: none;
    font-size: 9.5px;
    font-weight: 600;
    letter-spacing: 0.03em;
    text-transform: uppercase;
    padding: 1px 5px;
    border-radius: 4px;
    color: var(--ink-tertiary);
    background: var(--sunken);
  }
  /* A hit found by meaning: a small dot, explained by its tooltip. */
  .by-meaning {
    flex: none;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--accent);
    opacity: 0.7;
  }
  .tag-kind {
    color: var(--ink-tertiary);
  }
  .kind-select {
    margin-left: auto;
    font-size: 11.5px;
    padding: 2px 20px 2px 6px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background-color: var(--surface);
    background-position: right 5px center;
    background-size: 11px;
    color: var(--ink-secondary);
  }
  .tag-member {
    color: var(--ink-secondary);
    font-weight: 700;
    text-transform: none;
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
  .empty.error {
    color: var(--danger);
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
  .look-row {
    display: flex;
    align-items: center;
    gap: 10px;
    margin: 8px 16px 0;
    font-size: 12px;
    color: var(--ink-tertiary);
  }
  .qa-dig:hover {
    text-decoration: underline;
  }
</style>

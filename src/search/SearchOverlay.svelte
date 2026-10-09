<script lang="ts">
  import { onMount } from "svelte";
  import type { UnlistenFn } from "@tauri-apps/api/event";
  import {
    api,
    type Audience,
    type ContentType,
    type KindFilter,
    type QuickAnswer,
    type HybridHit,
    type RoutePlan,
    type RoutedSearchStateEvent,
  } from "../lib/api";
  import { app, forFocused } from "../lib/app.svelte";
  import { scope as sharedScope } from "../lib/scope.svelte";
  const teamName = $derived(sharedScope.team ?? app.workspace?.name ?? "the workspace");
  import { chats } from "../lib/chats.svelte";
  import { isQuestionQuery, stripStreamingBody } from "../lib/assist";
  import { renderMarkdown, renderSearchSnippet } from "../lib/markdown";
  import { kindForPath } from "../lib/format";
  import { inMember, parseCitation } from "../lib/citation";
  import FileGlyph from "../files/FileGlyph.svelte";
  import Search from "@lucide/svelte/icons/search";
  import { claudeInstallHelp, shortcut } from "../lib/platform";
  import { errorText } from "../lib/toast.svelte";

  let query = $state("");
  let selected = $state(0);
  let searched = $state(false);
  let input: HTMLInputElement;
  let timer: ReturnType<typeof setTimeout> | undefined;

  // "All projects" scope (workspace task 4.4 / kg-routing task 4.2): the
  // toggle is visible only in Workspace mode (`app.workspace` set).
  // `kgRoutingEnabled` upgrades the scope from `workspace`'s FTS fan-out to
  // `kgRouting`'s routed hybrid search — "same UI slot, richer results"
  // (kg-routing proposal). Single-project mode never renders the toggle and
  // `run()`'s project-scope branch is untouched, so that path (and its
  // `hybridSearch` call) stays byte-identical to before this change.
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
  // Routed search is built in (no longer the kgRouting flag).
  const kgRoutingEnabled = true;
  /** Narrow the all-projects scope to one member (ken-home-workspace
   *  3.3). `null` = every member. Routed search pins the plan to this id
   *  and skips the KG entirely — same result shape either way. */
  let pinnedMember = $state<string | null>(null);

  /** One results row, normalized from whichever endpoint answered the query
   *  (`hybrid_search`, `search_all_projects`, or `route_search`) so the
   *  template has a single rendering path regardless of scope. */
  interface DisplayHit {
    key: string;
    path: string;
    snippet: string;
    /** `"keyword"` | `"semantic"` | `"both"` | `null` — the all-projects
     *  keyword fan-out doesn't track this. */
    source: string | null;
    /** Only meaningful for single-project hybrid search. */
    tier: number | null;
    projectId: string | null;
    memberName: string | null;
    kgBreadcrumbs: string[];
    /** What the file is for: code, spec, doc, … */
    contentType: ContentType | null;
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
  /** Per-member coverage caveats for the current all-projects search — a
   *  dormant/missing/invalid member (keyword fan-out) or an index-building/
   *  unavailable one (routed) — so a short result list doesn't read as
   *  "nothing there" when it's really "not every member was searched". */
  let coverageNotes = $state<string[]>([]);
  let routedPlan = $state<RoutePlan | null>(null);
  let routedProgress = $state<RoutedSearchStateEvent | null>(null);

  const planLine = $derived.by(() => {
    if (!routedPlan) return null;
    const n = routedPlan.targets.length;
    const projects = `${n} project${n === 1 ? "" : "s"}`;
    if (routedPlan.reason.type === "named") return `Routed directly to ${projects}`;
    if (routedPlan.reason.type === "kgEntities")
      return `Routed via the knowledge graph to ${projects}`;
    return `Searched ${projects} (no direct or knowledge-graph match)`;
  });

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

  function fromHybrid(found: HybridHit[]): DisplayHit[] {
    return found.map((h) => ({
      key: h.path,
      path: h.path,
      snippet: h.snippet,
      source: h.source,
      tier: h.tier,
      projectId: null,
      memberName: null,
      kgBreadcrumbs: [],
      contentType: h.contentType,
    }));
  }

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

    if (scope === "all" && app.workspace) {
      routedProgress = null;
      if (kgRoutingEnabled) {
        // The shared scope store wins when it is narrowing (a group, or a
        // project chosen on Home); `pinnedMember` is the overlay's own
        // in-place narrowing for a single query.
        const res = await api.routeSearch(
          q,
          30,
          pinnedMember ?? sharedScope.projectId,
          sharedScope.groupName,
          audience,
          kinds,
        );
        if (q !== query.trim()) return; // stale
        searchError = null;
        routedPlan = res.plan;
        coverageNotes = res.memberStatus
          .filter((s) => s.status !== "searched")
          .map(
            (s) =>
              `${s.memberName}: ${s.status === "index-building" ? "still indexing" : "unavailable"}`,
          );
        results = res.results.map((h) => ({
          key: `${h.projectId}:${h.chunkId}`,
          path: h.path,
          snippet: h.snippet,
          source: h.source,
          tier: null,
          projectId: h.projectId,
          memberName: h.memberName,
          kgBreadcrumbs: h.kgBreadcrumbs,
          contentType: h.contentType,
        }));
      } else {
        routedPlan = null;
        const res = await api.searchAllProjects(q, 30, audience, kinds);
        if (q !== query.trim()) return;
        searchError = null;
        coverageNotes = res.memberStatus
          .filter((s) => s.status !== "searched")
          .map((s) => `${s.memberName}: ${s.status}`);
        results = res.results.map((h) => ({
          key: `${h.projectId}:${h.relPath}`,
          path: h.relPath,
          snippet: h.snippet,
          source: null,
          tier: null,
          projectId: h.projectId,
          memberName: h.memberName,
          kgBreadcrumbs: [],
          contentType: h.contentType,
        }));
      }
      searched = true;
      selected = 0;
      return;
    }

    // Project scope — unchanged from before the workspace/kg-routing scope
    // toggle existed.
    routedPlan = null;
    coverageNotes = [];
    const found = await api.hybridSearch(q, 30, audience, kinds);
    // A slower earlier request must not overwrite a newer query's results.
    if (q !== query.trim()) return;
    searchError = null;
    results = fromHybrid(found);
    searched = true;
    selected = 0;
  }

  // Item 2b: who the results are for, in every scope. Business: pages for
  // readers who never see code (Current, Design, Work, or a page that says
  // `audience: business`). Developers: everything else but the method's
  // own pages, code included. Any: everything.
  let audience = $state<Audience>(null);
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
    { v: "config", label: "Config" },
    { v: "data", label: "Data" },
    { v: "design", label: "Design" },
  ];
  const READERS: { v: Audience; label: string; title: string }[] = [
    { v: null, label: "Any reader", title: "Everything" },
    { v: "business", label: "Business readers", title: "Only pages written for people who never read code: Current, Design, Work" },
    { v: "dev", label: "Developers", title: "Dev pages and code; not the business pages or the method" },
  ];

  function setScope(next: Scope) {
    if (scope === next) return;
    // Writes through to the shared store, so Home reflects it too.
    if (next === "project") {
      pinnedMember = null;
      sharedScope.set("project", app.focused ?? null);
    } else {
      // The broad scope is the team chosen in the title bar.
      sharedScope.setTeamScope();
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
    // as needed"), then open — same order `federated-kg`'s wiki pointers
    // already use.
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
      placeholder={scope === "all" ? `Search ${teamName}…` : "Search this repo…"}
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
        This repo
      </button>
      <button class="scope-btn" class:active={scope === "all"} onclick={() => setScope("all")}>
        {teamName}
      </button>
      {#if scope === "all" && kgRoutingEnabled}
        <!-- Narrowing within all-projects: pins routing to one member,
             including one that is dormant. -->
        <select
          class="member-pin"
          value={pinnedMember ?? ""}
          onchange={(e) => setPinnedMember(e.currentTarget.value || null)}
          title="Narrow to one project"
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
  <div class="scope-row" role="group" aria-label="Who the results are for">
    {#each READERS as opt (opt.label)}
      <button
        class="scope-btn"
        class:active={audience === opt.v}
        aria-pressed={audience === opt.v}
        title={opt.title}
        onclick={() => {
          audience = opt.v;
          void run();
        }}
      >
        {opt.label}
      </button>
    {/each}
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
            onclick={() => app.openInFiles(source)}
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
      <div class="qa-body">
        Answers come from Claude Code. {claudeInstallHelp()}
      </div>
    </div>
  {/if}

  {#if scope === "all" && searched}
    <div class="scope-info">
      {#if routedPlan}
        <span class="scope-chip routed">routed</span>
        <span class="plan-line">{planLine}</span>
      {:else}
        <span class="scope-chip keyword">keyword</span>
      {/if}
      {#if routedProgress && routedProgress.state === "searching"}
        <span class="progress">searching {routedProgress.done}/{routedProgress.total}…</span>
      {/if}
    </div>
    {#each coverageNotes as note}
      <div class="coverage-note">{note}</div>
    {/each}
  {/if}

  {#if results.length > 0}
    <div class="section">Matches</div>
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
            <span class="snippet">{@html renderSearchSnippet(hit.snippet)}</span>
            <span class="meta">
              <!-- Show only the basename; full path stays in the tooltip so same-named files in different folders remain distinguishable. -->
              <span class="path mono" title={hit.path}>{hit.path.split("/").pop() || hit.path}</span>
              {#if hit.contentType}
                <span class="tag tag-kind" title="What this file is">{hit.contentType}</span>
              {/if}
              {#if hit.memberName}
                <span class="tag tag-member" title={hit.path}>{hit.memberName}</span>
              {/if}
              {#if hit.source === "semantic" || hit.source === "both"}
                <span class="tag tag-semantic" title="Matched by meaning, not just keywords">semantic</span>
              {/if}
              {#if hit.tier === 1}
                <span
                  class="tag tag-search-only"
                  title="Kept searchable by .kenignore but excluded from AI answers and the knowledge map"
                >search-only</span>
              {/if}
              {#each hit.kgBreadcrumbs as crumb}
                <span class="tag tag-kg" title="Routed via this knowledge-graph entity">{crumb}</span>
              {/each}
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
        <div class="qa-body qa-thinking">Looking through the folders itself…</div>
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
      <span>Not much in the index for this.</span>
      <button class="qa-dig" onclick={askKenToLook}>Ask Ken to look</button>
    </div>
  {/if}

  {#if searchError}
    <div class="empty error">Search failed: {searchError}</div>
  {:else if results.length === 0 && searched}
    <div class="empty">
      {#if scope === "all"}
        Nothing in {teamName} matches “{query}”.
      {:else if app.scanning}
        Nothing matches “{query}” <em>yet</em> — Ken is still reading your folder.
        Search lights up as files are indexed.
      {:else}
        Nothing in this repo matches “{query}”. Ken searches file contents and
        names — try fewer or different words.
      {/if}
    </div>
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
  .scope-chip {
    flex: none;
    font-size: 9.5px;
    font-weight: 700;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    padding: 2px 6px;
    border-radius: 4px;
    background: var(--sunken);
    color: var(--ink-tertiary);
  }
  .scope-chip.routed {
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 12%, transparent);
  }
  .plan-line {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
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
  .tag-semantic {
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 12%, transparent);
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
  .tag-kg {
    color: var(--accent-deep);
    background: color-mix(in srgb, var(--accent) 10%, transparent);
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

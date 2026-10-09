<script lang="ts">
  // A ticket opened in Files (Y1c): my tasks linked to it, with + Task to
  // add one already linked, and the wiki pages that cite the ticket id.
  import { api, type DayTask, type RoutedHit } from "../lib/api";
  import { app } from "../lib/app.svelte";
  import { scope } from "../lib/scope.svelte";
  import { onMount } from "svelte";
  import { day, openInRepo, ticketLink } from "../lib/day.svelte";
  import { shortTarget, stampLabel } from "../lib/day";
  import Check from "@lucide/svelte/icons/check";

  let { projectId, ticketId }: { projectId: string; ticketId: string } = $props();

  let tasks = $state<DayTask[]>([]);
  let pages = $state<{ projectId: string; path: string; name: string }[]>([]);
  let pagesLoaded = $state(false);

  // Tasks follow `day-changed`, which needs the day store listening even
  // when Home has not been opened.
  onMount(() => void day.init());

  // A reply for a ticket we have since left is dropped.
  let taskSeq = 0;
  let pageSeq = 0;
  let tasksFor = "";

  async function loadTasks() {
    const n = ++taskSeq;
    const key = `${projectId}:${ticketId}`;
    if (key !== tasksFor) {
      tasksFor = key;
      tasks = [];
    }
    const next = await api.ticketTasks(projectId, ticketId).catch(() => []);
    if (n === taskSeq) tasks = next;
  }

  async function loadPages() {
    const n = ++pageSeq;
    pages = [];
    pagesLoaded = false;
    const id = ticketId;
    const res = await api.routeSearch(id, 30, null, scope.team).catch(() => null);
    if (n !== pageSeq) return;
    const needle = id.toLowerCase();
    const seen = new Set<string>();
    const out: typeof pages = [];
    for (const h of res?.results ?? []) {
      if (!h.page) continue;
      // The ticket itself, and other tickets, are not wiki pages.
      if (/^tickets\//i.test(h.path)) continue;
      // Search ranks near misses too; a page cites the ticket only when the
      // id is in what it found.
      if (!citesId(h, needle)) continue;
      const key = `${h.projectId}:${h.path}`;
      if (seen.has(key)) continue;
      seen.add(key);
      out.push({ projectId: h.projectId, path: h.path, name: h.page.title || (h.path.split("/").pop() ?? h.path) });
    }
    pages = out;
    pagesLoaded = true;
  }

  function citesId(h: RoutedHit, needle: string): boolean {
    // Snippets may mark the match up; read the text without the tags.
    const text = `${h.snippet} ${h.page?.title ?? ""}`.replace(/<[^>]*>/g, "");
    return text.toLowerCase().includes(needle);
  }

  // Tasks change from Your day, the chat and agents (day.state follows
  // `day-changed`); read them again with it.
  $effect(() => {
    void projectId;
    void ticketId;
    void day.state;
    void loadTasks();
  });

  $effect(() => {
    void projectId;
    void ticketId;
    void loadPages();
  });

  async function toggle(t: DayTask) {
    await day.toggle(t).catch(() => null);
    await loadTasks();
  }

  function detail(t: DayTask): string {
    if (t.state === "done") {
      const at = stampLabel(t.updated, day.today);
      return at ? `done ${at}` : "done";
    }
    return shortTarget(t.target, day.today);
  }

  function addTask() {
    day.openNew([ticketLink(projectId, ticketId)]);
  }

  function openTask(t: DayTask) {
    // It may be done earlier or due another day, so not in today's list.
    day.openTask(t.id, { task: t, ticket: { projectId, ticketId } });
    app.screen = "home";
  }
</script>

<aside class="col" aria-label="Ticket {ticketId}">
  <div class="head">
    <span class="overline">Your tasks · {tasks.length}</span>
    <span class="sp"></span>
    <button class="link" onclick={addTask}>+ Task</button>
  </div>
  {#if tasks.length > 0}
    <div class="list">
      {#each tasks as t (t.id)}
        <div class="row" class:done={t.state === "done"}>
          <button
            class="cb"
            class:on={t.state === "done"}
            aria-label={t.state === "done" ? "Mark open" : "Mark done"}
            onclick={() => void toggle(t)}
          >
            {#if t.state === "done"}<Check size={10} strokeWidth={2.5} />{/if}
          </button>
          <button class="tx" onclick={() => openTask(t)}>
            <span class="tt">{t.title}</span>
            {#if detail(t)}<span class="td">{detail(t)}</span>{/if}
          </button>
        </div>
      {/each}
    </div>
  {:else}
    <p class="quiet">None linked to {ticketId}.</p>
  {/if}

  <div class="head second"><span class="overline">Wiki pages that cite it</span></div>
  {#if pages.length > 0}
    <div class="list">
      {#each pages as p (p.projectId + ":" + p.path)}
        <button class="entry" title={p.path} onclick={() => void openInRepo(p.projectId, p.path)}>{p.name}</button>
      {/each}
    </div>
  {:else if pagesLoaded}
    <p class="quiet">None.</p>
  {/if}
</aside>

<style>
  .col {
    width: 260px;
    flex: none;
    border-left: 1px solid var(--border);
    background: var(--paper);
    padding: 14px 14px 20px;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .head.second {
    margin-top: 14px;
  }
  .sp {
    flex: 1;
  }
  .link {
    border: none;
    background: transparent;
    padding: 0;
    font: inherit;
    font-size: 12px;
    color: var(--accent);
    cursor: pointer;
  }
  .link:hover {
    color: var(--accent-hover);
  }
  .list {
    display: flex;
    flex-direction: column;
  }
  .row {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    padding: 6px 0;
  }
  .row + .row {
    border-top: 1px solid var(--border);
  }
  .cb {
    flex: none;
    margin-top: 2px;
    width: 14px;
    height: 14px;
    border-radius: 4px;
    border: 1.5px solid var(--border-strong);
    background: var(--surface);
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    color: var(--surface);
  }
  .cb.on {
    background: var(--accent);
    border-color: var(--accent);
  }
  .tx {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
    border: none;
    background: transparent;
    padding: 0;
    font: inherit;
    text-align: left;
    color: var(--ink);
    cursor: pointer;
  }
  .tt {
    font-size: 12.5px;
    font-weight: 500;
  }
  .tx:hover .tt {
    color: var(--accent-deep);
  }
  .row.done .tt {
    font-weight: 400;
    text-decoration: line-through;
    color: var(--ink-tertiary);
  }
  .td {
    font-size: 11px;
    color: var(--ink-tertiary);
  }
  .entry {
    border: none;
    background: transparent;
    padding: 5px 0;
    font: inherit;
    font-size: 12.5px;
    text-align: left;
    color: var(--ink-secondary);
    cursor: pointer;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .entry:hover {
    color: var(--accent-deep);
  }
  .quiet {
    margin: 0;
    font-size: 12px;
    color: var(--ink-tertiary);
  }
</style>

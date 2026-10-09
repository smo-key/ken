<script lang="ts">
  // Home (the Briefing design): your morning briefing. The digest Ken wrote
  // for the team, your list (tickets are square, tasks are round), the files
  // you were in, and on the right the top of the Inbox and what Ken is
  // reading. Clicking a task opens its panel in place of the right rail.
  import { onMount } from "svelte";
  import { app } from "../lib/app.svelte";
  import { scope } from "../lib/scope.svelte";
  import { day, openInRepo, openMemberPath, repoName } from "../lib/day.svelte";
  import { shortTarget, taskDetail, ticketDetail } from "../lib/day";
  import { inbox, type InboxItem } from "../lib/inbox.svelte";
  import { digestMarkdown } from "../lib/assist";
  import { renderMarkdown } from "../lib/markdown";
  import { parseCitation } from "../lib/citation";
  import { loadRecents, mergeTeamRecents, fallbackRecents } from "../lib/recent";
  import { kindForPath, timeAgo } from "../lib/format";
  import { toast } from "../lib/toast.svelte";
  import type { DayTask } from "../lib/api";
  import TaskPanel from "../day/TaskPanel.svelte";
  import FileGlyph from "../files/FileGlyph.svelte";
  import InboxKindIcon from "./InboxKindIcon.svelte";
  import Loading from "../lib/ui/Loading.svelte";
  import ModelDownloadDialog from "../files/previews/ModelDownloadDialog.svelte";
  import { api, type ModelStatus } from "../lib/api";
  import Check from "@lucide/svelte/icons/check";
  import Copy from "@lucide/svelte/icons/copy";
  import Plus from "@lucide/svelte/icons/plus";

  onMount(() => {
    void scope.init();
    void day.init();
    void checkMeaning();
  });

  // Search by meaning needs a model on this computer; until one is
  // installed, search is by keyword only. Offer the default once, here.
  let meaningModel = $state<ModelStatus | null>(null);
  async function checkMeaning() {
    const models = await api.listModels().catch(() => [] as ModelStatus[]);
    const embedding = models.filter((m) => m.category === "embedding");
    meaningModel = embedding.some((m) => m.installed) ? null : (embedding.find((m) => m.recommended) ?? null);
  }

  // ── The greeting and the digest ───────────────────────────────────

  const firstName = $derived((day.state?.me.name ?? "").trim().split(/\s+/)[0] ?? "");
  const greeting = $derived.by(() => {
    const h = new Date().getHours();
    const part = h < 12 ? "Good morning" : h < 18 ? "Good afternoon" : "Good evening";
    return firstName ? `${part}, ${firstName}` : part;
  });
  const todayLong = $derived(
    new Date(`${day.today}T12:00:00`).toLocaleDateString(undefined, { weekday: "long", month: "long", day: "numeric" }),
  );

  let copied = $state(false);
  let copyTimer: ReturnType<typeof setTimeout> | undefined;
  async function share() {
    if (!day.digest) return;
    await navigator.clipboard.writeText(digestMarkdown({ date: todayLong, body: day.digest.body, sources: day.digest.sources }));
    copied = true;
    clearTimeout(copyTimer);
    copyTimer = setTimeout(() => (copied = false), 1500);
  }
  function digestTime(epoch: number): string {
    return new Date(epoch * 1000).toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" });
  }
  function leaf(path: string): string {
    return path.split("/").pop() || path;
  }
  // A cited file opens in Files at its line or heading; a web link is left
  // to the browser.
  function onDigestClick(e: MouseEvent) {
    const a = (e.target as HTMLElement).closest("a");
    if (!a) return;
    const href = a.getAttribute("href") ?? "";
    if (/^(https?|mailto):/i.test(href)) return;
    e.preventDefault();
    const c = parseCitation(href);
    if (!c) return;
    const where = { line: c.line, anchor: c.anchor };
    if (c.projectId) void openInRepo(c.projectId, c.path, where);
    else void openMemberPath(c.path, where);
  }
  // Files still being indexed. `queued` is the files the knowledge map has
  // not read yet: Claude's map build reads them, so they are not indexing.
  const indexing = $derived(app.scanning);

  // ── Your list ─────────────────────────────────────────────────────

  async function run(p: Promise<unknown>, what: string) {
    try {
      await p;
    } catch (e) {
      toast.error(what, e);
    }
  }
  const listSummary = $derived.by(() => {
    const t = day.tickets.length;
    const k = day.tasks.length;
    const parts: string[] = [];
    if (t > 0) parts.push(`${t} ${t === 1 ? "ticket" : "tickets"}`);
    parts.push(`${k} ${k === 1 ? "task" : "tasks"}`);
    return parts.join(" · ");
  });
  function dueClass(target: string | null, done = false): string {
    if (done || !target) return "";
    if (target < day.today) return "late";
    if (target === day.today) return "today";
    return "";
  }
  function dueLabel(target: string | null): string {
    if (!target) return "";
    if (target < day.today) {
      const days = Math.round((Date.parse(`${day.today}T12:00:00`) - Date.parse(`${target}T12:00:00`)) / 86_400_000);
      return `${days}d late`;
    }
    if (target === day.today) return "Today";
    return shortTarget(target, day.today);
  }
  const selectedId = $derived(day.panel?.mode === "edit" ? day.panel.id : null);
  const panelOpen = $derived(day.panel !== null && (day.panel.mode === "new" || day.panelTask !== null));

  function openTask(task: DayTask) {
    if (task.inbox) return;
    day.openTask(task.id);
  }

  // ── Picked up where you left off ──────────────────────────────────

  const recents = $derived.by(() => {
    const ids = scope.teamProjectIds;
    const focused = app.focused;
    const lists = ids.map((id) => ({
      projectId: id,
      entries: id === focused ? app.recents : loadRecents(id),
      known: id === focused ? new Set(app.files.map((f) => f.relPath)) : null,
    }));
    const merged = mergeTeamRecents(lists);
    if (merged.length > 0) return merged.slice(0, 8);
    return focused ? fallbackRecents(app.files).slice(0, 8).map((f) => ({ projectId: focused, path: f.relPath, at: f.at })) : [];
  });

  // ── A ticket's status, set from Your day ──────────────────────────
  /** The method's ticket statuses; done and cancelled leave the list. */
  const TICKET_STATUSES = ["todo", "in-progress", "blocked", "in-review", "testing", "done", "cancelled"];
  async function setTicketStatus(projectId: string, relPath: string, status: string) {
    try {
      await api.ticketSetStatus(projectId, relPath, status);
      await day.refreshState();
    } catch (e) {
      toast.error("Could not change the ticket's status", e);
    }
  }

  // ── Needs you: the top of the Inbox ───────────────────────────────

  const needs = $derived(inbox.items.slice(0, 4));
  function openInbox(item?: InboxItem) {
    if (item) inbox.select(item.id);
    app.screen = "inbox";
  }
  const reading = $derived(inbox.reading);
  const indexLine = $derived.by(() => {
    const h = day.health;
    if (!h) return "";
    const parts = [h.indexed >= h.total ? `All ${h.total} ${h.total === 1 ? "folder" : "folders"} indexed` : `${h.indexed} of ${h.total} folders indexed`];
    if (h.queued > 0) parts.push(`${h.queued} not in the map yet`);
    return parts.join(" · ");
  });
</script>

<div class="home">
  <main>
    <section class="brief">
      <h1 class="t-display">{greeting}</h1>
      <div class="meta">
        <span>{todayLong}</span>
        {#if day.digest}
          <span>·</span>
          <span>Digest at {digestTime(day.digest.generatedAt)}{day.digest.sources.length ? ` · ${day.digest.sources.length} ${day.digest.sources.length === 1 ? "source" : "sources"}` : ""}</span>
        {/if}
        <span class="grow"></span>
        {#if day.digest}
          <button class="btn btn-small" onclick={share} title="Copy the digest as markdown">
            {#if copied}<Check size={13} strokeWidth={2} /> Copied{:else}<Copy size={13} strokeWidth={1.75} /> Share{/if}
          </button>
        {/if}
      </div>
      {#if day.digest}
        <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
        <div class="digest" onclick={onDigestClick}>{@html renderMarkdown(day.digest.body)}</div>
        {#if day.digest.sources.length > 0}
          <div class="sources">
            {#each day.digest.sources as source}
              <button class="filechip" title={source} onclick={() => void openMemberPath(source)}>{leaf(source)}</button>
            {/each}
          </div>
        {/if}
      {:else if day.generating}
        <p class="digest quiet">Ken is writing today's digest.</p>
      {:else if !day.claudeFound}
        <p class="digest quiet">The digest needs Claude Code. Settings › AI says how to install it.</p>
      {:else}
        <p class="digest quiet">No digest yet. Ken writes one each morning.</p>
        <div>
          <button class="btn btn-small" title={indexing ? "Ken is still indexing; the digest may miss what it has not read." : undefined} onclick={() => void day.writeDigest()}>Write it now</button>
        </div>
      {/if}
      {#if day.digestError}
        <p class="t-small err">The last try did not finish: {day.digestError}</p>
      {/if}
    </section>

    <section class="list">
      <div class="lhead">
        <h2 class="t-heading">Your list</h2>
        <span class="t-small">{listSummary}</span>
        <span class="grow"></span>
        <button class="btn btn-link btn-small" onclick={() => day.openNew()}><Plus size={14} strokeWidth={2} /> Add task</button>
      </div>
      <div class="rows">
        {#if !day.state && !day.loadError}
          <Loading label="Reading your list…" lines={3} />
        {:else if !day.state && day.loadError}
          <p class="t-small err">Your list could not be read: {day.loadError}</p>
        {/if}
        {#each day.tickets as t (t.projectId + ":" + t.relPath)}
          <div class="row">
            <button class="rmain plain" title="{t.repo}/{t.relPath}" onclick={() => void openInRepo(t.projectId, t.relPath)}>
              <span class="tid">{t.id}</span>
              <span class="rtitle">{t.title}</span>
              <span class="rdetail">{ticketDetail(t)}</span>
            </button>
            <select
              class="status-pick"
              aria-label="Status of {t.id}"
              value={t.state}
              onchange={(e) => void setTicketStatus(t.projectId, t.relPath, (e.currentTarget as HTMLSelectElement).value)}
            >
              {#if !TICKET_STATUSES.includes(t.state)}<option value={t.state}>{t.state || "no status"}</option>{/if}
              {#each TICKET_STATUSES as st (st)}<option value={st}>{st.replace("-", " ")}</option>{/each}
            </select>
            <span class="due {dueClass(t.target)}">{dueLabel(t.target)}</span>
          </div>
        {/each}
        {#each day.tasks as task (task.relPath + ":" + task.id)}
          {@const done = task.state === "done"}
          <div class="row" class:sel={selectedId === task.id} class:done>
            {#if task.inbox}
              <span class="k-check pending" title="Sent by {task.from ?? 'a teammate'}; accept it first"></span>
            {:else}
              <button
                class="k-check"
                class:on={done}
                aria-label={done ? "Mark open" : "Mark done"}
                onclick={() => void run(day.toggle(task), "Could not change the task")}
              >
                {#if done}<Check size={11} strokeWidth={3} />{/if}
              </button>
            {/if}
            <button class="rmain plain" onclick={() => openTask(task)} disabled={!!task.inbox}>
              <span class="rtitle">{task.title}</span>
              <span class="rdetail" class:from={!!task.inbox}>{task.inbox ? `from ${task.from ?? "a teammate"}` : taskDetail(task)}</span>
            </button>
            {#if task.inbox}
              <button class="btn btn-primary btn-small" onclick={() => void run(day.accept(task), "Could not accept the task")}>Accept</button>
            {/if}
            <span class="due {dueClass(task.target, done)}">{dueLabel(task.target)}</span>
          </div>
        {/each}
        {#if day.state && day.tickets.length === 0 && day.tasks.length === 0}
          <p class="none">Nothing on your list. Add task puts one here, and so does Ken when you ask it to.</p>
        {/if}
      </div>
    </section>

    {#if recents.length > 0}
      <section class="recent">
        <div class="lhead">
          <h2 class="t-heading">Picked up where you left off</h2>
          <span class="grow"></span>
          <button class="btn btn-link btn-small" onclick={() => (app.screen = "files")}>All files</button>
        </div>
        <div class="cards">
          {#each recents as r (r.projectId + ":" + r.path)}
            <button class="fcard" title={r.path} onclick={() => void openInRepo(r.projectId, r.path)}>
              <FileGlyph kind={kindForPath(r.path)} size="sm" />
              <span class="fname">{leaf(r.path)}</span>
              <span class="t-small">{repoName(r.projectId)} · {timeAgo(r.at)}</span>
            </button>
          {/each}
        </div>
      </section>
    {/if}
  </main>

  <aside class="railcol">
    {#if panelOpen && day.panel}
      {#key day.panel.mode === "edit" ? day.panel.id : "new"}
        {#if day.panel.mode === "edit"}
          {#if day.panelTask}<TaskPanel task={day.panelTask} />{/if}
        {:else}
          <TaskPanel task={null} newLinks={day.panel.links} />
        {/if}
      {/key}
    {:else}
      <div class="nhead">
        <h2 class="t-heading">Needs you</h2>
        {#if inbox.count > 0}<span class="count">{inbox.count}</span>{/if}
        <span class="grow"></span>
        <button class="btn btn-link btn-small" onclick={() => openInbox()}>Open Inbox</button>
      </div>
      {#each needs as n (n.id)}
        <div class="k-action">
          <div class="kind {n.tone}"><InboxKindIcon item={n} bare />{n.kind} · {n.from}</div>
          <button class="title plain" onclick={() => openInbox(n)}>{n.title}</button>
          {#if n.short}<div class="short">{n.short}</div>{/if}
          <div class="acts">
            <button class="btn btn-primary btn-small" onclick={() => openInbox(n)}>Open</button>
          </div>
        </div>
      {/each}
      {#if needs.length === 0}
        <div class="k-empty card-empty">
          <span class="head">Nothing waiting on you</span>
          <span class="body">When a teammate sends you something, or Ken needs a decision, it shows up here.</span>
        </div>
      {/if}
      {#if meaningModel}
        <div class="k-action meaning">
          <div class="kind accent">Search by meaning is off</div>
          <div class="short">Search finds exact words only until a model that reads for meaning is on this computer. It runs here; nothing leaves it.</div>
          <ModelDownloadDialog status={meaningModel} compact onInstalled={() => void checkMeaning()} />
        </div>
      {/if}
      <button class="reading" onclick={() => (app.screen = "ingests")}>
        {#if reading}
          <span class="rline"><b>{reading.verb}</b><span class="rname">{reading.name}</span><span class="grow"></span>{#if reading.pct !== null}<span class="pct">{reading.pct}%</span>{/if}</span>
          <span class="k-progress"><span style="width: {reading.pct ?? 8}%"></span></span>
        {/if}
        <span class="t-small">
          {indexLine}{inbox.queued > 0 ? ` · ${inbox.queued} to read` : ""}{#if app.failedFiles.length > 0} · <span class="bad">{app.failedFiles.length} couldn't be read</span>{/if}
        </span>
      </button>
    {/if}
  </aside>
</div>

<style>
  .home {
    flex: 1;
    min-width: 0;
    display: grid;
    grid-template-columns: minmax(0, 1fr) clamp(300px, 26vw, 380px);
    min-height: 0;
  }
  main {
    padding: clamp(24px, 3vw, 44px) clamp(24px, 4vw, 64px) 48px;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 28px;
  }
  main > section {
    width: 100%;
    max-width: 880px;
    display: flex;
    flex-direction: column;
  }
  .brief {
    gap: 12px;
  }
  h1 {
    margin: 0;
  }
  .meta {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 13px;
    color: var(--ink-3);
    white-space: nowrap;
  }
  .grow {
    flex: 1;
  }
  .digest {
    font-family: var(--font-serif);
    font-size: clamp(18px, 1.25vw, 20px);
    line-height: 1.6;
    max-width: 68ch;
    text-wrap: pretty;
    margin: 0;
  }
  .digest :global(p) {
    margin: 0 0 10px;
  }
  .digest :global(p:last-child) {
    margin-bottom: 0;
  }
  .digest.quiet {
    font-size: 17px;
    color: var(--ink-2);
  }
  .sources {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
  }
  .filechip {
    cursor: pointer;
  }
  .filechip:hover {
    border-color: var(--accent);
  }
  .err {
    color: var(--danger);
  }

  .list {
    gap: 8px;
  }
  .lhead {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  h2 {
    margin: 0;
  }
  .rows {
    display: flex;
    flex-direction: column;
    border-top: 1px solid var(--line);
  }
  .row {
    display: flex;
    align-items: center;
    gap: 12px;
    min-height: 44px;
    border: none;
    border-bottom: 1px solid var(--line-soft);
    background: transparent;
    text-align: left;
    color: var(--ink);
    padding: 0;
    font: inherit;
  }
  .row:hover {
    background: color-mix(in oklch, var(--line-soft) 55%, transparent);
  }
  .row.sel {
    background: var(--accent-soft);
    margin: 0 -10px;
    padding: 0 10px;
    border-radius: 6px;
  }
  .rmain {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .plain {
    border: none;
    background: transparent;
    padding: 0;
    font: inherit;
    color: inherit;
    text-align: left;
    cursor: pointer;
  }
  .plain:disabled {
    cursor: default;
  }
  .tid {
    font-family: var(--font-mono);
    font-size: 12px;
    color: var(--accent-ink);
    flex: none;
  }
  .rtitle {
    font-size: 14px;
    font-weight: 500;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .row.done .rtitle {
    color: var(--ink-3);
    text-decoration: line-through;
  }
  .rdetail {
    font-size: 12.5px;
    color: var(--ink-3);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .rdetail.from {
    color: var(--attn-ink);
  }
  .status-pick {
    flex: none;
    font-size: 12px;
    padding: 3px 24px 3px 9px;
    border-radius: 999px;
    border-color: var(--line);
    background-color: var(--line-soft);
    background-position: right 7px center;
    background-size: 11px;
    color: var(--ink-2);
  }
  .due {
    width: 62px;
    flex: none;
    text-align: right;
    font-size: 12.5px;
    font-variant-numeric: tabular-nums;
    color: var(--ink-3);
  }
  .due.late {
    color: var(--danger);
    font-weight: 600;
  }
  .due.today {
    color: var(--accent-ink);
    font-weight: 600;
  }
  .none {
    margin: 0;
    padding: 14px 0;
    font-size: 13.5px;
    color: var(--ink-3);
  }

  .recent {
    gap: 10px;
  }
  .cards {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(170px, 1fr));
    gap: 10px;
  }
  .fcard {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 8px;
    padding: 12px;
    border-radius: 10px;
    background: var(--surface);
    border: 1px solid var(--line-soft);
    text-align: left;
    color: var(--ink);
    min-width: 0;
  }
  .fcard:hover {
    border-color: var(--line-strong);
  }
  .fname {
    font-size: 13px;
    font-weight: 500;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 100%;
  }

  .railcol {
    border-left: 1px solid var(--line-soft);
    background: var(--rail);
    padding: 24px 18px;
    display: flex;
    flex-direction: column;
    gap: 10px;
    overflow-y: auto;
    min-height: 0;
  }
  .nhead {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .kind.attn {
    color: var(--attn-ink);
  }
  .kind.danger {
    color: var(--danger);
  }
  .kind.accent {
    color: var(--accent-ink);
  }
  .kind.ink {
    color: var(--ink-2);
  }
  .title.plain {
    text-wrap: pretty;
  }
  .card-empty {
    background: var(--surface);
    border: 1px solid var(--line);
  }
  .reading {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px 14px;
    border-radius: var(--radius-card);
    border: 1px dashed var(--line-strong);
    background: transparent;
    text-align: left;
    color: var(--ink);
    margin-top: 4px;
  }
  .reading:hover {
    border-color: var(--accent);
  }
  .rline {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 13px;
    width: 100%;
  }
  .rline b {
    font-weight: 600;
  }
  .rname {
    color: var(--ink-2);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .pct {
    font-variant-numeric: tabular-nums;
    color: var(--ink-3);
  }
  .bad {
    color: var(--danger);
  }
</style>

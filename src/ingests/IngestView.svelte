<script lang="ts">
  import Loading from "../lib/ui/Loading.svelte";
  import FilePlus from "@lucide/svelte/icons/file-plus";
  import NotebookPen from "@lucide/svelte/icons/notebook-pen";
  import MessagesSquare from "@lucide/svelte/icons/messages-square";
  import FileText from "@lucide/svelte/icons/file-text";
  import CircleCheck from "@lucide/svelte/icons/circle-check";
  import Circle from "@lucide/svelte/icons/circle";
  import CircleX from "@lucide/svelte/icons/circle-x";
  import Gavel from "@lucide/svelte/icons/gavel";
  import TriangleAlert from "@lucide/svelte/icons/triangle-alert";
  import CircleAlert from "@lucide/svelte/icons/circle-alert";
  import Ticket from "@lucide/svelte/icons/ticket";
  import FilePen from "@lucide/svelte/icons/file-pen";
  // Ingest, the inbox that empties (the Wright white box, frame 8b). Four
  // ways to add a source: drop or choose files, record, write a note, or take
  // a chat. Each lands in the knowledge base's Research/Ingestion/Raw/ and is
  // read at once into a dated note in Ingested/, the source moving beside it.
  // The pages the note changes are written then and there, one section each,
  // citing the note. Four things wait on the card: a ruling as a ready entry
  // for the decisions log, a risk or issue for the RAID register, an action
  // as a drafted ticket, and an edit held for a person. Contradictions and open questions are findings, in the
  // Inbox. Each ingest is one commit in the knowledge base: git is the undo.
  import { onMount, untrack } from "svelte";
  import { open as openDialog } from "@tauri-apps/plugin-dialog";
  import { api, type IngestCard, type IngestOverview, type IngestProposal } from "../lib/api";
  import { app } from "../lib/app.svelte";
  import { record } from "../lib/record.svelte";
  import { rail } from "../lib/rail.svelte";
  import { renderMarkdown, renderTitle } from "../lib/markdown";
  import { timeAgo } from "../lib/format";
  import { errorText } from "../lib/toast.svelte";
  import { openConfirm } from "../lib/ui/ConfirmMenu.svelte";
  import {
    canReadNow,
    ingestBusy,
    pageName,
    raidDetail,
    rawActions,
    rawStatus,
  } from "../lib/ingestCard";
  import ProposalDetail from "./ProposalDetail.svelte";
  import RecordPanel from "./RecordPanel.svelte";
  import ChatSource from "./ChatSource.svelte";
  import EmptyState from "../lib/ui/EmptyState.svelte";
  import ScreenHeader from "../lib/ui/ScreenHeader.svelte";
  import { singleFlight } from "../lib/refresh";

  let overview = $state<IngestOverview | null>(null);
  let error = $state<string | null>(null);
  /** Why the list could not be read; kept apart from an action's error. */
  let loadError = $state<string | null>(null);
  let selected = $state<number | null>(null);
  let card = $state<IngestCard | null>(null);
  let busy = $state<string | null>(null);
  let dropping = $state(false);
  let adding = $state(0);
  let openDiff = $state<number | null>(null);

  // The ways to add a source besides a file: record a meeting, write a
  // note, or send part of a chat. Each opens its panel under the buttons.
  type Source = "record" | "note" | "chat";
  let source = $state<Source | null>(null);
  function toggleSource(s: Source) {
    source = source === s ? null : s;
  }
  /** A source still being read, chosen in the list (it has no card yet). */
  let selectedRaw = $state<string | null>(null);
  const rawSelected = $derived(selectedRaw ? (overview?.raw.find((r) => r.path === selectedRaw) ?? null) : null);
  function selectCard(id: number) {
    selected = id;
    selectedRaw = null;
  }
  function selectRaw(r: IngestOverview["raw"][number]) {
    if (r.cardId !== null) {
      selectCard(r.cardId);
      return;
    }
    selected = null;
    selectedRaw = r.path;
  }
  /** The steps a source goes through, as the progress card names them. A
   *  recording is transcribed first; a document is not. */
  const RECORDING_STEPS = ["Filed in Raw", "Transcribed", "Read", "Pages written", "Ready"];
  const DOCUMENT_STEPS = ["Filed in Raw", "Read", "Pages written", "Ready"];
  const RECORDING_EXT = /\.(mp4|mov|m4v|webm|mkv|avi|wav|mp3|m4a|aac|flac|ogg|opus)$/i;
  function isRecording(name: string, kind: string): boolean {
    return RECORDING_EXT.test(name) || kind === "recording";
  }
  function stepsFor(r: IngestOverview["raw"][number]): string[] {
    return isRecording(r.name, r.kind) ? RECORDING_STEPS : DOCUMENT_STEPS;
  }
  /** The step a source is on (or stopped at, when it failed). */
  function stepAt(r: IngestOverview["raw"][number]): number {
    const steps = stepsFor(r);
    const transcribed = steps.indexOf("Transcribed");
    const read = steps.indexOf("Read");
    switch (r.state) {
      case "queued":
        return 1;
      case "waiting":
      case "transcribing":
        return transcribed >= 0 ? transcribed : read;
      case "reading":
        return read;
      case "failed":
        // A recording that has no text yet stopped at its transcript.
        return transcribed >= 0 && /transcri/i.test(r.detail ?? "") ? transcribed : read;
      default:
        return steps.length;
    }
  }
  /** A note's line that only says there is nothing ("Nothing overturns",
   *  "There are none") is not shown: an empty section is left out. */
  function saysNothing(s: string | null | undefined): boolean {
    const text = (s ?? "").replace(/[*_`#>]/g, "").trim().toLowerCase();
    return !text || /^(nothing\b|none\b|no (contradictions|change|overturn)|there (are|is|were|was) (none|no)\b|n\/a\b)/.test(text);
  }
  async function openSource(path: string) {
    if (!overview) return;
    try {
      await api.revealInFolder(path, overview.projectId);
    } catch (e) {
      error = errorText(e);
    }
  }
  let noteTitle = $state("");
  let noteText = $state("");

  // Coming to Ingest while a take runs (the title-bar pill) shows the recorder.
  $effect(() => {
    if (app.screen === "ingests" && (record.recording || record.transcribing)) untrack(() => (source = "record"));
  });
  // A finished take is in Raw now.
  $effect(() => {
    if (record.savedPath) untrack(() => void refresh());
  });

  // Read sources still in Raw have their card; under them, the rest.
  const rawCards = $derived(new Set((overview?.raw ?? []).map((r) => r.cardId).filter((id) => id !== null)));
  const ingested = $derived((overview?.ingested ?? []).filter((s) => !rawCards.has(s.id)));

  // One read at a time: the 4-second follow and the events would otherwise
  // stack reads up behind a slow one (see `singleFlight`).
  const refresh = singleFlight(async () => {
    const ws = app.workspace?.id;
    try {
      const next = await api.ingestOverview();
      if (ws !== app.workspace?.id) return;
      overview = next;
      rail.setIngest(next.waiting ?? 0);
      loadError = null;
      if (selectedRaw) {
        // A source picked while it is read stays picked; once it has its
        // card, the card is shown.
        const r = next.raw.find((x) => x.path === selectedRaw);
        if (r?.cardId != null) selectCard(r.cardId);
        else if (!r) {
          // Read: the source moved beside its note, and its card is the
          // newest one.
          selectedRaw = null;
          selected = next.ingested[0]?.id ?? null;
        }
      } else if (selected === null) {
        // The newest read source is shown until the person picks another.
        selected = next.raw.find((r) => r.cardId !== null)?.cardId ?? next.ingested[0]?.id ?? null;
      }
    } catch (e) {
      loadError = String(e);
    }
  });

  async function loadCard(id: number | null) {
    if (id === null) {
      card = null;
      return;
    }
    try {
      card = await api.ingestCard(id);
    } catch (e) {
      card = null;
      error = String(e);
    }
  }

  const reload = singleFlight(async () => {
    await refresh();
    await loadCard(selected);
  });

  $effect(() => {
    void app.workspace?.id;
    void refresh();
  });
  $effect(() => {
    void loadCard(selected);
  });

  onMount(() => {
    // While anything is being read, the list follows it.
    const t = setInterval(() => {
      if (ingestBusy(overview)) void reload();
    }, 4000);
    // A file dropped outside the drop area would make the window open it.
    const stop = (e: DragEvent) => e.preventDefault();
    window.addEventListener("dragover", stop);
    window.addEventListener("drop", stop);
    return () => {
      clearInterval(t);
      window.removeEventListener("dragover", stop);
      window.removeEventListener("drop", stop);
    };
  });

  async function chooseFiles() {
    const picked = await openDialog({ multiple: true, title: "Choose files to ingest" });
    const paths = Array.isArray(picked) ? picked : typeof picked === "string" ? [picked] : [];
    if (paths.length === 0) return;
    adding += paths.length;
    try {
      await api.ingestAdd(paths);
    } catch (e) {
      error = String(e);
    } finally {
      adding -= paths.length;
      await refresh();
    }
  }

  async function onDrop(e: DragEvent) {
    e.preventDefault();
    dropping = false;
    const files = [...(e.dataTransfer?.files ?? [])];
    if (files.length === 0) return;
    adding += files.length;
    for (const f of files) {
      try {
        await api.ingestAddBytes(f);
      } catch (err) {
        error = String(err);
      } finally {
        adding -= 1;
      }
    }
    await refresh();
  }

  async function addNote() {
    const text = noteText.trim();
    if (!text) return;
    busy = "note";
    try {
      await api.ingestAddText(noteTitle.trim() || null, text);
      noteTitle = "";
      noteText = "";
      error = null;
    } catch (e) {
      error = errorText(e);
    } finally {
      busy = null;
      await refresh();
    }
  }

  async function chatAdded() {
    error = null;
    await refresh();
  }

  async function readNow() {
    busy = "now";
    try {
      await api.ingestNow();
    } catch (e) {
      error = errorText(e);
    } finally {
      busy = null;
      await refresh();
    }
  }

  async function retry(r: IngestOverview["raw"][number]) {
    busy = `r${r.path}`;
    try {
      await api.ingestRetry(r.path);
      error = null;
    } catch (e) {
      error = errorText(e);
    } finally {
      busy = null;
      await refresh();
    }
  }

  function askRemove(e: MouseEvent, r: IngestOverview["raw"][number]) {
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    openConfirm(rect.left, rect.bottom + 4, {
      title: `Remove ${r.name} from Raw?`,
      body: "It goes to the system trash, and Ken does not read it.",
      confirmLabel: "Move to Trash",
      onConfirm: () => void remove(r),
    });
  }

  async function remove(r: IngestOverview["raw"][number]) {
    busy = `x${r.path}`;
    try {
      await api.ingestRemove(r.path);
      error = null;
    } catch (e) {
      error = errorText(e);
    } finally {
      busy = null;
      await refresh();
    }
  }

  async function openIn(projectId: string, path: string) {
    if (app.focused !== projectId) await app.focusMember(projectId);
    app.openInFiles(path);
  }

  async function inLibrary(path: string) {
    if (overview) await openIn(overview.projectId, path);
  }

  /** A risk's or issue's title, from its waiting item. */
  function raidOf(p: IngestProposal): string {
    try {
      return (JSON.parse(p.payload ?? "{}") as { raid?: { title?: string } }).raid?.title ?? p.title;
    } catch {
      return p.title;
    }
  }

  /** A ruling's words, from its waiting item. */
  function rulingOf(p: IngestProposal): string {
    try {
      return (JSON.parse(p.payload ?? "{}") as { append?: { ruling?: string } }).append?.ruling ?? p.title;
    } catch {
      return p.title;
    }
  }

  async function apply(p: IngestProposal) {
    if (!overview) return;
    busy = `p${p.id}`;
    try {
      await api.applyPageProposal(p.id, overview.projectId);
      error = null;
    } catch (e) {
      error = String(e);
    } finally {
      busy = null;
      await reload();
    }
  }

  async function discard(p: IngestProposal) {
    if (!overview) return;
    busy = `p${p.id}`;
    try {
      await api.resolveReviewItem(p.id, overview.projectId);
    } finally {
      busy = null;
      await reload();
    }
  }

  async function seen() {
    if (!card) return;
    busy = "seen";
    try {
      await api.ingestSeen(card.id);
    } catch (e) {
      error = String(e);
    } finally {
      busy = null;
      await reload();
    }
  }

  // The note is written before the pages are: while Claude still writes
  // them, the card says so and it cannot be marked seen yet.
  const cardWriting = $derived(!!card?.writing);
  const pages = $derived(card?.pages ?? []);
  const rulings = $derived((card?.proposals ?? []).filter((p) => p.kind === "ruling"));
  const raid = $derived((card?.proposals ?? []).filter((p) => p.kind === "risk" || p.kind === "issue"));
  const tickets = $derived((card?.proposals ?? []).filter((p) => p.kind === "ticket"));
  const held = $derived((card?.proposals ?? []).filter((p) => p.kind === "page" || p.kind === "new page"));
  const contradictions = $derived((card?.takeaways.contradictions ?? []).filter((c) => !saysNothing(c)));
  const questions = $derived((card?.takeaways.openQuestions ?? []).filter((q) => !saysNothing(q)));
  const decisionsTotal = $derived(rulings.length + raid.length + tickets.length + held.length);
</script>

<div class="ingest">
  <div
    class="left"
    class:dropping
    role="region"
    aria-label="Drop files to ingest"
    ondragover={(e) => {
      e.preventDefault();
      dropping = true;
    }}
    ondragleave={() => (dropping = false)}
    ondrop={onDrop}
  >
    <div class="title"><ScreenHeader title="Ingest" /></div>

    <div class="adds">
      <button class="add drop" onclick={chooseFiles}>
        <FilePlus size={16} strokeWidth={1.75} aria-hidden="true" />
        <span class="alabel">Add a file</span>
        <span class="asub">or drop it here</span>
      </button>
      <button class="add" class:on={source === "record"} onclick={() => toggleSource("record")}>
        <span class="recdot" class:live={record.recording}></span>
        <span class="alabel">Record a meeting</span>
      </button>
      <button class="add" class:on={source === "note"} onclick={() => toggleSource("note")}>
        <NotebookPen size={16} strokeWidth={1.75} aria-hidden="true" />
        <span class="alabel">Write a note</span>
      </button>
      <button class="add" class:on={source === "chat"} onclick={() => toggleSource("chat")}>
        <MessagesSquare size={16} strokeWidth={1.75} aria-hidden="true" />
        <span class="alabel">From a chat</span>
      </button>
    </div>

    {#if source === "record"}
      <div class="panel"><RecordPanel /></div>
    {:else if source === "note"}
      <div class="panel note-form">
        <input class="input" bind:value={noteTitle} placeholder="Title (optional)" aria-label="Title" />
        <textarea class="textarea" bind:value={noteText} rows="6" placeholder="What was said, decided or found" aria-label="Note"></textarea>
        <div class="panel-actions">
          <button class="btn btn-small btn-primary" disabled={!noteText.trim() || busy !== null} onclick={addNote}>Add the note</button>
        </div>
      </div>
    {:else if source === "chat"}
      <div class="panel"><ChatSource onAdded={chatAdded} /></div>
    {/if}
    {#if adding > 0}<p class="note">Adding {adding} {adding === 1 ? "file" : "files"}…</p>{/if}
    {#if error}<p class="warn">{error}</p>{/if}
    {#if loadError}<p class="warn">{loadError}</p>{/if}
    {#if overview?.stopped}<p class="warn">The last read stopped: {overview.stopped}</p>{/if}
    {#if overview && !overview.claudeFound}
      <p class="warn">Claude Code not found</p>
    {/if}

    {#if !overview && !error}
      <Loading label="Reading the inbox…" lines={3} />
    {/if}
    {#if overview}
      {@const inProgress = overview.raw.filter((r) => r.state !== "read")}
      <div class="t-label lhead">
        <span>In progress · {inProgress.length}</span>
        {#if canReadNow(overview)}
          <button class="btn btn-link btn-small" disabled={busy !== null} onclick={readNow}>Read now</button>
        {/if}
      </div>
      {#each inProgress as r (r.path)}
        <button class="qrow" class:sel={selectedRaw === r.path} title={r.path} onclick={() => selectRaw(r)}>
          <span class="qdot {r.state}"></span>
          <span class="qname">{r.name}</span>
          <span class="qstat">{rawStatus(r)}</span>
        </button>
      {:else}
        <EmptyState />
      {/each}

      {@const readRaw = overview.raw.filter((r) => r.state === "read" && r.cardId !== null)}
      <div class="t-label lhead"><span>Ingested · {readRaw.length + ingested.length}</span></div>
      {#each readRaw as r (r.path)}
        <button class="srow" class:sel={r.cardId === selected} onclick={() => r.cardId !== null && selectCard(r.cardId)}>
          <span class="sicon"><FileText size={15} strokeWidth={1.75} aria-hidden="true" /></span>
          <span class="stext">
            <span class="stitle">{r.name}</span>
            <span class="sline">{r.kind || "source"}</span>
          </span>
          {#if r.waiting > 0}<span class="count">{r.waiting}</span>{/if}
        </button>
      {/each}
      {#each ingested as s (s.id)}
        <button class="srow" class:sel={s.id === selected} class:seen={!s.open} onclick={() => selectCard(s.id)}>
          <span class="sicon"><FileText size={15} strokeWidth={1.75} aria-hidden="true" /></span>
          <span class="stext">
            <span class="stitle">{@html renderTitle(s.title)}</span>
            <span class="sline">{s.kind || "source"} · {timeAgo(s.at)}</span>
          </span>
          {#if s.waiting > 0}<span class="count">{s.waiting}</span>{/if}
        </button>
      {/each}
      {#if readRaw.length === 0 && ingested.length === 0}
        <EmptyState />
      {/if}
    {/if}
  </div>

  <main class="right">
    <div class="rinner">
    {#if card}
      <div class="chead">
        <div class="ctitle">
          <div class="chips">
            <!-- The kind is the chip; the source's shape ("short minutes, one
                 page, four sections: …") is its tooltip. -->
            {#if card.takeaways.kind || card.takeaways.length}<span class="chip" title={card.takeaways.length || undefined}>{card.takeaways.kind || "source"}</span>{/if}
            {#if card.takeaways.present.length > 0}<span class="chip">{card.takeaways.present.join(" · ")}</span>{/if}
            {#if !card.open}<span class="chip">seen</span>{/if}
            {#if card.commit}<span class="chip mono">{card.commit}</span>{/if}
          </div>
          <h2>{@html renderTitle(card.takeaways.title || (card.note.split("/").pop() ?? ""))}</h2>
        </div>
        <div class="cacts">
          <button class="btn btn-ghost" title="Show the source file in your file manager" onclick={() => card && openSource(card.source)}>Open source</button>
          <button class="btn" onclick={() => card && inLibrary(card.note)}>Open the note</button>
          {#if card.open}
            <button class="btn btn-primary" disabled={busy !== null || cardWriting} onclick={seen}>Mark as seen</button>
          {/if}
        </div>
      </div>

      {#if cardWriting}
        {@const steps = isRecording(card.source, card.takeaways.kind) ? RECORDING_STEPS : DOCUMENT_STEPS}
        {@const at = steps.indexOf("Pages written")}
        <div class="steps">
          {#each steps as s, i (s)}
            <span class="step" class:done={i < at} class:now={i === at}>
              {#if i < at}<CircleCheck size={14} strokeWidth={1.75} aria-hidden="true" />{:else}<Circle size={14} strokeWidth={1.75} aria-hidden="true" />{/if}{s}
            </span>
          {/each}
        </div>
      {/if}

      {#if decisionsTotal > 0}
        <section class="decisions">
          <div class="shead"><span class="adot"></span><span class="t-label">to review · {decisionsTotal}</span></div>
          <div class="dgrid">
            {#each rulings as p (p.id)}
              <div class="dcard">
                <div class="dkind"><Gavel size={14} strokeWidth={1.75} aria-hidden="true" />Ruling{p.who ? ` · ${p.who}` : ""}</div>
                <div class="dshort">“{@html renderTitle(rulingOf(p))}”</div>
                {#if openDiff === p.id}<ProposalDetail payload={p.payload} />{/if}
                <div class="dacts">
                  <button class="btn btn-small btn-primary" disabled={busy !== null} onclick={() => apply(p)}>Add to the decisions log</button>
                  <button class="btn btn-small" onclick={() => (openDiff = openDiff === p.id ? null : p.id)}>{openDiff === p.id ? "Close" : "Read it"}</button>
                  <button class="btn btn-small btn-ghost" disabled={busy !== null} onclick={() => discard(p)}>Discard</button>
                </div>
              </div>
            {/each}
            {#each raid as p (p.id)}
              <div class="dcard">
                <div class="dkind">
                  {#if p.kind === "risk"}<TriangleAlert size={14} strokeWidth={1.75} aria-hidden="true" />Risk{:else}<CircleAlert size={14} strokeWidth={1.75} aria-hidden="true" />Issue{/if}{p.who ? ` · ${p.who}` : ""}
                </div>
                <div class="dshort">{@html renderTitle(raidOf(p))}</div>
                {#if raidDetail(p.payload)}<div class="dquote">{@html renderTitle(raidDetail(p.payload))}</div>{/if}
                {#if openDiff === p.id}<ProposalDetail payload={p.payload} />{/if}
                <div class="dacts">
                  <button class="btn btn-small btn-primary" disabled={busy !== null} onclick={() => apply(p)}>Add to the register</button>
                  <button class="btn btn-small" onclick={() => (openDiff = openDiff === p.id ? null : p.id)}>{openDiff === p.id ? "Close" : "Read it"}</button>
                  <button class="btn btn-small btn-ghost" disabled={busy !== null} onclick={() => discard(p)}>Discard</button>
                </div>
              </div>
            {/each}
            {#each tickets as p (p.id)}
              <div class="dcard">
                <div class="dkind"><Ticket size={14} strokeWidth={1.75} aria-hidden="true" />Ticket</div>
                <div class="dshort">{@html renderTitle(p.title)}</div>
                {#if openDiff === p.id}<ProposalDetail payload={p.payload} />{/if}
                <div class="dacts">
                  <button class="btn btn-small btn-primary" disabled={busy !== null} onclick={() => apply(p)}>Add to tickets</button>
                  <button class="btn btn-small" onclick={() => (openDiff = openDiff === p.id ? null : p.id)}>{openDiff === p.id ? "Close" : "Read it"}</button>
                  <button class="btn btn-small btn-ghost" disabled={busy !== null} onclick={() => discard(p)}>Discard</button>
                </div>
              </div>
            {/each}
            {#each held as p (p.id)}
              <div class="dcard wide">
                <div class="dkind"><FilePen size={14} strokeWidth={1.75} aria-hidden="true" />Held page edit</div>
                <div class="dshort">{pageName(p.page)}</div>
                <ProposalDetail payload={p.payload} />
                <div class="dacts">
                  <button class="btn btn-small btn-primary" disabled={busy !== null} onclick={() => apply(p)}>Apply</button>
                  <button class="btn btn-small" disabled={busy !== null} onclick={() => discard(p)}>Discard</button>
                </div>
              </div>
            {/each}
          </div>
        </section>
      {/if}

      <div class="cols">
        <section class="summary">
          <span class="t-label">summary</span>
          {#if card.takeaways.summary}
            <div class="lead">{@html renderMarkdown(card.takeaways.summary)}</div>
          {/if}
          {#if card.takeaways.keyTakeaways.length > 0}
            <ul class="takeaways">
              {#each card.takeaways.keyTakeaways as k}<li>{@html renderMarkdown(k)}</li>{/each}
            </ul>
          {/if}
          {#if !saysNothing(card.takeaways.overturns)}
            <div class="well">
              <span class="t-label">overturns</span>
              <div class="md">{@html renderMarkdown(card.takeaways.overturns)}</div>
            </div>
          {/if}
          {#if contradictions.length > 0}
            <div class="well danger">
              <span class="t-label">contradictions · {contradictions.length}</span>
              <ul>
                {#each contradictions as c}<li>{@html renderMarkdown(c)}</li>{/each}
              </ul>
            </div>
          {/if}
          {#if questions.length > 0}
            <div class="well">
              <span class="t-label">open questions · {questions.length}</span>
              <ul>
                {#each questions as q}<li>{@html renderMarkdown(q)}</li>{/each}
              </ul>
            </div>
          {/if}
        </section>

        <div class="side">
          <section>
            <div class="shead"><span class="t-label">pages updated · {pages.length}</span></div>
            {#each pages as w (w.path)}
              <div class="wrow">
                <span class="wtag">{w.new ? "new page" : "page"}</span>
                <span class="wtext">
                  <span class="wtarget">{pageName(w.path)}</span>
                  <span class="t-small">{@html renderTitle(w.label)}</span>
                </span>
                <span class="wacts">
                  <button class="btn btn-small" onclick={() => inLibrary(w.path)}>Open</button>
                </span>
              </div>
            {:else}
              <EmptyState />
            {/each}
          </section>
        </div>
      </div>

      {#if card.commitError}<p class="warn">not committed: {card.commitError}</p>{/if}
    {:else if rawSelected}
      {@const pct = rawSelected.detail?.match(/(\d+)%/)?.[1]}
      {@const acts = rawActions(rawSelected.state)}
      <div class="chead">
        <div class="ctitle">
          <div class="chips">
            {#if rawSelected.kind || rawSelected.length}<span class="chip" title={rawSelected.length || undefined}>{rawSelected.kind || "source"}</span>{/if}
          </div>
          <h2>{rawSelected.name}</h2>
        </div>
        <div class="cacts">
          <button class="btn btn-ghost" title="Show the source file in your file manager" onclick={() => rawSelected && openSource(rawSelected.path)}>Open source</button>
          {#if acts.includes("remove")}
            <button class="btn" disabled={busy !== null} onclick={(e) => rawSelected && askRemove(e, rawSelected)}>Remove</button>
          {/if}
          {#if acts.includes("retry")}
            <button class="btn btn-primary" disabled={busy !== null} onclick={() => rawSelected && retry(rawSelected)}>Try again</button>
          {/if}
        </div>
      </div>
      <div class="steps">
        {#each stepsFor(rawSelected) as s, i (s)}
          {@const at = stepAt(rawSelected)}
          {@const failed = rawSelected.state === "failed" && i === at}
          <span class="step" class:done={i < at} class:now={i === at && !failed} class:failed>
            {#if i < at}<CircleCheck size={14} strokeWidth={1.75} aria-hidden="true" />{:else if failed}<CircleX size={14} strokeWidth={1.75} aria-hidden="true" />{:else}<Circle size={14} strokeWidth={1.75} aria-hidden="true" />{/if}{s}{#if i === at && pct && !failed}<span class="pct">{pct}%</span>{/if}
          </span>
        {/each}
      </div>
      {#if (rawSelected.state === "failed" || rawSelected.state === "waiting") && rawSelected.detail}
        <p class="reason" class:bad={rawSelected.state === "failed"}>{rawSelected.detail}</p>
      {/if}
    {:else}
      <EmptyState title="No source selected" />
    {/if}
    </div>
  </main>
</div>

<style>
  .ingest {
    flex: 1;
    min-width: 0;
    display: flex;
    min-height: 0;
  }
  .left {
    width: clamp(260px, 21vw, 320px);
    flex: none;
    border-right: 1px solid var(--line-soft);
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 24px 14px;
    overflow-y: auto;
    box-sizing: border-box;
  }
  .left.dropping {
    background: var(--accent-soft);
  }
  .title {
    margin: 0 0 4px;
    padding: 0 4px;
  }
  .adds {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .add {
    display: flex;
    align-items: center;
    gap: 10px;
    height: 40px;
    padding: 0 12px;
    border-radius: 9px;
    border: 1px solid var(--line);
    background: var(--surface);
    text-align: left;
    color: var(--ink);
    white-space: nowrap;
  }
  .add:hover,
  .add.on {
    border-color: var(--line-strong);
  }
  .add.on {
    background: var(--accent-soft);
  }
  .add :global(svg) {
    color: var(--accent-ink);
    flex: none;
  }
  .add.drop {
    border: 1.5px dashed var(--line-strong);
  }
  .add.drop:hover {
    border-color: var(--accent);
  }
  .alabel {
    font-size: 13.5px;
    font-weight: 600;
  }
  .asub {
    font-size: 12px;
    color: var(--ink-3);
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .recdot {
    width: 10px;
    height: 10px;
    flex: none;
    border-radius: 5px;
    background: var(--danger);
    margin: 0 3px;
  }
  .recdot.live {
    animation: pulse 1.2s ease-in-out infinite;
  }
  @keyframes pulse {
    0%,
    100% {
      opacity: 0.35;
    }
    50% {
      opacity: 1;
    }
  }
  .panel {
    border: 1px solid var(--line);
    border-radius: var(--radius-card);
    background: var(--surface);
    padding: 12px;
  }
  .note-form {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .panel-actions {
    display: flex;
    justify-content: flex-end;
  }
  .note {
    margin: 0;
    font-size: 12.5px;
    color: var(--ink-3);
    line-height: 1.5;
  }
  .warn {
    margin: 0;
    font-size: 12.5px;
    color: var(--danger);
    line-height: 1.5;
  }
  .lhead {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 6px 2px;
  }
  .qrow {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 10px;
    border-radius: 9px;
    border: none;
    background: transparent;
    text-align: left;
    color: var(--ink);
    width: 100%;
  }
  .qrow:hover {
    background: var(--line-soft);
  }
  .qrow.sel {
    background: var(--accent-soft);
  }
  .qdot {
    width: 8px;
    height: 8px;
    border-radius: 4px;
    flex: none;
    background: var(--accent);
  }
  .qdot.failed {
    background: var(--danger);
  }
  .qdot.waiting {
    background: var(--attn);
  }
  .qdot.queued {
    background: var(--line-strong);
  }
  .qname {
    flex: 1;
    min-width: 0;
    font-size: 13.5px;
    font-weight: 600;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .qstat {
    font-size: 12px;
    color: var(--ink-3);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .reason {
    margin: 0;
    font-size: 12px;
    color: var(--attn-ink);
    line-height: 1.45;
  }
  .reason.bad {
    color: var(--danger);
  }
  .srow {
    display: flex;
    gap: 10px;
    padding: 10px;
    border-radius: 9px;
    border: none;
    background: transparent;
    text-align: left;
    color: var(--ink);
    align-items: flex-start;
  }
  .srow:hover {
    background: var(--line-soft);
  }
  .srow.sel {
    background: var(--accent-soft);
  }
  .srow.seen .stitle {
    color: var(--ink-3);
  }
  .sicon {
    color: var(--ink-2);
    margin-top: 2px;
    flex: none;
  }
  .stext {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .stitle {
    font-size: 13.5px;
    font-weight: 600;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .sline {
    font-size: 12px;
    color: var(--ink-3);
  }

  .right {
    flex: 1;
    min-width: 0;
    overflow-y: auto;
    padding: clamp(24px, 3vw, 40px) clamp(24px, 4vw, 56px) 56px;
    display: flex;
    flex-direction: column;
    align-items: center;
  }
  .rinner {
    width: 100%;
    max-width: 1040px;
    display: flex;
    flex-direction: column;
    gap: 28px;
  }
  .chead {
    display: flex;
    align-items: flex-end;
    gap: 20px;
    flex-wrap: wrap;
  }
  .ctitle {
    flex: 1;
    min-width: 280px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .chips {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
  }
  h2 {
    margin: 0;
    font-family: var(--font-serif);
    font-size: 32px;
    font-weight: 500;
    line-height: 1.15;
  }
  .cacts {
    display: flex;
    gap: 8px;
    flex: none;
  }
  .steps {
    display: flex;
    flex-wrap: wrap;
    gap: 8px 24px;
  }
  .step {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 12.5px;
    color: var(--ink-3);
    white-space: nowrap;
  }
  .step.done {
    color: var(--ok);
  }
  .step.now {
    color: var(--accent-ink);
    font-weight: 600;
  }
  .pct {
    margin-left: 4px;
    font-variant-numeric: tabular-nums;
  }
  .step.failed {
    color: var(--danger);
    font-weight: 600;
  }
  .shead {
    display: flex;
    align-items: center;
    gap: 8px;
    padding-bottom: 6px;
  }
  .adot {
    width: 8px;
    height: 8px;
    border-radius: 4px;
    background: var(--attn);
  }
  .decisions {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .dgrid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
    gap: 12px;
  }
  .dcard {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 14px 16px;
    border-radius: var(--radius-card);
    background: var(--surface);
    border: 1px solid var(--line);
  }
  .dcard.wide {
    grid-column: 1 / -1;
  }
  .dkind {
    display: flex;
    align-items: center;
    gap: 7px;
    font-size: 11.5px;
    font-weight: 600;
    color: var(--attn-ink);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  .dshort {
    font-size: 14.5px;
    font-weight: 600;
    line-height: 1.35;
  }
  .dquote {
    padding-left: 10px;
    border-left: 2px solid var(--line);
    font-size: 13px;
    color: var(--ink-2);
    line-height: 1.5;
  }
  .dacts {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
    margin-top: auto;
    padding-top: 4px;
  }
  .cols {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 380px), 1fr));
    gap: 32px;
    align-items: start;
  }
  .summary {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .lead {
    font-family: var(--font-serif);
    font-size: 18px;
    line-height: 1.6;
  }
  .lead :global(p) {
    margin: 0 0 8px;
  }
  .takeaways {
    margin: 0;
    padding-left: 20px;
    font-size: 14.5px;
    line-height: 1.65;
  }
  .well {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 12px 14px;
    border-radius: 10px;
    background: var(--panel);
    font-size: 14px;
    line-height: 1.55;
  }
  .well.danger {
    background: var(--danger-soft);
  }
  .well ul {
    margin: 0;
    padding-left: 18px;
  }
  .md :global(p) {
    margin: 0;
  }
  .side {
    display: flex;
    flex-direction: column;
    gap: 28px;
  }
  .side section {
    display: flex;
    flex-direction: column;
  }
  .wrow {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 9px 0;
    border-top: 1px solid var(--line-soft);
  }
  .wtag {
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--ink-3);
    min-width: 54px;
  }
  .wtext {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .wtarget {
    font-size: 14px;
    font-weight: 500;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .wacts {
    display: flex;
    gap: 4px;
    flex: none;
  }
</style>

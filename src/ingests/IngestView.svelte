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
  import Ticket from "@lucide/svelte/icons/ticket";
  import FilePen from "@lucide/svelte/icons/file-pen";
  // Ingest, the inbox that empties (the Wright white box, frame 8b). Four
  // ways to add a source: drop or choose files, record, write a note, or take
  // a chat. Each lands in the team library's Research/Ingestion/Raw/ and is
  // read at once into a dated note in Ingested/. What follows from the note
  // is written then and there, each write citing the note: page edits and
  // new pages, ideas, escalations, my next steps. The card is read afterwards, not confirmed: each write has
  // Open and Undo. Only four things wait: a ruling for its decider, a change
  // to Ways-of-Working as a ticket, an edit staging held, an action as a
  // ticket. Seen files the source beside its note; Undo all takes it back.
  import { onMount, untrack } from "svelte";
  import { open as openDialog } from "@tauri-apps/plugin-dialog";
  import { api, type IngestCard, type IngestOverview, type IngestProposal, type IngestWrite } from "../lib/api";
  import { app } from "../lib/app.svelte";
  import { scope } from "../lib/scope.svelte";
  import { record } from "../lib/record.svelte";
  import { rail } from "../lib/rail.svelte";
  import { renderMarkdown } from "../lib/markdown";
  import { timeAgo } from "../lib/format";
  import { errorText } from "../lib/toast.svelte";
  import { claudeInstallHelp } from "../lib/platform";
  import { openConfirm } from "../lib/ui/ConfirmMenu.svelte";
  import {
    canReadNow,
    ingestBusy,
    pageName,
    rawActions,
    rawStateLabel,
    stagingLine,
    writtenLine,
  } from "../lib/ingestCard";
  import ProposalDetail from "./ProposalDetail.svelte";
  import RecordPanel from "./RecordPanel.svelte";
  import ChatSource from "./ChatSource.svelte";
  import { singleFlight } from "../lib/refresh";

  let overview = $state<IngestOverview | null>(null);
  let error = $state<string | null>(null);
  /** Why the list could not be read; kept apart from an action's error. */
  let loadError = $state<string | null>(null);
  let notice = $state<string | null>(null);
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
  function rawPct(r: IngestOverview["raw"][number]): number {
    const m = r.detail?.match(/(\d+)%/);
    if (r.state === "read" || r.state === "failed" || r.state === "waiting") return 100;
    if (r.state === "transcribing") return m ? Math.round(Number(m[1]) * 0.6) : 10;
    if (r.state === "reading") return m ? 60 + Math.round(Number(m[1]) * 0.4) : 70;
    return 4;
  }
  function rawStatus(r: IngestOverview["raw"][number]): string {
    switch (r.state) {
      case "queued":
        return "Queued";
      case "transcribing": {
        const m = r.detail?.match(/(\d+)%/);
        return m ? `Transcribing ${m[1]}%` : "Transcribing";
      }
      case "reading":
        return "Reading";
      case "waiting":
        return "Waits for a transcript";
      case "failed":
        return "Couldn't read";
      default:
        return "Read";
    }
  }
  let noteTitle = $state("");
  let noteText = $state("");

  const team = $derived(scope.team);

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
    const t = team;
    try {
      const next = await api.ingestOverview(t);
      if (t !== team) return;
      overview = next;
      rail.setIngest(next.waiting ?? 0);
      loadError = null;
      if (selectedRaw) {
        // A source picked while it is read stays picked; once it has its
        // card, the card is shown.
        const r = next.raw.find((x) => x.path === selectedRaw);
        if (r?.cardId != null) selectCard(r.cardId);
        else if (!r) selectedRaw = null;
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
      card = await api.ingestCard(team, id);
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
    void team;
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
      await api.ingestAdd(team, paths);
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
        await api.ingestAddBytes(team, f);
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
      await api.ingestAddText(team, noteTitle.trim() || null, text);
      noteTitle = "";
      noteText = "";
      notice = "The note is in Raw. Ken reads it now.";
      error = null;
    } catch (e) {
      error = errorText(e);
    } finally {
      busy = null;
      await refresh();
    }
  }

  async function chatAdded() {
    notice = "The chat is in Raw. Ken reads it now.";
    error = null;
    await refresh();
  }

  async function readNow() {
    busy = "now";
    try {
      const started = await api.ingestNow(team);
      notice = started ? "Reading what is queued." : "A read is already running; the queue is next.";
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
      const started = await api.ingestRetry(team, r.path);
      notice = started ? `Reading ${r.name} again.` : `${r.name} is read on the next pass.`;
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
      await api.ingestRemove(team, r.path);
      notice = `${r.name} is in the trash.`;
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

  async function openWrite(w: IngestWrite) {
    if (w.kind === "task") {
      app.screen = "home";
      return;
    }
    if (!w.projectId) {
      error = `${w.path} is in a repo that is not open in this workspace.`;
      return;
    }
    await openIn(w.projectId, w.path);
  }

  async function undoWrite(w: IngestWrite) {
    if (!card) return;
    busy = `w${w.index}`;
    notice = null;
    try {
      await api.ingestUndoWrite(card.id, w.index, team);
      error = null;
    } catch (e) {
      error = String(e);
    } finally {
      busy = null;
      await reload();
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
      await api.ingestFile(card.id, team);
    } catch (e) {
      error = String(e);
    } finally {
      busy = null;
      await reload();
    }
  }

  async function undoAll() {
    if (!card) return;
    busy = "undo";
    try {
      const report = await api.ingestUndo(card.id, team);
      const kept = report.kept.length > 0 ? ` Kept, changed since: ${report.kept.join(", ")}.` : "";
      const note = report.noteRemoved ? "" : " The note was edited, so it stays.";
      notice = `Undone.${kept}${note}`;
    } catch (e) {
      error = String(e);
    } finally {
      busy = null;
      await reload();
    }
  }

  async function readAgain() {
    if (!card) return;
    busy = "again";
    try {
      const started = await api.ingestReadAgain(card.id, team);
      notice = started ? "Reading it again." : "It is read on the next pass.";
      const source = card.source;
      selected = null;
      selectedRaw = source;
    } catch (e) {
      error = String(e);
    } finally {
      busy = null;
      await refresh();
    }
  }

  // The note is written before the pages are: while Claude still writes
  // them, the card says so and nothing on it can be undone or filed yet (an
  // Undo then would be overwritten by the read's own record of the card).
  const cardWriting = $derived(
    !!card && !!overview && overview.raw.some((r) => r.cardId === card?.id && (r.state === "reading" || r.state === "transcribing")),
  );
  const written = $derived(card?.writes ?? []);
  const live = $derived(written.filter((w) => !w.undone));
  const rulings = $derived((card?.proposals ?? []).filter((p) => p.kind === "ruling"));
  const tickets = $derived((card?.proposals ?? []).filter((p) => p.kind === "ticket"));
  const held = $derived((card?.proposals ?? []).filter((p) => p.kind === "page" || p.kind === "new page"));
  const pageWrites = $derived(written.filter((w) => w.kind === "edit" || w.kind === "page").length);
  const contradictions = $derived((card?.takeaways.contradictions ?? []).filter((c) => !saysNothing(c)));
  const decisionsTotal = $derived(rulings.length + tickets.length + held.length);
  const decisionsLeft = $derived(decisionsTotal);

  const writeTag: Record<IngestWrite["kind"], string> = {
    edit: "page",
    page: "new page",
    idea: "idea",
    escalation: "escalation",
    task: "my day",
  };

  function rawSub(r: IngestOverview["raw"][number]): string {
    const bits = [r.kind, r.length, r.present.length > 0 ? `${r.present.length} present` : "", rawStateLabel(r.state)];
    if (r.state === "read") bits.push(writtenLine(r.written, r.waiting));
    return bits.filter((b) => b).join(" · ");
  }
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
    <h1>Ingest</h1>

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
      <div class="panel"><RecordPanel {team} /></div>
    {:else if source === "note"}
      <div class="panel note-form">
        <input class="input" bind:value={noteTitle} placeholder="Title (optional)" aria-label="Title" />
        <textarea class="textarea" bind:value={noteText} rows="6" placeholder="What was said, decided or found" aria-label="Note"></textarea>
        <div class="panel-actions">
          <button class="btn btn-small btn-primary" disabled={!noteText.trim() || busy !== null} onclick={addNote}>Add the note</button>
        </div>
      </div>
    {:else if source === "chat"}
      <div class="panel"><ChatSource {team} onAdded={chatAdded} /></div>
    {/if}
    {#if adding > 0}<p class="note">Adding {adding} {adding === 1 ? "file" : "files"}…</p>{/if}
    {#if error}<p class="warn">{error}</p>{/if}
    {#if notice}<p class="note">{notice}</p>{/if}
    {#if loadError}<p class="warn">{loadError}</p>{/if}
    {#if overview?.stopped}<p class="warn">The last read stopped: {overview.stopped}</p>{/if}
    {#if overview && !overview.claudeFound}
      <p class="warn">Reading needs Claude Code. {claudeInstallHelp()}</p>
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
        {@const actions = rawActions(r.state)}
        {@const pct = rawPct(r)}
        <div class="qitem" class:sel={selectedRaw === r.path}>
          <button class="qrow" title={r.path} onclick={() => selectRaw(r)}>
            <span class="qline">
              <span class="qdot {r.state}"></span>
              <span class="qname">{r.name}</span>
              <span class="qstat">{rawStatus(r)}</span>
            </span>
            <span class="k-progress thin"><span style="width: {pct}%" class:bad={r.state === "failed"} class:wait={r.state === "waiting"}></span></span>
          </button>
          {#if (r.state === "failed" || r.state === "waiting") && r.detail}
            <p class="reason" class:bad={r.state === "failed"}>{r.detail}</p>
          {/if}
          {#if actions.length > 0}
            <div class="raw-actions">
              {#if actions.includes("retry")}
                <button class="btn btn-small btn-ghost" disabled={busy !== null} onclick={() => retry(r)}>Try again</button>
              {/if}
              {#if actions.includes("remove")}
                <button class="btn btn-small btn-ghost" disabled={busy !== null} onclick={(e) => askRemove(e, r)}>Remove</button>
              {/if}
            </div>
          {/if}
        </div>
      {:else}
        <p class="note">Nothing waiting to be read.</p>
      {/each}

      {@const readRaw = overview.raw.filter((r) => r.state === "read" && r.cardId !== null)}
      <div class="t-label lhead"><span>Ingested</span></div>
      {#each readRaw as r (r.path)}
        <button class="srow" class:sel={r.cardId === selected} onclick={() => r.cardId !== null && selectCard(r.cardId)}>
          <span class="sicon"><FileText size={15} strokeWidth={1.75} aria-hidden="true" /></span>
          <span class="stext">
            <span class="stitle">{r.name}</span>
            <span class="sline">{rawSub(r)}</span>
          </span>
          {#if r.waiting > 0}<span class="count">{r.waiting}</span>{/if}
        </button>
      {/each}
      {#each ingested as s (s.id)}
        <button class="srow" class:sel={s.id === selected} onclick={() => selectCard(s.id)}>
          <span class="sicon"><FileText size={15} strokeWidth={1.75} aria-hidden="true" /></span>
          <span class="stext">
            <span class="stitle">{s.title}</span>
            <span class="sline">{s.kind || "source"} · {timeAgo(s.at)}{#if !s.open} · seen{/if}</span>
          </span>
          {#if s.waiting > 0}<span class="count">{s.waiting}</span>{/if}
        </button>
      {/each}
      {#if readRaw.length === 0 && ingested.length === 0}
        <p class="note">Nothing ingested yet.</p>
      {/if}
    {/if}
  </div>

  <main class="right">
    <div class="rinner">
    {#if card}
      <div class="chead">
        <div class="ctitle">
          <div class="chips">
            {#if card.takeaways.kind}<span class="chip">{card.takeaways.kind}</span>{/if}
            {#if card.takeaways.length}<span class="chip">{card.takeaways.length}</span>{/if}
            {#if card.takeaways.present.length > 0}<span class="chip">{card.takeaways.present.join(" · ")}</span>{/if}
            {#if !card.open}<span class="chip">seen</span>{/if}
          </div>
          <h2>{card.takeaways.title || card.note.split("/").pop()}</h2>
          <div class="t-small">{card.note}</div>
        </div>
        {#if !card.undone}
          <div class="cacts">
            <button class="btn btn-ghost" title="Show the source file in your file manager" onclick={() => card && openSource(card.source)}>Open source</button>
            <button class="btn" onclick={() => card && inLibrary(card.note)}>Open the note</button>
            {#if card.open}
              <button class="btn btn-primary" disabled={busy !== null || cardWriting} onclick={seen}>Mark as seen</button>
            {/if}
          </div>
        {/if}
      </div>

      {#if card.undone}
        <div class="k-empty soft">
          <span class="head">Undone</span>
          <span class="body">What was written from this source is taken back, and the source is in Raw. Ken does not read it again until you ask.</span>
          <div><button class="btn btn-primary" disabled={busy !== null} onclick={readAgain}>Read it again</button></div>
        </div>
      {:else}
        <div class="progress-card">
          {#if cardWriting}
            <div class="pline"><b>Writing the pages</b><span class="grow"></span><span class="t-small">The note is written; Claude is updating the pages it changes</span></div>
            <div class="k-progress fat"><span style="width: 80%"></span></div>
          {:else}
            <div class="pline"><b>Read and filed</b><span class="grow"></span><span class="t-small">Done</span></div>
            <div class="k-progress fat"><span style="width: 100%" class="okbar"></span></div>
          {/if}
          <div class="steps">
            {#each isRecording(card.source, card.takeaways.kind) ? RECORDING_STEPS : DOCUMENT_STEPS as s (s)}<span class="step done"><CircleCheck size={14} strokeWidth={1.75} aria-hidden="true" />{s}</span>{/each}
          </div>
        </div>

        {#if decisionsTotal > 0}
          <section class="decisions">
            <div class="shead">
              <span class="adot"></span>
              <h3>Needs your decision</h3>
              <span class="t-small">{decisionsLeft} of {decisionsTotal} left · also in your Inbox</span>
            </div>
            <div class="dgrid">
              {#each rulings as p (p.id)}
                <div class="dcard">
                  <div class="dkind"><Gavel size={14} strokeWidth={1.75} aria-hidden="true" />Ruling{p.decider ? ` · ${p.decider}` : ""}</div>
                  <div class="dshort">“{rulingOf(p)}”</div>
                  <div class="ddetail">Goes into the decisions log when {p.decider ?? "its decider"} accepts it.</div>
                  <div class="dacts">
                    <button class="btn btn-small btn-primary" disabled={busy !== null || !p.canAccept} title={p.canAccept ? "Write it to the decisions log" : `Only ${p.decider} can accept a ruling`} onclick={() => apply(p)}>{p.decider && p.canAccept ? `Accept as ${p.decider}` : "Accept"}</button>
                    <button class="btn btn-small" disabled={busy !== null} onclick={() => discard(p)}>Drop</button>
                  </div>
                </div>
              {/each}
              {#each tickets as p (p.id)}
                <div class="dcard">
                  <div class="dkind"><Ticket size={14} strokeWidth={1.75} aria-hidden="true" />Ticket</div>
                  <div class="dshort">{p.title}</div>
                  <div class="ddetail">Accepting writes the ticket in the team's tickets folder.</div>
                  {#if openDiff === p.id}<ProposalDetail payload={p.payload} />{/if}
                  <div class="dacts">
                    <button class="btn btn-small btn-primary" disabled={busy !== null} onclick={() => apply(p)}>Accept</button>
                    <button class="btn btn-small" onclick={() => (openDiff = openDiff === p.id ? null : p.id)}>{openDiff === p.id ? "Close" : "Read it"}</button>
                    <button class="btn btn-small btn-ghost" disabled={busy !== null} onclick={() => discard(p)}>Discard</button>
                  </div>
                </div>
              {/each}
              {#each held as p (p.id)}
                <div class="dcard wide">
                  <div class="dkind"><FilePen size={14} strokeWidth={1.75} aria-hidden="true" />Held page edit</div>
                  <div class="dshort">{pageName(p.page)}</div>
                  <div class="ddetail">{p.body}</div>
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
            <h3>Summary</h3>
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
                <span class="t-label">What it overturns</span>
                <div class="md">{@html renderMarkdown(card.takeaways.overturns)}</div>
              </div>
            {/if}
            {#if contradictions.length > 0}
              <div class="well danger">
                <span class="t-label">Contradictions</span>
                <ul>
                  {#each contradictions as c}<li>{@html renderMarkdown(c)}</li>{/each}
                </ul>
              </div>
            {/if}
          </section>

          <div class="side">
            {#if card.takeaways.nextSteps.length > 0}
              <section>
                <div class="shead"><h3>Next steps</h3><span class="t-small">{card.takeaways.nextSteps.length}</span></div>
                {#each card.takeaways.nextSteps as n}
                  <div class="nrow">{@html renderMarkdown(n)}</div>
                {/each}
              </section>
            {/if}
            <section>
              <div class="shead"><h3>Pages Ken updated</h3><span class="t-small">{live.length} · each cites the note</span></div>
              {#each written as w (w.index)}
                <div class="wrow" class:undone={w.undone}>
                  <span class="wtag">{writeTag[w.kind]}</span>
                  <span class="wtext">
                    <span class="wtarget">{w.kind === "task" ? w.label : pageName(w.path)}</span>
                    <span class="t-small">{w.kind === "escalation" && w.to ? `to ${w.to}` : w.kind !== "task" && w.label ? w.label : ""}{w.undone ? " · undone" : ""}</span>
                  </span>
                  {#if !w.undone}
                    <span class="wacts">
                      <button class="btn btn-small" onclick={() => openWrite(w)}>Open</button>
                      <button class="btn btn-small btn-ghost" disabled={busy !== null || cardWriting} onclick={() => undoWrite(w)}>Undo</button>
                    </span>
                  {/if}
                </div>
              {:else}
                <p class="note">Nothing was written from this source.</p>
              {/each}
              {#if pageWrites > 0 || held.length > 0}
                <p class="note">{stagingLine(held.length)}</p>
              {/if}
            </section>
            {#if card.listed.length > 0}
              <section>
                <div class="shead"><h3>On this card only</h3><span class="t-small">nowhere to write it</span></div>
                {#each card.listed as l, i (i)}
                  <div class="nrow"><span class="wtag">{l.kind}</span> {l.text}{#if l.to} · {l.to}{/if}</div>
                {/each}
              </section>
            {/if}
          </div>
        </div>

        <div class="foot">
          <span class="t-small grow">Undo all takes back every page edit and puts the source back in Raw.</span>
          <button class="btn btn-danger" disabled={busy !== null || cardWriting} onclick={undoAll}>Undo all</button>
        </div>
      {/if}
    {:else if rawSelected}
      {@const pct = rawPct(rawSelected)}
      {@const acts = rawActions(rawSelected.state)}
      <div class="chead">
        <div class="ctitle">
          <div class="chips">
            {#if rawSelected.kind}<span class="chip">{rawSelected.kind}</span>{/if}
            {#if rawSelected.length}<span class="chip">{rawSelected.length}</span>{/if}
          </div>
          <h2>{rawSelected.name}</h2>
          <div class="t-small">{rawSelected.path}</div>
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
      <div class="progress-card">
        <div class="pline"><b>{rawStatus(rawSelected)}</b><span class="grow"></span>{#if pct > 0 && pct < 100}<span class="t-small">{pct}%</span>{/if}</div>
        <div class="k-progress fat"><span style="width: {pct}%" class:bad={rawSelected.state === "failed"}></span></div>
        <div class="steps">
          {#each stepsFor(rawSelected) as s, i (s)}
            {@const at = stepAt(rawSelected)}
            {@const failed = rawSelected.state === "failed" && i === at}
            <span class="step" class:done={i < at} class:now={i === at && !failed} class:failed>
              {#if i < at}<CircleCheck size={14} strokeWidth={1.75} aria-hidden="true" />{:else if failed}<CircleX size={14} strokeWidth={1.75} aria-hidden="true" />{:else}<Circle size={14} strokeWidth={1.75} aria-hidden="true" />{/if}{s}
            </span>
          {/each}
        </div>
      </div>
      <div class="k-empty soft">
        <span class="head">{rawSelected.state === "failed" ? "Ken couldn't read this" : rawSelected.state === "waiting" ? "This waits for a transcript" : "Ken is still reading"}</span>
        <span class="body">{rawSelected.detail ?? "The summary, decisions, next steps and page updates appear here when it finishes. You can leave this screen."}</span>
      </div>
    {:else}
      <div class="k-empty soft">
        <span class="head">Add a source</span>
        <span class="body">Drop a transcript, a recording, a document or a note. Ken reads it into a dated note, updates the pages it changes, and shows here what it did, with an Undo for each.</span>
      </div>
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
  h1 {
    margin: 0 0 4px;
    padding: 0 4px;
    font-family: var(--font-serif);
    font-size: 28px;
    font-weight: 500;
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
  .qitem {
    border-radius: 9px;
    padding: 2px 0;
  }
  .qitem.sel {
    background: var(--accent-soft);
  }
  .qrow {
    display: flex;
    flex-direction: column;
    gap: 6px;
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
  .qline {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
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
  .k-progress.thin {
    height: 3px;
    width: 100%;
  }
  .k-progress.fat {
    height: 6px;
    border-radius: 3px;
  }
  .k-progress :global(span.bad) {
    background: var(--danger);
  }
  .k-progress :global(span.wait) {
    background: var(--attn);
  }
  .k-progress :global(span.okbar) {
    background: var(--ok);
  }
  .reason {
    margin: 0 10px 4px;
    font-size: 12px;
    color: var(--attn-ink);
    line-height: 1.45;
  }
  .reason.bad {
    color: var(--danger);
  }
  .raw-actions {
    display: flex;
    gap: 4px;
    padding: 0 6px 4px;
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
  .progress-card {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 16px 18px;
    border-radius: var(--radius-card);
    background: var(--surface);
    border: 1px solid var(--line);
  }
  .pline {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 13.5px;
  }
  .pline b {
    font-weight: 600;
  }
  .grow {
    flex: 1;
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
  .step.failed {
    color: var(--danger);
    font-weight: 600;
  }
  h3 {
    margin: 0;
    font-size: 16px;
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
  .ddetail {
    font-size: 13px;
    color: var(--ink-2);
    line-height: 1.5;
    flex: 1;
  }
  .dacts {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
    margin-top: 4px;
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
  .nrow {
    padding: 10px 0;
    border-top: 1px solid var(--line-soft);
    font-size: 14px;
    line-height: 1.45;
  }
  .nrow :global(p) {
    margin: 0;
  }
  .wrow {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 9px 0;
    border-top: 1px solid var(--line-soft);
  }
  .wrow.undone .wtarget {
    text-decoration: line-through;
    color: var(--ink-3);
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
  .foot {
    display: flex;
    align-items: center;
    gap: 12px;
    padding-top: 16px;
    border-top: 1px solid var(--line-soft);
  }
  .soft {
    background: var(--rail);
    padding: 20px;
  }
</style>

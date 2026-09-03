<script lang="ts">
  // ken-pipeline task 4.12, D16: the Ideas surface. Lists tickets sitting in
  // whatever lane `pipelineStore.ideaLaneFor()` resolves per pipeline (see
  // that method's doc comment for why it resolves by lane id rather than
  // `Lane.generative` — that flag marks the PRODUCING lane, not this one).
  // Cards are deliberately lighter than `PipelineCard` (D16: "a few
  // sentences, not a ticket brief" — these are notes, not work items) and
  // offer exactly two actions: Promote (advance along the lane's own
  // `on_pass` edge — the human editorial act) and Dismiss (archive, D16
  // never mentions deleting an idea, only acting on it or not).
  import type { Task } from "../lib/api";
  import { pipelineStore, projectSymbol, type IdeaEntry } from "../lib/pipeline.svelte";
  import { tasksStore } from "../lib/tasks.svelte";
  import Sparkles from "@lucide/svelte/icons/sparkles";
  import Link from "@lucide/svelte/icons/link";
  import ArrowRight from "@lucide/svelte/icons/arrow-right";
  import Archive from "@lucide/svelte/icons/archive";
  import CircleCheck from "@lucide/svelte/icons/circle-check";

  const ideas = $derived(pipelineStore.ideasForView);
  const multiplePipelines = $derived(pipelineStore.pipelines.length > 1);

  let promoting = $state<string | null>(null);
  let report = $state("");
  let busyId = $state<string | null>(null);
  let promoteError = $state<string | null>(null);
  let archivingId = $state<string | null>(null);

  function sourceTicket(id: string): Task | undefined {
    return tasksStore.board.tasks.find((t) => t.id.toLowerCase() === id.toLowerCase());
  }
  function sourceLabel(id: string): string {
    const t = sourceTicket(id);
    return t ? t.title || id : id;
  }
  function openSource(id: string) {
    const t = sourceTicket(id);
    if (t) void tasksStore.openFile(t);
  }

  function targetLaneName(entry: IdeaEntry): string {
    const targetId = entry.lane.onPass;
    if (!targetId) return "nowhere (lane has no on_pass edge)";
    return entry.pipeline.lanes.find((l) => l.id.toLowerCase() === targetId.toLowerCase())?.name ?? targetId;
  }

  function openPromote(id: string) {
    promoting = id;
    report = "";
    promoteError = null;
  }
  function cancelPromote() {
    promoting = null;
    report = "";
    promoteError = null;
  }

  async function confirmPromote(entry: IdeaEntry) {
    busyId = entry.task.id;
    promoteError = null;
    try {
      const dto = await pipelineStore.promoteIdea(entry.task.id, report.trim());
      if (dto.transition.kind === "refused") {
        promoteError = `Could not promote: ${dto.transition.reason.refusal}`;
        return;
      }
      promoting = null;
      report = "";
    } catch (e) {
      promoteError = String(e);
    } finally {
      busyId = null;
    }
  }

  async function dismiss(id: string) {
    archivingId = id;
    try {
      await tasksStore.archiveTask(id);
    } finally {
      archivingId = null;
    }
  }
</script>

<div class="ideas-view">
  <p class="lede">
    Short notes the Documentation lane spotted while finishing other work — a few sentences each, nothing planned yet.
    Promoting one is the deliberate step that turns it into real work; dismissing one just clears it from this list.
  </p>

  {#if ideas.length === 0}
    <div class="empty">
      <Sparkles size={18} strokeWidth={1.5} />
      <p>No ideas right now — that's the normal case, not a gap to fill.</p>
      <p class="sub">
        Proposing an idea is optional for every ticket the Documentation lane finishes; most runs produce none.
      </p>
    </div>
  {:else}
    <div class="grid">
      {#each ideas as entry (entry.task.id)}
        <div class="idea-card">
          <div class="row title-row">
            <span class="symbol" title={entry.task.project || "no project"}>{projectSymbol(entry.task.project)}</span>
            <span class="title">{entry.task.title || "Untitled idea"}</span>
            {#if multiplePipelines}<span class="pipeline-tag">{entry.pipeline.name}</span>{/if}
          </div>

          {#if entry.task.body}
            <p class="body">{entry.task.body}</p>
          {/if}

          {#if pipelineStore.fieldsFor(entry.task.id)?.spawnedBy}
            {@const spawnedBy = pipelineStore.fieldsFor(entry.task.id)?.spawnedBy ?? ""}
            <button class="source-chip" onclick={() => openSource(spawnedBy)} title="Open the ticket this idea came from">
              <Link size={10} strokeWidth={2} />from {sourceLabel(spawnedBy)}
            </button>
          {/if}

          {#if promoting === entry.task.id}
            <div class="promote-panel">
              <div class="promote-target">
                <ArrowRight size={11} strokeWidth={2} />moves to <strong>{targetLaneName(entry)}</strong>
              </div>
              <textarea bind:value={report} rows="2" placeholder="Note for the log (optional)"></textarea>
              <div class="promote-actions">
                <button class="btn btn-small btn-primary" onclick={() => confirmPromote(entry)} disabled={busyId === entry.task.id}>
                  {busyId === entry.task.id ? "Promoting…" : "Confirm promote"}
                </button>
                <button class="btn btn-small btn-ghost" onclick={cancelPromote} disabled={busyId === entry.task.id}>Cancel</button>
              </div>
              {#if promoteError}<div class="error">{promoteError}</div>{/if}
            </div>
          {:else}
            <div class="actions">
              <button class="btn btn-small btn-primary" onclick={() => openPromote(entry.task.id)}>
                <CircleCheck size={11} strokeWidth={2} />Promote…
              </button>
              <button
                class="btn btn-small btn-ghost"
                onclick={() => dismiss(entry.task.id)}
                disabled={archivingId === entry.task.id}
                title="Archive this idea — it can still be found under archive/"
              >
                <Archive size={11} strokeWidth={2} />{archivingId === entry.task.id ? "Dismissing…" : "Dismiss"}
              </button>
            </div>
          {/if}
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .ideas-view {
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .lede {
    margin: 0;
    font-size: 12px;
    color: var(--ink-tertiary);
    max-width: 62ch;
  }
  .empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 6px;
    padding: 48px 20px;
    color: var(--ink-tertiary);
    text-align: center;
  }
  .empty p {
    margin: 0;
    font-size: 13px;
  }
  .empty .sub {
    font-size: 11.5px;
    max-width: 44ch;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(240px, 1fr));
    gap: 10px;
  }
  .idea-card {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 10px 11px;
    border-radius: var(--radius-control);
    border: 1px dashed var(--border);
    background: color-mix(in srgb, var(--surface) 92%, transparent);
  }
  .row {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
  }
  .symbol {
    flex: none;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 18px;
    height: 18px;
    border-radius: 5px;
    font-size: 9px;
    font-weight: 700;
    letter-spacing: 0.02em;
    background: color-mix(in srgb, var(--file-doc) 14%, transparent);
    color: var(--file-doc);
  }
  .title {
    font-size: 12.5px;
    font-weight: 600;
    color: var(--ink);
    line-height: 1.35;
  }
  .pipeline-tag {
    margin-left: auto;
    font-size: 9.5px;
    color: var(--ink-tertiary);
    background: var(--sunken);
    border-radius: 999px;
    padding: 1px 6px;
  }
  .body {
    margin: 0;
    font-size: 11.5px;
    line-height: 1.45;
    color: var(--ink-secondary);
    display: -webkit-box;
    -webkit-line-clamp: 4;
    line-clamp: 4;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .source-chip {
    align-self: flex-start;
    display: inline-flex;
    align-items: center;
    gap: 3px;
    font-size: 10px;
    padding: 2px 6px;
    border-radius: 999px;
    border: 1px solid var(--border);
    background: var(--surface);
    color: var(--ink-secondary);
  }
  .source-chip:hover {
    background: var(--sunken);
    color: var(--ink);
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-top: 2px;
  }
  .promote-panel {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding-top: 4px;
    border-top: 1px solid var(--border);
  }
  .promote-target {
    display: flex;
    align-items: center;
    gap: 5px;
    font-size: 11px;
    color: var(--ink-secondary);
  }
  .promote-panel textarea {
    font-family: inherit;
    font-size: 11.5px;
    padding: 6px 8px;
    border-radius: 6px;
    border: 1px solid var(--border-strong);
    background: var(--surface);
    color: var(--ink);
    resize: vertical;
  }
  .promote-actions {
    display: flex;
    gap: 6px;
  }
  .error {
    font-size: 10.5px;
    color: var(--danger);
  }
</style>

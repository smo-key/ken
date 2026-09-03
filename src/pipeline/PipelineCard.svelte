<script lang="ts">
  // ken-pipeline task 4.2/4.3/4.6: one pipeline-board card — project symbol
  // top-left, lane's model/agent badge, bounce badge, and (D5) the blocked
  // badge + blocker chips + reason. Actions are explicit buttons rather
  // than free drag-drop between lanes: D3's whole point is that a mis-drag
  // must never start a code-writing agent, so every forward move goes
  // through `pipeline_kickoff` (agent lanes, gated) or `pipeline_advance`
  // (holding lanes, ungated because there's no agent to mis-fire).
  import type { Pipeline, PipelineLane, Task } from "../lib/api";
  import { pipelineStore, projectSymbol } from "../lib/pipeline.svelte";
  import { tasksStore } from "../lib/tasks.svelte";
  import Bot from "@lucide/svelte/icons/bot";
  import Play from "@lucide/svelte/icons/play";
  import Ban from "@lucide/svelte/icons/ban";
  import Link from "@lucide/svelte/icons/link";
  import ChevronRight from "@lucide/svelte/icons/chevron-right";
  import FileText from "@lucide/svelte/icons/file-text";
  import History from "@lucide/svelte/icons/history";
  import CircleCheck from "@lucide/svelte/icons/circle-check";
  import CircleX from "@lucide/svelte/icons/circle-x";

  let { task, lane, pipeline }: { task: Task; lane: PipelineLane; pipeline: Pipeline } = $props();

  const fields = $derived(pipelineStore.fieldsFor(task.id));
  // Mirrors `pipeline::matches_block_filter`'s "carries block evidence"
  // reading — not merely "sits in the blocked lane" (a hand-edited
  // `status: blocked` with no evidence is a tray orphan, not this).
  const blocked = $derived(!!fields && (fields.blockedBy.length > 0 || fields.blockReason !== null));
  const model = $derived(fields?.model ?? lane.model ?? "");
  const agent = $derived(fields?.agent ?? lane.agent ?? "");
  const extraProjects = $derived(fields?.projects.filter((p) => p.toLowerCase() !== task.project.toLowerCase()).length ?? 0);
  // Mirrors `Lane::is_holding` (ken-core, not serialized to the wire): no
  // agent, a human lane, or the blocked lane itself — a lane that can
  // never start a run whatever the gates say.
  const isHolding = $derived(!lane.agent || lane.human || lane.blocked);

  let showAdvance = $state<"pass" | "fail" | null>(null);
  let report = $state("");
  let movingForward = $state(false);

  function blockerTicket(id: string): Task | undefined {
    return tasksStore.board.tasks.find((x) => x.id.toLowerCase() === id.toLowerCase());
  }
  function blockerLabel(id: string): string {
    const t = blockerTicket(id);
    return t ? t.title || id : id;
  }
  function openBlocker(id: string) {
    const t = blockerTicket(id);
    if (t) void tasksStore.openFile(t);
  }

  async function moveForward() {
    movingForward = true;
    try {
      await pipelineStore.advance(task.id, "pass", "");
    } catch {
      // pipelineStore.advanceError already holds the message.
    } finally {
      movingForward = false;
    }
  }

  async function submitAdvance() {
    if (!showAdvance) return;
    try {
      await pipelineStore.advance(task.id, showAdvance, report.trim());
      showAdvance = null;
      report = "";
    } catch {
      // pipelineStore.advanceError already holds the message; keep the
      // panel open so the user can see it and retry.
    }
  }
</script>

<div class="card" class:blocked>
  <div class="row title-row">
    <span class="symbol" title={task.project || "no project"}>{projectSymbol(task.project)}</span>
    {#if extraProjects > 0}
      <span class="plus-n" title="{extraProjects} more project(s): {fields?.projects.join(', ')}">+{extraProjects}</span>
    {/if}
    <span class="title">{task.title || "Untitled ticket"}</span>
  </div>

  <div class="row meta-row">
    {#if model}<span class="chip model">{model}</span>{/if}
    {#if agent}<span class="chip agent"><Bot size={10} strokeWidth={2} />{agent}</span>{/if}
    {#if fields && fields.bounces > 0}
      <span class="chip bounce" title="{fields.bounces} bounce(s) so far (cap {pipeline.bounceCap})">
        <History size={10} strokeWidth={2} />{fields.bounces}
      </span>
    {/if}
  </div>

  {#if blocked || lane.blocked}
    <div class="block-panel">
      <div class="block-head">
        <Ban size={11} strokeWidth={2} />
        {#if fields?.returnLane}blocked · returns to {fields.returnLane}{:else}blocked{/if}
      </div>
      {#if fields?.blockReason}<div class="block-reason">{fields.blockReason}</div>{/if}
      {#if fields && fields.blockedBy.length > 0}
        <div class="blockers">
          {#each fields.blockedBy as id (id)}
            <button class="blocker-chip" onclick={() => openBlocker(id)} title="Open blocking ticket">
              <Link size={9} strokeWidth={2} />{blockerLabel(id)}
            </button>
          {/each}
        </div>
      {/if}
      <button class="btn btn-small btn-ghost" onclick={() => pipelineStore.openBlockDialog(task.id)}>Manage block</button>
    </div>
  {/if}

  <div class="actions">
    {#if lane.blocked}
      <!-- The blocked lane itself has no forward action — "Manage block" above is the only move. -->
    {:else if lane.human}
      <button class="btn btn-small btn-primary" onclick={() => pipelineStore.openSignoff(task.id)}>Review</button>
    {:else if isHolding}
      {#if lane.onPass}
        <button class="btn btn-small" disabled={blocked || movingForward} title={blocked ? `Blocked: ${fields?.blockReason ?? 'dependency'}` : `Move to ${lane.onPass}`} onclick={moveForward}>
          Move forward<ChevronRight size={12} strokeWidth={2} />
        </button>
      {/if}
    {:else}
      <button
        class="btn btn-small btn-primary"
        disabled={blocked}
        title={blocked ? `Blocked: ${fields?.blockReason ?? 'dependency'}` : "Start this lane's agent"}
        onclick={() => pipelineStore.openKickoff(task.id)}
      >
        <Play size={11} strokeWidth={2} />Start
      </button>
      {#if lane.onPass}
        <button class="btn btn-small btn-ghost" disabled={blocked} title="Manually record a pass (e.g. an agent reported outside this UI)" onclick={() => (showAdvance = 'pass')}>
          <CircleCheck size={11} strokeWidth={2} />
        </button>
      {/if}
      {#if lane.onFail}
        <button class="btn btn-small btn-ghost" disabled={blocked} title="Manually record a fail (bounces back per this lane's on_fail edge)" onclick={() => (showAdvance = 'fail')}>
          <CircleX size={11} strokeWidth={2} />
        </button>
      {/if}
    {/if}
    {#if !lane.blocked}
      <button class="btn btn-small btn-ghost" onclick={() => pipelineStore.openBlockDialog(task.id)}>Block</button>
    {/if}
    <button class="btn btn-small btn-ghost icon-only" title="Artifacts" onclick={() => pipelineStore.openArtifacts(task.id)}>
      <FileText size={12} strokeWidth={1.75} />
    </button>
  </div>

  {#if showAdvance}
    <div class="advance-panel">
      <textarea bind:value={report} rows="2" placeholder="Report (optional)"></textarea>
      <div class="advance-actions">
        <button class="btn btn-small btn-primary" onclick={submitAdvance} disabled={pipelineStore.advancingId === task.id}>
          Confirm {showAdvance}
        </button>
        <button class="btn btn-small btn-ghost" onclick={() => (showAdvance = null)}>Cancel</button>
      </div>
      {#if pipelineStore.advanceError}<div class="error">{pipelineStore.advanceError}</div>{/if}
    </div>
  {/if}
</div>

<style>
  .card {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 10px 11px;
    border-radius: var(--radius-control);
    border: 1px solid var(--border);
    background: var(--surface);
    box-shadow: var(--shadow-card);
  }
  .card.blocked {
    border-color: color-mix(in srgb, var(--needs-input) 45%, var(--border-strong));
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
    width: 20px;
    height: 20px;
    border-radius: 6px;
    font-size: 9.5px;
    font-weight: 700;
    letter-spacing: 0.02em;
    background: color-mix(in srgb, var(--file-doc) 16%, transparent);
    color: var(--file-doc);
  }
  .plus-n {
    font-size: 9.5px;
    font-weight: 700;
    color: var(--ink-tertiary);
    background: var(--sunken);
    border-radius: 999px;
    padding: 1px 5px;
  }
  .title {
    font-size: 13px;
    font-weight: 600;
    color: var(--ink);
    line-height: 1.35;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    font-size: 10.5px;
    font-weight: 500;
    padding: 2px 7px;
    border-radius: 999px;
    background: var(--sunken);
    color: var(--ink-secondary);
    white-space: nowrap;
  }
  .chip.model {
    font-family: var(--font-mono);
  }
  .chip.agent {
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    color: var(--accent-deep);
  }
  .chip.bounce {
    background: color-mix(in srgb, var(--needs-input) 16%, transparent);
    color: var(--needs-input-text);
  }
  .block-panel {
    display: flex;
    flex-direction: column;
    gap: 5px;
    padding: 7px 8px;
    border-radius: 8px;
    background: color-mix(in srgb, var(--needs-input) 9%, transparent);
    border: 1px solid color-mix(in srgb, var(--needs-input) 30%, transparent);
  }
  .block-head {
    display: flex;
    align-items: center;
    gap: 5px;
    font-size: 11px;
    font-weight: 700;
    color: var(--needs-input-text);
  }
  .block-reason {
    font-size: 11px;
    color: var(--ink-secondary);
  }
  .blockers {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }
  .blocker-chip {
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
  .blocker-chip:hover {
    background: var(--sunken);
    color: var(--ink);
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 5px;
    flex-wrap: wrap;
  }
  .icon-only {
    margin-left: auto;
    padding: 4px 6px;
  }
  .advance-panel {
    display: flex;
    flex-direction: column;
    gap: 5px;
    padding-top: 4px;
    border-top: 1px solid var(--border);
  }
  .advance-panel textarea {
    font-family: inherit;
    font-size: 11.5px;
    padding: 6px 8px;
    border-radius: 6px;
    border: 1px solid var(--border-strong);
    background: var(--surface);
    color: var(--ink);
    resize: vertical;
  }
  .advance-actions {
    display: flex;
    gap: 6px;
  }
  .error {
    font-size: 10.5px;
    color: var(--danger);
  }
</style>

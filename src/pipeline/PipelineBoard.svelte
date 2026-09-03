<script lang="ts">
  // ken-pipeline task 4.2/4.5: the pipeline board — one section per loaded
  // pipeline, horizontally scrolling lanes in DEFINITION order (D1: lane
  // order is column order, never re-sorted), filters, the run tray and
  // digest toggles, and banners for anything that must stay visible rather
  // than being silently swallowed (a bad definition's `validate_pipeline`
  // issues; unknown-lane/pipeline/blocker/return-lane tray entries).
  import { pipelineStore } from "../lib/pipeline.svelte";
  import { tasksStore } from "../lib/tasks.svelte";
  import type { PipelineIssue } from "../lib/api";
  import PipelineCard from "./PipelineCard.svelte";
  import KickoffDialog from "./KickoffDialog.svelte";
  import BlockDialog from "./BlockDialog.svelte";
  import SignoffDialog from "./SignoffDialog.svelte";
  import ArtifactViewer from "./ArtifactViewer.svelte";
  import RunTray from "./RunTray.svelte";
  import DigestPanel from "./DigestPanel.svelte";
  import IdeasView from "./IdeasView.svelte";
  import TriangleAlert from "@lucide/svelte/icons/triangle-alert";
  import ListChecks from "@lucide/svelte/icons/list-checks";
  import History from "@lucide/svelte/icons/history";
  import Lightbulb from "@lucide/svelte/icons/lightbulb";
  import X from "@lucide/svelte/icons/x";

  const laneOptions = $derived([...new Set(pipelineStore.pipelines.flatMap((p) => p.lanes.map((l) => l.id)))].sort());
  const assigneeOptions = $derived([...new Set(pipelineStore.pipelineTasks.map((t) => t.assignee).filter((a) => a))].sort());

  function issueLabel(issue: PipelineIssue): string {
    switch (issue.issue) {
      case "noLanes":
        return "no lanes declared";
      case "laneMissingId":
        return `lane at index ${issue.index} has no id`;
      case "duplicateLaneId":
        return `duplicate lane id "${issue.id}"`;
      case "multipleBlockedLanes":
        return `two lanes marked blocked: "${issue.first}" and "${issue.second}"`;
      case "multipleHumanLanes":
        return `two lanes marked human: "${issue.first}" and "${issue.second}"`;
      case "invalidMapsTo":
        return `lane "${issue.lane}" has an invalid maps_to "${issue.value}"`;
      case "invalidKickoff":
        return `lane "${issue.lane}" has an invalid kickoff "${issue.value}"`;
      case "unknownTransition":
        return `lane "${issue.lane}"'s ${issue.edge} points at unknown lane "${issue.target}"`;
    }
  }
</script>

{#if !pipelineStore.enabled}
  <div class="empty-state">
    <p>
      The pipeline board is off — turn on <span class="mono">kenPipeline</span> in Settings → Features (it requires
      <span class="mono">workspace</span> and <span class="mono">kenTasks</span>).
    </p>
  </div>
{:else}
  <div class="pipeline-screen">
    <div class="main">
      <div class="toolbar">
        <select bind:value={pipelineStore.filters.project}>
          <option value="">All projects</option>
          {#each pipelineStore.projects as p (p)}
            <option value={p}>{p}</option>
          {/each}
        </select>
        {#if !pipelineStore.showIdeas}
          <select bind:value={pipelineStore.filters.lane}>
            <option value="">All lanes</option>
            {#each laneOptions as l (l)}
              <option value={l}>{l}</option>
            {/each}
          </select>
          <select bind:value={pipelineStore.filters.model}>
            <option value="">Any model</option>
            {#each pipelineStore.models as m (m)}
              <option value={m}>{m}</option>
            {/each}
          </select>
          <select bind:value={pipelineStore.filters.assignee}>
            <option value="">Any assignee</option>
            {#each assigneeOptions as a (a)}
              <option value={a}>{a}</option>
            {/each}
          </select>
          <select bind:value={pipelineStore.filters.blockMode}>
            <option value="">Any block state</option>
            <option value="blocked">Blocked</option>
            <option value="notBlocked">Not blocked</option>
            <option value="newlyUnblocked">Newly unblocked</option>
            <option value="byTicket">Blocked by ticket…</option>
          </select>
          {#if pipelineStore.filters.blockMode === "byTicket"}
            <input class="filter-text" placeholder="Ticket id" bind:value={pipelineStore.filters.blockByTicketId} />
          {/if}
          <label class="check disabled" title="Requires per-project links (workspace.json `links`, D12) — no command exposes them to the frontend yet; deferred rather than invented.">
            <input type="checkbox" disabled />
            Include linked projects
          </label>
        {/if}
        {#if pipelineStore.filtersActive}
          <button class="clear-filters" onclick={() => pipelineStore.clearFilters()}>
            <X size={12} strokeWidth={2} /> Clear
          </button>
        {/if}

        <div class="toolbar-right">
          <button
            class="btn btn-small ideas-toggle"
            class:btn-ghost={!pipelineStore.showIdeas}
            class:btn-primary={pipelineStore.showIdeas}
            onclick={() => (pipelineStore.showIdeas = !pipelineStore.showIdeas)}
            title="Short notes proposed by the Documentation lane, landed inert (D16) — review and promote them here"
          >
            <Lightbulb size={13} strokeWidth={1.75} />
            Ideas
            {#if pipelineStore.ideaCount > 0}<span class="idea-badge">{pipelineStore.ideaCount}</span>{/if}
          </button>
          <button class="btn btn-small btn-ghost" onclick={() => (pipelineStore.digestOpen = !pipelineStore.digestOpen)}>
            <ListChecks size={13} strokeWidth={1.75} />Digest
          </button>
          <button class="btn btn-small btn-ghost" onclick={() => (pipelineStore.runTrayOpen = !pipelineStore.runTrayOpen)}>
            <History size={13} strokeWidth={1.75} />
            Runs ({pipelineStore.runs.running.length + pipelineStore.runs.queued.length})
          </button>
        </div>
      </div>

      {#each pipelineStore.defs as def (def.id)}
        {#if def.issues.length > 0}
          <div class="banner warn">
            <TriangleAlert size={13} strokeWidth={1.75} />
            <span>
              "{def.name || def.id}" has {def.issues.length} definition issue(s): {def.issues.map(issueLabel).join("; ")}
            </span>
          </div>
        {/if}
      {/each}

      {#if tasksStore.board.needsAttention.length > 0}
        <button class="banner attention" onclick={() => (tasksStore.attentionOpen = !tasksStore.attentionOpen)}>
          <TriangleAlert size={13} strokeWidth={1.75} />
          <span>{tasksStore.board.needsAttention.length} ticket(s) need attention (unknown lane/pipeline/blocker/return lane) — never rewritten, fix by hand</span>
        </button>
      {/if}

      {#if pipelineStore.showIdeas}
        <IdeasView />
      {:else}
        <div class="pipelines">
          {#each pipelineStore.pipelines as pipeline (pipeline.id)}
            <section class="pipeline-section">
              <div class="pipeline-head">
                <h2>{pipeline.name}</h2>
                <span class="auto-badge" class:on={pipeline.auto}>{pipeline.auto ? "auto-transitions on" : "manual only"}</span>
                <span class="cap-badge">cap {pipeline.concurrencyCap} · bounce cap {pipeline.bounceCap}</span>
              </div>
              <div class="lanes">
                {#each pipeline.lanes as lane (lane.id)}
                  {@const laneTasks = pipelineStore.tasksForLane(pipeline.id, lane.id)}
                  <div class="lane-col" class:blocked-lane={lane.blocked} class:human-lane={lane.human}>
                    <div class="lane-head">
                      <span class="lane-title">{lane.name}</span>
                      <span class="lane-count">{pipelineStore.laneCountsFor(pipeline.id)[lane.id] ?? laneTasks.length}</span>
                    </div>
                    <div class="lane-body">
                      {#each laneTasks as task (task.id)}
                        <PipelineCard {task} {lane} {pipeline} />
                      {:else}
                        <p class="lane-empty">Empty</p>
                      {/each}
                    </div>
                  </div>
                {/each}
              </div>
            </section>
          {:else}
            <p class="note">No pipeline definitions loaded yet.</p>
          {/each}
        </div>
      {/if}
    </div>

    {#if pipelineStore.runTrayOpen}<RunTray />{/if}
    {#if pipelineStore.digestOpen}<DigestPanel />{/if}
  </div>

  <KickoffDialog />
  <BlockDialog />
  <SignoffDialog />
  <ArtifactViewer />
{/if}

<style>
  .empty-state {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--ink-tertiary);
    font-size: 13px;
    padding: 40px;
    text-align: center;
  }
  .mono {
    font-family: var(--font-mono);
  }
  .pipeline-screen {
    flex: 1;
    min-width: 0;
    min-height: 0;
    display: flex;
    overflow: hidden;
  }
  .main {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 10px;
    overflow: auto;
    padding: 4px 0 16px;
  }
  .toolbar {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
  .toolbar select,
  .filter-text {
    font-family: inherit;
    font-size: 12px;
    padding: 6px 9px;
    border-radius: 7px;
    border: 1px solid var(--border-strong);
    background: var(--surface);
    color: var(--ink);
  }
  .filter-text {
    width: 110px;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 5px;
    font-size: 11.5px;
    color: var(--ink-tertiary);
  }
  .check.disabled {
    cursor: not-allowed;
  }
  .clear-filters {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    font-size: 11.5px;
    color: var(--ink-tertiary);
    border: none;
    background: transparent;
    padding: 4px 6px;
  }
  .clear-filters:hover {
    color: var(--ink);
  }
  .toolbar-right {
    margin-left: auto;
    display: flex;
    gap: 6px;
  }
  .ideas-toggle {
    gap: 5px;
  }
  .idea-badge {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: 15px;
    height: 15px;
    padding: 0 4px;
    border-radius: 999px;
    font-size: 10px;
    font-weight: 700;
    background: color-mix(in srgb, var(--accent) 20%, transparent);
    color: var(--accent-deep);
  }
  .ideas-toggle.btn-primary .idea-badge {
    background: color-mix(in srgb, white 30%, transparent);
    color: inherit;
  }
  .banner {
    display: flex;
    align-items: center;
    gap: 7px;
    font-size: 11.5px;
    padding: 7px 10px;
    border-radius: 8px;
    text-align: left;
    border: none;
    width: 100%;
  }
  .banner.warn {
    background: color-mix(in srgb, var(--danger) 8%, transparent);
    color: var(--danger);
    border: 1px solid color-mix(in srgb, var(--danger) 25%, transparent);
  }
  .banner.attention {
    background: color-mix(in srgb, var(--needs-input) 10%, transparent);
    color: var(--needs-input-text);
    border: 1px solid color-mix(in srgb, var(--needs-input) 30%, transparent);
    cursor: pointer;
  }
  .pipelines {
    display: flex;
    flex-direction: column;
    gap: 18px;
  }
  .pipeline-section {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .pipeline-head {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .pipeline-head h2 {
    margin: 0;
    font-family: var(--font-serif);
    font-size: 16px;
    font-weight: 500;
  }
  .auto-badge,
  .cap-badge {
    font-size: 10px;
    font-weight: 600;
    padding: 2px 8px;
    border-radius: 999px;
    background: var(--sunken);
    color: var(--ink-tertiary);
  }
  .auto-badge.on {
    background: color-mix(in srgb, var(--accent) 14%, transparent);
    color: var(--accent-deep);
  }
  .lanes {
    display: flex;
    gap: 12px;
    align-items: flex-start;
    overflow-x: auto;
    padding-bottom: 6px;
  }
  .lane-col {
    width: 260px;
    flex: none;
    display: flex;
    flex-direction: column;
    gap: 8px;
    border-radius: var(--radius-card);
    background: var(--sunken-2);
    border: 1px solid var(--border);
    padding: 10px;
    min-height: 120px;
  }
  .lane-col.blocked-lane {
    border-color: color-mix(in srgb, var(--needs-input) 45%, var(--border-strong));
    background: color-mix(in srgb, var(--needs-input) 4%, var(--sunken-2));
  }
  .lane-col.human-lane {
    border-color: color-mix(in srgb, var(--accent) 40%, var(--border-strong));
  }
  .lane-head {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .lane-title {
    font-size: 12px;
    font-weight: 700;
    color: var(--ink-secondary);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .lane-count {
    font-size: 11px;
    color: var(--ink-tertiary);
    margin-left: auto;
  }
  .lane-body {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .lane-empty {
    margin: 0;
    font-size: 11px;
    color: var(--ink-tertiary);
    font-style: italic;
  }
  .note {
    font-size: 12.5px;
    color: var(--ink-tertiary);
  }
</style>

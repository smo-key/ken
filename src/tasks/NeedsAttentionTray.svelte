<script lang="ts">
  // ken-tasks task 4.2: the needs-attention tray — tasks a hand edit or
  // agent error left with an out-of-vocabulary status/kind/board, or a
  // `goal:` id matching no goal file (design D7). These are deliberately
  // never auto-rewritten (spec: "the file is not rewritten"), so the tray
  // is read-only except for the same "open the file to fix it by hand" path
  // every other card offers.
  import { tasksStore } from "../lib/tasks.svelte";
  import { pipelineStore } from "../lib/pipeline.svelte";
  import { api, type AttentionReason } from "../lib/api";
  import TriangleAlert from "@lucide/svelte/icons/triangle-alert";

  function describe(r: AttentionReason): string {
    switch (r.reason) {
      case "invalidStatus":
        return `status: "${r.value}" isn't a recognized column`;
      case "invalidKind":
        return `kind: "${r.value}" isn't human/ai`;
      case "invalidBoard":
        return `board: "${r.value}" isn't main/daily`;
      case "unknownGoal":
        return `goal: "${r.value}" doesn't match any goal file`;
      // ken-pipeline task 1.4/4.9 — surfaced, never rewritten, same rule as
      // the four classic reasons above.
      case "unknownPipeline":
        return `pipeline: "${r.value}" names a definition that isn't loaded`;
      case "unknownLane":
        return `status: "${r.value}" matches no lane in this ticket's pipeline`;
      case "unknownBlocker":
        return `blocked_by: "${r.value}" matches no ticket on the board`;
      case "unknownReturnLane":
        return r.value ? `return_lane: "${r.value}" is no longer a valid lane` : "blocked with no return_lane recorded";
    }
  }

  function taskFor(id: string) {
    return tasksStore.board.tasks.find((t) => t.id === id) ?? null;
  }

  // ── ken-pipeline task 4.9: additions beyond `AttentionReason` — these
  // aren't tray REASONS on a `Task` (the backend enum has no "ageing" or
  // "missing boundary" variant), they're derived board-wide from data
  // `board-state`/`pipeline_runs` already carry, so they're rendered as
  // their own groups below rather than forced into `describe()`. ──────────

  const AGEING_HOURS = 24;

  function hoursSince(iso: string | null): number | null {
    if (!iso) return null;
    const t = Date.parse(iso);
    if (Number.isNaN(t)) return null;
    return (Date.now() - t) / 3_600_000;
  }

  const ageingBlocked = $derived(
    pipelineStore.enabled
      ? tasksStore.board.blocked
          .map((b) => ({ b, hrs: hoursSince(b.blockedAt) }))
          .filter((x) => x.hrs !== null && x.hrs >= AGEING_HOURS)
          .sort((a, c) => (c.hrs ?? 0) - (a.hrs ?? 0))
      : [],
  );

  /** A ticket in a lane that HAS an agent but is missing `scope` and/or
   *  `verify` — D3's "can never auto-run" ticket, worth flagging even
   *  though it's not a hard refusal (see `KickoffDialog.svelte`'s own note
   *  on why this doesn't hard-disable the Start button). */
  const missingBoundary = $derived(
    pipelineStore.enabled
      ? pipelineStore.pipelineTasks
          .map((t) => {
            const fields = pipelineStore.fieldsFor(t.id);
            const lane = pipelineStore.pipelines.find((p) => p.id.toLowerCase() === fields?.pipeline?.toLowerCase())?.lanes.find((l) => l.id === t.lane);
            return { t, fields, lane };
          })
          .filter((x) => x.lane && x.lane.agent && !x.lane.human && !x.lane.blocked && x.fields && (x.fields.scope.length === 0 || !x.fields.verify))
      : [],
  );

  // Expired artifacts: best-effort, scoped to tickets currently on the
  // board (no bulk "list every manifest" command exists — flagged as a
  // scaling gap for a very large board, not fixed here since it would need
  // a new backend command outside this session's touch scope).
  let expiredArtifacts = $state<{ ticketId: string; title: string; expires: string }[]>([]);
  let expiredChecked = false;
  $effect(() => {
    if (!pipelineStore.enabled || expiredChecked || pipelineStore.pipelineTasks.length === 0) return;
    expiredChecked = true;
    void Promise.all(
      pipelineStore.pipelineTasks.map(async (t) => {
        const m = await api.pipelineArtifacts(t.id).catch(() => null);
        return m?.expired ? { ticketId: t.id, title: t.title, expires: m.expires } : null;
      }),
    ).then((rows) => {
      expiredArtifacts = rows.filter((r): r is { ticketId: string; title: string; expires: string } => r !== null);
    });
  });

  async function prune(ticketId: string) {
    await pipelineStore.pruneArtifacts(ticketId);
    expiredArtifacts = expiredArtifacts.filter((r) => r.ticketId !== ticketId);
  }
</script>

<div class="tray">
  <div class="tray-head">
    <TriangleAlert size={14} strokeWidth={1.75} />
    <span>Needs attention</span>
    <span class="count">{tasksStore.board.needsAttention.length}</span>
  </div>
  <p class="note">
    Ken never rewrites a file it doesn't understand — fix these by hand, then
    they'll drop off this list on their own.
  </p>
  {#each tasksStore.board.needsAttention as item (item.id)}
    {@const task = taskFor(item.id)}
    {@const reason = task ? tasksStore.openReason(task) : "Task file no longer on the board."}
    <button
      class="item"
      class:disabled-link={!!reason}
      title={reason ?? "Open task file"}
      onclick={() => task && void tasksStore.openFile(task)}
    >
      <div class="item-title">{item.title || "Untitled task"}</div>
      <div class="reasons">
        {#each item.reasons as r, i (i)}
          <span class="reason">{describe(r)}</span>
        {/each}
      </div>
    </button>
  {:else}
    <p class="note empty">Nothing needs attention.</p>
  {/each}

  <!-- ken-pipeline task 4.9: derived pipeline groups — not `AttentionReason`
       tray entries, but the task explicitly asks for them alongside the
       classic tray, so they live in the same panel rather than a second
       disconnected surface. -->
  {#if pipelineStore.enabled}
    {#if ageingBlocked.length > 0}
      <div class="group-head">Blocked &gt;{AGEING_HOURS}h ({ageingBlocked.length})</div>
      {#each ageingBlocked as { b, hrs } (b.ticketId)}
        <div class="item static">
          <div class="item-title">{b.title || "Untitled ticket"}</div>
          <span class="reason">{b.blockReason ?? "dependency"} · blocked {Math.round(hrs ?? 0)}h ago</span>
        </div>
      {/each}
    {/if}

    {#if missingBoundary.length > 0}
      <div class="group-head">Missing scope/verify ({missingBoundary.length})</div>
      {#each missingBoundary as { t, fields } (t.id)}
        <div class="item static">
          <div class="item-title">{t.title || "Untitled ticket"}</div>
          <span class="reason">
            {fields && fields.scope.length === 0 ? "no scope" : ""}
            {fields && fields.scope.length === 0 && !fields.verify ? " · " : ""}
            {fields && !fields.verify ? "no verify command" : ""}
            — can never auto-run
          </span>
        </div>
      {/each}
    {/if}

    {#if expiredArtifacts.length > 0}
      <div class="group-head">Expired artifacts ({expiredArtifacts.length})</div>
      {#each expiredArtifacts as row (row.ticketId)}
        <div class="item static">
          <div class="item-title">{row.title || "Untitled ticket"}</div>
          <span class="reason">expired {row.expires}</span>
          <button class="prune-btn" onclick={() => void prune(row.ticketId)}>Prune</button>
        </div>
      {/each}
    {/if}

    {#if pipelineStore.runs.stale.length > 0}
      <div class="group-head">Stale runs ({pipelineStore.runs.stale.length})</div>
      {#each pipelineStore.runs.stale as r (r.id)}
        <div class="item static">
          <div class="item-title">{taskFor(r.ticket)?.title || r.ticket}</div>
          <span class="reason">{r.lane} · no live process observed since restart (D13)</span>
        </div>
      {/each}
    {/if}
  {/if}
</div>

<style>
  .tray {
    width: 280px;
    flex: none;
    border-left: 1px solid var(--border);
    padding: 14px;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .tray-head {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 13px;
    font-weight: 600;
    color: var(--needs-input-text);
  }
  .count {
    margin-left: auto;
    font-size: 11px;
    font-weight: 700;
    color: var(--surface);
    background: var(--needs-input);
    border-radius: 999px;
    padding: 1px 7px;
  }
  .note {
    font-size: 11.5px;
    line-height: 1.5;
    color: var(--ink-tertiary);
    margin: 0;
  }
  .note.empty {
    padding: 8px 0;
  }
  .item {
    display: flex;
    flex-direction: column;
    gap: 4px;
    text-align: left;
    padding: 9px 10px;
    border-radius: 8px;
    border: 1px solid var(--border);
    background: var(--surface);
  }
  .item:hover {
    background: var(--sunken);
  }
  .item.disabled-link {
    cursor: default;
  }
  .item-title {
    font-size: 12.5px;
    font-weight: 600;
    color: var(--ink);
  }
  .reasons {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .reason {
    font-size: 11px;
    color: var(--needs-input-text);
  }
  .group-head {
    margin-top: 4px;
    font-size: 10.5px;
    font-weight: 700;
    color: var(--ink-tertiary);
    text-transform: uppercase;
    letter-spacing: 0.03em;
  }
  .item.static {
    cursor: default;
  }
  .prune-btn {
    align-self: flex-start;
    font-size: 10.5px;
    font-weight: 600;
    border: 1px solid var(--border);
    background: var(--paper);
    color: var(--ink-tertiary);
    border-radius: 6px;
    padding: 2px 8px;
  }
  .prune-btn:hover {
    background: var(--sunken);
    color: var(--ink);
  }
</style>

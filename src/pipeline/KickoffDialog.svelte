<script lang="ts">
  // ken-pipeline task 4.6 (D3): the kickoff confirmation gate. Shows the
  // INTENT DIFF — lane, agent, model, scope globs, verify command — not a
  // generic "are you sure" (design.md: "a generic prompt trains people to
  // click through"). Opened by `pipelineStore.openKickoff`, which always
  // calls `pipeline_kickoff(id, confirmed: false)` first; this dialog only
  // ever renders what that call returned.
  import { pipelineStore } from "../lib/pipeline.svelte";
  import { tasksStore } from "../lib/tasks.svelte";
  import type { PipelineConfirmReason, PipelineRefusalReason } from "../lib/api";
  import ShieldAlert from "@lucide/svelte/icons/shield-alert";
  import Play from "@lucide/svelte/icons/play";

  const ticketId = $derived(pipelineStore.kickoffTicketId);
  const task = $derived(ticketId ? (tasksStore.board.tasks.find((t) => t.id === ticketId) ?? null) : null);
  const outcome = $derived(pipelineStore.kickoffOutcome);

  // Only meaningful for the `missingBoundary` reason (task 4.6: "disabled
  // ... when scope or verify is missing"). D3 itself doesn't hard-refuse an
  // unbounded ticket — it only downgrades the gate to `confirm` — so rather
  // than silently blocking a human's deliberate override, this makes them
  // explicitly acknowledge the missing boundary before the Confirm button
  // enables. Judgment call, noted in the session report.
  let acknowledged = $state(false);

  function reasonLabel(reason: PipelineConfirmReason): string {
    switch (reason.reason) {
      case "laneGate":
        return "This lane requires confirmation before every run.";
      case "manualKickoff":
        return "This lane never starts itself — you asked for it directly.";
      case "missingBoundary": {
        const parts: string[] = [];
        if (!reason.scope) parts.push("no scope");
        if (!reason.verify) parts.push("no verify command");
        return `No defined boundary (${parts.join(", ")}) — this lane can never auto-run. Starting it anyway is a deliberate override.`;
      }
      case "unblocked":
        return "This ticket just unblocked — unblocking never starts an agent by itself, even in an auto lane.";
      case "autoDisabled":
        return "This pipeline's auto switch is off, so even an auto lane waits for confirmation.";
    }
  }

  function refusalLabel(reason: PipelineRefusalReason): string {
    switch (reason.reason) {
      case "blocked":
        return `This ticket is blocked${reason.blockReason ? `: ${reason.blockReason}` : ""}${reason.blockedBy.length > 0 ? ` (waiting on ${reason.blockedBy.join(", ")})` : ""}.`;
      case "humanLane":
        return `"${reason.lane}" is a human sign-off lane — use the Review action instead.`;
      case "noAgent":
        return `"${reason.lane}" is a holding column with no agent — use "Move forward" instead.`;
      case "manualLane":
        return `"${reason.lane}" is a manual lane and can't be reached this way.`;
      default:
        return "Refused.";
    }
  }

  async function confirm() {
    await pipelineStore.confirmKickoff();
  }
</script>

{#if ticketId}
  <button class="scrim" onclick={() => pipelineStore.closeKickoff()} aria-label="Close"></button>
  <div class="modal" role="dialog" aria-label="Confirm kickoff">
    <h2>Start this lane's agent?</h2>
    {#if task}<p class="ticket-title">{task.title || "Untitled ticket"}</p>{/if}

    {#if pipelineStore.kickoffBusy && !outcome}
      <p class="note">Checking…</p>
    {:else if pipelineStore.kickoffError}
      <div class="error"><ShieldAlert size={14} strokeWidth={1.75} />{pipelineStore.kickoffError}</div>
    {:else if outcome?.kind === "refused"}
      <div class="error"><ShieldAlert size={14} strokeWidth={1.75} />{refusalLabel(outcome.reason)}</div>
    {:else if outcome?.kind === "queued"}
      <div class="success">
        Queued (run <code>{outcome.runId}</code>) —
        {outcome.ready ? "ready to claim now." : `waiting behind ${outcome.running}/${outcome.cap} running.`}
      </div>
    {:else if outcome?.kind === "needsConfirm"}
      <div class="intent">
        <div class="intent-row"><span class="label">Lane</span><span>{outcome.lane}</span></div>
        <div class="intent-row"><span class="label">Agent</span><span>{outcome.agent ?? "—"}</span></div>
        <div class="intent-row"><span class="label">Model</span><span>{outcome.model ?? "—"}</span></div>
        <div class="intent-row">
          <span class="label">Scope</span>
          <span class="globs">
            {#if outcome.scope.length > 0}
              {#each outcome.scope as g (g)}<code class="glob">{g}</code>{/each}
            {:else}
              <span class="missing">no scope set</span>
            {/if}
          </span>
        </div>
        <div class="intent-row">
          <span class="label">Verify</span>
          <span>
            {#if outcome.verify}<code class="glob">{outcome.verify}</code>{:else}<span class="missing">no verify command</span>{/if}
          </span>
        </div>
      </div>
      <p class="reason">{reasonLabel(outcome.reason)}</p>
      {#if outcome.reason.reason === "missingBoundary"}
        <label class="ack">
          <input type="checkbox" bind:checked={acknowledged} />
          I understand this run has no defined file boundary and no verify command.
        </label>
      {/if}
    {/if}

    <div class="actions">
      {#if outcome?.kind === "needsConfirm"}
        <button
          class="btn btn-primary"
          disabled={pipelineStore.kickoffBusy || (outcome.reason.reason === "missingBoundary" && !acknowledged)}
          onclick={confirm}
        >
          <Play size={12} strokeWidth={2} />{pipelineStore.kickoffBusy ? "Starting…" : "Confirm & start"}
        </button>
      {/if}
      <button class="btn btn-ghost" onclick={() => pipelineStore.closeKickoff()}>
        {outcome && outcome.kind !== "needsConfirm" ? "Close" : "Cancel"}
      </button>
    </div>
  </div>
{/if}

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: var(--scrim);
    border: none;
    z-index: 60;
  }
  .modal {
    position: fixed;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    width: min(480px, calc(100vw - 80px));
    max-height: calc(100vh - 120px);
    overflow-y: auto;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-overlay);
    box-shadow: var(--shadow-overlay);
    padding: 24px 26px;
    z-index: 61;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  h2 {
    margin: 0;
    font-family: var(--font-serif);
    font-size: 19px;
    font-weight: 500;
  }
  .ticket-title {
    margin: -6px 0 0;
    font-size: 13px;
    color: var(--ink-secondary);
  }
  .note {
    font-size: 12.5px;
    color: var(--ink-tertiary);
  }
  .error {
    display: flex;
    align-items: flex-start;
    gap: 7px;
    font-size: 12.5px;
    color: var(--danger);
    background: color-mix(in srgb, var(--danger) 7%, transparent);
    border: 1px solid color-mix(in srgb, var(--danger) 25%, transparent);
    border-radius: 9px;
    padding: 9px 12px;
  }
  .success {
    font-size: 12.5px;
    color: var(--ink);
    background: color-mix(in srgb, var(--accent) 8%, transparent);
    border: 1px solid color-mix(in srgb, var(--accent) 25%, transparent);
    border-radius: 9px;
    padding: 9px 12px;
  }
  .intent {
    display: flex;
    flex-direction: column;
    gap: 7px;
    background: var(--sunken-2);
    border-radius: 10px;
    padding: 12px 14px;
  }
  .intent-row {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    font-size: 12.5px;
  }
  .label {
    flex: none;
    width: 52px;
    font-weight: 700;
    color: var(--ink-tertiary);
    text-transform: uppercase;
    letter-spacing: 0.03em;
    font-size: 10.5px;
    padding-top: 2px;
  }
  .globs {
    display: flex;
    flex-wrap: wrap;
    gap: 5px;
  }
  .glob {
    font-family: var(--font-mono);
    font-size: 11.5px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 5px;
    padding: 1px 6px;
    word-break: break-all;
  }
  .missing {
    color: var(--needs-input-text);
    font-style: italic;
  }
  .reason {
    margin: 0;
    font-size: 12px;
    color: var(--ink-secondary);
    line-height: 1.5;
  }
  .ack {
    display: flex;
    align-items: flex-start;
    gap: 7px;
    font-size: 12px;
    color: var(--needs-input-text);
    background: color-mix(in srgb, var(--needs-input) 9%, transparent);
    border: 1px solid color-mix(in srgb, var(--needs-input) 30%, transparent);
    border-radius: 8px;
    padding: 8px 10px;
  }
  .ack input {
    margin-top: 2px;
  }
  .actions {
    display: flex;
    gap: 8px;
    margin-top: 4px;
  }
</style>

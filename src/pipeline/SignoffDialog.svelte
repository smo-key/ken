<script lang="ts">
  // ken-pipeline task 4.8 (D11): the human sign-off lane's review dialog.
  // Accept / Accept with comments (spawns a `todo`-lane child carrying the
  // comment, shown here once created) / Reject are three distinct,
  // deliberate actions — never a single button with a mode toggle, so a
  // misclick can't silently swap which one fires.
  import { pipelineStore } from "../lib/pipeline.svelte";
  import { tasksStore } from "../lib/tasks.svelte";
  import type { PipelineSignoffDto } from "../lib/api";
  import CircleCheck from "@lucide/svelte/icons/circle-check";
  import MessageSquare from "@lucide/svelte/icons/message-square";
  import CircleX from "@lucide/svelte/icons/circle-x";

  const ticketId = $derived(pipelineStore.signoffTicketId);
  const task = $derived(ticketId ? (tasksStore.board.tasks.find((t) => t.id === ticketId) ?? null) : null);

  let comment = $state("");
  let result = $state<PipelineSignoffDto | null>(null);

  $effect(() => {
    if (ticketId) {
      comment = "";
      result = null;
    }
  });

  async function accept() {
    if (!ticketId) return;
    try {
      result = await pipelineStore.signoff(ticketId, "accept");
      if (!result.child) close();
    } catch {
      // pipelineStore.signoffError already holds the message.
    }
  }
  async function acceptWithComments() {
    if (!ticketId || !comment.trim()) return;
    try {
      result = await pipelineStore.signoff(ticketId, "acceptWithComments", comment.trim());
    } catch {
      // pipelineStore.signoffError already holds the message.
    }
  }
  async function reject() {
    if (!ticketId) return;
    try {
      result = await pipelineStore.signoff(ticketId, "reject");
      close();
    } catch {
      // pipelineStore.signoffError already holds the message.
    }
  }
  function close() {
    pipelineStore.closeSignoff();
  }
</script>

{#if ticketId}
  <button class="scrim" onclick={close} aria-label="Close"></button>
  <div class="modal" role="dialog" aria-label="Sign-off">
    <h2>Sign-off</h2>
    {#if task}<p class="ticket-title">{task.title || "Untitled ticket"}</p>{/if}

    {#if result?.child}
      <div class="success">
        <div class="success-head"><MessageSquare size={13} strokeWidth={2} />Comment filed as a new ticket</div>
        <p><strong>{result.child.title}</strong> — created in <code>todo</code>, <code>parent</code> set to this ticket.</p>
        <p class="note">The parent has already advanced — a comment never blocks the thing that's done.</p>
      </div>
      <div class="actions">
        <button class="btn btn-primary" onclick={close}>Done</button>
      </div>
    {:else}
      <label class="field">
        Comment (only used by "Accept with comments" — spawns a child ticket carrying this text)
        <textarea bind:value={comment} rows="3" placeholder="What should follow up, without blocking this ticket?"></textarea>
      </label>

      {#if pipelineStore.signoffError}<div class="error">{pipelineStore.signoffError}</div>{/if}

      <div class="actions three">
        <button class="btn btn-primary" disabled={pipelineStore.signoffBusy} onclick={accept}>
          <CircleCheck size={13} strokeWidth={2} />Accept
        </button>
        <button class="btn" disabled={pipelineStore.signoffBusy || !comment.trim()} onclick={acceptWithComments}>
          <MessageSquare size={13} strokeWidth={2} />Accept with comments
        </button>
        <button class="btn btn-danger" disabled={pipelineStore.signoffBusy} onclick={reject}>
          <CircleX size={13} strokeWidth={2} />Reject
        </button>
      </div>
      <button class="btn btn-ghost close-link" onclick={close}>Cancel</button>
    {/if}
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
  .field {
    display: flex;
    flex-direction: column;
    gap: 6px;
    font-size: 11.5px;
    font-weight: 600;
    color: var(--ink-secondary);
  }
  textarea {
    font-family: inherit;
    font-size: 12.5px;
    font-weight: 400;
    padding: 8px 10px;
    border-radius: 8px;
    border: 1px solid var(--border-strong);
    background: var(--surface);
    color: var(--ink);
    resize: vertical;
  }
  .error {
    font-size: 12px;
    color: var(--danger);
    background: color-mix(in srgb, var(--danger) 7%, transparent);
    border: 1px solid color-mix(in srgb, var(--danger) 25%, transparent);
    border-radius: 8px;
    padding: 8px 10px;
  }
  .success {
    display: flex;
    flex-direction: column;
    gap: 5px;
    padding: 12px 14px;
    border-radius: 9px;
    background: color-mix(in srgb, var(--accent) 8%, transparent);
    border: 1px solid color-mix(in srgb, var(--accent) 25%, transparent);
    font-size: 12.5px;
  }
  .success p {
    margin: 0;
  }
  .success-head {
    display: flex;
    align-items: center;
    gap: 6px;
    font-weight: 700;
    color: var(--accent-deep);
  }
  .note {
    color: var(--ink-tertiary);
  }
  .actions {
    display: flex;
    gap: 8px;
  }
  .actions.three {
    flex-wrap: wrap;
  }
  .btn-danger {
    background: color-mix(in srgb, var(--danger) 10%, transparent);
    color: var(--danger);
    border: 1px solid color-mix(in srgb, var(--danger) 30%, transparent);
  }
  .close-link {
    align-self: flex-start;
  }
</style>

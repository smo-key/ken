<script lang="ts">
  // ken-pipeline task 4.4 (D5): block/unblock dialog. Set dependencies (a
  // ticket picker searching across every open project, since `blocked_by`
  // holds ULIDs and is valid across task homes) and/or a free-text reason
  // — both optional, at least one required; a cycle refusal renders the
  // backend's own readable path (`describe_block_refusal` already composes
  // it server-side, this dialog just displays the string). Unblock clears
  // dependencies and/or the reason independently (D5: clearing one must
  // not unblock a ticket the other still applies to).
  import { pipelineStore } from "../lib/pipeline.svelte";
  import { tasksStore } from "../lib/tasks.svelte";
  import Search from "@lucide/svelte/icons/search";
  import X from "@lucide/svelte/icons/x";
  import Link from "@lucide/svelte/icons/link";

  const ticketId = $derived(pipelineStore.blockTicketId);
  const task = $derived(ticketId ? (tasksStore.board.tasks.find((t) => t.id === ticketId) ?? null) : null);
  const fields = $derived(ticketId ? pipelineStore.fieldsFor(ticketId) : null);
  const alreadyBlocked = $derived(!!fields && (fields.blockedBy.length > 0 || fields.blockReason !== null));

  let query = $state("");
  let picked = $state<string[]>([]);
  let reason = $state("");
  let clearDeps = $state(false);
  let clearReason = $state(false);

  // Reset the add-block form whenever the dialog opens on a new ticket.
  $effect(() => {
    if (ticketId) {
      query = "";
      picked = [];
      reason = "";
      clearDeps = false;
      clearReason = false;
    }
  });

  const matches = $derived(
    query.trim().length < 2
      ? []
      : tasksStore.board.tasks
          .filter(
            (t) =>
              t.id !== ticketId &&
              !picked.includes(t.id) &&
              (t.title.toLowerCase().includes(query.trim().toLowerCase()) || t.id.toLowerCase().startsWith(query.trim().toLowerCase())),
          )
          .slice(0, 8),
  );

  function pick(id: string) {
    picked = [...picked, id];
    query = "";
  }
  function unpick(id: string) {
    picked = picked.filter((p) => p !== id);
  }
  function titleFor(id: string): string {
    return tasksStore.board.tasks.find((t) => t.id === id)?.title || id;
  }

  async function submitBlock() {
    if (!ticketId) return;
    try {
      await pipelineStore.block(ticketId, {
        blockedBy: picked.length > 0 ? picked : undefined,
        reason: reason.trim() ? reason.trim() : undefined,
      });
    } catch {
      // pipelineStore.blockError already holds the (server-composed,
      // readable-cycle-path) message.
    }
  }

  async function submitUnblock() {
    if (!ticketId) return;
    try {
      await pipelineStore.unblock(ticketId, { clearDeps, clearReason });
    } catch {
      // pipelineStore.blockError already holds the message.
    }
  }
</script>

{#if ticketId}
  <button class="scrim" onclick={() => pipelineStore.closeBlockDialog()} aria-label="Close"></button>
  <div class="modal" role="dialog" aria-label="Block / unblock ticket">
    <h2>{alreadyBlocked ? "Manage block" : "Block ticket"}</h2>
    {#if task}<p class="ticket-title">{task.title || "Untitled ticket"}</p>{/if}

    {#if alreadyBlocked && fields}
      <div class="current">
        <div class="current-head"><Link size={12} strokeWidth={2} />Currently blocked</div>
        {#if fields.blockReason}<p class="current-reason">{fields.blockReason}</p>{/if}
        {#if fields.blockedBy.length > 0}
          <div class="chip-row">
            {#each fields.blockedBy as id (id)}<span class="chip">{titleFor(id)}</span>{/each}
          </div>
        {/if}
        {#if fields.returnLane}<p class="return-lane">Returns to <strong>{fields.returnLane}</strong> once cleared.</p>{/if}
      </div>
    {/if}

    <section class="block-section">
      <h3>Add dependencies / a reason</h3>
      <label class="field">
        Search tickets to depend on (any project)
        <div class="search-wrap">
          <Search size={12} strokeWidth={2} />
          <input bind:value={query} placeholder="Title or ticket id…" />
        </div>
      </label>
      {#if matches.length > 0}
        <div class="matches">
          {#each matches as m (m.id)}
            <button class="match" onclick={() => pick(m.id)}>
              <span class="match-title">{m.title || "Untitled ticket"}</span>
              <span class="match-project">{m.project || "—"}</span>
            </button>
          {/each}
        </div>
      {/if}
      {#if picked.length > 0}
        <div class="chip-row">
          {#each picked as id (id)}
            <span class="chip removable">
              {titleFor(id)}
              <button onclick={() => unpick(id)} aria-label="Remove"><X size={10} strokeWidth={2} /></button>
            </span>
          {/each}
        </div>
      {/if}
      <label class="field">
        Reason (optional free text)
        <textarea bind:value={reason} rows="2" placeholder="e.g. waiting for the upstream 2.0 release"></textarea>
      </label>
      {#if pipelineStore.blockError}<div class="error">{pipelineStore.blockError}</div>{/if}
      <div class="actions">
        <button class="btn btn-primary" disabled={pipelineStore.blockBusy || (picked.length === 0 && !reason.trim())} onclick={submitBlock}>
          {pipelineStore.blockBusy ? "Saving…" : alreadyBlocked ? "Add to block" : "Block ticket"}
        </button>
      </div>
    </section>

    {#if alreadyBlocked}
      <section class="block-section">
        <h3>Unblock</h3>
        <label class="check"><input type="checkbox" bind:checked={clearDeps} /> Clear dependencies</label>
        <label class="check"><input type="checkbox" bind:checked={clearReason} /> Clear reason</label>
        <p class="note">Clearing only frees the ticket once BOTH are cleared (or the other was never set). It re-enters its return lane at a confirmation — never straight into a run.</p>
        <div class="actions">
          <button class="btn" disabled={pipelineStore.blockBusy || (!clearDeps && !clearReason)} onclick={submitUnblock}>
            {pipelineStore.blockBusy ? "Saving…" : "Apply unblock"}
          </button>
        </div>
      </section>
    {/if}

    <div class="actions">
      <button class="btn btn-ghost" onclick={() => pipelineStore.closeBlockDialog()}>Close</button>
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
    width: min(520px, calc(100vw - 80px));
    max-height: calc(100vh - 100px);
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
  h3 {
    margin: 0 0 2px;
    font-size: 12px;
    font-weight: 700;
    color: var(--ink-secondary);
    text-transform: uppercase;
    letter-spacing: 0.03em;
  }
  .ticket-title {
    margin: -6px 0 0;
    font-size: 13px;
    color: var(--ink-secondary);
  }
  .current {
    display: flex;
    flex-direction: column;
    gap: 5px;
    padding: 10px 12px;
    border-radius: 9px;
    background: color-mix(in srgb, var(--needs-input) 9%, transparent);
    border: 1px solid color-mix(in srgb, var(--needs-input) 30%, transparent);
  }
  .current-head {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 11.5px;
    font-weight: 700;
    color: var(--needs-input-text);
  }
  .current-reason,
  .return-lane {
    margin: 0;
    font-size: 12px;
    color: var(--ink-secondary);
  }
  .block-section {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding-top: 8px;
    border-top: 1px solid var(--border);
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 5px;
    font-size: 11.5px;
    font-weight: 600;
    color: var(--ink-secondary);
  }
  .search-wrap {
    display: flex;
    align-items: center;
    gap: 6px;
    border: 1px solid var(--border-strong);
    border-radius: 8px;
    padding: 6px 10px;
    background: var(--surface);
    color: var(--ink-tertiary);
  }
  .search-wrap input {
    border: none;
    outline: none;
    background: transparent;
    font-size: 12.5px;
    color: var(--ink);
    flex: 1;
  }
  textarea {
    font-family: inherit;
    font-size: 12.5px;
    padding: 7px 10px;
    border-radius: 8px;
    border: 1px solid var(--border-strong);
    background: var(--surface);
    color: var(--ink);
    resize: vertical;
  }
  .matches {
    display: flex;
    flex-direction: column;
    gap: 2px;
    max-height: 150px;
    overflow-y: auto;
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 4px;
  }
  .match {
    display: flex;
    justify-content: space-between;
    gap: 8px;
    text-align: left;
    padding: 6px 8px;
    border-radius: 6px;
    border: none;
    background: transparent;
    font-size: 12px;
    color: var(--ink);
  }
  .match:hover {
    background: var(--sunken);
  }
  .match-project {
    color: var(--ink-tertiary);
    font-size: 11px;
    flex: none;
  }
  .chip-row {
    display: flex;
    flex-wrap: wrap;
    gap: 5px;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: 11px;
    padding: 3px 8px;
    border-radius: 999px;
    background: var(--sunken);
    color: var(--ink-secondary);
  }
  .chip.removable button {
    border: none;
    background: transparent;
    color: inherit;
    display: inline-flex;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 12.5px;
    color: var(--ink-secondary);
  }
  .note {
    margin: 0;
    font-size: 11px;
    color: var(--ink-tertiary);
    line-height: 1.5;
  }
  .error {
    font-size: 12px;
    color: var(--danger);
    background: color-mix(in srgb, var(--danger) 7%, transparent);
    border: 1px solid color-mix(in srgb, var(--danger) 25%, transparent);
    border-radius: 8px;
    padding: 8px 10px;
    white-space: pre-wrap;
  }
  .actions {
    display: flex;
    gap: 8px;
  }
</style>

<script lang="ts">
  // ken-pipeline task 4.10 (D9): the artifact viewer for one ticket's
  // `.ken-workspace/artifacts/<ticket-id>/` folder — throwaway QA-lane
  // output (walkthroughs, screenshots, demo recordings), never part of any
  // project's real test suite. Always shows the "throwaway" marker and the
  // expiry date; prune is the only mutation (OPEN-5: "never auto-deleted").
  import { pipelineStore } from "../lib/pipeline.svelte";
  import { tasksStore } from "../lib/tasks.svelte";
  import FileText from "@lucide/svelte/icons/file-text";
  import Trash2 from "@lucide/svelte/icons/trash-2";
  import TriangleAlert from "@lucide/svelte/icons/triangle-alert";

  const ticketId = $derived(pipelineStore.artifactsTicketId);
  const task = $derived(ticketId ? (tasksStore.board.tasks.find((t) => t.id === ticketId) ?? null) : null);

  let newFilename = $state("");
  let registering = $state(false);
  let pruning = $state(false);

  async function registerNote() {
    if (!ticketId || !newFilename.trim()) return;
    registering = true;
    try {
      await pipelineStore.registerArtifact(ticketId, newFilename.trim());
      // `registerArtifact` doesn't update the store's manifest itself (it's
      // a one-shot API wrapper) — re-fetch so the list reflects the write.
      await pipelineStore.openArtifacts(ticketId);
      newFilename = "";
    } finally {
      registering = false;
    }
  }

  async function prune() {
    if (!ticketId) return;
    pruning = true;
    try {
      await pipelineStore.pruneArtifacts(ticketId);
    } finally {
      pruning = false;
    }
  }
</script>

{#if ticketId}
  <button class="scrim" onclick={() => pipelineStore.closeArtifacts()} aria-label="Close"></button>
  <div class="modal" role="dialog" aria-label="Artifacts">
    <h2>Artifacts</h2>
    {#if task}<p class="ticket-title">{task.title || "Untitled ticket"}</p>{/if}

    <div class="throwaway-marker">
      <TriangleAlert size={12} strokeWidth={2} />throwaway — not part of the test suite
    </div>

    {#if pipelineStore.artifactsLoading}
      <p class="note">Loading…</p>
    {:else if pipelineStore.artifactsError}
      <div class="error">{pipelineStore.artifactsError}</div>
    {:else if !pipelineStore.artifactsManifest}
      <p class="note">No artifact folder yet for this ticket.</p>
    {:else}
      {@const m = pipelineStore.artifactsManifest}
      <div class="meta-row">
        <span>Created {m.created}</span>
        <span class:expired={m.expired}>Expires {m.expires}{m.expired ? " (expired)" : ""}</span>
      </div>
      {#if m.files.length > 0}
        <ul class="files">
          {#each m.files as f (f)}
            <li><FileText size={12} strokeWidth={1.75} />{f}</li>
          {/each}
        </ul>
      {:else}
        <p class="note">Manifest exists but names no files yet.</p>
      {/if}
      <div class="actions">
        <button class="btn btn-ghost" disabled={pruning} onclick={prune}>
          <Trash2 size={12} strokeWidth={1.75} />{pruning ? "Pruning…" : "Prune folder"}
        </button>
      </div>
    {/if}

    <label class="field">
      Record a filename already written under this ticket's artifact folder
      <div class="row">
        <input bind:value={newFilename} placeholder="walkthrough.md" />
        <button class="btn btn-small" disabled={registering || !newFilename.trim()} onclick={registerNote}>Add</button>
      </div>
    </label>

    <div class="actions">
      <button class="btn btn-ghost" onclick={() => pipelineStore.closeArtifacts()}>Close</button>
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
    width: min(440px, calc(100vw - 80px));
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
  .throwaway-marker {
    display: inline-flex;
    align-self: flex-start;
    align-items: center;
    gap: 6px;
    font-size: 11px;
    font-weight: 700;
    color: var(--needs-input-text);
    background: color-mix(in srgb, var(--needs-input) 12%, transparent);
    border: 1px solid color-mix(in srgb, var(--needs-input) 30%, transparent);
    border-radius: 999px;
    padding: 3px 10px;
  }
  .note {
    font-size: 12.5px;
    color: var(--ink-tertiary);
  }
  .error {
    font-size: 12px;
    color: var(--danger);
  }
  .meta-row {
    display: flex;
    gap: 14px;
    font-size: 11.5px;
    color: var(--ink-tertiary);
  }
  .meta-row .expired {
    color: var(--danger);
    font-weight: 600;
  }
  .files {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .files li {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 12px;
    font-family: var(--font-mono);
    color: var(--ink);
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 5px;
    font-size: 11.5px;
    font-weight: 600;
    color: var(--ink-secondary);
    padding-top: 8px;
    border-top: 1px solid var(--border);
  }
  .row {
    display: flex;
    gap: 6px;
  }
  input {
    flex: 1;
    font-family: inherit;
    font-size: 12.5px;
    font-weight: 400;
    padding: 6px 9px;
    border-radius: 7px;
    border: 1px solid var(--border-strong);
    background: var(--surface);
    color: var(--ink);
  }
  .actions {
    display: flex;
    gap: 8px;
  }
</style>

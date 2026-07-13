<script lang="ts">
  // Merge-conflict detail: side-by-side "who changed what" cards, Ken's
  // drafted take, and plain-language resolution buttons (prototype: Review
  // screen conflict card). No git vocabulary anywhere.
  import { app } from "../lib/app.svelte";
  import { review, conflictPayload } from "../lib/review.svelte";
  import type { InboxItem } from "../lib/api";

  let { item }: { item: InboxItem } = $props();

  const payload = $derived(conflictPayload(item));
  let busy = $state(false);

  async function resolve(
    resolution: "accept-draft" | "keep-mine" | "take-theirs",
    thenEdit = false,
  ) {
    if (busy) return;
    busy = true;
    try {
      const path = await review.resolveConflict(item, resolution);
      if (thenEdit) app.openInFiles(path);
    } finally {
      busy = false;
    }
  }
</script>

<div class="conflict-card">
  {#if payload}
    <div class="versions">
      <div class="version mine">
        <div class="version-head mine-head">On this computer</div>
        <div class="version-body">{payload.ours}</div>
      </div>
      <div class="version theirs">
        <div class="version-head theirs-head">From your teammate</div>
        <div class="version-body">{payload.theirs}</div>
      </div>
    </div>

    <div class="take">
      {#if payload.draftStatus === "ready" && payload.draft}
        <strong>Ken's take:</strong>
        <div class="take-body">{payload.draft}</div>
      {:else if payload.draftStatus === "failed"}
        <strong>Ken's take:</strong>
        <span class="soft">
          Ken couldn't draft a combined version this time — pick one of the
          versions, or edit the document yourself.
        </span>
      {:else}
        <strong>Ken's take:</strong>
        <span class="soft drafting">Ken is drafting a suggestion…</span>
      {/if}
    </div>

    <div class="actions">
      {#if payload.draftStatus === "ready" && payload.draft}
        <button
          class="btn btn-primary"
          disabled={busy}
          onclick={() => void resolve("accept-draft")}
        >
          Accept Ken's merge
        </button>
      {/if}
      <button class="btn" disabled={busy} onclick={() => void resolve("keep-mine")}>
        Keep mine
      </button>
      <button class="btn" disabled={busy} onclick={() => void resolve("take-theirs")}>
        Use theirs
      </button>
      <button
        class="btn btn-ghost"
        disabled={busy}
        onclick={() => void resolve("accept-draft", true)}
      >
        Edit manually
      </button>
    </div>
  {:else}
    <div class="soft">This conflict's details couldn't be loaded.</div>
  {/if}
</div>

<style>
  .conflict-card {
    background: var(--surface);
    border: 1px solid rgba(163, 77, 63, 0.3);
    border-radius: var(--radius-card);
    padding: 18px 20px;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .versions {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
    gap: 10px;
    font-size: 12.5px;
    line-height: 1.6;
  }
  .version {
    border: 1px solid var(--sunken);
    border-radius: 9px;
    padding: 12px 14px;
    min-width: 0;
  }
  .version.mine {
    background: rgba(163, 77, 63, 0.04);
  }
  .version.theirs {
    background: rgba(90, 122, 94, 0.05);
  }
  .version-head {
    font-weight: 700;
    margin-bottom: 4px;
    font-size: 11px;
    letter-spacing: 0.05em;
    text-transform: uppercase;
  }
  .mine-head {
    color: var(--danger);
  }
  .theirs-head {
    color: var(--healthy-text);
  }
  .version-body,
  .take-body {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    max-height: 220px;
    overflow-y: auto;
  }
  .take {
    font-size: 13px;
    line-height: 1.6;
    color: var(--ink-secondary);
    background: var(--sunken-2);
    border-radius: 9px;
    padding: 11px 14px;
  }
  .take strong {
    color: var(--ink);
  }
  .take-body {
    margin-top: 4px;
  }
  .soft {
    color: var(--ink-tertiary);
  }
  .drafting {
    animation: drafting-pulse 1.4s ease-in-out infinite;
  }
  @keyframes drafting-pulse {
    50% {
      opacity: 0.45;
    }
  }
  .actions {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
</style>

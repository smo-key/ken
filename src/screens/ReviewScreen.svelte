<script lang="ts">
  import { onMount } from "svelte";
  import { app } from "../lib/app.svelte";
  import { ingests } from "../lib/ingests.svelte";
  import {
    review,
    actionsFor,
    dotFor,
    sourceLabel,
    type InboxAction,
  } from "../lib/review.svelte";
  import { timeAgo } from "../lib/format";
  import type { InboxItem } from "../lib/api";

  onMount(() => void review.init());

  const item = $derived(review.selectedItem);
  const itemIsDone = $derived(review.selectedIsDone);

  const LABELS: Record<InboxAction, string> = {
    approve: "Approve",
    discard: "Discard",
    "run-now": "Run now",
    "open-files": "Open in Files",
    "open-ingests": "Open in Ingests",
    "mark-done": "Mark as done",
  };

  function act(action: InboxAction, it: InboxItem) {
    switch (action) {
      case "approve":
        void review.approve(it);
        break;
      case "discard":
        void review.discard(it);
        break;
      case "run-now":
        void review.runNow(it);
        break;
      case "open-files":
        app.openInFiles(it.sourceRef);
        break;
      case "open-ingests":
        app.screen = "ingests";
        void ingests.select(it.sourceRef);
        break;
      case "mark-done":
        void review.markDone(it);
        break;
    }
  }
</script>

<div class="screen">
  <!-- inbox list -->
  <div class="list">
    <div class="list-head">Inbox</div>

    {#if review.items.length === 0}
      <div class="empty-list">Nothing waiting — you're all caught up.</div>
    {/if}

    {#each review.items as it (it.id)}
      <button
        class="row"
        class:active={review.selected === it.id}
        onclick={() => review.select(it.id)}
      >
        <span class="row-top">
          <span class="dot" style:background={dotFor(it.kind)}></span>
          {it.title}
        </span>
        <span class="row-caption">{sourceLabel(it)} · {timeAgo(it.when)}</span>
      </button>
    {/each}

    {#if review.done.length > 0}
      <div class="list-head done-head">Done</div>
      {#each review.done as it (it.id)}
        <button
          class="row done"
          class:active={review.selected === it.id}
          onclick={() => review.select(it.id)}
        >
          <span class="row-top muted">
            {it.title}
            <span class="check">✓</span>
          </span>
        </button>
      {/each}
    {/if}
  </div>

  <!-- detail -->
  <div class="detail">
    {#if item}
      <div class="detail-inner">
        <div class="detail-head">
          <h1>{item.title}</h1>
          <span class="meta mono">{sourceLabel(item)} · {timeAgo(item.when)}</span>
        </div>
        <div class="card">
          <div class="body">{item.body}</div>
          {#if itemIsDone}
            <div class="resolved-note">Resolved {timeAgo(item.when)}.</div>
          {:else}
            <div class="actions">
              {#each actionsFor(item.kind) as action, i (action)}
                <button
                  class="btn"
                  class:btn-primary={i === 0}
                  onclick={() => act(action, item)}
                >
                  {LABELS[action]}
                </button>
              {/each}
            </div>
          {/if}
        </div>
      </div>
    {:else if review.items.length === 0}
      <div class="empty">
        <div class="empty-card">
          <h2>Nothing needs you right now</h2>
          <p>
            When Ken holds a big refresh for approval, notices a document
            going stale, or hits a file it can't read, it lands here — one
            place for everything waiting on you.
          </p>
        </div>
      </div>
    {:else}
      <div class="empty"><p class="hint">Select an item from the inbox.</p></div>
    {/if}
  </div>
</div>

<style>
  .screen {
    flex: 1;
    min-width: 0;
    display: flex;
    min-height: 0;
  }
  .list {
    flex: 0 1 272px;
    min-width: 220px;
    border-right: 1px solid var(--border);
    background: var(--sunken-2);
    display: flex;
    flex-direction: column;
    overflow-y: auto;
    padding: 12px 8px;
  }
  .list-head {
    padding: 2px 10px 8px;
    font-size: 11px;
    font-weight: 700;
    color: var(--ink-tertiary);
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }
  .done-head {
    padding-top: 14px;
  }
  .row {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 9px 10px;
    border-radius: 8px;
    border: none;
    background: transparent;
    text-align: left;
    width: 100%;
  }
  .row:hover {
    background: var(--sunken);
  }
  .row.active {
    background: rgba(138, 90, 68, 0.1);
  }
  .row.active .row-top {
    color: var(--accent-deep);
  }
  .row-top {
    display: flex;
    align-items: center;
    gap: 7px;
    font-size: 12.5px;
    font-weight: 600;
    color: var(--ink);
  }
  .row-top.muted {
    font-weight: 400;
    color: var(--ink-tertiary);
  }
  .check {
    margin-left: auto;
    font-size: 10.5px;
  }
  .dot {
    width: 6px;
    height: 6px;
    border-radius: 3px;
    flex: none;
  }
  .row-caption {
    font-size: 11.5px;
    color: var(--ink-tertiary);
    padding-left: 13px;
  }
  .empty-list {
    padding: 10px;
    font-size: 12px;
    color: var(--ink-tertiary);
    line-height: 1.6;
  }

  .detail {
    flex: 1;
    min-width: 0;
    overflow-y: auto;
    padding: 32px 40px;
  }
  .detail-inner {
    max-width: 680px;
    margin: 0 auto;
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .detail-head {
    display: flex;
    align-items: baseline;
    gap: 10px;
    flex-wrap: wrap;
  }
  h1 {
    margin: 0;
    font-family: var(--font-serif);
    font-size: 24px;
    font-weight: 500;
  }
  .meta {
    font-size: 11.5px;
    color: var(--ink-tertiary);
  }
  .card {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-card);
    padding: 18px 20px;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .body {
    font-size: 13.5px;
    line-height: 1.7;
    white-space: pre-wrap;
  }
  .actions {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
  .resolved-note {
    font-size: 12px;
    color: var(--ink-tertiary);
  }
  .empty {
    height: 100%;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 0 40px;
  }
  .empty-card {
    max-width: 420px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-card);
    padding: 26px 28px;
    box-shadow: var(--shadow-card);
    text-align: center;
  }
  .empty-card h2 {
    margin: 0 0 8px;
    font-family: var(--font-serif);
    font-size: 19px;
    font-weight: 500;
  }
  .empty-card p {
    margin: 0;
    font-size: 13px;
    line-height: 1.65;
    color: var(--ink-secondary);
  }
  .hint {
    margin: 0;
    color: var(--ink-tertiary);
    font-size: 13px;
  }
</style>

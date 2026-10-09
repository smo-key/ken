<script lang="ts">
  // An edit Claude proposed, as a git-style diff: each change with its old
  // and new line numbers and its own Accept / Deny, or all of them at once.
  // Used on the chat card and over the page in the editor; both answer the
  // same proposal through the chat store.
  import Check from "@lucide/svelte/icons/check";
  import X from "@lucide/svelte/icons/x";
  import type { EditProposal } from "../lib/api";
  import { chats } from "../lib/chats.svelte";
  import { app } from "../lib/app.svelte";
  import { applyChoices, describeHunk, hunksOf } from "./editHunks";

  let {
    messageId,
    proposal,
    showOpen = true,
  }: { messageId: number; proposal: EditProposal; showOpen?: boolean } = $props();

  const hunks = $derived(hunksOf(proposal.base, proposal.proposed));
  let choices = $state<Record<number, boolean>>({});
  let busy = $state(false);
  let error = $state<string | null>(null);
  const file = $derived(proposal.relPath ?? proposal.path);
  const decided = $derived(proposal.decision);
  const allChosen = $derived(hunks.every((h) => h.id in choices));
  const isNew = $derived(proposal.base === "" && proposal.tool === "Write");

  function choose(id: number, accept: boolean) {
    choices = { ...choices, [id]: accept };
  }

  async function submit(all?: boolean) {
    if (busy || decided) return;
    const accepted = hunks.map((h) => (all === undefined ? choices[h.id] === true : all));
    busy = true;
    error = null;
    try {
      if (accepted.every(Boolean)) {
        await chats.answerEdit(messageId, "accepted", null, []);
      } else if (!accepted.some(Boolean)) {
        await chats.answerEdit(messageId, "declined", null, []);
      } else {
        const merged = applyChoices(proposal.base, proposal.proposed, accepted);
        const declined = hunks.filter((_, i) => !accepted[i]).map(describeHunk);
        await chats.answerEdit(messageId, "partial", merged, declined);
      }
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  function open() {
    if (proposal.relPath) app.openAt(proposal.relPath, { line: hunks[0]?.newStart });
  }
</script>

<div class="review" class:decided={!!decided}>
  <div class="head">
    <span class="title">
      {#if decided === "accepted"}Accepted{:else if decided === "declined"}Declined{:else if decided === "partial"}Partly accepted{:else}Claude proposes{/if}
      {isNew ? "a new file" : `${hunks.length} ${hunks.length === 1 ? "change" : "changes"}`} to
      {#if proposal.relPath && showOpen}
        <button class="file mono" onclick={open} title="Open {file}">{file}</button>
      {:else}
        <span class="file mono">{file}</span>
      {/if}
    </span>
    {#if !decided}
      <span class="all">
        <button class="btn btn-small btn-primary" disabled={busy} onclick={() => submit(true)}>Accept all</button>
        <button class="btn btn-small" disabled={busy} onclick={() => submit(false)}>Deny all</button>
      </span>
    {/if}
  </div>

  {#if !decided}
    {#each hunks as h (h.id)}
      {@const choice = choices[h.id]}
      <div class="hunk" class:accepted={choice === true} class:denied={choice === false}>
        <div class="hunk-head mono">
          <span>@@ −{h.oldStart},{h.removed.length} +{h.newStart},{h.added.length} @@</span>
          <span class="hunk-actions">
            <button
              class="pick"
              class:on={choice === true}
              aria-pressed={choice === true}
              onclick={() => choose(h.id, true)}
            ><Check size={12} strokeWidth={2} /> Accept</button>
            <button
              class="pick deny"
              class:on={choice === false}
              aria-pressed={choice === false}
              onclick={() => choose(h.id, false)}
            ><X size={12} strokeWidth={2} /> Deny</button>
          </span>
        </div>
        <div class="lines mono">
          {#each h.before as l, i (i)}
            <div class="line ctx"><span class="no">{h.oldStart - h.before.length + i}</span><span class="no">{h.newStart - h.before.length + i}</span><span class="g"> </span><span class="t">{l || " "}</span></div>
          {/each}
          {#each h.removed as l, i (i)}
            <div class="line del"><span class="no">{h.oldStart + i}</span><span class="no"></span><span class="g">−</span><span class="t">{l || " "}</span></div>
          {/each}
          {#each h.added as l, i (i)}
            <div class="line add"><span class="no"></span><span class="no">{h.newStart + i}</span><span class="g">+</span><span class="t">{l || " "}</span></div>
          {/each}
          {#each h.after as l, i (i)}
            <div class="line ctx"><span class="no">{h.oldStart + h.removed.length + i}</span><span class="no">{h.newStart + h.added.length + i}</span><span class="g"> </span><span class="t">{l || " "}</span></div>
          {/each}
        </div>
      </div>
    {/each}
    {#if hunks.length > 1}
      <div class="foot">
        <span class="count">
          {Object.values(choices).filter(Boolean).length} accepted · {Object.values(choices).filter((c) => !c).length} denied · {hunks.length - Object.keys(choices).length} to choose
        </span>
        <button class="btn btn-small btn-primary" disabled={busy || !allChosen} onclick={() => submit()}>
          {busy ? "Applying…" : "Apply choices"}
        </button>
      </div>
    {/if}
  {:else if proposal.note}
    <p class="note">{proposal.note.split("\n")[0]}</p>
  {/if}
  {#if error}<p class="error">{error}</p>{/if}
</div>

<style>
  .review {
    border: 1px solid var(--border-strong);
    border-radius: 9px;
    background: var(--surface);
    overflow: hidden;
    font-size: 12.5px;
  }
  .review.decided {
    border-color: var(--border);
    opacity: 0.85;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
    padding: 8px 10px;
    background: var(--sunken-2);
    border-bottom: 1px solid var(--border);
  }
  .decided .head {
    border-bottom: none;
  }
  .title {
    flex: 1;
    min-width: 0;
    color: var(--ink-secondary);
  }
  .file {
    color: var(--accent-deep);
    background: none;
    border: none;
    padding: 0;
    font-size: 12px;
    cursor: pointer;
    text-decoration: underline;
    text-underline-offset: 2px;
  }
  span.file {
    cursor: default;
    text-decoration: none;
  }
  .all {
    display: flex;
    gap: 6px;
  }
  .hunk {
    border-bottom: 1px solid var(--border);
  }
  .hunk.accepted .hunk-head {
    background: color-mix(in srgb, var(--healthy) 12%, var(--sunken-2));
  }
  .hunk.denied .lines {
    opacity: 0.5;
  }
  .hunk-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 4px 10px;
    font-size: 11px;
    color: var(--ink-tertiary);
    background: var(--sunken-2);
  }
  .hunk-actions {
    display: flex;
    gap: 4px;
  }
  .pick {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    font-size: 11px;
    padding: 2px 8px;
    border-radius: 6px;
    border: 1px solid var(--border-strong);
    background: var(--surface);
    color: var(--ink-secondary);
  }
  .pick.on {
    border-color: var(--healthy);
    color: var(--healthy-text);
    background: color-mix(in srgb, var(--healthy) 12%, var(--surface));
  }
  .pick.deny.on {
    border-color: var(--danger);
    color: var(--danger);
    background: color-mix(in srgb, var(--danger) 10%, var(--surface));
  }
  .lines {
    font-size: 11.5px;
    line-height: 1.55;
    max-height: 320px;
    overflow: auto;
  }
  .line {
    display: grid;
    grid-template-columns: 3.2em 3.2em 1.2em 1fr;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .no {
    text-align: right;
    padding-right: 6px;
    color: var(--ink-tertiary);
    user-select: none;
  }
  .g {
    text-align: center;
    color: var(--ink-tertiary);
    user-select: none;
  }
  .line.del {
    background: color-mix(in srgb, var(--danger) 9%, transparent);
  }
  .line.del .g {
    color: var(--danger);
  }
  .line.add {
    background: color-mix(in srgb, var(--healthy) 10%, transparent);
  }
  .line.add .g {
    color: var(--healthy-text);
  }
  .foot {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 10px;
  }
  .count {
    flex: 1;
    color: var(--ink-tertiary);
    font-size: 11.5px;
  }
  .note {
    margin: 0;
    padding: 8px 10px;
    color: var(--ink-tertiary);
  }
  .error {
    margin: 0;
    padding: 6px 10px;
    color: var(--danger);
  }
</style>

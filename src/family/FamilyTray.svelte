<script lang="ts">
  // ken-families task 4.2: the notification tray — unread inbox items
  // grouped by family, with inline accept/push-back/dismiss. Popover
  // pattern mirrors `WorkspaceSwitcher.svelte` (scrim + fixed panel
  // anchored under its nav-rail trigger), since a family connection has
  // no owning screen — inbox items can arrive whether or not a workspace
  // is even open (proposal: "a standalone collaboration bus").
  //
  // LOCKED (design.md D4, spec "Typed inbox items with an acceptance
  // gate"): accepting is the ONLY way a task item ever reaches a board,
  // and it only ever happens from the explicit "Accept" click below —
  // nothing in this file calls `families.acceptTask` except that handler.
  import { families } from "../lib/families.svelte";
  import type { FamilyInboxItem } from "../lib/api";
  import SquareCheck from "@lucide/svelte/icons/square-check";
  import MessageSquare from "@lucide/svelte/icons/message-square";
  import Bell from "@lucide/svelte/icons/bell";
  import TriangleAlert from "@lucide/svelte/icons/triangle-alert";
  import Reply from "@lucide/svelte/icons/reply";
  import Archive from "@lucide/svelte/icons/archive";
  import Check from "@lucide/svelte/icons/check";

  let { close }: { close: () => void } = $props();

  let pushBackFor = $state<string | null>(null);
  let pushBackNote = $state("");
  let actionError = $state<string | null>(null);

  function itemKind(item: FamilyInboxItem): "task" | "message" | "notification" | "unknown" {
    return item.kind ?? "unknown";
  }

  function preview(body: string, max = 200): string {
    const trimmed = body.trim();
    return trimmed.length > max ? `${trimmed.slice(0, max)}…` : trimmed;
  }

  async function accept(familyId: string, item: FamilyInboxItem) {
    actionError = null;
    try {
      await families.acceptTask(familyId, item.id);
    } catch (e) {
      actionError = String(e);
    }
  }

  async function dismiss(familyId: string, item: FamilyInboxItem) {
    actionError = null;
    try {
      await families.setItemStatus(familyId, item.id, "archived");
    } catch (e) {
      actionError = String(e);
    }
  }

  function beginPushBack(item: FamilyInboxItem) {
    pushBackFor = item.id;
    pushBackNote = "";
    actionError = null;
  }

  function cancelPushBack() {
    pushBackFor = null;
    pushBackNote = "";
  }

  async function sendPushBack(familyId: string, item: FamilyInboxItem) {
    const note = pushBackNote.trim();
    if (!note) return;
    actionError = null;
    try {
      await families.pushBack(familyId, item.id, note);
      pushBackFor = null;
      pushBackNote = "";
    } catch (e) {
      actionError = String(e);
    }
  }
</script>

<button class="scrim" onclick={close} aria-label="Close family notifications"></button>
<div class="tray">
  <div class="head">
    <span class="title">Family inbox</span>
    <span class="count">{families.totalUnread}</span>
  </div>

  {#if actionError}
    <p class="note warn">{actionError}</p>
  {/if}

  {#if families.trayGroups.length === 0}
    <p class="note empty">Nothing waiting — sent items land here as they arrive.</p>
  {/if}

  {#each families.trayGroups as group (group.connection.connection.familyId)}
    <div class="group">
      <div class="group-head">
        <span class="group-name">{group.connection.connection.name}</span>
        <span class="group-count">{families.unreadFor(group.connection.connection.familyId)}</span>
      </div>
      {#each group.items as item (item.id)}
        {@const familyId = group.connection.connection.familyId}
        {@const busy = families.isResolving(familyId, item.id)}
        <div class="item" class:malformed={item.malformed}>
          <div class="item-head">
            <span class="kind-icon kind-{itemKind(item)}">
              {#if item.malformed}
                <TriangleAlert size={12} strokeWidth={1.75} />
              {:else if item.kind === "task"}
                <SquareCheck size={12} strokeWidth={1.75} />
              {:else if item.kind === "notification"}
                <Bell size={12} strokeWidth={1.75} />
              {:else}
                <MessageSquare size={12} strokeWidth={1.75} />
              {/if}
            </span>
            <span class="item-title">{item.title || item.fileName}</span>
            {#if item.status === "unread"}<span class="unread-dot" title="Unread"></span>{/if}
          </div>
          <div class="item-meta">
            <span>from {item.from || "unknown"}</span>
            {#if item.created}<span>· {item.created}</span>{/if}
          </div>
          {#if item.malformed}
            <p class="item-body warn">
              Couldn't parse this item's frontmatter — shown exactly as received, never rewritten.
            </p>
          {/if}
          {#if item.body}
            <p class="item-body">{preview(item.body)}</p>
          {/if}
          {#if item.kind === "task" && item.task?.title}
            <div class="task-chip">
              <SquareCheck size={11} strokeWidth={1.75} />
              {item.task.title}
              {#if item.task.project}<span class="soft">· {item.task.project}</span>{/if}
            </div>
          {/if}

          {#if pushBackFor === item.id}
            <div class="push-back">
              <textarea
                bind:value={pushBackNote}
                placeholder="Note back to {item.from || 'the sender'}…"
                rows="2"
              ></textarea>
              <div class="push-back-actions">
                <button class="btn-mini" disabled={busy || !pushBackNote.trim()} onclick={() => void sendPushBack(familyId, item)}>
                  Send
                </button>
                <button class="btn-mini ghost" onclick={cancelPushBack}>Cancel</button>
              </div>
            </div>
          {:else}
            <div class="item-actions">
              {#if item.kind === "task" && !item.malformed}
                <button class="btn-mini primary" disabled={busy} onclick={() => void accept(familyId, item)}>
                  <Check size={11} strokeWidth={2} />Accept
                </button>
              {/if}
              {#if !item.malformed && item.from}
                <button class="btn-mini" disabled={busy} onclick={() => beginPushBack(item)}>
                  <Reply size={11} strokeWidth={1.75} />Push back
                </button>
              {/if}
              <button class="btn-mini ghost" disabled={busy} onclick={() => void dismiss(familyId, item)}>
                <Archive size={11} strokeWidth={1.75} />Dismiss
              </button>
            </div>
          {/if}
        </div>
      {/each}
    </div>
  {/each}
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: transparent;
    border: none;
    z-index: 39;
  }
  .tray {
    position: fixed;
    top: 60px;
    left: 70px;
    width: 320px;
    max-height: calc(100vh - 90px);
    overflow-y: auto;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-overlay);
    box-shadow: var(--shadow-overlay);
    padding: 10px;
    z-index: 40;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px 6px 6px;
  }
  .title {
    flex: 1;
    font-size: 12px;
    font-weight: 700;
    letter-spacing: 0.03em;
    text-transform: uppercase;
    color: var(--ink-tertiary);
  }
  .count {
    font-size: 11px;
    font-weight: 700;
    color: var(--surface);
    background: var(--accent);
    border-radius: 999px;
    padding: 1px 7px;
    min-width: 15px;
    text-align: center;
  }
  .note {
    font-size: 11.5px;
    line-height: 1.5;
    color: var(--ink-tertiary);
    margin: 0;
    padding: 0 6px;
  }
  .note.warn {
    color: var(--danger);
  }
  .note.empty {
    padding: 10px 6px;
  }
  .group {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .group-head {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 0 6px;
  }
  .group-name {
    font-size: 12px;
    font-weight: 600;
    color: var(--ink-secondary);
  }
  .group-count {
    font-size: 10.5px;
    font-weight: 700;
    color: var(--ink-tertiary);
    background: var(--sunken);
    border-radius: 999px;
    padding: 0 6px;
  }
  .item {
    display: flex;
    flex-direction: column;
    gap: 5px;
    padding: 9px 10px;
    border-radius: 9px;
    border: 1px solid var(--border);
    background: var(--surface);
  }
  .item.malformed {
    border-color: color-mix(in srgb, var(--danger) 35%, var(--border));
  }
  .item-head {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .kind-icon {
    flex: none;
    width: 18px;
    height: 18px;
    border-radius: 5px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    background: var(--sunken);
    color: var(--ink-tertiary);
  }
  .kind-icon.kind-task {
    background: color-mix(in srgb, var(--accent) 16%, transparent);
    color: var(--accent-deep);
  }
  .item-title {
    flex: 1;
    min-width: 0;
    font-size: 12.5px;
    font-weight: 600;
    color: var(--ink);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .unread-dot {
    flex: none;
    width: 7px;
    height: 7px;
    border-radius: 4px;
    background: var(--accent);
  }
  .item-meta {
    display: flex;
    gap: 5px;
    font-size: 10.5px;
    color: var(--ink-tertiary);
  }
  .item-body {
    margin: 0;
    font-size: 11.5px;
    line-height: 1.5;
    color: var(--ink-secondary);
  }
  .item-body.warn {
    color: var(--danger);
  }
  .task-chip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    align-self: flex-start;
    font-size: 10.5px;
    font-weight: 500;
    padding: 2px 8px;
    border-radius: 999px;
    background: color-mix(in srgb, var(--accent) 10%, transparent);
    color: var(--accent-deep);
  }
  .soft {
    color: var(--ink-tertiary);
  }
  .item-actions {
    display: flex;
    gap: 6px;
    margin-top: 2px;
  }
  .btn-mini {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: 11px;
    font-weight: 500;
    padding: 4px 8px;
    border-radius: 6px;
    border: 1px solid var(--border-strong);
    background: var(--paper);
    color: var(--ink-secondary);
  }
  .btn-mini:hover {
    background: var(--sunken);
  }
  .btn-mini.primary {
    border-color: var(--accent-deep);
    background: var(--accent);
    color: var(--surface);
  }
  .btn-mini.primary:hover {
    background: var(--accent-hover);
  }
  .btn-mini.ghost {
    background: transparent;
  }
  .push-back {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-top: 2px;
  }
  .push-back textarea {
    font-family: inherit;
    font-size: 11.5px;
    padding: 6px 8px;
    border-radius: 6px;
    border: 1px solid var(--border-strong);
    background: var(--surface);
    color: var(--ink);
    resize: vertical;
  }
  .push-back-actions {
    display: flex;
    gap: 6px;
  }
</style>

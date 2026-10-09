<script lang="ts">
  // An Inbox item's kind as a tinted tile (or a bare glyph in a heading).
  import type { Component } from "svelte";
  import type { InboxItem } from "../lib/inbox.svelte";
  import ListTodo from "@lucide/svelte/icons/list-todo";
  import MessageSquare from "@lucide/svelte/icons/message-square";
  import Gavel from "@lucide/svelte/icons/gavel";
  import Ticket from "@lucide/svelte/icons/ticket";
  import FilePen from "@lucide/svelte/icons/file-pen";
  import FilePlus from "@lucide/svelte/icons/file-plus";
  import GitCompare from "@lucide/svelte/icons/git-compare";
  import FileX from "@lucide/svelte/icons/file-x";
  import CircleHelp from "@lucide/svelte/icons/circle-help";
  import Link2Off from "@lucide/svelte/icons/link-2-off";
  import Clock from "@lucide/svelte/icons/clock";
  import BookOpen from "@lucide/svelte/icons/book-open";
  import Flag from "@lucide/svelte/icons/flag";

  let { item, bare = false }: { item: InboxItem; bare?: boolean } = $props();

  const icon: Component = $derived.by(() => {
    switch (item.kind) {
      case "Task":
        return ListTodo;
      case "Message":
      case "Notice":
        return MessageSquare;
      case "Ruling":
        return Gavel;
      case "Escalation":
        return Flag;
      case "Ticket":
        return Ticket;
      case "Page edit":
        return FilePen;
      case "New page":
        return FilePlus;
      case "Conflict":
      case "Conflicted copy":
        return GitCompare;
      case "Couldn't read":
        return FileX;
      case "Question":
        return CircleHelp;
      case "Broken link":
        return Link2Off;
      case "Not checked lately":
        return Clock;
      default:
        return BookOpen;
    }
  });
  const Icon = $derived(icon);
</script>

{#if bare}
  <Icon size={15} strokeWidth={1.75} aria-hidden="true" />
{:else}
  <span class="tile {item.tone}"><Icon size={14} strokeWidth={1.75} aria-hidden="true" /></span>
{/if}

<style>
  .tile {
    width: 28px;
    height: 28px;
    flex: none;
    border-radius: 8px;
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .tile.attn {
    background: var(--attn-soft);
    color: var(--attn-ink);
  }
  .tile.danger {
    background: var(--danger-soft);
    color: var(--danger);
  }
  .tile.accent {
    background: var(--accent-soft);
    color: var(--accent-ink);
  }
  .tile.ink {
    background: var(--panel);
    color: var(--ink-2);
  }
</style>

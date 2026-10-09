<script lang="ts">
  // The header (the Briefing design): search or ask the team, then what the
  // app wants of you right now (a take running, an update ready), then Ask
  // Ken, which opens the chat panel on any screen. The team and the screens
  // are in the sidebar.
  import { app } from "../lib/app.svelte";
  import { chats } from "../lib/chats.svelte";
  import { updater } from "../lib/updater.svelte";
  import { scope } from "../lib/scope.svelte";
  import { record, recordClock } from "../lib/record.svelte";
  import { shortcut } from "../lib/platform";
  import Search from "@lucide/svelte/icons/search";
  import Sparkles from "@lucide/svelte/icons/sparkles";
  import RefreshCw from "@lucide/svelte/icons/refresh-cw";

  updater.start();

  const teamLabel = $derived(scope.team ?? app.workspace?.name ?? app.project?.name ?? "");
  const updateTitle = $derived.by(() => {
    if (updater.phase === "downloading") {
      return updater.progress === null
        ? "Downloading the update"
        : `Downloading the update: ${Math.round(updater.progress * 100)}%`;
    }
    return `Restart Ken to update to v${updater.version}`;
  });
</script>

<header data-tauri-drag-region>
  <button class="search" onclick={() => (app.searchOpen = true)}>
    <Search size={15} strokeWidth={1.75} aria-hidden="true" />
    <span class="hint">{teamLabel ? `Search or ask ${teamLabel}…` : "Search or ask…"}</span>
    <span class="kbd">{shortcut("mod+K")}</span>
  </button>

  <span class="grow" data-tauri-drag-region></span>

  {#if record.recording || record.transcribing}
    <!-- A take runs on whatever screen you are on; this opens Ingest. -->
    <button
      class="recording"
      class:paused={record.phase === "paused"}
      title="Recording a meeting. Open Ingest to stop it."
      onclick={() => (app.screen = "ingests")}
    >
      <span class="rec-dot"></span>
      {#if record.transcribing}
        Transcribing{record.transcribePct !== null ? ` ${record.transcribePct}%` : "…"}
      {:else if record.phase === "paused"}
        Paused {recordClock(record.elapsedMs)}
      {:else}
        Recording {recordClock(record.elapsedMs)}
      {/if}
    </button>
  {/if}

  {#if updater.phase === "downloading"}
    <span class="update" title={updateTitle}>
      <RefreshCw size={14} strokeWidth={1.75} aria-hidden="true" />
      Downloading update
    </span>
  {:else if updater.phase === "ready"}
    <button class="update" title={updateTitle} onclick={() => updater.restart()}>
      <RefreshCw size={14} strokeWidth={1.75} aria-hidden="true" />
      Update ready
    </button>
  {/if}

  <button
    class="ask"
    class:open={chats.open}
    aria-pressed={chats.open}
    onclick={() => (chats.open = !chats.open)}
    title={chats.open ? "Close Ask Ken" : "Ask Ken"}
  >
    <Sparkles size={15} strokeWidth={1.75} aria-hidden="true" />
    Ask Ken
    {#if chats.needsInput}
      <span class="need" title="Ken asked you something"></span>
    {/if}
  </button>
</header>

<style>
  header {
    height: 56px;
    flex: none;
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 0 24px;
    border-bottom: 1px solid var(--line-soft);
    background: var(--bg);
  }
  .search {
    flex: 1;
    max-width: 520px;
    height: 36px;
    border-radius: 10px;
    border: 1px solid var(--line);
    background: var(--surface);
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 0 12px;
    font-size: 13.5px;
    color: var(--ink-3);
    text-align: left;
  }
  .search:hover {
    border-color: var(--line-strong);
  }
  .hint {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .grow {
    flex: 1;
    align-self: stretch;
  }
  .update {
    flex: none;
    white-space: nowrap;
    height: 32px;
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 0 12px;
    border-radius: 9px;
    border: 1px solid var(--line);
    background: var(--surface);
    font-size: 13px;
    color: var(--ink-2);
  }
  button.update:hover {
    border-color: var(--line-strong);
    color: var(--ink);
  }
  .ask {
    flex: none;
    white-space: nowrap;
    height: 32px;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 0 12px;
    border-radius: 9px;
    border: 1px solid var(--accent);
    background: transparent;
    color: var(--accent-ink);
    font-size: 13px;
    font-weight: 600;
  }
  .ask:hover {
    background: var(--accent-soft);
  }
  .ask.open {
    background: var(--accent);
    color: var(--on-accent);
  }
  .need {
    width: 7px;
    height: 7px;
    border-radius: 4px;
    background: var(--attn);
  }
  .recording {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 7px;
    height: 32px;
    padding: 0 12px;
    border-radius: 16px;
    border: 1px solid var(--danger);
    background: var(--danger-soft);
    color: var(--danger);
    font-size: 13px;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }
  .rec-dot {
    width: 8px;
    height: 8px;
    border-radius: 4px;
    background: var(--danger);
    animation: pulse 1.2s ease-in-out infinite;
  }
  .recording.paused .rec-dot {
    animation: none;
    opacity: 0.5;
  }
  @keyframes pulse {
    0%,
    100% {
      opacity: 0.35;
    }
    50% {
      opacity: 1;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .rec-dot {
      animation: none;
    }
  }
</style>

<script lang="ts">
  import { onDestroy } from "svelte";
  import type { UnlistenFn } from "@tauri-apps/api/event";
  import { api, type ModelStatus } from "../../lib/api";
  import ProgressBar from "../../lib/ProgressBar.svelte";

  let {
    status,
    onInstalled,
    compact = false,
  }: {
    status: ModelStatus;
    /** Called once the model is verified and installed on disk. */
    onInstalled?: () => void;
    /** Tighter layout for embedding in a Settings row. */
    compact?: boolean;
  } = $props();

  type Phase = "idle" | "downloading" | "done" | "error";
  let phase = $state<Phase>("idle");
  let downloaded = $state(0);
  let total = $state(0);
  let message = $state<string | null>(null);

  // The download runs in the backend; progress and terminal completion (a 100%
  // sample, emitted only after a verified atomic install) arrive as events.
  let unlistenProgress: UnlistenFn | undefined;
  let unlistenError: UnlistenFn | undefined;

  const pct = $derived(
    total > 0 ? Math.min(100, Math.round((downloaded / total) * 100)) : 0,
  );

  function fmtBytes(n: number): string {
    if (n <= 0) return "";
    const mb = n / (1024 * 1024);
    if (mb >= 1024) return `${(mb / 1024).toFixed(1)} GB`;
    return `${Math.round(mb)} MB`;
  }

  async function subscribe() {
    unlistenProgress = await api.onModelDownloadProgress((ev) => {
      if (ev.id !== status.id) return;
      downloaded = ev.downloaded;
      total = ev.total;
      // The backend emits 100% only after a successful install, so it's the
      // unambiguous completion signal.
      if (ev.total > 0 && ev.downloaded >= ev.total) {
        phase = "done";
        onInstalled?.();
      }
    });
    unlistenError = await api.onModelDownloadError((ev) => {
      if (ev.id !== status.id) return;
      phase = "error";
      message = ev.message;
    });
  }

  function teardown() {
    unlistenProgress?.();
    unlistenError?.();
    unlistenProgress = undefined;
    unlistenError = undefined;
  }

  async function start() {
    phase = "downloading";
    downloaded = 0;
    total = status.expectedBytes;
    message = null;
    if (!unlistenProgress) await subscribe();
    try {
      await api.downloadModel(status.id);
    } catch (e) {
      // A refused start (e.g. already downloading) — surface it, don't hang.
      phase = "error";
      message = `${e}`;
    }
  }

  onDestroy(teardown);
</script>

<div class="model" class:compact>
  <div class="head">
    <div class="meta">
      <span class="name">{status.name}</span>
      {#if status.recommended}<span class="badge">Recommended</span>{/if}
      <span class="size">
        {phase === "downloading" && total > 0
          ? `${fmtBytes(downloaded)} / ${fmtBytes(total)}`
          : fmtBytes(status.expectedBytes)}
      </span>
    </div>
    {#if phase === "idle" || phase === "error"}
      <button class="dl" onclick={start}>
        {phase === "error" ? "Retry" : "Download"}
      </button>
    {:else if phase === "done"}
      <span class="ok">Installed</span>
    {/if}
  </div>

  {#if phase === "downloading"}
    <ProgressBar
      {pct}
      label={pct >= 100 ? "Installing…" : `Downloading… ${pct}%`}
    />
  {:else if phase === "error" && message}
    <p class="status error">{message}</p>
  {/if}
</div>

<style>
  .model {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .meta {
    display: flex;
    align-items: baseline;
    gap: 8px;
    flex-wrap: wrap;
    min-width: 0;
  }
  .name {
    font-size: 13px;
    font-weight: 500;
    color: var(--ink);
  }
  .badge {
    font-size: 10px;
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 0 5px;
    color: var(--accent);
  }
  .size {
    font-size: 12px;
    color: var(--ink-tertiary);
    font-variant-numeric: tabular-nums;
  }
  .dl {
    margin-left: auto;
    flex: none;
    padding: 6px 14px;
    border: 1px solid var(--border-strong);
    border-radius: 8px;
    background: var(--accent);
    color: white;
    font: inherit;
    font-size: 13px;
    cursor: pointer;
  }
  .dl:hover {
    background: var(--accent-hover);
  }
  .ok {
    margin-left: auto;
    flex: none;
    font-size: 12px;
    font-weight: 600;
    color: var(--healthy-text);
  }
  .status {
    margin: 0;
    font-size: 12px;
    color: var(--ink-tertiary);
  }
  .status.error {
    color: var(--danger);
  }
  .compact .name {
    font-weight: 400;
  }
</style>

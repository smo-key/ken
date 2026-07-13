<script lang="ts">
  import { app } from "../lib/app.svelte";
  import { chats } from "../lib/chats.svelte";
  import { timeAgo } from "../lib/format";
  import ProjectSwitcher from "./ProjectSwitcher.svelte";

  let switcherOpen = $state(false);

  const initial = $derived(app.project?.name?.charAt(0).toUpperCase() ?? "?");
  const syncTitle = $derived(
    app.scanning
      ? "Indexing…"
      : app.lastScanAt
        ? `Watching · updated ${timeAgo(Math.floor(app.lastScanAt / 1000))}`
        : "Watching",
  );
</script>

<header data-tauri-drag-region>
  <!-- Space for the native macOS traffic lights (titleBarStyle: Overlay) -->
  <div class="traffic-space" data-tauri-drag-region></div>

  <button class="project" onclick={() => (switcherOpen = !switcherOpen)}>
    <span class="badge">{initial}</span>
    {app.project?.name}
    <span
      class="dot"
      class:busy={app.scanning}
      class:error={app.scanError !== null}
      title={app.scanError ?? syncTitle}
    ></span>
    <span class="chev">▾</span>
  </button>

  <button class="search" onclick={() => (app.searchOpen = true)}>
    <span class="lens" aria-hidden="true"></span>
    <span class="hint">Search project knowledge…</span>
    <span class="kbd">⌘K</span>
  </button>

  <button
    class="chats"
    class:open={chats.open}
    onclick={() => (chats.open = !chats.open)}
    title={chats.open ? "Close chats" : "Open chats"}
  >
    ◈ Chats
    {#if chats.needsInput}
      <span class="need-dot"></span>
    {/if}
  </button>

  {#if switcherOpen}
    <ProjectSwitcher close={() => (switcherOpen = false)} />
  {/if}
</header>

<style>
  header {
    flex: none;
    height: 52px;
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 0 18px;
    border-bottom: 1px solid var(--border);
    background: var(--sunken);
    position: relative;
    z-index: 30;
  }
  .traffic-space {
    width: 62px;
    flex: none;
  }
  .project {
    display: flex;
    align-items: center;
    gap: 8px;
    height: 32px;
    padding: 0 11px;
    border-radius: 8px;
    border: 1px solid var(--border-strong);
    background: var(--surface);
    font-size: 13px;
    font-weight: 600;
    color: var(--ink);
    flex: none;
    box-shadow: var(--shadow-control);
  }
  .project:hover {
    background: var(--paper);
  }
  .badge {
    width: 20px;
    height: 20px;
    border-radius: 6px;
    background: var(--ink);
    color: var(--paper);
    display: flex;
    align-items: center;
    justify-content: center;
    font-family: var(--font-serif);
    font-size: 11px;
  }
  .dot {
    width: 6px;
    height: 6px;
    border-radius: 3px;
    background: var(--healthy);
  }
  .dot.busy {
    background: var(--accent);
    animation: pulse 1.2s ease-in-out infinite;
  }
  .dot.error {
    background: var(--danger);
  }
  @keyframes pulse {
    50% {
      opacity: 0.35;
    }
  }
  .chev {
    color: var(--ink-tertiary);
    font-size: 10px;
  }
  .search {
    flex: 1;
    max-width: 560px;
    margin: 0 auto;
    height: 34px;
    border: 1px solid var(--border-strong);
    border-radius: 9px;
    background: var(--surface);
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 0 14px;
    box-shadow: var(--shadow-card);
    cursor: text;
    font-size: 13px;
  }
  .lens {
    width: 12px;
    height: 12px;
    border: 1.5px solid var(--ink-tertiary);
    border-radius: 50%;
    position: relative;
    flex: none;
  }
  .lens::after {
    content: "";
    position: absolute;
    width: 1.5px;
    height: 5px;
    background: var(--ink-tertiary);
    transform: rotate(-45deg);
    top: 9px;
    left: 11px;
  }
  .hint {
    color: var(--ink-tertiary);
    flex: 1;
    text-align: left;
  }
  .chats {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 7px;
    height: 28px;
    padding: 0 11px;
    border-radius: 8px;
    border: 1px solid rgba(138, 90, 68, 0.35);
    background: rgba(138, 90, 68, 0.08);
    color: var(--accent-deep);
    font-size: 12.5px;
    font-weight: 600;
    cursor: pointer;
  }
  .chats:hover,
  .chats.open {
    background: rgba(138, 90, 68, 0.16);
  }
  .need-dot {
    width: 7px;
    height: 7px;
    border-radius: 4px;
    background: var(--needs-input);
  }
</style>

<script lang="ts">
  import { onMount } from "svelte";
  import { open as openDialog } from "@tauri-apps/plugin-dialog";
  import { api, type RecentWorkspace } from "../lib/api";
  import { timeAgo } from "../lib/format";
  import { app } from "../lib/app.svelte";
  import KenMark from "../lib/ui/KenMark.svelte";
  import FolderOpen from "@lucide/svelte/icons/folder-open";
  import Trash2 from "@lucide/svelte/icons/trash-2";
  import { openContextMenu } from "../lib/ui/ContextMenu.svelte";
  import SetupFlow from "./SetupFlow.svelte";

  // Ken works in workspaces: this screen opens one it knows, or sets up a
  // new one (Folder, Team, Repos, Index).
  let error = $state<string | null>(null);
  let setupOpen = $state(false);

  // Workspaces opened before, newest first: open one again in a click.
  let recentWorkspaces = $state<RecentWorkspace[]>([]);
  async function loadRecentWorkspaces() {
    try {
      recentWorkspaces = await api.listRecentWorkspaces();
    } catch {
      recentWorkspaces = [];
    }
  }
  async function openRecentWorkspace(ws: RecentWorkspace) {
    if (!ws.available) return;
    error = null;
    try {
      await app.openWorkspace(ws.path);
    } catch (e) {
      error = String(e);
    }
  }
  // A folder picked to open that holds no workspace: said plainly, with the
  // way to make one.
  let noWorkspaceAt = $state<string | null>(null);
  async function openWorkspaceFolder() {
    error = null;
    noWorkspaceAt = null;
    const folder = await openDialog({ directory: true, title: "Choose the workspace's folder" });
    if (typeof folder !== "string") return;
    try {
      await app.openWorkspace(folder);
    } catch (e) {
      if (String(e).includes(".ken-workspace")) noWorkspaceAt = folder;
      else error = String(e);
    }
  }
  async function forgetWorkspace(id: string) {
    await api.forgetWorkspace(id);
    await loadRecentWorkspaces();
  }
  function workspaceMenu(e: MouseEvent, ws: RecentWorkspace) {
    e.preventDefault();
    const x = e.clientX;
    const y = e.clientY;
    openContextMenu(x, y, [
      { label: "Open", icon: FolderOpen, disabled: !ws.available, onSelect: () => openRecentWorkspace(ws) },
      "separator",
      {
        label: "Remove from this list",
        icon: Trash2,
        danger: true,
        onSelect: () => void forgetWorkspace(ws.id),
      },
    ]);
  }

  onMount(() => {
    void loadRecentWorkspaces();
  });
</script>

<div class="wrap" data-tauri-drag-region>
  <div class="panel">
    <div class="brand">
      <KenMark size={36} />
      <span class="wordmark">Ken</span>
    </div>
    <h1>Your team's knowledge, in one calm place.</h1>
    <p class="lede">
      Point Ken at a folder — notes, documents, spreadsheets, anything. Ken
      reads it, keeps watch, and makes every fact findable.
    </p>

    {#if setupOpen}
      <SetupFlow onclose={() => (setupOpen = false)} />
    {:else}
      <!-- Ken works in workspaces: open one Ken already knows, or set up a
           new one. Set-up is offered on a fresh install too: confirming it
           turns the workspace feature on. -->
      <div class="choose-row stacked">
        <button class="choice" onclick={openWorkspaceFolder}>
          <span class="choice-title">Open a workspace</span>
          <span class="choice-note">
            Pick the folder of a workspace set up before, on this machine or
            by a teammate.
          </span>
        </button>
        <button class="choice" onclick={() => (setupOpen = true)}>
          <span class="choice-title">Set up a new workspace</span>
          <span class="choice-note">
            Pick the repos Ken should know, wherever they live. It proposes what
            each is, its team and how deep to read it; nothing is written until
            you confirm.
          </span>
        </button>
      </div>
      {#if noWorkspaceAt}
        <div class="error">
          No Ken workspace in {noWorkspaceAt}.
          <button class="btn btn-ghost" onclick={() => { noWorkspaceAt = null; setupOpen = true; }}>Set one up</button>
        </div>
      {/if}
    {/if}

    {#if error}
      <div class="error">{error}</div>
    {/if}

    {#if recentWorkspaces.length > 0 && !setupOpen}
      <div class="recent-label">Recent workspaces</div>
      <div class="recents">
        {#each recentWorkspaces as ws (ws.id)}
          <button
            class="recent"
            class:unavailable={!ws.available}
            onclick={() => openRecentWorkspace(ws)}
            oncontextmenu={(e) => workspaceMenu(e, ws)}
          >
            <span class="badge">{ws.name.charAt(0).toUpperCase()}</span>
            <span class="info">
              <span class="name">{ws.name}</span>
              <span class="mono path-small">{ws.path}</span>
              {#if ws.available}
                <span class="path-small">{timeAgo(ws.openedAt)}</span>
              {:else}
                <span class="missing">Folder not found</span>
              {/if}
            </span>
            {#if !ws.available}
              <span class="forget" role="button" tabindex="0" onclick={(e) => { e.stopPropagation(); void forgetWorkspace(ws.id); }} onkeydown={() => {}}>Remove</span>
            {/if}
          </button>
        {/each}
      </div>
    {/if}
  </div>
</div>

<style>
  .wrap {
    height: 100vh;
    display: flex;
    /* A too-tall panel (Features expanded, long candidate list) must stay
       reachable: `align-items: center` alone clips it at BOTH ends with no
       scroll. `safe center` degrades to `flex-start` once it would overflow;
       the plain `center` above it is the fallback for engines without it. */
    align-items: center;
    align-items: safe center;
    justify-content: center;
    overflow-y: auto;
    /* Gentle lined paper: faint rules every 28px on the paper ground. */
    background:
      repeating-linear-gradient(
        to bottom,
        transparent,
        transparent 27px,
        var(--rule-line) 27px,
        var(--rule-line) 28px
      ),
      var(--paper);
  }
  .panel {
    width: 460px;
    max-width: 100%;
    /* Never let the centering flexbox compress the panel — it scrolls instead. */
    flex: none;
    display: flex;
    flex-direction: column;
    gap: 14px;
    padding: 32px;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .wordmark {
    font-family: var(--font-script);
    font-size: 26px;
    line-height: 1;
    color: var(--ink);
    /* Script baseline sits low; nudge up so it aligns with the mark. */
    transform: translateY(-3px);
  }
  h1 {
    margin: 6px 0 0;
    font-family: var(--font-serif);
    font-size: 30px;
    font-weight: 500;
    line-height: 1.2;
    letter-spacing: -0.01em;
  }
  .lede {
    margin: 0;
    font-size: 14px;
    line-height: 1.7;
    color: var(--ink-secondary);
  }
  .choose-row {
    display: flex;
    gap: 10px;
    align-items: center;
  }
  /* Two real options: stacked cards, equal weight, so neither wins by
     emphasis. */
  .choose-row.stacked {
    flex-direction: column;
    align-items: stretch;
  }
  .choice {
    display: flex;
    flex-direction: column;
    gap: 3px;
    width: 100%;
    text-align: left;
    padding: 12px 14px;
    background: var(--surface);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-control);
    box-shadow: var(--shadow-control);
    font: inherit;
    cursor: pointer;
    color: var(--ink);
  }
  .choice:hover {
    border-color: var(--accent);
  }
  .choice:focus-visible {
    border-color: var(--accent);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 12%, transparent);
    outline: none;
  }
  .choice-title {
    font-size: 14px;
    font-weight: 600;
  }
  .choice-note {
    font-size: 12px;
    line-height: 1.5;
    color: var(--ink-tertiary);
  }
  .error {
    font-size: 13px;
    color: var(--danger);
    background: color-mix(in srgb, var(--danger) 7%, transparent);
    border: 1px solid color-mix(in srgb, var(--danger) 25%, transparent);
    border-radius: 10px;
    padding: 10px 14px;
  }
  .recent-label {
    font-size: 11px;
    font-weight: 700;
    color: var(--ink-tertiary);
    letter-spacing: 0.08em;
    text-transform: uppercase;
    margin-top: 10px;
  }
  .recents {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .recent {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 9px 10px;
    border-radius: 9px;
    border: 1px solid var(--border);
    background: var(--surface);
    text-align: left;
    font-size: 13px;
  }
  .recent:hover {
    background: var(--sunken);
  }
  .recent.unavailable {
    opacity: 0.7;
  }
  .badge {
    width: 26px;
    height: 26px;
    border-radius: 7px;
    background: var(--ink);
    color: var(--paper);
    display: flex;
    align-items: center;
    justify-content: center;
    font-family: var(--font-serif);
    font-size: 13px;
    flex: none;
  }
  .info {
    display: flex;
    flex-direction: column;
    min-width: 0;
    flex: 1;
  }
  .name {
    font-weight: 600;
  }
  .path-small {
    font-size: 11px;
    color: var(--ink-tertiary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .missing {
    font-size: 11.5px;
    color: var(--danger);
  }
  .forget {
    font-size: 12px;
    font-weight: 600;
    color: var(--danger);
  }
</style>

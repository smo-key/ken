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
  // new one (its name, where the knowledge base is saved, its sources).
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
    <!-- The brand as the sidebar shows it: the K on its ink tile, the name in
         the interface's sans. -->
    <div class="brand">
      <KenMark size={30} />
      <span class="t-heading">Ken</span>
    </div>
    <h1 class="t-title">Your team's knowledge, in one calm place.</h1>

    {#if setupOpen}
      <SetupFlow onclose={() => (setupOpen = false)} />
    {:else}
      <!-- Ken works in workspaces: open one Ken already knows, or set up a
           new one. Set-up is offered on a fresh install too: confirming it
           turns the workspace feature on. Two real options: stacked cards,
           equal weight, so neither wins by emphasis. -->
      <div class="choices">
        <button class="k-card choice" onclick={openWorkspaceFolder}>
          <span class="t-heading">Open a workspace</span>
        </button>
        <button class="k-card choice" onclick={() => (setupOpen = true)}>
          <span class="t-heading">Set up a new workspace</span>
        </button>
      </div>
      {#if noWorkspaceAt}
        <div class="k-notice danger">
          <span class="grow">No Ken workspace in {noWorkspaceAt}.</span>
          <button class="btn btn-small" onclick={() => { noWorkspaceAt = null; setupOpen = true; }}>Set one up</button>
        </div>
      {/if}
    {/if}

    {#if error}
      <div class="k-notice danger">{error}</div>
    {/if}

    {#if recentWorkspaces.length > 0 && !setupOpen}
      <div class="t-label recent-label">Recent workspaces</div>
      <div class="k-card recents">
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
              <span class="mono path">{ws.path}</span>
              {#if ws.available}
                <span class="t-small">{timeAgo(ws.openedAt)}</span>
              {:else}
                <span class="t-small missing">Folder not found</span>
              {/if}
            </span>
            {#if !ws.available}
              <span class="btn btn-small btn-danger" role="button" tabindex="0" onclick={(e) => { e.stopPropagation(); void forgetWorkspace(ws.id); }} onkeydown={() => {}}>Remove</span>
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
    /* The app's own ground, as every screen behind the sidebar has. */
    background: var(--bg);
  }
  .panel {
    width: 460px;
    max-width: 100%;
    /* Never let the centering flexbox compress the panel — it scrolls instead. */
    flex: none;
    display: flex;
    flex-direction: column;
    gap: 16px;
    padding: 32px;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  h1 {
    margin: 8px 0 0;
  }
  .choices {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .choice {
    display: flex;
    flex-direction: column;
    gap: 4px;
    width: 100%;
    padding: 14px 16px;
    font: inherit;
    text-align: left;
    color: var(--ink);
  }
  .choice:hover {
    border-color: var(--line-strong);
  }
  .choice:focus-visible {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
  .grow {
    flex: 1;
    min-width: 0;
  }
  .recent-label {
    margin-top: 8px;
  }
  .recents {
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
  .recent {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 10px 12px;
    border-bottom: 1px solid var(--line-soft);
    font: inherit;
    text-align: left;
    color: var(--ink);
  }
  .recent:last-child {
    border-bottom: none;
  }
  .recent:hover {
    background: color-mix(in oklch, var(--line-soft) 55%, transparent);
  }
  .recent:focus-visible {
    outline: none;
    box-shadow: inset 0 0 0 2px var(--accent);
  }
  .recent.unavailable {
    opacity: 0.7;
  }
  /* The workspace's initial, as the workspace switcher draws it. */
  .badge {
    width: 26px;
    height: 26px;
    border-radius: 7px;
    background: var(--ink);
    color: var(--bg);
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
    font-size: 13.5px;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .path {
    font-size: 11.5px;
    color: var(--ink-3);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .missing {
    color: var(--danger);
  }
</style>

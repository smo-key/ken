<script lang="ts">
  // The sidebar's team switcher: the teams in this workspace, each chosen on
  // its own (there is no "all teams"), then the other workspaces opened
  // recently, and a way to open or set up another. Choosing a team scopes
  // questions, search and Home to it; its repos are picked inside Files.
  import { onMount } from "svelte";
  import { app } from "../lib/app.svelte";
  import { scope } from "../lib/scope.svelte";
  import { api, type RecentWorkspace } from "../lib/api";
  import { timeAgo } from "../lib/format";
  import FolderOpen from "@lucide/svelte/icons/folder-open";

  let { close }: { close: () => void } = $props();

  // The other workspaces opened recently: switch to one without going back
  // to the start screen.
  let others = $state<RecentWorkspace[]>([]);
  onMount(() => {
    api.listRecentWorkspaces().then(
      (list) => (others = list.filter((w) => w.available && w.path !== app.workspace?.root).slice(0, 5)),
      () => (others = []),
    );
  });

  async function pick(name: string) {
    close();
    await scope.selectTeam(name);
  }

  async function switchTo(ws: RecentWorkspace) {
    close();
    await app.closeWorkspaceSession();
    await app.openWorkspace(ws.path);
  }

  // The start screen has Open a workspace, Set up a new workspace and the
  // full recent list.
  async function openAnother() {
    close();
    await app.closeWorkspaceSession();
  }
</script>

<button class="scrim" onclick={close} aria-label="Close team switcher"></button>
<div class="menu" role="menu">
  <div class="head" title={app.workspace?.root}>{app.workspace?.name ?? "This workspace"}</div>
  {#if scope.groups.length === 0}
    <div class="empty">No team yet. Set-up puts each repo on a team.</div>
  {:else}
    {#each scope.groups as team (team.name)}
      <button class="row" class:current={team.name === scope.team} role="menuitem" onclick={() => pick(team.name)}>
        <span class="badge">{team.name.charAt(0).toUpperCase()}</span>
        <span class="info">
          <span class="name">{team.name}</span>
          <span class="sub">{team.projectIds.length} {team.projectIds.length === 1 ? "repo" : "repos"}</span>
        </span>
      </button>
    {/each}
  {/if}

  {#if others.length > 0}
    <div class="sep"></div>
    <div class="head">Other workspaces</div>
    {#each others as ws (ws.id)}
      <button class="row" role="menuitem" onclick={() => switchTo(ws)} title={ws.path}>
        <span class="badge quiet">{ws.name.charAt(0).toUpperCase()}</span>
        <span class="info">
          <span class="name">{ws.name}</span>
          <span class="sub">Opened {timeAgo(ws.openedAt)}</span>
        </span>
      </button>
    {/each}
  {/if}

  <div class="sep"></div>
  <button class="row foot" role="menuitem" onclick={openAnother}>
    <span class="icon"><FolderOpen size={15} strokeWidth={1.75} aria-hidden="true" /></span>
    <span class="name">Open or set up another workspace</span>
  </button>
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: transparent;
    border: none;
    z-index: 39;
  }
  .menu {
    position: fixed;
    top: 64px;
    left: 10px;
    width: 280px;
    max-height: calc(100vh - 80px);
    overflow-y: auto;
    background: var(--surface);
    border: 1px solid var(--line);
    border-radius: var(--radius-overlay);
    box-shadow: var(--shadow-overlay);
    padding: 6px;
    z-index: 40;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .head {
    padding: 6px 10px 4px;
    font-size: 10.5px;
    font-weight: 700;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--ink-3);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .sep {
    height: 1px;
    background: var(--line-soft);
    margin: 4px 2px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 10px;
    border-radius: 9px;
    border: none;
    background: transparent;
    text-align: left;
    color: var(--ink);
    flex: none;
  }
  .row:hover {
    background: var(--line-soft);
  }
  .row.current {
    background: var(--accent-soft);
  }
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
  .badge.quiet {
    background: var(--line-soft);
    color: var(--ink-2);
  }
  .icon {
    width: 26px;
    display: flex;
    justify-content: center;
    color: var(--ink-3);
    flex: none;
  }
  .info {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .name {
    font-size: 13px;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .foot .name {
    font-weight: 500;
    color: var(--ink-2);
  }
  .sub {
    font-size: 11.5px;
    color: var(--ink-3);
  }
  .empty {
    padding: 6px 10px 10px;
    font-size: 12.5px;
    color: var(--ink-2);
  }
</style>

<script lang="ts">
  // The sidebar (the Briefing design): the team at the top with how its sync
  // stands, then Home, Inbox, Files, Ingest and Explore, with Settings and
  // Collapse at the foot. Counts are amber only when they count things
  // waiting on you; a teal dot means something new with nothing to do.
  import type { Component } from "svelte";
  import { app, type Screen } from "../lib/app.svelte";
  import { scope } from "../lib/scope.svelte";
  import { inbox } from "../lib/inbox.svelte";
  import { rail } from "../lib/rail.svelte";
  import { record } from "../lib/record.svelte";
  import { timeAgo } from "../lib/format";
  import { isMac } from "../lib/platform";
  import TeamSwitcher from "./TeamSwitcher.svelte";
  import House from "@lucide/svelte/icons/house";
  import Inbox from "@lucide/svelte/icons/inbox";
  import FolderOpen from "@lucide/svelte/icons/folder-open";
  import FileInput from "@lucide/svelte/icons/file-input";
  import Compass from "@lucide/svelte/icons/compass";
  import Settings from "@lucide/svelte/icons/settings";
  import PanelLeftClose from "@lucide/svelte/icons/panel-left-close";
  import PanelLeftOpen from "@lucide/svelte/icons/panel-left-open";
  import ChevronsUpDown from "@lucide/svelte/icons/chevrons-up-down";

  const KEY = "ken.sidebar.collapsed";
  function readCollapsed(): boolean {
    try {
      return localStorage.getItem(KEY) === "1";
    } catch {
      return false;
    }
  }
  let collapsed = $state(readCollapsed());
  function toggle() {
    collapsed = !collapsed;
    try {
      localStorage.setItem(KEY, collapsed ? "1" : "0");
    } catch {
      // per-viewer convenience only
    }
  }

  let switcherOpen = $state(false);

  $effect(() => {
    if (app.workspace?.id) void scope.refreshGroups();
  });

  const teamName = $derived(scope.team ?? app.workspace?.name ?? app.project?.name ?? "Ken");

  /** How sync stands, in a few words, and its colour. */
  const status = $derived.by((): { text: string; tone: "ok" | "attn" | "danger" | "muted" } => {
    if (app.scanError) return { text: "Couldn't index", tone: "danger" };
    if (app.syncState === "attention") return { text: "A conflict waits", tone: "attn" };
    if (app.syncState === "syncing") return { text: "Syncing…", tone: "muted" };
    if (app.scanning) return { text: "Indexing…", tone: "muted" };
    if (app.syncState === "synced") return { text: "Synced", tone: "ok" };
    return {
      text: app.lastScanAt ? `Up to date · ${timeAgo(Math.floor(app.lastScanAt / 1000))}` : "Watching",
      tone: "ok",
    };
  });

  /** Ingest's note while a source is being read or a take transcribed. */
  const ingestNote = $derived(
    record.transcribing && record.transcribePct !== null ? `${record.transcribePct}%` : record.recording ? "Rec" : "",
  );

  const items: { key: Screen; icon: Component; label: string }[] = [
    { key: "home", icon: House, label: "Home" },
    { key: "inbox", icon: Inbox, label: "Inbox" },
    { key: "files", icon: FolderOpen, label: "Files" },
    { key: "ingests", icon: FileInput, label: "Ingest" },
    { key: "explore", icon: Compass, label: "Explore" },
  ];
</script>

<aside class:collapsed data-tauri-drag-region>
  {#if isMac}
    <!-- Room for the native traffic lights (titleBarStyle: Overlay). -->
    <div class="traffic" data-tauri-drag-region></div>
  {/if}

  <button class="team" onclick={() => (switcherOpen = !switcherOpen)} title="Switch team">
    <span class="k">K</span>
    {#if !collapsed}
      <span class="tinfo">
        <span class="tname">{teamName}</span>
        <span class="tstatus {status.tone}"><span class="sdot"></span>{status.text}</span>
      </span>
      <ChevronsUpDown size={14} strokeWidth={1.75} aria-hidden="true" />
    {/if}
  </button>

  {#each items as item (item.key)}
    {@const Icon = item.icon}
    {@const on = app.screen === item.key}
    <button class="nav" class:on onclick={() => (app.screen = item.key)} title={item.label} aria-current={on ? "page" : undefined}>
      <Icon size={17} strokeWidth={1.75} aria-hidden="true" />
      {#if !collapsed}<span class="label">{item.label}</span>{/if}
      {#if item.key === "inbox" && inbox.count > 0}
        <span class="count" class:float={collapsed} title="{inbox.count} waiting on you">{inbox.count}</span>
      {/if}
      {#if item.key === "files" && app.unread.length > 0 && !on}
        <span class="dot-new" class:float={collapsed} title="{app.unread.length} files changed since you last looked"></span>
      {/if}
      {#if item.key === "ingests"}
        {#if ingestNote && !collapsed}
          <span class="note">{ingestNote}</span>
        {:else if rail.ingestWaiting > 0}
          <span class="count" class:float={collapsed} title="{rail.ingestWaiting} {rail.ingestWaiting === 1 ? 'source has' : 'sources have'} something waiting">{rail.ingestWaiting}</span>
        {/if}
      {/if}
    </button>
  {/each}

  <span class="grow" data-tauri-drag-region></span>

  <button class="nav" class:on={app.screen === "settings"} onclick={() => app.openSettings()} title="Settings">
    <Settings size={17} strokeWidth={1.75} aria-hidden="true" />
    {#if !collapsed}<span class="label">Settings</span>{/if}
  </button>
  <button class="nav quiet" onclick={toggle} title={collapsed ? "Expand" : "Collapse"}>
    {#if collapsed}
      <PanelLeftOpen size={17} strokeWidth={1.75} aria-hidden="true" />
    {:else}
      <PanelLeftClose size={17} strokeWidth={1.75} aria-hidden="true" />
      <span class="label">Collapse</span>
    {/if}
  </button>

  {#if switcherOpen}
    <TeamSwitcher close={() => (switcherOpen = false)} />
  {/if}
</aside>

<style>
  aside {
    width: 232px;
    flex: none;
    background: var(--panel);
    border-right: 1px solid var(--line);
    display: flex;
    flex-direction: column;
    padding: 14px 10px;
    gap: 2px;
    box-sizing: border-box;
    transition: width 0.18s ease-out;
    overflow: hidden;
  }
  aside.collapsed {
    width: 62px;
  }
  .traffic {
    height: 22px;
    flex: none;
  }
  .team {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 7px;
    border-radius: 10px;
    border: 1px solid var(--line);
    background: var(--surface);
    text-align: left;
    margin-bottom: 12px;
    color: var(--ink);
    min-width: 0;
  }
  .team:hover {
    border-color: var(--line-strong);
  }
  .team :global(svg) {
    color: var(--ink-3);
    flex: none;
  }
  .k {
    width: 30px;
    height: 30px;
    border-radius: 8px;
    background: var(--ink);
    color: var(--bg);
    display: flex;
    align-items: center;
    justify-content: center;
    font-family: var(--font-script);
    font-size: 19px;
    flex: none;
  }
  .tinfo {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    white-space: nowrap;
  }
  .tname {
    font-size: 13.5px;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .tstatus {
    font-size: 11.5px;
    display: flex;
    align-items: center;
    gap: 5px;
    color: var(--ink-3);
  }
  .tstatus .sdot {
    width: 6px;
    height: 6px;
    border-radius: 3px;
    background: currentColor;
    flex: none;
  }
  .tstatus.ok {
    color: var(--ok);
  }
  .tstatus.attn {
    color: var(--attn-ink);
  }
  .tstatus.danger {
    color: var(--danger);
  }
  .nav {
    display: flex;
    align-items: center;
    gap: 10px;
    height: 36px;
    padding: 0 11px;
    border-radius: 8px;
    border: none;
    background: transparent;
    color: var(--ink-2);
    font-size: 13.5px;
    font-weight: 500;
    text-align: left;
    position: relative;
    white-space: nowrap;
    flex: none;
  }
  .nav :global(svg) {
    flex: none;
  }
  .nav:hover {
    background: var(--line-soft);
  }
  .nav.on {
    background: var(--accent-soft);
    color: var(--accent-ink);
    font-weight: 600;
  }
  .nav.quiet {
    color: var(--ink-3);
    font-size: 13px;
  }
  .label {
    flex: 1;
  }
  .note {
    font-size: 11.5px;
    color: var(--accent-ink);
    font-variant-numeric: tabular-nums;
  }
  .float {
    position: absolute;
    top: 2px;
    right: 2px;
  }
  .dot-new.float {
    top: 6px;
    right: 8px;
  }
  .grow {
    flex: 1;
  }
</style>

<script lang="ts">
  import type { Component } from "svelte";
  import { app, type Screen } from "../lib/app.svelte";
  import { review } from "../lib/review.svelte";
  import LayoutGrid from "@lucide/svelte/icons/layout-grid";
  import Files from "@lucide/svelte/icons/files";
  import SquareCheck from "@lucide/svelte/icons/square-check";
  import Layers from "@lucide/svelte/icons/layers";
  import Network from "@lucide/svelte/icons/network";
  import Clock from "@lucide/svelte/icons/clock";
  import Settings from "@lucide/svelte/icons/settings";

  const items: { key: Screen; icon: Component; label: string }[] = [
    { key: "home", icon: LayoutGrid, label: "Home" },
    { key: "files", icon: Files, label: "Files" },
    { key: "review", icon: SquareCheck, label: "Review" },
    { key: "ingests", icon: Layers, label: "Ingests" },
    { key: "map", icon: Network, label: "Map" },
    { key: "timeline", icon: Clock, label: "Timeline" },
  ];
</script>

<nav>
  {#each items as item (item.key)}
    {@const Icon = item.icon}
    <button
      class:active={app.screen === item.key}
      onclick={() => (app.screen = item.key)}
      title={item.label}
    >
      <span class="icon"><Icon size={16} strokeWidth={1.75} /></span>{item.label}
      {#if item.key === "files" && app.failedFiles.length > 0}
        <span class="dot" title="{app.failedFiles.length} files could not be indexed"></span>
      {/if}
      {#if item.key === "review" && review.count > 0}
        <span class="count" title="{review.count} things are waiting on you">{review.count}</span>
      {/if}
    </button>
  {/each}
  <button
    class="settings"
    class:active={app.screen === "settings"}
    onclick={() => (app.screen = "settings")}
    title="Settings"
  >
    <span class="icon"><Settings size={16} strokeWidth={1.75} /></span>Settings
  </button>
</nav>

<style>
  nav {
    width: 64px;
    flex: none;
    border-right: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    align-items: center;
    padding: 14px 0;
    gap: 6px;
  }
  button {
    width: 44px;
    height: 42px;
    border-radius: 10px;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 2px;
    color: var(--ink-tertiary);
    font-size: 9px;
    position: relative;
    border: none;
    background: transparent;
  }
  button:hover {
    background: var(--sunken);
    color: var(--ink-secondary);
  }
  button.active {
    background: rgba(138, 90, 68, 0.12);
    color: var(--accent-deep);
    font-weight: 600;
  }
  .icon {
    font-size: 14px;
  }
  .dot {
    position: absolute;
    top: 3px;
    right: 5px;
    width: 7px;
    height: 7px;
    border-radius: 4px;
    background: var(--danger);
    border: 1.5px solid var(--paper);
  }
  .count {
    position: absolute;
    top: 2px;
    right: 2px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: 15px;
    height: 15px;
    padding: 0 3px;
    border-radius: 8px;
    background: var(--danger);
    color: var(--surface);
    font-size: 9px;
    font-weight: 700;
    border: 1.5px solid var(--paper);
  }
  .settings {
    margin-top: auto;
  }
</style>

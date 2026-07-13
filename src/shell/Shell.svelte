<script lang="ts">
  import { app } from "../lib/app.svelte";
  import TitleBar from "./TitleBar.svelte";
  import NavRail from "./NavRail.svelte";
  import SearchOverlay from "../search/SearchOverlay.svelte";
  import ChatDrawer from "../chat/ChatDrawer.svelte";
  import { chats } from "../lib/chats.svelte";
  import HomeScreen from "../screens/HomeScreen.svelte";
  import FilesScreen from "../screens/FilesScreen.svelte";
  import ReviewScreen from "../screens/ReviewScreen.svelte";
  import IngestsScreen from "../screens/IngestsScreen.svelte";
  import MapScreen from "../screens/MapScreen.svelte";
  import TimelineScreen from "../screens/TimelineScreen.svelte";
  import SettingsScreen from "../screens/SettingsScreen.svelte";

  function onKeydown(e: KeyboardEvent) {
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
      e.preventDefault();
      app.searchOpen = !app.searchOpen;
    } else if (e.key === "Escape" && app.searchOpen) {
      app.searchOpen = false;
    }
  }
</script>

<svelte:window onkeydown={onKeydown} />

<div class="frame">
  <TitleBar />
  <div class="body">
    <NavRail />
    <main class="screen">
      <!-- Screens stay mounted so open file / query survive switching. -->
      <div class="pane" hidden={app.screen !== "home"}><HomeScreen /></div>
      <div class="pane" hidden={app.screen !== "files"}><FilesScreen /></div>
      <div class="pane" hidden={app.screen !== "review"}><ReviewScreen /></div>
      <div class="pane" hidden={app.screen !== "ingests"}><IngestsScreen /></div>
      <div class="pane" hidden={app.screen !== "map"}><MapScreen /></div>
      <div class="pane" hidden={app.screen !== "timeline"}><TimelineScreen /></div>
      <div class="pane" hidden={app.screen !== "settings"}><SettingsScreen /></div>
    </main>
    {#if chats.open}
      <ChatDrawer />
    {/if}
  </div>
  {#if app.searchOpen}
    <SearchOverlay />
  {/if}
</div>

<style>
  .frame {
    height: 100vh;
    display: flex;
    flex-direction: column;
    background: var(--paper);
    position: relative;
    overflow: hidden;
  }
  .body {
    display: flex;
    flex: 1;
    min-height: 0;
  }
  .screen {
    flex: 1;
    min-width: 0;
    display: flex;
    min-height: 0;
  }
  .pane {
    flex: 1;
    min-width: 0;
    display: flex;
    min-height: 0;
  }
  .pane[hidden] {
    display: none;
  }
</style>

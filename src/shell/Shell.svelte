<script lang="ts">
  import { app } from "../lib/app.svelte";
  import TitleBar from "./TitleBar.svelte";
  import Sidebar from "./Sidebar.svelte";
  import SearchOverlay from "../search/SearchOverlay.svelte";
  import ChatDrawer from "../chat/ChatDrawer.svelte";
  import { chats } from "../lib/chats.svelte";
  import HomeScreen from "../screens/HomeScreen.svelte";
  import FilesScreen from "../screens/FilesScreen.svelte";
  import IngestsScreen from "../screens/IngestsScreen.svelte";
  import InboxScreen from "../screens/InboxScreen.svelte";
  import ExploreScreen from "../screens/ExploreScreen.svelte";
  import SettingsScreen from "../screens/SettingsScreen.svelte";
  import { inbox } from "../lib/inbox.svelte";
  import WhatsNewDialog from "../whats-new/WhatsNewDialog.svelte";
  import { whatsNew } from "../whats-new/whatsNew.svelte";
  import Toasts from "../lib/ui/Toasts.svelte";
  import { record } from "../lib/record.svelte";
  import { conflicts } from "../lib/conflicts.svelte";
  import { rail } from "../lib/rail.svelte";
  import { onMount } from "svelte";
  import { api } from "../lib/api";
  import { SvelteSet } from "svelte/reactivity";

  // Screens the user has actually opened this session (home is always live).
  const visited = new SvelteSet<string>();
  $effect(() => {
    if (app.screen !== "home") visited.add(app.screen);
  });

  // The shell only renders with a project open, so the release notes never
  // interrupt onboarding.
  onMount(() => whatsNew.init());

  // App-wide state the shell shows from any screen: a take that is running
  // (the title-bar pill), the conflict banner in Files, the rail's counts.
  onMount(() => {
    void record.listen();
    void conflicts.subscribe();
    void rail.subscribe();
    void inbox.subscribe();
  });

  // The person asked Claude in chat to open something: focus its workspace
  // member if it is another one, then open it at the line or heading.
  onMount(() => {
    const off = api.onKenOpen(async (req) => {
      if (req.projectId && req.projectId !== app.focused && app.workspace?.members.some((m) => m.projectId === req.projectId)) {
        await app.focusMember(req.projectId);
      }
      app.openAt(req.path, { line: req.line ?? undefined, anchor: req.anchor ?? undefined });
    });
    return () => void off.then((u) => u());
  });

  function onKeydown(e: KeyboardEvent) {
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
      e.preventDefault();
      app.searchOpen = !app.searchOpen;
    } else if (e.key === "Escape" && app.searchOpen) {
      app.searchOpen = false;
    } else if (e.ctrlKey && !e.metaKey && e.key.toLowerCase() === "p" && app.workspace) {
      // Cycle the focused workspace member (workspace task 4.3).
      e.preventDefault();
      void app.cycleFocusedMember();
    }
  }
</script>

<svelte:window onkeydown={onKeydown} />

<div class="frame">
  <Sidebar />
  <div class="column">
  <TitleBar />
  <div class="body">
    <main class="screen">
      <!-- Screens mount on first visit and then stay mounted, so open file /
           query survive switching — without paying for every screen (and the
           restored editor tab) at boot. -->
      <div class="pane" hidden={app.screen !== "home"}><HomeScreen /></div>
      {#if visited.has("inbox")}
        <div class="pane" hidden={app.screen !== "inbox"}><InboxScreen /></div>
      {/if}
      {#if visited.has("files")}
        <div class="pane" hidden={app.screen !== "files"}><FilesScreen /></div>
      {/if}
      {#if visited.has("ingests")}
        <div class="pane" hidden={app.screen !== "ingests"}><IngestsScreen /></div>
      {/if}
      {#if visited.has("explore")}
        <div class="pane" hidden={app.screen !== "explore"}><ExploreScreen /></div>
      {/if}
      {#if visited.has("settings")}
        <div class="pane" hidden={app.screen !== "settings"}><SettingsScreen /></div>
      {/if}
    </main>
    {#if chats.open}
      <ChatDrawer />
    {/if}
  </div>
  </div>
  {#if app.searchOpen}
    <SearchOverlay />
  {/if}
  <WhatsNewDialog />
  <Toasts />
</div>

<style>
  .frame {
    height: 100vh;
    display: flex;
    background: var(--bg);
    position: relative;
    overflow: hidden;
  }
  .column {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
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

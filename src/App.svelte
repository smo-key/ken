<script lang="ts">
  import { onMount } from "svelte";
  import { app } from "./lib/app.svelte";
  import Shell from "./shell/Shell.svelte";
  import ProjectPicker from "./onboarding/ProjectPicker.svelte";
  import ContextMenu from "./lib/ui/ContextMenu.svelte";
  import ConfirmMenu from "./lib/ui/ConfirmMenu.svelte";
  import { installDropGuard } from "./lib/dropGuard";

  let ready = $state(false);

  // A file dropped where nothing accepts it must not navigate the window to
  // that file (WebKit's default) — see lib/dropGuard.ts.
  onMount(() => installDropGuard(window));

  onMount(async () => {
    try {
      await app.init();
    } finally {
      ready = true;
    }
  });

  // Suppress the native (OS/WebView) context menu everywhere — this is a
  // desktop app, not a web page. Runs at the window level in the bubble phase,
  // so it fires AFTER any element's own `oncontextmenu` handler (which already
  // called preventDefault + opened our custom menu). We ONLY preventDefault
  // here — never stopPropagation — so those element handlers still run and the
  // app's custom context menus keep working. WKWebView has no config toggle
  // for this, so it must be done in JS.
  function suppressNativeContextMenu(e: MouseEvent) {
    e.preventDefault();
  }
</script>

<svelte:window oncontextmenu={suppressNativeContextMenu} />

{#if !ready}
  <div class="boot"></div>
{:else if app.project}
  <Shell />
{:else}
  <ProjectPicker />
{/if}

<!-- Mounted exactly once, at the root: screens stay mounted but hidden when you
     switch tabs, so a per-screen menu would end up inside a `display:none`
     ancestor and never show. -->
<ContextMenu />
<ConfirmMenu />

<style>
  .boot {
    height: 100vh;
    background: var(--bg);
  }
</style>

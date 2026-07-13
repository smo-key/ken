<script lang="ts">
  import { onMount } from "svelte";
  import { app } from "./lib/app.svelte";
  import Shell from "./shell/Shell.svelte";
  import ProjectPicker from "./onboarding/ProjectPicker.svelte";

  let ready = $state(false);

  onMount(async () => {
    try {
      await app.init();
    } finally {
      ready = true;
    }
  });
</script>

{#if !ready}
  <div class="boot"></div>
{:else if app.project}
  <Shell />
{:else}
  <ProjectPicker />
{/if}

<style>
  .boot {
    height: 100vh;
    background: var(--paper);
  }
</style>

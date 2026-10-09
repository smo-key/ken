<script lang="ts">
  // Explore (the Briefing design): two views of the same knowledge, the
  // Timeline of what happened and the Map of how it connects, switched with
  // one control. Search reaches both.
  import { app, type ExploreView } from "../lib/app.svelte";
  import MapScreen from "./MapScreen.svelte";
  import TimelineScreen from "./TimelineScreen.svelte";

  const views: { key: ExploreView; label: string }[] = [
    { key: "timeline", label: "Timeline" },
    { key: "map", label: "Map" },
  ];
  // Both stay mounted once seen, so a view keeps its place when you switch.
  let seen = $state<Record<ExploreView, boolean>>({ timeline: false, map: false });
  $effect(() => {
    seen[app.exploreView] = true;
  });
</script>

<div class="explore">
  <div class="bar">
    <h1>Explore</h1>
    <div class="k-segmented" role="tablist" aria-label="Explore view">
      {#each views as v (v.key)}
        <button
          class:on={app.exploreView === v.key}
          role="tab"
          aria-selected={app.exploreView === v.key}
          onclick={() => (app.exploreView = v.key)}>{v.label}</button
        >
      {/each}
    </div>
  </div>
  <div class="view" hidden={app.exploreView !== "timeline"}>
    {#if seen.timeline}<TimelineScreen />{/if}
  </div>
  <div class="view" hidden={app.exploreView !== "map"}>
    {#if seen.map}<MapScreen />{/if}
  </div>
</div>

<style>
  .explore {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    min-height: 0;
  }
  .bar {
    flex: none;
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 24px clamp(24px, 4vw, 56px) 12px;
  }
  h1 {
    margin: 0;
    font-family: var(--font-serif);
    font-size: 30px;
    font-weight: 500;
  }
  .view {
    flex: 1;
    min-height: 0;
    display: flex;
  }
  .view[hidden] {
    display: none;
  }
</style>

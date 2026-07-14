<script lang="ts">
  // Home's primary action opens the ⌘K palette — the palette owns search, so
  // this is a calm affordance, not a second search box. A real <button> gives
  // us Enter/Space activation for free.
  import { app } from "../lib/app.svelte";
  import Search from "@lucide/svelte/icons/search";
</script>

<button
  class="trigger"
  onclick={() => (app.searchOpen = true)}
  aria-keyshortcuts="Meta+K"
>
  <Search class="lens" size={15} strokeWidth={1.75} aria-hidden="true" />
  <span class="label">Search your knowledge…</span>
  <span class="kbd">⌘K</span>
</button>

<style>
  .trigger {
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    height: 44px;
    padding: 0 16px;
    background: var(--surface);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-control);
    box-shadow: var(--shadow-control);
    font-family: inherit;
    cursor: text;
    text-align: left;
  }
  .trigger:hover {
    border-color: var(--accent);
  }
  .trigger:focus-visible {
    border-color: var(--accent);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 12%, transparent);
    outline: none;
  }
  .trigger :global(.lens) {
    color: var(--ink-tertiary);
    flex: none;
  }
  .label {
    flex: 1;
    min-width: 0;
    font-size: 14px;
    color: var(--ink-tertiary);
  }
  .kbd {
    flex: none;
  }
</style>

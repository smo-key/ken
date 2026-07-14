<script lang="ts">
  let { level = 0, active = false }: { level: number; active: boolean } = $props();
  // Map RMS (~0..0.5 typical speech) to a 0..100% bar, gently compressed.
  const pct = $derived(Math.min(100, Math.round(Math.sqrt(level) * 140)));
</script>

<div class="meter" class:active>
  <div class="fill" style:width="{pct}%"></div>
</div>

<style>
  .meter {
    height: 6px;
    border-radius: 3px;
    background: var(--sunken);
    overflow: hidden;
  }
  .fill {
    height: 100%;
    background: var(--ink-tertiary);
    transition: width 90ms linear;
  }
  .meter.active .fill {
    background: var(--accent);
  }
</style>

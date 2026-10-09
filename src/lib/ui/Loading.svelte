<script lang="ts">
  // What a screen shows while its first read is on the way: a quiet spinner
  // and what Ken is reading, or a few placeholder lines where a list will be.
  let { label = "Loading…", lines = 0 }: { label?: string; lines?: number } = $props();
</script>

<div class="loading" role="status" aria-live="polite">
  <span class="row"><span class="spin" aria-hidden="true"></span><span class="label">{label}</span></span>
  {#each Array.from({ length: lines }, (_, i) => i) as i (i)}
    <span class="bar" style="width: {92 - ((i * 17) % 40)}%"></span>
  {/each}
</div>

<style>
  .loading {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 6px 0;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 9px;
    font-size: 13px;
    color: var(--ink-3);
  }
  .spin {
    width: 14px;
    height: 14px;
    border-radius: 50%;
    border: 2px solid var(--line);
    border-top-color: var(--accent);
    animation: spin 0.8s linear infinite;
    flex: none;
  }
  .bar {
    height: 12px;
    border-radius: 6px;
    background: var(--line-soft);
    animation: fade 1.4s ease-in-out infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  @keyframes fade {
    0%,
    100% {
      opacity: 0.55;
    }
    50% {
      opacity: 1;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .spin,
    .bar {
      animation: none;
    }
  }
</style>

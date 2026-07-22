<script lang="ts">
  // The app's one determinate progress bar (extracted from the model download
  // dialog so hydration and transcription reuse it). `pct: null` renders an
  // indeterminate sweep for work whose extent isn't known yet.
  let { pct, label }: { pct: number | null; label?: string } = $props();
  const clamped = $derived(
    pct === null ? null : Math.max(0, Math.min(100, Math.round(pct))),
  );
</script>

<div class="progress">
  <div
    class="bar"
    class:indeterminate={clamped === null}
    role="progressbar"
    aria-valuenow={clamped ?? undefined}
    aria-valuemin={0}
    aria-valuemax={100}
  >
    <div class="fill" style:width={clamped === null ? "40%" : `${clamped}%`}></div>
  </div>
  {#if label}
    <p class="status">{label}</p>
  {/if}
</div>

<style>
  .progress {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .bar {
    height: 6px;
    border-radius: 4px;
    background: var(--sunken);
    overflow: hidden;
  }
  .fill {
    height: 100%;
    background: var(--accent);
    border-radius: 4px;
    transition: width 0.2s ease;
  }
  .bar.indeterminate .fill {
    animation: sweep 1.2s ease-in-out infinite;
  }
  @keyframes sweep {
    0% {
      transform: translateX(-100%);
    }
    100% {
      transform: translateX(350%);
    }
  }
  .status {
    margin: 0;
    font-size: 12px;
    color: var(--ink-tertiary);
  }
</style>

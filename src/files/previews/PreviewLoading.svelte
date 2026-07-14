<script lang="ts">
  // The one loading/downloading state every file view shares: a single spinner
  // centered in its container, one primary line, and an optional quieter second
  // line. Kept presentational so previews stop each rolling their own markup.
  let { label, detail }: { label: string; detail?: string } = $props();
</script>

<!-- role=status + aria-live so the message is announced while the file resolves,
     not silently swapped in. -->
<div class="loading" role="status" aria-live="polite">
  <span class="spinner"></span>
  <p class="label">{label}</p>
  {#if detail}
    <p class="detail">{detail}</p>
  {/if}
</div>

<style>
  .loading {
    /* Fill and center whether the parent is a flex column (flex:1 grows it) or a
       plain block/scroll region (min-height fills it); border-box keeps the
       padding from overflowing that 100%. */
    box-sizing: border-box;
    flex: 1;
    min-height: 100%;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 12px;
    padding: 24px;
    text-align: center;
  }
  /* The single spinner definition for the whole app's file views. */
  .spinner {
    width: 18px;
    height: 18px;
    border: 2px solid color-mix(in srgb, var(--ink-tertiary) 35%, transparent);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.7s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  .label {
    margin: 0;
    font-size: 13.5px;
    color: var(--ink-secondary);
    line-height: 1.5;
  }
  .detail {
    margin: 0;
    max-width: 340px;
    font-size: 12px;
    color: var(--ink-tertiary);
    line-height: 1.55;
  }
</style>

<script lang="ts">
  /** Ken's app mark: the script K, centered on an ink tile. */
  let { size = 36 }: { size?: number } = $props();
</script>

<span
  class="mark"
  style:width="{size}px"
  style:height="{size}px"
  style:border-radius="{Math.round(size * 0.25)}px"
  style:font-size="{Math.round(size * 0.62)}px"
  aria-hidden="true"
>
  <span class="k">K</span>
</span>

<style>
  .mark {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    background: var(--ink);
    color: var(--paper);
    flex: none;
    user-select: none;
  }
  .k {
    font-family: var(--font-script);
    font-weight: 400;
    line-height: 1;
    /* Optical centering nudge for the script glyph. */
    transform: translate(-0.02em, 0.03em);
  }
</style>

<script lang="ts">
  import { glyphFor } from "../lib/format";

  let { kind, size = "md" }: { kind: string; size?: "sm" | "md" } = $props();
  const g = $derived(glyphFor(kind));
</script>

<span
  class="glyph {size}"
  style:background={g.bg}
  style:border-color={g.border}
  style:color={g.color}
>{g.label}</span>

<style>
  .glyph {
    flex: none;
    border: 1px solid;
    display: flex;
    align-items: flex-end;
    justify-content: center;
    font-family: var(--font-mono);
    font-weight: 500;
  }
  .glyph.sm {
    width: 13px;
    height: 16px;
    border-radius: 2px 5px 2px 2px;
    font-size: 5px;
    padding-bottom: 1px;
  }
  .glyph.md {
    width: 18px;
    height: 22px;
    border-radius: 2px 6px 2px 2px;
    font-size: 6px;
    padding-bottom: 2px;
  }
</style>

<script lang="ts">
  import { api } from "../../lib/api";
  let { relPath, size }: { relPath: string; size: number } = $props();
</script>

<div class="too-large">
  <p>
    <strong>{relPath.split("/").pop()}</strong> is
    {(size / 1048576).toFixed(1)} MB — too big to open inside Ken without slowing
    everything down. It's still indexed and searchable.
  </p>
  <button class="btn" onclick={() => api.openExternal(relPath)}>
    Open in default app
  </button>
</div>

<style>
  .too-large {
    margin: 48px auto;
    max-width: 420px;
    text-align: center;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 14px;
  }
  .too-large p {
    margin: 0;
    font-size: 13.5px;
    line-height: 1.65;
    color: var(--ink-secondary);
  }
</style>

<script lang="ts">
  // The corner notices (lib/toast.svelte.ts). Mounted once, in the shell.
  import { toast } from "../toast.svelte";
  import X from "@lucide/svelte/icons/x";
</script>

{#if toast.items.length > 0}
  <div class="toasts" role="status" aria-live="polite">
    {#each toast.items as t (t.id)}
      <div class="toast" class:error={t.tone === "error"}>
        <span class="text">{t.text}</span>
        <button class="close" aria-label="Dismiss" onclick={() => toast.dismiss(t.id)}>
          <X size={13} strokeWidth={1.75} />
        </button>
      </div>
    {/each}
  </div>
{/if}

<style>
  .toasts {
    position: absolute;
    right: 18px;
    bottom: 18px;
    z-index: 60;
    display: flex;
    flex-direction: column;
    gap: 8px;
    max-width: min(420px, calc(100vw - 36px));
  }
  .toast {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    padding: 10px 12px;
    border-radius: var(--radius-control);
    border: 1px solid var(--border-strong);
    background: var(--surface);
    box-shadow: var(--shadow-card);
    font-size: 12.5px;
    line-height: 1.5;
    color: var(--ink);
  }
  .toast.error {
    border-color: color-mix(in srgb, var(--danger) 35%, var(--border));
    color: var(--danger);
  }
  .text {
    flex: 1;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .close {
    flex: none;
    display: inline-flex;
    border: none;
    background: transparent;
    padding: 2px;
    color: var(--ink-tertiary);
  }
  .close:hover {
    color: var(--ink);
  }
</style>

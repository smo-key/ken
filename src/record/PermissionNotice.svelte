<script lang="ts">
  import { api, type PermissionStatus } from "../lib/api";

  let {
    status,
    label,
    settingsUrl,
    onRequest,
  }: {
    status: PermissionStatus;
    label: string;
    settingsUrl: string;
    onRequest: () => void;
  } = $props();
</script>

{#if status !== "granted" && status !== "unsupported"}
  <div class="notice">
    <span class="text">
      {#if status === "notDetermined"}
        Ken needs your permission to use the {label}.
      {:else}
        {label} access is turned off for Ken.
      {/if}
    </span>
    {#if status === "notDetermined"}
      <button class="link" onclick={onRequest}>Allow</button>
    {:else}
      <button class="link" onclick={() => void api.openSettingsUrl(settingsUrl)}>
        Open Settings
      </button>
    {/if}
  </div>
{/if}

<style>
  .notice {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 9px 12px;
    border-radius: 9px;
    font-size: 12.5px;
    line-height: 1.5;
    color: var(--needs-input-text);
    background: color-mix(in srgb, var(--needs-input) 10%, transparent);
    border: 1px solid color-mix(in srgb, var(--needs-input) 26%, transparent);
  }
  .text {
    flex: 1;
  }
  .link {
    border: none;
    background: none;
    color: var(--needs-input-text);
    font-weight: 600;
    font-size: 12.5px;
    text-decoration: underline;
    cursor: pointer;
  }
</style>

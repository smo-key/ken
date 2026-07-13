<script lang="ts">
  import { open as openDialog } from "@tauri-apps/plugin-dialog";
  import { api } from "../lib/api";
  import { app } from "../lib/app.svelte";
  import Plus from "@lucide/svelte/icons/plus";

  let { close }: { close: () => void } = $props();
  let error = $state<string | null>(null);

  async function pick(path: string, available: boolean) {
    if (!available) return;
    try {
      await app.openProject(path);
      close();
    } catch (e) {
      error = String(e);
    }
  }

  async function forget(id: string, e: MouseEvent) {
    e.stopPropagation();
    await api.forgetProject(id);
    await app.refreshRegistry();
  }

  async function openFolder() {
    const folder = await openDialog({ directory: true, title: "Open a folder as a Ken project" });
    if (typeof folder !== "string") return;
    const name = folder.split("/").pop() ?? "Project";
    try {
      await app.createProject(folder, name);
      close();
    } catch (e) {
      error = String(e);
    }
  }
</script>

<button class="scrim" onclick={close} aria-label="Close project switcher"></button>
<div class="menu">
  {#each app.registry as entry (entry.id)}
    <button
      class="row"
      class:current={entry.id === app.project?.id}
      class:unavailable={!entry.available}
      onclick={() => pick(entry.path, entry.available)}
    >
      <span class="badge">{entry.name.charAt(0).toUpperCase()}</span>
      <span class="info">
        <span class="name">{entry.name}</span>
        <span class="path mono">{entry.path}</span>
        {#if !entry.available}
          <span class="missing">Folder not found — was it moved or deleted?</span>
        {/if}
      </span>
      {#if !entry.available}
        <span class="forget" role="button" tabindex="0" onclick={(e) => forget(entry.id, e)} onkeydown={() => {}}>Remove</span>
      {/if}
    </button>
  {/each}
  <button class="row new" onclick={openFolder}>
    <span class="badge plus"><Plus size={15} strokeWidth={1.75} /></span>
    <span class="info"><span class="name">Open a folder…</span>
      <span class="path">Any folder becomes a Ken project</span></span>
  </button>
  {#if error}
    <div class="error">{error}</div>
  {/if}
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: transparent;
    border: none;
    z-index: 39;
  }
  .menu {
    position: absolute;
    top: 46px;
    left: 86px;
    width: 340px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-overlay);
    box-shadow: var(--shadow-overlay);
    padding: 6px;
    z-index: 40;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 10px;
    border-radius: 9px;
    border: none;
    background: transparent;
    text-align: left;
    font-size: 13px;
    color: var(--ink);
  }
  .row:hover {
    background: var(--sunken);
  }
  .row.current {
    background: rgba(138, 90, 68, 0.08);
  }
  .row.unavailable {
    opacity: 0.7;
    cursor: default;
  }
  .badge {
    width: 26px;
    height: 26px;
    border-radius: 7px;
    background: var(--ink);
    color: var(--paper);
    display: flex;
    align-items: center;
    justify-content: center;
    font-family: var(--font-serif);
    font-size: 13px;
    flex: none;
  }
  .badge.plus {
    background: var(--accent);
  }
  .info {
    display: flex;
    flex-direction: column;
    min-width: 0;
    flex: 1;
  }
  .name {
    font-weight: 600;
  }
  .path {
    font-size: 11px;
    color: var(--ink-tertiary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .missing {
    font-size: 11.5px;
    color: var(--danger);
  }
  .forget {
    font-size: 12px;
    font-weight: 600;
    color: var(--danger);
    flex: none;
  }
  .error {
    padding: 8px 10px;
    font-size: 12px;
    color: var(--danger);
  }
</style>

<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { api, type FileRow } from "../lib/api";
  import { isEditable, timeAgo } from "../lib/format";
  import MarkdownEditor from "./MarkdownEditor.svelte";
  import PlainEditor from "./PlainEditor.svelte";
  import PreviewPane from "./PreviewPane.svelte";

  let { relPath }: { relPath: string } = $props();

  let meta = $state<FileRow | null>(null);
  let content = $state<string | null>(null);
  let loadError = $state<string | null>(null);

  let mode = $state<"wysiwyg" | "plain">("wysiwyg");
  let dirty = $state(false);
  let savedAt = $state<number | null>(null);
  let knownMtime = $state(0);
  let diskChanged = $state(false);
  let reloadKey = $state(0);

  let saveTimer: ReturnType<typeof setTimeout> | undefined;
  let latest = "";
  let unlisten: (() => void) | undefined;

  const editable = $derived(meta !== null && isEditable(meta.kind));
  const kind = $derived(meta?.kind ?? "binary");

  onMount(async () => {
    try {
      meta = await api.fileMeta(relPath);
      if (meta && isEditable(meta.kind)) {
        content = await api.readFile(relPath);
        latest = content;
        if (meta.kind !== "md") mode = "plain";
      }
      knownMtime = await api.fileMtime(relPath);
    } catch (e) {
      loadError = String(e);
    }
    unlisten = await api.onIndexUpdated(() => void checkDisk());
  });

  onDestroy(() => {
    unlisten?.();
    if (saveTimer) {
      clearTimeout(saveTimer);
      if (dirty) void doSave(); // flush pending edits on close
    }
  });

  async function checkDisk() {
    if (!editable) return;
    try {
      const mtime = await api.fileMtime(relPath);
      if (mtime > knownMtime) {
        if (dirty) {
          diskChanged = true; // conflict: user decides
        } else {
          await reloadFromDisk();
        }
      }
    } catch {
      /* file may be mid-rename; next event reconciles */
    }
  }

  async function reloadFromDisk() {
    content = await api.readFile(relPath);
    latest = content;
    knownMtime = await api.fileMtime(relPath);
    dirty = false;
    diskChanged = false;
    reloadKey += 1;
  }

  async function keepMine() {
    diskChanged = false;
    await doSave();
  }

  function onEdit(markdown: string) {
    latest = markdown;
    dirty = true;
    if (saveTimer) clearTimeout(saveTimer);
    saveTimer = setTimeout(() => void doSave(), 800);
  }

  async function doSave() {
    if (!dirty || diskChanged) return;
    try {
      knownMtime = await api.saveFile(relPath, latest);
      dirty = false;
      savedAt = Date.now();
    } catch (e) {
      loadError = `Couldn't save: ${e}`;
    }
  }

  function toggleMode() {
    content = latest;
    reloadKey += 1;
    mode = mode === "wysiwyg" ? "plain" : "wysiwyg";
  }
</script>

<div class="pane">
  <div class="header">
    <span class="mono path">{relPath}</span>
    {#if editable}
      <span class="save-state">
        <span class="sdot" class:dirty></span>
        {#if dirty}Editing…{:else if savedAt}Saved {timeAgo(Math.floor(savedAt / 1000))}{:else}Saved{/if}
      </span>
      {#if meta?.kind === "md"}
        <button class="mode" onclick={toggleMode}>
          {mode === "wysiwyg" ? "Plain text" : "Formatted"}
        </button>
      {/if}
    {/if}
    <span class="spacer"></span>
    <button class="mode" onclick={() => api.openExternal(relPath)}>Open in default app</button>
  </div>

  {#if diskChanged}
    <div class="conflict">
      <span class="cdot"></span>
      <span class="ctext">
        This file also changed on disk while you were editing — maybe a teammate
        or another app. Which version should Ken keep?
      </span>
      <button class="btn btn-small" onclick={keepMine}>Keep my version</button>
      <button class="btn btn-small" onclick={reloadFromDisk}>Take the disk version</button>
    </div>
  {/if}

  {#if loadError}
    <div class="error">{loadError}</div>
  {:else if editable && content !== null}
    {#key reloadKey}
      {#if mode === "wysiwyg" && meta?.kind === "md"}
        <MarkdownEditor initial={content} onchange={onEdit} />
      {:else}
        <PlainEditor initial={content} onchange={onEdit} />
      {/if}
    {/key}
  {:else if meta}
    <PreviewPane {relPath} {kind} {meta} />
  {/if}
</div>

<style>
  .pane {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    min-height: 0;
  }
  .header {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 10px 20px;
    border-bottom: 1px solid var(--sunken);
    font-size: 11.5px;
    color: var(--ink-tertiary);
    flex: none;
  }
  .path {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .save-state {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    flex: none;
  }
  .sdot {
    width: 5px;
    height: 5px;
    border-radius: 3px;
    background: var(--healthy);
  }
  .sdot.dirty {
    background: var(--needs-input);
  }
  .spacer {
    flex: 1;
  }
  .mode {
    border: none;
    background: none;
    color: var(--accent);
    font-size: 11.5px;
    font-weight: 600;
    flex: none;
    padding: 0;
  }
  .conflict {
    display: flex;
    align-items: center;
    gap: 10px;
    margin: 10px 20px 0;
    padding: 10px 14px;
    background: rgba(168, 116, 44, 0.08);
    border: 1px solid rgba(168, 116, 44, 0.28);
    border-radius: 10px;
    font-size: 12.5px;
    flex: none;
  }
  .cdot {
    width: 8px;
    height: 8px;
    border-radius: 4px;
    background: var(--needs-input);
    flex: none;
  }
  .ctext {
    flex: 1;
    line-height: 1.5;
  }
  .error {
    margin: 16px 20px;
    padding: 12px 16px;
    background: rgba(163, 77, 63, 0.07);
    border: 1px solid rgba(163, 77, 63, 0.25);
    border-radius: 10px;
    font-size: 13px;
    color: var(--danger);
  }
</style>

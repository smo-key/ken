<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import ExternalLink from "@lucide/svelte/icons/external-link";
  import Code from "@lucide/svelte/icons/code";
  import Eye from "@lucide/svelte/icons/eye";
  import { api, type FileRow } from "../lib/api";
  import { app } from "../lib/app.svelte";
  import { isEditable, timeAgo } from "../lib/format";
  import { delimiterForPath } from "../lib/csv";
  import MarkdownEditor from "./MarkdownEditor.svelte";
  import PlainEditor from "./PlainEditor.svelte";
  import CsvEditor from "./CsvEditor.svelte";
  import PreviewPane from "./PreviewPane.svelte";

  let { relPath }: { relPath: string } = $props();

  // .csv/.tsv get the grid editor even though the backend kind is "code"
  // (csv) or "binary" (tsv). Routed by extension, ahead of the plain editor.
  const ext = $derived(relPath.split(".").pop()?.toLowerCase() ?? "");
  const isCsv = $derived(ext === "csv" || ext === "tsv");

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

  // Editing limits: a 25MB CSV opened as an editable grid (or a huge doc fed
  // to the WYSIWYG editor) allocates unboundedly and freezes the app.
  const GRID_EDIT_MAX = 2 * 1024 * 1024; // csv/tsv grid
  const WYSIWYG_MAX = 1.5 * 1024 * 1024; // markdown in Crepe
  const TEXT_EDIT_MAX = 8 * 1024 * 1024; // plain-text editor

  const tooLarge = $derived(
    meta !== null &&
      (isCsv
        ? meta.size > GRID_EDIT_MAX
        : isEditable(meta.kind) && meta.size > TEXT_EDIT_MAX),
  );
  const editable = $derived(
    meta !== null && (isEditable(meta.kind) || isCsv) && !tooLarge,
  );
  const kind = $derived(meta?.kind ?? "binary");

  onMount(async () => {
    try {
      meta = await api.fileMeta(relPath);
      if (meta && (isEditable(meta.kind) || isCsv) && !tooLarge) {
        content = await api.readFile(relPath);
        latest = content;
        if (meta.kind !== "md" || meta.size > WYSIWYG_MAX) mode = "plain";
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
    app.makeTabPersistent(relPath); // editing promotes a preview tab
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
  <!-- Hidden while the disk-conflict banner is up: its buttons sit exactly
       where this bar floats, and the conflict decision takes priority. -->
  {#if !diskChanged}
  <div class="actions">
    {#if editable}
      <span class="save-state">
        <span class="sdot" class:dirty></span>
        {#if dirty}Editing…{:else if savedAt}Saved {timeAgo(Math.floor(savedAt / 1000))}{:else}Saved{/if}
      </span>
      {#if meta?.kind === "md" && meta.size <= WYSIWYG_MAX}
        <span class="sep"></span>
        <button class="action" onclick={toggleMode}>
          {#if mode === "wysiwyg"}<Code size={13} strokeWidth={1.75} /><span>Plain text</span>{:else}<Eye size={13} strokeWidth={1.75} /><span>Formatted</span>{/if}
        </button>
      {/if}
      <span class="sep"></span>
    {/if}
    <button class="action" onclick={() => api.openExternal(relPath)}>
      <ExternalLink size={13} strokeWidth={1.75} />
      <span>Open in default app</span>
    </button>
  </div>
  {/if}

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
  {:else if tooLarge && meta}
    <div class="too-large">
      <p>
        <strong>{relPath.split("/").pop()}</strong> is
        {(meta.size / 1048576).toFixed(1)} MB — too big to edit inside Ken
        without slowing everything down. It's still indexed and searchable.
      </p>
      <button class="btn" onclick={() => api.openExternal(relPath)}>
        Open in default app
      </button>
    </div>
  {:else if editable && content !== null}
    {#key reloadKey}
      {#if isCsv}
        <CsvEditor
          initial={content}
          delimiter={delimiterForPath(relPath)}
          onchange={onEdit}
        />
      {:else if mode === "wysiwyg" && meta?.kind === "md"}
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
    position: relative;
    /* Room so the floating action bar never sits on the first line of text. */
    padding-top: 6px;
  }
  /* Floating action bar: pinned top-right, overlaying the content. Offset from
     the right edge so it never covers the editor scrollbar. */
  .actions {
    position: absolute;
    top: 10px;
    right: 18px;
    z-index: 20;
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 3px 6px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-control);
    box-shadow: var(--shadow-control);
    font-size: 11.5px;
    color: var(--ink-tertiary);
    opacity: 0.82;
    transition: opacity 0.12s ease;
  }
  .actions:hover,
  .actions:focus-within {
    opacity: 1;
  }
  .action {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 3px 7px;
    border: none;
    border-radius: 6px;
    background: transparent;
    color: var(--ink-secondary);
    font-size: 11.5px;
    font-weight: 500;
    flex: none;
  }
  .action:hover {
    background: var(--sunken);
    color: var(--ink);
  }
  .sep {
    width: 1px;
    height: 14px;
    background: var(--border);
    flex: none;
  }
  .save-state {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 0 4px;
    flex: none;
    white-space: nowrap;
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

<script lang="ts">
  import { onMount } from "svelte";
  import { open as openDialog } from "@tauri-apps/plugin-dialog";
  import Sparkles from "@lucide/svelte/icons/sparkles";
  import FolderPlus from "@lucide/svelte/icons/folder-plus";
  import { api, type FileRow, type ImportDto } from "../lib/api";
  import { app } from "../lib/app.svelte";
  import { toProjectRelative } from "../lib/projectPath";
  import { formatSize } from "../lib/format";
  import FileGlyph from "./FileGlyph.svelte";
  import PreviewPane from "./PreviewPane.svelte";

  let { staged, close }: { staged: ImportDto; close: () => void } = $props();

  // The chosen destination folder (project-relative; "" = project root) and
  // whether it's a folder that doesn't exist yet. `createFolder` on save is
  // exactly `isNew`. Defaults to the root so Save works before AI answers.
  let folder = $state("");
  let isNew = $state(false);
  let rationale = $state<string | null>(null);
  // True while the AI is still deciding. Save stays enabled throughout.
  let classifying = $state(true);
  // Once the user overrides the destination, a late AI answer must not clobber
  // their choice.
  let overridden = $state(false);
  let saving = $state(false);
  let error = $state<string | null>(null);

  // The staged copy isn't indexed, so build the meta PreviewPane needs from the
  // begin-DTO. The preview commands read `.ken/imports/...` fine via resolve.
  const meta = $derived<FileRow>({
    relPath: staged.previewRel,
    kind: staged.kind,
    size: staged.size,
    mtime: Math.floor(Date.now() / 1000),
    status: "indexed",
    error: null,
    backgroundEligible: false,
  });

  const folderLabel = $derived(folder === "" ? "Project root" : folder);

  onMount(async () => {
    try {
      const placement = await api.importClassify(staged.importId);
      // A slow classify that lands after the user chose stands down.
      if (!overridden) {
        folder = placement.folder;
        isNew = placement.isNew;
        rationale = placement.rationale;
      }
    } catch {
      // Classification never hard-fails on the backend; if it somehow does,
      // the default (project root) is already in place.
    } finally {
      classifying = false;
    }
  });

  // Native folder dialog constrained to the project, handing back a
  // project-relative path. An overridden folder is one that exists on disk, so
  // it's never a "new" folder.
  async function overrideFolder() {
    error = null;
    const root = app.project?.root;
    if (!root) {
      error = "No project is open.";
      return;
    }
    const chosen = await openDialog({ directory: true, defaultPath: root });
    if (typeof chosen !== "string") return; // cancelled
    const r = toProjectRelative(root, chosen);
    if (!r.ok) {
      error = r.error;
      return;
    }
    overridden = true;
    folder = r.rel;
    isNew = false;
    rationale = null;
  }

  async function save() {
    if (saving) return;
    saving = true;
    error = null;
    try {
      const rel = await api.importCommit(staged.importId, folder, isNew);
      await app.refreshTree();
      app.screen = "files";
      app.openTab(rel, true);
      app.reveal(rel);
      close();
    } catch (e) {
      error = String(e);
      saving = false;
    }
  }

  async function cancel() {
    try {
      await api.importCancel(staged.importId);
    } catch {
      // A best-effort cleanup; a leftover staging dir is harmless.
    }
    close();
  }
</script>

<button class="scrim" onclick={cancel} aria-label="Close"></button>
<div class="modal" role="dialog" aria-label="Import file">
  <div class="head">
    <FileGlyph kind={staged.kind} />
    <div class="titles">
      <div class="fname" title={staged.fileName}>{staged.fileName}</div>
      <div class="fsub">{formatSize(staged.size)} · a copy will be added to this project</div>
    </div>
  </div>

  <div class="body">
    <div class="left">
      <div class="section-label">Destination</div>

      {#if classifying}
        <div class="deciding">
          <Sparkles size={15} strokeWidth={1.75} />
          <span>Deciding where this should live…</span>
        </div>
      {/if}

      <div class="dest">
        <div class="dest-row">
          <FileGlyph kind="folder" size="sm" />
          <span class="dest-path" title={folderLabel}>{folderLabel}</span>
          {#if isNew}
            <span class="badge" title="This folder will be created on save">
              <FolderPlus size={12} strokeWidth={1.75} />
              New folder
            </span>
          {/if}
        </div>
        {#if rationale && !overridden}
          <div class="why">{rationale}</div>
        {/if}
      </div>

      <button class="btn btn-small" onclick={overrideFolder}>Choose a different folder…</button>

      {#if error}
        <div class="error">{error}</div>
      {/if}

      <div class="actions">
        <button class="btn btn-primary" onclick={save} disabled={saving}>
          {saving ? "Saving…" : "Save"}
        </button>
        <button class="btn btn-ghost" onclick={cancel} disabled={saving}>Cancel</button>
      </div>
    </div>

    <div class="right">
      <div class="section-label">Preview</div>
      <div class="preview">
        {#key staged.previewRel}
          <PreviewPane relPath={staged.previewRel} kind={staged.kind} {meta} />
        {/key}
      </div>
    </div>
  </div>
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: var(--scrim);
    border: none;
    z-index: 60;
  }
  .modal {
    position: fixed;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    width: min(900px, calc(100vw - 80px));
    height: min(620px, calc(100vh - 100px));
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-overlay);
    box-shadow: var(--shadow-overlay);
    padding: 20px 22px;
    z-index: 61;
    display: flex;
    flex-direction: column;
    gap: 14px;
    min-height: 0;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 12px;
    flex: none;
  }
  .titles {
    min-width: 0;
  }
  .fname {
    font-family: var(--font-serif);
    font-size: 18px;
    font-weight: 500;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .fsub {
    font-size: 12px;
    color: var(--ink-tertiary);
    margin-top: 2px;
  }
  .body {
    flex: 1;
    display: flex;
    gap: 18px;
    min-height: 0;
  }
  .left {
    flex: none;
    width: 300px;
    display: flex;
    flex-direction: column;
    gap: 12px;
    min-height: 0;
  }
  .right {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 8px;
    min-width: 0;
    min-height: 0;
  }
  .section-label {
    font-size: 11px;
    font-weight: 700;
    color: var(--ink-tertiary);
    letter-spacing: 0.08em;
    text-transform: uppercase;
    flex: none;
  }
  .preview {
    flex: 1;
    min-height: 0;
    border: 1px solid var(--border);
    border-radius: 10px;
    overflow: hidden;
    background: var(--paper);
    display: flex;
  }
  .deciding {
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--accent-deep);
    font-size: 12.5px;
  }
  .dest {
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 12px;
    background: var(--paper);
  }
  .dest-row {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .dest-path {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 13.5px;
    color: var(--ink);
  }
  .badge {
    margin-left: auto;
    display: inline-flex;
    align-items: center;
    gap: 4px;
    flex: none;
    padding: 2px 7px;
    border-radius: 999px;
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    color: var(--accent-deep);
    font-size: 11px;
    font-weight: 600;
  }
  .why {
    margin-top: 8px;
    font-size: 12px;
    line-height: 1.5;
    color: var(--ink-secondary);
  }
  .actions {
    margin-top: auto;
    display: flex;
    gap: 8px;
  }
  .error {
    padding: 8px 10px;
    background: color-mix(in srgb, var(--danger) 8%, transparent);
    border: 1px solid color-mix(in srgb, var(--danger) 25%, transparent);
    border-radius: 8px;
    font-size: 12px;
    color: var(--danger);
  }
</style>

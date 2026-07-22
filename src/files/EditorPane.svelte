<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import ExternalLink from "@lucide/svelte/icons/external-link";
  import Code from "@lucide/svelte/icons/code";
  import Eye from "@lucide/svelte/icons/eye";
  import Lock from "@lucide/svelte/icons/lock";
  import Search from "@lucide/svelte/icons/search";
  import { api, type FileRow } from "../lib/api";
  import type { HydrationProgress } from "../lib/api";
  import { app } from "../lib/app.svelte";
  import { find } from "../lib/find.svelte";
  import FindBar from "./FindBar.svelte";
  import { isEditable, timeAgo } from "../lib/format";
  import { delimiterForPath } from "../lib/csv";
  import MarkdownEditor from "./MarkdownEditor.svelte";
  import PlainEditor from "./PlainEditor.svelte";
  import CsvEditor from "./CsvEditor.svelte";
  import PreviewPane from "./PreviewPane.svelte";
  import PreviewLoading from "./previews/PreviewLoading.svelte";
  import TooLargeNotice from "./previews/TooLargeNotice.svelte";
  import { isHtmlPath } from "./previews/html";

  let { relPath }: { relPath: string } = $props();

  // .csv/.tsv get the grid editor even though the backend kind is "code"
  // (csv) or "binary" (tsv). Routed by extension, ahead of the plain editor.
  const ext = $derived(relPath.split(".").pop()?.toLowerCase() ?? "");
  const isCsv = $derived(ext === "csv" || ext === "tsv");
  // .html/.htm index as "code", so they are editable — but a page is meant to
  // be looked at, so they open rendered and keep the source a toggle away.
  const isHtml = $derived(isHtmlPath(relPath));
  let showSource = $state(false);

  let meta = $state<FileRow | null>(null);
  let content = $state<string | null>(null);
  let loadError = $state<string | null>(null);
  // Cloud-storage placeholder being fetched, and why a fetch gave up.
  let downloading = $state(false);
  let cloudError = $state<string | null>(null);
  // Live byte progress for THIS file's cloud pull (on-demand or the background
  // worker happening to fetch the open file).
  let hydration = $state<HydrationProgress | null>(null);

  function fmtMb(n: number): string {
    return `${Math.max(1, Math.round(n / (1024 * 1024)))} MB`;
  }
  // A cloud-only file too big to auto-download (> CLOUD_DOWNLOAD_MAX): we hold
  // off the fetch and show a prompt so the user opts into the pull explicitly.
  let needsCloudConfirm = $state(false);
  // True from the first byte of load() until the file's kind/cloud-status is
  // known. Gates every preview/editor branch so a not-yet-hydrated online-only
  // file can't briefly mount a preview (flashing CLOUD_ONLY) before the
  // Downloading state appears.
  let resolving = $state(true);

  let mode = $state<"wysiwyg" | "plain">("wysiwyg");
  let dirty = $state(false);
  let savedAt = $state<number | null>(null);
  let knownMtime = $state(0);
  let diskChanged = $state(false);
  let reloadKey = $state(0);

  let saveTimer: ReturnType<typeof setTimeout> | undefined;
  let latest = "";
  let unlisten: (() => void) | undefined;
  let unlistenHydration: (() => void) | undefined;

  // Editing limits: a 25MB CSV opened as an editable grid (or a huge doc fed
  // to the WYSIWYG editor) allocates unboundedly and freezes the app.
  const GRID_EDIT_MAX = 2 * 1024 * 1024; // csv/tsv grid
  const WYSIWYG_MAX = 1.5 * 1024 * 1024; // markdown in Crepe
  const TEXT_EDIT_MAX = 8 * 1024 * 1024; // plain-text editor
  // A cloud-only file this big or larger is never auto-downloaded on open — the
  // pull can be slow and costly, so we ask first rather than fetch silently.
  const CLOUD_DOWNLOAD_MAX = 50 * 1024 * 1024;

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
  // Read-only = we know what the file is and it renders as a preview rather than
  // an editor: either not editable at all (PreviewPane — PDF/image/office/HTML
  // page), or text that's too big to edit. Suppressed until the file is resolved
  // (loading/downloading/errored) and while HTML is toggled to its editable
  // Source view. tooLarge already implies !editable, so !editable covers it.
  const readOnly = $derived(
    meta !== null &&
      !resolving &&
      !downloading &&
      cloudError === null &&
      loadError === null &&
      (!editable || (isHtml && !showSource)),
  );

  onMount(async () => {
    await load();
    unlisten = await api.onIndexUpdated(() => void checkDisk());
    unlistenHydration = await api.onHydrationProgress((ev) => {
      if (ev.relPath === relPath) hydration = ev;
    });
  });

  async function load(forceCloudDownload = false) {
    loadError = null;
    cloudError = null;
    hydration = null;
    needsCloudConfirm = false;
    resolving = true;
    try {
      meta = await api.fileMeta(relPath);
      // Files OneDrive/iCloud keep online-only have no bytes on disk yet, and
      // reading one blocks until the provider delivers it. Opening the file is
      // the user asking for exactly that — so download it deliberately, behind
      // a visible "Downloading…" state, rather than freezing on a silent read.
      // But a large pull can be slow and costly, so past CLOUD_DOWNLOAD_MAX we
      // hold off and prompt instead of fetching silently (unless the user has
      // already confirmed via the prompt).
      if (await api.isCloudOnly(relPath)) {
        if (!forceCloudDownload && meta && meta.size > CLOUD_DOWNLOAD_MAX) {
          needsCloudConfirm = true;
          return;
        }
        downloading = true;
        try {
          await api.hydrateFile(relPath);
          meta = await api.fileMeta(relPath); // its content is indexed now
        } catch (e) {
          cloudError = String(e);
          return;
        } finally {
          downloading = false;
        }
      }
      if (meta && (isEditable(meta.kind) || isCsv) && !tooLarge) {
        content = await api.readFile(relPath);
        latest = content;
        if (meta.kind !== "md" || meta.size > WYSIWYG_MAX) mode = "plain";
      }
      knownMtime = await api.fileMtime(relPath);
      // The user is now looking at this file: record its current version as
      // seen so it drops out of the unread set (and never counts as "changed by
      // someone else" until it actually changes again). Only once it truly
      // resolved to a real, indexed file.
      if (meta) void app.markSeen(relPath);
    } catch (e) {
      loadError = String(e);
    } finally {
      resolving = false;
    }
  }

  // The user accepted the large cloud pull: re-run load() forcing the download
  // so the file hydrates and the editor/preview mounts exactly as the normal
  // cloud path would have.
  async function confirmCloudDownload() {
    needsCloudConfirm = false;
    await load(true);
  }

  onDestroy(() => {
    unlisten?.();
    unlistenHydration?.();
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

  // ⌘F is find-within-this-document; ⌘K (Shell) stays project-wide search, so
  // this handler stands down whenever that overlay owns the keyboard.
  function onWindowKeydown(e: KeyboardEvent) {
    if (app.screen !== "files" || app.searchOpen) return;
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "f") {
      e.preventDefault();
      find.show();
    } else if (e.key === "Escape" && find.open) {
      find.close();
    }
  }

  async function toggleSource() {
    // The preview re-reads from disk, so pending edits have to land first or
    // the user would flip back to a stale-looking page.
    if (dirty) await doSave();
    content = latest;
    reloadKey += 1;
    showSource = !showSource;
  }
</script>

<svelte:window onkeydown={onWindowKeydown} />

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
      {:else if isHtml}
        <span class="sep"></span>
        <button class="action" onclick={() => void toggleSource()}>
          {#if showSource}<Eye size={13} strokeWidth={1.75} /><span>Page</span>{:else}<Code size={13} strokeWidth={1.75} /><span>Source</span>{/if}
        </button>
      {/if}
      <span class="sep"></span>
    {/if}
    <button
      class="action icon-only"
      title="Find in document (⌘F)"
      aria-label="Find in document"
      onclick={() => find.toggle()}
    >
      <Search size={13} strokeWidth={1.75} />
    </button>
    {#if readOnly}
      <span
        class="read-only"
        title="This file can't be edited in Ken"
        aria-label="Read only — this file can't be edited in Ken"
      >
        <Lock size={12} strokeWidth={1.75} />
        <span>Read only</span>
      </span>
    {/if}
    <button class="action" onclick={() => api.openExternal(relPath)}>
      <ExternalLink size={13} strokeWidth={1.75} />
      <span>Open in default app</span>
    </button>
    {#if find.open}
      <span class="sep"></span>
      <FindBar />
    {/if}
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

  {#if downloading}
    <PreviewLoading
      label="Downloading from the cloud"
      detail="{relPath.split('/').pop()} is stored online only. This can take a moment for a large file."
      progress={hydration && hydration.total > 0
        ? {
            pct: Math.round((hydration.downloaded / hydration.total) * 100),
            note: `${fmtMb(hydration.downloaded)} / ${fmtMb(hydration.total)}`,
          }
        : { pct: null }}
    />
  {:else if cloudError}
    <div class="cloud">
      <p>
        <strong>{relPath.split("/").pop()}</strong> is stored online only, and
        the download didn't finish. Check that OneDrive is running and signed
        in, then try again.
      </p>
      <p class="cloud-detail">{cloudError}</p>
      <div class="cloud-actions">
        <button class="btn" onclick={() => void load()}>Try again</button>
        <button class="btn" onclick={() => api.openExternal(relPath)}>
          Open in default app
        </button>
      </div>
    </div>
  {:else if needsCloudConfirm && meta}
    <div class="cloud">
      <p>
        <strong>{relPath.split("/").pop()}</strong> is stored online only and is
        large ({Math.round(meta.size / (1024 * 1024))} MB). Downloading it can
        take a while, so Ken won't fetch it automatically.
      </p>
      <div class="cloud-actions">
        <button class="btn" onclick={() => void confirmCloudDownload()}>
          Download from cloud ({Math.round(meta.size / (1024 * 1024))} MB)
        </button>
        <button class="btn" onclick={() => api.openExternal(relPath)}>
          Open in default app
        </button>
      </div>
    </div>
  {:else if loadError}
    <div class="error">{loadError}</div>
  {:else if resolving}
    <!-- Kind/cloud-status not yet known: no preview or editor mounts until
         load() resolves, so an online-only file never flashes a preview. The
         Cancel returns to the notice/back-out so the app never wedges here. -->
    <div class="opening">
      <PreviewLoading label="Opening…" />
      <button class="btn btn-small" onclick={() => (loadError = "Opening cancelled — try again, or open in the default app.")}>
        Cancel
      </button>
    </div>
  {:else if tooLarge && meta}
    <TooLargeNotice {relPath} size={meta.size} />
  {:else if editable && content !== null}
    {#key reloadKey}
      {#if isHtml && !showSource && meta}
        <PreviewPane {relPath} {kind} {meta} />
      {:else if isCsv}
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
  .action.icon-only {
    padding: 3px 5px;
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
  /* Non-interactive metadata badge, styled to sit quietly alongside
     .save-state rather than read as a button. */
  .read-only {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 0 4px;
    flex: none;
    white-space: nowrap;
    color: var(--ink-tertiary);
    font-size: 11.5px;
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
    background: color-mix(in srgb, var(--needs-input) 8%, transparent);
    border: 1px solid color-mix(in srgb, var(--needs-input) 28%, transparent);
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
  .opening {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 12px;
    margin-top: 24px;
  }
  .error {
    margin: 16px 20px;
    padding: 12px 16px;
    background: color-mix(in srgb, var(--danger) 7%, transparent);
    border: 1px solid color-mix(in srgb, var(--danger) 25%, transparent);
    border-radius: 10px;
    font-size: 13px;
    color: var(--danger);
  }
  .cloud {
    margin: 48px auto;
    max-width: 420px;
    text-align: center;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 14px;
  }
  .cloud p {
    margin: 0;
    font-size: 13.5px;
    line-height: 1.65;
    color: var(--ink-secondary);
  }
  .cloud-detail {
    font-family: var(--font-mono);
    font-size: 11.5px !important;
    color: var(--ink-tertiary) !important;
    word-break: break-word;
  }
  .cloud-actions {
    display: flex;
    gap: 8px;
  }
</style>

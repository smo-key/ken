<script lang="ts">
  // One repo's detail, opened from its row on the workspace settings screen
  // (table → detail): its actions in the header, its settings in the form,
  // and Save pinned at the bottom for what it is for. Index and kind act on
  // the repo by id; the rest act on the focused repo, so the repo is focused
  // as this opens.
  import { onMount, untrack } from "svelte";
  import { app } from "../lib/app.svelte";
  import { api, memberLeaf, type IndexState, type RepoKind, type SourceRow, type SyncStatus } from "../lib/api";
  import { toast } from "../lib/toast.svelte";
  import { openConfirm } from "../lib/ui/ConfirmMenu.svelte";
  import EmptyState from "../lib/ui/EmptyState.svelte";
  import Loading from "../lib/ui/Loading.svelte";
  import { REPO_KINDS, kindValue } from "../lib/workspaceSettings";
  import {
    buildFolderTree,
    folderTriState,
    isExcluded,
    toggleFolder as toggleFolderPaths,
    type FolderNode,
  } from "../lib/folderTree";
  import ChevronDown from "@lucide/svelte/icons/chevron-down";
  import ChevronLeft from "@lucide/svelte/icons/chevron-left";
  import ChevronRight from "@lucide/svelte/icons/chevron-right";

  let {
    repo,
    workspace,
    knowledgeBase = false,
    analysis = null,
    close,
    changed,
  }: {
    repo: SourceRow;
    /** The workspace's name: the way back to the table. */
    workspace: string;
    knowledgeBase?: boolean;
    /** Where its analysis stands, beside Analyse again. */
    analysis?: string | null;
    close: () => void;
    changed?: () => void;
  } = $props();

  const leaf = $derived(memberLeaf(repo.name));
  let busy = $state<string | null>(null);

  /** An action here: `what` names it in the notice when it fails. */
  async function run(label: string, what: string, f: () => Promise<unknown>) {
    busy = label;
    try {
      await f();
    } catch (e) {
      toast.error(what, e);
    } finally {
      busy = null;
      changed?.();
    }
  }

  // ── Focus: every card below the form acts on the focused repo ───────
  let focusError = $state<string | null>(null);
  onMount(() => {
    if (!repo.available) return;
    void app
      .focusMember(repo.id, { stay: true })
      .then(() => {
        if (app.focused === repo.id) void loadSync();
      })
      .catch((e) => {
        focusError = String(e);
        toast.error(`Could not open ${leaf}`, e);
      });
  });
  const focusedHere = $derived(app.focused === repo.id);

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") close();
  }

  // ── The header's actions ────────────────────────────────────────────
  function openInFiles() {
    close();
    app.screen = "files";
  }

  function reindex() {
    void run("reindex", "Could not reindex", () => app.reindex());
  }

  // "Analyse again" (Chris, 2026-10-14): the source's earlier no is
  // forgotten and one background call is queued; its proposal arrives in
  // the Inbox, replacing one still waiting.
  function analyseAgain() {
    void run("analyse", `Could not analyse ${leaf} again`, () => api.analyzeSourceAgain(repo.id));
  }

  function remove(e: MouseEvent) {
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    openConfirm(rect.left - 220, rect.bottom + 4, {
      title: `Remove ${leaf} from the workspace?`,
      body: "Ken stops indexing it. The folder and its files stay on disk.",
      confirmLabel: "Remove",
      onConfirm: () =>
        void run("remove", "Could not remove the source", async () => {
          await api.workspaceRemoveMember(repo.name);
          await app.refreshWorkspaceOverview();
          close();
        }),
    });
  }

  // ── Index and kind, by id ───────────────────────────────────────────
  // Two states (knowledge-core decision 34): a repo is indexed or it is off.
  function setIndex(value: string) {
    void run("index", "Could not change whether Ken indexes it", () =>
      api.setProjectIndex(repo.id, value === "off" ? ("off" as IndexState) : null),
    );
  }

  function setKind(value: string) {
    const kind = value === "" ? [] : (value.split("+") as RepoKind[]);
    void run("kind", "Could not set the kind", () => api.setProjectKind(repo.id, kind));
  }

  // ── What it is for: saved from the bar at the bottom ────────────────
  // In a person's words: Ken reads it when it decides where something
  // belongs.
  let description = $state(untrack(() => repo.description));
  const dirty = $derived(description.trim() !== repo.description.trim());
  function saveDescription() {
    if (!dirty) return;
    void run("description", "Could not save what the source is for", () =>
      api.setProjectDescription(repo.id, description.trim()),
    );
  }

  // ── Watched folders ─────────────────────────────────────────────────
  let toggling = $state(false);
  const excludedSet = $derived(new Set(app.project?.excluded ?? []));
  const folderTree = $derived(buildFolderTree(app.folders));
  let expanded = $state<Set<string>>(new Set());

  function toggleExpand(relPath: string) {
    const next = new Set(expanded);
    if (next.has(relPath)) next.delete(relPath);
    else next.add(relPath);
    expanded = next;
  }

  async function toggleFolder(relPath: string) {
    if (!app.project || toggling) return;
    toggling = true;
    try {
      const next = toggleFolderPaths(relPath, isExcluded(relPath, excludedSet), excludedSet);
      await app.setExcluded(next);
    } catch (e) {
      toast.error("Could not change the watched folders", e);
    } finally {
      toggling = false;
    }
  }

  // ── Sync, the knowledge base only ───────────────────────────────────
  // Its state shows beside its row on the table; here are the switch and
  // Sync now.
  let sync = $state<SyncStatus | null>(null);
  let syncingNow = $state(false);
  async function loadSync() {
    if (!knowledgeBase) return;
    sync = await api.syncStatus(repo.id).catch(() => null);
  }

  async function toggleSyncAuto() {
    if (!sync) return;
    try {
      sync = await api.setSyncAuto(!sync.auto);
      changed?.();
    } catch (e) {
      toast.error("Could not change sync", e);
    }
  }

  async function syncNow() {
    syncingNow = true;
    try {
      await api.syncNow();
    } catch (e) {
      toast.error("Could not sync", e);
    } finally {
      // Live progress shows beside the knowledge base's row.
      setTimeout(() => (syncingNow = false), 1200);
    }
  }

  const syncable = $derived(knowledgeBase && sync?.mode === "git" && !!sync.remote);
</script>

<svelte:window onkeydown={onKey} />

<div class="detail">
  <button class="back" onclick={close}><ChevronLeft size={14} strokeWidth={1.75} />{workspace}</button>

  <div class="dhead">
    <span class="name">{leaf}</span>
    <span class="sub">{repo.files} files</span>
    <div class="dactions">
      {#if analysis}<span class="sub">{analysis}</span>{/if}
      {#if repo.canAnalyzeAgain}
        <button class="btn btn-ghost btn-small" disabled={repo.analyzing || busy !== null} onclick={analyseAgain}>
          {repo.analyzing ? "Analysing…" : "Analyse again"}
        </button>
      {/if}
      {#if syncable}
        <button class="btn btn-ghost btn-small" disabled={!sync?.active || syncingNow} onclick={() => void syncNow()}>
          {syncingNow ? "Syncing…" : "Sync now"}
        </button>
      {/if}
      <button class="btn btn-ghost btn-small" disabled={!focusedHere} onclick={openInFiles}>Open in Files</button>
      <button class="btn btn-ghost btn-small" disabled={!focusedHere || busy !== null} onclick={reindex}>
        {busy === "reindex" ? "Rebuilding…" : "Reindex"}
      </button>
      {#if !knowledgeBase}
        <button class="btn btn-ghost btn-small danger" disabled={busy !== null} onclick={remove}>Remove</button>
      {/if}
    </div>
  </div>

  <div class="form">
    <div class="field">
      <span class="label">Index</span>
      <span class="sel">
        <select value={repo.effectiveIndex} disabled={busy !== null} onchange={(e) => setIndex(e.currentTarget.value)} aria-label="Index">
          <option value="indexed">indexed</option>
          <option value="off">off</option>
        </select>
        <ChevronDown size={12} strokeWidth={1.75} />
      </span>
    </div>
    <div class="field">
      <span class="label">Kind</span>
      <span class="sel">
        <select value={kindValue(repo)} disabled={busy !== null} onchange={(e) => setKind(e.currentTarget.value)} aria-label="Kind">
          {#if repo.kind.length > 1}<option value={kindValue(repo)}>{repo.kind.join(" + ")}</option>{/if}
          {#each REPO_KINDS as k (k.v)}<option value={k.v}>{k.label}</option>{/each}
        </select>
        <ChevronDown size={12} strokeWidth={1.75} />
      </span>
    </div>
    {#if repo.health}
      <div class="field">
        <span class="label">Health</span>
        <span>{repo.health}</span>
      </div>
    {/if}
    <label class="field top">
      <span class="label">For</span>
      <textarea rows="3" bind:value={description} placeholder="What it holds, who uses it"></textarea>
    </label>
  </div>

  {#if !repo.available}
    <EmptyState title="Folder gone" />
  {:else if focusError}
    <EmptyState title="Not open" />
  {:else if !focusedHere}
    <Loading label="Opening {leaf}…" />
  {:else}
    <div class="divider">watched folders · {app.folders.length}</div>
    {#if app.folders.length === 0}
      <EmptyState />
    {:else}
      <div class="folder-tree">
        {#each folderTree as node (node.relPath)}
          {@render folderRow(node, 0)}
        {/each}
      </div>
    {/if}

    {#snippet folderRow(node: FolderNode, depth: number)}
      {@const tri = folderTriState(node.relPath, excludedSet)}
      <div class="frow" style:padding-left={`${depth * 18}px`}>
        {#if node.children.length > 0}
          <button
            class="chev"
            class:open={expanded.has(node.relPath)}
            aria-label={expanded.has(node.relPath) ? "Collapse" : "Expand"}
            onclick={() => toggleExpand(node.relPath)}
          >
            <ChevronRight size={14} strokeWidth={2} />
          </button>
        {:else}
          <span class="chev-spacer"></span>
        {/if}
        <label class="fcheck">
          <input
            type="checkbox"
            checked={tri === "checked"}
            indeterminate={tri === "indeterminate"}
            disabled={toggling}
            onchange={() => toggleFolder(node.relPath)}
          />
          <span class="mono">{node.name}</span>
        </label>
      </div>
      {#if expanded.has(node.relPath)}
        {#each node.children as child (child.relPath)}
          {@render folderRow(child, depth + 1)}
        {/each}
      {/if}
    {/snippet}

    <div class="divider">indexing</div>
    <label class="check">
      <input type="checkbox" checked={app.backgroundIndex} onchange={(e) => void app.setBackgroundIndex(e.currentTarget.checked)} />
      Index cloud files in the background
    </label>
    <label class="check">
      <input
        type="checkbox"
        checked={app.transcribeVideosOnIndex}
        onchange={(e) => void app.setTranscribeVideosOnIndex(e.currentTarget.checked)}
      />
      Transcribe videos while indexing
    </label>
    {#if syncable && sync}
      <label class="check">
        <input type="checkbox" checked={sync.auto} onchange={() => void toggleSyncAuto()} />
        Keep in sync
      </label>
    {/if}
  {/if}

  <div class="savebar">
    {#if dirty}<button class="btn btn-ghost btn-small" onclick={() => (description = repo.description)}>Discard</button>{/if}
    <button class="btn btn-primary btn-small" disabled={!dirty || busy !== null} onclick={saveDescription}>
      {busy === "description" ? "Saving…" : "Save"}
    </button>
  </div>
</div>

<style>
  .detail {
    display: flex;
    flex-direction: column;
    gap: 8px;
    min-height: 100%;
  }
  .back {
    align-self: flex-start;
    display: inline-flex;
    align-items: center;
    gap: 2px;
    border: none;
    background: transparent;
    padding: 2px 4px 2px 0;
    font: inherit;
    font-size: 12.5px;
    color: var(--ink-tertiary);
    cursor: pointer;
  }
  .back:hover {
    color: var(--accent-deep);
  }
  .dhead {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 6px 10px;
  }
  .name {
    font-family: var(--font-serif);
    font-size: 21px;
    font-weight: 500;
  }
  .sub {
    font-size: 12px;
    color: var(--ink-tertiary);
    white-space: nowrap;
  }
  .dactions {
    margin-left: auto;
    align-self: center;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
  }
  .danger:hover:not(:disabled) {
    color: var(--danger);
  }
  .form {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin-top: 6px;
  }
  .field {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 12.5px;
  }
  .field.top {
    align-items: flex-start;
  }
  .label {
    width: 56px;
    flex: none;
    font-size: 12px;
    font-weight: 600;
    color: var(--ink-secondary);
  }
  .field.top .label {
    padding-top: 6px;
  }
  textarea {
    flex: 1;
    max-width: 560px;
    font: inherit;
    font-size: 12.5px;
    line-height: 1.5;
    padding: 6px 8px;
    border-radius: 7px;
    border: 1px solid var(--border-strong);
    background: var(--surface);
    color: var(--ink);
    resize: vertical;
  }
  .sel {
    position: relative;
    display: inline-flex;
    align-items: center;
    color: var(--ink-tertiary);
  }
  .sel :global(svg) {
    position: absolute;
    right: 8px;
    pointer-events: none;
  }
  .sel select {
    appearance: none;
    -webkit-appearance: none;
    font: inherit;
    font-size: 12px;
    color: var(--ink);
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 4px 26px 4px 9px;
    min-width: 120px;
    cursor: pointer;
    outline: none;
  }
  .sel select:hover:not(:disabled) {
    border-color: var(--border-strong);
  }
  .sel select:focus-visible {
    border-color: var(--accent);
  }
  .sel select:disabled {
    cursor: default;
    opacity: 0.6;
  }
  .sel select option {
    background: var(--surface);
    color: var(--ink);
  }
  .divider {
    margin-top: 10px;
    padding-bottom: 4px;
    border-bottom: 1px solid var(--border);
    font-size: 10.5px;
    font-weight: 700;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--ink-tertiary);
  }
  .check {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 13px;
    cursor: pointer;
  }
  .check input,
  .fcheck input {
    accent-color: var(--accent);
  }
  .folder-tree {
    display: flex;
    flex-direction: column;
    gap: 2px;
    max-height: 320px;
    overflow-y: auto;
  }
  .frow {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .chev {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 18px;
    height: 18px;
    border: none;
    background: transparent;
    color: var(--ink-tertiary);
    border-radius: 4px;
    transition: transform 0.15s ease;
  }
  .chev:hover {
    background: var(--sunken);
    color: var(--ink);
  }
  .chev.open {
    transform: rotate(90deg);
  }
  .chev-spacer {
    width: 18px;
    flex: none;
  }
  .fcheck {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    font-size: 12.5px;
    cursor: pointer;
  }
  /* Save, pinned to the bottom of the settings pane. */
  .savebar {
    position: sticky;
    bottom: 0;
    margin-top: auto;
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    padding: 10px 0;
    background: var(--paper);
    border-top: 1px solid var(--border);
  }
</style>

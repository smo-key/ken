<script lang="ts">
  import { app } from "../lib/app.svelte";
  import { ingests } from "../lib/ingests.svelte";
  import { api } from "../lib/api";

  let busy = $state(false);
  let toggling = $state(false);
  let runnerMode = $state<"hidden-tui" | "headless">(
    app.project?.ingestRunner ?? "hidden-tui",
  );

  async function setRunnerMode(mode: "hidden-tui" | "headless") {
    runnerMode = mode;
    await api.setIngestRunnerMode(mode);
  }

  const included = $derived(
    app.folders.filter((f) => !f.excluded).map((f) => f.relPath),
  );

  async function toggleFolder(relPath: string, currentlyExcluded: boolean) {
    if (!app.project || toggling) return;
    toggling = true;
    try {
      const ex = new Set(app.project.excluded);
      if (currentlyExcluded) {
        // Re-include: drop this path and any children from the exclusion list.
        for (const e of [...ex]) {
          if (e === relPath || e.startsWith(relPath + "/")) ex.delete(e);
        }
      } else {
        ex.add(relPath);
      }
      await app.setExcluded([...ex]);
    } finally {
      toggling = false;
    }
  }

  async function reindex() {
    busy = true;
    try {
      await app.reindex();
    } finally {
      busy = false;
    }
  }
</script>

<div class="wrap">
  <div class="inner">
    <h1>Settings</h1>

    <div class="card">
      <div class="card-title">Project</div>
      <div class="row">
        <span class="label">Name</span>
        <span>{app.project?.name}</span>
      </div>
      <div class="row">
        <span class="label">Folder</span>
        <span class="mono small">{app.project?.root}</span>
      </div>
      <div class="row">
        <span class="label">Index</span>
        <span>{app.files.length} files
          {#if app.failedFiles.length}· {app.failedFiles.length} failed{/if}
        </span>
        <button class="btn btn-small" onclick={reindex} disabled={busy}>
          {busy ? "Rebuilding…" : "Reindex"}
        </button>
      </div>
      <p class="note">
        Reindex rebuilds Ken's local index from your files. It never changes the
        files themselves.
      </p>
    </div>

    <div class="card">
      <div class="card-title">Watched folders</div>
      <p class="note">
        Ken ingests every folder by default. Uncheck one to leave it out of
        search and future AI features.
      </p>
      {#if app.folders.length === 0}
        <p class="note">No subfolders — everything at the top level is watched.</p>
      {/if}
      <div class="folders">
        {#each app.folders as folder (folder.relPath)}
          <label
            class="folder"
            style:padding-left={`${(folder.relPath.split("/").length - 1) * 18}px`}
          >
            <input
              type="checkbox"
              checked={!folder.excluded}
              disabled={toggling}
              onchange={() => toggleFolder(folder.relPath, folder.excluded)}
            />
            <span class="mono">{folder.relPath}</span>
            {#if folder.excluded}
              <span class="tag">excluded</span>
            {/if}
          </label>
        {/each}
      </div>
    </div>

    <div class="card">
      <div class="card-title">AI runner</div>
      {#if ingests.doctor?.found}
        <p class="note">
          <span class="ok-dot"></span>Claude Code found
          {#if ingests.doctor.version}({ingests.doctor.version}){/if}
          <span class="mono small">{ingests.doctor.path}</span>
        </p>
      {:else}
        <p class="note warn">
          Claude Code isn't installed — ingests can't run until it is.
          <span class="mono small">npm i -g @anthropic-ai/claude-code</span>
        </p>
      {/if}
      <div class="row">
        <span class="label">Mode</span>
        <label class="radio">
          <input
            type="radio"
            name="runner"
            checked={runnerMode === "hidden-tui"}
            onchange={() => setRunnerMode("hidden-tui")}
          />
          Interactive session <span class="soft">(recommended — you can watch or step in)</span>
        </label>
      </div>
      <div class="row">
        <span class="label"></span>
        <label class="radio">
          <input
            type="radio"
            name="runner"
            checked={runnerMode === "headless"}
            onchange={() => setRunnerMode("headless")}
          />
          Headless <span class="soft">(runs invisibly, no mid-run questions)</span>
        </label>
      </div>
    </div>

    <div class="card muted">
      <div class="card-title">Coming to Ken</div>
      <p class="note">
        MCP server for your agents, Git &amp; shared-drive sync with conflict
        review, chats with Claude, and a unified Review inbox — each arrives in
        an upcoming release.
      </p>
    </div>
  </div>
</div>

<style>
  .wrap {
    flex: 1;
    min-width: 0;
    overflow-y: auto;
    padding: 36px 44px;
  }
  .inner {
    max-width: 720px;
    margin: 0 auto;
    display: flex;
    flex-direction: column;
    gap: 18px;
  }
  h1 {
    margin: 0;
    font-family: var(--font-serif);
    font-size: 28px;
    font-weight: 500;
  }
  .card {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-card);
    padding: 20px 22px;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .card.muted {
    color: var(--ink-tertiary);
  }
  .card-title {
    font-size: 14px;
    font-weight: 600;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 13px;
  }
  .label {
    width: 64px;
    flex: none;
    font-size: 12px;
    font-weight: 600;
    color: var(--ink-secondary);
  }
  .small {
    font-size: 12px;
  }
  .note {
    margin: 0;
    font-size: 12.5px;
    color: var(--ink-tertiary);
    line-height: 1.6;
  }
  .folders {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .folder {
    display: flex;
    align-items: center;
    gap: 9px;
    font-size: 13px;
    cursor: pointer;
    padding: 3px 0;
  }
  .folder input {
    accent-color: var(--accent);
  }
  .tag {
    font-size: 10px;
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 0 5px;
    color: var(--ink-tertiary);
  }
  .btn {
    margin-left: auto;
  }
  .radio {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 13px;
    cursor: pointer;
  }
  .radio input {
    accent-color: var(--accent);
  }
  .soft {
    color: var(--ink-tertiary);
    font-size: 12px;
  }
  .ok-dot {
    display: inline-block;
    width: 7px;
    height: 7px;
    border-radius: 4px;
    background: var(--healthy);
    margin-right: 7px;
  }
  .note.warn {
    color: var(--needs-input-text);
  }
</style>

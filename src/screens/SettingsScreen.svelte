<script lang="ts">
  import { onMount } from "svelte";
  import { app } from "../lib/app.svelte";
  import { ingests } from "../lib/ingests.svelte";
  import { api, type McpInfo, type SyncStatus } from "../lib/api";

  let busy = $state(false);
  let toggling = $state(false);
  let runnerMode = $state<"hidden-tui" | "headless">(
    app.project?.ingestRunner ?? "headless",
  );
  let sync = $state<SyncStatus | null>(null);
  let syncingNow = $state(false);
  let mcp = $state<McpInfo | null>(null);
  let copied = $state<"command" | "instruction" | null>(null);
  let copyTimer: ReturnType<typeof setTimeout> | undefined;

  onMount(() => {
    void api.syncStatus().then((s) => (sync = s)).catch(() => (sync = null));
    void api.mcpInfo().then((m) => (mcp = m)).catch(() => (mcp = null));
    return () => clearTimeout(copyTimer);
  });

  async function copy(text: string, what: "command" | "instruction") {
    try {
      await navigator.clipboard.writeText(text);
      copied = what;
      clearTimeout(copyTimer);
      copyTimer = setTimeout(() => (copied = null), 1600);
    } catch {
      // Clipboard unavailable — leave the button as-is.
    }
  }

  async function toggleSyncAuto() {
    if (!sync) return;
    sync = await api.setSyncAuto(!sync.auto);
  }

  async function syncNow() {
    syncingNow = true;
    try {
      await api.syncNow();
    } finally {
      // Brief acknowledgement; live progress shows on the title-bar dot.
      setTimeout(() => (syncingNow = false), 1200);
    }
  }

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
            checked={runnerMode === "headless"}
            onchange={() => setRunnerMode("headless")}
          />
          Background <span class="soft">(recommended — can't get stuck on setup prompts)</span>
        </label>
      </div>
      <div class="row">
        <span class="label"></span>
        <label class="radio">
          <input
            type="radio"
            name="runner"
            checked={runnerMode === "hidden-tui"}
            onchange={() => setRunnerMode("hidden-tui")}
          />
          Interactive <span class="soft">(watch or step in via Chats; Claude's one-time prompts need answering there)</span>
        </label>
      </div>
    </div>

    <div class="card">
      <div class="card-title">Sync &amp; collaboration</div>
      {#if sync?.mode === "git"}
        <div class="row">
          <span class="chip mono">git</span>
          {#if sync.remote}
            <span class="mono small">{sync.remote} {sync.branch ?? ""}</span>
            <span class="soft">
              {sync.active
                ? "updates flow automatically · conflicts go to Review"
                : "automatic updates are off"}
            </span>
          {:else}
            <span class="soft">
              no shared location set up yet — Ken keeps everything local
            </span>
          {/if}
        </div>
        {#if sync.remote}
          <div class="row">
            <label class="radio">
              <input
                type="checkbox"
                checked={sync.auto}
                onchange={() => void toggleSyncAuto()}
              />
              Keep this project in sync automatically
            </label>
            <button
              class="btn btn-small sync-now"
              onclick={() => void syncNow()}
              disabled={!sync.active || syncingNow}
            >
              {syncingNow ? "Syncing…" : "Sync now"}
            </button>
          </div>
          <p class="note">
            Ken fetches your team's updates when you return to the app and
            shares your saves shortly after you make them. When two people
            change the same document, both versions land in Review.
          </p>
        {/if}
      {:else}
        <div class="row">
          <span class="chip mono">shared drive</span>
          <span class="soft">
            Ken watches for conflicting copies — they land in Review.
          </span>
        </div>
        <p class="note">
          If this folder lives in Dropbox, OneDrive, or Google Drive, the
          drive does the syncing; Ken keeps an eye out for the damage
          conflicting edits leave behind.
        </p>
      {/if}
    </div>

    <div class="card">
      <div class="mcp-head">
        <span class="card-title">Connect an agent</span>
        {#if mcp?.binaryPath}
          <span class="mcp-status">
            <span class="ok-dot"></span>Ready — agents start it on demand
          </span>
        {/if}
      </div>
      <p class="note">
        Ken's connector lets Claude Code, Cursor, and other agents search this
        project's knowledge and read its documents. It can only read — never
        change — your files.
      </p>
      {#if mcp?.binaryPath}
        <div class="mcp-block">
          <div class="mcp-comment"># add Ken to any agent — scoped to this project</div>
          <div class="mcp-cmd-row">
            <code class="mcp-cmd">{mcp.addCommand}</code>
            <button
              class="mcp-copy"
              onclick={() => mcp && copy(mcp.addCommand, "command")}
            >
              {copied === "command" ? "copied" : "⧉ copy"}
            </button>
          </div>
        </div>
        <div class="mcp-chips">
          <span class="mcp-chip">
            <strong>Scope</strong> — this project only
          </span>
          <button
            class="mcp-chip mcp-chip-btn"
            onclick={() => mcp && copy(mcp.llmInstruction, "instruction")}
          >
            <strong>LLM instruction</strong> — paste into any agent ·
            <span class="mcp-chip-action">
              {copied === "instruction" ? "copied" : "⧉ copy"}
            </span>
          </button>
        </div>
      {:else if mcp}
        <p class="note">
          The connector (<span class="mono small">ken-mcp</span>) ships with
          Ken's installer but wasn't found on this machine — reinstalling Ken
          restores it. Building from source? Run
          <span class="mono small">cargo build -p ken-mcp</span>.
        </p>
      {/if}
    </div>

    <div class="card muted">
      <div class="card-title">Coming to Ken</div>
      <p class="note">
        Knowledge Map and Timeline views — each arrives in an upcoming
        release.
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
  .chip {
    font-size: 12px;
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 3px 9px;
    background: var(--sunken);
    flex: none;
  }
  .sync-now {
    margin-left: auto;
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
  .mcp-head {
    display: flex;
    align-items: center;
    gap: 9px;
  }
  .mcp-head .card-title {
    flex: 1;
  }
  .mcp-status {
    display: inline-flex;
    align-items: center;
    font-size: 12px;
    font-weight: 600;
    color: var(--healthy-text);
  }
  .mcp-block {
    background: var(--terminal-bg);
    border-radius: 10px;
    padding: 13px 16px;
    font-family: var(--font-mono);
    font-size: 12px;
    line-height: 1.7;
  }
  .mcp-comment {
    color: var(--ink-tertiary);
  }
  .mcp-cmd-row {
    display: flex;
    align-items: baseline;
    gap: 12px;
  }
  .mcp-cmd {
    flex: 1;
    min-width: 0;
    color: var(--terminal-text);
    font-family: inherit;
    word-break: break-all;
  }
  .mcp-copy {
    flex: none;
    border: none;
    background: none;
    padding: 0;
    color: var(--terminal-prompt);
    font-family: inherit;
    font-size: 12px;
    cursor: pointer;
  }
  .mcp-chips {
    display: flex;
    gap: 10px;
    flex-wrap: wrap;
    font-size: 12.5px;
  }
  .mcp-chip {
    flex: 1;
    min-width: 200px;
    border: 1px solid var(--border);
    border-radius: 9px;
    padding: 10px 12px;
    background: var(--sunken);
    text-align: left;
    line-height: 1.5;
  }
  .mcp-chip-btn {
    font: inherit;
    color: inherit;
    cursor: pointer;
  }
  .mcp-chip-btn:hover {
    border-color: var(--border-strong);
  }
  .mcp-chip-action {
    color: var(--accent);
    font-weight: 600;
  }
</style>

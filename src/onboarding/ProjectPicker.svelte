<script lang="ts">
  import { onMount } from "svelte";
  import { open as openDialog } from "@tauri-apps/plugin-dialog";
  import {
    api,
    memberGroup,
    memberLeaf,
    type Candidate,
    type FeatureInfo,
    type ProjectProfile,
    type RegistryEntryStatus,
  } from "../lib/api";
  import { app } from "../lib/app.svelte";
  import KenMark from "../lib/ui/KenMark.svelte";
  import FolderOpen from "@lucide/svelte/icons/folder-open";
  import Trash2 from "@lucide/svelte/icons/trash-2";
  import ChevronRight from "@lucide/svelte/icons/chevron-right";
  import ContextMenu, { openContextMenu } from "../lib/ui/ContextMenu.svelte";
  import ConfirmMenu, { openConfirm } from "../lib/ui/ConfirmMenu.svelte";

  let error = $state<string | null>(null);

  async function forgetId(id: string) {
    await api.forgetProject(id);
    await app.refreshRegistry();
  }

  function rowMenu(e: MouseEvent, entry: RegistryEntryStatus) {
    e.preventDefault();
    const x = e.clientX;
    const y = e.clientY;
    openContextMenu(x, y, [
      {
        label: "Open",
        icon: FolderOpen,
        disabled: !entry.available,
        onSelect: () => openExisting(entry.path, entry.available),
      },
      "separator",
      {
        label: "Remove from Ken…",
        icon: Trash2,
        danger: true,
        onSelect: () =>
          openConfirm(x, y, {
            title: `Remove “${entry.name}”?`,
            body: "Removes it from Ken's list — the folder and its files stay on disk.",
            confirmLabel: "Remove from Ken",
            onConfirm: () => void forgetId(entry.id),
          }),
      },
    ]);
  }
  let pendingPath = $state<string | null>(null);
  let pendingName = $state("");

  // Project-scoped feature flags offered during onboarding, plus the
  // toggles the user actually changed from their default (only those are
  // written once the project exists — see confirmCreate).
  let features = $state<FeatureInfo[]>([]);
  let featureChoices = $state<Record<string, boolean>>({});
  let featuresOpen = $state(false);

  // project-profiler task 3.2: minimal "analyzing" affordance for the
  // chosen (not-yet-a-project) folder, driven by `profile_candidates` +
  // `onProfileState` (path-keyed — see api.ts's `ProfileState`). Only a
  // single candidate is ever in flight here (this flow creates one project
  // at a time); a real multi-candidate workspace-creation checklist is
  // deferred (tasks.md 3.2 note) — Phase 2 never built the picker UI it'd
  // need.
  let candidateProfileState = $state<"scanning" | "refining" | "ready" | "error" | null>(null);
  let candidateProfile = $state<ProjectProfile | null>(null);
  // Hides the affordance locally without cancelling the backend scan (no
  // cancel-profile command exists) — "skip" here means "stop waiting to see
  // it," which is honest since project creation was never gated on it.
  let analysisDismissed = $state(false);
  let unlistenProfile: (() => void) | undefined;

  onMount(() => {
    void api.onProfileState((ev) => {
      if (ev.path !== pendingPath) return;
      if (ev.state === "ready") candidateProfile = ev.profile;
      candidateProfileState = ev.state;
    }).then((fn) => (unlistenProfile = fn));
    return () => unlistenProfile?.();
  });

  async function chooseFolder() {
    error = null;
    const folder = await openDialog({
      directory: true,
      title: "Choose the folder that holds your knowledge",
    });
    if (typeof folder !== "string") return;
    pendingPath = folder;
    pendingName = folder.split("/").pop() ?? "My project";
    featuresOpen = false;
    candidateProfileState = null;
    candidateProfile = null;
    analysisDismissed = false;
    try {
      const all = await api.listFeatures();
      features = all.filter((f) => f.scope === "project");
      featureChoices = Object.fromEntries(features.map((f) => [f.name, f.effective]));
      // `profile_candidates` isn't flag-gated server-side (the folder isn't a
      // project yet, so there's no per-project override to read) — per its
      // lib.rs doc comment, callers are expected to check the *global*
      // `profiler` default themselves first.
      if (all.find((f) => f.name === "profiler")?.effective) {
        void api.profileCandidates([folder]);
      }
    } catch {
      features = [];
      featureChoices = {};
    }
  }

  async function confirmCreate() {
    if (!pendingPath) return;
    try {
      const changed = features.filter((f) => featureChoices[f.name] !== f.effective);
      await app.createProject(pendingPath, pendingName.trim() || "My project");
      for (const f of changed) {
        const value = featureChoices[f.name];
        if (f.name === "semanticIndex") {
          await app.setSemanticIndex(value);
        } else {
          await api.setProjectFeature(f.name, value);
        }
      }
    } catch (e) {
      error = String(e);
    }
  }

  async function openExisting(path: string, available: boolean) {
    if (!available) return;
    error = null;
    try {
      await app.openProject(path);
    } catch (e) {
      error = String(e);
    }
  }

  async function forget(id: string, e: MouseEvent) {
    e.stopPropagation();
    await api.forgetProject(id);
    await app.refreshRegistry();
  }

  // ---- Workspace creation flow (workspace task 4.2) ----
  // Flag-gated (app.workspaceFlagEnabled) parallel wizard to the single-
  // folder flow above: parent folder → candidate checklist (existing
  // projects pre-checked, marker/file-count captions, per-candidate
  // include toggle) → name → the same Features disclosure pattern
  // `confirmCreate` uses, filtered to workspace-scoped flags → create.
  //
  // No recent-workspaces section: `Registry` gained recent-workspace
  // entries (workspace task 1.4) but no command reads them back to the
  // frontend (grepped `src-tauri/src/lib.rs` for `workspace_statuses`/
  // `registry.workspaces` — nothing registered). Deferred rather than
  // invented; see final report.
  type WsStep = "candidates" | "name";
  let wsParent = $state<string | null>(null);
  let wsStep = $state<WsStep>("candidates");
  let wsCandidates = $state<Candidate[]>([]);
  let wsIncluded = $state<Record<string, boolean>>({});
  let wsName = $state("");
  let wsFeatures = $state<FeatureInfo[]>([]);
  let wsFeatureChoices = $state<Record<string, boolean>>({});
  let wsFeaturesOpen = $state(false);
  let wsBusy = $state(false);
  let wsError = $state<string | null>(null);

  async function chooseWorkspaceFolder() {
    error = null;
    const folder = await openDialog({
      directory: true,
      title: "Choose the parent folder that holds your projects",
    });
    if (typeof folder !== "string") return;
    wsError = null;
    wsParent = folder;
    wsStep = "candidates";
    wsBusy = true;
    try {
      wsCandidates = await api.discoverWorkspaceCandidates(folder);
      wsIncluded = Object.fromEntries(wsCandidates.map((c) => [c.name, c.existing]));
    } catch (e) {
      wsError = String(e);
      wsCandidates = [];
    } finally {
      wsBusy = false;
    }
  }

  function wsToName() {
    if (!wsParent) return;
    wsName = wsParent.split("/").pop() ?? "Workspace";
    wsStep = "name";
    wsFeaturesOpen = false;
    void api
      .listFeatures()
      .then((all) => {
        wsFeatures = all.filter((f) => f.scope === "workspace");
        wsFeatureChoices = Object.fromEntries(wsFeatures.map((f) => [f.name, f.effective]));
      })
      .catch(() => {
        wsFeatures = [];
        wsFeatureChoices = {};
      });
  }

  async function confirmCreateWorkspace() {
    if (!wsParent || wsBusy) return;
    const members = wsCandidates.filter((c) => wsIncluded[c.name]).map((c) => c.name);
    if (members.length === 0) {
      wsError = "Choose at least one folder to include.";
      return;
    }
    wsBusy = true;
    wsError = null;
    try {
      const changed = wsFeatures.filter((f) => wsFeatureChoices[f.name] !== f.effective);
      await app.createWorkspace(wsParent, wsName.trim() || "Workspace", members);
      for (const f of changed) {
        await api.setGlobalFeature(f.name, wsFeatureChoices[f.name]);
      }
    } catch (e) {
      wsError = String(e);
    } finally {
      wsBusy = false;
    }
  }

  function cancelWorkspaceFlow() {
    wsParent = null;
    wsStep = "candidates";
    wsCandidates = [];
    wsIncluded = {};
    wsError = null;
  }

  function wsBack() {
    if (wsStep === "name") {
      wsStep = "candidates";
    } else {
      cancelWorkspaceFlow();
    }
  }
</script>

<div class="wrap" data-tauri-drag-region>
  <div class="panel">
    <div class="brand">
      <KenMark size={36} />
      <span class="wordmark">Ken</span>
    </div>
    <h1>Your team's knowledge, in one calm place.</h1>
    <p class="lede">
      Point Ken at a folder — notes, documents, spreadsheets, anything. Ken
      reads it, keeps watch, and makes every fact findable.
    </p>

    {#if pendingPath}
      <div class="confirm">
        <div class="mono path">{pendingPath}</div>
        <label>
          Project name
          <input
            bind:value={pendingName}
            onkeydown={(e) => {
              e.stopPropagation();
              if (e.key === "Enter") confirmCreate();
            }}
          />
        </label>

        {#if !analysisDismissed && (candidateProfileState === "scanning" || candidateProfileState === "refining" || (candidateProfileState === "ready" && candidateProfile))}
          <div class="analyzing" role="status" aria-live="polite">
            {#if candidateProfileState === "ready" && candidateProfile}
              <span class="kind-badge">{candidateProfile.kind}</span>
              {#if candidateProfile.summary}
                <span class="analyzing-text">{candidateProfile.summary}</span>
              {/if}
            {:else}
              <span class="mini-spinner"></span>
              <span class="analyzing-text">
                {candidateProfileState === "refining"
                  ? "Refining analysis…"
                  : "Analyzing project…"}
              </span>
              <button
                type="button"
                class="skip-analysis"
                onclick={() => (analysisDismissed = true)}
              >
                Skip
              </button>
            {/if}
          </div>
        {/if}

        {#if features.length > 0}
          <div class="features">
            <button
              type="button"
              class="features-toggle"
              onclick={() => (featuresOpen = !featuresOpen)}
              aria-expanded={featuresOpen}
            >
              <span class="chev" class:open={featuresOpen}>
                <ChevronRight size={13} strokeWidth={2} />
              </span>
              Features
            </button>
            {#if featuresOpen}
              <div class="features-body">
                {#each features as flag (flag.name)}
                  <label class="radio feature-row">
                    <input type="checkbox" bind:checked={featureChoices[flag.name]} />
                    <span class="feature-text">
                      <span class="feature-name">{flag.name}</span>
                      <span class="note">{flag.description}</span>
                    </span>
                  </label>
                {/each}
                <p class="note">You can change these later in Settings.</p>
              </div>
            {/if}
          </div>
        {/if}

        <div class="confirm-actions">
          <button class="btn btn-primary" onclick={confirmCreate}>Create project</button>
          <button class="btn btn-ghost" onclick={() => (pendingPath = null)}>Back</button>
        </div>
      </div>
    {:else if wsParent}
      <div class="confirm">
        <div class="mono path">{wsParent}</div>

        {#if wsStep === "candidates"}
          <div class="ws-candidates">
            {#if wsBusy}
              <p class="note">Scanning folders…</p>
            {:else if wsCandidates.length === 0}
              <p class="note">No subfolders found in this parent.</p>
            {:else}
              {#each wsCandidates as c (c.name)}
                <label class="radio feature-row">
                  <input type="checkbox" bind:checked={wsIncluded[c.name]} />
                  <span class="feature-text">
                    <span class="feature-name ws-name">
                      {memberLeaf(c.name)}{#if memberGroup(c.name)}<span class="ws-group"> in {memberGroup(c.name)}/</span>{/if}
                    </span>
                    <span class="note">
                      {c.existing ? "Existing Ken project" : "New"} · {c.fileCount}
                      {c.fileCount === 1 ? "file" : "files"}{#if c.markers.length > 0} · {c.markers.join(", ")}{/if}
                    </span>
                  </span>
                </label>
              {/each}
            {/if}
          </div>
          <div class="confirm-actions">
            <button
              class="btn btn-primary"
              disabled={wsBusy || wsCandidates.length === 0}
              onclick={wsToName}
            >
              Next
            </button>
            <button class="btn btn-ghost" onclick={cancelWorkspaceFlow}>Cancel</button>
          </div>
        {:else}
          <label>
            Workspace name
            <input
              bind:value={wsName}
              onkeydown={(e) => {
                e.stopPropagation();
                if (e.key === "Enter") confirmCreateWorkspace();
              }}
            />
          </label>

          {#if wsFeatures.length > 0}
            <div class="features">
              <button
                type="button"
                class="features-toggle"
                onclick={() => (wsFeaturesOpen = !wsFeaturesOpen)}
                aria-expanded={wsFeaturesOpen}
              >
                <span class="chev" class:open={wsFeaturesOpen}>
                  <ChevronRight size={13} strokeWidth={2} />
                </span>
                Features
              </button>
              {#if wsFeaturesOpen}
                <div class="features-body">
                  {#each wsFeatures as flag (flag.name)}
                    <label class="radio feature-row">
                      <input type="checkbox" bind:checked={wsFeatureChoices[flag.name]} />
                      <span class="feature-text">
                        <span class="feature-name">{flag.name}</span>
                        <span class="note">{flag.description}</span>
                      </span>
                    </label>
                  {/each}
                  <p class="note">You can change these later in Settings.</p>
                </div>
              {/if}
            </div>
          {/if}

          <div class="confirm-actions">
            <button class="btn btn-primary" disabled={wsBusy} onclick={confirmCreateWorkspace}>
              Create workspace
            </button>
            <button class="btn btn-ghost" onclick={wsBack}>Back</button>
          </div>
        {/if}

        {#if wsError}
          <div class="error">{wsError}</div>
        {/if}
      </div>
    {:else}
      <!-- Two genuinely different outcomes, so they read as a choice
           rather than an action plus an afterthought. The old pair —
           a primary "Choose a folder…" above a ghost "Open a workspace…"
           — pushed people into single-project mode by emphasis, and
           "Open a workspace" sounded like opening an existing one rather
           than building one from a folder of repos. -->
      <div class="choose-row" class:stacked={app.workspaceFlagEnabled}>
        {#if app.workspaceFlagEnabled}
          <button class="choice" onclick={chooseWorkspaceFolder}>
            <span class="choice-title">Several projects</span>
            <span class="choice-note">
              Pick the folder that CONTAINS your repos. Ken tracks each one
              separately and you can search across all of them.
            </span>
          </button>
        {/if}
        <button class="choice" class:solo={!app.workspaceFlagEnabled} onclick={chooseFolder}>
          <span class="choice-title">One project</span>
          <span class="choice-note">
            Pick a single folder — one repo, or one set of notes.
          </span>
        </button>
      </div>
    {/if}

    {#if error}
      <div class="error">{error}</div>
    {/if}

    {#if app.registry.length > 0 && !pendingPath && !wsParent}
      <div class="recent-label">Recent projects</div>
      <div class="recents">
        {#each app.registry as entry (entry.id)}
          <button
            class="recent"
            class:unavailable={!entry.available}
            onclick={() => openExisting(entry.path, entry.available)}
            oncontextmenu={(e) => rowMenu(e, entry)}
          >
            <span class="badge">{entry.name.charAt(0).toUpperCase()}</span>
            <span class="info">
              <span class="name">{entry.name}</span>
              <span class="mono path-small">{entry.path}</span>
              {#if !entry.available}
                <span class="missing">Folder not found</span>
              {/if}
            </span>
            {#if !entry.available}
              <span class="forget" role="button" tabindex="0" onclick={(e) => forget(entry.id, e)} onkeydown={() => {}}>Remove</span>
            {/if}
          </button>
        {/each}
      </div>
    {/if}
  </div>
</div>

<ContextMenu />
<ConfirmMenu />

<style>
  .wrap {
    height: 100vh;
    display: flex;
    /* A too-tall panel (Features expanded, long candidate list) must stay
       reachable: `align-items: center` alone clips it at BOTH ends with no
       scroll. `safe center` degrades to `flex-start` once it would overflow;
       the plain `center` above it is the fallback for engines without it. */
    align-items: center;
    align-items: safe center;
    justify-content: center;
    overflow-y: auto;
    /* Gentle lined paper: faint rules every 28px on the paper ground. */
    background:
      repeating-linear-gradient(
        to bottom,
        transparent,
        transparent 27px,
        var(--rule-line) 27px,
        var(--rule-line) 28px
      ),
      var(--paper);
  }
  .panel {
    width: 460px;
    max-width: 100%;
    /* Never let the centering flexbox compress the panel — it scrolls instead. */
    flex: none;
    display: flex;
    flex-direction: column;
    gap: 14px;
    padding: 32px;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .wordmark {
    font-family: var(--font-script);
    font-size: 26px;
    line-height: 1;
    color: var(--ink);
    /* Script baseline sits low; nudge up so it aligns with the mark. */
    transform: translateY(-3px);
  }
  h1 {
    margin: 6px 0 0;
    font-family: var(--font-serif);
    font-size: 30px;
    font-weight: 500;
    line-height: 1.2;
    letter-spacing: -0.01em;
  }
  .lede {
    margin: 0;
    font-size: 14px;
    line-height: 1.7;
    color: var(--ink-secondary);
  }
  .choose-row {
    display: flex;
    gap: 10px;
    align-items: center;
  }
  /* Two real options: stacked cards, equal weight, so neither wins by
     emphasis. */
  .choose-row.stacked {
    flex-direction: column;
    align-items: stretch;
  }
  .choice {
    display: flex;
    flex-direction: column;
    gap: 3px;
    width: 100%;
    text-align: left;
    padding: 12px 14px;
    background: var(--surface);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-control);
    box-shadow: var(--shadow-control);
    font: inherit;
    cursor: pointer;
    color: var(--ink);
  }
  .choice:hover {
    border-color: var(--accent);
  }
  .choice:focus-visible {
    border-color: var(--accent);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 12%, transparent);
    outline: none;
  }
  .choice-title {
    font-size: 14px;
    font-weight: 600;
  }
  .choice-note {
    font-size: 12px;
    line-height: 1.5;
    color: var(--ink-tertiary);
  }
  .ws-candidates {
    display: flex;
    flex-direction: column;
    gap: 10px;
    max-height: 260px;
    overflow-y: auto;
  }
  .feature-name.ws-name {
    text-transform: none;
  }
  .ws-group {
    font-weight: 400;
    color: var(--ink-tertiary);
  }
  .confirm {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-card);
    padding: 16px 18px;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .path {
    font-size: 12px;
    color: var(--ink-secondary);
    word-break: break-all;
  }
  label {
    display: flex;
    flex-direction: column;
    gap: 6px;
    font-size: 12px;
    font-weight: 600;
    color: var(--ink-secondary);
  }
  input {
    height: 36px;
    padding: 0 12px;
    border-radius: 8px;
    border: 1px solid var(--border-strong);
    background: var(--surface);
    font-size: 14px;
    color: var(--ink);
    outline: none;
    font-family: inherit;
  }
  input:focus {
    border-color: var(--accent);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 12%, transparent);
  }
  .confirm-actions {
    display: flex;
    gap: 8px;
  }
  .analyzing {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 10px;
    border-radius: 8px;
    background: var(--sunken);
    font-size: 12.5px;
  }
  .mini-spinner {
    width: 12px;
    height: 12px;
    flex: none;
    border: 2px solid color-mix(in srgb, var(--ink-tertiary) 35%, transparent);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: profile-spin 0.7s linear infinite;
  }
  @keyframes profile-spin {
    to {
      transform: rotate(360deg);
    }
  }
  .kind-badge {
    flex: none;
    padding: 2px 7px;
    border-radius: 6px;
    background: var(--ink);
    color: var(--paper);
    font-size: 11px;
    font-weight: 600;
    text-transform: capitalize;
  }
  .analyzing-text {
    color: var(--ink-secondary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1;
  }
  .skip-analysis {
    flex: none;
    border: none;
    background: transparent;
    padding: 0;
    font-size: 12px;
    font-weight: 600;
    color: var(--ink-tertiary);
  }
  .skip-analysis:hover {
    color: var(--ink-secondary);
  }
  .features {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .features-toggle {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    align-self: flex-start;
    border: none;
    background: transparent;
    padding: 0;
    font-size: 12px;
    font-weight: 600;
    color: var(--ink-tertiary);
  }
  .features-toggle:hover {
    color: var(--ink-secondary);
  }
  .chev {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 14px;
    height: 14px;
    transition: transform 0.15s ease;
  }
  .chev.open {
    transform: rotate(90deg);
  }
  .features-body {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 2px 2px 0;
    /* Same treatment as `.ws-candidates`: nine flags overflow the panel. */
    max-height: 260px;
    overflow-y: auto;
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
    /* The checkbox is a flex item too — without this it shrinks to a sliver
       when the label text is long. */
    flex: none;
  }
  .feature-row {
    align-items: flex-start;
  }
  .feature-text {
    display: flex;
    flex-direction: column;
    gap: 2px;
    /* `min-width: auto` (the flex default) refuses to shrink below the
       content width, so a long candidate note pushes the row past the
       panel instead of wrapping. These two make it wrap in place. */
    flex: 1;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .feature-name {
    font-weight: 600;
    text-transform: capitalize;
  }
  .note {
    margin: 0;
    font-size: 12px;
    color: var(--ink-tertiary);
    line-height: 1.5;
  }
  .error {
    font-size: 13px;
    color: var(--danger);
    background: color-mix(in srgb, var(--danger) 7%, transparent);
    border: 1px solid color-mix(in srgb, var(--danger) 25%, transparent);
    border-radius: 10px;
    padding: 10px 14px;
  }
  .recent-label {
    font-size: 11px;
    font-weight: 700;
    color: var(--ink-tertiary);
    letter-spacing: 0.08em;
    text-transform: uppercase;
    margin-top: 10px;
  }
  .recents {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .recent {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 9px 10px;
    border-radius: 9px;
    border: 1px solid var(--border);
    background: var(--surface);
    text-align: left;
    font-size: 13px;
  }
  .recent:hover {
    background: var(--sunken);
  }
  .recent.unavailable {
    opacity: 0.7;
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
  .info {
    display: flex;
    flex-direction: column;
    min-width: 0;
    flex: 1;
  }
  .name {
    font-weight: 600;
  }
  .path-small {
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
  }
</style>

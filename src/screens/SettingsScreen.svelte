<script lang="ts">
  import TeamScreen from "./TeamScreen.svelte";
  import TeamInbox from "../team/TeamInbox.svelte";
  import type { SettingsSection } from "../lib/app.svelte";
  // Settings: what is yours (You), what is this computer's (This machine),
  // Ken's features, the connector for agents, and About. Each repo's own
  // settings open from its row on Team.
  import { onMount, untrack } from "svelte";
  import { app } from "../lib/app.svelte";
  import { memory } from "../lib/memory.svelte";
  import { toWorkspaceAddress, unopenableReason } from "../lib/kenAddress";
  import { theme, type ThemeMode } from "../lib/theme.svelte";
  import {
    api,
    type ClaudeDoctor,
    type EmbeddingState,
    type FeatureInfo,
    type GpuInfo,
    type McpInfo,
    type ModelCategory,
    type ModelStatus,
  } from "../lib/api";
  import { claudeInstallHelp } from "../lib/platform";
  import { embeddingLine, embeddingPct, modelSize, modelsFor, runsOnLine, tierLabel } from "../lib/models";
  import { toast } from "../lib/toast.svelte";
  import { openConfirm } from "../lib/ui/ConfirmMenu.svelte";
  import ModelDownloadDialog from "../files/previews/ModelDownloadDialog.svelte";
  import ProgressBar from "../lib/ProgressBar.svelte";
  import { whatsNew } from "../whats-new/whatsNew.svelte";
  import Copy from "@lucide/svelte/icons/copy";
  import Check from "@lucide/svelte/icons/check";

  let mcp = $state<McpInfo | null>(null);
  let copied = $state<"command" | "instruction" | null>(null);
  let copyTimer: ReturnType<typeof setTimeout> | undefined;
  let models = $state<ModelStatus[]>([]);
  let modelsLoading = $state(true);
  let removing = $state<string | null>(null);
  // Registry-driven feature flags: global defaults plus this repo's overrides.
  let features = $state<FeatureInfo[]>([]);
  let featuresBusy = $state<string | null>(null);
  let doctor = $state<ClaudeDoctor | null>(null);
  let gpu = $state<GpuInfo | null>(null);
  let embedding = $state<EmbeddingState | null>(null);

  const sections: { key: SettingsSection; label: string; sub: string }[] = [
    { key: "general", label: "General", sub: "Appearance, files, updates" },
    { key: "team", label: "Team library", sub: "Folders, rules, weekly check" },
    { key: "sync", label: "Sync", sub: "Git and the team inbox" },
    { key: "ai", label: "AI", sub: "Claude Code and models" },
    { key: "agents", label: "Agents", sub: "The connector for your tools" },
    { key: "rules", label: "Ingest rules", sub: "What Ken may write" },
  ];

  const themeOptions: { value: ThemeMode; title: string }[] = [
    { value: "light", title: "Light" },
    { value: "dark", title: "Dark" },
    { value: "system", title: "System" },
  ];

  const transcriptionModels = $derived(modelsFor(models, "transcription"));
  const embeddingModels = $derived(modelsFor(models, "embedding"));
  // Background reading is this machine's; the rest are Ken's features.
  const backgroundReading = $derived(features.find((f) => f.name === "backgroundExtraction") ?? null);
  const kenFeatures = $derived(features.filter((f) => f.name !== "backgroundExtraction"));

  onMount(() => {
    void api.mcpInfo().then((m) => (mcp = m)).catch(() => (mcp = null));
    void api.claudeDoctor().then((d) => (doctor = d)).catch(() => (doctor = null));
    void api.gpuInfo().then((g) => (gpu = g)).catch(() => (gpu = null));
    void api.embeddingState().then((s) => (embedding = s)).catch(() => (embedding = null));
    void refreshModels();
    void loadFeatures();
    void memory.init();
    let off: (() => void) | undefined;
    void api.onSemanticProgress((s) => (embedding = s)).then((fn) => (off = fn));
    return () => {
      clearTimeout(copyTimer);
      off?.();
    };
  });

  // Settings stays mounted: a change of repo reads that repo's features and
  // its agent command.
  let seenFocus: string | null | undefined;
  $effect(() => {
    const f = app.focused;
    if (seenFocus !== undefined && f !== seenFocus) {
      untrack(() => {
        void loadFeatures();
        void api.mcpInfo().then((m) => (mcp = m)).catch(() => (mcp = null));
      });
    }
    seenFocus = f;
  });

  async function loadFeatures() {
    features = await api.listFeatures(app.project?.id).catch(() => []);
  }

  /** A flag's default for every repo. `semanticIndex` refreshes the app's
   *  copy so the status line stays right when the default drives it. */
  async function setGlobalDefault(flag: FeatureInfo, value: boolean) {
    featuresBusy = flag.name;
    try {
      await api.setGlobalFeature(flag.name, value);
      if (flag.name === "semanticIndex") {
        app.semanticIndex = await api.getSemanticIndex().catch(() => app.semanticIndex);
      }
      await loadFeatures();
    } catch (e) {
      toast.error(`Could not change ${flag.label}`, e);
    } finally {
      featuresBusy = null;
    }
  }

  /** This repo's own value for a per-repo flag. */
  async function setRepoOverride(flag: FeatureInfo, value: boolean) {
    featuresBusy = flag.name;
    try {
      if (flag.name === "semanticIndex") await app.setSemanticIndex(value);
      else await api.setProjectFeature(flag.name, value);
      await loadFeatures();
    } catch (e) {
      toast.error(`Could not change ${flag.label}`, e);
    } finally {
      featuresBusy = null;
    }
  }

  async function refreshModels() {
    modelsLoading = true;
    try {
      models = await api.listModels();
    } catch {
      models = [];
    } finally {
      modelsLoading = false;
    }
  }

  async function removeModel(id: string) {
    removing = id;
    try {
      await api.removeModel(id);
      await refreshModels();
    } catch (e) {
      toast.error("Could not remove the model", e);
    } finally {
      removing = null;
    }
  }

  async function select(category: ModelCategory, id: string) {
    try {
      await api.setModelSelection(category, id);
      await refreshModels();
      if (category === "embedding") embedding = await api.embeddingState().catch(() => embedding);
    } catch (e) {
      toast.error("Could not switch the model", e);
      await refreshModels();
    }
  }

  /** Switching the search-by-meaning model re-reads every file once; say so
   *  before it starts. */
  function pick(e: Event, category: ModelCategory, m: ModelStatus) {
    if (category !== "embedding" || !embeddingModels.some((x) => x.selected)) {
      void select(category, m.id);
      return;
    }
    // Put the radios back as they were until the switch is confirmed.
    const el = e.currentTarget as HTMLInputElement;
    const current = embeddingModels.find((x) => x.selected)?.id;
    for (const input of document.getElementsByName(el.name) as NodeListOf<HTMLInputElement>) {
      input.checked = input.value === current;
    }
    const r = el.getBoundingClientRect();
    openConfirm(r.left, r.bottom + 6, {
      title: `Search by meaning with ${m.name}?`,
      body: "Ken reads every file once more for meaning. Keyword search works meanwhile.",
      confirmLabel: "Switch",
      onConfirm: () => void select(category, m.id),
    });
  }

  async function setUseGpu(on: boolean) {
    try {
      await api.setUseGpu(on);
      gpu = await api.gpuInfo().catch(() => (gpu ? { ...gpu, useGpu: on } : gpu));
    } catch (e) {
      toast.error("Could not change the graphics card setting", e);
    }
  }

  async function copy(text: string, what: "command" | "instruction") {
    try {
      await navigator.clipboard.writeText(text);
      copied = what;
      clearTimeout(copyTimer);
      copyTimer = setTimeout(() => (copied = null), 1600);
    } catch {
      // Clipboard unavailable: leave the button as it is.
    }
  }

  /** A distill candidate's body, cut short; approving opens the whole memory. */
  function bodyPreview(body: string, max = 320): string {
    const trimmed = body.trim();
    return trimmed.length > max ? `${trimmed.slice(0, max)}…` : trimmed;
  }
</script>

<div class="settings">
  <nav class="snav" aria-label="Settings">
    <h1>Settings</h1>
    {#each sections as s (s.key)}
      <button class:on={app.settingsSection === s.key} onclick={() => (app.settingsSection = s.key)}>
        <span class="slabel">{s.label}</span>
        <span class="ssub">{s.sub}</span>
      </button>
    {/each}
  </nav>
  <div class="wrap">
  <div class="inner">
    <h2 class="stitle">{sections.find((s) => s.key === app.settingsSection)?.label}</h2>

    {#if app.settingsSection === "team"}
      <TeamScreen />
    {/if}

    {#if app.settingsSection === "sync"}
      <section class="group">
        <div class="card">
          <div class="card-title">How the team's files sync</div>
          <p class="note">
            Each folder Ken reads syncs through its own Git remote, set in its row under Team library. A file two people
            changed at once shows in the Inbox under Sync &amp; files, with both versions side by side.
          </p>
        </div>
        <TeamInbox />
      </section>
    {/if}

    {#if app.settingsSection === "rules"}
      <section class="group">
        <div class="card">
          <div class="card-title">What Ken writes on its own</div>
          <p class="note">
            When a source is added, Ken writes the note and what follows from it at once, each citing the note: page edits,
            new pages, ideas, escalations and your next steps. Each has an Undo on its card in Ingest.
          </p>
        </div>
        <div class="card">
          <div class="card-title">What waits for you</div>
          <ul class="rules">
            <li>A ruling, in the words of the person who decides it. Only they can accept it.</li>
            <li>A change to Ways-of-Working, Conventions or a rule. It becomes a ticket.</li>
            <li>An edit that rewrites more than a fifth of a page, or lands on a page someone changed while Ken was reading.</li>
            <li>An action. It becomes a ticket once accepted.</li>
          </ul>
          <p class="note">These wait in the Inbox under Ken. Human edits always win.</p>
        </div>
        <div class="card">
          <div class="card-title">The team's own rules</div>
          <p class="note">The rules reviewers check work against are pages in the wiki. Add one under Team library.</p>
          <div class="row"><button class="btn btn-small" onclick={() => (app.settingsSection = "team")}>Open Team library</button></div>
        </div>
      </section>
    {/if}

    <!-- ── General ─────────────────────────────────────────────────── -->
    {#if app.settingsSection === "general"}
    <section class="group">

      <div class="card">
        <div class="card-title">Appearance</div>
        <div class="row">
          <span class="label">Theme</span>
          <div class="seg-group" role="radiogroup" aria-label="Theme">
            {#each themeOptions as opt (opt.value)}
              <button
                class="seg"
                class:on={theme.mode === opt.value}
                role="radio"
                aria-checked={theme.mode === opt.value}
                onclick={() => theme.set(opt.value)}>{opt.title}</button
              >
            {/each}
          </div>
        </div>
      </div>

      <div class="card">
        <div class="card-title">Files list</div>
        <label class="radio">
          <input type="checkbox" checked={app.followOpen} onchange={(e) => app.setFollowOpen(e.currentTarget.checked)} />
          Highlight the open file in the files list
        </label>
        <p class="note">The folder tree follows and opens to whichever file is open.</p>
      </div>

      {#if app.ignored.length > 0}
        <div class="card">
          <div class="card-title">Ignored files</div>
          <p class="note">
            Ken no longer flags these when it cannot read them. For you only, never shared. They stay indexed and
            searchable.
          </p>
          <div class="list">
            {#each app.ignored as path (path)}
              <div class="list-row">
                <span class="mono small">{path}</span>
                <button class="btn btn-small push" onclick={() => void app.unignoreFile(path)}>Un-ignore</button>
              </div>
            {/each}
          </div>
        </div>
      {/if}

    </section>
    {/if}

    <!-- ── AI ─────────────────────────────────────────────────────── -->
    {#if app.settingsSection === "ai"}
    <section class="group">

      <div class="card">
        <div class="card-title">Offline models</div>
        <p class="note">These run on this computer; nothing you say or store leaves it.</p>
        {#if modelsLoading}
          <p class="note">Checking for models…</p>
        {:else}
          {@render modelCategory("Transcription", "transcription", transcriptionModels)}

          {@render modelCategory("Search by meaning", "embedding", embeddingModels)}
          {#if embedding}
            <div class="mstate">
              {#if embedding.rebuilding}
                <ProgressBar pct={embeddingPct(embedding)} label={embeddingLine(embedding)} />
              {:else}
                <p class="note">{embeddingLine(embedding)}</p>
              {/if}
            </div>
          {/if}

          <div class="mcat">
            <div class="mcat-title">Graphics card</div>
            <label class="radio">
              <input type="checkbox" checked={gpu?.useGpu ?? true} disabled={!gpu} onchange={(e) => void setUseGpu(e.currentTarget.checked)} />
              Use the graphics card
            </label>
            {#if gpu}<p class="note">{runsOnLine(gpu)}</p>{/if}
          </div>
        {/if}
      </div>

      {#snippet modelCategory(title: string, cat: ModelCategory, list: ModelStatus[])}
        <div class="mcat">
          <div class="mcat-title">{title}</div>
          {#each list as m (m.id)}
            {@render modelRow(cat, m, true)}
          {:else}
            <p class="note">None on offer.</p>
          {/each}
        </div>
      {/snippet}

      {#snippet modelRow(cat: ModelCategory, m: ModelStatus, selectable: boolean)}
        <div class="mopt" class:selected={selectable && m.selected}>
          {#if selectable}
            <label class="mradio">
              <input
                type="radio"
                name={`model-${cat}`}
                value={m.id}
                checked={m.selected}
                disabled={!m.installed}
                onchange={(e) => pick(e, cat, m)}
              />
              {@render modelText(m)}
            </label>
          {:else}
            <div class="mradio indent">{@render modelText(m)}</div>
          {/if}
          {#if m.installed}
            <div class="mopt-actions">
              <span class="soft"><span class="ok-dot"></span>Installed{#if m.sizeBytes} · {modelSize(m.sizeBytes)}{/if}</span>
              {#if !m.selected || !selectable}
                <button class="btn btn-small push" onclick={() => void removeModel(m.id)} disabled={removing === m.id}>
                  {removing === m.id ? "Removing…" : "Remove"}
                </button>
              {/if}
            </div>
          {:else}
            <div class="mopt-actions">
              <ModelDownloadDialog status={m} compact onInstalled={refreshModels} />
            </div>
          {/if}
        </div>
      {/snippet}

      {#snippet modelText(m: ModelStatus)}
        <span class="mopt-main">
          <span class="mname">{m.name} <span class="mtier">{tierLabel(m.tier)}</span></span>
          <span class="mblurb">{m.blurb}{#if m.languages} · {m.languages}{/if}</span>
        </span>
      {/snippet}

      <div class="card">
        <div class="card-title">Claude Code</div>
        {#if doctor?.found}
          <p class="note">
            <span class="ok-dot"></span>Found{#if doctor.version}, version {doctor.version}{/if}
            {#if doctor.path}<span class="mono small path">{doctor.path}</span>{/if}
          </p>
          <p class="note">Ken writes, reads sources in, judges and answers with it.</p>
        {:else if doctor}
          <p class="note warn">Not found. Ingest, the digest, answers and the map need it.</p>
          <p class="note">{doctor.help || claudeInstallHelp()}</p>
        {:else}
          <p class="note">Checking…</p>
        {/if}
      </div>

      {#if backgroundReading}
        <div class="card">
          <div class="card-title">{backgroundReading.label}</div>
          <label class="radio">
            <input
              type="checkbox"
              checked={backgroundReading.global}
              disabled={featuresBusy === backgroundReading.name}
              onchange={(e) => backgroundReading && void setGlobalDefault(backgroundReading, e.currentTarget.checked)}
            />
            Read new files in the background
          </label>
          <p class="note">{backgroundReading.description}</p>
        </div>
      {/if}
      <div class="card">
        <div class="card-title">Memory: journal distillation</div>
        <p class="note">
          Ken reads the workspace journal and drafts long-term memories from what keeps coming back. Nothing is
          written to <span class="mono small">memory/</span> until you approve a draft below.
        </p>
        {#if memory.phase === "planning" || memory.phase === "distilling"}
          <div class="row">
            <span class="mini-spinner" aria-hidden="true"></span>
            <span class="soft">{memory.phase === "planning" ? "Reading the journal…" : "Drafting…"}</span>
          </div>
        {/if}
        {#if memory.phase === "error" && memory.errorReason}
          <p class="note warn">Distillation failed: {memory.errorReason}</p>
        {/if}
        <div class="row">
          <button
            class="btn btn-small"
            onclick={() => void memory.distill()}
            disabled={memory.phase === "planning" || memory.phase === "distilling"}
          >
            {memory.phase === "planning" || memory.phase === "distilling" ? "Distilling…" : "Distill the journal"}
          </button>
        </div>
      </div>

      {#each memory.candidates as c (c.slug)}
        <div class="card">
          <div class="card-title">{c.slug}</div>
          <div class="row"><span class="chip mono">.ken-workspace/memory/{c.slug}.md</span></div>
          {#if c.description}<p class="note">{c.description}</p>{/if}
          <pre class="memory-body">{bodyPreview(c.body)}</pre>
          {#if c.sources.length > 0}
            <div class="list">
              {#each c.sources as src}
                {@const reason = unopenableReason(toWorkspaceAddress(src))}
                <span class="mono small" class:disabled-link={!!reason} title={reason ?? src}>{src}</span>
              {/each}
            </div>
          {/if}
          <div class="row">
            <button class="btn btn-small" onclick={() => void memory.resolve(c.slug, true)} disabled={memory.resolvingSlug === c.slug}>
              Approve
            </button>
            <button class="btn btn-small" onclick={() => void memory.resolve(c.slug, false)} disabled={memory.resolvingSlug === c.slug}>
              Dismiss
            </button>
          </div>
        </div>
      {/each}
    </section>

    {#if kenFeatures.length > 0}
      <section class="group">
        <div class="group-head">Features</div>
        {#each kenFeatures as flag (flag.name)}
          <div class="card">
            <div class="card-title">{flag.label}</div>
            <p class="note">
              {flag.description}
              <span class="flag-meta">{flag.applies} · <span class="mono">{flag.name}</span></span>
            </p>
            <label class="radio">
              <input
                type="checkbox"
                checked={flag.global}
                disabled={featuresBusy === flag.name}
                onchange={(e) => void setGlobalDefault(flag, e.currentTarget.checked)}
              />
              On by default for every repo
            </label>
            {#if flag.scope === "project"}
              <label class="radio">
                <input
                  type="checkbox"
                  checked={flag.projectOverride ?? flag.global}
                  disabled={featuresBusy === flag.name}
                  onchange={(e) => void setRepoOverride(flag, e.currentTarget.checked)}
                />
                On for {app.project?.name ?? "this repo"}
              </label>
              <p class="note">
                {flag.projectOverride === null ? "This repo follows the default above." : "This repo has its own setting."}
              </p>
            {/if}
            {#if flag.name === "semanticIndex" && flag.effective && app.semanticIndexState}
              {#if app.semanticIndexState.state === "building"}
                <p class="note">Reading for meaning: {app.semanticIndexState.done} of {app.semanticIndexState.total} files.</p>
              {:else if app.semanticIndexState.state === "unavailable"}
                <p class="note">Not available now: {app.semanticIndexState.reason}</p>
              {:else if app.semanticIndexState.state === "warning"}
                <p class="note">{app.semanticIndexState.reason}</p>
              {/if}
            {/if}
          </div>
        {/each}
      </section>
    {/if}
    {/if}

    <!-- ── Agents ──────────────────────────────────────────────────── -->
    {#if app.settingsSection === "agents"}
    <section class="group">
      <div class="card">
        <div class="mcp-head">
          <span class="card-title">Connect an agent</span>
          {#if mcp?.binaryPath}
            <span class="mcp-status"><span class="ok-dot"></span>Ready; an agent starts it when it needs it</span>
          {/if}
        </div>
        <p class="note">
          Ken's connector lets Claude Code, Cursor and other agents search the team's repos and read their files. It
          writes only Your day tasks, memories, the journal and team inbox messages, never your files.
        </p>
        {#if mcp?.binaryPath}
          <div class="mcp-block">
            <div class="mcp-comment"># add Ken to an agent, scoped to this repo</div>
            <div class="mcp-cmd-row">
              <code class="mcp-cmd">{mcp.addCommand}</code>
              <button class="mcp-copy" onclick={() => mcp && copy(mcp.addCommand, "command")}>
                {#if copied === "command"}<Check size={13} strokeWidth={1.75} /> copied{:else}<Copy size={13} strokeWidth={1.75} /> copy{/if}
              </button>
            </div>
          </div>
          <div class="mcp-chips">
            <span class="mcp-chip"><strong>Scope</strong>: this repo</span>
            <button class="mcp-chip mcp-chip-btn" onclick={() => mcp && copy(mcp.llmInstruction, "instruction")}>
              <strong>Instructions for the agent</strong>: paste them in ·
              <span class="mcp-chip-action">
                {#if copied === "instruction"}<Check size={12} strokeWidth={1.75} /> copied{:else}<Copy size={12} strokeWidth={1.75} /> copy{/if}
              </span>
            </button>
          </div>
        {:else if mcp}
          <p class="note">
            The connector (<span class="mono small">ken-mcp</span>) comes with Ken's installer but is not on this
            computer; installing Ken again puts it back. From source: <span class="mono small">cargo build -p ken-mcp</span>.
          </p>
        {/if}
      </div>
    </section>
    {/if}

    <!-- ── About (General) ─────────────────────────────────────────── -->
    {#if app.settingsSection === "general"}
    <section class="group">
      <div class="group-head">About</div>
      <div class="card">
        <div class="row">
          <span class="about-version">Ken v{whatsNew.version}</span>
          <button class="whats-new" onclick={() => whatsNew.show()}>What's new in this version</button>
        </div>
        <p class="note">Each folder's own settings (reindex, watched folders, cloud files, sync) open from its row in Team library.</p>
      </div>
    </section>
    {/if}
  </div>
  </div>
</div>

<style>
  .settings {
    flex: 1;
    min-width: 0;
    display: flex;
    min-height: 0;
  }
  .snav {
    width: 230px;
    flex: none;
    border-right: 1px solid var(--line-soft);
    padding: 24px 12px;
    display: flex;
    flex-direction: column;
    gap: 2px;
    box-sizing: border-box;
    overflow-y: auto;
  }
  .snav h1 {
    margin: 0 0 14px;
    padding: 0 10px;
    font-family: var(--font-serif);
    font-size: 28px;
    font-weight: 500;
  }
  .snav button {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 1px;
    padding: 8px 10px;
    border-radius: 8px;
    border: none;
    background: transparent;
    text-align: left;
    color: var(--ink);
  }
  .snav button:hover {
    background: var(--line-soft);
  }
  .snav button.on {
    background: var(--accent-soft);
    color: var(--accent-ink);
  }
  .slabel {
    font-size: 13.5px;
    font-weight: 600;
  }
  .ssub {
    font-size: 12px;
    color: var(--ink-3);
  }
  .stitle {
    margin: 0;
    font-family: var(--font-serif);
    font-size: 24px;
    font-weight: 500;
  }
  .rules {
    margin: 0;
    padding-left: 18px;
    font-size: 13.5px;
    line-height: 1.6;
    color: var(--ink-2);
  }
  .wrap {
    flex: 1;
    min-width: 0;
    overflow-y: auto;
    padding: 32px 40px;
  }
  .inner {
    max-width: 760px;
    display: flex;
    flex-direction: column;
    gap: 20px;
  }
  .group {
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .group-head {
    font-size: 11px;
    font-weight: 700;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--ink-tertiary);
    margin-bottom: 2px;
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
  .note.warn {
    color: var(--needs-input-text);
  }
  .path {
    display: block;
    overflow-wrap: anywhere;
  }
  /* Where a feature applies, and its code name for anyone editing JSON. */
  .flag-meta {
    display: block;
    font-size: 11.5px;
    opacity: 0.85;
  }
  .list {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .list-row {
    display: flex;
    align-items: center;
    gap: 9px;
    font-size: 13px;
  }
  .push {
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
  .seg-group {
    display: inline-flex;
    border: 1px solid var(--border);
    border-radius: var(--radius-control);
    overflow: hidden;
  }
  .seg-group .seg {
    padding: 5px 14px;
    border: none;
    background: var(--surface);
    color: var(--ink-secondary);
    font-size: 12.5px;
    font-weight: 500;
  }
  .seg-group .seg:hover {
    background: var(--sunken);
  }
  .seg-group .seg.on {
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    color: var(--accent-deep);
    font-weight: 600;
  }
  /* Offline models */
  .mcat {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding-top: 14px;
    border-top: 1px solid var(--sunken);
  }
  .mcat-title {
    font-size: 12px;
    font-weight: 600;
    color: var(--ink-secondary);
  }
  .mopt {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 4px 0;
  }
  .mradio {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    cursor: pointer;
  }
  .mradio.indent {
    padding-left: 24px;
    cursor: default;
  }
  .mradio input {
    accent-color: var(--accent);
    margin-top: 3px;
  }
  .mopt-main {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .mname {
    font-size: 13px;
    font-weight: 500;
  }
  .mtier {
    margin-left: 6px;
    font-size: 11px;
    font-weight: 400;
    color: var(--accent);
  }
  .mblurb {
    font-size: 12px;
    color: var(--ink-tertiary);
  }
  .mopt-actions {
    display: flex;
    align-items: center;
    gap: 10px;
    padding-left: 24px;
  }
  .mopt-actions :global(.model) {
    flex: 1;
  }
  .mstate {
    padding-left: 24px;
    max-width: 420px;
  }
  .ok-dot {
    display: inline-block;
    width: 7px;
    height: 7px;
    border-radius: 4px;
    background: var(--healthy);
    margin-right: 7px;
  }
  .memory-body {
    margin: 0;
    padding: 10px 12px;
    background: var(--sunken);
    border: 1px solid var(--border);
    border-radius: 6px;
    font-family: var(--font-mono);
    font-size: 12px;
    line-height: 1.5;
    white-space: pre-wrap;
    word-break: break-word;
    max-height: 220px;
    overflow-y: auto;
  }
  /* A ken://workspace/... source that can't be opened yet: shown, with why. */
  .disabled-link {
    color: var(--ink-tertiary);
    cursor: not-allowed;
  }
  .mini-spinner {
    width: 12px;
    height: 12px;
    flex: none;
    border: 2px solid color-mix(in srgb, var(--ink-tertiary) 35%, transparent);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.7s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
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
    display: inline-flex;
    align-items: center;
    gap: 4px;
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
    display: inline-flex;
    align-items: center;
    gap: 3px;
    color: var(--accent);
    font-weight: 600;
    vertical-align: middle;
  }
  .about-version {
    font-size: 13px;
    color: var(--ink-secondary);
  }
  .whats-new {
    margin-left: auto;
    border: none;
    background: transparent;
    padding: 0;
    font-family: inherit;
    font-size: 12.5px;
    font-weight: 600;
    color: var(--accent);
    cursor: pointer;
  }
  .whats-new:hover {
    text-decoration: underline;
  }
</style>

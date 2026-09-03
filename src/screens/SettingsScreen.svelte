<script lang="ts">
  import { onMount } from "svelte";
  import { app, forFocused } from "../lib/app.svelte";
  import ProjectGroups from "./ProjectGroups.svelte";
  import { ingests } from "../lib/ingests.svelte";
  import { memory } from "../lib/memory.svelte";
  import { families } from "../lib/families.svelte";
  import { toWorkspaceAddress, unopenableReason } from "../lib/kenAddress";
  import { theme, type ThemeMode } from "../lib/theme.svelte";
  import {
    api,
    type McpInfo,
    type SyncStatus,
    type ModelStatus,
    type FeatureInfo,
    type ProjectProfile,
    type FamilyConnectionState,
  } from "../lib/api";
  import ModelDownloadDialog from "../files/previews/ModelDownloadDialog.svelte";
  import Copy from "@lucide/svelte/icons/copy";
  import Check from "@lucide/svelte/icons/check";
  import ChevronRight from "@lucide/svelte/icons/chevron-right";
  import {
    buildFolderTree,
    folderTriState,
    isExcluded,
    toggleFolder as toggleFolderPaths,
    type FolderNode,
  } from "../lib/folderTree";

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
  // Downloadable transcription models (discovered from the whisper.cpp repo).
  let models = $state<ModelStatus[]>([]);
  let modelsLoading = $state(true);
  let removing = $state<string | null>(null);
  // Registry-driven feature flags: global defaults plus this project's
  // overrides (see openspec/changes/feature-flags/design.md D5).
  let features = $state<FeatureInfo[]>([]);
  let featuresBusy = $state<string | null>(null);

  // project-profiler task 3.3: project profile card. There's no read-only
  // "get current profile" command (only `profile_project`, which re-scans),
  // so this card starts empty and only shows data once the user has
  // triggered Analyze/Re-analyze this session — a follow-up read command
  // would let it show a previously-generated profile on load instead.
  let profileState = $state<"scanning" | "refining" | "ready" | "error" | null>(null);
  let profileResult = $state<ProjectProfile | null>(null);
  let profileError = $state<string | null>(null);

  // ken-families task 4.1: create/join form fields. Both forms stay in
  // this screen's own local state (not the store) since they're
  // transient input, discarded on a successful submit or on navigating
  // away — `families.svelte.ts` only holds durable connection state.
  let familyCreateName = $state("");
  let familyCreateMemberName = $state("");
  let familyCreateRemote = $state("");
  let familyJoinRemote = $state("");
  let familyJoinMode = $state<"existing" | "new">("existing");
  let familyJoinMemberId = $state("");
  let familyJoinNewName = $state("");
  const profilerFlag = $derived(features.find((f) => f.name === "profiler"));
  const profilerEnabled = $derived(profilerFlag?.effective ?? false);

  /** Kick off `profile_project` for the active project. The command itself
   *  only spawns the background scan/refine/save pass and can reject
   *  synchronously (flag off, already profiling, bad id) — `profile-state`
   *  events (filtered to this project below) carry the actual progress. */
  async function analyzeProject() {
    profileError = null;
    try {
      await api.profileProject(app.project?.id);
    } catch (e) {
      profileState = "error";
      profileError = String(e);
    }
  }

  const themeOptions: { value: ThemeMode; title: string }[] = [
    { value: "light", title: "Light" },
    { value: "dark", title: "Dark" },
    { value: "system", title: "System" },
  ];

  const excludedSet = $derived(new Set(app.project?.excluded ?? []));
  const folderTree = $derived(buildFolderTree(app.folders));
  let expanded = $state<Set<string>>(new Set());

  function toggleExpand(relPath: string) {
    const next = new Set(expanded);
    next.has(relPath) ? next.delete(relPath) : next.add(relPath);
    expanded = next;
  }

  const transcriptionModels = $derived(models.filter((m) => m.category === "transcription"));
  // Language models arrive when the other plan appends them; this card renders
  // whatever categories the catalog returns.
  const languageModels = $derived(models.filter((m) => m.category === "language"));

  async function selectModel(category: "transcription" | "language", id: string) {
    await api.setModelSelection(category, id);
    await refreshModels();
  }

  onMount(() => {
    void api.syncStatus().then((s) => (sync = s)).catch(() => (sync = null));
    void api.mcpInfo().then((m) => (mcp = m)).catch(() => (mcp = null));
    void refreshModels();
    void loadFeatures();
    void memory.init();
    void families.init();
    let unlistenProfile: (() => void) | undefined;
    void api.onProfileState((ev) => {
      // `profile_project` events are project_id-keyed (`emit_member`); no
      // `path` ever accompanies them (that's the `profile_candidates`
      // shape) — reuse the same focused-member filter the rest of the app
      // store uses for member-scoped events.
      if (!forFocused(ev.project_id)) return;
      profileState = ev.state;
      if (ev.state === "ready") profileResult = ev.profile;
      if (ev.state === "error") profileError = ev.reason;
    }).then((fn) => (unlistenProfile = fn));
    return () => {
      clearTimeout(copyTimer);
      unlistenProfile?.();
    };
  });

  async function loadFeatures() {
    features = await api.listFeatures(app.project?.id).catch(() => []);
  }

  /** Change a flag's global default (`settings.json`). Project-scoped flags
   *  fall back to this when the active project has no override — refresh
   *  `app.semanticIndex` too so the status display above stays coherent when
   *  it's the default (rather than an override) that's driving it. */
  async function setGlobalDefault(flag: FeatureInfo, value: boolean) {
    featuresBusy = flag.name;
    try {
      await api.setGlobalFeature(flag.name, value);
      if (flag.name === "semanticIndex") {
        app.semanticIndex = await api.getSemanticIndex().catch(() => app.semanticIndex);
      }
      await loadFeatures();
    } finally {
      featuresBusy = null;
    }
  }

  /** Set this project's override for a project-scoped flag. `semanticIndex`
   *  routes through `app.setSemanticIndex` so `app.semanticIndexState`
   *  (build/availability status, driven by its own event subscription)
   *  keeps tracking the toggle instead of drifting from a second writer. */
  async function setProjectOverride(flag: FeatureInfo, value: boolean) {
    featuresBusy = flag.name;
    try {
      if (flag.name === "semanticIndex") {
        await app.setSemanticIndex(value);
      } else {
        await api.setProjectFeature(flag.name, value);
      }
      await loadFeatures();
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

  function fmtModelSize(n: number): string {
    if (n <= 0) return "";
    const mb = n / (1024 * 1024);
    if (mb >= 1024) return `${(mb / 1024).toFixed(1)} GB`;
    return `${Math.round(mb)} MB`;
  }

  async function removeModel(id: string) {
    removing = id;
    try {
      await api.removeModel(id);
      await refreshModels();
    } finally {
      removing = null;
    }
  }

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

  /** Distill-candidate card body preview — full text is available by
   *  approving (it opens like any memory file afterward); the card itself
   *  only needs enough to judge the proposal. */
  function bodyPreview(body: string, max = 320): string {
    const trimmed = body.trim();
    return trimmed.length > max ? `${trimmed.slice(0, max)}…` : trimmed;
  }

  // ── ken-families task 4.1: connection list actions ─────────────────────

  function familyStateLabel(state: FamilyConnectionState): string {
    switch (state.state) {
      case "idle":
        return "Synced";
      case "syncing":
        return "Syncing…";
      case "conflict":
        return "Conflict";
      case "error":
        return "Error";
      case "unavailable":
        return "Unavailable";
    }
  }

  async function toggleFamilyLiveSync(familyId: string, on: boolean) {
    await families.setLiveSync(familyId, on);
  }

  async function setFamilyPollInterval(familyId: string, secs: number) {
    if (!Number.isFinite(secs) || secs <= 0) return;
    await families.setPollInterval(familyId, Math.round(secs));
  }

  async function familySyncNowClick(familyId: string) {
    await families.syncNow(familyId).catch(() => {
      // The connection's own card shows the resulting error/conflict
      // state (from the `family-sync` event or the re-fetched DTO) —
      // no separate toast needed here.
    });
  }

  async function resolveFamilyConflict(familyId: string) {
    await families.resolveConflict(familyId);
  }

  async function attachFamilyWorkspace(familyId: string) {
    if (!app.workspace) return;
    await families.attachWorkspace(familyId, app.workspace.id);
  }

  async function detachFamilyWorkspace(familyId: string) {
    await families.detachWorkspace(familyId);
  }

  async function removeFamilyConnection(familyId: string) {
    await families.remove(familyId);
  }

  async function submitFamilyCreate() {
    const name = familyCreateName.trim();
    const memberName = familyCreateMemberName.trim();
    const remoteUrl = familyCreateRemote.trim();
    if (!name || !memberName || !remoteUrl) return;
    await families.create(name, memberName, remoteUrl);
    familyCreateName = "";
    familyCreateMemberName = "";
    familyCreateRemote = "";
  }

  async function submitFamilyJoin() {
    const remoteUrl = familyJoinRemote.trim();
    if (!remoteUrl) return;
    if (familyJoinMode === "existing") {
      const id = familyJoinMemberId.trim();
      if (!id) return;
      await families.join(remoteUrl, id, undefined);
      familyJoinMemberId = "";
    } else {
      const name = familyJoinNewName.trim();
      if (!name) return;
      await families.join(remoteUrl, undefined, name);
      familyJoinNewName = "";
    }
    familyJoinRemote = "";
  }

  async function toggleFolder(relPath: string) {
    if (!app.project || toggling) return;
    toggling = true;
    try {
      const currentlyExcluded = isExcluded(relPath, excludedSet);
      const next = toggleFolderPaths(relPath, currentlyExcluded, excludedSet);
      await app.setExcluded(next);
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

    <section class="group">
      <div class="group-head">This project</div>

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

    {#if profilerEnabled}
      <div class="card">
        <div class="card-title">Project profile</div>
        <p class="note">
          Ken scans this project's shape — code vs. docs, languages, build
          output to exclude — to tune indexing and knowledge extraction.
        </p>

        {#if profileState === "scanning" || profileState === "refining"}
          <div class="row">
            <span class="mini-spinner" aria-hidden="true"></span>
            <span class="soft">
              {profileState === "refining" ? "Refining analysis…" : "Scanning…"}
            </span>
          </div>
        {/if}

        {#if profileResult}
          <div class="row">
            <span class="chip mono">{profileResult.kind}</span>
            {#if profileResult.languages.length > 0}
              <span class="soft">{profileResult.languages.join(", ")}</span>
            {/if}
          </div>
          {#if profileResult.summary}
            <p class="note">{profileResult.summary}</p>
          {/if}
          {#if profileResult.excludes.length > 0}
            <div class="row"><span class="label">Excludes</span>
              <span class="soft">added by the profiler, additive to yours above</span>
            </div>
            <div class="folders">
              {#each profileResult.excludes as path (path)}
                <div class="folder ignored">
                  <span class="mono">{path}</span>
                  <button
                    class="btn btn-small"
                    disabled
                    title="Removing a single profiler-added exclude isn't wired up yet — it needs a backend command to persist the override (project-profiler tasks.md 3.3 follow-up). Delete .ken/index-profile.json and re-analyze to reset all of them."
                  >
                    Remove
                  </button>
                </div>
              {/each}
            </div>
          {/if}
        {/if}

        {#if profileState === "error" && profileError}
          <p class="note warn">
            {#if profileError.includes("hand-edited")}
              This project's <span class="mono small">.ken/index-profile.json</span>
              has been hand-edited, so Ken won't overwrite it automatically.
              Delete that file to let Ken generate a fresh one, or keep your
              edits as they are. (A proper "re-analyze and overwrite, with
              confirmation" flow needs a backend force parameter that doesn't
              exist yet.)
            {:else}
              Couldn't analyze this project: {profileError}
            {/if}
          </p>
        {/if}

        <div class="row">
          <button
            class="btn btn-small"
            onclick={analyzeProject}
            disabled={profileState === "scanning" || profileState === "refining"}
          >
            {profileResult ? "Re-analyze" : "Analyze project"}
          </button>
        </div>
      </div>
    {/if}

    <div class="card">
      <div class="card-title">Watched folders</div>
      <p class="note">
        Ken watches every folder by default. Uncheck one to leave it and
        everything inside it out of search and AI features.
      </p>
      {#if app.folders.length === 0}
        <p class="note">No subfolders — everything at the top level is watched.</p>
      {:else}
        <div class="folder-tree">
          {#each folderTree as node (node.relPath)}
            {@render folderRow(node, 0)}
          {/each}
        </div>
      {/if}
    </div>

    {#snippet folderRow(node: FolderNode, depth: number)}
      {@const tri = folderTriState(node.relPath, excludedSet)}
      <div class="frow" style:padding-left={`${depth * 20}px`}>
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
        <div class="subtree">
          {#each node.children as child (child.relPath)}
            {@render folderRow(child, depth + 1)}
          {/each}
        </div>
      {/if}
    {/snippet}

    {#if app.ignored.length > 0}
      <div class="card">
        <div class="card-title">Ignored files</div>
        <p class="note">
          Issues for these files are hidden from Review and Home — for you only,
          never shared with your team. They stay indexed and searchable.
        </p>
        <div class="folders">
          {#each app.ignored as path (path)}
            <div class="folder ignored">
              <span class="mono">{path}</span>
              <button
                class="btn btn-small"
                onclick={() => void app.unignoreFile(path)}
              >
                Un-ignore
              </button>
            </div>
          {/each}
        </div>
      </div>
    {/if}

    <div class="card">
      <div class="card-title">Cloud files</div>
      <div class="row">
        <label class="radio">
          <input
            type="checkbox"
            checked={app.backgroundIndex}
            onchange={(e) =>
              void app.setBackgroundIndex(e.currentTarget.checked)}
          />
          Index cloud files in the background
        </label>
      </div>
      <p class="note">
        Downloads cloud-offline documents so they're searchable without opening
        them. Large media still download on open.
      </p>
    </div>

    <div class="card">
      <div class="card-title">Video transcription</div>
      <div class="row">
        <label class="radio">
          <input
            type="checkbox"
            checked={app.transcribeVideosOnIndex}
            onchange={(e) =>
              void app.setTranscribeVideosOnIndex(e.currentTarget.checked)}
          />
          Transcribe videos during indexing
        </label>
      </div>
      <p class="note">
        Runs on-device speech-to-text (Whisper) to make video audio searchable
        as files are indexed. Off by default because it's slow and CPU-heavy;
        you can always transcribe a single video on demand from its player.
      </p>
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
    </section>

    {#if features.length > 0}
      <section class="group">
        <div class="group-head">Features</div>
        {#each features as flag (flag.name)}
          <div class="card">
            <div class="card-title">{flag.name}</div>
            <div class="row">
              <label class="radio">
                <input
                  type="checkbox"
                  checked={flag.global}
                  disabled={featuresBusy === flag.name}
                  onchange={(e) =>
                    void setGlobalDefault(flag, e.currentTarget.checked)}
                />
                On by default for every project
              </label>
            </div>
            {#if flag.scope === "project"}
              <div class="row">
                <label class="radio">
                  <input
                    type="checkbox"
                    checked={flag.projectOverride ?? flag.global}
                    disabled={featuresBusy === flag.name}
                    onchange={(e) =>
                      void setProjectOverride(flag, e.currentTarget.checked)}
                  />
                  On for this project
                </label>
              </div>
              <p class="note">
                {#if flag.projectOverride === null}
                  This project follows the global default above.
                {:else}
                  This project overrides the global default.
                {/if}
              </p>
            {/if}
            {#if flag.name === "semanticIndex" && flag.effective && app.semanticIndexState}
              {#if app.semanticIndexState.state === "building"}
                <p class="note">
                  Building the semantic index — {app.semanticIndexState.done} of
                  {app.semanticIndexState.total} files.
                </p>
              {:else if app.semanticIndexState.state === "unavailable"}
                <p class="note">
                  Not available right now: {app.semanticIndexState.reason}
                </p>
              {:else if app.semanticIndexState.state === "warning"}
                <p class="note">{app.semanticIndexState.reason}</p>
              {/if}
            {/if}
            <p class="note">{flag.description}</p>
          </div>
        {/each}
      </section>
    {/if}

    {#if memory.enabled}
      <section class="group">
        <div class="group-head">Memory</div>
        <div class="card">
          <div class="card-title">Journal distillation</div>
          <p class="note">
            Ken can review the workspace journal and draft candidate
            long-term memories from anything that keeps recurring. Nothing
            is written to <span class="mono small">memory/</span> until you
            approve a candidate below.
          </p>

          {#if memory.phase === "planning" || memory.phase === "distilling"}
            <div class="row">
              <span class="mini-spinner" aria-hidden="true"></span>
              <span class="soft">
                {memory.phase === "planning" ? "Reading the journal…" : "Drafting candidates…"}
              </span>
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
              {memory.phase === "planning" || memory.phase === "distilling"
                ? "Distilling…"
                : "Distill journal"}
            </button>
          </div>
        </div>

        {#each memory.candidates as c (c.slug)}
          <div class="card">
            <div class="card-title">{c.slug}</div>
            <div class="row">
              <span class="chip mono">.ken-workspace/memory/{c.slug}.md</span>
            </div>
            {#if c.description}
              <p class="note">{c.description}</p>
            {/if}
            <pre class="memory-body">{bodyPreview(c.body)}</pre>
            {#if c.sources.length > 0}
              <div class="row"><span class="label">Sources</span></div>
              <div class="folders">
                {#each c.sources as src (src)}
                  {@const reason = unopenableReason(toWorkspaceAddress(src))}
                  <div class="folder ignored">
                    <span class="mono small" class:disabled-link={!!reason} title={reason ?? src}>
                      {src}
                    </span>
                  </div>
                {/each}
              </div>
            {/if}
            <div class="row">
              <button
                class="btn btn-small"
                onclick={() => void memory.resolve(c.slug, true)}
                disabled={memory.resolvingSlug === c.slug}
              >
                Approve
              </button>
              <button
                class="btn btn-small"
                onclick={() => void memory.resolve(c.slug, false)}
                disabled={memory.resolvingSlug === c.slug}
              >
                Dismiss
              </button>
            </div>
          </div>
        {/each}
      </section>
    {/if}

    {#if app.workspace}
      <section class="group">
        <div class="group-head">Projects</div>
        <div class="card">
          <ProjectGroups />
        </div>
      </section>
    {/if}

    {#if families.enabled}
      <section class="group">
        <div class="group-head">Families</div>
        <div class="card">
          <div class="card-title">What a family is</div>
          <p class="note">
            A family is a shared git repo your team's Ken instances sync
            through — a task or message you send lands in a teammate's
            inbox, never directly on their board. They decide whether to
            accept it. <span class="mono small">shared/</span> is team
            knowledge and joins search once a connection is attached to a
            workspace.
          </p>
          {#if families.loading && families.connections.length === 0}
            <p class="note">Loading connections…</p>
          {:else if families.loadError}
            <p class="note warn">Couldn't load connections: {families.loadError}</p>
          {:else if families.connections.length === 0}
            <p class="note">No families yet — create one or join your team's below.</p>
          {/if}
        </div>

        {#each families.connections as dto (dto.connection.familyId)}
          <div class="card">
            <div class="card-title">{dto.connection.name}</div>

            <div class="row">
              <span class="label">State</span>
              <span class="state-badge state-{dto.state.state}">
                <span class="state-dot"></span>
                {familyStateLabel(dto.state)}
              </span>
            </div>

            {#if dto.state.state === "conflict"}
              <p class="note warn">
                Sync stopped — a rebase conflict needs a human. Resolve it
                by hand in the clone, then confirm below.
              </p>
              <p class="note mono small">{dto.state.detail}</p>
              <div class="row">
                <button
                  class="btn btn-small"
                  onclick={() => void resolveFamilyConflict(dto.connection.familyId)}
                >
                  I resolved it by hand
                </button>
              </div>
            {:else if dto.state.state === "unavailable"}
              <!-- Muted, not "warn" amber: unlike Conflict, there is
                   nothing to click here — the state badge above already
                   carries the visual distinction (gray dot vs. red). -->
              <p class="note">
                {dto.state.reason.toLowerCase().includes("git")
                  ? `Git isn't on PATH: ${dto.state.reason}. Install git and restart Ken to reconnect this family.`
                  : dto.state.reason}
              </p>
            {:else if dto.state.state === "error"}
              <p class="note warn">Last sync failed: {dto.state.detail}</p>
            {/if}

            <div class="row">
              <span class="label">Remote</span>
              <span class="chip mono small">{dto.connection.remoteUrl}</span>
            </div>
            <div class="row">
              <span class="label">You are</span>
              <span class="soft">{dto.connection.memberId}</span>
            </div>

            <div class="row">
              <label class="radio">
                <input
                  type="checkbox"
                  checked={dto.connection.liveSync}
                  disabled={dto.state.state === "unavailable"}
                  onchange={(e) =>
                    void toggleFamilyLiveSync(dto.connection.familyId, e.currentTarget.checked)}
                />
                Sync automatically
              </label>
              <button
                class="btn btn-small sync-now"
                disabled={families.syncingIds.has(dto.connection.familyId) ||
                  dto.state.state === "unavailable" ||
                  dto.state.state === "conflict"}
                onclick={() => void familySyncNowClick(dto.connection.familyId)}
              >
                {families.syncingIds.has(dto.connection.familyId) ? "Syncing…" : "Sync now"}
              </button>
            </div>

            <div class="row">
              <span class="label">Poll every</span>
              <input
                type="number"
                class="poll-input"
                min="30"
                max="1800"
                step="10"
                value={dto.connection.pollIntervalSecs}
                disabled={dto.state.state === "unavailable"}
                onchange={(e) => void setFamilyPollInterval(dto.connection.familyId, e.currentTarget.valueAsNumber)}
              />
              <span class="soft">seconds (30s–30min)</span>
            </div>

            <div class="row">
              <span class="label">Workspace</span>
              {#if dto.connection.attachedWorkspaceId && app.workspace?.id === dto.connection.attachedWorkspaceId}
                <span class="chip">{app.workspace.name}</span>
                <button
                  class="btn btn-small"
                  onclick={() => void detachFamilyWorkspace(dto.connection.familyId)}
                >
                  Detach
                </button>
              {:else if dto.connection.attachedWorkspaceId}
                <span
                  class="soft disabled-link"
                  title="Attached to a different workspace than the one open now (id {dto.connection.attachedWorkspaceId}). Open that workspace to see it in search, or detach here."
                >
                  Attached to another workspace
                </span>
                <button
                  class="btn btn-small"
                  onclick={() => void detachFamilyWorkspace(dto.connection.familyId)}
                >
                  Detach
                </button>
              {:else if app.workspace}
                <button
                  class="btn btn-small"
                  onclick={() => void attachFamilyWorkspace(dto.connection.familyId)}
                >
                  Attach to "{app.workspace.name}"
                </button>
              {:else}
                <span
                  class="soft disabled-link"
                  title="Open a workspace first — an attached family's shared/ folder joins that workspace's search and knowledge graph."
                >
                  Open a workspace to attach
                </span>
              {/if}
            </div>

            <div class="row">
              <button
                class="btn btn-small remove"
                onclick={() => void removeFamilyConnection(dto.connection.familyId)}
                title="Forgets this connection's settings — the on-disk clone is left in place."
              >
                Forget
              </button>
            </div>
          </div>
        {/each}

        <div class="card">
          <div class="card-title">Create a family</div>
          <p class="note">
            Create an empty repo on your git host first (GitHub, Gitea, a
            bare repo on a NAS — anything you already push to), then paste
            its URL here. Ken scaffolds the family template into it and
            makes you the first (owner) member.
          </p>
          <div class="row">
            <span class="label">Name</span>
            <input class="text-input" bind:value={familyCreateName} placeholder="e.g. Acme Team" />
          </div>
          <div class="row">
            <span class="label">You</span>
            <input class="text-input" bind:value={familyCreateMemberName} placeholder="Your display name" />
          </div>
          <div class="row">
            <span class="label">Remote</span>
            <input class="text-input" bind:value={familyCreateRemote} placeholder="git@host:team/family.git" />
          </div>
          {#if families.createError}
            <p class="note warn">{families.createError}</p>
          {/if}
          <div class="row">
            <button class="btn btn-small" onclick={() => void submitFamilyCreate()} disabled={families.createBusy}>
              {families.createBusy ? "Creating…" : "Create family"}
            </button>
          </div>
        </div>

        <div class="card">
          <div class="card-title">Join a family</div>
          <div class="row">
            <span class="label">Remote</span>
            <input class="text-input" bind:value={familyJoinRemote} placeholder="git@host:team/family.git" />
          </div>
          <div class="row">
            <label class="radio">
              <input
                type="radio"
                name="family-join-mode"
                checked={familyJoinMode === "existing"}
                onchange={() => (familyJoinMode = "existing")}
              />
              I'm already a member
            </label>
            <label class="radio">
              <input
                type="radio"
                name="family-join-mode"
                checked={familyJoinMode === "new"}
                onchange={() => (familyJoinMode = "new")}
              />
              I'm new to this family
            </label>
          </div>
          {#if familyJoinMode === "existing"}
            <div class="row">
              <span class="label">Member id</span>
              <input class="text-input" bind:value={familyJoinMemberId} placeholder="e.g. sarah" />
            </div>
            <p class="note">Ask a teammate for your member id — it's the short slug the manifest already knows you by.</p>
          {:else}
            <div class="row">
              <span class="label">Your name</span>
              <input class="text-input" bind:value={familyJoinNewName} placeholder="Your display name" />
            </div>
            <p class="note">You'll be appended to the family's member list.</p>
          {/if}
          {#if families.joinError}
            <p class="note warn">{families.joinError}</p>
          {/if}
          <div class="row">
            <button class="btn btn-small" onclick={() => void submitFamilyJoin()} disabled={families.joinBusy}>
              {families.joinBusy ? "Joining…" : "Join family"}
            </button>
          </div>
        </div>
      </section>
    {/if}

    <section class="group">
      <div class="group-head">On this Mac</div>

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
              onclick={() => theme.set(opt.value)}
            >{opt.title}</button>
          {/each}
        </div>
      </div>
    </div>

    <div class="card">
      <div class="card-title">Offline models</div>
      <p class="note">These run on your Mac — nothing you say or store leaves it.</p>
      {#if modelsLoading}
        <p class="note">Checking for models…</p>
      {:else}
        {@render modelCategory("Transcription", "transcription", transcriptionModels)}
        {#if languageModels.length > 0}
          {@render modelCategory("Answers & Map", "language", languageModels)}
        {/if}
      {/if}
    </div>

    {#snippet modelCategory(title: string, cat: "transcription" | "language", list: ModelStatus[])}
      <div class="mcat">
        <div class="mcat-title">{title}</div>
        {#each list as m (m.id)}
          <div class="mopt" class:selected={m.selected}>
            <label class="mradio">
              <input
                type="radio"
                name={`model-${cat}`}
                checked={m.selected}
                disabled={!m.installed}
                onchange={() => void selectModel(cat, m.id)}
              />
              <span class="mopt-main">
                <span class="mname">{m.name}</span>
                <span class="mtier">{m.tier === "recommended" ? "Recommended" : "Advanced"}</span>
                <span class="mblurb">{m.blurb}</span>
              </span>
            </label>
            {#if m.installed}
              <div class="mopt-actions">
                <span class="soft"><span class="ok-dot"></span>Installed{#if m.sizeBytes}· {fmtModelSize(m.sizeBytes)}{/if}</span>
                {#if !m.selected}
                  <button class="btn btn-small remove" onclick={() => void removeModel(m.id)} disabled={removing === m.id}>
                    {removing === m.id ? "Removing…" : "Remove"}
                  </button>
                {/if}
              </div>
            {:else}
              <ModelDownloadDialog status={m} compact onInstalled={refreshModels} />
            {/if}
          </div>
        {/each}
      </div>
    {/snippet}

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
    </section>

    <section class="group">
      <div class="group-head">Working with agents</div>

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
              {#if copied === "command"}
                <Check size={13} strokeWidth={1.75} /> copied
              {:else}
                <Copy size={13} strokeWidth={1.75} /> copy
              {/if}
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
              {#if copied === "instruction"}
                <Check size={12} strokeWidth={1.75} /> copied
              {:else}
                <Copy size={12} strokeWidth={1.75} /> copy
              {/if}
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
    </section>

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
    gap: 40px; /* between groups (overrides the old uniform 18px) */
  }
  /* Groups: generous separation between, tighter within — restores hierarchy
     without new chrome. */
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
  /* The ignored-files rows are read-only listings, not toggles. */
  .folder.ignored {
    cursor: default;
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
  /* Appearance / any segmented control, matching the Files All/Unread filter. */
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
  .seg-group .seg:hover { background: var(--sunken); }
  .seg-group .seg.on {
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    color: var(--accent-deep);
    font-weight: 600;
  }
  /* Watched-folders tree */
  .folder-tree { display: flex; flex-direction: column; gap: 2px; }
  .frow { display: flex; align-items: center; gap: 4px; }
  .chev {
    display: inline-flex; align-items: center; justify-content: center;
    width: 18px; height: 18px; border: none; background: transparent;
    color: var(--ink-tertiary); border-radius: 4px;
    transition: transform 0.15s ease;
  }
  .chev:hover { background: var(--sunken); color: var(--ink); }
  .chev.open { transform: rotate(90deg); }
  .chev-spacer { width: 18px; flex: none; }
  .fcheck { display: inline-flex; align-items: center; gap: 8px; font-size: 13px; cursor: pointer; }
  .fcheck input { accent-color: var(--accent); }
  .subtree { display: flex; flex-direction: column; gap: 2px; }
  /* Offline models */
  .mcat { display: flex; flex-direction: column; gap: 10px; }
  .mcat + .mcat { margin-top: 16px; }
  .mcat-title { font-size: 12px; font-weight: 600; color: var(--ink-secondary); }
  .mopt { display: flex; flex-direction: column; gap: 6px; padding: 8px 0; }
  .mradio { display: flex; align-items: flex-start; gap: 10px; cursor: pointer; }
  .mradio input { accent-color: var(--accent); margin-top: 3px; }
  .mopt-main { display: flex; flex-direction: column; gap: 2px; }
  .mname { font-size: 13px; font-weight: 500; }
  .mtier { font-size: 11px; color: var(--accent); }
  .mblurb { font-size: 12px; color: var(--ink-tertiary); }
  .mopt-actions { display: flex; align-items: center; gap: 10px; padding-left: 28px; }
  .remove {
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
  /* A `ken://workspace/...` source link that can't be opened yet (task 4.3 —
     see src/lib/kenAddress.ts) — shown, not hidden, with a tooltip saying why. */
  .disabled-link {
    color: var(--ink-tertiary);
    cursor: not-allowed;
  }
  /* ken-families task 4.1: connection state badge. Idle/syncing are
     informational; conflict and unavailable are deliberately distinct
     colors (danger vs. muted) since one needs a click and the other
     needs nothing at all — see the "I resolved it by hand" button vs.
     the plain reason text below each. */
  .state-badge {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 12px;
    font-weight: 600;
  }
  .state-dot {
    width: 7px;
    height: 7px;
    border-radius: 4px;
    flex: none;
    background: var(--ink-tertiary);
  }
  .state-idle .state-dot {
    background: var(--healthy);
  }
  .state-syncing .state-dot {
    background: var(--accent);
  }
  .state-conflict {
    color: var(--danger);
  }
  .state-conflict .state-dot {
    background: var(--danger);
  }
  .state-error {
    color: var(--needs-input-text);
  }
  .state-error .state-dot {
    background: var(--needs-input);
  }
  .state-unavailable {
    color: var(--ink-tertiary);
  }
  .text-input {
    flex: 1;
    min-width: 0;
    font-family: inherit;
    font-size: 12.5px;
    padding: 6px 9px;
    border-radius: 7px;
    border: 1px solid var(--border-strong);
    background: var(--surface);
    color: var(--ink);
  }
  .poll-input {
    width: 72px;
    font-family: inherit;
    font-size: 12.5px;
    padding: 6px 9px;
    border-radius: 7px;
    border: 1px solid var(--border-strong);
    background: var(--surface);
    color: var(--ink);
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
</style>

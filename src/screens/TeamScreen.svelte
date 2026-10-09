<script lang="ts">
  // Team (the Wright white box, frame 9): the chosen team's repos as set-up
  // wrote them, one row each (kind, how deep Ken reads it, branch, commit,
  // how far behind its upstream), the workspace's ignore lines, what a scan
  // found and what a repo lacks for its kind, the wiki's weekly sweep and what
  // its checks found, and the team's rules and templates. It is Settings ›
  // Team library in the Briefing design.
  // Each row opens that repo's own settings in a drawer. Add a repo (9b) and
  // Scan again sit at the top.
  import { open as openDialog } from "@tauri-apps/plugin-dialog";
  import {
    api,
    memberLeaf,
    type IndexState,
    type RepoKind,
    type SetupMoved,
    type TeamOverview,
    type TeamRepo,
  } from "../lib/api";
  import { app } from "../lib/app.svelte";
  import { scope } from "../lib/scope.svelte";
  import { timeAgo } from "../lib/format";
  import { openConfirm } from "../lib/ui/ConfirmMenu.svelte";
  import { toast } from "../lib/toast.svelte";
  import { rail } from "../lib/rail.svelte";
  import { findingLabel, kindlessRepos, orderFindings, proposedKinds } from "../lib/team";
  import type { TeamFinding } from "../lib/api";
  import RepoDrawer from "../team/RepoDrawer.svelte";
  import Loading from "../lib/ui/Loading.svelte";
  import X from "@lucide/svelte/icons/x";
  import ChevronDown from "@lucide/svelte/icons/chevron-down";
  import SlidersHorizontal from "@lucide/svelte/icons/sliders-horizontal";

  let overview = $state<TeamOverview | null>(null);
  let moved = $state<SetupMoved[]>([]);
  let error = $state<string | null>(null);
  let busy = $state<string | null>(null);
  let editingIgnores = $state(false);
  let ignoresDraft = $state("");
  let newRule = $state("");
  let scanned = $state<number | null>(null);

  const team = $derived(scope.team);

  async function refresh() {
    try {
      overview = await api.teamOverview(team);
      rail.setFindings(overview.findings?.length ?? 0);
      error = null;
      if (overview.wiki) drafting = await api.wikiDrafting(overview.wiki.name).catch(() => false);
    } catch (e) {
      error = String(e);
    }
  }
  $effect(() => {
    void team;
    void refresh();
  });

  /** Claude is writing the wiki's pages now. */
  let drafting = $state(false);
  $effect(() => {
    let off: (() => void) | undefined;
    void api
      .onWikiDrafted((wiki) => {
        if (wiki !== overview?.wiki?.name) return;
        drafting = false;
        toast.show("The wiki's pages are written. They are marked draft in Files, with their sources.");
        void refresh();
      })
      .then((fn) => (off = fn));
    return () => off?.();
  });

  /** An action on Team: `what` names it in the notice when it fails. */
  async function run(label: string, what: string, f: () => Promise<unknown>) {
    busy = label;
    try {
      await f();
    } catch (e) {
      toast.error(what, e);
    } finally {
      busy = null;
      await refresh();
    }
  }

  async function scanAgain() {
    if (!overview) return;
    const root = overview.root;
    await run("scan", "Could not scan the folders", async () => {
      moved = await api.setupRescan(root);
      scanned = Math.floor(Date.now() / 1000);
    });
  }

  // One kind per repo here; a repo set up as two (a team repo that is also
  // its wiki) keeps both unless a person picks one.
  const KINDS: { v: RepoKind | ""; label: string }[] = [
    { v: "code", label: "code" },
    { v: "wiki", label: "wiki" },
    { v: "team", label: "team" },
    { v: "reference", label: "reference" },
    { v: "", label: "not said" },
  ];
  const kindValue = (r: TeamRepo) => (r.kind.length > 1 ? r.kind.join("+") : (r.kind[0] ?? ""));

  function setKind(r: TeamRepo, value: string) {
    const kind = value === "" ? [] : (value.split("+") as RepoKind[]);
    void run(`k${r.id}`, "Could not set the kind", () => api.setProjectKind(r.id, kind, team));
  }

  function setIndex(r: TeamRepo, value: string) {
    void run(`i${r.id}`, "Could not change how Ken reads it", () =>
      api.setProjectIndex(r.id, value === "kind" ? null : (value as IndexState)),
    );
  }

  function takeOut(e: MouseEvent, r: TeamRepo) {
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    openConfirm(rect.left - 220, rect.bottom + 4, {
      title: `Remove ${memberLeaf(r.name)} from the team?`,
      body: "Ken stops indexing it. The folder and its files stay on disk.",
      confirmLabel: "Remove",
      onConfirm: () =>
        void run(`o${r.id}`, "Could not remove the repo", async () => {
          await api.workspaceRemoveMember(r.name);
          await app.refreshWorkspaceOverview();
        }),
    });
  }

  async function addRepos() {
    if (!overview) return;
    const picked = await openDialog({ directory: true, multiple: true, title: "Add a repo to the team" });
    const paths = Array.isArray(picked) ? picked : typeof picked === "string" ? [picked] : [];
    if (paths.length === 0) return;
    const name = overview.workspace;
    await run("add", "Could not add the repo", async () => {
      const proposal = await api.setupProposeRepos(paths);
      const rows = proposal.rows.map((r) => ({ ...r, include: true, team: team ?? r.team }));
      await app.confirmSetupRepos(name, rows, true);
      app.openTeam();
      await scope.refreshGroups();
      // The wiki drafts each new repo's page and proposes the rest.
      await api.wikiAddRepos(rows.map((r) => r.member)).catch((e) => toast.error("Could not update the wiki", e));
    });
  }

  function addFolder(member: string) {
    void run(`m${member}`, "Could not add the folder", async () => {
      await api.workspaceAddMember(member);
      moved = moved.filter((m) => !("member" in m) || m.member !== member);
      await app.refreshWorkspaceOverview();
      await scope.refreshGroups();
    });
  }

  function saveIgnores() {
    const lines = ignoresDraft.split("\n");
    void run("ignores", "Could not save the ignore list", async () => {
      await api.teamSaveIgnores(lines);
      editingIgnores = false;
    });
  }

  function runSweep() {
    const id = overview?.wiki?.id;
    if (!id) return;
    void run("sweep", "Could not run the sweep", () => api.runDriftNow(id));
  }

  // Claude writes the wiki's missing pages from the team's repos: a Repo
  // Map page for each, Current, Team and the architecture page. A page a
  // person wrote is left alone.
  function draftPages() {
    const name = overview?.wiki?.name;
    if (!name) return;
    void run("draft", "Could not start writing the wiki's pages", async () => {
      await api.draftWiki(name, null);
      drafting = true;
      toast.show("Claude is writing the wiki's missing pages. It takes a few minutes; you can keep using Ken.");
    });
  }

  function addRule() {
    const id = overview?.wiki?.id;
    const rule = newRule.trim();
    if (!id || !rule) return;
    void run("rule", "Could not add the rule", async () => {
      const path = await api.teamAddRule(id, rule);
      newRule = "";
      await openInWiki(path);
    });
  }

  async function openInWiki(path: string) {
    const id = overview?.wiki?.id;
    if (!id) return;
    if (app.focused !== id) await app.focusMember(id);
    app.openInFiles(path);
  }

  async function openFinding(f: TeamFinding) {
    if (!f.path) return;
    const id = f.projectId ?? overview?.wiki?.id;
    if (id && app.focused !== id) await app.focusMember(id);
    app.openInFiles(f.path);
  }

  // ── A repo's own settings, in a drawer from its row ─────────────────
  let drawer = $state<TeamRepo | null>(null);
  async function openDrawer(r: TeamRepo) {
    drawer = r;
    try {
      await app.focusMember(r.id, { stay: true });
    } catch (e) {
      drawer = null;
      toast.error(`Could not open ${memberLeaf(r.name)}`, e);
    }
  }

  // ── Repos no one has said the kind of ───────────────────────────────
  let kindsOpen = $state(false);
  let kindDraft = $state<Record<string, string>>({});
  async function openKinds() {
    const list = kindlessRepos(rows);
    kindsOpen = true;
    // Set-up's proposal, from what is in each repo, fills the choices in.
    const proposal = await api.setupProposeRepos(list.map((r) => r.path)).catch(() => null);
    const kinds = proposal ? proposedKinds(list, proposal.rows) : {};
    kindDraft = Object.fromEntries(list.map((r) => [r.id, (kinds[r.id] ?? []).join("+")]));
  }
  function saveKinds() {
    const chosen = Object.entries(kindDraft).filter(([, v]) => v !== "");
    if (chosen.length === 0) return;
    void run("kinds", "Could not set the kinds", async () => {
      for (const [id, v] of chosen) await api.setProjectKind(id, v.split("+") as RepoKind[], team);
      kindsOpen = false;
    });
  }

  const rows = $derived(overview ? [...(overview.wiki ? [overview.wiki] : []), ...overview.repos] : []);
  const driftCount = $derived(moved.length + rows.filter((r) => (r.behind ?? 0) > 0).length);
  const findings = $derived(orderFindings(overview?.findings ?? []));
  const kindless = $derived(kindlessRepos(rows));
</script>

<div class="team">
  <div class="head">
    <strong class="tname">{team ?? overview?.workspace ?? ""}</strong>
    <span class="spacer"></span>
    {#if overview}
      <span class="chip" class:warn={driftCount > 0}>drifted · {driftCount}</span>
      <span class="chip" class:warn={overview.gaps.length > 0}>gaps · {overview.gaps.length}</span>
      <span class="chip" class:warn={findings.length > 0}>findings · {findings.length}</span>
    {/if}
    <button class="btn btn-ghost" disabled={busy !== null} onclick={addRepos}>+ Add a repo</button>
    <button class="btn btn-primary" disabled={busy !== null} onclick={scanAgain}>{busy === "scan" ? "Scanning…" : "Scan again"}</button>
  </div>
  {#if error}<p class="warn-text">{error}</p>{/if}
  {#if !overview && !error}
    <Loading label="Reading the team's folders…" lines={4} />
  {/if}

  {#if overview && kindless.length > 0}
    <div class="kinds">
      <div class="kinds-head">
        <span>
          <strong>Set what each repo is.</strong>
          {kindless.length} {kindless.length === 1 ? "repo has" : "repos have"} no kind, so Ken does not know how deep to read
          {kindless.length === 1 ? "it" : "them"} or where the team's tickets and escalations go.
        </span>
        {#if !kindsOpen}
          <button class="btn btn-small btn-primary" onclick={() => void openKinds()}>Set kinds</button>
        {/if}
      </div>
      {#if kindsOpen}
        {#each kindless as r (r.id)}
          <div class="kind-row">
            <span class="rname">{memberLeaf(r.name)}</span>
            <span class="sel">
              <select bind:value={kindDraft[r.id]} disabled={busy !== null} aria-label="Kind of {memberLeaf(r.name)}">
                <option value="">not said</option>
                {#if (kindDraft[r.id] ?? "").includes("+")}<option value={kindDraft[r.id]}>{kindDraft[r.id].split("+").join(" + ")}</option>{/if}
                {#each KINDS.filter((k) => k.v !== "") as k (k.v)}<option value={k.v}>{k.label}</option>{/each}
              </select>
              <ChevronDown size={12} strokeWidth={1.75} />
            </span>
          </div>
        {/each}
        <p class="note">Filled in from what is in each repo. A wiki is the repo with the Research/Ingestion inbox.</p>
        <div class="actions">
          <button class="btn btn-ghost btn-small" onclick={() => (kindsOpen = false)}>Cancel</button>
          <button class="btn btn-primary btn-small" disabled={busy !== null} onclick={saveKinds}>Save</button>
        </div>
      {/if}
    </div>
  {/if}

  {#if overview}
    <div class="cols">
      <div class="left">
        <div class="divider">
          repos · {rows.length}{#if scanned} · last scan {timeAgo(scanned)}{/if}
        </div>
        <table>
          <thead>
            <tr><th>index</th><th>repo</th><th>kind</th><th>branch</th><th>commit</th><th>drift</th><th></th></tr>
          </thead>
          <tbody>
            {#each rows as r (r.id)}
              <tr class:gone={!r.available}>
                <td>
                  <span class="sel">
                    <select
                      value={r.index ?? "kind"}
                      disabled={busy !== null}
                      onchange={(e) => setIndex(r, e.currentTarget.value)}
                      title="How deeply Ken reads it"
                    >
                      <option value="kind">{r.effectiveIndex} (its kind)</option>
                      <option value="entities">entities</option>
                      <option value="search">search</option>
                      <option value="off">off</option>
                    </select>
                    <ChevronDown size={12} strokeWidth={1.75} />
                  </span>
                </td>
                <td>
                  <button class="repo" title="{memberLeaf(r.name)}: its settings" onclick={() => void openDrawer(r)}>
                    <span class="rname">{memberLeaf(r.name)}</span>
                    <span class="sub mono" title={r.path}>{r.description || r.path}</span>
                  </button>
                </td>
                <td>
                  <span class="sel">
                    <select value={kindValue(r)} disabled={busy !== null} onchange={(e) => setKind(r, e.currentTarget.value)}>
                      {#if r.kind.length > 1}<option value={kindValue(r)}>{r.kind.join(" + ")}</option>{/if}
                      {#each KINDS as k (k.v)}<option value={k.v}>{k.label}</option>{/each}
                    </select>
                    <ChevronDown size={12} strokeWidth={1.75} />
                  </span>
                </td>
                <td class="mono">{r.branch ?? "—"}</td>
                <td class="mono">{r.head ?? "—"}</td>
                <td>
                  {#if !r.available}<span class="bad">folder gone</span>
                  {:else if (r.behind ?? 0) > 0}<span class="bad">{r.behind} newer upstream</span>
                  {:else}—{/if}
                </td>
                <td class="row-actions">
                  <button
                    class="remove"
                    disabled={!r.available}
                    onclick={() => void openDrawer(r)}
                    title="Its settings"
                    aria-label="{memberLeaf(r.name)} settings"
                  >
                    <SlidersHorizontal size={14} strokeWidth={1.75} />
                  </button>
                  {#if r.id !== overview.wiki?.id}
                    <button
                      class="remove"
                      disabled={busy !== null}
                      onclick={(e) => takeOut(e, r)}
                      title="Remove from the team"
                      aria-label="Remove {memberLeaf(r.name)} from the team"
                    >
                      <X size={14} strokeWidth={1.75} />
                    </button>
                  {/if}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>

        <div class="divider">ignore list · .kenignore beside the repos · {overview.ignores.length} {overview.ignores.length === 1 ? "line" : "lines"}</div>
        {#if editingIgnores}
          <textarea class="mono" rows="8" bind:value={ignoresDraft}></textarea>
          <p class="note">One path or pattern a line; <span class="mono">~path/</span> keeps it in search but out of the graph. The next scan reads it.</p>
          <div class="actions">
            <button class="btn btn-ghost" onclick={() => (editingIgnores = false)}>Cancel</button>
            <button class="btn btn-primary" disabled={busy !== null} onclick={saveIgnores}>Save</button>
          </div>
        {:else}
          <div class="ignores">
            <span class="mono">{overview.ignores.length > 0 ? overview.ignores.join(" · ") : "worktrees, dependencies, build outputs and secrets are left out already"}</span>
            <button
              class="btn btn-ghost"
              onclick={() => {
                ignoresDraft = (overview?.ignores ?? []).join("\n");
                editingIgnores = true;
              }}>Edit</button
            >
          </div>
        {/if}
      </div>

      <div class="right">
        <div class="divider">drift and gaps · {moved.length + overview.gaps.length}</div>
        {#each moved as m, i (i)}
          <div class="entry">
            {#if m.change === "NewFolder"}
              <span><strong>{memberLeaf(m.member)}</strong> is a new folder beside the repos.</span>
              <span class="e-actions"><button class="btn btn-primary" disabled={busy !== null} onclick={() => addFolder(m.member)}>Add to team</button></span>
            {:else if m.change === "Gone"}
              <span><strong>{memberLeaf(m.member)}</strong>'s folder is gone.</span>
            {:else}
              <span>A new worktree, <span class="mono">{m.pattern}</span>, is left out.</span>
            {/if}
          </div>
        {/each}
        {#each overview.gaps as g, i (i)}
          <div class="entry">
            <span>{g.text}</span>
            {#if g.open}
              <span class="e-actions"><button class="btn btn-ghost" onclick={() => g.open && openInWiki(g.open)}>Open</button></span>
            {/if}
          </div>
        {/each}
        {#if moved.length + overview.gaps.length === 0}
          <p class="note">Nothing drifted and nothing missing{scanned ? "" : " (Scan again checks for new folders)"}.</p>
        {/if}

        <div class="divider">standing sweep · the wiki's drift check</div>
        {#if !overview.wiki}
          <p class="note">No wiki, so nothing to sweep.</p>
        {:else if overview.sweep}
          <div class="entry">
            <span>
              Last run {timeAgo(overview.sweep.at)} · {overview.sweep.pagesExamined} pages ·
              {overview.sweep.voidReason ? `void: ${overview.sweep.voidReason}` : overview.sweep.uncontrolled ? "no controls set" : "both controls held"}
            </span>
          </div>
        {:else}
          <p class="note">The sweep has not run yet.</p>
        {/if}
        {#if overview.wiki}
          <div class="actions">
            <button class="btn btn-ghost" disabled={busy !== null} onclick={runSweep}>{busy === "sweep" ? "Sweeping…" : "Run the sweep now"}</button>
            <button class="btn btn-ghost" disabled={busy !== null || drafting} onclick={draftPages}>
              {busy === "draft" ? "Starting…" : drafting ? "Claude is writing the pages…" : "Write the missing pages with Claude"}
            </button>
          </div>
        {/if}

        <div class="divider">findings · drift, links, the first draft · {findings.length}</div>
        {#each findings as f, i (i)}
          <div class="entry">
            <span class="tag">{findingLabel(f.kind)}</span>
            <span class="finding">
              <span class="ftitle">{f.title}</span>
              {#if f.detail}<span class="sub">{f.detail}</span>{/if}
            </span>
            {#if f.path}
              <span class="e-actions"><button class="btn btn-ghost" onclick={() => void openFinding(f)}>Open</button></span>
            {/if}
          </div>
        {:else}
          <p class="note">{overview.wiki ? "Nothing found." : "No wiki, so nothing to check."}</p>
        {/each}

        <div class="divider">rules · Ways-of-Working/Rules · {overview.rules.length}</div>
        {#each overview.rules as r (r.path)}
          <button class="page" onclick={() => openInWiki(r.path)}>{r.title}</button>
        {:else}
          <p class="note">No rules yet.</p>
        {/each}
        {#if overview.wiki}
          <div class="add-rule">
            <input bind:value={newRule} placeholder="Add a rule, as a sentence" onkeydown={(e) => e.key === "Enter" && addRule()} />
            <button class="btn btn-ghost" disabled={!newRule.trim() || busy !== null} onclick={addRule}>Add</button>
          </div>
        {/if}

        <div class="divider">templates · {overview.templates.length}</div>
        {#each overview.templates as t (t.path)}
          <button class="page" onclick={() => openInWiki(t.path)}>{t.title}</button>
        {:else}
          <p class="note">No templates.</p>
        {/each}
      </div>
    </div>

  {/if}
</div>

{#if drawer}
  {#key drawer.id}
    <RepoDrawer repo={drawer} close={() => (drawer = null)} changed={() => void refresh()} />
  {/key}
{/if}

<style>
  .team {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
  }
  h2 {
    margin: 0;
    font-size: 18px;
  }
  .tname {
    font-size: 15px;
  }
  .spacer {
    flex: 1;
  }
  .chip {
    font-size: 11px;
    padding: 2px 8px;
    border-radius: 99px;
    border: 1px solid var(--border);
    color: var(--ink-tertiary);
  }
  .chip.warn {
    color: var(--needs-input);
    border-color: color-mix(in srgb, var(--needs-input) 45%, transparent);
  }
  .cols {
    display: grid;
    grid-template-columns: minmax(0, 1.7fr) minmax(280px, 1fr);
    gap: 24px;
  }
  .left,
  .right {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 0;
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
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: 12.5px;
  }
  th {
    text-align: left;
    font-size: 10.5px;
    font-weight: 600;
    color: var(--ink-tertiary);
    padding: 4px 6px;
  }
  td {
    padding: 6px;
    border-bottom: 1px solid var(--border);
    vertical-align: middle;
  }
  tr.gone {
    opacity: 0.55;
  }
  .repo {
    display: flex;
    flex-direction: column;
    gap: 2px;
    max-width: 320px;
    border: none;
    background: transparent;
    padding: 0;
    font: inherit;
    text-align: left;
    color: inherit;
    cursor: pointer;
  }
  .repo:hover .rname {
    color: var(--accent-deep);
  }
  .row-actions {
    white-space: nowrap;
  }
  .kinds {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 10px 14px;
    border: 1px solid color-mix(in srgb, var(--needs-input) 35%, var(--border));
    border-radius: var(--radius-card);
    background: color-mix(in srgb, var(--needs-input) 6%, var(--surface));
    font-size: 12.5px;
  }
  .kinds-head {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .kinds-head > span {
    flex: 1;
    line-height: 1.5;
  }
  .kind-row {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .kind-row .rname {
    width: 180px;
    flex: none;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .tag {
    flex: none;
    width: 70px;
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--ink-tertiary);
  }
  .entry > span.finding {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .ftitle {
    font-weight: 600;
  }
  .rname {
    font-weight: 600;
    font-size: 13px;
  }
  .sub {
    font-size: 11px;
    color: var(--ink-tertiary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
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
  .remove {
    display: inline-grid;
    place-items: center;
    width: 24px;
    height: 24px;
    border: none;
    border-radius: 6px;
    background: transparent;
    color: var(--ink-tertiary);
    cursor: pointer;
  }
  .remove:hover:not(:disabled) {
    background: var(--sunken);
    color: var(--danger);
  }
  .bad {
    color: var(--needs-input);
  }
  .ignores {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 12px;
  }
  .ignores .mono {
    flex: 1;
    color: var(--ink-secondary, var(--ink));
  }
  textarea {
    width: 100%;
    font-size: 12px;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }
  .entry {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 12.5px;
    padding: 6px 0;
    border-bottom: 1px dashed var(--border);
  }
  .entry > span:first-child:not(.tag) {
    flex: 1;
  }
  .e-actions {
    display: flex;
    gap: 6px;
    flex: none;
  }
  .page {
    background: none;
    border: none;
    text-align: left;
    padding: 4px 0;
    font-size: 13px;
    color: var(--accent);
    cursor: pointer;
  }
  .page:hover {
    text-decoration: underline;
  }
  .add-rule {
    display: flex;
    gap: 6px;
  }
  .add-rule input {
    flex: 1;
    font-size: 12.5px;
  }
  .note {
    margin: 2px 0;
    font-size: 12.5px;
    color: var(--ink-tertiary);
  }
  .warn-text {
    margin: 0;
    font-size: 12.5px;
    color: var(--needs-input);
  }
</style>

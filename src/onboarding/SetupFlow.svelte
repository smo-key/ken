<script lang="ts">
  // Set-up for Ken on its own: Repos, Team, Index (step 4 of the
  // knowledge-layer plan). A person picks the repos, one or many at a time,
  // wherever they are; the workspace lives in Ken's app data, so nothing is
  // written into a shared parent. Each row is proposed from what is in the
  // repo and every cell can change; nothing is written until Confirm.
  import { open as openDialog } from "@tauri-apps/plugin-dialog";
  import { api, type IndexState, type RepoKind, type SetupRepoRow } from "../lib/api";
  import { app } from "../lib/app.svelte";
  import { toast } from "../lib/toast.svelte";
  import FolderOpen from "@lucide/svelte/icons/folder-open";
  import {
    commonParent,
    coveredRepos,
    defaultTeamName,
    defaultWikiChoice,
    indexSummary,
    KINDS,
    mergeRows,
    newWikiPath,
    onOneTeam,
    renamedWikiChoice,
    suggestedTeams,
    teamWikis,
    toggleKind,
    wikiChoicesReady,
    type WikiChoice,
  } from "./setupFlow";

  let { onclose }: { onclose: () => void } = $props();

  type Step = "repos" | "team" | "index";
  let step = $state<Step>("repos");
  let picked = $state<string[]>([]);
  // Repos taken out, so a folder of repos picked again does not bring them back.
  let removed = $state<string[]>([]);
  let rows = $state<SetupRepoRow[]>([]);
  let name = $state("Code");
  let busy = $state(false);
  let error = $state<string | null>(null);

  const summary = $derived(indexSummary(rows));
  const teams = $derived(suggestedTeams(rows));

  // A workspace is usually one team: every repo goes on it under one name.
  // A person splits the repos across teams only when they belong to more.
  let teamName = $state("");
  let split = $state(false);
  let teamProposed = false;
  let nameTouched = false;

  function goTeam() {
    if (!teamProposed) {
      teamProposed = true;
      const proposed = suggestedTeams(rows);
      split = proposed.length > 1;
      teamName = proposed[0] ?? defaultTeamName(rows);
    }
    if (!split) rows = onOneTeam(rows, teamName);
    step = "team";
  }

  function setTeamName(value: string) {
    teamName = value;
    rows = onOneTeam(rows, value);
  }

  function toggleSplit() {
    split = !split;
    if (!split) rows = onOneTeam(rows, teamName);
  }

  function goIndex() {
    if (!nameTouched) name = teamName.trim() || teams[0] || name;
    step = "index";
  }

  // A wiki made here gets its first pages drafted by Claude from the
  // team's repos; a wiki repo picked has its missing pages filled when the
  // person ticks it. Either may also read a folder of documents.
  let fillExisting = $state<Record<string, boolean>>({});
  let draftExtra = $state<string | null>(null);

  // The team repo (tickets, decisions, ideas): one picked, or a new one
  // from the method's template.
  type TeamRepoChoice = { create: boolean; parent: string | null; name: string };
  let teamRepoChoices = $state<Record<string, TeamRepoChoice>>({});
  let createdTeamRepos = $state<Record<string, SetupRepoRow>>({});
  const pickedTeamRepo = (t: string) => rows.find((r) => r.include && r.team === t && r.kind.includes("team")) ?? null;
  $effect(() => {
    const next: Record<string, TeamRepoChoice> = {};
    const before = Object.keys(teamRepoChoices);
    for (const t of teams) {
      const was = teamRepoChoices[t] ?? (teams.length === 1 && before.length === 1 ? teamRepoChoices[before[0]] : undefined);
      next[t] = was
        ? { ...was, name: was.name === `${before[0]}-Team` ? `${t}-Team` : was.name }
        : { create: true, parent: lastParent ?? commonParent(rows), name: `${t}-Team` };
    }
    if (JSON.stringify(next) !== JSON.stringify(teamRepoChoices)) teamRepoChoices = next;
  });

  async function chooseTeamRepoParent(team: string) {
    const c = teamRepoChoices[team];
    const folder = await openDialog({ directory: true, title: `Where the ${team} team repo goes`, defaultPath: c?.parent ?? undefined });
    if (typeof folder === "string" && c) {
      lastParent = folder;
      teamRepoChoices[team] = { ...c, parent: folder };
    }
  }


  // The wiki is the team's: each team uses a wiki repo it picked, creates a
  // new one from the method's template, or has none for now.
  let wikiChoices = $state<Record<string, WikiChoice>>({});
  // Wikis already created this session, by team, so a retry after a failed
  // Confirm does not try to create the folder again.
  let created = $state<Record<string, SetupRepoRow>>({});
  const choicesReady = $derived(
    wikiChoicesReady(wikiChoices) &&
      teams.every((t) => pickedTeamRepo(t) || !teamRepoChoices[t]?.create || (!!teamRepoChoices[t]?.parent && teamRepoChoices[t].name.trim().length > 0)),
  );
  $effect(() => {
    const parent = lastParent ?? commonParent(rows);
    const before = Object.keys(wikiChoices);
    const next: Record<string, WikiChoice> = {};
    for (const t of teams) {
      // Typing the one team's name keeps its wiki choice.
      const was =
        wikiChoices[t] ??
        (teams.length === 1 && before.length === 1 ? renamedWikiChoice(wikiChoices[before[0]], before[0], t) : undefined);
      const stillValid = was && (was.mode !== "existing" || teamWikis(rows, t).some((w) => w.member === was.member));
      next[t] = stillValid ? was : defaultWikiChoice(rows, t, parent);
    }
    if (JSON.stringify(next) !== JSON.stringify(wikiChoices)) wikiChoices = next;
  });

  // The folder last chosen for a new wiki, offered again when one is ticked.
  let lastParent = $state<string | null>(null);

  function chooseWiki(team: string, member: string) {
    wikiChoices[team] = { mode: "existing", member };
  }

  function toggleNewWiki(team: string, on: boolean) {
    wikiChoices[team] = on
      ? { mode: "new", parent: lastParent ?? commonParent(rows), name: `${team}-Wiki` }
      : { mode: "none" };
  }

  async function chooseWikiParent(team: string) {
    const c = wikiChoices[team];
    const folder = await openDialog({
      directory: true,
      title: `Where the ${team} wiki goes`,
      defaultPath: c?.mode === "new" && c.parent ? c.parent : undefined,
    });
    if (typeof folder === "string" && c?.mode === "new") {
      lastParent = folder;
      wikiChoices[team] = { ...c, parent: folder };
    }
  }

  async function chooseExtra() {
    const folder = await openDialog({ directory: true, title: "A folder of documents to read too (a Confluence export, say)" });
    if (typeof folder === "string") draftExtra = folder;
  }

  /** Pick one or more repos (a folder of repos stands for each inside it).
   *  The whole set is proposed again, so names stay unique; edits to rows
   *  already here are kept. */
  async function addRepos() {
    error = null;
    const chosen = await openDialog({ directory: true, multiple: true, title: "Pick the repos Ken should know" });
    const list = Array.isArray(chosen) ? chosen : typeof chosen === "string" ? [chosen] : [];
    if (list.length === 0) return;
    busy = true;
    try {
      const all = [...picked, ...list.filter((p) => !picked.includes(p))];
      const proposal = await api.setupProposeRepos(all);
      picked = all;
      // Picking a repo itself again undoes taking it out.
      removed = removed.filter((r) => !list.includes(r));
      rows = mergeRows(rows, proposal.rows.filter((r) => !r.path || !removed.includes(r.path)));
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  function remove(row: SetupRepoRow) {
    rows = rows.filter((r) => r !== row);
    if (row.path) removed = [...removed, row.path];
  }

  async function confirm() {
    if (busy) return;
    busy = true;
    error = null;
    try {
      let all = [...rows];
      // New team repos first, so a new wiki's Start Here lists them.
      for (const t of teams) {
        const c = teamRepoChoices[t];
        if (pickedTeamRepo(t) || !c?.create || !c.parent) continue;
        const w = wikiChoices[t];
        const wikiName = w?.mode === "new" ? w.name.trim() : w?.mode === "existing" ? w.member : null;
        const row =
          createdTeamRepos[t] ??
          (await api.setupCreateTeamRepo(newWikiPath(c.parent, c.name), t, wikiName, all.map((r) => r.member), coveredRepos(all, t)));
        createdTeamRepos[t] = row;
        all = [...all.filter((r) => r.path !== row.path), row];
      }
      // Then new wikis: each is laid down from the template and joins the
      // set-up as its team's wiki.
      const drafts: string[] = [];
      for (const t of teams) {
        const c = wikiChoices[t];
        if (c?.mode === "existing" && fillExisting[t]) drafts.push(c.member);
        if (c?.mode !== "new" || !c.parent) continue;
        // No team repo picked or made: the docs repo holds the tickets too.
        const holdsTeam = !all.some((r) => r.include && r.team === t && r.kind.includes("team"));
        const row =
          created[t] ??
          (await api.setupCreateWiki(newWikiPath(c.parent, c.name), t, coveredRepos(all, t), all.map((r) => r.member), holdsTeam));
        created[t] = row;
        all = [...all.filter((r) => r.path !== row.path), row];
        drafts.push(row.member);
      }
      await app.confirmSetupRepos(name.trim() || "Code", all);
      // Drafting runs in the background with Claude; set-up is done either way.
      for (const w of drafts) {
        await api.draftWiki(w, draftExtra).catch((e) => toast.error("Could not start drafting the wiki's first pages", e));
      }
      onclose();
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  const STATES: { value: IndexState; label: string }[] = [
    { value: "entities", label: "read in full" },
    { value: "search", label: "search only" },
    { value: "off", label: "left out" },
  ];
</script>

<div class="setup">
  <ol class="steps" aria-label="Set-up steps">
    {#each [["repos", "Repos"], ["team", "Team"], ["index", "Read"]] as [s, label], i}
      <li class:current={step === s}>{i + 1} · {label}</li>
    {/each}
  </ol>

  {#if step === "repos"}
    <h2>Which repos should Ken know?</h2>
    <p class="note">
      Pick each repo, wherever it lives; pick a folder of repos to add every repo in it. Ken reads
      what is in each one to propose what it is, and reads nothing until you confirm. Say in a line
      what each repo is for: it helps Ken use it well, and the first-wiki draft reads it.
    </p>
    <div class="actions">
      <button class="btn btn-primary" disabled={busy} onclick={addRepos}>
        {busy ? "Reading…" : rows.length === 0 ? "Pick repos…" : "Add more repos…"}
      </button>
    </div>

    {#if rows.length > 0}
      <div class="table" role="table">
        <div class="tr head" role="row">
          <span>Include</span><span>Repo</span><span>Kind</span><span>Index</span><span></span>
        </div>
        {#each rows as row (row.path ?? row.member)}
          <div class="tr" role="row" class:dim={!row.include}>
            <span><input type="checkbox" bind:checked={row.include} aria-label="Include {row.member}" /></span>
            <span class="repo">
              <span class="mono">{row.member}</span>
              <span class="evidence mono" title={row.path ?? ""}>{row.path}</span>
              {#if row.evidence.length > 0}<span class="evidence">{row.evidence.join(" · ")}</span>{/if}
              <textarea
                class="textarea description"
                rows="2"
                placeholder="What is this repo for, and how is it used?"
                bind:value={row.description}
                aria-label="What {row.member} is for"
              ></textarea>
            </span>
            <span class="kinds">
              {#each KINDS as k}
                <button
                  class="chip"
                  class:on={row.kind.includes(k)}
                  onclick={() => (row.kind = toggleKind(row.kind, k as RepoKind))}
                  aria-pressed={row.kind.includes(k)}>{k}</button
                >
              {/each}
            </span>
            <span>
              <select bind:value={row.index} aria-label="Index state for {row.member}">
                {#each STATES as s}<option value={s.value}>{s.label}</option>{/each}
              </select>
            </span>
            <span>
              <button class="remove" title="Take it out" aria-label="Remove {row.member}" onclick={() => remove(row)}>✕</button>
            </span>
          </div>
        {/each}
      </div>
      <p class="note">
        Team and wiki repos are read in full: Ken maps the people, decisions and tickets in them. Code
        and reference repos are search only: found by search and read by Claude when asked, and Ken never
        commits into them. Secrets (<span class="mono">.env</span>, keys,
        <span class="mono">credentials.json</span>) are never read.
      </p>
    {/if}
    <div class="actions">
      <button class="btn btn-primary" disabled={rows.every((r) => !r.include)} onclick={goTeam}>Next</button>
      <button class="btn btn-ghost" onclick={onclose}>Cancel</button>
    </div>
  {:else if step === "team"}
    <h2>Your team</h2>
    {#if !split}
      <p class="note">
        The repos you picked are one team. It keeps one wiki, and Home, search and chat cover its repos.
      </p>
      <label class="field">
        <span class="field-label">Team name</span>
        <input
          class="input"
          value={teamName}
          placeholder="The team's name"
          oninput={(e) => setTeamName((e.currentTarget as HTMLInputElement).value)}
        />
      </label>
      <p class="note">
        {#each rows.filter((r) => r.include) as row, i (row.path ?? row.member)}{i > 0 ? " · " : ""}<span class="mono">{row.member}</span>{/each}
      </p>
      {#if teams.length === 1}
        <h3>Wiki</h3>
        {@render wikiRow(teams[0])}
        <h3>Team repo</h3>
        {@render teamRepoRow(teams[0])}
      {/if}
    {:else}
      <p class="note">Repos with the same team name are one team, and each team keeps one wiki.</p>
      <div class="table" role="table">
        <div class="tr head teams" role="row"><span>Repo</span><span>Team</span></div>
        {#each rows.filter((r) => r.include) as row (row.path ?? row.member)}
          <div class="tr teams" role="row">
            <span class="mono">{row.member}</span>
            <input
              class="input team"
              placeholder="No team"
              value={row.team ?? ""}
              oninput={(e) => (row.team = (e.currentTarget as HTMLInputElement).value.trim() || null)}
              aria-label="Team for {row.member}"
            />
          </div>
        {/each}
      </div>
      {#each teams as t (t)}
        <h3>{t}'s wiki</h3>
        {@render wikiRow(t)}
        <h3>{t}'s team repo</h3>
        {@render teamRepoRow(t)}
      {/each}
    {/if}
    <div>
      <button class="btn btn-link" onclick={toggleSplit}>
        {split ? "Put them all on one team" : "These repos belong to more than one team"}
      </button>
    </div>
    <div class="actions">
      <button class="btn btn-primary" disabled={!choicesReady} onclick={goIndex}>Next</button>
      <button class="btn btn-ghost" onclick={() => (step = "repos")}>Back</button>
    </div>
  {:else}
    <h2>Ready to read</h2>
    <p class="note">
      Confirm saves the workspace on this computer and starts reading the repos in the background; you
      can use Ken while it runs. A wiki or team repo made in step 2 is created first.
    </p>
    <p class="counts">
      <strong>{summary.entities}</strong> read in full · <strong>{summary.search}</strong>
      search only · <strong>{summary.off}</strong> left out
    </p>
    <label class="field">
      <span class="field-label">Workspace name</span>
      <input class="input" bind:value={name} oninput={() => (nameTouched = true)} />
    </label>
    <div class="actions">
      <button class="btn btn-primary" disabled={busy || summary.entities + summary.search === 0} onclick={confirm}>
        {busy ? "Setting up…" : "Confirm and start reading"}
      </button>
      <button class="btn btn-ghost" onclick={() => (step = "team")}>Back</button>
    </div>
  {/if}

  {#snippet wikiRow(t: string)}
    {@const c = wikiChoices[t] ?? { mode: "none" }}
    {@const existing = teamWikis(rows, t)}
    <div class="wiki">
      {#if existing.length > 0}
        <span class="note">
          {#if existing.length === 1}
            Uses <span class="mono">{existing[0].member}</span>, the wiki repo you picked.
          {:else}
            Uses
            <select
              aria-label="Wiki for {t}"
              value={c.mode === "existing" ? c.member : existing[0].member}
              onchange={(e) => chooseWiki(t, (e.currentTarget as HTMLSelectElement).value)}
            >
              {#each existing as w (w.member)}<option value={w.member}>{w.member}</option>{/each}
            </select>
          {/if}
        </span>
        <label class="check-row">
          <input type="checkbox" checked={!!fillExisting[t]} onchange={(e) => (fillExisting[t] = e.currentTarget.checked)} />
          <span>Have Claude write the pages it is missing (a Repo Map page per repo, Current, Team)</span>
        </label>
      {:else}
        <label class="check-row">
          <input type="checkbox" checked={c.mode === "new"} onchange={(e) => toggleNewWiki(t, e.currentTarget.checked)} />
          <span>Create a wiki for {t} from the Ways of Working template</span>
        </label>
        {#if c.mode === "new"}
          <div class="wiki-where">
            <input
              class="input"
              value={c.name}
              aria-label="Folder name of the {t} wiki"
              oninput={(e) => (wikiChoices[t] = { ...c, name: (e.currentTarget as HTMLInputElement).value })}
            />
            <span class="in">in</span>
            <button class="btn where" title={c.parent ?? ""} onclick={() => chooseWikiParent(t)}>
              <FolderOpen size={14} strokeWidth={1.75} aria-hidden="true" />
              <span class="where-path">{c.parent ?? "Choose a folder…"}</span>
            </button>
          </div>
          <p class="note">
            {#if c.parent}
              Ken makes <span class="mono">{newWikiPath(c.parent, c.name || `${t}-Wiki`)}</span> from the template,
              with one git commit, then Claude writes its first pages from the team's repos: a Repo Map page
              for each, then Current, Team and the architecture page. Each is marked draft and names its
              sources. It stays on this computer; adding a remote and pushing is yours to do.
            {:else}
              Choose where the wiki folder goes, or untick it to go without one for now.
            {/if}
          </p>
        {/if}
      {/if}
      {#if c.mode === "new" || (c.mode === "existing" && fillExisting[t])}
        <div class="wiki-where">
          <button class="btn btn-ghost" onclick={chooseExtra}>
            {draftExtra ? `Also reading ${draftExtra.split(/[\\/]/).pop()}` : "Also read a folder of documents…"}
          </button>
        </div>
      {/if}
    </div>
  {/snippet}

  {#snippet teamRepoRow(t: string)}
    {@const picked = pickedTeamRepo(t)}
    {@const c = teamRepoChoices[t] ?? { create: false, parent: null, name: `${t}-Team` }}
    <div class="wiki">
      {#if picked}
        <span class="note">Uses <span class="mono">{picked.member}</span>, the team repo you picked: tickets, decisions and escalations.</span>
      {:else}
        <label class="check-row">
          <input type="checkbox" checked={c.create} onchange={(e) => (teamRepoChoices[t] = { ...c, create: e.currentTarget.checked })} />
          <span>Create a team repo for {t} from the Ways of Working template</span>
        </label>
        {#if c.create}
          <div class="wiki-where">
            <input
              class="input"
              value={c.name}
              aria-label="Folder name of the {t} team repo"
              oninput={(e) => (teamRepoChoices[t] = { ...c, name: (e.currentTarget as HTMLInputElement).value })}
            />
            <span class="in">in</span>
            <button class="btn where" title={c.parent ?? ""} onclick={() => chooseTeamRepoParent(t)}>
              <FolderOpen size={14} strokeWidth={1.75} aria-hidden="true" />
              <span class="where-path">{c.parent ?? "Choose a folder…"}</span>
            </button>
          </div>
          <p class="note">
            {#if c.parent}
              Ken makes <span class="mono">{newWikiPath(c.parent, c.name || `${t}-Team`)}</span> with tickets, the
              decisions log, the Ideas list, people and the method, and one git commit. Tickets are numbered
              {t.replace(/[^A-Za-z0-9]/g, "").slice(0, 6).toUpperCase() || "TEAM"}-001 onwards.
            {:else}
              Choose where the team repo goes, or untick it to go without one for now.
            {/if}
          </p>
        {/if}
      {/if}
    </div>
  {/snippet}

  {#if error}<div class="error">{error}</div>{/if}
</div>

<style>
  .setup {
    display: flex;
    flex-direction: column;
    gap: 12px;
    text-align: left;
  }
  .steps {
    display: flex;
    gap: 14px;
    margin: 0 0 4px;
    padding: 0;
    list-style: none;
    font-size: 12px;
    color: var(--ink-tertiary);
  }
  .steps .current {
    color: var(--accent);
    font-weight: 600;
  }
  h2 {
    margin: 0;
    font-size: 18px;
  }
  .note {
    margin: 0;
    font-size: 13px;
    line-height: 1.5;
    color: var(--ink-secondary);
  }
  .counts {
    margin: 0;
    font-size: 13px;
    color: var(--ink-secondary);
  }
  .actions {
    display: flex;
    gap: 8px;
    margin-top: 4px;
  }
  .table {
    display: flex;
    flex-direction: column;
    border: 1px solid var(--border);
    border-radius: 8px;
    overflow: auto;
    max-height: 46vh;
  }
  .tr {
    display: grid;
    grid-template-columns: 56px minmax(220px, 2.4fr) minmax(170px, 1.2fr) 140px 28px;
    gap: 8px;
    align-items: start;
    padding: 8px 10px;
    border-bottom: 1px solid var(--border);
    font-size: 12.5px;
  }
  .tr.teams {
    grid-template-columns: minmax(160px, 1fr) minmax(160px, 1fr);
    align-items: center;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 6px;
    max-width: 360px;
  }
  .field-label {
    font-size: 12.5px;
    font-weight: 600;
    color: var(--ink-2);
  }
  .wiki {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px 14px;
    border: 1px solid var(--line);
    border-radius: 10px;
    background: var(--surface);
  }
  .check-row {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 13.5px;
    color: var(--ink);
    cursor: pointer;
  }
  .wiki-where {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    padding-left: 26px;
  }
  .wiki-where .input {
    width: 160px;
    flex: none;
  }
  .wiki-where + .note {
    padding-left: 26px;
  }
  .in {
    font-size: 13px;
    color: var(--ink-3);
  }
  .where {
    flex: 1;
    min-width: 0;
    justify-content: flex-start;
  }
  .where-path {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  h3 {
    margin: 6px 0 0;
    font-size: 14px;
  }
  .description {
    width: 100%;
    margin-top: 4px;
    font: inherit;
    font-size: 12.5px;
    resize: vertical;
  }
  .remove {
    border: none;
    background: none;
    color: var(--ink-tertiary);
    cursor: pointer;
  }
  .remove:hover {
    color: var(--needs-input);
  }
  .tr:last-child {
    border-bottom: none;
  }
  .tr.head {
    position: sticky;
    top: 0;
    background: var(--sunken-2);
    font-weight: 600;
    color: var(--ink-secondary);
  }
  .tr.dim {
    opacity: 0.55;
  }
  .repo {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .evidence {
    font-size: 11.5px;
    color: var(--ink-tertiary);
  }
  .team {
    width: 100%;
  }
  .kinds {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }
  .chip {
    padding: 1px 8px;
    font-size: 11.5px;
    border: 1px solid var(--border);
    border-radius: 999px;
    background: transparent;
    color: var(--ink-secondary);
    cursor: pointer;
  }
  .chip.on {
    border-color: var(--accent);
    color: var(--accent);
  }
  .error {
    color: var(--needs-input);
    font-size: 13px;
  }
</style>

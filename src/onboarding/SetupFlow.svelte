<script lang="ts">
  // Set-up for Ken on its own, in three steps (knowledge-core decision 32):
  // 1, the workspace's name; 2, where its knowledge base is saved, a new repo
  // from Ken's template or an existing wiki to overlay (its pages mapped into
  // the sections after a dry run); 3, its sources, repos and folders picked
  // wherever they are, each with its kind. The workspace lives in Ken's app
  // data, so nothing is written into a shared parent. Nothing is written
  // until Confirm, which lays the knowledge base down and indexes every
  // source, so search and the code index work from day one.
  import { open as openDialog } from "@tauri-apps/plugin-dialog";
  import { api, type RepoKind, type SetupRepoRow, type WikiMapPlan } from "../lib/api";
  import { app } from "../lib/app.svelte";
  import { toast } from "../lib/toast.svelte";
  import FolderOpen from "@lucide/svelte/icons/folder-open";
  import {
    commonParent,
    coveredRepos,
    defaultKb,
    kbReady,
    KINDS,
    mergeRows,
    newWikiPath,
    renamedKb,
    confirmBlocked,
    rowEvidence,
    rowTip,
    sourceSummary,
    STEPS,
    toggleKind,
    withKnowledgeBase,
    type KbChoice,
    type Step,
  } from "./setupFlow";

  let { onclose }: { onclose: () => void } = $props();

  let step = $state<Step>("name");
  let name = $state("");
  let picked = $state<string[]>([]);
  // Repos taken out, so a folder of repos picked again does not bring them back.
  let removed = $state<string[]>([]);
  let rows = $state<SetupRepoRow[]>([]);
  let busy = $state(false);
  let error = $state<string | null>(null);

  const summary = $derived(sourceSummary(rows));
  const blocked = $derived(confirmBlocked(rows));

  // Step 2: the knowledge base. A new one follows the workspace's name until
  // a person names it.
  let kb = $state<KbChoice>(defaultKb(""));
  let namedFor = "";
  function setName(value: string) {
    kb = renamedKb(kb, namedFor, value);
    namedFor = value;
    name = value;
  }

  async function chooseParent() {
    const c = kb;
    if (c.mode !== "new") return;
    const folder = await openDialog({ directory: true, title: "Where the knowledge base goes", defaultPath: c.parent ?? undefined });
    if (typeof folder === "string") kb = { ...c, parent: folder };
  }

  // An existing wiki: picked, then its dry run read so the moves are seen
  // before anything moves.
  let mapPlan = $state<WikiMapPlan | null>(null);
  async function chooseExisting() {
    const folder = await openDialog({ directory: true, title: "The wiki to lay Ken's structure over" });
    if (typeof folder !== "string") return;
    kb = { mode: "existing", path: folder, map: true };
    mapPlan = null;
    try {
      mapPlan = await api.wikiMapPlan(folder);
    } catch (e) {
      error = String(e);
    }
  }

  /** Pick one or more repos (a folder of repos stands for each inside it).
   *  The whole set is proposed again, so names stay unique; edits to rows
   *  already here are kept. */
  async function addRepos() {
    error = null;
    const chosen = await openDialog({ directory: true, multiple: true, title: "Pick the sources Ken should know" });
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

  // The knowledge base made this session, so a retry after a failed Confirm
  // does not lay it down twice.
  let made = $state<SetupRepoRow | null>(null);

  async function confirm() {
    if (busy) return;
    busy = true;
    error = null;
    const workspace = name.trim() || "Workspace";
    try {
      const taken = rows.map((r) => r.member);
      let kbRow = made;
      if (!kbRow && kb.mode === "new" && kb.parent) {
        kbRow = await api.setupCreateWiki(newWikiPath(kb.parent, kb.name), workspace, coveredRepos(rows), taken);
      } else if (!kbRow && kb.mode === "existing" && kb.path) {
        kbRow = (await api.setupOverlayWiki(kb.path, workspace, coveredRepos(rows, kb.path), taken)).row;
      }
      if (!kbRow) throw new Error("Choose where the knowledge base is saved.");
      made = kbRow;
      await app.confirmSetupRepos(workspace, withKnowledgeBase(rows, kbRow));
      // An existing wiki's pages go into the sections once the person saw
      // the dry run and left it ticked.
      if (kb.mode === "existing" && kb.path && kb.map && mapPlan && mapPlan.moves.length > 0) {
        const done = await api.wikiMapApply(kb.path, mapPlan.moves);
        toast.show(`${done.moved.length} pages moved${done.unresolved.length ? ` · ${done.unresolved.length} links left as they were` : ""}`);
      }
      onclose();
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }
</script>

<div class="setup">
  <ol class="steps t-small" aria-label="Set-up steps">
    {#each STEPS as s, i (s.step)}
      <li class:current={step === s.step}>{i + 1} · {s.label}</li>
    {/each}
  </ol>

  {#if step === "name"}
    <h2 class="t-heading">Name the workspace</h2>
    <!-- The heading names the step; the field's placeholder is its label. -->
    <div class="field">
      <input
        class="input"
        value={name}
        placeholder="Workspace name"
        aria-label="Workspace name"
        oninput={(e) => setName((e.currentTarget as HTMLInputElement).value)}
      />
    </div>
    <div class="actions">
      <button
        class="btn btn-primary"
        disabled={!name.trim()}
        onclick={() => {
          if (kb.mode === "new" && !kb.parent) kb = { ...kb, parent: commonParent(rows) };
          step = "base";
        }}>Next</button
      >
      <button class="btn btn-ghost" onclick={onclose}>Cancel</button>
    </div>
  {:else if step === "base"}
    <h2 class="t-heading">Where the knowledge base is saved</h2>
    <p class="note">Ken commits only in the knowledge base.</p>
    <div class="k-card choice">
      <label class="check-row">
        <input type="radio" name="kb" checked={kb.mode === "new"} onchange={() => (kb = defaultKb(name, commonParent(rows)))} />
        <span>A new repo, from Ken's template</span>
      </label>
      {#if kb.mode === "new"}
        {@const c = kb}
        <div class="where-row">
          <input
            class="input"
            value={c.name}
            aria-label="Folder name of the knowledge base"
            oninput={(e) => (kb = { ...c, name: (e.currentTarget as HTMLInputElement).value })}
          />
          <span class="in">in</span>
          <button class="btn where" title={c.parent ?? ""} onclick={() => void chooseParent()}>
            <FolderOpen size={14} strokeWidth={1.75} aria-hidden="true" />
            <span class="where-path">{c.parent ?? "Choose a folder…"}</span>
          </button>
        </div>
      {/if}
      <label class="check-row">
        <input type="radio" name="kb" checked={kb.mode === "existing"} onchange={() => void chooseExisting()} />
        <span>An existing wiki, with Ken's structure laid over it</span>
      </label>
      {#if kb.mode === "existing"}
        {@const c = kb}
        <div class="where-row">
          <button class="btn where" title={c.path ?? ""} onclick={() => void chooseExisting()}>
            <FolderOpen size={14} strokeWidth={1.75} aria-hidden="true" />
            <span class="where-path">{c.path ?? "Choose the wiki's folder…"}</span>
          </button>
        </div>
        {#if c.path}
          {#if mapPlan}
            {#if mapPlan.moves.length > 0}
              <label class="check-row indent">
                <input type="checkbox" checked={c.map} onchange={(e) => (kb = { ...c, map: e.currentTarget.checked })} />
                <span>Move {mapPlan.moves.length} pages into the sections on Confirm (a dry run, below)</span>
              </label>
              <div class="moves indent">
                {#each mapPlan.moves as m (m.from)}
                  <div class="move"><span class="mono">{m.from}</span> → <span class="mono">{m.to}</span> <span class="evidence">{m.why}</span></div>
                {/each}
                {#each mapPlan.left as [page, why] (page)}
                  <div class="move"><span class="mono">{page}</span> <span class="evidence">stays: {why}</span></div>
                {/each}
              </div>
            {:else}
              <p class="note indent">Every page is already in a section.</p>
            {/if}
          {/if}
        {/if}
      {/if}
    </div>
    <div class="actions">
      <button class="btn btn-primary" disabled={!kbReady(kb)} onclick={() => (step = "sources")}>Next</button>
      <button class="btn btn-ghost" onclick={() => (step = "name")}>Back</button>
    </div>
  {:else}
    <h2 class="t-heading">Sources</h2>
    <div class="actions">
      <button class="btn" disabled={busy} onclick={addRepos}>
        {busy ? "Reading…" : rows.length === 0 ? "Pick sources…" : "Add more…"}
      </button>
    </div>

    {#if rows.length > 0}
      <div class="k-card table" role="table">
        <div class="tr head t-label" role="row">
          <span>Index</span><span>Source</span><span>Kind</span><span></span>
        </div>
        {#each rows as row (row.path ?? row.member)}
          <div class="tr" role="row" class:dim={!row.include}>
            <span><input type="checkbox" bind:checked={row.include} aria-label="Index {row.member}" /></span>
            <span class="repo">
              <span class="mono" title={rowTip(row) || undefined}>{row.member}</span>
              {#if rowEvidence(row)}<span class="evidence">{rowEvidence(row)}</span>{/if}
              <textarea
                class="textarea description"
                rows="2"
                placeholder="What is this source for, and how is it used?"
                bind:value={row.description}
                aria-label="What {row.member} is for"
              ></textarea>
            </span>
            <span class="kinds">
              {#each KINDS as k (k)}
                <button
                  class="chip"
                  class:on={row.kind.includes(k)}
                  onclick={() => (row.kind = toggleKind(row.kind, k as RepoKind))}
                  aria-pressed={row.kind.includes(k)}>{k}</button
                >
              {/each}
            </span>
            <span>
              <button class="remove" title="Take it out" aria-label="Remove {row.member}" onclick={() => remove(row)}>✕</button>
            </span>
          </div>
        {/each}
      </div>
    {/if}
    <p class="counts"><strong>{summary.indexed}</strong> indexed · <strong>{summary.left}</strong> left out</p>
    <div class="actions">
      <button class="btn btn-primary" disabled={busy || !kbReady(kb) || !!blocked} title={blocked ?? undefined} onclick={confirm}>
        {busy ? "Setting up…" : "Confirm and start indexing"}
      </button>
      <button class="btn btn-ghost" onclick={() => (step = "base")}>Back</button>
    </div>
  {/if}

  {#if error}<div class="k-notice danger">{error}</div>{/if}
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
    gap: 16px;
    margin: 0 0 4px;
    padding: 0;
    list-style: none;
  }
  .steps .current {
    color: var(--accent-ink);
    font-weight: 600;
  }
  h2 {
    margin: 0;
  }
  .note {
    margin: 0;
    font-size: 13px;
    line-height: 1.5;
    color: var(--ink-2);
  }
  .indent {
    padding-left: 26px;
  }
  .counts {
    margin: 0;
    font-size: 13px;
    color: var(--ink-2);
  }
  .actions {
    display: flex;
    gap: 8px;
    margin-top: 4px;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 6px;
    max-width: 360px;
  }
  .choice {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px 14px;
  }
  .check-row {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 13.5px;
    color: var(--ink);
    cursor: pointer;
  }
  .where-row {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    padding-left: 26px;
  }
  .where-row .input {
    width: 160px;
    flex: none;
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
  .moves {
    display: flex;
    flex-direction: column;
    gap: 2px;
    max-height: 30vh;
    overflow: auto;
    font-size: 12.5px;
  }
  .move {
    overflow-wrap: anywhere;
  }
  /* The table fits its card and scrolls down only (finding 10). The set-up
     panel is 460px wide at any window size; the old column minimums alone
     came to more than that, so a long path pushed it sideways. The source
     and kind columns now shrink to the card, the paths are the name's
     tooltip, and the kind chips wrap onto a second line. */
  .table {
    display: flex;
    flex-direction: column;
    overflow-x: hidden;
    overflow-y: auto;
    max-height: 46vh;
    font-size: 12.5px;
  }
  .tr {
    display: grid;
    grid-template-columns: 44px minmax(0, 1.8fr) minmax(88px, 1fr) 24px;
    gap: 8px;
    align-items: start;
    padding: 8px 12px;
    border-bottom: 1px solid var(--line-soft);
  }
  .tr:last-child {
    border-bottom: none;
  }
  .tr.head {
    position: sticky;
    top: 0;
    z-index: 1;
    background: var(--surface);
    border-bottom-color: var(--line);
  }
  .tr.dim {
    opacity: 0.55;
  }
  .description {
    width: 100%;
    margin-top: 4px;
    font-size: 12.5px;
  }
  .remove {
    width: 24px;
    height: 24px;
    padding: 0;
    border-radius: var(--radius-chip);
    color: var(--ink-3);
  }
  .remove:hover {
    background: var(--line-soft);
    color: var(--danger);
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
    color: var(--ink-3);
  }
  .kinds {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    min-width: 0;
  }
  /* The app's chip, as a toggle: on is the selection, in the accent. */
  .kinds .chip {
    cursor: pointer;
  }
  .kinds .chip:hover:not(.on) {
    color: var(--ink);
  }
  .kinds .chip.on {
    background: var(--accent-soft);
    color: var(--accent-ink);
  }
  .kinds .chip:focus-visible {
    outline: none;
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
</style>

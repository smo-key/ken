<script lang="ts">
  // The workspace-KG entity detail panel (federated-kg task 3.4): a
  // Karpathy-style wiki page — summary, out-links/back-links navigable
  // in-panel, and "mentioned in" doc pointers that focus-switch + open the
  // file in the owning member. Mounted by `MapScreen`'s Workspace mode
  // inside the same bottom-left `.detail` shell the per-project Map uses.
  import { app } from "../lib/app.svelte";
  import type { WorkspaceKgEntity, WorkspaceKgOverview } from "../lib/api";

  let {
    entity,
    loading,
    error,
    overview,
    onNavigate,
  }: {
    entity: WorkspaceKgEntity | null;
    loading: boolean;
    error: string | null;
    /** For friendly member names on pointers — only currently-open members
     *  have a name available (task 2.2's own enumeration limit). */
    overview: WorkspaceKgOverview | null;
    /** Follow an out-/back-link to another global entity, in-panel. */
    onNavigate: (id: number) => void;
  } = $props();

  function memberName(projectId: string): string {
    const known = overview?.members.find((m) => m.projectId === projectId);
    if (known) return known.name;
    // Not open this session — no name is available anywhere in the
    // workspace-KG payloads (task 2.1/2.2's own member-enumeration
    // deviation), so fall back to a short id rather than fabricate one.
    return `member ${projectId.slice(0, 8)}…`;
  }

  /** Only the focused project can be opened today — `app.members` is a
   *  single-project list until workspace mode grows a real multi-open UI
   *  (see `app.svelte.ts`). Follow-up: wire a real focus switch once that
   *  lands; until then this is an honest disabled state, not a fake one. */
  function isOpenMember(projectId: string): boolean {
    return app.project?.id === projectId;
  }

  function pointerTitle(p: { projectId: string; relPath: string; stale: boolean }): string {
    if (isOpenMember(p.projectId)) {
      return p.stale
        ? `${p.relPath} no longer exists in ${memberName(p.projectId)}.`
        : p.relPath;
    }
    return `Open ${memberName(p.projectId)} to view ${p.relPath} — switching focus to a different workspace member isn't wired up yet (only one project can be open at a time today).`;
  }

  function openPointer(p: { projectId: string; relPath: string; stale: boolean }) {
    if (!isOpenMember(p.projectId) || p.stale) return;
    app.openInFiles(p.relPath);
  }

  function chipLabel(path: string): string {
    return path.split("/").pop() ?? path;
  }
</script>

{#if loading}
  <div class="wiki-status">Loading…</div>
{:else if error}
  <div class="wiki-status error">Couldn't load this entity — {error}</div>
{:else if entity}
  <div class="wiki-head">
    <span class="tag kind-{entity.kind}" aria-hidden="true"></span>
    <span class="wiki-name">{entity.name}</span>
    <span class="wiki-kind kind-{entity.kind}">{entity.kind}</span>
  </div>
  {#if entity.summary}
    <p class="wiki-summary">{entity.summary}</p>
  {/if}

  {#if entity.outLinks.length > 0}
    <div class="wiki-section">
      <div class="wiki-label">Links to</div>
      <ul class="wiki-list">
        {#each entity.outLinks as link (link.id)}
          <li>
            <button class="wiki-link" onclick={() => onNavigate(link.otherId)}>
              <span class="wiki-link-name">{link.otherName}</span>
              <span class="wiki-link-meta">{link.relation}</span>
            </button>
          </li>
        {/each}
      </ul>
    </div>
  {/if}

  {#if entity.backLinks.length > 0}
    <div class="wiki-section">
      <div class="wiki-label">Linked from</div>
      <ul class="wiki-list">
        {#each entity.backLinks as link (link.id)}
          <li>
            <button class="wiki-link" onclick={() => onNavigate(link.otherId)}>
              <span class="wiki-link-name">{link.otherName}</span>
              <span class="wiki-link-meta">{link.relation}</span>
            </button>
          </li>
        {/each}
      </ul>
    </div>
  {/if}

  {#if entity.pointers.length > 0}
    <div class="wiki-section">
      <div class="wiki-label">Mentioned in</div>
      <div class="wiki-pointers">
        {#each entity.pointers as p (p.uri)}
          <button
            class="wiki-pointer"
            class:disabled={!isOpenMember(p.projectId) || p.stale}
            disabled={!isOpenMember(p.projectId) || p.stale}
            title={pointerTitle(p)}
            onclick={() => openPointer(p)}
          >
            {chipLabel(p.relPath)}
            {#if p.stale}<span class="wiki-pointer-flag">missing</span>{/if}
          </button>
        {/each}
      </div>
    </div>
  {/if}
{/if}

<style>
  .wiki-status {
    font-size: 12.5px;
    color: var(--ink-secondary);
  }
  .wiki-status.error {
    color: var(--danger);
  }
  .wiki-head {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .wiki-head .tag {
    flex: none;
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: var(--k);
  }
  .kind-person { --k: var(--accent); }
  .kind-organization { --k: var(--healthy); }
  .kind-topic { --k: var(--file-doc); }
  .kind-decision { --k: var(--needs-input); }
  .kind-other { --k: var(--ink-tertiary); }
  .wiki-name {
    font-family: var(--font-serif);
    font-size: 15px;
    font-weight: 500;
    color: var(--ink);
    flex: 1;
    min-width: 0;
  }
  .wiki-kind {
    flex: none;
    font-size: 10.5px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.03em;
    padding: 2px 7px;
    border-radius: 999px;
    color: color-mix(in srgb, var(--k) 75%, var(--ink));
    background: color-mix(in srgb, var(--k) 14%, transparent);
  }
  .wiki-summary {
    margin: 10px 0 0;
    font-size: 12.5px;
    line-height: 1.55;
    color: var(--ink-secondary);
  }
  .wiki-section {
    margin-top: 10px;
    display: flex;
    flex-direction: column;
    gap: 5px;
  }
  .wiki-label {
    font-size: 10.5px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--ink-tertiary);
  }
  .wiki-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .wiki-link {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 7px;
    width: 100%;
    padding: 3px 6px;
    border-radius: 6px;
    font-size: 12px;
    color: var(--ink-secondary);
    text-align: left;
  }
  .wiki-link:hover {
    background: var(--sunken);
    color: var(--ink);
  }
  .wiki-link-name {
    color: var(--ink);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .wiki-link-meta {
    flex: none;
    color: var(--ink-tertiary);
    font-size: 11px;
  }
  .wiki-pointers {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .wiki-pointer {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-size: 11px;
    font-family: var(--font-mono);
    color: var(--ink-secondary);
    border: 1px solid var(--border);
    border-radius: 5px;
    padding: 2px 7px;
    background: var(--paper);
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .wiki-pointer:hover:not(.disabled) {
    border-color: var(--border-strong);
    color: var(--ink);
  }
  .wiki-pointer.disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .wiki-pointer-flag {
    font-size: 9.5px;
    font-weight: 600;
    text-transform: uppercase;
    color: var(--danger);
  }
</style>

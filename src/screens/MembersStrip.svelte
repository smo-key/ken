<script lang="ts">
  // Home's members strip (ken-home-workspace 3.4): one row per MANIFEST
  // member, including the ones that no longer resolve.
  //
  // This is the only surface in the app that shows `missing`/`invalid`
  // members. `Workspace::open` deliberately doesn't fail on them, so
  // without this they are silently absent — which is exactly the failure
  // mode this change exists to fix.
  import { app } from "../lib/app.svelte";
  import { workspaceHome } from "../lib/workspaceHome.svelte";
  import { memberLeaf, type MemberOverview } from "../lib/api";

  let expanded = $state(false);

  const members = $derived(workspaceHome.members);
  const attention = $derived(workspaceHome.needsAttention);
  // Collapsed by default only when there are enough members for the full
  // list to swamp Home; a small workspace just shows everything.
  const showAll = $derived(expanded || !workspaceHome.shouldCollapse);
  const shown = $derived(showAll ? members : attention);

  function focus(m: MemberOverview) {
    if (m.status !== "ok" || !m.projectId) return;
    void app.focusMember(m.projectId);
  }

  /** The one-line explanation for a row, most-severe first. */
  function note(m: MemberOverview): string {
    if (m.status === "missing") return "folder not found";
    if (m.status === "invalid") return m.detail ?? "unreadable configuration";
    const parts: string[] = [];
    if (!m.indexReady) parts.push("index building");
    if (m.failedFiles > 0)
      parts.push(`${m.failedFiles} file${m.failedFiles === 1 ? "" : "s"} failed`);
    if (m.unread > 0) parts.push(`${m.unread} unread`);
    if (parts.length === 0)
      parts.push(`${m.fileCount} file${m.fileCount === 1 ? "" : "s"}`);
    return parts.join(" · ");
  }
</script>

{#if members.length > 0}
  <div class="overline">
    Projects
    {#if attention.length > 0}
      <span class="attention">{attention.length} need a look</span>
    {/if}
  </div>

  <div class="group">
    {#each shown as m (m.folder)}
      <button
        class="row"
        class:broken={m.status !== "ok"}
        class:inert={m.status !== "ok"}
        onclick={() => focus(m)}
        title={m.status === "ok" ? `Focus ${m.name}` : note(m)}
      >
        <span
          class="mdot"
          class:red={m.status !== "ok"}
          class:amber={m.status === "ok" && (!m.indexReady || m.failedFiles > 0)}
          class:dormant={m.status === "ok" && !m.resident}
        ></span>
        <span class="mname">{m.name ?? memberLeaf(m.folder)}</span>
        <span class="mnote">{note(m)}</span>
      </button>
    {/each}
  </div>

  {#if workspaceHome.shouldCollapse}
    <button class="more" onclick={() => (expanded = !expanded)}>
      {expanded ? "Show fewer" : `Show all ${members.length}`}
    </button>
  {/if}
{/if}

<style>
  .overline {
    display: flex;
    align-items: baseline;
    gap: 8px;
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--ink-tertiary);
    margin-bottom: 10px;
  }
  .attention {
    letter-spacing: 0;
    text-transform: none;
    font-weight: 500;
    color: var(--danger);
  }
  .group {
    display: flex;
    flex-direction: column;
  }
  .row {
    display: flex;
    align-items: baseline;
    gap: 10px;
    width: 100%;
    text-align: left;
    background: transparent;
    border: none;
    border-bottom: 1px solid var(--rule-line);
    padding: 8px 2px;
    font: inherit;
    font-size: 13px;
    cursor: pointer;
    color: var(--ink);
  }
  .row:last-child {
    border-bottom: none;
  }
  .row:hover {
    background: var(--surface);
  }
  .row.inert {
    cursor: default;
  }
  .row.inert:hover {
    background: transparent;
  }
  .mdot {
    flex: none;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--accent);
    transform: translateY(-1px);
  }
  /* A dormant member is healthy, just not loaded — hollow, not coloured. */
  .mdot.dormant {
    background: transparent;
    box-shadow: inset 0 0 0 1.5px var(--ink-tertiary);
  }
  .mdot.amber {
    background: var(--warning, #c98a00);
  }
  .mdot.red {
    background: var(--danger);
  }
  .mname {
    font-weight: 600;
    flex: none;
  }
  .broken .mname {
    color: var(--ink-tertiary);
    text-decoration: line-through;
  }
  .mnote {
    color: var(--ink-tertiary);
    font-size: 12px;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .more {
    margin-top: 8px;
    background: transparent;
    border: none;
    padding: 0;
    font: inherit;
    font-size: 12px;
    font-weight: 600;
    color: var(--ink-tertiary);
    cursor: pointer;
  }
  .more:hover {
    color: var(--ink-secondary);
  }
</style>

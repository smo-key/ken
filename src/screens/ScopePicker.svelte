<script lang="ts">
  // "What am I asking about?" — All projects, a group, or one project.
  //
  // This is a QUESTION scope, not a focus change: picking one project here
  // narrows search and chat context without moving which project Files or
  // Map are showing. That separation is deliberate — you often want to ask
  // about one repo while working in another.
  import { app } from "../lib/app.svelte";
  import { memberLeaf } from "../lib/api";
  import { scope } from "../lib/scope.svelte";

  const members = $derived(
    (app.workspace?.members ?? []).filter(
      (m) => m.projectId && (m.status === "active" || m.status === "dormant"),
    ),
  );

  // `bind:value` rather than a plain `value=` attribute: Svelte applies a
  // plain attribute before the `{#each}` options exist, so the control can
  // mount with nothing selected — the default "All projects" simply never
  // appears as chosen. Binding a local string and syncing both ways keeps
  // the store authoritative without that ordering trap.
  let sel = $state("all");

  $effect(() => {
    sel =
      scope.kind === "all"
        ? "all"
        : scope.kind === "group"
          ? `g:${scope.value}`
          : `p:${scope.value}`;
  });

  function onChange() {
    if (sel === "all") return scope.set("all", null);
    if (sel.startsWith("g:")) return scope.set("group", sel.slice(2));
    scope.set("project", sel.slice(2));
  }
</script>

{#if scope.enabled}
  <div class="picker">
    <span class="lead">Asking about</span>
    <select bind:value={sel} onchange={onChange} aria-label="Question scope">
      <option value="all">All projects</option>
      {#if scope.groups.length > 0}
        <optgroup label="Groups">
          {#each scope.groups as g (g.name)}
            <option value={`g:${g.name}`}>{g.name} ({g.projectIds.length})</option>
          {/each}
        </optgroup>
      {/if}
      <optgroup label="Projects">
        {#each members as m (m.projectId)}
          <option value={`p:${m.projectId}`}>{memberLeaf(m.name)}</option>
        {/each}
      </optgroup>
    </select>
  </div>
{/if}

<style>
  .picker {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 10px;
  }
  .lead {
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--ink-tertiary);
  }
  select {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-control, 6px);
    padding: 3px 8px;
    font: inherit;
    font-size: 13px;
    font-weight: 600;
    color: var(--ink);
    cursor: pointer;
    max-width: 260px;
  }
  select:hover {
    border-color: var(--accent);
  }
</style>

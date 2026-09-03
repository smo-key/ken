<script lang="ts">
  // The workspace digest (ken-home-workspace 3.2): every member's already
  // -stored digest for today, plus the board summary.
  //
  // Renders composed text only. Nothing here can trigger generation —
  // there is deliberately no "write it now" affordance, because that is
  // the per-project card's job and the per-project scheduler owns the
  // 07:00 gate and in-flight guard.
  import { app } from "../lib/app.svelte";
  import { renderMarkdown } from "../lib/markdown";
  import { workspaceHome } from "../lib/workspaceHome.svelte";

  const digest = $derived(workspaceHome.digest);

  function chipLabel(relPath: string): string {
    return relPath.split("/").pop() || relPath;
  }

  /** Open a member's source file: focus that member first, since paths
   *  are project-relative and Files reads the focused project. */
  async function openSource(projectId: string, relPath: string) {
    if (app.workspace?.focused !== projectId) {
      await app.focusMember(projectId);
    }
    app.openInFiles(relPath);
  }
</script>

{#if digest && (digest.hasContent || digest.awaiting.length > 0)}
  <!-- `hasContent` alone would render nothing until a member actually
       writes a digest, which reads as "the workspace layer isn't
       working". Naming the projects still waiting is honest and tells
       the user Home is looking at all of them. -->
  <div class="overline">Across your projects</div>

  {#each digest.members as m (m.projectId)}
    <div class="member">
      <button class="mname" onclick={() => void app.focusMember(m.projectId)}>
        {m.name}
      </button>
      <div class="body">{@html renderMarkdown(m.body)}</div>
      {#if m.sources.length > 0}
        <div class="sources">
          {#each m.sources as source (source)}
            <button
              class="chip mono"
              title={source}
              onclick={() => void openSource(m.projectId, source)}
            >
              {chipLabel(source)}
            </button>
          {/each}
        </div>
      {/if}
    </div>
  {/each}

  {#if digest.awaiting.length > 0}
    <p class="awaiting">
      No digest yet today for
      {digest.awaiting.map((a) => a.name).join(", ")}.
    </p>
  {/if}
{/if}

<style>
  .overline {
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--ink-tertiary);
    margin-bottom: 12px;
  }
  .member {
    margin-bottom: 16px;
  }
  .member:last-of-type {
    margin-bottom: 0;
  }
  .mname {
    background: transparent;
    border: none;
    padding: 0;
    font: inherit;
    font-size: 12px;
    font-weight: 600;
    color: var(--ink-secondary);
    cursor: pointer;
  }
  .mname:hover {
    color: var(--ink);
  }
  .body {
    font-size: 14px;
    line-height: 1.65;
    margin-top: 2px;
  }
  .sources {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: 6px;
  }
  .chip {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 2px 7px;
    font-size: 11px;
    color: var(--ink-secondary);
    cursor: pointer;
  }
  .chip:hover {
    color: var(--ink);
  }
  .mono {
    font-family: var(--mono, ui-monospace, monospace);
  }
  .awaiting {
    margin: 12px 0 0;
    font-size: 12px;
    color: var(--ink-tertiary);
  }
</style>

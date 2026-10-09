<script lang="ts">
  // The conflict view, opened from the banner at the top of Files: every
  // sync conflict and conflicted copy in the team's repos, one shown at a
  // time with its versions and the ways to settle it.
  import { app } from "../lib/app.svelte";
  import { conflicts } from "../lib/conflicts.svelte";
  import { memberLeaf } from "../lib/api";
  import { timeAgo } from "../lib/format";
  import ConflictDetail from "./ConflictDetail.svelte";

  const item = $derived(conflicts.selectedItem);

  function repoName(projectId: string | null | undefined): string {
    const id = projectId ?? app.focused;
    const m = app.workspace?.members.find((x) => x.projectId === id);
    return m ? memberLeaf(m.name) : (app.project?.name ?? "");
  }

  function fileName(path: string): string {
    return path.split("/").pop() || path;
  }
</script>

<div class="conflicts">
  <div class="head">
    <span class="overline">Sync conflicts · {conflicts.items.length}</span>
    <span class="sp"></span>
    <button class="btn btn-small btn-ghost" onclick={() => conflicts.close()}>Close</button>
  </div>

  {#if conflicts.items.length > 1}
    <div class="list">
      {#each conflicts.items as c (c.id)}
        <button class="row" class:current={item?.id === c.id} onclick={() => void conflicts.select(c.id)}>
          <span class="name">{fileName(c.sourceRef)}</span>
          <span class="sub">
            {c.kind === "conflict" ? "changed in two places" : "conflicted copy"} · {repoName(c.projectId)} · {timeAgo(c.when)}
          </span>
        </button>
      {/each}
    </div>
  {/if}

  {#if item}
    <div class="detail">
      <div class="dhead">
        <h3>{item.title}</h3>
        <span class="meta mono">{repoName(item.projectId)} · {item.sourceRef}</span>
      </div>
      {#if item.body}<p class="body">{item.body}</p>{/if}
      {#key item.id}
        <ConflictDetail {item} />
      {/key}
    </div>
  {:else}
    <p class="note">Nothing to resolve.</p>
  {/if}
</div>

<style>
  .conflicts {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 18px 24px 28px;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .sp {
    flex: 1;
  }
  .list {
    display: flex;
    flex-direction: column;
    gap: 2px;
    border: 1px solid var(--border);
    border-radius: var(--radius-card);
    background: var(--surface);
    padding: 4px;
  }
  .row {
    display: flex;
    flex-direction: column;
    gap: 1px;
    text-align: left;
    border: none;
    background: transparent;
    padding: 7px 10px;
    border-radius: 8px;
    color: var(--ink);
  }
  .row:hover {
    background: var(--sunken);
  }
  .row.current {
    background: color-mix(in srgb, var(--accent) 9%, transparent);
  }
  .name {
    font-size: 13px;
    font-weight: 600;
  }
  .sub {
    font-size: 11.5px;
    color: var(--ink-tertiary);
  }
  .detail {
    max-width: 760px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .dhead {
    display: flex;
    align-items: baseline;
    gap: 10px;
    flex-wrap: wrap;
  }
  h3 {
    margin: 0;
    font-family: var(--font-serif);
    font-size: 20px;
    font-weight: 500;
  }
  .meta {
    font-size: 11.5px;
    color: var(--ink-tertiary);
  }
  .body {
    margin: 0;
    font-size: 13px;
    line-height: 1.65;
    color: var(--ink-secondary);
  }
  .note {
    margin: 0;
    font-size: 12.5px;
    color: var(--ink-tertiary);
  }
</style>

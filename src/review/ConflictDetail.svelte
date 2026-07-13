<script lang="ts">
  // Conflict detail. Two shapes share this component:
  //  • merge conflict ("conflict") — two edits to the same document; shows the
  //    side-by-side "who changed what", Ken's drafted take, and an inline diff.
  //  • conflicted copy ("conflict-copy") — a shared drive saved a second file
  //    next to the original; shows a line diff of the two versions and lets the
  //    human keep one. No git vocabulary anywhere.
  import { app } from "../lib/app.svelte";
  import {
    review,
    conflictPayload,
    conflictCopyPayload,
    buildDiffRows,
    type DiffRow,
  } from "../lib/review.svelte";
  import { api, type InboxItem } from "../lib/api";

  let { item }: { item: InboxItem } = $props();

  const payload = $derived(conflictPayload(item));
  const copy = $derived(conflictCopyPayload(item));
  let busy = $state(false);

  function baseName(path: string): string {
    return path.split("/").pop() || path;
  }

  // --- merge conflict: diff the two edits (their text rides in the payload) ---
  const mergeRows = $derived<DiffRow[]>(
    payload ? buildDiffRows(payload.ours, payload.theirs) : [],
  );
  const mergeIdentical = $derived(
    payload ? payload.ours === payload.theirs : false,
  );

  // --- conflicted copy: load both files and diff them ---
  type CopyDiff =
    | { status: "loading" }
    | { status: "ready"; rows: DiffRow[]; identical: boolean }
    | { status: "unavailable"; note: string };
  let copyDiff = $state<CopyDiff>({ status: "loading" });

  function looksBinary(text: string): boolean {
    // A NUL byte is the cheapest reliable "this isn't text" signal.
    return text.includes("\u0000");
  }

  $effect(() => {
    const c = copy;
    if (!c) return;
    let cancelled = false;
    copyDiff = { status: "loading" };
    void (async () => {
      const next = await computeCopyDiff(c.originalPath, c.copyPath);
      if (!cancelled) copyDiff = next;
    })();
    return () => {
      cancelled = true;
    };
  });

  async function computeCopyDiff(
    originalPath: string | null,
    copyPath: string,
  ): Promise<CopyDiff> {
    if (!originalPath) {
      return {
        status: "unavailable",
        note: "The original file is gone, so there's nothing to compare against.",
      };
    }
    try {
      const [original, conflicting] = await Promise.all([
        api.readFile(originalPath),
        api.readFile(copyPath),
      ]);
      if (looksBinary(original) || looksBinary(conflicting)) {
        return {
          status: "unavailable",
          note: "These look like binary files, so a line-by-line comparison isn't possible. Open both to compare them yourself.",
        };
      }
      return {
        status: "ready",
        rows: buildDiffRows(original, conflicting),
        identical: original === conflicting,
      };
    } catch {
      return {
        status: "unavailable",
        note: "One of the files couldn't be read as text. Open both to compare them yourself.",
      };
    }
  }

  async function resolve(
    resolution: "accept-draft" | "keep-mine" | "take-theirs",
    thenEdit = false,
  ) {
    if (busy) return;
    busy = true;
    try {
      const path = await review.resolveConflict(item, resolution);
      if (thenEdit) app.openInFiles(path);
    } finally {
      busy = false;
    }
  }

  async function resolveCopy(resolution: "keep-copy" | "keep-original") {
    if (busy) return;
    busy = true;
    try {
      await review.resolveConflictCopy(item, resolution);
    } finally {
      busy = false;
    }
  }

  function openBoth() {
    app.openInFiles(copy?.originalPath ?? copy?.copyPath ?? item.sourceRef);
  }
</script>

{#snippet diffBox(rows: DiffRow[], leftLabel: string, rightLabel: string)}
  <div class="diff">
    <div class="diff-head">
      <span class="side del-side">− {leftLabel}</span>
      <span class="side add-side">+ {rightLabel}</span>
    </div>
    <div class="diff-body mono">
      {#each rows as row, i (i)}
        {#if row.type === "gap"}
          <div class="line gap">
            ⋯ {row.count} unchanged {row.count === 1 ? "line" : "lines"}
          </div>
        {:else}
          <div class="line {row.type}">
            <span class="gutter"
              >{row.type === "add" ? "+" : row.type === "del" ? "−" : " "}</span
            >
            <span class="text">{row.text === "" ? " " : row.text}</span>
          </div>
        {/if}
      {/each}
    </div>
  </div>
{/snippet}

<div class="conflict-card">
  {#if copy}
    <!-- Conflicted-copy variant: diff the two files, then choose one. -->
    {#if copyDiff.status === "loading"}
      <div class="soft drafting">Comparing the two versions…</div>
    {:else if copyDiff.status === "unavailable"}
      <div class="soft">{copyDiff.note}</div>
    {:else if copyDiff.identical}
      <div class="soft">
        The two files are identical — keeping either one loses nothing.
      </div>
    {:else}
      {@render diffBox(
        copyDiff.rows,
        copy.originalPath ? `Original · ${baseName(copy.originalPath)}` : "Original",
        `Conflicting copy · ${baseName(copy.copyPath)}`,
      )}
    {/if}

    <div class="actions">
      <button
        class="btn btn-primary"
        disabled={busy}
        onclick={() => void resolveCopy("keep-copy")}
      >
        Keep this copy
      </button>
      {#if copy.originalPath}
        <button
          class="btn"
          disabled={busy}
          onclick={() => void resolveCopy("keep-original")}
        >
          Keep the original
        </button>
      {/if}
      <button class="btn btn-ghost" disabled={busy} onclick={openBoth}>
        Open in Files
      </button>
    </div>
  {:else if payload}
    <div class="versions">
      <div class="version mine">
        <div class="version-head mine-head">On this computer</div>
        <div class="version-body">{payload.ours}</div>
      </div>
      <div class="version theirs">
        <div class="version-head theirs-head">From your teammate</div>
        <div class="version-body">{payload.theirs}</div>
      </div>
    </div>

    {#if !mergeIdentical}
      {@render diffBox(mergeRows, "On this computer", "From your teammate")}
    {/if}

    <div class="take">
      {#if payload.draftStatus === "ready" && payload.draft}
        <strong>Ken's take:</strong>
        <div class="take-body">{payload.draft}</div>
      {:else if payload.draftStatus === "failed"}
        <strong>Ken's take:</strong>
        <span class="soft">
          Ken couldn't draft a combined version this time — pick one of the
          versions, or edit the document yourself.
        </span>
      {:else}
        <strong>Ken's take:</strong>
        <span class="soft drafting">Ken is drafting a suggestion…</span>
      {/if}
    </div>

    <div class="actions">
      {#if payload.draftStatus === "ready" && payload.draft}
        <button
          class="btn btn-primary"
          disabled={busy}
          onclick={() => void resolve("accept-draft")}
        >
          Accept Ken's merge
        </button>
      {/if}
      <button class="btn" disabled={busy} onclick={() => void resolve("keep-mine")}>
        Keep mine
      </button>
      <button class="btn" disabled={busy} onclick={() => void resolve("take-theirs")}>
        Use theirs
      </button>
      <button
        class="btn btn-ghost"
        disabled={busy}
        onclick={() => void resolve("accept-draft", true)}
      >
        Edit manually
      </button>
    </div>
  {:else}
    <div class="soft">This conflict's details couldn't be loaded.</div>
  {/if}
</div>

<style>
  .conflict-card {
    background: var(--surface);
    border: 1px solid color-mix(in srgb, var(--danger) 30%, transparent);
    border-radius: var(--radius-card);
    padding: 18px 20px;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .versions {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
    gap: 10px;
    font-size: 12.5px;
    line-height: 1.6;
  }
  .version {
    border: 1px solid var(--sunken);
    border-radius: 9px;
    padding: 12px 14px;
    min-width: 0;
  }
  .version.mine {
    background: color-mix(in srgb, var(--danger) 4%, transparent);
  }
  .version.theirs {
    background: color-mix(in srgb, var(--healthy) 5%, transparent);
  }
  .version-head {
    font-weight: 700;
    margin-bottom: 4px;
    font-size: 11px;
    letter-spacing: 0.05em;
    text-transform: uppercase;
  }
  .mine-head {
    color: var(--danger);
  }
  .theirs-head {
    color: var(--healthy-text);
  }
  .version-body,
  .take-body {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    max-height: 220px;
    overflow-y: auto;
  }
  .take {
    font-size: 13px;
    line-height: 1.6;
    color: var(--ink-secondary);
    background: var(--sunken-2);
    border-radius: 9px;
    padding: 11px 14px;
  }
  .take strong {
    color: var(--ink);
  }
  .take-body {
    margin-top: 4px;
  }
  .soft {
    color: var(--ink-tertiary);
    font-size: 12.5px;
    line-height: 1.6;
  }
  .drafting {
    animation: drafting-pulse 1.4s ease-in-out infinite;
  }
  @keyframes drafting-pulse {
    50% {
      opacity: 0.45;
    }
  }
  .actions {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }

  /* Unified diff */
  .diff {
    border: 1px solid var(--sunken);
    border-radius: 9px;
    overflow: hidden;
    background: var(--surface);
  }
  .diff-head {
    display: flex;
    gap: 16px;
    flex-wrap: wrap;
    padding: 8px 12px;
    border-bottom: 1px solid var(--sunken);
    background: var(--sunken-2);
    font-size: 11px;
    font-weight: 700;
    letter-spacing: 0.03em;
  }
  .del-side {
    color: var(--danger);
  }
  .add-side {
    color: var(--healthy-text);
  }
  .diff-body {
    max-height: 320px;
    overflow: auto;
    font-size: 12px;
    line-height: 1.55;
    padding: 4px 0;
  }
  .line {
    display: flex;
    gap: 8px;
    padding: 0 12px;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .line .gutter {
    flex: none;
    width: 0.9em;
    text-align: center;
    color: var(--ink-tertiary);
    user-select: none;
  }
  .line .text {
    flex: 1;
    min-width: 0;
  }
  .line.del {
    background: color-mix(in srgb, var(--danger) 9%, transparent);
  }
  .line.del .gutter {
    color: var(--danger);
  }
  .line.add {
    background: color-mix(in srgb, var(--healthy) 10%, transparent);
  }
  .line.add .gutter {
    color: var(--healthy-text);
  }
  .line.gap {
    padding: 3px 12px;
    color: var(--ink-tertiary);
    font-size: 11px;
    background: var(--sunken-2);
  }
</style>

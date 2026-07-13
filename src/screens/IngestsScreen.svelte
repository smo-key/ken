<script lang="ts">
  import { onMount } from "svelte";
  import { app } from "../lib/app.svelte";
  import { ingests, statusCaption } from "../lib/ingests.svelte";
  import { timeAgo } from "../lib/format";
  import IngestForm from "../ingests/IngestForm.svelte";
  import TemplateGallery from "../ingests/TemplateGallery.svelte";
  import type { IngestTemplate } from "../lib/templates";
  import type { IngestMode, IngestRefresh, RunRow } from "../lib/api";
  import Pencil from "@lucide/svelte/icons/pencil";
  import Play from "@lucide/svelte/icons/play";
  import Trash2 from "@lucide/svelte/icons/trash-2";
  import ContextMenu, { openContextMenu } from "../lib/ui/ContextMenu.svelte";
  import ConfirmMenu, { openConfirm } from "../lib/ui/ConfirmMenu.svelte";

  type FormPreset = {
    name: string;
    description: string;
    instruction: string;
    output: string;
    mode: IngestMode;
    refresh: IngestRefresh;
  };

  let formOpen = $state(false);
  let editingSlug = $state<string | null>(null);
  let formPreset = $state<FormPreset | null>(null);
  let galleryOpen = $state(false);

  onMount(() => void ingests.init());

  const detail = $derived(ingests.detail);
  const liveForSelected = $derived(
    ingests.selected ? ingests.liveStatus(ingests.selected) : null,
  );
  const isRunning = $derived(
    liveForSelected === "running" || liveForSelected === "blocked",
  );

  function toneColor(tone: string): string {
    switch (tone) {
      case "healthy": return "var(--healthy)";
      case "busy": return "var(--accent)";
      case "attention": return "var(--needs-input)";
      case "danger": return "var(--danger)";
      default: return "var(--ink-tertiary)";
    }
  }

  function dotFor(run: RunRow): string {
    switch (run.status) {
      case "fresh": return "var(--healthy)";
      case "running": return "var(--accent)";
      case "pending_approval":
      case "blocked": return "var(--needs-input)";
      case "failed": return "var(--danger)";
      default: return "var(--ink-tertiary)";
    }
  }

  function runLine(run: RunRow): string {
    const when = timeAgo(run.startedAt);
    switch (run.status) {
      case "running": return `${when} — running…`;
      case "pending_approval": return `${when} — ${run.summary ?? "waiting for your approval"}`;
      case "fresh": return `${when} — ${run.summary ?? "completed"}`;
      case "failed": return `${when} — failed`;
      case "cancelled": return `${when} — cancelled`;
      case "discarded": return `${when} — discarded`;
      default: return `${when} — ${run.status}`;
    }
  }

  function openEdit(slug: string) {
    editingSlug = slug;
    formPreset = null;
    formOpen = true;
  }

  function openNew() {
    editingSlug = null;
    formPreset = null;
    formOpen = true;
  }

  function rowMenu(e: MouseEvent, slug: string, name: string) {
    e.preventDefault();
    const x = e.clientX;
    const y = e.clientY;
    openContextMenu(x, y, [
      { label: "Edit", icon: Pencil, onSelect: () => openEdit(slug) },
      { label: "Run now", icon: Play, onSelect: () => void ingests.run(slug) },
      "separator",
      {
        label: "Delete…",
        icon: Trash2,
        danger: true,
        onSelect: () =>
          openConfirm(x, y, {
            title: `Delete “${name}”?`,
            body: "Removes this ingest and its recipe from Ken. The documents it already produced stay on disk.",
            confirmLabel: "Delete ingest",
            onConfirm: () => void ingests.delete(slug),
          }),
      },
    ]);
  }

  function openFromTemplate(t: IngestTemplate) {
    galleryOpen = false;
    editingSlug = null;
    formPreset = {
      name: t.name,
      description: t.description,
      instruction: t.instruction,
      output: t.output,
      mode: t.mode,
      refresh: t.refresh,
    };
    formOpen = true;
  }
</script>

<div class="screen">
  <!-- list pane -->
  <div class="list">
    <div class="list-head">
      Ingests
      <button class="plus" title="New ingest" onclick={openNew}>+</button>
    </div>

    {#if ingests.doctor && !ingests.doctor.found}
      <div class="setup-note">
        <strong>Claude Code isn't set up yet.</strong>
        Ingests need it to run — install with
        <span class="mono">npm i -g @anthropic-ai/claude-code</span>, then log
        in once with <span class="mono">claude</span>. Browsing and creating
        ingests works meanwhile.
      </div>
    {/if}

    {#each ingests.summaries as s (s.entry.kind === "ok" ? s.entry.recipe.slug : s.entry.error.slug)}
      {@const slug = s.entry.kind === "ok" ? s.entry.recipe.slug : s.entry.error.slug}
      {@const name = s.entry.kind === "ok" ? s.entry.recipe.name : s.entry.error.slug}
      {@const cap = statusCaption(s, ingests.liveStatus(slug))}
      <button
        class="row"
        class:active={ingests.selected === slug}
        onclick={() => ingests.select(slug)}
        oncontextmenu={(e) => rowMenu(e, slug, name)}
      >
        <span class="row-top">
          {name}
          <span class="dot" style:background={toneColor(cap.tone)}></span>
        </span>
        <span class="row-caption">{cap.label}</span>
      </button>
    {/each}

    {#if ingests.summaries.length === 0}
      <div class="empty-list">
        No ingests yet. An ingest turns your raw files into a living document —
        start from a template.
      </div>
    {/if}

    <div class="list-foot">
      <button class="foot-link accent" onclick={openNew}>+ New ingest</button>
      <button class="foot-link" onclick={() => (galleryOpen = true)}>Browse templates</button>
    </div>
  </div>

  <!-- detail pane -->
  <div class="detail">
    {#if detail}
      {@const broken = false}
      <div class="detail-inner">
        <div class="detail-head">
          <h1>{detail.recipe.name}</h1>
          {#if liveForSelected === "blocked"}
            <span class="badge attention">blocked on you</span>
          {:else if liveForSelected === "running"}
            <span class="badge busy">running…</span>
          {/if}
          <span class="spacer"></span>
          <button class="btn btn-small" onclick={() => openEdit(detail.recipe.slug)}>
            Edit
          </button>
          {#if isRunning}
            <button class="btn btn-small" onclick={() => ingests.cancel(detail.recipe.slug)}>
              Cancel run
            </button>
          {:else}
            <button class="btn btn-small btn-primary" onclick={() => ingests.run(detail.recipe.slug)}>
              Run now
            </button>
          {/if}
        </div>

        {#if detail.recipe.description}
          <p class="desc">{detail.recipe.description}</p>
        {/if}

        <div class="card">
          <div class="card-label">Sources</div>
          <div class="chips">
            {#if detail.recipe.sources.length === 0}
              <span class="chip">all folders</span>
            {:else}
              {#each detail.recipe.sources as src (src)}
                <span class="chip mono">{src}/</span>
              {/each}
            {/if}
          </div>
          <div class="card-note">
            {detail.recipe.refresh === "on-change"
              ? "Refresh runs automatically when these change."
              : "Manual refresh only — use Run now."}
          </div>
        </div>

        <div class="card">
          <div class="card-label">Instruction</div>
          <div class="instruction">{detail.recipe.instruction}</div>
        </div>

        <div class="card">
          <div class="card-label">Output</div>
          <div class="output-row">
            <span class="ken-glyph"></span>
            <span class="mono">{detail.recipe.output}</span>
            <span class="card-note-inline">
              {detail.recipe.mode === "single"
                ? "· one document, many entries"
                : "· one document per entity"}
            </span>
          </div>
        </div>

        <div class="card">
          <div class="card-label">Rules</div>
          <div class="card-note">
            {detail.recipe.rules
              ? `Overridden — human edits win · review over ${detail.resolvedRules.reviewThresholdPct}% diff · stale check every ${detail.resolvedRules.staleDays} days.`
              : `Inherits defaults — human edits win · review over ${detail.resolvedRules.reviewThresholdPct}% diff · stale check every ${detail.resolvedRules.staleDays} days.`}
          </div>
        </div>

        <div class="card runs">
          <div class="card-label">Recent runs</div>
          {#if detail.runs.length === 0}
            <div class="card-note">No runs yet.</div>
          {/if}
          {#each detail.runs as run (run.id)}
            <div class="run-row">
              <span class="dot" style:background={dotFor(run)}></span>
              <span class="run-text">
                {runLine(run)}
                {#if run.error}
                  <span class="run-error">{run.error}</span>
                {/if}
              </span>
              {#if run.status === "pending_approval"}
                <button class="btn btn-small btn-primary" onclick={() => ingests.approve(run.id)}>Approve</button>
                <button class="btn btn-small" onclick={() => ingests.discard(run.id)}>Discard</button>
              {/if}
            </div>
          {/each}
        </div>
      </div>
    {:else}
      <div class="empty">
        <p>Select an ingest, or create one.</p>
        <p class="hint">
          Ingests keep structured documents — a people directory, a decision
          log — fresh as your files change.
        </p>
        <button class="btn btn-primary" onclick={() => (galleryOpen = true)}>Browse templates</button>
      </div>
    {/if}
  </div>
</div>

{#if formOpen}
  <IngestForm
    slug={editingSlug}
    preset={formPreset ?? undefined}
    close={(saved) => {
      formOpen = false;
      formPreset = null;
      void ingests.refresh();
      if (saved) void ingests.select(saved);
    }}
  />
{/if}

{#if galleryOpen}
  <TemplateGallery
    pick={openFromTemplate}
    close={() => (galleryOpen = false)}
  />
{/if}

<ContextMenu />
<ConfirmMenu />

<style>
  .screen {
    flex: 1;
    min-width: 0;
    display: flex;
    min-height: 0;
  }
  .list {
    flex: 0 1 272px;
    min-width: 220px;
    border-right: 1px solid var(--border);
    background: var(--sunken-2);
    display: flex;
    flex-direction: column;
    overflow-y: auto;
    padding: 12px 8px;
  }
  .list-head {
    display: flex;
    align-items: center;
    padding: 2px 10px 8px;
    font-size: 11px;
    font-weight: 700;
    color: var(--ink-tertiary);
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }
  .plus {
    margin-left: auto;
    font-size: 14px;
    color: var(--accent);
    font-weight: 600;
    border: none;
    background: none;
  }
  .setup-note {
    margin: 4px 6px 10px;
    padding: 10px 12px;
    font-size: 12px;
    line-height: 1.55;
    background: color-mix(in srgb, var(--needs-input) 8%, transparent);
    border: 1px solid color-mix(in srgb, var(--needs-input) 28%, transparent);
    border-radius: 9px;
    color: var(--ink-secondary);
  }
  .setup-note .mono {
    font-size: 11px;
  }
  .row {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 9px 10px;
    border-radius: 8px;
    border: none;
    background: transparent;
    text-align: left;
    width: 100%;
  }
  .row:hover {
    background: var(--sunken);
  }
  .row.active {
    background: color-mix(in srgb, var(--accent) 10%, transparent);
  }
  .row.active .row-top {
    color: var(--accent-deep);
  }
  .row-top {
    display: flex;
    align-items: center;
    gap: 7px;
    font-size: 12.5px;
    font-weight: 600;
    color: var(--ink);
  }
  .row-top .dot {
    margin-left: auto;
  }
  .dot {
    width: 6px;
    height: 6px;
    border-radius: 3px;
    flex: none;
  }
  .row-caption {
    font-size: 11.5px;
    color: var(--ink-tertiary);
  }
  .empty-list {
    padding: 10px;
    font-size: 12px;
    color: var(--ink-tertiary);
    line-height: 1.6;
  }
  .list-foot {
    margin-top: auto;
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding-top: 12px;
  }
  .foot-link {
    padding: 7px 10px;
    border-radius: 8px;
    font-size: 12.5px;
    color: var(--ink-secondary);
    border: none;
    background: none;
    text-align: left;
  }
  .foot-link:hover {
    background: var(--sunken);
  }
  .foot-link.accent {
    color: var(--accent);
    font-weight: 600;
  }

  .detail {
    flex: 1;
    min-width: 0;
    overflow-y: auto;
    padding: 32px 40px;
  }
  .detail-inner {
    max-width: 680px;
    margin: 0 auto;
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .detail-head {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
  }
  h1 {
    margin: 0;
    font-family: var(--font-serif);
    font-size: 24px;
    font-weight: 500;
  }
  .badge {
    display: inline-flex;
    align-items: center;
    height: 22px;
    padding: 0 9px;
    border-radius: 11px;
    font-size: 11.5px;
    font-weight: 600;
  }
  .badge.attention {
    background: color-mix(in srgb, var(--needs-input) 12%, transparent);
    color: var(--needs-input-text);
  }
  .badge.busy {
    background: color-mix(in srgb, var(--accent) 10%, transparent);
    color: var(--accent);
  }
  .spacer {
    flex: 1;
  }
  .desc {
    margin: -6px 0 0;
    font-size: 13px;
    color: var(--ink-secondary);
  }
  .card {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-card);
    padding: 18px 20px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .card-label {
    display: flex;
    align-items: center;
    font-size: 11px;
    font-weight: 700;
    color: var(--ink-tertiary);
    letter-spacing: 0.07em;
    text-transform: uppercase;
  }
  .chips {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    font-size: 12px;
    border: 1px solid var(--border);
    border-radius: 7px;
    padding: 5px 11px;
    background: var(--paper);
  }
  .card-note {
    font-size: 11.5px;
    color: var(--ink-tertiary);
    line-height: 1.6;
  }
  .card-note-inline {
    color: var(--ink-tertiary);
    font-size: 12px;
  }
  .instruction {
    font-size: 13.5px;
    line-height: 1.7;
    background: var(--paper);
    border-radius: 9px;
    padding: 12px 15px;
    white-space: pre-wrap;
  }
  .output-row {
    display: flex;
    align-items: center;
    gap: 9px;
    font-size: 13px;
  }
  .ken-glyph {
    width: 16px;
    height: 20px;
    flex: none;
    border-radius: 2px 5px 2px 2px;
    background: var(--ink);
  }
  .runs .run-row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 0;
    border-top: 1px solid var(--sunken);
    font-size: 12.5px;
  }
  .run-text {
    flex: 1;
    min-width: 0;
    line-height: 1.5;
  }
  .run-error {
    display: block;
    color: var(--danger);
    font-size: 11.5px;
    white-space: pre-wrap;
    max-height: 80px;
    overflow-y: auto;
  }
  .empty {
    height: 100%;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 8px;
  }
  .empty p {
    margin: 0;
    color: var(--ink-secondary);
    font-size: 14px;
  }
  .empty .hint {
    color: var(--ink-tertiary);
    font-size: 12.5px;
    max-width: 380px;
    text-align: center;
    line-height: 1.6;
  }
</style>

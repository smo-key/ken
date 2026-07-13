<script lang="ts">
  import { onMount } from "svelte";
  import { api, type IngestMode, type IngestRefresh } from "../lib/api";
  import { app } from "../lib/app.svelte";

  let {
    slug,
    close,
    preset,
  }: {
    slug: string | null;
    close: (savedSlug: string | null) => void;
    preset?: {
      name: string;
      description: string;
      instruction: string;
      output: string;
      mode: IngestMode;
      refresh: IngestRefresh;
    };
  } = $props();

  let name = $state(preset?.name ?? "");
  let description = $state(preset?.description ?? "");
  let instruction = $state(preset?.instruction ?? "");
  let output = $state(preset?.output ?? "knowledge/");
  let mode = $state<IngestMode>(preset?.mode ?? "single");
  let refresh = $state<IngestRefresh>(preset?.refresh ?? "on-change");
  let selectedSources = $state<string[]>([]);
  let error = $state<string | null>(null);
  let loading = $state(slug !== null);

  const topFolders = $derived(
    app.folders.filter((f) => !f.relPath.includes("/") && !f.excluded),
  );

  onMount(async () => {
    if (slug) {
      try {
        const detail = await api.getIngest(slug);
        name = detail.recipe.name;
        description = detail.recipe.description;
        instruction = detail.recipe.instruction;
        output = detail.recipe.output;
        mode = detail.recipe.mode;
        refresh = detail.recipe.refresh;
        selectedSources = detail.recipe.sources;
      } catch (e) {
        error = String(e);
      }
    }
    loading = false;
  });

  function toggleSource(rel: string) {
    selectedSources = selectedSources.includes(rel)
      ? selectedSources.filter((s) => s !== rel)
      : [...selectedSources, rel];
  }

  async function save() {
    error = null;
    try {
      const recipe = await api.saveIngest({
        slug: slug ?? undefined,
        name,
        description,
        instruction,
        sources: selectedSources,
        output,
        mode,
        refresh,
      });
      close(recipe.slug);
    } catch (e) {
      error = String(e);
    }
  }
</script>

<button class="scrim" onclick={() => close(null)} aria-label="Close"></button>
<div class="modal" role="dialog" aria-label={slug ? "Edit ingest" : "New ingest"}>
  <h2>{slug ? "Edit ingest" : "New ingest"}</h2>

  {#if loading}
    <p class="note">Loading…</p>
  {:else}
    <label>
      Name
      <input bind:value={name} placeholder="People" />
    </label>

    <label>
      What should Ken extract? <span class="soft">Plain words are perfect.</span>
      <textarea
        bind:value={instruction}
        rows="5"
        placeholder="Extract every person mentioned across the sources. For each: name, role, what they own…"
      ></textarea>
    </label>

    <div class="field">
      <span class="field-label">Read from <span class="soft">(default: everything)</span></span>
      <div class="chips">
        {#each topFolders as folder (folder.relPath)}
          <button
            class="chip"
            class:on={selectedSources.includes(folder.relPath)}
            onclick={() => toggleSource(folder.relPath)}
          >
            {selectedSources.includes(folder.relPath) ? "✓ " : ""}{folder.relPath}/
          </button>
        {/each}
        {#if topFolders.length === 0}
          <span class="note">No subfolders — Ken reads the whole project.</span>
        {/if}
      </div>
    </div>

    <div class="two-col">
      <label>
        Result
        <div class="seg">
          <button class:on={mode === "single"} onclick={() => (mode = "single")}>
            One document
          </button>
          <button class:on={mode === "collection"} onclick={() => (mode = "collection")}>
            One per entry
          </button>
        </div>
      </label>
      <label>
        Keep fresh
        <div class="seg">
          <button class:on={refresh === "on-change"} onclick={() => (refresh = "on-change")}>
            Automatically
          </button>
          <button class:on={refresh === "manual"} onclick={() => (refresh = "manual")}>
            Only when I ask
          </button>
        </div>
      </label>
    </div>

    <label>
      Save the result to
      <input bind:value={output} class="mono-input" placeholder={mode === "single" ? "knowledge/People.md" : "people/"} />
      <span class="soft small">
        {mode === "single" ? "A file path inside the project." : "A folder — Ken keeps one document per entry inside it."}
      </span>
    </label>

    {#if error}
      <div class="error">{error}</div>
    {/if}

    <div class="actions">
      <button class="btn btn-primary" onclick={save}>
        {slug ? "Save changes" : "Create ingest"}
      </button>
      <button class="btn btn-ghost" onclick={() => close(null)}>Cancel</button>
    </div>
  {/if}
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: var(--scrim);
    border: none;
    z-index: 60;
  }
  .modal {
    position: fixed;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    width: min(560px, calc(100vw - 80px));
    max-height: calc(100vh - 120px);
    overflow-y: auto;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-overlay);
    box-shadow: var(--shadow-overlay);
    padding: 24px 26px;
    z-index: 61;
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  h2 {
    margin: 0;
    font-family: var(--font-serif);
    font-size: 22px;
    font-weight: 500;
  }
  label,
  .field {
    display: flex;
    flex-direction: column;
    gap: 6px;
    font-size: 12px;
    font-weight: 600;
    color: var(--ink-secondary);
  }
  .field-label {
    font-size: 12px;
    font-weight: 600;
  }
  .soft {
    font-weight: 400;
    color: var(--ink-tertiary);
  }
  .small {
    font-size: 11.5px;
  }
  input,
  textarea {
    font-family: inherit;
    font-size: 13.5px;
    color: var(--ink);
    border: 1px solid var(--border-strong);
    border-radius: 8px;
    background: var(--surface);
    padding: 8px 12px;
    outline: none;
    resize: vertical;
  }
  .mono-input {
    font-family: var(--font-mono);
    font-size: 12.5px;
  }
  input:focus,
  textarea:focus {
    border-color: var(--accent);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 12%, transparent);
  }
  .chips {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
  .chip {
    font-size: 12px;
    font-family: var(--font-mono);
    border: 1px solid var(--border-strong);
    border-radius: 7px;
    padding: 5px 11px;
    background: var(--paper);
    color: var(--ink-secondary);
  }
  .chip.on {
    border-color: color-mix(in srgb, var(--accent) 50%, transparent);
    background: color-mix(in srgb, var(--accent) 8%, transparent);
    color: var(--accent-deep);
    font-weight: 600;
  }
  .two-col {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 14px;
  }
  .seg {
    display: flex;
    border: 1px solid var(--border-strong);
    border-radius: 8px;
    overflow: hidden;
  }
  .seg button {
    flex: 1;
    font-size: 12px;
    font-weight: 500;
    padding: 7px 4px;
    border: none;
    background: var(--surface);
    color: var(--ink-secondary);
  }
  .seg button.on {
    background: color-mix(in srgb, var(--accent) 10%, transparent);
    color: var(--accent-deep);
    font-weight: 600;
  }
  .error {
    font-size: 12.5px;
    color: var(--danger);
    background: color-mix(in srgb, var(--danger) 7%, transparent);
    border: 1px solid color-mix(in srgb, var(--danger) 25%, transparent);
    border-radius: 9px;
    padding: 9px 12px;
  }
  .note {
    font-size: 12px;
    color: var(--ink-tertiary);
    font-weight: 400;
  }
  .actions {
    display: flex;
    gap: 8px;
    margin-top: 4px;
  }
</style>

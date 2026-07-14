<script lang="ts">
  import { onMount } from "svelte";
  import { open as openDialog } from "@tauri-apps/plugin-dialog";
  import { api, type IngestMode, type IngestRefresh } from "../lib/api";
  import { app } from "../lib/app.svelte";
  import { composeSingleOutput, toProjectRelative } from "../lib/projectPath";

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

  // Recipes persist `output` as one string; the form edits it as a folder plus
  // (single mode only) a bare document name, since a folder dialog can't name a
  // file. Splitting on load keeps the two editors and the stored value in sync.
  function decompose(out: string, m: IngestMode): [folder: string, name: string] {
    const norm = out.replace(/\/+$/, "");
    if (m === "collection") return [norm, ""];
    const i = norm.lastIndexOf("/");
    return i >= 0 ? [norm.slice(0, i), norm.slice(i + 1)] : ["", norm];
  }

  const [seedFolder, seedName] = decompose(
    preset?.output ?? "",
    preset?.mode ?? "single",
  );

  let name = $state(preset?.name ?? "");
  let description = $state(preset?.description ?? "");
  let instruction = $state(preset?.instruction ?? "");
  let mode = $state<IngestMode>(preset?.mode ?? "single");
  let refresh = $state<IngestRefresh>(preset?.refresh ?? "on-change");
  let selectedSources = $state<string[]>([]);
  let outputFolder = $state(seedFolder); // project-relative folder from the dialog
  let outputName = $state(seedName); // single-mode document filename
  let error = $state<string | null>(null);
  let loading = $state(slug !== null);

  // Top-level folders offered as one-click "quick add" shortcuts; the dialog
  // remains the way to reach any nested folder.
  const topFolders = $derived(
    app.folders.filter((f) => !f.relPath.includes("/") && !f.excluded),
  );
  // Top folders not already chosen, offered as quick-add shortcuts.
  const sourceSuggestions = $derived(
    topFolders.filter((f) => !selectedSources.includes(f.relPath)),
  );

  onMount(async () => {
    if (slug) {
      try {
        const detail = await api.getIngest(slug);
        name = detail.recipe.name;
        description = detail.recipe.description;
        instruction = detail.recipe.instruction;
        mode = detail.recipe.mode;
        refresh = detail.recipe.refresh;
        selectedSources = detail.recipe.sources;
        [outputFolder, outputName] = decompose(
          detail.recipe.output,
          detail.recipe.mode,
        );
      } catch (e) {
        error = String(e);
      }
    }
    loading = false;
  });

  function addSource(rel: string) {
    if (!selectedSources.includes(rel)) selectedSources = [...selectedSources, rel];
  }

  function removeSource(rel: string) {
    selectedSources = selectedSources.filter((s) => s !== rel);
  }

  // Open the OS dialog constrained to the project, then hand back a
  // project-relative path — or null after setting an inline error.
  async function pickInsideProject(): Promise<string | null> {
    const root = app.project?.root;
    if (!root) {
      error = "No project is open.";
      return null;
    }
    const chosen = await openDialog({ directory: true, defaultPath: root });
    if (typeof chosen !== "string") return null; // dialog cancelled
    const r = toProjectRelative(root, chosen);
    if (!r.ok) {
      error = r.error;
      return null;
    }
    return r.rel;
  }

  async function addSourceFolder() {
    error = null;
    const rel = await pickInsideProject();
    if (rel === null) return;
    if (rel === "") {
      error = "That's the whole project — leave sources empty to read everything.";
      return;
    }
    addSource(rel);
  }

  async function chooseOutputFolder() {
    error = null;
    const rel = await pickInsideProject();
    if (rel === null) return;
    outputFolder = rel;
  }

  async function save() {
    error = null;
    let output: string;
    if (mode === "collection") {
      if (!outputFolder) {
        error = "Choose a folder for the documents.";
        return;
      }
      output = `${outputFolder}/`;
    } else {
      const r = composeSingleOutput(outputFolder, outputName);
      if (!r.ok) {
        error = r.error;
        return;
      }
      output = r.rel;
    }
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
        {#each selectedSources as src (src)}
          <span class="chip on">
            {src}/
            <button
              class="chip-x"
              onclick={() => removeSource(src)}
              aria-label={`Remove ${src}`}
            >✕</button>
          </span>
        {/each}
        <button class="chip add" onclick={addSourceFolder}>+ Add folder…</button>
      </div>
      {#if sourceSuggestions.length > 0}
        <div class="quick">
          <span class="soft small">Quick add:</span>
          {#each sourceSuggestions as folder (folder.relPath)}
            <button class="chip quick-chip" onclick={() => addSource(folder.relPath)}>
              {folder.relPath}/
            </button>
          {/each}
        </div>
      {/if}
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

    <div class="field">
      <span class="field-label">Save the result to</span>
      <div class="output-row">
        <button class="btn btn-small" onclick={chooseOutputFolder}>Choose folder…</button>
        <span class="mono folder-shown" class:placeholder={!outputFolder}>
          {outputFolder ? `${outputFolder}/` : mode === "single" ? "project root /" : "no folder yet"}
        </span>
        {#if mode === "single"}
          <input class="mono-input name-input" bind:value={outputName} placeholder="People.md" />
        {/if}
      </div>
      <span class="soft small">
        {mode === "single"
          ? "Pick a folder inside the project, then name the document (e.g. People.md)."
          : "Pick a folder inside the project — Ken keeps one document per entry inside it."}
      </span>
    </div>

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
    gap: 6px;
  }
  .chip-x {
    border: none;
    background: none;
    color: inherit;
    font-size: 11px;
    line-height: 1;
    padding: 0;
    opacity: 0.6;
  }
  .chip-x:hover {
    opacity: 1;
  }
  .chip.add {
    color: var(--accent);
    border-style: dashed;
    font-weight: 600;
  }
  .quick {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
  }
  .quick-chip {
    padding: 3px 9px;
    font-size: 11.5px;
  }
  .output-row {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
  .folder-shown {
    font-size: 12.5px;
    color: var(--ink-secondary);
  }
  .folder-shown.placeholder {
    color: var(--ink-tertiary);
  }
  .name-input {
    flex: 1;
    min-width: 120px;
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

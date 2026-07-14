<script lang="ts">
  import { onMount } from "svelte";
  import { api, type Automation } from "../lib/api";

  let { slug = null, close }: { slug: string | null; close: (saved?: string) => void } =
    $props();

  let name = $state("");
  let globsText = $state("");
  let prompt = $state("");
  let autoApply = $state(false);
  let enabled = $state(true);
  let error = $state<string | null>(null);
  let loading = $state(slug !== null);

  onMount(async () => {
    if (slug) {
      const d = await api.getAutomation(slug).catch(() => null);
      if (d) {
        const a: Automation = d.automation;
        name = a.name;
        globsText = a.globs.join("\n");
        prompt = a.prompt;
        autoApply = a.autoApply;
        enabled = a.enabled;
      }
    }
    loading = false;
  });

  async function save() {
    error = null;
    const globs = globsText
      .split("\n")
      .map((g) => g.trim())
      .filter(Boolean);
    try {
      const saved = await api.saveAutomation({
        slug: slug ?? undefined,
        name,
        globs,
        prompt,
        autoApply,
        enabled,
      });
      close(saved.slug);
    } catch (e) {
      error = String(e);
    }
  }
</script>

<button class="scrim" onclick={() => close()} aria-label="Close"></button>
<div class="modal" role="dialog" aria-label={slug ? "Edit automation" : "New automation"}>
  <h2>{slug ? "Edit automation" : "New automation"}</h2>

  {#if loading}
    <p class="note">Loading…</p>
  {:else}
    <label>
      Name
      <input bind:value={name} placeholder="Weekly Jira from recordings" />
    </label>

    <label>
      Watch these files
      <textarea bind:value={globsText} rows="3" placeholder="Recordings/*.md"></textarea>
      <span class="soft small">
        One pattern per line. <span class="mono">*</span> matches within a folder,
        <span class="mono">**</span> across folders.
      </span>
    </label>

    <label>
      What Ken should do
      <textarea
        bind:value={prompt}
        rows="6"
        placeholder="Summarize each recording and propose Jira tasks for the follow-ups."
      ></textarea>
    </label>

    <div class="toggle-row">
      <div class="toggle-copy">
        <div class="toggle-label">Act on its own</div>
        <div class="soft small">
          Off (recommended): Ken writes a plan and waits for your approval before
          doing anything outside your files. On: Ken carries out actions directly.
        </div>
      </div>
      <div class="seg" role="group" aria-label="Act on its own">
        <button class:on={!autoApply} onclick={() => (autoApply = false)}>Ask first</button>
        <button class:on={autoApply} onclick={() => (autoApply = true)}>Automatic</button>
      </div>
    </div>

    <p class="limitation">
      Phase 1 is <em>asked</em> not to act outside your files — but that restraint
      is only a request. The real safety is the review step: nothing happens
      outside your project until you approve.
    </p>

    <label class="check">
      <input type="checkbox" bind:checked={enabled} /> Enabled
    </label>

    {#if error}
      <div class="error">{error}</div>
    {/if}

    <div class="actions">
      <button class="btn btn-primary" onclick={save}>
        {slug ? "Save changes" : "Create automation"}
      </button>
      <button class="btn btn-ghost" onclick={() => close()}>Cancel</button>
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
  label {
    display: flex;
    flex-direction: column;
    gap: 6px;
    font-size: 12px;
    font-weight: 600;
    color: var(--ink-secondary);
  }
  .soft {
    font-weight: 400;
    color: var(--ink-tertiary);
  }
  .small {
    font-size: 11.5px;
    line-height: 1.55;
  }
  .mono {
    font-family: var(--font-mono);
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
  input:focus,
  textarea:focus {
    border-color: var(--accent);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 12%, transparent);
  }
  .toggle-row {
    display: flex;
    align-items: center;
    gap: 14px;
  }
  .toggle-copy {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .toggle-label {
    font-size: 12px;
    font-weight: 600;
    color: var(--ink-secondary);
  }
  .seg {
    display: flex;
    border: 1px solid var(--border-strong);
    border-radius: 8px;
    overflow: hidden;
    flex: none;
  }
  .seg button {
    font-size: 12px;
    font-weight: 500;
    padding: 7px 12px;
    border: none;
    background: var(--surface);
    color: var(--ink-secondary);
  }
  .seg button.on {
    background: color-mix(in srgb, var(--accent) 10%, transparent);
    color: var(--accent-deep);
    font-weight: 600;
  }
  .limitation {
    margin: 0;
    font-size: 11.5px;
    line-height: 1.6;
    color: var(--ink-tertiary);
    background: var(--paper);
    border: 1px solid var(--border);
    border-radius: 9px;
    padding: 10px 13px;
  }
  .check {
    flex-direction: row;
    align-items: center;
    gap: 8px;
    font-size: 13px;
    color: var(--ink-secondary);
  }
  .check input {
    width: auto;
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

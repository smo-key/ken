<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "../lib/api";
  import { chats } from "../lib/chats.svelte";

  let { close }: { close: () => void } = $props();

  let question = $state("");
  let options = $state<string[]>(["research"]);
  let selected = $state("research");
  let outputDir = $state("research");
  let error = $state<string | null>(null);
  let busy = $state(false);

  onMount(async () => {
    try {
      options = await api.researchOutputOptions();
      selected = options[0] ?? "research";
      outputDir = selected;
    } catch {
      // keep the defaults — the folder field is still editable
    }
  });

  async function start() {
    const q = question.trim();
    if (!q || busy) return;
    busy = true;
    error = null;
    try {
      const chatId = await api.startResearch(q, outputDir.trim());
      close();
      chats.open = true;
      await chats.refresh();
      await chats.select(chatId);
    } catch (e) {
      error = String(e);
      busy = false;
    }
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      void start();
    }
  }
</script>

<button class="scrim" onclick={close} aria-label="Close"></button>
<div class="modal" role="dialog" aria-label="Start research">
  <h2>Start research</h2>
  <p class="lede">
    Ken will search the web and write a cited report into your project.
  </p>

  {#if error}
    <div class="error">{error}</div>
  {/if}

  <label>
    What should Ken research on the web?
    <textarea
      bind:value={question}
      onkeydown={onKeydown}
      rows="3"
      placeholder="e.g. How are similar teams handling knowledge handover when someone leaves?"
    ></textarea>
  </label>

  <div class="two-col">
    <label>
      Save the report in
      <select bind:value={selected} onchange={() => (outputDir = selected)}>
        {#each options as opt (opt)}
          <option value={opt}>{opt}/</option>
        {/each}
      </select>
    </label>
    <label>
      Folder <span class="soft">(editable)</span>
      <input bind:value={outputDir} class="mono-input" placeholder="research" />
    </label>
  </div>

  <div class="actions">
    <button
      class="btn btn-primary"
      disabled={!question.trim() || busy}
      onclick={start}
    >
      {busy ? "Starting…" : "Start research"}
    </button>
    <button class="btn btn-ghost" onclick={close}>Cancel</button>
  </div>
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
    width: min(520px, calc(100vw - 80px));
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
  .lede {
    margin: 0;
    font-size: 13px;
    color: var(--ink-secondary);
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
  input,
  textarea,
  select {
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
  textarea:focus,
  select:focus {
    border-color: var(--accent);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 12%, transparent);
  }
  .two-col {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 14px;
  }
  .error {
    font-size: 12.5px;
    color: var(--danger);
    background: color-mix(in srgb, var(--danger) 7%, transparent);
    border: 1px solid color-mix(in srgb, var(--danger) 25%, transparent);
    border-radius: 9px;
    padding: 9px 12px;
  }
  .actions {
    display: flex;
    gap: 8px;
    margin-top: 4px;
  }
</style>

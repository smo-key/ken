<script lang="ts">
  import { TEMPLATES, type IngestTemplate } from "../lib/templates";

  // Picking a template opens the editable form pre-filled from the recipe —
  // it never saves on its own, so every field stays tailorable before saving.
  let {
    pick,
    close,
  }: {
    pick: (template: IngestTemplate) => void;
    close: () => void;
  } = $props();
</script>

<button class="scrim" onclick={close} aria-label="Close"></button>
<div class="modal" role="dialog" aria-label="Template library">
  <h2>Start from a template</h2>
  <p class="lede">
    Templates are starting points — after you pick one it's yours to tailor
    before saving.
  </p>
  <div class="grid">
    {#each TEMPLATES as t (t.id)}
      <div class="tile">
        <div class="tile-name">{t.name}</div>
        <div class="tile-desc">{t.description}</div>
        <div class="tile-foot">
          <span class="mono out">{t.output}</span>
          <button class="btn btn-small" onclick={() => pick(t)}>Use</button>
        </div>
      </div>
    {/each}
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
    width: min(680px, calc(100vw - 80px));
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
  .grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 12px;
  }
  .tile {
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 14px 16px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    background: var(--sunken-2);
  }
  .tile-name {
    font-size: 14px;
    font-weight: 600;
  }
  .tile-desc {
    font-size: 12.5px;
    color: var(--ink-secondary);
    line-height: 1.5;
    flex: 1;
  }
  .tile-foot {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 4px;
  }
  .out {
    font-size: 11px;
    color: var(--ink-tertiary);
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>

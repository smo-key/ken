<script lang="ts">
  import { api } from "../lib/api";
  import { TEMPLATES, type IngestTemplate } from "../lib/templates";

  let { close }: { close: (createdSlug: string | null) => void } = $props();
  let error = $state<string | null>(null);
  let busy = $state<string | null>(null);

  async function use(t: IngestTemplate) {
    busy = t.id;
    error = null;
    try {
      const recipe = await api.saveIngest({
        name: t.name,
        description: t.description,
        instruction: t.instruction,
        sources: [],
        output: t.output,
        mode: t.mode,
        refresh: t.refresh,
      });
      close(recipe.slug);
    } catch (e) {
      error = String(e);
      busy = null;
    }
  }
</script>

<button class="scrim" onclick={() => close(null)} aria-label="Close"></button>
<div class="modal" role="dialog" aria-label="Template library">
  <h2>Start from a template</h2>
  <p class="lede">
    Templates are starting points — after you pick one it's yours to tailor.
  </p>
  {#if error}
    <div class="error">{error}</div>
  {/if}
  <div class="grid">
    {#each TEMPLATES as t (t.id)}
      <div class="tile">
        <div class="tile-name">{t.name}</div>
        <div class="tile-desc">{t.description}</div>
        <div class="tile-foot">
          <span class="mono out">{t.output}</span>
          <button class="btn btn-small" disabled={busy !== null} onclick={() => use(t)}>
            {busy === t.id ? "Adding…" : "Use"}
          </button>
        </div>
      </div>
    {/each}
  </div>
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: rgba(33, 30, 25, 0.18);
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
  .error {
    font-size: 12.5px;
    color: var(--danger);
    background: rgba(163, 77, 63, 0.07);
    border: 1px solid rgba(163, 77, 63, 0.25);
    border-radius: 9px;
    padding: 9px 12px;
  }
</style>

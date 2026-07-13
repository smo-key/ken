<script lang="ts">
  let {
    overline,
    title,
    body,
    hint,
  }: { overline: string; title: string; body: string; hint?: string } = $props();
</script>

<div class="wrap">
  <div class="inner">
    <div class="overline">{overline}</div>
    <h1>{title}</h1>
    <div class="card">
      <p>{body}</p>
      {#if hint}
        <p class="hint">{hint}</p>
      {/if}
    </div>
  </div>
</div>

<style>
  .wrap {
    flex: 1;
    min-width: 0;
    overflow-y: auto;
    padding: 36px 44px;
  }
  .inner {
    max-width: 640px;
    margin: 0 auto;
    display: flex;
    flex-direction: column;
    gap: 18px;
  }
  .overline {
    font-size: 11.5px;
    font-weight: 700;
    color: var(--accent-deep);
    letter-spacing: 0.07em;
    text-transform: uppercase;
  }
  h1 {
    margin: 0;
    font-family: var(--font-serif);
    font-size: 28px;
    font-weight: 500;
    letter-spacing: -0.01em;
  }
  .card {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-card);
    padding: 22px 24px;
    box-shadow: var(--shadow-card);
  }
  p {
    margin: 0;
    font-size: 14px;
    line-height: 1.7;
    color: var(--ink-secondary);
  }
  .hint {
    margin-top: 12px;
    font-size: 12.5px;
    color: var(--ink-tertiary);
  }
</style>

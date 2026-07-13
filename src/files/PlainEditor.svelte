<script lang="ts">
  let {
    initial,
    onchange,
  }: { initial: string; onchange: (text: string) => void } = $props();

  let value = $state(initial);
</script>

<div class="scroll">
  <textarea
    bind:value
    oninput={() => onchange(value)}
    spellcheck="false"
  ></textarea>
</div>

<style>
  .scroll {
    flex: 1;
    min-height: 0;
    display: flex;
  }
  textarea {
    flex: 1;
    max-width: 720px;
    margin: 0 auto;
    width: 100%;
    padding: 24px clamp(20px, 5%, 48px) 80px;
    border: none;
    outline: none;
    resize: none;
    background: transparent;
    color: var(--ink);
    font-family: var(--font-mono);
    font-size: 13px;
    line-height: 1.7;
  }
</style>

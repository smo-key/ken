<script lang="ts">
  import { treeEdit } from "./treeEdit.svelte";

  let { indent }: { indent: number } = $props();
  let value = $state(treeEdit.initial);

  // Autofocus and select the stem (not the extension) so typing replaces the
  // interesting part of "Untitled.md". Runs once, on mount.
  function setup(el: HTMLInputElement) {
    el.focus();
    const dot = treeEdit.initial.lastIndexOf(".");
    el.setSelectionRange(0, dot > 0 ? dot : treeEdit.initial.length);
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      void treeEdit.commit(value);
    } else if (e.key === "Escape") {
      e.preventDefault();
      treeEdit.cancel();
    }
  }
</script>

<div class="edit-row" style:padding-left={`${indent}px`}>
  <input
    use:setup
    bind:value
    spellcheck="false"
    aria-label="Name"
    onkeydown={onKeydown}
    onblur={() => treeEdit.cancel()}
  />
</div>

<style>
  .edit-row {
    display: flex;
    align-items: center;
    padding-top: 2px;
    padding-bottom: 2px;
    padding-right: 8px;
  }
  input {
    width: 100%;
    min-width: 0;
    font-size: 13px;
    font-family: inherit;
    color: var(--ink);
    background: var(--surface);
    border: 1px solid var(--accent);
    border-radius: 6px;
    padding: 3px 7px;
    outline: none;
  }
</style>

<script lang="ts">
  import { app } from "../lib/app.svelte";
  import FileTree from "../files/FileTree.svelte";
  import EditorPane from "../files/EditorPane.svelte";
</script>

<div class="files">
  <FileTree />
  <div class="content">
    {#if app.openFile}
      {#key app.openFile}
        <EditorPane relPath={app.openFile} />
      {/key}
    {:else}
      <div class="empty">
        <p>Select a file to read or edit it.</p>
        <p class="hint">Markdown and text open in the editor; Word, Excel, PDF and images preview right here.</p>
      </div>
    {/if}
  </div>
</div>

<style>
  .files {
    flex: 1;
    min-width: 0;
    display: flex;
    min-height: 0;
  }
  .content {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    min-height: 0;
    background: var(--surface);
  }
  .empty {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 4px;
  }
  .empty p {
    margin: 0;
    color: var(--ink-secondary);
    font-size: 14px;
  }
  .empty .hint {
    color: var(--ink-tertiary);
    font-size: 12.5px;
  }
</style>

<script lang="ts">
  import { onMount } from "svelte";
  import { api, type FileRow } from "../../lib/api";
  import { formatSize, glyphFor, timeAgo } from "../../lib/format";
  import FileGlyph from "../FileGlyph.svelte";
  import { registerDomFind } from "../../lib/find-dom.svelte";

  let { relPath, meta }: { relPath: string; meta: FileRow } = $props();

  let text = $state<string>("");
  let inner = $state<HTMLDivElement | null>(null);

  // The extracted text is what Ken knows about this file, so that is what find
  // searches — the metadata card comes along because it is in the same subtree.
  registerDomFind(() => inner, { deps: () => text });

  onMount(async () => {
    try {
      text = await api.extractedText(relPath);
    } catch {
      text = "";
    }
  });
</script>

<div class="scroll">
  <div class="inner" bind:this={inner}>
    <div class="meta-card">
      <FileGlyph kind={meta.kind} />
      <div class="facts">
        <div class="name">{relPath.split("/").pop()}</div>
        <div class="detail">
          {glyphFor(meta.kind).label} · {formatSize(meta.size)} · modified
          {timeAgo(meta.mtime)}
        </div>
        {#if meta.status === "failed"}
          <div class="fail">
            Ken couldn't read the contents — {meta.error ?? "unknown reason"}.
            It's still findable by name.
          </div>
        {/if}
      </div>
      <button class="btn" onclick={() => api.openExternal(relPath)}>
        Open in default app
      </button>
    </div>

    {#if text.trim()}
      <div class="label">What Ken extracted</div>
      <pre>{text}</pre>
    {:else if meta.status !== "failed"}
      <p class="quiet">
        No preview for this format yet — the file is indexed by name
        {meta.kind === "pptx" ? " and its slide text is searchable" : ""}.
      </p>
    {/if}
  </div>
</div>

<style>
  .scroll {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }
  .inner {
    max-width: 720px;
    margin: 0 auto;
    padding: 32px clamp(20px, 5%, 48px) 80px;
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .meta-card {
    display: flex;
    align-items: center;
    gap: 14px;
    background: var(--paper);
    border: 1px solid var(--border);
    border-radius: var(--radius-card);
    padding: 18px 20px;
  }
  .facts {
    flex: 1;
    min-width: 0;
  }
  .name {
    font-weight: 600;
    font-size: 15px;
  }
  .detail {
    font-size: 12px;
    color: var(--ink-tertiary);
    margin-top: 2px;
  }
  .fail {
    font-size: 12.5px;
    color: var(--danger);
    margin-top: 6px;
    line-height: 1.5;
  }
  .label {
    font-size: 11px;
    font-weight: 700;
    color: var(--ink-tertiary);
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }
  pre {
    margin: 0;
    padding: 16px 18px;
    background: var(--paper);
    border: 1px solid var(--border);
    border-radius: 10px;
    font-family: var(--font-mono);
    font-size: 12px;
    line-height: 1.7;
    white-space: pre-wrap;
    word-break: break-word;
  }
  .quiet {
    margin: 0;
    color: var(--ink-tertiary);
    font-size: 13px;
  }
</style>

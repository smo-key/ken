<script lang="ts">
  // A code file's structure under it (the code map): its outline, what it
  // imports and what imports it, and for any definition, where it is used
  // across the workspace. "Go to definition" and "find usages" for people.
  import { api, type CodeFile, type CodeUsages, type CodeUse } from "../lib/api";
  import { app } from "../lib/app.svelte";

  let { relPath, refresh = 0 }: { relPath: string; refresh?: number } = $props();

  let file = $state<CodeFile | null>(null);
  let open = $state(false);
  let usagesOf = $state<string | null>(null);
  let usages = $state<CodeUsages | null>(null);
  let loadingUses = $state(false);

  $effect(() => {
    void refresh;
    const path = relPath;
    usagesOf = null;
    usages = null;
    api.codeFile(path).then(
      (f) => {
        if (path === relPath) file = f;
      },
      () => (file = null),
    );
  });

  async function showUsages(name: string) {
    usagesOf = name;
    usages = null;
    loadingUses = true;
    try {
      const found = await api.codeUsages(name);
      if (usagesOf === name) usages = found;
    } finally {
      if (usagesOf === name) loadingUses = false;
    }
  }

  async function goTo(u: { projectId?: string; path: string; line: number }) {
    if (u.projectId && u.projectId !== app.focused) await app.focusMember(u.projectId);
    app.openAt(u.path, { line: u.line });
  }

  const base = (p: string) => p.split("/").pop() ?? p;
  const place = (u: CodeUse) => `${u.memberName}/${u.path}:${u.line}`;
  const hasAnything = $derived(
    !!file && (file.outline.length > 0 || file.related.imports.length > 0 || file.related.importedBy.length > 0),
  );
</script>

{#if hasAnything && file}
  <div class="code" aria-label="Code structure">
    <div class="row">
      <button class="toggle" onclick={() => (open = !open)} aria-expanded={open}>
        {open ? "▾" : "▸"} Outline <span class="count">{file.outline.length}</span>
      </button>
      {#if file.related.imports.length > 0}
        <span class="label">Imports</span>
        {#each file.related.imports as p}
          <button class="link" title={p} onclick={() => app.openInFiles(p)}>{base(p)}</button>
        {/each}
      {/if}
      {#if file.related.importedBy.length > 0}
        <span class="label">Imported by</span>
        {#each file.related.importedBy as p}
          <button class="link" title={p} onclick={() => app.openInFiles(p)}>{base(p)}</button>
        {/each}
      {/if}
    </div>
    {#if open}
      <ul class="outline">
        {#each file.outline as s}
          <li style:padding-left="{s.depth * 14}px">
            <button class="def" title={s.docs ?? `${s.kind} at line ${s.line}`} onclick={() => app.openAt(relPath, { line: s.line })}>
              <span class="kind">{s.kind}</span>
              {s.name}
            </button>
            <span class="lines">{s.line}</span>
            <button class="uses-btn" onclick={() => showUsages(s.name)} title="Every place this is used">usages</button>
          </li>
        {/each}
      </ul>
    {/if}
    {#if usagesOf}
      <div class="usages">
        <div class="uhead">
          Usages of <code>{usagesOf}</code>
          {#if usages}
            — {usages.uses.length} in {new Set(usages.uses.map((u) => u.memberName + u.path)).size} files
          {/if}
          <button class="close" onclick={() => (usagesOf = null)} aria-label="Close usages">×</button>
        </div>
        {#if loadingUses}
          <div class="muted">Looking…</div>
        {:else if usages}
          {#each usages.definitions as d (place(d))}
            <button class="use" onclick={() => goTo(d)}>
              <span class="tag">defined</span>
              <span class="mono">{place(d)}</span>
            </button>
          {/each}
          {#each usages.uses as u (place(u) + u.kind)}
            <button class="use" onclick={() => goTo(u)}>
              <span class="tag">{u.kind === "mention" ? "ref" : u.kind}</span>
              <span class="mono">{place(u)}</span>
              {#if u.within}<span class="muted">in {u.within}</span>{/if}
            </button>
          {:else}
            <div class="muted">No uses found in the code map.</div>
          {/each}
        {/if}
      </div>
    {/if}
  </div>
{/if}

<style>
  .code {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 8px 16px;
    border-top: 1px solid var(--border);
    font-size: 12px;
    flex: none;
    max-height: 40%;
    overflow-y: auto;
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: baseline;
  }
  .toggle,
  .link,
  .def,
  .uses-btn,
  .use,
  .close {
    background: none;
    border: none;
    padding: 0;
    font: inherit;
    cursor: pointer;
    text-align: left;
  }
  .toggle {
    color: var(--ink);
    font-weight: 600;
    margin-right: 10px;
  }
  .count,
  .lines,
  .muted,
  .label {
    color: var(--ink-tertiary);
  }
  .label {
    margin-left: 6px;
  }
  .link,
  .def,
  .uses-btn {
    color: var(--accent);
  }
  .link:hover,
  .def:hover,
  .uses-btn:hover,
  .use:hover .mono {
    text-decoration: underline;
  }
  .outline {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .outline li {
    display: flex;
    gap: 8px;
    align-items: baseline;
  }
  .kind,
  .tag {
    font-size: 9.5px;
    text-transform: uppercase;
    letter-spacing: 0.03em;
    color: var(--ink-tertiary);
    margin-right: 4px;
  }
  .uses-btn {
    font-size: 11px;
    opacity: 0.7;
  }
  .usages {
    border-top: 1px dashed var(--border);
    padding-top: 6px;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .uhead {
    font-weight: 600;
    display: flex;
    gap: 6px;
    align-items: baseline;
  }
  .close {
    margin-left: auto;
    color: var(--ink-tertiary);
    font-size: 14px;
  }
  .use {
    display: flex;
    gap: 6px;
    align-items: baseline;
    color: var(--ink-secondary, var(--ink));
  }
  .mono {
    font-family: var(--font-mono, monospace);
    font-size: 11.5px;
  }
</style>

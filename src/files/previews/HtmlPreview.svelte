<script lang="ts">
  import Globe from "@lucide/svelte/icons/globe";
  import ShieldOff from "@lucide/svelte/icons/shield-off";
  import { api } from "../../lib/api";
  import { find, type FindAdapter } from "../../lib/find.svelte";
  import {
    SANDBOX,
    buildHtmlDocument,
    findInHtmlDocument,
    type AssetReader,
  } from "./html";
  import PreviewLoading from "./PreviewLoading.svelte";

  let { relPath }: { relPath: string } = $props();

  let clean = $state<string | null>(null);
  let marked = $state<string | null>(null);
  let error = $state<string | null>(null);

  // The opt-in is tied to the document it was given for, not stored as a flag:
  // open another file and it is false again, with nothing to remember to reset.
  // Closing the pane destroys it outright.
  let networkFor = $state<string | null>(null);
  const network = $derived(networkFor === relPath);

  // The frame reloads whenever srcdoc changes, so the marked-up copy is built
  // once per query — never per step, which would bounce the reader to the top.
  const doc = $derived(marked ?? clean);

  // Every asset the page needs comes through the project API: the backend
  // refuses paths outside the project root and downloads cloud-only files, so a
  // preview inherits both without re-implementing either.
  const reader: AssetReader = {
    text: (path) => api.readFile(path),
    bytes: (path) => api.readFileBytes(path),
  };

  let generation = 0;

  $effect(() => {
    const path = relPath;
    const allowed = network;
    const mine = ++generation;
    clean = null;
    marked = null;
    error = null;

    void (async () => {
      try {
        const raw = await api.readFile(path);
        const built = await buildHtmlDocument(raw, path, reader, {
          network: allowed,
        });
        if (mine === generation) clean = built;
      } catch (e) {
        if (mine === generation) {
          error = `Couldn't render this page — ${e}. Try “Open in default app”.`;
        }
      }
    })();
  });

  $effect(() => {
    const source = clean;
    const adapter: FindAdapter = {
      search(query) {
        if (!source) return { total: 0 };
        const hit = findInHtmlDocument(source, query);
        marked = hit.total > 0 ? hit.doc : null;
        return {
          total: hit.total,
          capped: hit.capped,
          note:
            hit.total > 0
              ? "Every match is highlighted, but this page runs in a locked-down frame Ken can't scroll — use Source to step through matches."
              : undefined,
        };
      },
      reveal() {
        // Nothing to do: the frame is an opaque origin the app can't script, so
        // the page can't be scrolled to a hit. The counter still moves.
      },
      clear() {
        marked = null;
      },
    };
    find.register(adapter);
    return () => find.unregister(adapter);
  });
</script>

<div class="wrap">
  <div class="scroll">
    {#if error}
      <div class="note error">{error}</div>
    {:else if doc === null}
      <PreviewLoading label="Rendering page…" />
    {:else}
      <!-- allow-scripts WITHOUT allow-same-origin (see SANDBOX): the page's own
           code runs, but in an opaque origin that can't reach the app window or
           Tauri. The two together would undo the sandbox entirely. The document
           arrives via srcdoc, never a file:// URL — its stylesheets, scripts and
           images are already inlined — and the meta-CSP inside decides whether
           it may touch the network at all. -->
      <iframe class="page" title="Preview" sandbox={SANDBOX} srcdoc={doc}></iframe>
    {/if}
  </div>

  <div class="bar" class:live={network}>
    {#if network}
      <span class="dot"></span>
      <span class="text">
        Network allowed — this page can load content from, and send information
        to, outside servers.
      </span>
      <button class="link" onclick={() => (networkFor = null)}>
        <ShieldOff size={12} strokeWidth={1.75} />
        <span>Block again</span>
      </button>
    {:else}
      <span class="text">
        Outside content is blocked. Files next to this page are shown; anything
        it loads from the internet is not.
      </span>
      <button class="link" onclick={() => (networkFor = relPath)}>
        <Globe size={12} strokeWidth={1.75} />
        <span>Allow network</span>
      </button>
    {/if}
  </div>
</div>

<style>
  .wrap {
    flex: 1;
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .scroll {
    flex: 1;
    min-height: 0;
    display: flex;
    overflow: hidden;
  }
  .page {
    flex: 1;
    min-width: 0;
    border: none;
    /* Documents assume a light page; the app theme must not tint them. */
    background: #fff;
  }
  .bar {
    flex: none;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 5px 12px;
    border-top: 1px solid var(--border);
    background: var(--surface);
    font-size: 11.5px;
    color: var(--ink-tertiary);
  }
  .bar.live {
    background: color-mix(in srgb, var(--needs-input) 8%, var(--surface));
    color: var(--ink-secondary);
  }
  .text {
    flex: 1;
    min-width: 0;
    line-height: 1.45;
  }
  .dot {
    width: 6px;
    height: 6px;
    border-radius: 3px;
    background: var(--needs-input);
    flex: none;
  }
  .link {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    flex: none;
    padding: 2px 7px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: transparent;
    color: var(--ink-secondary);
    font-size: 11.5px;
    font-weight: 500;
  }
  .link:hover {
    background: var(--sunken);
    color: var(--ink);
  }
  .note {
    text-align: center;
    color: var(--ink-tertiary);
    font-size: 13px;
    padding: 20px;
  }
  .note.error {
    color: var(--danger);
  }
</style>

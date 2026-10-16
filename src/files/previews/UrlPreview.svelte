<script lang="ts">
  // A .url internet shortcut is a one-line pointer, so the preview shows the
  // destination rather than the pointer: the address stays editable in a slim
  // toolbar and the page itself fills the pane underneath.
  import ExternalLink from "@lucide/svelte/icons/external-link";
  import { api } from "../../lib/api";
  import PreviewLoading from "./PreviewLoading.svelte";

  let { relPath }: { relPath: string } = $props();

  let stored = $state<string | null>(null);
  let draft = $state("");
  let error = $state<string | null>(null);
  let saveError = $state<string | null>(null);
  let saving = $state(false);
  let input = $state<HTMLInputElement | null>(null);

  const trimmed = $derived(draft.trim());
  const dirty = $derived(stored !== null && trimmed !== stored);
  const embeddable = $derived(/^https?:\/\//i.test(stored ?? ""));

  /**
   * Reads the address out of the `[InternetShortcut]` INI body. The key is
   * matched case-insensitively because writers disagree on `URL=` vs `url=`,
   * and a file that is nothing but a bare address is accepted too — that is
   * what a hand-made shortcut usually looks like.
   */
  function parseUrl(text: string): string {
    for (const line of text.split(/\r?\n/)) {
      const m = /^\s*url\s*=\s*(.*)$/i.exec(line);
      if (m) return m[1].trim();
    }
    const body = text.trim();
    if (/^[a-z][a-z0-9+.-]*:\/\//i.test(body) && !/\s/.test(body)) return body;
    return "";
  }

  let generation = 0;

  $effect(() => {
    const path = relPath;
    const mine = ++generation;
    stored = null;
    error = null;
    saveError = null;

    void (async () => {
      try {
        const raw = await api.readFile(path);
        if (mine !== generation) return;
        const found = parseUrl(raw);
        stored = found;
        draft = found;
      } catch (e) {
        if (mine === generation) {
          error = `Couldn't read this shortcut — ${e}.`;
        }
      }
    })();
  });

  // An empty shortcut is a prompt, not an error: put the cursor where the fix
  // goes as soon as the file resolves.
  $effect(() => {
    if (stored === "") input?.focus();
  });

  async function save() {
    if (!dirty || saving) return;
    const value = trimmed;
    saving = true;
    saveError = null;
    try {
      await api.saveFile(relPath, `[InternetShortcut]\nURL=${value}\n`);
      stored = value;
      draft = value;
    } catch (e) {
      saveError = `Couldn't save this shortcut — ${e}.`;
    } finally {
      saving = false;
    }
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      void save();
    }
  }

  async function open() {
    if (!stored) return;
    try {
      await api.openWebUrl(stored);
    } catch (e) {
      saveError = `Couldn't open this link — ${e}.`;
    }
  }
</script>

<div class="wrap">
  {#if error}
    <div class="note error">{error}</div>
  {:else if stored === null}
    <PreviewLoading label="Opening link…" />
  {:else}
    <div class="bar">
      <input
        bind:this={input}
        class="address"
        type="text"
        spellcheck="false"
        autocomplete="off"
        placeholder="https://"
        aria-label="Web address"
        bind:value={draft}
        onkeydown={onKeydown}
      />
      {#if dirty}
        <button class="save" disabled={saving} onclick={() => void save()}>
          {saving ? "Saving…" : "Save"}
        </button>
      {/if}
      <button
        class="icon"
        aria-label="Open in browser"
        title="Open in browser"
        disabled={!embeddable}
        onclick={() => void open()}
      >
        <ExternalLink size={14} strokeWidth={1.75} />
      </button>
    </div>

    {#if saveError}
      <div class="hint error">{saveError}</div>
    {/if}

    {#if embeddable}
      <!-- The page is a real site loaded over the network, so it gets the
           sandbox a browser tab would minus top-level navigation. Framing
           refusals (X-Frame-Options, frame-ancestors) are invisible from out
           here — hence the standing hint rather than a detected message. -->
      <iframe
        class="page"
        title="Linked page"
        src={stored}
        sandbox="allow-scripts allow-same-origin allow-forms allow-popups"
        referrerpolicy="no-referrer"
      ></iframe>
    {:else}
      <div class="empty">
        {stored === ""
          ? "No address"
          : "Can't show this here"}
      </div>
    {/if}
  {/if}
</div>

<style>
  .wrap {
    flex: 1;
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .bar {
    flex: none;
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 5px 10px;
    border-bottom: 1px solid var(--border);
    background: var(--surface);
  }
  .address {
    flex: 1;
    min-width: 0;
    border: 1px solid transparent;
    border-radius: 6px;
    outline: none;
    background: transparent;
    color: var(--ink);
    font-family: var(--font-mono);
    font-size: 12px;
    padding: 4px 8px;
  }
  .address:hover {
    border-color: var(--border);
  }
  .address:focus {
    border-color: var(--border-strong);
    background: var(--paper);
  }
  .address::placeholder {
    color: var(--ink-tertiary);
  }
  .save {
    flex: none;
    padding: 3px 9px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: transparent;
    color: var(--ink-secondary);
    font-size: 11.5px;
    font-weight: 500;
  }
  .save:hover:not(:disabled) {
    background: var(--sunken);
    color: var(--ink);
  }
  .save:disabled {
    color: var(--ink-tertiary);
  }
  .icon {
    flex: none;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 24px;
    border: none;
    border-radius: 6px;
    background: transparent;
    color: var(--ink-secondary);
  }
  .icon:hover:not(:disabled) {
    background: var(--sunken);
    color: var(--ink);
  }
  .icon:disabled {
    color: var(--ink-tertiary);
  }
  .page {
    flex: 1;
    min-width: 0;
    min-height: 0;
    border: none;
    background: #fff;
  }
  .hint {
    flex: none;
    padding: 4px 12px;
    border-top: 1px solid var(--border);
    background: var(--surface);
    font-size: 11.5px;
    line-height: 1.45;
    color: var(--ink-tertiary);
  }
  .hint.error {
    border-top: none;
    border-bottom: 1px solid var(--border);
    color: var(--danger);
  }
  .empty {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 24px;
    text-align: center;
    font-size: 13px;
    color: var(--ink-tertiary);
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

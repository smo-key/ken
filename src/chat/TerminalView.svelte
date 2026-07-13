<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { Terminal } from "@xterm/xterm";
  import { FitAddon } from "@xterm/addon-fit";
  import "@xterm/xterm/css/xterm.css";
  import { api } from "../lib/api";

  let { chatId }: { chatId: string } = $props();

  let host: HTMLDivElement;
  let term: Terminal | undefined;
  let unlisten: (() => void) | undefined;
  let resizeObserver: ResizeObserver | undefined;

  function b64ToBytes(b64: string): Uint8Array {
    const bin = atob(b64);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    return bytes;
  }

  function bytesToB64(bytes: Uint8Array): string {
    let bin = "";
    for (const b of bytes) bin += String.fromCharCode(b);
    return btoa(bin);
  }

  onMount(async () => {
    term = new Terminal({
      fontFamily: "'IBM Plex Mono', monospace",
      fontSize: 12,
      theme: {
        background: "#1C1916",
        foreground: "#C9C3B7",
        cursor: "#C9C3B7",
        selectionBackground: "#57524A",
      },
      cursorBlink: true,
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(host);
    fit.fit();

    term.onData((data) => {
      void api.chatPtyInput(chatId, bytesToB64(new TextEncoder().encode(data)));
    });
    term.onResize(({ rows, cols }) => {
      void api.chatPtyResize(chatId, rows, cols);
    });

    unlisten = await api.onChatPtyData((chunk) => {
      if (chunk.chatId === chatId && term) {
        term.write(b64ToBytes(chunk.data));
      }
    });

    resizeObserver = new ResizeObserver(() => fit.fit());
    resizeObserver.observe(host);
    void api.chatPtyResize(chatId, term.rows, term.cols);
    term.focus();
  });

  onDestroy(() => {
    unlisten?.();
    resizeObserver?.disconnect();
    term?.dispose();
  });
</script>

<div class="term" bind:this={host}></div>

<style>
  .term {
    flex: 1;
    min-height: 0;
    background: var(--terminal-bg);
    padding: 8px 0 8px 8px;
  }
  .term :global(.xterm) {
    height: 100%;
  }
</style>

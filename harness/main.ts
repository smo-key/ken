// Dev-only harness: mounts PptxPreview in a plain browser page so a deck can be
// rendered and screenshotted without launching Tauri. `?deck=<url>` picks the
// file; the deck is served from harness/decks/ (gitignored).
import { mount } from "svelte";
// Pulls in the bundled Carlito/Arimo faces, so harness renders use the same
// fonts the app does — otherwise substituted text measures differently here.
import "../src/app.css";
import { api } from "../src/lib/api";
import PptxPreview from "../src/files/previews/PptxPreview.svelte";

const params = new URLSearchParams(location.search);
const deck = params.get("deck") ?? "/sample1.pptx";

// PptxPreview only calls readFileBytes; serve the deck over HTTP instead of IPC.
(api as unknown as Record<string, unknown>).readFileBytes = async (path: string) => {
  const res = await fetch(path);
  if (!res.ok) throw new Error(`${res.status} fetching ${path}`);
  return res.arrayBuffer();
};

mount(PptxPreview, {
  target: document.getElementById("app")!,
  props: { relPath: deck },
});

import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

// Dev-only: serves harness/pptx.html on :1421 with harness/decks as static root.
export default defineConfig({
  root: new URL(".", import.meta.url).pathname,
  plugins: [svelte()],
  publicDir: new URL("./decks", import.meta.url).pathname,
  server: { port: 1421, strictPort: true },
});

# Wave 4 — UI fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the four UI-fix work items of Ken wave 4 — §3 (Files header pinning + relocated Import/filter), §9 (chat usability: first-message echo, focus-on-open, audit fixes), §10 (Settings simplification: curated offline-models catalog, tri-state watched-folders tree, page rhythm, one-row Appearance), and §11 (large-file preview hang) — **excluding** the "Answers & Map" language-model catalog entries, which a separate plan appends onto the catalog structure built here.

**Architecture:** Ken is a Svelte 5 (runes) + Tauri 2 desktop app. Business logic that can be unit-tested is extracted into **pure `.ts` modules** tested with vitest (mirroring `src/files/tabs.test.ts`); Svelte components stay thin and are verified by hand (the repo has no component-render testing-library). Rust logic in `crates/ken-core` is tested with `cargo test`. This plan **retires runtime Hugging-Face model discovery** (`crates/ken-core/src/model.rs`) and replaces it with a curated, category-tagged catalog whose download/verify/install plumbing is unchanged, so the language-model plan only appends entries.

**Tech Stack:** Svelte 5 runes (`$state`/`$derived`/`$effect`/`$props`), TypeScript, Tauri 2 (`@tauri-apps/api`), Rust (ken-core crate + `src-tauri`), vitest 4, `cargo test`. Package manager: pnpm.

## Global Constraints (exact values — copy verbatim)

**Preview size caps (§11)** — checked against `meta.size` BEFORE any bytes are read:
- xlsx / docx / pptx / ipynb: `15 * 1024 * 1024` (15 MB) each.
- Over cap → the existing "too large" notice treatment with an "Open in default app" action (same as big text files in `EditorPane.svelte`).

**Curated model catalog (§10)** — Transcription category only in this plan (Language entries appended by the local-LLM plan):
- Transcription · **Recommended**: `ggml-base.en.bin`, name `"Whisper Base (English)"`, `147_951_465` bytes (≈148 MB), blurb `"fast, accurate for meetings"`.
- Transcription · **Advanced**: `ggml-large-v3-turbo.bin`, name `"Whisper Large v3 Turbo"`, `1_624_555_275` bytes (≈1.6 GB, display/offline-floor only; real downloads verify against the server's `Content-Length`), blurb `"best accuracy, understands more languages, slower"`.
- Language entries the OTHER plan will add (documented here so the structure matches, NOT built here): *Qwen3 4B* ≈2.5 GB Recommended "instant answers, builds your map"; *Qwen3 8B* ≈5 GB Advanced "smarter answers, needs more memory".

**Settings copy strings (§10)** — verbatim:
- Offline Models promise (up front): `"These run on your Mac — nothing you say or store leaves it."`
- Three uppercase section headings, in order: `"This project"` (Project, Watched folders, Ignored files, Cloud files, Sync & collaboration), `"On this Mac"` (Appearance, Offline models, AI runner), `"Working with agents"` (Connect an agent).
- Appearance collapses three radio rows to ONE `"Theme"` row using the existing segmented-control idiom (as in the Files All/Unread filter).

**Chat (§9):** Enter sends, Shift+Enter inserts a newline (`ChatDrawer.svelte:123-128`, already correct — audit confirms and locks it with a test-backed pure helper). Optimistic user-message echo carries a **pending marker** and reconciles against the backend `chat-message` echo by id/content so it is never duplicated or dropped.

**Design (`.impeccable.md` — Paper & Ink):** plain human copy, accent (clay) rare, quiet motion (no bounce), editorial rhythm through varied spacing. Do not introduce new fonts/hues. Segmented controls, tri-state tree transitions, and the sticky header must feel calm.

---

## File Structure

**Created:**
- `src/files/previews/sizeGate.ts` — pure per-format preview size caps + over-cap predicate (§11).
- `src/files/previews/sizeGate.test.ts` — vitest for the size gate.
- `src/files/previews/TooLargeNotice.svelte` — shared "too large — open externally" notice, reused by PreviewPane and EditorPane (§11).
- `src/files/filesHeader.ts` — pure predicate for when the All/Unread filter shows (§3).
- `src/files/filesHeader.test.ts` — vitest for the header predicate.
- `src/lib/imports.svelte.ts` — shared import-staging store so the tree header and the (removed) toolbar share one `startImport()` flow (§3).
- `src/lib/chatEcho.ts` — pure optimistic-echo + reconcile logic for the chat transcript (§9).
- `src/lib/chatEcho.test.ts` — vitest for chat echo reconciliation.
- `src/lib/keySend.ts` — pure Enter-vs-Shift+Enter decision helper (§9 audit lock-in).
- `src/lib/keySend.test.ts` — vitest for the key helper.
- `src/lib/folderTree.ts` — pure watched-folders tree build + tri-state + toggle logic (§10).
- `src/lib/folderTree.test.ts` — vitest for the folder-tree logic.

**Modified:**
- `src/files/PreviewPane.svelte` — gate office/ipynb previews on `meta.size` before mounting the parser; show `TooLargeNotice` over cap (§11).
- `src/files/EditorPane.svelte` — reuse `TooLargeNotice`; add a working Cancel to the "Opening…" state (§11).
- `src/files/FileTree.svelte` — pin the "Files" header (sticky), drop "Manage folders", add icon-only Import + compact All/Unread filter (§3).
- `src/screens/FilesScreen.svelte` — drop the toolbar Import button + filter; keep tab strip + "Mark all as viewed"; render the ImportDialog from the shared store (§3).
- `src/lib/chats.svelte.ts` — optimistic echo in `send()`, reconcile in `onChatMessage`, widen `transcript` type; archive-active-chat state (§9).
- `src/chat/ChatDrawer.svelte` — focus the composer on open / newChat / select / exit-terminal; surface send failures near the composer; use `keySend` (§9).
- `src/chat/ChatTranscript.svelte` — render pending messages with a quiet marker; autoscroll only when already near the bottom (§9 audit).
- `src/screens/SettingsScreen.svelte` — tri-state watched-folders tree, Offline Models card, one-row Appearance segmented control, three section headings (§10).
- `crates/ken-core/src/model.rs` — retire discovery; add `ModelCategory`/`ModelTier`/`CatalogEntry`/`catalog()`/`selected*`/`ModelSelection` (§10).
- `crates/ken-core/src/transcript.rs` — resolve the transcription model from the selection with installed fallback (§10).
- `src-tauri/src/lib.rs` — rewire `list_models`/`download_model`/`model_status` to the catalog; add `set_model_selection`; extend `ModelStatusDto`; use the selected model at transcription call sites (§10).
- `src/lib/api.ts` — extend `ModelStatus` (category/tier/blurb/selected); add `setModelSelection` (§10).

---

## Task 1 — §11 Preview size gate (pure module + test)

Root cause: office previews (`XlsxPreview`/`DocxPreview`/`PptxPreview`/`IpynbPreview`) read and parse the whole file on the main thread with no cap, so a 148 MB xlsx (`Research/Data/irs-bmf/eo3.xlsx`) wedges the webview. Fix: a per-format cap checked against `meta.size` (already loaded — no extra I/O) BEFORE any bytes are read.

**Files:** Create `src/files/previews/sizeGate.ts`, `src/files/previews/sizeGate.test.ts`.

**Interfaces:**
- Consumes: nothing (pure). Callers pass `relPath` (for extension), backend `kind`, and `size` (bytes).
- Produces:
  - `export const PREVIEW_CAP_BYTES = 15 * 1024 * 1024;`
  - `export function previewFormat(relPath: string, kind: string): PreviewFormat | null` where `type PreviewFormat = "xlsx" | "docx" | "pptx" | "ipynb";`
  - `export function isPreviewTooLarge(relPath: string, kind: string, size: number): boolean`

### Steps

- [ ] **Write the failing test.** Create `src/files/previews/sizeGate.test.ts`:
```ts
import { describe, expect, it } from "vitest";
import { PREVIEW_CAP_BYTES, isPreviewTooLarge, previewFormat } from "./sizeGate";

describe("preview size gate", () => {
  const MB = 1024 * 1024;

  it("recognises the capped formats by kind and extension", () => {
    expect(previewFormat("book.xlsx", "xlsx")).toBe("xlsx");
    expect(previewFormat("memo.docx", "docx")).toBe("docx");
    expect(previewFormat("deck.pptx", "pptx")).toBe("pptx");
    // ipynb indexes as "binary"; it is routed by extension.
    expect(previewFormat("nb.ipynb", "binary")).toBe("ipynb");
  });

  it("does not gate formats that already stream or are cheap", () => {
    expect(previewFormat("photo.png", "image")).toBeNull();
    expect(previewFormat("doc.pdf", "pdf")).toBeNull();
    expect(previewFormat("clip.mp4", "binary")).toBeNull();
    expect(previewFormat("notes.md", "md")).toBeNull();
  });

  it("flags an over-cap workbook and clears an under-cap one", () => {
    expect(isPreviewTooLarge("eo3.xlsx", "xlsx", 148 * MB)).toBe(true); // the reported hang
    expect(isPreviewTooLarge("small.xlsx", "xlsx", 2 * MB)).toBe(false);
    // Exactly at the cap is allowed; one byte over is not.
    expect(isPreviewTooLarge("edge.docx", "docx", PREVIEW_CAP_BYTES)).toBe(false);
    expect(isPreviewTooLarge("edge.docx", "docx", PREVIEW_CAP_BYTES + 1)).toBe(true);
  });

  it("never gates a non-capped format regardless of size", () => {
    expect(isPreviewTooLarge("huge.pdf", "pdf", 500 * MB)).toBe(false);
  });
});
```

- [ ] **Run & see it fail:** `pnpm vitest run src/files/previews/sizeGate.test.ts` → fails (module missing). Expected: `Cannot find module './sizeGate'`.

- [ ] **Implement.** Create `src/files/previews/sizeGate.ts`:
```ts
// Per-format preview size caps (§11). Office/notebook previews parse the whole
// file synchronously; over a cap they would wedge the webview, so we gate on the
// already-loaded meta.size before any bytes are read. Formats that stream (pdf,
// image, video) or are cheap text are never gated here.

/** One cap for every heavy office/notebook format. Tuned against real parse
 *  times: a 15 MB workbook parses in well under a second; 148 MB hangs. */
export const PREVIEW_CAP_BYTES = 15 * 1024 * 1024;

export type PreviewFormat = "xlsx" | "docx" | "pptx" | "ipynb";

/** The capped format for a file, or null if this preview isn't size-gated.
 *  ipynb indexes as "binary", so it is matched by extension; the office kinds
 *  come straight from the backend classifier. */
export function previewFormat(relPath: string, kind: string): PreviewFormat | null {
  const ext = relPath.split(".").pop()?.toLowerCase() ?? "";
  if (ext === "ipynb") return "ipynb";
  if (kind === "xlsx") return "xlsx";
  if (kind === "docx") return "docx";
  if (kind === "pptx") return "pptx";
  return null;
}

/** Whether a preview would exceed its cap. False for formats we don't gate. */
export function isPreviewTooLarge(relPath: string, kind: string, size: number): boolean {
  return previewFormat(relPath, kind) !== null && size > PREVIEW_CAP_BYTES;
}
```

- [ ] **Run & pass:** `pnpm vitest run src/files/previews/sizeGate.test.ts` → all pass.

- [ ] **Commit:**
```bash
git add src/files/previews/sizeGate.ts src/files/previews/sizeGate.test.ts
git commit -m "feat(preview): pure per-format preview size gate (§11)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 2 — §11 Wire the size gate into the preview UI

Show the "too large" notice over cap, before any parser mounts; keep the app responsive. Reuse the existing `EditorPane` too-large treatment via a shared component and add a working Cancel to the "Opening…" state.

**Files:**
- Create `src/files/previews/TooLargeNotice.svelte`.
- Modify `src/files/PreviewPane.svelte` (whole file — it is the routing seam at `:24-42`).
- Modify `src/files/EditorPane.svelte` (`:307-317` too-large block; `:303-306` "Opening…" state).

**Interfaces:**
- `TooLargeNotice.svelte` props: `{ relPath: string; size: number }`. Emits nothing; calls `api.openExternal(relPath)`.
- `PreviewPane.svelte` already consumes `{ relPath, kind, meta: FileRow }` — `meta.size` is the gate input.

### Steps

- [ ] **Create `src/files/previews/TooLargeNotice.svelte`** (lifts the exact copy + actions from `EditorPane.svelte:307-317`):
```svelte
<script lang="ts">
  import { api } from "../../lib/api";
  let { relPath, size }: { relPath: string; size: number } = $props();
</script>

<div class="too-large">
  <p>
    <strong>{relPath.split("/").pop()}</strong> is
    {(size / 1048576).toFixed(1)} MB — too big to open inside Ken without slowing
    everything down. It's still indexed and searchable.
  </p>
  <button class="btn" onclick={() => api.openExternal(relPath)}>
    Open in default app
  </button>
</div>

<style>
  .too-large {
    margin: 48px auto;
    max-width: 420px;
    text-align: center;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 14px;
  }
  .too-large p {
    margin: 0;
    font-size: 13.5px;
    line-height: 1.65;
    color: var(--ink-secondary);
  }
</style>
```

- [ ] **Gate `PreviewPane.svelte`.** Replace the whole component with the gated router (adds the import + a top-of-render check; every heavy branch is unreachable when over cap):
```svelte
<script lang="ts">
  import type { FileRow } from "../lib/api";
  import PdfPreview from "./previews/PdfPreview.svelte";
  import DocxPreview from "./previews/DocxPreview.svelte";
  import XlsxPreview from "./previews/XlsxPreview.svelte";
  import ImagePreview from "./previews/ImagePreview.svelte";
  import IpynbPreview from "./previews/IpynbPreview.svelte";
  import PptxPreview from "./previews/PptxPreview.svelte";
  import HtmlPreview from "./previews/HtmlPreview.svelte";
  import VideoPreview from "./previews/VideoPreview.svelte";
  import FallbackPreview from "./previews/FallbackPreview.svelte";
  import TooLargeNotice from "./previews/TooLargeNotice.svelte";
  import { isPreviewTooLarge } from "./previews/sizeGate";
  import { isHtmlPath } from "./previews/html";

  let { relPath, kind, meta }: { relPath: string; kind: string; meta: FileRow } =
    $props();

  const ext = $derived(relPath.split(".").pop()?.toLowerCase() ?? "");
  const VIDEO_EXTS = new Set(["mp4", "mov", "m4v", "webm", "mkv", "avi"]);

  // Gate the heavy office/notebook parsers on the already-loaded size BEFORE any
  // bytes are read — a 148 MB workbook would otherwise wedge the webview.
  const tooLarge = $derived(isPreviewTooLarge(relPath, kind, meta.size));
</script>

{#if tooLarge}
  <TooLargeNotice {relPath} size={meta.size} />
{:else if ext === "ipynb"}
  <IpynbPreview {relPath} />
{:else if VIDEO_EXTS.has(ext)}
  <VideoPreview {relPath} />
{:else if isHtmlPath(relPath)}
  <HtmlPreview {relPath} />
{:else if kind === "pdf"}
  <PdfPreview {relPath} />
{:else if kind === "docx"}
  <DocxPreview {relPath} />
{:else if kind === "xlsx"}
  <XlsxPreview {relPath} />
{:else if kind === "pptx"}
  <PptxPreview {relPath} />
{:else if kind === "image"}
  <ImagePreview {relPath} />
{:else}
  <FallbackPreview {relPath} {meta} />
{/if}
```

- [ ] **Reuse the notice + add a Cancel in `EditorPane.svelte`.**
  1. Add the import after line 18 (`import PreviewLoading ...`): `import TooLargeNotice from "./previews/TooLargeNotice.svelte";`.
  2. Replace the too-large block (`:307-317`) with:
```svelte
  {:else if tooLarge && meta}
    <TooLargeNotice {relPath} size={meta.size} />
```
  3. Give the "Opening…" state (`:303-306`) a working Cancel so a slow resolve is never a dead end. Replace:
```svelte
  {:else if resolving}
    <!-- Kind/cloud-status not yet known: no preview or editor mounts until
         load() resolves, so an online-only file never flashes a preview. The
         Cancel returns to the notice/back-out so the app never wedges here. -->
    <div class="opening">
      <PreviewLoading label="Opening…" />
      <button class="btn btn-small" onclick={() => (loadError = "Opening cancelled — try again, or open in the default app.")}>
        Cancel
      </button>
    </div>
```
  4. Add to `EditorPane.svelte`'s `<style>`:
```css
  .opening {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 12px;
    margin-top: 24px;
  }
```

- [ ] **Manual verification (regression for the reported hang).** Per the `verify` skill, launch the app and open `Research/Data/irs-bmf/eo3.xlsx` (148 MB) in the user's real data: the "too big to open inside Ken" notice appears immediately, the tab strip and the rest of the app stay responsive, and "Open in default app" works. Open a small xlsx to confirm normal previews are unaffected. Acceptance: no "Opening…" wedge; no unbounded parse.

- [ ] **Type-check:** `pnpm exec svelte-check --tsconfig ./tsconfig.json` → no new errors in the touched files.

- [ ] **Commit:**
```bash
git add src/files/previews/TooLargeNotice.svelte src/files/PreviewPane.svelte src/files/EditorPane.svelte
git commit -m "fix(preview): size-gate office/notebook previews before parsing (§11)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 3 — §3 Files-header filter predicate (pure module + test)

The compact All/Unread filter shows only when something is unread OR the filter is active — same rule as `FilesScreen.svelte:103`. Extract the predicate so the tree header and any future caller agree, and so it is unit-tested.

**Files:** Create `src/files/filesHeader.ts`, `src/files/filesHeader.test.ts`.

**Interfaces:**
- `export type FilesFilter = "all" | "unread";`
- `export function showUnreadFilter(unreadCount: number, filter: FilesFilter): boolean`

### Steps

- [ ] **Write the failing test.** `src/files/filesHeader.test.ts`:
```ts
import { describe, expect, it } from "vitest";
import { showUnreadFilter } from "./filesHeader";

describe("files header filter visibility", () => {
  it("shows the filter when anything is unread", () => {
    expect(showUnreadFilter(3, "all")).toBe(true);
  });
  it("shows the filter while the unread view is active, even at zero", () => {
    expect(showUnreadFilter(0, "unread")).toBe(true);
  });
  it("hides the filter when nothing is unread and the view is all", () => {
    expect(showUnreadFilter(0, "all")).toBe(false);
  });
});
```

- [ ] **Run & see it fail:** `pnpm vitest run src/files/filesHeader.test.ts` → `Cannot find module './filesHeader'`.

- [ ] **Implement.** `src/files/filesHeader.ts`:
```ts
// The Files-tree header filter is a quiet control: it appears only when it can
// do something — there are unread files, or the user is already in the unread
// view (so they can switch back). Same rule the right-pane toolbar used.

export type FilesFilter = "all" | "unread";

export function showUnreadFilter(unreadCount: number, filter: FilesFilter): boolean {
  return unreadCount > 0 || filter === "unread";
}
```

- [ ] **Run & pass:** `pnpm vitest run src/files/filesHeader.test.ts` → pass.

- [ ] **Commit:**
```bash
git add src/files/filesHeader.ts src/files/filesHeader.test.ts
git commit -m "feat(files): pure predicate for the tree-header filter visibility (§3)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 4 — §3 Shared import store

Both the new tree-header Import button and the removed toolbar button must drive the *same* `startImport()` flow and the same `ImportDialog`. Hoist that state out of `FilesScreen.svelte:22-43` into a small store.

**Files:** Create `src/lib/imports.svelte.ts`.

**Interfaces (consumed by `FileTree.svelte` and `FilesScreen.svelte`):**
- `imports.staged: ImportDto | null`
- `imports.importing: boolean`
- `imports.importError: string | null`
- `imports.startImport(): Promise<void>` — pick a file, stage it, open the dialog.
- `imports.close(): void` — clear the staged file.

### Steps

- [ ] **Implement.** `src/lib/imports.svelte.ts` (moves the exact logic, unchanged in behavior):
```ts
// Import staging shared by the Files tree header and the ImportDialog host.
// Picking a file copies it into staging and opens the placement dialog (the
// AI's folder decision and preview resolve inside the dialog).
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { api, type ImportDto } from "./api";

class ImportsStore {
  staged = $state<ImportDto | null>(null);
  importing = $state(false);
  importError = $state<string | null>(null);

  async startImport() {
    if (this.importing || this.staged) return;
    this.importing = true;
    this.importError = null;
    try {
      const chosen = await openDialog({ directory: false });
      if (typeof chosen !== "string") return; // cancelled
      this.staged = await api.importBegin(chosen);
    } catch (e) {
      this.importError = String(e);
    } finally {
      this.importing = false;
    }
  }

  close() {
    this.staged = null;
  }
}

export const imports = new ImportsStore();
```

- [ ] **Type-check:** `pnpm exec svelte-check --tsconfig ./tsconfig.json` → no errors (store compiles; wiring lands in Task 5).

- [ ] **Commit:**
```bash
git add src/lib/imports.svelte.ts
git commit -m "feat(files): shared import store for tree header + dialog host (§3)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 5 — §3 Pin the Files header; relocate Import + filter

Make the "Files" header visually pinned (sticky within the existing scroll container), drop the "Manage folders" button, add an icon-only Import button and the compact All/Unread filter to it, and strip Import + filter from the right-pane toolbar (keeping the tab strip + "Mark all as viewed").

**Files:** Modify `src/files/FileTree.svelte` (`:1-16` imports, `:104-114` header, styles) and `src/screens/FilesScreen.svelte` (`:1-43` script, `:87-136` toolbar, `:197-199` dialog host).

**Interfaces:** Consumes `imports` (Task 4), `showUnreadFilter` (Task 3), `app.filesFilter`, `app.unread`.

### Steps

- [ ] **FileTree.svelte — imports.** Replace `SlidersHorizontal` import (`:2`) with:
```ts
  import Upload from "@lucide/svelte/icons/upload";
```
  and add after the existing `import { app } ...` line:
```ts
  import { imports } from "../lib/imports.svelte";
  import { showUnreadFilter } from "./filesHeader";
```

- [ ] **FileTree.svelte — header.** Replace the "Files" header block (`:104-114`) with the pinned header carrying the filter + icon-only Import:
```svelte
  <div class="tree-head files-head">
    <span class="ttl">Files</span>
    <div class="head-actions">
      {#if showUnreadFilter(app.unread.length, app.filesFilter)}
        <div class="filter" role="tablist" aria-label="Filter files">
          <button
            class="seg"
            class:on={app.filesFilter === "all"}
            role="tab"
            aria-selected={app.filesFilter === "all"}
            onclick={() => (app.filesFilter = "all")}
          >All</button>
          <button
            class="seg"
            class:on={app.filesFilter === "unread"}
            role="tab"
            aria-selected={app.filesFilter === "unread"}
            onclick={() => (app.filesFilter = "unread")}
          >Unread{#if app.unread.length > 0}&nbsp;·&nbsp;{app.unread.length}{/if}</button>
        </div>
      {/if}
      <button
        class="icon-btn"
        data-tooltip="Import file"
        aria-label="Import file"
        disabled={imports.importing}
        onclick={() => void imports.startImport()}
      >
        <Upload size={15} strokeWidth={1.75} />
      </button>
    </div>
  </div>
```

- [ ] **FileTree.svelte — styles.** Update `.tree-head` and add the sticky/header styles. Change the existing `.tree-head` rule (`:163-172`) to `justify-content: space-between` and add, in `<style>`:
```css
  /* The Files header stays visually pinned: sticky to the top of the scrolling
     .tree column so it never scrolls away with Favorites and the tree. A solid
     ground covers content sliding under it. */
  .files-head {
    position: sticky;
    top: 0;
    z-index: 5;
    background: var(--paper);
    justify-content: space-between;
    /* extend the ground to the column edges under the sticky row */
    margin: 0 -8px;
    padding-left: 18px;
    padding-right: 12px;
  }
  .files-head .ttl {
    flex: none;
  }
  .head-actions {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    /* header uppercases its text; keep control labels normal-case */
    text-transform: none;
    letter-spacing: 0;
    font-weight: 500;
  }
  .filter {
    display: inline-flex;
    border: 1px solid var(--border);
    border-radius: var(--radius-control);
    overflow: hidden;
  }
  .filter .seg {
    padding: 2px 8px;
    border: none;
    background: var(--surface);
    color: var(--ink-secondary);
    font-size: 11px;
    font-weight: 500;
  }
  .filter .seg:hover {
    background: var(--sunken);
  }
  .filter .seg.on {
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    color: var(--accent-deep);
    font-weight: 600;
  }
```
  (The existing `.icon-btn` + tooltip styles at `:199-238` are kept and reused as-is.)

- [ ] **FileTree.svelte — verify sticky ground.** During manual verification confirm the sticky header's `background` matches the tree column's actual background in both light and dark themes; if the column ground differs, set `.files-head { background: var(--surface); }` to match. Acceptance: no content bleeds through the pinned header while scrolling.

- [ ] **FilesScreen.svelte — script.** Remove the now-store-owned import state and helpers. Delete `staged`, `importing`, `importError` (`:23-26`) and the entire `startImport` function (`:29-43`); remove the now-unused imports `Upload` (`:5`), `open as openDialog` (`:7`), and `type ImportDto` from the api import (`:9`). Add:
```ts
  import { imports } from "../lib/imports.svelte";
```

- [ ] **FilesScreen.svelte — toolbar.** Replace the toolbar block (`:87-136`) with a slim bar keeping only "Mark all as viewed" (shown when unread):
```svelte
    <div class="toolbar">
      <div class="unread-controls">
        {#if app.unread.length > 0}
          <button
            class="mark-all"
            title="Mark every changed file as viewed"
            onclick={() => void app.markAllSeen()}
          >
            <CheckCheck size={14} strokeWidth={1.75} />
            <span>Mark all as viewed</span>
          </button>
        {/if}
      </div>
    </div>
```
  Remove the now-unused `.import-btn`, `.import-error`, `.filter`, `.seg` style rules (`:225-278`); keep `.toolbar`, `.unread-controls`, `.mark-all`.

- [ ] **FilesScreen.svelte — dialog host.** Replace the staged dialog block (`:197-199`) with the store-driven one:
```svelte
{#if imports.staged}
  <ImportDialog staged={imports.staged} close={() => imports.close()} />
{/if}
```

- [ ] **Type-check & manual verify:** `pnpm exec svelte-check --tsconfig ./tsconfig.json` passes. Per `verify`: the "Files" header stays pinned while scrolling a long tree; "Manage folders" is gone; the header Import button opens the picker + ImportDialog; the All/Unread filter appears only when something is unread and switches the tree; the right pane no longer shows Import/filter but still shows the tab strip and (when unread) "Mark all as viewed."

- [ ] **Commit:**
```bash
git add src/files/FileTree.svelte src/screens/FilesScreen.svelte
git commit -m "feat(files): pin Files header, move Import + filter into it (§3)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 6 — §9 Chat optimistic echo (pure module + test)

**First message vanishes:** `ChatsStore.send` (`chats.svelte.ts:86-99`) never appends the user message locally — the transcript grows only via backend `chat-message` events. Fix with an optimistic append carrying a **pending marker**, reconciled against the backend echo by content so it is never duplicated or dropped.

Diagnosis of the backend path (verified against `src-tauri/src/lib.rs:2982-2990`): `send_chat_message` DOES `append_chat_message(chat_id, "user", text)` and emits a `chat-message` (role `user`) synchronously before spawning the CLI, so `chat_transcript` reloads already include the user message. The fix is therefore frontend-side reconciliation; Task 8 adds a Rust test that pins the persistence guarantee so it can't silently regress.

**Files:** Create `src/lib/chatEcho.ts`, `src/lib/chatEcho.test.ts`.

**Interfaces (consumed by `chats.svelte.ts` and `ChatTranscript.svelte`):**
- `export interface PendingMessage extends ChatMessage { pending: true }`
- `export type TranscriptEntry = ChatMessage | PendingMessage`
- `export function isPending(m: TranscriptEntry): m is PendingMessage`
- `export function nextTempId(): number` — a unique, always-negative id so pending entries never collide with real DB ids.
- `export function optimisticUserMessage(chatId: string, content: string, now: number, tempId: number): PendingMessage`
- `export function reconcile(transcript: TranscriptEntry[], incoming: ChatMessage): TranscriptEntry[]`
- `export function dropPending(transcript: TranscriptEntry[], tempId: number): TranscriptEntry[]`

### Steps

- [ ] **Write the failing test.** `src/lib/chatEcho.test.ts`:
```ts
import { describe, expect, it } from "vitest";
import type { ChatMessage } from "./api";
import {
  dropPending,
  isPending,
  nextTempId,
  optimisticUserMessage,
  reconcile,
} from "./chatEcho";

const real = (id: number, role: ChatMessage["role"], content: string): ChatMessage => ({
  id,
  chatId: "c1",
  role,
  content,
  createdAt: 1000 + id,
});

describe("chat optimistic echo", () => {
  it("mints unique, always-negative temp ids", () => {
    const a = nextTempId();
    const b = nextTempId();
    expect(a).toBeLessThan(0);
    expect(b).toBeLessThan(0);
    expect(a).not.toBe(b);
  });

  it("marks an optimistic message pending", () => {
    const m = optimisticUserMessage("c1", "hello", 1234, nextTempId());
    expect(isPending(m)).toBe(true);
    expect(m.role).toBe("user");
    expect(m.content).toBe("hello");
  });

  it("replaces a pending echo with the backend user message (no dupe)", () => {
    const tid = nextTempId();
    let t: (ChatMessage | ReturnType<typeof optimisticUserMessage>)[] = [
      optimisticUserMessage("c1", "hello", 1234, tid),
    ];
    t = reconcile(t, real(7, "user", "hello"));
    expect(t).toHaveLength(1);
    expect(isPending(t[0])).toBe(false);
    expect(t[0].id).toBe(7);
  });

  it("ignores a duplicate backend echo of an already-reconciled message", () => {
    let t = reconcile([], real(7, "user", "hello"));
    t = reconcile(t, real(7, "user", "hello"));
    expect(t).toHaveLength(1);
  });

  it("appends assistant messages normally", () => {
    let t = reconcile([optimisticUserMessage("c1", "hi", 1, nextTempId())], real(8, "user", "hi"));
    t = reconcile(t, real(9, "assistant", "Hello!"));
    expect(t.map((m) => m.role)).toEqual(["user", "assistant"]);
  });

  it("only reconciles a pending message of the same content", () => {
    const t = reconcile(
      [optimisticUserMessage("c1", "first", 1, nextTempId())],
      real(5, "user", "different"),
    );
    // Pending 'first' stays; the unrelated backend user message appends.
    expect(t).toHaveLength(2);
    expect(isPending(t[0])).toBe(true);
    expect(t[1].content).toBe("different");
  });

  it("drops a pending message by temp id (send failure)", () => {
    const tid = nextTempId();
    const t = dropPending([optimisticUserMessage("c1", "oops", 1, tid)], tid);
    expect(t).toHaveLength(0);
  });
});
```

- [ ] **Run & see it fail:** `pnpm vitest run src/lib/chatEcho.test.ts` → `Cannot find module './chatEcho'`.

- [ ] **Implement.** `src/lib/chatEcho.ts`:
```ts
// Optimistic user-message echo for the chat transcript. send() appends a pending
// copy immediately so the message never vanishes; the backend's own chat-message
// echo then reconciles against it (by content) so it is neither duplicated nor
// dropped. Pending entries carry always-negative ids so they never collide with
// real DB ids and key distinctly until reconciled.
import type { ChatMessage } from "./api";

export interface PendingMessage extends ChatMessage {
  pending: true;
}

export type TranscriptEntry = ChatMessage | PendingMessage;

export function isPending(m: TranscriptEntry): m is PendingMessage {
  return (m as PendingMessage).pending === true;
}

let seq = 0;
/** Unique, always-negative temp id (real DB ids are positive). */
export function nextTempId(): number {
  seq -= 1;
  return seq;
}

export function optimisticUserMessage(
  chatId: string,
  content: string,
  now: number,
  tempId: number,
): PendingMessage {
  return { id: tempId, chatId, role: "user", content, createdAt: now, pending: true };
}

/** Merge a backend chat-message event into the transcript:
 *  - a real id already present → ignore (a re-fired echo);
 *  - a pending user message with the same content → replace it in place;
 *  - otherwise → append. */
export function reconcile(
  transcript: TranscriptEntry[],
  incoming: ChatMessage,
): TranscriptEntry[] {
  if (transcript.some((m) => !isPending(m) && m.id === incoming.id)) {
    return transcript;
  }
  if (incoming.role === "user") {
    const i = transcript.findIndex(
      (m) => isPending(m) && m.role === "user" && m.content === incoming.content,
    );
    if (i >= 0) {
      const next = transcript.slice();
      next[i] = incoming;
      return next;
    }
  }
  return [...transcript, incoming];
}

/** Remove a pending message (its send failed). */
export function dropPending(
  transcript: TranscriptEntry[],
  tempId: number,
): TranscriptEntry[] {
  return transcript.filter((m) => !(isPending(m) && m.id === tempId));
}
```

- [ ] **Run & pass:** `pnpm vitest run src/lib/chatEcho.test.ts` → all pass.

- [ ] **Commit:**
```bash
git add src/lib/chatEcho.ts src/lib/chatEcho.test.ts
git commit -m "feat(chat): pure optimistic-echo reconciliation for the transcript (§9)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 7 — §9 Wire optimistic echo + audit fixes into the chat store & UI

Wire `chatEcho` into `chats.svelte.ts`, fix focus-on-open, surface send failures near the composer, autoscroll only when near the bottom, and render pending messages quietly.

**Files:** Modify `src/lib/chats.svelte.ts` (`:13` transcript type, `:43-47` onChatMessage, `:86-99` send, archive), `src/chat/ChatDrawer.svelte` (focus + keySend), `src/chat/ChatTranscript.svelte` (pending render + autoscroll). Also creates `src/lib/keySend.ts` (Task 7a below).

### Task 7a — Enter/Shift+Enter pure helper (lock the audit finding)

- [ ] **Write the failing test.** `src/lib/keySend.test.ts`:
```ts
import { describe, expect, it } from "vitest";
import { shouldSend } from "./keySend";

describe("composer key handling", () => {
  it("sends on Enter", () => {
    expect(shouldSend({ key: "Enter", shiftKey: false })).toBe(true);
  });
  it("inserts a newline on Shift+Enter", () => {
    expect(shouldSend({ key: "Enter", shiftKey: true })).toBe(false);
  });
  it("ignores other keys", () => {
    expect(shouldSend({ key: "a", shiftKey: false })).toBe(false);
  });
});
```

- [ ] **Run & see it fail:** `pnpm vitest run src/lib/keySend.test.ts`.

- [ ] **Implement.** `src/lib/keySend.ts`:
```ts
// Composer key rule, extracted so it is test-covered (§9 audit): Enter sends,
// Shift+Enter inserts a newline. Any other key is not a send.
export function shouldSend(e: { key: string; shiftKey: boolean }): boolean {
  return e.key === "Enter" && !e.shiftKey;
}
```

- [ ] **Run & pass:** `pnpm vitest run src/lib/keySend.test.ts`.

### Task 7b — Store wiring

- [ ] **chats.svelte.ts imports.** After the existing `type ChatRow` import add:
```ts
import {
  dropPending,
  nextTempId,
  optimisticUserMessage,
  reconcile,
  type TranscriptEntry,
} from "./chatEcho";
```

- [ ] **chats.svelte.ts transcript type.** Change line 13 from `transcript = $state<ChatMessage[]>([]);` to:
```ts
  transcript = $state<TranscriptEntry[]>([]);
```

- [ ] **chats.svelte.ts onChatMessage.** Replace the handler (`:43-47`) with reconciliation:
```ts
    await api.onChatMessage((msg) => {
      if (msg.chatId === this.activeId) {
        this.transcript = reconcile(this.transcript, msg);
      }
    });
```

- [ ] **chats.svelte.ts send.** Replace `send` (`:86-99`) with the optimistic version:
```ts
  async send(text: string) {
    if (!this.activeId) return;
    this.sendError = null;
    const chatId = this.activeId;
    // Optimistically echo the user's message so it never vanishes; the backend's
    // chat-message event reconciles against this pending copy by content.
    const tempId = nextTempId();
    this.transcript = [
      ...this.transcript,
      optimisticUserMessage(chatId, text, Date.now(), tempId),
    ];
    // Attach the files the user has on screen as a weak, clearly-caveated hint
    // (the backend frames it as "not necessarily relevant"). Send all open tabs
    // plus which one is focused.
    const openFiles = app.fileTabs.map((t) => t.path);
    const focusedFile = app.openFile;
    try {
      await api.sendChatMessage(chatId, text, openFiles, focusedFile);
    } catch (e) {
      // The send failed: pull the pending echo and show why, so the message
      // doesn't sit there looking sent.
      this.transcript = dropPending(this.transcript, tempId);
      this.sendError = String(e);
    }
  }
```

- [ ] **chats.svelte.ts archive-active-chat state (§9 audit).** The `onChatUpdated` archived branch (`:34-37`) already reselects `rows[0]` and, when the last chat is archived, sets `activeId = null`. Add a guard so a stale transcript never lingers after the active chat is archived: at the end of that archived branch, when `this.activeId` becomes `null`, clear the transcript. Replace `:34-38`:
```ts
      if (row.archived) {
        if (i >= 0) this.rows = this.rows.toSpliced(i, 1);
        if (this.activeId === row.id) {
          this.activeId = this.rows[0]?.id ?? null;
          if (this.activeId) void this.select(this.activeId);
          else this.transcript = [];
        }
        return;
      }
```

### Task 7c — ChatDrawer focus + keySend

- [ ] **ChatDrawer.svelte imports.** Add:
```ts
  import { shouldSend } from "../lib/keySend";
```

- [ ] **ChatDrawer.svelte onKeydown.** Replace `onKeydown` (`:123-128`) to use the tested helper:
```ts
  // Enter sends, Shift+Enter inserts a newline (see keySend). "/" is an ordinary
  // character now — the terminal is revealed by the explicit Terminal toggle.
  function onKeydown(e: KeyboardEvent) {
    if (shouldSend(e)) {
      e.preventDefault();
      void submit();
    }
  }
```

- [ ] **ChatDrawer.svelte focus-on-open.** The composer `<textarea>` (`:296-302`) is bound to `replyEl`. Add an effect that focuses it whenever the drawer shows a typeable chat — on open, newChat, select, and exit-terminal. Insert after the auto-grow `$effect` (`:78-81`):
```ts
  // Focus the composer whenever it becomes the thing to type into: a chat is
  // active, the drawer is open, and we're not in terminal mode. Re-runs on chat
  // switch and on leaving the terminal, so the user can type immediately.
  $effect(() => {
    const typeable =
      chats.open && chats.activeId !== null && !inTerminal &&
      chats.active?.kind !== "ingest" && chats.active?.kind !== "research";
    // Read activeId + inTerminal so the effect re-runs on select/exit-terminal.
    void chats.activeId;
    void inTerminal;
    if (typeable && replyEl) {
      // Next microtask: the textarea may have just mounted after a branch swap.
      queueMicrotask(() => replyEl?.focus());
    }
  });
```
  Note: `inTerminal` and `chats.active` are declared just below at `:92-99`; move this `$effect` to sit AFTER those `$derived` declarations (after line 99) so it reads live values. Place it immediately before `cancelResearch` (`:101`).

### Task 7d — ChatTranscript pending render + near-bottom autoscroll

- [ ] **ChatTranscript.svelte pending render.** The user branch (`:38-39`) renders `msg.content`. Add a quiet pending affordance. Replace:
```svelte
    {#if msg.role === "user"}
      <div class="bubble user" class:pending={"pending" in msg && msg.pending}>{msg.content}</div>
```
  and add to `<style>`:
```css
  .bubble.user.pending {
    opacity: 0.6;
  }
```

- [ ] **ChatTranscript.svelte autoscroll (§9 audit).** Current effect (`:8-12`) force-scrolls to bottom on every transcript change, which yanks a user reading history. Scroll only when already near the bottom (or on the first paint). Replace:
```svelte
  let scroller = $state<HTMLDivElement | null>(null);

  // Follow the conversation, but only when the user is already near the bottom —
  // don't yank them down while they're reading earlier messages.
  $effect(() => {
    const len = chats.transcript.length;
    void len;
    const el = scroller;
    if (!el) return;
    const nearBottom =
      el.scrollHeight - el.scrollTop - el.clientHeight < 120;
    if (nearBottom) el.scrollTop = el.scrollHeight;
  });
```

- [ ] **Type-check & manual verify (drive the real app per `verify`).** Run the §9 usability pass on the live app and confirm each acceptance criterion:
  - **First message:** new chat → type → Enter → the message appears immediately (pending, faded) and stays put after the backend echo lands — no duplicate, no vanish. Reload the chat (select away and back) → the user message is still there (backend persistence).
  - **Focus on open:** opening the drawer, `New chat`, selecting a chat tab, and leaving the terminal each land keyboard focus in the composer (type without clicking).
  - **Enter vs Shift+Enter:** Enter sends; Shift+Enter inserts a newline (grows the composer).
  - **Send-failure visibility:** with the CLI unavailable (or a forced error), the pending message is pulled and `sendError` shows near the composer (`.send-error`, `ChatDrawer.svelte:263-265`).
  - **Suggested prompts:** clicking a starter (`ChatTranscript.svelte:31-33`) sends it and it echoes optimistically like any message.
  - **Autoscroll:** streaming a long answer keeps the view pinned to the bottom; scrolling up to read history is NOT interrupted by new deltas.
  - **Archive active chat:** archiving the active chat switches to the next chat (or clears to the empty state with no stale transcript).
  Anything structural found beyond these gets written up for the next wave, not fixed here.

- [ ] **Commit:**
```bash
git add src/lib/keySend.ts src/lib/keySend.test.ts src/lib/chats.svelte.ts src/chat/ChatDrawer.svelte src/chat/ChatTranscript.svelte
git commit -m "fix(chat): optimistic echo, focus-on-open, autoscroll + audit fixes (§9)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 8 — §10 Curated model catalog in ken-core (retire discovery)

Replace runtime HF-repo discovery in `crates/ken-core/src/model.rs` with a curated, category-tagged catalog. Keep the download/verify/progress/atomic-install plumbing untouched. Add a persisted per-category selection.

**Files:** Modify `crates/ken-core/src/model.rs`.

**Interfaces (consumed by `src-tauri/src/lib.rs`, `transcript.rs`, and the language-model plan):**
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelCategory { Transcription, Language }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelTier { Recommended, Advanced }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    pub category: ModelCategory,
    pub tier: ModelTier,
    pub blurb: &'static str,
    pub spec: ModelSpec,
}

pub fn catalog() -> Vec<CatalogEntry>;                    // Transcription now; Language appended by the other plan
pub fn category_specs(category: ModelCategory) -> Vec<ModelSpec>;
pub fn find_spec(id: &str) -> Option<ModelSpec>;
pub fn selected(base_dir: &Path, category: ModelCategory) -> ModelSpec;             // selection, else the category's Recommended
pub fn selected_model_path(base_dir: &Path, category: ModelCategory) -> Option<PathBuf>; // installed selection → any installed → None
pub fn set_selected(base_dir: &Path, category: ModelCategory, id: &str) -> Result<()>;
```
`ModelSpec` is unchanged (download identity). `blurb`/`category`/`tier` live on `CatalogEntry`.

> **Resolved ambiguity:** the prompt named `selected(category)` "persisted in user_state", but `user_state` is *per-project* while models install machine-wide under `base_dir/whisper` and are shared across projects. Persisting a machine-wide selection per-project would force re-selection in every project. Resolution: persist in a machine-level `base_dir/models/selection.json` (`ModelSelection`), and give `selected`/`set_selected` a `base_dir` parameter. The consumed catalog *types* and the fallback semantics are exactly as specified.

### Steps

- [ ] **Write the failing tests.** Add to `model.rs`'s `#[cfg(test)] mod tests` (replacing the discovery tests `listing_parse_*`, `discover_falls_back_*`, `discover_uses_the_listing_*`, which are removed with the discovery code):
```rust
    #[test]
    fn catalog_has_the_transcription_pair() {
        let cat = catalog();
        let trans: Vec<_> = cat
            .iter()
            .filter(|e| e.category == ModelCategory::Transcription)
            .collect();
        assert_eq!(trans.len(), 2, "one recommended + one advanced transcription model");
        let rec = trans.iter().find(|e| e.tier == ModelTier::Recommended).unwrap();
        assert_eq!(rec.spec.file, "ggml-base.en.bin");
        assert_eq!(rec.spec.name, "Whisper Base (English)");
        assert_eq!(rec.spec.expected_bytes, 147_951_465);
        assert!(rec.spec.recommended);
        let adv = trans.iter().find(|e| e.tier == ModelTier::Advanced).unwrap();
        assert_eq!(adv.spec.file, "ggml-large-v3-turbo.bin");
        assert_eq!(adv.spec.name, "Whisper Large v3 Turbo");
        assert!(!adv.spec.recommended);
        // Blurbs are the exact Settings copy.
        assert_eq!(rec.blurb, "fast, accurate for meetings");
        assert_eq!(adv.blurb, "best accuracy, understands more languages, slower");
    }

    #[test]
    fn recommended_still_resolves_to_base_english() {
        let r = recommended();
        assert_eq!(r.file, "ggml-base.en.bin");
        assert!(r.recommended);
        assert!(r.url.contains("ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin"));
    }

    #[test]
    fn find_spec_locates_catalog_models_and_rejects_unknown() {
        assert_eq!(find_spec("ggml-large-v3-turbo.bin").unwrap().name, "Whisper Large v3 Turbo");
        assert!(find_spec("ggml-nonsense.bin").is_none());
    }

    #[test]
    fn selected_defaults_to_recommended_then_honours_a_valid_choice() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        // No selection saved → the category's Recommended.
        assert_eq!(selected(base, ModelCategory::Transcription).file, "ggml-base.en.bin");
        // A valid choice persists and is returned.
        set_selected(base, ModelCategory::Transcription, "ggml-large-v3-turbo.bin").unwrap();
        assert_eq!(selected(base, ModelCategory::Transcription).file, "ggml-large-v3-turbo.bin");
        // An unknown persisted id degrades back to Recommended (never a broken spec).
        set_selected(base, ModelCategory::Transcription, "ggml-bogus.bin").unwrap();
        assert_eq!(selected(base, ModelCategory::Transcription).file, "ggml-base.en.bin");
    }

    #[test]
    fn selected_model_path_prefers_selection_then_any_installed() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        // Nothing installed → None (gating shows the download help).
        assert!(selected_model_path(base, ModelCategory::Transcription).is_none());

        // Install the ADVANCED model only, but leave the selection at Recommended.
        let adv = find_spec("ggml-large-v3-turbo.bin").unwrap();
        let p = target_path(base, &adv);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"weights").unwrap();
        // Selection (base.en) isn't installed → fall back to the installed advanced one.
        assert_eq!(selected_model_path(base, ModelCategory::Transcription), Some(p.clone()));

        // Now select the installed advanced model → its path.
        set_selected(base, ModelCategory::Transcription, "ggml-large-v3-turbo.bin").unwrap();
        assert_eq!(selected_model_path(base, ModelCategory::Transcription), Some(p));
    }

    #[test]
    fn model_selection_roundtrips_and_defaults_on_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        set_selected(base, ModelCategory::Transcription, "ggml-large-v3-turbo.bin").unwrap();
        let loaded = ModelSelection::load(base);
        assert_eq!(loaded.transcription.as_deref(), Some("ggml-large-v3-turbo.bin"));
        // Corrupt file → defaults, never an error.
        std::fs::write(base.join("models").join("selection.json"), "{ not json").unwrap();
        assert_eq!(ModelSelection::load(base), ModelSelection::default());
    }
```

- [ ] **Run & see them fail:** `cargo test -p ken-core model::` → compile errors / missing items.

- [ ] **Implement.** In `model.rs`:
  1. **Remove discovery:** delete `tree_url`, `resolve_url`'s discovery neighbours — specifically delete `parse_model_listing`, `is_model_file`, `discover_models`, `fetch_listing`, `TreeEntry`, `Lfs`, and the module doc paragraph about discovery. **Keep** `REPO`, `resolve_url`, `ByteSource`, `HttpSource`, `verify_download`, `download_to`, `progress_percent`, `ProgressThrottle`, `target_path`, `installed_size`, `remove`, `display_name`/`capitalize` (still handy), and `ModelSpec`.
  2. **Keep `RECOMMENDED_FILE`/`RECOMMENDED_BYTES`** (lib.rs references `RECOMMENDED_FILE`); redefine `recommended()` off the catalog (below). Update the module doc comment (`:1-13`) to describe a curated catalog, not discovery.
  3. **Add constants + catalog** near the top (after `RECOMMENDED_BYTES`):
```rust
use std::path::Path; // already imported alongside PathBuf

pub const WHISPER_LARGE_TURBO_FILE: &str = "ggml-large-v3-turbo.bin";
/// Display/offline-floor size only (~1.6 GB); real downloads verify against the
/// server's Content-Length.
pub const WHISPER_LARGE_TURBO_BYTES: u64 = 1_624_555_275;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelCategory {
    Transcription,
    Language,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelTier {
    Recommended,
    Advanced,
}

/// One curated, downloadable model with its category, tier, and Settings blurb.
/// `spec` is the unchanged download identity (id/file/url/size/recommended).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    pub category: ModelCategory,
    pub tier: ModelTier,
    pub blurb: &'static str,
    pub spec: ModelSpec,
}

fn spec(file: &str, name: &str, bytes: u64, recommended: bool) -> ModelSpec {
    ModelSpec {
        id: file.to_string(),
        name: name.to_string(),
        file: file.to_string(),
        url: resolve_url(file),
        expected_bytes: bytes,
        recommended,
    }
}

/// The transcription pair. Recommended = Whisper Base (English); Advanced =
/// Whisper Large v3 Turbo.
fn transcription_catalog() -> Vec<CatalogEntry> {
    vec![
        CatalogEntry {
            category: ModelCategory::Transcription,
            tier: ModelTier::Recommended,
            blurb: "fast, accurate for meetings",
            spec: spec(RECOMMENDED_FILE, "Whisper Base (English)", RECOMMENDED_BYTES, true),
        },
        CatalogEntry {
            category: ModelCategory::Transcription,
            tier: ModelTier::Advanced,
            blurb: "best accuracy, understands more languages, slower",
            spec: spec(WHISPER_LARGE_TURBO_FILE, "Whisper Large v3 Turbo", WHISPER_LARGE_TURBO_BYTES, false),
        },
    ]
}

/// Language models (Answers & Map) are appended by the local-LLM plan; the
/// category and structure exist now so that plan only adds entries here.
fn language_catalog() -> Vec<CatalogEntry> {
    Vec::new()
}

/// Every curated model, in display order (Transcription, then Language).
pub fn catalog() -> Vec<CatalogEntry> {
    let mut entries = transcription_catalog();
    entries.extend(language_catalog());
    entries
}

/// The specs in one category, in catalog order.
pub fn category_specs(category: ModelCategory) -> Vec<ModelSpec> {
    catalog()
        .into_iter()
        .filter(|e| e.category == category)
        .map(|e| e.spec)
        .collect()
}

/// Locate a spec by its file-name id across the whole catalog.
pub fn find_spec(id: &str) -> Option<ModelSpec> {
    catalog().into_iter().map(|e| e.spec).find(|s| s.id == id)
}
```
  4. **Redefine `recommended()`** (replacing the old body at `:59-68`):
```rust
/// The recommended transcription model — the offline fallback and the model the
/// transcript feature gates on when nothing is selected/installed.
pub fn recommended() -> ModelSpec {
    category_specs(ModelCategory::Transcription)
        .into_iter()
        .find(|s| s.recommended)
        .expect("transcription catalog has a recommended entry")
}
```
  5. **Add the selection store + resolvers** (after `remove`):
```rust
/// Machine-level, per-category model selection. Persisted under
/// `base_dir/models/selection.json` (models install machine-wide, so the choice
/// is machine-wide too). Best-effort like the registry: missing/corrupt → default.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModelSelection {
    #[serde(default)]
    pub transcription: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
}

fn selection_path(base_dir: &Path) -> PathBuf {
    base_dir.join("models").join("selection.json")
}

impl ModelSelection {
    pub fn load(base_dir: &Path) -> ModelSelection {
        match std::fs::read_to_string(selection_path(base_dir)) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            Err(_) => ModelSelection::default(),
        }
    }

    pub fn save(&self, base_dir: &Path) -> Result<()> {
        let path = selection_path(base_dir);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| Error::Other(e.to_string()))?;
        std::fs::write(&path, json + "\n").map_err(|e| Error::io(&path, e))
    }

    fn get(&self, category: ModelCategory) -> Option<&str> {
        match category {
            ModelCategory::Transcription => self.transcription.as_deref(),
            ModelCategory::Language => self.language.as_deref(),
        }
    }

    fn set(&mut self, category: ModelCategory, id: &str) {
        match category {
            ModelCategory::Transcription => self.transcription = Some(id.to_string()),
            ModelCategory::Language => self.language = Some(id.to_string()),
        }
    }
}

/// The spec the UI treats as selected for a category: the persisted choice if it
/// is a real catalog entry, else the category's Recommended.
pub fn selected(base_dir: &Path, category: ModelCategory) -> ModelSpec {
    let specs = category_specs(category);
    if let Some(id) = ModelSelection::load(base_dir).get(category) {
        if let Some(hit) = specs.iter().find(|s| s.id == id) {
            return hit.clone();
        }
    }
    specs
        .into_iter()
        .find(|s| s.recommended)
        .expect("every category has a recommended entry")
}

/// The on-disk path of the model to actually USE for a category: the selected
/// model if installed, else any installed model in the category, else None.
pub fn selected_model_path(base_dir: &Path, category: ModelCategory) -> Option<PathBuf> {
    let chosen = selected(base_dir, category);
    if installed_size(base_dir, &chosen).is_some() {
        return Some(target_path(base_dir, &chosen));
    }
    category_specs(category)
        .into_iter()
        .find(|s| installed_size(base_dir, s).is_some())
        .map(|s| target_path(base_dir, &s))
}

/// Persist a category's selection.
pub fn set_selected(base_dir: &Path, category: ModelCategory, id: &str) -> Result<()> {
    let mut sel = ModelSelection::load(base_dir);
    sel.set(category, id);
    sel.save(base_dir)
}
```
  6. **Update the test `FakeSource`** to drop the `listing` field and its `/api/models/` branch (only the download body is exercised now). Update `spec_for` to keep working (it already builds a `ModelSpec`). Remove any now-unused `use` for `Deserialize` if it becomes unused (it stays used by the new enums via `serde::Deserialize`).

- [ ] **Run & pass:** `cargo test -p ken-core model::` → all pass (including the retained download/verify/progress tests).

- [ ] **Commit:**
```bash
git add crates/ken-core/src/model.rs
git commit -m "feat(model): curated category/tier catalog + persisted selection; retire discovery (§10)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 9 — §10 Use the selected transcription model; lock backend persistence

Make `transcript.rs`/`lib.rs` use whichever transcription model is selected (with installed fallback) instead of the hardcoded `MODEL_FILE`, and add the Rust test that pins the §9 chat-message persistence guarantee.

**Files:** Modify `crates/ken-core/src/transcript.rs` (call sites use `model::selected_model_path`), `src-tauri/src/lib.rs` (`:1400`, `:1453` model resolution). Add a persistence test near `send_chat_message`/`chat_messages` (locate the `Db::append_chat_message` test module in `crates/ken-core/src/*` — likely `db.rs` or `chat` storage).

### Steps

- [ ] **lib.rs transcription call sites.** At `:1400` and `:1453`, replace:
```rust
    let model = transcript::model_path(&base);
```
  with the selection-aware resolution (falls back to the default recommended path so the existing gate still shows the download help when nothing is installed):
```rust
    let model = model::selected_model_path(&base, model::ModelCategory::Transcription)
        .unwrap_or_else(|| transcript::model_path(&base));
```
  Add `use ken_core::model::ModelCategory;` if not already in scope, or use the fully-qualified path as written. `transcript.rs`'s `MODEL_FILE` and `model_path` stay (they name the recommended file for the download target + gating help) but are no longer the only model — matching "the Whisper file constant stops being load-bearing."

- [ ] **Backend persistence test (§9 diagnosis lock-in).** Find the chat message store test module (grep `append_chat_message` under `crates/ken-core/src`). Add a test asserting a persisted user message survives a reload:
```rust
    #[test]
    fn user_message_persists_and_reloads() {
        // A user turn is appended and returned by chat_messages, so a transcript
        // reload always includes the user's own message (the §9 vanish is purely
        // a frontend echo-timing issue, not lost persistence).
        let dir = tempfile::tempdir().unwrap();
        let mut db = Db::open(dir.path(), uuid::Uuid::new_v4()).unwrap();
        // (create the chat row the same way the existing tests in this module do)
        let chat_id = "c-persist";
        // ...upsert a chat row for chat_id per the module's helper...
        db.append_chat_message(chat_id, "user", "hello there", 100).unwrap();
        let msgs = db.chat_messages(chat_id).unwrap();
        assert!(msgs.iter().any(|m| m.role == "user" && m.content == "hello there"));
    }
```
  Adapt the row-creation to the module's existing test helpers (match how neighbouring tests build a chat). If an equivalent assertion already exists in that module, note it in the commit instead of duplicating.

- [ ] **Run & pass:** `cargo test -p ken-core transcript::` and `cargo test -p ken-core <chat-store-module>::user_message_persists_and_reloads` → pass. Full crate: `cargo test -p ken-core`.

- [ ] **Commit:**
```bash
git add crates/ken-core/src/transcript.rs src-tauri/src/lib.rs crates/ken-core/src/*.rs
git commit -m "feat(transcript): use the selected transcription model; lock chat persistence (§9/§10)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 10 — §10 Rewire Tauri model commands to the catalog

Replace the discovery-backed commands with catalog-backed ones, extend the DTO with `category`/`tier`/`blurb`/`selected`, and add a `set_model_selection` command. Remove the `model_cache` state field.

**Files:** Modify `src-tauri/src/lib.rs` (`ModelStatusDto` `:1517-1525`, `status_dto` `:1542-1552`, `discovered_specs` `:1554-1565` (delete), `model_status` `:1570-1574`, `list_models` `:1579-1587`, `download_model` `:1595-1661`, `remove_model` `:1663-1677`, state field `:102` + init `:3380`, command registration `:3432-3433`), `src/lib/api.ts` (`ModelStatus` `:319-329`, add `setModelSelection`).

### Steps

- [ ] **DTO.** Extend `ModelStatusDto` (`:1517-1525`):
```rust
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ModelStatusDto {
    id: String,
    name: String,
    installed: bool,
    size_bytes: Option<u64>,
    expected_bytes: u64,
    recommended: bool,
    /// "transcription" | "language"
    category: String,
    /// "recommended" | "advanced"
    tier: String,
    blurb: String,
    /// Whether this is the selected model for its category.
    selected: bool,
}
```

- [ ] **status_dto from a catalog entry.** Replace `status_dto` (`:1542-1552`) so it takes an entry + the selected id:
```rust
fn status_dto(
    base_dir: &std::path::Path,
    entry: &model::CatalogEntry,
    selected_id: &str,
) -> ModelStatusDto {
    let size = model::installed_size(base_dir, &entry.spec);
    ModelStatusDto {
        id: entry.spec.id.clone(),
        name: entry.spec.name.clone(),
        installed: size.is_some(),
        size_bytes: size,
        expected_bytes: entry.spec.expected_bytes,
        recommended: entry.spec.recommended,
        category: match entry.category {
            model::ModelCategory::Transcription => "transcription".into(),
            model::ModelCategory::Language => "language".into(),
        },
        tier: match entry.tier {
            model::ModelTier::Recommended => "recommended".into(),
            model::ModelTier::Advanced => "advanced".into(),
        },
        blurb: entry.blurb.to_string(),
        selected: entry.spec.id == selected_id,
    }
}
```

- [ ] **Delete `discovered_specs`** (`:1554-1565`) and remove the `model_cache` field (`:102`) and its init (`:3380` `model_cache: Arc::new(Mutex::new(None)),`).

- [ ] **model_status** (`:1570-1574`) — unchanged behavior (recommended-only, offline):
```rust
#[tauri::command]
fn model_status(state: State<SharedState>) -> CmdResult<ModelStatusDto> {
    let base = { state.lock().unwrap().base_dir.clone() };
    let rec = model::catalog()
        .into_iter()
        .find(|e| e.category == model::ModelCategory::Transcription && e.spec.recommended)
        .expect("recommended transcription model");
    let selected_id = model::selected(&base, model::ModelCategory::Transcription).id;
    Ok(status_dto(&base, &rec, &selected_id))
}
```

- [ ] **list_models** (`:1579-1587`) — catalog-backed, offline, per-category selected flag:
```rust
#[tauri::command]
fn list_models(state: State<SharedState>) -> CmdResult<Vec<ModelStatusDto>> {
    let base = { state.lock().unwrap().base_dir.clone() };
    let sel_trans = model::selected(&base, model::ModelCategory::Transcription).id;
    let sel_lang = model::selected(&base, model::ModelCategory::Language).id;
    Ok(model::catalog()
        .iter()
        .map(|e| {
            let selected_id = match e.category {
                model::ModelCategory::Transcription => &sel_trans,
                model::ModelCategory::Language => &sel_lang,
            };
            status_dto(&base, e, selected_id)
        })
        .collect())
}
```

- [ ] **download_model** (`:1595-1661`) — resolve the spec via `find_spec`, drop the `cache` capture:
```rust
#[tauri::command]
fn download_model(app: AppHandle, state: State<SharedState>, id: String) -> CmdResult<()> {
    let (base, downloads) = {
        let guard = state.lock().unwrap();
        (guard.base_dir.clone(), guard.model_downloads.clone())
    };
    {
        let mut in_flight = downloads.lock().unwrap();
        if !in_flight.insert(id.clone()) {
            return Err("This model is already downloading.".into());
        }
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let Some(spec) = model::find_spec(&id) else {
            let _ = app.emit(
                "model-download-error",
                ModelError { id: id.clone(), message: "Unknown model.".into() },
            );
            downloads.lock().unwrap().remove(&id);
            return;
        };
        let mut throttle = model::ProgressThrottle::new();
        let start = Instant::now();
        let progress_app = app.clone();
        let progress_id = id.clone();
        let on_progress = |downloaded: u64, total: u64| {
            let now_ms = start.elapsed().as_millis() as u64;
            let done = total > 0 && downloaded >= total;
            if done || throttle.should_emit(downloaded, total, now_ms) {
                let _ = progress_app.emit(
                    "model-download-progress",
                    ModelProgress { id: progress_id.clone(), downloaded, total },
                );
            }
        };
        match model::download_to(&model::HttpSource, &spec, &base, on_progress) {
            Ok(()) => {}
            Err(e) => {
                let _ = app.emit(
                    "model-download-error",
                    ModelError { id: id.clone(), message: e.to_string() },
                );
            }
        }
        downloads.lock().unwrap().remove(&id);
    });
    Ok(())
}
```

- [ ] **remove_model** (`:1663-1677`) — prefer the catalog spec, fall back to a minimal one:
```rust
#[tauri::command]
fn remove_model(state: State<SharedState>, id: String) -> CmdResult<()> {
    let base = { state.lock().unwrap().base_dir.clone() };
    let spec = model::find_spec(&id).unwrap_or_else(|| model::ModelSpec {
        id: id.clone(),
        name: id.clone(),
        file: id.clone(),
        url: String::new(),
        expected_bytes: 0,
        recommended: false,
    });
    model::remove(&base, &spec).map_err(err)
}
```

- [ ] **New command `set_model_selection`.** Add after `remove_model`:
```rust
/// Persist the user's chosen model for a category ("transcription" | "language").
#[tauri::command]
fn set_model_selection(state: State<SharedState>, category: String, id: String) -> CmdResult<()> {
    let base = { state.lock().unwrap().base_dir.clone() };
    let cat = match category.as_str() {
        "transcription" => model::ModelCategory::Transcription,
        "language" => model::ModelCategory::Language,
        other => return Err(format!("unknown model category: {other}")),
    };
    model::set_selected(&base, cat, &id).map_err(err)
}
```
  Register it in the `invoke_handler!` list beside `model_status, list_models` (`:3432-3433`): add `set_model_selection,`.

- [ ] **api.ts.** Extend `ModelStatus` (`:319-329`):
```ts
export interface ModelStatus {
  id: string;
  name: string;
  installed: boolean;
  sizeBytes: number | null;
  expectedBytes: number;
  recommended: boolean;
  /** "transcription" | "language" */
  category: "transcription" | "language";
  /** "recommended" | "advanced" */
  tier: "recommended" | "advanced";
  blurb: string;
  /** Whether this is the selected model for its category. */
  selected: boolean;
}
```
  Add the command near `removeModel` (`:427-429` area):
```ts
  setModelSelection: (category: "transcription" | "language", id: string) =>
    invoke<void>("set_model_selection", { category, id }),
```

- [ ] **Build:** `cargo build -p ken-tauri` (or the app's tauri crate name) → compiles; `pnpm exec svelte-check` → no type errors from the new `ModelStatus` fields (Settings consumes them in Task 12).

- [ ] **Commit:**
```bash
git add src-tauri/src/lib.rs src/lib/api.ts
git commit -m "feat(model): catalog-backed Tauri commands + selection command (§10)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 11 — §10 Watched-folders tri-state tree (pure module + test)

Replace the flat indented checkbox list (`SettingsScreen.svelte:187-205`) logic with a roots-first tree and tri-state checkboxes. Extract the tree-build, tri-state, and toggle logic so it is unit-tested; the exclusion model is prefix-based.

**Files:** Create `src/lib/folderTree.ts`, `src/lib/folderTree.test.ts`.

**Interfaces (consumed by `SettingsScreen.svelte`):**
```ts
export interface FolderNode { relPath: string; name: string; children: FolderNode[] }
export type TriState = "checked" | "unchecked" | "indeterminate";
export function buildFolderTree(folders: { relPath: string }[]): FolderNode[];
export function isExcluded(relPath: string, excluded: Set<string>): boolean;
export function folderTriState(relPath: string, excluded: Set<string>): TriState;
export function toggleFolder(relPath: string, currentlyExcluded: boolean, excluded: Iterable<string>): string[];
```

### Steps

- [ ] **Write the failing test.** `src/lib/folderTree.test.ts`:
```ts
import { describe, expect, it } from "vitest";
import {
  buildFolderTree,
  folderTriState,
  isExcluded,
  toggleFolder,
} from "./folderTree";

describe("watched-folders tree", () => {
  const folders = [
    { relPath: "Meetings" },
    { relPath: "Meetings/2026" },
    { relPath: "Meetings/2026/Q1" },
    { relPath: "Research" },
    { relPath: "Research/Data" },
  ];

  it("builds a roots-first nested tree", () => {
    const tree = buildFolderTree(folders);
    expect(tree.map((n) => n.relPath)).toEqual(["Meetings", "Research"]);
    const meetings = tree[0];
    expect(meetings.name).toBe("Meetings");
    expect(meetings.children.map((n) => n.relPath)).toEqual(["Meetings/2026"]);
    expect(meetings.children[0].children.map((n) => n.relPath)).toEqual(["Meetings/2026/Q1"]);
  });

  it("treats exclusion as prefix-based (self or ancestor)", () => {
    const ex = new Set(["Meetings/2026"]);
    expect(isExcluded("Meetings/2026", ex)).toBe(true);
    expect(isExcluded("Meetings/2026/Q1", ex)).toBe(true); // descendant of an excluded folder
    expect(isExcluded("Meetings", ex)).toBe(false);
    expect(isExcluded("Research", ex)).toBe(false);
  });

  it("computes tri-state: checked / unchecked / indeterminate", () => {
    const ex = new Set(["Meetings/2026"]);
    // Meetings itself isn't excluded, but a descendant is → indeterminate.
    expect(folderTriState("Meetings", ex)).toBe("indeterminate");
    // The excluded folder and everything under it → unchecked.
    expect(folderTriState("Meetings/2026", ex)).toBe("unchecked");
    expect(folderTriState("Meetings/2026/Q1", ex)).toBe("unchecked");
    // A clean subtree → checked.
    expect(folderTriState("Research", ex)).toBe("checked");
  });

  it("excludes a whole subtree by adding the folder (prefix model)", () => {
    const next = toggleFolder("Meetings", false, []);
    expect(next).toEqual(["Meetings"]);
  });

  it("re-includes a folder by dropping it and any excluded descendants", () => {
    const next = toggleFolder("Meetings", true, ["Meetings", "Meetings/2026", "Research/Data"]);
    expect(next.sort()).toEqual(["Research/Data"]);
  });
});
```

- [ ] **Run & see it fail:** `pnpm vitest run src/lib/folderTree.test.ts`.

- [ ] **Implement.** `src/lib/folderTree.ts`:
```ts
// Watched-folders tree logic (§10). The exclusion model is prefix-based: an entry
// in the exclusion set excludes that folder and its whole subtree. Tri-state:
//   checked       = folder and everything under it watched
//   unchecked     = folder excluded (self or an ancestor is)
//   indeterminate = folder watched, but some descendant is excluded

export interface FolderNode {
  relPath: string;
  name: string;
  children: FolderNode[];
}

export type TriState = "checked" | "unchecked" | "indeterminate";

/** Build a roots-first nested tree from a flat, path-sorted folder list. */
export function buildFolderTree(folders: { relPath: string }[]): FolderNode[] {
  const byPath = new Map<string, FolderNode>();
  const roots: FolderNode[] = [];
  // Sort by path so parents are created before children.
  const sorted = [...folders].sort((a, b) => a.relPath.localeCompare(b.relPath));
  for (const f of sorted) {
    const node: FolderNode = {
      relPath: f.relPath,
      name: f.relPath.split("/").pop() ?? f.relPath,
      children: [],
    };
    byPath.set(f.relPath, node);
    const slash = f.relPath.lastIndexOf("/");
    const parent = slash >= 0 ? byPath.get(f.relPath.slice(0, slash)) : undefined;
    if (parent) parent.children.push(node);
    else roots.push(node);
  }
  return roots;
}

/** Excluded if the folder itself or any ancestor is in the exclusion set. */
export function isExcluded(relPath: string, excluded: Set<string>): boolean {
  if (excluded.has(relPath)) return true;
  const parts = relPath.split("/");
  for (let i = 1; i < parts.length; i++) {
    if (excluded.has(parts.slice(0, i).join("/"))) return true;
  }
  return false;
}

export function folderTriState(relPath: string, excluded: Set<string>): TriState {
  if (isExcluded(relPath, excluded)) return "unchecked";
  const prefix = relPath + "/";
  for (const e of excluded) {
    if (e.startsWith(prefix)) return "indeterminate";
  }
  return "checked";
}

/** The new exclusion list after toggling a folder. Excluding adds the folder
 *  (its prefix covers the subtree); re-including drops it and any excluded
 *  descendants. Mirrors the previous SettingsScreen.toggleFolder logic. */
export function toggleFolder(
  relPath: string,
  currentlyExcluded: boolean,
  excluded: Iterable<string>,
): string[] {
  const ex = new Set(excluded);
  if (currentlyExcluded) {
    for (const e of [...ex]) {
      if (e === relPath || e.startsWith(relPath + "/")) ex.delete(e);
    }
  } else {
    ex.add(relPath);
  }
  return [...ex];
}
```

- [ ] **Run & pass:** `pnpm vitest run src/lib/folderTree.test.ts`.

- [ ] **Commit:**
```bash
git add src/lib/folderTree.ts src/lib/folderTree.test.ts
git commit -m "feat(settings): pure watched-folders tri-state tree logic (§10)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 12 — §10 Settings screen: rhythm, Appearance segmented, Offline Models, tri-state tree

Rebuild `SettingsScreen.svelte` per §10: three quiet uppercase section headings with generous separation, a one-row Appearance segmented control, an Offline Models card (Recommended/Advanced radio pair per category, inline download, quiet Remove for the non-selected installed file, promise copy up front), and the tri-state watched-folders tree. No new fonts/hues; quiet motion.

**Files:** Modify `src/screens/SettingsScreen.svelte` (whole file).

**Interfaces:** Consumes `folderTree` (Task 11), `imports`/theme unchanged, `api.listModels`/`api.downloadModel`/`api.removeModel`/`api.setModelSelection`, `ModelDownloadDialog` (compact).

### Steps

- [ ] **Script additions.** In `<script>`:
  - Import the tree logic and Chevron:
```ts
  import ChevronRight from "@lucide/svelte/icons/chevron-right";
  import {
    buildFolderTree,
    folderTriState,
    isExcluded,
    toggleFolder as toggleFolderPaths,
    type FolderNode,
    type TriState,
  } from "../lib/folderTree";
```
  - Derive the tree, the exclusion set, and per-category model splits:
```ts
  const excludedSet = $derived(new Set(app.project?.excluded ?? []));
  const folderTree = $derived(buildFolderTree(app.folders));
  let expanded = $state<Set<string>>(new Set());

  function toggleExpand(relPath: string) {
    const next = new Set(expanded);
    next.has(relPath) ? next.delete(relPath) : next.add(relPath);
    expanded = next;
  }

  const transcriptionModels = $derived(models.filter((m) => m.category === "transcription"));
  // Language models arrive when the other plan appends them; this card renders
  // whatever categories the catalog returns.
  const languageModels = $derived(models.filter((m) => m.category === "language"));

  async function selectModel(category: "transcription" | "language", id: string) {
    await api.setModelSelection(category, id);
    await refreshModels();
  }
```
  - Replace `toggleFolder` (`:102-119`) to use the pure logic + tri-state (a checkbox click flips based on current tri-state; indeterminate counts as "watched" → clicking excludes it):
```ts
  async function toggleFolder(relPath: string) {
    if (!app.project || toggling) return;
    toggling = true;
    try {
      const currentlyExcluded = isExcluded(relPath, excludedSet);
      const next = toggleFolderPaths(relPath, currentlyExcluded, excludedSet);
      await app.setExcluded(next);
    } finally {
      toggling = false;
    }
  }
```

- [ ] **Markup — page shell with three groups.** Replace the `<div class="inner">…</div>` body. Wrap the existing cards in three `<section class="group">` blocks with headings, in this order and membership:
```svelte
  <div class="inner">
    <h1>Settings</h1>

    <section class="group">
      <div class="group-head">This project</div>
      <!-- Project card (unchanged: :153-176) -->
      <!-- Watched folders card (tri-state tree — below) -->
      <!-- Ignored files card (unchanged, still conditional: :208-229) -->
      <!-- Cloud files card (unchanged: :231-248) -->
      <!-- Sync & collaboration card (unchanged: :333-388) -->
    </section>

    <section class="group">
      <div class="group-head">On this Mac</div>
      <!-- Appearance card (one-row segmented — below) -->
      <!-- Offline models card (replaces "Transcription model" — below) -->
      <!-- AI runner card (unchanged: :293-331) -->
    </section>

    <section class="group">
      <div class="group-head">Working with agents</div>
      <!-- Connect an agent card (unchanged: :390-447) -->
    </section>
  </div>
```

- [ ] **Appearance — one-row segmented control.** Replace the Appearance card (`:135-151`) with a single Theme row using the seg idiom:
```svelte
    <div class="card">
      <div class="card-title">Appearance</div>
      <div class="row">
        <span class="label">Theme</span>
        <div class="seg-group" role="radiogroup" aria-label="Theme">
          {#each themeOptions as opt (opt.value)}
            <button
              class="seg"
              class:on={theme.mode === opt.value}
              role="radio"
              aria-checked={theme.mode === opt.value}
              onclick={() => theme.set(opt.value)}
            >{opt.title}</button>
          {/each}
        </div>
      </div>
    </div>
```
  (The `themeOptions` array stays; the `hint` field is now unused — drop `hint` from the array literal at `:26-30`, keeping `value`/`title`.)

- [ ] **Watched folders — tri-state tree.** Replace the Watched folders card body (`:178-206`) with a recursive tree. Define a snippet and render roots:
```svelte
    <div class="card">
      <div class="card-title">Watched folders</div>
      <p class="note">
        Ken watches every folder by default. Uncheck one to leave it and
        everything inside it out of search and AI features.
      </p>
      {#if app.folders.length === 0}
        <p class="note">No subfolders — everything at the top level is watched.</p>
      {:else}
        <div class="folder-tree">
          {#each folderTree as node (node.relPath)}
            {@render folderRow(node, 0)}
          {/each}
        </div>
      {/if}
    </div>

    {#snippet folderRow(node: FolderNode, depth: number)}
      {@const tri = folderTriState(node.relPath, excludedSet)}
      <div class="frow" style:padding-left={`${depth * 20}px`}>
        {#if node.children.length > 0}
          <button
            class="chev"
            class:open={expanded.has(node.relPath)}
            aria-label={expanded.has(node.relPath) ? "Collapse" : "Expand"}
            onclick={() => toggleExpand(node.relPath)}
          >
            <ChevronRight size={14} strokeWidth={2} />
          </button>
        {:else}
          <span class="chev-spacer"></span>
        {/if}
        <label class="fcheck">
          <input
            type="checkbox"
            checked={tri === "checked"}
            indeterminate={tri === "indeterminate"}
            disabled={toggling}
            onchange={() => toggleFolder(node.relPath)}
          />
          <span class="mono">{node.name}</span>
        </label>
      </div>
      {#if expanded.has(node.relPath)}
        <div class="subtree">
          {#each node.children as child (child.relPath)}
            {@render folderRow(child, depth + 1)}
          {/each}
        </div>
      {/if}
    {/snippet}
```
  Note: bind `indeterminate` via the attribute — Svelte 5 sets the DOM property from the `indeterminate={...}` attribute on the checkbox. Verify the native `[-]` renders; if the attribute doesn't drive the property in this Svelte version, add `use:` action `(el) => { el.indeterminate = tri === "indeterminate" }` re-run on `tri`.

- [ ] **Offline Models card.** Replace the "Transcription model" card (`:250-291`) with a category-grouped card. Promise copy first; one radio pair per category; picking an uninstalled model starts the compact download in place; the non-selected installed file offers a quiet Remove:
```svelte
    <div class="card">
      <div class="card-title">Offline models</div>
      <p class="note">These run on your Mac — nothing you say or store leaves it.</p>
      {#if modelsLoading}
        <p class="note">Checking for models…</p>
      {:else}
        {@render modelCategory("Transcription", "transcription", transcriptionModels)}
        {#if languageModels.length > 0}
          {@render modelCategory("Answers & Map", "language", languageModels)}
        {/if}
      {/if}
    </div>

    {#snippet modelCategory(title: string, cat: "transcription" | "language", list: ModelStatus[])}
      <div class="mcat">
        <div class="mcat-title">{title}</div>
        {#each list as m (m.id)}
          <div class="mopt" class:selected={m.selected}>
            <label class="mradio">
              <input
                type="radio"
                name={`model-${cat}`}
                checked={m.selected}
                disabled={!m.installed}
                onchange={() => void selectModel(cat, m.id)}
              />
              <span class="mopt-main">
                <span class="mname">{m.name}</span>
                <span class="mtier">{m.tier === "recommended" ? "Recommended" : "Advanced"}</span>
                <span class="mblurb">{m.blurb}</span>
              </span>
            </label>
            {#if m.installed}
              <div class="mopt-actions">
                <span class="soft"><span class="ok-dot"></span>Installed{#if m.sizeBytes}· {fmtModelSize(m.sizeBytes)}{/if}</span>
                {#if !m.selected}
                  <button class="btn btn-small remove" onclick={() => void removeModel(m.id)} disabled={removing === m.id}>
                    {removing === m.id ? "Removing…" : "Remove"}
                  </button>
                {/if}
              </div>
            {:else}
              <ModelDownloadDialog status={m} compact onInstalled={refreshModels} />
            {/if}
          </div>
        {/each}
      </div>
    {/snippet}
```
  Add `import type { ModelStatus } from "../lib/api";` to the existing api import (it already imports `type ModelStatus` at `:6` — reuse it).

- [ ] **Styles.** Add to `<style>` (and remove the now-dead `.tag`, flat `.folder` padding rules that no longer apply — keep `.folder.ignored` for the Ignored files card):
```css
  /* Groups: generous separation between, tighter within — restores hierarchy
     without new chrome. */
  .group {
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .inner {
    gap: 40px; /* between groups (overrides the old uniform 18px) */
  }
  .group-head {
    font-size: 11px;
    font-weight: 700;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--ink-tertiary);
    margin-bottom: 2px;
  }
  /* Appearance / any segmented control, matching the Files All/Unread filter. */
  .seg-group {
    display: inline-flex;
    border: 1px solid var(--border);
    border-radius: var(--radius-control);
    overflow: hidden;
  }
  .seg-group .seg {
    padding: 5px 14px;
    border: none;
    background: var(--surface);
    color: var(--ink-secondary);
    font-size: 12.5px;
    font-weight: 500;
  }
  .seg-group .seg:hover { background: var(--sunken); }
  .seg-group .seg.on {
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    color: var(--accent-deep);
    font-weight: 600;
  }
  /* Watched-folders tree */
  .folder-tree { display: flex; flex-direction: column; gap: 2px; }
  .frow { display: flex; align-items: center; gap: 4px; }
  .chev {
    display: inline-flex; align-items: center; justify-content: center;
    width: 18px; height: 18px; border: none; background: transparent;
    color: var(--ink-tertiary); border-radius: 4px;
    transition: transform 0.15s ease;
  }
  .chev:hover { background: var(--sunken); color: var(--ink); }
  .chev.open { transform: rotate(90deg); }
  .chev-spacer { width: 18px; flex: none; }
  .fcheck { display: inline-flex; align-items: center; gap: 8px; font-size: 13px; cursor: pointer; }
  .fcheck input { accent-color: var(--accent); }
  /* Quiet grid-rows expand: no bounce. */
  .subtree { display: flex; flex-direction: column; gap: 2px; }
  /* Offline models */
  .mcat { display: flex; flex-direction: column; gap: 10px; }
  .mcat + .mcat { margin-top: 16px; }
  .mcat-title { font-size: 12px; font-weight: 600; color: var(--ink-secondary); }
  .mopt { display: flex; flex-direction: column; gap: 6px; padding: 8px 0; }
  .mradio { display: flex; align-items: flex-start; gap: 10px; cursor: pointer; }
  .mradio input { accent-color: var(--accent); margin-top: 3px; }
  .mopt-main { display: flex; flex-direction: column; gap: 2px; }
  .mname { font-size: 13px; font-weight: 500; }
  .mtier { font-size: 11px; color: var(--accent); }
  .mblurb { font-size: 12px; color: var(--ink-tertiary); }
  .mopt-actions { display: flex; align-items: center; gap: 10px; padding-left: 28px; }
```
  Note: the `.subtree` collapse can also use a `grid-template-rows: 0fr → 1fr` transition per the spec's "quiet grid-rows transition"; if adopted, wrap children in an inner `<div>` and transition `grid-template-rows`. The `{#if expanded}` form above is the simpler baseline; add the grid transition only if it reads calm.

- [ ] **Type-check & manual verify (per `verify`).** `pnpm exec svelte-check` passes. In the live app Settings: three section headings with clear separation; Appearance is one segmented Theme row (light/dark/system) and switching works; Offline models shows the promise line, Transcription Recommended/Advanced pair, selecting an installed model marks it, picking an uninstalled one downloads in place (progress via `ModelDownloadDialog`), and the non-selected installed file shows a quiet Remove; Watched folders shows roots first, chevrons expand/collapse quietly, tri-state checkboxes reflect and drive exclusions (excluding a parent greys its subtree; a partially-excluded parent shows the native `[-]`).

- [ ] **Commit:**
```bash
git add src/screens/SettingsScreen.svelte
git commit -m "feat(settings): rhythm, segmented Appearance, offline-models card, tri-state folders (§10)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Final verification

- [ ] **Full test suites:** `pnpm vitest run` (all TS/Svelte pure modules pass) and `cargo test -p ken-core` (model catalog, selection, transcript, chat persistence pass).
- [ ] **Type + build:** `pnpm exec svelte-check --tsconfig ./tsconfig.json` clean; the tauri crate builds.
- [ ] **Live pass (per the `verify` skill):** exercise §3 (pinned header, moved Import/filter), §9 (first message, focus, autoscroll, send-failure, archive), §10 (settings rhythm, segmented Appearance, offline models select/download/remove, tri-state folders), §11 (148 MB xlsx notice, responsive app). Recording/TCC flows are out of this plan's scope.

---

## Coverage self-review (spec §3 / §9 / §10 / §11)

- **§3** — header pinned (Task 5 sticky), "Manage folders" removed (Task 5), icon-only Import + compact All/Unread in header (Tasks 3–5), toolbar drops Import+filter keeps tab strip + Mark-all (Task 5). ✔
- **§9** — first-message optimistic echo + reconcile (Tasks 6–7), focus on open/newChat/select/exit-terminal (Task 7c), backend persistence diagnosed + pinned (Tasks 6, 9). Audit items each a verify-and-fix task with acceptance criteria: send-failure visibility (drop pending + sendError, Task 7b + verify), autoscroll near-bottom (Task 7d), Enter/Shift+Enter (Task 7a keySend + test), suggested prompts (Task 7 verify), archive-active-chat state (Task 7b). ✔
- **§10** — Offline Models card with Recommended/Advanced pair, inline download, quiet Remove, promise copy (Task 12); curated catalog with `category` field replacing discovery, download/verify plumbing kept, Transcription pair with exact files/sizes, persisted selection, Language variant appendable (Task 8); `transcript.rs` uses the selected model with installed fallback (Task 9); watched-folders tri-state tree (Tasks 11–12); three section headings + rhythm (Task 12); one-row segmented Appearance (Task 12). ✔
- **§11** — per-format caps checked against `meta.size` before reading bytes, "too large" notice with open-external, app stays responsive, big-workbook regression as the pure `isPreviewTooLarge` test (Tasks 1–2). ✔

No `TBD`/"similar to Task N"/"add error handling" placeholders. Interface names match the prompt: `ModelCategory { Transcription, Language }`, `ModelTier { Recommended, Advanced }`, `CatalogEntry { category, tier, blurb, spec }`, `catalog()`, `selected(base_dir, category)`. Deviations noted inline: `selected`/`set_selected` take `base_dir` and persist in a machine-level `models/selection.json` (not per-project `user_state`) because models are machine-wide; `blurb` lives on `CatalogEntry` (not `ModelSpec`) to keep the download identity untouched.

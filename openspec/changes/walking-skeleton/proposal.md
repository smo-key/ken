# Proposal: walking-skeleton

## Why

Ken has an approved system design (`docs/superpowers/specs/2026-07-12-ken-design.md`)
and an authoritative UI prototype (`docs/design/ken-prototype-v2.dc.html`) but no
code. The walking skeleton is change 1 of 10 in the build order: it delivers the
smallest end-to-end usable app — open a folder as a project, index and watch it,
search it, read and edit its files — so every later change (AI ingests, chat,
review, sync, MCP) layers onto a running product instead of scaffolding.

## What Changes

- New Rust workspace: `ken-core` (scan/extract/index/search/watch library),
  `ken-mcp` (compilable stub binary only), `src-tauri` (Tauri 2 app shell).
- New Svelte 5 + TypeScript frontend implementing the Paper & Ink app shell
  from the prototype: title bar (project switcher, ⌘K search field, Chats
  toggle placeholder), left nav rail (Home, Files, Review, Ingests, Map,
  Timeline, Settings — non-skeleton screens render as designed placeholders).
- Project lifecycle: create/open a project from any existing folder; writes
  `.ken/project.json`; registers path in app-data `projects.json`; per-project
  SQLite database in app-data.
- Folder include/exclude selection (default all) persisted in `project.json`.
- Ingestion pipeline: initial scan + extractors (md/txt/code native, docx,
  xlsx, pptx, pdf, images by filename/EXIF) → cleaned text + metadata →
  SQLite FTS5. Per-file failures are recorded and surfaced, never blocking.
- File watcher with ~2s debounce keeping the index live; Reindex action
  rebuilds the database from scratch.
- ⌘K search overlay: FTS5/BM25 matches with highlighted snippets, keyboard
  navigation (↵ open, esc close). No AI quick answer in this change.
- Files screen: file tree (folder expand/collapse, file-type glyphs,
  exclusion state, watch-status footer) + full-bleed Milkdown WYSIWYG
  editor for markdown/text with plain-text toggle + in-webview preview
  (pdf.js, mammoth for docx, SheetJS for xlsx, native images; fallback
  metadata + extracted text + "Open in default app").

## Capabilities

### New Capabilities
- `project-management`: creating/opening projects from existing folders,
  project registry, per-project settings including folder include/exclude.
- `ingestion-indexing`: scanning, format extraction, SQLite FTS5 index,
  file watching, reindex, extraction-failure reporting.
- `knowledge-search`: ⌘K overlay search over the index with ranked,
  highlighted results and keyboard navigation.
- `document-editing`: WYSIWYG markdown/text editing writing directly to
  project files.
- `document-preview`: in-app preview of PDF, Word, Excel, and image files
  with graceful fallback for other formats.
- `app-shell`: Paper & Ink application chrome — title bar, nav rail, screen
  routing, placeholder screens for capabilities from later changes.

### Modified Capabilities

_None — this is the first change; no existing specs._

## Impact

- New code: entire repository (Rust workspace, Svelte app, Tauri config).
- Dependencies: Tauri 2, Svelte 5, Vite, Milkdown, pdf.js, mammoth, SheetJS
  (JS); rusqlite (bundled SQLite + FTS5), notify, calamine, pdf-extract,
  zip/quick-xml, serde, tokio (Rust).
- Toolchain: Rust stable, Node 24, pnpm.
- No external services; no Claude CLI dependency in this change (AI arrives
  in change 2).

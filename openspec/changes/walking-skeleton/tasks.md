# Tasks: walking-skeleton

## 1. Workspace scaffold

- [ ] 1.1 Create Cargo workspace with `crates/ken-core` (lib), `crates/ken-mcp` (stub bin), and Tauri 2 app in `src-tauri`; `cargo build` passes
- [ ] 1.2 Scaffold Svelte 5 + TypeScript + Vite frontend in `src/` wired to Tauri dev; `pnpm tauri dev` opens a window
- [ ] 1.3 Add Paper & Ink tokens as CSS custom properties in `src/app.css`; bundle Source Serif 4 + IBM Plex Mono woff2 locally
- [ ] 1.4 Set up test harness: `cargo test` for ken-core with a `fixtures/` project folder (md, docx, xlsx, pptx, pdf, png, unknown-binary); vitest for frontend stores

## 2. ken-core: project + database

- [ ] 2.1 Implement project model: create/adopt `.ken/project.json` (name, uuid, include/exclude, settings) with serde round-trip tests
- [ ] 2.2 Implement app-data registry (`projects.json`) with add/remove/list and missing-path detection
- [ ] 2.3 Create SQLite schema (files, contents, FTS5 search, meta with schema_version) via rusqlite bundled, WAL mode; migration-on-open test

## 3. ken-core: extraction + indexing

- [ ] 3.1 Define `Extractor` trait + plain-text extractor (md/txt/code allowlist) with fixture tests
- [ ] 3.2 Implement docx extractor (zip + quick-xml over word/document.xml) with fixture test
- [ ] 3.3 Implement xlsx extractor (calamine, rows joined per sheet) with fixture test
- [ ] 3.4 Implement pptx extractor (zip + quick-xml over slide XML) with fixture test
- [ ] 3.5 Implement pdf extractor (pdf-extract) incl. corrupt-file failure recorded as status+reason, file still name-searchable
- [ ] 3.6 Implement image/metadata-only extractor (filename + EXIF)
- [ ] 3.7 Implement scanner: walk included folders, apply exclusions, upsert index; test add/modify/delete reconciliation and exclusion changes
- [ ] 3.8 Implement search: FTS5 bm25 + snippet + prefix queries behind `ken_core::search`; ranking and highlight tests
- [ ] 3.9 Implement watcher: notify + 2s debounce batching into rescan of affected paths; burst test (500+ events) converges; Reindex rebuilds from scratch

## 4. Tauri command layer

- [ ] 4.1 Commands: create_project, open_project, list_projects, set_folder_selection, get_tree, reindex; state events for index progress and watch status
- [ ] 4.2 Commands: search(query) and read_file/save_file (path-validated to project root)
- [ ] 4.3 Frontend `src/lib/api.ts` typed wrappers + `project`/`files`/`search` stores with vitest coverage

## 5. App shell (frontend)

- [ ] 5.1 Title bar: project switcher (create/open/switch, unavailable-path state), search field affordance, Chats toggle placeholder
- [ ] 5.2 Nav rail with active/hover states per tokens; screen routing preserving per-screen state
- [ ] 5.3 Placeholder screens (Home, Review, Ingests, Map, Timeline, Settings) in prototype layout language with "coming in change N" copy
- [ ] 5.4 New/open project flow incl. folder picker and include/exclude management UI

## 6. Files screen

- [ ] 6.1 File tree: folders expand/collapse, format glyphs (dog-eared + tinted per tokens), excluded-dimmed state, watch-status footer
- [ ] 6.2 Milkdown WYSIWYG editor component (commonmark+gfm) with debounced save, "Saved just now" header, plain-text toggle
- [ ] 6.3 External-change handling: reload banner when clean, keep-mine/take-disk choice when dirty
- [ ] 6.4 Preview components: pdf.js, mammoth (docx), SheetJS grid with sheet tabs (xlsx), native images, metadata+text fallback with "Open in default app"

## 7. Search overlay

- [ ] 7.1 ⌘K overlay per prototype: as-you-type FTS results, highlighted snippets, glyphs+paths, empty state
- [ ] 7.2 Keyboard navigation (arrows, ↵ opens in Files, esc closes) and title-bar field opens overlay

## 8. Verification

- [ ] 8.1 All cargo + vitest tests pass; fixture project end-to-end: open → indexed → search finds content in every supported format
- [ ] 8.2 Manual run-through against every spec scenario in this change; fix gaps
- [ ] 8.3 Update README with dev setup (pnpm tauri dev) and commit

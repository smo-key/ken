# Design: walking-skeleton

## Context

Empty repository. The system design (`docs/superpowers/specs/2026-07-12-ken-design.md`)
and UI prototype (`docs/design/ken-prototype-v2.dc.html`, tokens in
`docs/design/design-tokens.md`) are approved. This change builds the
non-AI spine everything else attaches to: workspace, app shell, project
lifecycle, ingestion/index, search, editor, preview.

## Goals / Non-Goals

**Goals:**
- A runnable desktop app: open a folder → indexed, watched, searchable,
  files readable and editable, all in the Paper & Ink shell.
- `ken-core` owns all domain logic so change 6 (MCP) reuses it untouched.
- Fixture-tested extractors and index; runner-independent (no Claude).

**Non-Goals:**
- Anything AI: ingest recipes, chat, digest, quick answer, research.
- Review inbox logic, sync engine, Map/Timeline data (screens are static
  placeholders matching the prototype's layout language).
- Release packaging (change 10); `pnpm tauri dev` is the run story.

## Decisions

1. **Crate layout** — `crates/ken-core` (lib), `crates/ken-mcp` (bin stub
   printing a not-yet-implemented notice), `src-tauri` (bin). Cargo
   workspace at repo root. Alternative — single crate — rejected: MCP
   sidecar must not link Tauri.
2. **DB access** — `rusqlite` with `bundled` feature (ships SQLite with
   FTS5 compiled in; no system dependency). One DB per project at
   `<app-data>/ken/index/<project-id>.db`, WAL mode. Schema:
   `files(id, rel_path, kind, size, mtime, status, error)`,
   `contents(file_id, text)`, FTS5 table `search(text, rel_path)` with
   external-content on `contents`, `meta(key, value)` for schema version.
   Alternative — sqlx/async — rejected: watcher and commands are already
   on threads; rusqlite is simpler and synchronous fits the pipeline.
3. **Project identity** — `project.json` carries a UUID `id` generated at
   creation. App-data registry maps id → path; opening a folder whose
   `.ken/project.json` already exists re-registers it (team-shared
   projects keep one id).
4. **Extractor trait** — `trait Extractor { fn supports(&ext) -> bool;
   fn extract(&Path) -> Result<Extracted> }` with `Extracted { text,
   title, meta }`. Implementations: plain (md/txt/code by extension
   allowlist), docx (zip + `quick-xml` over `word/document.xml`), xlsx
   (`calamine`, cells joined row-wise per sheet), pptx (zip + quick-xml
   over `ppt/slides/slide*.xml`), pdf (`pdf-extract`; failures common —
   record and continue), image (filename + EXIF via `kamadak-exif`).
   Binary/unknown files get a metadata-only row (searchable by name).
5. **Watcher** — `notify` recommended watcher, events funneled into a
   2s debounce buffer keyed by path, then batch re-extract on a worker
   thread. Exclusion rules applied at event time. `.ken/` config changes
   reload settings; index DB lives outside the project so no self-events.
6. **Search** — FTS5 `bm25()` ranking, `snippet()` for highlights,
   prefix queries (`token*`) for as-you-type. Query goes through one
   `ken-core::search(project, query, limit)` used by both Tauri command
   and (later) ken-mcp.
7. **Frontend structure** — Svelte 5 runes; `src/lib/api.ts` wraps Tauri
   `invoke`/events; stores: `project`, `files`, `search`, `editorDoc`.
   Screens under `src/screens/`; shell components under `src/shell/`.
   Design tokens as CSS custom properties in `src/app.css` straight from
   `design-tokens.md`. Fonts (Source Serif 4, IBM Plex Mono) bundled as
   woff2 in `src/assets/fonts` — desktop app must not fetch Google Fonts
   at runtime.
8. **Editor** — Milkdown (commonmark preset + gfm) writing through a
   debounced (800ms) save command; plain-text CodeMirror-free fallback is
   a `<textarea>`-styled mode toggle. External file changes while open
   prompt a reload banner (no silent clobber); dirty-vs-changed conflict
   defers to the user (keep mine / take disk).
9. **Preview** — pdf.js, mammoth, SheetJS run in the webview from local
   file bytes served via Tauri `readFile` (no custom protocol needed at
   this size). Each preview is a lazy-loaded Svelte component.
10. **Placeholder screens** — Home/Review/Ingests/Map/Timeline/Settings
    render the prototype's static layout with "coming in change N" copy,
    so the shell is honest but complete-feeling.

## Risks / Trade-offs

- [pdf-extract quality varies] → record per-file failure with reason;
  fallback row keeps the file findable by name; revisit pdfium later.
- [Milkdown + Svelte 5 integration is community-tier] → wrap in one
  component with a plain-text mode as escape hatch; editor swap stays
  local to that component.
- [FTS5 prefix search on large corpora] → `detail=full` default; if slow
  on big projects, add `prefix='2 3'` index options — schema versioned
  via `meta` for cheap migration.
- [Watcher event storms (git checkout, OneDrive resync)] → debounce +
  batch, and a scan-diff reconcile pass after bursts over 500 events.

## Migration Plan

Greenfield; no migration. Rollback = git revert; the app-data DB is
rebuildable and versioned by `meta.schema_version` from day one.

## Open Questions

None blocking. Editor conflict UX (reload banner copy) can be tuned
during implementation against the prototype's plain-language voice.

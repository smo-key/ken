# Ken — Team Knowledge Manager: System Design

**Date:** 2026-07-12
**Status:** Approved pending user review
**Repo:** github.com/smo-key/ken (public)

## 1. Overview

Ken is a desktop app (Svelte 5 + Tauri 2) that turns an ordinary folder into a
team knowledge base. It ingests and indexes the folder's raw files, keeps AI-
maintained structured documents fresh as data changes, embeds Claude Code chat
sessions, offers deep-research runs, and exposes everything to external agents
through an MCP server. It is designed to be **usable by non-technical people**
and to collaborate through the team's existing sync mechanism — Git or a
shared drive (OneDrive etc.) — never through a Ken-specific server.

### Goals
- Point Ken at an existing folder; it becomes a searchable, AI-augmented
  knowledge project with zero migration.
- All shared state is plain text inside the project folder, so Git/OneDrive
  collaboration works and conflicts are always human/AI-mergeable.
- AI features run through the user's locally installed Claude Code CLI
  (their existing auth/subscription); Ken manages no API keys.
- One-line install for the app and one-line/one-instruction setup for the
  MCP server.

### Non-goals (v1)
- Public web-source ingestion (cut from scope; deep research covers ad-hoc
  web needs).
- OCR for images (indexed by filename/EXIF only).
- Faithful PPTX slide rendering (v1 shows slide text + embedded images).
- Semantic/embedding search (FTS5 only; indexer is behind a trait so
  embeddings can be added without rework).
- Direct Anthropic API integration (Claude Code CLI is the only AI engine).
- Real-time co-editing; sync is whatever Git/OneDrive provides.

## 2. Architecture

One Rust workspace + one Svelte frontend. The MCP server and the app share
all ingestion/index logic via a common crate.

```
ken/
├── crates/
│   ├── ken-core/        # library: scan, watch, extract, index, search,
│   │                    #   recipes, runner, conflict detection
│   └── ken-mcp/         # stdio MCP server binary over ken-core
├── src-tauri/           # Tauri 2 app shell (depends on ken-core)
├── src/                 # Svelte 5 + TypeScript frontend
├── openspec/            # OpenSpec specs & change proposals
├── install.sh           # one-line installer
└── .github/workflows/   # release CI (tag → bundles → GitHub Releases)
```

Frontend ↔ backend via Tauri commands + events. Long-lived subsystems
(watcher, PTY sessions, hook listener) live in the Rust side; the frontend is
a thin reactive view over Tauri state events.

## 3. Data model

**In the project folder (shared, text only):**

```
<project>/
├── .ken/
│   ├── project.json      # name, project id, folder include/exclude,
│   │                     #   settings (e.g. ingestRunner)
│   └── ingests/*.md      # recipes: YAML frontmatter + prompt body
├── ... user's raw files ...
└── <output paths>        # AI-generated structured docs (markdown);
                          # each ingest targets a file OR folder anywhere
                          # in the tree
```

**In OS app-data (local, never synced, 100% rebuildable):**
- `projects.json` — registry of known project paths.
- `index/<project-id>.db` — SQLite per project: file inventory, extracted
  text, FTS5 index, ingest run history, chat metadata (pins, status,
  session ids).

Rationale: Git and OneDrive can merge text but not a binary DB; SQLite
corrupts under concurrent shared-drive sync. With this split a conflict can
only ever be a text-file conflict — reviewable by AI with user escalation.
"Reindex" rebuilds the DB from source files and is the universal recovery
story.

## 4. Subsystems

### 4.1 Ingestion, indexing, search
- On project open: walk folder respecting include/exclude selection
  (default: all folders; ingest outputs included so structured docs are
  searchable), route each file to an extractor, store cleaned text +
  metadata in SQLite/FTS5.
- Watcher (`notify` crate, ~2s debounce) re-extracts affected files on
  create/modify/delete.
- Extractors (Rust, shared with ken-mcp): markdown/txt/code native;
  docx/xlsx/pptx via zipped-XML (`calamine` for xlsx); PDF via
  `pdf-extract`; images by filename/EXIF.
- Extraction failures are per-file, logged, visible in UI as "not indexed —
  reason", never blocking.
- Search: FTS5 + BM25, instant-as-you-type, snippet highlighting, filters
  by folder/file-type. Same engine exposed via MCP.

### 4.2 AI ingests (structured documents)
- Recipe = markdown in `.ken/ingests/`: frontmatter `name`, `description`,
  `output` (file or folder path anywhere in tree), `mode: single |
  collection`, `refresh: on-change | manual`; body = extraction prompt.
- Non-technical creation: a form (name, plain-words "what should Ken
  extract?", output picker, single/collection toggle, refresh choice) reads
  and writes the recipe file; power users and AI edit the file directly.
- Refresh engine: on debounced source changes or "Run now", compose prompt =
  recipe body + changed/new files since last successful run + current
  outputs. Prompt treats existing output as canonical: update only what new
  data implies, preserve human edits. First run = full corpus; later runs
  incremental. Runs recorded in a log (when, inputs, result).
- Outputs written by a run are marked to suppress watcher-triggered refresh
  loops; human edits to outputs do not trigger the ingest that owns them
  (only source-file changes do).
- **Runner:** default spawns `claude` in a hidden PTY (real interactive
  session, not rendered until opened); Ken submits the prompt and detects
  completion via the Stop hook. Runs appear in the chat list as
  system-initiated sessions — click to watch/intervene live. Per-project
  setting `ingestRunner: hidden-tui (default) | headless` switches to
  `claude -p`.
- Template library: bundled recipes (People, Requirements gold-standard,
  Decision log, Glossary, Meeting notes digest, FAQ, Risks). "Use template"
  copies into `.ken/ingests/` — a starting point, not a linked dependency.

### 4.3 Chat
- Each chat = a PTY running `claude` in the project folder, rendered with
  xterm.js; resumable via `claude --resume <session-id>`.
- Friendly frame for non-technical users: terminal inside a normal chat
  window, large input box, suggested starting prompts, plain-language
  status ("Ken is working…", "Ken has a question for you").
- Sidebar list: pin/unpin (pins float to top; stored in local DB), status
  badge **working / needs your input / done**.
- Status is signal, not scraping: Ken writes project-scoped Claude Code
  hooks (Notification, Stop, permission-request) that POST session state to
  Ken's localhost listener.
- Chats can do everything (query, edit recipes, update docs) because the
  agent operates on the same text files Ken watches; changes flow back into
  the index automatically.

### 4.4 Deep research mode
- First-class action: user types a question; output location is selectable
  from options (default `research/`, recent folders, or any folder in the
  project).
- Always runs on the hidden-TUI runner (regardless of the `ingestRunner`
  setting — research must be able to ask the user questions mid-run) with a
  research harness prompt:
  fan out web searches, read sources, cross-verify claims, write a cited
  report to the chosen location.
- Report is a normal project document: indexed, editable, previewable,
  available to ingests. Research runs show in the chat list with the same
  status badges (research may ask scoping questions mid-run).

### 4.5 Editor & preview
- WYSIWYG markdown/text editor (Milkdown, ProseMirror-based) with
  plain-text toggle; saves write straight to the file; watcher treats human
  edits identically to AI edits.
- Preview in-webview: PDF (pdf.js), Word (mammoth → HTML), Excel (SheetJS
  grid with per-sheet tabs), PowerPoint (slide text + embedded images —
  known v1 limitation), images native. Unpreviewable files: metadata +
  extracted text + "Open in default app".

### 4.6 MCP server
- `ken-mcp`: stdio binary over ken-core, read-only on the SQLite index.
- Tools: `search_knowledge`, `read_document`, `list_documents`,
  `list_projects`.
- Scoping: `ken-mcp --project <path>` locks to one project; unscoped serves
  all registered projects with `project` as a tool argument.
- "Connect an agent" screen: copy-paste one-liner
  (`claude mcp add ken -- ken-mcp --project <path>`), generic JSON config
  block for other agents, and an LLM instruction snippet ("paste this into
  any agent and it will configure itself").

### 4.7 Sync & conflicts
- Ken never syncs; it detects sync damage: Git conflict markers in text
  files and shared-drive "conflicted copy" filename patterns
  (OneDrive/Dropbox variants).
- Conflicted files land in a **Conflicts inbox**. "Resolve with AI" opens a
  session with both versions and requests a proposed merge; ambiguities are
  escalated as questions (surfaces as a needs-your-input chat). Nothing is
  written until the user approves. Plain-language presentation ("This file
  was edited in two places — here's a suggested combination"); merge
  markers only shown on request.

### 4.8 Install & release
- `curl -fsSL https://raw.githubusercontent.com/smo-key/ken/main/install.sh | sh`
  detects OS/arch, downloads latest GitHub Release bundle, installs the
  app, puts `ken-mcp` on PATH, checks for the `claude` CLI and prints
  install guidance if missing.
- A plain download link is offered alongside the one-liner for
  non-technical users.
- GitHub Actions: version tag → macOS (arm64 + x64), Windows, Linux
  bundles → GitHub Release.
- First-run experience checks for Claude Code and walks through login.

## 5. Error handling

- Extraction failures: per-file, visible, non-blocking.
- Missing `claude` binary: guided setup screen (blocking only for AI
  features; ingestion/search/preview work without it).
- Index corruption or drift: "Reindex" rebuilds from source files.
- Watcher failure: auto-restart with backoff; stale-index banner if it
  cannot recover.
- Ingest run failure: run log records error; ingest shows failed state with
  retry; never partially deletes existing outputs.

## 6. Testing

- `ken-core` unit tests: extractors against fixture files (docx/xlsx/pptx/
  pdf/md), recipe parsing, include/exclude selection, conflict-pattern
  detection, index/search behavior.
- Integration: drive `ken-mcp` over stdio (spawn, call tools, assert
  results against a fixture project).
- Runner tests against a fake `claude` script (PTY spawn, prompt submit,
  Stop-hook completion) so CI never needs real Claude.
- Frontend: component tests for critical stores (project state, chat list,
  search); e2e deferred.

## 7. Build order (one OpenSpec change each)

1. **Walking skeleton** — workspace scaffold; project create/open; folder
   selection; ingest + watch; index; search; preview + editor.
2. **AI ingests** — recipes, form editor, runner (hidden-TUI + headless
   setting), refresh engine, template library.
3. **Chat** — PTY sessions, xterm frame, hooks/status, pins.
4. **Deep research** — research action, harness prompt, output selection.
5. **MCP server** — ken-mcp binary, connect-an-agent screen.
6. **Conflicts** — detection, inbox, AI-assisted resolution.
7. **Install & release** — install.sh, release CI, first-run experience.

Each change lands runnable; the user can steer between layers.

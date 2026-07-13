# Ken — Team Knowledge Manager: System Design

**Date:** 2026-07-12 (rev 2 — reconciled with UI prototype)
**Status:** Approved pending user review
**Repo:** github.com/smo-key/ken (public)
**UI reference:** `docs/design/ken-prototype-v2.dc.html` (authoritative) and
`docs/design/design-tokens.md` ("Paper & Ink" design system), imported from
claude.ai/design project `051a5dee-d439-477b-9d0a-ea943f484534`.

## 1. Overview

Ken is a desktop app (Svelte 5 + Tauri 2) that turns an ordinary folder into a
team knowledge base. It ingests and indexes the folder's raw files, keeps AI-
maintained structured documents fresh as data changes, embeds Claude sessions
as a chat drawer, offers deep-research runs, and exposes everything to
external agents through an MCP server. It is designed to be **usable by
non-technical people** and to collaborate through the team's existing sync
mechanism — Git or a shared drive (OneDrive etc.) — never through a
Ken-specific server.

### Goals
- Point Ken at an existing folder; it becomes a searchable, AI-augmented
  knowledge project with zero migration.
- All shared state is plain text inside the project folder, so Git/OneDrive
  collaboration works and conflicts are always human/AI-mergeable.
- AI features run through the user's locally installed Claude Code CLI
  (their existing auth/subscription); Ken manages no API keys.
- Everything the AI does to shared documents is governed by review rules;
  the Review inbox is the single place a user answers the system.
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
- Real-time co-editing; sync granularity is whatever Git/OneDrive provides.

## 2. Architecture

One Rust workspace + one Svelte frontend. The MCP server and the app share
all ingestion/index logic via a common crate.

```
ken/
├── crates/
│   ├── ken-core/        # library: scan, watch, extract, index, search,
│   │                    #   recipes, runner, review rules, sync/conflicts
│   └── ken-mcp/         # stdio MCP server binary over ken-core
├── src-tauri/           # Tauri 2 app shell (depends on ken-core)
├── src/                 # Svelte 5 + TypeScript frontend
├── openspec/            # OpenSpec specs & change proposals
├── docs/design/         # UI prototype + design tokens (authoritative UI)
├── install.sh           # one-line installer
└── .github/workflows/   # release CI (tag → bundles → GitHub Releases)
```

Frontend ↔ backend via Tauri commands + events. Long-lived subsystems
(watcher, sessions, hook listener, sync engine) live in the Rust side; the
frontend is a thin reactive view over Tauri state events.

### App layout (from prototype)
Title bar (project switcher with sync-status dot · global ⌘K search · Chats
toggle) · left nav rail (Home, Files, Review with count badge, Ingests, Map,
Timeline, Settings) · main screen · right chat drawer (372px docked on wide
windows, overlay when narrow).

## 3. Data model

**In the project folder (shared, text only):**

```
<project>/
├── .ken/
│   ├── project.json      # name, project id, folder include/exclude,
│   │                     #   settings (ingestRunner, sync, global rules)
│   └── ingests/*.md      # recipes: YAML frontmatter + prompt body
├── ... user's raw files ...
└── <output paths>        # AI-generated structured docs (markdown);
                          # each ingest targets a file OR folder anywhere
                          # in the tree
```

**In OS app-data (local, never synced, 100% rebuildable):**
- `projects.json` — registry of known project paths.
- `index/<project-id>.db` — SQLite per project: file inventory, extracted
  text, FTS5 index, ingest run history, review items, timeline events,
  entity graph, chat metadata (pins, status, session ids).

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
- **Search (⌘K overlay):** FTS5 + BM25 matches with snippet highlighting,
  instant as you type — works with no AI involved. Above the matches, an
  optional **Quick answer** card: a short AI answer with source chips,
  produced by a bounded background session over the index; it fills in
  when ready and never blocks the match list. `⌘↵` hands the query off to
  a full chat. Files tree marks structured docs in full ink, shows
  since-your-last-visit dots, and folder exclusion state.

### 4.2 AI ingests (structured documents)
- Recipe = markdown in `.ken/ingests/`: frontmatter `name`, `description`,
  `sources` (folder list; default: all included folders), `output` (file or
  folder path anywhere in tree), `mode: single | collection`,
  `refresh: on-change | manual`, optional `rules` overrides; body =
  plain-language instruction.
- Non-technical creation: the Ingests screen renders the recipe as cards —
  Sources (folder chips), Instruction, Output, Rules, Recent runs — and a
  form-based editor writes the file; power users and AI edit it directly.
  Template library ships bundled recipes (People, Requirements
  gold-standard, Decision log, Glossary, Meeting notes digest, FAQ, Risks);
  "Use template" copies into `.ken/ingests/`.
- Refresh engine: on debounced source changes or "Run now", compose prompt =
  recipe instruction + changed/new files since last successful run + current
  outputs. The prompt treats existing output as canonical: update only what
  new data implies, preserve human edits. First run = full corpus; later
  runs incremental. Every run recorded (when, inputs, outcome) and shown in
  Recent runs (fresh / blocked on you / paused · conflict / failed).
- **Review rules** (global in `project.json`, per-ingest overrides):
  - *Human edits win* — a refresh never reverts a human change.
  - *Review threshold* — a refresh changing more than N% (default 20%) of
    an output is staged as a Review item, not written directly.
  - *Stale check* — every N days (default 30) an ingest whose sources
    haven't changed is checked for drift and flagged if stale.
- Outputs written by a run are marked to suppress watcher-triggered refresh
  loops; human edits to outputs do not trigger the ingest that owns them
  (only source-file changes do).
- **Runner:** default spawns `claude` in a hidden PTY (real interactive
  session, not rendered until opened); Ken submits the prompt and detects
  completion via the Stop hook. Runs appear in the chat drawer as
  system-initiated sessions — open to watch/intervene live. Per-project
  setting `ingestRunner: hidden-tui (default) | headless` switches to
  `claude -p`. When a run needs a human decision it emits a structured
  question (rendered as option buttons in the drawer) and its status
  becomes *blocked on you*.

### 4.3 Chat (drawer)
- Chat lives in a right-side drawer with tabbed sessions; docked at 372px
  on wide windows, overlay when narrow. Pin/unpin float tabs; badges show
  **working / needs your input / done**.
- **Dual-mode rendering, one engine.** Every session is a Claude Code CLI
  session using the user's auth:
  - *Conversation mode (default):* Ken drives the CLI with
    `--input-format stream-json --output-format stream-json` and renders
    messages natively — clean transcript, clickable source links,
    structured questions as stacked option buttons.
  - *Terminal mode:* typing `/` (or the toggle) attaches a real PTY running
    the Claude TUI (`claude --resume <session-id>`) in xterm.js — the full
    terminal experience on the same session.
- Status is signal, not scraping: Ken writes project-scoped Claude Code
  hooks (Notification, Stop, permission-request) that POST session state to
  Ken's localhost listener.
- Chats can do everything (query, edit recipes, update docs) because the
  agent operates on the same text files Ken watches; changes flow back into
  the index automatically — subject to the same review rules as ingests.

### 4.4 Home & daily digest
- Home shows the date, **Today's digest**, and **Waiting on you** cards
  (each links into Review or the chat drawer).
- The digest is an AI-written paragraph — what changed, what Ken did, what
  it's holding for review — generated by a scheduled ingest-like run
  (default 7:00 AM, on first app focus of the day if the machine was
  asleep) over the last day's changes, run log, and open review items.
  Digest history is kept locally; a "share" action copies it as markdown.

### 4.5 Deep research mode
- First-class action: user types a question; output location is selectable
  from options (default `research/`, recent folders, or any folder in the
  project).
- Always runs on the hidden-TUI runner (regardless of the `ingestRunner`
  setting — research must be able to ask the user questions mid-run) with a
  research harness prompt: fan out web searches, read sources, cross-verify
  claims, write a cited report to the chosen location.
- Report is a normal project document: indexed, editable, previewable,
  available to ingests. Research runs show in the chat drawer with the same
  status badges.

### 4.6 Editor & preview
- Files screen: file tree (with structured-doc glyphs, exclusion state,
  change dots, watch status footer) + full-bleed editor at a 720px reading
  measure.
- WYSIWYG markdown/text editor (Milkdown, ProseMirror-based) with
  plain-text toggle; saves write straight to the file; watcher treats human
  edits identically to AI edits. Header shows the path, save state, and
  which ingests the file feeds.
- Preview in-webview: PDF (pdf.js), Word (mammoth → HTML), Excel (SheetJS
  grid with per-sheet tabs), PowerPoint (slide text + embedded images —
  known v1 limitation), images native. Unpreviewable files: metadata +
  extracted text + "Open in default app".

### 4.7 Review inbox
One inbox for everything Ken needs a human for; nav badge shows the count.
Item types:
- **Merge conflict** — side-by-side "who changed what" cards, plain
  language, plus *Ken's take* (a drafted merge with reasoning). Actions:
  Accept Ken's merge / Edit manually / Ask <teammate> in chat. Raw markers
  only on request.
- **AI question** — e.g. entity ambiguity ("Same person?"); answering
  unblocks the run that asked.
- **Large refresh approval** — staged diff when a refresh exceeds the
  review threshold; approve or open in editor.
- **Staleness flag** — from the stale check rule.
Resolved items move to a Done section. Nothing is written to shared files
until the user approves the item that stages it.

### 4.8 Sync engine & conflict detection
- **Git projects:** Ken drives sync actively — pull on app focus, commit +
  push on save (debounced), configurable in Settings. Failed pushes retry
  with backoff; merge conflicts become Review items. Non-technical users
  never see git vocabulary — the title-bar dot says synced / syncing /
  attention.
- **Shared-drive projects:** the drive syncs; Ken watches for damage —
  conflicted-copy filename patterns (OneDrive/Dropbox variants) and
  concurrent-edit divergence — and files Review items.
- Since all shared state is text, every conflict class the tool can cause
  is AI-reviewable with user escalation.

### 4.9 Map & Timeline (knowledge views)
- Both are read models derived from a lightweight **entity/event
  extraction ingest** that Ken maintains internally (an ingest whose output
  is structured data in the local DB, not a project file).
- **Map:** graph of entities (people, decisions, vendors, topics) and their
  relations; dashed nodes = mentioned but unconnected; click-through to
  source docs.
- **Timeline:** chronological event stream with search filter, category
  chips (Decisions / People / Vendor / Files), source chips per event, and
  "View as of…" to see the knowledge state at a past date (events carry
  timestamps; the view filters by them).
- Both ship after the core loop; they read the same DB and add no new
  write paths.

### 4.10 MCP server
- `ken-mcp`: stdio binary over ken-core, read-only on the SQLite index.
- Tools: `search_knowledge`, `read_document`, `list_documents`,
  `list_projects`.
- Scoping: `ken-mcp --project <path>` locks to one project; unscoped serves
  all registered projects with `project` as a tool argument.
- Settings shows the server card: running state, connected-agent count,
  copyable add command (`claude mcp add ken -- ken-mcp --project <path>`),
  generic JSON config block, LLM instruction snippet ("paste this into any
  agent and it will configure itself"), and a recent-activity line.

### 4.11 Install & release
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
- Sync failure (git push/pull errors, auth): title-bar attention state with
  a plain-language fix-it panel.

## 6. Testing

- `ken-core` unit tests: extractors against fixture files (docx/xlsx/pptx/
  pdf/md), recipe parsing, include/exclude selection, review-rule
  evaluation (threshold, human-edits-win), conflict-pattern detection,
  index/search behavior.
- Integration: drive `ken-mcp` over stdio (spawn, call tools, assert
  results against a fixture project).
- Runner tests against a fake `claude` script (stream-json protocol, PTY
  spawn, Stop-hook completion) so CI never needs real Claude.
- Sync engine tests against local git fixtures (clean pull, conflicting
  push, conflicted-copy detection).
- Frontend: component tests for critical stores (project state, chat list,
  review inbox, search); e2e deferred.

## 7. Build order (one OpenSpec change each)

1. **Walking skeleton** — workspace scaffold; app shell per prototype
   (title bar, rail, screens stubbed); project create/open; folder
   selection; ingest + watch; index; ⌘K search (FTS only); Files screen
   with editor + preview.
2. **AI ingests** — recipes, Ingests screen, runner (hidden-TUI + headless
   setting), refresh engine, review rules evaluation, template library.
3. **Chat drawer** — stream-json conversation mode, terminal mode,
   hooks/status, pins, structured questions.
4. **Review inbox** — item model, all four item types, approval flow.
5. **Sync engine** — git pull-on-focus/push-on-save, shared-drive damage
   detection, conflict items with AI-drafted merges.
6. **MCP server** — ken-mcp binary, Settings server card.
7. **Home & digest** — digest run, Waiting-on-you cards, ⌘K quick answer.
8. **Deep research** — research action, harness prompt, output selection.
9. **Map & Timeline** — entity/event extraction ingest, both views.
10. **Install & release** — install.sh, release CI, first-run experience.

Each change lands runnable; the user can steer between layers.

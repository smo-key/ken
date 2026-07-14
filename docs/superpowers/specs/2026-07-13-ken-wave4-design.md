# Ken wave 4 — local model, denser Map, Record, automations, ingest visibility

Date: 2026-07-13. Status: approved by Arthur (this session); §9 (chat
usability), §10 (Settings simplification), §11 (large-file preview hang), and §12 (file operations)
added after approval at Arthur's request.

Eleven work items. Items 1 and 4 share one new subsystem (the embedded local
LLM); the rest are independent.

User decisions (recorded from the clarifying round):

- Map: **incremental extraction, powered by the local model** (not Claude).
- Local model tier: **~4B** (Qwen3-4B-Instruct, Q4_K_M GGUF, ~2.5 GB).
- Record v1 speaker labels: **Me/Them via channel separation** (mic vs system
  audio), not embedding-based diarization.
- Automations acting on external services: **ask first, per rule** — an
  auto-apply toggle per automation, default off (staged into Review).

## 1. Shared subsystem: embedded local LLM

New `local_llm` module in `ken-core` behind a default-on cargo feature
(mirroring the `whisper` feature, `crates/ken-core/Cargo.toml:29-37`).

- Runtime: `llama-cpp-2` crate with the Metal feature. One model loaded
  per app process, lazily, on first use.
- Model: Qwen3-4B-Instruct GGUF Q4_K_M downloaded from Hugging Face into the
  app-data `models/` directory (default; Settings offers an advanced 8B
  alternative — see §10). Download runs in Rust with progress events;
  the frontend reuses the Whisper flow's `ModelDownloadDialog`
  (`src/files/previews/ModelDownloadDialog.svelte`) generalized to name the
  model it is fetching. Checksum verified; partial downloads resume or restart.
- Serialization: a single inference queue with two priorities. Quick answers
  (interactive) preempt map extraction (background): an extraction job checks a
  "yield" flag between generations; a queued quick answer causes at most one
  in-flight extraction generation's latency before running.
- API surface (internal): `local_llm::generate(prompt, opts) -> stream of
  tokens` and `local_llm::generate_json(prompt, schema_hint) -> serde_json::Value`
  (greedy decode, retry-once-on-parse-failure).
- Failure handling: if the feature is off, the model file is missing and the
  user declines the download, or llama.cpp fails to init, callers fall back:
  quick answers use the current Claude oneshot path; map extraction pauses with
  a visible notice on the Map screen. No hard errors in the UI.

## 2. Map — incremental entity extraction (replaces auto one-shot build)

Today the Map is built by ONE headless Claude session over the whole corpus
with `MAX_ENTITIES = 200` (`crates/ken-core/src/knowledge_model.rs:26,205`),
so node count is decoupled from corpus size (Skipa: 181 files → 49 entities).

New pipeline:

- **Trigger**: when a file reaches `indexed` (initial scan or watcher rescan),
  it is enqueued for extraction if its content hash differs from the last
  extracted hash. New bookkeeping table `extractions(rel_path PRIMARY KEY,
  content_hash, extracted_at, status, error)`.
- **Worker**: one background extraction worker per open project, feeding the
  local-LLM queue at background priority, one file per job. Prompt: the file's
  already-extracted text (from `contents`), truncated to a token budget that
  fits the model context, with instructions to emit strict JSON:
  `{entities: [{kind, name, summary}], relations: [{a, b, label}],
  events: [{date, category, text}]}` — kinds and shapes identical to the
  current extraction parser (`knowledge_model.rs:106`).
- **Merge** (new `Db::merge_knowledge_delta`): entities dedup by
  (kind, casefolded/whitespace-normalized name); on match, union `sources`
  and keep the longer summary. Edges dedup by unordered entity pair (first
  label wins). Events dedup by (date, text). Re-extracting a changed file
  first removes that file from all `sources` arrays, deleting entities whose
  sources become empty (and their edges), then applies the new delta — so
  deletions and rewrites converge instead of accreting.
- **Caps**: no global entity cap. Per-file sanity caps only (e.g. ≤ 40
  entities, ≤ 60 relations, ≤ 20 events per file) to bound junk from one bad
  generation.
- **UI**: MapScreen shows coverage — "164 of 181 files analyzed" with a subtle
  progress treatment while the worker is behind; the graph refreshes
  incrementally (listen for a new `knowledge-updated` event emitted after each
  merged file, throttled). Verify layout behavior at ~500 nodes; keep
  `PROMINENT_LABELS` behavior as-is.
- **Old path**: the Claude one-shot build no longer auto-runs (retire the 30s
  `KNOWLEDGE_TICK` auto-build policy); it remains as a manual "Deep rebuild"
  action on the Map screen for a curated pass, which REPLACES the whole model
  (as today).

## 3. Files header (file-tree sidebar)

`src/files/FileTree.svelte`: the whole `.tree` column scrolls
(`overflow-y: auto`), so the "Files" `.tree-head` (`:104-114`) scrolls away.

- Restructure so Favorites and the tree scroll but the **Files header row is
  fixed**: header sits outside/sticky above the scrolling `.nodes` region
  (sticky within the scroll container is acceptable; visually pinned is the
  requirement).
- Remove the "Manage folders" settings button (`:106-113`).
- Add to the header, right-aligned: an **icon-only Import button** (Upload
  icon, tooltip "Import file", calls the same `startImport()` flow as today's
  toolbar button in `src/screens/FilesScreen.svelte:88-96`) and the
  **All/Unread segmented filter** (compact version of
  `FilesScreen.svelte:102-123`; same visibility rule — only when something is
  unread or the filter is active).
- The right-pane toolbar drops its Import button and filter; it keeps the tab
  strip and "Mark all as viewed."

## 4. Ingests — visibility and pacing

Root causes found: one run at a time with a 30s debounce and a 15-minute
ceiling (`crates/ken-core/src/engine.rs:47,89,121`), a single static "running"
state with no incremental progress, and a silent return on no-op plans
(`engine.rs:224-226` — no event at all).

- **Live activity**: run ingest sessions with `--output-format stream-json`
  (headless), parsing the same event stream chat already parses
  (`chat.rs:37-72`), and carry a transient `activity` string + elapsed seconds
  on the run's `running` state (e.g. "reading Meetings/2026-07-13…",
  "editing knowledge/People.md"). IngestsScreen renders the activity line and
  a live timer. Hidden-TUI fallback keeps today's behavior.
- **No silent runs**: the no-op path records a completed run ("checked —
  nothing to update") in `ingest_runs` and emits the event.
- **Queue visibility**: debouncing/pending recipes surface "queued — starts in
  Ns" (countdown) and "waiting for <current run>" states on the screen.
- **Pacing**: default debounce 30s → 10s (`EngineConfig::debounce`).
  Concurrency stays 1 (deliberate: Claude sessions are heavy; the complaint is
  visibility, not throughput).

## 5. Search — instant local quick answers

The ⌘K overlay already detects questions (`isQuestionQuery`,
`src/lib/assist.ts:4`) and renders a Quick answer card
(`src/search/SearchOverlay.svelte:122-141`) fed by `quick_answer`
(`src-tauri/src/lib.rs:2686-2717`, currently a Claude oneshot over the top-8
FTS hits).

- Reimplement `quick_answer` on the local model: same FTS grounding, streamed
  tokens via a `quick-answer-delta` event so the card fills in live; keep the
  final `quick-answer` event shape (query, body, sources) for completion.
- Cancel/supersede: a newer query cancels the in-flight generation.
- "⌘↵ dig deeper in chat" (send to chat → Claude) unchanged.
- Fallback: local model unavailable → current Claude oneshot path, silently.

## 6. Record

New nav-rail section **Record** (`src/shell/NavRail.svelte:13-20`,
`Screen` union `src/lib/app.svelte.ts:48-55`, lazy pane in
`src/shell/Shell.svelte:43-61`), plus a `record` module in ken-core.

- **Capture**: mic via `cpal` (input device picker); system audio via
  ScreenCaptureKit audio-only capture (macOS 13+; Screen Recording TCC).
  Either source can be toggled independently; both may run together.
  Each active source records to its own 16 kHz mono WAV in a temp workspace.
- **UI**: device picker, source toggles, level meters (rms events from Rust),
  elapsed clock, pause/resume, stop. Permission guidance inline: detect
  missing mic/screen permission and show how to grant (deep-link to System
  Settings panes) — consistent with the app's existing no-Accessibility
  posture (screen-recording permission is new and must be requested).
- **Transcription**: on stop, each channel runs through the existing Whisper
  integration (`crates/ken-core/src/transcript.rs`, segments with timestamps).
  Merge segments from both channels by start time into one transcript;
  speaker labels **Me** (mic) / **Them** (system). Single-source recordings
  get no labels.
- **Output & storage choice** (per recording, chosen before/at stop):
  *transcript only*, *audio only*, or *both*. Files land in the project under
  `Recordings/`: `YYYY-MM-DD HH.MM Recording.md` (transcript with a small
  metadata header) and one `.wav` per channel, kept as recorded (16 kHz mono;
  no ffmpeg requirement for saving). Transcript-only deletes the WAVs after a
  successful transcription — never before. Everything written to the project
  is picked up by the normal scan/index path, hence searchable and eligible to
  trigger automations.
- Failure handling: transcription failure keeps the audio (regardless of
  storage choice) and surfaces the error with a retry.

## 7. Automations

Generic trigger→agent rules; no per-service integrations. Jira (or anything
else) is reached through whatever **MCP servers the user has configured** for
their `claude` setup — Ken just runs the session.

- **Model**: an automation = `{ name, trigger glob(s) on indexed rel_paths,
  prompt, auto_apply: bool (default false), enabled }`. Stored alongside
  recipes in the project's `.ken/` config. Trigger evaluation hooks the same
  index-change notifications the ingest engine already consumes
  (`engine.rs` `SourcesChanged`), with the same debounce machinery; matched
  file list is passed to the prompt.
- **Execution**: through the existing runner/engine queue (shared
  serialization with ingests). Two-phase gating:
  - `auto_apply = false`: phase 1 runs with instructions to research and
    produce a **proposal** (markdown: summary + explicit list of intended
    external actions) and write no external changes. The proposal is staged
    as a `review_items` row (existing table + Review screen). Approving it
    queues phase 2: a session told to execute exactly the approved actions
    (MCP tools available). Rejecting discards it.
  - `auto_apply = true`: one session does research + actions directly.
  - Note: phase-1 restraint is prompt-enforced (the runner already grants
    `acceptEdits`); the Review gate is the real control — phase 2 is the only
    run the user has blessed to act externally. Good-enough for v1; noted as
    a known limitation in the UI copy.
- **Runs & visibility**: automation runs log into `ingest_runs` (new
  `kind` discriminator) and appear with the same live-activity treatment as
  ingests (§4).
- **UI**: IngestsScreen becomes two tabs — **Knowledge docs** (current
  recipes) and **Automations** (list, create/edit form: name, glob, prompt,
  auto-apply toggle, run history, run-now button).
- **Walkthrough of the target use case**: Record (§6) writes
  `Recordings/2026-07-13 14.02 Recording.md` → index picks it up → automation
  with glob `Recordings/*.md` triggers → proposal "summary + 3 Jira tasks"
  appears in Review → user approves → apply session creates the issues via
  the user's Atlassian MCP server and writes the summary doc.

## 8. PPTX renderer

Diagnosed against `Website Options v2.pptx` (Skipa): slide 1 has 139 shapes —
127 `custGeom` vector shapes inside 7 groups, colored by `schemeClr` theme
references; slide 2's content is one `a:tbl` inside a `p:graphicFrame`. The
parser (`src/files/previews/pptx.ts`) currently: drops shapes with no text and
no explicit `srgbClr` fill (`parseSp`), flattens `p:grpSp` while ignoring the
group's transform (children land at child-space coordinates), never resolves
theme colors (`solidFillColor`), and skips `p:graphicFrame` entirely
(`parseSlide` walk).

Upgrades, in the existing pure-parser + component split:

1. **Group transforms**: carry `a:xfrm` (`off/ext/chOff/chExt`, plus `rot`
   and `flipH/flipV` pass-through where cheap) down the group walk, mapping
   child coordinates into slide space. Nested groups compose.
2. **Theme colors**: parse the slide master's `clrMap` and the theme part's
   color scheme once per deck; resolve `a:schemeClr` (with `lumMod/lumOff/
   tint/shade/alpha` transforms) wherever `srgbClr` is handled today (fills,
   run colors, lines).
3. **Custom geometry**: translate `a:custGeom` `pathLst` (moveTo/lnTo/
   cubicBezTo/arcTo/close) into an SVG path scaled from `pathW/pathH` to the
   shape box; render as inline `<svg>` with resolved fill and outline
   (`a:ln` width/color). Unknown commands degrade to the bounding rect as
   today.
4. **Tables**: walk `p:graphicFrame` → `a:tbl`; render rows/cells (gridSpan/
   vMerge respected, cell fills and text via the existing paragraph parser)
   as an HTML table positioned by the frame's xfrm.

## 9. Chat usability

Two reported bugs plus an audit:

- **First message vanishes**: after typing the first message and hitting
  Enter, it does not appear in the transcript. `ChatsStore.send`
  (`src/lib/chats.svelte.ts:86-99`) never appends the user's message locally —
  the transcript only grows via backend `chat-message` events
  (`init`, `:43-47`), which evidently do not echo the user message (or arrive
  for a different/newer chat id). Fix: optimistically append the user message
  in `send()` (with a pending marker), and reconcile against any backend echo
  by id/ordinal so it is never duplicated or dropped; diagnose the backend
  event path while in there so history reloads (`chatTranscript`) also include
  it.
- **Focus on open**: whenever the chat drawer opens or a chat tab is
  selected (open drawer, `newChat`, `select`, exit-terminal), the message
  input receives keyboard focus so the user can type immediately.
- **Usability audit**: drive the real app (live-test recipe in the project's
  `verify` skill) through the chat surface — new chat, send/stream, model
  switch, needs-input flow, terminal mode enter/exit, archive, pin, drawer
  resize, error states — and file/fix the issues found. Known candidates to
  check: send() failures only surface via `sendError` (visibility?), transcript
  scroll behavior on new messages, Enter vs Shift+Enter, suggested prompts,
  state after archiving the active chat. Findings that are quick fixes land in
  this wave; anything structural gets written up for the next wave.

## 10. Settings simplification (reviewed under Paper & Ink / impeccable)

Current `src/screens/SettingsScreen.svelte` review findings: seven identical
cards at uniform 18px gaps (no editorial hierarchy or rhythm); the Appearance
card fakes its row label with a per-option ternary; "Transcription model"
lists every `ggml-*.bin` the whisper.cpp repo ships (runtime discovery,
`crates/ken-core/src/model.rs`) — an unbounded, jargon-named list; Watched
folders is a flat indented checkbox list of every folder with an "excluded"
tag. Changes:

- **Offline Models card** (replaces "Transcription model"): two categories,
  each offering exactly **Recommended** or **Advanced** — a curated pair, not
  the discovered repo listing (`model.rs` gains a curated catalog with the
  same download/verify/install plumbing; discovery code retires):
  - *Transcription* — Recommended: Whisper Base (English), 148 MB ("fast,
    accurate for meetings"). Advanced: Whisper Large v3 Turbo, ~1.6 GB
    ("best accuracy, understands more languages, slower").
  - *Answers & Map* (the §1 language model) — Recommended: Qwen3 4B, ~2.5 GB
    ("instant answers, builds your map"). Advanced: Qwen3 8B, ~5 GB ("smarter
    answers, needs more memory").
  - Interaction: one radio pair per category; picking an uninstalled model
    starts its download inline (existing compact download flow, progress in
    place); the non-selected installed file offers a quiet "Remove" to free
    disk. The selected model is what the feature uses. Copy keeps the promise
    up front: "These run on your Mac — nothing you say or store leaves it."
  - The Whisper file constant stops being load-bearing: `transcript.rs` uses
    whichever transcription model is selected (settings-persisted), falling
    back to any installed one.
- **Watched folders tree**: roots-only at first, chevron to expand/collapse
  (quiet grid-rows transition, no bounce). Tri-state checkboxes: checked =
  folder and everything under it watched; unchecked = excluded; indeterminate
  ([-], native `indeterminate`) = some descendant excluded. Toggling a parent
  applies to its whole subtree (the exclusion model is already prefix-based).
  The "excluded" tag goes — the checkbox says it. The note copy stays plain.
- **Page rhythm**: cards group under three quiet uppercase section headings —
  *This project* (Project, Watched folders, Ignored files, Cloud files,
  Sync & collaboration), *On this Mac* (Appearance, Offline models, AI
  runner), *Working with agents* (Connect an agent) — generous separation
  between groups, tighter within, restoring hierarchy without new chrome.
- **Appearance**: the three radio rows collapse to one "Theme" row with the
  app's existing segmented-control idiom (as in the Files All/Unread filter);
  the ternary label hack goes.

## 11. Large-file preview hang ("Opening…" stuck)

Reported with `Research/Data/irs-bmf/eo3.xlsx` (148 MB): opening it wedges
the UI on "Opening…" with no way out. Root cause class is the same as the
fixed CSV startup freeze: the size guards in `EditorPane.svelte`
(`GRID_EDIT_MAX`/`TEXT_EDIT_MAX`/`WYSIWYG_MAX`) only cover *editable*
text/CSV paths — office previews (`XlsxPreview`, `DocxPreview`,
`PptxPreview`, ipynb) read and parse the whole file on the main thread with
no cap, so a huge workbook blocks the webview (nothing is cancellable because
nothing yields).

- **Size gate**: per-format preview caps (e.g. xlsx/docx/pptx/ipynb ~15 MB;
  tune per format against real parse times) checked against `meta.size`
  BEFORE any bytes are read. Over the cap → the existing "too large" notice
  treatment with Open in Finder / external-app actions, same as big text
  files today. Trust `meta.size` (already loaded) — no extra I/O.
- **Never wedge**: under-cap previews parse off the main thread where the
  parser allows (yielding/chunked parse as done for the CSV grid), and the
  "Opening…" state always renders a working Cancel (back to the notice) —
  the tab strip and rest of the app must stay responsive while a preview
  loads. Regression test with a synthesized many-row workbook fixture.

## 12. Files tree — basic file operations

Requested after §10-11. What already exists: drag-and-drop file moves
(`src/files/dnd.svelte.ts`, drop targets in `TreeNodeRow.svelte`, root drop
zone in `FileTree.svelte`, backend `move_file` in `src-tauri/src/lib.rs:918`
with cross-device fallback) and right-click context menus on tree rows. Gaps
this section closes:

- **Context menu additions** (tree rows and the tree's empty/root area):
  *New folder*, *New document* (creates a markdown text file), and *Rename*
  (files and folders), alongside the existing entries. Folder rows' menus get
  New folder/New document scoped inside that folder.
- **Inline editing**: rename and both create actions edit the name in place
  in the tree row (autofocused input, Enter commits, Esc cancels). New
  documents default to `Untitled.md` (deduped `Untitled 2.md`, …) and open in
  a tab on create. Validation (no `/`, no duplicate sibling name) surfaces
  through the tree's existing non-blocking move-error notice.
- **Backends**: `create_folder(rel_path)`, `create_document(rel_path) ->
  final rel_path` (dedupe + empty file + index), and rename reuses
  `move_file`, extended to accept directories (same-parent rename and full
  moves); child index paths reconcile via the existing rescan/watcher path.
- **Folder drag-and-drop**: folders become draggable like files, with a
  guard against dropping a folder into itself or its own subtree; drop onto
  a collapsed folder targets that folder.

## Execution note

Implementation will run subagent-driven (per superpowers), with subagents on
**Opus** — Arthur's directive for all wave-4 work.

## Testing

TDD per item. Rust: extraction merge semantics (dedup, source-union,
re-extract-removes, empty-source GC), extraction queue priorities (quick
answer preempts), automation trigger matching + two-phase staging into
`review_items`, engine no-op run emission, transcript channel-merge ordering,
recorder WAV plumbing with synthesized fixtures. Vitest: pptx group
transforms / theme resolution / custGeom path building / table model (XML
fixtures cut from the real deck), FileTree header layout, SearchOverlay
streamed quick-answer state, chat store optimistic-echo reconciliation,
watched-folders tri-state tree logic, curated model catalog selection, and
preview size-gate behavior (big-workbook fixture never reaches the parser). Manual live-test per the project's existing
`verify` recipe; recording and screen-capture permission flows verified by
hand (TCC cannot be automated).

## Out of scope (this wave)

Embedding-based speaker diarization (Speaker 1/2/3), Windows/Linux system
audio capture, automation triggers other than file globs (cron, manual-only
is free via run-now), local-LLM chat, PPTX gradients/effects beyond solid
fills, and any per-service (Jira, etc.) client code in Ken.

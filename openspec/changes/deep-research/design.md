# Design: deep-research

## Context

The runner stack is already everything a research run needs: hidden-TUI
sessions in a PTY (`runner::run_hidden_tui`), completion via the Stop
hook, `Notification` → blocked, a 60-second startup-gate check for trust
prompts, live watch/answer through `pty_registry` + the chat drawer, and
chat rows that ingest runs already use for status badges. Deep research
is a thin new module that composes a different prompt and wires the same
machinery — plus a small amount of UI.

## Goals / Non-Goals

**Goals**

- One question → one new markdown report in a user-chosen project
  folder, written by the agent itself.
- Always interactive (hidden TUI): research may legitimately need to ask
  the user something mid-run, and the drawer is where that happens.
- The run is a first-class chat-drawer citizen: badge while working,
  needs-input when blocked, terminal view live and after the fact.
- No network in tests — the fake claude grows one additive trick.

**Non-Goals**

- No staging/review-threshold pipeline. Research writes a NEW document;
  there is nothing to overwrite, so nothing to gate. Novelty is enforced
  at plan time instead: the chosen filename must not already exist.
- No run history table — the chat row and its transcript are the record.
- No scheduling; research is always user-initiated.

## Decisions

### ken-core `research.rs`

- **`slugify(question) -> String`** — lowercase kebab of alphanumeric
  runs, capped at 50 chars without cutting a word mid-way when
  avoidable, falling back to `research` for empty input.
  **`unique_report_path(dir_abs, slug) -> String`** appends `-2`, `-3`,
  … until `<slug>.md` is free. Split from slugify so both are unit
  testable; `plan_report(project, rel_dir, question)` composes them and
  is what the command calls.
- **`validate_output_dir(project, rel_dir)`** — `Project::resolve`
  already refuses `..`/absolute; on top of that the dir must not be
  `.ken` or under it. Anything else inside the project is fair game
  (the folder is created on demand).
- **`compose_research_prompt`** carries the project-relative report path
  in prose (that's what the user sees) and a machine-checkable
  `OUTPUT_FILE=<abs>` line, mirroring the `STAGING_DIR=` convention the
  ingest prompt uses — one greppable contract line for the fake CLI and
  for humans debugging a session.
- **`run_research`** takes the session id from the caller (the Tauri
  command needs it up front — it doubles as the chat id), installs
  hooks, and calls `runner::run_session` with `RunnerMode::HiddenTui`
  unconditionally. The `ingestRunner` project setting is deliberately
  ignored: headless can't answer questions. Timeout is a parameter with
  a 30-minute default (`DEFAULT_TIMEOUT`) — research runs long.
  On `Completed` it verifies the report exists and downgrades to
  `Failed` with plain-language detail when it doesn't.

### src-tauri

- `ActiveProject.research: Arc<Mutex<HashMap<String, CancelToken>>>` —
  one token per live run, removed when the worker finishes. Cancel is a
  lookup + `token.cancel()`; the runner does the killing.
- `start_research` follows the ingest-event pattern in `activate()`:
  chat rows via the shared `chat_db`, `chat-updated` events on every
  status flip, `chat-message` events for activity lines. Status mapping:
  running → `working`, blocked → `needs_input`, Completed → `done`,
  everything else → `error` (cancelled → `done` with a "Cancelled"
  activity line — a user choice isn't an error).
- The report file lands in the project folder, so the existing watcher
  picks it up and indexes it — no extra wiring, and it immediately
  behaves like any other document (search, editor, preview, ingests).
- `research_output_options` reads top-level directories straight from
  disk (skipping `.ken`, hidden and junk dirs, and excluded folders),
  with `research` always first — same on-disk-first philosophy as
  `get_tree`.

### Chat drawer & UI

- `chats.svelte.ts::select()` auto-enters terminal mode for kind
  `research` exactly like `ingest`: while the run lives, the registry
  attach taps the real PTY; afterwards `enter_terminal_mode` falls back
  to spawning `claude --resume <chat id>`, so the transcript stays
  reachable. `send_chat_message` rejects research chats like ingest
  chats — the terminal is the only interaction surface.
- The drawer foot shows "Research session — opens in the terminal; you
  can answer its questions there." and a Cancel mini button whenever the
  active research chat is `working`/`needs_input` (visible in terminal
  mode too, since research chats open straight into it).
- `ResearchModal` keeps the choice simple: a select of known locations
  seeding an editable path field, so "any folder" costs one edit and the
  default costs zero.

## Risks / Trade-offs

- **The agent might not write the report.** Mitigated three ways: the
  prompt makes the file the mandatory deliverable (even on web failure),
  completion verifies existence, and the failure message names exactly
  what happened.
- **Interactive-only means a parked trust dialog can stall a run.** The
  existing 60s session-file gate check fires `on_blocked`, the chat row
  flips to needs-input, and the drawer terminal lets the user answer —
  the same story ingest hidden-TUI runs already have.
- **Question text in a filename.** Slugify strips to alphanumerics and
  caps length; collisions get numeric suffixes; the path never leaves
  the validated folder.

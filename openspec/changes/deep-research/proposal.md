# Proposal: deep-research

## Why

Ken's design promises a deep research mode (§4.5): the user types a
question, picks where the report should land (default `research/`, or
any folder in the project), and Ken fans out web searches, reads
sources, cross-checks claims, and writes a cited markdown report into
the project. The report is a normal project document — indexed,
editable, previewable, available to ingests — and the run shows in the
chat drawer with the same status badges as ingest runs. Change 8 of 10
ships all of it on top of the runner, hooks, PTY registry, and chat-row
plumbing that already exist.

## What Changes

- **New ken-core module `research.rs`**:
  - `slugify(question)` — kebab-case filename stem capped at ~50 chars —
    plus plan-time collision handling (`-2`, `-3`, …) against files that
    already exist, so a research run always creates a NEW document and
    can never overwrite anything.
  - `validate_output_dir(project, rel_dir)` — the chosen folder must be
    inside the project and must not be (or live under) `.ken`.
  - `compose_research_prompt(question, output_rel_path, output_abs)` — a
    strong research harness: role, METHOD (break the question into
    angles, multiple web searches per angle, read the strongest sources,
    cross-check every load-bearing claim across at least two independent
    sources, note disagreements honestly), OUTPUT (one complete markdown
    report at the exact path given: title, date, executive summary,
    findings by theme, "What remains uncertain", Sources with one-line
    notes, inline `[1]` citation markers), RULES (prefer primary
    sources; if the web is unavailable, still write the report saying
    exactly that; writing the report file is mandatory; prefer stating
    assumptions over asking, but a truly necessary clarifying question
    is allowed). The prompt carries an `OUTPUT_FILE=<abs path>` contract
    line.
  - `run_research(...)` — mirrors `engine::execute`'s runner wiring
    (session id supplied by the caller, `install_hooks`,
    `runner::run_session`) but ALWAYS in `RunnerMode::HiddenTui`,
    regardless of the `ingestRunner` setting: research must be able to
    ask the user questions mid-run, the PTY registry + chat drawer make
    the session watchable and answerable, and the 60s startup-gate
    detection covers trust prompts. Default timeout 30 minutes. On
    Completed, the report file must exist (the agent writes directly to
    the project path — research output is a new document, so there is
    nothing to stage or threshold-gate); a "completed" run with no
    report is a failure.
- **src-tauri research manager** on the active project — a map of
  session id → `CancelToken` — and three commands:
  - `start_research(question, outputDir) -> chatId`: validates the
    folder, plans a fresh `outputDir/slug.md`, upserts a ChatRow
    `{kind: "research", title: "Research — <question>", status:
    "working"}`, appends an activity line naming where the report will
    land, and spawns a worker thread running `run_research`. Blocked →
    chat status `needs_input`; done/failed/cancelled → status + a
    plain-language activity message with the report path or the reason.
    The finished report lands in the project folder, so the existing
    watcher indexes it with no extra wiring.
  - `cancel_research(chatId)` — cancels the token.
  - `research_output_options()` — `research` first (always, even before
    it exists), then the project's existing top-level folders, excluded
    ones omitted.
- **Chat drawer**: research chats already appear via chat rows. Kind
  `research` behaves like `ingest` — selecting one opens terminal mode
  (live PTY via the registry while running; `--resume` transcript after)
  — plus a research-specific foot note and a "Cancel" mini action while
  the run is `working`/`needs_input`.
- **Entry point**: a "Start research" button on Home (Your-knowledge
  card action row) opens `ResearchModal`: question textarea, output
  location select fed by `research_output_options()` seeding an editable
  path field, and a one-line plain-language note. Starting closes the
  modal, opens the chat drawer, and selects the new research chat.
- **Fake claude (tests, additive only)**: the TUI `complete` branch also
  parses `OUTPUT_FILE=<path>` from the prompt (alongside `STAGING_DIR=`)
  and writes a small fake report there before emitting Stop. Every
  existing behavior is untouched.

## Capabilities

### New Capabilities
- `deep-research`: the research harness, runner wiring, Tauri commands,
  chat-drawer integration, and the Home entry point.

### Modified Capabilities

_None — research is an additive layer over the existing runner, hooks,
PTY registry, chat rows, and watcher._

## Impact

- `crates/ken-core`: new `research.rs` (slugify, output-dir validation,
  prompt composer, `run_research`); `runner.rs` test_support learns
  `OUTPUT_FILE=`; `lib.rs` module line.
- `src-tauri`: research cancel-token map on `ActiveProject`;
  `start_research`, `cancel_research`, `research_output_options`
  commands; research chats rejected by `send_chat_message` like ingests;
  terminal resume covers research chats.
- Frontend: `api.ts` types + wrappers; `chats.svelte.ts` treats
  `research` like `ingest`; `ChatDrawer.svelte` foot note + Cancel;
  `HomeScreen.svelte` button; new `src/research/ResearchModal.svelte`.
- Tests: research unit tests (slugify incl. collisions, dir validation,
  prompt contract) and end-to-end runs against the fake claude
  (complete + report written, fail, blocked→cancel). All existing tests
  stay green.

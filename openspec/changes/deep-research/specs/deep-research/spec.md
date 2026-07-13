# deep-research

## ADDED Requirements

### Requirement: Report naming and placement
ken-core SHALL derive the report filename from the question via
`slugify` — lowercase kebab-case of the question's alphanumeric runs,
capped at roughly 50 characters, `research` when nothing survives — and
SHALL plan the report as `<outputDir>/<slug>.md`. When that file already
exists, a numeric suffix (`-2`, `-3`, …) SHALL be appended until the
name is free, so a research run always creates a new document and never
overwrites an existing one.

#### Scenario: Question becomes a kebab filename
- **WHEN** the question is "What's the state of EU AI regulation in 2026?"
- **THEN** the planned filename stem is a kebab-case slug such as
  `whats-the-state-of-eu-ai-regulation-in-2026`, at most ~50 characters

#### Scenario: Collision gets a numeric suffix
- **WHEN** `<outputDir>/<slug>.md` already exists
- **THEN** the planned path becomes `<outputDir>/<slug>-2.md` (then
  `-3`, …), never the existing file

### Requirement: Output folder validation
ken-core SHALL validate the chosen output folder: it must resolve inside
the project (no absolute paths, no `..`) and must not be `.ken` or any
path under it. The folder need not exist yet — it is created when the
report is written.

#### Scenario: Escaping paths are rejected
- **WHEN** the output folder is `../elsewhere` or `/tmp`
- **THEN** validation fails

#### Scenario: Ken's own state dir is rejected
- **WHEN** the output folder is `.ken` or `.ken/anything`
- **THEN** validation fails

#### Scenario: A folder that doesn't exist yet is accepted
- **WHEN** the output folder is `research` and no such folder exists
- **THEN** validation succeeds

### Requirement: Research harness prompt
`compose_research_prompt(question, output_rel_path, output_abs)` SHALL
produce a prompt that: casts the agent as Ken's research assistant;
directs it to break the question into angles, run multiple web searches
per angle, read the strongest sources, cross-check every load-bearing
claim across at least two independent sources, and note disagreements
honestly; requires ONE markdown report at the exact project-relative
path given — a complete document with title, date, a 3–5 sentence
executive summary, findings organized by theme, a "What remains
uncertain" section, and a Sources section listing every URL used with a
one-line note, tied to inline citation markers like `[1]`; and states
the rules — prefer primary sources, write the report even when the web
tools are unavailable (saying exactly that), the report file is the
mandatory deliverable, and prefer stating assumptions over asking,
though a truly necessary clarifying question is allowed. The prompt
SHALL carry the line `OUTPUT_FILE=<absolute path>`.

#### Scenario: Prompt carries the contract
- **WHEN** the prompt is composed for a question and report path
- **THEN** it contains the question, the project-relative report path,
  an `OUTPUT_FILE=` line with the absolute path, the cross-checking
  method, the report structure (executive summary, uncertainty section,
  Sources), and the write-the-report-no-matter-what rule

### Requirement: Interactive research runs
`run_research` SHALL run one hidden-TUI session (never headless,
regardless of the project's `ingestRunner` setting) with the caller's
session id, installing Ken's hooks first, honoring a cancel token and a
timeout that defaults to 30 minutes, and firing `on_blocked` when the
session waits on input. On a Completed outcome the report file MUST
exist; a completed session that wrote no report SHALL be reported as a
failure that says so.

#### Scenario: Successful run writes the report
- **WHEN** the agent completes and the report file exists at the planned
  path
- **THEN** the outcome is Completed

#### Scenario: Completed without a report is a failure
- **WHEN** the agent signals Stop but no file exists at the planned path
- **THEN** the outcome is a failure explaining the report was not
  written

#### Scenario: Crash is a failure with detail
- **WHEN** the agent process dies before completing
- **THEN** the outcome is a failure carrying diagnostic detail

#### Scenario: Blocked fires and cancel works
- **WHEN** the session signals it is waiting on user input and the user
  cancels
- **THEN** `on_blocked` has fired and the outcome is Cancelled

### Requirement: Research commands
The app SHALL expose `start_research(question, outputDir) -> chatId`,
`cancel_research(chatId)`, and `research_output_options() ->
Vec<String>`. `start_research` validates the folder, plans a fresh
report path, upserts a chat row `{kind: "research", title: "Research —
<question, trimmed to 40 chars>", status: "working"}`, emits
`chat-updated`, appends an activity message naming where the report will
land, and runs the research on a worker thread. Status flips (blocked →
`needs_input`, finished → `done`/`error`) update the chat row and emit
events; completion appends a plain-language activity message carrying
the report path or the failure reason; the cancel token is removed when
the worker ends. `research_output_options` returns `research` first —
always, even before the folder exists — followed by the project's
existing top-level folders, excluded ones omitted.

#### Scenario: Starting research creates a working chat
- **WHEN** `start_research` is called with a valid question and folder
- **THEN** it returns a chat id whose row has kind `research` and status
  `working`, and an activity message names the planned report path

#### Scenario: Finished research reports its outcome in the chat
- **WHEN** the run finishes
- **THEN** the chat status becomes `done` (or `error`) and an activity
  message carries the report's path (or a plain-language reason)

#### Scenario: Cancel stops the run
- **WHEN** `cancel_research(chatId)` is called while the run is live
- **THEN** the session is cancelled and the chat reflects it

#### Scenario: Output options start with research
- **WHEN** `research_output_options` is called
- **THEN** the first option is `research` and the rest are the project's
  existing top-level folders, excluded ones omitted

### Requirement: Research chats in the drawer
Research chats SHALL behave like ingest sessions in the chat drawer:
selecting one opens terminal mode (attached to the live PTY while the
run is active; resumed transcript afterwards), typed conversation
messages are rejected with a pointer to the terminal, and the drawer
foot explains "Research session — opens in the terminal; you can answer
its questions there." While a research chat is `working` or
`needs_input` the drawer SHALL offer a Cancel action that calls
`cancel_research`.

#### Scenario: Selecting a research chat opens the terminal
- **WHEN** the user selects a research chat in the drawer
- **THEN** terminal mode opens for that chat

#### Scenario: Cancel is offered while the run is live
- **WHEN** the active research chat's status is `working` or
  `needs_input`
- **THEN** a Cancel action is visible and invokes `cancel_research`

### Requirement: Start research from Home
Home's Your-knowledge card SHALL offer a "Start research" action opening
a modal with: a question textarea ("What should Ken research on the
web?"), an output-location select fed by `research_output_options()`
that seeds an editable path field, the note "Ken will search the web and
write a cited report into your project.", and a primary "Start research"
button. Starting closes the modal, opens the chat drawer, and selects
the new research chat.

#### Scenario: Modal starts a run and opens the drawer
- **WHEN** the user enters a question and clicks Start research
- **THEN** `start_research` is invoked with the question and the chosen
  folder, the modal closes, and the chat drawer opens on the new chat

### Requirement: Fake-claude research support (tests)
The test fake claude SHALL additionally parse `OUTPUT_FILE=<path>` from
the prompt in its TUI `complete` behavior and write a small fake report
to that path before emitting Stop. All existing behaviors (`fail`,
`hang`, `block`, `headless-fail`, staging writes, conversation mode)
SHALL remain unchanged.

#### Scenario: Fake writes the report on complete
- **WHEN** a hidden-TUI session runs with a prompt containing
  `OUTPUT_FILE=<path>` and behavior `complete`
- **THEN** a markdown file exists at `<path>` when the Stop hook fires

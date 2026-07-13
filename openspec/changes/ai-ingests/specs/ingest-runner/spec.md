# ingest-runner

## ADDED Requirements

### Requirement: Runs use the local Claude Code CLI
Ingest runs SHALL execute through the user's locally installed `claude`
CLI (their existing authentication) with the project folder as working
directory and a Ken-generated session id. Ken SHALL NOT require or store
any API key.

#### Scenario: Run uses local CLI
- **WHEN** an ingest run starts
- **THEN** a `claude` process is spawned with cwd = project root and a
  Ken-generated `--session-id`

### Requirement: Hidden-TUI default with headless setting
By default a run SHALL spawn an interactive `claude` session in a hidden
PTY, detecting completion via a Stop hook delivered to Ken's localhost
listener. A per-project setting `ingestRunner: headless` SHALL switch runs
to `claude -p` with process-exit completion. Hook configuration SHALL be
written to the project's `.claude/settings.local.json`, replacing only
Ken's own prior entries and preserving all other user settings, and SHALL
ignore hook events from sessions Ken did not start.

#### Scenario: Hidden-TUI completion
- **WHEN** a hidden-TUI run's Stop hook fires for Ken's session id
- **THEN** the run completes and the session process is shut down

#### Scenario: Headless completion
- **WHEN** `ingestRunner` is `headless` and the `claude -p` process exits
  successfully
- **THEN** the run completes without any hook involvement

#### Scenario: Foreign sessions unaffected
- **WHEN** the user runs their own `claude` session in the project folder
  while Ken's hooks are installed
- **THEN** Ken ignores that session's hook events and the user's session
  behaves normally

### Requirement: Failure, blocking, and timeout are explicit states
A run SHALL become `failed` (with diagnostic detail) if the CLI exits
before completing or exceeds the run timeout, and `blocked` if the agent
signals it is waiting on user input; blocked runs SHALL be cancellable.
If the `claude` binary is not installed, runs SHALL fail immediately with
guided install instructions, the Ingests screen SHALL show a setup notice,
and all non-AI features SHALL keep working.

#### Scenario: CLI missing
- **WHEN** the user clicks "Run now" with no `claude` on PATH
- **THEN** the run fails instantly with instructions for installing Claude
  Code, and search/editing/preview remain functional

#### Scenario: Process dies mid-run
- **WHEN** the CLI process exits before signalling completion
- **THEN** the run is recorded `failed` with the session's recent output
  as detail, and no output files are changed

#### Scenario: Agent waits for input
- **WHEN** a Notification hook arrives for Ken's session
- **THEN** the run shows `blocked on you` and offers Cancel

### Requirement: Runner is CI-testable without Claude
The runner SHALL accept the CLI binary path as a parameter so tests can
substitute a fake `claude` script, covering success, over-threshold hold,
failure, timeout, and blocked flows without a real Claude installation.

#### Scenario: CI run with fake CLI
- **WHEN** the test suite runs the runner against the fixture script
- **THEN** all lifecycle states are exercised and no network or real
  Claude is used

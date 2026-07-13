# sync-git

## ADDED Requirements

### Requirement: Git projects sync automatically
Ken SHALL sync a project automatically whenever its root is a git
repository with a remote and sync auto is enabled (the default): it pulls
updates (merge, never rebase, never force) when the app window gains
focus and after project activation,
throttled to at most one pull per 60 seconds, and SHALL commit and push
local changes debounced 30 seconds after watcher-detected changes. When
nothing is staged the commit is skipped; when no remote exists the push
is skipped. All git interaction SHALL shell out to the user's `git`
binary so their configuration and credentials apply unchanged.

#### Scenario: Teammate's update arrives on focus
- **WHEN** a teammate pushed a new file and the user refocuses the Ken
  window
- **THEN** the file appears in the project and the index within one pull

#### Scenario: Local edit flows out
- **WHEN** the user saves a document in a git project with a remote
- **THEN** within the debounce window Ken commits and pushes it without
  any user action

#### Scenario: Non-git project is left alone
- **WHEN** a project folder is not a git repository
- **THEN** Ken runs no git commands against it

### Requirement: Ken's transient files stay out of the repository
Ken SHALL idempotently append `.ken/.staging/` and
`.claude/settings.local.json` to the repository's `.git/info/exclude`
so its transient files never enter the user's history, without modifying
any tracked file.

#### Scenario: Excludes are added once
- **WHEN** sync activates twice on the same repository
- **THEN** `.git/info/exclude` contains each entry exactly once

### Requirement: Sync state surface
Ken SHALL emit a sync-state event (`off`, `synced`, `syncing`,
`attention`, with optional plain-language detail) and the title-bar dot
SHALL reflect it: green when synced, pulsing while syncing, danger with
an explanatory tooltip on attention. Default UI copy SHALL NOT contain
the words "git", "merge", or "rebase"; Settings MAY show the remote and
branch as secondary monospace detail.

#### Scenario: Failed push turns the dot to attention
- **WHEN** a push fails (for example, missing credentials) and the retry
  after a pull also fails
- **THEN** the title-bar dot turns to the danger state and its tooltip
  explains the problem in plain language

#### Scenario: Recovery clears attention
- **WHEN** a later pull and push complete cleanly
- **THEN** the state returns to synced

### Requirement: Sync settings
Settings SHALL show a "Sync & collaboration" card: for git projects a
`git` chip, the remote and branch, a plain-language summary ("updates
flow automatically · conflicts go to Review"), an auto on/off toggle
persisted to `project.json` as `"sync": {"auto": bool}`, and a "Sync
now" button that pulls and pushes immediately, bypassing throttle and
debounce. For non-git projects it SHALL show a `shared drive` chip and
explain that conflicting copies land in Review.

#### Scenario: Turning auto off stops syncing
- **WHEN** the user disables the sync toggle
- **THEN** no further automatic pulls or pushes run and the sync state
  becomes off, until re-enabled

#### Scenario: Sync now works while auto is on
- **WHEN** the user clicks Sync now moments after an automatic pull
- **THEN** a pull and push run immediately despite the 60s throttle

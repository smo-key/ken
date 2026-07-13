# home-digest

## ADDED Requirements

### Requirement: One-shot assistant runner
ken-core SHALL provide `assistant::oneshot(binary, project_root, prompt,
timeout, cancel)` which runs one headless Claude Code session (`claude
-p <prompt> --output-format json --permission-mode acceptEdits
--session-id <uuid>` with cwd at the project root) and returns the
`result` string parsed from the CLI's output JSON. It SHALL reuse
`runner::CancelToken`, kill the process on cancel or timeout, and report
missing-binary, failed, cancelled, and timed-out outcomes distinctly.

#### Scenario: Successful one-shot returns the result text
- **WHEN** `oneshot` runs against a CLI whose output JSON is
  `{"is_error": false, "result": "<text>"}`
- **THEN** it returns a completed outcome carrying exactly `<text>`

#### Scenario: Cancel kills the session
- **WHEN** the cancel token is cancelled while the session is running
- **THEN** the process is killed and the outcome is cancelled

#### Scenario: Timeout kills the session
- **WHEN** the session outlives the given timeout
- **THEN** the process is killed and the outcome is timed-out

#### Scenario: Missing binary fails with guidance
- **WHEN** the configured binary does not exist
- **THEN** the outcome is a failure whose message explains Claude Code
  isn't installed

### Requirement: Digest storage
The project database SHALL migrate to schema v5, adding
`digests(id INTEGER PRIMARY KEY, date TEXT NOT NULL UNIQUE, content TEXT
NOT NULL, created_at INTEGER NOT NULL)` where `date` is the user's local
`yyyy-mm-dd`. `Db` SHALL provide `upsert_digest` (replacing the row for
an existing date), `get_digest(date)`, and `latest_digest` (newest by
date). Existing v4 databases SHALL upgrade in place without data loss.

#### Scenario: One digest per day, replaceable
- **WHEN** `upsert_digest` is called twice for the same date
- **THEN** a single row exists for that date carrying the second content

#### Scenario: v4 database migrates to v5
- **WHEN** a database at schema v4 is opened
- **THEN** the digests table exists and is usable, and prior data
  (files, runs, chats, review items) is intact

### Requirement: Digest generation
ken-core SHALL gather digest inputs from the last 24 hours — files
changed (by indexed mtime), ingest runs finished
(`runs_finished_since`), and open stored review item titles — and
compose a prompt asking for one warm, concrete paragraph (~120 words
max, plain language, **bold** allowed) summarizing what changed and
what's waiting, followed by a line `SOURCES: path1, path2, ...` naming
up to 5 project-relative paths. It SHALL parse the model's result into
`{body, sources}`, tolerating a missing SOURCES line (whole text becomes
the body, sources empty).

#### Scenario: Prompt carries the day's activity
- **WHEN** the last 24h contain changed files, finished runs, and open
  review items
- **THEN** the composed prompt names those paths, run summaries, and
  item titles and states the paragraph + SOURCES-line contract

#### Scenario: Result parses into body and sources
- **WHEN** a result contains a paragraph followed by
  `SOURCES: a.md, notes/b.md`
- **THEN** parsing yields that paragraph as body and `["a.md",
  "notes/b.md"]` as sources

#### Scenario: Missing SOURCES line is tolerated
- **WHEN** a result contains only prose with no SOURCES line
- **THEN** parsing yields the full text as body and no sources

### Requirement: Digest scheduling
The app SHALL check on project activate and on every window focus:
if today (local) already has a digest, do nothing; otherwise, when local
time is at or past 07:00, Claude Code is installed, and no generation is
in flight, generate the digest in a background thread (3-minute
timeout), store it, and emit a `digest-updated` event. When the last 24
hours contain no changed files, no finished runs, and nothing waiting,
the app SHALL store the quiet fallback "A quiet day — nothing new since
yesterday." without calling Claude. A `refresh_digest` command SHALL
force-regenerate today's digest on demand.

#### Scenario: First focus of the morning writes the digest
- **WHEN** the window gains focus after 07:00 local and today has no
  digest and Claude is installed
- **THEN** a digest is generated and stored for today and
  `digest-updated` fires with the row

#### Scenario: Already digested today
- **WHEN** focus fires and today's digest already exists
- **THEN** no generation starts

#### Scenario: Quiet day skips the AI call
- **WHEN** generation is due but the last 24h show no changes, runs, or
  waiting items
- **THEN** the quiet fallback is stored as today's digest without
  invoking Claude

#### Scenario: Force refresh
- **WHEN** the user invokes `refresh_digest`
- **THEN** today's digest is regenerated and replaces the stored row

### Requirement: Home digest card
Home SHALL show a Today's-digest card: a TODAY'S DIGEST overline, the
generated-at time, and a `share` link that copies the digest as markdown
and shows a transient "Copied" confirmation. The body SHALL render the
digest paragraph (markdown, bold allowed) and mono source chips that
open the file in Files when clicked. States: no digest yet with Claude
installed → "Ken writes you a morning digest — it'll appear here." plus
a "Write it now" button; Claude missing → one honest line saying the
digest needs Claude Code; generation in flight → a subtle pulsing
placeholder line.

#### Scenario: Digest renders with sources
- **WHEN** today's digest exists with body and sources
- **THEN** the card shows the generated-at time, the rendered paragraph,
  and one mono chip per source that opens it in Files

#### Scenario: Share copies markdown
- **WHEN** the user clicks share
- **THEN** the clipboard holds the digest as markdown and "Copied" shows
  briefly

#### Scenario: No digest yet, Claude present
- **WHEN** no digest exists for today and Claude Code is installed
- **THEN** the card invites — "Ken writes you a morning digest" — with a
  "Write it now" button that triggers generation

#### Scenario: Claude missing
- **WHEN** Claude Code is not installed
- **THEN** the card states plainly that the daily digest needs Claude
  Code, with no fake content and no error

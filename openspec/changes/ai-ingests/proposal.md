# Proposal: ai-ingests

## Why

The walking skeleton indexes and searches raw files, but Ken's core promise —
AI-maintained structured documents that stay fresh as knowledge changes — is
still missing. This change (2 of 10 in the build order) adds ingests: recipes
that turn raw project files into living structured documents (a people
directory, a decisions log, a gold-standard requirements doc), refreshed
through the user's locally installed Claude Code CLI and governed by review
rules so the AI never silently overwrites human work.

## What Changes

- Ingest recipes: markdown files in `.ken/ingests/` with YAML frontmatter
  (`name`, `description`, `sources`, `output` file-or-folder, `mode:
  single | collection`, `refresh: on-change | manual`, optional `rules`
  overrides) and a plain-language instruction body. Diffable, shareable,
  editable by hand or by AI.
- Ingests screen per the prototype: ingest list with status dots + detail
  view as cards (Sources folder chips, Instruction, Output, Rules, Recent
  runs), plus a form-based create/edit flow so non-technical users never
  see YAML.
- Template library: bundled recipes (People, Requirements gold-standard,
  Decision log, Glossary, Meeting notes digest, FAQ, Risks); "Use template"
  copies one into `.ken/ingests/`.
- Runner driving the local `claude` CLI with the user's existing auth:
  hidden-PTY interactive session by default (completion detected via a
  Stop hook POSTing to Ken's localhost listener), with a per-project
  `ingestRunner: headless` setting that uses `claude -p` instead. Missing
  `claude` binary → guided setup message; AI features degrade, nothing
  else breaks.
- Refresh engine: composes incremental prompts (recipe instruction +
  files changed since the last successful run + current outputs as
  canonical), runs the agent against a staging copy of outputs, then
  applies review rules — human edits win; a refresh changing more than the
  threshold (default 20%) is held as *pending approval* instead of
  written; unchanged-source ingests get a stale check timestamp.
- Run log per ingest: recorded runs with status (fresh / running /
  blocked on you / pending approval / failed) and plain-language summaries;
  a held run can be approved (applies staged output) or discarded from the
  ingest detail view.
- Watcher integration: `refresh: on-change` ingests queue automatically
  (debounced) when files under their sources change; ingest-written
  outputs do not re-trigger the ingest that produced them.
- Testability: the runner is exercised in CI against a fake `claude`
  script (PTY spawn, prompt delivery, hook callback, output staging) — no
  real Claude needed.

## Capabilities

### New Capabilities
- `ingest-recipes`: the recipe format, parsing/validation, form-based
  editing, and the template library.
- `ingest-runner`: driving the Claude Code CLI (hidden-PTY and headless),
  session lifecycle, completion/failure detection, missing-CLI handling.
- `ingest-refresh`: the refresh engine — incremental prompt composition,
  staging, review rules (human-edits-win, approval threshold, stale
  check), run log, approval/discard of held runs, watcher-triggered
  refresh.

### Modified Capabilities

_None — the walking-skeleton specs are unchanged; ingest outputs are
ordinary project files that the existing scan/index/watch pipeline already
handles._

## Impact

- `ken-core`: new modules — recipe parsing (serde_yaml), runner
  (portable-pty, localhost hook listener), refresh engine (staging, diff
  ratio, run log tables in the existing per-project DB).
- `src-tauri`: new commands (list/create/update/delete ingests, run now,
  approve/discard run, runner setting) + events (run state changes).
- Frontend: Ingests screen (list, detail cards, form editor, template
  gallery), Home "waiting on you" wiring for pending approvals.
- New Rust deps: `serde_yaml` (or `serde_yml`), `portable-pty`, `tiny_http`
  (or axum-free minimal listener), `similar` (diff ratio).
- DB schema v2: `ingest_runs` table (+ migration from v1).
- External dependency at runtime only: the user's `claude` CLI.

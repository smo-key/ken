# Tasks: ai-ingests

## 1. Foundations

- [x] 1.1 Add deps (serde_yaml, portable-pty, tiny_http, similar); DB schema v2 migration adding `ingest_runs`; migration test v1→v2
- [x] 1.2 Recipe model in ken-core: parse/serialize `.ken/ingests/*.md` (frontmatter + body, unknown-field preservation), list/load/save/delete, validation with plain-language errors; unit tests incl. malformed recipes
- [x] 1.3 Rules resolution (recipe > project defaults > built-ins) with tests

## 2. Runner (ken-core)

- [x] 2.1 Localhost hook listener (tiny_http, port 0): parse hook POSTs, route by session_id over channels; unit test with raw HTTP
- [x] 2.2 `.claude/settings.local.json` merge: install/update Ken's Stop+Notification hooks (replace only entries containing /ken-hook, preserve everything else); tests incl. pre-existing user settings
- [x] 2.3 Hidden-TUI runner: portable-pty spawn with --session-id/--permission-mode/prompt arg, ring-buffer drain, Stop-hook completion, /exit shutdown, Notification→blocked, timeout→failed; CLI discovery (`which claude`) with guided-missing error
- [x] 2.4 Headless runner: `claude -p --output-format json`, exit/is_error handling; shared RunOutcome type
- [x] 2.5 Fake-claude fixture script + runner lifecycle tests: success, failure exit, timeout, blocked, headless variant

## 3. Refresh engine (ken-core)

- [x] 3.1 Prompt composer: instruction + rules text + changed-files-since-last-success (mtime vs last run; full corpus on first run) + current outputs + staging contract (single/collection, _removed.txt); snapshot tests
- [x] 3.2 Staging apply: change-ratio via `similar`, threshold decision, fs apply (renames + _removed deletions), first-run auto-apply, mid-run-human-edit demotion to pending; tests for each rule
- [x] 3.3 Run log CRUD on ingest_runs + statuses; approve/discard operations with staging cleanup; tests
- [x] 3.4 Refresh queue: on-change trigger from scan stats (source-path intersection, 30s debounce, collapse repeats, no self-retrigger from own outputs), one-at-a-time execution; tests with fake claude

## 4. Tauri commands + events

- [ ] 4.1 Commands: list_ingests, get_ingest (recipe+runs), save_ingest (form data→recipe file), delete_ingest, run_ingest (now/full), cancel_run, approve_run, discard_run, set_ingest_runner_mode, claude_doctor (binary presence/version)
- [ ] 4.2 Events: ingest-run-changed (status transitions) wired from queue; frontend store consumes into reactive run state

## 5. Frontend

- [ ] 5.1 Ingests store + api wrappers with types
- [ ] 5.2 Ingests screen: list pane with status dots/captions per prototype; detail cards (Sources chips, Instruction inline-edit, Output, Rules inherited/overridden, Recent runs with Approve/Discard/Cancel/error detail); Run now button
- [ ] 5.3 New/edit ingest form (name, instruction, source folders, output picker + single/collection, refresh toggle) writing through save_ingest
- [ ] 5.4 Template gallery (7 bundled templates as ?raw imports) with use-template flow
- [ ] 5.5 Home wiring: pending-approval + blocked runs as waiting-on-you cards; Claude-missing setup banner on Ingests
- [ ] 5.6 Settings: ingestRunner mode toggle + claude doctor status line

## 6. Verification

- [ ] 6.1 cargo + vitest green; fake-claude end-to-end: recipe → run → staging → threshold hold → approve → output file updated and indexed
- [ ] 6.2 Live run-through with real `claude` on the fixture project (People template): create from template, run, inspect output, verify incremental second run; fix gaps
- [ ] 6.3 Update README status; commit

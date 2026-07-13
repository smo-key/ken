# Design: ai-ingests

## Context

Walking skeleton is live: ken-core scans/indexes/watches, the app shell and
Files/search work. This change adds the AI layer on top: recipes, a runner
for the local Claude Code CLI, and a refresh engine with review rules. The
chat drawer (change 3) and Review inbox (change 4) don't exist yet, so this
change must surface run states on the Ingests screen alone without painting
itself into a corner.

## Goals / Non-Goals

**Goals:**
- Recipes as shareable text; forms over YAML for non-technical users.
- A runner that works with the user's existing Claude auth, is observable
  (real session ids, resumable later by the chat drawer), and is fully
  testable in CI with a fake `claude`.
- Refresh that never destroys human work: outputs-as-canonical prompting +
  staging + threshold approval.

**Non-Goals:**
- Rendering the agent's session (change 3 — chat drawer).
- The unified Review inbox (change 4) — pending approvals live on the
  Ingests screen for now; the run/approval data model is designed so
  change 4 only adds UI.
- Answering agent questions mid-run (needs the drawer). Mitigated by
  prompting: "make reasonable assumptions and record them in the output
  under 'Open questions' instead of asking."
- Scheduled/auto stale re-runs (stale is a flag + manual Run now in v1).

## Decisions

1. **Recipe format** — `.ken/ingests/<slug>.md`: YAML frontmatter between
   `---` fences (`name`, `description`, `sources: [notes/, specs/]`
   (empty = all included folders), `output: knowledge/People.md` or
   `people/`, `mode: single|collection`, `refresh: on-change|manual`,
   `rules: {reviewThresholdPct, staleDays}` optional) + markdown body =
   instruction. Parse with `serde_yaml`; unknown fields preserved on
   rewrite (same forward-compat rule as project.json). Slug = filename;
   renames are file renames.
2. **Hook plumbing** — Ken runs a localhost listener (`tiny_http`, port 0 →
   OS-assigned) started with the app. Before a run, Ken merges into the
   project's `.claude/settings.local.json` (auto-gitignored by Claude
   Code) Stop + Notification hooks whose command is
   `curl -s -X POST http://127.0.0.1:<port>/ken-hook -d @-`. Any existing
   hook entry containing `/ken-hook` is replaced (ours); all other user
   settings are preserved. Hook payloads carry `session_id`; the listener
   routes them over a channel to the run owning that id. Foreign session
   ids are ignored, so user-run claude sessions in the same folder are
   unaffected.
3. **Runner, hidden-TUI mode (default)** — spawn via `portable-pty`:
   `claude --session-id <uuid> --permission-mode acceptEdits "<prompt>"`
   with cwd = project root, 200×50 PTY, output drained to a ring buffer
   (later shown by the chat drawer; today used in error messages).
   Completion = Stop hook for our session id; then write `/exit\r`, wait
   5s, kill if needed. `Notification` hook (agent waiting on
   input/permission) → status `blocked`; v1 offers Cancel. Failure =
   process exit before Stop, or timeout (default 15 min) → status
   `failed` with the tail of the ring buffer as detail.
4. **Runner, headless mode** — per-project setting
   `ingestRunner: "hidden-tui" (default) | "headless"` in project.json
   `extra`. Headless = `claude -p "<prompt>" --output-format json
   --permission-mode acceptEdits --session-id <uuid>`; completion =
   process exit; `is_error` in the result JSON → failed. Same staging
   contract, no hooks needed.
5. **CLI discovery** — `which claude` at app start + before each run;
   missing → runs fail fast with a guided message ("Install Claude Code:
   npm i -g @anthropic-ai/claude-code, then log in"), Ingests screen shows
   a setup banner, everything else keeps working.
6. **Staging contract** — the prompt instructs the agent to write the full
   updated output files under `.ken/.staging/<slug>/<output-rel-path>`
   (staging is under `.ken/` → invisible to scan/watch by the existing
   hidden-folder rule). Ken never lets the agent write outputs in place;
   rules are enforced by Ken, not by trust. After completion Ken computes
   a change ratio (`similar` crate, changed lines / max total lines,
   aggregated over files; deletions of whole files count as fully
   changed). First run into an empty output auto-applies regardless.
7. **Review rules** — resolved as recipe.rules over project defaults over
   built-ins (20% / 30 days). ratio ≤ threshold → apply: move staged files
   over outputs (fs::rename per file), record run `fresh`. ratio >
   threshold → keep staging, record `pending_approval`; Approve = apply +
   mark `fresh`; Discard = delete staging, mark `discarded`. Human edits
   win by construction: refresh reads current outputs (which contain human
   edits) as canonical input, and staged output replaces a file only at
   apply time — if the file changed on disk between run start and apply
   (mtime check), the apply is demoted to `pending_approval` to avoid
   clobbering a mid-run human edit.
8. **Prompt composition** — sections: the recipe instruction; the resolved
   rules ("existing documents are canonical; update only what new data
   implies; preserve human edits; record assumptions in an 'Open
   questions' section rather than asking"); source scope (folder list);
   changed files since last successful run (paths — agent reads files
   itself with its own tools; first run = all indexed source files);
   current output paths; the staging directory to write into; output
   format contract (`mode: single` = exactly one file; `collection` = one
   file per entity, kebab-case names, deletions listed in a
   `_removed.txt`, which Ken applies as deletes on approval/apply).
9. **Change detection** — per-ingest `last_success_at` (epoch, from the
   run row); changed = indexed files under `sources` with mtime >
   last_success_at. Watcher integration: on `index-updated`, ingests with
   `refresh: on-change` whose sources intersect the changed paths are
   queued with a 30s debounce; a global queue runs one ingest at a time
   (per-project) to avoid PTY storms; re-triggers while queued collapse.
   Ingest outputs are excluded from that ingest's *trigger* set (no
   self-retrigger loops), but remain ordinary indexed files.
10. **DB schema v2** — new table `ingest_runs(id INTEGER PK, slug TEXT,
    session_id TEXT, started_at INT, finished_at INT, status TEXT,
    summary TEXT, error TEXT, change_ratio REAL)`; `meta.schema_version`
    1→2 migration (CREATE TABLE IF NOT EXISTS — v1 DBs upgrade in place,
    Reindex unaffected since runs are not derived data… but they ARE
    app-data-local: acceptable loss on DB reset; recipes/outputs — the
    shared truth — live in the project).
11. **Templates** — recipe files embedded in the frontend
    (`src/lib/templates/*.md?raw`); "Use template" round-trips through the
    normal create command. No linkage after copy.
12. **Ingests screen** — list pane (status dot per last run) + detail
    cards exactly per prototype: Sources (chips from recipe + "excluded"
    dashed chip), Instruction (inline edit), Output, Rules (inherited vs
    overridden), Recent runs (status, when, summary, Approve/Discard for
    held runs, error detail for failed). Create/edit = a single form
    (name, plain-language instruction textarea, source folders
    multi-select, output path + single/collection toggle, refresh toggle);
    the form writes the recipe file via Tauri command.
13. **Fake claude for CI** — runner takes the binary path as a parameter
    (prod: discovered `claude`). Tests point it at a fixture shell script
    that: parses `--session-id`/`-p`, writes deterministic files into the
    staging dir named in the prompt, POSTs a Stop payload to the hook URL
    it reads from `.claude/settings.local.json` (TUI mode) or just exits
    (headless mode). Covers: success/apply, over-threshold hold, failure
    exit, timeout, blocked (Notification payload).

## Risks / Trade-offs

- [Claude CLI flag drift (`--session-id`, `--permission-mode`, hook JSON
  shape)] → all CLI interaction isolated in one `runner.rs` module; fake-
  claude tests pin our side of the contract; a doctor check ("claude
  --version") logs the version for bug reports.
- [Agent writes outside staging] → prompt forbids it; Ken applies only
  from staging, so stray writes surface as ordinary watcher changes,
  never as silent output overwrites. Worst case is user-visible noise,
  not data loss.
- [PTY apps buffer/paint differently across versions] → we never parse
  TUI output for control flow (hooks only); ring buffer is diagnostic.
- [mtime-based change detection misses same-second edits] → acceptable;
  the next change to any source re-includes the file, and Run now always
  offers a full pass ("run with all sources" when nothing changed).
- [One-at-a-time run queue may lag behind bursts] → debounce collapses
  triggers; run log makes the queue visible; concurrency can lift later
  without schema changes.

## Migration Plan

DB v1→v2 is additive (one CREATE TABLE). Recipes/templates are new files;
no existing data changes. Rollback = git revert; v2 DBs open fine under
v1 readers (extra table ignored) but the app ships forward-only.

## Open Questions

None blocking. Exact wording of the built-in template instructions can be
tuned during implementation; the seven templates and their outputs are
fixed by the spec.

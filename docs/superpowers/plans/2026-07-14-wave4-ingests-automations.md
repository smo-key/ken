# Wave 4 — Ingests & Automations Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver spec §4 (Ingests visibility & pacing) and §7 (Automations). Make ingest runs *legible* — a live activity line, an elapsed timer, queued/waiting countdowns, and a recorded run even when nothing changed — and add generic trigger→agent **Automations** that reach external services through the user's own MCP config, gated by a two-phase Review approval.

**Architecture:** All heavy logic lives in `crates/ken-core` and is TDD'd against the existing fake-Claude test harness (`runner::test_support::write_fake_claude`). The `IngestEngine` (one project, one worker thread, concurrency 1) grows a unified job queue that carries both **ingest** recipes and **automation** rules; both stream their Claude session output as `stream-json` so a live `activity` line can be parsed with the *same* `chat::parse_event` the chat drawer uses. Automations persist under `.ken/automations/` exactly as recipes persist under `.ken/ingests/`. The two-phase gate (propose → approve → apply) reuses the existing `review_items` table and Review screen, with the phase-1 proposal staged as a `review_items` row and approval queuing a phase-2 "execute exactly these approved actions" session. Run history for both kinds lives in `ingest_runs`, now discriminated by a `kind` column. The frontend `IngestsScreen` becomes two tabs — **Knowledge docs** (today's recipes) and **Automations** — sharing one live-activity rendering.

**Tech Stack:** Rust (ken-core, rusqlite, serde, serde_yaml), Tauri 2 (`src-tauri/src/lib.rs`), Svelte 5 runes + TypeScript (`src/`), Vitest for pure frontend logic, `cargo test` for Rust. No new crates — glob matching is a small tested pure function; MCP reach is free via the user's `claude` config (no per-service code).

---

## Global Constraints (exact, non-negotiable)

- **Debounce default:** `EngineConfig::debounce = Duration::from_secs(10)` (was 30s). Concurrency stays **1**.
- **Timeout:** unchanged at `Duration::from_secs(15 * 60)`.
- **Poll interval:** unchanged at 200ms (`rx.recv_timeout`).
- **Event name:** unchanged — `"ingest-run-changed"` carries `IngestEvent` for BOTH kinds; the frontend routes by `ev.kind`.
- **`IngestEvent` fields (final):** `kind: String` (`"ingest"` | `"automation"`), `slug`, `run_id`, `session_id: Option<String>`, `status`, `detail: Option<String>`, `activity: Option<String>`, `elapsed_secs: Option<u64>`, `eta_secs: Option<u64>`.
- **Live/transient statuses (never persisted, not in `RunStatus` DB set):** `"queued"` (carries `eta_secs`), `"waiting"` (carries `detail = "waiting for <name>"`). Persisted run statuses are unchanged: `running | fresh | blocked | pending_approval | failed | discarded | cancelled`.
- **No-op run summary:** exactly `"Checked — nothing to update."` recorded as a `fresh` run.
- **Automation struct fields:** `{ slug, name, globs: Vec<String>, prompt: String, auto_apply: bool (default false), enabled: bool (default true) }`. Persisted at `.ken/automations/<slug>.md` (YAML frontmatter + prompt body).
- **Run-kind values (`ingest_runs.kind`):** `"ingest"` (default) | `"automation"`. Column added by migration to schema version **9**. **Ordering dependency:** the map-incremental plan's v8 migration (which creates `extractions(rel_path PRIMARY KEY, content_hash, extracted_at, status, error)`) must land before this plan's v9; if implementing while v8 is not yet present, still take v9 and coordinate — do not renumber.
- **Review-item kind for proposals:** `"automation-proposal"`. Payload JSON: `{ "automationSlug": "<slug>", "matched": ["<rel>", …] }`.
- **Automation session mode:** always **Headless streaming** (`--output-format stream-json --verbose`). The hidden-TUI opt-in (`ingestRunner = "hidden-tui"`) keeps today's behavior for ingests and is not offered for automations.
- **Two-phase gate wording (UI, verbatim intent):** "Phase 1 is *asked* not to act outside your files — but that restraint is only a request. The real safety is this step: nothing happens outside your project until you approve."

---

## File Structure

**New files**
- `crates/ken-core/src/automation.rs` — automation model, glob matching, `.ken/automations/` persistence.
- `src/lib/automations.svelte.ts` — frontend automations store (mirrors `ingests.svelte.ts`).
- `src/ingests/AutomationForm.svelte` — create/edit form (name, globs, prompt, auto-apply, enabled).
- `src/ingests/AutomationsPane.svelte` — list + detail pane for the Automations tab.
- `src/lib/liveRun.ts` — pure helpers: live-status captions, countdown derivation (Vitest target).
- `src/lib/liveRun.test.ts` — Vitest for the above.
- `src/lib/ingestTabs.test.ts` — Vitest for tab state + event routing.

**Modified files**
- `crates/ken-core/src/db.rs` — schema v9 (`ingest_runs.kind`; lands after map-incremental's v8 `extractions` migration); `insert_run_kind`; `RunRow.kind`; `RUN_COLS`/`map_run`; `list_runs_of_kind`.
- `crates/ken-core/src/engine.rs` — debounce 10s; `IngestEvent` new fields + constructor; unified `QueueKey`/`PendingJob` queue; no-op recording; queued/waiting emission; automation dispatch + two-phase; `approve_automation_proposal`/`discard_automation_proposal`.
- `crates/ken-core/src/runner.rs` — `run_ingest_session` + `run_headless_streaming` (activity callback); fake-Claude stream-json output.
- `crates/ken-core/src/lib.rs` — `pub mod automation;`.
- `src-tauri/src/lib.rs` — engine `on_event` carries new fields + kind; automation commands; proposal approve/discard commands; register all in `invoke_handler`.
- `src/lib/api.ts` — `IngestEvent`/`RunRow`/`InboxKind` updates; `Automation*` types; new invoke wrappers.
- `src/lib/ingests.svelte.ts` — route only `kind==="ingest"` events; expose live activity/elapsed.
- `src/lib/review.svelte.ts` — `automation-proposal` action routing.
- `src/screens/IngestsScreen.svelte` — two-tab shell.
- `src/screens/ReviewScreen.svelte` — `automation-proposal` labels/actions.

---

## Conventions for every task

Each task is: **write the failing test → run it → SEE it fail → minimal implementation → run it → SEE it pass → commit.** Never write implementation before its test exists and fails. Commit messages end with the co-author trailer already in the repo's git config.

Rust: `cargo test -p ken-core <name> -- --nocapture`. Frontend: `pnpm vitest run <file>`. Type check the whole frontend once per frontend-touching part: `pnpm check` (svelte-check) and `pnpm exec tsc --noEmit` if configured.

---

# PART A — `ingest_runs.kind` discriminator (DB)

### Task A1 — Migration to schema v9 adding `kind`, plus `RunRow.kind`

> **Ordering:** this migration is **v9** because the map-incremental plan (executed first) takes **v8** for its `extractions` table. Expect `SCHEMA_VERSION` to already be 8 when you start; if it is still 7 (map plan not yet merged), still take v9 and coordinate — do not renumber.

- [ ] **Test first.** Add to `crates/ken-core/src/db.rs` `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn runs_carry_kind_and_default_to_ingest() {
        let mut db = Db::open_in_memory().unwrap();
        // Legacy insert path defaults to "ingest".
        let a = db.insert_run("people", Some("s-a"), 100).unwrap();
        assert_eq!(db.get_run(a).unwrap().unwrap().kind, "ingest");
        // Explicit-kind insert records an automation run.
        let b = db.insert_run_kind("weekly-jira", Some("s-b"), 200, "automation").unwrap();
        assert_eq!(db.get_run(b).unwrap().unwrap().kind, "automation");
        // list_runs_of_kind filters by kind even when slugs would otherwise mix.
        let auto = db.list_runs_of_kind("weekly-jira", "automation", 10).unwrap();
        assert_eq!(auto.len(), 1);
        assert!(db.list_runs_of_kind("weekly-jira", "ingest", 10).unwrap().is_empty());
    }
```

- [ ] Run `cargo test -p ken-core runs_carry_kind_and_default_to_ingest` → **expect compile error** (`insert_run_kind`, `list_runs_of_kind`, `RunRow.kind` do not exist).

- [ ] **Implement.** In `db.rs`:
  1. `pub const SCHEMA_VERSION: i64 = 9;` (was 8 after the map-incremental plan's `extractions` migration landed).
  2. Add a `kind` field to `RunRow` (after `slug`), documented:
  ```rust
      pub slug: String,
      /// `ingest` | `automation` — which subsystem produced this run.
      pub kind: String,
      pub session_id: Option<String>,
  ```
  3. Add the migration block after the `version < 8` block (map-incremental's `extractions` table), before the final `INSERT OR REPLACE INTO meta`:
  ```rust
          if version < 9 {
              // Run-kind discriminator: automation runs share ingest_runs with
              // recipe runs. Guarded ADD COLUMN (no IF NOT EXISTS; a fresh DB
              // already has the column, and tests rewind schema_version).
              let has_kind: bool = self
                  .conn
                  .prepare("SELECT 1 FROM pragma_table_info('ingest_runs') WHERE name = 'kind'")?
                  .exists([])?;
              if !has_kind {
                  self.conn.execute_batch(
                      "ALTER TABLE ingest_runs ADD COLUMN kind TEXT NOT NULL DEFAULT 'ingest';",
                  )?;
              }
          }
  ```
  4. `RUN_COLS` gains `kind` in the same position as the struct:
  ```rust
      const RUN_COLS: &'static str =
          "id, slug, kind, session_id, started_at, finished_at, status, summary, error, change_ratio";
  ```
  5. `map_run` reads the shifted columns:
  ```rust
      fn map_run(r: &rusqlite::Row) -> rusqlite::Result<RunRow> {
          Ok(RunRow {
              id: r.get(0)?,
              slug: r.get(1)?,
              kind: r.get(2)?,
              session_id: r.get(3)?,
              started_at: r.get(4)?,
              finished_at: r.get(5)?,
              status: r.get(6)?,
              summary: r.get(7)?,
              error: r.get(8)?,
              change_ratio: r.get(9)?,
          })
      }
  ```
  6. Keep `insert_run` as a thin delegate (zero churn to its 12 callers) and add the kind-aware insert + a kind-filtered list:
  ```rust
      pub fn insert_run(&mut self, slug: &str, session_id: Option<&str>, started_at: i64) -> Result<i64> {
          self.insert_run_kind(slug, session_id, started_at, "ingest")
      }

      pub fn insert_run_kind(
          &mut self,
          slug: &str,
          session_id: Option<&str>,
          started_at: i64,
          kind: &str,
      ) -> Result<i64> {
          self.conn.execute(
              "INSERT INTO ingest_runs (slug, kind, session_id, started_at, status)
               VALUES (?1, ?2, ?3, ?4, 'running')",
              params![slug, kind, session_id, started_at],
          )?;
          Ok(self.conn.last_insert_rowid())
      }

      pub fn list_runs_of_kind(&self, slug: &str, kind: &str, limit: usize) -> Result<Vec<RunRow>> {
          let sql = format!(
              "SELECT {} FROM ingest_runs WHERE slug = ?1 AND kind = ?2
               ORDER BY started_at DESC, id DESC LIMIT ?3",
              Self::RUN_COLS
          );
          let mut stmt = self.conn.prepare(&sql)?;
          let rows = stmt
              .query_map(params![slug, kind, limit as i64], Self::map_run)?
              .collect::<std::result::Result<_, _>>()?;
          Ok(rows)
      }
  ```

- [ ] Run `cargo test -p ken-core db::` → all `db` tests pass (existing ones already construct `RunRow` via `map_run`, so no literal churn; grep confirms no external `RunRow { … }` literal exists outside `map_run`).
- [ ] **Commit:** `feat(db): add kind discriminator to ingest_runs (schema v9)`.

---

# PART B — Ingest pacing & no-op recording (engine)

### Task B1 — Debounce default → 10s

- [ ] **Test first.** In `engine.rs` tests:

```rust
    #[test]
    fn default_debounce_is_ten_seconds() {
        assert_eq!(EngineConfig::default().debounce, Duration::from_secs(10));
    }
```

- [ ] Run → **fails** (currently 30s).
- [ ] **Implement.** `engine.rs` `impl Default for EngineConfig`: change `debounce: Duration::from_secs(30)` → `Duration::from_secs(10)`. Update the doc note on the field/spec §4 mention.
- [ ] Run → passes. **Commit:** `feat(engine): debounce default 30s → 10s`.

### Task B2 — `IngestEvent` grows kind + live fields, with a constructor

This is a pure refactor that unblocks B3/C/D. No behavior change yet.

- [ ] **Test first.** In `engine.rs` tests:

```rust
    #[test]
    fn ingest_event_constructor_defaults_are_none() {
        let ev = IngestEvent::at("ingest", "people", 5, Some("s".into()), "running", None);
        assert_eq!(ev.kind, "ingest");
        assert!(ev.activity.is_none() && ev.elapsed_secs.is_none() && ev.eta_secs.is_none());
    }
```

- [ ] Run → **fails** (`IngestEvent::at` and fields absent).
- [ ] **Implement.** Replace the `IngestEvent` struct + add constructor:

```rust
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestEvent {
    /// `ingest` | `automation` — routes the event to the right UI surface.
    pub kind: String,
    pub slug: String,
    pub run_id: i64,
    pub session_id: Option<String>,
    /// Persisted: `running` | `blocked` | `fresh` | `pending_approval` |
    /// `failed` | `cancelled`. Transient (never stored): `queued` | `waiting`.
    pub status: String,
    pub detail: Option<String>,
    /// Latest human-readable activity line for a running run (transient).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity: Option<String>,
    /// Seconds the running run has been going (server snapshot; UI also ticks).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_secs: Option<u64>,
    /// For `queued`: whole seconds until the debounce deadline.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eta_secs: Option<u64>,
}

impl IngestEvent {
    /// The common case: a status transition with no live-activity payload.
    pub fn at(
        kind: &str,
        slug: &str,
        run_id: i64,
        session_id: Option<String>,
        status: &str,
        detail: Option<String>,
    ) -> IngestEvent {
        IngestEvent {
            kind: kind.to_string(),
            slug: slug.to_string(),
            run_id,
            session_id,
            status: status.to_string(),
            detail,
            activity: None,
            elapsed_secs: None,
            eta_secs: None,
        }
    }
}
```

- [ ] Update every `IngestEvent { … }` literal to the constructor OR add the three new fields. Sites: `engine.rs` `execute()` (emit_fail closure, the `running` emit, `blocked_event`, `finish` closure). `src-tauri/src/lib.rs` `emit_run_changed` — set `kind: run.kind` from the fetched `RunRow`. Example rewrite of `emit_fail` inside `execute`:

```rust
    let emit_fail = |run_id: i64, detail: String| {
        on_event(IngestEvent::at("ingest", slug, run_id, None, "failed", Some(detail)));
    };
```

(The `blocked_event`, `running`, and `finish` emits are rewritten wholesale in Part C when `execute` is split; for this task, mechanically convert them to `IngestEvent::at("ingest", …)`.)

- [ ] `emit_run_changed` in `src-tauri/src/lib.rs`:
```rust
fn emit_run_changed(app: &AppHandle, db: &Db, run_id: i64) {
    if let Ok(Some(run)) = db.get_run(run_id) {
        let _ = app.emit(
            "ingest-run-changed",
            IngestEvent::at(&run.kind, &run.slug, run_id, run.session_id, &run.status, run.summary),
        );
    }
}
```

- [ ] Run `cargo test -p ken-core` and `cargo check -p app` (or the workspace) → compiles, all tests pass.
- [ ] **Commit:** `refactor(engine): IngestEvent gains kind + live-activity fields`.

### Task B3 — No-op runs are recorded and emitted

- [ ] **Test first.** In `engine.rs` tests (uses the existing `rig`/`wait_status` helpers). Seed a prior success so the run is incremental, then trigger with nothing changed:

```rust
    #[test]
    fn noop_run_is_recorded_not_silent() {
        let r = rig("complete");
        {
            // A prior success in the far future so plan() finds nothing changed.
            let mut db = Db::open_at(&r.db_path).unwrap();
            let id = db.insert_run("people", None, now_epoch() + 10_000).unwrap();
            db.update_run(id, "fresh", Some(now_epoch()), None, None, None).unwrap();
        }
        // force_full = false: incremental, nothing newer than last success.
        r.engine.trigger("people", false);
        let done = wait_status(&r.events, "fresh", 20);
        assert_eq!(done.detail.as_deref(), Some("Checked — nothing to update."));
        let db = Db::open_at(&r.db_path).unwrap();
        // Two runs now exist: the seeded success and the recorded no-op.
        assert!(db.list_runs("people", 5).unwrap().len() >= 2);
    }
```

- [ ] Run → **fails** (today `Ok(None)` returns silently, no event).
- [ ] **Implement.** In `execute()` replace the `Ok(None) => return` arm. Record a completed run and emit a terminal `fresh` event:

```rust
    let plan = match refresh::plan(&project, db, &recipe, &rules, force_full) {
        Ok(Some(p)) => p,
        Ok(None) => {
            // Not silent anymore: record a "checked, nothing to do" run so the
            // user sees the engine looked. Marking it `fresh` advances the
            // last-success watermark harmlessly — nothing changed, so no source
            // file is skipped by the next incremental plan.
            let run_id = match db.insert_run(slug, None, now_epoch()) {
                Ok(id) => id,
                Err(e) => { emit_fail(0, e.to_string()); return; }
            };
            let summary = "Checked — nothing to update.";
            let _ = db.update_run(run_id, "fresh", Some(now_epoch()), Some(summary), None, None);
            on_event(IngestEvent::at("ingest", slug, run_id, None, "fresh", Some(summary.into())));
            return;
        }
        Err(e) => { emit_fail(0, e.to_string()); return; }
    };
```

- [ ] Run → passes. Run the full `engine::` suite to confirm `sources_changed_triggers_but_own_output_does_not` still holds (it asserts no event for an own-output change — that path never reaches `execute`, so it's unaffected).
- [ ] **Commit:** `feat(engine): record + emit a no-op "checked, nothing to update" run`.

---

# PART C — Streaming headless runner + live activity

### Task C1 — Fake Claude emits stream-json for headless ingest runs

The fake currently emits a single `{"is_error":…,"result":…}` object for `-p … --output-format json`. Ingest/automation runs will pass `--output-format stream-json --verbose`. Teach the fake to stream events in that case while leaving the `--output-format json` object shape (used by `oneshot`/digest/quick_answer/research) untouched.

- [ ] **Implement (test harness).** In `runner.rs` `test_support::write_fake_claude` script:
  1. In the arg loop, detect the output format. Change the `--output-format` handling to capture the value:
  ```bash
      --output-format) OUTFMT="${args[$((i+1))]}"; i=$((i+1));;
  ```
  and initialize `OUTFMT=""` near the other vars.
  2. Add a streamed-headless branch. Immediately after the existing `STREAM_INPUT` conversation block (before the "Staging dir is announced" comment), add:
  ```bash
  # Headless streamed output: `-p <prompt> --output-format stream-json --verbose`.
  # Emits the same event shapes chat parses, writes staged outputs, then a
  # terminal result. Behaviours reuse the BEHAVIOR file.
  if [ "$HEADLESS" = "1" ] && [ "$OUTFMT" = "stream-json" ]; then
    STAGING=$(echo "$PROMPT" | grep -o 'STAGING_DIR=[^ ]*' | head -1 | cut -d= -f2)
    echo '{"type":"system","subtype":"init","session_id":"'"$SESSION"'"}'
    case "$BEHAVIOR" in
      stream-hang) sleep 300;;
      stream-fail)
        echo '{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"notes/a.md"}}]}}'
        echo '{"type":"result","subtype":"error_during_execution","is_error":true}'
        exit 4;;
      *)
        echo '{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"notes/a.md"}}]}}'
        echo '{"type":"assistant","message":{"content":[{"type":"text","text":"working on it"}]}}'
        # Stage the same People.md the object-mode path writes (for ingest apply).
        if [ -n "$STAGING" ]; then
          mkdir -p "$STAGING/knowledge" 2>/dev/null
          printf '%s' "# People

  - Priya Natarajan — owns billing cutover
  " > "$STAGING/knowledge/People.md"
        fi
        # Automation phase-1 announces a proposal file to write.
        PROPOSAL=$(echo "$PROMPT" | grep -o 'PROPOSAL_FILE=[^ ]*' | head -1 | cut -d= -f2-)
        if [ -n "$PROPOSAL" ]; then
          mkdir -p "$(dirname "$PROPOSAL")" 2>/dev/null
          printf '%s' "## Proposed actions

  - Create Jira issue: follow up on billing cutover
  " > "$PROPOSAL"
        fi
        echo '{"type":"result","subtype":"success","is_error":false,"result":"done"}'
        exit 0;;
    esac
  fi
  ```
  3. Ensure `--verbose) ;;` remains a recognized no-op flag (it already is).

- [ ] This is infra only; verify it doesn't break existing runner/chat tests: `cargo test -p ken-core runner:: chat::`.
- [ ] **Commit:** `test(runner): fake claude streams stream-json for headless ingest runs`.

### Task C2 — `run_headless_streaming` with an activity callback

- [ ] **Test first.** In `runner.rs` tests, add a helper cfg and a test capturing activity lines:

```rust
    #[test]
    fn headless_streaming_reports_activity_and_completes() {
        let (dir, bin, hooks) = setup("complete");
        let staging = dir.path().join(".ken/.staging/people");
        let prompt = format!("Extract. STAGING_DIR={}", staging.display());
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen2 = seen.clone();
        let outcome = run_ingest_session(
            &cfg(&bin, RunnerMode::Headless, 30),
            dir.path(),
            "sess-stream",
            &prompt,
            &hooks,
            &CancelToken::new(),
            || {},
            move |line: &str| seen2.lock().unwrap().push(line.to_string()),
        )
        .unwrap();
        assert_eq!(outcome, RunOutcome::Completed);
        assert!(staging.join("knowledge/People.md").is_file());
        let lines = seen.lock().unwrap().clone();
        assert!(lines.iter().any(|l| l.contains("Read notes/a.md")), "{lines:?}");
    }

    #[test]
    fn headless_streaming_maps_error_result_to_failure() {
        let (dir, bin, hooks) = setup("stream-fail");
        let outcome = run_ingest_session(
            &cfg(&bin, RunnerMode::Headless, 30),
            dir.path(),
            "sess-stream-fail",
            "prompt",
            &hooks,
            &CancelToken::new(),
            || {},
            |_l: &str| {},
        )
        .unwrap();
        assert!(matches!(outcome, RunOutcome::Failed(_)), "{outcome:?}");
    }
```

- [ ] Run → **fails** (`run_ingest_session` absent).
- [ ] **Implement.** In `runner.rs`:
  1. Add `use crate::chat::{parse_event, ParsedEvent};` at the top.
  2. Add the public entry point that ingest/automation use. HiddenTui delegates to the unchanged path (activity ignored — its live view is the PTY broadcast); Headless streams:
  ```rust
  /// Run one ingest/automation session, streaming a live activity line. Headless
  /// mode uses `--output-format stream-json --verbose` and reports each tool /
  /// assistant line through `on_activity`; hidden-TUI keeps its PTY-broadcast
  /// behaviour and ignores `on_activity`.
  #[allow(clippy::too_many_arguments)]
  pub fn run_ingest_session(
      cfg: &RunnerConfig,
      project_root: &Path,
      session_id: &str,
      prompt: &str,
      hooks: &HookListener,
      cancel: &CancelToken,
      mut on_blocked: impl FnMut(),
      on_activity: impl FnMut(&str) + Send + 'static,
  ) -> Result<RunOutcome> {
      if !is_executable(&cfg.binary) {
          return Ok(RunOutcome::Failed(MISSING_CLAUDE_HELP.to_string()));
      }
      match cfg.mode {
          RunnerMode::HiddenTui => {
              run_hidden_tui(cfg, project_root, session_id, prompt, hooks, cancel, &mut on_blocked)
          }
          RunnerMode::Headless => {
              run_headless_streaming(cfg, project_root, session_id, prompt, cancel, on_activity)
          }
      }
  }
  ```
  3. Implement the streaming headless driver. It drains stdout line-by-line (activity), drains stderr concurrently (diagnostics), honours cancel + timeout, and derives the outcome from exit status + whether a non-error terminal `result` was seen:
  ```rust
  fn run_headless_streaming(
      cfg: &RunnerConfig,
      project_root: &Path,
      session_id: &str,
      prompt: &str,
      cancel: &CancelToken,
      mut on_activity: impl FnMut(&str) + Send + 'static,
  ) -> Result<RunOutcome> {
      use std::io::BufRead;
      let mut child = std::process::Command::new(&cfg.binary)
          .args([
              "-p", prompt,
              "--output-format", "stream-json",
              "--verbose",
              "--permission-mode", "acceptEdits",
              "--session-id", session_id,
          ])
          .current_dir(project_root)
          .stdout(std::process::Stdio::piped())
          .stderr(std::process::Stdio::piped())
          .stdin(std::process::Stdio::null())
          .spawn()
          .map_err(|e| Error::Other(format!("spawn {}: {e}", cfg.binary.display())))?;

      let stdout = child.stdout.take().ok_or_else(|| Error::Other("no stdout".into()))?;
      // Terminal-result signal shared with the reader thread: Some(is_error)
      // once a `result` event is seen.
      let result_seen: Arc<Mutex<Option<bool>>> = Arc::new(Mutex::new(None));
      let result_w = result_seen.clone();
      let reader = std::thread::spawn(move || {
          let buf = std::io::BufReader::new(stdout);
          for line in buf.lines().map_while(|l| l.ok()) {
              match parse_event(&line) {
                  ParsedEvent::AssistantText(t) => {
                      let one = t.lines().next().unwrap_or("").trim();
                      if !one.is_empty() {
                          on_activity(&truncate(one, 120).to_string());
                      }
                  }
                  ParsedEvent::Activity(s) => on_activity(&s),
                  ParsedEvent::TurnResult { is_error } => {
                      *result_w.lock().unwrap() = Some(is_error);
                  }
                  ParsedEvent::Init | ParsedEvent::Other => {}
              }
          }
      });

      // Drain stderr so a flood can't wedge the child (same reason as assistant).
      let (err_buf, err_thread) = crate::assistant::drain_pipe(child.stderr.take());

      let deadline = Instant::now() + cfg.timeout;
      let outcome = loop {
          if cancel.is_cancelled() {
              let _ = child.kill();
              break RunOutcome::Cancelled;
          }
          match child.try_wait() {
              Ok(Some(status)) => {
                  let is_error = *result_seen.lock().unwrap();
                  let stderr = err_buf.lock().unwrap().clone();
                  break match (status.success(), is_error) {
                      (true, Some(false)) | (true, None) => RunOutcome::Completed,
                      _ => RunOutcome::Failed(crate::assistant::with_stderr(
                          format!("the run exited with status {status:?}"),
                          &stderr,
                      )),
                  };
              }
              Ok(None) => {
                  if Instant::now() > deadline {
                      let _ = child.kill();
                      break RunOutcome::TimedOut(err_buf.lock().unwrap().clone());
                  }
                  std::thread::sleep(Duration::from_millis(150));
              }
              Err(e) => break RunOutcome::Failed(format!("wait failed: {e}")),
          }
      };
      let _ = reader.join();
      let _ = err_thread.join();
      Ok(outcome)
  }
  ```
  4. Make `assistant::drain_pipe` and `assistant::with_stderr` reachable: they're already `pub(crate)` — good (same crate). `truncate` already exists in `runner.rs`.

- [ ] Run the two new tests + `cargo test -p ken-core runner::` → pass. (`run_session`/`run_headless` stay for research/one-shot; untouched.)
- [ ] **Commit:** `feat(runner): streaming headless ingest sessions with a live activity callback`.

### Task C3 — Engine drives the streaming session and emits `activity` + `elapsed_secs`

- [ ] **Test first.** In `engine.rs` tests:

```rust
    #[test]
    fn running_event_carries_live_activity() {
        let r = rig("complete");
        r.engine.trigger("people", true);
        wait_status(&r.events, "running", 15);
        // At least one running event should carry an activity line before done.
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut saw_activity = false;
        while Instant::now() < deadline {
            if let Ok(ev) = r.events.recv_timeout(Duration::from_millis(200)) {
                if ev.status == "running" && ev.activity.as_deref().map(|a| a.contains("notes/a.md")).unwrap_or(false) {
                    saw_activity = true;
                }
                if ev.status == "fresh" { break; }
            }
        }
        assert!(saw_activity, "no running event carried an activity line");
    }
```

- [ ] Run → **fails** (engine still calls `runner::run_session`, no activity).
- [ ] **Implement.** In `execute()` (the ingest body), replace the `runner::run_session(…)` call with `run_ingest_session`, passing an `on_activity` closure that emits a `running` event carrying the activity line and elapsed seconds. Capture the run start `Instant` before the call:

```rust
    let started = Instant::now();
    let token = CancelToken::new();
    *current.lock().unwrap() = Some((slug.to_string(), token.clone()));

    // Live activity: each parsed tool/text line re-emits a `running` event with
    // the newest activity string and elapsed seconds. Transient — the frontend
    // overwrites its per-slug live marker; nothing is persisted here.
    let activity_emit = {
        let slug = slug.to_string();
        let sid = session_id.clone();
        // on_event is captured by ref elsewhere; clone what the closure needs.
        move |line: &str, on_event: &dyn Fn(IngestEvent)| {
            on_event(IngestEvent {
                activity: Some(line.to_string()),
                elapsed_secs: Some(started.elapsed().as_secs()),
                ..IngestEvent::at("ingest", &slug, run_id, Some(sid.clone()), "running", None)
            });
        }
    };
```

Because `on_event` is `&impl Fn`, wire the callback by cloning a sharable emitter. The cleanest concrete implementation: build an `Arc`-wrapped emitter used by both `execute` and the activity closure. Replace the `run_session` call block with:

```rust
    // Share the event sink so the streaming callback (which needs 'static) and
    // this function both emit through it.
    let emit: Arc<dyn Fn(IngestEvent) + Send + Sync> = {
        // SAFETY of lifetimes: `on_event` outlives the call; we forward each
        // event synchronously. Wrap in a channel to keep the callback 'static.
        let (etx, erx) = std::sync::mpsc::channel::<IngestEvent>();
        // Pump events from the streaming thread back to on_event on THIS thread
        // is not possible while blocked in run_ingest_session; instead emit
        // directly from the callback via a cloned sender and drain after.
        let _ = &erx; // see note below
        Arc::new(move |ev| { let _ = etx.send(ev); })
    };
```

> **Implementation note for the worker (choose the simpler wiring):** the `on_activity` callback in `run_ingest_session` is `impl FnMut(&str) + Send + 'static`, but `execute`'s `on_event` is a borrowed `&impl Fn`. The robust, race-free approach is a bounded channel: the streaming callback sends `IngestEvent`s into an `mpsc::Sender<IngestEvent>` (which is `Send + 'static`), and a **drain thread** spawned just before `run_ingest_session` forwards them to `on_event`. But `on_event` is borrowed, not `'static`. Therefore make `on_event` itself `Send + Sync + 'static` at the engine boundary (it already is — `IngestEngine::start`'s `on_event: impl Fn(IngestEvent) + Send + 'static`) by threading an `Arc<dyn Fn(IngestEvent) + Send + Sync>` through `execute` instead of `&impl Fn`. Concretely:

- [ ] Change `execute`'s signature so `on_event: &Arc<dyn Fn(IngestEvent) + Send + Sync>` (build the `Arc` once in `IngestEngine::start`'s worker: `let on_event = Arc::new(on_event);` and require `on_event: impl Fn(IngestEvent) + Send + Sync + 'static` in `start`). Then the activity closure clones the `Arc`:

```rust
    let started = Instant::now();
    let outcome = {
        let act_emit = on_event.clone();
        let a_slug = slug.to_string();
        let a_sid = session_id.clone();
        runner::run_ingest_session(
            &runner_cfg,
            &project.root,
            &session_id,
            &plan.prompt,
            hooks,
            &token,
            blocked_event,
            move |line: &str| {
                act_emit(IngestEvent {
                    activity: Some(line.to_string()),
                    elapsed_secs: Some(started.elapsed().as_secs()),
                    ..IngestEvent::at("ingest", &a_slug, run_id, Some(a_sid.clone()), "running", None)
                });
            },
        )
    };
    *current.lock().unwrap() = None;
```

- [ ] Update `IngestEngine::start` to take `on_event: impl Fn(IngestEvent) + Send + Sync + 'static` and inside the worker thread do `let on_event = Arc::new(on_event);` then pass `&on_event` where `execute` (and the loop's queued/waiting emits) need it. Update `src-tauri/src/lib.rs`'s closure — it's already `move |ev: IngestEvent| { … }` capturing `Send` state; add `Sync` by ensuring captured handles are `Sync` (they are: `AppHandle`, `Arc<Mutex<Db>>`). If a non-`Sync` capture appears, wrap in `Arc`.
- [ ] Keep `blocked_event` emitting via the same `on_event` (`IngestEvent::at("ingest", …, "blocked", …)`).
- [ ] Run the new test + full `engine::` suite → pass.
- [ ] **Commit:** `feat(engine): stream live activity + elapsed on running ingest events`.

---

# PART D — Queue visibility (queued / waiting) + unified job queue

This part refactors the queue to a keyed `PendingJob` (needed for automations too) and emits transient `queued`/`waiting` events.

### Task D1 — Introduce `QueueKey` / `RunKind` / `PendingJob`

- [ ] **Test first.** In `engine.rs` tests, assert a source change emits a `queued` event with an eta before the run, using a rig with a longer debounce. Add a rig variant parameter or a second constructor `rig_debounce(behavior, ms)` (extract from `rig`). Then:

```rust
    #[test]
    fn source_change_emits_queued_with_eta() {
        let r = rig_debounce("complete", 800);
        r.engine.sources_changed(vec!["notes/a.md".into()]);
        let q = wait_status(&r.events, "queued", 5);
        assert_eq!(q.kind, "ingest");
        assert_eq!(q.slug, "people");
        assert!(q.eta_secs.unwrap_or(0) >= 1, "eta should be ~1s for an 800ms debounce");
        // And it still runs after the debounce.
        wait_status(&r.events, "fresh", 20);
    }
```

- [ ] Run → **fails** (no `queued` emission; `rig_debounce` absent — extract it: `rig(behavior)` calls `rig_debounce(behavior, 200)`).
- [ ] **Implement.** In `engine.rs`:
  1. Add types:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
  pub enum RunKind { Ingest, Automation }

  impl RunKind {
      fn as_str(self) -> &'static str {
          match self { RunKind::Ingest => "ingest", RunKind::Automation => "automation" }
      }
  }

  #[derive(Debug, Clone, PartialEq, Eq, Hash)]
  struct QueueKey { kind: RunKind, slug: String }

  struct PendingJob {
      deadline: Instant,
      force_full: bool,
      /// Source files that matched an automation's globs across the debounce
      /// window (union). Empty for ingests and for run-now (recomputed).
      matched: Vec<String>,
      /// Phase-2 approved proposal text; Some only for an automation apply job.
      apply: Option<String>,
  }
  ```
  2. Replace `Msg`:
  ```rust
  enum Msg {
      Trigger { kind: RunKind, slug: String, force_full: bool },
      SourcesChanged(Vec<String>),
      AutomationApply { slug: String, proposal: String },
      Shutdown,
  }
  ```
  3. Replace the worker's `pending`/`force` maps with `pending: HashMap<QueueKey, PendingJob>`, and an `announced_queued: HashSet<QueueKey>` + `announced_waiting: HashSet<QueueKey>`.
  4. Handle `Trigger` (an ingest run-now, or an automation run-now):
  ```rust
      Ok(Msg::Trigger { kind, slug, force_full }) => {
          let key = QueueKey { kind, slug };
          pending.insert(key.clone(), PendingJob {
              deadline: Instant::now(), force_full, matched: vec![], apply: None,
          });
          announced_queued.remove(&key);
      }
  ```
  5. Handle `SourcesChanged` — evaluate recipes (as today) AND automations (Part F), enqueuing with a debounce deadline and unioning matched paths, then emit `queued` once per key:
  ```rust
      Ok(Msg::SourcesChanged(paths)) => {
          if let Ok(project) = Project::open(&project_root) {
              // Ingest recipes (unchanged trigger rule).
              if let Ok(entries) = recipe::list(&project) {
                  for entry in entries {
                      if let recipe::RecipeEntry::Ok { recipe: r } = entry {
                          if r.refresh == Refresh::OnChange && refresh::triggers(&r, &paths) {
                              let key = QueueKey { kind: RunKind::Ingest, slug: r.slug.clone() };
                              pending.entry(key.clone()).or_insert_with(|| PendingJob {
                                  deadline: Instant::now() + cfg.debounce,
                                  force_full: false, matched: vec![], apply: None,
                              });
                              maybe_announce_queued(&mut announced_queued, &pending, &key, &on_event);
                          }
                      }
                  }
              }
              // Automation rules (Part F wires automation::list + triggers).
              if let Ok(autos) = automation::list_ok(&project) {
                  for a in autos.iter().filter(|a| a.enabled) {
                      let hits = automation::triggers(a, &paths);
                      if hits.is_empty() { continue; }
                      let key = QueueKey { kind: RunKind::Automation, slug: a.slug.clone() };
                      let job = pending.entry(key.clone()).or_insert_with(|| PendingJob {
                          deadline: Instant::now() + cfg.debounce,
                          force_full: true, matched: vec![], apply: None,
                      });
                      for h in hits { if !job.matched.contains(&h) { job.matched.push(h); } }
                      maybe_announce_queued(&mut announced_queued, &pending, &key, &on_event);
                  }
              }
          }
      }
      Ok(Msg::AutomationApply { slug, proposal }) => {
          let key = QueueKey { kind: RunKind::Automation, slug };
          pending.insert(key.clone(), PendingJob {
              deadline: Instant::now(), force_full: true, matched: vec![], apply: Some(proposal),
          });
          announced_queued.remove(&key);
      }
  ```
  6. Add the queued/waiting helper (free functions in `engine.rs`):
  ```rust
  fn maybe_announce_queued(
      announced: &mut std::collections::HashSet<QueueKey>,
      pending: &HashMap<QueueKey, PendingJob>,
      key: &QueueKey,
      on_event: &Arc<dyn Fn(IngestEvent) + Send + Sync>,
  ) {
      if announced.contains(key) { return; }
      let Some(job) = pending.get(key) else { return };
      let eta = job.deadline.saturating_duration_since(Instant::now()).as_secs();
      let mut ev = IngestEvent::at(key.kind.as_str(), &key.slug, 0, None, "queued", None);
      ev.eta_secs = Some(eta);
      on_event(ev);
      announced.insert(key.clone());
  }
  ```
  7. Rewrite the run-dispatch block. Pick the earliest-due job; if one is due but `current` is busy, emit `waiting` once; otherwise dispatch by kind, clearing the announce sets for that key:
  ```rust
      let running = current_thread.lock().unwrap().is_some();
      let due: Option<QueueKey> = pending.iter()
          .filter(|(_, j)| j.deadline <= Instant::now())
          .min_by_key(|(_, j)| j.deadline)
          .map(|(k, _)| k.clone());
      if let Some(key) = due {
          if running {
              // Something's due but the single worker is busy: surface it once.
              if !announced_waiting.contains(&key) {
                  let current_name = current_thread.lock().unwrap()
                      .as_ref().map(|(s, _)| s.clone()).unwrap_or_default();
                  let mut ev = IngestEvent::at(key.kind.as_str(), &key.slug, 0, None, "waiting", None);
                  ev.detail = Some(format!("waiting for {current_name}"));
                  on_event(&ev.clone());   // deref the Arc emitter
                  announced_waiting.insert(key.clone());
              }
          } else {
              let job = pending.remove(&key).unwrap();
              announced_queued.remove(&key);
              announced_waiting.remove(&key);
              match key.kind {
                  RunKind::Ingest => execute_ingest(
                      &project_root, &mut db, &hooks, &cfg, &current_thread,
                      &key.slug, job.force_full, &on_event,
                  ),
                  RunKind::Automation => execute_automation(
                      &project_root, &mut db, &hooks, &cfg, &current_thread,
                      &key.slug, &job, &on_event,
                  ),
              }
          }
      }
  ```
  8. Update public API methods:
  ```rust
      pub fn trigger(&self, slug: &str, force_full: bool) {
          let _ = self.tx.send(Msg::Trigger { kind: RunKind::Ingest, slug: slug.into(), force_full });
      }
      pub fn run_automation(&self, slug: &str) {
          let _ = self.tx.send(Msg::Trigger { kind: RunKind::Automation, slug: slug.into(), force_full: true });
      }
      pub fn apply_automation(&self, slug: &str, proposal: &str) {
          let _ = self.tx.send(Msg::AutomationApply { slug: slug.into(), proposal: proposal.into() });
      }
  ```
  `sources_changed` and `cancel` unchanged in signature (cancel still matches on the running slug string; keep `current` keyed by slug string — collisions between an ingest and automation of the same slug are acceptable for cancel v1, documented).
  9. Rename the current `execute` to `execute_ingest` (body unchanged except it already emits with `IngestEvent::at("ingest", …)`), and thread `on_event: &Arc<dyn Fn(IngestEvent)+Send+Sync>`.

- [ ] Run the queued test + full `engine::` suite → pass. Existing `trigger_runs_and_applies_first_run`, `sources_changed_triggers_but_own_output_does_not`, `over_threshold_holds_then_approve_applies`, `discard_leaves_output_untouched` must all still pass (they use `trigger`/`sources_changed`, now routed through `RunKind::Ingest`).
- [ ] **Commit:** `feat(engine): unified job queue with queued/waiting visibility events`.

### Task D2 — `waiting` emitted when a second job is blocked by a running one

- [ ] **Test first.**

```rust
    #[test]
    fn second_due_job_reports_waiting() {
        // A slow first run so the second stays blocked long enough to observe.
        let r = rig("stream-hang"); // fake sleeps 300s in streamed headless
        r.engine.trigger("people", true);
        wait_status(&r.events, "running", 15);
        // A second recipe due immediately can't start; expect a waiting event.
        // (Create a second recipe in the rig, or reuse `people` via a distinct
        // slug — here trigger a run-now for a second recipe "places".)
        // See rig note below; assert we see a `waiting` event mentioning people.
        r.engine.trigger("places", true);
        let w = wait_status(&r.events, "waiting", 8);
        assert!(w.detail.unwrap_or_default().contains("people"));
        r.engine.cancel("people");
    }
```

> Rig note: extend `rig`/`rig_debounce` to also write a second recipe `places` (sources `notes`, output `knowledge/Places.md`) so two ingests can contend. Add that recipe in the rig setup.

- [ ] Run → **fails** until D1's waiting emission is correct and the `stream-hang` behaviour exists (added in C1).
- [ ] Confirm passing, then **Commit:** `test(engine): waiting event when the single worker is busy`.

---

# PART E — Frontend: ingest live activity + countdown

### Task E1 — API types for live events

- [ ] **Implement.** `src/lib/api.ts`:
  1. Add a live-status union and extend `IngestEvent`; keep `RunStatus` as the persisted set and add `kind` to `RunRow`:
  ```ts
  export type LiveStatus = RunStatus | "queued" | "waiting";

  export interface RunRow {
    id: number;
    slug: string;
    kind: "ingest" | "automation";
    sessionId: string | null;
    startedAt: number;
    finishedAt: number | null;
    status: RunStatus;
    summary: string | null;
    error: string | null;
    changeRatio: number | null;
  }

  export interface IngestEvent {
    kind: "ingest" | "automation";
    slug: string;
    runId: number;
    status: LiveStatus;
    detail: string | null;
    activity?: string | null;
    elapsedSecs?: number | null;
    etaSecs?: number | null;
  }
  ```
  2. Extend `InboxKind` with `"automation-proposal"`.
  3. Add automation types + wrappers (used in Parts H/I):
  ```ts
  export interface Automation {
    slug: string;
    name: string;
    globs: string[];
    prompt: string;
    autoApply: boolean;
    enabled: boolean;
  }
  export interface AutomationForm {
    slug?: string;
    name: string;
    globs: string[];
    prompt: string;
    autoApply: boolean;
    enabled: boolean;
  }
  export interface AutomationDetail {
    automation: Automation;
    runs: RunRow[];
  }
  ```
  and in the `api` object:
  ```ts
    listAutomations: () => invoke<Automation[]>("list_automations"),
    getAutomation: (slug: string) => invoke<AutomationDetail>("get_automation", { slug }),
    saveAutomation: (form: AutomationForm) => invoke<Automation>("save_automation", { form }),
    deleteAutomation: (slug: string) => invoke<void>("delete_automation", { slug }),
    runAutomation: (slug: string) => invoke<void>("run_automation", { slug }),
    approveAutomationProposal: (itemId: number) =>
      invoke<void>("approve_automation_proposal", { itemId }),
    discardAutomationProposal: (itemId: number) =>
      invoke<void>("discard_automation_proposal", { itemId }),
  ```

- [ ] `pnpm check` → clean. **Commit:** `feat(api): live-event fields + automation types/wrappers`.

### Task E2 — Pure live-run helpers (Vitest)

- [ ] **Test first.** `src/lib/liveRun.test.ts`:

```ts
import { describe, it, expect } from "vitest";
import { liveCaption, countdownFrom } from "./liveRun";

describe("liveCaption", () => {
  it("running with activity shows the activity and elapsed", () => {
    const c = liveCaption({ status: "running", activity: "Read notes/a.md", elapsedSecs: 12 });
    expect(c.label).toContain("Read notes/a.md");
    expect(c.label).toContain("12s");
    expect(c.tone).toBe("busy");
  });
  it("queued shows a countdown seeded from etaSecs", () => {
    const c = liveCaption({ status: "queued", etaSecs: 8 });
    expect(c.label.toLowerCase()).toContain("starts in");
    expect(c.label).toContain("8s");
  });
  it("waiting shows the blocking run", () => {
    const c = liveCaption({ status: "waiting", detail: "waiting for people" });
    expect(c.label).toContain("waiting for people");
    expect(c.tone).toBe("muted");
  });
});

describe("countdownFrom", () => {
  it("floors at zero and formats seconds", () => {
    const now = 10_000;
    expect(countdownFrom(now + 3000, now)).toBe("3s");
    expect(countdownFrom(now - 5000, now)).toBe("now");
  });
});
```

- [ ] Run `pnpm vitest run src/lib/liveRun.test.ts` → **fails** (module absent).
- [ ] **Implement.** `src/lib/liveRun.ts`:

```ts
import type { IngestEvent } from "./api";

export type Tone = "busy" | "attention" | "muted" | "healthy" | "danger";

/** A caption for a transient live event (queued/waiting/running). */
export function liveCaption(
  ev: Pick<IngestEvent, "status" | "activity" | "detail" | "elapsedSecs" | "etaSecs">,
): { label: string; tone: Tone } {
  switch (ev.status) {
    case "running": {
      const act = ev.activity?.trim();
      const el = ev.elapsedSecs != null ? ` · ${ev.elapsedSecs}s` : "";
      return { label: act ? `${act}${el}` : `running…${el}`, tone: "busy" };
    }
    case "queued":
      return { label: `queued — starts in ${ev.etaSecs ?? 0}s`, tone: "attention" };
    case "waiting":
      return { label: ev.detail ?? "waiting…", tone: "muted" };
    case "blocked":
      return { label: "blocked on you", tone: "attention" };
    default:
      return { label: String(ev.status), tone: "muted" };
  }
}

/** "3s" until `deadlineMs`, or "now" once reached. Pure for testing. */
export function countdownFrom(deadlineMs: number, nowMs = Date.now()): string {
  const s = Math.ceil((deadlineMs - nowMs) / 1000);
  return s > 0 ? `${s}s` : "now";
}
```

- [ ] Run → pass. **Commit:** `feat(ui): pure live-run caption + countdown helpers`.

### Task E3 — Ingests store consumes live fields, routes by kind

- [ ] **Test first.** `src/lib/ingestTabs.test.ts` (routing is pure; extract a `routesToIngest(ev)` predicate):

```ts
import { describe, it, expect } from "vitest";
import { routesToIngest } from "./ingests.svelte";

describe("event routing", () => {
  it("ingest events route to the ingests store", () => {
    expect(routesToIngest({ kind: "ingest" } as any)).toBe(true);
  });
  it("automation events do not", () => {
    expect(routesToIngest({ kind: "automation" } as any)).toBe(false);
  });
});
```

- [ ] Run → **fails**.
- [ ] **Implement.** In `src/lib/ingests.svelte.ts`:
  1. Export `export function routesToIngest(ev: IngestEvent): boolean { return ev.kind === "ingest"; }`.
  2. In `onEvent`, `if (!routesToIngest(ev)) return;` at the top.
  3. Extend `TERMINAL` handling: `queued`/`waiting`/`running` are non-terminal — keep the transient marker; only the existing `TERMINAL` set triggers a refresh + marker drop. Add `queued`/`waiting` to the `LiveStatus` the `live` map holds (it already stores the raw event).
  4. Expose the live event for the detail pane (`liveEvent(slug): IngestEvent | null`) so the screen can render `liveCaption`.
- [ ] Run → pass; `pnpm check` clean. **Commit:** `feat(ui): ingests store routes by kind and surfaces live activity`.

### Task E4 — IngestsScreen renders the activity line + timer + countdown

- [ ] **Implement (visual; no unit test — covered by the live-test recipe).** In `src/screens/IngestsScreen.svelte`:
  - In the list row caption and the detail head, when `ingests.liveEvent(slug)` is present, render `liveCaption(liveEvent)`. For `running`, run a local `setInterval` (1s) ticking a `$state` `nowTick` so the elapsed timer advances between server events; prefer `elapsedSecs` from the event when present, else derive from a captured start time. For `queued`, seed a deadline `Date.now() + etaSecs*1000` and render `countdownFrom(deadline, nowTick)`.
  - Follow Paper & Ink: activity line in `--ink-secondary` at 12px, monospace only for the file path portion; timer/countdown in `--ink-tertiary`; `busy` tone → `var(--accent)`, `attention` → `var(--needs-input)`, `muted` → `var(--ink-tertiary)`. No new colors. Quiet — no spinners that bounce; a single steady dot.
  - Empty/idle states unchanged.
- [ ] Manual verify via the project `verify` skill: trigger an ingest, watch the activity line advance and the timer tick; touch a source file and watch "queued — starts in Ns" count down, then "running…".
- [ ] **Commit:** `feat(ui): live activity line, elapsed timer, and queued countdown on Ingests`.

---

# PART F — Automation model (`ken-core/src/automation.rs`)

### Task F1 — Glob matching (pure, tested)

- [ ] **Test first.** New file `crates/ken-core/src/automation.rs` with a test module:

```rust
#[cfg(test)]
mod glob_tests {
    use super::glob_match;
    #[test]
    fn star_does_not_cross_slash() {
        assert!(glob_match("Recordings/*.md", "Recordings/a.md"));
        assert!(!glob_match("Recordings/*.md", "Recordings/sub/a.md"));
    }
    #[test]
    fn doublestar_crosses_slash() {
        assert!(glob_match("Recordings/**/*.md", "Recordings/sub/a.md"));
        assert!(glob_match("**/*.md", "a/b/c.md"));
        assert!(!glob_match("**/*.md", "a/b/c.txt"));
    }
    #[test]
    fn question_matches_single_non_slash() {
        assert!(glob_match("a?.md", "ab.md"));
        assert!(!glob_match("a?.md", "a/b.md"));
    }
    #[test]
    fn literal_and_case_sensitive() {
        assert!(glob_match("Notes/People.md", "Notes/People.md"));
        assert!(!glob_match("Notes/People.md", "notes/people.md"));
    }
}
```

- [ ] Run `cargo test -p ken-core glob_` → **fails** (module not declared / `glob_match` absent). First add `pub mod automation;` to `crates/ken-core/src/lib.rs` (alphabetical, after `assistant`/before `bg_hydrate` — place near `recipe`). Re-run to see the real failure.
- [ ] **Implement `glob_match`** — a small recursive matcher over path segments; `**` matches across `/`, `*`/`?` do not:

```rust
/// Match a project-relative path against a glob. `**` crosses `/`; `*` and `?`
/// do not. Case-sensitive. Pure so it's unit-tested without a filesystem.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let p: Vec<&str> = pattern.split('/').collect();
    let s: Vec<&str> = path.split('/').collect();
    seg_match(&p, &s)
}

fn seg_match(p: &[&str], s: &[&str]) -> bool {
    match p.first() {
        None => s.is_empty(),
        Some(&"**") => {
            // `**` consumes zero or more path segments.
            if seg_match(&p[1..], s) { return true; }
            !s.is_empty() && seg_match(p, &s[1..])
        }
        Some(seg) => {
            if s.is_empty() { return false; }
            wildcard_match(seg, s[0]) && seg_match(&p[1..], &s[1..])
        }
    }
}

/// `*` (any run, no `/`) and `?` (one char) within a single path segment.
fn wildcard_match(pat: &str, text: &str) -> bool {
    let pat: Vec<char> = pat.chars().collect();
    let text: Vec<char> = text.chars().collect();
    fn go(pat: &[char], text: &[char]) -> bool {
        match pat.first() {
            None => text.is_empty(),
            Some('*') => go(&pat[1..], text) || (!text.is_empty() && go(pat, &text[1..])),
            Some('?') => !text.is_empty() && go(&pat[1..], &text[1..]),
            Some(&c) => !text.is_empty() && text[0] == c && go(&pat[1..], &text[1..]),
        }
    }
    go(&pat, &text)
}
```

- [ ] Run → pass. **Commit:** `feat(automation): tested glob matcher (* / ** / ?)`.

### Task F2 — Automation model + persistence + `triggers`

- [ ] **Test first.** In `automation.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Project;
    use tempfile::tempdir;

    fn project() -> (tempfile::TempDir, Project) {
        let d = tempdir().unwrap();
        let p = Project::create(d.path(), "T").unwrap();
        (d, p)
    }

    fn sample() -> Automation {
        Automation {
            slug: "weekly-jira".into(),
            name: "Weekly Jira from recordings".into(),
            globs: vec!["Recordings/*.md".into()],
            prompt: "Summarize the recording and propose Jira tasks.".into(),
            auto_apply: false,
            enabled: true,
            extra: serde_yaml::Mapping::new(),
        }
    }

    #[test]
    fn save_load_roundtrip_and_default_auto_apply_false() {
        let (_d, p) = project();
        save(&p, &sample()).unwrap();
        let loaded = load_slug(&p, "weekly-jira").unwrap();
        assert_eq!(loaded, sample());
        assert!(!loaded.auto_apply);
        assert!(loaded.enabled);
    }

    #[test]
    fn triggers_returns_matched_paths_only() {
        let a = sample();
        let hits = triggers(&a, &[
            "Recordings/2026-07-13 14.02 Recording.md".into(),
            "Notes/other.md".into(),
        ]);
        assert_eq!(hits, vec!["Recordings/2026-07-13 14.02 Recording.md".to_string()]);
    }

    #[test]
    fn list_ok_skips_broken_files() {
        let (_d, p) = project();
        save(&p, &sample()).unwrap();
        std::fs::write(automation_path(&p.root, "broken"), "no frontmatter").unwrap();
        let ok = list_ok(&p).unwrap();
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].slug, "weekly-jira");
    }

    #[test]
    fn validate_rejects_empty_globs_and_prompt() {
        let mut a = sample();
        a.globs = vec![];
        assert!(validate(&a).is_err());
        a = sample();
        a.prompt = "  ".into();
        assert!(validate(&a).is_err());
    }
}
```

- [ ] Run → **fails**.
- [ ] **Implement.** `automation.rs` (mirrors `recipe.rs` structure — defensive parse, preserves unknown frontmatter):

```rust
//! Automations: `.ken/automations/<slug>.md` — YAML frontmatter + a
//! plain-language agent prompt body. Generic trigger→agent rules; external
//! reach comes only from the MCP servers the user configured for `claude`.
//! Parsing is defensive (one bad file never hides the rest) and rewriting
//! preserves fields this version doesn't know about.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::project::Project;
use crate::{Error, Result};

pub const AUTOMATIONS_DIR: &str = ".ken/automations";

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Automation {
    pub slug: String,
    pub name: String,
    pub globs: Vec<String>,
    pub prompt: String,
    pub auto_apply: bool,
    pub enabled: bool,
    #[serde(skip)]
    pub(crate) extra: serde_yaml::Mapping,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Frontmatter {
    name: String,
    #[serde(default)]
    globs: Vec<String>,
    #[serde(default)]
    auto_apply: bool,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(flatten)]
    extra: serde_yaml::Mapping,
}

fn default_true() -> bool { true }

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationError { pub slug: String, pub reason: String }

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum AutomationEntry {
    Ok { automation: Automation },
    Broken { error: AutomationError },
}

pub fn automations_dir(root: &Path) -> PathBuf { root.join(AUTOMATIONS_DIR) }
pub fn automation_path(root: &Path, slug: &str) -> PathBuf {
    automations_dir(root).join(format!("{slug}.md"))
}

/// Which of `changed` a rule's globs match — the file list handed to the prompt.
pub fn triggers(a: &Automation, changed: &[String]) -> Vec<String> {
    changed
        .iter()
        .filter(|p| a.globs.iter().any(|g| glob_match(g, p)))
        .cloned()
        .collect()
}

pub fn list(project: &Project) -> Result<Vec<AutomationEntry>> {
    let dir = automations_dir(&project.root);
    if !dir.is_dir() { return Ok(Vec::new()); }
    let mut names: Vec<_> = fs::read_dir(&dir)
        .map_err(|e| Error::io(&dir, e))?
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
        .map(|e| e.path())
        .collect();
    names.sort();
    let mut out = Vec::new();
    for path in names {
        let slug = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        match load(&path, &slug) {
            Ok(a) => out.push(AutomationEntry::Ok { automation: a }),
            Err(e) => out.push(AutomationEntry::Broken {
                error: AutomationError { slug, reason: e.to_string() },
            }),
        }
    }
    Ok(out)
}

/// Just the valid automations (the engine's trigger path ignores broken ones).
pub fn list_ok(project: &Project) -> Result<Vec<Automation>> {
    Ok(list(project)?
        .into_iter()
        .filter_map(|e| match e { AutomationEntry::Ok { automation } => Some(automation), _ => None })
        .collect())
}

pub fn load_slug(project: &Project, slug: &str) -> Result<Automation> {
    load(&automation_path(&project.root, slug), slug)
}

fn load(path: &Path, slug: &str) -> Result<Automation> {
    let raw = fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
    let (fm_str, body) = split_frontmatter(&raw)
        .ok_or_else(|| Error::Other("missing frontmatter — the file must start with a --- block".into()))?;
    let fm: Frontmatter = serde_yaml::from_str(fm_str)
        .map_err(|e| Error::Other(format!("frontmatter problem: {e}")))?;
    let a = Automation {
        slug: slug.to_string(),
        name: fm.name,
        globs: fm.globs,
        prompt: body.trim().to_string(),
        auto_apply: fm.auto_apply,
        enabled: fm.enabled,
        extra: fm.extra,
    };
    validate(&a)?;
    Ok(a)
}

pub fn save(project: &Project, a: &Automation) -> Result<()> {
    validate(a)?;
    let fm = Frontmatter {
        name: a.name.clone(),
        globs: a.globs.clone(),
        auto_apply: a.auto_apply,
        enabled: a.enabled,
        extra: a.extra.clone(),
    };
    let yaml = serde_yaml::to_string(&fm).map_err(|e| Error::Other(e.to_string()))?;
    let dir = automations_dir(&project.root);
    fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
    let path = automation_path(&project.root, &a.slug);
    fs::write(&path, format!("---\n{yaml}---\n\n{}\n", a.prompt.trim()))
        .map_err(|e| Error::io(&path, e))
}

pub fn delete(project: &Project, slug: &str) -> Result<()> {
    let path = automation_path(&project.root, slug);
    fs::remove_file(&path).map_err(|e| Error::io(&path, e))
}

pub fn validate(a: &Automation) -> Result<()> {
    let fail = |r: &str| Err(Error::Other(format!("automation '{}': {r}", a.slug)));
    if a.slug.trim().is_empty() || a.slug.contains('/') || a.slug.starts_with('.') {
        return fail("the file name must be a simple slug");
    }
    if a.name.trim().is_empty() { return fail("name can't be empty"); }
    if a.globs.is_empty() || a.globs.iter().all(|g| g.trim().is_empty()) {
        return fail("add at least one file pattern to watch");
    }
    if a.prompt.trim().is_empty() {
        return fail("the prompt can't be empty — say what Ken should do");
    }
    Ok(())
}

/// Same frontmatter splitter as recipes (kept local to avoid a cross-module dep).
fn split_frontmatter(raw: &str) -> Option<(&str, &str)> {
    let rest = raw.strip_prefix("---")?;
    let rest = rest.strip_prefix('\n').or_else(|| rest.strip_prefix("\r\n"))?;
    let end = rest.find("\n---")?;
    Some((&rest[..end + 1], {
        let after = &rest[end + 4..];
        after.strip_prefix('\n').unwrap_or(after)
    }))
}
```

(Keep the `glob_match`/`wildcard_match`/`seg_match` functions from F1 in this same file.)

- [ ] Run `cargo test -p ken-core automation::` → pass.
- [ ] **Commit:** `feat(automation): model, .ken/automations persistence, glob triggers`.

---

# PART G — Engine automation dispatch + two-phase gating

### Task G1 — Automation prompt composition (pure, tested)

- [ ] **Test first.** In `automation.rs`:

```rust
    #[test]
    fn proposal_prompt_names_files_staging_and_forbids_external_writes() {
        let a = sample();
        let matched = vec!["Recordings/a.md".to_string()];
        let staging = std::path::Path::new("/tmp/stg");
        let p = proposal_prompt(&a, &matched, staging);
        assert!(p.contains("Recordings/a.md"));
        assert!(p.contains("PROPOSAL_FILE=/tmp/stg/proposal.md"));
        assert!(p.to_lowercase().contains("do not") || p.to_lowercase().contains("must not"));
        assert!(p.contains(&a.prompt));
    }

    #[test]
    fn apply_prompt_embeds_the_approved_proposal() {
        let a = sample();
        let p = apply_prompt(&a, "## Proposed actions\n- Create issue X");
        assert!(p.contains("Create issue X"));
        assert!(p.to_lowercase().contains("execute"));
    }

    #[test]
    fn direct_prompt_used_when_auto_apply() {
        let a = sample();
        let p = direct_prompt(&a, &["Recordings/a.md".into()]);
        assert!(p.contains("Recordings/a.md"));
        assert!(p.contains(&a.prompt));
    }
```

- [ ] Run → **fails**.
- [ ] **Implement** in `automation.rs`:

```rust
/// Phase-1 (auto_apply=false): research + write a PROPOSAL, change nothing else.
pub fn proposal_prompt(a: &Automation, matched: &[String], staging: &Path) -> String {
    let files = if matched.is_empty() {
        "(no specific files — inspect the folders your patterns cover)".to_string()
    } else {
        matched.iter().map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n")
    };
    format!(
        r#"You are Ken running the automation "{name}".

## What to do

{prompt}

## Files that triggered this run

{files}

## THIS IS A PROPOSAL RUN — do not act outside the project

- Research whatever you need by READING files and using read-only tools.
- Do NOT create, edit, send, or delete anything outside this project folder.
  Do NOT call any tool that changes an external system (issue trackers, chat,
  email, calendars, etc.). This run only PLANS.
- Write a single markdown proposal to `PROPOSAL_FILE={proposal}` containing:
  1. A short summary of what you found.
  2. A section "## Proposed actions" listing each external action you intend,
     one bullet per action, concrete and self-contained (exactly what a later
     run must do). If no external action is warranted, say so plainly.
- Write ONLY that proposal file. Do not modify any other file.

PROPOSAL_FILE={proposal}
"#,
        name = a.name,
        prompt = a.prompt,
        files = files,
        proposal = staging.join("proposal.md").display(),
    )
}

/// Phase-2: execute exactly the approved actions (MCP tools available).
pub fn apply_prompt(a: &Automation, proposal: &str) -> String {
    format!(
        r#"You are Ken carrying out the approved actions for the automation "{name}".

The user has reviewed and APPROVED the plan below. Execute exactly these actions
using the tools available to you (including any MCP servers configured in the
user's Claude setup). Do only what the approved plan says — nothing more. When
done, reply with a one-line confirmation of what you did.

## Approved plan

{proposal}
"#,
        name = a.name,
        proposal = proposal,
    )
}

/// auto_apply=true: one session researches AND acts.
pub fn direct_prompt(a: &Automation, matched: &[String]) -> String {
    let files = if matched.is_empty() {
        String::from("(no specific files — inspect the folders your patterns cover)")
    } else {
        matched.iter().map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n")
    };
    format!(
        r#"You are Ken running the automation "{name}".

## What to do

{prompt}

## Files that triggered this run

{files}

Research what you need, then carry out the actions directly using the tools
available to you (including any MCP servers configured in the user's Claude
setup). When done, reply with a one-line confirmation.
"#,
        name = a.name, prompt = a.prompt, files = files,
    )
}
```

- [ ] Run → pass. **Commit:** `feat(automation): proposal / apply / direct prompt composition`.

### Task G2 — `execute_automation` (phase-1 stages a proposal review item; direct + apply run to fresh)

- [ ] **Test first.** In `engine.rs` tests, extend the `rig` so it can also create an automation. Add a helper `rig_with_automation(behavior, auto_apply)` that writes an automation `weekly` (globs `["notes/*.md"]`) and returns the same `Rig`. Then:

```rust
    #[test]
    fn automation_phase1_stages_a_proposal_review_item() {
        let r = rig_with_automation("complete", false);
        r.engine.run_automation("weekly");
        // Automation run logs as an automation-kind run and completes.
        let done = wait_status(&r.events, "fresh", 20);
        assert_eq!(done.kind, "automation");
        let db = Db::open_at(&r.db_path).unwrap();
        // A proposal review item was staged.
        let items = db.list_open_review_items().unwrap();
        let prop = items.iter().find(|i| i.kind == "automation-proposal").expect("proposal");
        assert!(prop.body.contains("Proposed actions"));
        assert_eq!(prop.source_ref, "weekly");
        // The automation run itself is recorded under kind=automation.
        assert!(!db.list_runs_of_kind("weekly", "automation", 5).unwrap().is_empty());
    }

    #[test]
    fn automation_auto_apply_runs_single_session_to_fresh() {
        let r = rig_with_automation("complete", true);
        r.engine.run_automation("weekly");
        let done = wait_status(&r.events, "fresh", 20);
        assert_eq!(done.kind, "automation");
        let db = Db::open_at(&r.db_path).unwrap();
        // No proposal item for an auto-apply automation.
        assert!(db.list_open_review_items().unwrap().iter().all(|i| i.kind != "automation-proposal"));
    }

    #[test]
    fn automation_apply_phase_runs_from_proposal() {
        let r = rig_with_automation("complete", false);
        r.engine.apply_automation("weekly", "## Proposed actions\n- do the thing");
        let done = wait_status(&r.events, "fresh", 20);
        assert_eq!(done.kind, "automation");
    }
```

- [ ] Run → **fails** (`execute_automation` unimplemented; `run_automation`/`apply_automation` from D1 route here).
- [ ] **Implement `execute_automation`** in `engine.rs`. It mirrors `execute_ingest`'s runner wiring (streaming, activity, cancel token, finish), but branches on phase:

```rust
#[allow(clippy::too_many_arguments)]
fn execute_automation(
    project_root: &PathBuf,
    db: &mut Db,
    hooks: &HookListener,
    cfg: &EngineConfig,
    current: &Arc<Mutex<Option<(String, CancelToken)>>>,
    slug: &str,
    job: &PendingJob,
    on_event: &Arc<dyn Fn(IngestEvent) + Send + Sync>,
) {
    let emit_fail = |run_id: i64, detail: String| {
        on_event(IngestEvent::at("automation", slug, run_id, None, "failed", Some(detail)));
    };
    let project = match Project::open(project_root) {
        Ok(p) => p, Err(e) => { emit_fail(0, e.to_string()); return; }
    };
    let a = match automation::load_slug(&project, slug) {
        Ok(a) => a, Err(e) => { emit_fail(0, e.to_string()); return; }
    };
    if !a.enabled && job.apply.is_none() { return; }

    // Matched files: from the queued change, else recompute from the index for
    // a run-now/apply.
    let matched: Vec<String> = if !job.matched.is_empty() {
        job.matched.clone()
    } else {
        db.list_files().map(|files| {
            files.into_iter()
                .filter(|f| f.status == "indexed")
                .map(|f| f.rel_path)
                .filter(|p| automation::triggers(&a, &[p.clone()]).len() == 1)
                .collect()
        }).unwrap_or_default()
    };

    // Fresh staging dir for a proposal file (reuse the refresh staging root).
    let staging = refresh::staging_dir(&project.root, &format!("auto-{slug}"));
    let _ = std::fs::remove_dir_all(&staging);
    if let Err(e) = std::fs::create_dir_all(&staging) { emit_fail(0, e.to_string()); return; }

    let phase = if job.apply.is_some() { Phase::Apply } else if a.auto_apply { Phase::Direct } else { Phase::Propose };
    let prompt = match &phase {
        Phase::Apply => automation::apply_prompt(&a, job.apply.as_deref().unwrap_or("")),
        Phase::Direct => automation::direct_prompt(&a, &matched),
        Phase::Propose => automation::proposal_prompt(&a, &matched, &staging),
    };

    let binary = cfg.binary.clone().or_else(runner::discover_claude)
        .unwrap_or_else(|| PathBuf::from("claude-not-found"));
    let session_id = Uuid::new_v4().to_string();
    let run_id = match db.insert_run_kind(slug, Some(&session_id), now_epoch(), "automation") {
        Ok(id) => id, Err(e) => { emit_fail(0, e.to_string()); return; }
    };
    on_event(IngestEvent::at("automation", slug, run_id, Some(session_id.clone()), "running", None));

    let token = CancelToken::new();
    *current.lock().unwrap() = Some((slug.to_string(), token.clone()));
    let runner_cfg = RunnerConfig { binary, mode: RunnerMode::Headless, timeout: cfg.timeout };
    let started = Instant::now();
    let outcome = {
        let act = on_event.clone();
        let (a_slug, a_sid) = (slug.to_string(), session_id.clone());
        runner::run_ingest_session(
            &runner_cfg, &project.root, &session_id, &prompt, hooks, &token,
            || {}, // headless never blocks on an interactive gate
            move |line: &str| {
                act(IngestEvent {
                    activity: Some(line.to_string()),
                    elapsed_secs: Some(started.elapsed().as_secs()),
                    ..IngestEvent::at("automation", &a_slug, run_id, Some(a_sid.clone()), "running", None)
                });
            },
        )
    };
    *current.lock().unwrap() = None;

    let finish = |db: &mut Db, status: &str, summary: Option<&str>, error: Option<&str>| {
        let _ = db.update_run(run_id, status, Some(now_epoch()), summary, error, None);
        on_event(IngestEvent::at("automation", slug, run_id, Some(session_id.clone()), status,
            summary.or(error).map(String::from)));
    };

    match outcome {
        Ok(RunOutcome::Completed) => match phase {
            Phase::Propose => {
                let proposal = std::fs::read_to_string(staging.join("proposal.md")).unwrap_or_default();
                let _ = std::fs::remove_dir_all(&staging);
                if proposal.trim().is_empty() {
                    finish(db, "fresh", Some("Checked — nothing to propose."), None);
                    return;
                }
                let payload = serde_json::json!({ "automationSlug": slug, "matched": matched }).to_string();
                let _ = db.insert_review_item(
                    "automation-proposal",
                    &format!("{} — proposal", a.name),
                    &proposal,
                    slug,
                    Some(&payload),
                    now_epoch(),
                );
                finish(db, "fresh", Some("Proposed actions — awaiting your approval."), None);
            }
            Phase::Direct => { let _ = std::fs::remove_dir_all(&staging); finish(db, "fresh", Some("Ran and applied."), None); }
            Phase::Apply => { let _ = std::fs::remove_dir_all(&staging); finish(db, "fresh", Some("Approved actions carried out."), None); }
        },
        Ok(RunOutcome::Cancelled) => { let _ = std::fs::remove_dir_all(&staging); finish(db, "cancelled", Some("Cancelled."), None); }
        Ok(RunOutcome::TimedOut(tail)) => {
            let _ = std::fs::remove_dir_all(&staging);
            finish(db, "failed", None, Some(&format!(
                "The run didn't finish within {} minutes and was stopped.\n{tail}",
                cfg.timeout.as_secs() / 60)));
        }
        Ok(RunOutcome::Failed(d)) => { let _ = std::fs::remove_dir_all(&staging); finish(db, "failed", None, Some(&d)); }
        Err(e) => { let _ = std::fs::remove_dir_all(&staging); finish(db, "failed", None, Some(&e.to_string())); }
    }
}

enum Phase { Propose, Direct, Apply }
```

Add `use crate::automation;` to `engine.rs` imports.

- [ ] Run the three G2 tests + full `engine::` suite → pass. Also confirm the `review-changed` refresh path: staging a review item requires the Tauri layer to emit `review-changed` — but that's the command layer. Here the engine writes the row; the frontend learns via the `ingest-run-changed` (automation) event which the review store also listens to (`onIngestRunChanged` → scheduleRefresh). Good — no extra event needed.
- [ ] **Commit:** `feat(engine): automation dispatch with two-phase proposal/apply gating`.

### Task G3 — Approve/discard a proposal (engine helpers)

- [ ] **Test first.** In `engine.rs` tests:

```rust
    #[test]
    fn approve_proposal_resolves_item_and_queues_apply() {
        let r = rig_with_automation("complete", false);
        r.engine.run_automation("weekly");
        wait_status(&r.events, "fresh", 20);
        let item_id = {
            let db = Db::open_at(&r.db_path).unwrap();
            db.list_open_review_items().unwrap().iter()
                .find(|i| i.kind == "automation-proposal").unwrap().id
        };
        let mut db = Db::open_at(&r.db_path).unwrap();
        approve_automation_proposal(&r.engine, &mut db, item_id).unwrap();
        // Item resolved…
        assert_eq!(db.get_review_item(item_id).unwrap().unwrap().status, "resolved");
        // …and a phase-2 automation run happens.
        let done = wait_status(&r.events, "fresh", 20);
        assert_eq!(done.kind, "automation");
    }

    #[test]
    fn discard_proposal_just_resolves() {
        let r = rig_with_automation("complete", false);
        r.engine.run_automation("weekly");
        wait_status(&r.events, "fresh", 20);
        let mut db = Db::open_at(&r.db_path).unwrap();
        let id = db.list_open_review_items().unwrap().iter()
            .find(|i| i.kind == "automation-proposal").unwrap().id;
        discard_automation_proposal(&mut db, id).unwrap();
        assert_eq!(db.get_review_item(id).unwrap().unwrap().status, "resolved");
    }
```

- [ ] Run → **fails**.
- [ ] **Implement** in `engine.rs`:

```rust
/// Approve a staged automation proposal: resolve the review item and queue the
/// phase-2 "execute exactly these approved actions" run through the engine.
pub fn approve_automation_proposal(engine: &IngestEngine, db: &mut Db, item_id: i64) -> Result<()> {
    let item = db.get_review_item(item_id)?
        .ok_or_else(|| Error::Other("proposal not found".into()))?;
    if item.kind != "automation-proposal" || item.status != "open" {
        return Err(Error::Other("this isn't an open automation proposal".into()));
    }
    let slug = item.source_ref.clone();
    db.resolve_review_item(item_id, now_epoch())?;
    engine.apply_automation(&slug, &item.body);
    Ok(())
}

/// Discard a proposal: resolve it, run nothing.
pub fn discard_automation_proposal(db: &mut Db, item_id: i64) -> Result<()> {
    let item = db.get_review_item(item_id)?
        .ok_or_else(|| Error::Other("proposal not found".into()))?;
    if item.kind != "automation-proposal" || item.status != "open" {
        return Err(Error::Other("this isn't an open automation proposal".into()));
    }
    db.resolve_review_item(item_id, now_epoch())
}
```

- [ ] Run → pass. **Commit:** `feat(engine): approve/discard automation proposals (queue phase-2)`.

---

# PART H — Tauri commands

### Task H1 — Automation CRUD + run + proposal approve/discard commands

- [ ] **Implement.** In `src-tauri/src/lib.rs`:
  1. Imports: `use ken_core::automation::{self, Automation};` and `use ken_core::engine::{approve_automation_proposal, discard_automation_proposal};`.
  2. DTOs + commands (place near the ingest commands, after `pending_approvals`):

```rust
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AutomationDetail {
    automation: Automation,
    runs: Vec<RunRow>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct AutomationForm {
    slug: Option<String>,
    name: String,
    globs: Vec<String>,
    prompt: String,
    #[serde(default)]
    auto_apply: bool,
    #[serde(default = "default_true_cmd")]
    enabled: bool,
}
fn default_true_cmd() -> bool { true }

#[tauri::command]
fn list_automations(state: State<SharedState>) -> CmdResult<Vec<Automation>> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    automation::list_ok(&active.project).map_err(err)
}

#[tauri::command]
fn get_automation(state: State<SharedState>, slug: String) -> CmdResult<AutomationDetail> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    let automation = automation::load_slug(&active.project, &slug).map_err(err)?;
    let runs = active.db.list_runs_of_kind(&slug, "automation", 20).map_err(err)?;
    Ok(AutomationDetail { automation, runs })
}

#[tauri::command]
fn save_automation(state: State<SharedState>, form: AutomationForm) -> CmdResult<Automation> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    let slug = match form.slug {
        Some(s) => s,
        None => {
            let base = kebab(&form.name);
            let mut slug = base.clone();
            let mut n = 2;
            while automation::automation_path(&active.project.root, &slug).exists() {
                slug = format!("{base}-{n}"); n += 1;
            }
            slug
        }
    };
    // Preserve unknown frontmatter on edit.
    let mut a = automation::load_slug(&active.project, &slug).unwrap_or_else(|_| Automation {
        slug: slug.clone(), name: String::new(), globs: vec![], prompt: String::from("-"),
        auto_apply: false, enabled: true, extra: Default::default(),
    });
    a.name = form.name; a.globs = form.globs; a.prompt = form.prompt;
    a.auto_apply = form.auto_apply; a.enabled = form.enabled;
    automation::save(&active.project, &a).map_err(err)?;
    automation::load_slug(&active.project, &a.slug).map_err(err)
}

#[tauri::command]
fn delete_automation(state: State<SharedState>, slug: String) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    automation::delete(&active.project, &slug).map_err(err)
}

#[tauri::command]
fn run_automation(state: State<SharedState>, slug: String) -> CmdResult<()> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    active.engine.run_automation(&slug);
    Ok(())
}

#[tauri::command]
fn approve_automation_proposal(app: AppHandle, state: State<SharedState>, item_id: i64) -> CmdResult<()> {
    let mut guard = state.lock().unwrap();
    let active = guard.active.as_mut().ok_or("no project open")?;
    engine::approve_automation_proposal(&active.engine, &mut active.db, item_id).map_err(err)?;
    let _ = app.emit("review-changed", ());
    Ok(())
}

#[tauri::command]
fn discard_automation_proposal(app: AppHandle, state: State<SharedState>, item_id: i64) -> CmdResult<()> {
    let mut guard = state.lock().unwrap();
    let active = guard.active.as_mut().ok_or("no project open")?;
    engine::discard_automation_proposal(&mut active.db, item_id).map_err(err)?;
    let _ = app.emit("review-changed", ());
    Ok(())
}
```

  3. Register all seven commands in the `tauri::generate_handler![…]` list (next to `run_ingest`, `approve_run`, …).
  4. In the engine `on_event` closure (~lib.rs:222): the chat-drawer surfacing should title automation runs sensibly. Where it builds the fallback `ChatRow` title `format!("Ingest — {}", ev.slug)`, branch on `ev.kind`: `if ev.kind == "automation" { format!("Automation — {}", ev.slug) } else { format!("Ingest — {}", ev.slug) }`, and set the chat `kind` to `ev.kind.clone()`. Leave the rest as-is.
  5. `emit_run_changed` already carries `kind` (Task B2).

- [ ] Build: `cargo build -p app` (or the workspace) → compiles.
- [ ] **Commit:** `feat(tauri): automation CRUD/run + proposal approve/discard commands`.

---

# PART I — Frontend: Automations tab + store + form

### Task I1 — Automations store

- [ ] **Implement.** `src/lib/automations.svelte.ts` (mirror `ingests.svelte.ts`, routing only `kind==="automation"` events):

```ts
import { api, type Automation, type AutomationDetail, type IngestEvent, type RunRow } from "./api";

const TERMINAL = new Set(["fresh", "failed", "discarded", "cancelled"]);

export function routesToAutomation(ev: IngestEvent): boolean {
  return ev.kind === "automation";
}

class AutomationsStore {
  list = $state<Automation[]>([]);
  selected = $state<string | null>(null);
  detail = $state<AutomationDetail | null>(null);
  live = $state<Record<string, IngestEvent>>({});

  liveEvent(slug: string): IngestEvent | null {
    return this.live[slug] ?? null;
  }

  async init() {
    await api.onIngestRunChanged((ev) => void this.onEvent(ev));
    await this.refresh();
  }

  private async onEvent(ev: IngestEvent) {
    if (!routesToAutomation(ev)) return;
    this.live = { ...this.live, [ev.slug]: ev };
    if (TERMINAL.has(ev.status)) {
      await this.refresh();
      if (this.selected === ev.slug) await this.select(ev.slug);
      const { [ev.slug]: _gone, ...rest } = this.live;
      this.live = rest;
    }
  }

  async refresh() {
    if (!(await hasProject())) return;
    this.list = await api.listAutomations();
  }

  async select(slug: string | null) {
    this.selected = slug;
    this.detail = slug ? await api.getAutomation(slug).catch(() => null) : null;
  }

  async run(slug: string) { await api.runAutomation(slug); }
  async remove(slug: string) {
    await api.deleteAutomation(slug);
    if (this.selected === slug) await this.select(null);
    await this.refresh();
  }
}

async function hasProject(): Promise<boolean> {
  return (await api.currentProject().catch(() => null)) !== null;
}

export const automations = new AutomationsStore();
```

- [ ] **Test.** Extend `src/lib/ingestTabs.test.ts` with `routesToAutomation`:
```ts
import { routesToAutomation } from "./automations.svelte";
it("automation events route to automations", () => {
  expect(routesToAutomation({ kind: "automation" } as any)).toBe(true);
  expect(routesToAutomation({ kind: "ingest" } as any)).toBe(false);
});
```
- [ ] `pnpm vitest run src/lib/ingestTabs.test.ts` → pass; `pnpm check` clean.
- [ ] **Commit:** `feat(ui): automations store`.

### Task I2 — AutomationForm component

- [ ] **Implement.** `src/ingests/AutomationForm.svelte` — Paper & Ink modeled on `IngestForm.svelte`. Fields: name (text), globs (one-per-line textarea, split/trim to `string[]`), prompt (textarea), auto-apply (segmented toggle Off/On using the app's segmented idiom), enabled (toggle). Include the known-limitation note near the auto-apply control:

```svelte
<script lang="ts">
  import { api, type Automation } from "../lib/api";
  let { slug = null, close }: { slug: string | null; close: (saved?: string) => void } = $props();

  let name = $state("");
  let globsText = $state("");
  let prompt = $state("");
  let autoApply = $state(false);
  let enabled = $state(true);
  let error = $state<string | null>(null);
  let loading = $state(slug !== null);

  $effect(() => {
    if (slug) void load(slug);
  });

  async function load(s: string) {
    const d = await api.getAutomation(s).catch(() => null);
    if (d) {
      const a: Automation = d.automation;
      name = a.name; globsText = a.globs.join("\n"); prompt = a.prompt;
      autoApply = a.autoApply; enabled = a.enabled;
    }
    loading = false;
  }

  async function save() {
    const globs = globsText.split("\n").map((g) => g.trim()).filter(Boolean);
    try {
      const saved = await api.saveAutomation({
        slug: slug ?? undefined, name, globs, prompt, autoApply, enabled,
      });
      close(saved.slug);
    } catch (e) {
      error = String(e);
    }
  }
</script>

<div class="scrim" onclick={() => close()} role="presentation">
  <div class="sheet" onclick={(e) => e.stopPropagation()} role="dialog" aria-modal="true">
    <h2>{slug ? "Edit automation" : "New automation"}</h2>
    {#if !loading}
      <label>Name<input bind:value={name} placeholder="Weekly Jira from recordings" /></label>
      <label>Watch these files
        <textarea bind:value={globsText} rows="3" placeholder="Recordings/*.md"></textarea>
        <span class="hint">One pattern per line. <code>*</code> matches within a folder, <code>**</code> across folders.</span>
      </label>
      <label>What Ken should do
        <textarea bind:value={prompt} rows="6"
          placeholder="Summarize each recording and propose Jira tasks for the follow-ups."></textarea>
      </label>
      <div class="toggle-row">
        <div>
          <div class="toggle-label">Act on its own</div>
          <div class="hint">
            Off (recommended): Ken writes a plan and waits for your approval before
            doing anything outside your files. On: Ken carries out actions directly.
          </div>
        </div>
        <div class="seg" role="group" aria-label="Act on its own">
          <button class:sel={!autoApply} onclick={() => (autoApply = false)}>Ask first</button>
          <button class:sel={autoApply} onclick={() => (autoApply = true)}>Automatic</button>
        </div>
      </div>
      <p class="limitation">
        Phase 1 is <em>asked</em> not to act outside your files — but that restraint is
        only a request. The real safety is the review step: nothing happens outside your
        project until you approve.
      </p>
      <label class="check"><input type="checkbox" bind:checked={enabled} /> Enabled</label>
      {#if error}<div class="err">{error}</div>{/if}
      <div class="actions">
        <button class="btn" onclick={() => close()}>Cancel</button>
        <button class="btn btn-primary" onclick={save}>Save</button>
      </div>
    {/if}
  </div>
</div>

<style>
  /* Reuse IngestForm's scrim/sheet look: --surface sheet, --border, radius-card,
     serif h2, 12px labels. Segmented control matches the Files All/Unread idiom
     (--accent tint on .sel). Keep spacing calm; accent used only on the primary
     button and the selected segment. */
</style>
```

> The worker copies the exact scrim/sheet/label/`.seg`/`.btn` CSS from `src/ingests/IngestForm.svelte` and `src/screens/FilesScreen.svelte`'s All/Unread segmented control so the form is visually consistent. No new tokens.

- [ ] `pnpm check` clean. **Commit:** `feat(ui): AutomationForm`.

### Task I3 — AutomationsPane (list + detail, live activity)

- [ ] **Implement.** `src/ingests/AutomationsPane.svelte` — mirrors the Ingests list/detail panes but for automations, using `automations` store + `liveCaption`/`countdownFrom` from `liveRun.ts`. Detail shows: name, globs chips, prompt, auto-apply state, "Run now" (and "Cancel run" when running), run history (from `detail.runs`, using `runLine`-style formatting), and the live activity line/timer/countdown when `automations.liveEvent(slug)` is set. Empty state teaches: "Automations watch for new files and do something with them — draft a summary, file a task. New files matching your patterns kick them off." Include a create/run flow via `AutomationForm`.

- [ ] Manual verify via the `verify` recipe (see Part K).
- [ ] **Commit:** `feat(ui): AutomationsPane with live activity`.

### Task I4 — IngestsScreen two-tab shell

- [ ] **Implement.** In `src/screens/IngestsScreen.svelte`, add a segmented tab at the top of the screen: **Knowledge docs** | **Automations** (`let tab = $state<"docs" | "automations">("docs")`). When `docs`, render today's list+detail (the current markup, unchanged). When `automations`, render `<AutomationsPane />`. Mount both stores' init once (`ingests.init()` on mount as today; `automations.init()` on mount too, so live events for automations update the badge/tab even when the docs tab is showing). Use the same segmented-control idiom as the All/Unread filter. Keep the "Knowledge docs" copy consistent with §7.

- [ ] `pnpm check` clean; manual verify tab switch.
- [ ] **Commit:** `feat(ui): Ingests screen — Knowledge docs / Automations tabs`.

---

# PART J — Review integration for automation proposals

### Task J1 — Backend: surface `automation-proposal` items with approve/discard rank

- [ ] **Implement.** In `src-tauri/src/lib.rs`:
  1. `stored_kind()` passes automation proposals through as their real kind:
  ```rust
  fn stored_kind(kind: &str) -> String {
      match kind {
          "conflict" | "conflict-copy" | "automation-proposal" => kind.to_string(),
          _ => "stored".to_string(),
      }
  }
  ```
  2. `inbox_rank()` places proposals just below live approvals:
  ```rust
      match kind {
          "approval" => 0,
          "automation-proposal" => 1,
          "conflict" | "conflict-copy" => 2,
          "stored" => 3,
          "broken-recipe" => 4,
          "failed-file" => 5,
          _ => 6, // stale
      }
  ```
  (The open-items loop at ~lib.rs:2033 already carries `it.payload` through for stored items, so the proposal's payload reaches the frontend unchanged.)
  3. `InboxItem` construction for open review items already sets `kind = stored_kind(&it.kind)` — automation proposals now keep their kind. No further backend change; the Done section keeps mapping resolved items via `stored_kind` too.

- [ ] Build → compiles. **Commit:** `feat(tauri): surface automation proposals in the Review inbox`.

### Task J2 — Frontend: `automation-proposal` kind, actions, and routing

- [ ] **Implement.** `src/lib/api.ts`: `InboxKind` already extended in E1 (`"automation-proposal"`).
  1. `src/lib/review.svelte.ts`:
     - `actionsFor("automation-proposal")` → `["approve", "discard"]`.
     - `dotFor("automation-proposal")` → `"var(--accent)"` (it's an action item).
     - `inboxFileRef` — proposals are slug-based (source_ref is the automation slug), so return `null` (no per-file ignore).
     - Branch `approve`/`discard` on kind:
     ```ts
     async approve(item: InboxItem) {
       if (item.kind === "automation-proposal") {
         await api.approveAutomationProposal(numericId(item));
       } else {
         await api.approveRun(numericId(item));
       }
       await this.refresh();
     }
     async discard(item: InboxItem) {
       if (item.kind === "automation-proposal") {
         await api.discardAutomationProposal(numericId(item));
       } else {
         await api.discardRun(numericId(item));
       }
       await this.refresh();
     }
     ```
  2. `src/screens/ReviewScreen.svelte`: the `act()` switch already routes `approve`/`discard` to `review.approve`/`review.discard`, so it works once the store branches. The detail pane renders `item.body` (the proposal markdown) — good. Optionally render the proposal body with the same monospace-for-paths treatment; plain `white-space: pre-wrap` (already present) is acceptable for v1.

- [ ] **Test.** Extend `src/lib/liveRun.test.ts` or add a small `review.test.ts` asserting `actionsFor("automation-proposal")` returns `["approve","discard"]`.
- [ ] `pnpm vitest run` + `pnpm check` clean.
- [ ] Manual verify: run an ask-first automation → a proposal appears in Review → Approve → a phase-2 automation run fires (visible in the Automations tab run history).
- [ ] **Commit:** `feat(ui): approve/discard automation proposals from Review`.

---

# PART K — End-to-end verification

### Task K1 — Rust suite green

- [ ] `cargo test -p ken-core` → all green (db, engine, runner, automation, refresh, chat, assistant).
- [ ] `cargo build -p app` (workspace) → compiles.

### Task K2 — Frontend checks

- [ ] `pnpm vitest run` → all green (`liveRun`, `ingestTabs`, review).
- [ ] `pnpm check` (svelte-check) → clean.

### Task K3 — Live-test recipe (project `verify` skill)

- [ ] Launch the app per the `verify` skill against real data.
- [ ] **§4:** open Ingests → Run now on a recipe; confirm the activity line advances ("Read …/editing …"), the elapsed timer ticks, and completion lands. Touch a source file; confirm "queued — starts in Ns" counts down (~10s), then "running…". Trigger two recipes; confirm the second shows "waiting for <name>". Force a no-op (Run now twice with no change); confirm a recorded run "Checked — nothing to update." appears.
- [ ] **§7:** create an automation (glob `Recordings/*.md` or a folder present in the test data, ask-first). Add a matching file (or Run now); confirm a proposal appears in Review with Approve/Discard. Approve; confirm a phase-2 automation run fires and its history logs under the Automations tab. Verify the known-limitation copy renders in the form.
- [ ] Note anything structural for a follow-up wave; land quick fixes here.

---

## Self-review against the spec

**§4 checklist**
- Live activity string + elapsed on the running state, event-forwarded like today → C3 (`activity`/`elapsed_secs` on transient `running` events), E2/E4 (render + tick).
- Countdown "queued — starts in Ns" for debouncing recipes; "waiting for <current run>" for queued ones → D1/D2 (engine emits `queued` with `eta_secs` and `waiting` with detail), E2 (`liveCaption`/`countdownFrom`).
- No-op runs recorded ("checked — nothing to update") + emitted → B3.
- Debounce 10s; concurrency 1 → B1 (debounce), D1 keeps a single `current` slot.
- stream-json parsing reusing chat's parser; hidden-TUI fallback keeps current behavior → C1–C3 (`run_ingest_session` streams headless; HiddenTui delegates unchanged).

**§7 checklist**
- Automation = { name, trigger globs on indexed rel_paths, prompt, auto_apply default false, enabled }, stored in `.ken/` → F2 (`.ken/automations/`, `auto_apply` default false, `enabled` default true).
- Trigger evaluation reuses the engine's SourcesChanged debounce; matched files passed to the prompt → D1 (SourcesChanged evaluates automations, unions matched), G1/G2 (matched files in prompts).
- Two-phase gating: auto_apply=false → phase-1 proposal (no external writes) staged as `review_items`; approval queues phase-2 "execute exactly these approved actions"; rejection discards. auto_apply=true → single session → G2/G3.
- Runs log to `ingest_runs` with a `kind` discriminator; same live-activity treatment → A1, G2 (`insert_run_kind("…","automation")`, streaming activity).
- UI: Automations tab with list, create/edit form (name, glob, prompt, auto-apply toggle), run history, run-now; known-limitation copy → I2/I3/I4.
- MCP reach free via the user's claude config — no per-service code → headless `claude -p` inherits the user's MCP servers; nothing added.

---

## Summary for the caller

- **Task count:** 24 tasks across 11 parts (A: 1, B: 3, C: 3, D: 2, E: 4, F: 2, G: 3, H: 1, I: 4, J: 2, K: 3). Every logic task is TDD (failing test → impl → pass → commit) against the existing fake-Claude harness; UI-only tasks (E4, I2–I4) are verified via the project `verify` live-test recipe.

- **Schema/config decisions:**
  - `ingest_runs` gains `kind TEXT NOT NULL DEFAULT 'ingest'` via guarded `ALTER TABLE`, **schema version 9** (the map-incremental plan's v8 `extractions` migration lands first; take v9 regardless — do not renumber). `insert_run` stays as an `"ingest"` delegate to a new `insert_run_kind`; added `list_runs_of_kind(slug, kind, n)`. `RunRow` gains `kind`.
  - Automations persist as `.ken/automations/<slug>.md` (YAML frontmatter `{name, globs, autoApply, enabled}` + prompt body), mirroring `.ken/ingests/`. Unknown frontmatter is preserved on rewrite.
  - Two-phase gate reuses `review_items` with a new kind `"automation-proposal"` (payload `{automationSlug, matched}`); the automation phase-1 run is recorded as a normal `fresh` automation run (it does NOT reuse the ingest `pending_approval` status, avoiding collision with `approve_run`, which applies staged ingest output). Approval resolves the item and queues a phase-2 run carrying the approved proposal text.
  - Debounce default lowered to **10s**; concurrency stays **1** (single `current` slot).

- **Interface decisions:**
  - `IngestEvent` is the single event for both kinds (event name `"ingest-run-changed"` unchanged), gaining `kind`, `activity`, `elapsed_secs`, `eta_secs`, plus a constructor `IngestEvent::at(kind, slug, run_id, session_id, status, detail)`. Two transient statuses `queued`/`waiting` are never persisted; the frontend routes by `ev.kind` (`routesToIngest`/`routesToAutomation`).
  - New runner entry `run_ingest_session(cfg, root, session_id, prompt, hooks, cancel, on_blocked, on_activity)`; Headless path streams `--output-format stream-json --verbose` and reports lines via `chat::parse_event`. `run_session`/`run_headless` are untouched (research + one-shot still use `--output-format json`).
  - Engine queue is keyed by `QueueKey { kind: RunKind, slug }` with a `PendingJob { deadline, force_full, matched, apply }`; public API adds `run_automation(slug)`, `apply_automation(slug, proposal)`, `approve_automation_proposal(engine, db, id)`, `discard_automation_proposal(db, id)`.
  - New Tauri commands: `list_automations`, `get_automation`, `save_automation`, `delete_automation`, `run_automation`, `approve_automation_proposal`, `discard_automation_proposal`. New pure module `src/lib/liveRun.ts` (`liveCaption`, `countdownFrom`) is the Vitest-covered rendering logic.

- **One deliberate v1 limitation** (documented in UI copy): phase-1 restraint is prompt-enforced (`acceptEdits` is granted to the runner), so the Review gate — not the phase-1 prompt — is the real control that anything external happens only after approval.

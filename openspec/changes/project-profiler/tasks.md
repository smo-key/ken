# Tasks: project-profiler

## 1. ken-core

- [x] 1.1 `profiler.rs` (new): `ScanStats`, `scan_stats(root,
      excluded)` reusing ingest walk rules, 50k-file cap with
      `truncated` marker; register module in `lib.rs`. Deviation:
      split a private `scan_stats_capped(root, excluded, cap)` out so
      the 50k-cap branch is unit-testable without creating 50k files;
      `scan_stats` is just that with `MAX_SCAN_FILES` hard-coded.
- [x] 1.2 `profiler.rs`: `ProjectProfile { kind, summary, languages,
      excludes, chunking: Vec<PatternProfile>, focus_hints,
      generated_hash, #[serde(flatten)] extra }` +
      `chunking_for(rel_path)` lookup; atomic save / tolerant load at
      `.ken/index-profile.json`; `hand_edited` detection via
      `generated_hash`. Note: `hand_edited` is a derived method
      (`is_hand_edited()`, comparing a fresh content hash against the
      stored `generated_hash`), not a stored field — matches the
      proposal's field list, which doesn't list `hand_edited`.
- [x] 1.3 `profiler.rs`: `deterministic_profile(stats)` — kind rules
      (doc-ratio thresholds), language guess from extension bytes,
      marker→exclude table, extension→chunk-mode table. The
      extension→chunk-mode table reuses `chunker::PROSE_EXTS`/
      `CODE_EXTS` (promoted from a private const inside
      `IndexProfile::default_for` to `pub` module-level consts in
      `chunker.rs`) rather than duplicating the list.
- [x] 1.4 `profiler.rs`: `compose_profile_prompt(stats, tree_sample)`
      (150-line depth-2 tree cap) + `parse_profile_refinement(raw)`
      with per-item validation (exclude dir must exist, no repo
      marker inside, additive-only); merge into profile. Deviations:
      (a) `tree_sample(stats)` is derived from `ScanStats`'s already-
      collected `largest_dirs`/`top_level_dirs` rather than a second
      filesystem walk — cheaper and keeps the profiler's only I/O in
      `scan_stats`; (b) `parse_profile_refinement` returns a plain
      `ProfileRefinement` (never fails) rather than `Result`, unlike
      `knowledge_model::parse_extraction` — the spec explicitly
      requires unparseable refinement output to reach `ready`, not
      `error`, so infallible tolerant parsing is the correct shape
      here; (c) per-item validation (dir exists, no repo marker
      inside) is a separate `apply_refinement(profile, refinement,
      root)` step, since it needs filesystem access that `parse_
      profile_refinement(raw)`'s stated signature (just `raw`)
      doesn't carry.
- [x] 1.5 Consumer hooks: engine ingest resolves chunk profile as
      stored-profile-else-default; `Project::effective_excluded()`
      union; `knowledge_model.rs` prompt addendum (≤500 chars, inside
      EXTRACT_CHAR_BUDGET). Deviation: each hook is a new `_with_
      profile`/`_with_addendum`/bool-flag entry point alongside the
      existing one, which now delegates to it with the inert default
      (`None`/`""`/`false`) — `rebuild_semantic_index` →
      `rebuild_semantic_index_with_profile`, `compose_file_prompt` →
      `compose_file_prompt_with_addendum`, `extract_one`/
      `process_next_pending` → `*_with_addendum`, and `Project::
      effective_excluded(profiler_enabled: bool)` is wholly new. This
      was necessary because the task scope is ken-core only (src-tauri
      is out of scope for this pass) and src-tauri already calls the
      unversioned functions (`process_next_pending`, `rebuild_
      semantic_index`) — changing their required signatures would
      break that build. Task 2.x should switch src-tauri's call sites
      to the new entry points, resolving `features::effective_flag
      (settings, project, "profiler")` and `ProjectProfile::load` to
      build the `Some(profile)`/non-empty-addendum/`true` arguments.
      Flag-off/no-profile inertness holds by construction: the old
      entry points always pass the inert default, so their output is
      byte-identical to before this change (tested in 1.6).
- [x] 1.6 Tests: fixture trees (Rust, Node, docs, mixed) → stats and
      deterministic profiles; refinement parse fixtures (fenced,
      junk-dropped, total-garbage fallback); hand-edit preservation;
      profile round-trip with unknown keys; consumer wiring (log-skip
      chunking, exclude union, prompt addendum + flag-off inertness).
      Written in `profiler.rs`, `chunker.rs` (Skip mode), `project.rs`
      (`effective_excluded`), `features.rs` (registry), `engine.rs`
      (`rebuild_semantic_index_with_profile`), and `knowledge_model.rs`
      (addendum threading). Not run this session (no cargo builds
      permitted) — deterministic and self-contained (tempdir fixtures,
      no network/model calls), meant to pass unmodified on the
      deferred verification build.

## 2. src-tauri

- [x] 2.1 `profile_project(id?)` command: scan → save deterministic →
      optional refinement (Background priority, settings toggle) →
      re-save; `profile-state` events
      (`scanning`/`refining`/`ready`/`error`); respect `profiler`
      per-project flag. Implemented as the shared `scan_and_profile`
      helper (lib.rs) that both this command and 2.2 call on a detached
      thread, off the global `AppState` lock per the lock-audit.md write
      template (snapshot `root`/`excluded`/id/`profiling_running` under
      the guard, drop it, scan+refine+save on the thread). Deviations:
      (a) no "settings toggle" for refinement exists anywhere in this
      codebase yet, so the only gate is `local_llm::llm_status() ==
      Ready` — refinement is skipped (not errored) when the model isn't
      ready, matching the spec's "model failure falls back" intent even
      though this isn't technically a failure; (b) hand-edit protection
      (design D2) is enforced here rather than deferred whole to 3.x, per
      `ProjectProfile::save`'s own doc comment assigning that
      responsibility to "task 2.x/3.x" jointly — `profile_project`
      refuses (`profile-state` `error`) to overwrite a profile that
      `is_hand_edited()`, since this command has no confirm/force
      parameter yet; task 3.x's confirm dialog + an eventual `force`
      param is how "Re-analyze after confirm" gets built. A
      `profiling_running` guard (new `MemberRuntime` field, mirrors
      `reindex_running`) rejects a second concurrent call for the same
      project with `Err` rather than silently no-oping.
- [x] 2.2 Workspace-creation integration: profile selected candidates
      with concurrency 2, emit per-candidate states, skip control,
      failures non-blocking. Implemented `profile_candidates(paths)`:
      two detached worker threads pull from a shared path queue
      (concurrency 2), each running `scan_and_profile` and emitting
      `profile-state` wrapped in a path-keyed envelope (no `Uuid` exists
      for a candidate folder, so `emit_member`'s `project_id` keying
      doesn't apply — see `CandidateProfileEnvelope`). One candidate's
      scan error only ends that worker's current iteration, never the
      others (each `scan_and_profile` call is independent and errors are
      swallowed into an `error` event, not propagated). Deviation: the
      actual workspace-creation UX (candidate picker, "Skip analysis"
      control, confirm screen) doesn't exist — Phase 2 built only
      `open_member`/`close_member` — so this is the honestly-
      implementable backend slice per the task prompt; wiring a real
      creation flow around it (including the skip control) is frontend
      work, deferred to task 3.x. Not gated on the `profiler` flag
      (no `Project`/`features` map exists yet for an unopened candidate
      folder) — the future picker UI is expected to check the global
      `profiler` default via `list_features` before calling this.
- [x] 2.3 Trigger re-ingest after a profile save changes chunking or
      excludes (reuse existing rebuild path). Implemented by generalizing
      `apply_semantic_index_flag` to take a `target: Option<Uuid>` (was
      hardcoded to the focused member via `member(&guard, None)`; the
      3 pre-existing call sites now pass `None` explicitly, byte-
      identical behavior) and, inside its background rebuild, resolving
      `profiler`'s effective flag + `ProjectProfile::load` and calling
      `engine::rebuild_semantic_index_with_profile` instead of the plain
      `rebuild_semantic_index` — this is "now passing the profile" from
      the task prompt. `scan_and_profile` reports `changed` (excludes or
      chunking differ from what was on disk before the pass); when
      `true`, `maybe_rebuild_semantic_index_for_profile` calls
      `apply_semantic_index_flag(.., target, true)` — but ONLY if
      `semanticIndex` is already enabled for that project, since calling
      it unconditionally would turn semantic indexing on as a side
      effect of profiling (the two flags must stay independent). Also
      switched the one other call site the task named:
      `extraction_worker`'s `process_next_pending` →
      `process_next_pending_with_addendum`, threading a per-file,
      flag-gated `profiler::profile_prompt_addendum` (empty string when
      the flag is off or no profile exists, reproducing the old prompt
      byte-for-byte — ken-core task 1.5's inertness guarantee).
      `compose_file_prompt` has no direct lib.rs call site (only reached
      through `process_next_pending`/`extract_one`, both already
      addressed); `build_knowledge_model`'s separate batch-rebuild path
      doesn't call `compose_file_prompt` at all, so it's untouched —
      out of scope per design D3, which only calls out the per-file
      extraction prompt.

## 3. Frontend

- [x] 3.1 `api.ts`: `ProjectProfile` type (mirrors `ProfileDto`, camelCase),
      `ProfileState` tagged-union type (mirrors `ProfileStateEvent`; carries
      optional `project_id` for `profile_project` or `path` for
      `profile_candidates`), `profileProject`/`profileCandidates` invoke
      wrappers, `onProfileState` listener — all following the existing
      `SemanticIndexState`/`onSemanticIndexState` conventions.
- [x] 3.2 No real multi-candidate workspace-creation checklist exists yet
      (Phase 2 built only `open_member`/`close_member`, per tasks.md 2.2's
      own deviation note) — deferred whole, per the task prompt's
      "prefer deferral over invention." Implemented instead in
      `src/onboarding/ProjectPicker.svelte` (the only folder-selection UI
      that exists), which already has a single-candidate "pending path"
      confirm step: when the chosen folder resolves and the *global*
      `profiler` default (`listFeatures()` with no project — the pattern
      `profile_candidates`'s own doc comment calls for, since the folder
      isn't a project yet) is on, fires `profileCandidates([folder])` and
      shows a spinner while scanning/refining, then a kind badge + one-line
      summary on `ready`, filtered by `ev.path === pendingPath` from
      `onProfileState`. Added a "Skip" control that hides the affordance
      locally — it does not cancel the backend scan (no cancel-profile
      command exists), which is honest here because project creation was
      never gated on profiling finishing (D5's "creates the workspace
      without waiting" already holds by construction — Skip just stops
      showing it). Nothing renders when the `profiler` flag is off or on
      `error` (silent — profiling failures are non-blocking by design).
      Follow-up: a real candidate-picker/checklist UI is still needed for
      true multi-folder workspace creation; this only covers the
      single-project onboarding path.
- [x] 3.3 Project settings: added a "Project profile" card in
      `src/screens/SettingsScreen.svelte`, gated on `features.some(f =>
      f.name === "profiler" && f.effective)` (Features-section styling:
      `.card`/`.card-title`/`.row`/`.chip`/`.folder`/`.note` reused as-is).
      Shows kind (chip), languages, summary, and profiler-added excludes in
      their own `.folders` list separate from the watched-folders tree
      above. Deviation: excludes are NOT actually removable — there is no
      backend command to persist "drop this one profiler-added exclude"
      (design D3's union is computed in `Project::effective_excluded`, not
      stored per-item anywhere removable); a disabled "Remove" button with
      an explanatory `title` tooltip is shown instead of a working one or
      no control at all, naming the missing backend piece rather than
      faking removal — follow-up: needs a command (e.g. persisting a
      profile-exclude denylist) before this can be real.
      "Re-analyze"/"Analyze project" button calls the existing
      `profile_project` command; since `profile_project` unconditionally
      refuses (`error` event) to overwrite a hand-edited profile and has no
      force parameter, there is no working confirm-then-force flow — on
      that specific error the card surfaces explanatory copy (points at
      `.ken/index-profile.json`, explains why Ken won't overwrite it) via
      `.note.warn`, not a fake "overwrite anyway" button. Also deferred: no
      read-only "get current profile" command exists (only
      `profile_project`, which re-scans), so the card shows nothing until
      the user analyzes/re-analyzes at least once *this session* — it
      can't show a profile generated in a previous session on load.
      Follow-ups noted here are both backend-command gaps, out of this
      pass's `src/`-only scope.

## 4. Verification

- [ ] 4.1 cargo test --workspace, pnpm test, pnpm check, pnpm build
      green
- [ ] 4.2 Manual: profile the real ken repo (expect code/Rust+TS,
      `target/` + `node_modules/` excluded) and a docs-heavy member;
      hand-edit a profile and confirm re-analyze prompts before
      overwrite; flag off ⇒ file ignored

# Tasks: ken-pipeline

Read `proposal.md` → `design.md` → `specs/ken-pipeline/spec.md`
before starting. Two sequencing rules this file encodes:

- **D6**: do not implement auto-transitions (2.12) until 5.4 has
  passed once.
- **D5**: the blocked refusal in `admit()` (1.8) is the invariant
  most likely to be broken by a later change. It lives in exactly one
  function and every caller — UI, MCP, and any future
  auto-transition — must go through it. Never re-implement it.

## 1. ken-core

- [x] 1.1 `pipeline.rs` (new): pipeline-definition model —
      `Pipeline { id, name, auto, concurrency_cap, bounce_cap,
      lanes: Vec<Lane> }`, `Lane { id, name, maps_to: TaskStatus,
      agent, model, kickoff, on_pass, on_fail, writes_code, human,
      terminal, generative, blocked, runner }`; `#[serde(default)]`
      on every optional field plus a flattened extras map;
      parse/serialize via the `tasks.rs` patch core so unknown keys,
      key order, and the body survive byte-for-byte;
      `pipelines_dir(workspace_root)`; validation that at most one
      lane sets `blocked: true` and at most one sets `human: true`;
      register in `lib.rs`
      - Note: parsed from a tolerant `serde_yaml::Mapping` rather than a
        `#[serde(default)]`+`flatten` struct — flatten inside a lane
        sequence is exactly where serde_yaml is fragile, and the mapping
        read gives the same defaults plus the extras map. Writes go
        through `patch_pipeline_text` → `tasks::patch_text`, so byte
        fidelity is the shipped patch core's, not a second one.
      - Note: `validate_pipeline` *reports* issues rather than failing the
        parse (a bad definition must not take the board down); it also
        catches duplicate lane ids, bad `maps_to`/`kickoff`, and dangling
        `on_pass`/`on_fail` targets.
- [x] 1.2 `pipeline.rs`: lane resolution — `resolve_lane(&Pipeline,
      status_raw) -> Option<&Lane>`, lane index lookup, and the
      `maps_to` projection. This is the single home of the
      board-scoped vocabulary check (D2); nothing else parses a lane
      - Note: an *empty* `status` on a pipeline ticket resolves to the
        first lane, mirroring `parse_task`'s "absent status ⇒ the intake
        column" — for a board-scoped vocabulary the intake column is
        whichever lane the human put first (D1: lane order is column
        order).
- [x] 1.3 `tasks.rs`: lane-aware parse — `Task` gains
      `lane: Option<String>`; when `pipeline` is set, `status` is
      derived from the resolved lane's `maps_to` instead of
      `TaskStatus::parse`; `status_raw` unchanged. `TaskFilter` gains
      `lane`, `pipeline`, and `blocked: Option<BlockedFilter>`
      (`Any | Blocked | NotBlocked | By(String) | NewlyUnblocked`) —
      all `Option`, all `#[serde(default)]`, and `matches` honours
      them. **No change to the pipeline-less path.**
      - Note: `parse_task` still leaves `lane: None` — it has no pipeline
        handle by design. `pipeline::resolve_task_lane` / `resolve_board`
        fill `lane` and re-derive `status` from `maps_to`, keeping the
        vocabulary check single-homed (1.2). A pipeline-less `Task` is
        byte-identical before and after resolution (asserted).
      - Note: `BlockedFilter` is evaluated from the ticket's own
        frontmatter only, so `matches` stays pure over one `Task`.
        "Blocked" therefore means *carries block evidence*
        (`blocked_by`/`block_reason`), not "status equals the blocked
        lane id" — the blocked lane id is data and `matches` has no
        pipeline. `NewlyUnblocked` = a surviving `return_lane` with
        nothing left holding it.
- [x] 1.4 `tasks.rs`: `needs_attention` takes the loaded pipelines as
      an extra input; new `AttentionReason::UnknownLane(String)`,
      `UnknownPipeline(String)`, `UnknownBlocker(String)`, and
      `UnknownReturnLane(String)`; `apply_patch` refuses a
      non-status patch on an unknown-lane ticket exactly as it
      already does for an invalid status
      - Judgment call: the signature was **added, not changed**.
        `needs_attention(tasks, goals)` and `apply_patch(path, patch,
        updated)` keep their exact shapes and delegate to
        `needs_attention_with_pipelines` / `apply_patch_with_pipelines`
        with an empty definition slice. Changing the signatures would
        break `src-tauri` and `ken-mcp`, which this session may not edit;
        the additive form also means a caller that doesn't know about
        pipelines conservatively refuses to edit around a lane it can't
        validate. Callers get rewired in 2.x/3.x.
      - Note: `UnknownLane`/`UnknownPipeline` *replace* `InvalidStatus`
        for a pipeline ticket rather than doubling up on it.
- [x] 1.5 `pipeline.rs`: ticket pipeline fields — read/patch helpers
      for `pipeline`, `model`, `agent`, `scope`, `verify`, `bounces`,
      `return_lane`, `blocked_by`, `block_reason`, `blocked_at`,
      `parent`, `spawned_by`, `origin`, `projects`, `target`. All
      ride the existing `extra` flatten on the task frontmatter;
      extend `TaskPatch` with the writable ones only (`blocked_by`
      renders as a sequence via the existing `seq_lines` path)
      - Note: `TaskPatch` gains `lane: Option<String>`, which writes the
        *same physical `status` key* as `TaskPatch::status` (D2: one
        status key, two vocabularies). `lane` wins when both are set.
      - Note: numeric/boolean values go through the shipped
        `scalar_lines`, which quotes every YAML-ambiguous scalar
        ("dates, numbers ... single-quoted, which is always safe"). So
        `bounces` lands as `bounces: '2'`, not `bounces: 2`, despite D4
        calling it a plain integer. Reusing the renderer beats forking a
        numeric one; `ticket_fields` reads both forms and it is asserted
        to round-trip to the integer 2.
- [x] 1.6 `pipeline.rs`: **the block model (D5)** — `Block {
      return_lane, blocked_by: Vec<Ulid>, reason: Option<String>,
      blocked_at }`; `block(ticket, &Pipeline, request) ->
      BlockResult` capturing `return_lane` from the ticket's *current*
      lane before the move (never supplied by the caller, so a
      blocked ticket without a return lane is unconstructable);
      `unblock(ticket, request)` clearing dependencies and/or reason
      independently. `blocked_by` holds ULIDs only — reject anything
      that parses as a path
      - `Block`'s fields are **private** with read-only accessors and
        `return_lane` is a plain `String`, so this module holds its only
        constructors and "blocked with no return lane" does not
        typecheck. `BlockRequest` has no `return_lane` field at all.
      - Judgment call: re-blocking a ticket already in the blocked lane
        **preserves** its recorded `return_lane` instead of capturing
        "blocked" as the return lane; if that field is missing/orphaned
        the block is refused (`MissingReturnLane`) rather than invented.
        The design didn't cover extending an existing block.
      - Judgment call: `blocked_by` entries are rejected as
        `PathBlocker` when path-shaped and `MalformedBlocker` when not a
        26-char Crockford ULID. The stricter half means a hand-created
        ticket whose `id` fell back to a non-ULID filename stem cannot be
        a blocker — deliberate, since the spec says "SHALL hold ticket
        ULIDs", but it is a real edge.
      - Judgment call: `unblock` resets `bounces` only when the reason
        matches D4's retry-cap prefix; a dependency-blocked ticket keeps
        its bounce history.
- [x] 1.7 `pipeline.rs`: **write-time cycle detection (D5)** —
      `check_cycle(&BlockGraph, from, to) -> Result<(), CyclePath>`,
      a depth-first walk over the existing blocked tickets; refuse
      the edge and report the full path. Called by `block()` before
      any write; pure and table-tested with direct, 3-hop, and
      long-chain cases
      - Note: each offered edge is checked against the graph **plus the
        siblings already accepted from the same request**, so a set of
        dependencies that only closes a cycle in combination is still
        refused.
      - Note: the walk carries a `seen` set, so it terminates on a graph
        that a hand edit has *already* made cyclic.
      - Note: a "shortcut" edge along an existing chain (A→…→F, then
        A→F directly) is correctly **allowed** — it adds no loop.
- [x] 1.8 `pipeline.rs`: admission — `admit(ticket, &Lane,
      &Pipeline, &[RunRecord], entry: EntryKind) -> Admission`
      returning `Start | Confirm{reason} | Queued | Refused{reason}`.
      Order is load-bearing: **(1) blocked ⇒ `Refused` first, before
      anything else**; (2) missing `scope`/`verify` ⇒ downgrade to
      `Confirm`; (3) `entry == Unblock` ⇒ downgrade to `Confirm` even
      for an `auto` lane; (4) gate mode; (5) concurrency cap over
      currently-`running` records ⇒ `Queued`. Pure; table-tested
      across the full cross-product
      - Blocked-first is implemented as the literal first statement and
        tested across ticket-shape × pipeline(auto on/off) × 6 lanes ×
        4 entry kinds × ledger(empty/full) — 240 combinations, every one
        `Refused{Blocked}`, plus a positive control in the same lane.
      - Step (3) `EntryKind::Unblock` is an **unconditional early
        return**, not a flag consulted later, and is backed by a
        property test that no `Unblock` input in any lane of any
        pipeline reaches `Start`. Comment states it becomes the
        load-bearing rule under a future `command` runner.
      - Judgment call: two refusals not in the numbered list sit between
        (1) and (2) — `human: true` lanes (D11: no agent, no kickoff)
        and `agent: none` holding columns. Both mean "nothing to run",
        so they are refusals rather than gates; they are placed *after*
        the blocked check so the ordering claim is unaffected.
      - Judgment call: entry-kind semantics the design left implicit —
        `Kickoff` on a `manual` lane yields `Confirm` (the spec bars a
        lane from starting *itself*, not a human from asking);
        `AutoTransition`/`Unblock` into a `manual` lane is
        `Refused{ManualLane}`; `Claim` treats the lane gate as already
        satisfied (the queued run record is the authorisation) but is
        still subject to the boundary downgrade and the cap.
- [x] 1.9 `pipeline.rs`: transition resolution — `advance(ticket,
      &Pipeline, outcome) -> Transition`, resolving `on_pass` /
      `on_fail`, classifying backward moves as bounces, incrementing
      `bounces`, and — when `bounce_cap` would be exceeded —
      returning a **block** result (D4: `block_reason: exceeded retry
      cap`, `return_lane` = the lane it was bouncing to) rather than
      a separate halted state. There is no `halted` field anywhere in
      this codebase
      - Signature deviation: `advance(ticket, &Pipeline, outcome, now)`.
        The cap-breach path has to write `blocked_at` and this module
        owns no clock (same caller-supplied-date convention as
        `tasks::create_task` and `memory.rs`).
      - Boundary asserted exactly: with `bounce_cap: 3`, bounces 0→1,
        1→2, 2→3 all move; the attempt that would make 4 returns
        `Transition::Blocked` with `block_reason: exceeded retry cap (4
        bounces)` and `return_lane` = the lane it was bouncing *to*
        (note this is the opposite capture rule from `block()`, per D4).
        `bounces: 4` is still written — the count is the evidence.
      - No `Halted` variant exists; grep confirms no `halted` anywhere.
- [x] 1.10 `pipeline.rs`: unblock evaluation —
      `evaluate_unblocks(&[Task], &Pipeline, terminal_ticket_id) ->
      Vec<Unblocked>`, resolving which dependents are now free (all
      `blocked_by` terminal **and** `block_reason` empty) and
      returning them with their `return_lane`. Callers re-enter them
      with `EntryKind::Unblock` (1.8), never with a start
      - `evaluate_all_unblocks(&[Task], &Pipeline)` added alongside for
        the workspace-open sweep the spec requires ("and again on
        workspace open").
      - Note: a `blocked_by` id that resolves to no ticket is **not**
        treated as terminal — an unresolvable dependency must not read
        as a satisfied one; it is an `UnknownBlocker` tray entry.
        Likewise a missing/orphaned `return_lane` is skipped, not
        guessed at.
      - The 1.10 → 1.8 handoff is tested end to end: what
        `evaluate_unblocks` returns, re-admitted with
        `EntryKind::Unblock` into an `auto` lane of an `auto: true`
        pipeline, is `Confirm{Unblocked}`.
- [x] 1.11 `pipeline.rs`: run-record model + pathing —
      `RunRecord` + `RunOutcome` + `running_runs()` were the prior
      session's seam; this session added `runs_dir`/`run_month_dir`/
      `run_path`/`run_month`, `parse_run`/`compose_run`/
      `patch_run_text`, `scan_runs` (a caller-supplied `(path, raw)`
      map — this module still does no filesystem I/O of its own), and
      `derive_queue` (the `running`/`queued`/`blocked`/`waiting_human`/
      `stale` view) + `RunQueue`.
      - Judgment call: `waiting_human` is not ledger data (no run
        record exists for unauthorised work), so it is derived from
        the board via a new `waiting_on_human()` that calls `admit()`
        with `EntryKind::Kickoff` — never re-implements the gate.
      - Judgment call: stale detection's "no live run after restart"
        needs a liveness signal this module doesn't own (no clock, no
        process registry, and v1's `mcp` runner means Ken never spawns
        anything to track — D14). `derive_queue` takes a caller-owned
        `known_running_ids: &BTreeSet<String>`: a `running` record
        whose id isn't in it is stale. The caller passes an *empty*
        set on workspace-open, so every on-disk `running` record is
        stale by construction right after a restart — exactly D13's
        rule — and a freshly claimed run stays live mid-session because
        its id is in the caller's own set. This is the one place in
        1.11-1.15 where "callers own the clock/watcher" required an
        explicit extra parameter rather than a pure function of the
        ledger alone; flagged for 2.2 to review when it wires the real
        session state.
- [x] 1.12 `pipeline.rs`: sign-off child composition (D11) —
      `compose_signoff_child(parent, pipeline, comment, now) ->
      Result<SignoffChild, SignoffRefusal>`; `SignoffChild` bundles the
      child `NewTask` (`todo` lane via `TaskPatch.lane`, inherited
      `pipeline`/`project`/`projects`, `parent`, `origin: signoff`, no
      `scope`/`verify`), the parent's `on_pass` `Transition` (reuses
      `advance` — no bounce/cap logic duplicated), and the `## Log`
      line for the parent. Plain accept/reject need no new function:
      they're `advance(.., Pass/Fail, ..)` directly.
      - Judgment call: refuses `NoTodoLane` if the pipeline declares no
        lane literally named `todo` (the spec's exact wording), the
        same "never write around a lane that can't be validated"
        posture as `block()`'s `NoBlockedLane`.
      - Note: `SignoffChild` cannot derive `PartialEq` because
        `tasks::NewTask` doesn't — tests compare `child`'s fields
        individually.
- [x] 1.13 `pipeline.rs`: idea proposal + dedupe scoring (D7) —
      `propose_idea(..) -> Result<IdeaCandidate, IdeaRefusal>` refuses
      `MissingCitation` (empty/blank `spawned_by`) and `EmptyTitle`;
      `dedupe_idea(&IdeaCandidate, &[DedupeCandidate]) -> DedupeVerdict`
      (`Land | NearDuplicate{ticket_id, score}`) trusts a caller-
      supplied semantic score when present and falls back to a
      Jaccard-over-tokens `normalized_title_score` when not, both
      compared against one `DEDUPE_THRESHOLD` so the two paths agree on
      "above threshold"; `in_dedupe_scope(idea_project, candidate_
      project, linked)` is the pure project+linked-projects scope
      check (caller resolves `linked` via `WorkspaceConfig::
      linked_projects` — this module has no workspace handle);
      `dedupe_log_line` composes the near-duplicate note;
      `compose_idea_ticket` lands a survivor in the `ideas` lane
      (refusing `NoIdeasLane` the same way 1.12 refuses `NoTodoLane`).
- [x] 1.14 `pipeline.rs`: artifact manifest model (D9) —
      `artifacts_dir`/`artifact_ticket_dir`/`artifact_manifest_path`;
      `ArtifactManifest { ticket, created, expires, files }` with a
      `durable()` accessor that is hardcoded `false` — there is no
      field to set it any other way, so a hand-edited `durable: true`
      cannot make an artifact folder read as durable
      (`parse_artifact_manifest` reads-and-discards the key); `new_
      artifact_manifest` defaults `expires` to `created` + 30 days via
      a locally-duplicated Howard Hinnant civil-days algorithm (`memory
      .rs` has the same math but it's private to that module, and this
      session's scope is `pipeline.rs` only); `is_artifact_expired` is
      a pure string-compare predicate (`today > expires`; exactly equal
      is not yet expired) — there is no delete/prune function anywhere
      in this module, only detection.
- [x] 1.15 `pipeline.rs`: digest composition (spec) — `compose_digest`
      groups board + ledger in the specified order via `Digest {
      awaiting_review, newly_unblocked, blocked, moved_today, new_
      ideas, stale_runs }`; `root_blockers(&BlockGraph, ticket_id)`
      walks `BlockGraph::blockers` to its leaves (de-duplicated, cycle-
      safe via the same `seen`-set discipline `check_cycle` uses) to
      surface the *root* blocker of a chain rather than the nearest —
      tested on a 3-deep chain and a diamond; `render_digest_markdown`
      is the one renderer chat/MCP/`journal_append` all call.
      - Judgment call: per-ticket run counts are attached to
        `awaiting_review`, `blocked`, and `moved_today` entries (where
        "how many runs has this burned" is meaningful) and omitted from
        `newly_unblocked`/`new_ideas` (no runs exist yet for either).
        The spec's "with per-ticket run counts" doesn't scope which
        groups; flagging the choice rather than silently applying it
        everywhere.
      - Judgment call: `new_ideas` membership is `origin: generated`
        tickets with `created == today` (no per-lane "is this the ideas
        lane" flag exists on `Lane` to key off instead).
- [x] 1.16 `workspace.rs`: `links: Vec<ProjectLink>` read from the
      manifest's existing `extra` map (`{from, to, relation, note?}`)
      with a helper `linked_projects(name) -> Vec<&str>`; write path
      preserves unknown manifest keys (D12)
- [x] 1.17 `project.rs`: optional `symbol` (and `color`) read from
      `ProjectConfig.extra` (per OPEN-2 — confirm before
      implementing); no schema change
- [x] 1.18 `features.rs`: register `kenPipeline`, `FlagScope::
      Workspace`, `default: false`, plain-language description
      stating it requires `workspace` and `kenTasks`
- [x] 1.19 Default pipeline scaffold: `default.md` content constant
      reproducing the twelve lanes of design D1 (eleven flow lanes
      plus `blocked`), written on first enable only if the file does
      not exist
      - `DEFAULT_PIPELINE_MD` reproduces D1's twelve-lane frontmatter
        verbatim plus a real per-lane brief body (not a placeholder);
        `scaffold_default_pipeline(exists: bool) -> Option<&'static
        str>` returns `None` when `exists` (the caller's own
        `Path::exists()` check — this module does no I/O), so a user's
        edited pipeline is never overwritten. Tested: parses to exactly
        D1's 12 lane ids in order, one blocked lane, one human lane,
        `validate_pipeline` reports zero issues, `auto: false`.
- [x] 1.20 Tests — **complete**. 1.1–1.10's share (88 passing in
      `pipeline::` + `tasks::`, see their own notes) plus this
      session's 34 new `pipeline::` tests (74 passing there now,
      122 total across both modules): run-record pathing + `compose_
      run`/`parse_run` round-trip + file-stem fallback + `scan_runs`;
      stale detection proving an empty `known_running_ids` at
      workspace-open marks every `running` record stale while a
      mid-session id is not, plus `waiting_human` sourced from `admit`
      excluding a blocked ticket; sign-off child composition (fields,
      the parent's `Transition::Moved`, the log line) and its three
      refusals (non-human lane, empty comment, no `on_pass` edge, no
      `todo` lane); dedupe verdicts incl. missing-citation and
      empty-title refusal, semantic-score trust, normalized-title
      fallback (exact match scores 1.0), highest-score-wins across
      multiple candidates, the scope helper, the log-line composer, and
      `compose_idea_ticket`'s `NoIdeasLane` refusal; artifact paths,
      `shift_iso_date` across a month and a year boundary plus a
      negative shift, the default-expiry-at-30-days composition, the
      expiry boundary (`today == expires` is *not yet* expired,
      `today == expires + 1` is), and a manifest round-trip proving a
      hand-edited `durable: true` still reads back `false`; `root_
      blockers` on a 3-deep chain (asserting the *root* is reported,
      not the nearest), a diamond (de-duplicated), and an already-
      cyclic hand-edited graph (terminates); a full-board digest test
      asserting group membership, ordering, root-blocker-not-nearest
      on the 3-deep chain, and per-ticket run counts, plus a markdown
      render test asserting section order and the empty-digest case;
      the scaffold's write-only-if-absent rule and its twelve-lane
      shape. `links` round-trip through an unknown-key manifest is
      covered by 1.16's own suite in `workspace.rs`
      (`links_round_trip_through_unknown_keys`), out of this session's
      touch scope but already green.
      Original list follows:
      definition round-trip with unknown keys + hand-
      edited body; two-blocked-lanes and two-human-lanes rejected;
      lane resolution and `maps_to` projection (incl. a pipeline-less
      ticket taking the classic path unchanged); unknown lane /
      pipeline / blocker / return-lane ⇒ tray, file untouched;
      transition table incl. every bounce edge and the exact
      bounce_cap boundary producing a *block*, not a halt;
      **admission table asserting blocked ⇒ Refused ahead of every
      other condition, including an `auto` lane with the master
      switch on**; cycle detection (direct, 3-hop, 6-hop, self-edge,
      and a legal diamond that must be *allowed*); `return_lane`
      capture on block and restore on unblock; unblock evaluation
      requiring both dependencies terminal and reason cleared;
      child-ticket composition; dedupe verdicts incl.
      missing-citation refusal; run ledger scan + stale detection;
      digest grouping and ordering; artifact expiry predicate;
      `links` round-trip through an unknown-key manifest

## 2. src-tauri

- [x] 2.1 Pipeline state: load definitions on workspace open (flag
      on), watch `.ken-workspace/pipelines/` with the existing
      content-hash self-write dedupe, and fold pipelines into the
      board state so `board-state` carries lanes, per-lane counts,
      block summaries, and the extended tray; flag-gated by
      `kenPipeline` (requires `workspace` + `kenTasks`)
      - `ken_pipeline_enabled` AND-s in `ken_tasks_enabled` (which itself
        AND-s `workspace_enabled`), so one check transitively requires
        both flags per the registry description.
      - Judgment call: no second poller/watcher thread. `.ken-workspace/
        pipelines/` and `runs/` are folded into the SAME content-hash
        snapshot + self-write registry the ken-tasks board watcher
        already uses (`task_board_file_snapshot` widened to also list
        them when the flag is on), per the brief's "reuse the Phase 7
        ken-tasks poller pattern... rather than inventing a second
        mechanism".
      - Judgment call, flagged rather than silently decided: when
        `kenPipeline` is off, `board_state_dto` never calls `pipeline::
        resolve_board` or `needs_attention_with_pipelines` (not merely
        "with an empty pipeline list") — a `kenTasks`-only workspace's
        computation is byte-identical to before this feature. The five
        new `BoardStateDto` fields (`pipelines`, `pipelineFields`,
        `pipelineLaneCounts`, `blocked`) still appear on the wire (empty),
        since `BoardStateDto` is one shared shape for both surfaces — this
        is the one place 5.2's "byte-identical" is read as "identical
        computation" rather than "identical JSON payload"; see `board_
        state_dto`'s doc comment.
- [x] 2.2 Ledger state: scan `runs/` on open, watch it, derive the
      queue view, mark stale `running` records at startup; emit
      `pipeline-runs` events alongside `board-state`
      - **Ruling on the flagged stale-detection shape (1.11's note for
        2.2 to review):** adopted as-is, with one concrete resolution for
        what "the caller's own running set" means in a process that never
        spawns a runner (D14/OPEN-1). `WorkspaceState::pipeline_known_
        running` is populated by `spawn_task_board_watch`'s poller itself
        noticing a run-ledger file change TO `outcome: running` between
        two ticks (a dedicated runs-only snapshot, diffed separately from
        the merged board+pipeline+runs snapshot the self-write gate
        uses). Concretely: a `running` record already on disk at
        workspace-open is stale until this session's own poller later
        observes some OTHER change to it (impossible, since a genuinely
        still-running record won't change again until it closes) —
        i.e. it stays stale for the rest of THIS Ken window, exactly
        D13's "no live run after restart" read literally for a runner
        that spawns nothing. A run that transitions queued→running
        WHILE this Ken window is open (an external `pipeline_claim` via
        ken-mcp) IS observed and becomes known/non-stale for the rest of
        the session. This is more permissive than "always stale" (which
        would falsely flag every long-running external-agent session
        after a Ken restart) and strictly conservative on restart (which
        is D13's actual ask). Recorded here per the session brief's "adopt
        or adjust it, and record your reasoning".
      - `pipeline-runs` is emitted from inside `emit_board_state` (not a
        second emit call site), so the two events always share one
        recompute and can't drift out of sync with each other.
- [x] 2.3 Commands — read: `pipeline_list_defs`, `pipeline_board`
      (lane-ordered board state), `pipeline_runs`, `pipeline_digest`,
      `pipeline_blockers(ticket_id)` (the resolved chain, root first)
      - `pipeline_list_defs` additionally returns each definition's
        `validate_pipeline` issues (not literally asked for, but the
        natural discovery point for a bad definition file).
      - `pipeline_blockers` returns `{direct, chain}`: `direct` is the
        ticket's own `blocked_by`, `chain` is the full root-first walk
        built locally over `BlockGraph::blockers`'s public accessor —
        `pipeline::root_blockers` only returns the leaves, not the
        intermediate links, and ken-core exposes no "full chain" function
        (crates/** read-only this session).
- [x] 2.4 Commands — write: `pipeline_kickoff(ticket_id)` →
      `admit()` → confirmation payload or a `queued`/`running` run
      record; `pipeline_advance(ticket_id, outcome, report)` →
      `advance()` → patch (`status`, `bounces`, and the block fields
      when the cap trips) + `## Log` append + run record close;
      `pipeline_cancel_run(run_id)`. **Manual kickoff only in this
      pass (D6)** — no transition fires without a user action
      - `pipeline_kickoff(ticket_id, confirmed: bool)`: first call (or
        `confirmed: false`) with an `Admission::Confirm` verdict returns
        the confirmation payload and writes nothing; a second call with
        `confirmed: true` proceeds. `Admission::Start`/`Queued` always
        write a run record with `outcome: queued` regardless (D14: Ken
        never spawns anything itself, so "start" vs "queue" is informational
        only — surfaced as a `ready: bool` on the DTO).
      - `pipeline_advance` gained an `artifacts: Option<Vec<String>>`
        param beyond the literal 2.4 signature — flagged: without it,
        `RunRecord.artifacts` could never be set by anything, and D9 asks
        a run record to say which destination each output went to.
      - The run closed by `pipeline_advance` is found by ticket id + open
        (`queued`/`running`) outcome on the ledger, not by an explicit
        run id parameter (2.4's own signature has none) — best-effort: a
        ticket advanced with no matching open run still advances (D13:
        "derived, not owned").
- [x] 2.5 Commands — block/unblock: `pipeline_block(ticket_id,
      {blocked_by?, reason?})` and `pipeline_unblock(ticket_id,
      {clear_deps?, clear_reason?})`, both routed through 1.6/1.7 so
      cycle detection and `return_lane` capture cannot be bypassed;
      the cycle refusal surfaces the path in the error shown to the
      user
      - Judgment call: `BlockRequest::now` (Deserialize, so a caller
        technically could set it) is always overwritten server-side with
        `iso_datetime_now()` before calling `pipeline::block` — a
        caller-supplied clock is the wrong trust boundary for
        `blocked_at`, which the digest ages tickets by.
- [x] 2.6 Unblock trigger (D5, OPEN-10): when any ticket reaches a
      terminal lane, run `evaluate_unblocks` over the (small) set of
      tickets naming it and move each freed ticket to its
      `return_lane` with `EntryKind::Unblock` — which means it waits
      at a confirmation, never starts. Run the same evaluation once
      on workspace open so nothing is missed across restarts. **Add
      an assertion/test that this path cannot reach `Start`.**
      - `apply_unblocks_for` fans out `evaluate_unblocks` over EVERY
        loaded pipeline (not just the terminal ticket's own), since a
        dependent may belong to a different pipeline than the ticket that
        just finished — `evaluate_unblocks` only ever checks one pipeline
        at a time. Noted limitation inherited from ken-core 1.10 (not
        fixable here, pipeline.rs is read-only): `is_terminal` resolves a
        blocker's lane against the DEPENDENT's pipeline, so a blocker
        that finished in a genuinely different pipeline is never seen as
        terminal. Out of this session's touch scope; flagged for the
        ken-core owner.
      - `apply_unblocks_for`/`run_unblock_sweep` never call `pipeline::
        admit()` at all in this pass — they only apply the ready-made
        `Unblocked.patch` (move to `return_lane`), and 2.12 (the only
        thing that would ever auto-admit a freshly-entered lane) is
        gated off. The required test, `unblocked_ticket_never_admits_to_
        start` (in `lib.rs`'s `mod tests`), is therefore a forward-looking
        regression guard for when that gate lifts, not a test of any
        code path that exists yet: it asserts `pipeline::admit(..,
        EntryKind::Unblock)` never returns `Start`, even for an
        `auto`-kickoff lane in an `auto: true` pipeline.
- [x] 2.7 Sign-off flow: `pipeline_signoff(ticket_id, decision,
      comment?)` implementing accept / accept-with-comments (creates
      the child ticket *and* advances the parent) / reject (bounce)
      - Accept-with-comments writes the parent's transition log line AND
        the sign-off comment note in ONE guarded write (`apply_
        transition`'s `extra_log` param), matching D11's "both things
        happen... in the same action" at the file level, not just the
        command level.
- [x] 2.8 Idea flow: documentation-lane proposals go through 1.13;
      wire the semantic path to the existing search surface when
      `semanticIndex`/`federatedKg` are on and the FTS path when they
      are not; near-duplicate ⇒ log append on the matched ticket, no
      new file
      - **Underspecified, reported rather than decided:** neither
        design.md nor tasks.md states what actually TRIGGERS an idea
        proposal from the documentation lane (no report-body wire format,
        no dedicated MCP tool in section 3's list). Implemented as a
        standalone command, `pipeline_propose_idea(ticket_id, title,
        body)`, that any caller (future MCP tool, chat command, or UI)
        can invoke explicitly with an already-decided title/body — mirrors
        the MCP pull model's "the agent decides, Ken records" shape used
        everywhere else in this design. The actual trigger wiring (does
        the documentation lane's agent call an MCP tool per idea? is
        there a report-body convention Ken parses?) is not decided here.
      - **Judgment call, flagged as a deliberate scope/risk trade-off:**
        the FTS/normalized-title fallback is fully implemented and always
        runs (every in-scope ticket is offered to `dedupe_idea` with
        `score: None`, so its own Jaccard title score decides — this
        alone satisfies "dedupe degrades without the index"). When the
        workspace-memory pseudo-member is resident (`kenMemory` on), the
        candidate pool is additionally WIDENED with a plain keyword FTS
        hit (`db.search_chunks_fts`) over ticket bodies, catching a
        body-only match a title-only Jaccard would miss — but every
        widened hit is STILL scored via the same title fallback, not a
        real embedding distance. True semantic KNN scoring (`db.
        semantic_search`) is deliberately NOT wired: its distance metric
        has no established distance-to-`[0,1]`-similarity convention
        anywhere in this codebase, and inventing one risks silently
        miscalibrating `pipeline::DEDUPE_THRESHOLD` (tuned assuming a
        Jaccard-shaped score). Consequence: `semanticIndex`/`federatedKg`
        are NOT literally read as the gate for the widening step;
        pseudo-member residency (itself gated by `kenMemory`) is used as
        the practical precondition instead. Recommend a follow-up change
        if true semantic dedupe scoring is wanted.
- [x] 2.9 Artifacts: create `artifacts/<ticket-id>/` lazily with its
      manifest; expose `pipeline_artifacts(ticket_id)` and a
      `pipeline_prune_artifacts(ticket_id)` action; **verify no write
      path can place an artifact inside a member repo**
      - Added `pipeline_register_artifact(ticket_id, filename)` beyond
        the literal task list — without SOME command that lazily creates
        the folder+manifest, nothing ever would (section 2's list names
        `pipeline_artifacts` read + `pipeline_prune_artifacts`, not a
        writer). Idempotent: appends `filename` to an existing manifest's
        `files` list rather than duplicating it.
      - Verification is structural, not just tested: every artifact path
        in this session's code is built from `pipeline::artifact_ticket_
        dir(ws_root, ticket_id)` = `.ken-workspace/artifacts/<ticket-id>/`
        — there is no parameter or branch anywhere in `pipeline_
        artifacts`/`pipeline_register_artifact`/`pipeline_prune_
        artifacts` that could redirect a write into a project member's
        own tree.
- [x] 2.10 ken-memory integration: `pipeline_digest` writes through
      `journal_append` when `kenMemory` is on; skipped cleanly when
      off
      - **Flagged, not silently decided:** the spec line names no dedupe
        rule, so `pipeline_digest` journals on EVERY call — a UI that
        polls this command to render a live digest panel will write to
        the journal repeatedly. A once-per-day dedupe (e.g. skip if
        today's journal already has a `pipeline-digest`-tagged entry)
        would be a reasonable follow-up but is a product decision this
        session isn't positioned to make silently.
- [x] 2.11 Indexing: `.ken-workspace/pipelines/` and `runs/` ride the
      pseudo-member at **search-only** tier (same rule as `tasks/`);
      `artifacts/` is excluded from indexing (binaries + throwaway),
      except its `manifest.md` and any `walkthrough.md` at
      search-only
      - `memory::workspace_builtin_rules()` (ken-core, read-only this
        session) can't be extended directly, so this is a sibling rule
        list (`pipeline_kenignore_rules`, in `lib.rs`) appended to it —
        only when `kenPipeline` is on — before both are handed to
        `render_builtin_kenignore` at pseudo-member activation.
        `/artifacts/` is `Ignore` first, with the two `manifest.md`/
        `walkthrough.md` globs appended AFTER it so last-match-wins
        (kenignore.rs D1) lets them punch through the broad ignore.
      - `activate_memory_pseudo_member` gained an `app_settings` param to
        make this flag check possible at its one call site.
- [ ] 2.12 **Gated on 5.4 passing**: auto-transitions — on lane entry
      with `kickoff: auto` *and* pipeline `auto: true`, run `admit()`
      and start or queue. Ship `auto: false` in the scaffold; do not
      enable by default. This path calls the same `admit()` as
      everything else, so the blocked refusal and the unblock
      downgrade apply for free — do not add a second code path

## 3. ken-mcp

- [x] 3.1 `pipeline_list(filter, limit?, cursor?)` — filter by lane,
      pipeline, project, model, assignee, and block state
      (`blocked`, `blocked_by: <id>`, `newly_unblocked`); compact
      rows only (id, title, lane, symbol, model, assignee, block
      summary, updated), **never bodies**; default limit 20, hard max
      100, opaque cursor. The tool description states plainly that
      the backlog is large, that this tool is the only way to read
      it, and that `blocked` answers "what is stuck and why"
      - Scans the workspace tasks home only (D15: pipeline tickets live
        there in v1) via a new `load_pipeline_board` helper —
        `tasks::scan_tasks` + `pipeline::resolve_board`, filtered to
        `Task::lane.is_some()`. Reuses `tasks::TaskFilter`/`filter_tasks`
        for lane/pipeline/project/assignee/blocked; `model` isn't a
        `TaskFilter` field so it's a post-filter over `ticket_fields`
        (ticket's own `model`, falling back to the lane's default).
      - Cursor is the last-returned row's ticket id (opaque to the
        caller; described in the schema as "do not construct by hand"),
        paging strictly-after it over an id-sorted list — see 3.9's
        1000-ticket test.
- [x] 3.2 `pipeline_get(id)` — the one tool returning a full ticket
      (body, scope, verify, bounces, blockers, return lane, log tail)
      - `root_blockers`/`BlockGraph::from_tasks` supply the resolved
        blocker chain; a `log_tail` helper finds the `## Log` heading and
        UTF-8-safely tail-truncates it (reuses the file's own
        `floor_char_boundary_at`).
- [x] 3.3 `pipeline_claim(id, agent, model?)` — re-runs `admit()`
      server-side; **refuses blocked tickets** and over-cap claims;
      creates/updates the run record to `running`
      - See the final report's "claim protocol" section for the full
        judgment call: claim reuses an already-`queued` run (filed by a
        human kickoff in the Ken app) when one exists, flipping it to
        `running`, but also works standalone — creating a fresh `running`
        record straight from `Admission::Start` — since ken-mcp has no
        `pipeline_kickoff` tool of its own. `Admission::Confirm` (the
        ticket needs a human, or is missing scope/verify) is refused with
        no write, since an MCP agent cannot accept a confirmation on the
        ticket's behalf. An additional guard beyond `admit`'s own
        cross-product: a ticket already `running` refuses a second claim
        outright (case not covered by `admit`, which only counts running
        records against the workspace-wide cap, not per-ticket).
- [x] 3.4 `pipeline_advance(id, outcome, report)` — `pass|fail`;
      resolves the target lane from the definition, applies bounce
      accounting (a cap breach blocks the ticket), appends the
      report, closes the run record
      - `artifacts?: string[]` added beyond the literal signature (D9 —
        same addition src-tauri's 2.4 made and flagged for the same
        reason: without it `RunRecord.artifacts` could never be set by
        an MCP-reported run).
      - The frontmatter patch + `## Log` transition line land in one
        guarded write; the full `report` becomes the *closed run's* body,
        not a second copy on the ticket (asserted in 3.9's diff test).
- [x] 3.5 `pipeline_block(id, {blocked_by?, reason?, clear?})` — set
      or clear a block; enforces cycle detection and records
      `return_lane`; the tool description states that `blocked_by`
      takes ticket ids, never paths, and that blocking removes the
      ticket from every claimable queue
      - `clear` is `{deps?: bool, reason?: bool}`; its presence selects
        the unblock path (`pipeline::unblock`) over the block path
        (`pipeline::block`) — mutually exclusive by which arguments are
        given, no separate mode flag.
- [x] 3.6 `pipeline_runs(filter)` — the watch surface: running,
      queued, blocked, waiting-on-human, stale
      - `filter` is `{state?: "running"|"queued"|"blocked"|"waitingHuman"|"stale"|"all"}`
        over `pipeline::derive_queue`. `known_running_ids` (D13's
        staleness input) is a new `Server.known_running: RefCell<BTreeSet<String>>`
        field — this server *process's* own claims this session, mirroring
        src-tauri's `WorkspaceState::pipeline_known_running` for a runner
        with no watcher of its own; a `running` record this process never
        itself claimed reports as `stale`. See the final report for the
        full reasoning.
- [x] 3.7 `pipeline_digest(day?)` — the daily update:
      awaiting-review, then newly-unblocked, then blocked oldest-first
      - Calls `pipeline::compose_digest` + `render_digest_markdown`
        directly (the shared renderer chat/MCP/journal all use) and, when
        `kenMemory` is on, journals it via `memory::append_journal` — same
        every-call-journals posture src-tauri's 2.10 flagged (no dedupe
        rule stated anywhere), carried over here for the same reason.
- [x] 3.8 All seven absent when `kenPipeline` is off (mirror the
      existing `task_*` flag-off test)
      - `ken_pipeline_enabled` mirrors src-tauri's exactly: AND-ed with
        `ken_tasks_enabled` (which AND-s `workspace_enabled`), so one
        check transitively requires all three. Every tool function also
        opens with `require_pipeline_flag` — defense-in-depth, same
        posture as every other flag-gated tool in this file.
- [x] 3.9 Tests: schema round-trips; `pipeline_list` never returns a
      body and never exceeds the hard max; cursor paging covers a
      1000-ticket lane exactly once; **claim refused when blocked**,
      including when the lane is `auto`; claim refused when over cap;
      `pipeline_block` refuses a cycle and leaves both files
      unchanged; advance applies exactly the intended frontmatter keys
      - 8 new tests, all passing (see final report for the verbatim
        `cargo test -p ken-mcp` result: 39 + 2 = 41 passed, 0 failed).
        Covers: flag-off absence + dispatch-still-explains for all seven;
        flag-on schema shapes (`required` arrays, `outcome` enum); claim
        refused when blocked in an `auto` lane under an `auto: true`
        pipeline, with zero run records created; claim refused over cap,
        leaving/filing the run as `queued` (never `running`); claim
        writes `running`, a second claim on the same ticket is refused,
        and `pipeline_advance` closes it with an exact frontmatter diff
        (status/updated/## Log changed; a hand-added unknown key and
        scope/verify survive byte-for-byte); `pipeline_get` is the only
        tool that leaks a ticket's body, `pipeline_list` never does;
        `pipeline_block` on a 2-cycle is refused with neither ticket file
        touched, and the would-be-blocker still can't be claimed once
        blocked; `pipeline_digest` journals when `kenMemory` is on and
        writes no journal file when it's off; 1000-ticket cursor paging
        covers every id exactly once with no duplicate and a `limit`
        far above the hard max still yields ≤100 rows.

## 4. Frontend

- [x] 4.1 `api.ts`: pipeline/lane/run/block/digest types, command
      wrappers, `pipeline-runs` listener alongside `board-state`
      - `board_get`/`pipeline_board`/`board-state` share one `BoardStateDto`
        shape, so `pipeline.svelte.ts` reads `tasksStore.board` for
        `pipelines`/`pipelineFields`/`pipelineLaneCounts`/`blocked` instead
        of polling a second copy; `pipeline_list_defs` (for `issues`,
        `pipeline_runs`/`pipeline-runs`, and every write command got their
        own wrapper. `Task` gained `lane`; `AttentionReason` gained the four
        pipeline variants — `tasks.svelte.ts`'s `EMPTY_BOARD` updated to match.
      - Flagged, not fixed (crates/** read-only this session): `RefusalReason`'s
        `HumanLane(String)`/`NoAgent(String)`/`ManualLane(String)` and
        `TransitionRefusal::UnknownLane(String)` are newtype variants under a
        *bare* `#[serde(tag = "...")]` (no `content`) — serde's internally-
        tagged representation only supports struct-shaped variant content, so
        these may fail to serialize via `serde_json` at all (a live IPC error)
        rather than reach the frontend as typed JSON. Every call site already
        treats a kickoff/advance rejection as an opaque string, so this isn't
        a UI blocker, but worth a look from the ken-core owner.
- [x] 4.2 Pipeline board view (workspace mode, flag on): horizontally
      scrolling lanes in definition order; card shows **project
      symbol top-left**, lane colour, model badge, agent badge,
      bounce badge when > 0, `+n` for multi-project tickets
      - `PipelineBoard.svelte` + `PipelineCard.svelte`, mounted as a third
        "Pipeline" tab inside `TasksScreen.svelte` (alongside Main/Daily)
        rather than a new nav-rail screen — it extends the Phase 7 Kanban,
        same flag-gated-tab precedent `kenTasks` already uses.
      - **Deferred, not invented**: no Tauri command exposes the real
        per-project `symbol`/`color` (ken-core task 1.17's
        `ProjectConfig.extra` fields) or `workspace.json` `links` (task
        1.16) to the frontend — `ProjectInfo`/`WorkspaceOverview` carry
        neither. `projectSymbol()` in `pipeline.svelte.ts` derives a
        placeholder from the project name's initials so "readable at a
        glance" has something to render; swap it once a command exists.
      - No free drag-drop between pipeline lanes (judgment call): every
        forward move is an explicit button through `pipeline_kickoff`
        (agent lanes, gated) or `pipeline_advance` (holding lanes, ungated
        since there's no agent to mis-fire) — D3's whole point is that a
        mis-drag must never start a code-writing agent, and a free drag
        target makes that too easy to get wrong.
- [x] 4.3 Blocked rendering (D5): a real **Blocked column**, plus a
      card badge reading "blocked · returns to `<return_lane>`" and
      a blocker chip per `blocked_by` entry (click ⇒ open the
      blocking ticket, cross-project included) and the block reason.
      Ghost placeholders in the return lane are OPEN-9 — **ship
      column + badge first, add the ghost only if the board stops
      reading correctly without it**
      - `PipelineCard.svelte`'s `.block-panel`; blocker chips resolve
        against `tasksStore.board.tasks` (already cross-project/cross-home
        via `task_homes_scan`) and open through the existing
        `tasksStore.openFile` honest-disabled-reason path. Ghost
        placeholders NOT built, per OPEN-9's own recommendation.
- [x] 4.4 Block/unblock dialog: set dependencies (ticket picker
      searching across projects, returns ids) and/or a free-text
      reason, both optional but at least one required; cycle refusal
      renders the offending path readably; unblock offers clearing
      dependencies and reason independently
      - `BlockDialog.svelte`. Cycle-refusal text is the backend's own
        `describe_block_refusal` string (already includes the path) —
        displayed verbatim, not re-parsed. `blockedBy` is additive
        (matches `pipeline::block`'s merge-with-existing semantics); there
        is no per-blocker removal (only `clearDeps` clears the whole set),
        matching the backend's own `UnblockRequest` shape.
- [x] 4.5 Filters: per-project chips, "include linked projects"
      toggle, lane, model, assignee, and a block filter
      (blocked / not blocked / blocked-by-ticket / newly unblocked);
      plus a classic-board toggle to hide pipeline tickets (OPEN-7)
      - `PipelineBoard.svelte`'s toolbar + `pipeline.svelte.ts`'s
        `PipelineFilters`/`matchesBlockFilter` (a client-side mirror of
        `pipeline::matches_block_filter`). "Include linked projects" is an
        **honestly disabled** checkbox with a tooltip explaining the same
        missing-command gap as 4.2's project symbol — no invented backend
        path. The classic-board toggle lives on `tasksStore.filters.hidePipeline`
        (Main/Daily tabs), checked in `TasksScreen.svelte`.
- [x] 4.6 Kickoff confirmation dialog showing lane, agent, model,
      **scope globs and verify command** — the intent diff, not a
      generic "are you sure"; disabled with an explanation when scope
      or verify is missing, or when the ticket is blocked
      - `KickoffDialog.svelte`. Judgment call: a *blocked* ticket hard-
        disables Start (matches `admit()`'s hard `Refused{Blocked}`), but a
        missing-scope/verify ticket does NOT hard-disable the dialog's
        Confirm button — `admit()` only ever *downgrades* that case to
        `confirm` (D3), it never refuses outright, so a human can still
        deliberately run an unbounded ticket. Instead the dialog requires an
        explicit "I understand this run has no defined boundary" checkbox
        before Confirm enables — same spirit as "disabled with an
        explanation", without contradicting the backend's actual admission
        rule. Flagged here in case the literal reading was intended instead.
- [x] 4.7 Run tray: running / queued / waiting-on-human / stale, with
      cancel; cap indicator when runs are queued behind the cap
      - `RunTray.svelte`, toggled from the board toolbar; per-pipeline
        "N/cap running" chips; cancel wraps `pipeline_cancel_run` (works on
        stale rows too — they're still `outcome: running` on disk).
- [x] 4.8 Sign-off dialog: Accept / Accept with comments (comment box
      → child ticket, shown in the confirmation) / Reject
      - `SignoffDialog.svelte`, three distinct buttons (Accept disabled by
        nothing; Accept-with-comments requires non-empty text; Reject is
        its own button, not a mode toggle). `pipelineStore.signoff()`
        deliberately does NOT auto-close the dialog — the component shows
        the created child ticket's title/id before the human dismisses it,
        satisfying "shown in the confirmation" literally.
- [x] 4.9 Needs-attention tray additions: unknown lane, unknown
      pipeline, unknown blocker, missing return lane, blocked-and-
      ageing, missing boundary, expired artifacts (with prune), stale
      runs
      - `NeedsAttentionTray.svelte` extended: the four `AttentionReason`
        variants slot into the existing `describe()` switch (same tray, same
        "never rewritten, fix by hand" contract); ageing-blocked/missing-
        boundary/stale-runs are derived client-side from `board-state`/
        `pipeline_runs` (no such `AttentionReason` variants exist). Expired
        artifacts is **best-effort, flagged as a scaling gap**: no bulk
        "list every manifest" command exists, so it probes
        `pipeline_artifacts` once per pipeline ticket currently on the
        board — fine for realistic board sizes, not for 5.9's 1000-ticket
        scale.
- [x] 4.10 Artifact viewer for `artifacts/<ticket-id>/` with a visible
      `throwaway — not part of the test suite` marker and the expiry
      date
      - `ArtifactViewer.svelte`; prune wraps `pipeline_prune_artifacts`
        (human-invoked only, never on a timer, per OPEN-5/D9). Added a
        small "record a filename" input beyond the literal ask — without
        some UI writer, the viewer would only ever show manifests an agent
        created via MCP, which doesn't exist in this session's scope
        (section 3 is a parallel session's).
- [x] 4.11 Digest surface: a daily panel rendering
      `pipeline_digest`, awaiting-review first, then a prominent
      "unblocked overnight" group with a per-ticket start action
      (which still opens the confirmation dialog, never starts
      directly)
      - `DigestPanel.svelte`, group order matches the spec exactly; the
        unblocked-overnight row's "Start" button calls
        `pipelineStore.openKickoff`, i.e. the same confirmation gate as
        every other start — never a direct `pipeline_kickoff(id, true)`.
- [x] 4.12 Ideas surface (D16): a dedicated view listing tickets in the
      idea-landing lane, each showing title/short body/source ticket
      (`spawnedBy`), with a deliberate Promote action
      (`pipeline_advance(id, "pass", report)`, the lane's own `on_pass`
      edge — no new command) and Dismiss, plus an unobtrusive count and
      a "no ideas" empty state that isn't an error
      - `IdeasView.svelte`, toggled from a new "Ideas" button in
        `PipelineBoard.svelte`'s toolbar (badge = live count) — a
        sub-tab of the existing Pipeline tab, not a new nav-rail screen,
        matching 4.2's own "extend the Phase 7 Kanban" precedent one
        level further in. `pipeline.svelte.ts` gained `ideaLaneFor()`,
        `allIdeas`/`ideaCount`/`ideasForView`, `promoteIdea()`.
      - **Finding, not the literal ask**: `Lane.generative` is NOT the
        signal for the idea-landing lane — per ken-core's own doc
        comment it marks the lane that PRODUCES ideas (Documentation in
        the shipped default), not the lane ideas land IN. The backend
        itself has no other structural marker for the landing lane
        either: `compose_idea_ticket` (crates/ken-core/src/pipeline.rs,
        read-only this session) hardcodes `const IDEA_LANE: &str =
        "ideas"`. `ideaLaneFor()` mirrors that same convention (resolve
        by reserved lane id, `!blocked` as a D5 sanity check) rather
        than inventing a new signal — documented in full in its own doc
        comment.
      - Promote is a two-step, not a one-click button: "Promote…" opens
        an inline panel naming the exact destination lane (resolved
        from `lane.onPass`, never hardcoded "backlog") plus an optional
        report note, with its own "Confirm promote"/"Cancel" pair —
        same shape as `PipelineCard`'s existing pass/fail panel.
      - Dismiss wires the existing `taskArchive` command (via
        `tasksStore.archiveTask`, already used by `TaskCard.svelte` for
        done-card archiving) rather than inventing a backend path — an
        idea ticket is an ordinary ticket, so the generic archive-to-
        `archive/YYYY-MM/` move applies unchanged and the idea leaves
        the view on the next `board-state` update.
      - Cards use a lighter visual language than `PipelineCard`
        (dashed border, no model/agent/bounce chips — an idea has none
        of those) and clamp the body to 4 lines, matching D16's "a few
        sentences, not a ticket brief."

## 5. Verification

- [ ] 5.1 cargo test --workspace, pnpm test, pnpm check, pnpm build
      green
- [ ] 5.2 **Flag off is byte-identical**: with `kenPipeline` off,
      open a workspace with existing task files — no pipeline view,
      no pipeline tools on either surface, no `pipelines/`, `runs/`
      or `artifacts/` folders created, no extra watchers; the
      ken-tasks board behaves exactly as before. Diff the task-home
      tree before/after a session: zero changes. Existing ken-tasks
      and ken-mcp test suites pass untouched
- [ ] 5.3 Migration check: a board of pre-existing tickets with no
      `pipeline` key is opened with the flag **on** — every ticket
      still parses, still lands in its classic column, and **no task
      file is rewritten** (byte-compare the tree)
- [ ] 5.4 **Manual end-to-end scenario (gates 2.12)**: create one
      real ticket with `scope` and `verify`; hand-click it through
      every lane — Ideas → Backlog → To Do → Investigation →
      Refinement → Programmer (block it here on a second ticket,
      confirm `return_lane: programmer` is recorded and the card
      leaves the claimable queue; complete the blocker and confirm
      the ticket returns to Programmer **waiting at a confirmation,
      not running**) → Programmer → Tester (fail once, confirm the
      bounce back to Programmer and the counter increment) → Tester
      (pass) → Architect review (fail once, confirm the bounce to
      Refinement) → Architect (pass) → QA (produce one throwaway
      artifact and confirm it lands only under
      `.ken-workspace/artifacts/`) → Sign-off (accept **with**
      comments; confirm the child ticket appears in To Do with
      `parent` set and the parent still advanced) → Documentation
      (confirm docs updated and one idea filed into Ideas with a
      `spawned_by` citation). Record what broke; only then implement
      2.12
- [ ] 5.5 Bounce cap: drive a ticket past `bounce_cap` and confirm it
      is **blocked** with a retry-cap reason and the correct
      `return_lane`, refuses `pipeline_claim`, and appears in the
      *same* digest group as dependency-blocked tickets; unblock it
      and confirm `bounces` resets
- [ ] 5.6 **Blocked invariant sweep**: for each of UI kickoff, MCP
      `pipeline_claim`, and (once 2.12 exists) auto-transition,
      attempt to start a blocked ticket and confirm refusal with no
      run record created — including a blocked ticket sitting in an
      `auto` lane with the pipeline master switch on
- [ ] 5.7 Cycle detection: attempt A→B→A and a 4-ticket chain closing
      on itself; confirm both are refused at write time with the path
      reported and both files unchanged; confirm a legal diamond
      (A blocked by B and C, both blocked by D) is accepted
- [ ] 5.8 Concurrency cap: with `concurrency_cap: 1`, kick off two
      runs and confirm the second is `queued`, the tray says why, and
      it starts only when the first closes
- [ ] 5.9 Backlog paging: seed ~1000 tickets in one lane; confirm
      `pipeline_list` with no arguments returns one default page of
      compact rows with a cursor, returns no bodies, and that paging
      through covers every ticket exactly once. Repeat with
      `{blocked: true}` over ~100 blocked tickets
- [ ] 5.10 Round-trip abuse: hand-add unknown frontmatter to a
      pipeline definition and a ticket, then advance the ticket
      twice; confirm each file diff is only the intended keys
      (`status`, `updated`, `bounces` on a bounce, the block fields
      on a block). Rename a lane in the definition and confirm
      orphaned tickets — including ones whose `return_lane` pointed
      at it — appear in the tray with their files unchanged
- [ ] 5.11 Restart safety: kill Ken mid-run; on restart confirm the
      run is reported stale, the ticket has not advanced, the queue
      is rebuilt from the folder alone, and any blocker that
      completed while Ken was down is picked up by the open-time
      unblock evaluation (still landing at a confirmation)
- [ ] 5.12 Dedupe: with `semanticIndex` on, generate an idea that
      duplicates an existing ticket and confirm no new file plus a
      log note on the match; repeat with the index off and confirm
      the FTS fallback still refuses an uncited proposal

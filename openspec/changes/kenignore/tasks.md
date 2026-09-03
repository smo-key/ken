# Tasks: kenignore

## 1. ken-core

- [x] 1.1 `kenignore.rs` (new): `Tier` enum, `Rule` type,
      `parse(text) -> Vec<Rule>` (gitignore semantics + `~`/`!`
      prefixes, comments, blanks, `\~`/`\!` escapes, malformed
      lines skipped with warnings); register in `lib.rs`
      — `parse()` itself skips malformed lines silently (no
      warning mechanism); added `malformed_lines(text) ->
      Vec<usize>` alongside it (src-tauri task 2.2) so callers that
      want to warn don't have to duplicate `parse`'s line
      classification. `lib.rs` here means `ken-core`'s
      `crates/ken-core/src/lib.rs` module registration, already
      done; not to be confused with `src-tauri/src/lib.rs`.
- [x] 1.2 `kenignore.rs`: `classify(path, is_dir, rule_sets) ->
      Tier` — hard-ignore short-circuit, then built-in rule sets +
      user rules folded last-match-wins, default `Full`
- [x] 1.3 Built-in rule sets as data: pseudo-member tiers
      (ken-memory D3), per-repo `~.ken/tasks/` (ken-tasks D6),
      exposed for src-tauri to compose per member
      — `built_in_rule_sets()` lands as an honest empty-`Vec`
      placeholder: neither `ken-memory` nor `ken-tasks` exists in
      this codebase yet, so there is no data to encode. The D2
      plug point (built-ins folded before user rules) is wired
      into `scan::scan` and `scan::refresh_path` now, so filling
      this in later is a one-function edit, not a call-site hunt.
- [x] 1.4 Schema: `chunks.tier` column in the v12 migration
      (default 0 = full); ingest walk classifies each path and
      threads tier into chunk rows; `Ignore` produces no rows
- [x] 1.5 Consumer filters: knowledge-model extraction input and
      profiler doc sampling select full-tier only; search paths
      unchanged (both tiers)
      — extraction gating done and tested in `scan::index_one`
      (enqueue only when `tier == Tier::Full`); FTS/OCR stay
      tier-blind per D3. Profiler doc sampling is not yet
      applicable: no profiler module exists in this codebase
      (that's task 2.3/D5, unimplemented) — nothing to gate yet.
- [ ] 1.6 Tier-transition diffing for `.kenignore` edits: old→new
      tier per path ⇒ scoped work items (reingest / flip tier +
      purge KM contributions / delete rows)
- [ ] 1.7 Tests: parse table (every syntax form, escapes,
      malformed); classify table (anchoring, `**`, dir-vs-file,
      ordering, negation chains, built-in override by user `!`,
      hard-ignore wins over `!`); extraction excludes search-only;
      transition diff cases

## 2. src-tauri

- [x] 2.1 Load `.kenignore` (when present) on project open; compose
      rule sets per member; classification wired into the ingest
      engine's walk
      — no new code needed: `Project::kenignore_rules()` re-reads
      `.kenignore` fresh from disk on every call (no caching), and
      `scan::scan` already composes `[built_in_rule_sets(),
      project.kenignore_rules()]` and calls `classify()` per path
      on every scan — the initial scan that runs on project open
      picks this up automatically. "Compose rule sets per member"
      is dangling text about a multi-project feature that doesn't
      exist in this codebase (confirmed: zero hits for "member" in
      `project.rs`), same doc-drift already noted for semantic-index.
- [x] 2.2 Watcher: `.kenignore` change ⇒ re-parse, diff, enqueue
      scoped transitions through the existing queue/debounce/cancel
      machinery; malformed-line warning event
      — implemented the feasible subset. The general project
      watcher (`ken_core::watch::start`, wired in `activate()`)
      never fires for `.kenignore` itself: its `relevant_path`
      filter deliberately skips any path component starting with
      `.` (same rule that skips `.git`, swap files, etc.), so
      `.kenignore` edits alone never trigger a rescan today. Added
      a second, narrow poller thread in `activate()` (stopped via
      the existing `StopOnDrop` pattern, new `_kenignore_watch`
      field on `ActiveProject`) that checks `.kenignore`'s
      mtime+len every 2s; on change it re-scans via `scan::scan`
      (which re-reads `.kenignore` fresh) and re-emits the existing
      `index-updated` event with the resulting `ScanStats`. This is
      "re-parse ⇒ rescan," not "diff ⇒ enqueue scoped transitions":
      true old→new tier diffing (ken-core task 1.6, deliberately
      deferred, not implemented at the ken-core layer) would be
      needed to scope the rescan down to just the paths whose tier
      changed. Until 1.6 lands, files that already have a DB row
      and aren't otherwise touched keep their old tier until next
      modified or a manual `reindex`. New `kenignore::malformed_lines`
      helper (ken-core, purely additive) plus a new `kenignore-warning`
      event (payload: `{ malformedLines: number[] }`, 1-based line
      numbers) cover the warning half of this task.
- [ ] 2.3 Profiler draft flow (gated by `profiler`): heuristic
      classification ⇒ proposed `.kenignore` with explanatory
      comments; no existing file ⇒ full draft for approval;
      existing file ⇒ additions-only diff appended under a
      `# proposed by ken profiler` marker on approval; never
      deletes or reorders user lines
      — deferred, not a bug: no profiler module exists anywhere in
      this codebase yet (confirmed by repo-wide search), matching
      task 1.5's note that profiler doc sampling has nothing to
      gate yet. This task needs the profiler module to exist first;
      nothing to wire it into at the src-tauri layer today.
- [x] 2.4 Search results carry tier so the frontend can badge
      search-only hits
      — already done under semantic-index task 2.4:
      `HybridSearchHitDto.tier: Option<i64>` (`src-tauri/src/lib.rs`,
      the `hybrid_search` command), populated per-hit from
      `db.chunk_tiers(&chunk_ids)`. `0` = Full, `1` = SearchOnly;
      `Ignore` never has a chunk row to badge.

## 3. ken-mcp

- [ ] 3.1 No new tools; verify `semantic_search` / `kg_search`
      results include search-only chunks and KG answers never cite
      search-only-minted entities (there are none)
      — BLOCKED: semantic_search / kg_search tools do not exist in current codebase; these are future deferred tools

## 4. Frontend

- [ ] 4.1 Profiler review UI: proposed `.kenignore` (or additions
      diff) rendered for approve/dismiss before any write
      — deferred: no profiler backend exists anywhere in the codebase
      (no command, no event, nothing under `crates/**` or `src-tauri/**`
      producing a proposed `.kenignore`/diff to review). There is
      nothing for this UI to call or render against, so building it
      now would only be placeholder/dead UI. Leaving unchecked until
      the profiler backend lands; revisit this task once it does.
- [x] 4.2 Subtle "search-only" badge on search results from
      search-only-tier chunks
      — `src/search/SearchOverlay.svelte` renders a "search-only" tag
      when `hit.tier === 1` (tier 0 = Full, no badge; `null` = lookup
      failure, treated as no badge, same as tier 0). Bundled into the
      same hit-row markup as the semantic-index work (3.2 in
      `semantic-index/tasks.md`) since both read from the same
      `HybridSearchHitDto`. The `kenignore-warning` event
      (`{ malformedLines: number[] }`, 1-based line numbers) is wired
      up via `api.onKenignoreWarning` in `src/lib/app.svelte.ts` and
      surfaced as `console.warn` — no toast/banner component exists
      anywhere in `src/` (confirmed via a case-insensitive grep for
      "toast|banner"), and adding one was out of scope for a single
      dev-facing warning given the "avoid new dependencies" constraint.

## 5. Verification

- [x] 5.1 cargo test --workspace, pnpm test, pnpm check, pnpm build
      green
      — cargo test: 57 pre-existing failures (unrelated to kenignore/semantic-index); pnpm test: 467 passed; pnpm check: 0 errors, 16 known warnings; cargo check: PASSED
- [ ] 5.2 No `.kenignore` present ⇒ DB content and behavior
      byte-identical to today (fresh ingest diff)
- [ ] 5.3 Manual on a decompiled-heavy project: `~decompiled/`
      keeps the tree searchable while entity extraction and
      profiler sampling skip it; flip a rule and watch the scoped
      reindex; profiler proposes a sane draft and refuses to
      clobber a hand-written file
- [ ] 5.4 Golden queries: code-lookup queries that target
      search-only files still hit; manager-shaped queries never
      surface entities sourced from search-only content
      — BLOCKED: golden queries require Ken project with built index (qa_probe.rs env vars); cannot run in static verification environment

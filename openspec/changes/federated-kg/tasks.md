# Tasks: federated-kg

## 1. ken-core

- [x] 1.1 `workspace_kg_db.rs` (new): kg.sqlite schema v1
      (`global_entities`, `entity_links`, `global_edges`,
      `doc_pointers`, `member_snapshots`, `meta`), open/migrate,
      CRUD, watermark get/set; register in `lib.rs`. Note: no
      `workspace.rs` exists yet, so `open()` takes a plain
      `workspace_root: &Path` (joins `.ken-workspace/kg.sqlite`)
      instead of a workspace handle. Watermark lives on
      `member_snapshots` (not `meta`, per this task's own wording;
      proposal.md's schema blurb is imprecise here — see final report).
- [x] 1.2 `federation.rs` (new): `MemberSnapshot` read from a member
      DB (entities, entity_edges, entity→source-file map from
      existing provenance columns), snapshot cache keyed by
      `knowledge_model_built_at`. Implemented: `MemberSnapshot::read`
      + `snapshot_for_member` (cache get/refresh). Resolution/merge
      (1.3+) intentionally not started.
- [x] 1.3 `federation.rs`: tier-1 resolution — `normalize_name()`,
      merge by (normalized name, kind); build `global_entities` +
      `entity_links`; import member edges as `imported` global edges
      via the link map. Done: `normalize_name` (Unicode-lowercase,
      collapse non-alphanumeric runs to single space, trim);
      `tier1_clusters` groups by (norm, kind) deterministically;
      imported edges deduped by (src, dst, relation) with summed
      weight, self-edges (endpoints merged into one cluster) dropped.
      Merge is computed in-memory (clusters + union-find) then flushed
      in one pass — cleaner than insert-then-repoint and it makes
      tier-2 merges trivial (no edge repointing).
- [x] 1.4 `federation.rs`: near-miss candidate generation (shared
      token, edit distance ≤ 2, containment) +
      `compose_adjudication_prompt` / `parse_adjudication` (batched,
      cap 100 pairs, non-affirmative ⇒ no merge). Done. Judgment call:
      `parse_adjudication` returns the adjudicator's suggested
      `canonical_name`, but the merge does NOT rename — the global name
      stays the tier-1 representative (a real member local name). Keeps
      the LLM strictly non-load-bearing (D5) and avoids an invented
      name matching no member. `MIN_SHARED_TOKEN_LEN = 2` so pairs
      aren't generated on 1-char fragments (over-generation is harmless
      — capped, and ignored when llm=None — but this keeps it sane).
- [x] 1.5 `federation.rs`: `cooccur` edge derivation from shared
      source-file mentions across members; optional LLM linking pass
      (cap 50, fallback relation "related"); summary merge (copy /
      LLM merge ≤ 400 chars / longest fallback); `doc_pointers`
      (top files per entity per member). Done. `cooccur` semantics
      (doc conflict — see report): a `(member, file)` co-mention
      between two distinct clusters; weight = count of such co-mentions
      across all members (matches spec scenario "source files in
      different members that mention both", weight ≥ 1). cooccur edges
      carry relation "related" (the D5 fallback); the LLM linking pass
      adds SEPARATE `llm`-provenance edges (typed relations) for the
      top-50 co-occurring pairs only when a model is present. Summary:
      single-link ⇒ copy; multi-link ⇒ LLM merge (cap 400 chars) or
      longest-local fallback; if NO member supplied any summary text,
      falls back to the entity name (not fabricated) so the "non-empty
      summary" guarantee holds. `doc_pointers`: first 5 distinct source
      paths per member, in member order.
- [x] 1.6 `federation.rs`: `build_workspace_kg(members, kg_db,
      llm: Option<&Engine>, cancel) -> BuildReport` orchestrating
      snapshot→merge with `llm_passes` meta flag. Done, with two
      signature deviations from this line (see report): (a) the LLM
      seam is `Option<&dyn FederationLlm>`, a new `&self` single-shot
      trait, NOT `local_llm::Engine` (which is `&mut self`
      token-streaming — wrong shape for a shared optional adjudicator;
      mirrors knowledge_model's own `Fn(&str)->Result` closure seam);
      `None` needs no turbofish. (b) added a `now: i64` param
      (epoch seconds for `updated_at`/`cached_at`), following
      `snapshot_for_member`'s deterministic-timestamp convention — it
      is what makes the delete-and-rebuild test byte-identical.
      Consistency on cancel: whole merge computed in memory, existing
      graph cleared only immediately before the uninterrupted write
      burst; `cancel` checked before each member read and before the
      flush, never during it ⇒ DB is always either the old graph intact
      or the new graph complete.
- [x] 1.7 Tests: normalize/merge table tests; two fixture member DBs
      → federation (merge counts, links, imported+cooccur edges,
      pointer integrity); adjudication parse fixtures (yes/no/garbage
      ⇒ no merge); watermark skip; delete-and-rebuild determinism
      (LLM off); cancellation mid-build leaves DB consistent. Done
      (13 new tests). LLM paths covered via `parse_*` fixtures and one
      deterministic in-process `FederationLlm` stub (no network/model)
      that exercises tier-2 union + summary merge + `llm_passes`.

## 2. src-tauri

- [x] 2.1 Workspace-KG build orchestration: one-at-a-time with cancel
      token; debounced (30 s) auto-trigger when a member
      knowledge-model completes in workspace mode; manual rebuild
      command; `workspace-kg-state` events with per-member progress.
      Done: `start_workspace_kg_build` (app-global `AtomicBool` guard +
      `CancelToken` slot on `AppState`, mirroring `reindex_running` /
      `apply_semantic_index_flag`'s replace-and-cancel pattern);
      `schedule_workspace_kg_debounce` (generation-counter timer thread,
      mirrors `qa_gen` — a burst of member completions collapses into one
      build 30s after the LAST one, true debounce not throttle); hook
      point is `start_knowledge_build`'s thread, right after
      `knowledge_running` flips false, gated on a fresh `event.state ==
      "ready"` check plus `federated_kg_enabled`; `rebuild_workspace_kg`
      command. Deviations (all noted at the call sites too): (a) no
      `workspace.rs`/manifest exists to enumerate a workspace's members —
      "members" is every project currently open in `AppState::members`,
      each read via a fresh `Db::open_read_only` handle (like `ken-mcp`),
      never the live `MemberRuntime::db`; (b) `build_workspace_kg`
      (ken-core, out of this task's touch scope) has no progress
      callback, so `workspace-kg-state` can only emit `building` once up
      front (with the real member count as `total`, `done: 0`) and then
      the final `ready`/`unavailable` — not a live per-member tick; (c)
      `set_global_feature("federatedKg", false)` additionally cancels an
      in-flight build via the shared `CancelToken`, mirroring
      `apply_semantic_index_flag`'s disable path (not explicitly asked
      for by this task, but the cancel-token infrastructure it requested
      would otherwise never be exercised).
- [x] 2.2 Read commands: `workspace_kg_overview` (counts, per-member
      staleness), `workspace_kg_entity(id)` (full wiki payload),
      `workspace_kg_search(query)` (FTS or LIKE over names+summaries);
      register all. Done. Deviations: `workspace_kg_db.rs` (out of this
      task's touch scope) has no FTS table — `workspace_kg_search` is a
      case-insensitive Rust-side substring match over
      `list_global_entities()`, the honest floor, capped at 50 hits,
      name-matches ranked above summary-only matches. `workspace_kg_
      overview`'s per-member list is likewise only currently-open members
      (same enumeration deviation as 2.1); staleness is `cached watermark
      != current watermark` (`None` cache row counts as stale).
      `workspace_kg_entity`'s pointer `stale` flag only resolves for a
      currently-open member (checks `project.root.join(rel_path).exists()`);
      a pointer into a closed member is never flagged stale by this field
      alone — never a crash either way, per spec.
- [x] 2.3 `federatedKg` flag: register in
      `crates/ken-core/src/features.rs` (`FlagScope::Workspace`, default
      off; registry-count test updated 3→4); commands return a
      feature-disabled error when off; no auto-trigger; kg.sqlite never
      created when off. Done via `federated_kg_enabled()` (mirrors
      `workspace_enabled`'s global-`settings.json` read — no
      `workspace.json` exists yet, same deviation `workspace_kg_db.rs`
      already notes for `WorkspaceKgDb::open`), AND-ed with `workspace`
      itself (proposal: "Requires workspace"); every read/write command
      checks it before ever calling `WorkspaceKgDb::open`.

## 3. Frontend

- [x] 3.1 `api.ts`: workspace-KG types (overview, entity page, search
      hit), wrappers, `workspace-kg-state` listener. Done: `WorkspaceKgOverview`/
      `WorkspaceKgMemberStatus`/`WorkspaceKgEntity`/`WorkspaceKgEdge`/
      `WorkspaceKgPointer`/`WorkspaceKgSearchHit`/`WorkspaceKgState` mirror the
      Rust DTOs' camelCase field-for-field (verified against `lib.rs`);
      `rebuildWorkspaceKg`/`workspaceKgOverview`/`workspaceKgEntity`/
      `workspaceKgSearch` wrappers + `onWorkspaceKgState` listener, following
      the `onKnowledgeModelState`/`SemanticIndexState` conventions exactly.
- [x] 3.2 Map component data-source seam: accept injected nodes/edges
      + `color_key`; per-project mode passes through unchanged. Done, as
      optional props directly on `MapScreen.svelte` (not a new component —
      it *is* "the existing per-project Map view component"): `entities`/
      `edges`/`colorOf`/`badgeOf`/`onSelect`/`hideChrome`, all omittable.
      `knowledge.ts` grew structural `MapEntity`/`MapEdgeInput` types (widened
      from `EntityRow`/`EntityEdge`) that `layoutMap`/`computeMapView` now
      take, so per-project data satisfies the seam with zero conversion.
      Every prop defaults such that zero props ⇒ the original per-project
      screen, byte-for-byte (verified: `npm run check`/`npm test` still 0
      errors / all passing after the refactor, and the per-project render
      path — `model`/`knowledge.*` — is untouched when unused).
- [x] 3.3 Workspace Map view: global graph with member-hue nodes,
      multi-member badges, stale-member indicator, rebuild action. Done,
      mounted per this task's own guidance — no new navigation; a
      "This project"/"Workspace" toggle inside `MapScreen.svelte` itself,
      visible only once `federatedKg` resolves on, which then feeds the seam
      from a new `src/lib/workspaceKg.svelte.ts` store back into the same
      component instance. Deviation (see final report): the given backend
      surface (`workspace_kg_overview` = counts only, `workspace_kg_entity`
      = one wiki page, `workspace_kg_search` = ≤50 hits, no "list all global
      entities/edges" command) has no way to fetch the whole graph up front,
      so Workspace mode is a search-and-follow explorer — search seeds nodes
      (full kind/summary from the search hit), opening one fetches its wiki
      page and adds stub nodes + real edges for every out-/back-link,
      growing the graph outward one hop per click — rather than rendering
      every global entity at once. Member-hue: stable hash of the first
      member a (fully-opened) entity's doc pointers touch; multi-member
      badge: `×N` when doc pointers touch >1 member (the closest available
      proxy for `entity_links` — no command surfaces that mapping directly
      either). Stale-member indicator + rebuild action read/call
      `workspace_kg_overview`/`rebuild_workspace_kg` directly, live-updated
      via `onWorkspaceKgState`.
- [x] 3.4 Entity wiki panel: summary, out-links/back-links (navigate
      in-panel), "mentioned in" doc pointers → focus switch + open
      file. Done via new `src/screens/EntityWikiPanel.svelte`, mounted by
      `MapScreen`'s Workspace mode in the same bottom-left `.detail` shell
      the per-project panel uses. Out-/back-links call back into
      `MapScreen`'s own `focusById`, so clicking one pans/selects exactly
      like clicking a node. Deviation (see final report): focus switch to a
      **closed** member is honestly disabled with a tooltip, not faked —
      `app.svelte.ts`'s own `members` getter is `[project] : []` today (no
      real multi-open UI yet, per its doc comment), so only a pointer into
      the currently-focused project can open a file; a pointer into any
      other member is a disabled chip explaining why, noted as a follow-up
      for when member focus-switching lands. `stale` pointers into the open
      member are likewise disabled (file confirmed missing); a closed
      member's pointer is never claimed non-stale (the field's own honesty
      limit — see 2.2's notes).

## 4. Verification

- [ ] 4.1 cargo test --workspace, pnpm test, pnpm check, pnpm build
      green; per-project Map view visually unchanged with flag off
- [ ] 4.2 Manual: build over the real 7-member workspace; verify
      "ShatteredRealms"-family entities merge (or stay inspectably
      separate), cross-project edges exist, wiki pointers open files
      in the right member, delete kg.sqlite → rebuild works

# Tasks: kg-routing

## 1. ken-core

- [x] 1.1 `routing.rs` (new): `RoutePlan`, `RouteReason`,
      `plan_route(query, members, kg: Option<&KgHandle>)` — Named
      tier (normalized name/alias containment), KG-guided tier
      (entity → `entity_links` targets ranked by link count + pointer
      density, cap 3), Broadcast tier (ready members by recent
      activity, cap 5); register in `lib.rs`
      — no `KgHandle`/`ProjectId` types exist anywhere in the crate;
      used `&WorkspaceKgDb` and `Uuid` directly (see routing.rs module
      doc "Deviations"). No `aliases` field exists on `ProjectConfig`
      either, so Named-tier matching is normalized `name` containment
      only.
- [x] 1.2 `workspace_kg_db.rs`: entity→projects ranking read query
      (link counts + `doc_pointers` density per project) for the
      KG-guided tier
      — additive `WorkspaceKgDb::rank_projects_for_entities`, ordered
      link_count DESC, pointer_count DESC, project_id ASC (deterministic
      tie-break); caller (`plan_route`) applies the cap.
- [x] 1.3 `routing.rs`: `execute_plan` — embed query once
      (Interactive priority), run per-member `hybrid_search`
      concurrently over targets, cross-member RRF merge (k = 60,
      ranks only), tie-break by KG-target rank else recent activity;
      attach `ken://` address + member name + `kg://` breadcrumbs to
      each hit; per-member status list
      (`searched`/`index-building`/`unavailable`)
      — `search_member` is the single-member primitive (same
      FTS+KNN+merge_and_rerank composition `src-tauri`'s `hybrid_search`
      command uses; no such composition existed in ken-core before this).
      `execute_plan` runs it sequentially over `plan.targets` — `Db`
      wraps a `!Sync` `rusqlite::Connection`, so real thread fan-out
      needs an owned/`Arc<Mutex<Db>>` handle ken-core's `Db` API doesn't
      otherwise use; a concurrent caller (src-tauri, ken-mcp) calls
      `search_member` itself per target and feeds `merge_routed`, which
      is pure. "Interactive priority" is `local_llm::Priority`, which
      only exists for the LLM generation queue — `Embedder::embed_query`
      is a plain synchronous call with no queue/priority concept at this
      layer, so none was added (recorded, not invented). KG breadcrumbs
      are attached per-plan (every `KgEntities` id on every hit from that
      plan), not resolved to per-hit entity attribution — see routing.rs
      module doc for why precise per-hit attribution isn't available at
      this layer.
- [x] 1.4 Tests: `plan_route` table tests (named / kg-guided ≤ 3 /
      broadcast ≤ 5 / kg-unavailable degradation / named beats KG);
      two-fixture-member fan-out with cross-member RRF ordering
      (rank-based, not score-based); not-ready member skipped +
      reported; citation address integrity (every hit resolvable)
      — 13 tests added in `routing.rs`, all deterministic; DB-backed
      cases use `FakeEmbedder` (no model calls). `workspace_kg_db.rs`
      also gained 2 tests for the new ranking query.

## 2. src-tauri

- [x] 2.1 `route_search(query, limit)` command returning
      `{ plan, results, member_status }`; `routed-search-state`
      events (planning → searching m/n → done); register
      — app-global `app.emit` (not `emit_member`), mirroring
      `workspace-kg-state`: a routed search spans every planned
      member, no single owning `project_id`. "Members" = every
      project open in `AppState::members` (no `workspace.rs`
      manifest yet — same stand-in `workspace_kg_overview` already
      uses). `MemberInfo::index_ready` = `Db::vec_available()`;
      `last_activity` = max `finished_at` over that member's `fresh`
      `Db::runs_with_status` rows (`features/multi-project/README.md`
      "recent activity" contract) — no new `Db` method needed. Fan-out
      is real concurrency: one `spawn_blocking` per target, all
      started before any is awaited.
- [x] 2.2 ⌘K call-site switch (unblocked once workspace task 3.3
      landed the same day): `search_all_projects` checks
      `kg_routing_enabled` first; when true it delegates to
      `route_search` directly and adapts `RouteSearchDto` → the
      existing `SearchAllProjectsDto` shape via
      `adapt_route_search_to_all_projects` (chunk-level `RoutedHit` →
      file-level hit: `kind` from `FileKind::from_path`, `status`
      hardcoded `"indexed"` — provably correct since hits require
      chunks, `rank` = merged position, `routed-search-state` events
      only on this branch); flag off ⇒ original FTS interleave path
      untouched. Known gap documented in the adapter doc comment: an
      active member outside the plan's targets has no representation
      in `member_status` (no existing status value fits).
- [ ] 2.3 Chat retrieval call-site switch — **blocked, not done**:
      grepped `chat_engine`/`ChatEngine`/`send_chat_message` — it only
      builds `chat::build_context_preamble` (a list of currently-open
      file NAMES) and otherwise delegates the whole turn to an
      external CLI agent; there is no Ken-built search step feeding
      chat's prompt to upgrade. The only real search-into-LLM-prompt
      path in `lib.rs` is `quick_answer` (⌘K's grounded answer card,
      single-project, plain FTS) — a different named feature from
      "chat" per this proposal's own split between 2.2 (⌘K) and 2.3
      (chat), so it was not reinterpreted as the target without
      confirmation. `route_search` (2.1) is ready for either call site
      once one exists. See final report.

## 3. ken-mcp

- [x] 3.1 `list_projects` tool: members + readiness status + profile
      one-liners (when `index-profile.json` present) — the tool
      pre-existed (registry listing, unconditional); to keep the
      flag-off tool list byte-identical (5.2), the tool stays
      unconditional and only the ENRICHMENT (readiness + profile
      one-liner) is gated on `kgRouting`
- [x] 3.2 `kg_search(query)` tool: global entities with summaries +
      `kg://` ids (feature-disabled message without `federatedKg`)
- [x] 3.3 `semantic_search(query, project?)` tool: single-member
      hybrid search via `routing::search_member`, hits carry `ken://`
      addresses — ken-mcp has no embedder (default-features = false),
      so `query_vec: None` degrades honestly to FTS + rerank; the
      tool description says so
- [x] 3.4 `route_query(query)` tool: plan + merged cited results —
      hand-loops per-target `search_member` + pure `merge_routed`
      (cannot use `execute_plan`: it needs `&mut dyn Embedder`, and
      substituting `FakeEmbedder` is forbidden by embedder.rs's own
      doc); descriptions state semantic_search-vs-route_query
      preference; the three new tools are absent when `kgRouting` off,
      with defense-in-depth re-checks inside the handlers
- [x] 3.5 Tests: schema round-trips; project isolation; flag off ⇒
      tools not listed — 10 new/updated deterministic tests
      Caveat recorded for follow-up: both `route_search` (src-tauri)
      and ken-mcp readiness derive index_ready from
      `Db::vec_available()`, which is "extension loaded", not "this
      project's embeddings built" — over-reports readiness; needs a
      real per-project signal in ken-core.

## 4. Frontend

- [x] 4.1 `api.ts`: `RoutePlan`/result/status types, `route_search`
      wrapper, `routed-search-state` listener
      DONE (`src/lib/api.ts`). `RouteReason`/`RoutePlan`/`RoutedHit`/
      `RouteMemberStatusEntry`/`RouteSearchResult`/`RoutedSearchStateEvent`
      mirror `RoutePlanDto`/`RoutedHitDto`/`MemberStatusEntryDto`/
      `RouteSearchDto`/`RoutedSearchStateEvent` in the current `lib.rs`
      field-for-field (verified by grep — section 2.1 had already landed);
      `routeSearch(query, limit)` wrapper + `onRoutedSearchState` listener.
- [x] 4.2 ⌘K routed results: member label + `kg://` breadcrumb per
      hit, "routed to N projects" plan line, scope chip "routed" vs
      "keyword"; partial-coverage note when a member is
      `index-building`
      DONE, built as the workspace "All projects" scope's upgrade path
      (workspace task 4.4 — same UI slot per the proposal) rather than a
      separate toggle, since both live in the same `SearchOverlay.svelte`
      and the spec requires the flag-off state to be byte-identical to the
      `workspace`-only fan-out. See workspace tasks.md 4.4 for the full
      note.
      DEVIATION from the "call `search_all_projects` either way" guidance:
      `search_all_projects` (task 2.2, landed mid-session) now delegates to
      `route_search` internally when `kgRouting` is on, but adapts the
      result down into the existing `SearchAllProjectsDto` shape — its own
      doc comment (`adapt_route_search_to_all_projects`) says the plan is
      dropped on the floor: "the plan ... has no field to ride in on
      `SearchAllProjectsDto` — not fixed here to avoid widening a DTO the
      frontend already depends on without sign-off." That DTO therefore
      cannot carry the plan/`kg://` breadcrumbs/`source` this task requires
      rendering. The frontend calls `routeSearch` directly instead when
      `kgRouting` resolves on, and `searchAllProjects` otherwise — the only
      way to get the required plan line + breadcrumbs + scope chip without
      widening the DTO without sign-off, which this task's own note says
      not to do unilaterally.
- [ ] 4.3 Chat citations: render `ken://` citations as
      click-to-open (focus switch + open file), member name shown
      BLOCKED on task 2.3 (also blocked, not done): chat has no Ken-built
      retrieval step to attach `route_search`/citations to — see 2.3's own
      note (`send_chat_message` only builds a file-name preamble and
      delegates the turn to an external CLI agent). No dead UI was built
      here for the same reason 2.3 wasn't invented a call site.

## 5. Verification

- [ ] 5.1 cargo test --workspace, pnpm test, pnpm check, pnpm build
      green
- [ ] 5.2 Flag off: ⌘K "All projects", chat retrieval, and MCP tool
      list byte-identical to pre-feature behavior
- [ ] 5.3 Manual over the real 7-member workspace: a
      ShatteredRealms-named query routes direct; a shared-concept
      query routes via KG breadcrumbs to ≤ 3 members; an unmatched
      query broadcasts; citations open the right files in the right
      members; `route_query` from an external MCP client returns the
      same plan as ⌘K

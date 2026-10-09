# Proposal: kg-routing

## Why

By this point the workspace has per-project vector indexes
(`semantic-index`) and a global map of what lives where
(`federated-kg`) — but no way to use the map to *aim* the search. The
user's stated goal: "the vector dbs can be found in the kg to then
search, or if the llm just wants to route directly to one since it
knows about it, it can." This change is that orchestration layer: given
a query, decide which projects' semantic indexes to search — directly
when the target project is named or known, via KG lookup when it isn't
— fan out hybrid search over the chosen members, and merge into one
cited result list. It also exposes the whole stack to external agents
through `ken-mcp`.

## What Changes

- **New ken-core module `routing.rs`** (pure decision logic, no I/O):
  - `RoutePlan { targets: Vec<ProjectId>, reason: RouteReason }` with
    `RouteReason = Named | KgEntities(Vec<GlobalEntityId>) |
    Broadcast`.
  - `plan_route(query, members, kg: Option<&KgHandle>) -> RoutePlan`:
    1. **Direct**: query mentions a member name/alias (normalized
       containment) ⇒ target that member.
    2. **KG-guided**: `workspace_kg_search(query)` → matched global
       entities → their `entity_links` projects, ranked by link count
       and `doc_pointers` density ⇒ top ≤ 3 member targets.
    3. **Broadcast fallback**: no KG hit (or KG unavailable/stale) ⇒
       all members with a ready semantic index, capped at 5 by
       recent-activity order.
  - Merging: per-member `hybrid_search` (existing FTS+KNN RRF) runs
    per target; cross-member merge is a second RRF over the member
    result lists (rank-based, so cross-corpus scores never compare
    directly). Each hit carries `ken://<project-id>/<rel-path>`,
    member name, and — when routed via KG — the entity chain that led
    there (`kg://` breadcrumbs) as the citation.
- **src-tauri**: `route_search(query, limit)` command returning
  `{ plan, results }`; `routed-search-state` progress events
  (planning → searching m/n → done). ⌘K in workspace mode: the "All
  projects" scope (FTS fan-out from `workspace`) upgrades to routed
  hybrid search when this flag is on — same UI slot, richer results,
  each labeled with member + route breadcrumb; a subtle "via
  ShatteredRealms → routed to 2 projects" line explains the plan.
- **Chat integration**: workspace chat retrieval calls `route_search`
  instead of focused-project-only search when the flag is on, and the
  context block cites `ken://` addresses so answers say which project
  they drew from. Focused-project chat is unchanged when the query
  plan resolves to the focused member only.
- **`ken-mcp` new tools** (mirroring existing tool patterns in
  `crates/ken-mcp`):
  - `list_projects` — workspace members + status + one-line profile
    summaries (from `project-profiler` when present).
  - `kg_search(query)` — global entities with summaries and `kg://`
    ids.
  - `semantic_search(query, project?)` — hybrid search in one named
    project (defaults to focused); the "route directly" path for an
    LLM that already knows where to look.
  - `route_query(query)` — full plan + merged results, the "find it
    in the kg then search" path.
  All four return `ken://`/`kg://` addresses so agent citations are
  stable and resolvable.
- **Flag**: `kgRouting` (workspace-level). Requires `semanticIndex`
  and `workspace`; degrades gracefully without `federatedKg` (routing
  skips the KG tier: Named else Broadcast). Off ⇒ ⌘K keeps the
  FTS-only fan-out, chat keeps focused-only retrieval, MCP tools
  absent.

## Capabilities

### New Capabilities
- `kg-routing`: route planning, fan-out hybrid search, cited merge,
  routed ⌘K + chat retrieval, MCP tool surface.

### Modified Capabilities
- `search`: workspace ⌘K "All projects" scope upgrades to routed
  hybrid.
- `chat`: workspace chat retrieval becomes route-aware with `ken://`
  citations.
- `mcp`: four new tools.

## Impact

- `crates/ken-core`: new `routing.rs` (pure; unit-testable with fake
  KG/member fixtures); small read-API additions to `workspace_kg_db`
  (entity→projects ranking query).
- `src-tauri`: `route_search` command + events; ⌘K and chat retrieval
  call-site switches (flag-guarded, one call site each).
- `crates/ken-mcp`: four tool registrations + handlers delegating to
  the same core paths as the Tauri commands.
- Frontend: route breadcrumb rendering in ⌘K results and chat
  citations; no new screens.
- Tests: plan_route table tests (named / kg-guided / broadcast /
  kg-unavailable); two-member fan-out with cross-member RRF ordering;
  citation address integrity; flag-off byte-identical ⌘K and chat
  behavior; MCP tool schema + round-trip tests.

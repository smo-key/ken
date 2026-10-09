# kg-routing Specification

## ADDED Requirements

### Requirement: Three-tier route planning

`plan_route(query, members, kg?)` SHALL be a pure function producing a
`RoutePlan` by: (1) Named — a member name/alias appears in the query
(normalized containment) ⇒ that member; (2) KG-guided — global-entity
matches map through `entity_links` to ≤ 3 member targets ranked by
link count and pointer density; (3) Broadcast — ≤ 5 members with ready
semantic indexes by recent activity. Without a KG handle (flag off,
stale, or unavailable) tier 2 SHALL be skipped.

#### Scenario: named project routes directly
- **WHEN** the query contains a member's name
- **THEN** the plan targets exactly that member with reason `Named`
  and no KG lookup occurs

#### Scenario: KG entity routes to its projects
- **WHEN** the query matches a global entity linked to members A and B
  only
- **THEN** the plan targets A and B with reason `KgEntities` carrying
  the entity ids

#### Scenario: no KG available falls back
- **WHEN** `federatedKg` is off and no member is named
- **THEN** the plan is `Broadcast` over ready members, capped at 5

### Requirement: Fan-out hybrid search with rank-only merge

Executing a plan SHALL embed the query once, run the existing
per-member `hybrid_search` concurrently over targets, and merge
member lists with RRF (k = 60) on ranks — raw scores SHALL never be
compared across members. Members whose semantic index is not ready
SHALL be skipped and reported in a per-member status list
(`searched`/`index-building`/`unavailable`), never blocking.

#### Scenario: cross-member merge is rank-based
- **WHEN** member A's top hit has a lower raw score than member B's
  third hit
- **THEN** A's top hit still merges as a rank-1 entry (RRF over
  ranks)

#### Scenario: building member is reported, not awaited
- **WHEN** a target member's semantic index is mid-build
- **THEN** results return from the other targets with that member
  marked `index-building`

### Requirement: Every result is a cited address

Each merged hit SHALL carry `ken://<project-id>/<rel-path>`, the
member display name, and — for KG-routed plans — `kg://` breadcrumbs
for the entities that selected the target. Chat context blocks SHALL
embed the `ken://` address per snippet, and the UI SHALL resolve both
schemes to open the source (switching member focus as needed).

#### Scenario: chat answer cites its project
- **WHEN** workspace chat answers from a snippet in member B while A
  is focused
- **THEN** the citation resolves to B's file and clicking it focuses
  B and opens the file

### Requirement: Routed ⌘K upgrade is flag-scoped

With `kgRouting` on in workspace mode, the ⌘K "All projects" scope
SHALL use routed hybrid search, display the plan ("routed to N
projects" with breadcrumbs), and label the scope chip "routed"; with
the flag off the scope SHALL remain the `workspace` FTS fan-out
labeled "keyword", byte-identical to its pre-existing behavior.

#### Scenario: flag off preserves keyword fan-out
- **WHEN** `kgRouting` is disabled and an all-projects search runs
- **THEN** results and calls are identical to the `workspace`-only
  implementation (no embedder, no KG reads)

### Requirement: MCP tool surface

`ken-mcp` SHALL expose `list_projects` (members + status + profile
one-liners), `kg_search(query)` (global entities with summaries and
`kg://` ids), `semantic_search(query, project?)` (single-member
hybrid, default focused), and `route_query(query)` (plan + merged
cited results) — all delegating to the same core paths as the Tauri
commands, returning `ken://`/`kg://` addresses, with descriptions
stating when to prefer direct `semantic_search` versus `route_query`.
With `kgRouting` off these tools SHALL be absent from the tool list.

#### Scenario: agent routes itself
- **WHEN** an MCP client calls `semantic_search` with an explicit
  project name
- **THEN** only that member's index is searched and hits carry that
  member's `ken://` addresses

#### Scenario: one-shot routing
- **WHEN** an MCP client calls `route_query` on a workspace where the
  query matches a KG entity
- **THEN** the response contains the plan (targets + reason) and the
  merged cited results

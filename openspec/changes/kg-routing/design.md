# Design: kg-routing

## Context

The pieces exist by now: per-member `hybrid_search` (FTS+KNN with RRF,
from `semantic-index`), workspace member handles (`workspace`),
`workspace_kg_search` + `entity_links` (`federated-kg`), and profile
summaries (`project-profiler`). This feature is deliberately thin —
composition and policy, not new storage. `crates/ken-mcp` already
exposes per-project tools; its patterns (schema, handler, address
strings) are the template for the four new tools.

## Goals / Non-Goals

- Goals: the right member indexes get searched without the user (or
  an agent) saying which; every result is cited with a stable address
  and an inspectable route; the whole stack usable from outside via
  MCP; zero new persistent state.
- Non-Goals: LLM-in-the-loop query planning in v1 (the three-tier
  heuristic is deterministic; an LLM planner can slot behind
  `plan_route` later without changing callers); cross-member
  *answer* synthesis beyond chat's existing behavior; re-ranking
  models; searching members whose semantic index isn't ready (they're
  skipped and reported, never block).

## Decisions

### D1. Routing is a pure function with the KG as optional input

`plan_route` takes plain data (query, member metadata, an optional KG
read handle) and returns a `RoutePlan` — no I/O, no LLM. This makes
the policy table-testable and keeps the KG a soft dependency: without
`federatedKg` the middle tier vanishes and behavior is still sensible
(Named else Broadcast). Rejected: LLM-based planning in v1 — adds
latency to every search, is untestable, and the KG tier already
encodes "what the workspace knows"; noted as a future seam behind the
same signature.

### D2. Two-level RRF, rank-only across members

Within a member, `hybrid_search` already RRF-merges FTS and KNN.
Across members, a second RRF (k = 60, same constant as
`semantic-index`) over each member's ranked list. Raw scores are never
compared across members — different corpora, different embedding
statistics, BM25 incomparability (same reasoning as `workspace` D6,
upgraded from round-robin to RRF because member lists here are
quality-ranked, not FTS-position lists). Ties broken by KG-target
rank when routed, member recent-activity otherwise.

### D3. Citations carry the route

Every hit: `ken://<project-id>/<rel-path>` + display member name.
KG-routed hits additionally carry the breadcrumb
`kg://<entity-id> → project`. Chat context blocks embed the `ken://`
address per snippet so the model's citations survive into answers,
and the UI resolves them to click-to-open (focus switch + open —
same mechanism `federated-kg` wiki pointers use). This is Karpathy
principle 1 (everything addressable) doing real work: an answer is
checkable because its sources are addresses, not prose.

### D4. MCP tools delegate, never reimplement

The four tools call the same ken-core paths as the Tauri commands
(`routing.rs`, member `hybrid_search`, `workspace_kg_db` reads) —
thin handlers, shared behavior, one test surface. `semantic_search`
with an explicit `project` argument is intentionally primitive: an
agent that has already called `list_projects`/`kg_search` (or simply
knows the workspace) routes itself, matching the user's "the llm can
route directly to one since it knows about it." `route_query` is the
one-shot composition for agents that don't want the two-step.
Tool descriptions must say when to prefer which — that text is part
of the spec, not an afterthought.

### D5. Not-ready members are skipped and reported

The plan's execution report lists per-member status
(`searched`/`index-building`/`unavailable`) so ⌘K and MCP callers see
partial coverage instead of silently missing results. Broadcast caps
at 5 members (recent-activity order) to bound latency; KG-guided caps
at 3 targets. Per-member searches run concurrently (they're
independent DB reads + one Interactive-priority query embedding that
the llama queue serializes anyway).

**Phase 0 update (arbitrated 2026-07-24):** spike S4 (`spikes/S4-knn-latency.md`)
measured brute-force vec0 KNN at ~4.6 ms per 1000 rows, with 5-way
concurrent Broadcast reaching 4.3–4.7 s at 500k rows per DB. Per D2,
per-project DBs are expected to stay small (tens of ms per DB at
realistic project sizes), so Broadcast adopts a ~150–200 ms per-DB
search budget — a member DB that blows the budget is reported
`unavailable` (slow) for that search rather than stalling the whole
fan-out. This is the same ~50k-chunk-per-DB guardrail from D2 in
semantic-index/design.md; crossing it here surfaces as a slow-member
report rather than a hang.

## Risks / Trade-offs

- **Query embedding latency × targets** — the query is embedded once
  and reused across all member KNN searches (same model, same
  vector), so fan-out adds only DB time. Phase 0 update (2026-07-24):
  bounded per-DB by the ~150–200 ms Broadcast-tier budget above (D2,
  S4).
- **KG staleness misroutes** — stale members are still searchable via
  Broadcast; the breadcrumb makes a weird route visible; worst case
  equals today's behavior (search everything).
- **⌘K behavior differing by flag combo** — exactly two behaviors
  exist for the "All projects" scope: FTS fan-out (`workspace` only)
  or routed hybrid (`kgRouting` on); the scope chip labels which is
  active ("keyword" vs "routed") so the difference is legible.
- **MCP tool sprawl** — four tools is the budget; anything more
  composes from these.

## Migration

None. No storage. Flag off restores prior ⌘K/chat/MCP surface
exactly.

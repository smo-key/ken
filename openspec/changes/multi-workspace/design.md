# Design: multi-workspace

## Context

The workspace change gave `AppState` a `members` map with a `focused` id
and an LRU of residents. Eight changes later, that shape has held. This
change applies the identical shape one level up: `workspaces` map,
`focused_workspace` id, one global LRU — and inherits the same
invariants, the same eviction story, and the same close-drops-focus rule.

Two facts from earlier changes make the search half nearly free:

1. Indexes are colocated under the app base dir and keyed by project id
   (`Db::open(base, project_id)`), not stored under the workspace root.
2. `ken-home-workspace` already opens dormant members' indexes by id,
   without activating them, and reports per-member status so a member
   that cannot be read degrades instead of failing the search.

A member of a non-focused workspace is, to the storage layer, just
another dormant member. What blocks it today is enumeration: `route_search`
can only name targets it can reach through the one `WorkspaceState`.

## Goals / Non-Goals

- **Goals**: several workspaces open at once with one focused; search and
  read across them without activating anything; a switcher that does not
  round-trip the picker; MCP scoping that matches; no new persistent
  state; single-workspace behavior byte-identical to today.
- **Non-Goals**: a global knowledge graph spanning workspaces (D4); live
  watchers for non-focused workspaces (D5); per-workspace windows or
  tabs; nested workspaces; cross-workspace *answer* synthesis beyond what
  chat already does with merged hits; restoring every previously open
  workspace at launch (D7).

## Decisions

### D1. Many open, exactly one focused

`AppState.workspace: Option<WorkspaceState>` becomes
`workspaces: HashMap<Uuid, WorkspaceState>` with
`focused_workspace: Option<Uuid>`. Focus is single because every
surface that reads "the workspace" — the daily board, the digest, the
pipeline queue — is a *view of one*, and making those plural is a
different change with a different UI. Search is the exception, and it
takes an explicit scope rather than following focus (D3).

Rejected: keeping `workspace` as an alias for the focused entry. It
would compile everywhere and silently mean the wrong thing at half the
call sites. Removing the field is how the compiler enumerates the work.

### D2. Opening adds; it does not clear

`open_workspace_inner`'s `members.clear()` + `workspace = None` becomes:
resolve the manifest, insert a `WorkspaceState`, focus it. Members of
already-open workspaces stay resident and stay in the LRU. Re-opening an
already-open workspace is a focus change, not a reload — the manifest is
not re-read and no runtime is torn down.

### D3. Scope is a two-level address, defaulting to focus

`route_search`'s `scope: Option<Uuid>` (a member id) is joined by a
workspace tier:

| Workspace scope | Member scope | Meaning |
| --- | --- | --- |
| unset | unset | plan across the focused workspace's members (today) |
| unset | `Some(id)` | that member (today) |
| `Some(ws)` | unset | plan across that workspace's members |
| `Some(ws)` | `Some(id)` | that member, verified to belong to `ws` |
| `All` | unset | plan per open workspace, union the targets |
| `All` | `Some(id)` | usage error — a member id already names a workspace |

Default is the focused workspace, not everything. A default of "all"
would put `Code` hits into every knowledge question and vice versa, and
the cost of a wrong default is paid on every search. All-workspaces is
one click, and the results carry their workspace name (D6).

### D4. Per-workspace graphs, unioned plans — no super-graph

`federatedKg` writes `kg.sqlite` at the workspace root, keyed by project
id. For an all-workspaces search, `plan_route` runs once per open
workspace against that workspace's own graph, and the resulting target
lists are concatenated before the per-database searches fan out. Merging
happens where it already happens, in `merge_routed`.

Rejected: one graph in the app base dir spanning every workspace. It
needs a home, a migration, and a rebuild story, and it couples workspaces
that have no relationship. The honest cost of rejecting it: the graph
cannot discover a link *between* workspaces, so a `Documents` note about
the Hytale protocol will not pull in the `Code` member that implements
it via the Named tier. Broadcast still reaches both; only the ranking
loses. Revisit if cross-workspace Named routing turns out to be the
common case rather than the exception.

### D5. One global resident budget; watchers follow focus

The LRU moves from `WorkspaceState` to `AppState` and caps residents
across all open workspaces. Otherwise N workspaces means N times the
memory and N times the watchers, and the cap stops meaning anything.

`task_watch` and `pipeline_known_running` stay on `WorkspaceState`, but
only the focused workspace holds a live one: focusing constructs the
poller, defocusing drops the `StopOnDrop` and clears the run-ledger set.
This preserves `pipeline_known_running`'s existing contract exactly —
"run ids this poller saw transition to running *during this workspace
session*" — because a defocus/refocus cycle is a new session by that
definition, same as a close/open is today.

Consequence, stated plainly: a non-focused workspace is searchable and
readable but not live. Its board does not update until it is focused.

### D6. Every cross-workspace result names its workspace

`RoutedHit` already carries a member name and a
`ken://<project-id>/<rel-path>` address. For an all-workspaces search the
UI additionally shows the workspace name, because "Protocol.md in
sr-docs" is ambiguous the moment two workspaces can hold a member of the
same name. The address itself needs nothing new — project ids are already
globally unique, which is why by-id opening works across workspaces at
all.

### D7. Restore focus, offer the rest

At launch, reopen the workspace that was focused at exit and nothing
else. The mechanism already exists: `Registry.last_workspace:
Option<Uuid>` records it and `Registry.workspaces` carries the rest as
recents, so this is a read of existing state rather than new persistence.
The switcher makes the others one click. Rejected: restoring every
previously open workspace, which turns a slow start into a slower one and
re-resolves manifests the user may not want this session.

### D9. Groups are the cluster unit, and they already work

The stated goal is not "search two workspaces" for its own sake — it is
that a folder of related repos (`Hytale/Shattered-Realms`,
`-Docs`, `-Tools`, `hytale-shared-source`) behaves as one cluster that a
question crosses, while staying separate projects. That already exists in
`ken-core` and is unreached from the UI:

- `WorkspaceConfig::derived_groups()` makes a group per parent folder
  from the `Group/Child` member names, with no configuration at all. The
  Code workspace therefore already *has* a `Hytale` group and a `Personal`
  group.
- `effective_group_members(name)` resolves a derived or manifest group.
- `route_search` already takes `group: Option<String>` and pins
  `RoutePlan.targets` to that group's members.
- `workspace_groups` / `workspace_set_group` / `workspace_remove_group`
  are exposed as commands.

Grepping the frontend for group usage finds only families and regex
captures. So the whole cluster feature is backend-complete and has no
control anywhere in the UI. The scope picker in task 4.3 therefore offers
three tiers, not two: workspace, then group, then member — and the group
tier is the one with the shortest path to being useful, because nothing
below the UI needs writing.

**Grouping is the default presentation, not an added tier.** The flat
member list in today's picker is not a neutral starting point that groups
get layered on top of; it is a flattening of structure `derived_groups()`
has already worked out and then discarded at the render. So the
correction is to draw the tree Ken already knows about. A flat list is the
exception, and belongs in per-workspace configuration for someone who
genuinely wants the four `Shattered-Realms*` folders listed apart. A
workspace whose member names imply no groups renders flat regardless,
with no grouping control shown for it.

**Every tier needs an explicit "all".** Today "everything" is expressed as
the absence of a selection, which is undiscoverable and leaves no way back
out once something is pinned. Each tier offers the widening choice as an
entry beside the individual ones.

Scope resolution order stays narrowest-wins: a pinned member beats a
pinned group, which beats a pinned workspace.

Depth stays at one level for now, matching `member_group`'s single-segment
derivation. Deeper nesting (`A/B/C`) is deliberately out of scope until
the one-level version is in daily use.

### D8. MCP scope mirrors the app's

`ken-mcp` gains `--workspace <path>`, mutually exclusive with
`--project`. Scope resolution becomes: pinned project, else pinned
workspace's members, else every workspace in the registry (widening
today's every-project meaning). The tools themselves are unchanged —
they already take addresses, and addresses are already workspace-agnostic
by D6's reasoning.

## Risks / Trade-offs

- **Wide mechanical edit in `src-tauri`.** Mitigated by removing the
  `workspace` field outright so every call site is a compile error, and
  by doing the reshape (task 2.1) before any behavior change.
- **Two-level scope can confuse.** Mitigated by the focused-workspace
  default and by never showing the workspace tier when only one workspace
  is open — the control degrades to today's member picker.
- **A non-focused workspace looks stale.** It is. The members strip
  shows which workspace is live so this is visible rather than
  surprising.

## Open Questions

- Should an all-workspaces search cap targets, or trust the existing
  per-database budget? Leaning trust — the budget already degrades slow
  members and a cap adds a second, less predictable truncation.
- Does chat's context assembly need a workspace tier too, or is scoping
  the search enough? Deferred until the search half is in use.

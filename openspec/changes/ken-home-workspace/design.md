# Design: ken-home-workspace

## Context

The workspace change introduced `.ken-workspace/workspace.json`, member
resolution, lazy activation (`WORKSPACE_RESIDENT_CAP = 12`, the rest
dormant until focused), and per-member runtimes. `federated-kg` built
`kg.sqlite` at the workspace root. `kg-routing` added `routing.rs` —
three-tier planning (Named → KG-guided → Broadcast), per-member search,
merge with per-member status — and the `route_search` command over it.
`ken-tasks` and `ken-pipeline` added the workspace board and
`pipeline::compose_digest`.

Home predates all of it. It reads `digest`, `review`, `ingests`,
`homeRecents` and `HomeStatus` from the focused project and renders six
sections, none of which know a workspace exists.

Two existing facts shape every decision below:

- **A member's index is addressable without activating the member.**
  Indexes live at `<base>/index/<project-id>.db`, opened by
  `Db::open(base, project_id)`. Residency is about runtimes — watcher,
  engine, extraction worker — not about whether the database can be
  read. Dormancy is therefore not the barrier it appears to be.
- **`route_search` already does the hard part** (plan, fan out, merge,
  report per-member status) and is limited only by where it gets its
  member list: `AppState::members`, chosen because no manifest existed
  when it was written. That comment is now stale.

## Goals / Non-Goals

**Goals:**
- Home tells the truth about the whole workspace, or says plainly which
  members it could not reach.
- Cross-member search reaches every manifest member, including dormant
  ones, without activating them.
- Reuse `routing.rs` and `digest.rs` as-is; add composition above them,
  not alternatives beside them.
- Flags off ⇒ Home is byte-identical to today.

**Non-Goals:**
- Regenerating or rescheduling per-member digests. The ≥07:00 gate,
  quiet-day fallback, and one-row-per-local-day contract are untouched.
- Per-tab independent project state. One focus, optional narrowing.
- A new workspace-level AI call. The workspace digest composes text that
  already exists; it does not ask a model to rewrite it.
- Making dormant members resident. Reading an index is not activation.

## Decisions

1. **Targets come from the manifest; indexes open by id.**
   `route_search` builds its `MemberInfo` list from `Workspace::members`
   (`MemberStatus::Ok` entries), not from `AppState::members`. For a
   member already resident, its live `Arc<Mutex<Db>>` is reused — no
   second connection to the same file. For a dormant member, a
   short-lived `Db::open(base, project_id)` is opened for the query and
   dropped after. `Missing` and `Invalid` members never become targets;
   they surface in the members strip instead.

2. **Opening a dormant index is bounded, and failure is a status, not an
   error.** Each open costs pragmas plus a migration probe. `routing.rs`
   already models exactly this outcome — `MemberStatus::Unavailable`
   covers "couldn't be searched for any other reason", and the Broadcast
   tier already has a per-DB latency budget. A dormant member whose open
   or search blows the budget is reported `Unavailable` and skipped,
   never blocking the merged result. This is why the KG does *not* need
   to answer alone: routing's existing skip-and-report contract already
   handles the slow tail.

3. **Scope is expressed as a plan, not a separate code path.** "All
   projects" calls `plan_route` normally. A pinned member short-circuits
   to `RoutePlan { targets: vec![id], reason: Named }` without consulting
   the KG. `merge_routed` and the result shape are identical either way,
   so the UI renders one thing and the scope control is genuinely just a
   scope control.

4. **The workspace digest composes, and never generates.** It reads each
   member's stored digest row for today (`get_digest`) and
   `pipeline::compose_digest`, and assembles them. A member with no row
   today is listed as "not yet written" rather than triggering
   generation — generation stays owned by the per-project scheduler,
   which already has the in-flight guard, the 07:00 gate, and the
   claude-missing check. Composition is pure and testable with fixture
   rows; no AI call, no thread, no scheduling.

5. **No new feature flag.** Each block is gated by the flag that already
   owns its data: members strip and workspace digest on `workspace`,
   daily board on `kenTasks`, cross-member search on `kgRouting` (which
   already treats `federatedKg` as a soft dependency — without it the
   KG-guided tier is skipped, Named/Broadcast still work). A tenth flag
   would gate a *view* over data whose availability is already gated,
   giving two switches for one outcome. With every flag off, Home falls
   through to exactly today's rendering.

6. **The members strip is the honesty surface.** It is the only place
   that shows `Missing` and `Invalid` members, which are invisible
   everywhere else in the app — `Workspace::open` deliberately does not
   fail on them, so without this they are silently absent. Each row
   carries index state, unread count, failed files, and reachability.

7. **One focus, per-tab override that defaults to inherit.**
   `app.project` remains the workspace-wide focus every screen reads. A
   tab may override it with a visible chip; the chip defaults to
   *inherit*, so changing focus moves every non-overridden tab together.
   Rejected: per-tab project state. `app.project` is read by most
   screens and their stores, so per-tab scope is a state refactor rather
   than a UI change, and independent focus per tab produces genuinely
   confusing states (Files on one project while Tasks shows another)
   with no way to answer "which project am I in".

8. **The members-overview command closes an existing gap.**
   `loadFocusedMemberState` documents that no per-member `ProjectInfo`
   read path exists, so `excluded` and `ingestRunner` fall back to
   defaults rather than the focused member's real values. The strip
   needs per-member state anyway; the same command serves both, and the
   deviation note comes out with it.

## Risks / Trade-offs

- **Search latency scales with member count.** Broadcast over twelve
  members means up to twelve index opens and searches. Mitigated by the
  existing per-DB budget and skip-and-report, by reusing live handles for
  resident members, and by Named/KG tiers usually narrowing the set well
  below the cap. Worst case degrades to partial results with an honest
  per-member status list — never a hang.
- **Composed digests read less fluently than a written one.** A roll-up
  of seven paragraphs is not one warm paragraph. Accepted for v1: it is
  truthful and free, and a single workspace-level generation can be
  layered on later using the same `assistant::oneshot` the per-project
  digest already uses.
- **The members strip can be long.** Twelve rows plus status is a lot of
  Home. It should collapse to a summary line with a count of members
  needing attention, expanding on click.
- **A dormant member's index may be stale**, since nothing has watched it
  since it went dormant. Results from it are correct as of its last
  ingest, not as of now. The per-hit member attribution plus the index
  state in the strip make this legible rather than hidden.
- **Reusing existing flags means partial Home states** — `workspace` on
  but `kenTasks` off yields a members strip and no daily board. Accepted:
  each block independently degrades to absent, which is the same
  contract every other flagged surface in Ken follows.

# ken-families Specification

## ADDED Requirements

### Requirement: Family connections behind a global flag

Ken SHALL support multiple family connections, each storing a
remote URL, the local member identity, a live-sync toggle, a poll
interval (default 120s), and an optional attached workspace. With
`kenFamilies` off, Ken SHALL create no clones, run no timers, show
no Families page, and register no family tools — byte-identical to
pre-feature behavior. If system `git` is not on PATH, the feature
SHALL report "unavailable {reason}" and stay inert with no error
dialogs.

#### Scenario: flag off is inert
- **WHEN** `kenFamilies` is disabled and Ken starts with saved
  connections present
- **THEN** no git process runs, no network traffic occurs, and the
  UI shows no family surfaces

#### Scenario: git missing degrades quietly
- **WHEN** `kenFamilies` is on but `git` is not on PATH
- **THEN** the Families page shows one unavailable state and no
  sync is attempted

### Requirement: Templated repo bootstrap

"Create family" SHALL scaffold and commit `family.json` (id, name,
`template` schema version starting at 1, members with short stable
slug ids, flattened `extra`),
`members/<id>/{inbox,board,workspace}/`, and `shared/` including
`conventions.md` — the Ken behavior contract each member's Ken
loads and adheres to — into a new repo. "Join family" SHALL clone
an existing remote into
`<app data>/ken/families/<family-id>/` and either select an
existing member or append one to the manifest's member array.
Manifest reads SHALL be tolerant: unknown keys round-trip
untouched. A manifest declaring a `template` version newer than
this Ken supports SHALL make the connection unavailable ("needs a
newer Ken") with no sync attempted.

#### Scenario: join appends, never rewrites
- **WHEN** a new member joins an existing family
- **THEN** the only manifest change is one appended entry in
  `members`

#### Scenario: newer template halts, doesn't guess
- **WHEN** `family.json` declares a `template` version greater
  than this Ken supports
- **THEN** the connection shows "needs a newer Ken" and no sync,
  ingest, or write runs against the clone

### Requirement: Strict write lanes enforced in code

The local Ken SHALL only ever commit paths that are (1) under
`members/<self>/`, (2) new files under another member's `inbox/`,
or (3) the member-array append in `family.json` during join.
Editing or deleting another member's files SHALL be impossible:
`commit_paths` SHALL validate every staged path and refuse the
commit on violation. `shared/` edits in v1 SHALL travel as proposal
inbox items to the family's owner member.

#### Scenario: cross-lane write is refused
- **WHEN** any code path stages a modification to
  `members/other/board/x.md`
- **THEN** the commit is refused and the violation is reported as
  a bug

#### Scenario: inbox delivery is create-only
- **WHEN** Ken sends a task to another member
- **THEN** exactly one new ULID-named file appears under that
  member's `inbox/` and no existing file changes

### Requirement: Typed inbox items with an acceptance gate

Inbox items SHALL be frontmatter files with `id` (ULID), `kind`
(`task|message|notification`), `from`, `status`
(`unread|seen|accepted|archived`), `created`, `updated`, `title`,
and for tasks a `task` payload — patched via the shared
frontmatter patch core. Nothing arriving from a family repo SHALL
enter the user's board, daily board, or any agent-claimable queue
until explicitly accepted; there SHALL be no auto-accept in v1.
Accepting a task SHALL mint a new task file in
`members/<self>/board/` with a fresh id and provenance in `## Log`,
then mark the item `accepted`. Malformed items SHALL surface raw in
the tray, never crash, never be rewritten. Ken MAY propose accept /
push-back / archive per item as secretary; a push-back SHALL travel
as an ordinary message item created in the sender's inbox (lane
rule 2), never as a modification of the sender's files.

#### Scenario: unaccepted task is invisible to agents
- **WHEN** a task item sits `unread` in the inbox and an MCP agent
  lists claimable tasks
- **THEN** the incoming task does not appear

#### Scenario: accept copies, not moves
- **WHEN** the user accepts a task item
- **THEN** a new board task exists with a fresh id citing the
  sender, and the inbox item remains with `status: accepted`

### Requirement: Poll-based sync that never force-pushes

While live-sync is on, each connection SHALL fetch and integrate
via rebase every poll interval and commit + push after local
writes, retrying once with a fresh integrate on non-fast-forward.
A "Sync now" action SHALL run the same cycle on demand. Any rebase
conflict SHALL put the connection in an error state, stop polling,
and require manual resolution; Ken SHALL never auto-resolve or
force-push.

#### Scenario: concurrent senders both land
- **WHEN** two members each push a new inbox item to a third
  member between that member's polls
- **THEN** the next poll integrates both files with no conflict

#### Scenario: conflict halts, loudly and safely
- **WHEN** a rebase conflict occurs
- **THEN** polling stops, the connection shows the error with
  git's own output, and the remote is untouched

### Requirement: Family member indexing and addressing

A connection attached to a workspace SHALL be ingested as a member
of kind `family` with project id = the manifest id, addressed as
`ken://<family-id>/<rel-path>` and listed by `list_projects` with
`kind: "family"`. Built-in tier rules SHALL index `shared/**` at
full tier (entities federate) and `members/**` plus `family.json`
at search-only. Unattached connections SHALL sync and notify but
not join search.

#### Scenario: shared knowledge federates
- **WHEN** `shared/architecture.md` names a system and the
  workspace KG rebuilds
- **THEN** the entity exists with a source link into the family
  member

#### Scenario: boards never mint entities
- **WHEN** fifty board tasks mention the same system
- **THEN** no KG entity originates from `members/**`

### Requirement: Merged UI surfaces

Family board tasks SHALL merge into the existing Tasks tab Kanban
with a per-family filter chip; status changes SHALL write to the
family clone and sync like any board write. Unread inbox items
SHALL surface in a notification tray grouped by family with inline
accept/dismiss. Families SHALL be a settings page, not a new
top-level tab.

#### Scenario: drag-drop crosses the wire
- **WHEN** the user drags a family task from `todo` to `doing`
- **THEN** the task file in the clone changes only `status` and
  `updated`, and the change is pushed

## MODIFIED Requirements

### Requirement: Hybrid homes with one aggregated board (ken-tasks)

Task homes SHALL additionally include family boards
(`members/<self>/board/` in each attached connection) as a third
home. Aggregation, filtering, archiving to `archive/YYYY-MM/`, and
the MCP claim-and-complete protocol SHALL work identically across
all three homes; family claims SHALL only ever touch the local
member's own board.

#### Scenario: family task completes over MCP
- **WHEN** an agent claims and completes an accepted family task
- **THEN** the board file moves todo → doing → done in the clone
  and the completion is pushed for teammates to see

# multi-workspace

## ADDED Requirements

### Requirement: Several workspaces open at once, exactly one focused
Ken SHALL hold any number of open workspaces and SHALL designate exactly
one of them as focused whenever at least one is open. Opening a workspace
SHALL NOT tear down the runtime of any other open workspace, and SHALL
NOT close, re-resolve, or reload it. Surfaces that present a single
workspace — the daily board, the digest, the pipeline queue — SHALL read
the focused workspace.

#### Scenario: Opening a second workspace keeps the first
- **WHEN** a workspace is open and the user opens a second one
- **THEN** both are open, the second is focused, and the first's members
  remain resolvable with their runtimes intact

#### Scenario: Re-opening an open workspace only changes focus
- **WHEN** the user opens a workspace that is already open
- **THEN** focus moves to it, its manifest is not re-read, and no member
  runtime is torn down

#### Scenario: Single-workspace behavior is unchanged
- **WHEN** exactly one workspace is open
- **THEN** every workspace-scoped surface behaves as it did before this
  capability existed

### Requirement: Focus can move without reopening
Ken SHALL provide a way to focus an already-open workspace directly,
without closing the current one and without re-entering the project
picker. Focusing SHALL NOT activate any member as a side effect.

#### Scenario: Switching focus is not a picker round-trip
- **WHEN** the user focuses another open workspace from the switcher
- **THEN** focus moves, no workspace is closed, and no member is
  activated

#### Scenario: The switcher is absent when it would be empty
- **WHEN** exactly one workspace is open
- **THEN** no workspace switcher is shown

### Requirement: Closing the focused workspace falls through to another
Closing a workspace SHALL close only that workspace and drop only its
members' runtimes. When the closed workspace was focused and another
remains open, focus SHALL move to a remaining open workspace. When none
remains, Ken SHALL hold no focused workspace.

#### Scenario: Focus falls through on close
- **WHEN** two workspaces are open and the focused one is closed
- **THEN** the other remains open and becomes focused

#### Scenario: Closing the last workspace leaves none focused
- **WHEN** the only open workspace is closed
- **THEN** no workspace is open and none is focused

### Requirement: One resident-member budget across all workspaces
The cap on resident members SHALL apply across every open workspace
rather than per workspace. When admitting a member would exceed the cap,
the least-recently-focused resident SHALL be evicted regardless of which
workspace owns it. Eviction SHALL NOT close the evicted member's
workspace or remove it from its manifest.

#### Scenario: Eviction crosses the workspace boundary
- **WHEN** members of a newly focused workspace push the resident count
  past the cap
- **THEN** the least-recently-focused resident is evicted even if it
  belongs to a different workspace, and its workspace stays open

### Requirement: Live watchers belong to the focused workspace
The task-board poller and the pipeline run-ledger watch SHALL run for the
focused workspace only. Focusing a workspace SHALL start them; defocusing
SHALL stop them and clear the run ids observed during that focus session.
A non-focused workspace SHALL remain searchable and readable while not
receiving live updates.

#### Scenario: Defocusing stops the poller
- **WHEN** focus moves away from a workspace
- **THEN** its task-board poller stops and its observed-running run ids
  are cleared

#### Scenario: A non-focused workspace is still readable
- **WHEN** a workspace is open but not focused
- **THEN** its members can be searched and read, and its board is not
  updating

### Requirement: Restore the focused workspace at launch
On launch Ken SHALL reopen the workspace that was focused when it last
exited, and SHALL NOT reopen the others automatically. Previously opened
workspaces SHALL remain listed for one-click opening.

#### Scenario: Only the focused workspace returns
- **WHEN** Ken is launched after exiting with two workspaces open
- **THEN** the previously focused one is open and focused, and the other
  is offered rather than opened

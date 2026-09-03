# workspace-home

## ADDED Requirements

### Requirement: Home is scoped to the workspace, not the focused member
When a workspace is open, Home's summary surfaces SHALL describe every
member, not only the focused one. Changing the focused member SHALL NOT
change which members Home reports on.

#### Scenario: Home covers members other than the focused one
- **WHEN** a workspace with several members is open and one is focused
- **THEN** Home's summary reflects all resolvable members

#### Scenario: Focus change does not narrow Home
- **WHEN** the user focuses a different member
- **THEN** Home continues to report on the whole workspace

### Requirement: Workspace digest composes existing digests
The workspace digest SHALL be assembled from each member's already-stored
digest for the current local day together with the task-board summary. It
SHALL NOT generate, schedule, or regenerate any member's digest, and SHALL
NOT make an AI call of its own. A member with no digest stored for today
SHALL be shown as not yet written. Per-member detail SHALL be reachable
from the composed view.

#### Scenario: Members' digests roll up
- **WHEN** several members have a digest stored for today
- **THEN** the workspace digest presents them together with the board
  summary

#### Scenario: A member without a digest is named, not hidden
- **WHEN** one member has no digest stored for today
- **THEN** it is listed as not yet written and no generation is started

#### Scenario: Composition triggers no generation
- **WHEN** the workspace digest is assembled
- **THEN** no member digest is created or refreshed as a result

### Requirement: Members strip shows reachability
Home SHALL show one entry per manifest member carrying its index state,
unread count, failed-file count, and whether it resolved. Members whose
folder is missing or whose configuration does not parse SHALL be shown
here, since they are surfaced nowhere else. The strip SHALL collapse to a
summary when the workspace has many members, showing how many need
attention.

#### Scenario: A broken member is visible
- **WHEN** the manifest lists a member whose folder has been moved away
- **THEN** Home shows that member as unresolved

#### Scenario: Many members collapse to a summary
- **WHEN** the workspace has more members than fit comfortably
- **THEN** the strip collapses and reports how many need attention

### Requirement: Daily board spans members
Home's waiting and needs-attention surfaces SHALL read the workspace task
board across members rather than the focused member's tasks alone.

#### Scenario: Work from an unfocused member appears
- **WHEN** a task needing attention belongs to a member that is not
  focused
- **THEN** it appears in Home's needs-attention surface

### Requirement: One focus with optional per-tab override
The workspace SHALL have a single focused member that screens read by
default. A tab MAY override the scope for itself through a visible
control that defaults to inheriting the workspace focus. Changing the
workspace focus SHALL move every tab that has not overridden it.

#### Scenario: Tabs follow the workspace focus by default
- **WHEN** the user changes the focused member
- **THEN** every tab that has not overridden its scope follows

#### Scenario: An overridden tab holds its scope
- **WHEN** a tab's scope is pinned to a member and the workspace focus
  changes
- **THEN** that tab keeps its pinned scope and shows that it is pinned

### Requirement: Every added block is flag-gated and inert when off
Each new Home block SHALL be gated by the existing flag that owns its
data, and no new feature flag SHALL be introduced. With those flags off,
Home SHALL render exactly as it did before this change.

#### Scenario: Flags off leaves Home unchanged
- **WHEN** the workspace, task, and routing flags are all off
- **THEN** Home renders its previous single-project layout

#### Scenario: Blocks degrade independently
- **WHEN** the workspace flag is on but the task flag is off
- **THEN** the members strip appears and the daily board does not

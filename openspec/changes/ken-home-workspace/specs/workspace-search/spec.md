# workspace-search

## ADDED Requirements

### Requirement: Search reaches every manifest member
Routed search SHALL take its targets from the workspace manifest's
resolvable members, not from the members that happen to be open in the
current session. A member whose runtime is dormant SHALL still be
searched, by opening its index by project id, and SHALL NOT be activated
— no watcher, no engine, and no eviction of a resident member — as a
side effect of being searched. A member already resident SHALL have its
live database handle reused rather than opened a second time. Members
whose folder is missing or whose configuration does not parse SHALL NOT
become targets.

#### Scenario: A dormant member contributes results
- **WHEN** a query is run over a workspace where a member has never been
  focused this session
- **THEN** that member's index is searched and its hits appear in the
  merged results, and the member is still dormant afterwards

#### Scenario: A resident member is not opened twice
- **WHEN** a query is run over a workspace with a resident member
- **THEN** that member is searched through its existing database handle

#### Scenario: An unresolvable member is skipped, not fatal
- **WHEN** the manifest lists a member whose folder no longer exists
- **THEN** it is not searched, the remaining members return results, and
  the search does not fail

### Requirement: Slow or unreadable members degrade, never block
A target whose index cannot be opened or whose search exceeds the
per-database budget SHALL be reported with an unavailable status and
skipped. Merged results SHALL be returned from the members that did
respond, and the per-member status list SHALL let the caller tell
"searched and found nothing" apart from "not searched".

#### Scenario: One unreadable index does not fail the search
- **WHEN** one member's index cannot be opened and three others can
- **THEN** results merge from the three, and the fourth is reported
  unavailable

#### Scenario: Empty results are distinguishable from skipped members
- **WHEN** a member is searched and matches nothing
- **THEN** its status is reported as searched, not as unavailable

### Requirement: Scope control over the search
Home search SHALL offer a scope of all projects or one named member, and
SHALL default to all projects. All projects SHALL route through normal
planning. A pinned member SHALL search exactly that member, without
consulting the knowledge graph, and SHALL produce the same result shape
as an unpinned search.

#### Scenario: Default scope searches everything
- **WHEN** the user searches without changing the scope
- **THEN** planning runs normally and eligible members are searched

#### Scenario: Pinned scope searches one member
- **WHEN** the user pins the scope to a single member and searches
- **THEN** only that member is searched and the knowledge graph is not
  consulted

### Requirement: Every result names its project
Each merged result SHALL carry the member name it came from and its
addressable identity, and the UI SHALL show which project each result
belongs to.

#### Scenario: Results are attributed
- **WHEN** results merge from more than one member
- **THEN** each result displays the name of the project it came from

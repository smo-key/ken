# member-onboarding

## ADDED Requirements

### Requirement: A member that has never been mapped gets mapped
A project with no stored knowledge model SHALL have one built
automatically once its scan has settled, without anyone triggering it by
hand. The decision to build SHALL be taken by the existing
`should_auto_build` policy — including its never-built, quiet-period,
in-flight and minimum-interval rules — and SHALL NOT be re-implemented or
re-tuned outside the module those rules are tested in.

#### Scenario: First contact produces a model
- **WHEN** a member has been scanned, has indexed files, has never been
  mapped, and its scan has settled
- **THEN** a knowledge build starts without user action

#### Scenario: The policy is not duplicated
- **WHEN** the automatic path decides whether to build
- **THEN** it asks the same decision function the manual path's thresholds
  are defined by, and applies no thresholds of its own

#### Scenario: A burst produces one build, not many
- **WHEN** many files change in quick succession
- **THEN** at most one build starts, after the burst settles

#### Scenario: No Claude CLI means no build and no error
- **WHEN** the Claude Code CLI cannot be found
- **THEN** no build is attempted and the member is reported as skipped
  with that reason, not failed

### Requirement: Joining a workspace enqueues a member for analysis
A member SHALL be enqueued for onboarding when it joins a workspace, and
every already-listed member without a knowledge model SHALL be enqueued
when the workspace opens. Onboarding SHALL proceed as an ordered queue of
scan, then knowledge model, and at most one knowledge build SHALL run at a
time across the workspace.

#### Scenario: A new member is picked up
- **WHEN** a folder is added as a member
- **THEN** it is enqueued and progresses to a knowledge model without
  further action

#### Scenario: An existing workspace is not stranded
- **WHEN** a workspace whose members have never been mapped is opened
- **THEN** every unmapped member is enqueued, not only members added later

#### Scenario: Builds do not stack
- **WHEN** two members are eligible at the same moment
- **THEN** one builds and the other waits

### Requirement: Dormant members are analysed without being activated
A member that is not resident SHALL still be onboarded, by opening its
index by project id for the duration of the build and releasing it
afterwards. Onboarding SHALL NOT activate the member, start its watcher,
evict a resident, or change the recently-focused order.

#### Scenario: A dormant member is mapped and stays dormant
- **WHEN** a dormant member is onboarded
- **THEN** it has a knowledge model afterwards and is still dormant

#### Scenario: Onboarding does not disturb residency
- **WHEN** a dormant member is onboarded while residents are at the cap
- **THEN** no resident is evicted and the recently-focused order is
  unchanged

#### Scenario: An unresolvable member is skipped, not failed
- **WHEN** a queued member's folder is missing or its configuration does
  not parse
- **THEN** it is reported as skipped with that reason and the queue
  continues

### Requirement: Onboarding state is visible per member
Each member SHALL carry an observable onboarding state — queued,
scanning, mapping, ready, failed, or skipped — and failed and skipped
SHALL carry a human-readable reason. The state SHALL be derived from the
existing scan and knowledge-model signals rather than a second source of
truth.

#### Scenario: Progress is visible without polling
- **WHEN** a member moves between onboarding states
- **THEN** the change is emitted and the members strip reflects it

#### Scenario: A skip explains itself
- **WHEN** a member is skipped
- **THEN** the reason is shown alongside it

#### Scenario: Nothing to show is shown as nothing
- **WHEN** every member is ready
- **THEN** no onboarding queue is displayed

### Requirement: Analysis is incremental and its progress is visible
Onboarding SHALL NOT impose a file limit and SHALL NOT skip a member for
being large. Per-file extraction already proceeds one file at a time on a
background worker that yields to interactive work, so a large member is a
longer job rather than an excluded one. Onboarding SHALL report analysed
and total counts as progress while a member is being mapped.

#### Scenario: A large member is mapped, not refused
- **WHEN** a member has tens of thousands of eligible files
- **THEN** it is analysed incrementally and reports progress, and is never
  skipped for its size

#### Scenario: Progress is legible
- **WHEN** a member is being mapped
- **THEN** its analysed and total file counts are reported

#### Scenario: Background work yields
- **WHEN** an interactive request arrives while a member is being mapped
- **THEN** the interactive request is served first and mapping resumes

### Requirement: A stalled analyser says so
When per-file extraction cannot proceed because no local model is
installed, onboarding SHALL report that as its own state, distinct from
queued, and SHALL offer the action that resolves it. It SHALL NOT present
a member as queued or in progress while nothing is happening.

#### Scenario: No local model is a stated condition, not silence
- **WHEN** no local model is installed
- **THEN** affected members report that they are waiting for it, with the
  install action attached

#### Scenario: Installing the model resumes the queue
- **WHEN** a local model is installed while members are waiting
- **THEN** extraction resumes without restarting the app, and previously
  errored files are retried once

### Requirement: A member that arrives already configured is adopted, not re-created
When a member carries committed Ken configuration but has no local index,
onboarding SHALL adopt that configuration unchanged and build only what is
local. It SHALL NOT assign a new project id to a project that already has
one, and the committed ignore rules SHALL apply from the first scan.

#### Scenario: A teammate's clone keeps the shared identity
- **WHEN** a project with a committed configuration is opened on another
  machine with no index
- **THEN** its index is built under the project's existing id, and shared
  addresses referring to it continue to resolve

#### Scenario: Committed ignore rules apply immediately
- **WHEN** a configured project is indexed for the first time on a machine
- **THEN** its committed ignore rules govern that first scan, so content
  the repository excludes is never indexed even once

#### Scenario: An existing workspace manifest is adopted
- **WHEN** a workspace manifest is already present at the parent folder
- **THEN** it is adopted unchanged rather than rewritten, and its members
  are enqueued for local indexing

### Requirement: The workspace graph follows from member models
Once member knowledge models complete, the workspace knowledge graph
SHALL be built by the existing debounced trigger, and a burst of member
completions SHALL collapse into a single graph build. No graph SHALL be
built when the federated-graph flag is off, and member models SHALL still
build in that case.

#### Scenario: Ten members produce one graph build
- **WHEN** ten members complete their knowledge models within one debounce
  window
- **THEN** exactly one workspace graph build runs

#### Scenario: Models without a graph
- **WHEN** the federated-graph flag is off
- **THEN** member knowledge models are still built and no graph file is
  written

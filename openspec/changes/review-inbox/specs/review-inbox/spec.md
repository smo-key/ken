# review-inbox

## ADDED Requirements

### Requirement: Unified inbox assembled at read time
The Review inbox SHALL present everything Ken needs a human for in one
list, assembled at read time from existing sources of truth without
duplicating state: pending-approval ingest runs, stale ingests (derived
with the same rule as the Ingests screen), files that could not be
indexed, broken recipes, and open stored review items. Each item SHALL
carry a kind, a plain-language title and body, a timestamp, and a source
reference.

#### Scenario: All kinds appear together
- **WHEN** a run is pending approval, an ingest is stale, a file failed
  to index, and a recipe is broken
- **THEN** the inbox lists four items, one per situation, ordered by kind
  severity and recency

#### Scenario: Inbox agrees with its sources
- **WHEN** a pending run is approved from the Ingests screen
- **THEN** the next inbox read no longer contains that approval item,
  with no separate cleanup step

### Requirement: Per-kind actions
Each inbox item SHALL offer actions matching its kind: approvals offer
Approve (applies the staged output via the existing engine operation) and
Discard; stale items offer Run now; failed files open in Files where the
fallback preview shows the reason; broken recipes open in Ingests; stored
items can be marked done. Actions SHALL be derived client-side from the
item's kind.

#### Scenario: Approving from the inbox
- **WHEN** the user clicks Approve on a large-refresh item
- **THEN** the staged output is applied exactly as if approved from the
  ingest detail, and the item leaves the inbox

#### Scenario: Failed file leads to the reason
- **WHEN** the user opens a failed-file item
- **THEN** Files opens on that file, whose preview explains why it could
  not be indexed

### Requirement: Nav badge count
The navigation rail SHALL show a count badge on Review whenever open
inbox items exist, and no badge when none do. The count SHALL stay
current as runs change state and the index updates, without visiting the
Review screen.

#### Scenario: Badge reflects open items
- **WHEN** two items are open in the inbox
- **THEN** the Review rail item shows a badge reading 2

#### Scenario: Badge clears itself
- **WHEN** the last open item is resolved
- **THEN** the badge disappears without a manual refresh

### Requirement: Done section
The inbox SHALL show a Done section of items resolved in the last 7 days
below the open items: discarded runs, approved large refreshes, and
resolved stored items.

#### Scenario: Approval moves to Done
- **WHEN** the user approves a pending run
- **THEN** the item leaves the open list and a corresponding entry
  appears under Done

### Requirement: Stored review-item substrate
The per-project database SHALL provide a `review_items` table (kind,
title, body, source reference, open/resolved status, free-form payload,
created/resolved timestamps) with operations to insert, resolve, list
open, and list recently resolved items, migrated additively from schema
v3. Open stored items SHALL appear in the unified inbox alongside derived
items so future kinds require no inbox reshaping. Nothing inserts stored
items in this change.

#### Scenario: Existing database upgrades in place
- **WHEN** a project database created at schema v3 is opened
- **THEN** it migrates to v4 and review items can be inserted and listed

#### Scenario: Stored item lifecycle
- **WHEN** a stored item is inserted and later resolved
- **THEN** it appears in the open list until resolution, then in the
  recently-resolved list with its resolution time

### Requirement: Empty state
When nothing is open, the Review screen SHALL show a friendly
plain-language empty state instead of a blank pane.

#### Scenario: Nothing to review
- **WHEN** the user opens Review with no open items
- **THEN** a "Nothing needs you right now" card is shown

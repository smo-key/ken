# sync-conflicts

## ADDED Requirements

### Requirement: Merge conflicts become Review items
When a pull reports conflicts, Ken SHALL abort the merge so the working
tree is restored exactly to its pre-pull state, capture each conflicted
file's local and incoming versions, and file one stored review item of
kind `conflict` per file with payload `{path, ours, theirs, draft,
draftStatus}`. The sync state SHALL become attention. Ken SHALL NOT
leave conflict markers in any project file, and SHALL NOT file a
duplicate item while one is already open for the same path.

#### Scenario: Conflicting edits land in Review
- **WHEN** the user and a teammate changed the same lines of the same
  document and Ken pulls
- **THEN** the document on disk still holds the user's version, and
  Review shows a conflict item carrying both versions

#### Scenario: Re-pull does not double-file
- **WHEN** the next throttled pull hits the same conflict before the
  user resolves it
- **THEN** the inbox still shows exactly one item for that path

### Requirement: AI merge draft
For each open conflict item Ken SHALL queue an AI merge draft (one at a
time): the agent receives both versions and must write the complete
merged document, preserving both sides' intent, into a staging path;
the result is stored in the item's payload as `draft` with `draftStatus`
`ready`. While pending, the Review detail SHALL say Ken is drafting a
suggestion; if the CLI is missing or the run fails, `draftStatus`
becomes `failed` and the remaining resolution choices still work.

#### Scenario: Draft appears in the conflict detail
- **WHEN** the draft run completes
- **THEN** the conflict detail shows "Ken's take" with the drafted
  content and an "Accept Ken's merge" action

#### Scenario: No Claude CLI
- **WHEN** the claude binary cannot be found
- **THEN** the item shows both versions with keep-mine / take-theirs /
  edit choices and no drafting spinner stuck forever

### Requirement: Conflict resolution actions
A conflict item SHALL offer: accept Ken's merge (the draft; falls back
to the local version when no draft exists), keep mine (local version),
take theirs (incoming version), and edit manually (writes the draft or
local version, then opens the file in Files). Every resolution SHALL
write the chosen content to the project file, resolve the review item,
reindex the file, and let the normal push path sync it.

#### Scenario: Take theirs
- **WHEN** the user chooses "Use theirs" on a conflict item
- **THEN** the file on disk now holds the incoming version, the item
  moves to Done, and the change syncs out

#### Scenario: Edit manually
- **WHEN** the user chooses "Edit manually"
- **THEN** the best available version is written, the item resolves, and
  the file opens in the Files editor

### Requirement: Conflicted-copy detection on shared drives
Ken SHALL detect files whose name carries a sync-service conflict marker
— a parenthesized segment containing "conflicted copy" (case-insensitive,
including "(Bob's conflicted copy 2026-07-12)") or starting with "case
conflict" — among watcher-reported changes in any project, and file a
stored review item of kind `conflict-copy` with payload
`{copyPath, originalPath}` where originalPath is the name with the
marker stripped when that file exists. Names that merely contain similar
words without the parenthesized marker SHALL NOT match. Detection SHALL
be a pure function with unit tests.

#### Scenario: Dropbox conflicted copy lands in Review
- **WHEN** "notes (conflicted copy 2026-07-12).md" appears next to
  "notes.md"
- **THEN** Review shows a conflicting-copy item pairing the two files

#### Scenario: Ordinary names do not match
- **WHEN** files named "copy of notes.md" or "conflicted-copy-analysis.md"
  change
- **THEN** no conflict item is filed

### Requirement: Conflicted-copy resolution actions
A conflict-copy item SHALL offer: keep the copy (its content replaces
the original — or the copy is renamed to the original name when no
original exists — and the copy file is deleted), keep the original
(the copy file is deleted), and open both in Files (non-resolving).

#### Scenario: Keep the copy
- **WHEN** the user keeps the copy
- **THEN** the original file holds the copy's content, the marker-named
  file is gone, and the item is resolved

#### Scenario: Keep the original
- **WHEN** the user keeps the original
- **THEN** the marker-named file is deleted and the original is untouched

### Requirement: Conflict detail per prototype
The Review detail for a conflict SHALL show side-by-side cards labelled
in plain language (the user's version and the teammate's version), a
"Ken's take" box with the draft or a drafting notice, and the action
buttons. Conflict items SHALL rank directly below approvals in the
inbox order.

#### Scenario: Side-by-side versions
- **WHEN** the user opens a conflict item
- **THEN** both versions are visible at once with plain-language headers
  and no git terminology

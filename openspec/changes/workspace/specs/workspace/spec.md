# workspace Specification

## ADDED Requirements

### Requirement: Workspace manifest in the parent folder

A workspace SHALL be defined by
`<parent>/.ken-workspace/workspace.json` containing `name`, `id`
(UUID), parent-relative `members`, and a flattened `extra` map, written
atomically and adopted if already present. Missing member folders SHALL
be reported as `missing` status, never fail the open.

#### Scenario: manifest round-trips with unknown fields
- **WHEN** a manifest containing unrecognized keys is opened and saved
- **THEN** the unrecognized keys survive byte-for-byte (flatten map)

#### Scenario: moved parent folder still opens
- **WHEN** the parent folder is renamed/moved and the workspace is
  reopened from its new path
- **THEN** all members resolve via relative names and open normally

### Requirement: Candidate discovery for the selection experience

`discover_candidates(parent)` SHALL list immediate subfolders (no
recursion), excluding hidden folders, `.ken-workspace`, and junk dirs
(`node_modules`, `target`), tagging each with `existing`/`new` (by
presence of `.ken/project.json`), file count, and repo markers.

#### Scenario: existing Ken project is recognized
- **WHEN** a subfolder already contains `.ken/project.json`
- **THEN** its candidate is tagged `existing` and pre-checked in the UI

### Requirement: N member projects open concurrently

Opening a workspace SHALL create a `ProjectHandle` (db, engine,
watcher) per resolvable member, run their ingest engines with
concurrency capped at 2, and expose one `focused` member that all
existing per-project commands operate on. `focus_project(id)` SHALL
switch focus without closing any member.

#### Scenario: both members ingest after workspace open
- **WHEN** a two-member workspace is opened with fresh folders
- **THEN** both member DBs reach indexed state without any focus change

#### Scenario: existing commands are focus-scoped
- **WHEN** a search/chat/files command runs while member A is focused
- **THEN** it reads and writes only member A's DB and state

### Requirement: Single-project mode is untouched

With the global `workspace` flag off, the launcher SHALL show no
workspace entry point and the app SHALL run entirely through the
`Single` mode path; opening a plain project with the flag on SHALL also
use `Single` mode. All pre-existing tests SHALL pass unmodified after
the `ProjectHandle` extraction.

#### Scenario: flag off hides the feature
- **WHEN** `workspace` is disabled in global settings
- **THEN** the launcher, commands, and events expose no workspace
  surface and single-project behavior is unchanged

### Requirement: All-projects keyword search

⌘K SHALL offer an "All projects" scope in workspace mode that queries
every member's FTS index, merges by round-robin rank interleave,
labels each hit with the member name, and opens hits in the owning
member (switching focus).

#### Scenario: hit opens in the owning project
- **WHEN** an all-projects result belonging to unfocused member B is
  chosen
- **THEN** focus switches to B and the file opens there

### Requirement: Workspace recents

Recently opened workspaces SHALL be listed alongside recent projects
and reopening one SHALL restore its last focused member.

#### Scenario: reopen restores focus
- **WHEN** a workspace focused on member B is closed and reopened
- **THEN** member B is focused

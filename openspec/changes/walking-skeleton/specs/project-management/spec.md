# project-management

## ADDED Requirements

### Requirement: Create a project from an existing folder
The system SHALL let the user create a project by selecting any existing
folder. Creation SHALL write `.ken/project.json` (name, generated UUID id,
folder include/exclude list, settings), register the project in the
app-data registry, and start an initial scan.

#### Scenario: Create from folder picker
- **WHEN** the user chooses "New project" and selects an existing folder
- **THEN** `.ken/project.json` is created in that folder, the project
  appears in the project switcher, and indexing begins

#### Scenario: Folder already contains .ken/project.json
- **WHEN** the user opens a folder that already has `.ken/project.json`
  (e.g. cloned from a teammate)
- **THEN** the existing project id and settings are adopted unchanged and
  the project is registered locally without rewriting the file

### Requirement: Project registry and switching
The system SHALL keep a local registry of known projects in app-data and
let the user switch between them from the title-bar project switcher.

#### Scenario: Switch projects
- **WHEN** the user picks another project in the switcher
- **THEN** the app loads that project's index, tree, and settings without
  restarting

#### Scenario: Registered path no longer exists
- **WHEN** a registered project's folder is missing on disk
- **THEN** the switcher marks it unavailable with a plain-language message
  and offers to locate or remove it, and the app does not crash

### Requirement: Folder include/exclude selection
The system SHALL let the user select which folders within the project are
ingested (default: all). Exclusions SHALL be persisted in `project.json`,
applied by scan and watcher, and shown in the file tree.

#### Scenario: Exclude a folder
- **WHEN** the user excludes `archive/`
- **THEN** its files are removed from the index, it renders dimmed with an
  "excluded" tag in the tree, and future changes inside it are ignored

#### Scenario: Re-include a folder
- **WHEN** the user re-includes a previously excluded folder
- **THEN** its files are scanned and indexed again

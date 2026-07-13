# document-editing

## ADDED Requirements

### Requirement: WYSIWYG editing of markdown and text
The Files screen SHALL edit markdown and plain-text files in a WYSIWYG
editor (headings, bold/italic, lists, links rendered as they will read) at
a comfortable reading measure, with a toggle to raw plain-text mode. Saves
SHALL write directly to the file on disk.

#### Scenario: Edit and save
- **WHEN** the user edits a markdown file and pauses typing
- **THEN** the change is saved to the file within a second and the header
  shows "Saved just now"

#### Scenario: Plain-text toggle
- **WHEN** the user switches to plain-text mode
- **THEN** the raw markdown source is shown and remains editable, and
  switching back preserves content exactly

### Requirement: External changes are never silently clobbered
If the open file changes on disk (another app, a teammate's sync), the
editor SHALL show a reload notice; if the user also has unsaved edits it
SHALL ask which version to keep rather than overwriting either silently.

#### Scenario: Clean reload
- **WHEN** the open file changes on disk and the editor has no unsaved edits
- **THEN** the editor offers/perform a reload showing the new content

#### Scenario: Conflicting edit
- **WHEN** the open file changes on disk while the editor has unsaved edits
- **THEN** the user is asked in plain language whether to keep their
  version or take the disk version, and nothing is written until they choose

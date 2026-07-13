# ingestion-indexing

## ADDED Requirements

### Requirement: Initial scan and extraction
On project open the system SHALL walk all included folders, route each file
to a format extractor, and store cleaned text plus metadata (path, kind,
size, mtime) in the project's SQLite database with an FTS5 index. Supported
extraction: markdown/txt/code natively; docx, xlsx, pptx via their zipped
XML; PDF text; images by filename and EXIF metadata. Unsupported or binary
files SHALL get a metadata-only entry searchable by name.

#### Scenario: Mixed-format folder is indexed
- **WHEN** a project containing .md, .docx, .xlsx, .pptx, .pdf and .png
  files is opened for the first time
- **THEN** every file gains an index entry and text extracted from each
  supported format is searchable

#### Scenario: Extraction failure does not block
- **WHEN** one PDF is corrupt and fails extraction
- **THEN** scanning continues, the file is recorded as "not indexed" with a
  reason visible in the UI, and it remains findable by filename

### Requirement: Live index via file watching
The system SHALL watch included folders and update the index within a few
seconds of file creation, modification, deletion, or rename, batching
rapid bursts of events.

#### Scenario: Edited file re-indexed
- **WHEN** a watched markdown file is saved with new content
- **THEN** within ~5 seconds a search for the new content finds the file

#### Scenario: Deleted file removed
- **WHEN** a watched file is deleted
- **THEN** it no longer appears in search results or the file tree

#### Scenario: Event burst
- **WHEN** hundreds of files change at once (e.g. a git checkout)
- **THEN** the index converges to the new state without the UI freezing

### Requirement: Rebuildable index
The index SHALL be fully derivable from the project folder. A "Reindex"
action SHALL rebuild the database from scratch, and the database SHALL
never be stored inside the project folder.

#### Scenario: Reindex recovers from corruption
- **WHEN** the user triggers Reindex
- **THEN** the database is rebuilt from the current folder contents and
  search reflects exactly the files on disk

#### Scenario: No database artifacts in project
- **WHEN** a project has been indexed
- **THEN** no Ken database or cache files exist under the project folder
  other than `.ken/` text configuration

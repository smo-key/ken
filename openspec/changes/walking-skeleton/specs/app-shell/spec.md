# app-shell

## ADDED Requirements

### Requirement: Paper & Ink application chrome
The app SHALL implement the prototype's shell: a title bar with project
switcher (name, glyph, sync-status dot placeholder), centered global
search field, and Chats toggle placeholder; and a left nav rail with Home,
Files, Review, Ingests, Map, Timeline and a bottom-pinned Settings entry.
Visual styling SHALL follow `docs/design/design-tokens.md` (colors,
typography with bundled fonts, spacing, radii, shadows).

#### Scenario: Navigate between screens
- **WHEN** the user clicks a rail item
- **THEN** the corresponding screen renders, the item shows the active
  clay treatment, and state (open file, query) survives switching away and
  back within the session

#### Scenario: Tokens match the design system
- **WHEN** any screen renders
- **THEN** backgrounds, text colors, accent usage, fonts, radii and
  shadows use the Paper & Ink token values, and no runtime requests are
  made to external font or asset hosts

### Requirement: Honest placeholders for future capabilities
Screens whose capabilities arrive in later changes (Home, Review, Ingests,
Map, Timeline, Settings beyond project basics) SHALL render designed
placeholder states in the prototype's layout language stating what is
coming, never blank panes or dead-looking UI.

#### Scenario: Visit a future screen
- **WHEN** the user opens Review in this change
- **THEN** a Paper & Ink styled placeholder explains the Review inbox is
  coming and points to what works today (Files, search)

### Requirement: File tree reflects knowledge state
The Files screen tree SHALL show folders and files with format glyphs,
mark excluded folders dimmed with an "excluded" tag, and show a footer
with watch status ("Watching · synced …" language per prototype).

#### Scenario: Tree renders states
- **WHEN** the Files screen loads for a project with an excluded folder
- **THEN** included folders/files show with correct glyphs, the excluded
  folder is dimmed and tagged, and the footer shows watching status

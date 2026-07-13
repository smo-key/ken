# knowledge-search

## ADDED Requirements

### Requirement: Search overlay
The system SHALL provide a search overlay opened via ⌘K (Ctrl+K on
non-mac) or the title-bar search field, querying the project index as the
user types and rendering ranked matches with highlighted snippets, file
glyphs, and paths per the prototype.

#### Scenario: As-you-type results
- **WHEN** the user types "billing cutover" in the overlay
- **THEN** matching documents appear ranked with the matched terms
  highlighted in their snippets, updating as the query changes

#### Scenario: No matches
- **WHEN** the query matches nothing
- **THEN** the overlay shows a plain-language empty state, not a blank area

### Requirement: Keyboard-first navigation
The overlay SHALL support full keyboard control: arrow keys move the
selection, ↵ opens the selected result in the Files screen, esc closes.

#### Scenario: Open a result
- **WHEN** the user presses ↵ on a selected result
- **THEN** the overlay closes and the file opens in the Files screen
  (editor for editable formats, preview otherwise)

#### Scenario: Dismiss
- **WHEN** the user presses esc
- **THEN** the overlay closes and focus returns to the previous screen

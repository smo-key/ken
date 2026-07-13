# ingest-recipes

## ADDED Requirements

### Requirement: Recipes are shareable text files
An ingest SHALL be defined by a markdown file in `.ken/ingests/` with YAML
frontmatter (`name`, `description`, `sources` folder list, `output` file or
folder path anywhere in the project, `mode: single | collection`,
`refresh: on-change | manual`, optional `rules` overrides) and a
plain-language instruction body. Ken SHALL parse recipes on project open
and when they change, and SHALL preserve unknown frontmatter fields when
rewriting a recipe.

#### Scenario: Recipe from a teammate just works
- **WHEN** a recipe file appears in `.ken/ingests/` (e.g. via Git pull)
- **THEN** it shows up in the Ingests list with its name, sources, output,
  and rules without any import step

#### Scenario: Invalid recipe is surfaced, not fatal
- **WHEN** a recipe has malformed frontmatter
- **THEN** the Ingests list shows it in an error state with a
  plain-language reason, and other ingests are unaffected

#### Scenario: Unknown fields survive editing
- **WHEN** a recipe containing a field Ken doesn't know is edited through
  the form
- **THEN** the rewritten file still contains that field

### Requirement: Form-based creation and editing
The Ingests screen SHALL offer creating and editing recipes through a form
(name, plain-words instruction, source folder selection, output location,
single-document vs collection toggle, refresh choice) that reads and
writes the recipe file. Users SHALL never be required to edit YAML.

#### Scenario: Create an ingest without touching YAML
- **WHEN** the user completes the New Ingest form
- **THEN** a valid recipe file exists in `.ken/ingests/` and the ingest
  appears in the list, without the user seeing frontmatter

#### Scenario: Edits round-trip
- **WHEN** the user edits the instruction in the detail view and saves
- **THEN** the recipe file body is updated and re-parsed

### Requirement: Template library
Ken SHALL ship bundled recipe templates — People, Requirements
(gold-standard), Decision log, Glossary, Meeting notes digest, FAQ,
Risks — and a browse-templates view. Using a template SHALL copy it into
`.ken/ingests/` as an ordinary recipe with no ongoing link to the
template.

#### Scenario: Start from a template
- **WHEN** the user picks "People" from the template gallery and confirms
  an output location
- **THEN** a `people` recipe is created in the project and is immediately
  editable like any hand-made recipe

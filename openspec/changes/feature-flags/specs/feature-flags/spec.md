# feature-flags Specification

## ADDED Requirements

### Requirement: Flag registry

ken-core SHALL define a static registry of implemented feature flags,
each with a name, scope (global, project, or workspace), default
value, and plain-language description. Command validation and UI
rendering SHALL both derive from the registry. Flags described in
product docs but not yet implemented SHALL NOT be registered, and
setting an unregistered flag SHALL be rejected with an error naming
the flag.

#### Scenario: unknown flag is rejected
- **WHEN** `set_project_feature` is called with name `"profiler"`
  and no `profiler` entry exists in the registry
- **THEN** the call fails with an error identifying `profiler` as
  unknown, and no file is written

### Requirement: Layered storage with fixed precedence

A flag's effective value SHALL be resolved as: project override
(`.ken/project.json` `features` map), then workspace override
(reserved — always absent until workspace ships), then global default
(`settings.json` `features` map in the app data home), then the
registry default. Global settings SHALL load as defaults when the
file is missing or corrupt, and both stores SHALL preserve unknown
keys on round-trip so older and newer Kens can share files.

#### Scenario: project override wins over global
- **WHEN** `settings.json` sets `semanticIndex: true` and the
  project's `.ken/project.json` sets `features.semanticIndex: false`
- **THEN** the effective value for that project is `false`

#### Scenario: registry default when nothing is set
- **WHEN** neither `settings.json` nor the project sets
  `semanticIndex`
- **THEN** the effective value is `false` (the registry default)

#### Scenario: older Ken round-trips the features map
- **WHEN** a Ken build without the typed `features` field saves a
  `.ken/project.json` containing `"features": {"semanticIndex": true}`
- **THEN** the map survives unchanged via the `extra` flatten

### Requirement: Legacy semanticIndex migration

The resolver SHALL honor the legacy top-level
`extra["semanticIndex"]` key in `.ken/project.json` when the typed
`features` map does not set the flag. The first
`set_project_feature("semanticIndex", ...)` write SHALL move the
value into `features` and delete the legacy key in the same save.
No other `extra` key SHALL be interpreted as a flag.

#### Scenario: legacy key still enables the feature
- **WHEN** `.ken/project.json` contains top-level
  `"semanticIndex": true` and no `features` map
- **THEN** `semanticIndex` resolves to `true` for that project

#### Scenario: write migrates the legacy key
- **WHEN** that project's flag is set to `true` via
  `set_project_feature`
- **THEN** the saved file has `features.semanticIndex: true` and no
  top-level `semanticIndex` key

### Requirement: Feature commands

The app SHALL expose `list_features` (registry metadata plus
per-layer and effective values, optionally for a project),
`set_project_feature` (any registered project-scoped flag — the
current semanticIndex-only restriction is removed), and
`set_global_feature` (any registered flag's global default).
`set_project_feature` SHALL keep the in-memory project in sync and
SHALL run per-flag side effects (`apply_semantic_index_flag` for
`semanticIndex`).

#### Scenario: enabling semanticIndex still starts indexing
- **WHEN** `set_project_feature(project, "semanticIndex", true)` is
  called and the embedding model is installed
- **THEN** the flag persists and semantic indexing begins for that
  project, identical to today's behavior

### Requirement: Onboarding and settings disclosure

Adding a project SHALL surface a collapsed "Features" disclosure
listing registered project-scoped flags with descriptions and
toggles, defaulting to each flag's effective value; choices are
persisted through `set_project_feature`. Settings SHALL expose
global defaults for all registered flags and allow changing a
project's overrides after onboarding. Flags SHALL appear in these
surfaces solely by being registered.

#### Scenario: onboarding shows only implemented flags
- **WHEN** the registry contains only `semanticIndex`
- **THEN** the Features disclosure lists exactly one toggle, with
  its description, defaulting to off

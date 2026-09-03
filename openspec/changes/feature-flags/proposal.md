# Feature flags

## Why

Ken is growing optional subsystems — semantic index today; workspace
mode, profiler, ken-families, and the knowledge-graph features next.
Each needs an opt-in switch, but no general mechanism exists: the only
flag that works today is `semanticIndex`, stored as an ad-hoc
top-level key in `.ken/project.json`'s untyped `extra` map, toggled by
a `set_project_feature` command that hard-rejects every other flag
name. The doc comment on `semantic_index_enabled`
(`src-tauri/src/lib.rs:249`) already admits the general layer is
missing.

`features/multi-project/README.md` (lines 81–119) specifies the
intended design: a flag table with per-flag scope, global defaults in
per-user app settings, per-project overrides in `.ken/project.json`,
a reserved workspace layer, and an onboarding disclosure so users
discover optional features when they add a project instead of hunting
through settings.

## What Changes

- **ken-core: flag registry.** A static registry of implemented flags
  (name, scope, default, plain-language description) as the single
  source of truth for validation and UI. Ships with one entry:
  `semanticIndex` (per-project, default off). Entries are added as
  features land; unimplemented README flags are *not* registered.
- **ken-core: global app settings.** New `settings.json` in the app
  data home (sibling of `models/`, `registry.json`) holding an
  `AppSettings` struct with a `features` map and a flattened `extra`
  passthrough. Load defaults on missing/corrupt, same pattern as
  `ModelSelection`.
- **ken-core: typed project overrides.** `ProjectConfig` gains a
  `#[serde(default)] features: Map<String, bool>` field. Older Kens
  round-trip it untouched via the existing `extra` flatten contract.
- **src-tauri: generalized commands.** `set_project_feature` accepts
  any registered project-scoped flag (not just `semanticIndex`); new
  `set_global_feature` and `list_features` (per-layer values +
  effective value + descriptions). Effective read precedence:
  project override > workspace (reserved, always absent today) >
  global default > registry default.
- **Migration.** Legacy top-level `extra["semanticIndex"]` is still
  honored on read (after the typed `features` map); the first write
  through `set_project_feature` moves it into `features` and removes
  the legacy key.
- **UI: onboarding disclosure.** The folder-select step
  (`src/onboarding/ProjectPicker.svelte`) shows a collapsed
  "Features" section listing registered per-project flags with their
  descriptions and toggles. Settings
  (`src/screens/SettingsScreen.svelte`) exposes global defaults and
  lets users change per-project flags later.

## Non-goals

- `backgroundIndex` (`bg_hydrate.rs`) stays outside the mechanism —
  it defaults **on** and is an operational knob, not a feature
  opt-in.
- `kenignore` has no flag by design: presence of the file is the
  opt-in.
- The workspace layer (`.ken-workspace/workspace.json`) is a reserved
  precedence slot only; no workspace code exists yet.

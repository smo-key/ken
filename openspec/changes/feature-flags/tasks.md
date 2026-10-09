# Tasks: feature flags

## 1. ken-core

- [x] 1.1 `features.rs` (new): `FlagScope`, `FlagDef`, `FLAGS`
      registry with the single `semanticIndex` entry (project scope,
      default false, user-facing description); `flag(name) ->
      Option<&FlagDef>` lookup; register module in
      `crates/ken-core/src/lib.rs`
- [x] 1.2 `settings.rs` (new): `AppSettings { features,
      #[serde(flatten)] extra }`, `settings_path(base_dir)` =
      `base_dir/settings.json`, `load` (defaults on missing/corrupt)
      and `save` (temp+rename) mirroring `ModelSelection`
      (`model.rs:315-330`); unit tests for missing file, corrupt
      file, unknown-key round-trip — save uses atomic temp+rename per
      D2; `ModelSelection` itself uses a plain `fs::write`, so this
      follows the spec's stated intent rather than that code verbatim
- [x] 1.3 `project.rs`: add `#[serde(default)] pub features:
      serde_json::Map<String, Value>` to `ProjectConfig`; extend the
      extra-preservation test (`project.rs:213`) to cover a
      `features` map written by a newer Ken
- [x] 1.4 `features.rs`: `effective_flag(app_settings, project,
      name) -> bool` implementing D3 precedence including the legacy
      `extra["semanticIndex"]` fallback; unit tests for each
      precedence layer and the legacy path

## 2. src-tauri

- [x] 2.1 `AppState`: hold `AppSettings` loaded at startup, same
      lock discipline as the project registry
- [x] 2.2 Rewrite `semantic_index_enabled` (`lib.rs:260`) as a call
      to `effective_flag`; delete the "mechanism doesn't exist yet"
      doc comment (`lib.rs:249-259`) — all 4 call sites now thread
      `app_settings` through
- [x] 2.3 Generalize `set_project_feature` (`lib.rs:1274`): registry
      validation replaces the semanticIndex-only check; write
      `features[name]`, remove legacy key on semanticIndex writes
      (D3), keep in-memory sync and `apply_semantic_index_flag`
      side effect — kept the existing `(flag, value)` signature
      operating on the active project (no `project_id` param, since
      the frontend is out of this backend scope and every write path
      already targets the open project)
- [x] 2.4 New commands `set_global_feature` and `list_features`
      (D4); register in the invoke handler — `list_features` resolves
      a non-active `project_id` from disk via the registry (no by-id
      project loader exists), matching `rename_project`

## 3. Frontend

- [x] 3.1 `src/lib/api.ts`: bindings for `list_features`,
      `set_global_feature`, generalized `set_project_feature`;
      shared `FeatureInfo` type
- [x] 3.2 `src/onboarding/ProjectPicker.svelte`: collapsed
      "Features" disclosure after folder selection — toggle +
      description per project-scoped flag, applied via
      `set_project_feature` after project creation; "change later in
      Settings" copy
- [x] 3.3 `src/screens/SettingsScreen.svelte`: Features section —
      global defaults for all registered flags, per-project
      overrides for the active project; two independent checkboxes
      per flag (global default, project override) rather than a
      tri-state default/on/off control, since `set_project_feature`
      has no "clear override" path to make a "revert to default"
      option honest; folds the existing semanticIndex toggle in
      rather than leaving a second, competing one

## 4. Verification

- [x] 4.1 `cargo check -p ken-app` and `cargo test -p ken-core`
      green (Vulkan/Ninja env per build recipe); new features/
      settings tests pass — 25/25 across features/settings/project;
      also fixed a pre-existing Windows path-escape bug in
      `Project::resolve` (`is_absolute` → `has_root`, since rooted
      drive-less paths like `/etc/passwd` escaped the project root
      via `join` on Windows)
- [ ] 4.2 Manual: fresh project → onboarding shows the semanticIndex
      toggle; enabling it round-trips to `.ken/project.json`
      `features` map; a legacy top-level `semanticIndex` project
      still works and migrates on first toggle

# Design: feature flags

## D1. Registry as single source of truth

A const table in ken-core (`features.rs`):

```rust
pub enum FlagScope { Global, Project, Workspace }

pub struct FlagDef {
    pub name: &'static str,        // camelCase, matches JSON keys
    pub scope: FlagScope,
    pub default: bool,
    pub description: &'static str, // plain language, shown in UI
}

pub const FLAGS: &[FlagDef] = &[
    FlagDef {
        name: "semanticIndex",
        scope: FlagScope::Project,
        default: false,
        description: "Meaning-based search using a local embedding \
                      model. Requires downloading Nomic Embed v1.5 \
                      (~140 MB).",
    },
];
```

Only implemented flags are registered. The README table
(`features/multi-project/README.md:81-91`) lists the eventual set
(workspace, profiler, federatedKg, kgRouting, kenMemory, kenTasks,
kenFamilies); each is added here in the change that implements it.
Validation in the commands and rendering in the UI both derive from
this table, so an unknown flag name is rejected in exactly one place
and the UI can never show a switch that does nothing.

## D2. Storage layers and precedence

Three layers, highest precedence first:

1. **Project** — `.ken/project.json` `"features": {"<flag>": bool}`.
   Typed field on `ProjectConfig` with `#[serde(default)]`; absent map
   means no overrides. Older Kens that don't know the field carry it
   through the `extra` flatten (the round-trip contract already tested
   at `project.rs:213`).
2. **Workspace** — `.ken-workspace/workspace.json` `features` map.
   *Reserved slot only.* No workspace concept exists in the codebase;
   the effective-value resolver has the hole in its precedence chain
   but nothing fills it yet.
3. **Global** — `settings.json` in the app data home, `AppSettings`:

```rust
#[derive(Serialize, Deserialize, Default)]
pub struct AppSettings {
    #[serde(default)]
    pub features: serde_json::Map<String, Value>, // bool values
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,    // forward-compat
}
```

Load/save mirrors `ModelSelection` (`model.rs:315-330`): defaults on
missing or corrupt file, atomic write via temp+rename. Path:
`base_dir/settings.json`.

Fallthrough: registry default when no layer sets the flag.

## D3. Effective read and legacy migration

`effective_flag(app_settings, project, name) -> bool`:

1. `project.config.features[name]` if present;
2. legacy: `project.config.extra[name]` if present **and** `name`
   is `"semanticIndex"` (the only flag ever written to the legacy
   location — don't let arbitrary extra keys masquerade as flags);
3. (workspace slot — always absent);
4. `app_settings.features[name]` if present;
5. registry default.

`semantic_index_enabled` in `src-tauri/src/lib.rs:260` becomes a thin
call to this resolver. Write path: `set_project_feature` writes
`features[name]` and, when `name == "semanticIndex"`, removes the
legacy `extra` key in the same save — migration happens lazily on
first write, never as a bulk rewrite of user files.

## D4. Commands

- `list_features(project_id: Option<String>)` → for each registered
  flag: name, scope, description, global value, project override
  (if a project is given and the flag is project-scoped), effective
  value. One command feeds both UI surfaces.
- `set_project_feature(project_id, name, value)` — generalized:
  validates `name` against the registry (must be project-scoped),
  persists to `.ken/project.json`, updates the in-memory project
  copy (existing behavior at `lib.rs:1298`), runs per-flag side
  effects (`apply_semantic_index_flag` for `semanticIndex`).
- `set_global_feature(name, value)` — validates against the registry
  (any scope: global-scoped flags live only here, and project-scoped
  flags use it as their default), persists `settings.json`, holds
  `AppSettings` in `AppState` behind the same lock discipline as the
  registry.

## D5. UI surfaces

- **Onboarding / add-project** (`src/onboarding/ProjectPicker.svelte`):
  after folder selection, a collapsed "Features" disclosure listing
  project-scoped registered flags — name, description, toggle,
  defaulting to the effective value. Selections are written through
  `set_project_feature` once the project is created. Copy notes the
  flags can be changed later in Settings.
- **Settings** (`src/screens/SettingsScreen.svelte`): a Features
  section showing global defaults (all scopes) and, for the active
  project, its overrides. Backed entirely by `list_features` /
  `set_*_feature` via `src/lib/api.ts`.

Only `semanticIndex` renders today; the sections grow automatically
as registry entries land.

## D6. Out of scope

- `backgroundIndex` keeps its own convention (`bg_hydrate.rs`,
  default on) — it is not user-facing feature adoption.
- No flag for `kenignore` (file presence is the opt-in, per the
  kenignore change).
- Workspace flag storage, AppMode (single vs multi project), and
  ken-families ship in their own changes; this change only reserves
  their precedence slot and registry scope variant.

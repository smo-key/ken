//! Feature-flag registry and effective-value resolution. The registry is the
//! single source of truth: command validation and UI rendering both derive
//! from it, so an unknown flag is rejected in one place and no switch can be
//! shown that does nothing. Only *implemented* flags are registered — product
//! docs may list more, but each lands here in the change that ships it (e.g.
//! `profiler`, registered by the project-profiler change's ken-core layer;
//! its src-tauri/frontend wiring is a later phase of that same change).

use serde_json::Value;

use crate::project::Project;
use crate::settings::AppSettings;

/// Which storage layer a flag's default lives in. `Workspace` is a reserved
/// precedence slot only — no flag reads from it yet (it will apply to
/// per-member overrides once the workspace manifest carries them).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlagScope {
    Global,
    Project,
    Workspace,
}

pub struct FlagDef {
    /// camelCase, matches the JSON keys in `settings.json` / `project.json`.
    pub name: &'static str,
    pub scope: FlagScope,
    pub default: bool,
    /// Plain language, shown in the UI.
    pub description: &'static str,
    /// The flag's plain name in Settings (`semanticIndex` is "Search by
    /// meaning"); the code name stays the key.
    pub label: &'static str,
    /// Where it applies, in the reader's words: this repo, this machine,
    /// the team, or mine.
    pub applies: &'static str,
}

pub const FLAGS: &[FlagDef] = &[
    FlagDef {
        name: "semanticIndex",
        label: "Search by meaning",
        applies: "this repo",
        scope: FlagScope::Project,
        default: true,
        description: "Finds passages by meaning as well as by keyword, \
                      with a model that runs on this computer. It needs a \
                      search-by-meaning model from Settings › AI; \
                      until one is installed, search is by keyword.",
    },
    FlagDef {
        name: "profiler",
        label: "Repo profile",
        applies: "this repo",
        scope: FlagScope::Project,
        default: false,
        description: "Analyze each project's shape (deterministic scan, \
                      optional local-LLM refinement) to tune semantic-index \
                      chunking, exclusions, and knowledge extraction.",
    },
    FlagDef {
        name: "backgroundExtraction",
        label: "Background reading",
        applies: "this machine",
        scope: FlagScope::Global,
        default: true,
        description: "Read new files with the local model in the background \
                      so the Map and Timeline fill in on their own. On by \
                      default. Turn it off to leave the GPU alone — queued \
                      files stay queued and resume when it goes back on.",
    },
];

/// What were flags and are now how Ken works: a workspace of several repos,
/// its knowledge graph, search routed across it, and Ken's memory. Always on; a value an
/// older `settings.json` still holds for one is ignored, and none is shown.
pub const BUILT_IN: &[&str] = &["workspace", "federatedKg", "kgRouting", "kenMemory"];

/// Is `name` one of the [`BUILT_IN`] features, always on?
pub fn built_in(name: &str) -> bool {
    BUILT_IN.contains(&name)
}

/// Registry lookup by name. `None` means the flag is not implemented and any
/// attempt to set it must be rejected.
pub fn flag(name: &str) -> Option<&'static FlagDef> {
    FLAGS.iter().find(|f| f.name == name)
}

/// Resolve a flag's effective value with fixed precedence (highest first):
///   1. project `features` map;
///   2. legacy top-level `extra["semanticIndex"]` — *only* for
///      `"semanticIndex"`, the sole flag ever written to the legacy location
///      (arbitrary `extra` keys must not masquerade as flags);
///   3. workspace layer (reserved — always absent today);
///   4. global `settings.json` `features` map;
///   5. registry default (false for an unregistered name).
pub fn effective_flag(app_settings: &AppSettings, project: &Project, name: &str) -> bool {
    if built_in(name) {
        return true;
    }
    if let Some(v) = project.config.features.get(name).and_then(Value::as_bool) {
        return v;
    }
    if name == "semanticIndex" {
        if let Some(v) = project.config.extra.get(name).and_then(Value::as_bool) {
            return v;
        }
    }
    // (workspace slot — nothing fills it yet)
    if let Some(v) = app_settings.features.get(name).and_then(Value::as_bool) {
        return v;
    }
    flag(name).map(|f| f.default).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::ProjectConfig;
    use serde_json::Map;
    use std::path::PathBuf;

    fn project_with(
        features: Map<String, Value>,
        extra: Map<String, Value>,
    ) -> Project {
        Project {
            root: PathBuf::from("/tmp/x"),
            config: ProjectConfig {
                name: "T".into(),
                id: uuid::Uuid::nil(),
                excluded: Vec::new(),
                features,
                extra,
            },
        }
    }

    fn settings_with(features: Map<String, Value>) -> AppSettings {
        AppSettings { features, extra: Map::new() }
    }

    #[test]
    fn registry_has_semantic_index_workspace_and_profiler() {
        assert_eq!(FLAGS.len(), 3);
        assert!(flag("semanticIndex").is_some());
        for name in BUILT_IN {
            assert!(flag(name).is_none(), "{name} is how Ken works, not a switch");
        }
        // The only flag in the registry that defaults ON: background
        // extraction is the behaviour Ken shipped with, and the switch exists
        // to stop it rather than to start it.
        let bg_extraction = flag("backgroundExtraction").expect("backgroundExtraction registered");
        assert_eq!(bg_extraction.scope, FlagScope::Global);
        assert!(bg_extraction.default);
        let profiler = flag("profiler").expect("profiler flag registered");
        assert_eq!(profiler.scope, FlagScope::Project);
        assert!(!profiler.default);
        // The task board and the team inbox are how Ken works now (Your day,
        // the inbox once one is set up), not switches.
        assert!(flag("kenTasks").is_none());
        assert!(flag("kenFamilies").is_none());
    }

    #[test]
    fn built_in_features_are_on_whatever_settings_say() {
        let project = project_with(Map::new(), Map::new());
        let mut off = Map::new();
        for name in BUILT_IN {
            off.insert(name.to_string(), Value::Bool(false));
        }
        let settings = settings_with(off);
        for name in BUILT_IN {
            assert!(effective_flag(&settings, &project, name), "{name}");
        }
    }

    #[test]
    fn registry_default_when_nothing_set() {
        let project = project_with(Map::new(), Map::new());
        let settings = AppSettings::default();
        assert!(effective_flag(&settings, &project, "semanticIndex"), "meaning search is on unless turned off");
    }

    #[test]
    fn global_default_used_when_project_silent() {
        let project = project_with(Map::new(), Map::new());
        let mut features = Map::new();
        features.insert("semanticIndex".into(), false.into());
        let settings = settings_with(features);
        assert!(!effective_flag(&settings, &project, "semanticIndex"));
    }

    #[test]
    fn project_override_wins_over_global() {
        let mut pf = Map::new();
        pf.insert("semanticIndex".into(), false.into());
        let project = project_with(pf, Map::new());
        let mut gf = Map::new();
        gf.insert("semanticIndex".into(), true.into());
        let settings = settings_with(gf);
        assert!(!effective_flag(&settings, &project, "semanticIndex"));
    }

    #[test]
    fn legacy_extra_key_still_enables() {
        let mut extra = Map::new();
        extra.insert("semanticIndex".into(), true.into());
        let project = project_with(Map::new(), extra);
        assert!(effective_flag(&AppSettings::default(), &project, "semanticIndex"));
    }

    #[test]
    fn features_map_wins_over_legacy_extra() {
        let mut pf = Map::new();
        pf.insert("semanticIndex".into(), false.into());
        let mut extra = Map::new();
        extra.insert("semanticIndex".into(), true.into());
        let project = project_with(pf, extra);
        assert!(!effective_flag(&AppSettings::default(), &project, "semanticIndex"));
    }

    #[test]
    fn legacy_extra_beats_global() {
        let mut extra = Map::new();
        extra.insert("semanticIndex".into(), false.into());
        let project = project_with(Map::new(), extra);
        let mut gf = Map::new();
        gf.insert("semanticIndex".into(), true.into());
        let settings = settings_with(gf);
        assert!(!effective_flag(&settings, &project, "semanticIndex"));
    }

    #[test]
    fn arbitrary_extra_key_is_not_a_flag() {
        // Only "semanticIndex" is honored from `extra`; any other key there
        // must not be treated as a flag override.
        let mut extra = Map::new();
        extra.insert("someOtherKey".into(), true.into());
        let project = project_with(Map::new(), extra);
        assert!(!effective_flag(&AppSettings::default(), &project, "someOtherKey"));
    }
}

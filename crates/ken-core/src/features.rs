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
}

pub const FLAGS: &[FlagDef] = &[
    FlagDef {
        name: "semanticIndex",
        scope: FlagScope::Project,
        default: false,
        description: "Meaning-based search using a local embedding model. \
                      Requires downloading Nomic Embed v1.5 (~140 MB).",
    },
    FlagDef {
        name: "workspace",
        scope: FlagScope::Global,
        default: false,
        description: "Open a parent folder's sibling projects together, \
                      each with its own DB, engine, and watcher.",
    },
    FlagDef {
        name: "profiler",
        scope: FlagScope::Project,
        default: false,
        description: "Analyze each project's shape (deterministic scan, \
                      optional local-LLM refinement) to tune semantic-index \
                      chunking, exclusions, and knowledge extraction.",
    },
    FlagDef {
        name: "federatedKg",
        scope: FlagScope::Workspace,
        default: false,
        description: "Build a workspace-wide knowledge graph that federates \
                      each open member's knowledge model into merged \
                      entities, cross-project edges, and a wiki-page view. \
                      Requires the workspace flag.",
    },
    FlagDef {
        name: "kgRouting",
        scope: FlagScope::Workspace,
        default: false,
        description: "Route search across workspace members: aim directly \
                      at a named project, via the workspace knowledge graph \
                      when it isn't named, or broadcast when neither \
                      applies — every result cited with a stable address. \
                      Requires the workspace flag.",
    },
    FlagDef {
        name: "kenMemory",
        scope: FlagScope::Workspace,
        default: false,
        description: "Ken's own working memory: long-term memory files, a \
                      daily journal, and an approval-gated promotion pass, \
                      indexed by a reserved workspace pseudo-member and \
                      injected into chat context. Requires the workspace \
                      flag.",
    },
    FlagDef {
        name: "kenTasks",
        scope: FlagScope::Workspace,
        default: false,
        description: "A shared task board (Kanban + a daily view) backed by \
                      markdown files in the workspace and per-project \
                      homes, with overarching goals, drag-drop status, and \
                      MCP claim/complete tools for agent handoff. Requires \
                      the workspace flag.",
    },
    FlagDef {
        name: "kenFamilies",
        scope: FlagScope::Global,
        default: false,
        description: "Team collaboration over a shared git repo: clone, \
                      poll, and push a templated family repo that delivers \
                      tasks and messages to teammates' inboxes and keeps \
                      each member's board visible to the team. Uses the \
                      system git CLI; no accounts, no server.",
    },
    FlagDef {
        name: "kenPipeline",
        scope: FlagScope::Workspace,
        default: false,
        description: "A configurable multi-lane pipeline board layered over \
                      the task board: custom lanes, agent kickoff per lane, \
                      run records, blockers, and a digest of what needs \
                      attention. Requires the workspace and kenTasks flags.",
    },
];

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
        assert_eq!(FLAGS.len(), 9);
        assert!(flag("semanticIndex").is_some());
        assert!(flag("workspace").is_some());
        let profiler = flag("profiler").expect("profiler flag registered");
        assert_eq!(profiler.scope, FlagScope::Project);
        assert!(!profiler.default);
        let federated_kg = flag("federatedKg").expect("federatedKg flag registered");
        assert_eq!(federated_kg.scope, FlagScope::Workspace);
        assert!(!federated_kg.default);
        let kg_routing = flag("kgRouting").expect("kgRouting flag registered");
        assert_eq!(kg_routing.scope, FlagScope::Workspace);
        assert!(!kg_routing.default);
        let ken_memory = flag("kenMemory").expect("kenMemory flag registered");
        assert_eq!(ken_memory.scope, FlagScope::Workspace);
        assert!(!ken_memory.default);
        let ken_tasks = flag("kenTasks").expect("kenTasks flag registered");
        assert_eq!(ken_tasks.scope, FlagScope::Workspace);
        assert!(!ken_tasks.default);
        let ken_families = flag("kenFamilies").expect("kenFamilies flag registered");
        assert_eq!(ken_families.scope, FlagScope::Global);
        assert!(!ken_families.default);
        let ken_pipeline = flag("kenPipeline").expect("kenPipeline flag registered");
        assert_eq!(ken_pipeline.scope, FlagScope::Workspace);
        assert!(!ken_pipeline.default);
    }

    #[test]
    fn registry_default_when_nothing_set() {
        let project = project_with(Map::new(), Map::new());
        let settings = AppSettings::default();
        assert!(!effective_flag(&settings, &project, "semanticIndex"));
    }

    #[test]
    fn global_default_used_when_project_silent() {
        let project = project_with(Map::new(), Map::new());
        let mut features = Map::new();
        features.insert("semanticIndex".into(), true.into());
        let settings = settings_with(features);
        assert!(effective_flag(&settings, &project, "semanticIndex"));
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

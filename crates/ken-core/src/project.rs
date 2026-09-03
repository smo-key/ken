//! Project lifecycle: `.ken/project.json` inside the project folder is the
//! shared, text-only source of truth. Unknown fields are preserved on
//! rewrite so newer Ken versions (or teammates' configs) aren't clobbered.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Error, Result};

pub const CONFIG_DIR: &str = ".ken";
pub const CONFIG_FILE: &str = "project.json";

/// Upper bound on a project name's length. A name is a display label, not an
/// identifier, so this is generous — it only exists to reject pathological
/// input, not to shape naming.
pub const NAME_MAX_LEN: usize = 200;

/// Trim and validate a user-supplied project name, returning the normalized
/// (trimmed) form. Names are single-line display labels: control characters
/// (newlines, tabs) would break the switcher and title bar, and empty names
/// leave nothing to show, so both are rejected.
pub fn normalize_name(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(Error::Other("project name cannot be empty".into()));
    }
    if trimmed.chars().count() > NAME_MAX_LEN {
        return Err(Error::Other(format!(
            "project name is too long (max {NAME_MAX_LEN} characters)"
        )));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(Error::Other(
            "project name cannot contain control characters".into(),
        ));
    }
    Ok(trimmed.to_string())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectConfig {
    pub name: String,
    pub id: Uuid,
    /// Project-relative folder paths excluded from ingestion. Default: none
    /// (everything is included).
    #[serde(default)]
    pub excluded: Vec<String>,
    /// Per-project feature-flag overrides; bool values keyed by flag name.
    /// Absent map means no overrides. Older Kens that don't know this field
    /// carry it through the `extra` flatten below.
    #[serde(default)]
    pub features: serde_json::Map<String, serde_json::Value>,
    /// Fields written by newer versions or other capabilities survive a
    /// round-trip through this one.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl ProjectConfig {
    /// Short display symbol (1-3 characters or an emoji), rendered
    /// top-left on every board card carrying this project (design D12,
    /// `ken-pipeline`). Lives in `extra`, not a typed field — per OPEN-2,
    /// this keeps `project.json` round-tripping through older Ken
    /// untouched; there is no schema change to make.
    pub fn symbol(&self) -> Option<&str> {
        self.extra.get("symbol").and_then(|v| v.as_str())
    }

    /// Optional display colour paired with `symbol`. Same `extra`-only
    /// treatment as `symbol` — see OPEN-2.
    pub fn color(&self) -> Option<&str> {
        self.extra.get("color").and_then(|v| v.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct Project {
    pub root: PathBuf,
    pub config: ProjectConfig,
}

pub fn config_path(root: &Path) -> PathBuf {
    root.join(CONFIG_DIR).join(CONFIG_FILE)
}

impl Project {
    /// Create a new project in an existing folder. If the folder already has
    /// a `.ken/project.json` (e.g. cloned from a teammate), it is adopted
    /// unchanged — same id, same settings.
    pub fn create(root: &Path, name: &str) -> Result<Project> {
        if !root.is_dir() {
            return Err(Error::ProjectMissing(root.to_path_buf()));
        }
        if config_path(root).exists() {
            return Project::open(root);
        }
        let config = ProjectConfig {
            name: name.to_string(),
            id: Uuid::new_v4(),
            excluded: Vec::new(),
            features: serde_json::Map::new(),
            extra: serde_json::Map::new(),
        };
        let project = Project {
            root: root.to_path_buf(),
            config,
        };
        project.save()?;
        Ok(project)
    }

    /// Open a folder that already contains `.ken/project.json`.
    pub fn open(root: &Path) -> Result<Project> {
        let path = config_path(root);
        let raw = fs::read_to_string(&path).map_err(|e| {
            if !root.is_dir() {
                Error::ProjectMissing(root.to_path_buf())
            } else {
                Error::io(&path, e)
            }
        })?;
        let config: ProjectConfig =
            serde_json::from_str(&raw).map_err(|e| Error::InvalidProject {
                path: path.clone(),
                reason: e.to_string(),
            })?;
        Ok(Project {
            root: root.to_path_buf(),
            config,
        })
    }

    pub fn save(&self) -> Result<()> {
        let dir = self.root.join(CONFIG_DIR);
        fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
        let path = config_path(&self.root);
        let json = serde_json::to_string_pretty(&self.config)
            .map_err(|e| Error::Other(e.to_string()))?;
        fs::write(&path, json + "\n").map_err(|e| Error::io(&path, e))
    }

    /// Is a project-relative path inside an excluded folder?
    pub fn is_excluded(&self, rel_path: &str) -> bool {
        let rel = rel_path.trim_start_matches('/');
        self.config.excluded.iter().any(|ex| {
            let ex = ex.trim_matches('/');
            !ex.is_empty() && (rel == ex || rel.starts_with(&format!("{ex}/")))
        })
    }

    /// Load and parse this project's `.kenignore` (project root, not
    /// `.ken/`) into rules per kenignore design D6/D1. Missing file reads as
    /// empty rules, not an error — most projects won't have one. This is the
    /// "user rule set" tier in D2's precedence order; callers combine it with
    /// any built-in rule sets (task 1.3) via `kenignore::classify`'s
    /// `rule_sets` slice, user rules last so they can override built-ins.
    pub fn kenignore_rules(&self) -> Vec<crate::kenignore::Rule> {
        let path = self.root.join(".kenignore");
        let text = fs::read_to_string(&path).unwrap_or_default();
        crate::kenignore::parse(&text)
    }

    /// Rename the project, rewriting `.ken/project.json`. The invalid-name
    /// check runs before any write, so a rejected name leaves the config
    /// untouched. The user-level registry is a separate store the caller
    /// updates alongside this.
    pub fn set_name(&mut self, name: &str) -> Result<()> {
        let name = normalize_name(name)?;
        self.config.name = name;
        self.save()
    }

    pub fn set_excluded(&mut self, excluded: Vec<String>) -> Result<()> {
        self.config.excluded = excluded;
        self.save()
    }

    /// Effective exclusion set (project-profiler D3): user `excluded` ∪ the
    /// stored profile's `excludes`, additive only — a profile exclude never
    /// replaces or removes a user entry. `profiler_enabled` is the caller's
    /// already-resolved `profiler` flag value (see `features::effective_flag`);
    /// this method has no `AppSettings` access of its own, so passing `false`
    /// reproduces plain `excluded`-only behavior exactly, which is how
    /// flag-off inertness holds even when a profile file is present on disk
    /// (spec: "flag off is inert").
    pub fn effective_excluded(&self, profiler_enabled: bool) -> Vec<String> {
        let mut set = self.config.excluded.clone();
        if profiler_enabled {
            let profile = crate::profiler::ProjectProfile::load(&self.root);
            for ex in profile.excludes {
                if !set.iter().any(|e| e == &ex) {
                    set.push(ex);
                }
            }
        }
        set
    }

    /// Resolve a project-relative path, refusing anything that escapes the
    /// project root (`..`, absolute paths).
    pub fn resolve(&self, rel_path: &str) -> Result<PathBuf> {
        let rel = Path::new(rel_path);
        // has_root() rather than is_absolute(): on Windows "/etc/passwd" is
        // rooted but not absolute, yet join() would still escape the project.
        if rel.has_root()
            || rel
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(Error::PathOutsideProject(rel.to_path_buf()));
        }
        Ok(self.root.join(rel))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn create_writes_config_and_roundtrips() {
        let dir = tempdir().unwrap();
        let p = Project::create(dir.path(), "Atlas Migration").unwrap();
        assert!(config_path(dir.path()).exists());

        let reopened = Project::open(dir.path()).unwrap();
        assert_eq!(reopened.config, p.config);
        assert_eq!(reopened.config.name, "Atlas Migration");
    }

    #[test]
    fn create_adopts_existing_config() {
        let dir = tempdir().unwrap();
        let first = Project::create(dir.path(), "Original").unwrap();
        // A second create (e.g. teammate opening a cloned folder) adopts.
        let second = Project::create(dir.path(), "Renamed").unwrap();
        assert_eq!(second.config.id, first.config.id);
        assert_eq!(second.config.name, "Original");
    }

    #[test]
    fn unknown_fields_survive_roundtrip() {
        let dir = tempdir().unwrap();
        let p = Project::create(dir.path(), "X").unwrap();
        // Simulate a newer version adding a field.
        let path = config_path(dir.path());
        let mut v: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        v["ingestRunner"] = "hidden-tui".into();
        // A newer Ken also wrote a features map, including a flag this build
        // doesn't recognize by name. It lives in the typed `features` field but
        // must still survive a save round-trip unchanged.
        v["features"] = serde_json::json!({ "semanticIndex": true, "futureFlag": true });
        fs::write(&path, serde_json::to_string(&v).unwrap()).unwrap();

        let mut reopened = Project::open(dir.path()).unwrap();
        assert_eq!(
            reopened.config.features.get("semanticIndex").and_then(|x| x.as_bool()),
            Some(true)
        );
        assert_eq!(
            reopened.config.features.get("futureFlag").and_then(|x| x.as_bool()),
            Some(true)
        );
        reopened.set_excluded(vec!["archive".into()]).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("ingestRunner"), "extra field lost: {raw}");
        assert!(raw.contains("futureFlag"), "features map lost: {raw}");
        drop(p);
    }

    #[test]
    fn symbol_and_color_read_from_extra_when_present() {
        let dir = tempdir().unwrap();
        let mut p = Project::create(dir.path(), "X").unwrap();
        p.config.extra.insert("symbol".into(), "SR".into());
        p.config.extra.insert("color".into(), "#5566ee".into());
        assert_eq!(p.config.symbol(), Some("SR"));
        assert_eq!(p.config.color(), Some("#5566ee"));
    }

    #[test]
    fn symbol_and_color_absent_when_not_set() {
        let dir = tempdir().unwrap();
        let p = Project::create(dir.path(), "X").unwrap();
        assert_eq!(p.config.symbol(), None);
        assert_eq!(p.config.color(), None);
    }

    #[test]
    fn symbol_survives_roundtrip_via_extra() {
        let dir = tempdir().unwrap();
        let path = config_path(dir.path());
        let p = Project::create(dir.path(), "X").unwrap();
        let mut v: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        v["symbol"] = "SR".into();
        fs::write(&path, serde_json::to_string(&v).unwrap()).unwrap();

        let reopened = Project::open(dir.path()).unwrap();
        assert_eq!(reopened.config.symbol(), Some("SR"));
        reopened.save().unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("\"symbol\""), "symbol lost: {raw}");
        drop(p);
    }

    #[test]
    fn exclusion_matches_folders_not_prefixes() {
        let dir = tempdir().unwrap();
        let mut p = Project::create(dir.path(), "X").unwrap();
        p.config.excluded = vec!["archive".into()];
        assert!(p.is_excluded("archive/old.md"));
        assert!(p.is_excluded("archive"));
        assert!(!p.is_excluded("archive-2/notes.md"));
        assert!(!p.is_excluded("notes/archive.md"));
    }

    #[test]
    fn effective_excluded_unions_profile_excludes_only_when_enabled() {
        let dir = tempdir().unwrap();
        let mut p = Project::create(dir.path(), "X").unwrap();
        p.config.excluded = vec!["archive".into()];

        // No profile file yet: enabled or not, effective set is just user excluded.
        assert_eq!(p.effective_excluded(true), vec!["archive".to_string()]);
        assert_eq!(p.effective_excluded(false), vec!["archive".to_string()]);

        let mut profile = crate::profiler::ProjectProfile::default();
        profile.excludes = vec!["target/".to_string()];
        profile.save(&p.root).unwrap();

        // Flag off: the profile file is not read at all (spec: flag off is inert).
        assert_eq!(p.effective_excluded(false), vec!["archive".to_string()]);
        // Flag on: additive union, user entry first.
        assert_eq!(p.effective_excluded(true), vec!["archive".to_string(), "target/".to_string()]);
    }

    #[test]
    fn effective_excluded_never_duplicates_an_overlapping_entry() {
        let dir = tempdir().unwrap();
        let mut p = Project::create(dir.path(), "X").unwrap();
        p.config.excluded = vec!["target/".to_string()];
        let mut profile = crate::profiler::ProjectProfile::default();
        profile.excludes = vec!["target/".to_string()];
        profile.save(&p.root).unwrap();

        assert_eq!(p.effective_excluded(true), vec!["target/".to_string()]);
    }

    #[test]
    fn resolve_rejects_escapes() {
        let dir = tempdir().unwrap();
        let p = Project::create(dir.path(), "X").unwrap();
        assert!(p.resolve("notes/a.md").is_ok());
        assert!(p.resolve("../outside.md").is_err());
        assert!(p.resolve("/etc/passwd").is_err());
    }

    #[test]
    fn open_missing_folder_errors() {
        let err = Project::open(Path::new("/nonexistent/ken-test")).unwrap_err();
        assert!(matches!(err, Error::ProjectMissing(_)));
    }

    #[test]
    fn normalize_name_trims_and_accepts() {
        assert_eq!(normalize_name("  Atlas Migration  ").unwrap(), "Atlas Migration");
        assert_eq!(normalize_name("Q3 Planning").unwrap(), "Q3 Planning");
    }

    #[test]
    fn normalize_name_rejects_empty() {
        assert!(normalize_name("").is_err());
        assert!(normalize_name("   ").is_err());
        assert!(normalize_name("\t\n").is_err());
    }

    #[test]
    fn normalize_name_rejects_control_chars() {
        // Newlines/tabs would break the single-line switcher and title bar.
        assert!(normalize_name("Line one\nLine two").is_err());
        assert!(normalize_name("tab\there").is_err());
    }

    #[test]
    fn normalize_name_rejects_overlong() {
        let long = "x".repeat(NAME_MAX_LEN + 1);
        assert!(normalize_name(&long).is_err());
        assert!(normalize_name(&"x".repeat(NAME_MAX_LEN)).is_ok());
    }

    #[test]
    fn set_name_rewrites_config() {
        let dir = tempdir().unwrap();
        Project::create(dir.path(), "Original").unwrap();
        let mut p = Project::open(dir.path()).unwrap();
        p.set_name("  Renamed  ").unwrap();
        // Trimmed on write, and durable across a reopen.
        assert_eq!(p.config.name, "Renamed");
        assert_eq!(Project::open(dir.path()).unwrap().config.name, "Renamed");
    }

    #[test]
    fn set_name_rejects_invalid_and_leaves_config() {
        let dir = tempdir().unwrap();
        let mut p = Project::create(dir.path(), "Keep").unwrap();
        assert!(p.set_name("   ").is_err());
        assert_eq!(p.config.name, "Keep");
        assert_eq!(Project::open(dir.path()).unwrap().config.name, "Keep");
    }
}

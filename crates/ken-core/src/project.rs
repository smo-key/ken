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
    /// Fields written by newer versions or other capabilities survive a
    /// round-trip through this one.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
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

    /// Resolve a project-relative path, refusing anything that escapes the
    /// project root (`..`, absolute paths).
    pub fn resolve(&self, rel_path: &str) -> Result<PathBuf> {
        let rel = Path::new(rel_path);
        if rel.is_absolute()
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
        fs::write(&path, serde_json::to_string(&v).unwrap()).unwrap();

        let mut reopened = Project::open(dir.path()).unwrap();
        reopened.set_excluded(vec!["archive".into()]).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("ingestRunner"), "extra field lost: {raw}");
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

//! Local registry of known projects, kept in the OS app-data directory
//! (`<data-dir>/ken/projects.json`). Purely local — never synced; losing it
//! costs nothing but re-opening folders.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::project::Project;
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryEntry {
    pub id: Uuid,
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub projects: Vec<RegistryEntry>,
    /// The project that was open last — reopened on launch.
    #[serde(default, rename = "lastProject", skip_serializing_if = "Option::is_none")]
    pub last_project: Option<Uuid>,
}

/// A registry entry plus whether its folder still exists on disk.
#[derive(Debug, Clone, Serialize)]
pub struct RegistryEntryStatus {
    #[serde(flatten)]
    pub entry: RegistryEntry,
    pub available: bool,
}

/// Default app-data base directory (`~/Library/Application Support/ken` on
/// macOS). A `KEN_DATA_DIR` environment variable overrides it — that's how
/// tests isolate themselves and how power users relocate app data. All
/// registry/db functions still take the base explicitly.
pub fn default_base_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("KEN_DATA_DIR").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    dirs::data_dir()
        .map(|d| d.join("ken"))
        .ok_or_else(|| Error::Other("no OS data directory available".into()))
}

fn registry_path(base: &Path) -> PathBuf {
    base.join("projects.json")
}

impl Registry {
    pub fn load(base: &Path) -> Result<Registry> {
        let path = registry_path(base);
        if !path.exists() {
            return Ok(Registry::default());
        }
        let raw = fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
        serde_json::from_str(&raw).map_err(|e| Error::Other(format!("bad registry: {e}")))
    }

    pub fn save(&self, base: &Path) -> Result<()> {
        fs::create_dir_all(base).map_err(|e| Error::io(base, e))?;
        let path = registry_path(base);
        let json =
            serde_json::to_string_pretty(self).map_err(|e| Error::Other(e.to_string()))?;
        fs::write(&path, json + "\n").map_err(|e| Error::io(&path, e))
    }

    /// Register (or re-register) a project. Same id updates path/name in
    /// place — e.g. a moved folder or a teammate's clone with the shared id.
    pub fn add(&mut self, project: &Project) {
        let entry = RegistryEntry {
            id: project.config.id,
            name: project.config.name.clone(),
            path: project.root.clone(),
        };
        match self.projects.iter_mut().find(|e| e.id == entry.id) {
            Some(existing) => *existing = entry,
            None => self.projects.push(entry),
        }
    }

    pub fn remove(&mut self, id: Uuid) {
        self.projects.retain(|e| e.id != id);
    }

    /// Entries with availability (does the folder still exist?).
    pub fn statuses(&self) -> Vec<RegistryEntryStatus> {
        self.projects
            .iter()
            .map(|e| RegistryEntryStatus {
                entry: e.clone(),
                available: e.path.is_dir(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn add_save_load_roundtrip() {
        let app = tempdir().unwrap();
        let proj_dir = tempdir().unwrap();
        let project = Project::create(proj_dir.path(), "Atlas").unwrap();

        let mut reg = Registry::load(app.path()).unwrap();
        assert!(reg.projects.is_empty());
        reg.add(&project);
        reg.save(app.path()).unwrap();

        let loaded = Registry::load(app.path()).unwrap();
        assert_eq!(loaded.projects.len(), 1);
        assert_eq!(loaded.projects[0].id, project.config.id);
    }

    #[test]
    fn re_add_same_id_updates_in_place() {
        let proj_dir = tempdir().unwrap();
        let project = Project::create(proj_dir.path(), "Atlas").unwrap();

        let mut reg = Registry::default();
        reg.add(&project);
        let mut moved = project.clone();
        moved.root = PathBuf::from("/somewhere/else");
        reg.add(&moved);
        assert_eq!(reg.projects.len(), 1);
        assert_eq!(reg.projects[0].path, PathBuf::from("/somewhere/else"));
    }

    #[test]
    fn missing_path_detected() {
        let proj_dir = tempdir().unwrap();
        let project = Project::create(proj_dir.path(), "Atlas").unwrap();
        let mut reg = Registry::default();
        reg.add(&project);

        assert!(reg.statuses()[0].available);
        drop(proj_dir); // folder deleted
        assert!(!reg.statuses()[0].available);
    }

    #[test]
    fn ken_data_dir_overrides_base_dir() {
        // No other test reads KEN_DATA_DIR, so mutating it here is safe.
        std::env::set_var("KEN_DATA_DIR", "/tmp/ken-test-base");
        assert_eq!(
            default_base_dir().unwrap(),
            PathBuf::from("/tmp/ken-test-base")
        );
        std::env::remove_var("KEN_DATA_DIR");
        let default = default_base_dir().unwrap();
        assert!(default.ends_with("ken"), "unexpected default: {default:?}");
    }

    #[test]
    fn remove_entry() {
        let proj_dir = tempdir().unwrap();
        let project = Project::create(proj_dir.path(), "Atlas").unwrap();
        let mut reg = Registry::default();
        reg.add(&project);
        reg.remove(project.config.id);
        assert!(reg.projects.is_empty());
    }
}

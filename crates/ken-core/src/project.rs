//! Project lifecycle: `.ken/project.json` inside the project folder is the
//! shared, text-only source of truth. Unknown fields are preserved on
//! rewrite so newer Ken versions (or teammates' configs) aren't clobbered.
//!
//! A repo Ken must leave untouched (a code or reference repo: Ken never
//! commits into it, and it should gain no file from Ken) keeps the same
//! config in Ken's app data instead, keyed by its folder
//! ([`outside_config_path`]); [`Project::create_outside`] makes one, and
//! [`Project::open`] and [`Project::save`] find it when the repo has no
//! `.ken/project.json` of its own.

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

/// Where a repo that gets no file from Ken keeps its config: Ken's app data,
/// `project-configs/<hash of the folder>.json`. None when the app-data
/// folder cannot be found.
pub fn outside_config_path(root: &Path) -> Option<PathBuf> {
    let canon = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let key = canon.to_string_lossy().trim_start_matches(r"\\?\").to_lowercase().replace('\\', "/");
    let hash = twox_hash::XxHash64::oneshot(0x4B45_4E43_4647, key.as_bytes());
    crate::registry::default_base_dir().ok().map(|b| b.join("project-configs").join(format!("{hash:016x}.json")))
}

/// The config file a project uses: its own `.ken/project.json`, else the
/// one kept for it in app data, when either exists.
pub fn existing_config(root: &Path) -> Option<PathBuf> {
    let inside = config_path(root);
    if inside.exists() {
        return Some(inside);
    }
    outside_config_path(root).filter(|p| p.exists())
}

fn write_config(path: &Path, config: &ProjectConfig) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
    }
    let json = serde_json::to_string_pretty(config).map_err(|e| Error::Other(e.to_string()))?;
    fs::write(path, json + "\n").map_err(|e| Error::io(path, e))
}

/// Whether a project-relative path is one of `folders` or inside one.
pub fn in_folders(folders: &[String], rel_path: &str) -> bool {
    let rel = rel_path.trim_start_matches('/');
    folders.iter().any(|ex| {
        let ex = ex.trim_matches('/');
        !ex.is_empty() && (rel == ex || rel.starts_with(&format!("{ex}/")))
    })
}

impl Project {
    /// Create a new project in an existing folder. If the folder already has
    /// a `.ken/project.json` (e.g. cloned from a teammate), it is adopted
    /// unchanged — same id, same settings.
    pub fn create(root: &Path, name: &str) -> Result<Project> {
        if !root.is_dir() {
            return Err(Error::ProjectMissing(root.to_path_buf()));
        }
        if existing_config(root).is_some() {
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

    /// Make a project whose config lives in Ken's app data, not in the repo:
    /// the repo gains no `.ken/`. A repo that already has its own
    /// `.ken/project.json` (a teammate's, say) is adopted unchanged.
    pub fn create_outside(root: &Path, name: &str) -> Result<Project> {
        if !root.is_dir() {
            return Err(Error::ProjectMissing(root.to_path_buf()));
        }
        if existing_config(root).is_some() {
            return Project::open(root);
        }
        let path = outside_config_path(root).ok_or_else(|| Error::Other("Ken's app-data folder was not found".into()))?;
        let config = ProjectConfig {
            name: name.to_string(),
            id: Uuid::new_v4(),
            excluded: Vec::new(),
            features: serde_json::Map::new(),
            extra: serde_json::Map::new(),
        };
        write_config(&path, &config)?;
        Ok(Project { root: root.to_path_buf(), config })
    }

    /// Whether this project's config lives in app data, outside the repo.
    pub fn is_outside(&self) -> bool {
        !config_path(&self.root).exists() && outside_config_path(&self.root).is_some_and(|p| p.exists())
    }

    /// Open a folder that has a `.ken/project.json`, or a config kept for it
    /// in app data.
    pub fn open(root: &Path) -> Result<Project> {
        let path = existing_config(root).unwrap_or_else(|| config_path(root));
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
        // Kept in app data: stay there, so the repo still gains no file.
        if self.is_outside() {
            if let Some(path) = outside_config_path(&self.root) {
                return write_config(&path, &self.config);
            }
        }
        write_config(&config_path(&self.root), &self.config)
    }

    /// Whether this source's older ignore files have moved into the
    /// knowledge base's `.ken/knowledge.json`
    /// (`knowledge_settings::move_legacy_ignores`): from then on its
    /// `.kenignore` files and `project.json`'s `excluded` are not read.
    pub fn ignores_moved(&self) -> bool {
        crate::knowledge_settings::for_repo(&self.root).is_some_and(|s| s.ignores_moved())
    }

    /// The folders a scan still leaves out by `project.json`'s `excluded`:
    /// none once they have moved into `.ken/knowledge.json`, where they are
    /// index rules like any other.
    pub fn legacy_excluded(&self) -> Vec<String> {
        if self.ignores_moved() { Vec::new() } else { self.config.excluded.clone() }
    }

    /// The folders the person left out of this source, as the repo drawer
    /// shows them: the `/folder` lines of its entry in `.ken/knowledge.json`
    /// once its older ignore files have moved there, else `project.json`'s
    /// `excluded`.
    pub fn excluded_folders(&self) -> Vec<String> {
        match crate::knowledge_settings::for_repo(&self.root).filter(|s| s.ignores_moved()) {
            Some(s) => s.excluded_folders(),
            None => self.config.excluded.clone(),
        }
    }

    /// Every index rule this source reads, in the order `kenignore::classify`
    /// folds them (knowledge-core decision 34): the fixed built-ins, the repo
    /// switched off as a whole, the older `.kenignore` files until they have
    /// moved into `.ken/knowledge.json` (read only: Ken writes none now),
    /// then the rules in the knowledge base's `.ken/knowledge.json`, the one
    /// place a person writes them, which win.
    pub fn index_rules(&self) -> Vec<crate::kenignore::Rule> {
        self.index_rules_with(crate::knowledge_settings::for_repo(&self.root).as_ref())
    }

    /// [`Project::index_rules`] with the settings already found.
    pub fn index_rules_with(&self, settings: Option<&crate::knowledge_settings::Settings>) -> Vec<crate::kenignore::Rule> {
        let mut rules = crate::kenignore::built_in_rule_sets();
        rules.extend(crate::kenignore::repo_rules_for(&self.root));
        if !settings.is_some_and(|s| s.ignores_moved()) {
            rules.extend(self.kenignore_rules());
        }
        if let Some(settings) = settings {
            rules.extend(crate::kenignore::parse_lines(&settings.index_rules()));
        }
        rules
    }

    /// The lines of this source's older ignore files, each with where it came
    /// from, in the order they were read: its lines of the workspace's
    /// `.kenignore`, its own `.kenignore`, then `project.json`'s `excluded`
    /// as `/folder` lines (checked before every rule, so they come last
    /// here). Comments, blank and malformed lines are left out. What
    /// `knowledge_settings::move_legacy_ignores` moves.
    pub fn legacy_ignore_lines(&self) -> Vec<(String, Vec<String>)> {
        let rule_lines = |text: &str| -> Vec<String> {
            text.lines()
                .map(str::trim_end)
                .filter(|l| !crate::kenignore::parse(l).is_empty())
                .map(str::to_string)
                .collect()
        };
        let name = self.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut out = Vec::new();
        if let Some((ws, member)) = self.workspace_of() {
            if let Ok(text) = fs::read_to_string(ws.join(".kenignore")) {
                out.push(("the workspace's .kenignore".to_string(), rule_lines(&crate::kenignore::lines_for_member(&text, &member))));
            }
        }
        if let Ok(text) = fs::read_to_string(self.root.join(".kenignore")) {
            out.push((format!("{name}/.kenignore"), rule_lines(&text)));
        }
        let excluded: Vec<String> =
            self.config.excluded.iter().map(|e| e.trim().trim_matches('/')).filter(|e| !e.is_empty()).map(|e| format!("/{e}")).collect();
        out.push((format!("{name}'s project.json excluded"), excluded));
        out
    }

    /// The workspace folder this project sits in (the one holding
    /// `.ken-workspace/`, up to three levels up), and the project's folder
    /// relative to it.
    fn workspace_of(&self) -> Option<(PathBuf, String)> {
        let ws = self.root.ancestors().skip(1).take(3).find(|a| a.join(crate::workspace::CONFIG_DIR).is_dir())?;
        let member = self.root.strip_prefix(ws).ok()?.to_string_lossy().replace('\\', "/");
        Some((ws.to_path_buf(), member))
    }

    /// The older `.kenignore` rules, read as legacy: this project's own file
    /// (project root, not `.ken/`) and the lines of its workspace's
    /// `.kenignore` (the one an older set-up wrote, beside
    /// `.ken-workspace/`) that reach inside it: `member/path` lines, and
    /// patterns with no folder in them (`*.log`). The workspace's lines come
    /// first, so the member's own file overrides. Missing files read as no
    /// rules.
    pub fn kenignore_rules(&self) -> Vec<crate::kenignore::Rule> {
        let path = self.root.join(".kenignore");
        let text = fs::read_to_string(&path).unwrap_or_default();
        let mut rules = self.workspace_kenignore_rules();
        rules.extend(crate::kenignore::parse(&text));
        rules
    }

    fn workspace_kenignore_rules(&self) -> Vec<crate::kenignore::Rule> {
        let Some((ws, member)) = self.workspace_of() else { return Vec::new() };
        let Ok(text) = fs::read_to_string(ws.join(".kenignore")) else { return Vec::new() };
        crate::kenignore::parse(&crate::kenignore::lines_for_member(&text, &member))
    }

    pub fn set_excluded(&mut self, excluded: Vec<String>) -> Result<()> {
        self.config.excluded = excluded;
        self.save()
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
        let excluded = vec!["archive".to_string()];
        assert!(in_folders(&excluded, "archive/old.md"));
        assert!(in_folders(&excluded, "archive"));
        assert!(!in_folders(&excluded, "archive-2/notes.md"));
        assert!(!in_folders(&excluded, "notes/archive.md"));
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
}

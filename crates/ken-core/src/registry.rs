//! Local registry of known projects, kept in the OS app-data directory
//! (`<data-dir>/ken/projects.json`). Purely local — never synced; losing it
//! costs nothing but re-opening folders.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::project::Project;
use crate::workspace::Workspace;
use crate::{Error, Result};

/// What a repo is for, which decides what Ken does there: sync, and how
/// deep to read. Team and wiki repos hold people, rulings and dates, so they
/// sync and are read for entities; code and reference repos are only made
/// searchable, and Ken never commits into them. A repo can be more than one
/// kind (a wiki that also carries code).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RepoKind {
    Team,
    Wiki,
    Code,
    Reference,
}

impl RepoKind {
    /// Kinds whose repos Ken keeps in step with the team by default.
    pub fn syncs(self) -> bool {
        matches!(self, RepoKind::Team | RepoKind::Wiki)
    }

    /// Kinds whose files are read for entities. Code and reference repos
    /// are only made searchable: entities read out of code are functions
    /// and variables that clutter the graph, and each one costs a local
    /// model generation.
    pub fn reads_entities(self) -> bool {
        matches!(self, RepoKind::Team | RepoKind::Wiki)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryEntry {
    pub id: Uuid,
    pub name: String,
    pub path: PathBuf,
    /// Empty means not yet said. Kept here, in local app data, rather than
    /// in the repo, so a code repo gets no file from Ken.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kind: Vec<RepoKind>,
    /// The team this repo belongs to, by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    /// How deep Ken reads the repo when a person chose other than what the
    /// kind implies (a code repo whose docs are read for entities, say).
    /// None means follow the kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<IndexState>,
    /// What the repo is and how to use it, in a person's words (set at
    /// set-up, first proposed from its README). Read by the first-wiki
    /// draft so it knows what each source is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A repo's index state, set once for the whole repo: the three kenignore
/// tiers. `Off` repos are not members at all, so only these two are stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IndexState {
    Off,
    Search,
    Entities,
}

impl IndexState {
    /// The state a repo of these kinds gets unless a person says otherwise.
    pub fn for_kind(kind: &[RepoKind]) -> IndexState {
        if kind.is_empty() || kind.iter().any(|k| k.reads_entities()) {
            IndexState::Entities
        } else {
            IndexState::Search
        }
    }
}

/// A recently opened workspace (recent-projects' sibling list — see
/// `Registry::workspaces`). `last_focused` is the member project id that
/// was focused when the workspace was last left, so reopening can restore
/// it (spec: "Workspace recents" / "reopen restores focus").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentWorkspaceEntry {
    pub id: Uuid,
    pub name: String,
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_focused: Option<Uuid>,
    /// Unix seconds, most-recent-first display in the launcher.
    pub opened_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub projects: Vec<RegistryEntry>,
    /// The project that was open last — reopened on launch.
    #[serde(default, rename = "lastProject", skip_serializing_if = "Option::is_none")]
    pub last_project: Option<Uuid>,
    /// The workspace that was open last — reopened on launch. `serde(default)`
    /// so registries written before the workspace flag existed still load.
    #[serde(default, rename = "lastWorkspace", skip_serializing_if = "Option::is_none")]
    pub last_workspace: Option<Uuid>,
    /// Recently opened workspaces, beside `projects`. `serde(default)` so
    /// registries written before the `workspace` feature existed still
    /// load, defaulting to an empty list.
    #[serde(default)]
    pub workspaces: Vec<RecentWorkspaceEntry>,
}

/// A registry entry plus whether its folder still exists on disk.
#[derive(Debug, Clone, Serialize)]
pub struct RegistryEntryStatus {
    #[serde(flatten)]
    pub entry: RegistryEntry,
    pub available: bool,
}

/// A recent-workspace entry plus whether its parent folder still exists on
/// disk — same shape as `RegistryEntryStatus`, for the launcher's recent
/// workspaces section.
#[derive(Debug, Clone, Serialize)]
pub struct RecentWorkspaceStatus {
    #[serde(flatten)]
    pub entry: RecentWorkspaceEntry,
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
    // Unit tests never read or write the person's real app data.
    #[cfg(test)]
    {
        static TEST_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        return Ok(TEST_DIR
            .get_or_init(|| std::env::temp_dir().join(format!("ken-test-data-{}", std::process::id())))
            .clone());
    }
    #[allow(unreachable_code)]
    dirs::data_dir()
        .map(|d| d.join("ken"))
        .ok_or_else(|| Error::Other("no OS data directory available".into()))
}

fn registry_path(base: &Path) -> PathBuf {
    base.join("projects.json")
}

fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The registered folder and kinds of project `id`, from the default
/// registry.
pub fn entry_of(id: Uuid) -> Option<(PathBuf, Vec<RepoKind>)> {
    let reg = default_base_dir().and_then(|base| Registry::load(&base)).ok()?;
    reg.projects.into_iter().find(|e| e.id == id).map(|e| (e.path, e.kind))
}

/// The kinds and index override registered for the repo at `root`, from
/// the default registry.
pub fn index_of(root: &Path) -> (Vec<RepoKind>, Option<IndexState>) {
    default_base_dir()
        .and_then(|base| Registry::load(&base))
        .ok()
        .and_then(|reg| reg.entry_at(root).map(|e| (e.kind.clone(), e.index)))
        .unwrap_or_default()
}

/// A repo nobody has said the kind of yet: registered, a git repo, and not
/// a team-inbox clone. Ken reads none of it until a person sets its kind, so
/// a repo is never indexed on a guess (a code repo with no kind is read for
/// entities in full). Ken's own folders (the workspace memory) are not git
/// repos and are read as before.
pub fn awaits_kind(root: &Path) -> bool {
    if !root.join(".git").exists() || root.join("family.json").exists() {
        return false;
    }
    default_base_dir()
        .and_then(|base| Registry::load(&base))
        .ok()
        .and_then(|reg| reg.entry_at(root).map(|e| e.kind.is_empty() && e.index.is_none()))
        .unwrap_or(false)
}

/// What a person said the repo at `root` is for, from the default registry.
pub fn description_of(root: &Path) -> Option<String> {
    default_base_dir()
        .and_then(|base| Registry::load(&base))
        .ok()
        .and_then(|reg| reg.entry_at(root).and_then(|e| e.description.clone()))
}

/// [`Registry::kind_of`] against the registry in the default app-data
/// directory. Empty when it cannot be read.
pub fn kind_of(root: &Path) -> Vec<RepoKind> {
    default_base_dir()
        .and_then(|base| Registry::load(&base))
        .map(|reg| reg.kind_of(root))
        .unwrap_or_default()
}

/// Whether the repo at `root` is a team's library: said to be a team or wiki
/// repo, or holding the inbox (`Research/Ingestion/Raw/`) whatever its kind.
pub fn is_library(root: &Path) -> bool {
    crate::ingest::has_inbox(root) || kind_of(root).iter().any(|k| matches!(k, RepoKind::Team | RepoKind::Wiki))
}

/// Whether a project folder is a workspace's own `.ken-workspace` (its
/// memory), which is not a repo and is never registered as one.
pub fn is_workspace_folder(path: &Path) -> bool {
    path.file_name().is_some_and(|n| n == crate::workspace::CONFIG_DIR)
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
    /// Kind and team are what a person said about the repo, so a re-add
    /// keeps them.
    pub fn add(&mut self, project: &Project) {
        // One entry per folder: a repo made again in the same folder (a new
        // id) replaces the old entry, which lookups by folder would otherwise
        // keep finding first, with its old kind.
        let here = canonical(&project.root);
        self.projects.retain(|e| e.id == project.config.id || canonical(&e.path) != here);
        match self.projects.iter_mut().find(|e| e.id == project.config.id) {
            Some(existing) => {
                existing.name = project.config.name.clone();
                existing.path = project.root.clone();
            }
            None => self.projects.push(RegistryEntry {
                id: project.config.id,
                name: project.config.name.clone(),
                path: project.root.clone(),
                kind: Vec::new(),
                team: None,
                index: None,
                description: None,
            }),
        }
    }

    /// A repo with an inbox and no kind said is the team's wiki: give it the
    /// wiki kind, and `team` when it has none. Returns whether it changed.
    pub fn infer_library(&mut self, root: &Path, team: Option<&str>) -> bool {
        if !crate::ingest::has_inbox(root) {
            return false;
        }
        let want = canonical(root);
        let Some(entry) = self.projects.iter_mut().find(|e| canonical(&e.path) == want) else {
            return false;
        };
        if !entry.kind.is_empty() {
            return false;
        }
        entry.kind = vec![RepoKind::Wiki];
        if entry.team.is_none() {
            entry.team = team.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
        }
        true
    }

    /// Drop entries for a workspace's own `.ken-workspace` folder, which
    /// earlier versions registered. Returns whether any were there.
    pub fn forget_workspace_folders(&mut self) -> bool {
        let before = self.projects.len();
        self.projects.retain(|e| !is_workspace_folder(&e.path));
        self.projects.len() != before
    }

    /// Set a project's kind and team. Returns false for an unknown id.
    pub fn set_kind(&mut self, id: Uuid, kind: Vec<RepoKind>, team: Option<String>) -> bool {
        let Some(entry) = self.projects.iter_mut().find(|e| e.id == id) else {
            return false;
        };
        entry.kind.clear();
        for k in kind {
            if !entry.kind.contains(&k) {
                entry.kind.push(k);
            }
        }
        entry.team = team.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
        true
    }

    /// Set how deep Ken reads a project, or None to follow its kind.
    pub fn set_index(&mut self, id: Uuid, index: Option<IndexState>) -> bool {
        let Some(entry) = self.projects.iter_mut().find(|e| e.id == id) else {
            return false;
        };
        entry.index = index.filter(|i| *i != IndexState::for_kind(&entry.kind));
        true
    }

    /// Set what a repo is for, in a person's words; empty clears it.
    pub fn set_description(&mut self, id: Uuid, description: &str) -> bool {
        let Some(entry) = self.projects.iter_mut().find(|e| e.id == id) else {
            return false;
        };
        let d = description.trim();
        entry.description = (!d.is_empty()).then(|| d.to_string());
        true
    }

    /// The registered entry for the repo at `root`.
    pub fn entry_at(&self, root: &Path) -> Option<&RegistryEntry> {
        let want = canonical(root);
        self.projects.iter().find(|e| canonical(&e.path) == want)
    }

    /// The kinds registered for the repo at `root`, empty when it is not
    /// registered or no kind has been said. Paths compare canonicalized, so
    /// a trailing slash or a different drive-letter case still matches.
    pub fn kind_of(&self, root: &Path) -> Vec<RepoKind> {
        let want = canonical(root);
        self.projects
            .iter()
            .find(|e| canonical(&e.path) == want)
            .map(|e| e.kind.clone())
            .unwrap_or_default()
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

    /// Register (or re-register) a workspace. Same id updates
    /// path/name/`last_focused`/`opened_at` in place — e.g. reopening bumps
    /// its recency, same as `add` does for projects.
    pub fn add_workspace(&mut self, workspace: &Workspace, last_focused: Option<Uuid>, opened_at: i64) {
        let entry = RecentWorkspaceEntry {
            id: workspace.config.id,
            name: workspace.config.name.clone(),
            path: workspace.root.clone(),
            last_focused,
            opened_at,
        };
        match self.workspaces.iter_mut().find(|e| e.id == entry.id) {
            Some(existing) => *existing = entry,
            None => self.workspaces.push(entry),
        }
    }

    pub fn remove_workspace(&mut self, id: Uuid) {
        self.workspaces.retain(|e| e.id != id);
    }

    /// Recent-workspace entries with availability (does the parent folder
    /// still exist?).
    pub fn workspace_statuses(&self) -> Vec<RecentWorkspaceStatus> {
        self.workspaces
            .iter()
            .map(|e| RecentWorkspaceStatus {
                entry: e.clone(),
                available: e.path.is_dir(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repo_made_again_in_the_same_folder_replaces_the_old_entry() {
        let dir = tempfile::tempdir().unwrap();
        let first = Project::create(dir.path(), "Wiki").unwrap();
        let mut reg = Registry::default();
        reg.add(&first);
        reg.set_kind(first.config.id, vec![RepoKind::Wiki], None);
        std::fs::remove_dir_all(dir.path().join(".ken")).unwrap();
        let again = Project::create(dir.path(), "Wiki").unwrap();
        assert_ne!(first.config.id, again.config.id);
        reg.add(&again);
        reg.set_kind(again.config.id, vec![RepoKind::Team, RepoKind::Wiki], None);
        assert_eq!(reg.projects.len(), 1);
        assert_eq!(reg.entry_at(dir.path()).unwrap().kind, vec![RepoKind::Team, RepoKind::Wiki]);
    }
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
        // Under test the default is a per-run temp folder, never real app data.
        let default = default_base_dir().unwrap();
        assert!(
            default.file_name().is_some_and(|n| n.to_string_lossy().starts_with("ken-test-data-")),
            "unexpected default: {default:?}"
        );
    }

    #[test]
    fn last_workspace_roundtrips_and_old_registry_still_loads() {
        let app = tempdir().unwrap();

        // A registry written before `lastWorkspace` existed must still load,
        // defaulting the new field to `None`.
        let old_json = r#"{"projects":[],"lastProject":null}"#;
        fs::write(registry_path(app.path()), old_json).unwrap();
        let loaded = Registry::load(app.path()).unwrap();
        assert_eq!(loaded.last_workspace, None);

        let mut reg = loaded;
        let id = Uuid::new_v4();
        reg.last_workspace = Some(id);
        reg.save(app.path()).unwrap();

        let reloaded = Registry::load(app.path()).unwrap();
        assert_eq!(reloaded.last_workspace, Some(id));
    }

    #[test]
    fn kind_and_team_survive_a_re_add_and_a_reload() {
        let app = tempdir().unwrap();
        let proj_dir = tempdir().unwrap();
        let project = Project::create(proj_dir.path(), "Atlas").unwrap();
        let mut reg = Registry::default();
        reg.add(&project);
        assert!(reg.set_kind(
            project.config.id,
            vec![RepoKind::Wiki, RepoKind::Code, RepoKind::Wiki],
            Some("  Atlas team ".into()),
        ));
        assert_eq!(reg.projects[0].kind, vec![RepoKind::Wiki, RepoKind::Code], "duplicates dropped, order kept");
        assert_eq!(reg.projects[0].team.as_deref(), Some("Atlas team"));

        reg.add(&project); // reopening re-registers
        reg.save(app.path()).unwrap();
        let loaded = Registry::load(app.path()).unwrap();
        assert_eq!(loaded.projects[0].kind, vec![RepoKind::Wiki, RepoKind::Code]);
        assert_eq!(loaded.projects[0].team.as_deref(), Some("Atlas team"));
        assert!(!reg.set_kind(Uuid::new_v4(), vec![RepoKind::Code], None), "unknown id");
    }

    #[test]
    fn kind_of_matches_the_folder_however_it_is_spelled() {
        let proj_dir = tempdir().unwrap();
        let project = Project::create(proj_dir.path(), "Atlas").unwrap();
        let mut reg = Registry::default();
        reg.add(&project);
        reg.set_kind(project.config.id, vec![RepoKind::Team], None);
        let dotted = proj_dir.path().join(".");
        assert_eq!(reg.kind_of(&dotted), vec![RepoKind::Team]);
        let other = tempdir().unwrap();
        assert!(reg.kind_of(other.path()).is_empty(), "unregistered folder has no kind");
    }

    #[test]
    fn a_registry_without_kinds_still_loads() {
        let app = tempdir().unwrap();
        let old_json = r#"{"projects":[{"id":"6f1c2c1e-9a55-4c55-9d7a-1d2a3b4c5d6e","name":"Old","path":"/old"}]}"#;
        fs::write(registry_path(app.path()), old_json).unwrap();
        let loaded = Registry::load(app.path()).unwrap();
        assert!(loaded.projects[0].kind.is_empty());
        assert_eq!(loaded.projects[0].team, None);
        let raw = serde_json::to_string(&loaded).unwrap();
        assert!(!raw.contains("kind") && !raw.contains("team"), "unset fields are not written");
    }

    #[test]
    fn a_repo_with_an_inbox_and_no_kind_is_the_wiki() {
        let wiki_dir = tempdir().unwrap();
        fs::create_dir_all(wiki_dir.path().join(crate::ingest::RAW)).unwrap();
        let wiki = Project::create(wiki_dir.path(), "ATT-Wiki").unwrap();
        let code_dir = tempdir().unwrap();
        let code = Project::create(code_dir.path(), "att-web").unwrap();
        let mut reg = Registry::default();
        reg.add(&wiki);
        reg.add(&code);
        assert!(reg.infer_library(wiki_dir.path(), Some("ATT")));
        assert_eq!(reg.kind_of(wiki_dir.path()), vec![RepoKind::Wiki]);
        assert_eq!(reg.entry_at(wiki_dir.path()).unwrap().team.as_deref(), Some("ATT"));
        assert!(!reg.infer_library(wiki_dir.path(), Some("Other")), "said once, kept");
        assert!(!reg.infer_library(code_dir.path(), Some("ATT")), "no inbox, no kind");
        reg.set_kind(wiki.config.id, vec![RepoKind::Team], None);
        assert!(!reg.infer_library(wiki_dir.path(), Some("ATT")), "a kind a person said stands");
    }

    #[test]
    fn a_workspace_folder_is_not_kept_as_a_repo() {
        let ws_dir = tempdir().unwrap();
        let folder = ws_dir.path().join(crate::workspace::CONFIG_DIR);
        fs::create_dir_all(&folder).unwrap();
        let memory = Project::create(&folder, "Workspace Memory").unwrap();
        let mut reg = Registry::default();
        reg.add(&memory);
        assert!(is_workspace_folder(&folder));
        assert!(reg.forget_workspace_folders());
        assert!(reg.projects.is_empty());
        assert!(!reg.forget_workspace_folders());
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

    #[test]
    fn workspace_add_save_load_roundtrip() {
        let app = tempdir().unwrap();
        let ws_dir = tempdir().unwrap();
        fs::create_dir_all(ws_dir.path().join("alpha")).unwrap();
        let workspace = crate::workspace::Workspace::create(ws_dir.path(), "Atlas WS", &["alpha".into()]).unwrap();
        let member_id = match &workspace.members[0].status {
            crate::workspace::MemberStatus::Ok(p) => p.config.id,
            other => panic!("expected ok member, got {other:?}"),
        };

        let mut reg = Registry::load(app.path()).unwrap();
        assert!(reg.workspaces.is_empty());
        reg.add_workspace(&workspace, Some(member_id), 1_700_000_000);
        reg.save(app.path()).unwrap();

        let loaded = Registry::load(app.path()).unwrap();
        assert_eq!(loaded.workspaces.len(), 1);
        assert_eq!(loaded.workspaces[0].id, workspace.config.id);
        assert_eq!(loaded.workspaces[0].name, "Atlas WS");
        assert_eq!(loaded.workspaces[0].last_focused, Some(member_id));
        assert_eq!(loaded.workspaces[0].opened_at, 1_700_000_000);
    }

    #[test]
    fn workspace_re_add_same_id_updates_in_place() {
        let ws_dir = tempdir().unwrap();
        fs::create_dir_all(ws_dir.path().join("alpha")).unwrap();
        let workspace = crate::workspace::Workspace::create(ws_dir.path(), "Atlas WS", &["alpha".into()]).unwrap();

        let mut reg = Registry::default();
        reg.add_workspace(&workspace, None, 100);
        // Reopening later bumps recency and can add a focused member in place.
        let member_id = match &workspace.members[0].status {
            crate::workspace::MemberStatus::Ok(p) => p.config.id,
            other => panic!("expected ok member, got {other:?}"),
        };
        reg.add_workspace(&workspace, Some(member_id), 200);
        assert_eq!(reg.workspaces.len(), 1);
        assert_eq!(reg.workspaces[0].last_focused, Some(member_id));
        assert_eq!(reg.workspaces[0].opened_at, 200);
    }

    #[test]
    fn workspace_missing_path_detected() {
        let ws_dir = tempdir().unwrap();
        fs::create_dir_all(ws_dir.path().join("alpha")).unwrap();
        let workspace = crate::workspace::Workspace::create(ws_dir.path(), "Atlas WS", &["alpha".into()]).unwrap();
        let mut reg = Registry::default();
        reg.add_workspace(&workspace, None, 100);

        assert!(reg.workspace_statuses()[0].available);
        drop(ws_dir); // folder deleted
        assert!(!reg.workspace_statuses()[0].available);
    }

    #[test]
    fn workspaces_field_defaults_empty_for_old_registry() {
        let app = tempdir().unwrap();
        // A registry written before the `workspace` feature existed must
        // still load, defaulting `workspaces` to an empty list.
        let old_json = r#"{"projects":[],"lastProject":null}"#;
        fs::write(registry_path(app.path()), old_json).unwrap();
        let loaded = Registry::load(app.path()).unwrap();
        assert!(loaded.workspaces.is_empty());
    }

    #[test]
    fn remove_workspace_entry() {
        let ws_dir = tempdir().unwrap();
        fs::create_dir_all(ws_dir.path().join("alpha")).unwrap();
        let workspace = crate::workspace::Workspace::create(ws_dir.path(), "Atlas WS", &["alpha".into()]).unwrap();
        let mut reg = Registry::default();
        reg.add_workspace(&workspace, None, 100);
        reg.remove_workspace(workspace.config.id);
        assert!(reg.workspaces.is_empty());
    }
}

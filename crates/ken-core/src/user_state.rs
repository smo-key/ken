//! Per-user, per-project private state, kept in the OS app-data directory
//! (`<data-dir>/ken/user-state/<project-id>.json`) — the same non-synced home
//! as the project registry. This is deliberately NOT `.ken/project.json`, which
//! is version-controlled and syncs to teammates: everything stored here is a
//! preference private to this machine and this user (which review issues they
//! have chosen to ignore today, and — soon — per-file "unread" tracking), so it
//! must never leak into the shared config where it would silence a file for the
//! whole team.
//!
//! ## Unread tracking (files "modified by someone else")
//! `seen` records, per file, the `(size, mtime)` version the user last looked
//! at. A file is UNREAD when the index holds a version different from `seen`
//! (or has no `seen` entry at all) — i.e. it changed since the user last saw it.
//! Because Ken records the post-write version on the user's OWN saves, their
//! edits never count as unread; what remains is exactly changes made by someone
//! or something else (a teammate's sync, a cloud hydrate, an external editor).
//! To avoid lighting up every file the first time the feature ships, the first
//! activation BASELINES the project: it snapshots all currently-indexed files as
//! seen and sets `baselined`. Thereafter only files that CHANGE or are ADDED
//! after the baseline are unread; pre-existing files are never retroactively
//! flagged.
//!
//! Load/save are best-effort, like the registry: a missing OR corrupt file
//! yields defaults rather than an error, because losing a private preference is
//! harmless and must never block opening a project.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Error, Result};

/// A file's index version — `(size, mtime)`, the same pair the scanner tracks.
/// Equality is our "unchanged since last seen" test; any difference (a byte
/// edit moves both, a touch moves mtime) means the file changed.
pub type FileVersion = (i64, i64);

/// One user's private state for one project. Kept small and forward-compatible:
/// `#[serde(default)]` on every field means new fields (e.g. read tracking) can
/// be added later without invalidating files written by older builds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserState {
    /// Project-relative paths whose review issues the user has silenced. The
    /// file itself stays indexed and searchable — only its nag is hidden. A set
    /// gives O(log n) membership tests and serializes as a stable, sorted array.
    #[serde(default)]
    pub ignored: BTreeSet<String>,

    /// The `(size, mtime)` version of each file the last time the user looked at
    /// it (opened, saved, or explicitly marked viewed). A file is unread when
    /// the index disagrees with this. Serializes as a stable, sorted map.
    #[serde(default)]
    pub seen: BTreeMap<String, FileVersion>,

    /// Whether this project has had its one-time unread baseline taken. Guards
    /// against re-snapshotting on every activation (which would hide changes)
    /// and, while unset, keeps the unread list empty so nothing flags on the
    /// first run.
    #[serde(default)]
    pub baselined: bool,
}

fn state_dir(base: &Path) -> PathBuf {
    base.join("user-state")
}

fn state_path(base: &Path, project_id: Uuid) -> PathBuf {
    state_dir(base).join(format!("{project_id}.json"))
}

impl UserState {
    /// Load this project's private state, defaulting on absence or corruption.
    /// Infallible on purpose: a preference read must never surface an error.
    pub fn load(base: &Path, project_id: Uuid) -> UserState {
        match fs::read_to_string(state_path(base, project_id)) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            Err(_) => UserState::default(),
        }
    }

    pub fn save(&self, base: &Path, project_id: Uuid) -> Result<()> {
        let dir = state_dir(base);
        fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
        let path = state_path(base, project_id);
        let json =
            serde_json::to_string_pretty(self).map_err(|e| Error::Other(e.to_string()))?;
        fs::write(&path, json + "\n").map_err(|e| Error::io(&path, e))
    }

    /// Silence a file's issues. Returns whether the set actually changed.
    pub fn ignore(&mut self, rel_path: impl Into<String>) -> bool {
        self.ignored.insert(rel_path.into())
    }

    /// Stop silencing a file. Returns whether the set actually changed.
    pub fn unignore(&mut self, rel_path: &str) -> bool {
        self.ignored.remove(rel_path)
    }

    pub fn is_ignored(&self, rel_path: &str) -> bool {
        self.ignored.contains(rel_path)
    }

    /// Files the user hasn't seen at their current version: present in `index`
    /// with a `(size, mtime)` that differs from `seen` (or is absent from it).
    /// Returns nothing until the project is baselined, so a not-yet-baselined
    /// project never reports its whole tree as unread.
    pub fn unread(&self, index: &[(String, FileVersion)]) -> Vec<String> {
        if !self.baselined {
            return Vec::new();
        }
        index
            .iter()
            .filter(|(rel, ver)| self.seen.get(rel) != Some(ver))
            .map(|(rel, _)| rel.clone())
            .collect()
    }

    /// Take the one-time unread baseline: snapshot every currently-indexed file
    /// as seen and flip `baselined`. A no-op (returns false) once already
    /// baselined, so a later activation can't silently mark changes as seen.
    pub fn baseline(&mut self, index: &[(String, FileVersion)]) -> bool {
        if self.baselined {
            return false;
        }
        self.snapshot(index);
        self.baselined = true;
        true
    }

    /// Record `rel_path` as seen at `version`. Called on open and on the user's
    /// own save (so self-edits stay seen). Returns whether `seen` changed.
    pub fn mark_seen(&mut self, rel_path: impl Into<String>, version: FileVersion) -> bool {
        let rel = rel_path.into();
        if self.seen.get(&rel) == Some(&version) {
            return false;
        }
        self.seen.insert(rel, version);
        true
    }

    /// Mark every currently-indexed file seen (the "mark all as viewed" action).
    /// Also ensures `baselined`, since it establishes a clean seen snapshot.
    /// Returns whether anything changed.
    pub fn mark_all_seen(&mut self, index: &[(String, FileVersion)]) -> bool {
        let before = (self.seen.clone(), self.baselined);
        self.snapshot(index);
        self.baselined = true;
        (self.seen.clone(), self.baselined) != before
    }

    /// Replace `seen` with exactly the index's current versions. Dropping files
    /// no longer indexed keeps the map from growing unbounded with deletions.
    fn snapshot(&mut self, index: &[(String, FileVersion)]) {
        self.seen = index.iter().cloned().collect();
    }
}

/// The `source_ref` of these inbox kinds is a project-relative file path (as
/// opposed to a recipe or run slug), so a per-file ignore can silence them.
/// Slug-based kinds (`approval`, `stale`, `broken-recipe`) reference no file and
/// so always pass through regardless of what is ignored.
pub fn inbox_item_file_ref<'a>(kind: &str, source_ref: &'a str) -> Option<&'a str> {
    match kind {
        "failed-file" | "conflict" | "conflict-copy" | "stored" => Some(source_ref),
        _ => None,
    }
}

/// Whether an inbox item should be hidden because the file it references is in
/// `ignored`. Items with no file ref are never hidden.
pub fn inbox_item_ignored(kind: &str, source_ref: &str, ignored: &BTreeSet<String>) -> bool {
    inbox_item_file_ref(kind, source_ref).is_some_and(|f| ignored.contains(f))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn load_missing_file_is_default_empty() {
        let base = tempdir().unwrap();
        let state = UserState::load(base.path(), Uuid::new_v4());
        assert!(state.ignored.is_empty());
    }

    #[test]
    fn ignore_unignore_set_semantics() {
        let mut state = UserState::default();
        assert!(state.ignore("notes/a.pdf"));
        // Re-ignoring the same path is a no-op (idempotent set membership).
        assert!(!state.ignore("notes/a.pdf"));
        assert!(state.is_ignored("notes/a.pdf"));
        assert_eq!(state.ignored.len(), 1);

        assert!(state.unignore("notes/a.pdf"));
        // Un-ignoring what isn't ignored is a no-op.
        assert!(!state.unignore("notes/a.pdf"));
        assert!(!state.is_ignored("notes/a.pdf"));
    }

    #[test]
    fn save_load_roundtrip() {
        let base = tempdir().unwrap();
        let id = Uuid::new_v4();
        let mut state = UserState::default();
        state.ignore("a.pdf");
        state.ignore("sub/b.docx");
        state.save(base.path(), id).unwrap();

        let loaded = UserState::load(base.path(), id);
        assert_eq!(loaded, state);
        assert!(loaded.is_ignored("a.pdf"));
        assert!(loaded.is_ignored("sub/b.docx"));
    }

    #[test]
    fn state_lives_under_app_data_not_project() {
        // Proves the file is written beneath the app-data base in a dedicated
        // `user-state` folder — never inside a `.ken` project directory.
        let base = tempdir().unwrap();
        let id = Uuid::new_v4();
        UserState::default().save(base.path(), id).unwrap();
        let path = state_path(base.path(), id);
        assert!(path.starts_with(base.path().join("user-state")));
        assert!(path.exists());
    }

    #[test]
    fn corrupt_file_tolerated_as_default() {
        let base = tempdir().unwrap();
        let id = Uuid::new_v4();
        let dir = state_dir(base.path());
        fs::create_dir_all(&dir).unwrap();
        fs::write(state_path(base.path(), id), "{ not valid json ").unwrap();

        let state = UserState::load(base.path(), id);
        assert!(state.ignored.is_empty());
    }

    #[test]
    fn separate_projects_have_separate_state() {
        let base = tempdir().unwrap();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let mut sa = UserState::default();
        sa.ignore("only-in-a.pdf");
        sa.save(base.path(), a).unwrap();

        let loaded_b = UserState::load(base.path(), b);
        assert!(loaded_b.ignored.is_empty());
    }

    #[test]
    fn failed_file_ignored_when_its_path_is_in_the_set() {
        let ignored: BTreeSet<String> = ["notes/broken.pdf".to_string()].into_iter().collect();
        assert!(inbox_item_ignored("failed-file", "notes/broken.pdf", &ignored));
        assert!(!inbox_item_ignored("failed-file", "notes/other.pdf", &ignored));
    }

    #[test]
    fn conflict_items_respect_ignore_by_file_ref() {
        let ignored: BTreeSet<String> = ["shared/People.md".to_string()].into_iter().collect();
        assert!(inbox_item_ignored("conflict", "shared/People.md", &ignored));
        assert!(inbox_item_ignored("conflict-copy", "shared/People.md", &ignored));
    }

    #[test]
    fn slug_based_items_have_no_file_ref_and_pass_through() {
        // A recipe slug happening to equal an ignored path must NOT be silenced:
        // stale/broken/approval reference slugs, not files.
        let ignored: BTreeSet<String> = ["people".to_string()].into_iter().collect();
        assert_eq!(inbox_item_file_ref("stale", "people"), None);
        assert_eq!(inbox_item_file_ref("broken-recipe", "people"), None);
        assert_eq!(inbox_item_file_ref("approval", "people"), None);
        assert!(!inbox_item_ignored("stale", "people", &ignored));
        assert!(!inbox_item_ignored("approval", "people", &ignored));
    }

    // ── unread tracking ───────────────────────────────────────────────────
    // Version pairs are (size, mtime); equality is the "unchanged" test.
    fn idx(entries: &[(&str, i64, i64)]) -> Vec<(String, FileVersion)> {
        entries
            .iter()
            .map(|(p, s, m)| (p.to_string(), (*s, *m)))
            .collect()
    }

    #[test]
    fn unread_is_empty_before_baseline() {
        // Until a project is baselined, NOTHING is unread — otherwise every
        // pre-existing file would flag the moment the feature ships.
        let state = UserState::default();
        assert!(!state.baselined);
        let index = idx(&[("a.md", 1, 100), ("b.md", 2, 200)]);
        assert!(state.unread(&index).is_empty());
    }

    #[test]
    fn baseline_snapshots_all_and_flags() {
        let mut state = UserState::default();
        let index = idx(&[("a.md", 1, 100), ("b.md", 2, 200)]);
        assert!(state.baseline(&index));
        assert!(state.baselined);
        // Every baselined file is now "seen" at its current version, so the
        // freshly-opened project starts with an empty unread list.
        assert!(state.unread(&index).is_empty());
        assert_eq!(state.seen.len(), 2);
    }

    #[test]
    fn baseline_is_idempotent() {
        let mut state = UserState::default();
        assert!(state.baseline(&idx(&[("a.md", 1, 100)])));
        // A second baseline (e.g. next activation) must NOT re-snapshot, or it
        // would silently mark everything seen and hide real changes.
        assert!(!state.baseline(&idx(&[("a.md", 9, 900), ("b.md", 2, 200)])));
        // The first snapshot stands: a.md changed → unread, b.md added → unread.
        let now = idx(&[("a.md", 9, 900), ("b.md", 2, 200)]);
        assert_eq!(state.unread(&now), vec!["a.md".to_string(), "b.md".to_string()]);
    }

    #[test]
    fn changed_file_is_unread_after_baseline() {
        let mut state = UserState::default();
        state.baseline(&idx(&[("a.md", 1, 100), ("b.md", 2, 200)]));
        // a.md's mtime moved (a teammate/sync edited it) → unread; b.md steady.
        let now = idx(&[("a.md", 1, 150), ("b.md", 2, 200)]);
        assert_eq!(state.unread(&now), vec!["a.md".to_string()]);
    }

    #[test]
    fn added_file_after_baseline_is_unread() {
        let mut state = UserState::default();
        state.baseline(&idx(&[("a.md", 1, 100)]));
        let now = idx(&[("a.md", 1, 100), ("new.md", 3, 300)]);
        assert_eq!(state.unread(&now), vec!["new.md".to_string()]);
    }

    #[test]
    fn removed_file_is_not_reported_unread() {
        // A file recorded as seen but no longer in the index is simply gone —
        // unread only walks the current index, so it never surfaces.
        let mut state = UserState::default();
        state.baseline(&idx(&[("a.md", 1, 100), ("b.md", 2, 200)]));
        let now = idx(&[("a.md", 1, 100)]);
        assert!(state.unread(&now).is_empty());
    }

    #[test]
    fn mark_seen_clears_a_single_unread() {
        let mut state = UserState::default();
        state.baseline(&idx(&[("a.md", 1, 100)]));
        let now = idx(&[("a.md", 1, 100), ("new.md", 3, 300)]);
        assert_eq!(state.unread(&now), vec!["new.md".to_string()]);
        // Opening new.md records its current version → no longer unread.
        assert!(state.mark_seen("new.md", (3, 300)));
        assert!(state.unread(&now).is_empty());
        // Re-marking the same version is a no-op.
        assert!(!state.mark_seen("new.md", (3, 300)));
    }

    #[test]
    fn self_save_keeps_file_seen() {
        // The user's own save records the NEW version as seen, so their edit is
        // never counted as "modified by someone else".
        let mut state = UserState::default();
        state.baseline(&idx(&[("a.md", 1, 100)]));
        // User edits a.md through Ken; save records the post-write version.
        assert!(state.mark_seen("a.md", (5, 500)));
        let now = idx(&[("a.md", 5, 500)]);
        assert!(state.unread(&now).is_empty());
    }

    #[test]
    fn mark_all_seen_clears_everything() {
        let mut state = UserState::default();
        state.baseline(&idx(&[("a.md", 1, 100)]));
        let now = idx(&[("a.md", 1, 150), ("b.md", 2, 200), ("c.md", 3, 300)]);
        assert_eq!(state.unread(&now).len(), 3);
        assert!(state.mark_all_seen(&now));
        assert!(state.unread(&now).is_empty());
        assert!(state.baselined);
    }

    #[test]
    fn seen_and_baselined_survive_roundtrip() {
        let base = tempdir().unwrap();
        let id = Uuid::new_v4();
        let mut state = UserState::default();
        state.baseline(&idx(&[("a.md", 1, 100)]));
        state.mark_seen("b.md", (2, 200));
        state.ignore("x.pdf");
        state.save(base.path(), id).unwrap();

        let loaded = UserState::load(base.path(), id);
        assert_eq!(loaded, state);
        assert!(loaded.baselined);
        assert_eq!(loaded.seen.get("b.md"), Some(&(2, 200)));
        assert!(loaded.is_ignored("x.pdf"));
    }
}

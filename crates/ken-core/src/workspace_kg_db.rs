//! Workspace knowledge graph store: `.ken-workspace/kg.sqlite`.
//!
//! This is the **workspace KG** from `openspec/changes/federated-kg`: a
//! derived federation of the member per-project knowledge models (the
//! `entities` / `entity_edges` tables in [`crate::db::Db`]). Every table
//! here is fully rebuildable from member DBs (design D1/D3) — deleting
//! `kg.sqlite` and rebuilding is always safe, and no member DB is ever
//! written by this module.
//!
//! This module owns schema, open/migrate, and CRUD only. Entity
//! resolution and the snapshot→merge build orchestration live in
//! [`crate::federation`].
//!
//! Deviation from `design.md`: the design assumes a `workspace.rs`
//! (Phase 2) that resolves the workspace root and enumerates members;
//! that module does not exist yet in this codebase. [`open`] therefore
//! takes a plain `workspace_root: &Path` and joins the fixed
//! `.ken-workspace/kg.sqlite` location the design specifies (mirroring
//! `db::db_path`'s `base.join(...)` pattern) rather than depending on a
//! workspace handle type.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use uuid::Uuid;

use crate::{Error, Result};

pub const SCHEMA_VERSION: i64 = 1;

pub struct WorkspaceKgDb {
    conn: Connection,
}

/// `<workspace_root>/.ken-workspace/kg.sqlite` — fixed per design D1
/// ("lives beside workspace.json").
pub fn kg_db_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".ken-workspace").join("kg.sqlite")
}

impl WorkspaceKgDb {
    /// Open (creating if absent) the workspace KG for `workspace_root`,
    /// migrating it to [`SCHEMA_VERSION`].
    pub fn open(workspace_root: &Path) -> Result<Self> {
        let path = kg_db_path(workspace_root);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        Self::open_at(&path)
    }

    pub fn open_at(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // Mirrors db::Db::open_at: kg.sqlite can be written by both the
        // build orchestrator and (later) read commands on separate
        // connections; a short busy wait beats an immediate SQLITE_BUSY.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let db = WorkspaceKgDb { conn };
        db.migrate()?;
        Ok(db)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let db = WorkspaceKgDb { conn };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        let version: i64 = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |r| r.get::<_, String>(0),
            )
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);

        if version < 1 {
            self.conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS meta (
                    key   TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                );

                -- One row per merged concept (proposal.md: kinds reuse the
                -- per-project set person|organization|topic|decision|other).
                -- `summary` is always non-empty once written by the merge
                -- pass (spec: "every global entity is a wiki page").
                CREATE TABLE IF NOT EXISTS global_entities (
                    id         INTEGER PRIMARY KEY,
                    kind       TEXT NOT NULL,
                    name       TEXT NOT NULL,
                    summary    TEXT NOT NULL DEFAULT '',
                    updated_at INTEGER NOT NULL
                );

                -- The federation mapping: one global entity <-> N local
                -- (project_id, local_entity_id) entities. `project_id` is a
                -- member's `ProjectConfig.id` (Uuid) stored as TEXT — rusqlite
                -- has no native Uuid binding in this workspace (see db.rs,
                -- which only ever uses Uuid for file naming, never as a
                -- column value). `local_entity_id` is that member's
                -- `entities.id`. UNIQUE enforces "one local entity maps to at
                -- most one global entity".
                CREATE TABLE IF NOT EXISTS entity_links (
                    id              INTEGER PRIMARY KEY,
                    global_id       INTEGER NOT NULL
                        REFERENCES global_entities(id) ON DELETE CASCADE,
                    project_id      TEXT NOT NULL,
                    local_entity_id INTEGER NOT NULL,
                    local_name      TEXT NOT NULL,
                    UNIQUE(project_id, local_entity_id)
                );
                CREATE INDEX IF NOT EXISTS entity_links_global
                    ON entity_links(global_id);
                CREATE INDEX IF NOT EXISTS entity_links_project
                    ON entity_links(project_id);

                -- Cross-project relations. `provenance` is one of
                -- 'imported' | 'cooccur' | 'llm' (spec: "Cross-project edges
                -- with provenance"); not CHECK-constrained here so a future
                -- provenance kind never requires a migration, but every
                -- writer in federation.rs must use one of the three.
                CREATE TABLE IF NOT EXISTS global_edges (
                    id            INTEGER PRIMARY KEY,
                    src_global_id INTEGER NOT NULL
                        REFERENCES global_entities(id) ON DELETE CASCADE,
                    dst_global_id INTEGER NOT NULL
                        REFERENCES global_entities(id) ON DELETE CASCADE,
                    relation      TEXT NOT NULL DEFAULT '',
                    weight        REAL NOT NULL DEFAULT 1.0,
                    provenance    TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS global_edges_src
                    ON global_edges(src_global_id);
                CREATE INDEX IF NOT EXISTS global_edges_dst
                    ON global_edges(dst_global_id);

                -- Top source files per entity per member, for wiki-page
                -- "mentioned in" sections; resolves to
                -- `ken://<project_id>/<rel_path>` (design D4).
                CREATE TABLE IF NOT EXISTS doc_pointers (
                    id         INTEGER PRIMARY KEY,
                    global_id  INTEGER NOT NULL
                        REFERENCES global_entities(id) ON DELETE CASCADE,
                    project_id TEXT NOT NULL,
                    rel_path   TEXT NOT NULL,
                    snippet    TEXT NOT NULL DEFAULT ''
                );
                CREATE INDEX IF NOT EXISTS doc_pointers_global
                    ON doc_pointers(global_id);

                -- Snapshot cache (design D3): one row per member, keyed by
                -- that member's own `knowledge_model_built_at` watermark, so
                -- a rebuild can skip re-reading members whose watermark
                -- hasn't advanced. `watermark` is nullable because a member
                -- that has never built a knowledge model reports `None` from
                -- `Db::knowledge_model_built_at()` — that is itself a valid,
                -- cacheable (empty) snapshot state, not "no cache row".
                CREATE TABLE IF NOT EXISTS member_snapshots (
                    project_id TEXT PRIMARY KEY,
                    watermark  INTEGER,
                    snapshot   TEXT NOT NULL,
                    cached_at  INTEGER NOT NULL
                );
                "#,
            )?;
        }
        self.conn.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', ?1)",
            params![SCHEMA_VERSION.to_string()],
        )?;
        Ok(())
    }

    // --- meta ---

    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        self.conn
            .query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| {
                r.get::<_, String>(0)
            })
            .optional()
            .map_err(Into::into)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    // --- global_entities ---

    pub fn insert_global_entity(&self, kind: &str, name: &str, summary: &str, updated_at: i64) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO global_entities (kind, name, summary, updated_at) VALUES (?1, ?2, ?3, ?4)",
            params![kind, name, summary, updated_at],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn get_global_entity(&self, id: i64) -> Result<Option<GlobalEntityRow>> {
        self.conn
            .query_row(
                "SELECT id, kind, name, summary, updated_at FROM global_entities WHERE id = ?1",
                params![id],
                Self::map_global_entity,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list_global_entities(&self) -> Result<Vec<GlobalEntityRow>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, kind, name, summary, updated_at FROM global_entities ORDER BY id")?;
        let rows = stmt
            .query_map([], Self::map_global_entity)?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    pub fn update_global_entity_summary(&self, id: i64, summary: &str, updated_at: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE global_entities SET summary = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, summary, updated_at],
        )?;
        Ok(())
    }

    fn map_global_entity(r: &rusqlite::Row) -> rusqlite::Result<GlobalEntityRow> {
        Ok(GlobalEntityRow {
            id: r.get(0)?,
            kind: r.get(1)?,
            name: r.get(2)?,
            summary: r.get(3)?,
            updated_at: r.get(4)?,
        })
    }

    /// Wipe every merged/derived table (`global_entities` and, via
    /// `ON DELETE CASCADE`, `entity_links`/`global_edges`/`doc_pointers`).
    /// The merge pass (task 1.3+) re-runs in full each build (design D3:
    /// "full re-merge each time" — at workspace scale this is cheap and
    /// sidesteps incremental merge-state bugs). `member_snapshots` is
    /// deliberately untouched — that cache survives a re-merge.
    pub fn clear_merged(&mut self) -> Result<()> {
        self.conn.execute("DELETE FROM global_entities", [])?;
        Ok(())
    }

    // --- entity_links ---

    pub fn insert_entity_link(
        &self,
        global_id: i64,
        project_id: Uuid,
        local_entity_id: i64,
        local_name: &str,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO entity_links (global_id, project_id, local_entity_id, local_name)
             VALUES (?1, ?2, ?3, ?4)",
            params![global_id, project_id.to_string(), local_entity_id, local_name],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn list_links_for_global(&self, global_id: i64) -> Result<Vec<EntityLinkRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, global_id, project_id, local_entity_id, local_name
             FROM entity_links WHERE global_id = ?1 ORDER BY id",
        )?;
        let rows = stmt
            .query_map(params![global_id], Self::map_entity_link)?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    pub fn list_links_for_project(&self, project_id: Uuid) -> Result<Vec<EntityLinkRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, global_id, project_id, local_entity_id, local_name
             FROM entity_links WHERE project_id = ?1 ORDER BY id",
        )?;
        let rows = stmt
            .query_map(params![project_id.to_string()], Self::map_entity_link)?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    /// The global entity a specific member's local entity currently maps
    /// to, if the merge pass has linked it.
    pub fn find_global_id_for_local(&self, project_id: Uuid, local_entity_id: i64) -> Result<Option<i64>> {
        self.conn
            .query_row(
                "SELECT global_id FROM entity_links WHERE project_id = ?1 AND local_entity_id = ?2",
                params![project_id.to_string(), local_entity_id],
                |r| r.get::<_, i64>(0),
            )
            .optional()
            .map_err(Into::into)
    }

    fn map_entity_link(r: &rusqlite::Row) -> rusqlite::Result<EntityLinkRow> {
        Ok(EntityLinkRow {
            id: r.get(0)?,
            global_id: r.get(1)?,
            project_id: r.get(2)?,
            local_entity_id: r.get(3)?,
            local_name: r.get(4)?,
        })
    }

    // --- global_edges ---

    /// `provenance` must be one of `"imported"` | `"cooccur"` | `"llm"`
    /// (spec: "Cross-project edges with provenance") — not enforced by a
    /// CHECK constraint (see schema comment), so callers are the contract.
    pub fn insert_global_edge(
        &self,
        src_global_id: i64,
        dst_global_id: i64,
        relation: &str,
        weight: f64,
        provenance: &str,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO global_edges (src_global_id, dst_global_id, relation, weight, provenance)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![src_global_id, dst_global_id, relation, weight, provenance],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Out-edges (`src_global_id = id`) — the wiki page's out-links.
    pub fn list_edges_from(&self, global_id: i64) -> Result<Vec<GlobalEdgeRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, src_global_id, dst_global_id, relation, weight, provenance
             FROM global_edges WHERE src_global_id = ?1 ORDER BY id",
        )?;
        let rows = stmt
            .query_map(params![global_id], Self::map_global_edge)?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    /// In-edges (`dst_global_id = id`) — the wiki page's back-links
    /// (design D4: "Back-links are not stored separately — `global_edges`
    /// is queried in both directions").
    pub fn list_edges_to(&self, global_id: i64) -> Result<Vec<GlobalEdgeRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, src_global_id, dst_global_id, relation, weight, provenance
             FROM global_edges WHERE dst_global_id = ?1 ORDER BY id",
        )?;
        let rows = stmt
            .query_map(params![global_id], Self::map_global_edge)?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    pub fn list_all_edges(&self) -> Result<Vec<GlobalEdgeRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, src_global_id, dst_global_id, relation, weight, provenance
             FROM global_edges ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], Self::map_global_edge)?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    fn map_global_edge(r: &rusqlite::Row) -> rusqlite::Result<GlobalEdgeRow> {
        Ok(GlobalEdgeRow {
            id: r.get(0)?,
            src_global_id: r.get(1)?,
            dst_global_id: r.get(2)?,
            relation: r.get(3)?,
            weight: r.get(4)?,
            provenance: r.get(5)?,
        })
    }

    // --- doc_pointers ---

    pub fn insert_doc_pointer(
        &self,
        global_id: i64,
        project_id: Uuid,
        rel_path: &str,
        snippet: &str,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO doc_pointers (global_id, project_id, rel_path, snippet)
             VALUES (?1, ?2, ?3, ?4)",
            params![global_id, project_id.to_string(), rel_path, snippet],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn list_pointers_for_global(&self, global_id: i64) -> Result<Vec<DocPointerRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, global_id, project_id, rel_path, snippet
             FROM doc_pointers WHERE global_id = ?1 ORDER BY id",
        )?;
        let rows = stmt
            .query_map(params![global_id], Self::map_doc_pointer)?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    fn map_doc_pointer(r: &rusqlite::Row) -> rusqlite::Result<DocPointerRow> {
        Ok(DocPointerRow {
            id: r.get(0)?,
            global_id: r.get(1)?,
            project_id: r.get(2)?,
            rel_path: r.get(3)?,
            snippet: r.get(4)?,
        })
    }

    // --- entity -> project ranking (kg-routing task 1.2) ---

    /// Rank member projects by how strongly they're grounded in a set of
    /// matched global entities — the read `routing::plan_route`'s KG-guided
    /// tier uses to turn "these global entities matched the query" into
    /// "search these member projects" (design: "ranked by link count and
    /// pointer density"). For each project holding at least one
    /// `entity_links` row for any id in `global_ids`: `link_count` is how
    /// many of the matched entities that project participates in, and
    /// `pointer_count` is the total `doc_pointers` rows for those same
    /// entities in that project (a density signal — a project with more
    /// grounding text for the matched entities, not just more of them,
    /// ranks higher on ties). Ordered `link_count` DESC, `pointer_count`
    /// DESC, `project_id` ASC (the last a deterministic tie-break with no
    /// product meaning — routing.rs is the layer that decides what a tie
    /// means, this is a plain read). Returns every scored project
    /// uncapped; callers (`plan_route`) apply the "cap 3" policy so that
    /// decision stays out of the storage layer. Empty `global_ids` returns
    /// an empty `Vec` rather than every project (there is nothing to rank).
    pub fn rank_projects_for_entities(&self, global_ids: &[i64]) -> Result<Vec<ProjectEntityRank>> {
        if global_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = global_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");

        let mut link_counts: HashMap<String, i64> = HashMap::new();
        {
            let sql = format!(
                "SELECT project_id, COUNT(*) FROM entity_links
                 WHERE global_id IN ({placeholders}) GROUP BY project_id"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(global_ids.iter()), |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?;
            for row in rows {
                let (project_id, count) = row?;
                link_counts.insert(project_id, count);
            }
        }

        let mut pointer_counts: HashMap<String, i64> = HashMap::new();
        {
            let sql = format!(
                "SELECT project_id, COUNT(*) FROM doc_pointers
                 WHERE global_id IN ({placeholders}) GROUP BY project_id"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(global_ids.iter()), |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?;
            for row in rows {
                let (project_id, count) = row?;
                pointer_counts.insert(project_id, count);
            }
        }

        let mut out: Vec<ProjectEntityRank> = link_counts
            .into_iter()
            .map(|(project_id, link_count)| {
                let pointer_count = pointer_counts.get(&project_id).copied().unwrap_or(0);
                ProjectEntityRank {
                    project_id,
                    link_count,
                    pointer_count,
                }
            })
            .collect();
        out.sort_by(|a, b| {
            b.link_count
                .cmp(&a.link_count)
                .then(b.pointer_count.cmp(&a.pointer_count))
                .then(a.project_id.cmp(&b.project_id))
        });
        Ok(out)
    }

    // --- member_snapshots (snapshot cache + incremental-rebuild watermark) ---

    /// The cached watermark for `project_id`, distinguishing "no cache row
    /// yet" (outer `None`) from "cached, but the member has never built a
    /// knowledge model" (inner `None` — see the `member_snapshots` schema
    /// comment). Cheaper than [`Self::get_snapshot`] when the caller only
    /// needs to decide whether to re-read the member.
    pub fn get_watermark(&self, project_id: Uuid) -> Result<Option<Option<i64>>> {
        self.conn
            .query_row(
                "SELECT watermark FROM member_snapshots WHERE project_id = ?1",
                params![project_id.to_string()],
                |r| r.get::<_, Option<i64>>(0),
            )
            .optional()
            .map_err(Into::into)
    }

    /// Upsert the cached snapshot for `project_id`. `snapshot` is an
    /// opaque, caller-serialized blob (federation.rs stores JSON); this
    /// layer never inspects it.
    pub fn set_snapshot(&self, project_id: Uuid, watermark: Option<i64>, snapshot: &str, cached_at: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO member_snapshots (project_id, watermark, snapshot, cached_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(project_id) DO UPDATE SET
                watermark = excluded.watermark,
                snapshot = excluded.snapshot,
                cached_at = excluded.cached_at",
            params![project_id.to_string(), watermark, snapshot, cached_at],
        )?;
        Ok(())
    }

    pub fn get_snapshot(&self, project_id: Uuid) -> Result<Option<MemberSnapshotRow>> {
        self.conn
            .query_row(
                "SELECT project_id, watermark, snapshot, cached_at
                 FROM member_snapshots WHERE project_id = ?1",
                params![project_id.to_string()],
                Self::map_snapshot_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn delete_snapshot(&self, project_id: Uuid) -> Result<()> {
        self.conn.execute(
            "DELETE FROM member_snapshots WHERE project_id = ?1",
            params![project_id.to_string()],
        )?;
        Ok(())
    }

    /// Every member currently holding a cached snapshot — used to garbage
    /// collect entries for members removed from the workspace (not wired
    /// up yet; a later task's concern).
    pub fn list_cached_project_ids(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT project_id FROM member_snapshots ORDER BY project_id")?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    fn map_snapshot_row(r: &rusqlite::Row) -> rusqlite::Result<MemberSnapshotRow> {
        Ok(MemberSnapshotRow {
            project_id: r.get(0)?,
            watermark: r.get(1)?,
            snapshot: r.get(2)?,
            cached_at: r.get(3)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalEntityRow {
    pub id: i64,
    pub kind: String,
    pub name: String,
    pub summary: String,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityLinkRow {
    pub id: i64,
    pub global_id: i64,
    /// Member project id (`Uuid` as `to_string()`).
    pub project_id: String,
    pub local_entity_id: i64,
    pub local_name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalEdgeRow {
    pub id: i64,
    pub src_global_id: i64,
    pub dst_global_id: i64,
    pub relation: String,
    pub weight: f64,
    /// `"imported"` | `"cooccur"` | `"llm"`.
    pub provenance: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocPointerRow {
    pub id: i64,
    pub global_id: i64,
    pub project_id: String,
    pub rel_path: String,
    pub snippet: String,
}

/// One project's ranking for a set of matched global entities (see
/// [`WorkspaceKgDb::rank_projects_for_entities`]).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectEntityRank {
    /// Member project id (`Uuid` as `to_string()`), matching
    /// [`EntityLinkRow::project_id`]'s representation.
    pub project_id: String,
    pub link_count: i64,
    pub pointer_count: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberSnapshotRow {
    pub project_id: String,
    pub watermark: Option<i64>,
    pub snapshot: String,
    pub cached_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_version_recorded_on_fresh_db() {
        let db = WorkspaceKgDb::open_in_memory().unwrap();
        assert_eq!(
            db.get_meta("schema_version").unwrap(),
            Some(SCHEMA_VERSION.to_string())
        );
    }

    #[test]
    fn opening_twice_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kg.sqlite");
        {
            let db = WorkspaceKgDb::open_at(&path).unwrap();
            db.insert_global_entity("topic", "Shattered Realms", "A game.", 100)
                .unwrap();
        }
        let db = WorkspaceKgDb::open_at(&path).unwrap();
        assert_eq!(db.list_global_entities().unwrap().len(), 1);
    }

    #[test]
    fn kg_db_path_is_dot_ken_workspace() {
        let root = Path::new("/some/workspace");
        assert_eq!(kg_db_path(root), root.join(".ken-workspace").join("kg.sqlite"));
    }

    #[test]
    fn global_entity_round_trip() {
        let db = WorkspaceKgDb::open_in_memory().unwrap();
        let id = db
            .insert_global_entity("topic", "Shattered Realms", "A game.", 100)
            .unwrap();
        let row = db.get_global_entity(id).unwrap().unwrap();
        assert_eq!(row.name, "Shattered Realms");
        assert_eq!(row.kind, "topic");
        assert_eq!(row.summary, "A game.");

        db.update_global_entity_summary(id, "An MMO.", 200).unwrap();
        let row = db.get_global_entity(id).unwrap().unwrap();
        assert_eq!(row.summary, "An MMO.");
        assert_eq!(row.updated_at, 200);
    }

    #[test]
    fn entity_links_and_edges_cascade_on_clear_merged() {
        let mut db = WorkspaceKgDb::open_in_memory().unwrap();
        let a = db.insert_global_entity("topic", "A", "sa", 1).unwrap();
        let b = db.insert_global_entity("topic", "B", "sb", 1).unwrap();
        let p1 = Uuid::new_v4();
        let p2 = Uuid::new_v4();
        db.insert_entity_link(a, p1, 10, "A local").unwrap();
        db.insert_entity_link(b, p2, 20, "B local").unwrap();
        db.insert_global_edge(a, b, "related", 1.0, "cooccur").unwrap();
        db.insert_doc_pointer(a, p1, "notes/a.md", "").unwrap();

        assert_eq!(db.list_links_for_global(a).unwrap().len(), 1);
        assert_eq!(db.list_edges_from(a).unwrap().len(), 1);
        assert_eq!(db.list_edges_to(b).unwrap().len(), 1);
        assert_eq!(db.list_pointers_for_global(a).unwrap().len(), 1);
        assert_eq!(
            db.find_global_id_for_local(p1, 10).unwrap(),
            Some(a)
        );

        db.clear_merged().unwrap();
        assert!(db.list_global_entities().unwrap().is_empty());
        assert!(db.list_links_for_global(a).unwrap().is_empty());
        assert!(db.list_edges_from(a).unwrap().is_empty());
        assert!(db.list_pointers_for_global(a).unwrap().is_empty());
    }

    #[test]
    fn entity_links_unique_per_project_and_local_id() {
        let db = WorkspaceKgDb::open_in_memory().unwrap();
        let a = db.insert_global_entity("topic", "A", "sa", 1).unwrap();
        let b = db.insert_global_entity("topic", "B", "sb", 1).unwrap();
        let p1 = Uuid::new_v4();
        db.insert_entity_link(a, p1, 10, "A local").unwrap();
        // Same (project_id, local_entity_id) can't map to a second global
        // entity — the merge pass must resolve BEFORE inserting, not rely
        // on this as a race guard, but it still catches a logic bug.
        assert!(db.insert_entity_link(b, p1, 10, "A local").is_err());
    }

    #[test]
    fn rank_projects_for_entities_orders_by_link_count_then_pointer_density() {
        let db = WorkspaceKgDb::open_in_memory().unwrap();
        let a = db.insert_global_entity("topic", "Shattered Realms", "sa", 1).unwrap();
        let b = db.insert_global_entity("topic", "Priya", "sb", 1).unwrap();
        let p_high = Uuid::new_v4(); // linked to both entities, 1 pointer
        let p_mid = Uuid::new_v4(); // linked to one entity, 2 pointers (density beats a tie)
        let p_low = Uuid::new_v4(); // linked to one entity, 0 pointers
        let p_unrelated = Uuid::new_v4(); // no links to the matched entities at all

        db.insert_entity_link(a, p_high, 1, "A@high").unwrap();
        db.insert_entity_link(b, p_high, 2, "B@high").unwrap();
        db.insert_doc_pointer(a, p_high, "notes/high.md", "").unwrap();

        db.insert_entity_link(a, p_mid, 3, "A@mid").unwrap();
        db.insert_doc_pointer(a, p_mid, "notes/mid1.md", "").unwrap();
        db.insert_doc_pointer(a, p_mid, "notes/mid2.md", "").unwrap();

        db.insert_entity_link(a, p_low, 4, "A@low").unwrap();
        // p_unrelated gets no entity_links row at all — must not appear.

        let ranked = db.rank_projects_for_entities(&[a, b]).unwrap();
        let ids: Vec<String> = ranked.iter().map(|r| r.project_id.clone()).collect();
        // p_high has 2 links (both matched entities) so it leads regardless
        // of pointer density; p_mid and p_low both have 1 link, so pointer
        // density (2 vs 0) breaks the tie.
        assert_eq!(
            ids,
            vec![p_high.to_string(), p_mid.to_string(), p_low.to_string()]
        );
        assert_eq!(ranked[0].link_count, 2);
        assert_eq!(ranked[0].pointer_count, 1);
        assert_eq!(ranked[1].link_count, 1);
        assert_eq!(ranked[1].pointer_count, 2);
        assert_eq!(ranked[2].link_count, 1);
        assert_eq!(ranked[2].pointer_count, 0);
        assert!(!ids.contains(&p_unrelated.to_string()));
    }

    #[test]
    fn rank_projects_for_entities_empty_input_is_empty_output() {
        let db = WorkspaceKgDb::open_in_memory().unwrap();
        assert!(db.rank_projects_for_entities(&[]).unwrap().is_empty());
    }

    #[test]
    fn watermark_get_set_distinguishes_no_row_from_null_watermark() {
        let db = WorkspaceKgDb::open_in_memory().unwrap();
        let p = Uuid::new_v4();
        // No cache row yet.
        assert_eq!(db.get_watermark(p).unwrap(), None);

        // Cached, but the member had never built a knowledge model.
        db.set_snapshot(p, None, "{}", 100).unwrap();
        assert_eq!(db.get_watermark(p).unwrap(), Some(None));

        // Cached with a real watermark; upsert overwrites in place.
        db.set_snapshot(p, Some(42), "{\"v\":1}", 200).unwrap();
        assert_eq!(db.get_watermark(p).unwrap(), Some(Some(42)));
        let row = db.get_snapshot(p).unwrap().unwrap();
        assert_eq!(row.watermark, Some(42));
        assert_eq!(row.snapshot, "{\"v\":1}");
        assert_eq!(row.cached_at, 200);
        assert_eq!(db.list_cached_project_ids().unwrap(), vec![p.to_string()]);

        db.delete_snapshot(p).unwrap();
        assert_eq!(db.get_watermark(p).unwrap(), None);
        assert!(db.list_cached_project_ids().unwrap().is_empty());
    }
}

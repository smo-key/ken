//! Per-project SQLite database in app-data: file inventory, extracted text,
//! FTS5 search index. Entirely derived — `rebuild()` from the folder is the
//! universal recovery story — and never stored inside the project folder.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};
use serde::Serialize;
use uuid::Uuid;

use crate::{Error, Result};

pub const SCHEMA_VERSION: i64 = 7;

pub struct Db {
    conn: Connection,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileRow {
    pub rel_path: String,
    pub kind: String,
    pub size: i64,
    pub mtime: i64,
    /// `indexed` | `metadata_only` | `failed`
    pub status: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub rel_path: String,
    pub kind: String,
    pub status: String,
    /// Snippet with `<mark>` tags around matched terms.
    pub snippet: String,
    pub rank: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRow {
    pub id: i64,
    pub slug: String,
    pub session_id: Option<String>,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    /// `running` | `fresh` | `blocked` | `pending_approval` | `failed` |
    /// `discarded` | `cancelled`
    pub status: String,
    pub summary: Option<String>,
    pub error: Option<String>,
    pub change_ratio: Option<f64>,
}

pub fn db_path(base: &Path, project_id: Uuid) -> PathBuf {
    base.join("index").join(format!("{project_id}.db"))
}

impl Db {
    pub fn open(base: &Path, project_id: Uuid) -> Result<Db> {
        let path = db_path(base, project_id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        Db::open_at(&path)
    }

    /// Open an existing index without any ability to write — no migration,
    /// no pragmas, and SQLite itself refuses every write. This is the only
    /// handle `ken-mcp` uses; a missing index is an error (the app has to
    /// build it first).
    pub fn open_read_only(base: &Path, project_id: Uuid) -> Result<Db> {
        let path = db_path(base, project_id);
        let conn = Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        Ok(Db { conn })
    }

    pub fn open_at(path: &Path) -> Result<Db> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let db = Db { conn };
        db.migrate()?;
        Ok(db)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Db> {
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let db = Db { conn };
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
                CREATE TABLE files (
                    id       INTEGER PRIMARY KEY,
                    rel_path TEXT NOT NULL UNIQUE,
                    kind     TEXT NOT NULL,
                    size     INTEGER NOT NULL,
                    mtime    INTEGER NOT NULL,
                    status   TEXT NOT NULL,
                    error    TEXT
                );
                CREATE TABLE contents (
                    file_id INTEGER PRIMARY KEY
                            REFERENCES files(id) ON DELETE CASCADE,
                    text    TEXT NOT NULL,
                    name    TEXT NOT NULL
                );
                CREATE VIRTUAL TABLE search USING fts5(
                    text, name,
                    content='contents', content_rowid='file_id'
                );
                CREATE TRIGGER contents_ai AFTER INSERT ON contents BEGIN
                    INSERT INTO search(rowid, text, name)
                    VALUES (new.file_id, new.text, new.name);
                END;
                CREATE TRIGGER contents_ad AFTER DELETE ON contents BEGIN
                    INSERT INTO search(search, rowid, text, name)
                    VALUES ('delete', old.file_id, old.text, old.name);
                END;
                CREATE TRIGGER contents_au AFTER UPDATE ON contents BEGIN
                    INSERT INTO search(search, rowid, text, name)
                    VALUES ('delete', old.file_id, old.text, old.name);
                    INSERT INTO search(rowid, text, name)
                    VALUES (new.file_id, new.text, new.name);
                END;
                "#,
            )?;
        }
        if version < 2 {
            self.conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS ingest_runs (
                    id           INTEGER PRIMARY KEY,
                    slug         TEXT NOT NULL,
                    session_id   TEXT,
                    started_at   INTEGER NOT NULL,
                    finished_at  INTEGER,
                    status       TEXT NOT NULL,
                    summary      TEXT,
                    error        TEXT,
                    change_ratio REAL
                );
                CREATE INDEX IF NOT EXISTS ingest_runs_slug
                    ON ingest_runs(slug, started_at DESC);
                "#,
            )?;
        }
        if version < 3 {
            self.conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS chats (
                    id             TEXT PRIMARY KEY,
                    title          TEXT NOT NULL,
                    kind           TEXT NOT NULL DEFAULT 'user',
                    pinned         INTEGER NOT NULL DEFAULT 0,
                    status         TEXT NOT NULL DEFAULT 'done',
                    created_at     INTEGER NOT NULL,
                    last_active_at INTEGER NOT NULL,
                    archived       INTEGER NOT NULL DEFAULT 0
                );
                CREATE TABLE IF NOT EXISTS chat_messages (
                    id         INTEGER PRIMARY KEY,
                    chat_id    TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
                    role       TEXT NOT NULL,
                    content    TEXT NOT NULL,
                    created_at INTEGER NOT NULL
                );
                CREATE INDEX IF NOT EXISTS chat_messages_chat
                    ON chat_messages(chat_id, id);
                "#,
            )?;
        }
        if version < 4 {
            self.conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS review_items (
                    id          INTEGER PRIMARY KEY,
                    kind        TEXT NOT NULL,
                    title       TEXT NOT NULL,
                    body        TEXT NOT NULL DEFAULT '',
                    source_ref  TEXT NOT NULL DEFAULT '',
                    status      TEXT NOT NULL DEFAULT 'open',
                    payload     TEXT,
                    created_at  INTEGER NOT NULL,
                    resolved_at INTEGER
                );
                CREATE INDEX IF NOT EXISTS review_items_status
                    ON review_items(status, created_at DESC);
                "#,
            )?;
        }
        if version < 5 {
            self.conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS digests (
                    id         INTEGER PRIMARY KEY,
                    date       TEXT NOT NULL UNIQUE,
                    content    TEXT NOT NULL,
                    created_at INTEGER NOT NULL
                );
                "#,
            )?;
        }
        if version < 6 {
            self.conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS entities (
                    id      INTEGER PRIMARY KEY,
                    kind    TEXT NOT NULL,
                    name    TEXT NOT NULL,
                    summary TEXT NOT NULL DEFAULT '',
                    sources TEXT NOT NULL DEFAULT '[]'
                );
                CREATE TABLE IF NOT EXISTS entity_edges (
                    id    INTEGER PRIMARY KEY,
                    a     INTEGER NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
                    b     INTEGER NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
                    label TEXT NOT NULL DEFAULT ''
                );
                CREATE TABLE IF NOT EXISTS events (
                    id       INTEGER PRIMARY KEY,
                    date     TEXT NOT NULL,
                    category TEXT NOT NULL,
                    text     TEXT NOT NULL,
                    source   TEXT NOT NULL DEFAULT ''
                );
                "#,
            )?;
        }
        if version < 7 {
            // Per-chat model choice (a stable tier alias like 'opus', or NULL
            // for the CLI's own default). Nullable so existing chats keep the
            // default with no backfill. Guarded because ADD COLUMN has no
            // IF NOT EXISTS and the column may already be present (a fresh DB
            // creates the full schema, then tests rewind schema_version).
            let has_model: bool = self
                .conn
                .prepare("SELECT 1 FROM pragma_table_info('chats') WHERE name = 'model'")?
                .exists([])?;
            if !has_model {
                self.conn
                    .execute_batch("ALTER TABLE chats ADD COLUMN model TEXT;")?;
            }
        }
        self.conn.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', ?1)",
            params![SCHEMA_VERSION.to_string()],
        )?;
        Ok(())
    }

    /// Insert or update a file's index entry. `text` is the extracted
    /// content (empty for metadata-only/failed files); `name` is the
    /// filename tokenized for name search.
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_file(
        &mut self,
        rel_path: &str,
        kind: &str,
        size: i64,
        mtime: i64,
        status: &str,
        error: Option<&str>,
        text: &str,
    ) -> Result<()> {
        let name = name_tokens(rel_path);
        let tx = self.conn.transaction()?;
        tx.execute(
            r#"INSERT INTO files (rel_path, kind, size, mtime, status, error)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6)
               ON CONFLICT(rel_path) DO UPDATE SET
                 kind = ?2, size = ?3, mtime = ?4, status = ?5, error = ?6"#,
            params![rel_path, kind, size, mtime, status, error],
        )?;
        let file_id: i64 = tx.query_row(
            "SELECT id FROM files WHERE rel_path = ?1",
            params![rel_path],
            |r| r.get(0),
        )?;
        let existing: bool = tx
            .query_row(
                "SELECT 1 FROM contents WHERE file_id = ?1",
                params![file_id],
                |_| Ok(true),
            )
            .unwrap_or(false);
        if existing {
            tx.execute(
                "UPDATE contents SET text = ?2, name = ?3 WHERE file_id = ?1",
                params![file_id, text, name],
            )?;
        } else {
            tx.execute(
                "INSERT INTO contents (file_id, text, name) VALUES (?1, ?2, ?3)",
                params![file_id, text, name],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn remove_file(&mut self, rel_path: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        let file_id: Option<i64> = tx
            .query_row(
                "SELECT id FROM files WHERE rel_path = ?1",
                params![rel_path],
                |r| r.get(0),
            )
            .ok();
        if let Some(id) = file_id {
            // Delete contents first so the FTS delete trigger fires, then
            // the file row.
            tx.execute("DELETE FROM contents WHERE file_id = ?1", params![id])?;
            tx.execute("DELETE FROM files WHERE id = ?1", params![id])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Remove every indexed file under a folder (used when a folder is
    /// excluded or deleted).
    pub fn remove_folder(&mut self, rel_folder: &str) -> Result<()> {
        let prefix = format!("{}/", rel_folder.trim_matches('/'));
        let mut stmt = self.conn.prepare(
            "SELECT rel_path FROM files WHERE rel_path = ?1 OR rel_path LIKE ?2 ESCAPE '\\'",
        )?;
        let like = format!("{}%", like_escape(&prefix));
        let paths: Vec<String> = stmt
            .query_map(params![rel_folder.trim_matches('/'), like], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        drop(stmt);
        for p in paths {
            self.remove_file(&p)?;
        }
        Ok(())
    }

    pub fn get_file(&self, rel_path: &str) -> Result<Option<FileRow>> {
        let row = self
            .conn
            .query_row(
                "SELECT rel_path, kind, size, mtime, status, error FROM files WHERE rel_path = ?1",
                params![rel_path],
                |r| {
                    Ok(FileRow {
                        rel_path: r.get(0)?,
                        kind: r.get(1)?,
                        size: r.get(2)?,
                        mtime: r.get(3)?,
                        status: r.get(4)?,
                        error: r.get(5)?,
                    })
                },
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        Ok(row)
    }

    /// Extracted text stored in the index for a file, if any. This is what
    /// binary kinds (docx/pdf/…) expose to readers that can't parse the
    /// original bytes.
    pub fn get_text(&self, rel_path: &str) -> Result<Option<String>> {
        match self.conn.query_row(
            "SELECT c.text FROM contents c JOIN files f ON f.id = c.file_id
             WHERE f.rel_path = ?1",
            params![rel_path],
            |r| r.get(0),
        ) {
            Ok(text) => Ok(Some(text)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn list_files(&self) -> Result<Vec<FileRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT rel_path, kind, size, mtime, status, error FROM files ORDER BY rel_path",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(FileRow {
                    rel_path: r.get(0)?,
                    kind: r.get(1)?,
                    size: r.get(2)?,
                    mtime: r.get(3)?,
                    status: r.get(4)?,
                    error: r.get(5)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    /// Ranked full-text search. Query is treated as-you-type: all terms
    /// required, last term matched as a prefix. Filename matches are
    /// boosted over body matches, and a second query-time pass matches
    /// filenames by case-insensitive substring (every token must appear
    /// somewhere in the rel_path) so partial names like "roadmap" still
    /// find `Q3RoadmapFinal.docx`; both passes are merged and deduped.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let tokens = query_tokens(query);
        if tokens.is_empty() {
            return Ok(Vec::new());
        }
        let fts_query = build_fts_query(&tokens);
        let mut stmt = self.conn.prepare(
            r#"SELECT f.rel_path, f.kind, f.status,
                      snippet(search, 0, '<mark>', '</mark>', '…', 12),
                      bm25(search, 1.0, 4.0)
               FROM search JOIN files f ON f.id = search.rowid
               WHERE search MATCH ?1
               ORDER BY bm25(search, 1.0, 4.0)
               LIMIT ?2"#,
        )?;
        let mut hits: Vec<SearchHit> = stmt
            .query_map(params![fts_query, limit as i64], |r| {
                Ok(SearchHit {
                    rel_path: r.get(0)?,
                    kind: r.get(1)?,
                    status: r.get(2)?,
                    snippet: r.get(3)?,
                    rank: r.get(4)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        drop(stmt);

        // Filename pass: every token as a case-insensitive substring of the
        // rel_path. Catches what FTS name tokens can't — mid-word matches
        // and non-final prefixes.
        let lowered: Vec<String> = tokens.iter().map(|t| t.to_lowercase()).collect();
        let clauses = vec!["lower(rel_path) LIKE ? ESCAPE '\\'"; lowered.len()].join(" AND ");
        let sql = format!("SELECT rel_path, kind, status FROM files WHERE {clauses}");
        let mut stmt = self.conn.prepare(&sql)?;
        let patterns: Vec<String> = lowered
            .iter()
            .map(|t| format!("%{}%", like_escape(t)))
            .collect();
        let name_rows: Vec<(String, String, String)> = stmt
            .query_map(rusqlite::params_from_iter(&patterns), |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect::<std::result::Result<_, _>>()?;

        for (rel_path, kind, status) in name_rows {
            let rank = name_match_rank(&rel_path, query);
            match hits.iter_mut().find(|h| h.rel_path == rel_path) {
                Some(hit) => {
                    // Both passes hit: keep the better rank, and the content
                    // snippet unless it's empty (metadata-only/failed files).
                    if rank < hit.rank {
                        hit.rank = rank;
                    }
                    if hit.snippet.is_empty() {
                        hit.snippet = name_snippet(&rel_path, &lowered);
                    }
                }
                None => hits.push(SearchHit {
                    snippet: name_snippet(&rel_path, &lowered),
                    rel_path,
                    kind,
                    status,
                    rank,
                }),
            }
        }
        hits.sort_by(|a, b| {
            a.rank
                .partial_cmp(&b.rank)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.rel_path.cmp(&b.rel_path))
        });
        hits.truncate(limit);
        Ok(hits)
    }

    /// Drop all indexed data (schema stays). Used by reindex.
    pub fn clear(&mut self) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM contents", [])?;
        tx.execute("DELETE FROM files", [])?;
        tx.commit()?;
        Ok(())
    }

    pub fn file_count(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?)
    }

    /// Count of files whose content is actually in the index (status
    /// `indexed`). The auto-build guard ("nothing to read → don't build")
    /// and the model builder both read only these rows, so a folder of
    /// image (metadata_only) / failed / cloud_only files — nonzero
    /// `file_count` but zero readable content — must count as nothing to
    /// build, or the tick launches a paid session that reads nothing.
    pub fn indexed_file_count(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM files WHERE status = 'indexed'",
            [],
            |r| r.get(0),
        )?)
    }

    // --- ingest run log ---

    pub fn insert_run(&mut self, slug: &str, session_id: Option<&str>, started_at: i64) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO ingest_runs (slug, session_id, started_at, status)
             VALUES (?1, ?2, ?3, 'running')",
            params![slug, session_id, started_at],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn update_run(
        &mut self,
        id: i64,
        status: &str,
        finished_at: Option<i64>,
        summary: Option<&str>,
        error: Option<&str>,
        change_ratio: Option<f64>,
    ) -> Result<()> {
        self.conn.execute(
            r#"UPDATE ingest_runs SET
                 status = ?2,
                 finished_at = COALESCE(?3, finished_at),
                 summary = COALESCE(?4, summary),
                 error = COALESCE(?5, error),
                 change_ratio = COALESCE(?6, change_ratio)
               WHERE id = ?1"#,
            params![id, status, finished_at, summary, error, change_ratio],
        )?;
        Ok(())
    }

    fn map_run(r: &rusqlite::Row) -> rusqlite::Result<RunRow> {
        Ok(RunRow {
            id: r.get(0)?,
            slug: r.get(1)?,
            session_id: r.get(2)?,
            started_at: r.get(3)?,
            finished_at: r.get(4)?,
            status: r.get(5)?,
            summary: r.get(6)?,
            error: r.get(7)?,
            change_ratio: r.get(8)?,
        })
    }

    const RUN_COLS: &'static str =
        "id, slug, session_id, started_at, finished_at, status, summary, error, change_ratio";

    pub fn get_run(&self, id: i64) -> Result<Option<RunRow>> {
        let sql = format!("SELECT {} FROM ingest_runs WHERE id = ?1", Self::RUN_COLS);
        match self.conn.query_row(&sql, params![id], Self::map_run) {
            Ok(row) => Ok(Some(row)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn list_runs(&self, slug: &str, limit: usize) -> Result<Vec<RunRow>> {
        let sql = format!(
            "SELECT {} FROM ingest_runs WHERE slug = ?1 ORDER BY started_at DESC, id DESC LIMIT ?2",
            Self::RUN_COLS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params![slug, limit as i64], Self::map_run)?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    pub fn runs_with_status(&self, status: &str) -> Result<Vec<RunRow>> {
        let sql = format!(
            "SELECT {} FROM ingest_runs WHERE status = ?1 ORDER BY started_at DESC",
            Self::RUN_COLS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params![status], Self::map_run)?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    /// Epoch seconds of the last run that ended `fresh` for this ingest.
    pub fn last_success_at(&self, slug: &str) -> Result<Option<i64>> {
        Ok(self.conn.query_row(
            "SELECT MAX(started_at) FROM ingest_runs WHERE slug = ?1 AND status = 'fresh'",
            params![slug],
            |r| r.get(0),
        )?)
    }

    /// Runs finished at or after `since`, newest first. Feeds the Review
    /// inbox's Done section.
    pub fn runs_finished_since(&self, since: i64) -> Result<Vec<RunRow>> {
        let sql = format!(
            "SELECT {} FROM ingest_runs
             WHERE finished_at IS NOT NULL AND finished_at >= ?1
             ORDER BY finished_at DESC, id DESC",
            Self::RUN_COLS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params![since], Self::map_run)?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    // --- review items (stored substrate; derived inbox kinds live in
    //     their own tables and are assembled at read time) ---

    pub fn insert_review_item(
        &mut self,
        kind: &str,
        title: &str,
        body: &str,
        source_ref: &str,
        payload: Option<&str>,
        created_at: i64,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO review_items (kind, title, body, source_ref, payload, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![kind, title, body, source_ref, payload, created_at],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn get_review_item(&self, id: i64) -> Result<Option<ReviewItemRow>> {
        let sql = format!(
            "SELECT {} FROM review_items WHERE id = ?1",
            Self::REVIEW_ITEM_COLS
        );
        match self.conn.query_row(&sql, params![id], Self::map_review_item) {
            Ok(row) => Ok(Some(row)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Replace a stored item's kind-specific payload (e.g. when an AI merge
    /// draft lands after the item was filed).
    pub fn set_review_item_payload(&mut self, id: i64, payload: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE review_items SET payload = ?2 WHERE id = ?1",
            params![id, payload],
        )?;
        Ok(())
    }

    pub fn resolve_review_item(&mut self, id: i64, at: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE review_items SET status = 'resolved', resolved_at = ?2
             WHERE id = ?1 AND status = 'open'",
            params![id, at],
        )?;
        Ok(())
    }

    fn map_review_item(r: &rusqlite::Row) -> rusqlite::Result<ReviewItemRow> {
        Ok(ReviewItemRow {
            id: r.get(0)?,
            kind: r.get(1)?,
            title: r.get(2)?,
            body: r.get(3)?,
            source_ref: r.get(4)?,
            status: r.get(5)?,
            payload: r.get(6)?,
            created_at: r.get(7)?,
            resolved_at: r.get(8)?,
        })
    }

    const REVIEW_ITEM_COLS: &'static str =
        "id, kind, title, body, source_ref, status, payload, created_at, resolved_at";

    pub fn list_open_review_items(&self) -> Result<Vec<ReviewItemRow>> {
        let sql = format!(
            "SELECT {} FROM review_items WHERE status = 'open'
             ORDER BY created_at DESC, id DESC",
            Self::REVIEW_ITEM_COLS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map([], Self::map_review_item)?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    /// Items resolved at or after `since`, newest first.
    pub fn list_recent_resolved_review_items(&self, since: i64) -> Result<Vec<ReviewItemRow>> {
        let sql = format!(
            "SELECT {} FROM review_items
             WHERE status = 'resolved' AND resolved_at >= ?1
             ORDER BY resolved_at DESC, id DESC",
            Self::REVIEW_ITEM_COLS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params![since], Self::map_review_item)?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    // --- daily digests (one row per local calendar day) ---

    /// Store the digest for a local day, replacing any earlier one.
    pub fn upsert_digest(&mut self, date: &str, content: &str, created_at: i64) -> Result<()> {
        self.conn.execute(
            r#"INSERT INTO digests (date, content, created_at)
               VALUES (?1, ?2, ?3)
               ON CONFLICT(date) DO UPDATE SET content = ?2, created_at = ?3"#,
            params![date, content, created_at],
        )?;
        Ok(())
    }

    fn map_digest(r: &rusqlite::Row) -> rusqlite::Result<DigestRow> {
        Ok(DigestRow {
            id: r.get(0)?,
            date: r.get(1)?,
            content: r.get(2)?,
            created_at: r.get(3)?,
        })
    }

    pub fn get_digest(&self, date: &str) -> Result<Option<DigestRow>> {
        match self.conn.query_row(
            "SELECT id, date, content, created_at FROM digests WHERE date = ?1",
            params![date],
            Self::map_digest,
        ) {
            Ok(row) => Ok(Some(row)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// The newest digest by date (`yyyy-mm-dd` sorts chronologically).
    pub fn latest_digest(&self) -> Result<Option<DigestRow>> {
        match self.conn.query_row(
            "SELECT id, date, content, created_at FROM digests
             ORDER BY date DESC LIMIT 1",
            [],
            Self::map_digest,
        ) {
            Ok(row) => Ok(Some(row)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    // --- chats ---

    pub fn upsert_chat(&mut self, chat: &ChatRow) -> Result<()> {
        self.conn.execute(
            r#"INSERT INTO chats (id, title, kind, pinned, status, created_at, last_active_at, archived, model)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
               ON CONFLICT(id) DO UPDATE SET
                 title = ?2, kind = ?3, pinned = ?4, status = ?5,
                 last_active_at = ?7, archived = ?8, model = ?9"#,
            params![
                chat.id,
                chat.title,
                chat.kind,
                chat.pinned as i64,
                chat.status,
                chat.created_at,
                chat.last_active_at,
                chat.archived as i64,
                chat.model
            ],
        )?;
        Ok(())
    }

    fn map_chat(r: &rusqlite::Row) -> rusqlite::Result<ChatRow> {
        Ok(ChatRow {
            id: r.get(0)?,
            title: r.get(1)?,
            kind: r.get(2)?,
            pinned: r.get::<_, i64>(3)? != 0,
            status: r.get(4)?,
            created_at: r.get(5)?,
            last_active_at: r.get(6)?,
            archived: r.get::<_, i64>(7)? != 0,
            model: r.get(8)?,
        })
    }

    const CHAT_COLS: &'static str =
        "id, title, kind, pinned, status, created_at, last_active_at, archived, model";

    pub fn get_chat(&self, id: &str) -> Result<Option<ChatRow>> {
        let sql = format!("SELECT {} FROM chats WHERE id = ?1", Self::CHAT_COLS);
        match self.conn.query_row(&sql, params![id], Self::map_chat) {
            Ok(row) => Ok(Some(row)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Non-archived chats, pinned first, most recently active first.
    pub fn list_chats(&self) -> Result<Vec<ChatRow>> {
        let sql = format!(
            "SELECT {} FROM chats WHERE archived = 0
             ORDER BY pinned DESC, last_active_at DESC",
            Self::CHAT_COLS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map([], Self::map_chat)?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    pub fn set_chat_field(&mut self, id: &str, field: ChatField, value: &str) -> Result<()> {
        let sql = match field {
            ChatField::Title => "UPDATE chats SET title = ?2 WHERE id = ?1",
            ChatField::Status => "UPDATE chats SET status = ?2 WHERE id = ?1",
        };
        self.conn.execute(sql, params![id, value])?;
        Ok(())
    }

    pub fn set_chat_flag(&mut self, id: &str, field: ChatFlag, value: bool) -> Result<()> {
        let sql = match field {
            ChatFlag::Pinned => "UPDATE chats SET pinned = ?2 WHERE id = ?1",
            ChatFlag::Archived => "UPDATE chats SET archived = ?2 WHERE id = ?1",
        };
        self.conn.execute(sql, params![id, value as i64])?;
        Ok(())
    }

    /// Set (or clear, with None) a chat's chosen model. Applies to the next
    /// message/session — the live process, if any, keeps its current model.
    pub fn set_chat_model(&mut self, id: &str, model: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE chats SET model = ?2 WHERE id = ?1",
            params![id, model],
        )?;
        Ok(())
    }

    pub fn touch_chat(&mut self, id: &str, at: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE chats SET last_active_at = ?2 WHERE id = ?1",
            params![id, at],
        )?;
        Ok(())
    }

    pub fn append_chat_message(
        &mut self,
        chat_id: &str,
        role: &str,
        content: &str,
        at: i64,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO chat_messages (chat_id, role, content, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![chat_id, role, content, at],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn chat_messages(&self, chat_id: &str) -> Result<Vec<ChatMessage>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, chat_id, role, content, created_at
             FROM chat_messages WHERE chat_id = ?1 ORDER BY id",
        )?;
        let rows = stmt
            .query_map(params![chat_id], |r| {
                Ok(ChatMessage {
                    id: r.get(0)?,
                    chat_id: r.get(1)?,
                    role: r.get(2)?,
                    content: r.get(3)?,
                    created_at: r.get(4)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    // --- knowledge model (entities, edges, events — derived read model
    //     for the Map and Timeline screens; replaced wholesale on build) ---

    /// Replace the whole knowledge model in one transaction: entities,
    /// their edges (given as indices into `entities`), events, and the
    /// build timestamp (`knowledge_model_built_at` in `meta`).
    pub fn replace_knowledge_model(
        &mut self,
        entities: &[EntityInput],
        events: &[EventInput],
        built_at: i64,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM entities", [])?; // cascades to entity_edges
        tx.execute("DELETE FROM events", [])?;
        let mut ids: Vec<i64> = Vec::with_capacity(entities.len());
        for e in entities {
            let sources = serde_json::to_string(&e.sources)
                .unwrap_or_else(|_| "[]".into());
            tx.execute(
                "INSERT INTO entities (kind, name, summary, sources)
                 VALUES (?1, ?2, ?3, ?4)",
                params![e.kind, e.name, e.summary, sources],
            )?;
            ids.push(tx.last_insert_rowid());
        }
        for (from, e) in entities.iter().enumerate() {
            for (to, label) in &e.connections {
                let (Some(a), Some(b)) = (ids.get(from), ids.get(*to)) else {
                    continue;
                };
                tx.execute(
                    "INSERT INTO entity_edges (a, b, label) VALUES (?1, ?2, ?3)",
                    params![a, b, label],
                )?;
            }
        }
        for ev in events {
            tx.execute(
                "INSERT INTO events (date, category, text, source)
                 VALUES (?1, ?2, ?3, ?4)",
                params![ev.date, ev.category, ev.text, ev.source],
            )?;
        }
        tx.execute(
            "INSERT OR REPLACE INTO meta(key, value)
             VALUES ('knowledge_model_built_at', ?1)",
            params![built_at.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn list_entities_with_edges(&self) -> Result<(Vec<EntityRow>, Vec<EdgeRow>)> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, name, summary, sources FROM entities ORDER BY id",
        )?;
        let entities: Vec<EntityRow> = stmt
            .query_map([], |r| {
                let sources: String = r.get(4)?;
                Ok(EntityRow {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    name: r.get(2)?,
                    summary: r.get(3)?,
                    sources: serde_json::from_str(&sources).unwrap_or_default(),
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        let mut stmt = self.conn.prepare(
            "SELECT id, a, b, label FROM entity_edges ORDER BY id",
        )?;
        let edges: Vec<EdgeRow> = stmt
            .query_map([], |r| {
                Ok(EdgeRow {
                    id: r.get(0)?,
                    a: r.get(1)?,
                    b: r.get(2)?,
                    label: r.get(3)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        Ok((entities, edges))
    }

    /// Events newest first (`yyyy-mm-dd` sorts chronologically).
    pub fn list_events(&self) -> Result<Vec<EventRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, date, category, text, source FROM events
             ORDER BY date DESC, id DESC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(EventRow {
                    id: r.get(0)?,
                    date: r.get(1)?,
                    category: r.get(2)?,
                    text: r.get(3)?,
                    source: r.get(4)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    /// Epoch seconds of the last knowledge-model build; None before the
    /// first one.
    pub fn knowledge_model_built_at(&self) -> Result<Option<i64>> {
        match self.conn.query_row(
            "SELECT value FROM meta WHERE key = 'knowledge_model_built_at'",
            [],
            |r| r.get::<_, String>(0),
        ) {
            Ok(v) => Ok(v.parse().ok()),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ChatField {
    Title,
    Status,
}

#[derive(Debug, Clone, Copy)]
pub enum ChatFlag {
    Pinned,
    Archived,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatRow {
    pub id: String,
    pub title: String,
    /// `user` | `ingest`
    pub kind: String,
    pub pinned: bool,
    /// `working` | `needs_input` | `done` | `error`
    pub status: String,
    pub created_at: i64,
    pub last_active_at: i64,
    pub archived: bool,
    /// Chosen model as a stable tier alias (`haiku`/`sonnet`/`opus`/`fable`),
    /// or None for the CLI's own default. Applied when a session is spawned.
    pub model: Option<String>,
}

/// One day's digest. `content` is the raw model output — a paragraph
/// plus an optional `SOURCES:` line — parsed at read time.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DigestRow {
    pub id: i64,
    /// Local calendar day, `yyyy-mm-dd`.
    pub date: String,
    pub content: String,
    pub created_at: i64,
}

/// A stored Review item — the substrate for inbox kinds that have no
/// other table of their own (sync conflicts, AI questions, …).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewItemRow {
    pub id: i64,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub source_ref: String,
    /// `open` | `resolved`
    pub status: String,
    /// Free-form JSON for kind-specific data.
    pub payload: Option<String>,
    pub created_at: i64,
    pub resolved_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    pub id: i64,
    pub chat_id: String,
    /// `user` | `assistant` | `activity` | `divider`
    pub role: String,
    pub content: String,
    pub created_at: i64,
}

/// A stored knowledge-model entity.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityRow {
    pub id: i64,
    /// `person` | `organization` | `topic` | `decision` | `other`
    pub kind: String,
    pub name: String,
    pub summary: String,
    /// Project-relative paths this entity is grounded in.
    pub sources: Vec<String>,
}

/// A stored relation between two entities.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgeRow {
    pub id: i64,
    pub a: i64,
    pub b: i64,
    pub label: String,
}

/// A stored knowledge-model event.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventRow {
    pub id: i64,
    /// Best-effort `yyyy-mm-dd`.
    pub date: String,
    pub category: String,
    pub text: String,
    /// Project-relative path the event came from.
    pub source: String,
}

/// An entity as the extractor produced it, edges as indices into the
/// same batch — `replace_knowledge_model` maps them to row ids.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityInput {
    pub kind: String,
    pub name: String,
    pub summary: String,
    pub sources: Vec<String>,
    /// (index of the other entity in this batch, edge label)
    pub connections: Vec<(usize, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EventInput {
    pub date: String,
    pub category: String,
    pub text: String,
    pub source: String,
}

/// Filename tokens for the `name` FTS column: stem + extension with
/// separators spaced out ("Meeting-Notes_Jul8.md" → "Meeting Notes Jul8 md").
fn name_tokens(rel_path: &str) -> String {
    let file_name = rel_path.rsplit('/').next().unwrap_or(rel_path);
    file_name
        .replace(['-', '_', '.', '—'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn like_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

/// Split raw user input into safe query tokens: alphanumeric runs only —
/// FTS5 syntax and LIKE wildcards can't survive this.
fn query_tokens(input: &str) -> Vec<String> {
    input
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect()
}

/// Build an FTS5 query from tokens: each quoted, all ANDed, last token as
/// prefix for as-you-type feel.
fn build_fts_query(tokens: &[String]) -> String {
    let last = tokens.len().saturating_sub(1);
    let parts: Vec<String> = tokens
        .iter()
        .enumerate()
        .map(|(i, t)| {
            if i == last {
                format!("\"{t}\" *")
            } else {
                format!("\"{t}\"")
            }
        })
        .collect();
    parts.join(" ")
}

// Synthetic ranks for filename matches, on the bm25 scale (lower = better).
// Exact and prefix basename matches outrank any realistic content score; a
// plain substring match beats weak content hits (bm25 near zero) but yields
// to strongly relevant content.
const NAME_RANK_EXACT: f64 = -100.0;
const NAME_RANK_PREFIX: f64 = -50.0;
const NAME_RANK_SUBSTRING: f64 = -2.0;

/// Rank a filename match: exact basename (with or without extension) first,
/// then basename prefix, then plain substring-in-path.
fn name_match_rank(rel_path: &str, raw_query: &str) -> f64 {
    let base = rel_path.rsplit('/').next().unwrap_or(rel_path).to_lowercase();
    let stem = base.rsplit_once('.').map(|(s, _)| s.to_string()).unwrap_or_else(|| base.clone());
    let q = raw_query.trim().to_lowercase();
    if base == q || stem == q {
        NAME_RANK_EXACT
    } else if base.starts_with(&q) {
        NAME_RANK_PREFIX
    } else {
        NAME_RANK_SUBSTRING
    }
}

/// Snippet for a filename hit: the basename with each matched token wrapped
/// in `<mark>` — the same shape content snippets have, so the UI renders it
/// unchanged. Tokens that only matched in the directory part stay unmarked.
fn name_snippet(rel_path: &str, lowered_tokens: &[String]) -> String {
    let base = rel_path.rsplit('/').next().unwrap_or(rel_path);
    let lower = base.to_lowercase();
    // Lowercasing can shift byte offsets for exotic Unicode; skip marking
    // rather than risk splitting a char.
    if lower.len() != base.len() {
        return base.to_string();
    }
    let mut ranges: Vec<(usize, usize)> = lowered_tokens
        .iter()
        .filter_map(|t| lower.find(t.as_str()).map(|i| (i, i + t.len())))
        .collect();
    ranges.sort_unstable();
    let mut out = String::new();
    let mut pos = 0;
    for (start, end) in ranges {
        if start < pos {
            continue; // overlaps a previous mark; keep it simple
        }
        let (Some(before), Some(hit)) = (base.get(pos..start), base.get(start..end)) else {
            return base.to_string();
        };
        out.push_str(before);
        out.push_str("<mark>");
        out.push_str(hit);
        out.push_str("</mark>");
        pos = end;
    }
    match base.get(pos..) {
        Some(rest) => out + rest,
        None => base.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> Db {
        let mut db = Db::open_in_memory().unwrap();
        db.upsert_file(
            "notes/meeting-jul-8.md", "md", 100, 1, "indexed", None,
            "Confirmed the billing cutover date slips to Sept 12. Attendees Priya Marcus Dana.",
        )
        .unwrap();
        db.upsert_file(
            "knowledge/People.md", "md", 200, 1, "indexed", None,
            "Priya Natarajan owns billing cutover with Marcus as backup.",
        )
        .unwrap();
        db.upsert_file("vendor/Contract v3.pdf", "pdf", 900, 1, "failed",
            Some("encrypted"), "")
            .unwrap();
        db
    }

    #[test]
    fn search_finds_and_highlights() {
        let db = seeded();
        let hits = db.search("billing cutover", 10).unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits[0].snippet.contains("<mark>"));
    }

    #[test]
    fn search_prefix_as_you_type() {
        let db = seeded();
        let hits = db.search("cutov", 10).unwrap();
        assert!(!hits.is_empty(), "prefix of last token should match");
    }

    #[test]
    fn failed_file_findable_by_name() {
        let db = seeded();
        let hits = db.search("contract", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].rel_path, "vendor/Contract v3.pdf");
        assert_eq!(hits[0].status, "failed");
    }

    #[test]
    fn filename_substring_matches_without_content_hit() {
        let mut db = seeded();
        // "roadmap" is neither a name token (camelCase stays one token) nor
        // in the content — only the substring pass can find this.
        db.upsert_file("plans/Q3RoadmapFinal.docx", "docx", 300, 1, "indexed", None,
            "Milestones for the third quarter.")
            .unwrap();
        let hits = db.search("roadmap", 10).unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].rel_path, "plans/Q3RoadmapFinal.docx");
        // Snippet is the basename with the match marked.
        assert_eq!(hits[0].snippet, "Q3<mark>Roadmap</mark>Final.docx");
    }

    #[test]
    fn filename_match_is_case_insensitive() {
        let mut db = seeded();
        db.upsert_file("plans/Q3RoadmapFinal.docx", "docx", 300, 1, "indexed", None, "")
            .unwrap();
        for q in ["ROADMAP", "q3roadmap", "RoadMapFin"] {
            let hits = db.search(q, 10).unwrap();
            assert!(
                hits.iter().any(|h| h.rel_path == "plans/Q3RoadmapFinal.docx"),
                "query {q:?} should match by filename: {hits:?}"
            );
        }
    }

    #[test]
    fn exact_basename_match_outranks_weak_content_hits() {
        let mut db = seeded();
        db.upsert_file("finance/budget.md", "md", 50, 1, "indexed", None,
            "Numbers for the quarter.")
            .unwrap();
        db.upsert_file("notes/long-doc.md", "md", 5000, 1, "indexed", None,
            "A long meandering note that mentions the budget once in passing.")
            .unwrap();
        let hits = db.search("budget", 10).unwrap();
        assert!(hits.len() >= 2, "{hits:?}");
        assert_eq!(hits[0].rel_path, "finance/budget.md", "{hits:?}");
        assert_eq!(hits[0].rank, NAME_RANK_EXACT);
    }

    #[test]
    fn name_and_content_match_dedupes_to_one_hit() {
        let mut db = seeded();
        db.upsert_file("finance/budget.md", "md", 50, 1, "indexed", None,
            "The budget was approved on Friday.")
            .unwrap();
        let hits = db.search("budget", 10).unwrap();
        let count = hits.iter().filter(|h| h.rel_path == "finance/budget.md").count();
        assert_eq!(count, 1, "{hits:?}");
        // The better (filename) rank wins; the content snippet is kept.
        assert_eq!(hits[0].rank, NAME_RANK_EXACT);
        assert!(hits[0].snippet.contains("<mark>budget</mark>"), "{hits:?}");
    }

    #[test]
    fn multi_token_filename_query_requires_all_tokens() {
        let mut db = seeded();
        db.upsert_file("plans/Q3RoadmapFinal.docx", "docx", 300, 1, "indexed", None, "")
            .unwrap();
        // Both tokens appear in the rel_path (one in the folder, one in the
        // basename) — hit. A token that appears nowhere — no hit.
        assert!(db
            .search("plans roadmap", 10)
            .unwrap()
            .iter()
            .any(|h| h.rel_path == "plans/Q3RoadmapFinal.docx"));
        assert!(db.search("roadmap missing", 10).unwrap().is_empty());
    }

    #[test]
    fn upsert_replaces_content() {
        let mut db = seeded();
        db.upsert_file("notes/meeting-jul-8.md", "md", 120, 2, "indexed", None,
            "Entirely new content about rollback rehearsal.")
            .unwrap();
        assert!(db.search("rollback", 10).unwrap().len() == 1);
        assert!(db.search("attendees", 10).unwrap().is_empty());
        assert_eq!(db.file_count().unwrap(), 3);
    }

    #[test]
    fn indexed_file_count_excludes_non_indexed_rows() {
        // seeded() has 2 indexed rows + 1 failed row.
        let mut db = seeded();
        assert_eq!(db.file_count().unwrap(), 3);
        assert_eq!(db.indexed_file_count().unwrap(), 2);

        // Image-only (metadata_only) and online-only (cloud_only) rows have
        // no readable content and must not count toward the auto-build guard.
        db.upsert_file("img/photo.jpg", "jpg", 1000, 1, "metadata_only", None, "")
            .unwrap();
        db.upsert_file("docs/remote.pdf", "pdf", 2000, 1, "cloud_only", None, "")
            .unwrap();
        assert_eq!(db.file_count().unwrap(), 5, "all rows still present");
        assert_eq!(
            db.indexed_file_count().unwrap(),
            2,
            "only status='indexed' rows count"
        );
    }

    #[test]
    fn remove_file_removes_from_search() {
        let mut db = seeded();
        db.remove_file("knowledge/People.md").unwrap();
        let hits = db.search("Natarajan", 10).unwrap();
        assert!(hits.is_empty());
        assert_eq!(db.file_count().unwrap(), 2);
    }

    #[test]
    fn remove_folder_removes_children_only() {
        let mut db = seeded();
        db.remove_folder("notes").unwrap();
        assert!(db.get_file("notes/meeting-jul-8.md").unwrap().is_none());
        assert!(db.get_file("knowledge/People.md").unwrap().is_some());
    }

    #[test]
    fn clear_empties_everything() {
        let mut db = seeded();
        db.clear().unwrap();
        assert_eq!(db.file_count().unwrap(), 0);
        assert!(db.search("billing", 10).unwrap().is_empty());
    }

    #[test]
    fn hostile_query_is_safe() {
        let db = seeded();
        // FTS5 syntax characters must not panic or error.
        for q in ["\"unbalanced", "a AND OR NOT", "col:x", "(((", "*", "  "] {
            let _ = db.search(q, 10).unwrap();
        }
    }

    #[test]
    fn get_text_returns_indexed_content() {
        let db = seeded();
        let text = db.get_text("knowledge/People.md").unwrap().unwrap();
        assert!(text.contains("Priya Natarajan"));
        // Failed extraction stored empty text; missing file is None.
        assert_eq!(db.get_text("vendor/Contract v3.pdf").unwrap().unwrap(), "");
        assert!(db.get_text("nope.md").unwrap().is_none());
    }

    #[test]
    fn open_read_only_reads_but_never_writes() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let id = Uuid::new_v4();
        {
            let mut db = Db::open(base, id).unwrap();
            db.upsert_file("notes/a.md", "md", 10, 1, "indexed", None, "billing cutover")
                .unwrap();
        }
        let ro = Db::open_read_only(base, id).unwrap();
        assert_eq!(ro.file_count().unwrap(), 1);
        assert_eq!(ro.search("billing", 5).unwrap().len(), 1);
        assert!(ro.get_text("notes/a.md").unwrap().is_some());
        // SQLite itself refuses writes on this handle.
        assert!(ro
            .conn
            .execute("INSERT INTO meta(key, value) VALUES ('x', 'y')", [])
            .is_err());
        // A missing index is an open error, not an empty database.
        assert!(Db::open_read_only(base, Uuid::new_v4()).is_err());
    }

    // The search command opens its read-only handle once at project activation,
    // while the writer keeps indexing. This asserts the structural guarantee the
    // fix relies on: a WAL reader opened BEFORE a write still sees that write
    // once committed, and returns the same hits the writer would. (The freedom
    // from lock contention this buys is structural — it can't be unit-tested.)
    #[test]
    fn read_only_search_sees_writes_committed_after_it_opened() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let id = Uuid::new_v4();
        let mut writer = Db::open(base, id).unwrap();
        writer
            .upsert_file("notes/a.md", "md", 10, 1, "indexed", None, "billing cutover")
            .unwrap();

        // Reader opened while the writer is still live, before the next commit.
        let reader = Db::open_read_only(base, id).unwrap();
        assert_eq!(reader.search("billing", 5).unwrap().len(), 1);

        // A freshly-indexed file must be findable through the same reader.
        writer
            .upsert_file("notes/b.md", "md", 20, 2, "indexed", None, "quarterly forecast")
            .unwrap();
        let via_reader = reader.search("forecast", 5).unwrap();
        let via_writer = writer.search("forecast", 5).unwrap();
        assert_eq!(via_reader.len(), 1);
        let paths = |hits: &[SearchHit]| hits.iter().map(|h| h.rel_path.clone()).collect::<Vec<_>>();
        assert_eq!(paths(&via_reader), paths(&via_writer));
    }

    #[test]
    fn schema_version_recorded() {
        let db = Db::open_in_memory().unwrap();
        let v: String = db
            .conn
            .query_row("SELECT value FROM meta WHERE key='schema_version'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION.to_string());
    }

    #[test]
    fn chat_crud_roundtrip() {
        let mut db = Db::open_in_memory().unwrap();
        let chat = ChatRow {
            id: "sess-1".into(),
            title: "Draft the FAQ".into(),
            kind: "user".into(),
            pinned: false,
            status: "done".into(),
            created_at: 100,
            last_active_at: 100,
            archived: false,
            model: None,
        };
        db.upsert_chat(&chat).unwrap();
        db.upsert_chat(&ChatRow { id: "sess-2".into(), title: "Second".into(), last_active_at: 200, created_at: 200, ..chat.clone() }).unwrap();

        // Pinned floats first even though it's older.
        db.set_chat_flag("sess-1", ChatFlag::Pinned, true).unwrap();
        let list = db.list_chats().unwrap();
        assert_eq!(list[0].id, "sess-1");
        assert!(list[0].pinned);

        db.set_chat_field("sess-1", ChatField::Status, "needs_input").unwrap();
        assert_eq!(db.get_chat("sess-1").unwrap().unwrap().status, "needs_input");

        db.append_chat_message("sess-1", "user", "hello", 101).unwrap();
        db.append_chat_message("sess-1", "assistant", "**hi**", 102).unwrap();
        let msgs = db.chat_messages("sess-1").unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "user");

        db.set_chat_flag("sess-1", ChatFlag::Archived, true).unwrap();
        assert_eq!(db.list_chats().unwrap().len(), 1);
    }

    #[test]
    fn chat_model_defaults_none_and_persists() {
        let mut db = Db::open_in_memory().unwrap();
        let chat = ChatRow {
            id: "m-1".into(),
            title: "Model chat".into(),
            kind: "user".into(),
            pinned: false,
            status: "done".into(),
            created_at: 1,
            last_active_at: 1,
            archived: false,
            model: None,
        };
        db.upsert_chat(&chat).unwrap();
        // A fresh chat carries no model → CLI default.
        assert_eq!(db.get_chat("m-1").unwrap().unwrap().model, None);

        db.set_chat_model("m-1", Some("opus")).unwrap();
        assert_eq!(db.get_chat("m-1").unwrap().unwrap().model.as_deref(), Some("opus"));

        // Clearing returns to the default.
        db.set_chat_model("m-1", None).unwrap();
        assert_eq!(db.get_chat("m-1").unwrap().unwrap().model, None);
    }

    #[test]
    fn v1_db_migrates_to_v2() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.db");
        {
            // Rewind a fresh DB to v1 state.
            let db = Db::open_at(&path).unwrap();
            db.conn.execute("DROP TABLE ingest_runs", []).unwrap();
            db.conn
                .execute("UPDATE meta SET value='1' WHERE key='schema_version'", [])
                .unwrap();
        }
        let mut db = Db::open_at(&path).unwrap();
        // v2 table exists again and is usable.
        let id = db.insert_run("people", Some("s-1"), 100).unwrap();
        assert!(db.get_run(id).unwrap().is_some());
    }

    #[test]
    fn v3_db_migrates_to_v4() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.db");
        {
            // Rewind a fresh DB to v3 state.
            let db = Db::open_at(&path).unwrap();
            db.conn.execute("DROP TABLE review_items", []).unwrap();
            db.conn
                .execute("UPDATE meta SET value='3' WHERE key='schema_version'", [])
                .unwrap();
        }
        let mut db = Db::open_at(&path).unwrap();
        // v4 table exists again and is usable.
        let id = db
            .insert_review_item("conflict", "Merge conflict", "", "Decisions.md", None, 100)
            .unwrap();
        assert_eq!(db.list_open_review_items().unwrap().len(), 1);
        assert_eq!(db.list_open_review_items().unwrap()[0].id, id);
    }

    #[test]
    fn v4_db_migrates_to_v5() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.db");
        {
            // Rewind a fresh DB to v4 state.
            let mut db = Db::open_at(&path).unwrap();
            db.upsert_file("notes/a.md", "md", 10, 1, "indexed", None, "kept").unwrap();
            db.conn.execute("DROP TABLE digests", []).unwrap();
            db.conn
                .execute("UPDATE meta SET value='4' WHERE key='schema_version'", [])
                .unwrap();
        }
        let mut db = Db::open_at(&path).unwrap();
        // v5 table exists again and is usable; earlier data survives.
        db.upsert_digest("2026-07-12", "A quiet day.", 100).unwrap();
        assert!(db.get_digest("2026-07-12").unwrap().is_some());
        assert_eq!(db.file_count().unwrap(), 1);
    }

    #[test]
    fn v5_db_migrates_to_v6() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.db");
        {
            // Rewind a fresh DB to v5 state.
            let mut db = Db::open_at(&path).unwrap();
            db.upsert_file("notes/a.md", "md", 10, 1, "indexed", None, "kept").unwrap();
            db.conn.execute("DROP TABLE entity_edges", []).unwrap();
            db.conn.execute("DROP TABLE entities", []).unwrap();
            db.conn.execute("DROP TABLE events", []).unwrap();
            db.conn
                .execute("UPDATE meta SET value='5' WHERE key='schema_version'", [])
                .unwrap();
        }
        let mut db = Db::open_at(&path).unwrap();
        // v6 tables exist again and are usable; earlier data survives.
        db.replace_knowledge_model(
            &[EntityInput {
                kind: "person".into(),
                name: "Priya N.".into(),
                summary: "Owns billing cutover.".into(),
                sources: vec!["knowledge/People.md".into()],
                connections: vec![],
            }],
            &[EventInput {
                date: "2026-07-11".into(),
                category: "decision".into(),
                text: "Cutover date is firm.".into(),
                source: "notes/standup.md".into(),
            }],
            100,
        )
        .unwrap();
        assert_eq!(db.list_events().unwrap().len(), 1);
        assert_eq!(db.file_count().unwrap(), 1);
    }

    fn sample_model() -> (Vec<EntityInput>, Vec<EventInput>) {
        let entities = vec![
            EntityInput {
                kind: "topic".into(),
                name: "Billing cutover".into(),
                summary: "The migration to the new billing system.".into(),
                sources: vec!["notes/meeting-jul-8.md".into()],
                connections: vec![(1, "owned by".into()), (2, "vendor".into())],
            },
            EntityInput {
                kind: "person".into(),
                name: "Priya N.".into(),
                summary: "Owns the cutover.".into(),
                sources: vec!["knowledge/People.md".into()],
                connections: vec![],
            },
            EntityInput {
                kind: "organization".into(),
                name: "LangdonSoft".into(),
                summary: "Billing vendor.".into(),
                sources: vec![],
                connections: vec![],
            },
        ];
        let events = vec![
            EventInput {
                date: "2026-07-11".into(),
                category: "decision".into(),
                text: "Vendor sign-off confirmed.".into(),
                source: "notes/standup.md".into(),
            },
            EventInput {
                date: "2026-06-26".into(),
                category: "people".into(),
                text: "Retro calls for a public FAQ.".into(),
                source: "notes/retro.md".into(),
            },
        ];
        (entities, events)
    }

    #[test]
    fn replace_knowledge_model_is_atomic_and_idempotent() {
        let mut db = Db::open_in_memory().unwrap();
        // Empty until built.
        let (entities, edges) = db.list_entities_with_edges().unwrap();
        assert!(entities.is_empty() && edges.is_empty());
        assert!(db.list_events().unwrap().is_empty());
        assert!(db.knowledge_model_built_at().unwrap().is_none());

        let (ents, evs) = sample_model();
        db.replace_knowledge_model(&ents, &evs, 100).unwrap();
        db.replace_knowledge_model(&ents, &evs, 200).unwrap();

        // Replacing twice leaves exactly one copy — no duplicates, no
        // orphaned edges — and the newest timestamp.
        let (entities, edges) = db.list_entities_with_edges().unwrap();
        assert_eq!(entities.len(), 3);
        assert_eq!(edges.len(), 2);
        assert_eq!(db.knowledge_model_built_at().unwrap(), Some(200));
        let priya = entities.iter().find(|e| e.name == "Priya N.").unwrap();
        assert_eq!(priya.kind, "person");
        assert_eq!(priya.sources, vec!["knowledge/People.md"]);
        // Edge endpoints reference live entity rows with the right label.
        let owned = edges.iter().find(|e| e.label == "owned by").unwrap();
        let topic = entities.iter().find(|e| e.name == "Billing cutover").unwrap();
        assert_eq!(owned.a, topic.id);
        assert_eq!(owned.b, priya.id);
        let orphans: i64 = db.conn.query_row(
            "SELECT COUNT(*) FROM entity_edges
             WHERE a NOT IN (SELECT id FROM entities)
                OR b NOT IN (SELECT id FROM entities)",
            [], |r| r.get(0),
        ).unwrap();
        assert_eq!(orphans, 0);

        // Events come back newest first.
        let events = db.list_events().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].date, "2026-07-11");
        assert_eq!(events[1].category, "people");
    }

    #[test]
    fn digest_upsert_replaces_same_day() {
        let mut db = Db::open_in_memory().unwrap();
        assert!(db.get_digest("2026-07-12").unwrap().is_none());
        assert!(db.latest_digest().unwrap().is_none());

        db.upsert_digest("2026-07-11", "Yesterday.", 100).unwrap();
        db.upsert_digest("2026-07-12", "First draft.", 200).unwrap();
        db.upsert_digest("2026-07-12", "Second draft.\nSOURCES: a.md", 300)
            .unwrap();

        let today = db.get_digest("2026-07-12").unwrap().unwrap();
        assert_eq!(today.content, "Second draft.\nSOURCES: a.md");
        assert_eq!(today.created_at, 300);
        // Still one row per day.
        let count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM digests WHERE date='2026-07-12'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);

        let latest = db.latest_digest().unwrap().unwrap();
        assert_eq!(latest.date, "2026-07-12");
    }

    #[test]
    fn review_item_lifecycle() {
        let mut db = Db::open_in_memory().unwrap();
        let a = db
            .insert_review_item(
                "question",
                "Same person?",
                "Priya N. and Priya Natarajan look alike.",
                "People.md",
                Some(r#"{"options":["yes","no"]}"#),
                100,
            )
            .unwrap();
        let b = db
            .insert_review_item("conflict", "Merge conflict", "", "Decisions.md", None, 200)
            .unwrap();

        let open = db.list_open_review_items().unwrap();
        assert_eq!(open.len(), 2);
        // Newest first.
        assert_eq!(open[0].id, b);
        assert_eq!(open[1].status, "open");
        assert_eq!(open[1].payload.as_deref(), Some(r#"{"options":["yes","no"]}"#));

        // Payload can be replaced in place (AI draft landing later).
        db.set_review_item_payload(b, r#"{"path":"Decisions.md","draft":"x"}"#)
            .unwrap();
        assert_eq!(
            db.get_review_item(b).unwrap().unwrap().payload.as_deref(),
            Some(r#"{"path":"Decisions.md","draft":"x"}"#)
        );
        assert!(db.get_review_item(9999).unwrap().is_none());

        db.resolve_review_item(a, 300).unwrap();
        assert_eq!(db.list_open_review_items().unwrap().len(), 1);
        let resolved = db.list_recent_resolved_review_items(250).unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].id, a);
        assert_eq!(resolved[0].resolved_at, Some(300));
        // Outside the window → not listed.
        assert!(db.list_recent_resolved_review_items(301).unwrap().is_empty());
        // Resolving again is a no-op that keeps the original timestamp.
        db.resolve_review_item(a, 999).unwrap();
        assert_eq!(
            db.list_recent_resolved_review_items(0).unwrap()[0].resolved_at,
            Some(300)
        );
    }

    #[test]
    fn runs_finished_since_window() {
        let mut db = Db::open_in_memory().unwrap();
        let old = db.insert_run("people", None, 10).unwrap();
        db.update_run(old, "discarded", Some(20), None, None, None).unwrap();
        let recent = db.insert_run("people", None, 100).unwrap();
        db.update_run(recent, "fresh", Some(120), None, None, Some(0.5)).unwrap();
        let unfinished = db.insert_run("people", None, 130).unwrap();

        let rows = db.runs_finished_since(50).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, recent);
        let all = db.runs_finished_since(0).unwrap();
        assert_eq!(all.len(), 2, "running run {unfinished} must be excluded");
        assert_eq!(all[0].id, recent, "newest finish first");
    }

    #[test]
    fn run_log_lifecycle() {
        let mut db = Db::open_in_memory().unwrap();
        let id = db.insert_run("people", Some("sess-1"), 1000).unwrap();
        assert_eq!(db.get_run(id).unwrap().unwrap().status, "running");

        db.update_run(id, "pending_approval", Some(1060), Some("2 additions"), None, Some(0.4))
            .unwrap();
        let row = db.get_run(id).unwrap().unwrap();
        assert_eq!(row.status, "pending_approval");
        assert_eq!(row.change_ratio, Some(0.4));

        assert_eq!(db.runs_with_status("pending_approval").unwrap().len(), 1);
        assert!(db.last_success_at("people").unwrap().is_none());

        db.update_run(id, "fresh", None, None, None, None).unwrap();
        assert_eq!(db.last_success_at("people").unwrap(), Some(1000));
        // COALESCE keeps earlier fields.
        let row = db.get_run(id).unwrap().unwrap();
        assert_eq!(row.summary.as_deref(), Some("2 additions"));
        assert_eq!(row.finished_at, Some(1060));

        let runs = db.list_runs("people", 10).unwrap();
        assert_eq!(runs.len(), 1);
    }
}

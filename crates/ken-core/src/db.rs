//! Per-project SQLite database in app-data: file inventory, extracted text,
//! FTS5 search index. Entirely derived — `rebuild()` from the folder is the
//! universal recovery story — and never stored inside the project folder.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};
use serde::Serialize;
use uuid::Uuid;

use crate::{Error, Result};

pub const SCHEMA_VERSION: i64 = 3;

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
    /// boosted over body matches.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let Some(fts_query) = build_fts_query(query) else {
            return Ok(Vec::new());
        };
        let mut stmt = self.conn.prepare(
            r#"SELECT f.rel_path, f.kind, f.status,
                      snippet(search, 0, '<mark>', '</mark>', '…', 12),
                      bm25(search, 1.0, 4.0)
               FROM search JOIN files f ON f.id = search.rowid
               WHERE search MATCH ?1
               ORDER BY bm25(search, 1.0, 4.0)
               LIMIT ?2"#,
        )?;
        let rows = stmt
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
        Ok(rows)
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

    // --- chats ---

    pub fn upsert_chat(&mut self, chat: &ChatRow) -> Result<()> {
        self.conn.execute(
            r#"INSERT INTO chats (id, title, kind, pinned, status, created_at, last_active_at, archived)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
               ON CONFLICT(id) DO UPDATE SET
                 title = ?2, kind = ?3, pinned = ?4, status = ?5,
                 last_active_at = ?7, archived = ?8"#,
            params![
                chat.id,
                chat.title,
                chat.kind,
                chat.pinned as i64,
                chat.status,
                chat.created_at,
                chat.last_active_at,
                chat.archived as i64
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
        })
    }

    const CHAT_COLS: &'static str =
        "id, title, kind, pinned, status, created_at, last_active_at, archived";

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

/// Build a safe FTS5 query from raw user input: alphanumeric tokens only,
/// each quoted, all ANDed, last token as prefix for as-you-type feel.
fn build_fts_query(input: &str) -> Option<String> {
    let tokens: Vec<String> = input
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect();
    if tokens.is_empty() {
        return None;
    }
    let last = tokens.len() - 1;
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
    Some(parts.join(" "))
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

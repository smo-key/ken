//! Per-project SQLite database in app-data: file inventory, extracted text,
//! FTS5 search index. Entirely derived — `rebuild()` from the folder is the
//! universal recovery story — and never stored inside the project folder.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Once, OnceLock};

use regex::Regex;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use uuid::Uuid;

use crate::knowledge_model;
use crate::search::FtsHit;
use crate::{Error, Result};

pub const SCHEMA_VERSION: i64 = 20;

/// Meta key: the code map was filled from stored text for this index.
const CODE_MAP_BACKFILLED: &str = "code_map_backfilled";

/// Install the statically-linked sqlite-vec (`vec0`) extension into SQLite's
/// process-global auto-extension list exactly once. sqlite-vec is compiled into
/// this binary (no runtime `.dll`/`.so` load); `sqlite3_auto_extension` runs its
/// init hook on every connection opened *after* this call, so it must precede
/// the first `Connection::open`. Idempotent via `Once`; safe to call from every
/// constructor. Whether `vec0` is actually usable on a given connection is a
/// separate question answered by [`load_vec_extension`] (a `vec_version()`
/// probe), so a registration that silently no-ops still degrades gracefully.
fn register_vec_extension() {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        // SAFETY: `sqlite3_vec_init` is the extension's C entry point; it is
        // transmuted to the `sqlite3_auto_extension` callback signature exactly
        // as sqlite-vec's own rusqlite example does. Runs once per process.
        unsafe {
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute(
                sqlite_vec::sqlite3_vec_init as *const (),
            )));
        }
    });
}

/// Probe whether the `vec0` KNN virtual table is available on `conn` by running
/// `vec_version()`, and log the version once per process. Registration is done
/// up front in the constructors ([`register_vec_extension`]); this only reports
/// usability. It never errors — a failed probe returns `false` so the caller
/// degrades to FTS5-only search rather than breaking the whole index. Logging
/// the version once at startup (task 1.9) makes a future extension/on-disk
/// format mismatch visible immediately instead of as a silent KNN failure.
fn load_vec_extension(conn: &Connection) -> bool {
    match conn.query_row("SELECT vec_version()", [], |r| r.get::<_, String>(0)) {
        Ok(version) => {
            static LOGGED: Once = Once::new();
            LOGGED.call_once(|| {
                eprintln!("[ken-core] sqlite-vec {version} registered (vec0 KNN available)");
            });
            true
        }
        Err(_) => false,
    }
}

/// How many times an errored extraction is automatically re-queued before it is
/// left `error` for good. Bounds retries so a persistently/deterministically
/// failing file self-heals from a *transient* fault (bad model load, momentary
/// unparseable JSON) but can never wedge the queue in an infinite retry loop.
pub const MAX_EXTRACTION_ATTEMPTS: i64 = 3;

/// Bounded retries for the OCR queue, mirroring [`MAX_EXTRACTION_ATTEMPTS`]. A
/// scanned page that deterministically fails Vision (unrasterizable, corrupt)
/// self-heals from a transient fault but never wedges the queue.
pub const MAX_OCR_ATTEMPTS: i64 = 3;

pub struct Db {
    conn: Connection,
    /// Whether the sqlite-vec (`vec0`) extension is usable on this connection,
    /// cached from a `vec_version()` probe at open time (see
    /// [`load_vec_extension`]). Drives graceful degradation: when `false`, KNN
    /// paths (`ensure_vec_chunks`, semantic search) no-op and callers fall back
    /// to FTS5-only search instead of erroring.
    vec_available: bool,
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

/// One stored OCR text region for a file: which page it came from, the
/// recognized line, and its normalized **top-left origin** bounding box
/// `[x, y, w, h]` (each in `0.0..=1.0`). Mirrors [`crate::ocr::OcrRegion`] but
/// keeps the DB layer free of any dependency on the native OCR module, so it is
/// unit-testable without Vision. Consumed by the `get_ocr_regions` command that
/// feeds the Phase 3 Cmd+F highlight overlay.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrRegionRow {
    pub page: i64,
    pub text: String,
    /// `[x, y, w, h]`, normalized, top-left origin.
    pub bbox: [f32; 4],
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

/// File-level index counts for one project (see [`Db::file_health_counts`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FileHealthCounts {
    pub total: i64,
    pub failed: i64,
    /// Waiting to be indexed.
    pub queued: i64,
}

/// Extraction health for one project (see [`Db::index_health`]).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexHealth {
    /// Read for entities, of `extractable`.
    pub analyzed: i64,
    pub extractable: i64,
    /// Queued, not read yet.
    pub pending: i64,
    /// Errored, with attempts left.
    pub retrying: i64,
    /// Errored three times: will not be tried again until the file changes.
    pub failed: i64,
    /// Searchable only (a code or reference repo, a `~` line): never read
    /// for entities, on purpose.
    pub skipped: i64,
    /// The last rebuild's control query, if one has run.
    pub control: Option<ControlResult>,
}

/// One control query: a known page searched by its title, which must come
/// back first. When it does not, the index answers but cannot be trusted.
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlResult {
    pub page: String,
    pub query: String,
    /// What ranked first instead, when it was not `page`.
    pub top: Option<String>,
    pub ok: bool,
    /// Unix seconds.
    pub at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRow {
    pub id: i64,
    pub slug: String,
    /// `ingest` | `automation` — which subsystem produced this run.
    pub kind: String,
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
        register_vec_extension();
        let conn = Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let vec_available = load_vec_extension(&conn);
        Ok(Db {
            conn,
            vec_available,
        })
    }

    pub fn open_at(path: &Path) -> Result<Db> {
        register_vec_extension();
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // The index is derived from the files and a rescan rebuilds it, so a
        // commit need not wait for the disk: in WAL mode NORMAL never
        // corrupts the database, and a power cut loses at most the last
        // commits, which the next scan redoes. FULL waited on every one of a
        // scan's per-file commits, which made a first scan disk-bound.
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // Multiple writers share this file on separate connections (the state
        // mutex's Db, the scanner's, the extraction worker's). WAL allows
        // concurrent readers but still one writer at a time — without a busy
        // handler a second writer gets SQLITE_BUSY *immediately*: the worker
        // would waste a whole re-generation on retry, and a scanner write could
        // simply be lost. Waiting briefly instead makes contention invisible.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let vec_available = load_vec_extension(&conn);
        let db = Db {
            conn,
            vec_available,
        };
        db.migrate()?;
        Ok(db)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Db> {
        register_vec_extension();
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let vec_available = load_vec_extension(&conn);
        let db = Db {
            conn,
            vec_available,
        };
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
        if version < 8 {
            // Per-file extraction bookkeeping for the incremental Map. One row
            // per indexed file: the hash we last extracted, when, and the
            // outcome. Enqueue = a 'pending' row; the worker drains oldest-first.
            self.conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS extractions (
                    rel_path     TEXT PRIMARY KEY,
                    content_hash TEXT NOT NULL,
                    extracted_at INTEGER,
                    status       TEXT NOT NULL,
                    error        TEXT
                );
                CREATE INDEX IF NOT EXISTS extractions_status
                    ON extractions(status);
                "#,
            )?;
        }
        if version < 9 {
            // Run-kind discriminator: automation runs share ingest_runs with
            // recipe runs. Guarded ADD COLUMN (no IF NOT EXISTS; a fresh DB
            // already has the column, and tests rewind schema_version).
            let has_kind: bool = self
                .conn
                .prepare("SELECT 1 FROM pragma_table_info('ingest_runs') WHERE name = 'kind'")?
                .exists([])?;
            if !has_kind {
                self.conn.execute_batch(
                    "ALTER TABLE ingest_runs ADD COLUMN kind TEXT NOT NULL DEFAULT 'ingest';",
                )?;
            }
        }
        if version < 10 {
            // Bounded-retry bookkeeping for the extraction queue: how many times
            // this file's extraction has errored. Guarded ADD COLUMN (no IF NOT
            // EXISTS; a fresh DB already created the column via this same block,
            // and tests rewind schema_version).
            let has_attempts: bool = self
                .conn
                .prepare("SELECT 1 FROM pragma_table_info('extractions') WHERE name = 'attempts'")?
                .exists([])?;
            if !has_attempts {
                self.conn.execute_batch(
                    "ALTER TABLE extractions ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0;",
                )?;
            }
        }
        if version < 11 {
            // OCR (Phase 2): per-region text boxes for image/scanned-PDF files,
            // plus an async work queue mirroring `extractions`. `ocr_regions`
            // holds one row per recognized line (normalized top-left bbox) so a
            // later Cmd+F overlay can highlight in place; `ocr_pending` drives a
            // background worker that reads bytes, runs Vision, and merges the
            // recognized text into `contents` (→ FTS) so images/scans become
            // searchable. Kept entirely separate from the knowledge-model
            // extraction queue and its coverage accounting.
            self.conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS ocr_regions (
                    rel_path TEXT NOT NULL,
                    page     INTEGER NOT NULL,
                    text     TEXT NOT NULL,
                    x        REAL NOT NULL,
                    y        REAL NOT NULL,
                    w        REAL NOT NULL,
                    h        REAL NOT NULL
                );
                CREATE INDEX IF NOT EXISTS ocr_regions_rel
                    ON ocr_regions(rel_path);
                CREATE TABLE IF NOT EXISTS ocr_pending (
                    rel_path     TEXT PRIMARY KEY,
                    content_hash TEXT NOT NULL,
                    status       TEXT NOT NULL,
                    error        TEXT,
                    attempts     INTEGER NOT NULL DEFAULT 0
                );
                CREATE INDEX IF NOT EXISTS ocr_pending_status
                    ON ocr_pending(status);
                "#,
            )?;
        }
        if version < 12 {
            // semantic-index (Phase 0): relational chunk store feeding hybrid
            // (FTS5 keyword + vec0 KNN) search. `chunks` holds one row per
            // embeddable text window produced by the chunker; `token_est` is the
            // chars/4 estimate and `content_hash` (xxHash) drives incremental
            // re-embedding. `tier` mirrors `kenignore::Tier` (0=Full, 1=SearchOnly;
            // `Ignore`-tier files never produce chunk rows at all — see
            // kenignore.rs and design D3) and is set by the caller's classify
            // pass in `upsert_chunks`, not computed here.
            //
            // The companion `vec_chunks` vec0 virtual table is deliberately NOT
            // created here: its schema must bake in the embedding dimension
            // (unknown until an embedder is chosen) and the sqlite-vec extension
            // may be unavailable. It is built lazily by `ensure_vec_chunks(dim)`,
            // so this migration always succeeds even when vec init fails.
            self.conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS chunks (
                    id           INTEGER PRIMARY KEY,
                    path         TEXT NOT NULL,
                    seq          INTEGER NOT NULL,
                    text         TEXT NOT NULL,
                    token_est    INTEGER NOT NULL,
                    content_hash TEXT NOT NULL,
                    tier         INTEGER NOT NULL DEFAULT 0,
                    UNIQUE(path, seq)
                );
                CREATE INDEX IF NOT EXISTS chunks_path ON chunks(path);
                "#,
            )?;
            // Chunk-level keyword index (task 1.6/1.7). This is deliberately a
            // *standalone* FTS5 table — not `content='contents'` external-content
            // like `search` above — for two reasons: (1) `search` is file-grain
            // and this is chunk-grain, and (2) task 1.10 (Condition A) feeds this
            // table augmented text (path tokens + filename stem + symbol names
            // prepended) that differs from the pristine `chunks.text` kept for
            // embeddings, so it can't just mirror `chunks` via triggers the way
            // `search` mirrors `contents`. Rows are written explicitly by
            // `upsert_chunks`/`delete_chunks` with `rowid` pinned to `chunks.id`
            // so a chunk-id join is a plain rowid lookup. No `content=` option is
            // set, so FTS5 stores the text itself and plain INSERT/DELETE by
            // rowid work like a normal table (no external-content 'delete'
            // command dance needed).
            self.conn.execute_batch(
                r#"
                CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(text);
                "#,
            )?;
        }
        if version < 13 {
            // ken-home-workspace: a chat remembers the scope it was opened
            // with, so "all projects" survives a restart and switching the
            // Home picker later cannot silently re-scope an existing
            // conversation.
            //
            // NULL means "this project only" — the pre-feature behavior and
            // the value every existing row gets, so upgrading changes no
            // chat's meaning. `"all"` is every workspace member; any other
            // value names a group. Guarded like `model` above: tests rewind
            // schema_version on a DB that already has the column.
            let has_scope: bool = self
                .conn
                .prepare("SELECT 1 FROM pragma_table_info('chats') WHERE name = 'scope'")?
                .exists([])?;
            if !has_scope {
                self.conn
                    .execute_batch("ALTER TABLE chats ADD COLUMN scope TEXT;")?;
            }
        }
        if version < 14 {
            // knowledge-layer: each file's kenignore tier (0 full, 1
            // search-only), kept by the scan so a change of a repo's kind or
            // a `~` line re-tiers files already indexed, and so extraction
            // coverage counts only files that are read for entities. 0 for
            // every existing row: the next scan corrects it.
            let has_tier: bool = self
                .conn
                .prepare("SELECT 1 FROM pragma_table_info('files') WHERE name = 'tier'")?
                .exists([])?;
            if !has_tier {
                self.conn.execute_batch(
                    "ALTER TABLE files ADD COLUMN tier INTEGER NOT NULL DEFAULT 0;",
                )?;
            }
            // And a hash of the file's bytes, so a rescan that sees only a
            // new modified time (a sync client touching the file) can tell
            // the content did not change without parsing it again. NULL
            // until the file is next read.
            let has_hash: bool = self
                .conn
                .prepare("SELECT 1 FROM pragma_table_info('files') WHERE name = 'byte_hash'")?
                .exists([])?;
            if !has_hash {
                self.conn.execute_batch("ALTER TABLE files ADD COLUMN byte_hash TEXT;")?;
            }
            // And the 1-based line each chunk starts on, for `repo:path:line`
            // citations. NULL until the file is next chunked.
            let has_line: bool = self
                .conn
                .prepare("SELECT 1 FROM pragma_table_info('chunks') WHERE name = 'line'")?
                .exists([])?;
            if !has_line {
                self.conn.execute_batch("ALTER TABLE chunks ADD COLUMN line INTEGER;")?;
            }
            // And what each Markdown page says about itself in its
            // frontmatter (`pagemeta`), read at index time, so a hit can carry
            // its verified date and rank by whether it is binding, evidence,
            // retired or generated. The lists are JSON arrays.
            self.conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS drift_runs (
                    id        INTEGER PRIMARY KEY,
                    at        INTEGER NOT NULL,
                    exit_code INTEGER NOT NULL,
                    pages     INTEGER NOT NULL,
                    log       TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS page_links (
                    from_path TEXT NOT NULL,
                    kind      TEXT NOT NULL,
                    target    TEXT NOT NULL,
                    PRIMARY KEY (from_path, kind, target)
                );
                CREATE TABLE IF NOT EXISTS page_meta (
                    rel_path    TEXT PRIMARY KEY,
                    title       TEXT,
                    aliases     TEXT NOT NULL DEFAULT '[]',
                    status      TEXT,
                    verified    TEXT,
                    updated     TEXT,
                    sources     TEXT NOT NULL DEFAULT '[]',
                    replaced_by TEXT NOT NULL DEFAULT '[]',
                    generated   INTEGER NOT NULL DEFAULT 0,
                    audience    TEXT
                );
                "#,
            )?;
            // `audience` (item 2b) came after the table did; a DB made by an
            // earlier build of this branch gets it here.
            let has_audience: bool = self
                .conn
                .prepare("SELECT 1 FROM pragma_table_info('page_meta') WHERE name = 'audience'")?
                .exists([])?;
            if !has_audience {
                self.conn.execute_batch("ALTER TABLE page_meta ADD COLUMN audience TEXT;")?;
            }
        }
        if version < 15 {
            // The code map (`codemap`): each code file's definitions and
            // references, from tree-sitter's tags queries, and what it
            // imports. Filled as files are indexed, and from stored text for
            // an index made before (`backfill_code_map`).
            self.conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS code_symbols (
                    path     TEXT NOT NULL,
                    name     TEXT NOT NULL,
                    kind     TEXT NOT NULL,
                    line     INTEGER NOT NULL,
                    end_line INTEGER NOT NULL,
                    is_def   INTEGER NOT NULL,
                    docs     TEXT
                );
                CREATE INDEX IF NOT EXISTS code_symbols_name ON code_symbols(name);
                CREATE INDEX IF NOT EXISTS code_symbols_path ON code_symbols(path);
                CREATE TABLE IF NOT EXISTS code_imports (
                    path   TEXT NOT NULL,
                    target TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS code_imports_path ON code_imports(path);
                "#,
            )?;
        }
        if version < 16 {
            // index-words (2026-10-06): the keyword index gains a `names`
            // column (`fts_names`), so it is made again and filled from the
            // stored chunks here, with no file read. Files whose chunks or
            // code map changed with it are read again by the next scan, once:
            // JSON (each chunk now starts with its key outline) and the
            // languages whose code map gained kinds (Java enums, fields and
            // records; TypeScript enums and type aliases). Clearing the byte
            // hash makes the scan read them rather than see an unchanged file.
            self.rebuild_chunks_fts()?;
            self.conn.execute(
                r#"UPDATE files SET mtime = ?1, byte_hash = NULL
                   WHERE status = 'indexed'
                     AND (lower(rel_path) LIKE '%.json' OR lower(rel_path) LIKE '%.java'
                          OR lower(rel_path) LIKE '%.ts' OR lower(rel_path) LIKE '%.tsx'
                          OR lower(rel_path) LIKE '%.mts' OR lower(rel_path) LIKE '%.cts')"#,
                params![Self::REINDEX_SENTINEL_MTIME],
            )?;
        }
        if version < 17 {
            // search-regression (2026-10-06): a prose paragraph or table
            // longer than a chunk is cut at its line ends, and a file too
            // long for the chunk cap gets bigger chunks (`chunker`). Prose
            // files are read again by the next scan, once, to be chunked
            // that way; a chunk whose text is unchanged keeps its vector.
            self.conn.execute(
                r#"UPDATE files SET mtime = ?1, byte_hash = NULL
                   WHERE status = 'indexed'
                     AND (lower(rel_path) LIKE '%.md' OR lower(rel_path) LIKE '%.mdx'
                          OR lower(rel_path) LIKE '%.txt' OR lower(rel_path) LIKE '%.pdf')"#,
                params![Self::REINDEX_SENTINEL_MTIME],
            )?;
        }
        if version < 18 {
            // kb-ranking (2026-10-07): a log-shaped page (a decisions log) is
            // chunked one entry per chunk, past the chunk cap, and each entry
            // chunk is named by its id and title. Which pages are logs is read
            // from their stored text; only those are read again by the next
            // scan, and an entry whose text is unchanged keeps its vector.
            self.mark_log_pages_for_reindex()?;
        }
        if version < 19 {
            // kb-ranking (2026-10-07): each file's authority in search
            // (`authority`), the tiers that are not neutral. Filled here with
            // the default tiers from the stored frontmatter, no file read;
            // the next scan applies the team file's overrides.
            self.conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS file_authority (
                    rel_path TEXT PRIMARY KEY,
                    tier     TEXT NOT NULL,
                    weight   REAL NOT NULL
                );
                "#,
            )?;
            crate::authority::refresh_with(self, &crate::authority::Roles::default(), None)?;
        }
        if version < 20 {
            // kb-trust (2026-10-07): which later entry of a decisions log
            // supersedes or refines which earlier one, and on what point
            // (`supersede`). Written by the pass, never into the log itself.
            self.conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS supersessions (
                    log_path     TEXT NOT NULL,
                    earlier      TEXT NOT NULL,
                    later        TEXT NOT NULL,
                    relation     TEXT NOT NULL,
                    point        TEXT NOT NULL,
                    evidence     TEXT NOT NULL,
                    earlier_line INTEGER NOT NULL,
                    later_line   INTEGER NOT NULL,
                    source       TEXT NOT NULL,
                    made_at      INTEGER NOT NULL,
                    PRIMARY KEY (log_path, earlier, later)
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

    /// Set the Markdown pages whose stored text is a log (`chunker::is_log`)
    /// to be read again by the next scan. Returns how many.
    fn mark_log_pages_for_reindex(&self) -> Result<usize> {
        let logs: Vec<String> = {
            let mut stmt = self.conn.prepare(
                "SELECT f.rel_path, c.text FROM files f JOIN contents c ON c.file_id = f.id
                  WHERE f.kind = 'md' AND f.status = 'indexed'",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            let mut logs = Vec::new();
            for row in rows {
                let (rel, text) = row?;
                if crate::chunker::is_log(&rel, &text) {
                    logs.push(rel);
                }
            }
            logs
        };
        for rel in &logs {
            self.conn.execute(
                "UPDATE files SET mtime = ?1, byte_hash = NULL WHERE rel_path = ?2",
                params![Self::REINDEX_SENTINEL_MTIME, rel],
            )?;
        }
        Ok(logs.len())
    }

    /// Make the chunk keyword index (`chunks_fts`) again with today's columns
    /// and fill it from the stored chunks, in one transaction.
    fn rebuild_chunks_fts(&self) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute_batch(
            r#"
            DROP TABLE IF EXISTS chunks_fts;
            CREATE VIRTUAL TABLE chunks_fts USING fts5(text, names);
            "#,
        )?;
        {
            let mut read = tx.prepare("SELECT id, path, text FROM chunks")?;
            let mut write = tx.prepare("INSERT INTO chunks_fts(rowid, text, names) VALUES (?1, ?2, ?3)")?;
            let mut rows = read.query([])?;
            while let Some(row) = rows.next()? {
                let (id, path, text): (i64, String, String) = (row.get(0)?, row.get(1)?, row.get(2)?);
                write.execute(params![id, text, fts_names(&path, &text)])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Whether the sqlite-vec (`vec0`) KNN extension is usable on this handle.
    /// Callers gate semantic (vector) search on this and fall back to FTS5-only
    /// when it is `false`.
    pub fn vec_available(&self) -> bool {
        self.vec_available
    }

    /// Lazily create the `vec_chunks` KNN virtual table for `dim`-dimensional
    /// embeddings. The `vec0` table bakes the embedding dimension into its
    /// schema, which isn't known until an embedder is chosen, so it is created
    /// on demand rather than in the fixed migration. Idempotent — `IF NOT
    /// EXISTS` makes repeat calls cheap no-ops. Returns `Ok(false)` without
    /// touching the DB when the extension is unavailable, so callers degrade to
    /// FTS5-only rather than erroring.
    pub fn ensure_vec_chunks(&self, dim: usize) -> Result<bool> {
        if !self.vec_available {
            return Ok(false);
        }
        self.conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE IF NOT EXISTS vec_chunks USING vec0(\
                 chunk_id INTEGER PRIMARY KEY, \
                 embedding FLOAT[{dim}]\
             );"
        ))?;
        Ok(true)
    }

    fn meta_get(&self, key: &str) -> Result<Option<String>> {
        let value = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                params![key],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        Ok(value)
    }

    /// The vocabulary a scan built (`vocab::Vocabulary::rebuild`), as JSON.
    pub fn stored_vocabulary(&self) -> Result<Option<String>> {
        self.meta_get("vocabulary")
    }

    pub fn store_vocabulary(&self, json: &str) -> Result<()> {
        self.meta_set("vocabulary", json)
    }

    /// The drift sweep's results carried between runs (see `drift`).
    pub fn drift_cache(&self) -> Result<Option<String>> {
        self.meta_get("drift_cache")
    }

    pub fn store_drift_cache(&self, json: &str) -> Result<()> {
        self.meta_set("drift_cache", json)
    }

    /// The last wiki draft's result (first draft or a repo added), as JSON
    /// (see `wikidraft::Drafted`).
    pub fn wiki_draft(&self) -> Result<Option<String>> {
        self.meta_get("wiki_draft")
    }

    pub fn store_wiki_draft(&self, json: &str) -> Result<()> {
        self.meta_set("wiki_draft", json)
    }

    /// Repos that left the team while the wiki still cited them, as JSON
    /// (see `wikidraft::removed_repos`).
    pub fn removed_repos(&self) -> Result<Option<String>> {
        self.meta_get("removed_repos")
    }

    pub fn store_removed_repos(&self, json: &str) -> Result<()> {
        self.meta_set("removed_repos", json)
    }

    /// Start a batch: the writes that follow, until [`Db::commit_batch`],
    /// land as one transaction. Each write method's own savepoint nests
    /// inside it, so a scan pays one commit per batch, not several per file.
    pub fn begin_batch(&self) -> Result<()> {
        self.conn.execute_batch("BEGIN")?;
        Ok(())
    }

    pub fn commit_batch(&self) -> Result<()> {
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }

    /// Drop a batch after an error; a no-op when none is open.
    pub fn rollback_batch(&self) {
        if !self.conn.is_autocommit() {
            let _ = self.conn.execute_batch("ROLLBACK");
        }
    }

    fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    /// The embedding model id the semantic index was built with (`meta`
    /// `embed_model`), or `None` before the first build. Paired with
    /// [`Db::embed_dim`] so a later model/dimension change can be detected and
    /// force a rebuild.
    pub fn embed_model(&self) -> Result<Option<String>> {
        self.meta_get("embed_model")
    }

    pub fn set_embed_model(&self, model_id: &str) -> Result<()> {
        self.meta_set("embed_model", model_id)
    }

    /// The embedding dimension the semantic index was built with (`meta`
    /// `embed_dim`), or `None` before the first build.
    pub fn embed_dim(&self) -> Result<Option<usize>> {
        Ok(self.meta_get("embed_dim")?.and_then(|v| v.parse().ok()))
    }

    pub fn set_embed_dim(&self, dim: usize) -> Result<()> {
        self.meta_set("embed_dim", &dim.to_string())
    }

    /// Unix epoch (seconds) the semantic index last finished building (`meta`
    /// `semantic_built_at`), or `None` if never built.
    pub fn semantic_built_at(&self) -> Result<Option<i64>> {
        Ok(self
            .meta_get("semantic_built_at")?
            .and_then(|v| v.parse().ok()))
    }

    pub fn set_semantic_built_at(&self, epoch_secs: i64) -> Result<()> {
        self.meta_set("semantic_built_at", &epoch_secs.to_string())
    }

    /// Diff-based upsert of `path`'s chunks against the `chunks` table (and
    /// the parallel `chunks_fts` keyword index): each incoming chunk is
    /// compared against the existing row at the same `seq` by
    /// `content_hash`. Unchanged chunks are left untouched entirely
    /// (including their `vec_chunks` embedding, if any); new or
    /// text-changed chunks are written and their stale embedding (if any)
    /// is dropped so `semantic_search` can't return an outdated vector
    /// before the background backfill re-embeds them; seqs that existed
    /// before but aren't present in `chunks` anymore (file shrank or was
    /// re-chunked) are deleted from all three tables.
    ///
    /// Returns the `(chunk_id, content_hash)` pairs that were newly
    /// inserted or changed — the caller's embedding backfill only needs to
    /// process these, not the whole file.
    pub fn upsert_chunks(
        &mut self,
        path: &str,
        chunks: &[crate::chunker::Chunk],
        tier: crate::kenignore::Tier,
    ) -> Result<Vec<(i64, String)>> {
        let tx = self.conn.savepoint()?;
        let vec_exists = table_exists(&tx, "vec_chunks")?;
        let tier = tier as i64;

        let mut existing: std::collections::HashMap<i64, (i64, String)> = {
            let mut stmt = tx.prepare("SELECT seq, id, content_hash FROM chunks WHERE path = ?1")?;
            let rows = stmt.query_map(params![path], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    (r.get::<_, i64>(1)?, r.get::<_, String>(2)?),
                ))
            })?;
            rows.collect::<std::result::Result<_, _>>()?
        };

        let mut changed = Vec::new();
        for chunk in chunks {
            let seq = chunk.seq as i64;
            let unchanged = matches!(existing.get(&seq), Some((_, hash)) if hash == &chunk.content_hash);
            if unchanged {
                // Same text, but lines added above it move where it starts.
                tx.execute(
                    "UPDATE chunks SET line = ?3 WHERE path = ?1 AND seq = ?2",
                    params![path, seq, chunk.line as i64],
                )?;
            } else {
                tx.execute(
                    r#"INSERT INTO chunks (path, seq, text, token_est, content_hash, tier, line)
                       VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                       ON CONFLICT(path, seq) DO UPDATE SET
                         text = ?3, token_est = ?4, content_hash = ?5, tier = ?6, line = ?7"#,
                    params![path, seq, chunk.text, chunk.token_est as i64, chunk.content_hash, tier, chunk.line as i64],
                )?;
                let id: i64 = tx.query_row(
                    "SELECT id FROM chunks WHERE path = ?1 AND seq = ?2",
                    params![path, seq],
                    |r| r.get(0),
                )?;
                // chunks_fts row: delete-then-reinsert rather than UPDATE since
                // FTS5 doesn't support partial-column UPDATE semantics the way
                // a normal table does. The `names` column holds the path,
                // file name and symbol names and their parts (`fts_names`);
                // `text` is the chunk's own text, as embedded.
                tx.execute("DELETE FROM chunks_fts WHERE rowid = ?1", params![id])?;
                tx.execute(
                    "INSERT INTO chunks_fts(rowid, text, names) VALUES (?1, ?2, ?3)",
                    params![id, chunk.text, fts_names(path, &chunk.text)],
                )?;
                if vec_exists {
                    tx.execute("DELETE FROM vec_chunks WHERE chunk_id = ?1", params![id])?;
                }
                changed.push((id, chunk.content_hash.clone()));
            }
            existing.remove(&seq);
        }

        // Anything left in `existing` is a seq the new chunk set no longer
        // produces — the file shrank or was re-chunked differently.
        for (id, _hash) in existing.into_values() {
            tx.execute("DELETE FROM chunks WHERE id = ?1", params![id])?;
            tx.execute("DELETE FROM chunks_fts WHERE rowid = ?1", params![id])?;
            if vec_exists {
                tx.execute("DELETE FROM vec_chunks WHERE chunk_id = ?1", params![id])?;
            }
        }

        tx.commit()?;
        Ok(changed)
    }

    /// Remove every chunk (and its `chunks_fts`/`vec_chunks` rows) for
    /// `path`. Mirrors [`Db::remove_file`]'s multi-table cleanup breadth;
    /// called when a file is removed or re-classified to the `Ignore` tier.
    pub fn delete_chunks(&mut self, path: &str) -> Result<()> {
        let tx = self.conn.savepoint()?;
        let ids: Vec<i64> = {
            let mut stmt = tx.prepare("SELECT id FROM chunks WHERE path = ?1")?;
            let rows = stmt.query_map(params![path], |r| r.get(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let vec_exists = table_exists(&tx, "vec_chunks")?;
        for id in &ids {
            tx.execute("DELETE FROM chunks_fts WHERE rowid = ?1", params![id])?;
            if vec_exists {
                tx.execute("DELETE FROM vec_chunks WHERE chunk_id = ?1", params![id])?;
            }
        }
        tx.execute("DELETE FROM chunks WHERE path = ?1", params![path])?;
        tx.commit()?;
        Ok(())
    }

    /// Write embedding vectors for already-persisted chunk ids into
    /// `vec_chunks`, replacing any prior vector for the same id
    /// (delete-then-insert, since vec0 doesn't support `ON CONFLICT`
    /// upserts). Callers must have already sized the table via
    /// `ensure_vec_chunks(dim)` — this never creates it, since the
    /// embedding dimension is baked into the table's schema at creation
    /// time and this function has no way to know the caller's chosen `dim`
    /// on its own. No-ops when `vec_available()` is false, `vec_chunks`
    /// doesn't exist yet, or the two slices are mismatched/empty, so
    /// callers can invoke this unconditionally and rely on FTS-only
    /// degradation rather than checking availability themselves first.
    pub fn store_embeddings(&mut self, ids: &[i64], vecs: &[Vec<f32>]) -> Result<()> {
        if !self.vec_available || ids.len() != vecs.len() || ids.is_empty() {
            return Ok(());
        }
        let tx = self.conn.savepoint()?;
        if !table_exists(&tx, "vec_chunks")? {
            return Ok(());
        }
        for (id, vec) in ids.iter().zip(vecs.iter()) {
            let blob = f32_slice_to_blob(vec);
            tx.execute("DELETE FROM vec_chunks WHERE chunk_id = ?1", params![id])?;
            tx.execute(
                "INSERT INTO vec_chunks(chunk_id, embedding) VALUES (?1, ?2)",
                params![id, blob],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// KNN vector search over `vec_chunks`, joined back to `chunks` for the
    /// owning path/text. Returns up to `k` `(chunk_id, path, text,
    /// distance)` tuples ordered nearest-first. Returns an empty `Vec`
    /// (never errors) when `vec_available()` is false, `vec_chunks` doesn't
    /// exist yet, or `k` is 0, so callers can treat this as "no semantic
    /// hits" and fall back to FTS-only results rather than branching on
    /// availability themselves.
    pub fn semantic_search(
        &self,
        query_vec: &[f32],
        k: usize,
    ) -> Result<Vec<(i64, String, String, f64)>> {
        if !self.vec_available || k == 0 {
            return Ok(Vec::new());
        }
        if !table_exists(&self.conn, "vec_chunks")? {
            return Ok(Vec::new());
        }
        let blob = f32_slice_to_blob(query_vec);
        let mut stmt = self.conn.prepare(
            r#"SELECT c.id, c.path, c.text, v.distance
               FROM vec_chunks v
               JOIN chunks c ON c.id = v.chunk_id
               WHERE v.embedding MATCH ?1 AND v.k = ?2
               ORDER BY v.distance"#,
        )?;
        let rows = stmt.query_map(params![blob, k as i64], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, f64>(3)?,
            ))
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Distance from `query_vec` to each listed chunk's stored vector, for
    /// the chunks that have one: the meaning score of a keyword hit the KNN
    /// list did not return. Same L2 distance `semantic_search` reports.
    pub fn chunk_distances(&self, query_vec: &[f32], chunk_ids: &[i64]) -> Result<std::collections::HashMap<i64, f64>> {
        let mut out = std::collections::HashMap::new();
        if !self.vec_available || chunk_ids.is_empty() || !table_exists(&self.conn, "vec_chunks")? {
            return Ok(out);
        }
        let blob = f32_slice_to_blob(query_vec);
        let mut stmt = self
            .conn
            .prepare_cached("SELECT vec_distance_l2(embedding, ?1) FROM vec_chunks WHERE chunk_id = ?2")?;
        for id in chunk_ids {
            if let Some(d) = stmt.query_row(params![blob, id], |r| r.get::<_, f64>(0)).optional()? {
                out.insert(*id, d);
            }
        }
        Ok(out)
    }

    /// Chunk-level FTS5 search over `chunks_fts`, joined back to `chunks`
    /// for the owning path/text — the chunk-grain counterpart to
    /// `semantic_search`, added for task 1.8 so hybrid search (this crate's
    /// tests today; the src-tauri `hybrid_search` command, task 2.2,
    /// eventually) has a ready-made `Vec<FtsHit>` to feed into
    /// `search::merge_hits` alongside `semantic_search`'s KNN hits. Reuses
    /// the same tokenize/stopword/prefix-match query building as the
    /// file-grain `search()` above. Returns up to `k` hits, each the best
    /// chunk of a different file, ordered best-first (by BM25 rank); returns
    /// an empty `Vec` (never errors) for an empty/stopword-only query,
    /// `k == 0`, or a pre-v12 DB where `chunks_fts` doesn't exist yet.
    pub fn search_chunks_fts(&self, query: &str, k: usize) -> Result<Vec<FtsHit>> {
        if k == 0 {
            return Ok(Vec::new());
        }
        let tokens = query_tokens(query);
        if tokens.is_empty() {
            return Ok(Vec::new());
        }
        let tokens = significant_token_list(&tokens);
        if tokens.is_empty() {
            return Ok(Vec::new());
        }
        if !table_exists(&self.conn, "chunks_fts")? {
            return Ok(Vec::new());
        }
        let fts_query = build_chunk_query(&tokens, false);
        // Rank inside the full-text table first and join only the top ones:
        // joined first, SQLite reads every match's section text before it
        // sorts, which for a common word at tens of thousands of files cost
        // most of a second. Same rows, same order. The text is read only for
        // the chunks kept.
        let mut ranked = self.conn.prepare(
            r#"SELECT c.id, c.path
               FROM (SELECT rowid AS id, bm25(chunks_fts, 1.0, ?3) AS r
                     FROM chunks_fts
                     WHERE chunks_fts MATCH ?1
                     ORDER BY r
                     LIMIT ?2) t
               JOIN chunks c ON c.id = t.id
               ORDER BY t.r"#,
        )?;
        let mut text_of = self.conn.prepare_cached("SELECT text FROM chunks WHERE id = ?1")?;
        let depth = (k * CHUNKS_READ_PER_FILE) as i64;
        let mut add = |q: &str, hits: &mut Vec<FtsHit>, names: f64| -> Result<()> {
            let rows = ranked
                .query_map(params![q, depth, names], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            for (chunk_id, path) in rows {
                if hits.len() >= k {
                    break;
                }
                if hits.iter().any(|h| h.path == path) {
                    continue;
                }
                let text = text_of.query_row(params![chunk_id], |r| r.get(0))?;
                hits.push(FtsHit { chunk_id, path, text });
            }
            Ok(())
        };
        let mut hits = Vec::new();
        add(&fts_query, &mut hits, 1.0)?;
        // A question asks in its own words: "how are tokens validated in the
        // backend" has no chunk with every word ("validation", not
        // "validated"). When all-words finds too little, chunks with any of
        // them follow, bm25 putting those with the most and rarest first.
        // Such a question is in plain words, so there a name counts for less
        // ([`W_NAMES_ANY`]).
        if hits.len() < k && tokens.len() > 1 {
            add(&build_chunk_query(&tokens, true), &mut hits, W_NAMES_ANY)?;
        }
        Ok(hits)
    }

    /// Chunk-tier lookup for a set of chunk ids (kenignore task 2.4): the
    /// src-tauri `hybrid_search` command uses this to badge each hit with
    /// its `chunks.tier` (ken-core's `kenignore::Tier` numeric values — `0`
    /// = Full, `1` = SearchOnly; `Ignore` rows are never stored) without
    /// recomputing classification itself. Ids not found in `chunks` are
    /// simply absent from the returned map.
    pub fn chunk_tiers(&self, chunk_ids: &[i64]) -> Result<std::collections::HashMap<i64, i64>> {
        let mut out = std::collections::HashMap::new();
        if chunk_ids.is_empty() {
            return Ok(out);
        }
        let placeholders = chunk_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!("SELECT id, tier FROM chunks WHERE id IN ({placeholders})");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk_ids.iter()), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (id, tier) = row?;
            out.insert(id, tier);
        }
        Ok(out)
    }

    /// Total row count in `chunks` (semantic-index task 2.4's ~50k guardrail
    /// check). A cheap `COUNT(*)`, meant to be called once after each
    /// build/incremental update — not per search.
    pub fn chunk_count(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))?)
    }

    /// Chunks that have a meaning vector (0 before the first build, or
    /// without the vector extension).
    pub fn vector_count(&self) -> Result<i64> {
        if !self.vec_available {
            return Ok(0);
        }
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = 'vec_chunks')",
            [],
            |r| r.get(0),
        )?;
        if !exists {
            return Ok(0);
        }
        Ok(self.conn.query_row("SELECT COUNT(*) FROM vec_chunks", [], |r| r.get(0))?)
    }

    /// Forget every meaning vector and the model they came from, so the next
    /// build re-reads every chunk with a different model. The chunks and the
    /// keyword index stay: keyword search keeps working meanwhile.
    pub fn reset_vectors(&self) -> Result<()> {
        if self.vec_available {
            self.conn.execute_batch("DROP TABLE IF EXISTS vec_chunks;")?;
        }
        self.conn.execute(
            "DELETE FROM meta WHERE key IN ('embed_model', 'embed_dim', 'semantic_built_at')",
            [],
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
        let tx = self.conn.savepoint()?;
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
        let tx = self.conn.savepoint()?;
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
            strip_file(&tx, rel_path)?;
            tx.execute("DELETE FROM extractions WHERE rel_path = ?1", params![rel_path])?;
            tx.execute("DELETE FROM ocr_regions WHERE rel_path = ?1", params![rel_path])?;
            tx.execute("DELETE FROM ocr_pending WHERE rel_path = ?1", params![rel_path])?;
            tx.execute("DELETE FROM page_meta WHERE rel_path = ?1", params![rel_path])?;
            tx.execute("DELETE FROM file_authority WHERE rel_path = ?1", params![rel_path])?;
            tx.execute("DELETE FROM page_links WHERE from_path = ?1", params![rel_path])?;
            tx.execute("DELETE FROM code_symbols WHERE path = ?1", params![rel_path])?;
            tx.execute("DELETE FROM code_imports WHERE path = ?1", params![rel_path])?;
        }
        tx.commit()?;
        // Its chunks too, or a removed file keeps answering searches.
        self.delete_chunks(rel_path)
    }

    /// The chunks of `path` that have no vector yet, as (id, text): what a
    /// semantic rebuild still has to embed. Chunks are written by every scan
    /// (keyword search over chunks needs no model); vectors only by the
    /// rebuild. With no vector table, every chunk counts as missing.
    pub fn chunks_missing_vectors(&self, path: &str) -> Result<Vec<(i64, String)>> {
        let sql = if table_exists(&self.conn, "vec_chunks")? {
            "SELECT c.id, c.text FROM chunks c WHERE c.path = ?1
               AND NOT EXISTS (SELECT 1 FROM vec_chunks v WHERE v.chunk_id = c.id)
             ORDER BY c.seq"
        } else {
            "SELECT c.id, c.text FROM chunks c WHERE c.path = ?1 ORDER BY c.seq"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt
            .query_map(params![path], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Remove every indexed file under a folder (used when a folder is
    /// excluded or deleted).
    /// Remove every indexed file under a folder; returns how many.
    pub fn remove_folder(&mut self, rel_folder: &str) -> Result<usize> {
        let prefix = format!("{}/", rel_folder.trim_matches('/'));
        let mut stmt = self.conn.prepare(
            "SELECT rel_path FROM files WHERE rel_path = ?1 OR rel_path LIKE ?2 ESCAPE '\\'",
        )?;
        let like = format!("{}%", like_escape(&prefix));
        let paths: Vec<String> = stmt
            .query_map(params![rel_folder.trim_matches('/'), like], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        drop(stmt);
        let n = paths.len();
        for p in paths {
            self.remove_file(&p)?;
        }
        Ok(n)
    }

    /// Rewrite every row keyed by `rel_path` from one path prefix to another —
    /// the index-side half of a file/folder move. Returns how many `files` rows
    /// moved.
    ///
    /// This is the cheap alternative to "forget the subtree and rescan": a move
    /// changes no file's bytes, mtime or size, so re-extracting (or worse,
    /// clearing the whole index) would throw away extractions, OCR and
    /// transcripts to achieve a pure string rename. Everything happens in one
    /// transaction so a crash can't leave half the subtree at each path.
    ///
    /// Matching mirrors [`Self::remove_folder`]: the exact path (so a single
    /// file rename works too) plus `from/…` descendants, with `LIKE … ESCAPE`
    /// so a folder called `50%_plans` or `a_b` can't act as a wildcard. The
    /// trailing slash is what keeps a string-sibling like `docs2/` out of a
    /// `docs` move.
    ///
    /// Tables covered: `files`, `extractions`, `ocr_regions`, `ocr_pending`,
    /// `events.source`, `entities.sources` (a JSON array) and the file-valued
    /// `review_items.source_ref` kinds. `contents` is keyed by `file_id`, not by
    /// path, so its rows need no move — only the FTS `name` tokens of a row
    /// whose *basename* changed are rebuilt, and the `contents` update trigger
    /// carries that into the `search` index (which stores no path of its own:
    /// it is an external-content table over `contents`, joined back to `files`
    /// by rowid at query time).
    pub fn rename_prefix(&mut self, from_rel: &str, to_rel: &str) -> Result<usize> {
        let from = from_rel.trim_matches('/').to_string();
        let to = to_rel.trim_matches('/').to_string();
        if from.is_empty() || to.is_empty() || from == to {
            return Ok(0);
        }
        // `LIKE 'from/%'`, with the prefix escaped so `%`/`_`/`\` in a real
        // folder name stay literal. `substr` is 1-based, so cutting at
        // `from.len() + 1` keeps everything after the old prefix.
        let like = format!("{}/%", like_escape(&from));
        let cut = from.chars().count() as i64 + 1;
        let tx = self.conn.transaction()?;

        // Which rows are moving, and where to. Needed up front because the
        // basename-dependent FTS `name` and the JSON `entities.sources` can't be
        // rewritten by a single UPDATE.
        let pairs: Vec<(i64, String, String)> = {
            let mut stmt = tx.prepare(
                "SELECT id, rel_path FROM files
                 WHERE rel_path = ?1 OR rel_path LIKE ?2 ESCAPE '\\'",
            )?;
            let rows = stmt.query_map(params![from, like], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?;
            rows.map(|r| {
                r.map(|(id, old)| {
                    let new = format!("{to}{}", &old[from.len()..]);
                    (id, old, new)
                })
            })
            .collect::<std::result::Result<_, _>>()?
        };
        if pairs.is_empty() {
            tx.commit()?;
            return Ok(0);
        }

        // A move refuses an existing destination on disk, but the index can
        // still hold a stale row there (a path deleted outside the app, say).
        // Clear the destination first so the UNIQUE/PK constraints can't trip.
        for (table, col) in [
            ("files", "rel_path"),
            ("extractions", "rel_path"),
            ("ocr_regions", "rel_path"),
            ("ocr_pending", "rel_path"),
        ] {
            tx.execute(
                &format!(
                    "DELETE FROM {table}
                     WHERE {col} = ?1 OR {col} LIKE ?2 ESCAPE '\\'"
                ),
                params![to, format!("{}/%", like_escape(&to))],
            )?;
        }

        // The bulk of the work: one prefix rewrite per path-keyed table.
        for (table, col) in [
            ("files", "rel_path"),
            ("extractions", "rel_path"),
            ("ocr_regions", "rel_path"),
            ("ocr_pending", "rel_path"),
            ("events", "source"),
        ] {
            tx.execute(
                &format!(
                    "UPDATE {table} SET {col} = ?1 || substr({col}, ?2)
                     WHERE {col} = ?3 OR {col} LIKE ?4 ESCAPE '\\'"
                ),
                params![to, cut, from, like],
            )?;
        }
        // Inbox items whose `source_ref` is a file path (the slug-valued kinds
        // must not be touched) — see `user_state::inbox_item_file_ref`.
        tx.execute(
            "UPDATE review_items SET source_ref = ?1 || substr(source_ref, ?2)
             WHERE kind IN ('failed-file', 'conflict', 'conflict-copy', 'stored')
               AND (source_ref = ?3 OR source_ref LIKE ?4 ESCAPE '\\')",
            params![to, cut, from, like],
        )?;

        // FTS name tokens: only rows whose basename actually changed (a folder
        // move leaves every child's filename alone, so this is usually a no-op).
        for (id, old, new) in &pairs {
            let (old_name, new_name) = (name_tokens(old), name_tokens(new));
            if old_name != new_name {
                tx.execute(
                    "UPDATE contents SET name = ?2 WHERE file_id = ?1",
                    params![id, new_name],
                )?;
            }
        }

        // `entities.sources` is a JSON array of rel_paths, so it needs a
        // decode/rewrite/encode rather than SQL string surgery.
        let renames: std::collections::HashMap<&str, &str> =
            pairs.iter().map(|(_, o, n)| (o.as_str(), n.as_str())).collect();
        let ent_rows: Vec<(i64, String)> = {
            let mut stmt = tx.prepare("SELECT id, sources FROM entities")?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        for (id, json) in ent_rows {
            let mut sources: Vec<String> = serde_json::from_str(&json).unwrap_or_default();
            let mut touched = false;
            for src in sources.iter_mut() {
                if let Some(new) = renames.get(src.as_str()) {
                    *src = (*new).to_string();
                    touched = true;
                }
            }
            if touched {
                tx.execute(
                    "UPDATE entities SET sources = ?2 WHERE id = ?1",
                    params![id, serde_json::to_string(&sources).unwrap()],
                )?;
            }
        }

        tx.commit()?;
        Ok(pairs.len())
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

    /// A ~`window_chars` paragraph excerpt from a file's stored text, centered
    /// on the first case-insensitive occurrence of any `query_tokens` token and
    /// trimmed to word boundaries so neither end splits a UTF-8 char or lands
    /// mid-word. Richer context than the ~12-token FTS `snippet()` for feeding a
    /// quick-answer prompt. Returns `None` when there is no stored content
    /// (binary/OCR-pending/failed) or no token matches, so the caller can fall
    /// back to the FTS snippet. Pass tokens from [`query_tokens`] for the same
    /// tokenization the search used.
    pub fn excerpt(
        &self,
        rel_path: &str,
        query_tokens: &[String],
        window_chars: usize,
    ) -> Option<String> {
        let text = self.get_text(rel_path).ok().flatten()?;
        if text.trim().is_empty() {
            return None;
        }
        // First match of any token, case-insensitively. Lowercasing can shift
        // byte offsets for exotic Unicode, so we only use `pos` to locate the
        // match and always re-snap to a char boundary of the *original* text
        // before slicing.
        let lower = text.to_lowercase();
        let raw_pos = query_tokens
            .iter()
            .filter(|t| !t.is_empty())
            .filter_map(|t| lower.find(&t.to_lowercase()))
            .min()?;
        let mut pos = raw_pos.min(text.len());
        while pos > 0 && !text.is_char_boundary(pos) {
            pos -= 1;
        }

        // Grow a window of ~`window_chars` chars centered on the match: half the
        // budget of chars before `pos`, half from `pos` forward (which covers the
        // matched token itself). `char_indices` keeps every offset on a boundary.
        let half = window_chars / 2;
        let start = text[..pos]
            .char_indices()
            .rev()
            .take(half)
            .last()
            .map(|(i, _)| i)
            .unwrap_or(0);
        let end = text[pos..]
            .char_indices()
            .take(half.max(1))
            .last()
            .map(|(i, c)| pos + i + c.len_utf8())
            .unwrap_or_else(|| text.len());

        // Pull each edge in to the nearest whitespace so we never emit a partial
        // word (only when we actually cut — a window that reaches the document
        // edge keeps that edge).
        let mut s = start;
        if s > 0 {
            if let Some(off) = text[s..].find(char::is_whitespace) {
                s += off;
            }
            let trimmed = text[s..].trim_start();
            s = text.len() - trimmed.len();
        }
        let mut e = end;
        if e < text.len() {
            if let Some(off) = text[..e].rfind(char::is_whitespace) {
                e = off;
            }
        }
        if s >= e {
            return None;
        }
        let mut excerpt = text[s..e].trim().to_string();
        if excerpt.is_empty() {
            return None;
        }
        // Leading/trailing ellipses mark a mid-document fragment, matching the
        // shape of FTS snippets.
        if s > 0 {
            excerpt = format!("…{excerpt}");
        }
        if e < text.len() {
            excerpt.push('…');
        }
        Some(excerpt)
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
        // Drop stopwords before building any pass (FTS, phrase, filename LIKE) so
        // question-form queries don't pay for intersecting their giant posting
        // lists — the last (prefix) token and pure-stopword queries are preserved
        // by `significant_token_list`.
        let tokens = significant_token_list(&tokens);
        let fts_query = build_fts_query(&tokens);
        let mut stmt = self.conn.prepare(
            r#"SELECT f.rel_path, f.kind, f.status,
                      snippet(search, 0, '<mark>', '</mark>', '…', 12),
                      bm25(search, 1.0, 4.0), f.mtime
               FROM search JOIN files f ON f.id = search.rowid
               WHERE search MATCH ?1
               ORDER BY bm25(search, 1.0, 4.0)
               LIMIT ?2"#,
        )?;
        // `mtimes` carries each hit's file mtime (unix seconds) out of the query
        // rows so the recency blend can be applied after all passes merge,
        // without widening `SearchHit` (the frontend never needs it).
        let mut mtimes: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
        let mut hits: Vec<SearchHit> = stmt
            .query_map(params![fts_query, limit as i64], |r| {
                let rel_path: String = r.get(0)?;
                let mtime: i64 = r.get(5)?;
                Ok((
                    SearchHit {
                        rel_path,
                        kind: r.get(1)?,
                        status: r.get(2)?,
                        snippet: r.get(3)?,
                        rank: r.get(4)?,
                    },
                    mtime,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(|(h, m)| {
                mtimes.insert(h.rel_path.clone(), m);
                h
            })
            .collect();
        drop(stmt);

        // Phrase boost: for multi-token queries, docs where the tokens occur as
        // an adjacent phrase are the strongest lexical matches. A second MATCH
        // pass with an FTS5 phrase query ("tok1 tok2 …") finds them; each such
        // rel_path gets `PHRASE_BONUS` shaved off its rank. A phrase match always
        // contains every token, so it is already a subset of the main pass — we
        // only ever adjust ranks here, never introduce new hits.
        if tokens.len() >= 2 {
            let phrase_query = format!("\"{}\"", tokens.join(" "));
            let mut pstmt = self.conn.prepare(
                r#"SELECT f.rel_path FROM search JOIN files f ON f.id = search.rowid
                   WHERE search MATCH ?1
                   ORDER BY bm25(search, 1.0, 4.0)
                   LIMIT ?2"#,
            )?;
            let phrase_paths: std::collections::HashSet<String> = pstmt
                .query_map(params![phrase_query, limit as i64], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<_, _>>()?;
            drop(pstmt);
            for hit in hits.iter_mut() {
                if phrase_paths.contains(&hit.rel_path) {
                    hit.rank -= PHRASE_BONUS;
                }
            }
        }

        // Filename pass: every token as a case-insensitive substring of the
        // rel_path. Catches what FTS name tokens can't — mid-word matches
        // and non-final prefixes.
        let lowered: Vec<String> = tokens.iter().map(|t| t.to_lowercase()).collect();
        let clauses = vec!["lower(rel_path) LIKE ? ESCAPE '\\'"; lowered.len()].join(" AND ");
        // Bound the scan: without a LIMIT a short/common token (e.g. a single
        // char) scans and returns the whole `files` table, which — combined with
        // this running synchronously on the command thread — froze the UI. We
        // only ever keep `limit` results anyway, so cap the scan there.
        let sql = format!(
            "SELECT rel_path, kind, status, mtime FROM files WHERE {clauses} LIMIT {limit}"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let patterns: Vec<String> = lowered
            .iter()
            .map(|t| format!("%{}%", like_escape(t)))
            .collect();
        let name_rows: Vec<(String, String, String, i64)> = stmt
            .query_map(rusqlite::params_from_iter(&patterns), |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<std::result::Result<_, _>>()?;

        // Merge in O(n) via a rel_path -> index map instead of an O(n²) linear
        // scan of `hits` per name row.
        let mut index: std::collections::HashMap<String, usize> = hits
            .iter()
            .enumerate()
            .map(|(i, h)| (h.rel_path.clone(), i))
            .collect();
        for (rel_path, kind, status, mtime) in name_rows {
            let rank = name_match_rank(&rel_path, query);
            mtimes.entry(rel_path.clone()).or_insert(mtime);
            match index.get(&rel_path) {
                Some(&i) => {
                    // Both passes hit: keep the better rank, and the content
                    // snippet unless it's empty (metadata-only/failed files).
                    let hit = &mut hits[i];
                    if rank < hit.rank {
                        hit.rank = rank;
                    }
                    if hit.snippet.is_empty() {
                        hit.snippet = name_snippet(&rel_path, &lowered);
                    }
                }
                None => {
                    let snippet = name_snippet(&rel_path, &lowered);
                    index.insert(rel_path.clone(), hits.len());
                    hits.push(SearchHit {
                        snippet,
                        rel_path,
                        kind,
                        status,
                        rank,
                    });
                }
            }
        }
        // Recency blend: fold a small, bounded (magnitude ≤ RECENCY_MAX) negative
        // offset from exponential mtime decay into every rank, so a recent file
        // edges out an equally-scored stale one. RECENCY_MAX is far smaller than
        // the gaps between the synthetic filename tiers (−2 / −50 / −100), so an
        // exact/prefix filename match still dominates any content hit regardless
        // of how recent the content is.
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        for hit in hits.iter_mut() {
            let mtime = mtimes.get(&hit.rel_path).copied().unwrap_or(0);
            hit.rank += recency_bonus(mtime, now_secs);
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
        let tx = self.conn.savepoint()?;
        tx.execute("DELETE FROM contents", [])?;
        tx.execute("DELETE FROM files", [])?;
        tx.execute("DELETE FROM extractions", [])?;
        // OCR is derived like everything else: a reindex starts from scratch, so
        // stored regions and the OCR queue are wiped too.
        tx.execute("DELETE FROM ocr_regions", [])?;
        tx.execute("DELETE FROM ocr_pending", [])?;
        tx.execute("DELETE FROM entities", [])?; // cascades entity_edges
        tx.execute("DELETE FROM events", [])?;
        // Drop the knowledge-model build stamp too, so after a reindex the Map
        // reads as "not built yet" instead of showing a stale "built at" time
        // for a model whose entities/events we just deleted. User data
        // (chats/chat_messages) is intentionally left untouched.
        tx.execute(
            "DELETE FROM meta WHERE key = 'knowledge_model_built_at'",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn file_count(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?)
    }

    /// Index health in counts, for Your day: every file row, the failed
    /// ones, and the files still waiting to be indexed (a `pending` file
    /// row, or a file whose OCR text is still queued). Counting queries
    /// only, so it is cheap on a large index. Extraction backlog is a
    /// separate number ([`Db::index_health`]).
    pub fn file_health_counts(&self) -> Result<FileHealthCounts> {
        let (total, failed, pending): (i64, i64, i64) = self.conn.query_row(
            "SELECT COUNT(*),
                    COALESCE(SUM(status = 'failed'), 0),
                    COALESCE(SUM(status = 'pending'), 0)
               FROM files",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        // Queued is what waits to be read for entities (the white box's 8c:
        // "files queued for entities"), the same pending + retrying count the
        // Map's health line uses, plus anything not yet indexed.
        let extraction: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM extractions
                  WHERE status = 'pending' OR (status = 'error' AND attempts < ?1)",
                params![MAX_EXTRACTION_ATTEMPTS],
                |r| r.get(0),
            )
            .unwrap_or(0);
        Ok(FileHealthCounts { total, failed, queued: pending + extraction })
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

    /// Count of indexed files that actually have a stored `contents` row — the
    /// exact population `backfill_extractions` can enqueue and the worker can
    /// process. This is the extraction-coverage denominator: an `indexed` row
    /// with no `contents` row (a legacy/corrupt state) can never be extracted,
    /// so counting it toward "N of M analyzed" would strand coverage below 100%
    /// forever with no queued work to close the gap. In the healthy case every
    /// `indexed` file has a `contents` row (`upsert_file` always writes one), so
    /// this equals `indexed_file_count`; it only diverges to drop files that are
    /// uncountable by construction.
    pub fn extractable_file_count(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM files f
             WHERE f.status = 'indexed' AND f.tier = 0
               AND EXISTS (SELECT 1 FROM contents c WHERE c.file_id = f.id)",
            [],
            |r| r.get(0),
        )?)
    }

    /// Count of indexed-with-content files that have NO `extractions` row yet —
    /// i.e. the queue never picked them up. Should be 0 after `backfill_extractions`;
    /// a nonzero value means the coverage denominator and the enqueuable
    /// population disagree (the Map would stick below 100% with no work queued).
    /// Used as a lightweight post-backfill sanity check.
    pub fn unqueued_extractable_count(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM files f
             WHERE f.status = 'indexed' AND f.tier = 0
               AND EXISTS (SELECT 1 FROM contents c WHERE c.file_id = f.id)
               AND NOT EXISTS (SELECT 1 FROM extractions e WHERE e.rel_path = f.rel_path)",
            [],
            |r| r.get(0),
        )?)
    }

    // --- ingest run log ---

    pub fn insert_run(&mut self, slug: &str, session_id: Option<&str>, started_at: i64) -> Result<i64> {
        self.insert_run_kind(slug, session_id, started_at, "ingest")
    }

    pub fn insert_run_kind(
        &mut self,
        slug: &str,
        session_id: Option<&str>,
        started_at: i64,
        kind: &str,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO ingest_runs (slug, kind, session_id, started_at, status)
             VALUES (?1, ?2, ?3, ?4, 'running')",
            params![slug, kind, session_id, started_at],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn list_runs_of_kind(&self, slug: &str, kind: &str, limit: usize) -> Result<Vec<RunRow>> {
        let sql = format!(
            "SELECT {} FROM ingest_runs WHERE slug = ?1 AND kind = ?2
             ORDER BY started_at DESC, id DESC LIMIT ?3",
            Self::RUN_COLS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params![slug, kind, limit as i64], Self::map_run)?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
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
            kind: r.get(2)?,
            session_id: r.get(3)?,
            started_at: r.get(4)?,
            finished_at: r.get(5)?,
            status: r.get(6)?,
            summary: r.get(7)?,
            error: r.get(8)?,
            change_ratio: r.get(9)?,
        })
    }

    const RUN_COLS: &'static str =
        "id, slug, kind, session_id, started_at, finished_at, status, summary, error, change_ratio";

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

    /// Open a resolved review item again.
    pub fn reopen_review_item(&mut self, id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE review_items SET status = 'open', resolved_at = NULL WHERE id = ?1",
            params![id],
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
    /// Every review item of one kind, open or resolved, newest first: an
    /// inbox's history.
    pub fn review_items_of_kind(&self, kind: &str, limit: usize) -> Result<Vec<ReviewItemRow>> {
        let sql = format!(
            "SELECT {} FROM review_items WHERE kind = ?1 ORDER BY created_at DESC, id DESC LIMIT ?2",
            Self::REVIEW_ITEM_COLS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params![kind, limit as i64], Self::map_review_item)?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

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
            r#"INSERT INTO chats (id, title, kind, pinned, status, created_at, last_active_at, archived, model, scope)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
               ON CONFLICT(id) DO UPDATE SET
                 title = ?2, kind = ?3, pinned = ?4, status = ?5,
                 last_active_at = ?7, archived = ?8, model = ?9, scope = ?10"#,
            params![
                chat.id,
                chat.title,
                chat.kind,
                chat.pinned as i64,
                chat.status,
                chat.created_at,
                chat.last_active_at,
                chat.archived as i64,
                chat.model,
                chat.scope
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
            scope: r.get(9)?,
        })
    }

    const CHAT_COLS: &'static str =
        "id, title, kind, pinned, status, created_at, last_active_at, archived, model, scope";

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
            ChatField::Scope => "UPDATE chats SET scope = ?2 WHERE id = ?1",
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

    /// Rewrite one message's content in place (a `question` row gaining its
    /// answers, so a transcript reload shows the choice already made).
    pub fn update_chat_message_content(&self, message_id: i64, content: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE chat_messages SET content = ?2 WHERE id = ?1",
            params![message_id, content],
        )?;
        Ok(())
    }

    /// One message's content (a tool card, before its result is added).
    pub fn chat_message_content(&self, message_id: i64) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT content FROM chat_messages WHERE id = ?1", params![message_id], |r| r.get(0))
            .optional()?)
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

    // --- incremental extraction queue (Map worker bookkeeping) ---

    /// Enqueue a file for extraction unless its content is already extracted.
    /// Returns true if a `pending` row was written (a new file or a changed
    /// hash), false if an identical hash is already `done`.
    pub fn enqueue_extraction_if_changed(
        &mut self,
        rel_path: &str,
        content_hash: &str,
    ) -> Result<bool> {
        let already_done: bool = self.conn.query_row(
            "SELECT 1 FROM extractions
             WHERE rel_path = ?1 AND content_hash = ?2 AND status = 'done'",
            params![rel_path, content_hash],
            |_| Ok(true),
        ).unwrap_or(false);
        if already_done {
            return Ok(false);
        }
        // A changed file starts its retries over: the three attempts belong to
        // the content that failed, not the path. The same content re-queued
        // keeps its count.
        self.conn.execute(
            "INSERT INTO extractions (rel_path, content_hash, status)
             VALUES (?1, ?2, 'pending')
             ON CONFLICT(rel_path) DO UPDATE SET
               attempts = CASE WHEN extractions.content_hash = ?2 THEN extractions.attempts ELSE 0 END,
               content_hash = ?2, status = 'pending', error = NULL",
            params![rel_path, content_hash],
        )?;
        Ok(true)
    }

    /// The index as an instrument (docs-system: "the tool shows index health:
    /// pending, failed and skipped extractions"). An index that has stopped
    /// extracting still answers searches, with thin results and no error;
    /// these numbers are how anyone can tell.
    pub fn index_health(&self) -> Result<IndexHealth> {
        let count = |sql: &str| -> Result<i64> { Ok(self.conn.query_row(sql, [], |r| r.get(0))?) };
        let (analyzed, extractable) = self.extraction_coverage()?;
        Ok(IndexHealth {
            analyzed,
            extractable,
            pending: count("SELECT COUNT(*) FROM extractions WHERE status = 'pending'")?,
            retrying: self.conn.query_row(
                "SELECT COUNT(*) FROM extractions WHERE status = 'error' AND attempts < ?1",
                params![MAX_EXTRACTION_ATTEMPTS],
                |r| r.get(0),
            )?,
            failed: self.extraction_failed_count()?,
            skipped: count(
                "SELECT COUNT(*) FROM files f
                  WHERE f.status = 'indexed' AND f.tier <> 0
                    AND EXISTS (SELECT 1 FROM contents c WHERE c.file_id = f.id)",
            )?,
            control: self
                .meta_get("index_control")?
                .and_then(|s| serde_json::from_str(&s).ok()),
        })
    }

    /// Record the last control query's result (see `scan::control_query`).
    pub fn set_index_control(&self, result: &ControlResult) -> Result<()> {
        let json = serde_json::to_string(result).map_err(|e| crate::Error::Other(e.to_string()))?;
        self.meta_set("index_control", &json)
    }

    /// The oldest `pending` file (insertion order via implicit rowid) and the
    /// hash to stamp when it completes.
    pub fn next_pending_extraction(&self) -> Result<Option<(String, String)>> {
        match self.conn.query_row(
            "SELECT rel_path, content_hash FROM extractions
             WHERE status = 'pending' ORDER BY rowid LIMIT 1",
            [],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        ) {
            Ok(row) => Ok(Some(row)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Mark a file's extraction `done`, stamping the hash the worker actually
    /// extracted — but ONLY while that hash is still the one on the row. The
    /// worker pops a hash, drops the DB lock for the (slow) generation, and marks
    /// done on its own connection; meanwhile the scanner may re-index the file and
    /// re-enqueue it under a NEW hash. Guarding on `content_hash` keeps this call
    /// from clobbering that fresher `pending` row back to `done` (which would lose
    /// the change until the next scan) — a mismatch is a harmless no-op and the
    /// newer hash stays queued for a re-run.
    pub fn mark_extraction_done(
        &mut self,
        rel_path: &str,
        content_hash: &str,
        at: i64,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE extractions
             SET extracted_at = ?3, status = 'done', error = NULL
             WHERE rel_path = ?1 AND content_hash = ?2",
            params![rel_path, content_hash, at],
        )?;
        Ok(())
    }

    /// Every file still waiting for extraction (pending or errored), with the
    /// hash it was queued at.
    pub fn waiting_extractions(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT rel_path, content_hash FROM extractions WHERE status IN ('pending', 'error')")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Mark these queued files done in one transaction, each only while its
    /// hash still matches (a file edited since stays queued).
    pub fn settle_extractions(&mut self, rows: &[(String, String)], at: i64) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let mut n = 0;
        {
            let mut stmt = tx.prepare(
                "UPDATE extractions SET extracted_at = ?3, status = 'done', error = NULL
                 WHERE rel_path = ?1 AND content_hash = ?2",
            )?;
            for (path, hash) in rows {
                n += stmt.execute(params![path, hash, at])?;
            }
        }
        tx.commit()?;
        Ok(n)
    }

    /// How many entities the knowledge model holds.
    pub fn entity_count(&self) -> Result<i64> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM entities", [], |r| r.get(0))?)
    }

    /// Mark a file's extraction `error` and bump its retry counter — but ONLY
    /// while the queued `content_hash` still matches the row (same guard as
    /// `mark_extraction_done`). The worker pops a hash, drops the DB lock for the
    /// slow generation, then reports back on its own connection; meanwhile the
    /// scanner may re-index the file and re-enqueue it under a NEW hash. Guarding
    /// on `content_hash` keeps a stale generation failure from flipping that
    /// freshly re-enqueued `pending` row back to `error` — a mismatch is a
    /// harmless no-op and the newer hash stays queued for a re-run.
    pub fn mark_extraction_error(
        &mut self,
        rel_path: &str,
        content_hash: &str,
        message: &str,
        _at: i64,
    ) -> Result<()> {
        // Deliberately do NOT stamp `extracted_at` — it must reflect the LAST
        // SUCCESS only. Stamping it here would let an errored-then-changed file
        // (re-enqueued to `pending`, keeping this timestamp) count toward
        // `extraction_coverage`, inflating "N of M analyzed" for a file that
        // never actually succeeded. `attempts` is bumped so bounded retries
        // (see `requeue_errored_extractions`) eventually give up.
        self.conn.execute(
            "UPDATE extractions
             SET status = 'error', error = ?3, attempts = attempts + 1
             WHERE rel_path = ?1 AND content_hash = ?2",
            params![rel_path, content_hash, message],
        )?;
        Ok(())
    }

    /// `(files extracted, extractable files)` — the Map coverage line's
    /// numerator and denominator. A file counts once it has a successful
    /// `extracted_at` on record and is not currently in `error`; it keeps
    /// counting after a content change re-queues it (`pending` with the prior
    /// `extracted_at` preserved) until the re-run overwrites or it errors.
    ///
    /// The denominator is `extractable_file_count` (indexed AND has content),
    /// NOT the raw indexed count: that is exactly the population `backfill_extractions`
    /// enqueues, so an `indexed` file that can never be extracted (no `contents`
    /// row) can't create a permanent gap that freezes the count at "0 of N".
    pub fn extraction_coverage(&self) -> Result<(i64, i64)> {
        let done: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM extractions
             WHERE extracted_at IS NOT NULL AND status <> 'error'",
            [],
            |r| r.get(0),
        )?;
        Ok((done, self.extractable_file_count()?))
    }

    /// Count of files whose extraction has failed *permanently*: `status =
    /// 'error'` with the retry budget exhausted (`attempts >= MAX_EXTRACTION_ATTEMPTS`).
    /// These are excluded from the coverage numerator forever, so the Map can
    /// surface them explicitly ("Y failed") instead of showing a silent gap
    /// below 100% that looks identical to "no progress". A row still within its
    /// retry budget is NOT counted — it may yet succeed on a self-heal requeue.
    pub fn extraction_failed_count(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM extractions
             WHERE status = 'error' AND attempts >= ?1",
            params![MAX_EXTRACTION_ATTEMPTS],
            |r| r.get(0),
        )?)
    }

    /// Store (or, with None, forget) what a page's frontmatter says.
    pub fn set_page_meta(&mut self, rel_path: &str, meta: Option<&crate::pagemeta::PageMeta>) -> Result<()> {
        let Some(m) = meta else {
            self.conn.execute("DELETE FROM page_meta WHERE rel_path = ?1", params![rel_path])?;
            return Ok(());
        };
        let json = |v: &Vec<String>| serde_json::to_string(v).unwrap_or_else(|_| "[]".into());
        self.conn.execute(
            r#"INSERT INTO page_meta (rel_path, title, aliases, status, verified, updated, sources, replaced_by, generated, audience)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
               ON CONFLICT(rel_path) DO UPDATE SET
                 title = ?2, aliases = ?3, status = ?4, verified = ?5, updated = ?6,
                 sources = ?7, replaced_by = ?8, generated = ?9, audience = ?10"#,
            params![
                rel_path,
                m.title,
                json(&m.aliases),
                m.status,
                m.verified,
                m.updated,
                json(&m.sources),
                json(&m.replaced_by),
                m.generated as i64,
                m.audience
            ],
        )?;
        Ok(())
    }

    /// The indexed files whose authority is worked out (`authority`): the
    /// Markdown pages, or with `all` every file (a team override can reach
    /// code too).
    pub fn authority_candidates(&self, all: bool) -> Result<Vec<String>> {
        let sql = if all {
            "SELECT rel_path FROM files WHERE status = 'indexed'"
        } else {
            "SELECT rel_path FROM files WHERE status = 'indexed' AND kind = 'md'"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    /// Whether `rel_path` is an indexed file.
    pub fn is_indexed(&self, rel_path: &str) -> Result<bool> {
        Ok(self
            .conn
            .prepare_cached("SELECT 1 FROM files WHERE rel_path = ?1 AND status = 'indexed'")?
            .exists(params![rel_path])?)
    }

    /// Store the files' authority (`authority`): with `only`, for those
    /// paths (each forgotten first, so one that became neutral loses its
    /// row); without, for every file, replacing what was stored.
    pub fn set_authority(&self, rows: &[(String, crate::authority::Authority)], only: Option<&[String]>) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        match only {
            Some(paths) => {
                for p in paths {
                    tx.execute("DELETE FROM file_authority WHERE rel_path = ?1", params![p])?;
                }
            }
            None => {
                tx.execute("DELETE FROM file_authority", [])?;
            }
        }
        for (rel, a) in rows {
            tx.execute(
                "INSERT OR REPLACE INTO file_authority (rel_path, tier, weight) VALUES (?1, ?2, ?3)",
                params![rel, a.tier.name(), a.weight],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// What each of `paths` adds to a hit's score for its authority; a path
    /// with none stored is neutral and absent. None for an index made before
    /// authority was stored (opened read-only, so not upgraded): search then
    /// ranks by the page's band, as it did.
    pub fn authority_weights(&self, paths: &[String]) -> Result<Option<std::collections::HashMap<String, f64>>> {
        if !table_exists(&self.conn, "file_authority")? {
            return Ok(None);
        }
        let mut out = std::collections::HashMap::new();
        let mut stmt = self.conn.prepare_cached("SELECT weight FROM file_authority WHERE rel_path = ?1")?;
        for p in paths {
            if out.contains_key(p) {
                continue;
            }
            if let Some(w) = stmt.query_row(params![p], |r| r.get::<_, f64>(0)).optional()? {
                out.insert(p.clone(), w);
            }
        }
        Ok(Some(out))
    }

    /// A file's chunks (id, first line, text), in order.
    pub fn chunks_of(&self, rel_path: &str) -> Result<Vec<(i64, Option<i64>, String)>> {
        let mut stmt = self.conn.prepare_cached("SELECT id, line, text FROM chunks WHERE path = ?1 ORDER BY seq")?;
        let rows = stmt
            .query_map(params![rel_path], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The stored meaning vectors of `chunk_ids`, those that have one. Empty
    /// without a meaning index.
    pub fn chunk_vectors(&self, chunk_ids: &[i64]) -> Result<std::collections::HashMap<i64, Vec<f32>>> {
        let mut out = std::collections::HashMap::new();
        if !self.vec_available || chunk_ids.is_empty() || !table_exists(&self.conn, "vec_chunks")? {
            return Ok(out);
        }
        let mut stmt = self.conn.prepare_cached("SELECT embedding FROM vec_chunks WHERE chunk_id = ?1")?;
        for id in chunk_ids {
            if let Some(blob) = stmt.query_row(params![id], |r| r.get::<_, Vec<u8>>(0)).optional()? {
                let v: Vec<f32> = blob.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
                out.insert(*id, v);
            }
        }
        Ok(out)
    }

    /// Store a log's supersessions (`supersede`), replacing the ones the
    /// last pass stored for it.
    pub fn replace_supersessions(&self, log: &str, pairs: &[crate::supersede::Supersession]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM supersessions WHERE log_path = ?1", params![log])?;
        let now = crate::engine::now_epoch();
        for s in pairs {
            tx.execute(
                "INSERT OR REPLACE INTO supersessions
                   (log_path, earlier, later, relation, point, evidence, earlier_line, later_line, source, made_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![log, s.earlier, s.later, s.relation, s.point, s.evidence, s.earlier_line, s.later_line, s.source, now],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Whether the index holds any supersessions: false for an index made
    /// before the pass, or opened read-only before it was upgraded.
    pub fn has_supersessions(&self) -> Result<bool> {
        if !table_exists(&self.conn, "supersessions")? {
            return Ok(false);
        }
        Ok(self.conn.prepare_cached("SELECT 1 FROM supersessions LIMIT 1")?.exists([])?)
    }

    fn supersessions_where(&self, clause: &str, args: &[&dyn rusqlite::ToSql]) -> Result<Vec<crate::supersede::Supersession>> {
        if !table_exists(&self.conn, "supersessions")? {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT log_path, earlier, later, relation, point, evidence, earlier_line, later_line, source
               FROM supersessions {clause} ORDER BY log_path, later_line, earlier_line"
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt
            .query_map(args, |r| {
                Ok(crate::supersede::Supersession {
                    log: r.get(0)?,
                    earlier: r.get(1)?,
                    later: r.get(2)?,
                    relation: r.get(3)?,
                    point: r.get(4)?,
                    evidence: r.get(5)?,
                    earlier_line: r.get(6)?,
                    later_line: r.get(7)?,
                    source: r.get(8)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Every stored supersession, or a log's.
    pub fn supersessions(&self, log: Option<&str>) -> Result<Vec<crate::supersede::Supersession>> {
        match log {
            Some(l) => self.supersessions_where("WHERE log_path = ?1", &[&l]),
            None => self.supersessions_where("", &[]),
        }
    }

    /// The pairs that touch entry `id` of `log`: those that supersede it,
    /// and those it supersedes.
    pub fn supersessions_of(&self, log: &str, id: &str) -> Result<(Vec<crate::supersede::Supersession>, Vec<crate::supersede::Supersession>)> {
        Ok((
            self.supersessions_where("WHERE log_path = ?1 AND earlier = ?2", &[&log, &id])?,
            self.supersessions_where("WHERE log_path = ?1 AND later = ?2", &[&log, &id])?,
        ))
    }

    /// A file's stored authority (`authority`): its tier's name and the
    /// score it adds. None when it is neutral, or for an index made before
    /// authority was stored.
    pub fn authority_of(&self, rel_path: &str) -> Result<Option<(String, f64)>> {
        if !table_exists(&self.conn, "file_authority")? {
            return Ok(None);
        }
        Ok(self
            .conn
            .prepare_cached("SELECT tier, weight FROM file_authority WHERE rel_path = ?1")?
            .query_row(params![rel_path], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)))
            .optional()?)
    }

    /// Read the frontmatter of every indexed Markdown page that has no
    /// `page_meta` row, from its stored text (no file read). Pages indexed
    /// before frontmatter was read get theirs on the next open. Returns how
    /// many pages had frontmatter.
    pub fn backfill_page_meta(&mut self) -> Result<usize> {
        let rows: Vec<(String, String)> = {
            let mut stmt = self.conn.prepare(
                "SELECT f.rel_path, c.text FROM files f JOIN contents c ON c.file_id = f.id
                  WHERE f.kind = 'md' AND f.status = 'indexed'
                    AND NOT EXISTS (SELECT 1 FROM page_meta p WHERE p.rel_path = f.rel_path)",
            )?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<std::result::Result<_, _>>()?;
            rows
        };
        let mut n = 0;
        for (rel, text) in rows {
            if let Some(meta) = crate::pagemeta::parse(&text) {
                self.set_page_meta(&rel, Some(&meta))?;
                n += 1;
            }
        }
        Ok(n)
    }

    /// Extract the links of every indexed Markdown page that has none stored,
    /// from its stored text. A page with no links is re-read each time,
    /// which costs a parse, not a file read.
    pub fn backfill_page_links(&mut self) -> Result<usize> {
        let rows: Vec<(String, String)> = {
            let mut stmt = self.conn.prepare(
                "SELECT f.rel_path, c.text FROM files f JOIN contents c ON c.file_id = f.id
                  WHERE f.kind = 'md' AND f.status = 'indexed'
                    AND NOT EXISTS (SELECT 1 FROM page_links l WHERE l.from_path = f.rel_path)",
            )?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<std::result::Result<_, _>>()?;
            rows
        };
        let mut n = 0;
        for (rel, text) in rows {
            let links = crate::links::extract(&rel, &text);
            if !links.is_empty() {
                self.set_page_links(&rel, &links)?;
                n += 1;
            }
        }
        Ok(n)
    }

    /// Record a drift sweep: when, its exit code, how many pages, and the
    /// whole result as the log.
    pub fn insert_drift_run(&mut self, run: &crate::drift::DriftRun) -> Result<()> {
        let log = serde_json::to_string(run).map_err(|e| crate::Error::Other(e.to_string()))?;
        self.conn.execute(
            "INSERT INTO drift_runs (at, exit_code, pages, log) VALUES (?1, ?2, ?3, ?4)",
            params![run.at, run.exit_code, run.pages_examined as i64, log],
        )?;
        Ok(())
    }

    /// When the last drift sweep ran, if one has.
    pub fn last_drift_run_at(&self) -> Result<Option<i64>> {
        Ok(self.conn.query_row("SELECT MAX(at) FROM drift_runs", [], |r| r.get::<_, Option<i64>>(0))?)
    }

    /// The last drift sweep's full result.
    pub fn last_drift_run(&self) -> Result<Option<crate::drift::DriftRun>> {
        let log: Option<String> = self
            .conn
            .query_row("SELECT log FROM drift_runs ORDER BY at DESC, id DESC LIMIT 1", [], |r| r.get(0))
            .optional()?;
        Ok(log.and_then(|l| serde_json::from_str(&l).ok()))
    }

    /// Replace the links stored for the page at `from`.
    pub fn set_page_links(&mut self, from: &str, links: &[crate::links::Link]) -> Result<()> {
        let tx = self.conn.savepoint()?;
        tx.execute("DELETE FROM page_links WHERE from_path = ?1", params![from])?;
        {
            let mut stmt = tx.prepare("INSERT OR IGNORE INTO page_links (from_path, kind, target) VALUES (?1, ?2, ?3)")?;
            for l in links {
                stmt.execute(params![from, l.kind.as_str(), l.target])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Every stored link, as (linking page, link).
    /// Replace one file's code map: its symbols and its imports.
    pub fn set_code_map(&mut self, path: &str, map: &crate::codemap::FileMap) -> Result<()> {
        // A savepoint: the scan calls this inside its own transaction.
        let tx = self.conn.savepoint()?;
        tx.execute("DELETE FROM code_symbols WHERE path = ?1", params![path])?;
        tx.execute("DELETE FROM code_imports WHERE path = ?1", params![path])?;
        {
            let mut sym = tx.prepare(
                "INSERT INTO code_symbols (path, name, kind, line, end_line, is_def, docs) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for s in &map.symbols {
                sym.execute(params![path, s.name, s.kind, s.line, s.end_line, s.is_def as i64, s.docs])?;
            }
            let mut imp = tx.prepare("INSERT INTO code_imports (path, target) VALUES (?1, ?2)")?;
            for target in &map.imports {
                imp.execute(params![path, target])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Code-map symbols matching a query: `name` exactly (case-insensitive),
    /// definitions only or references only, in `path` when given.
    pub fn code_symbols(&self, name: Option<&str>, path: Option<&str>, is_def: Option<bool>, limit: usize) -> Result<Vec<crate::codemap::Symbol>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, name, kind, line, end_line, is_def, docs FROM code_symbols
             WHERE (?1 IS NULL OR name = ?1 COLLATE NOCASE)
               AND (?2 IS NULL OR path = ?2)
               AND (?3 IS NULL OR is_def = ?3)
             ORDER BY path, line
             LIMIT ?4",
        )?;
        let rows = stmt.query_map(params![name, path, is_def.map(i64::from), limit as i64], |r| {
            Ok(crate::codemap::Symbol {
                path: r.get(0)?,
                name: r.get(1)?,
                kind: r.get(2)?,
                line: r.get(3)?,
                end_line: r.get(4)?,
                is_def: r.get::<_, i64>(5)? != 0,
                docs: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Every import of every file: (importing file, target as written).
    pub fn code_imports(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare("SELECT path, target FROM code_imports ORDER BY path")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// How many files have a code map: none on an index made before it.
    pub fn code_mapped_files(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM (SELECT DISTINCT path FROM code_symbols UNION SELECT DISTINCT path FROM code_imports)",
            [],
            |r| r.get(0),
        )?)
    }

    /// Map every indexed code file from its stored text, for an index made
    /// before the code map. Returns how many files were mapped.
    pub fn backfill_code_map(&mut self) -> Result<usize> {
        // Once per index. Not "is anything mapped": a scan maps the files it
        // re-reads, so a few mapped files say nothing about the rest.
        if self.meta_get(CODE_MAP_BACKFILLED)?.is_some() {
            return Ok(0);
        }
        let files: Vec<(String, String)> = {
            let mut stmt = self.conn.prepare(
                "SELECT f.rel_path, c.text FROM files f JOIN contents c ON c.file_id = f.id
                 WHERE f.status = 'indexed'
                   AND f.rel_path NOT IN (SELECT path FROM code_symbols UNION SELECT path FROM code_imports)",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let mut n = 0;
        let tx = self.conn.savepoint()?;
        for (path, text) in files {
            if let Some(map) = crate::codemap::map_file(&path, &text) {
                tx.execute("DELETE FROM code_symbols WHERE path = ?1", params![path])?;
                tx.execute("DELETE FROM code_imports WHERE path = ?1", params![path])?;
                for s in &map.symbols {
                    tx.execute(
                        "INSERT INTO code_symbols (path, name, kind, line, end_line, is_def, docs) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        params![path, s.name, s.kind, s.line, s.end_line, s.is_def as i64, s.docs],
                    )?;
                }
                for target in &map.imports {
                    tx.execute("INSERT INTO code_imports (path, target) VALUES (?1, ?2)", params![path, target])?;
                }
                n += 1;
            }
        }
        tx.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, '1')",
            params![CODE_MAP_BACKFILLED],
        )?;
        tx.commit()?;
        Ok(n)
    }

    /// The chunk that holds `line` of `path` (id, text): a definition as a hit.
    pub fn chunk_at_line(&self, path: &str, line: i64) -> Result<Option<(i64, String)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, text FROM chunks WHERE path = ?1 AND (line IS NULL OR line <= ?2) ORDER BY line DESC LIMIT 1",
                params![path, line],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    /// A file's first chunk (id, text): what a linked page shows as a hit.
    pub fn first_chunk(&self, rel_path: &str) -> Result<Option<(i64, String)>> {
        Ok(self
            .conn
            .query_row("SELECT id, text FROM chunks WHERE path = ?1 ORDER BY id LIMIT 1", params![rel_path], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?)
    }

    pub fn all_page_links(&self) -> Result<Vec<(String, crate::links::Link)>> {
        let mut stmt = self.conn.prepare("SELECT from_path, kind, target FROM page_links ORDER BY from_path")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .filter_map(|(from, kind, target)| {
                crate::links::LinkKind::parse(&kind).map(|kind| (from, crate::links::Link { kind, target }))
            })
            .collect())
    }

    /// Every indexed Markdown page, and every indexed file a path link can
    /// reach (an image, a PDF): path links are checked against all of them.
    pub fn page_paths(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT rel_path FROM files ORDER BY rel_path")?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Each page's frontmatter aliases, by path.
    pub fn page_aliases_by_path(&self) -> Result<Vec<(String, Vec<String>)>> {
        let mut stmt = self.conn.prepare("SELECT rel_path, aliases FROM page_meta WHERE aliases <> '[]'")?;
        let rows = stmt
            .query_map([], |r| {
                let aliases: String = r.get(1)?;
                Ok((r.get::<_, String>(0)?, serde_json::from_str(&aliases).unwrap_or_default()))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The open Review item of `kind`, if one exists: (id, body).
    pub fn open_review_item_of_kind(&self, kind: &str) -> Result<Option<(i64, String)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, body FROM review_items WHERE kind = ?1 AND status = 'open' ORDER BY id DESC LIMIT 1",
                params![kind],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    /// Indexed files whose file name is one of `names` (case-insensitive),
    /// wherever they sit.
    pub fn paths_named(&self, names: &[&str]) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT rel_path FROM files
              WHERE status = 'indexed' AND (lower(rel_path) = ?1 OR lower(rel_path) LIKE '%/' || ?1)
              ORDER BY rel_path",
        )?;
        let mut out = Vec::new();
        for name in names {
            let rows = stmt
                .query_map(params![name.to_lowercase()], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            out.extend(rows);
        }
        Ok(out)
    }

    /// Every page's frontmatter title and aliases (pages that have aliases).
    pub fn page_aliases(&self) -> Result<Vec<(Option<String>, Vec<String>)>> {
        let mut stmt = self.conn.prepare("SELECT title, aliases FROM page_meta WHERE aliases <> '[]'")?;
        let rows = stmt
            .query_map([], |r| {
                let aliases: String = r.get(1)?;
                Ok((r.get::<_, Option<String>>(0)?, serde_json::from_str(&aliases).unwrap_or_default()))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// What a page's frontmatter said when it was last indexed.
    pub fn page_meta(&self, rel_path: &str) -> Result<Option<crate::pagemeta::PageMeta>> {
        let list = |s: String| serde_json::from_str::<Vec<String>>(&s).unwrap_or_default();
        Ok(self
            .conn
            .query_row(
                "SELECT title, aliases, status, verified, updated, sources, replaced_by, generated, audience
                   FROM page_meta WHERE rel_path = ?1",
                params![rel_path],
                |r| {
                    Ok(crate::pagemeta::PageMeta {
                        title: r.get(0)?,
                        aliases: list(r.get(1)?),
                        status: r.get(2)?,
                        verified: r.get(3)?,
                        updated: r.get(4)?,
                        sources: list(r.get(5)?),
                        replaced_by: list(r.get(6)?),
                        generated: r.get::<_, i64>(7)? != 0,
                        audience: r.get(8)?,
                    })
                },
            )
            .optional()?)
    }

    /// The line a chunk starts on, when it has been chunked since lines were
    /// recorded.
    pub fn chunk_line(&self, chunk_id: i64) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row("SELECT line FROM chunks WHERE id = ?1", params![chunk_id], |r| {
                r.get::<_, Option<i64>>(0)
            })
            .optional()?
            .flatten())
    }

    /// The stored hash of a file's bytes, if it has one.
    pub fn file_byte_hash(&self, rel_path: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT byte_hash FROM files WHERE rel_path = ?1",
                params![rel_path],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    /// Record a file's byte hash, and its modified time when only that moved.
    pub fn set_file_byte_hash(&mut self, rel_path: &str, hash: &str, mtime: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE files SET byte_hash = ?2, mtime = ?3 WHERE rel_path = ?1",
            params![rel_path, hash, mtime],
        )?;
        Ok(())
    }

    /// Indexed files at the full tier: the ones read for entities.
    pub fn entity_tier_paths(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT rel_path FROM files WHERE status = 'indexed' AND tier = 0 ORDER BY rel_path",
        )?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Store each file's current kenignore tier, then take every file that is
    /// not at the full tier off the extraction queue. This is how a change of
    /// a repo's kind, or a new `~` line, reaches files already indexed:
    /// the scan skips an unchanged file, but not this. Done and errored rows
    /// stay, so nothing already in the graph loses its record. Returns
    /// (files re-tiered, pending extractions dropped).
    pub fn set_file_tiers(&mut self, tiers: &[(String, crate::kenignore::Tier)]) -> Result<(usize, usize)> {
        let tx = self.conn.savepoint()?;
        let mut retiered = 0;
        // Files leaving the full tier: what was read out of them for the graph
        // leaves with them, and their extraction record goes, so a file that
        // comes back to full is read again rather than counted as done.
        let mut demoted: Vec<&str> = Vec::new();
        {
            let mut was = tx.prepare("SELECT tier FROM files WHERE rel_path = ?1")?;
            let mut stmt =
                tx.prepare("UPDATE files SET tier = ?2 WHERE rel_path = ?1 AND tier <> ?2")?;
            for (rel, tier) in tiers {
                let before: Option<i64> = was.query_row(params![rel], |r| r.get(0)).ok();
                let changed = stmt.execute(params![rel, *tier as i64])?;
                retiered += changed;
                if changed > 0 && before == Some(0) && *tier as i64 != 0 {
                    demoted.push(rel.as_str());
                }
            }
        }
        strip_files(&tx, &demoted)?;
        {
            let mut del = tx.prepare("DELETE FROM extractions WHERE rel_path = ?1")?;
            for rel in &demoted {
                del.execute(params![rel])?;
            }
        }
        let dropped = tx.execute(
            "DELETE FROM extractions
              WHERE status = 'pending'
                AND rel_path IN (SELECT rel_path FROM files WHERE tier <> 0)",
            [],
        )?;
        tx.commit()?;
        Ok((retiered, dropped))
    }

    pub fn remove_extraction(&mut self, rel_path: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM extractions WHERE rel_path = ?1",
            params![rel_path],
        )?;
        Ok(())
    }

    /// One-time catch-up for the incremental Map: enqueue extraction for every
    /// `indexed` file that has NO `extractions` row yet. Projects indexed before
    /// the extraction queue existed have no rows, and the scanner never re-runs
    /// `index_one` for unchanged files, so those files would never reach the
    /// queue. Hashes the already-stored `contents` text (no byte re-read), and
    /// leaves any file that already has a row (pending/done/error) untouched, so
    /// it's idempotent and safe to call on every project open. Returns how many
    /// files were enqueued.
    /// A search-only file (`files.tier` <> 0) is never enqueued (kenignore D3).
    pub fn backfill_extractions(&mut self) -> Result<usize> {
        let mut stmt = self.conn.prepare(
            "SELECT f.rel_path, c.text
               FROM files f JOIN contents c ON c.file_id = f.id
              WHERE f.status = 'indexed' AND f.tier = 0
                AND NOT EXISTS (
                  SELECT 1 FROM extractions e WHERE e.rel_path = f.rel_path
                )",
        )?;
        let rows: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<std::result::Result<_, _>>()?;
        drop(stmt);
        let mut n = 0;
        for (rel, text) in rows {
            let hash = crate::knowledge_model::content_hash(&text);
            if self.enqueue_extraction_if_changed(&rel, &hash)? {
                n += 1;
            }
        }
        Ok(n)
    }

    /// Sentinel `mtime` stamped on a row that must be re-indexed by the next
    /// incremental scan. `scan::scan` treats a file as unchanged only when the
    /// stored `(size, mtime)` equals what's on disk; no real filesystem mtime
    /// can be negative, so this value guarantees the row no longer matches and
    /// `index_one` re-runs for it.
    const REINDEX_SENTINEL_MTIME: i64 = -1;

    /// One-time, idempotent reclassification of stored file kinds. `kind` is
    /// STORED at index time (via `FileKind::from_path` in `scan::index_one`),
    /// and the incremental scanner never re-runs `index_one` for an unchanged
    /// file — so when a kind's classification later changes (e.g. `.vtt` moving
    /// from "binary" to "txt"), pre-existing rows keep the stale kind forever.
    /// This walks every `files` row, recomputes the kind from its `rel_path`,
    /// and UPDATEs any that drifted — correcting the file-tree glyph and the
    /// search-hit kind WITHOUT re-reading file bytes. Returns how many rows were
    /// reclassified; safe to call on every project open.
    ///
    /// A row transitioning from a non-content kind ("binary"/"image") to a
    /// content kind ("txt"/"md"/…) must ALSO become content-searchable. We
    /// can't extract here (no byte read), but we CAN reset the row's stored
    /// `mtime` to [`Self::REINDEX_SENTINEL_MTIME`] so the next incremental
    /// `scan::scan` sees it as changed and re-runs `index_one` (re-extracting
    /// content + enqueuing extraction). Only those transitioned rows are
    /// re-index-flagged, to avoid needless work for a mere glyph change.
    pub fn refresh_stored_kinds(&mut self) -> Result<usize> {
        let mut stmt = self.conn.prepare("SELECT rel_path, kind FROM files")?;
        let rows: Vec<(String, String)> = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<_, _>>()?;
        drop(stmt);
        let tx = self.conn.savepoint()?;
        let mut n = 0;
        for (rel, stored_kind) in rows {
            let new_kind = crate::extract::FileKind::from_path(Path::new(&rel));
            let new_str = new_kind.as_str();
            if new_str == stored_kind {
                continue;
            }
            // The only kinds that carry no extractable content serialize as
            // "binary" / "image" (see `FileKind::has_content`). A row was
            // non-content iff its stored kind is one of those.
            let old_was_content = stored_kind != "binary" && stored_kind != "image";
            let gains_content = !old_was_content && new_kind.has_content();
            if gains_content {
                // The byte hash goes too: with it, the scan sees the same
                // bytes, takes the new mtime for a sync client's touch and
                // never reads the file (2026-10-06: `.ui` and `.kts` rows
                // stayed name-only after they became text).
                tx.execute(
                    "UPDATE files SET kind = ?2, mtime = ?3, byte_hash = NULL WHERE rel_path = ?1",
                    params![rel, new_str, Self::REINDEX_SENTINEL_MTIME],
                )?;
            } else {
                tx.execute(
                    "UPDATE files SET kind = ?2 WHERE rel_path = ?1",
                    params![rel, new_str],
                )?;
            }
            n += 1;
        }
        tx.commit()?;
        Ok(n)
    }

    /// Return `error` extraction rows that still have retries left (attempts <
    /// `MAX_EXTRACTION_ATTEMPTS`) to `pending` so the worker retries them. Called
    /// both when the local model becomes ready (install / selection / error
    /// recovery) AND every time the worker drains the queue and goes idle, so a
    /// file that errored under a transient fault — the batch-overflow bug, a bad
    /// load, momentary unparseable JSON — self-heals without a model restart or a
    /// content edit. The content hash is preserved, so a still-failing file
    /// simply errors again; once it exhausts the attempt cap it stays `error`
    /// permanently and won't be re-queued, so the queue can never spin forever.
    /// Returns how many rows were re-queued.
    pub fn requeue_errored_extractions(&mut self) -> Result<usize> {
        let n = self.conn.execute(
            "UPDATE extractions SET status = 'pending', error = NULL
             WHERE status = 'error' AND attempts < ?1",
            params![MAX_EXTRACTION_ATTEMPTS],
        )?;
        Ok(n)
    }

    // --- OCR queue + region storage (Phase 2; independent of extraction) ---

    /// Enqueue a file for OCR unless it is already `done` at this exact hash.
    /// The hash is a cheap per-version fingerprint (see `scan::index_one`), so a
    /// rescan that leaves a file unchanged never re-OCRs it, while a real change
    /// (new hash) re-queues it. Returns true if a `pending` row was written.
    /// Mirrors [`Self::enqueue_extraction_if_changed`].
    pub fn enqueue_ocr_if_changed(&mut self, rel_path: &str, content_hash: &str) -> Result<bool> {
        let already_done: bool = self
            .conn
            .query_row(
                "SELECT 1 FROM ocr_pending
                 WHERE rel_path = ?1 AND content_hash = ?2 AND status = 'done'",
                params![rel_path, content_hash],
                |_| Ok(true),
            )
            .unwrap_or(false);
        if already_done {
            return Ok(false);
        }
        self.conn.execute(
            "INSERT INTO ocr_pending (rel_path, content_hash, status)
             VALUES (?1, ?2, 'pending')
             ON CONFLICT(rel_path) DO UPDATE SET
               content_hash = ?2, status = 'pending', error = NULL, attempts = 0",
            params![rel_path, content_hash],
        )?;
        Ok(true)
    }

    /// The oldest `pending` OCR job (rel_path, hash-to-stamp), insertion order.
    pub fn next_pending_ocr(&self) -> Result<Option<(String, String)>> {
        match self.conn.query_row(
            "SELECT rel_path, content_hash FROM ocr_pending
             WHERE status = 'pending' ORDER BY rowid LIMIT 1",
            [],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        ) {
            Ok(row) => Ok(Some(row)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Store the recognized `regions` for a file and mark its OCR `done` —
    /// but ONLY while the row is still `pending` at this exact hash. The worker
    /// pops a hash, drops the DB lock for the (slow) Vision pass, then reports
    /// back; meanwhile the scanner may re-enqueue the file under a NEW hash.
    /// Guarding on `(hash, pending)` keeps a stale result from clobbering that
    /// fresher row and makes a duplicate call a harmless no-op.
    ///
    /// The recognized text is also merged into `contents` (appended to whatever
    /// the base extractor produced — empty EXIF for images, the sparse text
    /// layer for scanned PDFs), which fires the FTS trigger so the file becomes
    /// searchable by its OCR'd words. Because a content change re-runs
    /// `index_one` (rewriting `contents` to the fresh base text) *before*
    /// re-enqueuing OCR, this append never accumulates stale text across
    /// versions.
    pub fn mark_ocr_done(
        &mut self,
        rel_path: &str,
        content_hash: &str,
        regions: &[OcrRegionRow],
    ) -> Result<()> {
        let tx = self.conn.savepoint()?;
        let matches: bool = tx
            .query_row(
                "SELECT 1 FROM ocr_pending
                 WHERE rel_path = ?1 AND content_hash = ?2 AND status = 'pending'",
                params![rel_path, content_hash],
                |_| Ok(true),
            )
            .unwrap_or(false);
        if !matches {
            // Superseded by a newer hash, or already done: nothing to do.
            tx.commit()?;
            return Ok(());
        }
        // Replace any prior regions for this file (idempotent re-OCR).
        tx.execute("DELETE FROM ocr_regions WHERE rel_path = ?1", params![rel_path])?;
        for r in regions {
            tx.execute(
                "INSERT INTO ocr_regions (rel_path, page, text, x, y, w, h)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![rel_path, r.page, r.text, r.bbox[0], r.bbox[1], r.bbox[2], r.bbox[3]],
            )?;
        }
        // Merge the recognized text into `contents` so search picks it up.
        let ocr_text: String = regions
            .iter()
            .map(|r| r.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if !ocr_text.trim().is_empty() {
            let file_id: Option<i64> = tx
                .query_row(
                    "SELECT id FROM files WHERE rel_path = ?1",
                    params![rel_path],
                    |r| r.get(0),
                )
                .ok();
            if let Some(id) = file_id {
                let existing: Option<(String, String)> = tx
                    .query_row(
                        "SELECT text, name FROM contents WHERE file_id = ?1",
                        params![id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .ok();
                match existing {
                    Some((base, _name)) => {
                        let merged = if base.trim().is_empty() {
                            ocr_text
                        } else {
                            format!("{base}\n{ocr_text}")
                        };
                        tx.execute(
                            "UPDATE contents SET text = ?2 WHERE file_id = ?1",
                            params![id, merged],
                        )?;
                    }
                    None => {
                        tx.execute(
                            "INSERT INTO contents (file_id, text, name) VALUES (?1, ?2, ?3)",
                            params![id, ocr_text, name_tokens(rel_path)],
                        )?;
                    }
                }
            }
        }
        tx.execute(
            "UPDATE ocr_pending SET status = 'done', error = NULL
             WHERE rel_path = ?1 AND content_hash = ?2",
            params![rel_path, content_hash],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Mark a file's OCR `error` and bump its retry counter — but ONLY while the
    /// queued hash still matches (same guard as [`Self::mark_ocr_done`]).
    pub fn mark_ocr_error(&mut self, rel_path: &str, content_hash: &str, message: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE ocr_pending
             SET status = 'error', error = ?3, attempts = attempts + 1
             WHERE rel_path = ?1 AND content_hash = ?2",
            params![rel_path, content_hash, message],
        )?;
        Ok(())
    }

    /// Return `error` OCR rows that still have retries left to `pending` so the
    /// worker retries them when it next goes idle. Mirrors
    /// [`Self::requeue_errored_extractions`]. Returns how many were re-queued.
    pub fn requeue_errored_ocr(&mut self) -> Result<usize> {
        let n = self.conn.execute(
            "UPDATE ocr_pending SET status = 'pending', error = NULL
             WHERE status = 'error' AND attempts < ?1",
            params![MAX_OCR_ATTEMPTS],
        )?;
        Ok(n)
    }

    /// Queue again the OCR rows marked `done` with no regions once `engine` is
    /// the one reading them: before Windows had a backend, every queued image
    /// and scanned PDF there was marked done without being read. The engine is
    /// stamped in `meta` (`ocr_engine`), so this runs once per engine change.
    /// `unset_as` names the engine an index without a stamp was read by (on
    /// macOS, Vision, so nothing is read twice there). Returns the row count.
    pub fn requeue_ocr_for_engine(&mut self, engine: &str, unset_as: Option<&str>) -> Result<usize> {
        let stamped = self.meta_get("ocr_engine")?;
        if stamped.as_deref().or(unset_as) == Some(engine) {
            if stamped.is_none() {
                self.meta_set("ocr_engine", engine)?;
            }
            return Ok(0);
        }
        let n = self.conn.execute(
            "UPDATE ocr_pending SET status = 'pending', error = NULL, attempts = 0
             WHERE status = 'done'
               AND rel_path NOT IN (SELECT DISTINCT rel_path FROM ocr_regions)",
            [],
        )?;
        self.meta_set("ocr_engine", engine)?;
        Ok(n)
    }

    /// All stored OCR regions for a file, page-then-insertion order — the input
    /// to the Cmd+F highlight overlay (Phase 3). Empty when the file was never
    /// OCR'd (or had no text).
    pub fn get_ocr_regions(&self, rel_path: &str) -> Result<Vec<OcrRegionRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT page, text, x, y, w, h FROM ocr_regions
             WHERE rel_path = ?1 ORDER BY page, rowid",
        )?;
        let rows = stmt
            .query_map(params![rel_path], |r| {
                Ok(OcrRegionRow {
                    page: r.get(0)?,
                    text: r.get(1)?,
                    bbox: [r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?],
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    /// Drop a file's OCR bookkeeping (regions + queue row). Used by callers that
    /// invalidate a file's OCR without going through `remove_file`.
    pub fn remove_ocr(&mut self, rel_path: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM ocr_regions WHERE rel_path = ?1", params![rel_path])?;
        self.conn
            .execute("DELETE FROM ocr_pending WHERE rel_path = ?1", params![rel_path])?;
        Ok(())
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
        let tx = self.conn.savepoint()?;
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

    /// Merge one file's extraction delta into the persisted model. This is the
    /// convergence guarantee: it first strips the file's PRIOR contribution
    /// (so a rewrite replaces rather than accretes), then applies the delta
    /// with entity dedup (kind + normalized name), source union, longer-summary
    /// wins, unordered-pair edge dedup (first label wins), and (date,text)
    /// event dedup. Every entity/event is attributed to `rel_path`.
    pub fn merge_knowledge_delta(
        &mut self,
        rel_path: &str,
        delta: &knowledge_model::Extraction,
        at: i64,
    ) -> Result<()> {
        let tx = self.conn.savepoint()?;
        strip_file(&tx, rel_path)?;

        // 1. Existing entity identity → row id (over what survived the strip).
        let mut by_key: std::collections::HashMap<String, i64> = Default::default();
        {
            let mut stmt = tx.prepare("SELECT id, kind, name FROM entities")?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
            })?;
            for row in rows {
                let (id, kind, name) = row?;
                by_key.insert(entity_key(&kind, &name), id);
            }
        }

        // 2. Upsert delta entities; record batch index → row id for edges.
        let mut ids: Vec<i64> = Vec::with_capacity(delta.entities.len());
        for e in &delta.entities {
            let key = entity_key(&e.kind, &e.name);
            if let Some(&id) = by_key.get(&key) {
                // Union sources (add this file), keep the longer summary.
                let existing_sources: String = tx.query_row(
                    "SELECT sources FROM entities WHERE id = ?1",
                    params![id],
                    |r| r.get(0),
                )?;
                let mut sources: Vec<String> =
                    serde_json::from_str(&existing_sources).unwrap_or_default();
                if !sources.iter().any(|s| s == rel_path) {
                    sources.push(rel_path.to_string());
                }
                let existing_summary: String = tx.query_row(
                    "SELECT summary FROM entities WHERE id = ?1",
                    params![id],
                    |r| r.get(0),
                )?;
                let summary = if e.summary.chars().count() > existing_summary.chars().count() {
                    e.summary.clone()
                } else {
                    existing_summary
                };
                tx.execute(
                    "UPDATE entities SET summary = ?2, sources = ?3 WHERE id = ?1",
                    params![id, summary, serde_json::to_string(&sources).unwrap()],
                )?;
                ids.push(id);
            } else {
                let sources = serde_json::to_string(&vec![rel_path.to_string()]).unwrap();
                tx.execute(
                    "INSERT INTO entities (kind, name, summary, sources)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![e.kind, e.name, e.summary, sources],
                )?;
                let id = tx.last_insert_rowid();
                by_key.insert(key, id);
                ids.push(id);
            }
        }

        // 3. Edges: dedup by unordered pair across the WHOLE model, first wins.
        let mut pairs: std::collections::HashSet<(i64, i64)> = Default::default();
        {
            let mut stmt = tx.prepare("SELECT a, b FROM entity_edges")?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
            })?;
            for row in rows {
                let (a, b) = row?;
                pairs.insert((a.min(b), a.max(b)));
            }
        }
        for (from, e) in delta.entities.iter().enumerate() {
            for (to, label) in &e.connections {
                let (Some(&a), Some(&b)) = (ids.get(from), ids.get(*to)) else {
                    continue;
                };
                if a == b {
                    continue;
                }
                let pair = (a.min(b), a.max(b));
                if !pairs.insert(pair) {
                    continue;
                }
                tx.execute(
                    "INSERT INTO entity_edges (a, b, label) VALUES (?1, ?2, ?3)",
                    params![a, b, label],
                )?;
            }
        }

        // 4. Events: dedup by (date, text) across the whole model.
        let mut event_keys: std::collections::HashSet<(String, String)> = Default::default();
        {
            let mut stmt = tx.prepare("SELECT date, text FROM events")?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            for row in rows {
                event_keys.insert(row?);
            }
        }
        for ev in &delta.events {
            let key = (ev.date.clone(), ev.text.clone());
            if !event_keys.insert(key) {
                continue;
            }
            tx.execute(
                "INSERT INTO events (date, category, text, source)
                 VALUES (?1, ?2, ?3, ?4)",
                params![ev.date, ev.category, ev.text, rel_path],
            )?;
        }

        // Stamp the model timestamp so the Map treats the growing model as
        // "built" (the frontend's empty check also honours entity presence).
        tx.execute(
            "INSERT OR REPLACE INTO meta(key, value)
             VALUES ('knowledge_model_built_at', ?1)",
            params![at.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Remove a file's entire contribution from the model (used when the file
    /// is deleted or excluded). Entities grounded elsewhere survive; orphans
    /// and their edges are GC'd.
    pub fn purge_file_knowledge(&mut self, rel_path: &str) -> Result<()> {
        let tx = self.conn.savepoint()?;
        strip_file(&tx, rel_path)?;
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
    Scope,
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
    /// The projects this chat asks about, bound when it was created
    /// (schema v13). `None` = this project only, which is what every chat
    /// created before this column existed means. `Some("all")` = every
    /// workspace member; any other value names a group. Widens what the
    /// session may READ; writes stay pinned to the focused project.
    pub scope: Option<String>,
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
    /// `user` | `assistant` | `activity` | `divider` | `question`
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

/// The entity identity used by incremental merge: kind plus a
/// whitespace-normalized, case-folded name. `\u{1}` can't occur in a real
/// name, so it's a safe key separator.
fn entity_key(kind: &str, name: &str) -> String {
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    format!("{kind}\u{1}{name}")
}

/// Whether a table or virtual table named `name` exists. Used by the
/// semantic-index chunk CRUD to guard `vec_chunks` access: that table is
/// created lazily (only once an embedder's dimension is known via
/// `ensure_vec_chunks`), so code that runs before any embedding has
/// happened must not assume it's there.
fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type IN ('table','view') AND name = ?1",
            params![name],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false))
}

/// Serialize an f32 vector into the raw little-endian byte blob `vec0`
/// expects for both inserting into `vec_chunks.embedding` and binding a KNN
/// query vector.
fn f32_slice_to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// Strip one file's prior contribution to the knowledge model, in the caller's
/// transaction: delete its events, remove it from every entity's `sources`,
/// and delete entities left with no sources (their edges cascade via the
/// `entity_edges` FK `ON DELETE CASCADE`).
/// [`strip_file`] for many files at once, reading the entities once: a whole
/// repo re-tiered to search-only would otherwise scan them once per file.
fn strip_files(conn: &Connection, rel_paths: &[&str]) -> Result<()> {
    if rel_paths.is_empty() {
        return Ok(());
    }
    {
        let mut del = conn.prepare("DELETE FROM events WHERE source = ?1")?;
        for p in rel_paths {
            del.execute(params![p])?;
        }
    }
    let gone: std::collections::HashSet<&str> = rel_paths.iter().copied().collect();
    let rows: Vec<(i64, String)> = {
        let mut stmt = conn.prepare("SELECT id, sources FROM entities")?;
        let mapped = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        mapped.collect::<std::result::Result<_, _>>()?
    };
    for (id, sources_json) in rows {
        let mut sources: Vec<String> = serde_json::from_str(&sources_json).unwrap_or_default();
        let before = sources.len();
        sources.retain(|s| !gone.contains(s.as_str()));
        if sources.len() == before {
            continue;
        }
        if sources.is_empty() {
            conn.execute("DELETE FROM entities WHERE id = ?1", params![id])?;
        } else {
            conn.execute("UPDATE entities SET sources = ?2 WHERE id = ?1", params![id, serde_json::to_string(&sources).unwrap()])?;
        }
    }
    Ok(())
}

fn strip_file(conn: &Connection, rel_path: &str) -> Result<()> {
    conn.execute("DELETE FROM events WHERE source = ?1", params![rel_path])?;

    let rows: Vec<(i64, String)> = {
        let mut stmt = conn.prepare("SELECT id, sources FROM entities")?;
        let mapped = stmt.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?;
        mapped.collect::<std::result::Result<_, _>>()?
    };
    for (id, sources_json) in rows {
        let mut sources: Vec<String> =
            serde_json::from_str(&sources_json).unwrap_or_default();
        if !sources.iter().any(|s| s == rel_path) {
            continue;
        }
        sources.retain(|s| s != rel_path);
        if sources.is_empty() {
            conn.execute("DELETE FROM entities WHERE id = ?1", params![id])?;
        } else {
            conn.execute(
                "UPDATE entities SET sources = ?2 WHERE id = ?1",
                params![id, serde_json::to_string(&sources).unwrap()],
            )?;
        }
    }
    Ok(())
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

/// Relative-path tokens for FTS index augmentation: the *whole* path (every
/// segment, not just the filename), separators spaced out
/// ("src/local_llm.rs" → "src local_llm rs" → "src local llm rs" after
/// separator-splitting). Distinct from `name_tokens`, which only tokenizes
/// the last path segment.
fn path_tokens(rel_path: &str) -> String {
    rel_path
        .replace(['/', '\\', '-', '_', '.', '—'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Declaration patterns, a recall aid for search and not a parser: false
/// negatives on exotic syntax are fine, a false positive adds a stray word.
/// The first is a declaration keyword after any modifiers (`pub fn x`,
/// `public final class X`, `export const x`). The second is a typed member
/// after at least one modifier (`private static final int MAX_ROWS = 7;`,
/// `public void open()`, `private readonly x: T`): on 2026-10-06 the single
/// old pattern read `static` as the keyword in Java and indexed the return
/// types (`void boolean final`) instead of the names.
fn symbol_regexes() -> &'static [Regex; 2] {
    static RE: OnceLock<[Regex; 2]> = OnceLock::new();
    RE.get_or_init(|| {
        [
            Regex::new(
                r"(?m)^[ \t]*(?:(?:pub(?:\([^)]*\))?|export|default|public|private|protected|internal|static|final|abstract|sealed|async|override|unsafe|extern)\s+)*(?:fn|function|def|class|struct|enum|trait|interface|impl|type|const|let|var|val|fun|func|mod|module|record)\s+([A-Za-z_][A-Za-z0-9_]*)",
            )
            .expect("a fixed, valid pattern"),
            Regex::new(
                r"(?m)^[ \t]*(?:(?:pub|public|private|protected|internal|static|final|abstract|synchronized|native|override|virtual|readonly|volatile|transient|default)\s+)+(?:[A-Za-z_][A-Za-z0-9_.]*(?:<[^;{}()=]*>)?(?:\[\])*\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*[(=;:]",
            )
            .expect("a fixed, valid pattern"),
        ]
    })
}

/// Declared names in a chunk's text (see [`symbol_regexes`]).
fn extract_symbol_names(text: &str) -> Vec<String> {
    symbol_regexes()
        .iter()
        .flat_map(|re| re.captures_iter(text).filter_map(|c| c.get(1).map(|m| m.as_str().to_string())))
        .collect()
}

/// A name's parts: split at `_` and `-`, then at camelCase and digit
/// boundaries (`search::camel_parts`), keeping each `_`/`-` part whole too.
/// Parts of one letter are dropped. Empty when the name has no parts.
fn name_parts(word: &str) -> Vec<String> {
    let mut out = Vec::new();
    let pieces: Vec<&str> = word.split(['_', '-']).filter(|p| !p.is_empty()).collect();
    for piece in &pieces {
        let camel = crate::search::camel_parts(piece);
        if pieces.len() > 1 {
            out.push(piece.to_string());
        }
        if camel.len() > 1 {
            out.extend(camel.into_iter().filter(|p| p.chars().count() > 1));
        }
    }
    out
}

/// The `names` column's BM25 weight (the text's is 1) in the any-word
/// search, which runs when no chunk holds every word of a question: a
/// question in plain words. At full weight one common word among a file's
/// names outranked a chunk whose text held most of the question: for "why
/// doesn't a looted world chest come back on the next tick", every file
/// with `Chest` in a name came before ChestPoolRespawn.java, whose text has
/// six of the eight words (2026-10-06: file 14 in its repo, 4 with names
/// left out). A quarter keeps a name in the score without letting it lead.
const W_NAMES_ANY: f64 = 0.25;

/// How many chunks the keyword search reads for each file it returns. A
/// file's best chunk stands for it. Read chunk by chunk, "do I keep the
/// bolts I already loaded" (2026-10-06) filled a repo's eight places with
/// four sections of AmmoKeepDecisionTest.java and two of CHANGELOG.md, and
/// AmmoKeep.java, the sixth file, never reached the ranking.
const CHUNKS_READ_PER_FILE: usize = 4;

/// Words longer than this in a chunk are not split into the `names` column:
/// base64 and hashes, not names.
const NAME_WORD_MAX: usize = 64;

/// The `names` column of the chunk keyword index: the path's words, the
/// declared names in the chunk, and the parts of every camelCase or
/// snake_case word in the chunk, each part also as its light stem
/// (`search::stem`), distinct words only. On 2026-10-06 "base damage ranges
/// for each weapon" never matched data/equipment/WeaponBases.json, because
/// `WeaponBases` is one token to the index; now `weapon`, `bases` and `bas`
/// are in its names. A column of its own rather than a header line on the
/// text: BM25 normalises each column by its own length, so a name match in
/// a long chunk of numbers is not diluted by the numbers, and a word repeated
/// fifty times in the code still counts once here.
fn fts_names(rel_path: &str, chunk_text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut add = |w: &str| {
        if !w.is_empty() && seen.insert(w.to_lowercase()) {
            out.push(w.to_string());
        }
    };
    let declared = extract_symbol_names(chunk_text);
    let named = path_tokens(rel_path).split(' ').map(str::to_string).chain(declared).collect::<Vec<_>>();
    for word in &named {
        add(word);
        add(&crate::search::stem(&word.to_lowercase()));
        for part in name_parts(word) {
            add(&part);
            add(&crate::search::stem(&part.to_lowercase()));
        }
    }
    // A log entry's id and title (`chunker::chunk_entry`) name its chunk as a
    // definition names a code chunk: "D-410" (also `D410`) and the words of
    // the ruling find the entry, not the one before it.
    if let Some((id, title)) = crate::chunker::is_markdown(rel_path).then(|| crate::chunker::chunk_entry(chunk_text)).flatten() {
        add(&id.replace('-', ""));
        for part in id.split('-') {
            add(part);
        }
        for word in title.split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() > 1 && !is_stopword(w)) {
            add(word);
            add(&crate::search::stem(&word.to_lowercase()));
        }
    }
    for word in chunk_text.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
        if word.len() > NAME_WORD_MAX || word.chars().all(|c| c.is_ascii_digit() || c == '_') {
            continue;
        }
        for part in name_parts(word) {
            add(&part);
            add(&crate::search::stem(&part.to_lowercase()));
        }
    }
    out.join(" ")
}

fn like_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

/// Split raw user input into safe query tokens: alphanumeric runs only —
/// FTS5 syntax and LIKE wildcards can't survive this. The ending of a
/// possessive or contraction goes first ("game's" is `game`, "doesn't" is
/// `doesn`): as a word of its own, `s` or `t` had to be in every chunk an
/// all-words search returned, and in the any-word search it matched every
/// `t` variable in the code.
fn query_tokens(input: &str) -> Vec<String> {
    input
        .split_whitespace()
        .map(|word| match word.trim_end_matches(|c: char| !c.is_alphanumeric()).rsplit_once(['\'', '’']) {
            Some((stem, end)) if ["s", "t", "d", "ll", "re", "ve", "m"].contains(&end.to_lowercase().as_str()) => stem,
            _ => word,
        })
        .flat_map(|word| word.split(|c: char| !c.is_alphanumeric()))
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect()
}

/// Common English stopwords dropped from the search path. Question-form
/// queries ("what is the operating model for the AI pilot") otherwise force
/// FTS5 to intersect the enormous posting lists of these ubiquitous words —
/// ~40× slower than the keyword-only form — and the AND semantics wrongly
/// exclude docs that simply lack a word like "what". Lowercase; compared
/// case-insensitively.
const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "do", "does", "for", "from", "has",
    "have", "how", "i", "in", "is", "it", "my", "of", "on", "or", "that", "the", "this", "to",
    "was", "we", "what", "when", "where", "which", "who", "why", "will", "with", "you",
];

fn is_stopword(token: &str) -> bool {
    let lower = token.to_lowercase();
    STOPWORDS.contains(&lower.as_str())
}

/// Filter stopwords out of an already-tokenized query, with two guards that keep
/// as-you-type and pure-stopword queries working:
/// (a) the **last** token is never dropped — it is the as-you-type prefix token,
///     and "the|" might be the user starting to type "theme";
/// (b) if filtering would leave no non-stopword token *besides* that last one
///     (e.g. a pure-stopword query like "what is the"), the original token list
///     is returned unchanged so the query still matches something.
fn significant_token_list(tokens: &[String]) -> Vec<String> {
    if tokens.len() < 2 {
        return tokens.to_vec();
    }
    let last = tokens.len() - 1;
    let filtered: Vec<String> = tokens
        .iter()
        .enumerate()
        .filter(|&(i, t)| i == last || !is_stopword(t))
        .map(|(_, t)| t.clone())
        .collect();
    // Every survivor besides the always-kept last token is a non-stopword, so
    // the count is simply `len - 1`. Fewer than one means the filter gutted the
    // query — keep it verbatim.
    if filtered.len().saturating_sub(1) < 1 {
        tokens.to_vec()
    } else {
        filtered
    }
}

/// Tokenize raw user input and drop stopwords per [`significant_token_list`]'s
/// rules. Exposed so quick-answer / excerpt callers reuse the exact token set
/// the search path builds its FTS query from, instead of hand-tokenizing.
pub fn significant_tokens(query: &str) -> Vec<String> {
    significant_token_list(&query_tokens(query))
}

/// A query for the chunk keyword index: all of `tokens` (the last as an
/// as-you-type prefix), or with `any` any of them, each exact; the any-word
/// form is the fallback when all of them together match too little. Each
/// word also matches its light stem (`search::stem`) when that differs, the
/// form the `names` column holds (`fts_names`): "base" then meets
/// `WeaponBases`.
fn build_chunk_query(tokens: &[String], any: bool) -> String {
    let last = tokens.len().saturating_sub(1);
    let terms: Vec<String> = tokens
        .iter()
        .enumerate()
        .map(|(i, t)| {
            // The as-you-type prefix as in `build_fts_query`.
            let word = if !any && i == last && t.chars().count() >= 2 { format!("\"{t}\" *") } else { format!("\"{t}\"") };
            let lower = t.to_lowercase();
            let stem = crate::search::stem(&lower);
            if stem == lower {
                word
            } else {
                format!("({word} OR \"{stem}\")")
            }
        })
        .collect();
    // FTS5 ANDs bare phrases side by side, but a bracketed group needs
    // the word.
    terms.join(if any { " OR " } else { " AND " })
}

/// Build an FTS5 query from tokens: each quoted, all ANDed, last token as
/// prefix for as-you-type feel.
fn build_fts_query(tokens: &[String]) -> String {
    let last = tokens.len().saturating_sub(1);
    let parts: Vec<String> = tokens
        .iter()
        .enumerate()
        .map(|(i, t)| {
            // Only apply the as-you-type prefix (`*`) once the last token is at
            // least two chars. A 1-char prefix like `a*` matches a huge share of
            // the index, turning a single keystroke into a near-full scan; match
            // the char exactly instead until a second char narrows it.
            if i == last && t.chars().count() >= 2 {
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

// Rank shaved off a doc whose query tokens occur as an adjacent phrase. Big
// enough to lift a true phrase match over docs with the same tokens scattered,
// small enough to stay well inside the gap to the filename tiers so filename
// dominance is untouched.
const PHRASE_BONUS: f64 = 5.0;

// Recency blend. `RECENCY_MAX` bounds the (negative) bonus a file can earn from
// being fresh; `RECENCY_TAU_DAYS` is the exponential decay constant. At ~1 week
// old the bonus is still ~−2.7 (near the −RECENCY_MAX floor), and by ~1 year it
// has decayed to essentially 0 — a recent file wins ties without ever
// overpowering the filename tiers (RECENCY_MAX ≪ 48, the smallest tier gap).
const RECENCY_MAX: f64 = 3.0;
const RECENCY_TAU_DAYS: f64 = 60.0;

/// Bounded negative recency offset for a file modified at `mtime` (unix
/// seconds), given `now_secs`. Exponential decay on age: −`RECENCY_MAX` for a
/// just-touched file, smoothly approaching 0 as the file ages. Future/clock-skew
/// mtimes clamp to age 0 (max freshness).
fn recency_bonus(mtime: i64, now_secs: i64) -> f64 {
    let age_days = (now_secs - mtime).max(0) as f64 / 86_400.0;
    -RECENCY_MAX * (-age_days / RECENCY_TAU_DAYS).exp()
}

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
    use crate::knowledge_model::Extraction;

    /// Build a one-entity delta quickly (connections empty).
    fn ent(kind: &str, name: &str, summary: &str) -> EntityInput {
        EntityInput {
            kind: kind.into(),
            name: name.into(),
            summary: summary.into(),
            sources: Vec::new(),
            connections: Vec::new(),
        }
    }

    // --- semantic-index task 1.2: v12 schema migration ---

    #[test]
    fn v11_db_migrates_to_v12() {
        let mut db = Db::open_in_memory().unwrap();
        // Seed pre-existing data that must survive the upgrade.
        db.upsert_file("notes/plan.md", "text", 12, 1000, "indexed", None, "hybrid search plan")
            .unwrap();
        // Rewind this connection to a genuine v11 database: drop the v12
        // artifacts and reset the stored schema_version so migrate() re-runs the
        // v11 -> v12 step exactly as it would on a real old index.
        db.conn.execute_batch("DROP TABLE IF EXISTS chunks;").unwrap();
        db.conn
            .execute(
                "INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', '11')",
                [],
            )
            .unwrap();

        db.migrate().unwrap();

        // Version advanced past 12, to the current schema.
        let version: String = db
            .conn
            .query_row("SELECT value FROM meta WHERE key='schema_version'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION.to_string());
        // Pre-existing file survived the migration.
        let surviving: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE rel_path='notes/plan.md'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(surviving, 1, "existing data must survive v11->v12");
        // chunks table exists and starts empty.
        let chunks: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(chunks, 0, "new chunks table starts empty");
        // The `tier` column exists and defaults to 0 (kenignore 1.4 schema half).
        db.conn
            .execute(
                "INSERT INTO chunks(path, seq, text, token_est, content_hash) \
                 VALUES ('a.md', 0, 'x', 1, 'h')",
                [],
            )
            .unwrap();
        let tier: i64 = db
            .conn
            .query_row("SELECT tier FROM chunks WHERE path='a.md'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(tier, 0, "tier defaults to 0");
    }

    #[test]
    fn migration_succeeds_when_vec_unavailable() {
        // Simulate sqlite-vec failing to register (vec_available = false). The
        // v12 migration must NOT depend on the extension — `vec_chunks` is built
        // lazily — so a fresh DB still migrates cleanly and gets the relational
        // `chunks` table, and the KNN paths degrade to graceful no-ops.
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        let db = Db {
            conn,
            vec_available: false,
        };

        db.migrate().unwrap();

        let chunks: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(chunks, 0, "chunks table exists despite vec unavailable");
        // ensure_vec_chunks is a no-op (returns false) when vec is unavailable.
        assert!(
            !db.ensure_vec_chunks(8).unwrap(),
            "ensure_vec_chunks must no-op when vec is unavailable"
        );
    }

    #[test]
    fn ensure_vec_chunks_idempotent() {
        let db = Db::open_in_memory().unwrap();
        if !db.vec_available() {
            // No usable vec0 in this build: the only guarantee is a graceful
            // no-op. (Statically linked, this branch shouldn't be taken.)
            assert!(!db.ensure_vec_chunks(8).unwrap());
            return;
        }
        assert!(db.ensure_vec_chunks(8).unwrap(), "first create succeeds");
        // Repeat calls with the same dim are cheap no-ops, not errors.
        assert!(db.ensure_vec_chunks(8).unwrap(), "second call is idempotent");
        assert!(db.ensure_vec_chunks(8).unwrap(), "third call is idempotent");
        let tables: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name='vec_chunks'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(tables >= 1, "vec_chunks virtual table exists");
    }

    // --- semantic-index task 1.6: chunk CRUD ---

    fn chunk(seq: usize, text: &str, hash: &str) -> crate::chunker::Chunk {
        crate::chunker::Chunk {
            seq,
            text: text.to_string(),
            token_est: text.split_whitespace().count(),
            content_hash: hash.to_string(),
            line: seq * 10 + 1,
        }
    }

    #[test]
    fn a_chunk_keeps_its_line_and_it_moves_when_text_is_added_above() {
        let mut db = Db::open_in_memory().unwrap();
        db.upsert_chunks("a.md", &[chunk(0, "one", "h0"), chunk(1, "two", "h1")], crate::kenignore::Tier::Full)
            .unwrap();
        let id: i64 = db
            .conn
            .query_row("SELECT id FROM chunks WHERE path='a.md' AND seq=1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(db.chunk_line(id).unwrap(), Some(11));
        let mut moved = chunk(1, "two", "h1");
        moved.line = 40;
        let changed = db
            .upsert_chunks("a.md", &[chunk(0, "one", "h0"), moved], crate::kenignore::Tier::Full)
            .unwrap();
        assert!(changed.is_empty(), "same text: nothing to re-embed");
        assert_eq!(db.chunk_line(id).unwrap(), Some(40));
        assert_eq!(db.chunk_line(9999).unwrap(), None);
    }

    #[test]
    fn upsert_chunks_inserts_and_reports_changed() {
        let mut db = Db::open_in_memory().unwrap();
        let chunks = vec![chunk(0, "hello world", "h0"), chunk(1, "second chunk", "h1")];

        let changed = db
            .upsert_chunks("a.md", &chunks, crate::kenignore::Tier::Full)
            .unwrap();
        assert_eq!(changed.len(), 2, "both new chunks reported as changed");

        let count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM chunks WHERE path='a.md'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);

        // chunks_fts got a mirrored row for each chunk id.
        let fts_count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM chunks_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fts_count, 2, "chunks_fts mirrors every chunk");
    }

    // --- task 1.10: FTS index augmentation (path/filename/symbol header) ---

    #[test]
    fn extract_symbol_names_recognizes_common_declaration_keywords() {
        let text = "pub fn frobnicate() {}\nclass Widget:\n    def handle(self):\n        pass\nexport function makeThing() {}\n";
        let names = extract_symbol_names(text);
        assert!(names.contains(&"frobnicate".to_string()), "{names:?}");
        assert!(names.contains(&"Widget".to_string()), "{names:?}");
        assert!(names.contains(&"handle".to_string()), "{names:?}");
        assert!(names.contains(&"makeThing".to_string()), "{names:?}");
    }

    #[test]
    fn java_declarations_give_names_not_return_types() {
        let text = "public enum StashTab {\n    ITEMS;\n    public static final short MAX_ROWS = 7;\n    private final Map<String, List<Row>> rows = new HashMap<>();\n    public static boolean isOpen(int x) {\n    public final class Inner {\n    public void close() {\n    public record Slot(int i) {}\n";
        let names = extract_symbol_names(text);
        for want in ["StashTab", "MAX_ROWS", "rows", "isOpen", "Inner", "close", "Slot"] {
            assert!(names.contains(&want.to_string()), "{want} in {names:?}");
        }
        for junk in ["void", "boolean", "final", "short", "static"] {
            assert!(!names.contains(&junk.to_string()), "{junk} in {names:?}");
        }
        // Rust and TypeScript still read as before.
        let names = extract_symbol_names("pub static mut COUNT: u32 = 0;\npub(crate) struct Store;\n  private readonly cache: Map<string, number>;\n");
        for want in ["COUNT", "Store", "cache"] {
            assert!(names.contains(&want.to_string()), "{want} in {names:?}");
        }
    }

    #[test]
    fn fts_names_split_compound_names_and_keep_them_whole() {
        let names = fts_names("data/equipment/WeaponBases.json", "{ \"CritMultiplier\": 1.5, \"MAX_ROWS\": 7, \"plain\": \"aGVsbG8gd29ybGQgdGhpcyBpcyBiYXNlNjQgZW5jb2RlZCBkYXRhIHRoYXQgaXMgbG9uZw\" }");
        let words: Vec<&str> = names.split(' ').collect();
        for want in ["data", "equipment", "WeaponBases", "Weapon", "Bases", "bas", "json", "Crit", "Multiplier", "MAX", "ROWS", "row"] {
            assert!(words.contains(&want), "{want} in {names}");
        }
        assert!(!words.contains(&"plain"), "a word with no parts stays in the text column only: {names}");
        assert!(!names.contains("GVsb"), "a long base64 run is not split: {names}");
        assert_eq!(words.len(), words.iter().map(|w| w.to_lowercase()).collect::<std::collections::HashSet<_>>().len(), "distinct words: {names}");
        assert_eq!(name_parts("Sword_T1_Stone"), vec!["Sword", "T1", "Stone"]);
        assert_eq!(name_parts("HTTPServer2Config"), vec!["HTTP", "Server", "Config"]);
        assert!(name_parts("plain").is_empty());
    }

    #[test]
    fn split_names_and_stems_find_a_data_file_by_its_words() {
        let mut db = Db::open_in_memory().unwrap();
        db.upsert_chunks("data/equipment/WeaponBases.json", &[chunk(0, "{ \"Bases\": { \"Sword_T1\": { \"MinLow\": 4 } } }", "h0")], crate::kenignore::Tier::Full)
            .unwrap();
        db.upsert_chunks("docs/notes.md", &[chunk(0, "Nothing about arms here.", "h1")], crate::kenignore::Tier::Full)
            .unwrap();
        for q in ["weapon bases", "base weapon", "WeaponBases", "weapon base damage ranges"] {
            let hits = db.search_chunks_fts(q, 5).unwrap();
            assert_eq!(hits.first().map(|h| h.path.as_str()), Some("data/equipment/WeaponBases.json"), "{q}: {hits:?}");
        }
        // The text column is the chunk's own text, nothing added.
        let text: String = db.conn.query_row("SELECT text FROM chunks_fts WHERE rowid = 1", [], |r| r.get(0)).unwrap();
        assert!(text.starts_with("{ \"Bases\""), "{text}");
    }

    #[test]
    fn chunk_queries_add_each_words_stem() {
        let t = |s: &[&str]| s.iter().map(|w| w.to_string()).collect::<Vec<_>>();
        assert_eq!(build_chunk_query(&t(&["weapon", "bases"]), false), "\"weapon\" AND (\"bases\" * OR \"bas\")");
        assert_eq!(build_chunk_query(&t(&["Ranges", "x"]), true), "(\"Ranges\" OR \"rang\") OR \"x\"");
    }

    #[test]
    fn schema_16_rebuilds_the_keyword_index_and_marks_files_to_read_again() {
        let mut db = Db::open_in_memory().unwrap();
        for (path, status) in [("a/WeaponBases.json", "indexed"), ("src/Stash.java", "indexed"), ("notes.md", "indexed"), ("x.ts", "metadata_only")] {
            db.upsert_file(path, "code", 1, 5, status, None, "x").unwrap();
            db.set_file_byte_hash(path, "abc", 5).unwrap();
        }
        db.upsert_chunks("a/WeaponBases.json", &[chunk(0, "{ \"MinLow\": 4 }", "h0")], crate::kenignore::Tier::Full)
            .unwrap();
        // An index from before: the old one-column table, filled the old way.
        db.conn
            .execute_batch(
                "DROP TABLE chunks_fts; CREATE VIRTUAL TABLE chunks_fts USING fts5(text);
                 INSERT INTO chunks_fts(rowid, text) SELECT id, text FROM chunks;
                 UPDATE meta SET value = '15' WHERE key = 'schema_version';",
            )
            .unwrap();
        db.migrate().unwrap();
        assert_eq!(db.search_chunks_fts("weapon bases", 5).unwrap().len(), 1, "filled again from the stored chunks");
        let names: String = db.conn.query_row("SELECT names FROM chunks_fts", [], |r| r.get(0)).unwrap();
        assert!(names.contains("Weapon") && names.contains("Min"), "{names}");
        // notes.md is read again too, by schema 17's step.
        for (path, again) in [("a/WeaponBases.json", true), ("src/Stash.java", true), ("notes.md", true), ("x.ts", false)] {
            let row = db.get_file(path).unwrap().unwrap();
            assert_eq!(row.mtime == -1, again, "{path}");
            assert_eq!(db.file_byte_hash(path).unwrap().is_none(), again, "{path}");
        }
    }

    #[test]
    fn a_possessive_or_contraction_ending_is_not_a_word_of_its_own() {
        assert_eq!(query_tokens("the base game's assets jar"), vec!["the", "base", "game", "assets", "jar"]);
        assert_eq!(query_tokens("Why doesn't a chest come back?"), vec!["Why", "doesn", "a", "chest", "come", "back"]);
        assert_eq!(query_tokens("a party’s trip"), vec!["a", "party", "trip"]);
        assert_eq!(query_tokens("O'Brien's 'quoted' notes"), vec!["O", "Brien", "quoted", "notes"]);
        assert_eq!(query_tokens("auth.rs::validate"), vec!["auth", "rs", "validate"]);
    }

    #[test]
    fn keyword_search_returns_files_not_sections_of_one_file() {
        let mut db = Db::open_in_memory().unwrap();
        let sections: Vec<_> = (0..6).map(|i| chunk(i, &format!("the crossbow keeps its bolts loaded, case {i}"), &format!("t{i}"))).collect();
        db.upsert_chunks("src/test/AmmoKeepDecisionTest.java", &sections, crate::kenignore::Tier::Full).unwrap();
        db.upsert_chunks("src/main/AmmoKeep.java", &[chunk(0, "bolts stay loaded when the crossbow is put away", "a0")], crate::kenignore::Tier::Full)
            .unwrap();
        let hits = db.search_chunks_fts("crossbow bolts loaded", 2).unwrap();
        let paths: Vec<&str> = hits.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths.len(), 2, "{paths:?}");
        assert!(paths.contains(&"src/main/AmmoKeep.java"), "{paths:?}");
    }

    #[test]
    fn a_plain_words_question_weighs_names_less_than_the_text() {
        let mut db = Db::open_in_memory().unwrap();
        // Names that hold three of the words; a text that holds five.
        db.upsert_chunks("src/chest/LootedWorldChest.java", &[chunk(0, "class LootedWorldChest { int x; }", "n0")], crate::kenignore::Tier::Full)
            .unwrap();
        db.upsert_chunks(
            "src/chest/PoolRespawn.java",
            &[chunk(0, "a chest emptied this tick comes back on a later tick, never the next one", "t0")],
            crate::kenignore::Tier::Full,
        )
        .unwrap();
        for i in 0..20 {
            db.upsert_chunks(&format!("src/other/Filler{i}.java"), &[chunk(0, &format!("unrelated code number {i}"), "f")], crate::kenignore::Tier::Full)
                .unwrap();
        }
        // No chunk has every word, so the any-word search ranks them.
        let hits = db.search_chunks_fts("why doesn't a looted world chest come back on the next tick", 2).unwrap();
        assert_eq!(hits.first().map(|h| h.path.as_str()), Some("src/chest/PoolRespawn.java"), "{hits:?}");
    }

    #[test]
    fn schema_17_marks_prose_files_to_read_again() {
        let mut db = Db::open_in_memory().unwrap();
        for (path, status) in [("decisions/Cited-in-Code.md", "indexed"), ("logs/boot.txt", "indexed"), ("src/Stash.java", "indexed"), ("old.md", "metadata_only")] {
            db.upsert_file(path, "code", 1, 5, status, None, "x").unwrap();
            db.set_file_byte_hash(path, "abc", 5).unwrap();
        }
        db.conn.execute("UPDATE meta SET value = '16' WHERE key = 'schema_version'", []).unwrap();
        db.migrate().unwrap();
        for (path, again) in [("decisions/Cited-in-Code.md", true), ("logs/boot.txt", true), ("src/Stash.java", false), ("old.md", false)] {
            assert_eq!(db.get_file(path).unwrap().unwrap().mtime == -1, again, "{path}");
            assert_eq!(db.file_byte_hash(path).unwrap().is_none(), again, "{path}");
        }
    }

    fn decisions_log(n: usize) -> String {
        let mut text = String::from("# DECISIONS\n\nThe record of every ruling.\n\n## THE LOG\n\n### 2026-10-06\n\n");
        for i in (1..=n).rev() {
            text.push_str(&format!("**D-{i:03}** · 2026-10-06 · topic — **RULING NUMBER {i} ABOUT WIDGET {i}.** Chris, in chat. The body of ruling {i}.\n\n"));
        }
        text
    }

    #[test]
    fn schema_18_marks_log_pages_to_read_again() {
        let mut db = Db::open_in_memory().unwrap();
        for (path, text) in [("decisions/DECISIONS.md", decisions_log(8)), ("notes.md", "# Notes\n\n**D-001** once.\n".to_string())] {
            db.upsert_file(path, "md", 1, 5, "indexed", None, &text).unwrap();
            db.set_file_byte_hash(path, "abc", 5).unwrap();
        }
        db.conn.execute("UPDATE meta SET value = '17' WHERE key = 'schema_version'", []).unwrap();
        db.migrate().unwrap();
        for (path, again) in [("decisions/DECISIONS.md", true), ("notes.md", false)] {
            assert_eq!(db.get_file(path).unwrap().unwrap().mtime == -1, again, "{path}");
        }
    }

    /// A ruling's id and its title's words name its chunk: "D-410" finds
    /// D-410, not the ruling packed beside it.
    #[test]
    fn a_log_entry_is_found_by_its_id_and_title() {
        let mut db = Db::open_in_memory().unwrap();
        let text = decisions_log(12);
        let chunks = crate::chunker::chunk_file("decisions/DECISIONS.md", &text, &crate::chunker::IndexProfile::default_for("x.md"));
        db.upsert_chunks("decisions/DECISIONS.md", &chunks, crate::kenignore::Tier::Full).unwrap();
        let names: String = db.conn.query_row("SELECT names FROM chunks_fts WHERE text LIKE '%**D-007**%'", [], |r| r.get(0)).unwrap();
        for want in ["D007", "007", "RULING", "WIDGET"] {
            assert!(names.split(' ').any(|w| w == want), "{want} in {names}");
        }
        let hit = db.search_chunks_fts("D-007", 5).unwrap().into_iter().next().unwrap();
        assert!(hit.text.contains("**D-007**") && !hit.text.contains("**D-008**"), "{}", hit.text);
    }

    #[test]
    fn fts_augmented_header_finds_chunk_by_filename_even_when_body_lacks_the_term() {
        let mut db = Db::open_in_memory().unwrap();
        let chunks = vec![chunk(
            0,
            "this passage never spells out the module by name",
            "h0",
        )];
        db.upsert_chunks("src/local_llm.rs", &chunks, crate::kenignore::Tier::Full)
            .unwrap();

        let hits = db.search_chunks_fts("local_llm", 10).unwrap();
        assert!(
            hits.iter().any(|h| h.path == "src/local_llm.rs"),
            "expected filename-derived tokens to make the chunk findable, got {hits:?}"
        );
    }

    #[test]
    fn upsert_chunks_leaves_chunks_text_unaugmented_for_embeddings() {
        let mut db = Db::open_in_memory().unwrap();
        let plain = "fn frobnicate() { /* body */ }";
        let chunks = vec![chunk(0, plain, "h0")];
        db.upsert_chunks("src/util.rs", &chunks, crate::kenignore::Tier::Full)
            .unwrap();

        let stored: String = db
            .conn
            .query_row("SELECT text FROM chunks WHERE path='src/util.rs'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            stored, plain,
            "chunks.text (the embedding input) must stay the plain chunk text, not the FTS-augmented header"
        );
    }

    #[test]
    fn upsert_chunks_is_diff_based() {
        let mut db = Db::open_in_memory().unwrap();
        let first = vec![chunk(0, "hello world", "h0"), chunk(1, "second chunk", "h1")];
        let ids1 = db
            .upsert_chunks("a.md", &first, crate::kenignore::Tier::Full)
            .unwrap();
        assert_eq!(ids1.len(), 2);
        let id_seq0 = ids1[0].0;

        // Re-upsert with seq 0 unchanged (same hash) and seq 1 changed (new hash).
        let second = vec![chunk(0, "hello world", "h0"), chunk(1, "second chunk EDITED", "h1-new")];
        let changed = db
            .upsert_chunks("a.md", &second, crate::kenignore::Tier::Full)
            .unwrap();
        assert_eq!(changed.len(), 1, "only the changed chunk should be reported");

        // seq 0's row (and its id) must be untouched.
        let id_after: i64 = db
            .conn
            .query_row(
                "SELECT id FROM chunks WHERE path='a.md' AND seq=0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(id_after, id_seq0, "unchanged chunk keeps its id");

        let text1: String = db
            .conn
            .query_row(
                "SELECT text FROM chunks WHERE path='a.md' AND seq=1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(text1, "second chunk EDITED");
    }

    #[test]
    fn upsert_chunks_removes_stale_seqs() {
        let mut db = Db::open_in_memory().unwrap();
        let first = vec![chunk(0, "a", "h0"), chunk(1, "b", "h1"), chunk(2, "c", "h2")];
        db.upsert_chunks("a.md", &first, crate::kenignore::Tier::Full)
            .unwrap();

        // Re-chunk down to a single chunk: seq 1 and 2 must disappear from
        // `chunks` and their `chunks_fts` mirror rows.
        let second = vec![chunk(0, "a", "h0")];
        db.upsert_chunks("a.md", &second, crate::kenignore::Tier::Full)
            .unwrap();

        let count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM chunks WHERE path='a.md'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "stale seqs removed from chunks");

        let fts_count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM chunks_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fts_count, 1, "stale seqs removed from chunks_fts too");
    }

    #[test]
    fn delete_chunks_removes_all_tables() {
        let mut db = Db::open_in_memory().unwrap();
        let chunks = vec![chunk(0, "a", "h0"), chunk(1, "b", "h1")];
        let ids = db
            .upsert_chunks("a.md", &chunks, crate::kenignore::Tier::Full)
            .unwrap();
        if db.vec_available() {
            db.ensure_vec_chunks(8).unwrap();
            let vecs: Vec<Vec<f32>> = ids.iter().map(|_| vec![0.1f32; 8]).collect();
            let just_ids: Vec<i64> = ids.iter().map(|(id, _)| *id).collect();
            db.store_embeddings(&just_ids, &vecs).unwrap();
        }

        db.delete_chunks("a.md").unwrap();

        let count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM chunks WHERE path='a.md'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
        let fts_count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM chunks_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fts_count, 0, "chunks_fts cleaned up too");
        if db.vec_available() {
            let vec_count: i64 = db
                .conn
                .query_row("SELECT COUNT(*) FROM vec_chunks", [], |r| r.get(0))
                .unwrap();
            assert_eq!(vec_count, 0, "vec_chunks cleaned up too");
        }
    }

    #[test]
    fn store_embeddings_and_semantic_search_roundtrip() {
        let mut db = Db::open_in_memory().unwrap();
        if !db.vec_available() {
            // No usable vec0 in this build: store_embeddings/semantic_search
            // must degrade to graceful no-ops rather than erroring.
            let changed = db
                .upsert_chunks("a.md", &[chunk(0, "hello", "h0")], crate::kenignore::Tier::Full)
                .unwrap();
            let ids: Vec<i64> = changed.iter().map(|(id, _)| *id).collect();
            db.store_embeddings(&ids, &[vec![0.1f32; 8]]).unwrap();
            assert!(db.semantic_search(&[0.1f32; 8], 5).unwrap().is_empty());
            return;
        }

        db.ensure_vec_chunks(8).unwrap();
        let chunks = vec![
            chunk(0, "the quick brown fox", "h0"),
            chunk(1, "a totally different sentence", "h1"),
        ];
        let changed = db
            .upsert_chunks("a.md", &chunks, crate::kenignore::Tier::Full)
            .unwrap();
        let ids: Vec<i64> = changed.iter().map(|(id, _)| *id).collect();

        // Chunk 0 gets a vector near the query; chunk 1 gets a far one.
        let near = vec![1.0f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let far = vec![0.0f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0];
        db.store_embeddings(&ids, &[near.clone(), far]).unwrap();

        let results = db.semantic_search(&near, 2).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, ids[0], "closest vector ranks first");
        assert_eq!(results[0].1, "a.md");
        assert_eq!(results[0].2, "the quick brown fox");
    }

    #[test]
    fn semantic_search_no_ops_when_vec_chunks_absent() {
        // ensure_vec_chunks was never called, so vec_chunks doesn't exist yet
        // even though vec0 itself might be available.
        let db = Db::open_in_memory().unwrap();
        let results = db.semantic_search(&[0.1f32; 8], 5).unwrap();
        assert!(results.is_empty(), "no vec_chunks table means no results, not an error");
    }

    #[test]
    fn runs_carry_kind_and_default_to_ingest() {
        let mut db = Db::open_in_memory().unwrap();
        // Legacy insert path defaults to "ingest".
        let a = db.insert_run("people", Some("s-a"), 100).unwrap();
        assert_eq!(db.get_run(a).unwrap().unwrap().kind, "ingest");
        // Explicit-kind insert records an automation run.
        let b = db.insert_run_kind("weekly-jira", Some("s-b"), 200, "automation").unwrap();
        assert_eq!(db.get_run(b).unwrap().unwrap().kind, "automation");
        // list_runs_of_kind filters by kind even when slugs would otherwise mix.
        let auto = db.list_runs_of_kind("weekly-jira", "automation", 10).unwrap();
        assert_eq!(auto.len(), 1);
        assert!(db.list_runs_of_kind("weekly-jira", "ingest", 10).unwrap().is_empty());
    }

    #[test]
    fn merge_dedups_entities_unions_sources_keeps_longer_summary() {
        let mut db = Db::open_in_memory().unwrap();
        // File 1: Priya (short summary), edge Priya→Billing.
        let mut a = ent("person", "Priya N.", "Owns it.");
        a.connections = vec![(1, "owns".into())];
        let d1 = Extraction {
            entities: vec![a, ent("topic", "Billing cutover", "")],
            events: vec![],
        };
        db.merge_knowledge_delta("f1.md", &d1, 10).unwrap();

        // File 2: same Priya (normalized name, longer summary), different file.
        let d2 = Extraction {
            entities: vec![ent("person", "priya   n.", "Owns the whole billing cutover programme.")],
            events: vec![],
        };
        db.merge_knowledge_delta("f2.md", &d2, 20).unwrap();

        let (entities, edges) = db.list_entities_with_edges().unwrap();
        assert_eq!(entities.len(), 2, "Priya deduped across files");
        let priya = entities.iter().find(|e| e.kind == "person").unwrap();
        assert_eq!(priya.summary, "Owns the whole billing cutover programme.");
        let mut srcs = priya.sources.clone();
        srcs.sort();
        assert_eq!(srcs, vec!["f1.md".to_string(), "f2.md".to_string()]);
        assert_eq!(edges.len(), 1, "edge preserved, not duplicated");
    }

    #[test]
    fn merge_dedups_edges_first_label_wins() {
        let mut db = Db::open_in_memory().unwrap();
        let mk = |label: &str| {
            let mut a = ent("person", "A", "");
            a.connections = vec![(1, label.into())];
            Extraction { entities: vec![a, ent("person", "B", "")], events: vec![] }
        };
        db.merge_knowledge_delta("f1.md", &mk("knows"), 10).unwrap();
        db.merge_knowledge_delta("f2.md", &mk("later label"), 20).unwrap();
        let (_, edges) = db.list_entities_with_edges().unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].label, "knows", "first label wins");
    }

    #[test]
    fn merge_dedups_events_by_date_and_text() {
        let mut db = Db::open_in_memory().unwrap();
        let ev = || Extraction {
            entities: vec![],
            events: vec![EventInput {
                date: "2026-07-11".into(),
                category: "decision".into(),
                text: "Sign-off.".into(),
                source: String::new(),
            }],
        };
        db.merge_knowledge_delta("f1.md", &ev(), 10).unwrap();
        db.merge_knowledge_delta("f2.md", &ev(), 20).unwrap();
        let events = db.list_events().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].source, "f1.md", "first occurrence kept");
    }

    #[test]
    fn re_extraction_removes_the_file_then_gcs_orphans() {
        let mut db = Db::open_in_memory().unwrap();
        // f1 grounds Priya + Billing + their edge + an event.
        let mut a = ent("person", "Priya N.", "Owns it.");
        a.connections = vec![(1, "owns".into())];
        db.merge_knowledge_delta("f1.md", &Extraction {
            entities: vec![a, ent("topic", "Billing cutover", "")],
            events: vec![EventInput {
                date: "2026-07-11".into(), category: "decision".into(),
                text: "Sign-off.".into(), source: String::new(),
            }],
        }, 10).unwrap();
        // f2 also grounds Priya (so she survives) but not Billing.
        db.merge_knowledge_delta("f2.md", &Extraction {
            entities: vec![ent("person", "Priya N.", "Owns it.")],
            events: vec![],
        }, 20).unwrap();

        // Re-extract f1 with an EMPTY delta (the file lost all its content).
        db.merge_knowledge_delta("f1.md", &Extraction::default(), 30).unwrap();

        let (entities, edges) = db.list_entities_with_edges().unwrap();
        // Priya survives (grounded by f2); Billing GC'd (only f1 grounded it);
        // the edge cascades away with Billing; the event (only f1) is gone.
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].name, "Priya N.");
        assert_eq!(entities[0].sources, vec!["f2.md".to_string()]);
        assert!(edges.is_empty(), "orphaned edge cascaded");
        assert!(db.list_events().unwrap().is_empty(), "f1's event removed");
    }

    #[test]
    fn purge_file_knowledge_strips_a_removed_file() {
        let mut db = Db::open_in_memory().unwrap();
        db.merge_knowledge_delta("gone.md", &Extraction {
            entities: vec![ent("person", "Solo", "Only here.")],
            events: vec![EventInput {
                date: "2026-07-11".into(), category: "x".into(),
                text: "e".into(), source: String::new(),
            }],
        }, 10).unwrap();
        db.purge_file_knowledge("gone.md").unwrap();
        let (entities, _) = db.list_entities_with_edges().unwrap();
        assert!(entities.is_empty());
        assert!(db.list_events().unwrap().is_empty());
    }

    /// An index made before the code map gets one from its stored text, even
    /// when a scan since has mapped a file or two, and only once.
    #[test]
    fn the_code_map_backfills_every_unmapped_file_once() {
        let mut db = Db::open_in_memory().unwrap();
        for (p, t) in [("a.py", "def alpha():\n    beta()\n"), ("b.py", "def beta():\n    pass\n"), ("c.md", "# not code")] {
            db.upsert_file(p, "code", 1, 1, "indexed", None, t).unwrap();
        }
        let a = crate::codemap::map_file("a.py", "def alpha():\n    beta()\n").unwrap();
        db.set_code_map("a.py", &a).unwrap(); // a scan re-read this one
        assert_eq!(db.backfill_code_map().unwrap(), 1, "b.py, not the mapped one or the page");
        assert_eq!(db.code_symbols(Some("beta"), None, Some(true), 5).unwrap().len(), 1);
        assert_eq!(db.backfill_code_map().unwrap(), 0, "once");
    }

    /// A whole-project build settles the queue it read, but a file edited
    /// while it ran (a new hash) stays waiting for the per-file worker.
    #[test]
    fn a_build_settles_what_it_read_and_nothing_edited_since() {
        let mut db = Db::open_in_memory().unwrap();
        for f in ["a.md", "b.md"] {
            db.upsert_file(f, "md", 1, 1, "indexed", None, "text").unwrap();
            db.enqueue_extraction_if_changed(f, "h1").unwrap();
        }
        let waiting = db.waiting_extractions().unwrap();
        assert_eq!(waiting.len(), 2);
        db.enqueue_extraction_if_changed("b.md", "h2").unwrap(); // edited mid-build
        assert_eq!(db.settle_extractions(&waiting, 5).unwrap(), 1);
        assert_eq!(db.next_pending_extraction().unwrap(), Some(("b.md".into(), "h2".into())));
        assert_eq!(db.entity_count().unwrap(), 0);
    }

    #[test]
    fn removing_a_file_purges_its_knowledge_and_queue_row() {
        let mut db = Db::open_in_memory().unwrap();
        db.upsert_file("gone.md", "md", 1, 1, "indexed", None, "solo content").unwrap();
        db.enqueue_extraction_if_changed("gone.md", "h1").unwrap();
        db.merge_knowledge_delta("gone.md", &Extraction {
            entities: vec![ent("person", "Solo", "Only here.")],
            events: vec![],
        }, 10).unwrap();

        db.remove_file("gone.md").unwrap();

        assert!(db.list_entities_with_edges().unwrap().0.is_empty());
        assert_eq!(db.extraction_coverage().unwrap(), (0, 0));
        assert!(db.next_pending_extraction().unwrap().is_none());
    }

    #[test]
    fn ocr_queue_enqueue_next_and_done_makes_text_searchable() {
        let mut db = Db::open_in_memory().unwrap();
        // An image indexes as metadata-only with no content — not searchable yet.
        db.upsert_file("scans/receipt.png", "image", 500, 1, "metadata_only", None, "")
            .unwrap();
        assert!(db.search("espresso", 5).unwrap().is_empty());

        assert!(db.enqueue_ocr_if_changed("scans/receipt.png", "h1").unwrap());
        assert_eq!(
            db.next_pending_ocr().unwrap(),
            Some(("scans/receipt.png".into(), "h1".into()))
        );

        db.mark_ocr_done(
            "scans/receipt.png", "h1",
            &[
                OcrRegionRow { page: 0, text: "espresso machine".into(), bbox: [0.1, 0.2, 0.3, 0.05] },
                OcrRegionRow { page: 0, text: "total 42.00".into(), bbox: [0.1, 0.3, 0.3, 0.05] },
            ],
        )
        .unwrap();

        // Queue drained, regions stored, and the OCR text is now searchable.
        assert!(db.next_pending_ocr().unwrap().is_none());
        let regions = db.get_ocr_regions("scans/receipt.png").unwrap();
        assert_eq!(regions.len(), 2);
        assert_eq!(regions[0].text, "espresso machine");
        assert_eq!(regions[0].bbox, [0.1, 0.2, 0.3, 0.05]);
        assert!(db
            .search("espresso", 5)
            .unwrap()
            .iter()
            .any(|h| h.rel_path == "scans/receipt.png"));
    }

    #[test]
    fn ocr_enqueue_skips_unchanged_but_requeues_on_change() {
        let mut db = Db::open_in_memory().unwrap();
        db.upsert_file("a.png", "image", 1, 1, "metadata_only", None, "").unwrap();
        assert!(db.enqueue_ocr_if_changed("a.png", "h1").unwrap());
        db.mark_ocr_done("a.png", "h1", &[]).unwrap();
        // Same hash after completion: no re-OCR.
        assert!(!db.enqueue_ocr_if_changed("a.png", "h1").unwrap());
        assert!(db.next_pending_ocr().unwrap().is_none());
        // A new hash (file changed): re-queued.
        assert!(db.enqueue_ocr_if_changed("a.png", "h2").unwrap());
        assert_eq!(db.next_pending_ocr().unwrap(), Some(("a.png".into(), "h2".into())));
    }

    #[test]
    fn ocr_merges_onto_existing_pdf_text_without_double_appending() {
        let mut db = Db::open_in_memory().unwrap();
        // A scanned PDF whose text layer is sparse but non-empty.
        db.upsert_file("doc.pdf", "pdf", 1, 1, "indexed", None, "page 1").unwrap();
        db.enqueue_ocr_if_changed("doc.pdf", "h1").unwrap();
        db.mark_ocr_done(
            "doc.pdf", "h1",
            &[OcrRegionRow { page: 0, text: "handwritten clause".into(), bbox: [0.0, 0.0, 1.0, 0.1] }],
        )
        .unwrap();
        let text = db.get_text("doc.pdf").unwrap().unwrap();
        assert!(text.contains("page 1"), "base text kept: {text}");
        assert!(text.contains("handwritten clause"), "ocr merged: {text}");
        // A duplicate mark at the same (now `done`) hash is a no-op: no double append.
        db.mark_ocr_done(
            "doc.pdf", "h1",
            &[OcrRegionRow { page: 0, text: "handwritten clause".into(), bbox: [0.0, 0.0, 1.0, 0.1] }],
        )
        .unwrap();
        let text2 = db.get_text("doc.pdf").unwrap().unwrap();
        assert_eq!(text, text2, "duplicate done must not re-append");
    }

    #[test]
    fn ocr_error_bounded_retries_then_stays_error() {
        let mut db = Db::open_in_memory().unwrap();
        db.upsert_file("bad.png", "image", 1, 1, "metadata_only", None, "").unwrap();
        db.enqueue_ocr_if_changed("bad.png", "h1").unwrap();
        // Fail up to the cap, re-queuing each time it still has budget.
        let mut errors = 0;
        loop {
            match db.next_pending_ocr().unwrap() {
                Some((rel, hash)) => {
                    db.mark_ocr_error(&rel, &hash, "vision failed").unwrap();
                    errors += 1;
                }
                None => {
                    if db.requeue_errored_ocr().unwrap() == 0 {
                        break;
                    }
                }
            }
            assert!(errors <= MAX_OCR_ATTEMPTS + 1, "runaway retries");
        }
        assert_eq!(errors as i64, MAX_OCR_ATTEMPTS);
        assert!(db.next_pending_ocr().unwrap().is_none());
    }

    #[test]
    fn mark_ocr_done_does_not_clobber_a_re_enqueued_hash() {
        let mut db = Db::open_in_memory().unwrap();
        db.upsert_file("a.png", "image", 1, 1, "metadata_only", None, "").unwrap();
        db.enqueue_ocr_if_changed("a.png", "h1").unwrap();
        // Worker popped h1; meanwhile the scanner re-enqueues h2.
        db.enqueue_ocr_if_changed("a.png", "h2").unwrap();
        // The stale h1 result must NOT flip the fresh h2 row to done.
        db.mark_ocr_done("a.png", "h1", &[]).unwrap();
        assert_eq!(db.next_pending_ocr().unwrap(), Some(("a.png".into(), "h2".into())));
    }

    #[test]
    fn removing_a_file_purges_its_ocr_rows() {
        let mut db = Db::open_in_memory().unwrap();
        db.upsert_file("gone.png", "image", 1, 1, "metadata_only", None, "").unwrap();
        db.enqueue_ocr_if_changed("gone.png", "h1").unwrap();
        db.mark_ocr_done(
            "gone.png", "h1",
            &[OcrRegionRow { page: 0, text: "text".into(), bbox: [0.0, 0.0, 1.0, 1.0] }],
        )
        .unwrap();
        db.remove_file("gone.png").unwrap();
        assert!(db.get_ocr_regions("gone.png").unwrap().is_empty());
        assert!(db.next_pending_ocr().unwrap().is_none());
    }

    /// Files marked done with nothing read (Windows before it had an OCR
    /// backend) are read again once, when an engine is first stamped; files
    /// with regions stay done, and the same engine never re-queues twice.
    #[test]
    fn a_new_ocr_engine_rereads_files_marked_done_without_regions() {
        let mut db = Db::open_in_memory().unwrap();
        for f in ["empty.png", "read.png"] {
            db.upsert_file(f, "image", 1, 1, "metadata_only", None, "").unwrap();
            db.enqueue_ocr_if_changed(f, "h1").unwrap();
        }
        db.mark_ocr_done("empty.png", "h1", &[]).unwrap();
        db.mark_ocr_done("read.png", "h1", &[OcrRegionRow { page: 0, text: "x".into(), bbox: [0.0, 0.0, 1.0, 1.0] }])
            .unwrap();
        assert_eq!(db.requeue_ocr_for_engine("winrt", None).unwrap(), 1);
        assert_eq!(db.next_pending_ocr().unwrap(), Some(("empty.png".into(), "h1".into())));
        db.mark_ocr_done("empty.png", "h1", &[]).unwrap();
        assert_eq!(db.requeue_ocr_for_engine("winrt", None).unwrap(), 0, "once per engine");
        assert!(db.next_pending_ocr().unwrap().is_none());

        // An unstamped index read by the same engine (macOS) is only stamped.
        let mut mac = Db::open_in_memory().unwrap();
        mac.upsert_file("blank.png", "image", 1, 1, "metadata_only", None, "").unwrap();
        mac.enqueue_ocr_if_changed("blank.png", "h1").unwrap();
        mac.mark_ocr_done("blank.png", "h1", &[]).unwrap();
        assert_eq!(mac.requeue_ocr_for_engine("vision", Some("vision")).unwrap(), 0);
        assert_eq!(mac.meta_get("ocr_engine").unwrap().as_deref(), Some("vision"));
    }

    #[test]
    fn errored_then_changed_file_is_not_counted_as_analyzed() {
        let mut db = Db::open_in_memory().unwrap();
        db.upsert_file("b.md", "md", 1, 1, "indexed", None, "beta").unwrap();
        db.enqueue_extraction_if_changed("b.md", "h1").unwrap();
        // Extraction fails — the file was never successfully analyzed.
        db.mark_extraction_error("b.md", "h1", "boom", 101).unwrap();
        assert_eq!(db.extraction_coverage().unwrap(), (0, 1));
        // Content changes and re-enqueues; still never succeeded → still not
        // counted (extracted_at must reflect last SUCCESS only, never an error).
        assert!(db.enqueue_extraction_if_changed("b.md", "h2").unwrap());
        assert_eq!(
            db.extraction_coverage().unwrap(),
            (0, 1),
            "a file that only ever errored must not count as analyzed after re-enqueue"
        );
    }

    #[test]
    fn backfill_extractions_enqueues_indexed_files_without_rows() {
        let mut db = Db::open_in_memory().unwrap();
        // Two indexed files with content, one metadata-only, one already queued.
        db.upsert_file("a.md", "md", 1, 1, "indexed", None, "alpha").unwrap();
        db.upsert_file("b.md", "md", 1, 1, "indexed", None, "beta").unwrap();
        db.upsert_file("c.png", "png", 1, 1, "metadata_only", None, "").unwrap();
        db.upsert_file("d.md", "md", 1, 1, "indexed", None, "delta").unwrap();
        // d already has an extraction row (a normal indexed-after-v8 file).
        db.enqueue_extraction_if_changed("d.md", &crate::knowledge_model::content_hash("delta"))
            .unwrap();
        db.mark_extraction_done("d.md", &crate::knowledge_model::content_hash("delta"), 50)
            .unwrap();

        // Backfill picks up a and b only (c isn't indexed; d already has a row).
        assert_eq!(db.backfill_extractions().unwrap(), 2);
        let mut pending = Vec::new();
        while let Some((rel, hash)) = db.next_pending_extraction().unwrap() {
            db.mark_extraction_done(&rel, &hash, 100).unwrap();
            pending.push(rel);
        }
        pending.sort();
        assert_eq!(pending, vec!["a.md".to_string(), "b.md".to_string()]);

        // Idempotent: a second call enqueues nothing (every file now has a row).
        assert_eq!(db.backfill_extractions().unwrap(), 0);
    }

    #[test]
    fn refresh_stored_kinds_reclassifies_stale_binary_vtt_to_txt() {
        let mut db = Db::open_in_memory().unwrap();
        // A `.vtt` indexed before it was reclassified as text: stored "binary",
        // metadata-only, with no content — exactly the stale-row bug.
        db.upsert_file("m/talk.vtt", "binary", 10, 5, "metadata_only", None, "")
            .unwrap();

        assert_eq!(db.refresh_stored_kinds().unwrap(), 1);

        let row = db.get_file("m/talk.vtt").unwrap().unwrap();
        assert_eq!(row.kind, "txt", "the stale binary kind must be corrected to txt");
    }

    #[test]
    fn refresh_stored_kinds_is_idempotent_and_leaves_correct_rows_untouched() {
        let mut db = Db::open_in_memory().unwrap();
        // Already-correct rows: a content file classified as its true kind, and
        // a genuine binary that must stay binary.
        db.upsert_file("notes/a.md", "md", 1, 5, "indexed", None, "alpha").unwrap();
        db.upsert_file("data/blob.bin", "binary", 1, 5, "metadata_only", None, "").unwrap();

        // No drift → nothing changed.
        assert_eq!(db.refresh_stored_kinds().unwrap(), 0);
        assert_eq!(db.get_file("notes/a.md").unwrap().unwrap().kind, "md");
        assert_eq!(db.get_file("data/blob.bin").unwrap().unwrap().kind, "binary");

        // Second call is a no-op too (idempotent).
        db.upsert_file("m/talk.vtt", "binary", 10, 5, "metadata_only", None, "").unwrap();
        assert_eq!(db.refresh_stored_kinds().unwrap(), 1);
        assert_eq!(db.refresh_stored_kinds().unwrap(), 0);
    }

    #[test]
    fn refresh_stored_kinds_resets_mtime_only_for_content_transitions() {
        let mut db = Db::open_in_memory().unwrap();
        // Non-content → content: mtime must be reset to the sentinel so the next
        // incremental scan sees it as changed and re-runs index_one (re-extract).
        db.upsert_file("m/talk.vtt", "binary", 10, 5, "metadata_only", None, "").unwrap();

        assert_eq!(db.refresh_stored_kinds().unwrap(), 1);
        let row = db.get_file("m/talk.vtt").unwrap().unwrap();
        assert_eq!(row.kind, "txt");
        assert_eq!(
            row.mtime, -1,
            "a non-content→content transition must reset mtime so scan re-indexes it"
        );
    }

    #[test]
    fn refresh_stored_kinds_preserves_mtime_for_non_content_transitions() {
        let mut db = Db::open_in_memory().unwrap();
        // Kind drift that does NOT gain content (content stays present): the
        // glyph updates but there's no need to force a re-index, so mtime stays.
        // Simulate a stored "md" for a path that now classifies as "code".
        db.upsert_file("src/main.rs", "md", 10, 5, "indexed", None, "fn main").unwrap();

        assert_eq!(db.refresh_stored_kinds().unwrap(), 1);
        let row = db.get_file("src/main.rs").unwrap().unwrap();
        assert_eq!(row.kind, "code");
        assert_eq!(
            row.mtime, 5,
            "a content→content reclassification must not disturb mtime (no re-index needed)"
        );
    }

    #[test]
    fn requeue_errored_extractions_returns_error_rows_to_pending() {
        let mut db = Db::open_in_memory().unwrap();
        db.upsert_file("a.md", "md", 1, 1, "indexed", None, "alpha").unwrap();
        db.upsert_file("b.md", "md", 1, 1, "indexed", None, "beta").unwrap();
        db.upsert_file("c.md", "md", 1, 1, "indexed", None, "gamma").unwrap();
        db.enqueue_extraction_if_changed("a.md", "ha").unwrap();
        db.enqueue_extraction_if_changed("b.md", "hb").unwrap();
        db.enqueue_extraction_if_changed("c.md", "hc").unwrap();
        // a done, b errored, c still pending.
        db.mark_extraction_done("a.md", "ha", 100).unwrap();
        db.mark_extraction_error("b.md", "hb", "batch add failed", 101).unwrap();
        // Only the errored row is re-queued; done and pending untouched.
        assert_eq!(db.requeue_errored_extractions().unwrap(), 1);
        // Drain the queue: b (re-queued, hash preserved) and c (already pending)
        // are the only pending rows; a stayed done.
        let mut pending = Vec::new();
        while let Some((rel, hash)) = db.next_pending_extraction().unwrap() {
            db.mark_extraction_done(&rel, &hash, 200).unwrap();
            pending.push((rel, hash));
        }
        assert!(pending.iter().any(|(r, h)| r == "b.md" && h == "hb"),
            "errored file b returned to pending with its hash");
        assert!(pending.iter().any(|(r, _)| r == "c.md"));
        assert!(!pending.iter().any(|(r, _)| r == "a.md"), "a stayed done");
        // Re-queueing again is a no-op now that nothing is errored.
        assert_eq!(db.requeue_errored_extractions().unwrap(), 0);
    }

    #[test]
    fn errored_extraction_is_requeued_up_to_cap_then_stays_error() {
        let mut db = Db::open_in_memory().unwrap();
        db.upsert_file("b.md", "md", 1, 1, "indexed", None, "beta").unwrap();
        db.enqueue_extraction_if_changed("b.md", "hb").unwrap();

        // A deterministically failing file, driven the way the worker does: pop
        // it, mark it error (guarding on its hash), and when the queue drains,
        // rescue errored rows that still have retries left. It must self-heal
        // only up to MAX_EXTRACTION_ATTEMPTS total failures, then be left
        // `error` forever so the queue can never spin.
        let mut requeued = 0;
        for _ in 0..20 {
            match db.next_pending_extraction().unwrap() {
                Some((rel, hash)) => {
                    db.mark_extraction_error(&rel, &hash, "boom", 100).unwrap();
                }
                None => {
                    if db.requeue_errored_extractions().unwrap() == 0 {
                        break; // exhausted the cap — no infinite loop
                    }
                    requeued += 1;
                }
            }
        }
        // First failure sets attempts=1; it was requeued (MAX-1) more times,
        // failing on each, before attempts reached the cap.
        assert_eq!(requeued, (MAX_EXTRACTION_ATTEMPTS - 1) as usize);
        // Exhausted: stays error, never returns to pending, never re-queued.
        assert_eq!(db.next_pending_extraction().unwrap(), None);
        assert_eq!(db.requeue_errored_extractions().unwrap(), 0);
        assert_eq!(db.extraction_coverage().unwrap(), (0, 1));
        assert_eq!(db.index_health().unwrap().failed, 1);

        // An edit gives the new content the full three tries, not one.
        db.enqueue_extraction_if_changed("b.md", "hb-edited").unwrap();
        let attempts: i64 = db
            .conn
            .query_row("SELECT attempts FROM extractions WHERE rel_path = 'b.md'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(attempts, 0);
        let h = db.index_health().unwrap();
        assert_eq!((h.failed, h.pending), (0, 1));
    }

    #[test]
    fn mark_extraction_error_does_not_clobber_a_re_enqueued_hash() {
        let mut db = Db::open_in_memory().unwrap();
        db.upsert_file("a.md", "md", 1, 1, "indexed", None, "alpha").unwrap();
        // The worker popped "h1"; before it reports back, the scanner re-indexes
        // and re-enqueues the file under a NEW hash "h2" (now pending).
        db.enqueue_extraction_if_changed("a.md", "h1").unwrap();
        db.enqueue_extraction_if_changed("a.md", "h2").unwrap();
        // A stale failure for the OLD hash must NOT flip the fresh pending row to
        // error — the mismatched content_hash makes it a harmless no-op.
        db.mark_extraction_error("a.md", "h1", "stale boom", 100).unwrap();
        assert_eq!(
            db.next_pending_extraction().unwrap(),
            Some(("a.md".into(), "h2".into())),
            "the re-enqueued hash stays pending; a stale error is a no-op"
        );
        // The matching hash does record the error.
        db.mark_extraction_error("a.md", "h2", "real boom", 101).unwrap();
        assert_eq!(db.next_pending_extraction().unwrap(), None);
        assert_eq!(db.extraction_coverage().unwrap(), (0, 1));
    }

    #[test]
    fn indexed_file_without_contents_row_does_not_freeze_coverage() {
        let mut db = Db::open_in_memory().unwrap();
        // Two normal indexed files (each gets a contents row via upsert_file).
        db.upsert_file("a.md", "md", 1, 1, "indexed", None, "alpha").unwrap();
        db.upsert_file("b.md", "md", 1, 1, "indexed", None, "beta").unwrap();
        // Simulate a legacy/corrupt state: an `indexed` file with NO contents
        // row — counted by the raw indexed count but impossible to extract.
        db.conn
            .execute(
                "INSERT INTO files (rel_path, kind, size, mtime, status, error)
                 VALUES ('ghost.md', 'md', 1, 1, 'indexed', NULL)",
                [],
            )
            .unwrap();

        // Raw indexed count includes the ghost; the extractable population does
        // not — that is the denominator the coverage line now uses.
        assert_eq!(db.indexed_file_count().unwrap(), 3);
        assert_eq!(db.extractable_file_count().unwrap(), 2);

        // Backfill enqueues exactly the extractable files (a, b), never the
        // ghost, and leaves nothing extractable unqueued.
        assert_eq!(db.backfill_extractions().unwrap(), 2);
        assert_eq!(db.unqueued_extractable_count().unwrap(), 0);

        // Draining the queue reaches the full extractable denominator (2 of 2),
        // NOT 2 of 3 — the un-extractable ghost can't strand it below 100%.
        while let Some((rel, hash)) = db.next_pending_extraction().unwrap() {
            db.mark_extraction_done(&rel, &hash, 100).unwrap();
        }
        assert_eq!(db.extraction_coverage().unwrap(), (2, 2));
    }

    #[test]
    fn extraction_failed_count_counts_only_terminally_errored_rows() {
        let mut db = Db::open_in_memory().unwrap();
        db.upsert_file("a.md", "md", 1, 1, "indexed", None, "alpha").unwrap();
        db.upsert_file("b.md", "md", 1, 1, "indexed", None, "beta").unwrap();
        db.enqueue_extraction_if_changed("a.md", "ha").unwrap();
        db.enqueue_extraction_if_changed("b.md", "hb").unwrap();

        // a errored once — still within the retry budget, so NOT terminally
        // failed (a self-heal requeue may yet make it succeed).
        db.mark_extraction_error("a.md", "ha", "boom", 100).unwrap();
        assert_eq!(
            db.extraction_failed_count().unwrap(),
            0,
            "a row still within its retry budget must not count as failed"
        );

        // b errors until its retry budget is exhausted — now terminally failed.
        for _ in 0..MAX_EXTRACTION_ATTEMPTS {
            db.mark_extraction_error("b.md", "hb", "boom", 100).unwrap();
        }
        assert_eq!(
            db.extraction_failed_count().unwrap(),
            1,
            "only b, which exhausted its retry budget, counts as failed"
        );

        // Boundary check: the exhausted row (b) is never re-queued, but a (still
        // under the cap) is — so failures don't wedge the live queue.
        assert_eq!(db.requeue_errored_extractions().unwrap(), 1);
    }

    #[test]
    fn extraction_queue_tracks_pending_done_and_coverage() {
        let mut db = Db::open_in_memory().unwrap();
        // Two indexed files, one metadata-only (must not count toward total).
        db.upsert_file("a.md", "md", 1, 1, "indexed", None, "alpha").unwrap();
        db.upsert_file("b.md", "md", 1, 1, "indexed", None, "beta").unwrap();
        db.upsert_file("c.png", "png", 1, 1, "metadata_only", None, "").unwrap();

        // New hashes enqueue; the queue drains oldest-first.
        assert!(db.enqueue_extraction_if_changed("a.md", "h1").unwrap());
        assert!(db.enqueue_extraction_if_changed("b.md", "h2").unwrap());
        assert_eq!(db.extraction_coverage().unwrap(), (0, 2));
        assert_eq!(
            db.next_pending_extraction().unwrap(),
            Some(("a.md".into(), "h1".into()))
        );

        // Marking a.md done advances the queue and lifts coverage.
        db.mark_extraction_done("a.md", "h1", 100).unwrap();
        assert_eq!(db.extraction_coverage().unwrap(), (1, 2));
        assert_eq!(
            db.next_pending_extraction().unwrap(),
            Some(("b.md".into(), "h2".into()))
        );

        // Re-enqueuing the same done hash is a no-op; a changed hash re-queues.
        assert!(!db.enqueue_extraction_if_changed("a.md", "h1").unwrap());
        assert!(db.enqueue_extraction_if_changed("a.md", "h1b").unwrap());
        assert_eq!(db.extraction_coverage().unwrap(), (1, 2)); // still 'done' count until re-run overwrites

        // Errors stay out of the pending queue but keep the row.
        db.mark_extraction_error("b.md", "h2", "boom", 101).unwrap();
        db.mark_extraction_done("a.md", "h1b", 102).unwrap();
        assert_eq!(db.next_pending_extraction().unwrap(), None);

        // Removing an extraction row drops it entirely.
        db.remove_extraction("a.md").unwrap();
        assert_eq!(db.extraction_coverage().unwrap(), (0, 2));
    }

    #[test]
    fn mark_extraction_done_does_not_clobber_a_re_enqueued_hash() {
        // The worker pops hash "h1", drops the DB lock, and generates. Meanwhile
        // the scanner re-indexes the file and re-enqueues it under a NEW hash
        // "h2". When the (stale) worker finally marks "h1" done, it must NOT flip
        // the fresher "h2" row to done — the change would be lost until the next
        // scan. The mismatch is a harmless no-op; "h2" stays pending for a re-run.
        let mut db = Db::open_in_memory().unwrap();
        db.upsert_file("a.md", "md", 1, 1, "indexed", None, "alpha").unwrap();
        assert!(db.enqueue_extraction_if_changed("a.md", "h1").unwrap());
        // Scanner re-enqueues under the new hash while the worker is generating.
        assert!(db.enqueue_extraction_if_changed("a.md", "h2").unwrap());
        // The stale worker marks the OLD hash done.
        db.mark_extraction_done("a.md", "h1", 100).unwrap();
        // No-op: the newer hash is still pending and still uncounted.
        assert_eq!(
            db.next_pending_extraction().unwrap(),
            Some(("a.md".into(), "h2".into())),
            "the re-enqueued hash must survive a stale worker's mark-done"
        );
        assert_eq!(db.extraction_coverage().unwrap(), (0, 1));
        // Marking the CURRENT hash done does advance it.
        db.mark_extraction_done("a.md", "h2", 101).unwrap();
        assert_eq!(db.next_pending_extraction().unwrap(), None);
        assert_eq!(db.extraction_coverage().unwrap(), (1, 1));
    }

    #[test]
    fn file_health_counts_count_files_failed_and_waiting() {
        let mut db = Db::open_in_memory().unwrap();
        assert_eq!(db.file_health_counts().unwrap(), FileHealthCounts::default());
        db.upsert_file("a.md", "md", 1, 1, "indexed", None, "alpha").unwrap();
        db.upsert_file("b.pdf", "pdf", 1, 1, "failed", Some("bad pdf"), "").unwrap();
        db.upsert_file("c.png", "image", 1, 1, "metadata_only", None, "").unwrap();
        db.upsert_file("d.md", "md", 1, 1, "pending", None, "").unwrap();
        // OCR backlog is not counted; the extraction backlog is.
        assert!(db.enqueue_ocr_if_changed("c.png", "h1").unwrap());
        assert!(db.enqueue_extraction_if_changed("a.md", "h1").unwrap());
        assert_eq!(db.file_health_counts().unwrap(), FileHealthCounts { total: 4, failed: 1, queued: 2 });
    }

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

    /// A cloud placeholder is indexed with empty text but must remain findable by
    /// name: `upsert_file` writes name tokens into FTS even for contentless rows,
    /// and the query-time rel_path LIKE pass catches mid-word fragments. This
    /// pins the guarantee scan.rs relies on ("Name-searchable; content arrives
    /// when the user opens the file").
    #[test]
    fn cloud_only_rows_are_name_searchable() {
        let mut db = Db::open_in_memory().unwrap();
        db.upsert_file(
            "meetings/Standup Recording 06.15.mp4",
            "video",
            999,
            0,
            crate::scan::STATUS_CLOUD_ONLY,
            None,
            "",
        )
        .unwrap();
        // Whole-token match via the FTS name column.
        let hits = db.search("standup", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].status, crate::scan::STATUS_CLOUD_ONLY);
        // Mid-word fragment via the filename substring pass.
        let hits = db.search("cording", 10).unwrap();
        assert_eq!(hits.len(), 1, "filename LIKE pass must catch mid-word fragments");
    }

    #[test]
    fn search_prefix_as_you_type() {
        let db = seeded();
        let hits = db.search("cutov", 10).unwrap();
        assert!(!hits.is_empty(), "prefix of last token should match");
    }

    #[test]
    fn single_char_query_is_bounded_and_fast() {
        // A 1-char query must never scan/return the whole index. Seed many
        // files that all contain the char in both name and body, then assert the
        // result honors `limit` (both the FTS LIMIT and the filename-scan LIMIT).
        let mut db = Db::open_in_memory().unwrap();
        for i in 0..50 {
            db.upsert_file(
                &format!("area/alpha-{i}.md"),
                "md",
                1,
                1,
                "indexed",
                None,
                "alpha and more alpha",
            )
            .unwrap();
        }
        let hits = db.search("a", 5).unwrap();
        assert!(hits.len() <= 5, "1-char query must respect limit: {}", hits.len());
        assert!(!hits.is_empty(), "1-char query should still return matches");
    }

    fn now_secs() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    #[test]
    fn excerpt_centers_on_match_and_trims_word_boundaries() {
        let mut db = Db::open_in_memory().unwrap();
        let text = "alpha beta gamma delta epsilon zeta eta theta iota kappa \
                    NEEDLE lambda mu nu xi omicron pi rho sigma tau upsilon phi";
        db.upsert_file("doc.md", "md", 1, 1, "indexed", None, text).unwrap();

        let ex = db
            .excerpt("doc.md", &query_tokens("needle"), 40)
            .expect("content present and token matches");
        // Centered on the match (case-insensitive), and short of the full doc.
        assert!(ex.to_lowercase().contains("needle"), "{ex:?}");
        assert!(ex.len() < text.len(), "window must be smaller than the doc: {ex:?}");
        // Match sits in the middle, so both ends are truncated with ellipses.
        assert!(ex.starts_with('…') && ex.ends_with('…'), "{ex:?}");

        // The core (sans ellipses) is a contiguous slice of the source that
        // begins and ends on a word boundary — never mid-word, never mid-char.
        let core = ex.trim_matches('…');
        let idx = text.find(core).expect("core must be a contiguous source slice");
        assert!(idx == 0 || text[..idx].ends_with(char::is_whitespace), "start mid-word: {core:?}");
        let after = idx + core.len();
        assert!(
            after == text.len() || text[after..].starts_with(char::is_whitespace),
            "end mid-word: {core:?}"
        );
    }

    #[test]
    fn excerpt_missing_or_empty_content_returns_none() {
        let mut db = Db::open_in_memory().unwrap();
        // No such file at all.
        assert!(db.excerpt("ghost.md", &query_tokens("anything"), 350).is_none());
        // File exists but has empty stored text (binary / OCR-pending shape).
        db.upsert_file("scan.png", "png", 1, 1, "metadata_only", None, "").unwrap();
        assert!(db.excerpt("scan.png", &query_tokens("anything"), 350).is_none());
        // Content present but no token matches → also None (caller falls back).
        db.upsert_file("note.md", "md", 1, 1, "indexed", None, "just some prose").unwrap();
        assert!(db.excerpt("note.md", &query_tokens("absent"), 350).is_none());
    }

    #[test]
    fn phrase_match_ranks_above_scattered_tokens() {
        let mut db = Db::open_in_memory().unwrap();
        // A: long doc containing the exact phrase "quick brown" (weaker bm25 on
        // its own). B: short doc with both tokens but never adjacent in order.
        db.upsert_file(
            "a.md", "md", 1, 1, "indexed", None,
            "In the morning report the quick brown metric rose sharply across \
             every regional division and subteam over the quarter.",
        )
        .unwrap();
        db.upsert_file("b.md", "md", 1, 1, "indexed", None, "brown then quick").unwrap();
        let hits = db.search("quick brown", 10).unwrap();
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert_eq!(hits[0].rel_path, "a.md", "phrase doc must rank first: {hits:?}");
    }

    #[test]
    fn recency_breaks_ties_between_equal_content_matches() {
        let mut db = Db::open_in_memory().unwrap();
        // Identical content → identical bm25. Names chosen so the alphabetical
        // tie-break would put the *old* file first; recency must override that.
        db.upsert_file("aaa-old.md", "md", 1, 1, "indexed", None, "unicorn stampede").unwrap();
        db.upsert_file("zzz-new.md", "md", 1, now_secs(), "indexed", None, "unicorn stampede")
            .unwrap();
        let hits = db.search("unicorn", 10).unwrap();
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert_eq!(hits[0].rel_path, "zzz-new.md", "recent file must win the tie: {hits:?}");
    }

    #[test]
    fn exact_filename_match_beats_recent_content_regardless_of_recency() {
        let mut db = Db::open_in_memory().unwrap();
        // Old file whose *name* exactly matches the query, unrelated content.
        db.upsert_file("budget.md", "md", 1, 1, "indexed", None, "quarterly figures").unwrap();
        // Fresh file whose *content* matches the query.
        db.upsert_file("notes.md", "md", 1, now_secs(), "indexed", None, "the budget was revised")
            .unwrap();
        let hits = db.search("budget", 10).unwrap();
        assert_eq!(
            hits[0].rel_path, "budget.md",
            "exact filename match must dominate even a brand-new content match: {hits:?}"
        );
        // And its rank stays at the exact-name tier (recency barely perturbs an
        // old file, and never enough to close the gap to content ranks anyway).
        assert!(hits[0].rank <= NAME_RANK_EXACT + 0.001, "{hits:?}");
    }

    #[test]
    fn stopwords_dropped_so_question_form_still_matches() {
        let mut db = Db::open_in_memory().unwrap();
        // Content lacks "what"/"is"/"the" entirely; the pre-stopword AND query
        // would exclude it. After filtering, only the content words remain.
        db.upsert_file(
            "brief.md", "md", 1, 1, "indexed", None,
            "Operating model for our AI pilot program spans several teams.",
        )
        .unwrap();
        let hits = db
            .search("what is the operating model for the ai pilot", 10)
            .unwrap();
        assert!(
            hits.iter().any(|h| h.rel_path == "brief.md"),
            "question-form query should still find the content doc: {hits:?}"
        );
        // The filter keeps exactly the content-bearing tokens (last token kept).
        assert_eq!(
            significant_tokens("what is the operating model for the ai pilot"),
            vec!["operating", "model", "ai", "pilot"],
        );
    }

    #[test]
    fn last_stopword_token_is_never_dropped() {
        let mut db = Db::open_in_memory().unwrap();
        // "the" is a stopword, but as the sole/last token it must survive so its
        // as-you-type prefix still reaches "theme.txt".
        db.upsert_file("theme.txt", "txt", 1, 1, "indexed", None, "themes and motifs").unwrap();
        assert_eq!(significant_tokens("the"), vec!["the"]);
        let hits = db.search("the", 10).unwrap();
        assert!(
            hits.iter().any(|h| h.rel_path == "theme.txt"),
            "sole stopword token must still prefix-match: {hits:?}"
        );
    }

    #[test]
    fn pure_stopword_query_is_kept_verbatim() {
        let mut db = Db::open_in_memory().unwrap();
        db.upsert_file("q.md", "md", 1, 1, "indexed", None, "what is the plan").unwrap();
        // All tokens are stopwords → filter would gut the query, so it is kept
        // unchanged and still matches as before.
        assert_eq!(significant_tokens("what is"), vec!["what", "is"]);
        let hits = db.search("what is", 10).unwrap();
        assert!(
            hits.iter().any(|h| h.rel_path == "q.md"),
            "pure-stopword query must still return results: {hits:?}"
        );
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

    /// Renaming a folder rewrites every keyed row to the new prefix in place:
    /// no content is dropped, FTS keeps finding the same text at the new path,
    /// and a sibling folder that merely *starts with* the same characters
    /// (`docs2/` vs `docs/`) is left completely alone.
    #[test]
    fn rename_prefix_rewrites_subtree_and_leaves_string_siblings_alone() {
        let mut db = Db::open_in_memory().unwrap();
        db.upsert_file("docs/a.md", "md", 10, 1, "indexed", None, "alpha widget").unwrap();
        db.upsert_file("docs/deep/b.md", "md", 20, 2, "indexed", None, "beta widget").unwrap();
        db.upsert_file("docs2/c.md", "md", 30, 3, "indexed", None, "gamma widget").unwrap();
        db.enqueue_extraction_if_changed("docs/a.md", "h1").unwrap();
        db.enqueue_extraction_if_changed("docs2/c.md", "h3").unwrap();
        db.enqueue_ocr_if_changed("docs/deep/b.md", "h2").unwrap();
        db.mark_ocr_done(
            "docs/deep/b.md", "h2",
            &[OcrRegionRow { page: 0, text: "beta scan".into(), bbox: [0.0, 0.0, 1.0, 0.1] }],
        )
        .unwrap();

        let moved = db.rename_prefix("docs", "Archive/docs").unwrap();
        assert_eq!(moved, 2, "only the two rows under docs/ move");

        // Rows live at the new paths, with content and status intact.
        assert!(db.get_file("docs/a.md").unwrap().is_none());
        assert!(db.get_file("docs/deep/b.md").unwrap().is_none());
        let a = db.get_file("Archive/docs/a.md").unwrap().expect("a moved");
        assert_eq!(a.status, "indexed");
        assert_eq!(a.size, 10);
        assert_eq!(db.get_text("Archive/docs/a.md").unwrap().as_deref(), Some("alpha widget"));
        // b.md's content carries its merged OCR text too — all of it survives.
        assert_eq!(
            db.get_text("Archive/docs/deep/b.md").unwrap().as_deref(),
            Some("beta widget\nbeta scan")
        );

        // The string-sibling folder is untouched.
        assert!(db.get_file("docs2/c.md").unwrap().is_some());
        assert_eq!(db.file_count().unwrap(), 3, "nothing was dropped");

        // Search still finds the same content, now reported at the new path.
        let hits = db.search("widget", 10).unwrap();
        let paths: Vec<&str> = hits.iter().map(|h| h.rel_path.as_str()).collect();
        assert!(paths.contains(&"Archive/docs/a.md"), "got {paths:?}");
        assert!(paths.contains(&"Archive/docs/deep/b.md"), "got {paths:?}");
        assert!(paths.contains(&"docs2/c.md"), "got {paths:?}");
        assert!(!paths.iter().any(|p| p.starts_with("docs/")), "got {paths:?}");
        assert!(!db.search("alpha", 10).unwrap().is_empty(), "content still searchable");

        // Side tables follow the rename; the sibling's queue row does not move.
        assert!(db.get_ocr_regions("Archive/docs/deep/b.md").unwrap().len() == 1);
        assert!(db.get_ocr_regions("docs/deep/b.md").unwrap().is_empty());
        let pending: Vec<String> = db
            .conn
            .prepare("SELECT rel_path FROM extractions ORDER BY rel_path")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(pending, vec!["Archive/docs/a.md".to_string(), "docs2/c.md".to_string()]);
    }

    /// A single file rename goes through the same call: the row moves AND its
    /// FTS `name` tokens are rebuilt from the new basename.
    #[test]
    fn rename_prefix_renames_a_single_file_and_its_name_tokens() {
        let mut db = Db::open_in_memory().unwrap();
        db.upsert_file("notes/kickoff.md", "md", 10, 1, "indexed", None, "body text").unwrap();

        assert_eq!(db.rename_prefix("notes/kickoff.md", "notes/launch-plan.md").unwrap(), 1);
        assert!(db.get_file("notes/kickoff.md").unwrap().is_none());
        assert!(db.get_file("notes/launch-plan.md").unwrap().is_some());

        let hits = db.search("launch", 10).unwrap();
        assert_eq!(hits.len(), 1, "findable by the new name");
        assert_eq!(hits[0].rel_path, "notes/launch-plan.md");
        assert!(db.search("kickoff", 10).unwrap().is_empty(), "old name is gone");
    }

    #[test]
    fn clear_empties_everything() {
        let mut db = seeded();
        // Stamp a knowledge-model build so we can prove clear() resets it.
        let (ents, evs) = sample_model();
        db.replace_knowledge_model(&ents, &evs, 200).unwrap();
        assert_eq!(db.knowledge_model_built_at().unwrap(), Some(200));

        // Seed the OCR queue + stored regions so clear() proves it wipes them.
        db.upsert_file("scan.png", "png", 10, 1, "metadata_only", None, "").unwrap();
        db.enqueue_ocr_if_changed("scan.png", "h1").unwrap();
        db.mark_ocr_done(
            "scan.png", "h1",
            &[OcrRegionRow { page: 0, text: "invoice total".into(), bbox: [0.1, 0.1, 0.2, 0.05] }],
        )
        .unwrap();
        assert!(!db.get_ocr_regions("scan.png").unwrap().is_empty());

        db.clear().unwrap();
        assert_eq!(db.file_count().unwrap(), 0);
        assert!(db.search("billing", 10).unwrap().is_empty());
        // From scratch: OCR regions and queue are gone too.
        assert!(db.get_ocr_regions("scan.png").unwrap().is_empty());
        assert!(db.next_pending_ocr().unwrap().is_none());
        // From scratch: no entities/events and no stale build stamp.
        let (entities, edges) = db.list_entities_with_edges().unwrap();
        assert!(entities.is_empty() && edges.is_empty());
        assert!(db.list_events().unwrap().is_empty());
        assert!(db.knowledge_model_built_at().unwrap().is_none());
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

    /// A chat created before schema v13 must read back as `scope: None`
    /// (this project only), i.e. the upgrade changes no existing chat's
    /// meaning — and a v13 chat must round-trip its scope.
    #[test]
    fn chat_scope_defaults_to_none_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        let mut db = Db::open(dir.path(), id).unwrap();

        // Simulate a pre-v13 row by writing one with no scope set.
        let legacy = ChatRow {
            id: "c-legacy".into(),
            title: "Old chat".into(),
            kind: "user".into(),
            pinned: false,
            status: "done".into(),
            created_at: 1,
            last_active_at: 1,
            archived: false,
            model: None,
            scope: None,
        };
        db.upsert_chat(&legacy).unwrap();
        assert_eq!(db.get_chat("c-legacy").unwrap().unwrap().scope, None);

        let scoped = ChatRow {
            id: "c-all".into(),
            scope: Some("all".into()),
            ..legacy.clone()
        };
        db.upsert_chat(&scoped).unwrap();
        assert_eq!(
            db.get_chat("c-all").unwrap().unwrap().scope.as_deref(),
            Some("all")
        );

        // And it is settable after the fact (re-scoping an existing chat).
        db.set_chat_field("c-legacy", ChatField::Scope, "Shattered Realms")
            .unwrap();
        assert_eq!(
            db.get_chat("c-legacy").unwrap().unwrap().scope.as_deref(),
            Some("Shattered Realms")
        );
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
            scope: None,
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
    fn question_message_content_updates_in_place() {
        let mut db = Db::open_in_memory().unwrap();
        db.upsert_chat(&ChatRow {
            id: "q-1".into(),
            title: "Questions".into(),
            kind: "user".into(),
            pinned: false,
            status: "done".into(),
            created_at: 1,
            last_active_at: 1,
            archived: false,
            model: None,
            scope: None,
        })
        .unwrap();
        let id = db
            .append_chat_message("q-1", "question", r#"{"requestId":"r1"}"#, 2)
            .unwrap();
        let other = db.append_chat_message("q-1", "assistant", "hi", 3).unwrap();

        db.update_chat_message_content(id, r#"{"requestId":"r1","answers":{"Color?":"Blue"}}"#)
            .unwrap();
        let msgs = db.chat_messages("q-1").unwrap();
        let q = msgs.iter().find(|m| m.id == id).unwrap();
        assert_eq!(q.role, "question");
        assert!(q.content.contains("\"answers\""));
        // Only the targeted row changes.
        assert_eq!(msgs.iter().find(|m| m.id == other).unwrap().content, "hi");
    }

    #[test]
    fn user_message_persists_and_reloads() {
        // A user turn is appended and returned by chat_messages, so a transcript
        // reload always includes the user's own message (the §9 vanish is purely
        // a frontend echo-timing issue, not lost persistence).
        let mut db = Db::open_in_memory().unwrap();
        let chat_id = "c-persist";
        db.upsert_chat(&ChatRow {
            id: chat_id.into(),
            title: "Persist".into(),
            kind: "user".into(),
            pinned: false,
            status: "done".into(),
            created_at: 100,
            last_active_at: 100,
            archived: false,
            model: None,
            scope: None,
        })
        .unwrap();
        db.append_chat_message(chat_id, "user", "hello there", 100).unwrap();
        let msgs = db.chat_messages(chat_id).unwrap();
        assert!(msgs.iter().any(|m| m.role == "user" && m.content == "hello there"));
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
            scope: None,
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

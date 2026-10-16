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

use crate::search::FtsHit;
use crate::{Error, Result};

pub const SCHEMA_VERSION: i64 = 27;

/// How many times a scan tries a file that fails to read before it is
/// counted as failed (knowledge-core decision 34).
pub const MAX_READ_ATTEMPTS: i64 = 3;

/// Meta key: the code index was filled from stored text for this index. The
/// stored key keeps its old name, so an index filled before the rename is not
/// filled twice.
const CODE_INDEX_BACKFILLED: &str = "code_map_backfilled";

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

/// A deterministic 64-bit FNV-1a of `text`, hex-encoded: a cheap
/// fingerprint for telling whether content changed (the OCR queue's
/// per-version key, a profile's generated hash).
pub fn content_hash(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Bounded retries for the OCR queue. A scanned page that deterministically fails Vision (unrasterizable, corrupt)
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

/// One source's index health, the one health line (knowledge-core
/// decision 19): files indexed, failed or skipped; how much of the meaning
/// index is built; how the controls did; whether the embedding model is
/// missing. Counting queries only, so it is cheap on a large index (see
/// [`Db::index_health`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexHealth {
    /// Every file row.
    pub files: i64,
    /// Read in full: their text is searchable.
    pub indexed: i64,
    /// Could not be read.
    pub failed: i64,
    /// Searchable by name only: minified, binary, a cloud placeholder, a
    /// video with no transcript yet.
    pub skipped: i64,
    /// Waiting to be indexed.
    pub queued: i64,
    /// Cut at the per-file chunk cap: their end is not searchable
    /// (`chunker::CHUNK_CAP`, knowledge-core decision 16).
    pub capped: i64,
    /// Passages in the keyword index.
    pub chunks: i64,
    /// Passages with a meaning vector, of `chunks`.
    pub embedded: i64,
    /// Controls that found their file near the top on the last run, of
    /// `controls_total` (`controls`).
    pub controls_passed: i64,
    pub controls_total: i64,
    /// The embedding model is not installed, so meaning search is off. The
    /// index cannot know this; the caller sets it.
    pub model_missing: bool,
}

impl IndexHealth {
    /// Another source's health added to this one: counts summed, the model
    /// missing if it is missing for either.
    pub fn add(&mut self, other: &IndexHealth) {
        self.files += other.files;
        self.indexed += other.indexed;
        self.failed += other.failed;
        self.skipped += other.skipped;
        self.queued += other.queued;
        self.capped += other.capped;
        self.chunks += other.chunks;
        self.embedded += other.embedded;
        self.controls_passed += other.controls_passed;
        self.controls_total += other.controls_total;
        self.model_missing |= other.model_missing;
    }

    /// The one health line: `412 indexed, 3 failed, 10 skipped · meaning
    /// 80% built · controls 4 of 5 passing · 2 too long to index whole`.
    /// The parts that are nothing are left out, except the controls, which
    /// say when there are none.
    pub fn line(&self) -> String {
        let mut files = format!("{} indexed", self.indexed);
        for (n, what) in [(self.failed, "failed"), (self.skipped, "skipped"), (self.queued, "queued")] {
            if n > 0 {
                files.push_str(&format!(", {n} {what}"));
            }
        }
        let mut parts = vec![files];
        if self.model_missing {
            parts.push("meaning search off: the embedding model is missing".into());
        } else if self.chunks > 0 {
            let pct = (self.embedded.clamp(0, self.chunks) * 100) / self.chunks;
            parts.push(if pct >= 100 { "meaning built".into() } else { format!("meaning {pct}% built") });
        }
        parts.push(match self.controls_total {
            0 => "no controls".into(),
            t => format!("controls {} of {t} passing", self.controls_passed),
        });
        if self.capped > 0 {
            parts.push(format!("{} too long to index whole", self.capped));
        }
        parts.join(" · ")
    }
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
        // mutex's Db, the scanner's, the background worker's). WAL allows
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
            // What each Markdown page says about itself (`page_meta`) is
            // made by v22, in the shape of knowledge-core decision 5.
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
                "#,
            )?;
        }
        if version < 15 {
            // The code index (`codeindex`): each code file's definitions and
            // references, from tree-sitter's tags queries, and what it
            // imports. Filled as files are indexed, and from stored text for
            // an index made before (`backfill_code_index`).
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
            // stored chunks, with no file read (now by v23). Files whose chunks or
            // code index changed with it are read again by the next scan, once:
            // JSON (each chunk now starts with its key outline) and the
            // languages whose code index gained kinds (Java enums, fields and
            // records; TypeScript enums and type aliases). Clearing the byte
            // hash makes the scan read them rather than see an unchanged file.
            // The keyword index itself is made again by v23, with today's
            // columns, which every database from before v16 also passes.
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
            // (`authority`), the tiers that are not neutral. Filled by v22,
            // once the frontmatter it reads is stored.
            self.conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS file_authority (
                    rel_path TEXT PRIMARY KEY,
                    tier     TEXT NOT NULL,
                    weight   REAL NOT NULL
                );
                "#,
            )?;
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
        if version < 21 {
            // knowledge-core decision 15: one index, one ranker. The
            // file-level `search` index, its triggers and the file-name
            // column it read go; the chunk index (`chunks_fts`) answers every
            // search. The title-query control that read it goes too
            // (decision 19).
            self.conn.execute_batch(
                r#"
                DROP TRIGGER IF EXISTS contents_ai;
                DROP TRIGGER IF EXISTS contents_ad;
                DROP TRIGGER IF EXISTS contents_au;
                DROP TABLE IF EXISTS search;
                DELETE FROM meta WHERE key = 'index_control';
                "#,
            )?;
            let has_name: bool = self
                .conn
                .prepare("SELECT 1 FROM pragma_table_info('contents') WHERE name = 'name'")?
                .exists([])?;
            if has_name {
                self.conn.execute_batch("ALTER TABLE contents DROP COLUMN name;")?;
            }
        }
        if version < 22 {
            // knowledge-core decision 5: a page's frontmatter is seven keys,
            // `title`, `aliases`, `status`, `superseded_by`, `updated`,
            // `sources` and `checked` (a date and a commit, in its written
            // form), plus an optional `reviewed_by`. `superseded_by` (and an
            // older page's `replaced_by`) is kept in the `replaced_by` column;
            // `generated` is still read from older pages. `verified` and
            // `audience` are gone. The table is made again and filled from
            // the stored text, no file read, and each file's tier with it (the
            // default tiers; the next scan applies `.ken/knowledge.json`'s
            // overrides).
            self.conn.execute_batch(
                r#"
                DROP TABLE IF EXISTS page_meta;
                CREATE TABLE page_meta (
                    rel_path    TEXT PRIMARY KEY,
                    title       TEXT,
                    aliases     TEXT NOT NULL DEFAULT '[]',
                    status      TEXT,
                    updated     TEXT,
                    sources     TEXT NOT NULL DEFAULT '[]',
                    checked     TEXT,
                    reviewed_by TEXT,
                    replaced_by TEXT NOT NULL DEFAULT '[]',
                    generated   INTEGER NOT NULL DEFAULT 0
                );
                "#,
            )?;
            self.backfill_page_meta()?;
            crate::authority::refresh_with(self, &crate::authority::Roles::default(), None)?;
        }
        if version < 23 {
            // knowledge-core decision 16: chunking. A chunk carries its last
            // line, a title (a prose chunk's heading breadcrumb, a code
            // chunk's doc comment) and a code chunk's enclosing symbol; a
            // file records whether the chunk cap cut it short, for index
            // health; and a chunk of a knowledge-base page can have a
            // one-line summary, cached by its content hash and used for
            // retrieval only. Guarded ADD COLUMNs: a fresh database made
            // `chunks` and `files` above, and tests rewind schema_version.
            for (table, column, ty) in [
                ("chunks", "end_line", "INTEGER"),
                ("chunks", "title", "TEXT NOT NULL DEFAULT ''"),
                ("chunks", "symbol", "TEXT"),
                ("files", "chunk_capped", "INTEGER NOT NULL DEFAULT 0"),
            ] {
                let has: bool = self
                    .conn
                    .prepare(&format!("SELECT 1 FROM pragma_table_info('{table}') WHERE name = '{column}'"))?
                    .exists([])?;
                if !has {
                    self.conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {ty};"))?;
                }
            }
            self.conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS chunk_summaries (
                    content_hash TEXT PRIMARY KEY,
                    summary      TEXT NOT NULL,
                    made_at      INTEGER NOT NULL
                );
                CREATE INDEX IF NOT EXISTS chunks_hash ON chunks(content_hash);
                "#,
            )?;
            // The keyword index gains its `title` field, weighted above the
            // body (`W_TITLE`): made again and filled from the stored chunks.
            self.rebuild_chunks_fts()?;
            // Prose is read again by the next scan, once, for its
            // breadcrumbs, and code with a grammar for its cut by
            // definition. A chunk whose text, title and symbol are unchanged
            // keeps its vector (`upsert_chunks` diffs by hash), but a
            // breadcrumb is new on every prose chunk, so the first semantic
            // rebuild after this re-embeds the prose and the code it re-cut.
            self.conn.execute(
                r#"UPDATE files SET mtime = ?1, byte_hash = NULL
                   WHERE status = 'indexed'
                     AND (lower(rel_path) LIKE '%.md' OR lower(rel_path) LIKE '%.mdx'
                          OR lower(rel_path) LIKE '%.txt' OR lower(rel_path) LIKE '%.pdf'
                          OR lower(rel_path) LIKE '%.rs' OR lower(rel_path) LIKE '%.py'
                          OR lower(rel_path) LIKE '%.pyi' OR lower(rel_path) LIKE '%.js'
                          OR lower(rel_path) LIKE '%.jsx' OR lower(rel_path) LIKE '%.mjs'
                          OR lower(rel_path) LIKE '%.cjs' OR lower(rel_path) LIKE '%.ts'
                          OR lower(rel_path) LIKE '%.mts' OR lower(rel_path) LIKE '%.cts'
                          OR lower(rel_path) LIKE '%.tsx' OR lower(rel_path) LIKE '%.go'
                          OR lower(rel_path) LIKE '%.java' OR lower(rel_path) LIKE '%.cs')"#,
                params![Self::REINDEX_SENTINEL_MTIME],
            )?;
        }
        if version < 24 {
            // knowledge-core decisions 17 and 18: a code file's role is
            // stored with it (`coderole`), and goes in its chunks' names
            // field and embedded header; a page's frontmatter title and
            // aliases go in its chunks' names field instead of the query
            // rewrites. The keyword index is filled again so every chunk's
            // names carry its page's title and aliases. Roles are worked out
            // by the next scan, which drops the vectors of the code files
            // that gain one, so the next semantic rebuild embeds them again.
            // The stored vocabulary is dropped, so the next search or scan
            // builds it without page aliases. Tests leave the index on the
            // next scan: a built-in rule now ignores them
            // (`kenignore::built_in_rule_sets`), and a scan removes what is
            // ignored. Controls run after the next rebuild.
            let has: bool = self.conn.prepare("SELECT 1 FROM pragma_table_info('files') WHERE name = 'role'")?.exists([])?;
            if !has {
                self.conn.execute_batch("ALTER TABLE files ADD COLUMN role TEXT;")?;
            }
            self.conn.execute_batch("DELETE FROM meta WHERE key = 'vocabulary';")?;
            self.rebuild_chunks_fts()?;
        }
        if version < 25 {
            // knowledge-core decision 26: plain words find a definition by
            // its name in any casing ("save world" is `saveWorld`,
            // `save_world`, `SaveWorld`), through an index on the name as
            // `codeindex::joined_key` writes it. Definitions only. Nothing is
            // read again.
            self.conn.execute_batch(
                "CREATE INDEX IF NOT EXISTS code_symbols_joined ON code_symbols(lower(replace(name, '_', ''))) WHERE is_def = 1;",
            )?;
        }
        if version < 26 {
            // knowledge-core decision 21: the project graph from exact sources
            // (`graph`), one edge per row between two nodes written
            // `kind:id`, with what made it (`origin`: the scan, the code
            // citations, the commits, or a write's topic tags). The model the
            // local reader extracted goes: its entities, their edges, its
            // events, its queue and its build stamp. Nothing is read again;
            // the next scan of a knowledge base makes its edges.
            self.conn.execute_batch(
                r#"
                DROP TABLE IF EXISTS entity_edges;
                DROP TABLE IF EXISTS entities;
                DROP TABLE IF EXISTS events;
                DROP TABLE IF EXISTS extractions;
                DELETE FROM meta WHERE key = 'knowledge_model_built_at';
                CREATE TABLE IF NOT EXISTS graph_edges (
                    src    TEXT NOT NULL,
                    dst    TEXT NOT NULL,
                    kind   TEXT NOT NULL,
                    origin TEXT NOT NULL,
                    detail TEXT NOT NULL DEFAULT '',
                    PRIMARY KEY (src, dst, kind)
                );
                CREATE INDEX IF NOT EXISTS graph_edges_dst ON graph_edges(dst);
                "#,
            )?;
        }
        if version < 27 {
            // knowledge-core decision 34: a file that fails to read is tried
            // again by the next scans and counted as failed after its third
            // try (`MAX_READ_ATTEMPTS`). A file that failed before this
            // counts one try. Nothing is read again now.
            let has: bool = self.conn.prepare("SELECT 1 FROM pragma_table_info('files') WHERE name = 'attempts'")?.exists([])?;
            if !has {
                self.conn.execute_batch(
                    "ALTER TABLE files ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0;
                     UPDATE files SET attempts = 1 WHERE status = 'failed';",
                )?;
            }
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
            CREATE VIRTUAL TABLE chunks_fts USING fts5(text, names, title);
            "#,
        )?;
        {
            let mut read = tx.prepare(
                "SELECT c.id, c.path, c.text, c.title, c.symbol, s.summary
                   FROM chunks c LEFT JOIN chunk_summaries s ON s.content_hash = c.content_hash
                  ORDER BY c.path",
            )?;
            let mut write = tx.prepare("INSERT INTO chunks_fts(rowid, text, names, title) VALUES (?1, ?2, ?3, ?4)")?;
            let mut rows = read.query([])?;
            // A file's extra names are read once for all its chunks.
            let mut extra: (String, Vec<String>) = (String::new(), Vec::new());
            while let Some(row) = rows.next()? {
                let (id, path, text): (i64, String, String) = (row.get(0)?, row.get(1)?, row.get(2)?);
                let (title, symbol, summary): (String, Option<String>, Option<String>) = (row.get(3)?, row.get(4)?, row.get(5)?);
                if extra.0 != path {
                    extra = (path.clone(), names_extra(&tx, &path)?);
                }
                write.execute(params![id, text, fts_names(&path, symbol.as_deref(), &text, &extra.1), fts_title(&title, summary.as_deref())])?;
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

    /// What a check keeps between its runs, by name: the lint's queue of
    /// pages to compare and when it last ran weekly, the supersession pass's
    /// entries already read (see `lint` and `supersede`).
    pub fn check_state(&self, name: &str) -> Result<Option<String>> {
        self.meta_get(&format!("check:{name}"))
    }

    pub fn store_check_state(&self, name: &str, value: &str) -> Result<()> {
        self.meta_set(&format!("check:{name}"), value)
    }

    /// The code files whose text holds `needle`, with their text: where a
    /// code comment may cite a ruling (`lint::unknown_rulings`).
    pub fn code_texts_containing(&self, needle: &str) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT f.rel_path, c.text FROM files f JOIN contents c ON c.file_id = f.id
              WHERE f.kind = 'code' AND f.status = 'indexed' AND instr(c.text, ?1) > 0
              ORDER BY f.rel_path",
        )?;
        let rows = stmt
            .query_map(params![needle], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
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
    ) -> Result<Vec<(i64, String)>> {
        let tx = self.conn.savepoint()?;
        let vec_exists = table_exists(&tx, "vec_chunks")?;
        // The `tier` column is from when a path could be searchable but not
        // read for the graph; with two states (decision 34) every chunk
        // written is indexed, 0.
        let tier = 0i64;

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
        // The page's title and aliases and the file's role, in every
        // chunk's names field (`names_extra`).
        let extra = names_extra(&tx, path)?;
        for chunk in chunks {
            let seq = chunk.seq as i64;
            let unchanged = matches!(existing.get(&seq), Some((_, hash)) if hash == &chunk.content_hash);
            if unchanged {
                // Same text, but lines added above it move where it starts.
                tx.execute(
                    "UPDATE chunks SET line = ?3, end_line = ?4 WHERE path = ?1 AND seq = ?2",
                    params![path, seq, chunk.line as i64, chunk.end_line as i64],
                )?;
            } else {
                tx.execute(
                    r#"INSERT INTO chunks (path, seq, text, token_est, content_hash, tier, line, end_line, title, symbol)
                       VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                       ON CONFLICT(path, seq) DO UPDATE SET
                         text = ?3, token_est = ?4, content_hash = ?5, tier = ?6, line = ?7,
                         end_line = ?8, title = ?9, symbol = ?10"#,
                    params![
                        path,
                        seq,
                        chunk.text,
                        chunk.token_est as i64,
                        chunk.content_hash,
                        tier,
                        chunk.line as i64,
                        chunk.end_line as i64,
                        chunk.title,
                        chunk.symbol
                    ],
                )?;
                let id: i64 = tx.query_row(
                    "SELECT id FROM chunks WHERE path = ?1 AND seq = ?2",
                    params![path, seq],
                    |r| r.get(0),
                )?;
                // A summary cached for this text already (the same passage
                // elsewhere, or this one before an edit was undone).
                let summary: Option<String> = tx
                    .query_row(
                        "SELECT summary FROM chunk_summaries WHERE content_hash = ?1",
                        params![chunk.content_hash],
                        |r| r.get(0),
                    )
                    .optional()?;
                // chunks_fts row: delete-then-reinsert rather than UPDATE since
                // FTS5 doesn't support partial-column UPDATE semantics the way
                // a normal table does. The `names` column holds the path,
                // file name and symbol names and their parts (`fts_names`);
                // `title` the breadcrumb or doc comment and the summary
                // (`fts_title`); `text` is the chunk's own text, as shown.
                tx.execute("DELETE FROM chunks_fts WHERE rowid = ?1", params![id])?;
                tx.execute(
                    "INSERT INTO chunks_fts(rowid, text, names, title) VALUES (?1, ?2, ?3, ?4)",
                    params![
                        id,
                        chunk.text,
                        fts_names(path, chunk.symbol.as_deref(), &chunk.text, &extra),
                        fts_title(&chunk.title, summary.as_deref())
                    ],
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

    /// Write a file's chunks ([`Db::upsert_chunks`]) and whether the chunk
    /// cap cut it short, for index health.
    pub fn store_chunked(
        &mut self,
        path: &str,
        chunked: &crate::chunker::Chunked,
    ) -> Result<Vec<(i64, String)>> {
        let changed = self.upsert_chunks(path, &chunked.chunks)?;
        self.conn.execute(
            "UPDATE files SET chunk_capped = ?2 WHERE rel_path = ?1",
            params![path, chunked.capped as i64],
        )?;
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

    /// Keyword search over the chunk index (`chunks_fts`), the only keyword
    /// index (knowledge-core decision 15): the keyword half of
    /// `routing::search_member`, beside `semantic_search`'s meaning half.
    /// Query words are tokenized with stopwords dropped and the last word
    /// matched as a prefix. Returns up to `k` hits, each the best chunk of a
    /// different file, ordered best-first (by BM25 rank); returns an empty
    /// `Vec` (never errors) for an empty/stopword-only query, `k == 0`, or a
    /// pre-v12 DB where `chunks_fts` doesn't exist yet.
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
               FROM (SELECT rowid AS id, bm25(chunks_fts, 1.0, ?3, ?4) AS r
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
                .query_map(params![q, depth, names, W_TITLE], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
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
    /// content (empty for metadata-only/failed files). Search reads the
    /// file's chunks, which the caller writes (`upsert_chunks`).
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
        let tx = self.conn.savepoint()?;
        tx.execute(
            // A failed read counts one more try; any other status starts the
            // count again (decision 34: failed after the third try).
            r#"INSERT INTO files (rel_path, kind, size, mtime, status, error, attempts)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, CASE WHEN ?5 = 'failed' THEN 1 ELSE 0 END)
               ON CONFLICT(rel_path) DO UPDATE SET
                 kind = ?2, size = ?3, mtime = ?4, status = ?5, error = ?6,
                 attempts = CASE WHEN ?5 = 'failed' THEN attempts + 1 ELSE 0 END"#,
            params![rel_path, kind, size, mtime, status, error],
        )?;
        let file_id: i64 = tx.query_row(
            "SELECT id FROM files WHERE rel_path = ?1",
            params![rel_path],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO contents (file_id, text) VALUES (?1, ?2)
             ON CONFLICT(file_id) DO UPDATE SET text = ?2",
            params![file_id, text],
        )?;
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
            tx.execute("DELETE FROM contents WHERE file_id = ?1", params![id])?;
            tx.execute("DELETE FROM files WHERE id = ?1", params![id])?;
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

    /// Whether `path` has any chunk in the index.
    pub fn has_chunks(&self, path: &str) -> Result<bool> {
        Ok(self.conn.query_row("SELECT EXISTS(SELECT 1 FROM chunks WHERE path = ?1)", params![path], |r| r.get(0))?)
    }

    /// The paths with at least one chunk that has no vector yet, in path
    /// order: what a catch-up after a scan has to embed. With no vector
    /// table, every path with a chunk.
    pub fn paths_missing_vectors(&self) -> Result<Vec<String>> {
        let sql = if table_exists(&self.conn, "vec_chunks")? {
            "SELECT DISTINCT c.path FROM chunks c
              WHERE NOT EXISTS (SELECT 1 FROM vec_chunks v WHERE v.chunk_id = c.id)
              ORDER BY c.path"
        } else {
            "SELECT DISTINCT path FROM chunks ORDER BY path"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([], |r| r.get(0))?.collect::<std::result::Result<Vec<String>, _>>()?;
        Ok(rows)
    }

    /// The chunks of `path` that have no vector yet, as (id, text): what a
    /// semantic rebuild still has to embed. Chunks are written by every scan
    /// (keyword search over chunks needs no model); vectors only by the
    /// rebuild. With no vector table, every chunk counts as missing.
    ///
    /// The text is what is embedded (`chunker::embed_text`): the chunk's own
    /// text under its breadcrumb or its path, symbol and doc comment, and its
    /// one-line summary when one is cached by then.
    pub fn chunks_missing_vectors(&self, path: &str) -> Result<Vec<(i64, String)>> {
        let missing = if table_exists(&self.conn, "vec_chunks")? {
            "AND NOT EXISTS (SELECT 1 FROM vec_chunks v WHERE v.chunk_id = c.id)"
        } else {
            ""
        };
        let sql = format!(
            "SELECT c.id, c.text, c.title, c.symbol, s.summary
               FROM chunks c LEFT JOIN chunk_summaries s ON s.content_hash = c.content_hash
              WHERE c.path = ?1 {missing}
              ORDER BY c.seq"
        );
        let role = self.file_role(path)?;
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params![path], |r| {
                let (id, text, title): (i64, String, String) = (r.get(0)?, r.get(1)?, r.get(2)?);
                let (symbol, summary): (Option<String>, Option<String>) = (r.get(3)?, r.get(4)?);
                let embed = crate::chunker::embed_text(path, &title, symbol.as_deref(), role.as_deref(), summary.as_deref(), &text);
                Ok((id, embed))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Up to `limit` chunks of this index's pages that have no one-line
    /// summary cached for their text yet, as (content hash, path, title,
    /// text), one per distinct text, in path order. Pages are the Markdown
    /// files that were read (pages, notes, decisions, tickets); a raw source
    /// waiting in `Inbox/` or kept under `Ingestion/Raw/` is not one, and
    /// code never is (`summary`, knowledge-core decision 16).
    pub fn chunks_wanting_summaries(&self, limit: usize) -> Result<Vec<(String, String, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.content_hash, c.path, c.title, c.text
               FROM chunks c JOIN files f ON f.rel_path = c.path
              WHERE f.status = 'indexed'
                AND c.symbol IS NULL
                AND c.text <> c.path
                AND (lower(c.path) LIKE '%.md' OR lower(c.path) LIKE '%.mdx' OR lower(c.path) LIKE '%.markdown')
                AND c.path NOT LIKE 'Inbox/%'
                AND c.path NOT LIKE '%Ingestion/Raw/%'
                AND NOT EXISTS (SELECT 1 FROM chunk_summaries s WHERE s.content_hash = c.content_hash)
              GROUP BY c.content_hash
              ORDER BY c.path, c.seq
              LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Cache one-line summaries by chunk content hash, and put each in the
    /// `title` field of the keyword rows of the chunks holding that text. An
    /// empty summary marks a text Claude was asked about and gave none, so
    /// it is not asked again; it adds nothing to search. Summaries of text
    /// no chunk holds any more are forgotten. Vectors are left as they are:
    /// a summary joins the embedded text the next time its chunk is embedded.
    pub fn store_chunk_summaries(&mut self, summaries: &[(String, String)], at: i64) -> Result<()> {
        let tx = self.conn.savepoint()?;
        for (hash, summary) in summaries {
            tx.execute(
                "INSERT OR REPLACE INTO chunk_summaries(content_hash, summary, made_at) VALUES (?1, ?2, ?3)",
                params![hash, summary, at],
            )?;
            let rows: Vec<(i64, String)> = {
                let mut stmt = tx.prepare("SELECT id, title FROM chunks WHERE content_hash = ?1")?;
                let rows = stmt.query_map(params![hash], |r| Ok((r.get(0)?, r.get(1)?)))?;
                rows.collect::<std::result::Result<_, _>>()?
            };
            for (id, title) in rows {
                tx.execute(
                    "UPDATE chunks_fts SET title = ?2 WHERE rowid = ?1",
                    params![id, fts_title(&title, Some(summary.as_str()))],
                )?;
            }
        }
        tx.execute("DELETE FROM chunk_summaries WHERE content_hash NOT IN (SELECT content_hash FROM chunks)", [])?;
        tx.commit()?;
        Ok(())
    }

    /// The one-line summary cached for a chunk's text, if any (empty when
    /// Claude gave none).
    pub fn chunk_summary(&self, content_hash: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT summary FROM chunk_summaries WHERE content_hash = ?1", params![content_hash], |r| r.get(0))
            .optional()?)
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
    /// changes no file's bytes, mtime or size, so re-reading it (or worse,
    /// clearing the whole index) would throw away OCR and transcripts to
    /// achieve a pure string rename. Everything happens in one
    /// transaction so a crash can't leave half the subtree at each path.
    ///
    /// Matching mirrors [`Self::remove_folder`]: the exact path (so a single
    /// file rename works too) plus `from/…` descendants, with `LIKE … ESCAPE`
    /// so a folder called `50%_plans` or `a_b` can't act as a wildcard. The
    /// trailing slash is what keeps a string-sibling like `docs2/` out of a
    /// `docs` move.
    ///
    /// Tables covered: `files`, `ocr_regions`, `ocr_pending`, the file-valued
    /// `review_items.source_ref` kinds, what search reads by path (the chunks,
    /// with the path words in their keyword row, `fts_names`; page facts,
    /// tiers, page links and the code index), and the topic tags of the
    /// project graph (`graph_edges`), which only a write makes again. The
    /// graph's other edges are made again by the next scan. `contents` is
    /// keyed by `file_id`, not by path, so its rows need no move.
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

        // Which rows are moving, and where to.
        let pairs: Vec<(String, String)> = {
            let mut stmt = tx.prepare(
                "SELECT rel_path FROM files
                 WHERE rel_path = ?1 OR rel_path LIKE ?2 ESCAPE '\\'",
            )?;
            let rows = stmt.query_map(params![from, like], |r| r.get::<_, String>(0))?;
            rows.map(|r| {
                r.map(|old| {
                    let new = format!("{to}{}", &old[from.len()..]);
                    (old, new)
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
        let to_like = format!("{}/%", like_escape(&to));
        let stale_chunks: Vec<i64> = {
            let mut stmt = tx.prepare("SELECT id FROM chunks WHERE path = ?1 OR path LIKE ?2 ESCAPE '\\'")?;
            let rows = stmt.query_map(params![to, to_like], |r| r.get(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let vec_exists = table_exists(&tx, "vec_chunks")?;
        for id in &stale_chunks {
            tx.execute("DELETE FROM chunks_fts WHERE rowid = ?1", params![id])?;
            if vec_exists {
                tx.execute("DELETE FROM vec_chunks WHERE chunk_id = ?1", params![id])?;
            }
        }
        for (table, col) in PATH_KEYED {
            tx.execute(
                &format!(
                    "DELETE FROM {table}
                     WHERE {col} = ?1 OR {col} LIKE ?2 ESCAPE '\\'"
                ),
                params![to, to_like],
            )?;
        }

        // The bulk of the work: one prefix rewrite per path-keyed table.
        for (table, col) in PATH_KEYED {
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

        // A chunk's keyword row holds its path's words (`fts_names`), so the
        // moved chunks' rows are written again with the new path. The chunk
        // of a file with no text holds its path (`chunker::name_chunk`), so
        // it is written again whole.
        {
            let moved: Vec<(i64, String, String, Option<String>)> = {
                let mut stmt = tx.prepare("SELECT id, path, text, symbol FROM chunks WHERE path = ?1 OR path LIKE ?2 ESCAPE '\\'")?;
                let rows = stmt.query_map(params![to, to_like], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
                rows.collect::<std::result::Result<_, _>>()?
            };
            let mut write = tx.prepare("UPDATE chunks_fts SET names = ?2 WHERE rowid = ?1")?;
            for (id, path, mut text, symbol) in moved {
                let extra = names_extra(&tx, &path)?;
                let old = format!("{from}{}", &path[to.len()..]);
                if text == old {
                    let c = crate::chunker::name_chunk(&path);
                    text = c.text.clone();
                    tx.execute(
                        "UPDATE chunks SET text = ?2, token_est = ?3, content_hash = ?4 WHERE id = ?1",
                        params![id, c.text, c.token_est as i64, c.content_hash],
                    )?;
                    tx.execute("UPDATE chunks_fts SET text = ?2 WHERE rowid = ?1", params![id, c.text])?;
                    if vec_exists {
                        tx.execute("DELETE FROM vec_chunks WHERE chunk_id = ?1", params![id])?;
                    }
                }
                write.execute(params![id, fts_names(&path, symbol.as_deref(), &text, &extra)])?;
            }
        }

        // The topic tags of the pages that moved follow them.
        move_graph_topics(&tx, &pairs)?;

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

    /// Drop all indexed data (schema stays). Used by reindex.
    pub fn clear(&mut self) -> Result<()> {
        let tx = self.conn.savepoint()?;
        tx.execute("DELETE FROM contents", [])?;
        tx.execute("DELETE FROM files", [])?;
        // OCR is derived like everything else: a reindex starts from scratch, so
        // stored regions and the OCR queue are wiped too. User data (chats)
        // and what a scan does not make again (the graph's topic tags and
        // commit edges) are left untouched.
        tx.execute("DELETE FROM ocr_regions", [])?;
        tx.execute("DELETE FROM ocr_pending", [])?;
        tx.commit()?;
        Ok(())
    }

    /// Drop the chunks of every path no longer in `files`: what a reindex
    /// ([`Self::clear`], then a scan) leaves behind of a file that is gone.
    /// The chunks of files still there keep their vectors. Returns how many
    /// paths were dropped.
    pub fn drop_orphan_chunks(&mut self) -> Result<usize> {
        let paths: Vec<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT DISTINCT path FROM chunks WHERE path NOT IN (SELECT rel_path FROM files)")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        for path in &paths {
            self.delete_chunks(path)?;
        }
        Ok(paths.len())
    }

    pub fn file_count(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?)
    }

    /// The index as an instrument: the one health line (knowledge-core
    /// decision 19, [`IndexHealth`]). An index that has stopped reading
    /// still answers searches, with thin results and no error; these
    /// numbers are how anyone can tell. `model_missing` is left false for
    /// the caller to set.
    pub fn index_health(&self) -> Result<IndexHealth> {
        // A file that failed fewer than `MAX_READ_ATTEMPTS` times waits for
        // the next scan's try: queued, not failed.
        let (files, indexed, failed, skipped, queued, capped): (i64, i64, i64, i64, i64, i64) = self.conn.query_row(
            "SELECT COUNT(*),
                    COALESCE(SUM(status = 'indexed'), 0),
                    COALESCE(SUM(status = 'failed' AND attempts >= ?1), 0),
                    COALESCE(SUM(status IN ('metadata_only', 'cloud_only')), 0),
                    COALESCE(SUM(status = 'pending' OR (status = 'failed' AND attempts < ?1)), 0),
                    COALESCE(SUM(chunk_capped <> 0), 0)
               FROM files",
            params![MAX_READ_ATTEMPTS],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )?;
        let controls = crate::controls::stored(self);
        Ok(IndexHealth {
            files,
            indexed,
            failed,
            skipped,
            queued,
            capped,
            chunks: self.chunk_count()?,
            embedded: self.vector_count()?,
            controls_passed: controls.iter().filter(|c| c.passed).count() as i64,
            controls_total: controls.len() as i64,
            model_missing: false,
        })
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

    pub fn resolve_review_item(&mut self, id: i64, at: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE review_items SET status = 'resolved', resolved_at = ?2
             WHERE id = ?1 AND status = 'open'",
            params![id, at],
        )?;
        Ok(())
    }

    /// Remove a review item, open or resolved, as if it was never filed.
    pub fn delete_review_item(&mut self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM review_items WHERE id = ?1", params![id])?;
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

    /// Store (or, with None, forget) what a page's frontmatter says. A
    /// change of title or aliases writes the page's chunks' names again,
    /// since they carry them (`names_extra`).
    pub fn set_page_meta(&self, rel_path: &str, meta: Option<&crate::pagemeta::PageMeta>) -> Result<()> {
        let before = names_extra(&self.conn, rel_path)?;
        self.write_page_meta(rel_path, meta)?;
        if names_extra(&self.conn, rel_path)? != before {
            self.refresh_chunk_names(rel_path)?;
        }
        Ok(())
    }

    /// The page's successor (`superseded_by`) is kept in the `replaced_by`
    /// column, the name v22 gave it, so no index is made again for it.
    fn write_page_meta(&self, rel_path: &str, meta: Option<&crate::pagemeta::PageMeta>) -> Result<()> {
        let Some(m) = meta else {
            self.conn.execute("DELETE FROM page_meta WHERE rel_path = ?1", params![rel_path])?;
            return Ok(());
        };
        let json = |v: &Vec<String>| serde_json::to_string(v).unwrap_or_else(|_| "[]".into());
        self.conn.execute(
            r#"INSERT INTO page_meta (rel_path, title, aliases, status, updated, sources, checked, reviewed_by, replaced_by, generated)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
               ON CONFLICT(rel_path) DO UPDATE SET
                 title = ?2, aliases = ?3, status = ?4, updated = ?5, sources = ?6,
                 checked = ?7, reviewed_by = ?8, replaced_by = ?9, generated = ?10"#,
            params![
                rel_path,
                m.title,
                json(&m.aliases),
                m.status,
                m.updated,
                json(&m.sources),
                m.checked.as_ref().map(|c| c.to_string()),
                m.reviewed_by,
                json(&m.superseded_by),
                m.generated as i64
            ],
        )?;
        Ok(())
    }

    /// Write the names field of every chunk of `path` again, from its
    /// stored text, symbol and extra names ([`names_extra`]).
    pub fn refresh_chunk_names(&self, path: &str) -> Result<()> {
        let extra = names_extra(&self.conn, path)?;
        let rows: Vec<(i64, String, Option<String>)> = {
            let mut stmt = self.conn.prepare("SELECT id, text, symbol FROM chunks WHERE path = ?1")?;
            let rows = stmt.query_map(params![path], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let mut write = self.conn.prepare("UPDATE chunks_fts SET names = ?2 WHERE rowid = ?1")?;
        for (id, text, symbol) in rows {
            write.execute(params![id, fts_names(path, symbol.as_deref(), &text, &extra)])?;
        }
        Ok(())
    }

    /// The names field of each chunk of `path`, in order.
    #[cfg(test)]
    pub fn chunk_names(&self, path: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT f.names FROM chunks c JOIN chunks_fts f ON f.rowid = c.id WHERE c.path = ?1 ORDER BY c.seq",
        )?;
        let rows = stmt.query_map(params![path], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    /// Every indexed file's path.
    pub fn indexed_paths(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT rel_path FROM files WHERE status = 'indexed' ORDER BY rel_path")?;
        let rows = stmt.query_map([], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    /// A code file's stored role (`coderole`), None when it has none.
    pub fn file_role(&self, rel_path: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT role FROM files WHERE rel_path = ?1", params![rel_path], |r| r.get::<_, Option<String>>(0))
            .optional()?
            .flatten())
    }

    /// Store the files' roles (`coderole::refresh`). A file whose role
    /// changed has its chunks' names written again and their vectors
    /// dropped, since the role is in both. Returns the paths that changed.
    pub fn set_file_roles(&self, rows: &[(String, Option<String>)]) -> Result<Vec<String>> {
        let mut changed = Vec::new();
        let tx = self.conn.unchecked_transaction()?;
        let vec_exists = table_exists(&tx, "vec_chunks")?;
        for (path, role) in rows {
            let stored: Option<Option<String>> =
                tx.query_row("SELECT role FROM files WHERE rel_path = ?1", params![path], |r| r.get(0)).optional()?;
            let Some(stored) = stored else { continue };
            if stored == *role {
                continue;
            }
            tx.execute("UPDATE files SET role = ?2 WHERE rel_path = ?1", params![path, role])?;
            changed.push(path.clone());
        }
        for path in &changed {
            let extra = names_extra(&tx, path)?;
            let chunks: Vec<(i64, String, Option<String>)> = {
                let mut stmt = tx.prepare("SELECT id, text, symbol FROM chunks WHERE path = ?1")?;
                let rows = stmt.query_map(params![path], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
                rows.collect::<std::result::Result<_, _>>()?
            };
            for (id, text, symbol) in chunks {
                tx.execute("UPDATE chunks_fts SET names = ?2 WHERE rowid = ?1", params![id, fts_names(path, symbol.as_deref(), &text, &extra)])?;
                if vec_exists {
                    tx.execute("DELETE FROM vec_chunks WHERE chunk_id = ?1", params![id])?;
                }
            }
        }
        tx.commit()?;
        Ok(changed)
    }

    /// The last controls run (`controls`), as JSON.
    pub fn control_results(&self) -> Result<Option<String>> {
        self.meta_get("controls")
    }

    pub fn store_control_results(&self, json: &str) -> Result<()> {
        self.meta_set("controls", json)
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
    /// ranks every hit as neutral.
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
    /// many pages had frontmatter. The chunks' names are not written again
    /// here: this runs inside migrations older than the columns that needs,
    /// and the v24 migration fills the keyword index from these rows.
    pub fn backfill_page_meta(&self) -> Result<usize> {
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
                self.write_page_meta(&rel, Some(&meta))?;
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
    /// Replace one file's code index: its symbols and its imports.
    pub fn set_code_index(&mut self, path: &str, map: &crate::codeindex::FileMap) -> Result<()> {
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

    /// Code-index symbols matching a query: `name` exactly (case-insensitive),
    /// definitions only or references only, in `path` when given.
    pub fn code_symbols(&self, name: Option<&str>, path: Option<&str>, is_def: Option<bool>, limit: usize) -> Result<Vec<crate::codeindex::Symbol>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, name, kind, line, end_line, is_def, docs FROM code_symbols
             WHERE (?1 IS NULL OR name = ?1 COLLATE NOCASE)
               AND (?2 IS NULL OR path = ?2)
               AND (?3 IS NULL OR is_def = ?3)
             ORDER BY path, line
             LIMIT ?4",
        )?;
        let rows = stmt.query_map(params![name, path, is_def.map(i64::from), limit as i64], symbol_row)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Definitions whose name, ASCII lowercase and without underscores
    /// (`codeindex::joined_key`), is one of `keys`, at most `limit`: the
    /// plain-word lookup (`codeindex::definitions_named`), read through the
    /// `code_symbols_joined` index.
    pub fn definitions_by_joined_name(&self, keys: &[String], limit: usize) -> Result<Vec<crate::codeindex::Symbol>> {
        if keys.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let marks = vec!["?"; keys.len()].join(", ");
        let mut stmt = self.conn.prepare(&format!(
            "SELECT path, name, kind, line, end_line, is_def, docs FROM code_symbols
             WHERE is_def = 1 AND lower(replace(name, '_', '')) IN ({marks})
             ORDER BY path, line
             LIMIT {limit}"
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(keys.iter()), symbol_row)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Every import of every file: (importing file, target as written).
    pub fn code_imports(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare("SELECT path, target FROM code_imports ORDER BY path")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Map every indexed code file from its stored text, for an index made
    /// before the code index. Returns how many files were mapped.
    pub fn backfill_code_index(&mut self) -> Result<usize> {
        // Once per index. Not "is anything mapped": a scan maps the files it
        // re-reads, so a few mapped files say nothing about the rest.
        if self.meta_get(CODE_INDEX_BACKFILLED)?.is_some() {
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
            if let Some(map) = crate::codeindex::map_file(&path, &text) {
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
            params![CODE_INDEX_BACKFILLED],
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

    // --- the project graph (knowledge-core decision 21, `graph`) ---

    /// Replace every edge `origin` made with `edges`, in one transaction.
    pub fn replace_graph_edges(&mut self, origin: &str, edges: &[crate::graph::Edge]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM graph_edges WHERE origin = ?1", params![origin])?;
        insert_graph_edges(&tx, origin, edges)?;
        tx.commit()?;
        Ok(())
    }

    /// Add edges `origin` made, keeping the ones already stored. Returns how
    /// many were new.
    pub fn add_graph_edges(&mut self, origin: &str, edges: &[crate::graph::Edge]) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let n = insert_graph_edges(&tx, origin, edges)?;
        tx.commit()?;
        Ok(n)
    }

    /// Replace the edges `origin` made from `src` with `edges`.
    pub fn replace_graph_edges_from(&self, origin: &str, src: &str, edges: &[crate::graph::Edge]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM graph_edges WHERE origin = ?1 AND src = ?2", params![origin, src])?;
        insert_graph_edges(&tx, origin, edges)?;
        tx.commit()?;
        Ok(())
    }

    /// Every stored edge from or to `node` (written `kind:id`).
    pub fn graph_edges_touching(&self, node: &str) -> Result<Vec<crate::graph::Edge>> {
        self.graph_edges_where("WHERE src = ?1 OR dst = ?1", &[&node])
    }

    /// Every stored edge `origin` made.
    pub fn graph_edges_of(&self, origin: &str) -> Result<Vec<crate::graph::Edge>> {
        self.graph_edges_where("WHERE origin = ?1", &[&origin])
    }

    fn graph_edges_where(&self, filter: &str, args: &[&dyn rusqlite::ToSql]) -> Result<Vec<crate::graph::Edge>> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT src, dst, kind, detail FROM graph_edges {filter} ORDER BY src, kind, dst"))?;
        let rows = stmt.query_map(args, |r| {
            Ok(crate::graph::Edge { src: r.get(0)?, dst: r.get(1)?, kind: r.get(2)?, detail: r.get(3)? })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Drop the edges `origin` made from any of `srcs`.
    pub fn delete_graph_edges_from(&mut self, origin: &str, srcs: &[String]) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let mut n = 0;
        {
            let mut del = tx.prepare("DELETE FROM graph_edges WHERE origin = ?1 AND src = ?2")?;
            for src in srcs {
                n += del.execute(params![origin, src])?;
            }
        }
        tx.commit()?;
        Ok(n)
    }

    /// What a page's frontmatter said when it was last indexed.
    pub fn page_meta(&self, rel_path: &str) -> Result<Option<crate::pagemeta::PageMeta>> {
        let list = |s: String| serde_json::from_str::<Vec<String>>(&s).unwrap_or_default();
        Ok(self
            .conn
            .query_row(
                "SELECT title, aliases, status, updated, sources, checked, reviewed_by, replaced_by, generated
                   FROM page_meta WHERE rel_path = ?1",
                params![rel_path],
                |r| {
                    Ok(crate::pagemeta::PageMeta {
                        title: r.get(0)?,
                        aliases: list(r.get(1)?),
                        status: r.get(2)?,
                        updated: r.get(3)?,
                        sources: list(r.get(4)?),
                        checked: r.get::<_, Option<String>>(5)?.as_deref().and_then(crate::pagemeta::Checked::parse),
                        reviewed_by: r.get(6)?,
                        superseded_by: list(r.get(7)?),
                        generated: r.get::<_, i64>(8)? != 0,
                    })
                },
            )
            .optional()?)
    }

    /// The line a chunk starts on and the symbol it holds (`Store.open`, or
    /// `save, load` for packed definitions; None for prose and line blocks).
    pub fn chunk_line_symbol(&self, chunk_id: i64) -> Result<(Option<i64>, Option<String>)> {
        Ok(self
            .conn
            .query_row("SELECT line, symbol FROM chunks WHERE id = ?1", params![chunk_id], |r| {
                Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<String>>(1)?))
            })
            .optional()?
            .unwrap_or((None, None)))
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

    /// How many times each failed file has been tried, by path.
    pub fn read_attempts(&self) -> Result<std::collections::HashMap<String, i64>> {
        let mut stmt = self.conn.prepare("SELECT rel_path, attempts FROM files WHERE status = 'failed'")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Record a file's byte hash, and its modified time when only that moved.
    pub fn set_file_byte_hash(&mut self, rel_path: &str, hash: &str, mtime: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE files SET byte_hash = ?2, mtime = ?3 WHERE rel_path = ?1",
            params![rel_path, hash, mtime],
        )?;
        Ok(())
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
    /// its content). Only those transitioned rows are
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

    // --- OCR queue + region storage (Phase 2; independent of extraction) ---

    /// Enqueue a file for OCR unless it is already `done` at this exact hash.
    /// The hash is a cheap per-version fingerprint (see `scan::index_one`), so a
    /// rescan that leaves a file unchanged never re-OCRs it, while a real change
    /// (new hash) re-queues it. Returns true if a `pending` row was written.
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
    /// layer for scanned PDFs) and chunked, so the file becomes searchable by
    /// its OCR'd words. Because a content change re-runs
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
        // Merge the recognized text into `contents`, and below into the
        // file's chunks, so search picks it up.
        let ocr_text: String = regions
            .iter()
            .map(|r| r.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let mut merged_text: Option<String> = None;
        if !ocr_text.trim().is_empty() {
            let file: Option<i64> = tx
                .query_row("SELECT id FROM files WHERE rel_path = ?1", params![rel_path], |r| r.get(0))
                .ok();
            if let Some(id) = file {
                let existing: Option<String> = tx
                    .query_row("SELECT text FROM contents WHERE file_id = ?1", params![id], |r| r.get(0))
                    .ok();
                let merged = match existing {
                    Some(base) if !base.trim().is_empty() => format!("{base}\n{ocr_text}"),
                    _ => ocr_text,
                };
                tx.execute(
                    "INSERT INTO contents (file_id, text) VALUES (?1, ?2)
                     ON CONFLICT(file_id) DO UPDATE SET text = ?2",
                    params![id, merged],
                )?;
                merged_text = Some(merged);
            }
        }
        tx.execute(
            "UPDATE ocr_pending SET status = 'done', error = NULL
             WHERE rel_path = ?1 AND content_hash = ?2",
            params![rel_path, content_hash],
        )?;
        tx.commit()?;
        // The chunk index is the only search index (knowledge-core decision
        // 15), so the recognized words are chunked like any file's text.
        if let Some(text) = merged_text {
            let chunked = crate::chunker::chunk_with(rel_path, &text, &crate::chunker::IndexProfile::default_for(rel_path), &[]);
            self.store_chunked(rel_path, &chunked)?;
        }
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
    /// worker retries them when it next goes idle. Returns how many were
    /// re-queued.
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

/// Insert graph edges made by `origin`, an edge already stored left as it
/// is. Returns how many were new.
fn insert_graph_edges(conn: &Connection, origin: &str, edges: &[crate::graph::Edge]) -> Result<usize> {
    let mut stmt = conn.prepare(
        "INSERT OR IGNORE INTO graph_edges (src, dst, kind, origin, detail) VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    let mut n = 0;
    for e in edges {
        n += stmt.execute(params![e.src, e.dst, e.kind, origin, e.detail])?;
    }
    Ok(n)
}

/// The topic tags of moved pages follow them (`Db::rename_prefix`): each
/// (old path, new path) pair's node is written again, its kind worked out
/// from the new path (a note moved out of the notes is a page).
fn move_graph_topics(conn: &Connection, pairs: &[(String, String)]) -> Result<()> {
    let mut stmt = conn.prepare("UPDATE OR REPLACE graph_edges SET src = ?2 WHERE origin = ?3 AND src = ?1")?;
    for (old, new) in pairs {
        let (from, to) = (crate::graph::Node::of_path(old).key(), crate::graph::Node::of_path(new).key());
        stmt.execute(params![from, to, crate::graph::ORIGIN_TOPICS])?;
    }
    Ok(())
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

/// The tables a move rewrites by path (`Db::rename_prefix`), each with its
/// path column. A stale row at the destination is cleared first.
const PATH_KEYED: [(&str, &str); 9] = [
    ("files", "rel_path"),
    ("ocr_regions", "rel_path"),
    ("ocr_pending", "rel_path"),
    ("chunks", "path"),
    ("page_meta", "rel_path"),
    ("file_authority", "rel_path"),
    ("page_links", "from_path"),
    ("code_symbols", "path"),
    ("code_imports", "path"),
];

/// Relative-path tokens for FTS index augmentation: the *whole* path (every
/// segment, not just the filename), separators spaced out
/// ("src/local_llm.rs" → "src local_llm rs" → "src local llm rs" after
/// separator-splitting).
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

/// The `title` column's BM25 weight (the text's is 1): a prose chunk's
/// heading breadcrumb, a code chunk's doc comment, and a page chunk's
/// one-line summary (`fts_title`). Knowledge-core decision 16 weights it
/// about two to three times the body: a passage under "Retries › Backoff"
/// is about backoff even when its own sentences never say the word.
const W_TITLE: f64 = 2.5;

/// The names a file gives every one of its chunks, beyond its path: a
/// page's frontmatter title and aliases (decision 18: they feed the names
/// field, not the query rewrites) and a code file's role (decision 17). Read
/// from what the index stored; a database from before either was kept reads
/// as none.
fn names_extra(conn: &Connection, path: &str) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    let page: Option<(Option<String>, String)> = conn
        .query_row("SELECT title, aliases FROM page_meta WHERE rel_path = ?1", params![path], |r| Ok((r.get(0)?, r.get(1)?)))
        .ok();
    if let Some((title, aliases)) = page {
        out.extend(title.filter(|t| !t.trim().is_empty()));
        out.extend(serde_json::from_str::<Vec<String>>(&aliases).unwrap_or_default().into_iter().filter(|a| !a.trim().is_empty()));
    }
    let role: Option<String> = conn.query_row("SELECT role FROM files WHERE rel_path = ?1", params![path], |r| r.get(0)).ok().flatten();
    out.extend(role);
    Ok(out)
}

/// The `title` column of a chunk's keyword row: its title (breadcrumb or
/// doc comment) and its one-line summary, when it has one.
fn fts_title(title: &str, summary: Option<&str>) -> String {
    match summary.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) if title.is_empty() => s.to_string(),
        Some(s) => format!("{title}\n{s}"),
        None => title.to_string(),
    }
}

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
///
/// A code chunk's symbol (`Store.open`, or `save, load` for packed
/// definitions) is named here too, each name and its parts, and so are the
/// file's `extra` names ([`names_extra`]): a page's frontmatter title and
/// aliases, a code file's role (knowledge-core decisions 17 and 18).
fn fts_names(rel_path: &str, symbol: Option<&str>, chunk_text: &str, extra: &[String]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut add = |w: &str| {
        if !w.is_empty() && seen.insert(w.to_lowercase()) {
            out.push(w.to_string());
        }
    };
    let declared = extract_symbol_names(chunk_text);
    let symbols = symbol
        .into_iter()
        .flat_map(|s| s.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-')))
        .filter(|w| !w.is_empty())
        .map(str::to_string);
    let extra_words = extra
        .iter()
        .flat_map(|e| e.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-')))
        .filter(|w| !w.is_empty() && !is_stopword(w))
        .map(str::to_string);
    let named = path_tokens(rel_path)
        .split(' ')
        .map(str::to_string)
        .chain(symbols)
        .chain(extra_words)
        .chain(declared)
        .collect::<Vec<_>>();
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

/// One `code_symbols` row (path, name, kind, line, end_line, is_def, docs).
fn symbol_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<crate::codeindex::Symbol> {
    Ok(crate::codeindex::Symbol {
        path: r.get(0)?,
        name: r.get(1)?,
        kind: r.get(2)?,
        line: r.get(3)?,
        end_line: r.get(4)?,
        is_def: r.get::<_, i64>(5)? != 0,
        docs: r.get(6)?,
    })
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
            // The last word is an as-you-type prefix once it has two
            // letters: a one-letter prefix matches most of the index.
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

#[cfg(test)]
mod tests {
    use super::*;

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
            end_line: seq * 10 + 1,
            title: String::new(),
            symbol: None,
        }
    }

    #[test]
    fn a_chunk_keeps_its_line_and_it_moves_when_text_is_added_above() {
        let mut db = Db::open_in_memory().unwrap();
        db.upsert_chunks("a.md", &[chunk(0, "one", "h0"), chunk(1, "two", "h1")])
            .unwrap();
        let id: i64 = db
            .conn
            .query_row("SELECT id FROM chunks WHERE path='a.md' AND seq=1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(db.chunk_line(id).unwrap(), Some(11));
        let mut moved = chunk(1, "two", "h1");
        moved.line = 40;
        let changed = db
            .upsert_chunks("a.md", &[chunk(0, "one", "h0"), moved])
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
            .upsert_chunks("a.md", &chunks)
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
        let names = fts_names("data/equipment/WeaponBases.json", None, "{ \"CritMultiplier\": 1.5, \"MAX_ROWS\": 7, \"plain\": \"aGVsbG8gd29ybGQgdGhpcyBpcyBiYXNlNjQgZW5jb2RlZCBkYXRhIHRoYXQgaXMgbG9uZw\" }", &[]);
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
        db.upsert_chunks("data/equipment/WeaponBases.json", &[chunk(0, "{ \"Bases\": { \"Sword_T1\": { \"MinLow\": 4 } } }", "h0")])
            .unwrap();
        db.upsert_chunks("docs/notes.md", &[chunk(0, "Nothing about arms here.", "h1")])
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

    fn titled(seq: usize, title: &str, symbol: Option<&str>, text: &str) -> crate::chunker::Chunk {
        let mut c = chunk(seq, text, "");
        c.title = title.to_string();
        c.symbol = symbol.map(str::to_string);
        c.content_hash = crate::chunker::chunk_hash(title, symbol, text);
        c
    }

    /// A breadcrumb is searched, weighted above the body: a passage under
    /// "Retries › Backoff" answers "backoff" though its sentences never say
    /// it, ahead of a page that says it once in passing.
    #[test]
    fn the_title_field_is_searched_and_weighted_above_the_body() {
        let mut db = Db::open_in_memory().unwrap();
        db.upsert_chunks("Design/Payments.md", &[titled(0, "Payments › Retries › Backoff", None, "Each wait doubles, to a minute at most.")])
            .unwrap();
        db.upsert_chunks(
            "Research/Notes.md",
            &[titled(0, "", None, "Notes from the call: queues, workers, dashboards, alerts, and a word on backoff among many other things discussed.")],
        )
        .unwrap();
        let hits = db.search_chunks_fts("backoff", 5).unwrap();
        assert_eq!(hits.first().map(|h| h.path.as_str()), Some("Design/Payments.md"), "{hits:?}");
        // The snippet is the chunk's own text, never the breadcrumb.
        assert_eq!(hits[0].text, "Each wait doubles, to a minute at most.");
    }

    #[test]
    fn a_code_chunks_symbol_is_in_its_names() {
        let mut db = Db::open_in_memory().unwrap();
        db.upsert_chunks("src/books.rs", &[titled(0, "Settles the month.", Some("Ledger.reconcile"), "fn x() {}")])
            .unwrap();
        assert_eq!(db.search_chunks_fts("reconcile ledger", 5).unwrap().len(), 1);
        assert_eq!(db.search_chunks_fts("settles the month", 5).unwrap().len(), 1, "the doc comment is the title");
        // What is embedded carries the path, the symbol and the doc comment.
        let (_, embed) = db.chunks_missing_vectors("src/books.rs").unwrap().remove(0);
        assert_eq!(embed, "src/books.rs › Ledger.reconcile\nSettles the month.\n\nfn x() {}");
    }

    #[test]
    fn summaries_are_cached_by_hash_and_searched_but_not_shown() {
        let mut db = Db::open_in_memory().unwrap();
        for path in ["Design/Payments.md", "src/pay.rs", "Inbox/raw.md"] {
            db.upsert_file(path, "md", 1, 1, "indexed", None, "x").unwrap();
        }
        let page = titled(0, "Payments › Retries", None, "Each wait doubles.");
        db.upsert_chunks("Design/Payments.md", std::slice::from_ref(&page)).unwrap();
        db.upsert_chunks("src/pay.rs", &[titled(0, "", Some("pay"), "fn pay() {}")]).unwrap();
        db.upsert_chunks("Inbox/raw.md", &[titled(0, "Raw", None, "A transcript.")]).unwrap();
        let wanted = db.chunks_wanting_summaries(10).unwrap();
        assert_eq!(wanted.len(), 1, "pages only, not code or raw sources: {wanted:?}");
        assert_eq!(wanted[0].0, page.content_hash);

        db.store_chunk_summaries(&[(page.content_hash.clone(), "How payment retries back off exponentially.".into())], 7).unwrap();
        assert!(db.chunks_wanting_summaries(10).unwrap().is_empty());
        let hits = db.search_chunks_fts("exponentially", 5).unwrap();
        assert_eq!(hits.first().map(|h| h.text.as_str()), Some("Each wait doubles."), "found by its summary, shown by its text");
        let (_, embed) = db.chunks_missing_vectors("Design/Payments.md").unwrap().remove(0);
        assert_eq!(embed, "Payments › Retries\nHow payment retries back off exponentially.\n\nEach wait doubles.");

        // The same text written again finds its summary in the cache.
        db.delete_chunks("Design/Payments.md").unwrap();
        db.upsert_chunks("Design/Payments.md", &[page]).unwrap();
        assert_eq!(db.search_chunks_fts("exponentially", 5).unwrap().len(), 1);
    }

    #[test]
    fn schema_23_adds_the_title_field_and_marks_prose_and_code_to_read_again() {
        let mut db = Db::open_in_memory().unwrap();
        for path in ["notes.md", "src/a.rs", "data.json", "src/b.lua"] {
            db.upsert_file(path, "code", 1, 5, "indexed", None, "x").unwrap();
            db.set_file_byte_hash(path, "abc", 5).unwrap();
        }
        db.upsert_chunks("notes.md", &[chunk(0, "plain words", "h0")]).unwrap();
        db.conn
            .execute_batch(
                "DROP TABLE chunks_fts; CREATE VIRTUAL TABLE chunks_fts USING fts5(text, names);
                 INSERT INTO chunks_fts(rowid, text, names) SELECT id, text, '' FROM chunks;
                 UPDATE meta SET value = '22' WHERE key = 'schema_version';",
            )
            .unwrap();
        db.migrate().unwrap();
        let title: String = db.conn.query_row("SELECT title FROM chunks_fts", [], |r| r.get(0)).unwrap();
        assert_eq!(title, "");
        assert_eq!(db.search_chunks_fts("plain words", 5).unwrap().len(), 1, "filled again from the stored chunks");
        for (path, again) in [("notes.md", true), ("src/a.rs", true), ("data.json", false), ("src/b.lua", false)] {
            assert_eq!(db.get_file(path).unwrap().unwrap().mtime == -1, again, "{path}");
        }
        let chunked = crate::chunker::Chunked { chunks: vec![chunk(0, "plain words", "h0")], capped: true };
        db.store_chunked("notes.md", &chunked).unwrap();
        assert_eq!(db.index_health().unwrap().capped, 1, "a file cut at the cap is counted");
    }

    #[test]
    fn schema_16_rebuilds_the_keyword_index_and_marks_files_to_read_again() {
        let mut db = Db::open_in_memory().unwrap();
        for (path, status) in [("a/WeaponBases.json", "indexed"), ("src/Stash.java", "indexed"), ("notes.md", "indexed"), ("x.ts", "metadata_only")] {
            db.upsert_file(path, "code", 1, 5, status, None, "x").unwrap();
            db.set_file_byte_hash(path, "abc", 5).unwrap();
        }
        db.upsert_chunks("a/WeaponBases.json", &[chunk(0, "{ \"MinLow\": 4 }", "h0")])
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
        db.upsert_chunks("src/test/AmmoKeepDecisionTest.java", &sections).unwrap();
        db.upsert_chunks("src/main/AmmoKeep.java", &[chunk(0, "bolts stay loaded when the crossbow is put away", "a0")])
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
        db.upsert_chunks("src/chest/LootedWorldChest.java", &[chunk(0, "class LootedWorldChest { int x; }", "n0")])
            .unwrap();
        db.upsert_chunks(
            "src/chest/PoolRespawn.java",
            &[chunk(0, "a chest emptied this tick comes back on a later tick, never the next one", "t0")],
        )
        .unwrap();
        for i in 0..20 {
            db.upsert_chunks(&format!("src/other/Filler{i}.java"), &[chunk(0, &format!("unrelated code number {i}"), "f")])
                .unwrap();
        }
        // No chunk has every word, so the any-word search ranks them.
        let hits = db.search_chunks_fts("why doesn't a looted world chest come back on the next tick", 2).unwrap();
        assert_eq!(hits.first().map(|h| h.path.as_str()), Some("src/chest/PoolRespawn.java"), "{hits:?}");
    }

    /// Rewound to 16, every later migration runs too, and v23 reads prose
    /// and code with a grammar again, so the file left alone is one no
    /// migration reads again.
    #[test]
    fn schema_17_marks_prose_files_to_read_again() {
        let mut db = Db::open_in_memory().unwrap();
        for (path, status) in [("Reference/Error-Codes.md", "indexed"), ("logs/boot.txt", "indexed"), ("config/app.yaml", "indexed"), ("old.md", "metadata_only")] {
            db.upsert_file(path, "code", 1, 5, status, None, "x").unwrap();
            db.set_file_byte_hash(path, "abc", 5).unwrap();
        }
        db.conn.execute("UPDATE meta SET value = '16' WHERE key = 'schema_version'", []).unwrap();
        db.migrate().unwrap();
        for (path, again) in [("Reference/Error-Codes.md", true), ("logs/boot.txt", true), ("config/app.yaml", false), ("old.md", false)] {
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

    /// What v18 runs, on its own: rewound to 17, v23 reads every page again
    /// too, so the migration as a whole cannot show which pages are logs.
    #[test]
    fn schema_18_marks_log_pages_to_read_again() {
        let mut db = Db::open_in_memory().unwrap();
        for (path, text) in [("decisions/DECISIONS.md", decisions_log(8)), ("notes.md", "# Notes\n\n**D-001** once.\n".to_string())] {
            db.upsert_file(path, "md", 1, 5, "indexed", None, &text).unwrap();
            db.set_file_byte_hash(path, "abc", 5).unwrap();
        }
        assert_eq!(db.mark_log_pages_for_reindex().unwrap(), 1);
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
        db.upsert_chunks("decisions/DECISIONS.md", &chunks).unwrap();
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
        db.upsert_chunks("src/local_llm.rs", &chunks)
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
        db.upsert_chunks("src/util.rs", &chunks)
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
            .upsert_chunks("a.md", &first)
            .unwrap();
        assert_eq!(ids1.len(), 2);
        let id_seq0 = ids1[0].0;

        // Re-upsert with seq 0 unchanged (same hash) and seq 1 changed (new hash).
        let second = vec![chunk(0, "hello world", "h0"), chunk(1, "second chunk EDITED", "h1-new")];
        let changed = db
            .upsert_chunks("a.md", &second)
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
        db.upsert_chunks("a.md", &first)
            .unwrap();

        // Re-chunk down to a single chunk: seq 1 and 2 must disappear from
        // `chunks` and their `chunks_fts` mirror rows.
        let second = vec![chunk(0, "a", "h0")];
        db.upsert_chunks("a.md", &second)
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
            .upsert_chunks("a.md", &chunks)
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
                .upsert_chunks("a.md", &[chunk(0, "hello", "h0")])
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
            .upsert_chunks("a.md", &chunks)
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

    /// An index made before the code index gets one from its stored text, even
    /// when a scan since has mapped a file or two, and only once.
    #[test]
    fn the_code_index_backfills_every_unmapped_file_once() {
        let mut db = Db::open_in_memory().unwrap();
        for (p, t) in [("a.py", "def alpha():\n    beta()\n"), ("b.py", "def beta():\n    pass\n"), ("c.md", "# not code")] {
            db.upsert_file(p, "code", 1, 1, "indexed", None, t).unwrap();
        }
        let a = crate::codeindex::map_file("a.py", "def alpha():\n    beta()\n").unwrap();
        db.set_code_index("a.py", &a).unwrap(); // a scan re-read this one
        assert_eq!(db.backfill_code_index().unwrap(), 1, "b.py, not the mapped one or the page");
        assert_eq!(db.code_symbols(Some("beta"), None, Some(true), 5).unwrap().len(), 1);
        assert_eq!(db.backfill_code_index().unwrap(), 0, "once");
    }

    #[test]
    fn ocr_queue_enqueue_next_and_done_makes_text_searchable() {
        let mut db = Db::open_in_memory().unwrap();
        // An image indexes as metadata-only with no content — not searchable yet.
        db.upsert_file("scans/receipt.png", "image", 500, 1, "metadata_only", None, "")
            .unwrap();
        assert!(db.search_chunks_fts("espresso", 5).unwrap().is_empty());

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
            .search_chunks_fts("espresso", 5)
            .unwrap()
            .iter()
            .any(|h| h.path == "scans/receipt.png"));
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
    fn index_health_is_one_line_of_counts() {
        let mut db = Db::open_in_memory().unwrap();
        assert_eq!(db.index_health().unwrap(), IndexHealth::default());
        db.upsert_file("a.md", "md", 1, 1, "indexed", None, "alpha").unwrap();
        db.upsert_chunks("a.md", &[chunk(0, "alpha", "h0"), chunk(1, "beta", "h1")]).unwrap();
        // Failed after its third try; before it, a failed file is queued.
        for _ in 0..MAX_READ_ATTEMPTS {
            db.upsert_file("b.pdf", "pdf", 1, 1, "failed", Some("bad pdf"), "").unwrap();
        }
        db.upsert_file("e.pdf", "pdf", 1, 1, "failed", Some("bad pdf"), "").unwrap();
        db.upsert_file("c.png", "image", 1, 1, "metadata_only", None, "").unwrap();
        db.upsert_file("d.md", "md", 1, 1, "pending", None, "").unwrap();
        // The OCR backlog is not counted.
        assert!(db.enqueue_ocr_if_changed("c.png", "h1").unwrap());
        db.store_control_results(r#"[{"question":"q","expect":"a.md","rank":1,"passed":true},{"question":"r","expect":"z.md","rank":null,"passed":false}]"#)
            .unwrap();
        let h = db.index_health().unwrap();
        assert_eq!(
            h,
            IndexHealth {
                files: 5,
                indexed: 1,
                failed: 1,
                skipped: 1,
                queued: 2,
                capped: 0,
                chunks: 2,
                embedded: 0,
                controls_passed: 1,
                controls_total: 2,
                model_missing: false
            }
        );
    }

    #[test]
    fn the_health_line_says_what_matters_and_adds_up() {
        let mut h = IndexHealth { indexed: 412, failed: 3, chunks: 1000, embedded: 800, controls_passed: 4, controls_total: 5, capped: 2, ..Default::default() };
        assert_eq!(h.line(), "412 indexed, 3 failed · meaning 80% built · controls 4 of 5 passing · 2 too long to index whole");
        h.add(&IndexHealth { indexed: 8, chunks: 0, model_missing: true, ..Default::default() });
        assert_eq!(h.indexed, 420);
        assert_eq!(h.line(), "420 indexed, 3 failed · meaning search off: the embedding model is missing · controls 4 of 5 passing · 2 too long to index whole");
        assert_eq!(IndexHealth { indexed: 1, chunks: 1, embedded: 1, ..Default::default() }.line(), "1 indexed · meaning built · no controls");
    }

    #[test]
    fn a_pages_title_and_aliases_and_a_files_role_are_in_its_names() {
        let mut db = Db::open_in_memory().unwrap();
        db.upsert_file("Reference/Regions.md", "md", 1, 1, "indexed", None, "x").unwrap();
        db.upsert_chunks("Reference/Regions.md", &[chunk(0, "How the map is cut up.", "h0")]).unwrap();
        let meta = crate::pagemeta::PageMeta { title: Some("World regions".into()), aliases: vec!["shard layout".into()], ..Default::default() };
        db.set_page_meta("Reference/Regions.md", Some(&meta)).unwrap();
        let names = db.chunk_names("Reference/Regions.md").unwrap().join(" ");
        // A word the path already gave ("Regions") is not said twice, so the
        // title shows by its other word.
        assert!(names.contains("shard") && names.contains("layout") && names.contains("World"), "{names}");
        assert_eq!(db.search_chunks_fts("shard layout", 5).unwrap().len(), 1, "an alias finds its page through the names field");

        db.upsert_file("src/Pay.java", "code", 1, 1, "indexed", None, "class Pay {}").unwrap();
        db.upsert_chunks("src/Pay.java", &[chunk(0, "class Pay {}", "h1")]).unwrap();
        assert_eq!(db.set_file_roles(&[("src/Pay.java".into(), Some("billing".into()))]).unwrap(), vec!["src/Pay.java".to_string()]);
        assert!(db.chunk_names("src/Pay.java").unwrap()[0].contains("billing"));
        assert!(db.chunks_missing_vectors("src/Pay.java").unwrap()[0].1.starts_with("src/Pay.java (billing)"), "the role is in the embedded header");
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
    fn significant_tokens_drop_stopwords_but_keep_a_last_or_lone_one() {
        assert_eq!(
            significant_tokens("what is the operating model for the ai pilot"),
            vec!["operating", "model", "ai", "pilot"],
        );
        // The last word is kept for its as-you-type prefix.
        assert_eq!(significant_tokens("the"), vec!["the"]);
        // A question of stopwords only is kept whole.
        assert_eq!(significant_tokens("what is"), vec!["what", "is"]);
    }

    /// The file-level `search` index and its file-name column are gone
    /// (knowledge-core decision 15): every search reads the chunk index.
    #[test]
    fn the_file_level_search_index_is_gone() {
        let db = Db::open_in_memory().unwrap();
        assert!(!table_exists(&db.conn, "search").unwrap());
        assert!(table_exists(&db.conn, "chunks_fts").unwrap());
        let has_name = db
            .conn
            .prepare("SELECT 1 FROM pragma_table_info('contents') WHERE name = 'name'")
            .unwrap()
            .exists([])
            .unwrap();
        assert!(!has_name);
    }

    #[test]
    fn upsert_replaces_content() {
        let mut db = seeded();
        db.upsert_file("notes/meeting-jul-8.md", "md", 120, 2, "indexed", None,
            "Entirely new content about rollback rehearsal.")
            .unwrap();
        let text = db.get_text("notes/meeting-jul-8.md").unwrap().unwrap();
        assert!(text.contains("rollback") && !text.contains("Attendees"), "{text}");
        assert_eq!(db.file_count().unwrap(), 3);
    }

    #[test]
    fn remove_file_removes_its_text_and_chunks() {
        let mut db = seeded();
        db.upsert_chunks("knowledge/People.md", &[chunk(0, "Priya Natarajan owns billing cutover", "h0")])
            .unwrap();
        assert!(!db.search_chunks_fts("Natarajan", 10).unwrap().is_empty());
        db.remove_file("knowledge/People.md").unwrap();
        assert!(db.search_chunks_fts("Natarajan", 10).unwrap().is_empty());
        assert!(db.get_text("knowledge/People.md").unwrap().is_none());
        assert_eq!(db.file_count().unwrap(), 2);
    }

    /// A reindex clears `files` and scans again: the chunks of a file that is
    /// gone go with it, and the chunks of a file still there stay.
    #[test]
    fn orphan_chunks_are_dropped() {
        let mut db = seeded();
        db.upsert_chunks("knowledge/People.md", &[chunk(0, "Priya Natarajan", "h0")]).unwrap();
        db.upsert_chunks("ghost.md", &[chunk(0, "ghost content", "h1")]).unwrap();
        assert_eq!(db.drop_orphan_chunks().unwrap(), 1);
        assert!(db.search_chunks_fts("ghost", 5).unwrap().is_empty());
        assert!(!db.search_chunks_fts("Natarajan", 5).unwrap().is_empty());
    }

    #[test]
    fn remove_folder_removes_children_only() {
        let mut db = seeded();
        db.remove_folder("notes").unwrap();
        assert!(db.get_file("notes/meeting-jul-8.md").unwrap().is_none());
        assert!(db.get_file("knowledge/People.md").unwrap().is_some());
    }

    /// Renaming a folder rewrites every keyed row to the new prefix in place:
    /// no content is dropped, the chunk index keeps finding the same text at
    /// the new path, and a sibling folder that merely *starts with* the same characters
    /// (`docs2/` vs `docs/`) is left completely alone.
    #[test]
    fn rename_prefix_rewrites_subtree_and_leaves_string_siblings_alone() {
        let mut db = Db::open_in_memory().unwrap();
        db.upsert_file("docs/a.md", "md", 10, 1, "indexed", None, "alpha widget").unwrap();
        db.upsert_file("docs/deep/b.md", "md", 20, 2, "indexed", None, "beta widget").unwrap();
        db.upsert_file("docs2/c.md", "md", 30, 3, "indexed", None, "gamma widget").unwrap();
        for (path, text) in [("docs/a.md", "alpha widget"), ("docs/deep/b.md", "beta widget"), ("docs2/c.md", "gamma widget")] {
            db.upsert_chunks(path, &[chunk(0, text, path)]).unwrap();
        }
        crate::graph::set_topics(&db, "docs/a.md", &["billing".to_string()]).unwrap();
        crate::graph::set_topics(&db, "docs2/c.md", &["billing".to_string()]).unwrap();
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

        // Search still finds the same content, now reported at the new path,
        // and finds it by the new path's words.
        let hits = db.search_chunks_fts("widget", 10).unwrap();
        let paths: Vec<&str> = hits.iter().map(|h| h.path.as_str()).collect();
        assert!(paths.contains(&"Archive/docs/a.md"), "got {paths:?}");
        assert!(paths.contains(&"Archive/docs/deep/b.md"), "got {paths:?}");
        assert!(paths.contains(&"docs2/c.md"), "got {paths:?}");
        assert!(!paths.iter().any(|p| p.starts_with("docs/")), "got {paths:?}");
        let by_folder = db.search_chunks_fts("archive alpha", 10).unwrap();
        assert_eq!(by_folder.first().map(|h| h.path.as_str()), Some("Archive/docs/a.md"), "{by_folder:?}");

        // Side tables follow the rename, and so do the moved page's topic
        // tags; the sibling's do not move.
        assert!(db.get_ocr_regions("Archive/docs/deep/b.md").unwrap().len() == 1);
        assert!(db.get_ocr_regions("docs/deep/b.md").unwrap().is_empty());
        assert_eq!(crate::graph::topics_of(&db, "Archive/docs/a.md").unwrap(), vec!["billing".to_string()]);
        assert!(crate::graph::topics_of(&db, "docs/a.md").unwrap().is_empty());
        assert_eq!(crate::graph::topics_of(&db, "docs2/c.md").unwrap(), vec!["billing".to_string()]);
    }

    /// A single file rename goes through the same call: the row moves, and
    /// so do its chunks, found by the new name and no longer by the old.
    #[test]
    fn rename_prefix_renames_a_single_file_and_its_chunks() {
        let mut db = Db::open_in_memory().unwrap();
        db.upsert_file("notes/kickoff.md", "md", 10, 1, "indexed", None, "body text").unwrap();
        db.upsert_chunks("notes/kickoff.md", &[chunk(0, "body text", "h0")]).unwrap();

        assert_eq!(db.rename_prefix("notes/kickoff.md", "notes/launch-plan.md").unwrap(), 1);
        assert!(db.get_file("notes/kickoff.md").unwrap().is_none());
        assert!(db.get_file("notes/launch-plan.md").unwrap().is_some());

        let hits = db.search_chunks_fts("launch", 10).unwrap();
        assert_eq!(hits.len(), 1, "findable by the new name");
        assert_eq!(hits[0].path, "notes/launch-plan.md");
        assert!(db.search_chunks_fts("kickoff", 10).unwrap().is_empty(), "old name is gone");
    }

    #[test]
    fn clear_empties_everything() {
        let mut db = seeded();
        // A topic tag is a write's, not the scan's: it survives a reindex.
        crate::graph::set_topics(&db, "knowledge/People.md", &["people".to_string()]).unwrap();

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
        assert!(db.get_text("knowledge/People.md").unwrap().is_none());
        // From scratch: OCR regions and queue are gone too.
        assert!(db.get_ocr_regions("scan.png").unwrap().is_empty());
        assert!(db.next_pending_ocr().unwrap().is_none());
        assert_eq!(crate::graph::topics_of(&db, "knowledge/People.md").unwrap(), vec!["people".to_string()]);
    }

    #[test]
    fn hostile_query_is_safe() {
        let db = seeded();
        // FTS5 syntax characters must not panic or error.
        for q in ["\"unbalanced", "a AND OR NOT", "col:x", "(((", "*", "  "] {
            let _ = db.search_chunks_fts(q, 10).unwrap();
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
        assert!(ro.get_text("notes/a.md").unwrap().is_some());
        // SQLite itself refuses writes on this handle.
        assert!(ro
            .conn
            .execute("INSERT INTO meta(key, value) VALUES ('x', 'y')", [])
            .is_err());
        // A missing index is an open error, not an empty database.
        assert!(Db::open_read_only(base, Uuid::new_v4()).is_err());
    }

    // Search opens its read-only handle once at project activation, while
    // the writer keeps indexing. This asserts the structural guarantee that
    // relies on: a WAL reader opened BEFORE a write still sees that write
    // once committed. (The freedom from lock contention this buys is
    // structural; it can't be unit-tested.)
    #[test]
    fn read_only_handle_sees_writes_committed_after_it_opened() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let id = Uuid::new_v4();
        let mut writer = Db::open(base, id).unwrap();
        writer
            .upsert_file("notes/a.md", "md", 10, 1, "indexed", None, "billing cutover")
            .unwrap();

        // Reader opened while the writer is still live, before the next commit.
        let reader = Db::open_read_only(base, id).unwrap();
        assert!(reader.get_text("notes/a.md").unwrap().is_some());

        // A freshly-indexed file must be findable through the same reader.
        writer
            .upsert_file("notes/b.md", "md", 20, 2, "indexed", None, "quarterly forecast")
            .unwrap();
        writer.upsert_chunks("notes/b.md", &[chunk(0, "quarterly forecast", "h0")]).unwrap();
        let via_reader = reader.search_chunks_fts("forecast", 5).unwrap();
        let via_writer = writer.search_chunks_fts("forecast", 5).unwrap();
        assert_eq!(via_reader.len(), 1);
        assert_eq!(via_reader, via_writer);
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

    /// v26 (knowledge-core decision 21): the local reader's model goes
    /// (entities, their edges, events, the extraction queue and the build
    /// stamp) and the graph's edge table comes; the index itself survives.
    #[test]
    fn v25_db_migrates_to_v26() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.db");
        {
            let mut db = Db::open_at(&path).unwrap();
            db.upsert_file("notes/a.md", "md", 10, 1, "indexed", None, "kept").unwrap();
            db.conn
                .execute_batch(
                    "CREATE TABLE entities (id INTEGER PRIMARY KEY, name TEXT);
                     CREATE TABLE events (id INTEGER PRIMARY KEY, text TEXT);
                     CREATE TABLE extractions (rel_path TEXT PRIMARY KEY);
                     DROP TABLE graph_edges;
                     INSERT INTO meta(key, value) VALUES ('knowledge_model_built_at', '100');
                     UPDATE meta SET value = '25' WHERE key = 'schema_version';",
                )
                .unwrap();
        }
        let db = Db::open_at(&path).unwrap();
        for gone in ["entities", "entity_edges", "events", "extractions"] {
            assert!(!table_exists(&db.conn, gone).unwrap(), "{gone} is dropped");
        }
        assert!(table_exists(&db.conn, "graph_edges").unwrap());
        let stamp: Option<String> = db
            .conn
            .query_row("SELECT value FROM meta WHERE key = 'knowledge_model_built_at'", [], |r| r.get(0))
            .optional()
            .unwrap();
        assert!(stamp.is_none());
        assert_eq!(db.file_count().unwrap(), 1);
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

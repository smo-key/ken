# Wave 4 — Incremental Map Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the one-shot, whole-corpus Claude build of the knowledge model (Map + Timeline) with an incremental, per-file extraction pipeline powered by the embedded local LLM. Each file that reaches `indexed` is enqueued, extracted on a background worker, and merged into the persisted model with strict dedup/GC rules so edits, rewrites, and deletions converge instead of accreting. The old Claude one-shot survives only as a manual **Deep rebuild** button that replaces the whole model and no longer auto-runs.

**Architecture:** A new `extractions` bookkeeping table (one row per `rel_path`) tracks each file's last-extracted content hash and status. `scan::index_one` enqueues a file (marks it `pending`) when its content hash changes. One background worker thread per open project drains the pending queue: for each file it composes a per-file prompt over the file's already-extracted `contents` text (truncated to a token budget), asks the local LLM for strict JSON, parses it into a per-file delta, and calls `Db::merge_knowledge_delta`, which is the heart of the system — it removes the file's prior contribution, GCs orphaned entities/edges, then merges the delta with entity dedup (kind + normalized name), source union, longer-summary-wins, unordered-pair edge dedup, and (date,text) event dedup. After each merged file the backend emits a throttled `knowledge-updated` event; the frontend reloads and shows a coverage line ("N of M files analyzed"). The merge logic and worker core live in `ken-core` behind a `Fn(&str) -> Result<Value>` generation seam so they are fully unit-testable with no LLM; the real seam (constructed in `src-tauri`) calls `local_llm::generate_json(prompt, Priority::Background)`.

**Tech Stack:** Rust (`ken-core` crate: `rusqlite` 0.38 bundled SQLite, `serde_json`), Tauri 2 (`src-tauri`), Svelte 5 runes (`src/`). Tests: `cargo test -p ken-core` (Rust unit tests against `Db::open_in_memory`), `pnpm vitest run` (frontend). No new crate dependencies (content hashing is an inline FNV-1a).

## Global Constraints

- **Per-file sanity caps (exact):** `FILE_MAX_ENTITIES = 40`, `FILE_MAX_RELATIONS = 60`, `FILE_MAX_EVENTS = 20`. Applied inside `parse_delta_value` (entities/events) and during relation resolution (relations). **No global entity cap** — the old `MAX_ENTITIES = 200` applies only to the manual Deep-rebuild path (`parse_extraction`, unchanged).
- **Token budget:** `EXTRACT_CHAR_BUDGET = 24_000` chars (~6k tokens at 4 chars/token) — the file's extracted text is truncated to this before prompting.
- **Entity identity key:** `kind` + `\u{1}` + whitespace-normalized, lowercased name (`name.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()`). `kind` is already lowercased by the parser.
- **Merge rules:** on entity match → union `sources` (add the file's `rel_path` if absent) and keep the longer summary (by `chars().count()`). Edges dedup by unordered `(min(a,b), max(a,b))` pair, first label wins (existing edges and earlier delta edges both win over later). Events dedup by `(date, text)`. Re-extraction: first `DELETE FROM events WHERE source = rel_path`, remove `rel_path` from every entity's `sources`, delete entities whose `sources` become empty (edges cascade via the `entity_edges` FK `ON DELETE CASCADE`), THEN apply the delta.
- **`extractions` table schema (exact):**
  ```sql
  CREATE TABLE IF NOT EXISTS extractions (
      rel_path     TEXT PRIMARY KEY,
      content_hash TEXT NOT NULL,
      extracted_at INTEGER,
      status       TEXT NOT NULL,   -- 'pending' | 'done' | 'error'
      error        TEXT
  );
  ```
  Migration is `SCHEMA_VERSION` bump `7 → 8` in `crates/ken-core/src/db.rs`.
- **Event name (exact):** `knowledge-updated` (unit payload). Backend emits it after each merged file, throttled to at most once per 750 ms; the frontend also throttles the reload it triggers.
- **JSON delta shape (exact, per spec §2):** `{entities:[{kind,name,summary}], relations:[{a,b,label}], events:[{date,category,text}]}`. `a`/`b` in relations name entities by display name (resolved case-insensitively, like the current parser). No `sources`/`source` fields in the per-file JSON — the merge injects `rel_path`.
- **Dependency note:** This plan **consumes the local_llm contract** (`crates/ken-core/src/local_llm.rs`): `pub enum Priority { Interactive, Background }`, `pub fn generate_json(prompt: &str, priority: Priority) -> Result<serde_json::Value>`, `pub fn llm_status() -> LlmStatus` (`NotInstalled | Ready | Error(String)`). It is **buildable before that lands only behind the seam**: all `ken-core` logic (Tasks 1–7) is written against a `Fn(&str) -> Result<serde_json::Value>` closure and needs no `local_llm` module. Only the `src-tauri` wiring (Task 8) references `ken_core::local_llm::*`; that task depends on the local-llm plan landing.
- **Design copy** obeys `.impeccable.md`: plain first-person language, no jargon. The paused notice reads "Ken maps your project on your Mac. Waiting for the local model to be ready…" — never an error banner (nobody asked for it).

## File Structure

```
crates/ken-core/src/
  db.rs                 # NEW: extractions table (migration v8), merge_knowledge_delta,
                        #      purge_file_knowledge, strip_file helper, extraction queue
                        #      methods, extraction_coverage; remove_file/clear updated
  knowledge_model.rs    # NEW: FILE_MAX_* caps, content_hash, compose_file_prompt,
                        #      parse_delta_value, extract_one, process_next_pending
  scan.rs               # index_one enqueues on indexed+changed-hash
src-tauri/src/
  lib.rs                # retire KNOWLEDGE_TICK auto-build; spawn extraction worker per
                        #      project; knowledge_model DTO gains coverage + llm status;
                        #      emit throttled knowledge-updated
src/
  lib/api.ts            # KnowledgeModel gains analyzed/total/llm fields; onKnowledgeUpdated
  lib/knowledge.svelte.ts # throttled knowledge-updated listener; coverage/llm getters
  screens/MapScreen.svelte # coverage line, paused notice, "Deep rebuild" button
```

---

## Task 1 — `extractions` table + queue methods (migration v8)

**Files:** `crates/ken-core/src/db.rs` (SCHEMA_VERSION `:14`; migration block after `:273`; new methods near the knowledge section `:926`).

**Interfaces — Produces:**
- `Db::enqueue_extraction_if_changed(&mut self, rel_path: &str, content_hash: &str) -> Result<bool>` — upserts a `pending` row unless an identical hash is already `done`; returns whether it enqueued.
- `Db::next_pending_extraction(&self) -> Result<Option<(String, String)>>` — oldest pending `(rel_path, content_hash)` by insertion order.
- `Db::mark_extraction_done(&mut self, rel_path: &str, content_hash: &str, at: i64) -> Result<()>`
- `Db::mark_extraction_error(&mut self, rel_path: &str, message: &str, at: i64) -> Result<()>`
- `Db::extraction_coverage(&self) -> Result<(i64, i64)>` — `(files_extracted_done, indexed_file_count)`.
- `Db::remove_extraction(&mut self, rel_path: &str) -> Result<()>`

### Step 1.1 — Failing test for the queue lifecycle

- [ ] Add to the `#[cfg(test)] mod tests` block at the bottom of `crates/ken-core/src/db.rs`:

```rust
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
        db.mark_extraction_error("b.md", "boom", 101).unwrap();
        db.mark_extraction_done("a.md", "h1b", 102).unwrap();
        assert_eq!(db.next_pending_extraction().unwrap(), None);

        // Removing an extraction row drops it entirely.
        db.remove_extraction("a.md").unwrap();
        assert_eq!(db.extraction_coverage().unwrap(), (0, 2));
    }
```

- [ ] Run: `cargo test -p ken-core extraction_queue_tracks_pending_done_and_coverage`
  **Expected:** fails to compile — `enqueue_extraction_if_changed` etc. do not exist.

### Step 1.2 — Migration v8 + methods

- [ ] Bump the schema version in `crates/ken-core/src/db.rs:14`:

```rust
pub const SCHEMA_VERSION: i64 = 8;
```

- [ ] Add the migration block immediately after the `if version < 7 { … }` block (before the `INSERT OR REPLACE INTO meta … 'schema_version'` at `:274`):

```rust
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
```

- [ ] Add the queue methods in `crates/ken-core/src/db.rs` just before the `// --- knowledge model …` comment at `:926`:

```rust
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
        self.conn.execute(
            "INSERT INTO extractions (rel_path, content_hash, status)
             VALUES (?1, ?2, 'pending')
             ON CONFLICT(rel_path) DO UPDATE SET
               content_hash = ?2, status = 'pending', error = NULL",
            params![rel_path, content_hash],
        )?;
        Ok(true)
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

    pub fn mark_extraction_done(
        &mut self,
        rel_path: &str,
        content_hash: &str,
        at: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO extractions (rel_path, content_hash, extracted_at, status)
             VALUES (?1, ?2, ?3, 'done')
             ON CONFLICT(rel_path) DO UPDATE SET
               content_hash = ?2, extracted_at = ?3, status = 'done', error = NULL",
            params![rel_path, content_hash, at],
        )?;
        Ok(())
    }

    pub fn mark_extraction_error(
        &mut self,
        rel_path: &str,
        message: &str,
        at: i64,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE extractions
             SET status = 'error', error = ?2, extracted_at = ?3
             WHERE rel_path = ?1",
            params![rel_path, message, at],
        )?;
        Ok(())
    }

    /// `(files extracted, indexed files)` — the Map coverage line's numerator
    /// and denominator.
    pub fn extraction_coverage(&self) -> Result<(i64, i64)> {
        let done: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM extractions WHERE status = 'done'",
            [],
            |r| r.get(0),
        )?;
        Ok((done, self.indexed_file_count()?))
    }

    pub fn remove_extraction(&mut self, rel_path: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM extractions WHERE rel_path = ?1",
            params![rel_path],
        )?;
        Ok(())
    }
```

- [ ] Run: `cargo test -p ken-core extraction_queue_tracks_pending_done_and_coverage`
  **Expected:** passes.

- [ ] Commit: `git commit -am "feat(map): extractions queue table + methods (migration v8)"`

---

## Task 2 — Content hash + per-file caps + prompt (pure helpers)

**Files:** `crates/ken-core/src/knowledge_model.rs` (constants near `:26`; new pub fns after `compose_extraction_prompt` at `:99`).

**Interfaces — Produces:**
- `pub const FILE_MAX_ENTITIES: usize = 40; pub const FILE_MAX_RELATIONS: usize = 60; pub const FILE_MAX_EVENTS: usize = 20;`
- `pub const EXTRACT_CHAR_BUDGET: usize = 24_000;`
- `pub fn content_hash(text: &str) -> String` — 16-hex-digit FNV-1a, deterministic.
- `pub fn compose_file_prompt(rel_path: &str, text: &str, today: &str) -> String`

### Step 2.1 — Failing tests

- [ ] Add to `#[cfg(test)] mod tests` in `crates/ken-core/src/knowledge_model.rs`:

```rust
    #[test]
    fn content_hash_is_stable_and_sensitive() {
        assert_eq!(content_hash("hello"), content_hash("hello"));
        assert_ne!(content_hash("hello"), content_hash("hello "));
        // 16 lowercase hex digits.
        let h = content_hash("anything");
        assert_eq!(h.len(), 16);
        assert!(h.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn file_prompt_is_single_file_and_strict() {
        let p = compose_file_prompt("notes/kickoff.md", "We hired Priya.", "2026-07-14");
        assert!(p.contains("notes/kickoff.md"));
        assert!(p.contains("We hired Priya."));
        assert!(p.contains("ONLY a JSON object"));
        assert!(p.contains("\"relations\""));
        assert!(p.contains("\"events\""));
        assert!(p.contains("person|organization|topic|decision|other"));
        assert!(p.contains("yyyy-mm-dd"));
        assert!(p.contains("2026-07-14"));
        // Per-file caps are stated so the model self-limits.
        assert!(p.contains("40 entities"));
    }

    #[test]
    fn file_prompt_truncates_to_budget() {
        let big = "x".repeat(EXTRACT_CHAR_BUDGET + 5_000);
        let p = compose_file_prompt("big.md", &big, "2026-07-14");
        // The document body is capped; the surrounding instructions are small.
        assert!(p.matches('x').count() <= EXTRACT_CHAR_BUDGET);
    }
```

- [ ] Run: `cargo test -p ken-core -- content_hash_is_stable file_prompt_is_single file_prompt_truncates`
  **Expected:** fails to compile.

### Step 2.2 — Implement

- [ ] Add the caps and budget constants after `const MAX_EVENTS: usize = 150;` at `crates/ken-core/src/knowledge_model.rs:27`:

```rust
/// Per-file sanity caps for incremental extraction — bound the junk one bad
/// generation can inject. Deliberately NOT a global cap; the whole model grows
/// with the corpus.
pub const FILE_MAX_ENTITIES: usize = 40;
pub const FILE_MAX_RELATIONS: usize = 60;
pub const FILE_MAX_EVENTS: usize = 20;

/// The file's extracted text is truncated to this many characters before
/// prompting — ~6k tokens at 4 chars/token, comfortably inside the local
/// model's context alongside the instructions.
pub const EXTRACT_CHAR_BUDGET: usize = 24_000;
```

- [ ] Add the helpers after `compose_extraction_prompt` (after `:99`):

```rust
/// A deterministic 64-bit FNV-1a of the extracted text, hex-encoded. Stored in
/// `extractions.content_hash` to detect when a re-index actually changed a
/// file's content (mtime/size churn without content change is common on sync
/// clients, and must not re-run extraction).
pub fn content_hash(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    format!("{h:016x}")
}

/// The per-file extraction prompt: read ONE file's already-extracted text and
/// answer with a single strict-JSON delta. Shapes match `parse_delta_value`:
/// entities carry no sources (the merge attributes them to this file), and
/// relations name entities by their display name.
pub fn compose_file_prompt(rel_path: &str, text: &str, today: &str) -> String {
    let body: String = if text.chars().count() > EXTRACT_CHAR_BUDGET {
        text.chars().take(EXTRACT_CHAR_BUDGET).collect()
    } else {
        text.to_string()
    };
    format!(
        "You are Ken, extracting the knowledge in ONE document for a project's \
Map and Timeline. Today's date is {today}.\n\n\
Read the document below (path: {rel_path}) and output ONLY a JSON object — no \
prose before or after, no code fences — shaped exactly like this:\n\
{{\n\
  \"entities\": [\n\
    {{\"kind\": \"person|organization|topic|decision|other\", \
\"name\": \"short display name\", \"summary\": \"one plain sentence\"}}\n\
  ],\n\
  \"relations\": [\n\
    {{\"a\": \"one entity's name\", \"b\": \"another entity's name\", \
\"label\": \"short relation\"}}\n\
  ],\n\
  \"events\": [\n\
    {{\"date\": \"yyyy-mm-dd\", \"category\": \"one lowercase word\", \
\"text\": \"one plain sentence\"}}\n\
  ]\n\
}}\n\n\
Rules:\n\
- At most {FILE_MAX_ENTITIES} entities, {FILE_MAX_RELATIONS} relations, and \
{FILE_MAX_EVENTS} events — only what THIS document grounds; never invent.\n\
- relations.a and relations.b must each name an entity in your list.\n\
- Dates are best effort — omit events with no inferable yyyy-mm-dd date.\n\n\
Document:\n{body}\n"
    )
}
```

- [ ] Run: `cargo test -p ken-core -- content_hash_is_stable file_prompt_is_single file_prompt_truncates`
  **Expected:** passes.

- [ ] Commit: `git commit -am "feat(map): per-file caps, content hash, single-file prompt"`

---

## Task 3 — `parse_delta_value` (strict-JSON → per-file delta)

**Files:** `crates/ken-core/src/knowledge_model.rs` (new pub fn after `parse_extraction` at `:200`).

**Interfaces — Consumes:** `serde_json::Value`, the existing helpers `non_empty_str`, `valid_date`, `ENTITY_KINDS`. **Produces:** `pub fn parse_delta_value(value: &serde_json::Value) -> Extraction`. Reuses the existing `Extraction { entities: Vec<EntityInput>, events: Vec<EventInput> }` — connections are carried on `EntityInput.connections` as `(other_index, label)`.

Semantics (mirrors `parse_extraction` hygiene, per-file caps, `relations` array instead of nested `connections`, no `sources`/`source` in JSON — those are injected at merge):
- Entities: `name` required (drop nameless), `kind` coerced to one of `ENTITY_KINDS` (default `other`), `summary` trimmed, `sources` left empty. Capped at `FILE_MAX_ENTITIES`.
- Relations: resolve `a`/`b` to entity indices by case-insensitive name; drop dangling and self; dedup unordered pairs (first label wins); cap total at `FILE_MAX_RELATIONS`.
- Events: `text` required, `date` must pass `valid_date`, `category` normalized to one lowercase word (default `other`), `source` left empty. Capped at `FILE_MAX_EVENTS`.
- Infallible: a malformed/empty value yields an empty `Extraction` (unlike `parse_extraction`, which errors on no-JSON — here the caller already holds a parsed `Value`).

### Step 3.1 — Failing test

- [ ] Add to `#[cfg(test)] mod tests` in `crates/ken-core/src/knowledge_model.rs`:

```rust
    #[test]
    fn parse_delta_resolves_relations_and_enforces_caps() {
        let v: serde_json::Value = serde_json::from_str(r#"{
            "entities": [
                {"kind": "person", "name": "Priya N.", "summary": "Owns billing."},
                {"kind": "topic", "name": "Billing cutover", "summary": "The migration."},
                {"kind": "person"},
                {"kind": "sorcerer", "name": "LangdonSoft", "summary": "Vendor."}
            ],
            "relations": [
                {"a": "priya n.", "b": "BILLING CUTOVER", "label": "owns"},
                {"a": "Billing cutover", "b": "Priya N.", "label": "dup pair"},
                {"a": "Priya N.", "b": "Priya N.", "label": "self"},
                {"a": "Priya N.", "b": "Nobody", "label": "dangling"}
            ],
            "events": [
                {"date": "2026-07-11", "category": "Decision Made", "text": "Sign-off."},
                {"date": "sometime", "category": "x", "text": "bad date dropped"},
                {"date": "2026-06-01", "text": ""}
            ]
        }"#).unwrap();
        let ex = parse_delta_value(&v);
        assert_eq!(ex.entities.len(), 3, "nameless dropped");
        assert_eq!(ex.entities[2].kind, "other", "unknown kind coerced");
        assert!(ex.entities[0].sources.is_empty(), "sources injected at merge");
        // Relation resolved once (reverse duplicate + self + dangling dropped).
        assert_eq!(ex.entities[0].connections, vec![(1, "owns".to_string())]);
        assert!(ex.entities[1].connections.is_empty());
        assert_eq!(ex.events.len(), 1);
        assert_eq!(ex.events[0].category, "decision");
        assert!(ex.events[0].source.is_empty());
    }

    #[test]
    fn parse_delta_caps_are_per_file() {
        let entities: Vec<String> = (0..60)
            .map(|i| format!(r#"{{"kind":"topic","name":"E{i}","summary":""}}"#))
            .collect();
        let events: Vec<String> = (0..40)
            .map(|i| format!(r#"{{"date":"2026-01-{:02}","category":"x","text":"e{i}"}}"#, i % 28 + 1))
            .collect();
        let v: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"entities":[{}],"events":[{}]}}"#,
            entities.join(","), events.join(",")
        )).unwrap();
        let ex = parse_delta_value(&v);
        assert_eq!(ex.entities.len(), FILE_MAX_ENTITIES);
        assert_eq!(ex.events.len(), FILE_MAX_EVENTS);
    }

    #[test]
    fn parse_delta_empty_is_empty() {
        let ex = parse_delta_value(&serde_json::json!({}));
        assert!(ex.entities.is_empty() && ex.events.is_empty());
    }
```

- [ ] Run: `cargo test -p ken-core -- parse_delta_resolves parse_delta_caps parse_delta_empty`
  **Expected:** fails to compile.

### Step 3.2 — Implement

- [ ] Add after `parse_extraction` (after `:200`) in `crates/ken-core/src/knowledge_model.rs`:

```rust
/// Parse an already-decoded per-file extraction `Value` into a delta. Same
/// hygiene as `parse_extraction` (name-required, kind coercion, dangling/self
/// edge drops, unordered-pair dedup, date validation) but with per-file caps,
/// a top-level `relations` array (a/b name entities), and no sources/source in
/// the JSON — the merge attributes everything to the file being extracted.
/// Infallible: a malformed value yields an empty delta.
pub fn parse_delta_value(value: &serde_json::Value) -> Extraction {
    let empty = Vec::new();
    let mut entities: Vec<EntityInput> = Vec::new();
    for item in value["entities"].as_array().unwrap_or(&empty) {
        if entities.len() >= FILE_MAX_ENTITIES {
            break;
        }
        let Some(name) = non_empty_str(&item["name"]) else {
            continue;
        };
        let kind = item["kind"]
            .as_str()
            .map(|k| k.trim().to_lowercase())
            .filter(|k| ENTITY_KINDS.contains(&k.as_str()))
            .unwrap_or_else(|| "other".into());
        entities.push(EntityInput {
            kind,
            name,
            summary: item["summary"].as_str().unwrap_or("").trim().to_string(),
            sources: Vec::new(),
            connections: Vec::new(),
        });
    }

    // Resolve relations by case-insensitive name; drop dangling/self; collapse
    // duplicate unordered pairs (first label wins); cap total relations.
    let mut by_name: std::collections::HashMap<String, usize> = Default::default();
    for (i, e) in entities.iter().enumerate() {
        by_name.entry(e.name.to_lowercase()).or_insert(i);
    }
    let mut seen: std::collections::HashSet<(usize, usize)> = Default::default();
    let mut relation_count = 0usize;
    for rel in value["relations"].as_array().unwrap_or(&empty) {
        if relation_count >= FILE_MAX_RELATIONS {
            break;
        }
        let (Some(a_name), Some(b_name)) =
            (non_empty_str(&rel["a"]), non_empty_str(&rel["b"]))
        else {
            continue;
        };
        let (Some(&a), Some(&b)) = (
            by_name.get(&a_name.to_lowercase()),
            by_name.get(&b_name.to_lowercase()),
        ) else {
            continue;
        };
        if a == b {
            continue;
        }
        let pair = (a.min(b), a.max(b));
        if !seen.insert(pair) {
            continue;
        }
        let label = rel["label"].as_str().unwrap_or("").trim().to_string();
        entities[a].connections.push((b, label));
        relation_count += 1;
    }

    let mut events: Vec<EventInput> = Vec::new();
    for item in value["events"].as_array().unwrap_or(&empty) {
        if events.len() >= FILE_MAX_EVENTS {
            break;
        }
        let Some(text) = non_empty_str(&item["text"]) else {
            continue;
        };
        let Some(date) = item["date"].as_str().map(str::trim).filter(|d| valid_date(d))
        else {
            continue;
        };
        let category = item["category"]
            .as_str()
            .and_then(|c| c.to_lowercase().split_whitespace().next().map(String::from))
            .unwrap_or_else(|| "other".into());
        events.push(EventInput {
            date: date.to_string(),
            category,
            text,
            source: String::new(),
        });
    }

    Extraction { entities, events }
}
```

- [ ] Run: `cargo test -p ken-core -- parse_delta_resolves parse_delta_caps parse_delta_empty`
  **Expected:** passes.

- [ ] Commit: `git commit -am "feat(map): parse_delta_value with per-file caps and relation resolution"`

---

## Task 4 — `Db::merge_knowledge_delta` + `purge_file_knowledge` (THE HEART)

**Files:** `crates/ken-core/src/db.rs` (new methods + private `strip_file` helper after `replace_knowledge_model` at `:977`; `remove_file` and `clear` updated).

**Interfaces — Produces:**
- `pub fn merge_knowledge_delta(&mut self, rel_path: &str, delta: &knowledge_model::Extraction, at: i64) -> Result<()>`
- `pub fn purge_file_knowledge(&mut self, rel_path: &str) -> Result<()>`
- private `fn strip_file(conn: &Connection, rel_path: &str) -> Result<()>` (used by both, and by `remove_file`).
- private `fn entity_key(kind: &str, name: &str) -> String` (identity key).

Import note: `merge_knowledge_delta` takes `&knowledge_model::Extraction`; add `use crate::knowledge_model;` at the top of `db.rs` (it already has `use crate::{Error, Result};` at `:12`).

### Step 4.1 — Failing tests (dedup, source-union, longer-summary, re-extract, GC cascade, edge/event dedup)

- [ ] Add a helper + tests to `#[cfg(test)] mod tests` in `crates/ken-core/src/db.rs`:

```rust
    use crate::db::EntityInput;
    use crate::knowledge_model::Extraction;

    /// Build a one-entity/one-event delta quickly (connections empty).
    fn ent(kind: &str, name: &str, summary: &str) -> EntityInput {
        EntityInput {
            kind: kind.into(),
            name: name.into(),
            summary: summary.into(),
            sources: Vec::new(),
            connections: Vec::new(),
        }
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
            events: vec![crate::db::EventInput {
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
            events: vec![crate::db::EventInput {
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
            events: vec![crate::db::EventInput {
                date: "2026-07-11".into(), category: "x".into(),
                text: "e".into(), source: String::new(),
            }],
        }, 10).unwrap();
        db.purge_file_knowledge("gone.md").unwrap();
        let (entities, _) = db.list_entities_with_edges().unwrap();
        assert!(entities.is_empty());
        assert!(db.list_events().unwrap().is_empty());
    }
```

- [ ] Run: `cargo test -p ken-core -- merge_ re_extraction_removes purge_file_knowledge`
  **Expected:** fails to compile — methods missing.

### Step 4.2 — Implement `strip_file`, `merge_knowledge_delta`, `purge_file_knowledge`

- [ ] Add `use crate::knowledge_model;` to the imports at the top of `crates/ken-core/src/db.rs` (after `use crate::{Error, Result};` at `:12`).

- [ ] Add these methods after `replace_knowledge_model` (after `:977`) in `crates/ken-core/src/db.rs`:

```rust
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
        let tx = self.conn.transaction()?;
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
        let tx = self.conn.transaction()?;
        strip_file(&tx, rel_path)?;
        tx.commit()?;
        Ok(())
    }
```

- [ ] Add the two free helpers near the other private `db.rs` helpers (e.g. just above `fn name_tokens` / the `EntityRow` structs — anywhere at module scope, not inside `impl Db`):

```rust
/// The entity identity used by incremental merge: kind plus a
/// whitespace-normalized, case-folded name. `\u{1}` can't occur in a real
/// name, so it's a safe key separator.
fn entity_key(kind: &str, name: &str) -> String {
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    format!("{kind}\u{1}{name}")
}

/// Strip one file's prior contribution to the knowledge model, in the caller's
/// transaction: delete its events, remove it from every entity's `sources`,
/// and delete entities left with no sources (their edges cascade via the
/// `entity_edges` FK `ON DELETE CASCADE`).
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
```

Note: `Connection` is already imported at `db.rs:8` (`use rusqlite::{params, Connection};`), and a `Transaction` derefs to `&Connection`, so `strip_file(&tx, …)` type-checks.

- [ ] Run: `cargo test -p ken-core -- merge_ re_extraction_removes purge_file_knowledge`
  **Expected:** passes.

- [ ] Commit: `git commit -am "feat(map): merge_knowledge_delta + purge_file_knowledge (dedup/GC heart)"`

### Step 4.3 — Wire purge/extractions into removal + clear

- [ ] In `crates/ken-core/src/db.rs`, extend `remove_file` (`:331`): inside the transaction, after `DELETE FROM files WHERE id = ?1` at `:344`, add the strip + extraction cleanup so deletions converge:

```rust
            tx.execute("DELETE FROM files WHERE id = ?1", params![id])?;
            strip_file(&tx, rel_path)?;
            tx.execute("DELETE FROM extractions WHERE rel_path = ?1", params![rel_path])?;
```

- [ ] In `clear` (`:512`), add extractions + knowledge reset so a full reindex re-extracts from scratch:

```rust
    pub fn clear(&mut self) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM contents", [])?;
        tx.execute("DELETE FROM files", [])?;
        tx.execute("DELETE FROM extractions", [])?;
        tx.execute("DELETE FROM entities", [])?; // cascades entity_edges
        tx.execute("DELETE FROM events", [])?;
        tx.commit()?;
        Ok(())
    }
```

- [ ] Add a test to `#[cfg(test)] mod tests` in `db.rs`:

```rust
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
```

- [ ] Run: `cargo test -p ken-core -- removing_a_file_purges`
  **Expected:** passes. Then `cargo test -p ken-core` to confirm no regressions.

- [ ] Commit: `git commit -am "feat(map): purge knowledge + extraction rows on file removal/clear"`

---

## Task 5 — Worker core: `extract_one` + `process_next_pending`

**Files:** `crates/ken-core/src/knowledge_model.rs` (new pub fns after `parse_delta_value`).

**Interfaces — Consumes:** a generation seam `G: Fn(&str) -> Result<serde_json::Value>` (the real one calls `local_llm::generate_json`; tests pass a closure). **Produces:**
- `pub fn extract_one<G>(db: &mut Db, rel_path: &str, content_hash: &str, today: &str, at: i64, generate: &G) -> Result<()> where G: Fn(&str) -> Result<serde_json::Value>` — read text, compose prompt, generate, parse delta, merge, mark done; on generate error, mark the extraction row `error` and propagate.
- `pub fn process_next_pending<G>(db: &mut Db, today: &str, at: i64, generate: &G) -> Result<Option<String>> where G: Fn(&str) -> Result<serde_json::Value>` — pop the oldest pending file and extract it; `Ok(Some(rel_path))` if one was processed, `Ok(None)` if the queue is empty.

### Step 5.1 — Failing test with a fake generator

- [ ] Add to `#[cfg(test)] mod tests` in `crates/ken-core/src/knowledge_model.rs`:

```rust
    #[test]
    fn worker_drains_queue_merges_and_marks_done() {
        let mut db = Db::open_in_memory().unwrap();
        db.upsert_file("kickoff.md", "md", 1, 1, "indexed", None, "We hired Priya N.").unwrap();
        let hash = content_hash("We hired Priya N.");
        db.enqueue_extraction_if_changed("kickoff.md", &hash).unwrap();

        // Fake local model: returns a canned delta regardless of prompt.
        let generate = |_prompt: &str| -> Result<serde_json::Value> {
            Ok(serde_json::json!({
                "entities": [{"kind": "person", "name": "Priya N.", "summary": "New hire."}],
                "relations": [],
                "events": [{"date": "2026-07-14", "category": "people", "text": "Priya joined."}]
            }))
        };

        let done = process_next_pending(&mut db, "2026-07-14", 100, &generate).unwrap();
        assert_eq!(done, Some("kickoff.md".to_string()));
        assert_eq!(db.extraction_coverage().unwrap(), (1, 1));
        assert_eq!(db.list_entities_with_edges().unwrap().0.len(), 1);
        assert_eq!(db.list_events().unwrap().len(), 1);
        // Queue now empty.
        assert_eq!(process_next_pending(&mut db, "2026-07-14", 101, &generate).unwrap(), None);
    }

    #[test]
    fn worker_records_generation_failure_without_touching_the_model() {
        let mut db = Db::open_in_memory().unwrap();
        db.upsert_file("bad.md", "md", 1, 1, "indexed", None, "content").unwrap();
        db.enqueue_extraction_if_changed("bad.md", &content_hash("content")).unwrap();
        let generate = |_: &str| -> Result<serde_json::Value> {
            Err(Error::Other("model timed out".into()))
        };
        assert!(process_next_pending(&mut db, "2026-07-14", 100, &generate).is_err());
        // Model untouched; the row is 'error', not 'pending' (no retry storm).
        assert!(db.list_entities_with_edges().unwrap().0.is_empty());
        assert_eq!(db.next_pending_extraction().unwrap(), None);
    }
```

- [ ] Run: `cargo test -p ken-core -- worker_drains worker_records_generation`
  **Expected:** fails to compile.

### Step 5.2 — Implement

- [ ] Add after `parse_delta_value` in `crates/ken-core/src/knowledge_model.rs`:

```rust
/// Extract one file: read its indexed text, prompt the model, parse the delta,
/// and merge it. Marks the extraction row `done` on success (stamping the hash
/// that was queued) or `error` on generation failure — a failed file leaves
/// the model untouched and does NOT return to `pending`, so a persistently
/// failing file can't wedge the queue.
pub fn extract_one<G>(
    db: &mut Db,
    rel_path: &str,
    content_hash: &str,
    today: &str,
    at: i64,
    generate: &G,
) -> Result<()>
where
    G: Fn(&str) -> Result<serde_json::Value>,
{
    let text = db.get_text(rel_path)?.unwrap_or_default();
    let prompt = compose_file_prompt(rel_path, &text, today);
    match generate(&prompt) {
        Ok(value) => {
            let delta = parse_delta_value(&value);
            db.merge_knowledge_delta(rel_path, &delta, at)?;
            db.mark_extraction_done(rel_path, content_hash, at)?;
            Ok(())
        }
        Err(e) => {
            db.mark_extraction_error(rel_path, &e.to_string(), at)?;
            Err(e)
        }
    }
}

/// Pop the oldest pending file and extract it. `Ok(None)` when the queue is
/// empty. The caller (one background thread per project) loops on this,
/// emitting `knowledge-updated` after each `Ok(Some(_))`.
pub fn process_next_pending<G>(
    db: &mut Db,
    today: &str,
    at: i64,
    generate: &G,
) -> Result<Option<String>>
where
    G: Fn(&str) -> Result<serde_json::Value>,
{
    let Some((rel_path, content_hash)) = db.next_pending_extraction()? else {
        return Ok(None);
    };
    extract_one(db, &rel_path, &content_hash, today, at, generate)?;
    Ok(Some(rel_path))
}
```

- [ ] Run: `cargo test -p ken-core -- worker_drains worker_records_generation`
  **Expected:** passes.

- [ ] Commit: `git commit -am "feat(map): extraction worker core (extract_one, process_next_pending)"`

---

## Task 6 — Enqueue hook in `scan::index_one`

**Files:** `crates/ken-core/src/scan.rs` (`index_one` at `:174`).

**Interfaces — Consumes:** `knowledge_model::content_hash`, `Db::enqueue_extraction_if_changed`. **Produces:** side effect — an `indexed` file whose content hash changed is left `pending` in `extractions`.

### Step 6.1 — Failing test

- [ ] Add to `#[cfg(test)] mod tests` in `crates/ken-core/src/scan.rs` (it already has `use crate::db::Db;` and `temp_project()`):

```rust
    #[test]
    fn indexing_enqueues_changed_files_for_extraction() {
        let (dir, project) = temp_project();
        let mut db = Db::open(&PathBuf::from(dir.path()).join("idx"), project.config.id)
            .unwrap_or_else(|_| Db::open_in_memory().unwrap());
        // Index a real indexed file directly through index_one.
        fs::write(project.root.join("note.md"), "Priya leads billing.").unwrap();
        let meta = project.root.join("note.md").metadata().unwrap();
        let status = index_one(
            &project, &mut db, "note.md",
            meta.len() as i64, 0, false,
        ).unwrap();
        assert_eq!(status, STATUS_INDEXED);
        // The file is now queued with the hash of its extracted text.
        assert_eq!(
            db.next_pending_extraction().unwrap(),
            Some(("note.md".into(), crate::knowledge_model::content_hash("Priya leads billing.").into()))
        );

        // Re-indexing identical content does NOT re-queue once it's done.
        db.mark_extraction_done("note.md", &crate::knowledge_model::content_hash("Priya leads billing."), 1).unwrap();
        index_one(&project, &mut db, "note.md", meta.len() as i64, 0, false).unwrap();
        assert!(db.next_pending_extraction().unwrap().is_none());
    }
```

(If `temp_project`'s DB helper differs, use `Db::open_in_memory()` — the assertion only needs a writable `Db`. Keep whichever compiles; the extraction assertions are what matter.)

- [ ] Run: `cargo test -p ken-core indexing_enqueues_changed_files`
  **Expected:** fails (no enqueue happens yet).

### Step 6.2 — Implement

- [ ] In `crates/ken-core/src/scan.rs`, edit `index_one` (`:191-202`). After the `db.upsert_file(...)` call at `:201`, before `Ok(status)`, enqueue when the file is `indexed`:

```rust
    db.upsert_file(rel, kind.as_str(), size, mtime, status, error.as_deref(), &text)?;
    // Incremental Map: an indexed file whose content changed is queued for
    // local-LLM extraction. The hash is over the extracted text, so mtime/size
    // churn without a content change never re-runs extraction.
    if status == STATUS_INDEXED {
        let hash = crate::knowledge_model::content_hash(&text);
        db.enqueue_extraction_if_changed(rel, &hash)?;
    }
    Ok(status)
```

- [ ] Run: `cargo test -p ken-core indexing_enqueues_changed_files`
  **Expected:** passes. Then `cargo test -p ken-core`
  **Expected:** all green.

- [ ] Commit: `git commit -am "feat(map): enqueue changed indexed files for extraction"`

---

## Task 7 — Retire auto-build; spawn extraction worker per project (`src-tauri`)

**Files:** `src-tauri/src/lib.rs` (imports `:17`; `open_project` tick thread `:407-421`; `maybe_auto_build_knowledge` `:2864-2910`; `KNOWLEDGE_TICK` `:2723`; `knowledge_model` DTO/command `:2727-2766`; `ActiveProject` field near `:368`).

**Dependency:** this task references `ken_core::local_llm::{Priority, generate_json, llm_status, LlmStatus}` — it lands with, or after, the local-llm plan. Everything in Tasks 1–6 compiles and tests without it.

**Interfaces — Consumes:** `ken_core::knowledge_model::process_next_pending`, `ken_core::local_llm::*`. **Produces:** a `knowledge-updated` Tauri event (unit); `KnowledgeModelDto` gains `analyzed`, `total`, `llm_status`, `llm_error`.

### Step 7.1 — Replace the auto-build tick with an extraction worker

- [ ] In `src-tauri/src/lib.rs`, delete the `maybe_auto_build_knowledge` function (`:2864-2910`) and the `KNOWLEDGE_TICK` constant (`:2721-2723`). Keep `AutoBuildTracker`/`should_auto_build` in `ken-core` (still referenced by `start_knowledge_build`/`refresh_knowledge_model` for the manual Deep rebuild) — only the automatic *invocation* is retired.

- [ ] Replace the tick-thread block in `open_project` (`:407-421`) with an extraction worker thread. The `stop`/`StopOnDrop(stop.clone())` machinery and the `_knowledge_ticker` field are reused verbatim (rename the field to `_extraction_worker` for clarity — update its declaration in the `ActiveProject` struct and the initializer at `:368`). New block:

```rust
    // One background extraction worker per open project: it drains the
    // `extractions` queue at the local model's background priority, one file
    // per generation, merging each delta into the Map/Timeline model and
    // emitting a throttled `knowledge-updated` after each merged file. When the
    // local model isn't ready it idles quietly (the Map shows a plain notice).
    let worker_app = app.clone();
    let worker_state = state.clone();
    let worker_id = project.config.id;
    let worker_stop = stop.clone();
    std::thread::spawn(move || {
        extraction_worker(worker_app, worker_state, worker_id, worker_stop);
    });
```

- [ ] Add the worker function (near `start_knowledge_build`, e.g. after `:2910`):

```rust
/// The incremental-Map worker: one per open project. Loops draining the
/// extraction queue while the local model is ready, emitting a throttled
/// `knowledge-updated` after each merged file. Every wait is short so a newly
/// indexed file is picked up promptly; the model's own queue (background
/// priority) yields to interactive quick answers upstream.
fn extraction_worker(
    app: AppHandle,
    state: SharedState,
    project_id: uuid::Uuid,
    stop: Arc<AtomicBool>,
) {
    let mut last_emit = Instant::now() - Duration::from_secs(1);
    let mut pending_emit = false;
    while !stop.load(Ordering::SeqCst) {
        // Pause quietly unless the local model is ready.
        if !matches!(ken_core::local_llm::llm_status(), ken_core::local_llm::LlmStatus::Ready) {
            std::thread::sleep(Duration::from_secs(2));
            continue;
        }
        // Resolve base + project id under the lock, then drop it before the
        // (slow) generation so IPC stays responsive.
        let base = {
            let guard = state.lock().unwrap();
            match guard.active.as_ref() {
                Some(active) if active.project.config.id == project_id => {
                    guard.base_dir.clone()
                }
                _ => return, // project closed or switched — this worker is done
            }
        };
        let Ok(mut db) = Db::open(&base, project_id) else {
            std::thread::sleep(Duration::from_secs(2));
            continue;
        };
        let today = local_date_today();
        let at = engine::now_epoch();
        let generate = |prompt: &str| {
            ken_core::local_llm::generate_json(prompt, ken_core::local_llm::Priority::Background)
        };
        match ken_core::knowledge_model::process_next_pending(&mut db, &today, at, &generate) {
            Ok(Some(_)) => {
                pending_emit = true;
                // Throttle: coalesce a burst into at most one event / 750ms.
                if last_emit.elapsed() >= Duration::from_millis(750) {
                    let _ = app.emit("knowledge-updated", ());
                    last_emit = Instant::now();
                    pending_emit = false;
                }
                // Immediately loop for the next pending file.
            }
            Ok(None) => {
                // Queue empty: flush any trailing throttled emit, then idle.
                if pending_emit {
                    let _ = app.emit("knowledge-updated", ());
                    last_emit = Instant::now();
                    pending_emit = false;
                }
                std::thread::sleep(Duration::from_secs(2));
            }
            Err(_) => {
                // A generation failed (already recorded on the row). Brief
                // backoff so a bad model state doesn't spin.
                std::thread::sleep(Duration::from_secs(2));
            }
        }
    }
}
```

- [ ] Update the `ActiveProject` struct field `_knowledge_ticker: StopOnDrop` → `_extraction_worker: StopOnDrop` (declaration + the `:368` initializer `_extraction_worker: StopOnDrop(stop.clone()),`).

### Step 7.2 — Coverage + LLM status in the DTO

- [ ] Extend `KnowledgeModelDto` (`:2727-2737`):

```rust
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct KnowledgeModelDto {
    entities: Vec<EntityRow>,
    edges: Vec<EdgeRow>,
    events: Vec<EventRow>,
    built_at: Option<i64>,
    building: bool,
    /// Incremental coverage: files extracted / indexed files.
    analyzed: i64,
    total: i64,
    /// `ready` | `notInstalled` | `error` — drives the Map's paused notice.
    llm_status: String,
    /// The model's error message when `llm_status == "error"`, else null.
    llm_error: Option<String>,
}
```

- [ ] Update the `knowledge_model` command (`:2754-2766`) to populate them:

```rust
#[tauri::command]
fn knowledge_model(state: State<SharedState>) -> CmdResult<KnowledgeModelDto> {
    let guard = state.lock().unwrap();
    let active = guard.active.as_ref().ok_or("no project open")?;
    let (entities, edges) = active.db.list_entities_with_edges().map_err(err)?;
    let (analyzed, total) = active.db.extraction_coverage().map_err(err)?;
    let (llm_status, llm_error) = match ken_core::local_llm::llm_status() {
        ken_core::local_llm::LlmStatus::Ready => ("ready".to_string(), None),
        ken_core::local_llm::LlmStatus::NotInstalled => ("notInstalled".to_string(), None),
        ken_core::local_llm::LlmStatus::Error(e) => ("error".to_string(), Some(e)),
    };
    Ok(KnowledgeModelDto {
        entities,
        edges,
        events: active.db.list_events().map_err(err)?,
        built_at: active.db.knowledge_model_built_at().map_err(err)?,
        building: active.knowledge_running.load(Ordering::SeqCst),
        analyzed,
        total,
        llm_status,
        llm_error,
    })
}
```

- [ ] Confirm `refresh_knowledge_model` (`:2772`) is unchanged — it stays the manual **Deep rebuild** (whole-model replace via `build_knowledge_model`). The `AutoBuildTracker.changed()` calls scattered through `lib.rs` (`:541, :814, :1177`) are now inert (no auto tick reads them) but harmless; leave them to minimize churn.

- [ ] Build the app crate: `cargo build -p ken-tauri` (or the workspace: `cargo build`).
  **Expected:** compiles **once the local-llm module exists**. If implementing this task before local-llm lands, stub `ken_core::local_llm` with the three contract items so the workspace builds, and delete the stub when the real module arrives. (Coordinate via the cross-plan contract; do not merge a stub.)

- [ ] Commit: `git commit -am "feat(map): retire auto-build tick, run per-project extraction worker, expose coverage+llm status"`

---

## Task 8 — Frontend: coverage line, paused notice, throttled refresh, Deep rebuild

**Files:** `src/lib/api.ts` (`KnowledgeModel` `:294-303`; listeners `:547`), `src/lib/knowledge.svelte.ts`, `src/screens/MapScreen.svelte`.

**Interfaces — Consumes:** the extended `knowledge_model` DTO; a new `knowledge-updated` event. **Produces:** `api.onKnowledgeUpdated`; store getters `coverage`, `llmPaused`, `llmNotice`; MapScreen coverage line + paused notice + Deep rebuild button.

### Step 8.1 — API types + listener

- [ ] Extend `KnowledgeModel` in `src/lib/api.ts` (`:294-303`):

```ts
export interface KnowledgeModel {
  entities: EntityRow[];
  edges: EntityEdge[];
  events: EventRow[];
  builtAt: number | null;
  building: boolean;
  /** Files extracted so far / indexed files — the coverage line. */
  analyzed: number;
  total: number;
  /** `ready` | `notInstalled` | `error`. */
  llmStatus: "ready" | "notInstalled" | "error";
  llmError: string | null;
}
```

- [ ] Add the listener alongside `onKnowledgeModelState` in `src/lib/api.ts` (after `:550`):

```ts
  onKnowledgeUpdated: (fn: () => void): Promise<UnlistenFn> =>
    listen<null>("knowledge-updated", () => fn()),
```

### Step 8.2 — Store: throttled reload + getters

- [ ] Update `src/lib/knowledge.svelte.ts`. Add a throttled `knowledge-updated` subscription in `visit()`, and getters for coverage and the paused state. Full new file:

```ts
// Knowledge-model state shared by the Map and Timeline screens: the stored
// model, whether a manual Deep rebuild is running, incremental coverage, and
// whether the local model is available to keep extracting.
import { api, type KnowledgeModel } from "./api";

class KnowledgeStore {
  model = $state<KnowledgeModel | null>(null);
  building = $state(false);
  error = $state<string | null>(null);
  /** Optimistic until claude_doctor answers (Deep rebuild needs Claude). */
  claudeFound = $state(true);
  private initDone = false;

  /** No entities yet AND no deep build has ever stamped the model. */
  get empty(): boolean {
    return (
      this.model === null ||
      (this.model.entities.length === 0 && this.model.builtAt === null)
    );
  }

  /** "N of M files analyzed", or null when fully caught up / nothing indexed. */
  get coverage(): { analyzed: number; total: number } | null {
    const m = this.model;
    if (!m || m.total === 0 || m.analyzed >= m.total) return null;
    return { analyzed: m.analyzed, total: m.total };
  }

  /** The local model can't extract right now — show the plain notice. */
  get llmPaused(): boolean {
    return this.model?.llmStatus === "notInstalled" || this.model?.llmStatus === "error";
  }

  get llmNotice(): string {
    if (this.model?.llmStatus === "error") {
      return "Ken's on-device model hit a snag — mapping is paused. Open Settings to check the model.";
    }
    // notInstalled
    return "Ken maps your project on your Mac. Choose the on-device model in Settings to begin.";
  }

  /** Call on screen mount: subscribe once, re-read every visit. */
  async visit() {
    if (!this.initDone) {
      this.initDone = true;
      await api.onKnowledgeModelState((ev) => {
        if (ev.state === "building") {
          this.building = true;
          this.error = null;
        } else if (ev.state === "ready") {
          this.building = false;
          this.error = null;
          void this.load();
        } else if (ev.state === "idle") {
          this.building = false;
          this.error = null;
        } else {
          this.building = false;
          this.error = ev.detail ?? "The rebuild didn't finish.";
        }
      });
      // Incremental merges land continuously; throttle the reload so a burst
      // of merged files triggers at most one refetch per ~600ms.
      await api.onKnowledgeUpdated(() => this.throttledLoad());
      this.claudeFound =
        (await api.claudeDoctor().catch(() => null))?.found ?? false;
    }
    await this.load();
  }

  private loadTimer: ReturnType<typeof setTimeout> | undefined;
  private throttledLoad() {
    if (this.loadTimer) return;
    this.loadTimer = setTimeout(() => {
      this.loadTimer = undefined;
      void this.load();
    }, 600);
  }

  async load() {
    if (!(await api.currentProject().catch(() => null))) return;
    this.model = await api.knowledgeModel().catch(() => null);
    if (this.model?.building) this.building = true;
  }

  /** Manual Deep rebuild (Claude, whole-model replace). */
  async refresh() {
    this.error = null;
    this.building = true;
    try {
      await api.refreshKnowledgeModel();
    } catch (e) {
      this.building = false;
      this.error = String(e);
    }
  }
}

export const knowledge = new KnowledgeStore();
```

### Step 8.3 — MapScreen: coverage line, paused notice, Deep rebuild button

- [ ] In `src/screens/MapScreen.svelte`, update the empty-state block (`:258-290`) so an unavailable local model shows the plain notice instead of the Claude copy, and reuse coverage. Replace the empty-card `{:else if …}` branches with a paused-aware version:

```svelte
  {#if knowledge.empty}
    <div class="empty">
      {#if knowledge.error}
        <div class="error">Last rebuild didn't finish — {knowledge.error}</div>
      {/if}
      <div class="empty-card">
        <h2>
          {knowledge.llmPaused
            ? "Ken hasn't mapped this project yet"
            : "Ken is mapping this project…"}
        </h2>
        <p>
          As Ken reads each document, it finds the people, organizations,
          topics, and decisions — and how they connect.
        </p>
        {#if knowledge.llmPaused}
          <p class="note">{knowledge.llmNotice}</p>
        {:else}
          <p class="pulse">
            This happens on your Mac, a file at a time — the map fills in as it
            goes.
          </p>
        {/if}
      </div>
    </div>
  {:else if model}
```

- [ ] Add a coverage line and a Deep rebuild button to the canvas overlay. Replace the existing `{#if knowledge.building}…{/if}` status overlay (`:390-394`) with:

```svelte
      {#if knowledge.llmPaused}
        <div class="status-overlay">{knowledge.llmNotice}</div>
      {:else if knowledge.coverage}
        <div class="status-overlay pulse">
          {knowledge.coverage.analyzed} of {knowledge.coverage.total} files analyzed
        </div>
      {:else if knowledge.building}
        <div class="status-overlay pulse">Deep rebuild in progress…</div>
      {:else if knowledge.error}
        <div class="status-overlay error">Last rebuild didn't finish — {knowledge.error}</div>
      {/if}

      <div class="rebuild">
        <button
          class="btn btn-small"
          title="Rebuild the whole map with a deeper, curated pass (uses Claude)"
          disabled={knowledge.building || !knowledge.claudeFound}
          onclick={() => void knowledge.refresh()}
        >
          Deep rebuild
        </button>
      </div>
```

- [ ] Add the `.rebuild` positioning rule to the `<style>` block (near `.zoom` at `:610`):

```css
  .rebuild {
    position: absolute;
    left: 16px;
    top: 16px;
  }
```

(If `.status-overlay` at top-left collides with `.rebuild`, move the status overlay to `top: 56px` — verify visually per the run recipe.)

### Step 8.4 — Verify

- [ ] Frontend type-check + tests: `pnpm vitest run` and `pnpm exec svelte-check` (or the repo's configured check).
  **Expected:** no type errors from the new fields; existing tests pass. (No new vitest file is required by spec §2 — the merge logic is Rust-tested — but if a `knowledge` store test exists, extend it for the `coverage`/`llmPaused` getters.)

- [ ] Manual: follow the `verify` skill to launch the app; open the Map on a project with files. **Expected:** coverage line ("N of M files analyzed") appears while the worker is behind and disappears when caught up; the graph fills in incrementally as `knowledge-updated` fires; **Deep rebuild** button triggers the old whole-model pass; with the local model uninstalled, the plain paused notice shows and no error banner appears.

- [ ] Commit: `git commit -am "feat(map): coverage line, paused notice, throttled refresh, Deep rebuild button"`

---

## Final verification

- [ ] `cargo test -p ken-core` — all extraction/merge/queue/worker tests green, no regressions in `knowledge_model`/`db`/`scan` suites.
- [ ] `cargo build` (workspace) — compiles with the local-llm contract present.
- [ ] `pnpm vitest run` — frontend green.
- [ ] Manual live-test via the `verify` recipe: incremental map fills in; edit a file → its entities update (re-extract converges, no duplicates); delete a file → its orphaned entities disappear; Deep rebuild replaces the whole model; local-model-off shows the paused notice.

## Notes / resolved ambiguities

- **Per-file JSON uses a top-level `relations` array** (a/b by entity name), per spec §2's stated shape — distinct from the manual one-shot's nested `connections`. The `Extraction`/`EntityInput` structs are reused unchanged; `parse_delta_value` maps `relations` onto `EntityInput.connections`.
- **Sources are injected at merge, not trusted from the model.** Per-file entities carry `sources = [rel_path]`; events carry `source = rel_path`. This keeps the sources-array the single source of truth for GC.
- **`built_at` is stamped on every merge** so the Map treats a growing incremental model as "built"; the frontend `empty` getter also honours entity presence, so the map renders after the first merged file even though no deep build ran.
- **Content hash is an inline FNV-1a** (no new dependency), over the extracted text; deterministic within and across app runs. A hash-format change across app versions would cause a one-time re-extraction of every file — acceptable.
- **Removed/excluded files converge** via `strip_file` wired into `Db::remove_file` and `clear`, plus `purge_file_knowledge` for direct use — not strictly in spec §2's trigger list but required for deletions to "converge instead of accreting."
- **Auto-build policy code is retired from running, not deleted**: `AutoBuildTracker`/`should_auto_build` remain because `refresh_knowledge_model` (Deep rebuild) still uses the tracker's `build_started`/`build_finished` bookkeeping.
- **Throttling is split**: backend coalesces `knowledge-updated` to ≤1/750ms with a trailing flush; the frontend additionally debounces its reload to ≤1/600ms.
```

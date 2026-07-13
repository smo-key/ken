# Tasks: mcp-server

## 1. ken-core accessors

- [x] 1.1 `registry::default_base_dir()` honors `KEN_DATA_DIR` (checked first); test
- [x] 1.2 `db.rs`: `Db::open_read_only(base, project_id)` with `SQLITE_OPEN_READ_ONLY` (no migration, fails when the index is missing) and `Db::get_text(rel_path)` returning the indexed extracted text; tests including write-refusal on the read-only handle

## 2. ken-mcp server

- [x] 2.1 `Cargo.toml`: serde + serde_json deps, tempfile dev-dep
- [x] 2.2 `main.rs` protocol shell: stdin line loop, `handle_request` routing (`initialize` with version echo, `notifications/initialized`, `ping`, `tools/list`, `tools/call`), `-32601` unknown method, `-32700` malformed line without dying, notifications never answered
- [x] 2.3 Tools: `search_knowledge` (bolded snippets), `read_document` (raw text kinds capped 200 KB via `Project::resolve`; extracted text for binary kinds, labeled), `list_documents` (optional folder filter), `list_projects`; failures as `isError` content
- [x] 2.4 Scoping: `--project <path>` lock (supplied `project` arg noted and ignored); unscoped requires `project` by registry name or path with a helpful error naming projects
- [x] 2.5 Unit tests in `main.rs` for routing, parse tolerance, and tool behavior over a tempdir fixture
- [x] 2.6 Integration test `tests/stdio.rs`: fixture project + registry under a `KEN_DATA_DIR` tempdir, spawn `env!("CARGO_BIN_EXE_ken-mcp")`, drive initialize → initialized → tools/list (4 tools) → search finds seeded content → read_document round-trips → path-escape fails safely → unscoped-without-project errors helpfully

## 3. Tauri + Settings

- [x] 3.1 `mcp_info` command: binary discovery (exe sibling → `~/.local/bin` → PATH → dev `target/debug`), add command, JSON config, LLM instruction; register
- [x] 3.2 api.ts: `McpInfo` type + wrapper
- [x] 3.3 Settings "Connect an agent" card: status line, dark mono command block with Copy, Scope + LLM-instruction chips, missing-binary note with dev hint; drop the MCP mention from Coming-to-Ken

## 4. Verification

- [x] 4.1 cargo test --workspace, pnpm test, pnpm check, pnpm build all green; openspec validate mcp-server
- [x] 4.2 Commit

# Proposal: mcp-server

## Why

Ken's design promises that everything the app indexes is available to the
user's other agents (§4.10): Claude Code, Cursor, or anything that speaks
MCP should be able to search the knowledge base and read its documents
without Ken running. Today `ken-mcp` is a stub that exits with an error,
and Settings only hints that a server is "coming". Change 6 of 10 ships
the real thing: a small stdio MCP server over ken-core's existing index,
read-only by construction, plus the Settings card that gets a
non-technical user from "I have an agent" to "my agent knows my project"
with one copy-paste.

## What Changes

- **`ken-mcp` becomes a real stdio MCP server** — hand-rolled JSON-RPC
  2.0, one JSON object per line, no new heavyweight dependencies
  (`serde`/`serde_json` only; ken-core stays the domain layer).
  Implements `initialize`, `notifications/initialized`, `ping`,
  `tools/list`, and `tools/call`; unknown methods get `-32601`, malformed
  lines get `-32700` and the server keeps serving, notifications never
  get replies.
- **Four tools**, results as MCP text content (tool failures are
  `isError` content, not protocol errors):
  - `search_knowledge {query, limit?, project?}` — ranked FTS hits from
    `Db::search`, snippets with matches emphasized in **bold**.
  - `read_document {path, project?}` — raw content for text-kind files
    (capped at 200 KB, path validated through `Project::resolve` so it
    can never escape the root); for binary kinds the extracted text
    stored in the index, labeled as extracted.
  - `list_documents {folder?, project?}` — indexed files (path, kind,
    size, mtime), optionally under a folder.
  - `list_projects {}` — registry entries (name, path, available).
- **Scoping.** `ken-mcp --project <path>` locks every tool to that
  project (a supplied `project` argument is noted and ignored). Unscoped,
  the three project tools require `project` (name or path, matched
  against the registry) and a missing/unknown value errors helpfully,
  naming the available projects.
- **Read-only by construction.** New `Db::open_read_only(base,
  project_id)` opens the SQLite index with
  `SQLITE_OPEN_READ_ONLY`; the server never writes anything anywhere.
  New small `Db::get_text(rel_path)` accessor returns the indexed
  extracted text for binary kinds.
- **`KEN_DATA_DIR` override** for `registry::default_base_dir()`
  (checked first) so tests — and power users — can relocate app data.
- **Settings "Connect an agent" card** replacing the MCP mention in the
  Coming-to-Ken card, per the prototype: status line ("Ready — agents
  start it on demand" when the binary is found), dark mono block with the
  `claude mcp add ken -- <binary> --project <root>` command and a working
  Copy button, Scope chip, and an "LLM instruction" chip that copies a
  paste-into-any-agent setup instruction (what ken-mcp is, the add
  command, and the generic JSON config). Honest copy throughout — no
  fake connected-agent counts. Backed by a new Tauri command `mcp_info`
  that resolves the binary (app-executable sibling → `~/.local/bin` →
  PATH → dev build) and assembles the copy strings.

## Capabilities

### New Capabilities
- `mcp-server`: the ken-mcp stdio server (protocol, tools, scoping,
  read-only guarantees) and the Settings card that connects agents to it.

### Modified Capabilities

_None — the server reads the index and registry that walking-skeleton
established; no existing behavior changes._

## Impact

- `crates/ken-mcp`: `main.rs` replaces the stub (server loop, request
  routing, tools); `Cargo.toml` gains `serde`/`serde_json` and dev-deps
  for the integration test; new `tests/stdio.rs` drives the real binary
  over stdio against a fixture project in a tempdir.
- `crates/ken-core`: `db.rs` gains `open_read_only` + `get_text`;
  `registry.rs` gains the `KEN_DATA_DIR` override.
- `src-tauri`: new `mcp_info` command; register.
- Frontend: `api.ts` McpInfo type + wrapper; Settings "Connect an agent"
  card.
- Tests: unit tests for routing/parse tolerance in `main.rs`, stdio
  integration test, ken-core accessor tests. Nothing touches the real
  app-data dir — everything runs under `KEN_DATA_DIR` tempdirs.

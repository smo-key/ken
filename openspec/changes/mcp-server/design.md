# Design: mcp-server

## Context

Changes 1–5 shipped the index (SQLite + FTS5 per project in app-data),
the registry of known projects, and `Project::resolve` path validation.
`ken-mcp` exists as a compiling stub so ken-core stays an honest shared
library. The design doc (§4.10) fixes the surface: a stdio binary,
read-only on the index, four tools, `--project` scoping, and a Settings
card whose whole job is copy-paste onboarding. The prototype's Settings
screen shows the card: status line, dark mono command block with copy,
Scope + LLM-instruction chips.

## Goals / Non-Goals

**Goals:**
- Any MCP client (Claude Code, Cursor, …) can search and read a Ken
  project without the Ken app running.
- The server cannot write: read-only SQLite handle, no file writes, path
  validation on every document read.
- Fully testable in `cargo test`: the real binary driven over stdio
  against a tempdir fixture, no network, no real app-data.
- A non-technical user connects an agent from Settings with one copy.

**Non-Goals:**
- MCP resources, prompts, sampling, or notifications beyond
  `initialized` — tools only.
- Live activity reporting in Settings (the server runs in the agent's
  process; Ken doesn't observe it yet). The card shows honest copy, not
  a fake connection count.
- Semantic search — the tools expose the same FTS the app uses.
- Auto-registering the server into agent configs; the card produces the
  command/config, the user pastes it.

## Decisions

1. **Hand-rolled minimal MCP over stdio.** The protocol subset Ken needs
   is five methods and a line-delimited JSON-RPC 2.0 framing; a protocol
   SDK would be the largest dependency in the crate. `main.rs` reads
   stdin line-by-line, writes exactly one JSON object per line on
   stdout, and logs nothing to stdout (diagnostics go to stderr).
   `initialize` replies `protocolVersion: "2024-11-05"` — echoing the
   client's requested version when it is a known newer one —
   `capabilities: {tools: {}}`, and `serverInfo {name: "ken", version:
   env!("CARGO_PKG_VERSION")}`. Unknown methods → `-32601`; malformed
   lines → `-32700` and the loop keeps serving; requests without an id
   are notifications and never get replies.
2. **Tool failures are content, not protocol errors.** A bad path, an
   unknown project, or an unindexed file returns
   `content: [{type:"text", …}], isError: true` so the calling model
   sees the explanation and can self-correct; protocol errors are
   reserved for malformed JSON-RPC.
3. **Read-only by construction.** `Db::open_read_only` opens with
   `SQLITE_OPEN_READ_ONLY` and skips migration — if the index doesn't
   exist yet the open fails and the tool answers "open the project in
   Ken first". `read_document` resolves paths through
   `Project::resolve`, which rejects `..` and absolute paths, and caps
   output at 200 KB. Binary kinds (docx/xlsx/pptx/pdf/image) return the
   indexed extracted text via the new `Db::get_text`, labeled as
   extracted, instead of dumping raw bytes.
4. **Scoping is explicit, never guessed.** With `--project <path>` every
   tool is locked to that project and a supplied `project` argument is
   ignored with a note in the result (so an agent that passes it anyway
   learns why it had no effect). Unscoped, the three project tools
   require `project` — matched against registry names
   (case-insensitive) or paths — and the error names the registered
   projects so the model's next call can succeed.
5. **`KEN_DATA_DIR` env override, checked first in
   `default_base_dir()`.** One tiny hook makes the whole binary
   integration-testable (spawn with the env var pointing at a tempdir)
   and doubles as a power-user escape hatch. The Tauri app picks it up
   too, which is exactly right: point both at the same dir and they
   agree.
6. **`handle_request` is a pure-ish function.** Routing, parse
   tolerance, and tool dispatch live in a testable
   `fn handle_request(&mut Server, &Value) -> Option<Value>` unit-tested
   without spawning; the stdio loop is a thin shell around it. The
   integration test (`tests/stdio.rs`) spawns the real binary via
   `env!("CARGO_BIN_EXE_ken-mcp")` and drives the full handshake.
7. **Binary discovery for the Settings card** mirrors how the installer
   lays files out: sibling of the running app executable, then
   `~/.local/bin/ken-mcp`, then `which ken-mcp`, then the dev fallback
   `target/debug/ken-mcp` relative to cwd. Found → "Ready — agents start
   it on demand" (stdio servers are started by the client; there is no
   daemon to show as "running"). Not found → plain-language note that it
   ships with Ken's installer, plus the dev hint.

## Risks / Trade-offs

- **WAL + read-only:** a read-only connection to a WAL database needs
  the `-shm` file to exist or be creatable; the index lives in the
  user-writable app-data dir, so this holds. If the app later moves the
  index somewhere read-only this assumption breaks loudly (open error),
  not silently.
- **Stale reads:** the server sees the index as of the app's last write.
  That is the intended semantics (the index itself is derived data), and
  SQLite WAL gives readers a consistent snapshot without blocking the
  app's writer.
- **Protocol drift:** MCP evolves; hand-rolling means tracking spec
  revisions by hand. The subset used here (initialize/tools) is the
  stable core every client ships, and echoing known newer protocol
  versions keeps modern clients happy without claiming features we
  don't have.

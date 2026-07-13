# mcp-server

## ADDED Requirements

### Requirement: ken-mcp speaks MCP over stdio
`ken-mcp` SHALL be a stdio MCP server: JSON-RPC 2.0, one JSON object per
line on stdout, reading stdin line-by-line, with nothing but protocol on
stdout. It SHALL implement `initialize` (replying protocol version
`2024-11-05` — or echoing the client's requested version when it is a
known newer one — `capabilities: {tools: {}}`, and `serverInfo` naming
`ken` with the crate version), `notifications/initialized` (no reply),
`ping` (empty result), `tools/list`, and `tools/call`. Unknown methods
SHALL error with code `-32601`; a malformed line SHALL error with code
`-32700` and the server SHALL keep serving; notifications (requests
without an id) SHALL never receive replies.

#### Scenario: Handshake with a modern client
- **WHEN** a client sends `initialize` requesting protocol version
  `2025-06-18`, then `notifications/initialized`, then `tools/list`
- **THEN** the initialize reply echoes `2025-06-18`, names the server
  `ken`, the notification gets no reply, and `tools/list` returns the
  four tools

#### Scenario: Garbage on stdin does not kill the server
- **WHEN** a client sends a line that is not valid JSON, then a valid
  `ping`
- **THEN** the first line is answered with a `-32700` error and the
  `ping` still receives an empty result

#### Scenario: Unknown method
- **WHEN** a client sends `resources/list` with an id
- **THEN** the reply is a JSON-RPC error with code `-32601`

### Requirement: Knowledge tools over the read-only index
The server SHALL expose exactly four tools, each returning MCP text
content and reporting tool failures as `isError: true` content rather
than protocol errors:
- `search_knowledge {query, limit?, project?}` — ranked full-text hits
  (path and snippet) from the project index, with matched terms
  emphasized in bold instead of `<mark>` markup.
- `read_document {path, project?}` — for text-kind files (md, txt,
  code) the raw file content capped at 200 KB; for other kinds the
  extracted text stored in the index, labeled as extracted.
- `list_documents {folder?, project?}` — indexed files (path, kind,
  size, mtime), optionally limited to a folder.
- `list_projects {}` — registry entries (name, path, availability).

The server SHALL open the SQLite index read-only and SHALL NOT write to
any file or database. `read_document` SHALL validate paths against the
project root and refuse any path that escapes it.

#### Scenario: Search finds indexed content
- **WHEN** an agent calls `search_knowledge` with a query matching a
  document in an indexed project
- **THEN** the result lists that document's path with a snippet whose
  matches are bolded

#### Scenario: Reading a document round-trips
- **WHEN** an agent calls `read_document` for an indexed markdown file
- **THEN** the result contains the file's content as stored on disk

#### Scenario: Path escape is refused
- **WHEN** an agent calls `read_document` with a path containing `..`
  or an absolute path
- **THEN** the tool returns an `isError` result explaining the path is
  outside the project, and no file outside the root is read

#### Scenario: Unindexed project explains itself
- **WHEN** a tool targets a registered project whose index database does
  not exist
- **THEN** the tool returns an `isError` result telling the agent to
  open the project in Ken first

### Requirement: Project scoping
When started with `--project <path>`, the server SHALL lock every tool
to that project and SHALL ignore a supplied `project` argument, noting
the lock in the result. When started unscoped, the server SHALL require
`project` (registry name, case-insensitive, or path) on
`search_knowledge`, `read_document`, and `list_documents`, and a missing
or unknown value SHALL produce an `isError` result naming the registered
projects.

#### Scenario: Scoped server ignores the project argument
- **WHEN** a server started with `--project /work/atlas` receives a
  `search_knowledge` call with `project: "other"`
- **THEN** the search runs against `/work/atlas` and the result notes
  that the server is locked to that project

#### Scenario: Unscoped call without a project
- **WHEN** an unscoped server receives `search_knowledge` without a
  `project` argument
- **THEN** the result is `isError` and names the projects the agent can
  choose from

### Requirement: App data location override
`registry::default_base_dir()` SHALL honor a `KEN_DATA_DIR` environment
variable, checked before the OS data directory, so tests and power users
can relocate all app data (registry and indexes).

#### Scenario: Tests run against a tempdir
- **WHEN** `ken-mcp` is spawned with `KEN_DATA_DIR` pointing at a
  tempdir containing a registry and index
- **THEN** all tools operate on that data and nothing under the real OS
  data directory is touched

### Requirement: Settings connects agents
Settings SHALL show a "Connect an agent" card. When the `ken-mcp` binary
is found (app-executable sibling, `~/.local/bin`, PATH, or a dev build),
the card SHALL show a ready status ("agents start it on demand"), a dark
monospace block with the `claude mcp add ken -- <binary> --project
<root>` command and a working Copy button, a scope chip ("this project
only"), and an "LLM instruction" chip that copies a paste-into-any-agent
instruction containing what ken-mcp is, the add command, and the generic
JSON `mcpServers` config. When the binary is not found, the card SHALL
say in plain language that it ships with Ken's installer and offer the
dev hint (`cargo build -p ken-mcp`). The card SHALL NOT show fabricated
activity or connection counts.

#### Scenario: Copying the add command
- **WHEN** the binary is found and the user clicks Copy on the command
  block
- **THEN** the clipboard contains the `claude mcp add` command with the
  resolved binary path and the current project's root

#### Scenario: Binary missing
- **WHEN** no ken-mcp binary can be found
- **THEN** the card explains it ships with Ken's installer, shows the
  dev build hint, and shows no ready status

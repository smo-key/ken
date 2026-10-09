# mcp-server

## MODIFIED Requirements

### Requirement: Project scoping
When started with `--project <path>`, the server SHALL lock every tool to
that project and SHALL ignore a supplied `project` argument, noting the
lock in the result. When started with `--workspace <path>`, the server
SHALL lock every tool to that workspace's resolvable members, SHALL
accept a `project` argument naming one of those members, and SHALL
produce an `isError` result naming the workspace's members when the
argument names something outside it. `--project` and `--workspace`
together SHALL be a usage error, exiting non-zero with the usage string
rather than silently preferring one. When started unscoped, the server
SHALL require `project` (registry name, case-insensitive, or path) on
`search_knowledge`, `read_document`, and `list_documents`, resolving it
across every workspace in the registry, and a missing or unknown value
SHALL produce an `isError` result naming the projects the agent can
choose from.

#### Scenario: Scoped server ignores the project argument
- **WHEN** a server started with `--project /work/atlas` receives a
  `search_knowledge` call with `project: "other"`
- **THEN** the search runs against `/work/atlas` and the result notes
  that the server is locked to that project

#### Scenario: Workspace-scoped server searches its members
- **WHEN** a server started with `--workspace /home/u/Documents` receives
  a `search_knowledge` call without a `project` argument
- **THEN** the search runs across that workspace's resolvable members and
  no member of another workspace is consulted

#### Scenario: Workspace-scoped server rejects an outside project
- **WHEN** a workspace-scoped server receives a call naming a project
  that is not one of that workspace's members
- **THEN** the result is `isError` and names the members the agent can
  choose from

#### Scenario: Both scope flags is a usage error
- **WHEN** the server is started with both `--project` and `--workspace`
- **THEN** it exits non-zero with the usage string and starts no server

#### Scenario: Unscoped call without a project
- **WHEN** an unscoped server receives `search_knowledge` without a
  `project` argument
- **THEN** the result is `isError` and names the projects the agent can
  choose from

#### Scenario: Unscoped resolution spans workspaces
- **WHEN** an unscoped server receives a call naming a project that
  belongs to any registered workspace
- **THEN** it resolves and searches that project

### Requirement: Settings connects agents
The Settings page SHALL include a "Connect an agent" card. When the
binary is found (app-executable sibling, `~/.local/bin`, PATH, or a dev
build), the card SHALL show a ready status ("agents start it on demand"),
a dark monospace block with a `claude mcp add` command and a working Copy
button, a scope chip, and an "LLM instruction" chip that copies a
paste-into-any-agent instruction containing what ken-mcp is, the add
command, and the generic JSON `mcpServers` config. The offered command
SHALL match the current scope: `--project <root>` with a scope chip
reading "this project only" when a single project is open, and
`--workspace <root>` with a chip naming the workspace when a workspace is
open. When more than one workspace is open, the card SHALL let the user
choose which workspace the command targets. When the binary is not found,
the card SHALL say in plain language that it ships with Ken's installer
and offer the dev hint (`cargo build -p ken-mcp`). The card SHALL NOT
show fabricated activity or connection counts.

#### Scenario: The card offers a workspace-scoped command
- **WHEN** a workspace is open and the binary is found
- **THEN** the copyable command uses `--workspace` with that workspace's
  root and the scope chip names the workspace

#### Scenario: The card lets the user pick among open workspaces
- **WHEN** more than one workspace is open
- **THEN** the card offers a choice of which workspace the generated
  command targets

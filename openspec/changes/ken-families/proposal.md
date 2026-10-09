# Proposal: ken-families

## Why

Everything planned so far serves one person on one machine. Real
projects have teams. A "family" is Ken's collaboration unit: a
**separate, templated git repository** that every member's Ken
clones, polls, and pushes to — the collaboration bus. No server, no
accounts, no protocol beyond git; whatever remote the team already
uses (GitHub, Gitea, a bare repo on a NAS) works.

The family repo is a **no-human repo**: people never open it in an
editor. Each member's Ken is their secretary inside it — it delivers
tasks and messages to teammates' inboxes, surfaces what arrives for
its own user, and keeps the member's board visible to the team. A
`shared/` area holds the team's common knowledge and is indexed
like any other project source.

## What Changes

- New **global flag `kenFamilies`**; off means no clones, no
  polling, no Families settings page, no new tools — byte-identical
  to today.
- **Family connections** in settings: remote URL, which member you
  are, live-sync toggle, poll interval, attach-to-workspace.
  Multiple connections, multiple teams.
- A **sync engine** per connection: clone into app data, poll with
  fetch + fast-forward/rebase, commit + push local changes. Uses
  the system `git` CLI so the user's existing credential helpers
  (SSH agent, Windows Credential Manager) just work.
- A **templated repo layout**: `family.json` manifest, per-member
  `members/<id>/{inbox,board,workspace}/`, and `shared/`.
- **Strict write lanes** (locked): your Ken writes only inside your
  own member folder, plus *new files only* into other members'
  `inbox/`. Merge conflicts are impossible by construction.
- **Typed inbox items** (locked): small frontmatter files with
  `kind: task|message|notification` and
  `status: unread|seen|accepted|archived`, driven by the same
  frontmatter patch-rewrite core as ken-tasks.
- **Acceptance gate**: an incoming task never auto-enters your
  board or your agents' work queue. You (or a chat action) accept
  it; only then does it become a task file on your family board.
- **Tasks tab merge** (locked): family board tasks appear on the
  existing Kanban with a family filter chip; inbox items surface as
  a notification tray and daily-board candidates. Families is a
  settings page, not a new tab.
- **Indexing**: when a connection is attached to a workspace, the
  clone joins search as a member of kind `family`. `shared/` is
  **full tier** (locked) — it federates into the KG and routes like
  team documentation; `members/**` is search-only via built-in
  tier rules.
- **MCP tools** so external agents can list families, read their
  own inbox, and send items within lane rules.

## Capabilities

- **ADDED: ken-families** — connections, sync engine, template,
  lanes, inbox lifecycle, notifications, secretary actions.
- **MODIFIED: ken-tasks** — a third task home: family boards
  (`members/<you>/board/` per connection) merged into the Tasks
  tab and claimable over MCP.
- **MODIFIED: search / federated-kg / kg-routing** — a new member
  kind `family` with built-in tier rules and normal `ken://`
  addressing; `shared/` entities federate.
- **MODIFIED: settings** — Families page.

## Impact

- `crates/ken-core`: `family.rs` (manifest, lanes, inbox items),
  `family_sync.rs` (`trait GitTransport` + system-git impl + fake).
- `src-tauri`: connection store, poll scheduler, accept flow,
  notification events, Tasks-tab merge plumbing.
- `crates/ken-mcp`: family tools.
- Frontend: Families settings page, notification tray, board chips.
- New external requirement: system `git` on PATH — feature reports
  "unavailable" and stays inert without it (no error dialogs).
- Depends on: workspace (2), ken-tasks (7); richer with ken-memory
  (6) for journal notes on sync/accept events.

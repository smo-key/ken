# Design: ken-families

## Context

ken-tasks made tasks one-file-per-task frontmatter markdown with
ULID filenames; ken-memory made knowledge plain markdown; kenignore
gave every path a tier. A family repo is those same file formats in
a git repository shared by a team, with each member's Ken as the
only writer for their lane. This design adds the sync loop, the
repo template, and the delivery/acceptance flow — no new file
format is invented here.

## Goals / Non-Goals

- Goals: multi-team collaboration over plain git remotes; zero
  human editing of the family repo; conflict-free by construction;
  incoming work gated behind explicit acceptance; team knowledge
  searchable and federated.
- Non-Goals: real-time presence/chat, server components, access
  control beyond what the git host provides, CRDTs or merge
  resolution UIs, cross-family search ranking tuning.

## Decisions

### D1. Transport: system git CLI, clone in app data

Each connection clones to `<app data>/ken/families/<family-id>/`.
Sync loop per connection (only while `kenFamilies` on and live-sync
enabled): every `poll_interval` (default 120s) run `git fetch`;
integrate with `git pull --rebase` (lanes make conflicts
impossible — see D3); after any local write, commit and `git push`
(retry with a fresh pull --rebase on non-fast-forward).

Shelling out to the system `git` is deliberate: it reuses the
user's credential helpers (SSH agent, Windows Credential Manager,
gh auth) instead of reimplementing auth in-process via git2. If
`git` is not on PATH, the Families page shows "unavailable
{reason}" and the feature is inert — same degradation style as
`vec_available`.

Seam: `trait GitTransport { fetch, integrate, commit_paths, push,
head_status }` with `SystemGit` and `FakeTransport` (in-memory
"remote" for tests). All sync-loop logic is testable offline.

If a rebase ever *does* conflict (lane bug, hand-edited repo), the
connection enters an `error: conflict` state, polling stops, and
the user is told to resolve manually. Never auto-resolve, never
force-push.

**Phase 0 update (arbitrated 2026-07-24):** spike S8
(`spikes/S8-git-sync-windows.md`) settled the sync loop's recovery
procedure, arbitrated as D7:

- Rebase flag semantics: `-X ours` keeps upstream content on
  conflict, `-X theirs` keeps local — lanes make this moot in
  practice (D3), but the flag choice is pinned for the rare
  hand-edit case.
- `index.lock` is only ever deleted after a process-liveness check
  (confirm no `git` process actually holds it) — never blind-deleted
  on sight.
- `git status` exits 0 even mid-rebase, so sync-loop state is read by
  parsing its output, never by trusting its exit code.
- Credential prompts are suppressed unconditionally
  (`GIT_TERMINAL_PROMPT=0`, `GCM_INTERACTIVE=never`, plus an explicit
  `-c credential.helper=` override or stored PAT where configured) so
  a spawned `git` never hangs waiting on an interactive prompt.
- Per-clone config sets `core.longpaths=true`, `core.autocrlf=false`,
  `pull.rebase=true`; one clone per device.

Pre-ship checklist item (from S8, still open): verify Git Credential
Manager's behavior against a live HTTP 401 (expired/revoked
credential) before hosted-https family sync ships — the spike
exercised the happy path and induced failures, but not a real
expired-credential prompt/refusal cycle.

### D2. Repo template and manifest

```
family.json                       manifest (see below)
members/<member-id>/
  inbox/                          typed items addressed to me
  board/                          my tasks, visible to the team
  workspace/                      my AI working area (no-human)
shared/                           team knowledge (markdown)
  conventions.md                  Ken behavior contract (see D3)
```

`family.json`: `{ id: Uuid, name, template: u32, members:
[{ id, name }], #[serde(flatten)] extra }`. `template` is the
repo-template schema version, starting at 1. The template shape is
entirely ours to define, so it evolves freely behind this one
number; a manifest declaring a version newer than this Ken
supports makes the connection unavailable ("needs a newer Ken") —
no sync, no guessing at unknown structure. Member ids are short
stable slugs
(`"owner"`, `"sarah"`), not emails. "Create family" in settings
scaffolds this template into a new repo and commits it; "Join
family" clones an existing one and asks which member you are (or
adds you to the manifest — a manifest edit is the one allowed
write outside your lane, append-only to the `members` array).

### D3. Strict write lanes (locked)

Your Ken may write:

1. anything under `members/<you>/`, and
2. **new files only** under `members/<other>/inbox/` (ULID
   filenames guarantee uniqueness; never edit or delete another
   member's files), and
3. appends to `family.json`'s member array (join flow only).

Rule 2 is what makes "Ken puts stuff on people's boards" safe: a
sender *creates* an inbox item once; from then on only the
recipient's Ken mutates it. Every file has exactly one writer at
any time, so git merges are trivially clean.

`shared/` in v1 is written by lanes too: edits to shared knowledge
go through a proposal inbox item to the family's designated owner
member (first member in the manifest by default). Open question
below tracks loosening this.

Enforcement is code, not convention: `commit_paths` validates every
staged path against the lane rules for the local member id and
refuses the commit on violation (that's a bug, not a user error).

Trust model: everyone with push access to the family remote is
trusted. Lanes protect against *bugs*, not adversaries — a
malicious member already has git access and needs no lane
loophole. The human-level safeguards are behavioral: the receiving
Ken acts as secretary (D4) — it reviews, organizes, and can push
back on incoming items before anything is accepted — and every
member's side is Ken-driven, so the family repo ships strict
written conventions (`shared/conventions.md`, scaffolded by the
template) that each member's Ken loads and adheres to: lane rules
restated, inbox etiquette, what belongs in `shared/`, and how
push-back works.

### D4. Typed inbox items (locked) and the acceptance gate

An inbox item is a frontmatter file, patched with the same
patch-rewrite core as ken-tasks (spike S6):

```
---
id: <ulid>
kind: task | message | notification
from: <member-id>
status: unread | seen | accepted | archived
created / updated: <iso datetime>
title: <short line>
task: { title, project?, tags?, due?, kind: human|ai }   # kind: task only
---
free-form body (the message, or task context/brief)
```

Lifecycle: sender creates with `status: unread` → recipient's Ken
marks `seen` when surfaced → for tasks, **accept** copies the
payload into a new task file in `members/<me>/board/` (normal
ken-tasks frontmatter, `id` freshly minted, provenance noted in
the body's `## Log`) and sets the inbox item `accepted`; messages
and notifications go straight to `archived` on dismiss.

**The acceptance gate is a trust boundary**: nothing arriving from
a family repo enters your board, your daily board, or an agent's
claimable queue until accepted. Incoming content is other people's
input, not your instructions. No auto-accept in v1.

The recipient's Ken is a secretary, not a mailbox: on new items it
can summarize and group them in the tray, propose an accept /
push-back / archive per item, and — with approval — send a
push-back as a normal message item into the sender's inbox (its
own lane, rule 2). Push-back is how "review and negotiate" works
without anyone ever editing anyone else's files.

### D5. Identity and assignees

Settings store, per connection, which manifest member you are.
Family board tasks use manifest member ids in `assignee`; the
Tasks-tab merge maps your family member ids onto "me" for
filtering. MCP agents claiming family-board tasks use the same
claim-and-complete protocol as ken-tasks — claims only ever touch
`members/<you>/board/`, so lanes hold.

### D6. Indexing, addressing, federation

A connection may be **attached to one workspace**. Attached, the
clone is ingested as a member of kind `family` with project id =
the manifest `id`: normal `ken://<family-id>/<rel-path>` addresses,
listed by `list_projects` with `kind: "family"`, included in
Broadcast. Unattached connections still sync and notify but do not
join search.

Built-in tier rules (same mechanism as journal/tasks rules in
kenignore): `shared/**` **full** (locked — it's curated team
knowledge; its entities federate into the workspace KG),
`members/**` and `family.json` **search-only** — inboxes, boards,
and working folders are findable but never mint entities, never
feed the profiler. A member project is never *created* from a
family clone; it is its own member kind.

### D7. UI: merged Tasks tab, tray, settings page (locked)

- Tasks tab: family board tasks merge into the existing Kanban
  with a per-family filter chip; drag-drop status writes go
  through the family repo home (commit + push like any board
  write).
- A notification tray (badge on poll results): unread inbox items,
  grouped by family, with accept / dismiss inline. Accepted tasks
  become daily-board candidates like any other task.
- Settings → Families: connection list (remote, member identity,
  live-sync toggle, poll interval, attached workspace, last-sync
  status/error), plus Create / Join flows. No new top-level tab.

## Risks / Trade-offs

- **Polling latency** (up to poll interval) — acceptable for a
  secretary model; "Sync now" button covers impatience. Push-based
  webhooks are out of scope (no server).
- **Repo growth** — boards archive completed tasks to
  `board/archive/YYYY-MM/` like ken-tasks; inbox items archive in
  place. Git history growth is accepted; shallow clones if it ever
  matters.
- **Credential/host variance** — mitigated by using system git,
  but "clone failed" UX must show git's own stderr plainly. Spike
  S8 probes this on Windows. Phase 0 update (2026-07-24): S8 came
  back GO-with-caveats (D1 above); pre-ship checklist item — verify
  Git Credential Manager behavior against a live HTTP 401 before
  hosted https family sync ships.
- **A hand-edited "no-human" repo** — lanes are validated on our
  writes, but others' Kens (or humans) may misbehave. Reads are
  tolerant (bad frontmatter ⇒ item shown raw, never crash);
  conflicts stop sync per D1.

## Open Questions (resolve during build, defaults stated)

- Poll interval default: 120s (bounds 30s–30min).
- `shared/` write model: proposal-to-owner in v1; direct writes
  with last-writer-wins is the candidate loosening once real teams
  hit friction.
- Per-member auto-accept ("tasks from Sarah go straight to my
  board"): deferred, default off; requires explicit per-member,
  per-family opt-in when it comes.
- Message threading (reply-to): deferred; `body` quoting suffices
  in v1.

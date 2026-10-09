# Proposal: workspace-group-folders

## Why

Groups exist twice and satisfy no one. The manifest `groups` field
(ken-home-workspace) is Settings bookkeeping: invisible in the
filesystem, editable only through one screen, and disconnected from how
agents see the work. Meanwhile the user's actual grouping instinct is a
folder — "put all the SR repos into one folder" — because a folder is
visible everywhere, and a `CLAUDE.md` dropped in it auto-loads for every
agent session in any repo beneath it (Claude Code resolves context files
by walking up from the working directory).

The workspace change's D-decisions pinned members to direct children of
the workspace root (`workspace_add_member` hard-rejects separators;
discovery is one level deep by design D5). So the folder the user wants
to make cannot hold workspace members at all: moving
`ShatteredRealms` into `SR\` silently removes it from the workspace.

## What Changes

- **A member may live one level inside a *group folder*.** Manifest
  member names grow from one path segment to at most two
  (`SR/ShatteredRealms`), always forward slashes. The full string stays
  the identity everywhere (manifest, DTOs, task-home labels, events);
  the last segment is the display name.
- **The group folder IS a group.** `derived_groups()` synthesizes a
  `ProjectGroup` per group folder from the member paths alone;
  `effective_groups()` = derived first, then manifest groups whose names
  don't collide. Every group consumer (Home's scope picker, routed
  search, chat scope, the groups DTO) reads the effective view, so a
  folder group scopes questions exactly like a Settings group.
- **`set_group` refuses a name a group folder already owns** — a
  manifest group that `effective_groups` would shadow forever is a
  silent no-op the user would read as a bug.
- **Discovery descends into group folders.** A depth-1 folder that is
  not repo-ish itself (no `.ken/project.json`, no repo markers) but has
  at least one repo-ish immediate child is treated as a group folder:
  its children surface as `Group/Child` candidates and the container
  itself does not. Everything else keeps depth-1 candidacy unchanged.
- **Validation replaces the separator ban.** `validate_member_name`
  normalizes `\` to `/` and rejects empties, `..`, dot-prefixed
  segments, drive/absolute forms, and more than two segments — it is the
  single gate between user input and `parent.join`, so nothing it passes
  can escape the workspace root.
- **The merged Files tree emits group ancestor nodes**, and the
  frontend resolves a merged path to its member by longest-prefix match
  instead of first-segment split.
- **Chat scope stops degrading silently.** A named scope that resolves
  to zero members is now an error; previously the chat was sent with
  single-project scope while the UI claimed the group. (Bug found while
  sweeping for flat-member assumptions; fixed here because the shared
  resolver touches the same lines.)
- **Workspace-root `.kenignore` accepts nested entries**
  (`SR/scratch/`), and the ignored-folders listing walks two levels so
  write and read agree.

## Non-Goals

- Deeper nesting than one group folder. Two segments is a grouping
  convention; three is a filesystem hierarchy Ken should not model.
- Auto-migrating existing manifests. Renaming `ShatteredRealms` to
  `SR/ShatteredRealms` in `workspace.json` is a hand edit (or a
  one-time assisted move); the manifest format itself is unchanged.
- Group-level feature flags, digests, or KG scoping. A derived group is
  a *question scope*, the same thing a manifest group already was.

# Tasks: workspace-group-folders

## 1. Core (`crates/ken-core/src/workspace.rs`)

- [x] 1.1 `member_leaf` / `member_group` / `validate_member_name`
      helpers; doc updates on `WorkspaceConfig::members` (D6/D7).
- [x] 1.2 `derived_groups` / `effective_groups` /
      `effective_group_members`; `set_group` rejects derived names (D9).
- [x] 1.3 `Workspace::create` validates member names and passes the leaf
      as the project display name; `resolve_member_tolerant` likewise.
- [x] 1.4 Discovery: `group_folder_children` + the evidence rule (D8);
      `Candidate::name` documented as a relative path.
- [x] 1.5 `ignore_folder` accepts one nesting level via
      `validate_member_name`; nested rules match through the existing
      `classify` parent matching.
- [x] 1.6 Tests: validator shapes, derived/effective groups + collision,
      nested create/open leaf names, group-folder discovery,
      `SR/`-rule hiding, nested ignore round-trip.

## 2. Tauri (`src-tauri/src/lib.rs`)

- [x] 2.1 `workspace_add_member`: validator replaces the separator ban;
      leaf display name.
- [x] 2.2 `workspace_candidates`: exclude members AND group folders
      containing members (prefix filter).
- [x] 2.3 `workspace_ignored`: two-level walk, parent-relative rows; an
      ignored top-level folder reports alone.
- [x] 2.4 `get_tree_all`: emit group ancestor `FolderInfo` nodes,
      dedup after sort.
- [x] 2.5 `group_dtos` / `route_search` / `send_chat_message` read the
      effective group view; chat errors on an empty named scope.

## 3. Frontend

- [x] 3.1 `memberLeaf` / `memberGroup` in `api.ts`; doc on
      `WorkspaceMember.name`.
- [x] 3.2 `openTab` longest-member-prefix match.
- [x] 3.3 Leaf labels: WorkspaceSwitcher, MembersStrip, ScopePicker,
      scope store label, ProjectGroups (rows, checkboxes, candidates),
      creation wizard candidates ("X in SR/").
- [x] 3.4 ProjectGroups marks derived groups read-only ("from folder").

## 4. Spec

- [x] 4.1 Amend workspace design.md Non-Goals (nested members are now
      in scope via this change).

## 5. Verification

- [x] 5.1 `cargo test -p ken-core workspace::` green (23/23, release
      profile via `migration/test-workspace.bat`).
- [x] 5.2 `cargo check -p ken-app` clean; `npm run check` at baseline
      (0 errors / 18 pre-existing warnings).
- [ ] 5.3 Live: move the three SR repos under `SR\`, update the
      manifest, reopen — group appears in Home's picker, merged tree
      shows `SR/…`, chat scoped to SR names both repos, per-repo
      display names show leaves everywhere.

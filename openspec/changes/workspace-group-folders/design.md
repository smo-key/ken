# Design: workspace-group-folders

## Decisions

### D6. The leading segment of a nested member name IS its group

(Numbered continuing the workspace change's D1–D5.)

No new manifest field. `members: ["ken", "SR/ShatteredRealms",
"SR/sr-docs"]` already encodes the grouping — deriving it beats storing
it, because stored grouping can drift from the folders on disk and
derived grouping cannot. Case-insensitive name matching everywhere a
user types a group name, consistent with `group()`.

The workspace stays flat: one manifest, one root, one `.kenignore`
level above members. A group folder is not a nested workspace — it has
no manifest, no identity, no state. Deleting the last nested member
makes the group vanish from the effective view with no cleanup.

### D7. Full path = identity, leaf = display; no DTO widening

Every `name` field in DTOs, events, and task-home labels keeps carrying
the manifest key. The frontend derives labels with `memberLeaf()`
(`api.ts`) at render sites. Rationale: the alternative — adding
`label`/`group` fields to every member-shaped DTO — spreads the same
split across a dozen structs, and the sweep showed at least one
matcher (`tasks.svelte.ts` project-label matching) that MUST see the
same string on both sides. One vocabulary, derived display.

The one backend concession: `Project::create`/`resolve_member_tolerant`
pass `member_leaf(name)` as the project's *display* name, so
`project.config.name` (which already feeds chat preambles and the
registry) never shows a path.

### D8. Group-folder detection is evidence-based, not configured

A folder is a group folder iff it is not repo-ish itself AND wraps at
least one repo-ish child (existing `.ken/project.json` or repo
markers). No marker file, no registry of group folders. This keeps D5's
"shallow and dumb" discovery honest — still at most two `read_dir`
levels, still no recursion — while a folder of loose notes keeps its
old depth-1 candidacy because nothing inside it looks like a repo.

Known consequence, accepted: a group folder containing only
*not-yet-initialized, marker-less* repos reads as a plain folder. The
first `git init` (or adding one child as a member by hand) flips it.

### D9. Manifest groups stay, folder groups win name collisions

`effective_groups()` = derived groups, then non-colliding manifest
groups. Manifest groups remain the only way to group across folders
("Everything" = ken + SR/sr-docs), so the machinery stays; the
Settings editor marks derived groups read-only ("from folder") since
their membership is the filesystem's to change. `set_group` refuses
derived names at write time so the shadowing filter only ever matters
for hand-edited manifests.

## Deviations found and fixed in passing

- `send_chat_message` silently sent single-project scope when a named
  scope resolved to no members (unknown group, emptied group). Now an
  error. `route_search` already errored; the two agree.
- The merged-tree contract ("first segment names the member") was
  implicit in `openTab`. It is now longest-member-prefix, documented at
  both ends.

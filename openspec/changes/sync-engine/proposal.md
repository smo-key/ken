# Proposal: sync-engine

## Why

Ken projects are meant to be shared — the design doc (§3) makes all shared
state plain text precisely so folders can ride on git or a shared drive.
Today nothing moves that text: a git-backed project only syncs when the
user runs git by hand, and a Dropbox/OneDrive project can silently sprout
"conflicted copy" files that nobody reviews. Change 5 of 10 builds the
sync engine (§4.8): Ken drives git actively for git projects, watches for
conflicted-copy damage on shared drives, and turns every conflict either
path can produce into a Review item with an AI-drafted resolution —
without the user ever seeing git vocabulary.

## What Changes

- New `ken-core` module `sync.rs` — all git interaction in one place,
  shelling out to the user's `git` binary (their config, their auth;
  never a git library, never force/rebase, never touching global config):
  - `pull` = `git pull --no-rebase --no-edit`, throttled to once per
    60s, triggered on app-window focus and after project activation.
  - push-after-save = debounced 30s after watcher-detected changes:
    `git add -A && git commit && git push` (skip commit when nothing
    staged, skip push when no remote; a rejected push pulls and retries
    once).
  - `.git/info/exclude` gains `.ken/.staging/` and
    `.claude/settings.local.json` (idempotent) so Ken's transient files
    never enter the user's history.
- **Merge conflicts become Review items.** A conflicted pull is aborted
  cleanly (working tree restored), ours/theirs captured per file, and a
  stored review item of kind `conflict` filed with payload
  `{path, ours, theirs, draft}`. A background worker then drafts an AI
  merge (one at a time, `claude -p`, staged output — fake-claude
  compatible) into the payload. Resolutions: accept Ken's merge / keep
  mine / take theirs / edit manually — each writes the chosen content,
  resolves the item, and lets the normal push path sync it.
- **Shared-drive conflicted copies.** Watcher-reported paths whose file
  name matches conflicted-copy patterns ("… (conflicted copy …)",
  "… (Case Conflict …)") file a `conflict-copy` review item with
  `{copyPath, originalPath}`. Actions: keep the copy, keep the original,
  or open both in Files. Pattern detection is a pure function with unit
  tests.
- **Sync state surface.** A `sync-state` event
  (`off | synced | syncing | attention`) drives the title-bar dot
  (green / pulsing / danger with plain-language tooltip). Settings gains
  the prototype's "Sync & collaboration" card: git chip with remote +
  branch (mono, secondary detail), auto on/off toggle, Sync now button;
  shared-drive note otherwise. Project sync config lives in
  `project.json` `extra` as `"sync": {"auto": bool}` — default auto ON
  for git repos with a remote.
- Review screen renders the two new kinds: conflicts get the prototype's
  side-by-side "who changed what" cards + "Ken's take" + action buttons;
  conflicted copies get plain-language keep/open actions.

## Capabilities

### New Capabilities
- `sync-git`: active git sync for git-backed projects — pull on focus,
  debounced commit+push on save, exclude hygiene, sync-state surface,
  Settings card.
- `sync-conflicts`: conflict detection and resolution — merge conflicts
  and conflicted copies as Review items, AI merge drafts, resolution
  actions.

### Modified Capabilities

_None structurally — `review-inbox` already ships the stored-item
substrate (`review_items`, kind + payload) this change writes into; the
inbox read model gains payload passthrough and two kind mappings, which
its spec anticipated._

## Impact

- `ken-core`: new `sync.rs` (git helpers, conflicted-copy detection,
  AI merge drafts, `SyncEngine` background worker); `db.rs` gains
  `get_review_item` / `set_review_item_payload` (no schema change).
- `src-tauri`: sync engine wired into `activate()`, window-focus pull,
  new commands `sync_status`, `set_sync_auto`, `sync_now`,
  `resolve_conflict`, `resolve_conflict_copy`; `review_inbox` passes
  payload + conflict kinds; new events `sync-state`, `review-changed`.
- Frontend: api types/wrappers, app store sync state, TitleBar dot,
  review store + screen for the new kinds, new
  `src/review/ConflictDetail.svelte`, Settings card.
- Tests: git fixtures (bare repo + two clones in tempdirs), pattern
  unit tests, AI-draft test on the fake claude. No real network, no real
  Claude, nothing outside tempdirs.

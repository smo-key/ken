# Tasks: sync-engine

## 1. ken-core sync module

- [x] 1.1 `sync.rs` git helpers: `run_git` (GIT_TERMINAL_PROMPT=0), `is_git_repo`, `remote_and_branch`, `ensure_excludes` (idempotent), `pull` (merge-only, conflict capture via `:2:`/`:3:` then `merge --abort`), `commit_and_push` (skip empty commit / missing remote); git-fixture tests: clean pull propagates, divergent non-conflicting merges, conflicting pull aborts cleanly with ours/theirs extracted
- [x] 1.2 Conflicted-copy pure function `conflicted_copy_original` with positive/negative unit tests
- [x] 1.3 AI merge draft: `draft_merge` spawning `claude -p` headless with STAGING_DIR prompt contract; fake-claude test
- [x] 1.4 `SyncEngine` worker: pull throttle (60s), push debounce (30s, earliest-deadline), rejected-push pull-and-retry-once, conflict items (deduped) + draft queue worker, conflicted-copy items from changed paths, `State`/`ReviewChanged` notices; integration test on git fixtures + fake claude
- [x] 1.5 `db.rs`: `get_review_item`, `set_review_item_payload` (+ test)

## 2. Tauri integration

- [x] 2.1 Wire `SyncEngine` into `activate()` (watcher + initial-scan changed paths, pull after activate), window-focus pull, `sync-state` / `review-changed` events
- [x] 2.2 Commands: `sync_status`, `set_sync_auto`, `sync_now`, `resolve_conflict` (accept-draft | keep-mine | take-theirs | manual), `resolve_conflict_copy` (keep-copy | keep-original); register
- [x] 2.3 `review_inbox`: payload passthrough, stored kinds `conflict` / `conflict-copy`, severity order update

## 3. Frontend

- [x] 3.1 api.ts: SyncStatus/SyncStateEvent types, InboxItem payload + new kinds, wrappers, onSyncState/onReviewChanged
- [x] 3.2 app store sync state + TitleBar dot (synced green, syncing pulse, attention danger + tooltip)
- [x] 3.3 review store: new kinds in actionsFor/dotFor, conflict payload parsing, resolve actions, review-changed subscription; tests
- [x] 3.4 ReviewScreen + `src/review/ConflictDetail.svelte`: side-by-side cards, Ken's take / drafting notice, action buttons; conflict-copy actions
- [x] 3.5 Settings "Sync & collaboration" card (git chip + remote/branch + auto toggle + Sync now; shared-drive note otherwise)

## 4. Verification

- [x] 4.1 cargo test --workspace, pnpm test, pnpm check, pnpm build all green
- [x] 4.2 Commit

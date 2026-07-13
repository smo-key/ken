# Tasks: review-inbox

## 1. Stored-item substrate (ken-core)

- [x] 1.1 DB schema v4: `review_items` table + index, SCHEMA_VERSION bump; migration test v3→v4 following the v1→v2 pattern
- [x] 1.2 Review-item CRUD: insert, resolve, list open, list recently resolved; `runs_finished_since` for the Done section; unit tests

## 2. Tauri commands

- [x] 2.1 `review_inbox()` returning `{ items, done }` of camelCase InboxItems: approvals from pending runs, stale ingests (same derivation as list_ingests), failed files, broken recipes, open stored items; Done = last-7-days discarded runs + over-threshold fresh runs + resolved stored items
- [x] 2.2 `resolve_review_item` command; register both commands

## 3. Frontend

- [x] 3.1 api.ts: InboxItem/ReviewInbox types + reviewInbox/resolveReviewItem wrappers
- [x] 3.2 `review.svelte.ts` store: items/done/selected state, init() subscribing to onIngestRunChanged + onIndexUpdated, per-kind action dispatch; helper test
- [x] 3.3 ReviewScreen per prototype: inbox list pane (kind dots, source · time-ago captions), Done section, detail pane with plain-language body + action buttons, empty state card
- [x] 3.4 NavRail: Review count badge (danger pill) fed from the review store
- [x] 3.5 Home: aggregate "waiting in Review" card for open items beyond the approvals already shown

## 4. Verification

- [x] 4.1 cargo test --workspace, pnpm test, pnpm check, pnpm build all green
- [x] 4.2 Commit

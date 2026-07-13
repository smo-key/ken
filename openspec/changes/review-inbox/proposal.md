# Proposal: review-inbox

## Why

Ken already asks the user for things — large refresh approvals, ingests
going stale, files it couldn't read, recipes it couldn't parse — but each
lives on a different screen and nothing counts them up. The design doc
(§4.7) promises one inbox for everything Ken needs a human for, with a nav
badge showing the count. This change (4 of 10 in the build order) builds
that inbox as a unified view assembled at read time from state the app
already keeps, plus a small stored-item substrate so later changes (sync
conflicts in change 5, AI questions from chats) can file items without
reshaping the UI.

## What Changes

- One Tauri command `review_inbox()` returning `{ items, done }` of
  `InboxItem`s, assembled at read time — no duplicated state:
  - **Large refresh approvals** from `ingest_runs` with status
    `pending_approval` (actions: Approve / Discard, reusing the existing
    engine operations).
  - **Stale ingests** derived exactly as `list_ingests` derives them
    (last fresh run older than the resolved stale window) — action: Run
    now.
  - **Files that couldn't be indexed** from `files` rows with status
    `failed` — action: open in Files.
  - **Broken recipes** from `recipe::list` — action: open in Ingests.
  - **Stored items** from the new `review_items` table (empty in this
    change; future kinds plug in here).
- DB schema v3→v4: `review_items` table + CRUD (insert, resolve,
  list open, list recently resolved) with tests. Nothing inserts into it
  yet in this change.
- Review screen per the prototype: inbox list pane (colored dot by kind,
  "source · time-ago" caption), detail pane with plain-language body and
  per-kind action buttons, a Done section of recently resolved items, and
  a friendly empty state.
- Nav rail: a count badge on Review (danger pill per the prototype) fed
  from a new frontend store that refreshes on ingest-run and index events.
- Home: the "Waiting on you" section gains one aggregate card linking to
  Review when items beyond the already-shown approvals are open.

## Capabilities

### New Capabilities
- `review-inbox`: the unified inbox — item assembly, per-kind actions,
  done section, stored-item substrate, nav badge, empty state.

### Modified Capabilities

_None — approvals, staleness, failed files, and broken recipes keep their
existing sources of truth; the inbox is a read model over them._

## Impact

- `ken-core`: `db.rs` schema v4 (`review_items` + migration), review-item
  CRUD, `runs_finished_since` query for the Done section.
- `src-tauri`: new commands `review_inbox`, `resolve_review_item`.
- Frontend: new `review.svelte.ts` store, rebuilt `ReviewScreen.svelte`,
  badge in `NavRail.svelte`, aggregate card in `HomeScreen.svelte`,
  api types/wrappers.
- No new dependencies; no changes to project-folder files.

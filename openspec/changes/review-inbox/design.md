# Design: review-inbox

## Context

Changes 1–3 shipped indexing/search, AI ingests with review rules, and the
chat drawer. Everything Ken needs a human for already exists as state
somewhere — pending-approval runs in `ingest_runs`, staleness derivable
from run history + rules, failed files in `files`, broken recipes from
`recipe::list` — but it's scattered across Ingests, Files, and Home.
Change 5 (sync engine) will add merge conflicts and chats will add AI
questions; both need a place to file items that no existing table models.

## Goals / Non-Goals

**Goals:**
- One inbox, one count. Everything Ken is waiting on, in one list, with
  the nav badge as the single "does Ken need me?" signal.
- No duplicated state: whatever already has a source of truth is derived
  from it at read time, so the inbox can never disagree with the screens
  it links to.
- A stored-item substrate (`review_items`) sized so change 5 and the chat
  drawer only insert rows — the inbox DTO, store, and UI already handle
  the `stored` kind.

**Non-Goals:**
- Merge conflicts and AI questions (changes 5+ / chat work) — the two
  remaining §4.7 item types. Nothing inserts into `review_items` in this
  change; the table, CRUD, and UI plumbing ship empty and tested.
- Diff rendering for approvals — Approve/Discard with the run's
  plain-language summary is the v1 detail; "open in editor" review comes
  with later editor work.
- Scheduled re-runs of stale ingests (stale stays a flag + Run now).

## Decisions

1. **Unified view assembled at read time** — `review_inbox()` builds
   `InboxItem`s on every call:
   - *approval*: `ingest_runs` where status = `pending_approval`. Actions
     reuse the existing `approve_run` / `discard_run` commands.
   - *stale*: derived exactly like `list_ingests` (last run is `fresh`
     and older than resolvedRules.staleDays). Action: `run_ingest`.
   - *failed-file*: `files` rows with status `failed`. Action: open in
     Files (the existing fallback preview shows the reason).
   - *broken-recipe*: `recipe::list` Broken entries. Action: open in
     Ingests.
   - *stored*: open rows of `review_items`, merged into the same list so
     future kinds need no DTO or UI reshaping.
2. **InboxItem DTO** — `{ id, kind, title, body, when, sourceRef }`
   (serde camelCase). `id` is kind-prefixed and stable across refreshes
   (`run-12`, `stale-people`, `file-notes/x.pdf`, `broken-people`,
   `item-3`) so selection survives a refresh; numeric ids for actions are
   parsed back out of it client-side. `actions` are derived client-side
   from `kind` — the backend stays a pure read model. `title`/`body` are
   composed in Rust in plain language; `sourceRef` is the slug or rel
   path the actions target and feeds the list caption.
3. **Ordering** — items sort by kind severity (approval, stored,
   broken-recipe, failed-file, stale — mirrors the prototype's top-down
   order) and newest-first within a kind.
4. **Done section (simplest honest cut)** — last 7 days of:
   - `discarded` runs (only reachable from `pending_approval`, so each
     one was a resolved inbox item);
   - `fresh` runs whose change_ratio exceeded their recipe's resolved
     threshold — i.e. runs that were held and approved. Known honest
     rough edge, documented here: a first full build of a new output also
     exceeds the threshold and appears as "refresh applied", which is
     true, just not the result of an approval. Distinguishing would need
     per-run threshold history; not worth a schema column yet.
   - resolved `review_items`.
5. **DB schema v4** — `review_items(id INTEGER PK, kind TEXT, title TEXT,
   body TEXT, source_ref TEXT, status TEXT 'open'|'resolved', payload
   TEXT, created_at INT, resolved_at INT)` + index on (status,
   created_at). CRUD: `insert_review_item`, `resolve_review_item`,
   `list_open_review_items`, `list_recent_resolved_review_items(since)`.
   `payload` is free-form JSON for kind-specific data (change 5 will use
   it for conflict details). Additive migration in the existing
   `migrate()` chain; `v3_db_migrates_to_v4` test follows the v1→v2
   pattern. **Nothing writes rows in this change** — the substrate ships
   ahead of its producers so the inbox contract is fixed now.
6. **`resolve_review_item` command** — thin wrapper over the CRUD so the
   UI can close a stored item generically ("Mark as done"); kind-specific
   actions arrive with the kinds that use them.
7. **Frontend store** — `review.svelte.ts` mirrors `ingests.svelte.ts`:
   a class with `$state`, `init()` subscribing to `onIngestRunChanged` +
   `onIndexUpdated` (both refresh the inbox; approvals/staleness move
   with run events, failed files with index events). `ReviewScreen`
   stays mounted in the shell like every screen, so `init()` runs at
   startup and the nav badge is live without visiting Review.
8. **Review screen** — per the prototype's REVIEW section: 272px list
   pane on `--sunken-2` with an INBOX header, rows of dot + title +
   "source · time-ago" caption, a DONE header below with muted ✓ rows;
   detail pane (680px measure) with serif title, mono source line,
   plain-language body card, and action buttons (primary styled
   `.btn-primary`). Dot colors by kind: approval `--accent`, stored
   `--needs-input`, failed/broken `--danger`, stale `--ink-tertiary`.
   Empty inbox: a friendly "Nothing needs you right now" card.
9. **Nav badge** — count pill per the prototype (min-width 15px, height
   15px, `--danger` bg, paper border) on the Review rail item, showing
   `review.items.length`. The Files failed-dot stays as-is.
10. **Approve/discard emit `ingest-run-changed`** — the engine only emits
    for runs it executes; approvals and discards resolve runs from a
    command. Those two commands now emit the same event so both the
    ingests and review stores (and the nav badge) stay current no matter
    which screen the action came from.
11. **Home wiring** — approvals and blocked runs keep their individual
    cards (already shipped). One new aggregate card appears when other
    kinds (stale / broken-recipe / stored) are open: "N more things are
    waiting in Review", linking to the Review screen. Failed files are
    excluded from that count because Home already lists them under
    "Needs a look".

## Risks / Trade-offs

- [Read-time assembly re-lists recipes and files on every refresh] →
  both are already cheap (one dir read, one indexed table scan) and the
  same cost `list_ingests`/`get_tree` pay; fine at project scale.
- [Approved-run detection via ratio > threshold misreads first runs] →
  accepted and documented (Decision 4); worst case is a harmless extra
  Done row.
- [Recipe renamed/deleted while a run is pending] → item falls back to
  the slug for its title; Approve/Discard still work through the run id
  (approve fails gracefully if the recipe is gone).
- [Two event subscriptions can double-refresh] → refreshes are idempotent
  reads; no writes, no flicker beyond a state swap.

## Migration Plan

DB v3→v4 is additive (one CREATE TABLE + index) inside the existing
migration chain; v3 DBs upgrade in place on open. Rollback = git revert;
v4 DBs open fine under v3 readers (extra table ignored) but the app ships
forward-only.

## Open Questions

None blocking. Wording of item bodies can be tuned freely — the contract
is the DTO shape, not the copy.

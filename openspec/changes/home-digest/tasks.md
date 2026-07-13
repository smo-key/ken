# Tasks: home-digest

## 1. ken-core

- [x] 1.1 `runner.rs::test_support`: fake claude prints a JSON-escaped `oneshot_result` file (when present next to the script) as the headless `result`; all existing behaviors unchanged
- [x] 1.2 `assistant.rs`: `OneshotOutcome` + `oneshot(binary, project_root, prompt, timeout, cancel)` mirroring `run_headless` but returning the parsed `result` string; register module in `lib.rs`
- [x] 1.3 assistant tests against the fake claude: success returns the file's text, default result without the file, failure (`headless-fail`), cancel, timeout, missing binary
- [x] 1.4 `db.rs`: SCHEMA_VERSION 5, `digests` table migration, `upsert_digest` / `get_digest(date)` / `latest_digest`; tests incl. `v4_db_migrates_to_v5`
- [x] 1.5 `digest.rs`: `gather(db, since)` (changed files by mtime, finished runs, open stored review item titles), `is_quiet`, `QUIET_DIGEST` copy, `compose_digest_prompt`, `parse_digest`; register module; unit tests for composer and parser (incl. missing-SOURCES tolerance)

## 2. src-tauri

- [x] 2.1 `chrono` dependency; local-date (`yyyy-mm-dd`) and local-hour helpers
- [x] 2.2 `maybe_generate_digest(app, state, force)`: has-digest / ≥07:00 / claude-present / in-flight gates, quiet-day fallback stored without Claude, background thread with 3-min oneshot → upsert → `digest-updated` event with the parsed row
- [x] 2.3 Call the check from `activate()` and from the existing window-Focused handler (alongside the sync pull); `refresh_digest` and `current_digest` commands; register
- [x] 2.4 `quick_answer(query)` command: top 8 FTS hits, `<mark>` stripped, grounding prompt, 60s oneshot in a thread, `quick-answer` event `{query, body, sources}`; returns false when claude is missing

## 3. Frontend

- [x] 3.1 `api.ts`: `DigestDto`, `QuickAnswer` types; `currentDigest`, `refreshDigest`, `quickAnswer` wrappers; `onDigestUpdated`, `onQuickAnswer` listeners
- [x] 3.2 `src/lib/assist.ts`: `isQuestionQuery` (≥3 words or trailing "?") + `digestMarkdown` share formatter; tests
- [x] 3.3 `src/lib/digest.svelte.ts` store: digest row, generating flag, claude availability, init/subscribe/writeNow
- [x] 3.4 HomeScreen digest card: overline + time + share ("Copied" transient), `renderMarkdown` body, mono source chips → `app.openInFiles`; empty/missing-claude/generating states; drop the digest mention from Coming-to-Ken
- [x] 3.5 SearchOverlay: debounced quick-answer requests with per-query cache, card above Matches only when the answer matches the live query, stale answers dropped, `⌘↵ continue in chat` (footer hint + handler via `chats` store)

## 4. Verification

- [x] 4.1 cargo test --workspace, pnpm test, pnpm check, pnpm build all green; openspec validate home-digest
- [x] 4.2 Commit

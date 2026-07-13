# Proposal: home-digest

## Why

Ken's design promises a Home screen that greets you with **Today's
digest** — an AI-written paragraph covering what changed, what Ken did,
and what it's holding for review (§4.4) — and a **Quick answer** card in
the ⌘K overlay: a short AI answer with source chips that fills in above
the instant matches and never blocks them (§4.1). Today Home shows a
static stats card and the search overlay's footer says "AI answers
arrive in an upcoming release". Change 7 of 10 ships both, on top of a
new one-shot assistant runner in ken-core that both features (and future
ones) share.

## What Changes

- **New ken-core module `assistant.rs`** — `oneshot(binary,
  project_root, prompt, timeout, cancel)` spawns `claude -p` headless
  (same CLI contract as `runner::run_headless`) but parses and returns
  the `result` string from the output JSON instead of only
  pass/fail. Reuses `runner::CancelToken`. The fake claude in
  `runner.rs::test_support` learns one new trick: when an
  `oneshot_result` file sits next to the script, headless mode prints
  its contents (JSON-escaped) as the `result` — every existing behavior
  stays intact.
- **Digest storage** — DB schema v5 adds `digests(id, date /* yyyy-mm-dd
  local, UNIQUE */, content, created_at)` with `Db::{upsert_digest,
  get_digest, latest_digest}`. Content is the raw model output (body
  paragraph + optional `SOURCES:` line); parsing happens at read time so
  a malformed generation can never corrupt storage.
- **Digest generation** (ken-core `digest.rs`) — gathers the last 24h
  from what already exists (files by `mtime`, ingest runs via
  `runs_finished_since`, open stored review items), composes a prompt
  asking for one warm plain-language paragraph (~120 words, **bold**
  allowed) plus a `SOURCES: path1, path2` line (≤5 project-relative
  paths), and parses the result into `{body, sources}` tolerating a
  missing SOURCES line. Derived review kinds (stale ingests, failed
  files) are not folded in — stored items only; see design.
- **Digest scheduling** (src-tauri) — on project activate and on every
  window focus (alongside the sync pull that's already there), a cheap
  check: today already has a digest → nothing; otherwise if it's past
  7:00 local, claude is installed, and no generation is in flight, a
  background thread runs oneshot (3 min timeout), stores the row, and
  emits `digest-updated`. A `refresh_digest` command force-regenerates
  today's. A day with zero changed files, zero finished runs, and
  nothing waiting stores a quiet fallback — "A quiet day — nothing new
  since yesterday." — without calling Claude.
- **Home digest card** per the prototype: TODAY'S DIGEST overline,
  generated-at time, `share` link copying the digest as markdown
  ("Copied" transient), body rendered through the existing
  `renderMarkdown`, mono source chips opening in Files. Honest states:
  no digest yet + claude present → invitation with a "Write it now"
  button; claude missing → one honest line; generating → subtle pulse.
- **⌘K Quick answer** — when the query looks like a question (≥3 words
  or ends with "?"), an 800ms debounce calls new command
  `quick_answer(query)`: top 8 FTS hits (paths + `<mark>`-stripped
  snippets) go to oneshot (60s timeout) with a strict grounding prompt
  (answer in 1–2 sentences from this material only, name sources, say
  "don't know" otherwise), and the answer comes back as a `quick-answer`
  event. The overlay shows the clay-tinted card above Matches only when
  the event's query matches the current input; instant FTS matches are
  never blocked or reordered; new keystrokes hide stale cards; answers
  are cached per query for the overlay's lifetime. Footer gains
  `⌘↵ continue in chat`, which opens the chat drawer, creates a chat,
  and sends the query. Claude missing → no card, no error.

## Capabilities

### New Capabilities
- `home-digest`: the daily digest — one-shot assistant runner, storage,
  generation, scheduling, and the Home card.
- `quick-answer`: the ⌘K Quick answer card and its `⌘↵ continue in
  chat` handoff.

### Modified Capabilities

_None — Home's existing cards and the overlay's instant FTS matches keep
working exactly as before; both features are additive layers._

## Impact

- `crates/ken-core`: new `assistant.rs` (oneshot runner) and `digest.rs`
  (gather/compose/parse); `db.rs` v5 migration + digest accessors;
  `runner.rs` test_support extension; `lib.rs` module lines.
- `src-tauri`: digest scheduling (activate + Focused handler),
  `refresh_digest`, `current_digest`, `quick_answer` commands; `chrono`
  dependency for local dates.
- Frontend: `api.ts` digest/quick-answer types + wrappers; new
  `src/lib/digest.svelte.ts` store; `src/lib/assist.ts` helpers
  (question detection, share markdown) + tests; Home digest card;
  SearchOverlay quick answer card + ⌘↵; Coming-to-Ken card loses the
  digest mention.
- Tests: assistant oneshot against the fake claude (success, cancel,
  timeout, failure, missing binary), digest compose/parse, db v5
  including a v4→v5 upgrade test, frontend helper tests. Nothing touches
  the real app-data dir.

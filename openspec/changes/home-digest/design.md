# Design: home-digest

## Context

Changes 1–6 shipped the index, ingest engine (with the headless/hidden-TUI
runner and the fake-claude test harness), the review inbox, the chat
drawer, git sync (which owns the window-Focused pull), and the MCP
server. The design doc fixes two surfaces this change fills in: Home's
Today's-digest card (§4.4) and the ⌘K Quick answer card (§4.1). The
prototype nails the visuals: digest card with overline/time/share, prose
body with inline emphasis, mono source chips; quick answer as a
clay-tinted card above Matches with `⌘↵ dig deeper in chat`.

## Goals / Non-Goals

**Goals:**
- One shared, tested primitive for "ask Claude once, get text back"
  (`assistant::oneshot`) — the digest and quick answer are its first two
  callers.
- Digest generation is scheduled, resumable, and honest: at most one row
  per local day, generated after 7:00 on activate/focus, quiet-day
  fallback without burning an AI call, force-refresh on demand.
- Quick answer never degrades search: FTS matches render instantly and
  are never blocked, reordered, or replaced; the AI card is purely
  additive and stale answers are dropped.
- Everything testable offline via the existing fake claude.

**Non-Goals:**
- Digest history UI (rows accumulate in the DB; only today's shows).
- Waiting-on-you cards on Home beyond what already exists there.
- Streaming quick answers or multi-turn refinement — `⌘↵` hands off to
  the chat drawer for that.
- A general scheduler/cron; the check runs on activate and focus, which
  matches the design's "first app focus of the day" semantics.

## Decisions

1. **`assistant::oneshot` mirrors `run_headless`, returns the text.**
   Same CLI contract (`claude -p <prompt> --output-format json
   --permission-mode acceptEdits --session-id <uuid>`), same
   cancel/timeout/kill discipline, same `CancelToken`. The one
   difference: it parses the output JSON and returns
   `OneshotOutcome::Completed(result_string)`. Keeping it a separate
   module (not a `RunOutcome` variant) avoids threading a payload
   through every ingest call site that doesn't want one.
2. **Fake claude grows an `oneshot_result` hook, additively.** A file
   named `oneshot_result` next to the script makes headless `complete`
   mode print `{"is_error":false,"result":"<contents JSON-escaped>"}`.
   No behavior file value changes meaning; every existing test still
   sees `{"is_error": false, "result": "done"}` when the file is absent.
3. **Digest content is stored raw, parsed at read time.** `digests.content`
   holds exactly what the model returned (paragraph + optional
   `SOURCES:` line). `digest::parse_digest` splits it into `{body,
   sources}` and tolerates a missing/mangled SOURCES line (everything
   becomes body, sources empty). Storage can't be corrupted by a bad
   generation, and reparsing improvements apply retroactively.
4. **Digest inputs are what the DB already knows.** Files changed in the
   last 24h (`files.mtime`, capped at 20 paths in the prompt), ingest
   runs finished in the last 24h (`runs_finished_since`), and open
   *stored* review items (`list_open_review_items`). Derived inbox kinds
   (stale ingests, failed files, pending approvals) are intentionally
   skipped: assembling them means re-running the whole `review_inbox`
   walk inside ken-core, and the digest paragraph only needs "what's
   waiting" color, which stored items (conflicts, AI questions) carry.
   The prompt tells the model these are open review items so the wording
   stays honest.
5. **Quiet-day fallback is decided before spawning.** Zero changed
   files, zero finished runs, and zero open items → store
   "A quiet day — nothing new since yesterday." directly and emit
   `digest-updated`. No Claude call, no thread.
6. **Local dates live in src-tauri, not ken-core.** The `date` key is
   the user's local `yyyy-mm-dd` and the ≥07:00 gate is local time;
   `chrono` (already transitively compiled by the tauri stack) computes
   both in the app layer. ken-core's digest module takes epoch instants
   and stays timezone-free.
7. **One generation in flight, guarded by an `AtomicBool`.** Activate
   and every window focus call the same cheap `maybe_generate_digest`;
   the flag (owned by the shared app state, swapped before spawning)
   makes double-fires harmless. `refresh_digest` uses the same path
   with `force: true` (skips the has-digest and 7:00 gates, still
   respects claude-missing and in-flight).
8. **Quick answer is event-shaped, not request/response.** The command
   returns immediately (`false` when claude is missing so the overlay
   can stop asking); the answer arrives as a `quick-answer` event
   `{query, body, sources}`. The overlay compares the event's query to
   the live input — stale answers are silently dropped, and each
   successful answer is cached in a Map for the overlay's lifetime so
   retyping a query is instant. FTS list rendering is untouched: the
   card mounts above the Matches section only when an answer for the
   current query exists.
9. **Grounding prompt for quick answers.** Top 8 FTS hits, `<mark>`
   tags stripped from snippets, with an instruction to answer in one or
   two sentences using ONLY the provided material, name the source paths
   used, say "don't know" when the material doesn't answer, and end
   with `SOURCES: …`. Parsing reuses `digest::parse_digest` — same
   contract, same tolerance.
10. **`⌘↵ continue in chat`** opens the drawer, creates a chat via the
    existing `chats` store, and sends the raw query — the same flow a
    user would do by hand, so status, persistence, and errors all ride
    the existing rails.

## Risks / Trade-offs

- **Digest quality depends on FTS-adjacent metadata only** (paths,
  run summaries, item titles — not diffs). Accepted for v1: the prompt
  asks for a concrete, warm summary of *what* changed and *what's
  waiting*, and the model can read files in the project if it wants —
  it runs with cwd at the project root under `acceptEdits`.
- **A 3-minute digest timeout on a focus event** could waste a thread if
  the machine is offline. The thread is detached and the in-flight flag
  clears on completion either way; a failed generation stores nothing
  and retries on the next focus.
- **`date` uniqueness vs. timezone travel:** crossing timezones can make
  a "new" local day repeat or skip. `UNIQUE(date)` + upsert makes both
  harmless (regenerate-or-keep, never a crash).
- **Quick answer cost:** one headless session per settled question-ish
  query. The 800ms debounce, the ≥3-words/question gate, and the
  per-query cache keep the call rate low; answers never block anything
  user-facing.

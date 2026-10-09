# Tasks: ken-memory

## 1. ken-core

- [x] 1.1 `memory.rs` (new): memory-file frontmatter model
      (`#[serde(default)]`, flattened extras preserved on rewrite),
      parse/serialize round-trip, slug validation; register in
      `lib.rs`
      — `Frontmatter`/`Memory`/`parse_memory`/`render_memory` follow
      the serde_yaml `Mapping`-flatten idiom already shipped for
      `recipe.rs`/`automation.rs` (not spikes/S6's raw-line-splitter
      recommendation — recorded as a conflict in the module doc
      comment); registered in `lib.rs` alphabetically after `local_llm`.
- [x] 1.2 `memory.rs`: `write_memory(scope, slug, content)` (create
      ⇒ collision error; replace ⇒ body swap + `updated` bump) and
      `append_journal(text, project?, tags?)` (`## HH:MM` entry,
      creates today's file) — path resolution only, no engine calls
      — `WriteMode::{Create,Replace}` made explicit (spec.md's own
      scenario says "called in create mode"), since a single
      implicit upsert can't satisfy both "collision on create errors"
      and "replace swaps body" on the same slug without contradiction.
- [x] 1.3 `memory.rs`: injection builder — collect workspace +
      focused-project memories, order by `updated` desc, 4,000-char
      whole-file budget with description-line fallback; pure over an
      in-memory file list
      — `build_injection(&[Memory]) -> String`; budget accounts for
      each memory's own rendered block only (not the `## Memories`
      wrapper); empty input yields `""`, no bare heading.
- [x] 1.4 `memory.rs`: archive roll — journal files > 30 days old
      move to `journal/archive/`, idempotent, returns moved list
      — `roll_archive` diffs `YYYY-MM-DD` filenames against a
      caller-passed `today` via a local Howard-Hinnant
      `days_from_civil` (no date/time crate is a ken-core dependency).
- [x] 1.5 `memory.rs`: distillation `compose_distill_prompt`
      (journal window + existing descriptions) /
      `parse_distill_candidates` (tolerant, cap 5, garbage ⇒ empty)
      — JSON-object contract (`{"candidates":[...]}`), same
      find-braces-then-parse tolerance as `knowledge_model.rs`.
- [x] 1.6 Workspace pseudo-member: reserved namespace-uuid from
      workspace id; built-in tier rules (memory/ full, journal/ +
      tasks/ search-only, workspace.json + kg.sqlite ignored) wired
      through the `kenignore` tier filter
      — `workspace_pseudo_member_id` hashes the workspace id with two
      seeded `XxHash64` passes into a version-8/RFC4122-variant UUID
      (no `v5` in this workspace's `uuid` feature set; adding one was
      out of scope) instead of `Uuid::new_v5`. `workspace_builtin_rules()`
      is a **new, separate** function, deliberately NOT folded into
      `kenignore::built_in_rule_sets()`: that function is parameterless
      and called for every project's classify (`scan.rs`), so filling
      it with workspace-only patterns would leak them onto every
      ordinary member; `kenignore.rs` itself is untouched — see its
      doc comment for the full reasoning. Left for src-tauri task 2.1
      to fold `workspace_builtin_rules()` into the pseudo-member's own
      classify call specifically.
- [x] 1.7 Tests: frontmatter round-trip incl. unknown keys and
      no-frontmatter files; injection ordering + budget cutoff;
      journal append formatting + midnight boundary; archive roll
      idempotence; distill parse fixtures (valid / partial /
      garbage); slug collision
      — 21 tests added in `memory.rs`'s `#[cfg(test)]` module, all
      tempdir-based with explicit caller-passed dates/times, no
      wall-clock reads.

## 2. src-tauri

- [x] 2.1 Spin up the pseudo-member engine on workspace open when
      `kenMemory` is on (folders created lazily on first write, not
      on open); include it in ⌘K fan-out and routing Broadcast;
      exclude from member list, profiler, federation
      — `activate_memory_pseudo_member` reuses `activate()` unchanged,
      rooted at `.ken-workspace/` under `workspace_pseudo_member_id`;
      `.ken-workspace/` already exists (workspace-open precondition), so
      only `.ken-workspace/.ken/` (this pseudo-project's own metadata,
      via `Project::save`) is created eagerly — memory/journal/tasks stay
      lazy. D3's built-in tier rules can't reach `classify` as a third
      `rule_sets` slot without editing `scan.rs`/`kenignore.rs` (out of
      touch-boundary), so they're written through the SAME
      `Project::kenignore_rules()` disk-read channel as a generated,
      regenerated-every-open `.ken-workspace/.kenignore` (reverse of
      `kenignore::parse`) — functionally identical Rules, different
      channel; recorded as a judgment call, see `activate_memory_pseudo_
      member`'s doc comment. `route_search` already reaches it for free
      (iterates `AppState::members` with no filter); `search_all_
      projects`'s non-routed FTS branch iterates the workspace manifest's
      own `ws.ws.members` instead, which the pseudo-member is deliberately
      never in, so it's folded in by hand from `AppState::members` there.
      Excluded from `workspace_overview` "by construction" (never added to
      `ws.ws.members`, no filter needed); excluded from `profile_project`
      and both federation loops (`start_workspace_kg_build`'s member_ids,
      `workspace_kg_overview`'s loop) by an explicit `memory_pseudo_
      member_id` check. Known accepted side effects of reusing `activate()`
      wholesale: pseudo-member also gets a Registry entry (recents-list
      leak, not in this task's named exclusion list), a harmless chat
      drawer, and a passive sync engine.
- [x] 2.2 Commands: `memory_write`, `journal_append`,
      `read_journal(days_back)`, `distill_journal`,
      `resolve_distill_candidate(slug, approve)`; dismissed slugs
      persisted in workspace user-state; `memory-state` events
      — `memory_write(scope, slug, content, mode)`: `scope` is
      `"workspace"` or `"project"` (= the focused member, no explicit
      project-id param — keeps the tool surface small); `mode` is
      `"create"`/`"replace"` spelling out `WriteMode`. `distill_journal`
      caches its candidates in new `AppState::memory_distill_candidates`
      so `resolve_distill_candidate`'s 2-arg contract can look one up by
      slug. `UserState` (`ken_core::user_state`) is per-project with no
      `extra`/catch-all field and is out of this task's touch-boundary, so
      dismissed slugs reuse the pseudo-member's OWN `UserState.ignored`
      set (it already has a real project id) with a `distill-dismissed:`
      prefix — the honest workspace-level home available without editing
      `user_state.rs`. `memory-state` mirrors `WorkspaceKgStateEvent`'s tag
      shape: `planning`/`distilling`/`ready{candidates}`/`error{reason}`,
      app-global (`app.emit`, not `emit_member`) like the other
      whole-workspace state events.
- [x] 2.3 Chat wiring: `## Memories` block injected into system
      context; `memory_write` / `journal_append` / `read_journal`
      exposed as chat tools; archive roll triggered on workspace
      open (background)
      — Injection: composed at `send_chat_message`'s call site (workspace
      + focused-project memories via `memory::build_injection`), not
      inside `chat::build_context_preamble` itself (ken-core/chat.rs is
      outside this task's touch-boundary) — "extend its usage" rather than
      the function. Chat tools: NOT wired. Ken's chat is a spawned Claude
      Code CLI subprocess (`chat.rs::ChatEngine`) with no `--mcp-config`
      or any tool-declaration mechanism in this codebase today (confirmed
      by reading the CLI invocation directly) — per this task's own
      escape hatch, deferred with this note rather than inventing a tool
      protocol. Archive roll: background thread spawned from
      `open_workspace_inner`, ignore-errors-log-stderr, using the already-
      dependency `chrono` (`local_date_today()`/new `local_time_hhmm()`
      sibling) for the caller-supplied date/time strings core requires.
- [x] 2.4 `kenMemory` flag gate: off ⇒ no pseudo-member, no
      commands, no injection, no folder creation
      — Registered in `features.rs` as `FlagScope::Workspace` (mirrors
      `federatedKg`/`kgRouting`); registry test count bumped 5→6 with an
      assertion block for it. `ken_memory_enabled()` in src-tauri gates:
      pseudo-member spin-up (`open_workspace_inner`), all 5 new commands,
      and the chat injection — each checks the flag itself rather than
      trusting a single call site, matching `federated_kg_enabled`/
      `kg_routing_enabled`'s own defense-in-depth precedent.

## 3. ken-mcp

- [x] 3.1 `memory_write(scope, slug, content, mode?)` and
      `journal_append(text, project?, tags?)` tools delegating to
      the same core paths; descriptions state the journal is where
      agents report task findings, with `ken://` citations; tools
      absent when `kenMemory` is off (mirrors the kgRouting gating,
      defense-in-depth re-checks in handlers). Deviations: optional
      `mode` enum (default create) so agents can update their own
      memories; timestamps are UTC (ken-mcp has no local-time source
      — documented in the tool output); workspace resolved via
      `Registry::last_workspace`, clear "no workspace" error
      otherwise.
- [x] 3.2 Tests: schema round-trip; write lands in the right file
      (workspace + project scopes, collision, replace); flag off ⇒
      tool list byte-identical; no-workspace error path; 7 tests.

## 4. Frontend

- [x] 4.1 `api.ts`: memory types, command wrappers, `memory-state`
      listener
      — `Memory`/`JournalDay`/`DistillCandidate`/`MemoryStateEvent` mirror
      the Rust structs field-for-field (none of them carry `rename_all`
      beyond single-word fields, so no casing translation was needed);
      `memoryWrite`/`journalAppend`/`readJournal`/`distillJournal`/
      `resolveDistillCandidate` wrappers plus `onMemoryState` added in a new
      "Memory" group after `workspaceKgSearch`, mirroring the
      `onWorkspaceKgState` doc-comment convention.
- [x] 4.2 Chat: render memory/journal tool calls; distill candidate
      approval cards (folder, body, source links, approve/dismiss)
      — Tool-call rendering: BLOCKED, not implemented, per this task's own
      escape hatch and backend task 2.3's note — `ChatMessage.role` is
      `"user"|"assistant"|"activity"|"divider"` with no tool-call shape at
      all, and Ken's chat has no tool-declaration mechanism (confirmed by
      re-reading `chat.rs`'s CLI invocation). Nothing to render until a chat
      tool protocol exists. Approval cards: implemented in
      `src/screens/SettingsScreen.svelte`, a new flag-gated "Memory"
      section (`{#if memory.enabled}`, mirroring the existing
      `{#if profilerEnabled}` Project-profile card's shape) placed after
      the generic Features list. New store `src/lib/memory.svelte.ts`
      (mirrors `workspaceKg.svelte.ts`'s init/flag/event-subscription
      shape) holds `phase`/`candidates`/`resolvingSlug`, driven by
      `onMemoryState`. Judgment calls: (1) home — Settings, not the Review
      inbox (`InboxItem`/`review_inbox` has no distill-candidate kind and
      extending it is a backend change out of this task's touch-boundary)
      and not the chat sidebar (chat has no tool-call surface per above);
      Settings already hosts every other workspace-scoped flag control
      (federatedKg/kgRouting), and `KEN_MEMORY_DISABLED_MSG` itself already
      says "turn on kenMemory in Settings → Features". (2) "folder" on each
      card is a literal `.ken-workspace/memory/<slug>.md` label, not a field
      — `DistillCandidate` has no scope, and `resolve_distill_candidate`'s
      own doc comment confirms candidates are always workspace-scope (the
      journal has no per-project home). (3) `resolve()` trims the resolved
      slug from `candidates` locally rather than waiting for a server echo
      — `resolve_distill_candidate` never re-emits `memory-state` after a
      write, so there is no event to wait for.
- [x] 4.3 `ken://workspace/...` addresses resolve to open the file
      like any member document
      — Investigated whether the pseudo-member can be focused from the
      frontend today: NO. `focus_member_inner`'s only lookup path for a
      not-yet-resident id is `ws.member_root(id)` against the workspace
      manifest's `members`, which D3 deliberately never includes the
      pseudo-member in; and even though it IS resident (activated eagerly
      on workspace open per task 2.1, so `members.contains_key` would hit),
      forcing focus onto it doesn't error but desyncs state —
      `WorkspaceOverviewDto.members` is built from the same
      pseudo-member-excluding `ws.ws.members`, so
      `AppStore.loadFocusedMemberState` finds no roster match and nulls out
      `app.project`, breaking every screen that reads it. No read/open
      command takes an explicit project id either (`read_file`,
      `read_file_bytes`, `open_external` all resolve via `resolve_path`,
      hardcoded to the focused member). New `src/lib/kenAddress.ts`:
      `parseKenAddress`/`toWorkspaceAddress` (pure, tested in
      `kenAddress.test.ts`) plus `unopenableReason`/`openKenAddress`. Real
      member addresses (`ken://<project-id>/...`) resolve exactly like
      `SearchOverlay.openHit` (focus-switch if needed, then
      `app.openInFiles`). `ken://workspace/...` addresses render as an
      honest disabled state with a tooltip (same pattern as
      `EntityWikiPanel.pointerTitle`) — wired into the distill-candidate
      source links in 4.2 rather than left unused, since that's the one
      place `ken://workspace/` addresses actually surface in the UI today.
      Follow-up noted in `kenAddress.ts`'s doc comment: a `read_file`-style
      command taking an explicit project id (or a dedicated "open workspace
      memory file" command) would let this resolve for real.

## 5. Verification

- [ ] 5.1 cargo test --workspace, pnpm test, pnpm check, pnpm build
      green
- [ ] 5.2 Flag off: byte-identical behavior; no `.ken-workspace`
      subfolders created
- [ ] 5.3 Manual on the real workspace: write a ways-of-working
      memory via chat, see it injected next session; agent-desktop
      `journal_append` via MCP shows up in search; roll a
      back-dated journal file to archive and confirm it is
      searchable but mints no KG entities; run distill and approve
      one candidate
- [ ] 5.4 Golden queries: memory-recall queries (GOLDEN-QUERIES.md)
      return the expected memory/journal files

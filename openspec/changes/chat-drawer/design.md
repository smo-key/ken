# Design: chat-drawer

## Context

Changes 1–2 delivered indexing/search/editing and AI ingests. The runner
infrastructure (PTY spawning, hook listener, session ids) exists in
ken-core. This change adds user-facing Claude sessions in a drawer, per the
prototype, without an API key — everything runs through the local CLI.

## Goals / Non-Goals

**Goals:**
- Friendly chat by default; the real terminal one toggle away; one session
  id across both.
- Honest status (working / needs your input / done) driving badges and the
  title-bar dot.
- Chat list + transcripts survive restarts; sessions are resumable.
- Conversation engine testable in CI against a fake stream-json CLI.

**Non-Goals:**
- Rendering option-buttons for agent questions (conversation mode can't
  receive interactive tool prompts from `-p`; agents ask in prose and the
  user replies in prose — the Review inbox (change 4) owns structured
  approvals).
- Re-importing turns that happened in terminal mode into Ken's stored
  transcript (the terminal shows them; the session keeps them; v1 marks a
  "continued in the terminal" divider).
- Cross-project chat; a chat belongs to the project that created it.

## Decisions

1. **Conversation engine** — one child process per *active* chat:
   `claude -p --input-format stream-json --output-format stream-json
   --verbose --permission-mode acceptEdits` plus `--session-id <uuid>` on
   first spawn or `--resume <uuid>` afterwards, cwd = project root. Ken
   writes user turns as JSONL
   (`{"type":"user","message":{"role":"user","content":[{"type":"text",…}]}}`)
   and consumes stdout events: `assistant` messages (text blocks appended
   to the transcript; `tool_use` blocks summarized to one activity line
   like "Read notes/meeting.md"), and `result` (turn end → status done).
   Send while no process → lazy spawn with `--resume`. Unexpected exit →
   chat status `error` with stderr tail; next send respawns. At most 3
   conversation processes alive (LRU kill; lazy resume makes this
   invisible).
2. **Terminal mode** — reuse `portable-pty`: spawn `claude --resume
   <session-id>` (or `--session-id` for a brand-new chat), stream PTY
   output to the frontend via a per-chat Tauri event (base64 chunks),
   accept input/resize via commands, render with `@xterm/xterm` +
   fit addon. Mode switching kills the other mode's process first — one
   process per session at a time. Status in terminal mode comes from the
   existing HookListener (Stop → done, Notification → needs_input); the
   TUI's trust/permission prompts are visible and answerable there.
3. **DB schema v3** — additive:
   `chats(id TEXT PK, title, kind 'user'|'ingest', pinned INT, status,
   created_at, last_active_at, archived INT)`;
   `chat_messages(id PK, chat_id, role 'user'|'assistant'|'activity'|'divider',
   content, created_at)`. Chat id = the Claude session uuid. Migration
   2→3; `meta.schema_version = 3`.
4. **Ingest runs as system sessions** — when the app relays an
   `IngestEvent`, it upserts a chat row (`kind='ingest'`, id = the run's
   session id, title "Ingest — <name>", status mapped from the run
   status). Opening one lands in terminal mode via `--resume` (read-only
   in spirit; the user can watch or intervene). No conversation mode for
   ingest sessions (their prompt protocol is Ken's, not the user's).
5. **Status model** — `working` (between send and result / hook silence),
   `needs_input` (Notification hook, or process waiting on TUI prompt),
   `done` (result event / Stop hook), `error`. Title-bar dot shows when
   any non-archived chat is `needs_input`. Auto-title = first user message
   truncated to 40 chars; rename allowed.
6. **Drawer layout** — matches prototype: docked `flex: 0 0 372px` when
   window ≥1140px, absolute overlay (340px, shadow) below; tabs with
   overflow "+N ⌄" menu; pins order first; transcript on paper; reply box
   with "/ for terminal" hint; three suggested prompts on an empty chat.
7. **Markdown rendering** — assistant text through `marked` +
   `dompurify` sanitize; links to project-relative paths open in Files.
8. **Fake CLI for tests** — the existing fake-claude script gains a
   stream-json branch: on `--input-format stream-json` it loops over stdin
   lines and answers each with an `assistant` event (+ optional `tool_use`
   when the message contains "usetool") and a `result` event. ken-core
   tests drive ChatEngine end-to-end: send → transcript rows → status
   transitions → resume flag on respawn.

## Risks / Trade-offs

- [stream-json event shape drift across CLI versions] → all parsing in one
  `chat.rs` module; unknown event types ignored; a parse failure degrades
  to an activity line, never a crash.
- [Two processes racing one session file on fast mode toggles] → engine
  serializes: kill + wait before spawning the other mode.
- [acceptEdits lets chat agents edit files without per-edit prompts] →
  same policy as ingests; every edit lands in watched files (visible,
  reviewable, and under Git/OneDrive versioning per the collaboration
  model). Terminal mode users see the TUI's own permission UI instead.
- [Conversation process per chat costs memory] → LRU cap 3 + lazy resume.

## Migration Plan

Additive v2→v3 migration; older DBs upgrade on open. Rollback = git
revert; chat rows are app-local (loss = list/transcripts, sessions remain
in ~/.claude and resumable by id).

## Open Questions

None blocking.

# Tasks: chat-drawer

## 1. Foundations

- [x] 1.1 DB schema v3: chats + chat_messages tables, migration test, CRUD (upsert chat, set pinned/status/title/archived, append message, list chats sorted pinned-first, load transcript)
- [x] 1.2 Extend fake-claude with a stream-json branch (stdin JSONL loop → assistant/tool_use/result events; `usetool` marker; `--resume` acknowledged)

## 2. ken-core chat engine

- [x] 2.1 stream-json event parsing (assistant text, tool_use → activity summary line, result, unknown-event tolerance) with unit tests
- [x] 2.2 ChatEngine: per-chat conversation process lifecycle (spawn with --session-id/--resume, JSONL send, event pump thread, LRU cap 3, kill/respawn on death, status transitions via callback) — tests against fake CLI: send/receive, activity line, resume-after-kill, error state
- [x] 2.3 Terminal attach: PTY spawn `claude --resume` with output callback + input/resize/kill API; serialize mode switches (conversation process stopped before PTY and vice versa); hook-based status (Stop/Notification) for PTY sessions; smoke test with fake CLI

## 3. Tauri layer

- [x] 3.1 Commands: list_chats, create_chat, send_chat_message, rename_chat, set_chat_pinned, archive_chat, enter_terminal_mode, leave_terminal_mode, chat_pty_input, chat_pty_resize; events: chat-updated (row), chat-message (append), chat-pty-data:<id>
- [x] 3.2 Ingest sessions: upsert chat rows (kind=ingest) from IngestEvent relay, status mapping, resume-into-terminal for ingest session ids

## 4. Frontend

- [x] 4.1 Deps (@xterm/xterm, @xterm/addon-fit, marked, dompurify); chats store (rows, transcripts, live events, needsInput derived)
- [x] 4.2 ChatDrawer shell: dock/overlay by window width, tabs with pins + overflow menu + status dots, new-chat button, title-bar Chats toggle live with needs-input dot
- [x] 4.3 Transcript view: bubbles, sanitized markdown with project-file links opening Files, activity lines, divider rows, suggested prompts, reply box ("/ for terminal" hint, Enter to send)
- [x] 4.4 Terminal view: xterm + fit, pty data/input/resize wiring, mode toggle both ways
- [x] 4.5 Ingest system sessions in the tab list with distinct label, open-in-terminal

## 5. Verification

- [x] 5.1 cargo + vitest + svelte-check green; fake-CLI engine tests cover the spec scenarios
- [ ] 5.2 Live run-through with real claude: converse, toggle terminal, resume after restart; fix gaps
- [ ] 5.3 README status; commit

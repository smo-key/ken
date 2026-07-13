# Proposal: chat-drawer

## Why

Ken can index knowledge and run ingests, but users can't yet talk to their
knowledge. Change 3 adds the chat drawer: Claude sessions inside Ken —
friendly by default for non-technical users, a real terminal one keystroke
away — with pins and honest status badges, all running on the user's local
Claude Code CLI and auth.

## What Changes

- Right-side chat drawer per the prototype: docked 372px on windows ≥1140px
  wide, overlay below; session tabs across the top; reply box with
  suggested starting prompts; Chats toggle in the title bar goes live with
  a needs-input dot.
- Dual-mode sessions, one engine, one session id:
  - *Conversation mode (default):* Ken drives a persistent
    `claude -p --input-format stream-json --output-format stream-json`
    process per active chat — user bubbles, assistant markdown, and small
    tool-activity lines rendered natively.
  - *Terminal mode:* toggling attaches a PTY running the Claude TUI
    (`claude --resume <session-id>`) rendered in xterm.js; switching modes
    swaps the process, the session (history, context) carries over.
- Chat management: create, rename (auto-titled from the first message),
  pin/unpin (pins float to top), archive; status badges **working / needs
  your input / done** from turn lifecycle (conversation mode) and hooks
  (terminal mode).
- Persistence: DB schema v3 adds `chats` and `chat_messages` so the list
  and transcripts survive restarts; the underlying Claude session files
  make every chat resumable.
- Ingest runs surface in the drawer as system sessions: watch a running
  ingest live (terminal mode) or answer the question blocking it.
- CI-testable: the conversation engine is exercised against a fake
  `claude` that speaks stream-json.

## Capabilities

### New Capabilities
- `chat-sessions`: the session engine — conversation-mode stream-json
  driving, terminal-mode PTY attach, mode switching, status derivation,
  persistence.
- `chat-drawer-ui`: the drawer itself — layout, tabs, pins, badges,
  transcript rendering, reply box, suggested prompts, ingest-run sessions.

### Modified Capabilities

_None — existing specs unchanged. (The ingest runner already records
session ids; the drawer reads them.)_

## Impact

- `ken-core`: new `chat` module (conversation process management,
  stream-json parsing, transcript model), DB schema v3, PTY reuse from the
  runner.
- `src-tauri`: chat commands (create/list/send/pin/archive/switch-mode,
  PTY attach with data events), terminal resize plumbing.
- Frontend: drawer components (tabs, transcript, terminal view via
  `@xterm/xterm`), title-bar wiring; new deps `@xterm/xterm` +
  `@xterm/addon-fit`, a markdown renderer for assistant messages
  (`marked` + sanitizer).
- DB migration v2→v3 (additive tables).

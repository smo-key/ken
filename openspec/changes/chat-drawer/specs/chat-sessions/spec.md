# chat-sessions

## ADDED Requirements

### Requirement: Sessions run on the local Claude Code CLI
Every chat SHALL be a Claude Code session (user's local CLI and auth) with
a Ken-generated session id, working directory = the project root. Ken
SHALL NOT require an API key.

#### Scenario: New chat creates a resumable session
- **WHEN** the user starts a chat and sends a message
- **THEN** a CLI process is spawned with Ken's session id, and after Ken
  restarts the same chat can continue via session resume

### Requirement: Conversation mode over stream-json
The default mode SHALL drive the CLI in print mode with stream-json input
and output: user turns written as JSON lines, assistant text and tool
activity parsed from events into a stored transcript, turn completion
detected from the result event. Unknown or malformed events SHALL degrade
gracefully (ignored or shown as a plain activity line), never crash the
session.

#### Scenario: Send and receive a turn
- **WHEN** the user sends "Who owns billing cutover?"
- **THEN** the transcript stores the user message, then assistant
  markdown, and the chat status goes working → done when the result event
  arrives

#### Scenario: Tool use becomes an activity line
- **WHEN** the assistant uses a tool mid-turn (e.g. reading a file)
- **THEN** the transcript shows a one-line activity entry (not raw JSON)

#### Scenario: Process death recovers on next send
- **WHEN** the CLI process exits unexpectedly
- **THEN** the chat shows an error state, and the next send respawns the
  process with session resume so context is kept

### Requirement: Terminal mode on the same session
The user SHALL be able to switch any of their chats to a real terminal
(the Claude TUI resumed on the same session id) and back. Mode switches
SHALL never run two processes on one session simultaneously. Turns taken
in the terminal SHALL remain in the session; Ken's stored transcript marks
a "continued in the terminal" divider rather than losing the thread.

#### Scenario: Toggle to terminal and back
- **WHEN** the user toggles terminal mode, types a message in the TUI,
  then toggles back and sends from the reply box
- **THEN** both messages belong to the same session and the conversation
  continues with full context

### Requirement: Status reflects reality
Chat status SHALL be `working` while a turn is in flight, `needs your
input` when the agent is waiting on the user (Notification hook in
terminal mode), `done` when a turn completes (result event or Stop hook),
and an error state on failures — never a stale or invented state.

#### Scenario: Needs-input from the terminal
- **WHEN** a terminal-mode session hits a question or permission prompt
- **THEN** the chat's badge shows needs-your-input until the user answers

### Requirement: Persistence and testability
Chat list (title, pinned, status, timestamps) and transcripts SHALL be
stored in the project DB (schema v3) and survive restarts. The
conversation engine SHALL be covered in CI by a fake CLI speaking
stream-json — no real Claude needed.

#### Scenario: Restart keeps chats
- **WHEN** Ken restarts
- **THEN** the chat list and transcripts render as before, and sending in
  an old chat resumes its session

#### Scenario: CI coverage
- **WHEN** the test suite runs
- **THEN** send/receive, tool-activity, status transitions, and resume
  behavior are verified against the fake CLI

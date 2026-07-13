# chat-drawer-ui

## ADDED Requirements

### Requirement: Drawer layout per prototype
The chat drawer SHALL dock at 372px on windows at least 1140px wide and
overlay (340px with shadow) below that, toggled from the title-bar Chats
button. Session tabs render across the top (overflowing into a "+N" menu);
the transcript sits on paper tones; the reply box shows a "/ for terminal"
hint. The title-bar Chats button SHALL show a dot when any chat needs the
user's input.

#### Scenario: Toggle and dock behavior
- **WHEN** the user clicks Chats on a wide window
- **THEN** the drawer docks beside the content (content reflows), and on a
  narrow window it overlays instead

#### Scenario: Needs-input dot
- **WHEN** any open chat is waiting on the user
- **THEN** the Chats button shows an attention dot even while the drawer
  is closed

### Requirement: Chat management
Users SHALL be able to create chats, rename them (default title from the
first message), pin/unpin (pinned float to the top of the tab order), and
archive. Status badges (working / needs your input / done) SHALL show on
tabs and in the overflow list.

#### Scenario: Pin floats to top
- **WHEN** the user pins a chat
- **THEN** it moves ahead of unpinned chats in the tab order and stays
  there across restarts

### Requirement: Friendly transcript
Conversation mode SHALL render user messages as bubbles, assistant
messages as sanitized markdown, and tool usage as small activity lines.
Links to files inside the project SHALL open them in the Files screen. An
empty chat SHALL offer three suggested starting prompts relevant to a
knowledge project.

#### Scenario: Assistant markdown renders safely
- **WHEN** the assistant replies with headings, lists, and a link to
  `notes/meeting.md`
- **THEN** formatted text renders (no raw markdown, no script execution)
  and clicking the link opens the file in Files

#### Scenario: Suggested prompts
- **WHEN** the user opens a brand-new chat
- **THEN** three starter prompts are offered and clicking one sends it

### Requirement: Terminal view
Terminal mode SHALL render the live TUI in an embedded terminal
(xterm.js) with keyboard input and resize handled, typing `/` in an empty
reply box (or the toggle) entering it, and an obvious way back to
conversation mode.

#### Scenario: Terminal round-trip
- **WHEN** the user enters terminal mode
- **THEN** the real Claude TUI renders and accepts typing, and the toggle
  returns to the friendly transcript

### Requirement: Ingest runs appear as system sessions
Running or recent ingest runs SHALL appear in the drawer as system
sessions (distinct label), openable in terminal mode to watch or step in —
including answering a question that has an ingest blocked.

#### Scenario: Watch a running ingest
- **WHEN** an ingest run is in progress and the user opens its session
  from the drawer
- **THEN** the live TUI of that run is visible, and if it was blocked on a
  question the user can answer it there

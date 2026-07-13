# quick-answer

## ADDED Requirements

### Requirement: Quick answer over the index
The app SHALL treat a ⌘K query as a question when it has at least 3
words or ends with "?", and for those queries it SHALL wait an 800ms
debounce, then gather the top 8 FTS hits (paths and snippets with `<mark>` markup stripped) and run a
one-shot session (60-second timeout) instructing the model to answer in
one or two sentences using ONLY the provided material, name the source
paths it used, say it doesn't know when the material doesn't answer, and
end with a `SOURCES:` line. The answer SHALL arrive as a `quick-answer`
event `{query, body, sources}`. When Claude Code is missing there SHALL
be no card and no error — search behaves exactly as without the feature.

#### Scenario: Question gets a grounded answer
- **WHEN** the user types a ≥3-word query and pauses, and the model
  answers
- **THEN** a `quick-answer` event carries the query, a 1–2 sentence
  body, and source paths drawn from the supplied hits

#### Scenario: Short non-question stays AI-free
- **WHEN** the query is one or two words without a "?"
- **THEN** no one-shot session is started

#### Scenario: Claude missing degrades silently
- **WHEN** Claude Code is not installed and a question-shaped query is
  typed
- **THEN** the match list works as before, with no answer card and no
  error

### Requirement: Quick answer card never blocks matches
The overlay SHALL render the clay-tinted Quick answer card above the
Matches section only when an answer's query matches the current input.
Instant FTS matches SHALL never be blocked, reordered, or delayed by the
answer; new keystrokes SHALL hide a now-stale card; answers arriving for
a query the user has since changed SHALL be dropped; and answers SHALL
be cached per query string for the overlay's lifetime so retyping a
query shows its card instantly. The card's source chips SHALL open the
file in Files.

#### Scenario: Answer appears above intact matches
- **WHEN** an answer arrives for the query still in the input
- **THEN** the card appears above Matches and the match list is
  unchanged in content and order

#### Scenario: Stale answer dropped
- **WHEN** an answer arrives for a query the user has edited away from
- **THEN** no card is shown for it

#### Scenario: Cached answer returns instantly
- **WHEN** the user retypes a query already answered in this overlay
  session
- **THEN** its card shows immediately without a new session

### Requirement: Continue in chat
The overlay footer SHALL show `⌘↵ continue in chat`. Pressing ⌘+Enter
SHALL close the overlay, open the chat drawer, create a new chat, and
send the current query as its first message.

#### Scenario: Handing a query to chat
- **WHEN** the user presses ⌘+Enter with a query in the search input
- **THEN** the overlay closes, the drawer opens on a new chat, and the
  query is sent as the first message

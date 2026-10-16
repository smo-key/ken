---
title: "Ingestion"
aliases: ["Ingestion", "the inbox", "where do I drop a meeting recording", "where do transcripts go"]
status: current
updated: {{date}}
---

# Ingestion

The inbox for what is not yet in the library: meeting transcripts, recordings, design sessions, documents, exports and pasted notes.

| folder | holds |
|---|---|
| `Raw/` | what was dropped · it empties |
| `Ingested/` | one dated note per source, with the source filed beside it · evidence at its date, cited, never read as current rules |

A source dropped in `Raw/` is read and written up from `Templates/Ingested-note.md`. The pages it changes are updated, citing the note; what it contradicts is named; a ruling waits for one confirm into the decisions log, and an action becomes a drafted ticket. Each ingest is one commit. A source is never edited.

---
title: "Templates"
aliases: ["Templates", "page templates", "which blank do I copy"]
status: current
updated: {{date}}
---

# Templates

The blanks a new page is copied from, and where each copy goes. A copy keeps the frontmatter keys, fills or deletes every `{{placeholder}}`, and is added to its section's index.

| blank | copy to |
|---|---|
| `Rule.md` | `Rules/{{The Rule as a Sentence}}.md` |
| `Design-Note.md` | `Design/{{Question}}.md` |
| `Roadmap.md` | `Current/Roadmap.md` |
| `Feature-Status.md` | `Current/Feature-Status.md` |
| `How-to.md` | `Reference/{{How to Do the Thing}}.md` |
| `Platform.md` | `Reference/Platform/{{Dependency}}.md` |
| `Finding.md` | `Research/{{date}}-{{question}}.md` |
| `Ingested-note.md` | `Research/Ingestion/Ingested/…` (Ken writes these) |
| `Ticket.md` | `tickets/{{KEY}}-nnn.md` |

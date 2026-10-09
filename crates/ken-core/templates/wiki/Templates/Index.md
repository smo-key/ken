---
title: "Templates"
aliases: ["Templates", "page templates", "how do I start a new page", "which blank do I copy"]
status: current
audience: dev
updated: {{date}}
---

# Templates

The blanks a new page is copied from, and where each copy goes. Not here: the pages that exist once per library, which are already in their sections → [[Start Here]].

[[#Blanks]]

## Blanks

| blank | copy to | one per |
|---|---|---|
| `Repo-Architecture.md` | `Conventions/Architecture-{{repo}}.md` | code repo |
| `Convention.md` | `Conventions/{{Area}}.md` | area a reviewer checks |
| `Design-Note.md` | `Design/{{Question}}.md` | design question |
| `Finding.md` | `Research/Findings/{{date}}-{{question}}.md` | question a session answered |
| `How-to.md` | `Reference/How-tos/{{How to Do the Thing}}.md` | task or runbook |
| `Incident.md` | `Work/Incidents/{{date}}-{{name}}.md` | incident |
| `Ingested-note.md` | `Research/Ingestion/Ingested/{{date}}-{{name}}.md` | source dropped in `Raw/` |
| `Dependency.md` | `Platform/{{Dependency}}.md` | upstream or reference dependency |
| `Rule.md` | `Ways-of-Working/Rules/{{the-rule-as-a-sentence}}.md` | rule |
| `System.md` | `Reference/Systems/{{System}}.md` | system with more to say than its row on [[Systems]] |

A copy keeps every frontmatter key, fills every `{{placeholder}}` or deletes its row, and is linked from its section's index, or from the list page for its kind (How-tos, Incidents, Rules, Systems), before it is done. `verified:` stays empty until a person has read the page against its sources.

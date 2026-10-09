---
title: "Team"
aliases: ["Team", "how is the team structured", "who decides", "who reviews", "who releases"]
status: current
audience: business
updated: {{date}}
verified:               # the date a person read this page against its sources · empty until then · never stamped in bulk
pin: "{{repo@sha for each repo the sources are in}}"   # the commit each repo's sources were read at
changed_by: "[[{{the Ingested note that last changed it}}]]"
sources:
  - "{{team_repo}}:people/"
  - "{{repo:path of any other doc that names a role}}"
---

# Team

The roles on the team, who decides what, and who takes work at each stage. Not here: who owns which area or repo → [[Who Does What]]; each person's own preferences → their file in the team repo's `people/`; the stages themselves → [[Lifecycle]].

[[#Roles]] · [[#Handoffs]]

## Roles

| role | who | decides |
|---|---|---|
| {{a role a people/ file or a repo doc names}} | {{name, as in people/}} | {{what they rule on}} |

## Handoffs

| stage | who | hands to |
|---|---|---|
| Plan | {{who takes requests in and writes the story}} | {{who sizes}} |
| Size | {{who confirms the size}} | {{who builds}} |
| Build | {{who builds}} | {{who reviews}} |
| Release | {{who sees it working and ships it}} | {{who writes it down}} |
| Learn | {{who writes the closing notes and the rulings}} | — |

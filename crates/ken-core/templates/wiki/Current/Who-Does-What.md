---
title: "Who Does What"
aliases: ["Who Does What", "who owns this", "who do I ask", "who maintains this repo"]
status: current
audience: business
updated: {{date}}
verified:               # the date a person read this page against its sources · empty until then · never stamped in bulk
pin: "{{repo@sha for each repo the sources are in}}"   # the commit each repo's sources were read at
changed_by: "[[{{the Ingested note that last changed it}}]]"
sources:
  - "{{team_repo}}:people/"
  - "{{repo:path of the CODEOWNERS file}}"
  - "{{repo@sha the commit counts were read at}}"
---

# Who Does What

Who owns each area and repo, and who to ask about it. Not here: roles and who decides → [[Team]]; what each person wants from a ticket and from progress updates → their file in `people/`.

[[#Areas]]

## Areas

| area · repo | owner | ask | recent committers |
|---|---|---|---|
| {{an area or a top-level folder, with its repo}} | {{the owner a people/ file names, else a CODEOWNERS file or a repo doc}} | {{who answers questions about it, from the same sources}} | {{who has committed there recently, named as in people/ · bots and agents left out}} |

The owner and who to ask come from the roster in `people/` first, then a CODEOWNERS file or a repo doc. Recent committers only fill in who has worked on an area lately, and never make anyone its owner. Counted {{date}}, over {{the period counted}}.

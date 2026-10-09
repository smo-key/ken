---
title: "Feature Status"
aliases: ["Feature Status", "is it implemented", "is it built", "does the product do this", "can a user reach it"]
status: current
audience: business
updated: {{date}}
verified:               # the date a person read this page against its sources · empty until then · never stamped in bulk
pin: "{{repo@sha for each repo the sources are in}}"   # the commit each repo's sources were read at
changed_by: "[[{{the Ingested note or ticket that last changed it}}]]"
sources:
  - "{{repo:path of the changelog, the registration code or the test folder the rows were read from}}"
---

# Feature Status

Each feature with whether it is built, whether a user can reach it, what tests it and when someone last checked, so "is X built?" is answered here rather than by a code search. Not here: what changed in each release → [[Releases]]; what is planned → [[Roadmap]]; how a system works → [[Systems]].

[[#{{Area}}]]

## {{Area}}

| feature | built | reachable | tested by | checked | evidence |
|---|---|---|---|---|---|
| {{the feature, in the user's words}} | {{yes · partly · no · retired}} | {{how a user or admin reaches it in the running product, or no}} | {{the test names, or none}} | {{the commit and date someone last read the code or used it}} | {{repo:path, D-nnn or ticket id}} |

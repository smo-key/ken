---
title: "RAID"
aliases: ["RAID", "risks and issues", "risk register", "issue log", "what might go wrong", "what is wrong now"]
status: current
updated: {{date}}
---

# RAID

The project's risks and issues, one entry each. A risk is something that might go wrong; an issue is a problem now. The actions are the open tickets and the decisions are the decisions log; the Log screen shows all four, with the milestones.

An entry is added with the next number and never renumbered, because commits, tickets and notes cite the ids. Its status and owner change in place as it moves on; a closed entry stays. A risk or issue found in an ingested note waits on its card, and one click adds it here.

Entry format (`R-nnn` a risk, `I-nnn` an issue):

```
## R-001 · 2026-01-01 · the risk in a few words

status: open
owner: who watches it, if anyone
sources: [[Note Name]]

What might go wrong and what would show it, in a line or two.
```

A risk is `open`, `mitigated` or `closed`; an issue is `open`, `resolved` or `closed`. A closed entry folds away on the Log.

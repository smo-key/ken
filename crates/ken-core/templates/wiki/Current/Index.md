---
title: "Current"
aliases: ["Current", "what is true now", "what is the state of the project"]
status: current
audience: business
updated: {{date}}
---

# Current

What is true now about the project and the team, rewritten in place when it changes and never appended to. Not here: how the product works → [[Reference]] and [[Platform]]; what we chose to build and why → [[Design]]; how we work → [[Ways-of-Working]]; a ruling → the decisions log; what was said in a meeting → [[Ingestion]].

[[#Pages]] · [[#Belongs Here]]

## Pages

| page | holds |
|---|---|
| [[Project]] | the product · who it is for · the goal · what is out of scope |
| [[Roadmap]] | the milestones, next first, each with its target date and state |
| [[Feature Status]] | each feature: built · reachable · tested · when someone last checked |
| [[Team]] | roles · who decides what · who hands work to whom |
| [[Who Does What]] | who owns each area and repo, and who to ask |

## Belongs Here

A fact a reader needs as it stands today, which goes stale unless someone rewrites it. Each page carries `verified:`, the date a person last read it against its sources, and `changed_by:`, the note or ticket that last changed it. When an Ingested note changes what is true here, the page is rewritten and cites the note. An owner or a decider is named only from the roster in `people/`, a CODEOWNERS file or a repo doc; git gives only the recent committers, bots left out. A change that lands, retires or verifies a feature updates its row on [[Feature Status]] in the same branch. The standing sweep raises a page unverified for thirty days, because a stale overview still reads as current to every session that loads it.

---
title: "{{Design Question}}"
aliases: ["{{Design Question}}", "why does {{thing}} work this way", "why not {{alternative}}"]
status: current
audience: business
updated: {{date}}
verified:               # the date a person read this page against its sources · empty until then · never stamped in bulk
pin: "{{repo@sha for each repo the sources are in}}"   # the commit each repo's sources were read at
sources:
  - "{{team_repo}}:decisions/DECISIONS.md"
  - "[[{{the Ingested note it came from}}]]"
  - "{{repo:path of the white box (the plain HTML screens) it came from, or delete this line}}"
---

# {{Design Question}}

What we chose for {{the question}}, and why not the alternatives. Not here: whether it is built → [[Feature Status]]; the rulings in the decider's words → the decisions log.

[[#Problem]] · [[#Choice]] · [[#Alternatives]] · [[#Rulings]] · [[#Open Escalations]]

## Problem

{{One paragraph: what the user or the team needs, and what goes wrong today.}}

## Choice

{{One paragraph: what we build, in words a person who never reads code can check against the product.}}

## Alternatives

| alternative | why not |
|---|---|
| {{an option considered}} | {{the reason it lost, or the ruling}} |

## Rulings

| ruling | says |
|---|---|
| {{D-nnn}} | {{the ruling in one line; the log holds the decider's words}} |

## Open Escalations

| escalation | question | to |
|---|---|---|
| {{E-nnn}} | {{the decision still open}} | {{the person who owns it}} |

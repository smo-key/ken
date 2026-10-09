---
title: "{{System Name}}"
aliases: ["{{System Name}}", "{{the name in the code}}", "how does {{system}} work", "where is {{system}} configured"]
status: current
audience: dev
updated: {{date}}
verified:               # the date a person read this page against its sources · empty until then · never stamped in bulk
pin: {{commit}}         # the commit the sources were read at
sources:
  - "{{repo:path of its entry point}}"
  - "{{repo:path of its data}}"
---

# {{System Name}}

How {{system}} works: where it starts, its code, its data, the rulings it rests on and what is still wrong with it. Not here: its row among the others → [[Systems]]; whether each part is built → [[Feature Status]]; the layer rules → [[{{Repo}} Architecture]].

[[#Purpose]] · [[#Entry Points]] · [[#Code]] · [[#Data]] · [[#Rulings]] · [[#Known Gaps]]

## Purpose

{{One paragraph: what the system does for the user, and which other systems it feeds or reads.}}

## Entry Points

| entry point | triggered by | code |
|---|---|---|
| {{a command, route, event, schedule or screen}} | {{who or what starts it}} | {{repo:path:line}} |

## Code

| part | holds | lives in |
|---|---|---|
| {{class, module or folder}} | {{what it decides or stores}} | {{repo:path}} |

## Data

| file or table | holds | keys |
|---|---|---|
| {{repo:path, or the table}} | {{what it holds}} | {{→ [[Config Map]] rows, or the main fields}} |

## Rulings

| ruling | says |
|---|---|
| {{D-nnn}} | {{the ruling in one line}} |

## Known Gaps

| gap | state |
|---|---|
| {{what is missing or wrong}} | {{ticket id, or the person whose ruling it waits on}} |

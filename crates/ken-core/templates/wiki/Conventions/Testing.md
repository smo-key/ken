---
title: "Testing"
aliases: ["Testing", "testing standard", "which tests run when", "what does the fast suite cover", "where do fixtures live"]
status: current
audience: dev
lens: test              # the reviewer that reads this page (presets.json lenses)
updated: {{date}}
verified:               # the date a person read this page against its sources · empty until then · never stamped in bulk
pin: {{commit}}         # the commit the sources were read at
sources:
  - "{{repo:.ken/gates.json}}"
  - "{{repo:path of the test config}}"
---

# Testing

What each suite proves, when it runs, what a test may assume and where its fixtures come from. Not here: the commands to run each suite → [[Build and Run]]; which gate runs which suite → each code repo's `.ken/gates.json`.

[[#Suites]] · [[#Rules]] · [[#Fixtures]]

## Suites

| suite | proves | runs when | time |
|---|---|---|---|
| fast | {{the cases a ticket's acceptance calls for, plus the smoke set}} | {{after every build and fix lane · on demand}} | {{the budget, measured in this repo, and the date}} |
| full | {{every case, including those moved out of the fast suite}} | {{once, on the version that merges}} | {{measured time, and the date}} |
| {{other suite}} | {{what it proves}} | {{when}} | {{time, and the date}} |

## Rules

| rule | why | checked by |
|---|---|---|
| {{what a test may assume, or must never do}} | {{the incident or the cost}} | {{the reviewer, or the check}} |

## Fixtures

| fixture | lives in | real or made up |
|---|---|---|
| {{the files or data a test reads}} | {{repo:path}} | {{real: a copy of shipped files · made up: written for the test}} |

A case whose result depends on real files, such as a loader, a parser or a catalogue, runs on the real files, because a case fed made-up data passes whatever the code does with the real ones.

---
title: "Code Conventions"
aliases: ["Code Conventions", "code standards", "coding style", "how should I name this", "how do we handle errors"]
status: current
audience: dev
lens: conventions       # the reviewer that reads this page (presets.json lenses)
updated: {{date}}
verified:               # the date a person read this page against its sources · empty until then · never stamped in bulk
pin: {{commit}}         # the commit the sources were read at
sources:
  - "{{repo:path of the lint config, the brief's code section or the code that shows the rule}}"
---

# Code Conventions

The rules for writing code in every repo, each with its reason, and the helpers that already exist. Not here: where code goes → [[Architecture]]; data and config files → [[Data and Config]]; tests → [[Testing]]; one area's own rules → its page on [[Conventions]].

[[#Covers]] · [[#Rules]] · [[#Already Exists]]

## Covers

{{The repos and paths these rules apply to, and any repo they do not.}}

## Rules

| topic | rule | why | checked by |
|---|---|---|---|
| naming | {{the rule}} | {{the incident or the cost}} | {{the reviewer, or the lint rule}} |
| errors | {{how an error is raised, returned and reported}} | {{why}} | {{checked by}} |
| logging | {{the logger, the levels, what is never logged}} | {{why}} | {{checked by}} |
| comments | {{what a comment holds, and what goes to a ticket or the decisions log instead}} | {{why}} | {{checked by}} |
| dependencies | {{who may add a package, and how}} | {{why}} | {{checked by}} |
| dead code | {{what happens to code nothing calls}} | {{why}} | {{checked by}} |

## Already Exists

| thing | lives in | use it instead of |
|---|---|---|
| {{the helper, client or pattern}} | {{repo:path}} | {{the second version people tend to write}} |

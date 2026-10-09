---
title: "Decisions Cited in Code"
aliases: ["Decisions Cited in Code", "Cited in Code", "which rulings does the code cite", "is this ruling in the log"]
generated: true
upstream: "{{code repos, each with the path searched}}"
upstream_commit: "{{repo@sha for each code repo, from one run}}"
updated: {{date}}
regenerate: "{{the command that rebuilds this page}}"
---

# Decisions Cited in Code

Generated from {{repos}} at {{repo@sha}} on {{date}} by `{{command}}`: every ruling id a code comment cites, and whether the decisions log has it. Not here: the rulings themselves → `DECISIONS.md`.

[[#Not in the Log]] · [[#In the Log]]

## Not in the Log

Ids the code cites that `DECISIONS.md` does not hold. Each is unverified until the person who ruled it accepts it into the log, or the comment is corrected.

| id | cited at | the comment |
|---|---|---|
| {{D-nnn}} | {{repo:path:line}} | {{the comment line, verbatim}} |

## In the Log

| id | cited at | log entry |
|---|---|---|
| {{D-nnn}} | {{repo:path:line}} | {{the entry's one-line ruling}} |

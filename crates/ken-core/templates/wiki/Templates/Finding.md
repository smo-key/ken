---
title: "{{Question Answered}} {{date}}"
aliases: ["{{Question Answered}} {{date}}", "{{the question, as someone would ask it}}"]
status: evidence
audience: dev
updated: {{date}}
pin: "{{repo@sha for each repo the sources are in}}"   # the commit each repo's sources were read at
sources:
  - "{{repo:path:line of each file the answer rests on}}"
---

# {{Question Answered}} {{date}}

What a session found on {{date}} about {{the question}}, as evidence at that date. Not here: what is true now → {{the page that owns the answer}}.

[[#Question]] · [[#Answer]] · [[#Evidence]] · [[#Pages Read]] · [[#Pages Missing]]

## Question

{{The question, as it was asked, and the ticket it came from.}}

## Answer

{{One paragraph. Say what was not checked.}}

## Evidence

| source | shows |
|---|---|
| {{repo:path:line, a command and its output, or repo@sha}} | {{what it shows}} |

## Pages Read

| page | state |
|---|---|
| {{[[page]]}} | {{accurate · stale and corrected · stale and not corrected}} |

## Pages Missing

| page | would hold |
|---|---|
| {{the page a search expected and did not find}} | {{the answer it would hold}} |

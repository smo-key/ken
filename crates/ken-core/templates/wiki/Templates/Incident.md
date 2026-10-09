---
title: "{{Incident Name}} {{date}}"
aliases: ["{{Incident Name}} {{date}}", "{{the symptom people reported}}", "why did {{thing}} fail on {{date}}"]
status: current
audience: business
updated: {{date}}
verified:               # the date a person read this page against its sources · empty until then · never stamped in bulk
pin: "{{repo@sha for each repo the sources are in}}"   # the commit each repo's sources were read at
sources:
  - "{{repo:path of the fix, or repo@sha}}"
  - "{{the log, alert or ticket that first showed it}}"
---

# {{Incident Name}} {{date}}

What happened on {{date}}, what it cost, why, and what changed because of it. Not here: the rule it produced → [[{{the-rule-as-a-sentence}}]]; the fix's code → {{ticket id}}.

[[#Summary]] · [[#Impact]] · [[#Timeline]] · [[#Cause]] · [[#Fix]] · [[#Changes]]

## Summary

{{One paragraph: what broke, for whom, and how it was found.}}

## Impact

| measure | value |
|---|---|
| {{users affected · time down · data lost · work redone}} | {{the number, measured, and how}} |

## Timeline

| time | event |
|---|---|
| {{date and time}} | {{what happened or was done, and by whom}} |

## Cause

{{One paragraph: the cause, with the file, commit or config that shows it.}}

## Fix

{{What stopped it, with repo@sha or the ticket.}}

## Changes

| change | where |
|---|---|
| {{a new rule, convention, check or test}} | {{[[the rule]], the page, or the ticket id}} |

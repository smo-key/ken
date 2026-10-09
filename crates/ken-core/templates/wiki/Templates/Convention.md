---
title: "{{Area}} Conventions"
aliases: ["{{Area}} Conventions", "how do we do {{area}} here", "is there already a {{thing}}"]
status: current
audience: dev
lens: conventions       # the reviewer that reads this page (presets.json lenses)
updated: {{date}}
verified:               # the date a person read this page against its sources · empty until then · never stamped in bulk
pin: {{commit}}         # the commit the sources were read at
sources:
  - "{{repo:path of the code this page describes}}"
---

# {{Area}} Conventions

How {{area}} is done here, and what already exists so nobody builds a second one. Not here: the layers → [[{{Repo}} Architecture]]; a person's own preferences → their file in `people/`.

[[#Covers]] · [[#Rules]] · [[#Already Exists]]

## Covers

{{The paths this page describes, as repo:path.}}

## Rules

| rule | why | checked by |
|---|---|---|
| {{instead of the thing people reach for, use the thing that exists}} | {{the incident or the cost}} | {{the reviewer, or the check and its command}} |

## Already Exists

| thing | lives in | use it instead of |
|---|---|---|
| {{the helper, client, component or pattern}} | {{repo:path}} | {{the second version people tend to write}} |

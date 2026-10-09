---
title: "{{Repo}} Architecture"
aliases: ["{{Repo}} Architecture", "{{repo}} layers", "where does code go in {{repo}}", "what may call what in {{repo}}"]
status: current
audience: dev
lens: arch              # the reviewer that reads this page (presets.json lenses)
updated: {{date}}
verified:               # the date a person read this page against its sources · empty until then · never stamped in bulk
pin: {{commit}}         # the commit the sources were read at
sources:
  - "{{repo:path of the entry point}}"
  - "{{repo:path of the brief section or the check that states the layers}}"
---

# {{Repo}} Architecture

The layers of {{repo}}: where each lives, what it holds, what it may call, what it may not, and where the code breaks those rules today. Not here: how {{repo}} fits with the other repos → [[Architecture]]; its systems → [[Systems]]; how to build it → [[Build and Run]].

[[#Layers]] · [[#May Not]] · [[#Known Gaps]] · [[#Checks]] · [[#Diagram]]

## Layers

| layer | lives in | holds | may call |
|---|---|---|---|
| {{layer name}} | {{repo:path of the folder}} | {{what belongs in it}} | {{the layers it may call, named}} |

## May Not

| rule | why |
|---|---|
| {{a call or a placement that must never happen}} | {{the incident, or the ruling D-nnn}} |

## Known Gaps

| where | rule broken | state |
|---|---|---|
| {{repo:path, or the folder pair that imports each other}} | {{the layer rule above it breaks}} | {{ticket id, or the person whose ruling it waits on}} |

Gaps counted {{date}}.

## Checks

| check | command | catches |
|---|---|---|
| {{the test, lint or script that enforces a layer rule}} | `{{command}}` | {{which rule it enforces}} |

## Diagram

```mermaid
flowchart TD
  A[{{layer}}] -->|may call| B[{{layer}}]
  A -.->|may not| C[{{layer}}]
```

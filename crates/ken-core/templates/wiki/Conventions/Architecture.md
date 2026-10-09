---
title: "Architecture"
aliases: ["Architecture", "how do the repos fit together", "which repo calls which", "system diagram"]
status: current
audience: dev
lens: arch              # the reviewer that reads this page (presets.json lenses)
updated: {{date}}
verified:               # the date a person read this page against its sources · empty until then · never stamped in bulk
pin: "{{repo@sha for each repo the sources are in}}"   # the commit each repo's sources were read at
sources:
  - "{{repo:path of each repo's entry point or build file}}"
---

# Architecture

The code repos, what each holds and which may call which, with one page per repo for its layers. Not here: a repo's own layers → its [[{{Repo}} Architecture]] page; each system and where its code lives → [[Systems]]; how to build each repo → [[Build and Run]].

[[#Repos]] · [[#May Not]] · [[#Diagram]]

## Repos

| repo | holds | may call | page |
|---|---|---|---|
| {{code repo}} | {{what it builds, in one line}} | {{the other repos or services it may call, and how: a package · an API · a file it reads}} | [[{{Repo}} Architecture]] |

## May Not

| call | why |
|---|---|
| {{a call between repos that must never happen}} | {{the incident, or the ruling D-nnn}} |

## Diagram

```mermaid
flowchart LR
  A[{{repo}}] -->|{{how it calls}}| B[{{repo}}]
```

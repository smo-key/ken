---
title: "Build and Run"
aliases: ["Build and Run", "how do I build it", "how do I run the tests", "how do I run it locally", "setup", "getting started"]
status: current
audience: dev
updated: {{date}}
verified:               # the date a person read this page against its sources · empty until then · never stamped in bulk
pin: "{{repo@sha for each repo the sources are in}}"   # the commit each repo's sources were read at
sources:
  - "{{repo:path of each build file}}"
  - "{{repo:.ken/gates.json}}"
---

# Build and Run

For each repo, what to install, the commands to build, test and run it, the environment it needs and the failures people hit. Not here: what each suite proves → [[Testing]]; how the repo is layered → [[{{Repo}} Architecture]]; how a dependency is updated → its page on [[Platform]].

[[#{{repo}}]]

## {{repo}}

### Prerequisites

| tool | version | needed for |
|---|---|---|
| {{tool}} | {{version the build files or CI pin}} | {{build · test · run}} |

### Commands

| task | command | notes |
|---|---|---|
| install | `{{command}}` | {{notes}} |
| build | `{{command}}` | {{notes}} |
| fast suite | `{{command, as .ken/gates.json names it}}` | {{notes}} |
| full suite | `{{command}}` | {{notes}} |
| run | `{{command, as .ken/gates.json names it for G4l, the live-verify gate}}` | {{where it opens · the port · the account to use}} |
| lint | `{{command}}` | {{notes}} |
| package | `{{command}}` | {{where the artifact lands}} |

### Environment

| variable | value | needed by |
|---|---|---|
| {{NAME}} | {{value, or where to get it}} | {{the command that fails without it}} |

### Failures

| symptom | cause | fix |
|---|---|---|
| {{the message or behaviour}} | {{why}} | {{what to do}} |

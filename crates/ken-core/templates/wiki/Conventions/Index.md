---
title: "Conventions"
aliases: ["Conventions", "how do we do it here", "what does review check", "house style"]
status: current
audience: dev
updated: {{date}}
---

# Conventions

How the team does things here, one page per area, which a review reads a change against. Not here: how we work as a team → [[Ways-of-Working]]; how a dependency behaves → [[Platform]]; where a system's code and data are → [[Systems]]; a person's own preferences → their file in `people/`.

[[#Pages]] · [[#Reviewers]] · [[#Belongs Here]]

## Pages

| page | covers | reviewer |
|---|---|---|
| [[Architecture]] | the repos · what each holds · which may call which | architecture |
| [[{{Repo}} Architecture]] | one code repo's layers: lives in · holds · may call · what may not · known gaps | architecture |
| [[Code Conventions]] | naming · errors · logging · comments · dependencies · dead code | conventions |
| [[Data and Config]] | where data and config files live · their shape · how a new one is added | conventions |
| [[Testing]] | each suite: what it proves · when it runs · what a test may assume | test |
| [[Registries]] | what has to be listed when something is added, and where | integrity |
| {{[[Area Conventions]]}} | {{the paths it covers}} | {{reviewer}} |

## Reviewers

Each reviewer checks a change from one angle, against the pages named for it under Pages. A review with no page behind it is one reviewer's opinion. The pages covering the files a lane may change go into its prompt in full, because a lane, one agent working one step of a ticket, breaks a convention it was never given. The design-system reviewer reads a Design System page, which a team with a user interface writes from `Templates/Convention.md`.

## Belongs Here

A practice a reviewer can check by reading the page against a diff, with the incident or cost that made it the team's. A new area gets its page from `Templates/Convention.md`, a new code repo gets its architecture page from `Templates/Repo-Architecture.md`, and both carry `sources:` and `pin:` so the Docs drift check flags a page the code has moved away from. A preference no reviewer could check belongs in that person's `people/` file.

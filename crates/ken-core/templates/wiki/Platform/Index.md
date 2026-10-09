---
title: "Platform"
aliases: ["Platform", "upstream", "dependencies", "what do we build on", "which version are we on"]
status: current
audience: dev
updated: {{date}}
---

# Platform

How each upstream or reference dependency behaves, checked against its source, one page each. Not here: our own code and how it is layered → [[Conventions]]; our systems → [[Systems]]; the platform's word for one of ours → [[Vocabulary]]; a library pulled in as an ordinary package with nothing to learn about it → the repo's build files.

[[#Pages]] · [[#Belongs Here]]

## Pages

| dependency | version pinned | page | source in the workspace |
|---|---|---|---|
| {{the engine, framework, SDK, service or upstream codebase}} | {{version, and the commit if pinned to one}} | {{[[its page]]}} | {{the reference repo's name, or none}} |

## Belongs Here

A dependency whose behaviour the team has had to learn: an engine, a framework, a hosted service, a vendor API, an upstream codebase kept as a read-only reference repo. Each page is written from `Templates/Dependency.md` and says what the dependency is, the version pinned, its quirks with the evidence for each, where its content lives and how an update lands. A claim on a Platform page is checked against the dependency's source or its running behaviour, not its published docs alone, because the source wins when the two disagree ([[Research-Ladder]]). Search ranks Platform pages ahead of Research, because a Platform page has been checked and a Research page is evidence from its date.

---
title: "Releases"
aliases: ["Releases", "release notes", "changelog", "what changed in {{version}}", "what is unreleased"]
status: current
audience: business
updated: {{date}}
verified:               # the date a person read this page against its sources · empty until then · never stamped in bulk
pin: "{{repo@sha for each repo the sources are in}}"   # the commit each repo's sources were read at
sources:
  - "{{repo:CHANGELOG.md, or the version tags}}"
---

# Releases

What changed in each release, newest first, in words for a reader who never opens the code. Not here: whether a feature is built → [[Feature Status]]; what is planned → [[Roadmap]]; a dependency's own releases → its page on [[Platform]].

[[#Unreleased]] · [[#{{version}}]]

## Unreleased

Merged since {{the newest version tag}}.

| change | kind | ticket |
|---|---|---|
| {{what a user gets, in their words}} | {{new · changed · fixed · removed}} | {{ticket id}} |

## {{version}}

Released {{date}} from tag {{tag}} in {{repo}}.

| change | kind | ticket |
|---|---|---|
| {{what a user gets, in their words}} | {{new · changed · fixed · removed}} | {{ticket id}} |

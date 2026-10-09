---
title: "Registries"
aliases: ["Registries", "what do I have to list when I add something", "where is it registered", "integrity review"]
status: current
audience: dev
lens: integrity         # the reviewer that reads this page (presets.json lenses)
updated: {{date}}
verified:               # the date a person read this page against its sources · empty until then · never stamped in bulk
pin: {{commit}}         # the commit the sources were read at
sources:
  - "{{repo:path of each registry in the code}}"
---

# Registries

What has to be listed when something is added, in the code and in the library, and where. Not here: how the thing itself is written → [[Code Conventions]]; where its files go → [[Data and Config]].

[[#In the Code]] · [[#In the Library]]

## In the Code

| when you add | register it in | missing it means |
|---|---|---|
| {{a command · route · handler · plugin · migration · kind of data file}} | {{repo:path of the registry, list or installer}} | {{what fails, or what silently never runs}} |

## In the Library

| when you add | list it on |
|---|---|
| a feature | [[Feature Status]] |
| a system | [[Systems]] |
| a config file or key | [[Config Map]] |
| a dependency, or a new version of one | its page on [[Platform]] |
| a word the code or the platform says differently | [[Vocabulary]] |
| a change a user will see | [[Releases]], under Unreleased |
| {{anything else this team lists}} | {{the page}} |

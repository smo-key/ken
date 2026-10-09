---
title: "Reference"
aliases: ["Reference", "where is it", "look-up pages"]
status: current
audience: dev
updated: {{date}}
---

# Reference

The look-up pages: the systems, the config map, how to build and run each repo, the vocabulary and the how-tos. Not here: the rules a review checks code against → [[Conventions]]; how a dependency behaves → [[Platform]]; whether a feature is built → [[Feature Status]].

[[#Pages]] · [[#Belongs Here]]

## Pages

| page | holds |
|---|---|
| [[Systems]] | one row per system: what it does · its code · its data · its rulings · its page |
| [[Config Map]] | every config file and key: default · read by · what it changes |
| [[Build and Run]] | per repo: prerequisites · the commands to build, test and run · environment · common failures |
| [[Vocabulary]] | the words where a search misses: words that differ · words with two meanings · renames |
| [[How-tos]] | each how-to and runbook, with when you need it |
| {{[[Calculator or Look-up Page]]}} | {{what it answers}} |

## Belongs Here

A page a reader opens to look one thing up, written from the code and data and checked against them. A system with more to say than its row gets its own page in `Reference/Systems/` from `Templates/System.md`. A how-to or a runbook goes in `Reference/How-tos/` from `Templates/How-to.md`; a runbook is a how-to for operating the running product.

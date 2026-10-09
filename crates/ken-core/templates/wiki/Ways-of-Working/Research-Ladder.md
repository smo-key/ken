---
title: "Research Ladder"
aliases: ["Research Ladder", "Research ladder", "where do I look", "what order do I search in", "which source wins"]
status: current
audience: method
updated: {{date}}
---

# Research Ladder

Which source wins when two disagree, and the order to search them in. Not here: the session checklist → [[Session Start]]; the words to search with → [[Vocabulary]].

[[#Authority]] · [[#Search Order]] · [[#Write-Back]]

## Authority

On what the system does, the lower row wins. Where a page records what was agreed and the code differs, the code is fixed to match the page, and the page is never edited down to the code.

| rung | what it is |
|---|---|
| outside docs | a summary of something else · weakest |
| our library | what we wrote about our own work |
| our code | what we built · wins over the library |
| the platform's source | the upstream code we build on · the final word on what it does |

## Search Order

Cheapest first, with the team repo after the library, because the cheap sources tell you where to look in the expensive ones:

1. the library, starting at [[Start Here]] and [[Vocabulary]]
2. the team repo: tickets and the decisions log
3. our code and the data files: the code map first (definition, usages, outline, imports), then grep
4. the platform's config and source, starting at its page on [[Platform]]

For why code is as it is, read its git history: the commits that touched the file, or the commits whose message or diff contains the term.

When a question takes the shape "does X exist" or "what are the options for X", the next action is a library search. Search the library three ways at once: the knowledge graph, search, and the links between its pages. Every hit carries its kind (code · test · spec · doc · config · data · design · meeting · ticket); filter by kind when you know which one you need. An empty result proves nothing until a wrong term, a filtered path and a timed-out tool are ruled out; for a wrong term, check [[Vocabulary]]. If search returns little, read the repos directly before answering. Before reporting that something does not exist, search all four sources in the search order, and name what you searched.

## Write-Back

A finding lists every page it read, marks each page accurate or stale and corrected, and lists each page it found missing. When the answer was found below the library, it is written back into the page that owns it before the question is closed.

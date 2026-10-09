---
title: "Session Start"
aliases: ["Session Start", "where do I start a session", "what do I read before a ticket", "Agents"]
status: current
audience: method
updated: {{date}}
---

# Session Start

The checklist for a new ticket or a fresh session, read first every time. Not here: which source wins and the search order → [[Research-Ladder]]; the rules → [[Rules]].

[[#Checklist]] · [[#Reading Cost]]

## Checklist

1. Open the ticket and set `status: in-progress`.
2. Open the repo map (`Repo-Map/`, where things live in each repo) for where the code lives, and [[Build and Run]] for how to run it.
3. Open one matching how-to, system page or convention page.
4. Search the code only then, under the paths those pages named. Use the code map before grep: the definition, its usages, the file's outline and its imports. Read the lines around each hit, never a long file whole.
5. Read the platform's generated or decompiled sources last.
6. Write what you learn that will last back into the page that owns it, because a fact left in a chat log is found by no later session.

## Reading Cost

Reading the repo map or one how-to costs fewer tokens than searching many files, and re-deriving what a page already holds costs the most. After a hard bug, the lesson goes into the how-to or the convention page it belongs to. For why code changed, read its git history.

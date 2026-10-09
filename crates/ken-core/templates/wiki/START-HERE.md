---
title: "Start Here"
aliases: ["Start Here", "where do I start", "what is in the library", "what should I read first"]
status: current
audience: business
updated: {{date}}
---

# Start Here

The {{team_name}} library: the traps that cost the most, the repos, the sections and the order to read them in. Not here: how we work → [[Ways-of-Working]]; what is true about the project now → [[Current]].

[[#Traps]] · [[#Repos]] · [[#Sections]] · [[#Reading Order]] · [[#Other Folders]]

## Traps

| trap | what happens | page |
|---|---|---|
| {{the trap that costs a newcomer the most time, such as where the platform's content lives or a word the code spells differently}} | {{what a newcomer loses before finding it}} | {{[[the page that holds it]]}} |

## Repos

Each repo sits beside this one under one folder. Name a sibling by its repo name, never by an absolute path, because the parent folder differs per machine.

| repo | what it is |
|---|---|
| `{{wiki_repo}}` | this repo · the library |
| `{{team_repo}}` | tickets · ideas · escalations · decisions · people · method and team config |
| `{{code_repo}}` | {{what the code repo builds}} |
| `{{reference_repo}}` | {{the upstream source we read and never write}} |

## Sections

Every page has one home, and each section's index says what belongs there and where the rest goes.

| section | holds | pages |
|---|---|---|
| [[Ways-of-Working]] | how we work · the rules, one page each · the only section people must follow | {{n}} |
| [[Conventions]] | how we do things here, one page per area · what a review reads a change against | {{n}} |
| [[Platform]] | each upstream or reference dependency: its version · its quirks · where its content lives | {{n}} |
| [[Design]] | what we chose to build, and why not the alternative | {{n}} |
| [[Work]] | release notes and release history · incidents | {{n}} |
| [[Reference]] | systems · config map · build and run · vocabulary · how-tos | {{n}} |
| [[Current]] | the project · the team · who does what · feature status · roadmap | {{n}} |
| [[Research]] | what past sessions found, dated · the Ingestion inbox | {{n}} |

Pages counted {{date}}. Research is evidence at the time it was written, never current state, so a Research page is cited with its date.

## Reading Order

1. The first row of Traps.
2. [[Vocabulary]] · the words where a search misses. Check it before deciding something does not exist.
3. [[Research-Ladder]] · where to look, in what order.
4. [[Project]] and [[Feature Status]] · what we are building, and what is built.
5. [[Build and Run]] · how to build, test and run each repo.
6. [[Lifecycle]] · Plan, Size, Build, Release, Learn.
7. [[Rules]] · the rules, one page each, with the incident behind each.

## Other Folders

| folder | holds |
|---|---|
| `Templates/` | the blanks a new page is copied from · listed on [[Templates]] |
| `_meta/` | the library's own checks and housekeeping |
| `Repo-Map/` | where things live in each repo · written by Ken from the code |
| `tickets/` · `ideas/` · `escalations/` · `decisions/` · `people/` · `method/` · `team/` · `runs/` | the team repo's folders, here when the team keeps tickets and library in one repo |

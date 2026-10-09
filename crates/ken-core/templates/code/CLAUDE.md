# {{code_repo}}

{{One line: what this repo is.}}

## Where Everything Else Lives

| what | where |
|---|---|
| documentation | `../{{wiki_repo}}` · start at `START-HERE.md` |
| how to build, test and run this repo | `../{{wiki_repo}}/Reference/Build-and-Run.md` |
| this repo's layers and what may call what | `../{{wiki_repo}}/Conventions/Architecture-{{code_repo}}.md` |
| tickets · ideas · escalations · decisions · method | `../{{team_repo}}` |

This file loads into every session, so it carries the short form of every rule and trap for this repo. If it and the library disagree, this file wins and the library is fixed the same day.

**Search the library first.** Re-deriving from code is slower than reading the page that holds the answer. If the answer is not there and you find it, write it back; the finding is not done until you do.

**Search before saying something is missing.** Before you report that something does not exist, search the code, the data files, the platform's config and the platform's source. Name what you searched. Search the library three ways at once: the knowledge graph, search, and the links between its pages.

**Find the place with a search, then read the lines around it.** Do not read a long file whole.

**Escalate a dead end.** When your approach cannot meet the ticket's acceptance, escalate it to a person. Give the other approaches you found and what each would cost. Only a person may shrink the goal. A report that something cannot be done names each approach tried and what ruled it out, and says no way has been found yet rather than that there is none.

## Traps

- {{the thing that costs the most time if you do not know it, and the page that holds it}}

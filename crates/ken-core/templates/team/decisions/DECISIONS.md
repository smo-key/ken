# Decisions

Every ruling a person made, in their words, added and never edited. Not here: why a design went the way it did → the library's Design section; a ruling the code cites that is not here yet → `Cited-in-Code.md`.

[[#Entry Format]] · [[#Topic Index]] · [[#Conflicts]] · [[#Open Questions]] · [[#Log]]

## Entry Format

```
D-001 - 2026-01-01 - topic - THE RULING IN ONE BOLD SENTENCE.
  Supporting mechanics, traps and context.
  - "the words the decider used, typos included and marked [sic]" -
  sources: repo:path:line, [[Note Name]], repo@sha
  aliases: every term anyone might search for this by
  [REFINES] D-nnn   (or [SUPERSEDED BY D-nnn])
```

| rule | why |
|---|---|
| Write the ruling when it is made. | A ruling that lives only in a ticket disappears when the ticket closes. |
| Quote the decider verbatim. | A paraphrase is an interpretation, and interpretations drift. |
| Add the term you searched for to `aliases:` when a search missed the entry. | The next person to search may use the same term. |
| Cite where the ruling landed in the fixed form: `repo:path:line`, `[[Note Name]]` or `repo@sha`. | The Decision drift check reads each entry's `sources:` against what is there now, measured from the entry's own date. |
| Never delete or renumber. A reversed ruling stays, stamped `[SUPERSEDED BY D-nnn]`. | Commits and code cite the numbers. |
| Check the supersession field before re-opening a ruling. | With none, the ruling stands. |

## Topic Index

| topic | rulings |
|---|---|
| {{topic}} | {{D-nnn · D-nnn}} |

## Conflicts

Two of a decider's own rulings that contradict each other. Do not build on either until the decider is asked.

| conflict | rulings | asked |
|---|---|---|
| {{what the two disagree on}} | {{D-nnn · D-nnn}} | {{who, when, and the escalation E-nnn}} |

## Open Questions

Not decisions, and never cited as settled.

| question | to | escalation |
|---|---|---|
| {{the question}} | {{who decides}} | {{E-nnn}} |

## Log

# {{wiki_repo}}

The {{workspace_name}} knowledge base. Entry point: `START-HERE.md`.

This file loads into every session, so it holds the short form of every rule and how to work in this library. The full rules are in `Rules/`. If the two disagree, this file wins and the rule's page is fixed the same day.

## What This Is

A library Claude keeps for the project, beside the code. **The code is the truth for how the product works today. The library complements it and never copies it:** a page holds what the code cannot say (why it is built this way, what was decided and reversed, what came out of a meeting, how the parts fit) and points to a file by its path, in backticks, for the detail.

| folder | holds |
|---|---|
| `Rules/` | how the team works and the conventions it holds itself to, one rule per page · binding |
| `Decisions/DECISIONS.md` | every ruling, added and never edited · binding until superseded |
| `Design/` | what we chose to build, and why not the alternative |
| `Current/` | what is true now: the project, the roadmap, feature status · rewritten in place, never appended to |
| `Current/RAID.md` | the risks and issues, `## R-001 · date · title` then `status:`, `owner:`, `sources:` · an entry is added with the next number, then its status changes in place |
| `Reference/` | the vocabulary, how-tos, where things live · `Platform/`, one page per dependency we build on |
| `Research/` | dated findings · `Ingestion/Raw/` is the inbox, `Ingestion/Ingested/` the notes · evidence at its date, never current state |
| `tickets/` | the work in flight and the work done |
| `Templates/` | the blanks a new page is copied from |
| `log.md` | one dated line per ingest, drift run and lint pass |

## Answering

- **Search first, with Ken's tools.** For code, use the code index (definition, usages, outline, imports) before grep, and read the lines around a hit, not the whole file. For why code changed, read its git history.
- **A ruling is settled.** Do not reopen it in an answer. Where the code does not match it, say so and cite both.
- **The code wins on what was built.** A page, ticket or plan says what was intended; when it disagrees with the code, say so and cite both.
- **An empty search proves nothing.** Check `Reference/Vocabulary.md` for the other word, then read the repos directly before saying something is not there, and say where you looked.
- **A Research page or an ingested note is evidence from its date.** Cite it with its date.
- **File a good answer back** as a source when it will be asked again (`add_source`); ingest makes the pages.

## Ingesting

A source dropped in `Research/Ingestion/Raw/` becomes a dated note in `Ingested/`, and the source moves beside it. The pages it changes are updated one section at a time, citing the note, and a new page joins its section's index. What it contradicts, in a page, a ruling, a ticket or the code, is named and becomes a finding, as does each question it leaves open. A ruling waits for one click into the decisions log, and a risk or issue for one click into `Current/RAID.md`; an action becomes a drafted ticket. An edit that rewrites more than a fifth of a page, lands on a `Rules/` page, or contradicts a ruling or the code waits for a person. Each ingest is one commit in this repo with one line in `log.md`, so git takes it back.

## Keeping It True

- A page lists what its claims rest on under `sources:`, each with the commit it was read at.
- A page that is no longer true is retired, not deleted: `status: superseded` and `superseded_by:` naming the page that replaces it (a path or a `[[link]]`). Move anything true that exists nowhere else first.
- A move rewrites the links to the page in the same change.
- A quoted number carries the date it was measured.
- {{a trap that costs time on this team, and the page that holds it}}

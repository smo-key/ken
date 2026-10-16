# A project knowledge base, on one page

## The idea

This is Karpathy's [LLM wiki](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f), applied to a project. An agent doesn't search raw documents again for every question. It keeps a wiki of the project up to date, so the summaries, cross-links and contradictions are already worked out when someone asks.

- **The code is the truth** for how things work today. The knowledge base holds what code can't: why, what was decided, and what is true now. It never copies the code.
- **People bring sources and ask questions. The agent does the bookkeeping:** notes, page edits, links and the log.
- **It is one git repo of Markdown.** Git gives you history, undo and sharing.

## Three layers

| layer | what | who writes it |
|---|---|---|
| Sources | meeting notes, documents, transcripts, the code repos | people; never edited |
| Library | the pages below | the agent, with people reviewing what binds |
| Brief | `CLAUDE.md` (or `AGENTS.md`): the structure, conventions and workflows | people and the agent, over time |

## Structure

| part | holds |
|---|---|
| `START-HERE.md` | where to begin, the repos covered, reading order |
| `Rules/` | how the team works. Binding. |
| `Decisions/DECISIONS.md` | rulings, appended and never edited. Binding until superseded. |
| `Current/` | what is true now: Project, Roadmap, Feature Status, `RAID.md` |
| `Design/` | why something was built this way |
| `Reference/` | vocabulary and platform facts |
| `Research/` | `Raw/` for new sources, `Ingested/` for their notes |
| `tickets/` | actions, one file each |
| `log.md` | one line per operation |
| `Index.md` | one per section, a line per page |

## Conventions

- **Frontmatter on every page:** `title`, `aliases`, `status` (current, draft, superseded), `updated`, `sources` (what the page rests on, with the commit it was read at), `checked`, and `superseded_by`.
- **A ruling** reads `## D-001 · 2026-10-14 · topic`, then the ruling in one sentence, the quote it was made in, `Why:`, `sources:`, and `supersedes:` when it replaces an earlier one.
- **The RAID register** holds risks `R-001` and issues `I-001`, each with a status and an owner. Actions are tickets.
- **The log** has one line per operation: `## [2026-10-14] ingest | title`. Then `grep "^## \[" log.md` is the history.
- **Tiers, for answering:** binding (rules, rulings), then current, reference, record (notes, tickets), and superseded. A higher tier wins on the same point.

## Four operations

1. **Ingest.** Drop a source into `Raw/`. The agent:
   - writes a dated note and moves the source beside it;
   - edits the pages it changes, section by section;
   - adds rulings, tickets and risks;
   - flags contradictions and open questions;
   - adds a log line.

   A person reviews edits to Rules, and anything that goes against the code. One commit per ingest.
2. **Query.** Ask; the agent reads the index and the pages, and answers with citations. A binding ruling beats a note. Where the code differs from a ruling, the answer says so. A good answer is filed back as a page.
3. **Lint.** Periodically look for broken links, orphan pages, missing pages, contradictions, superseded rulings and pages not checked in a while.
4. **Drift.** When cited code moves, re-read only the pages that cite it. Then either fix the page or stamp `checked:` with the new commit.

## Start your own

1. **Make the repo** beside your code repos: `git init`, the folders above, an empty `DECISIONS.md`, `log.md` and `RAID.md`.
2. **Write the brief.** In `CLAUDE.md`, put the structure, the conventions above, and the four operations as workflows the agent follows.
3. **Write `START-HERE.md`:** what the project is, the repos it covers, where to begin.
4. **Ingest your first source:** drop a meeting note into `Raw/` and ask the agent to ingest it per the brief. Review the result and commit.
5. **Keep going:** ingest each new source, ask questions in the agent, lint weekly, re-check pages after big merges.

## Ground rules

- The agent commits only in the knowledge base, never in a code repo.
- Pages say why and what was decided. The code says how.
- Rulings are never edited, only superseded.
- When something isn't known, the page says so.

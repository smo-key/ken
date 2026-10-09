# {{wiki_repo}}

The {{team_name}} library. Entry point: `START-HERE.md`.

This file loads into every session, so it carries the short form of every rule. The library holds the evidence behind each. If the two disagree, this file wins and the library is fixed the same day.

## Sections

`Ways-of-Working/` is the only section people must follow. The others describe what exists.

| read | for |
|---|---|
| `Ways-of-Working/Ways-of-Working.md` | the method, and where to look in what order |
| `Ways-of-Working/Rules/` | the rules · one file per rule, named as the rule, each carrying its incident |
| `Ways-of-Working/Agents/Index.md` | session start: the checklist for a new ticket or session |
| `Conventions/` | how we do things here, one page per area · what a review reads a change against · architecture per repo |
| `Platform/` | each upstream or reference dependency: version pinned · quirks · where its content lives · how updates land |
| `Design/` | what we chose to build, and why not the alternative |
| `Work/` | release notes and release history · incidents |
| `Reference/` | systems · config map · build and run per repo · vocabulary · how-tos |
| `Current/` | what is true now about the project and the team · rewritten in place, never appended to |
| `Research/` | dated findings · cite with the date, never read as current state |
| `Research/Ingestion/` | the inbox: a source dropped in `Raw/` becomes a dated note in `Ingested/` · neither folder is a research source |
| `Templates/` | the blanks a new page is copied from |

## Rules

- **Search the library first.** Re-deriving from code is slower than reading the page that holds the answer. If the answer is not here and you find it, write it back; the finding is not done until you do.
- **Search before saying something is missing.** Before reporting that something does not exist, search the code, the data files, the platform's config and the platform's source, and name what was searched. Search the library three ways at once: the knowledge graph, search, and the links between its pages. If search returns little, read the repos directly before answering.
- **Check the vocabulary on an empty search.** `Reference/Vocabulary.md` holds the words where the team, the code and the platform differ.
- **Use the code map before grep.** The code map gives each symbol's definition and usages, and each file's outline and imports. For why code changed, read its git history.
- **A Research page is evidence from its date, not current state.** Cite it with its date.
- **Open the repo map or a how-to before searching files.** The repo map (`Repo-Map/`, where things live in each repo) says where to look and a how-to says how a task is done, and reading one page costs fewer tokens than searching many files.
- **A source dropped in `Research/Ingestion/Raw/` is written up at once, and nobody confirms it.** Each source gets a write-up from `Templates/Ingested-note.md`, keeping the sections that kind of source needs. What follows from it is written and cites the note. A ruling waits for its decider, and a change to Ways-of-Working, Conventions or a rule becomes a ticket, because work and reviews are read against them.
- **A page is done when something links to it.** A page nothing links to is never found by a later search.
- {{a trap that costs time on this team, and the page that holds it}}

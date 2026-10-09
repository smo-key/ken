# {{team_name}}

The team's operational state: tickets, ideas, escalations, decisions, the method and the team's overlay, people and run records. Not here: the library → `START-HERE.md`, in this repo when the team keeps both together, otherwise in `../{{wiki_repo}}`.

## Folders

| folder | holds |
|---|---|
| `tickets/` | one file per ticket: frontmatter · body · thread · written from `TICKET.md` |
| `ideas/` | the Ideas list on Intake's Ideas tab · one file per rough idea, I-001 onwards · written from `IDEA.md` |
| `escalations/` | one file per escalation, from an agent or a person, with its thread of replies · E-001 onwards · written from `ESCALATION.md` |
| `decisions/` | the decisions log, added to and never edited · the rulings the code cites |
| `method/` | the shared definition: steps · presets · gates |
| `team/` | this team's overlay · add-only |
| `people/` | who is on the team · one file each, written from `PERSON.md` |
| `runs/` | run records: which gate ran · when · its exit code · its log |
| `.wright/team.json` | the team's repos and their kinds · the ticket key · the model per size · the standing sweep |

## One Repo or Two

These folders change hourly, and the library's pages pin themselves to commits: the Docs drift check reads `git diff <pin>..<default branch>`, so a library kept in its own repo has a history that stays readable. A team with no team repo keeps these folders in its docs repo, beside the library's sections; no file here shares a path with a library file, so the two templates lay down into one folder. `.wright/team.json` then lists that repo once, with kind `["team", "wiki"]`.

## Not in the Template

These files belong in the team repo, and the template lays none of them down. Until a team adds the guard, nothing but review stops an overlay change from removing something load-bearing.

| file | what it does | where it comes from |
|---|---|---|
| `method/presets.json` | the presets as data · the source of truth the prose pages cite, never copy | the method's `presets/presets.json` |
| `team/overlay.json` | the overlay as data: custom fields and their types, extra statuses, presets, steps and gates | written by the team · empty until it adds something |
| `.github/workflows/guard.yml` | the configuration guard: on every push, checks the six core fields, the thread section, the never-droppable gates, a type on every custom field and a stage for every status · carries a broken overlay as its control | not written yet |

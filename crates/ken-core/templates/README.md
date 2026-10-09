# Templates

The three repo templates, every page in them, who drafts each page, and what Ken can fill from the repos against what only a person knows. Not here: what the library's sections are for → `docs/manual/docs-system.html`; how a page is written → `docs/manual/writing.html`.

Contents: Directories · One Folder or Two · Placeholders · Library Pages · Library Blanks · Team Repo · Code Repo

## Directories

`ken init` lays these down, and `ken doctor` runs the same code later and reports what is missing or out of date rather than overwriting it.

| directory | becomes | changes |
|---|---|---|
| `wiki/` | the library: the eight sections, each with an index · the pages a team needs from day one · the blanks in `Templates/` · the Ingestion inbox | weekly |
| `team/` | the team repo: tickets · the Ideas list · escalations · decisions · method and team config · people · run records | hourly |
| `code/` | two files dropped into an existing code repo: the brief and the gate manifest | rarely |

`code/` is the smallest because the team does not always own the code repo, and a method that restructures somebody else's repo does not get adopted.

## One Folder or Two

A team with no team repo keeps tickets, ideas, escalations, decisions and people in its docs repo. `wiki/` and `team/` then lay down into one folder: no path is in both, the team's folders sit beside the library's sections, and `.wright/team.json` lists the repo once with kind `["team", "wiki"]`.

## Placeholders

A placeholder is written `{{what goes here}}` and says what goes there. Ken fills `{{team_name}}`, `{{wiki_repo}}`, `{{team_repo}}`, `{{KEY}}`, the repo table on Start Here and every `updated:` date at set-up. A page Ken drafts fills what the repos show, and the rest stays for a person. Only a page with `sources:` carries `pin:` and `verified:`; the entry point, the section indexes and the list pages carry neither, and a Research finding has `pin:` but no `verified:`. `verified:` stays empty until a person reads the page against its sources.

## Library Pages

Drafted by: **set-up** is filled at set-up with names only · **Ken** is drafted from the repos for a person to read · **method** ships as written · **person** is written by the team. Ken drafts a page only while it is missing or still has placeholders, marks it `status: draft`, and never touches a page a person wrote.

| page | section | drafted by | Ken can fill from the repos | only a person knows |
|---|---|---|---|---|
| `START-HERE.md` | entry point | set-up, then Ken | the repo table · page counts · the traps in the code briefs' Traps sections | which trap costs the most |
| `CLAUDE.md` | brief | method | — | the team's own traps |
| `_meta/README.md` | housekeeping | method | — | which scripts the team adds |
| `*/Index.md` and `Ways-of-Working/Ways-of-Working.md` | each section's index | method | the page lists, as pages are added | a section's own scope rules |
| `Ways-of-Working/Lifecycle.md` · `Research-Ladder.md` · `Rules.md` · `Agents/Index.md` · `Rules/write-for-the-altitude-like-teammates-talking.md` | Ways-of-Working | method | — | the team's own rules and their incidents |
| `Conventions/Architecture.md` | Conventions | Ken | each code repo · what it builds · which repos import or call which | which calls are forbidden, and why |
| `Conventions/Code.md` | Conventions | Ken | the lint and formatter config · the helpers that exist and where | the rules no tool enforces · the incidents behind them |
| `Conventions/Data-and-Config.md` | Conventions | Ken | the data and config folders · formats · the loaders | the rules for shape and naming · where secrets live |
| `Conventions/Testing.md` | Conventions | Ken | the suites from `.ken/gates.json` and the test config · fixture folders | what a test may assume · measured suite times |
| `Conventions/Registries.md` | Conventions | Ken | the registries in the code: installers · routes · command lists | what the team lists in the library beyond the defaults |
| `Platform/Index.md` | Platform | Ken | one row per `reference` repo and pinned dependency | which dependencies the team has had to learn |
| `Design/Principles.md` | Design | person | — | the product rules and their rulings |
| `Work/Releases.md` | Work | Ken | Unreleased from the commits since the newest version tag · one section per version from the changelog and tags | what a change means to a user, where the commit does not say |
| `Work/Incidents.md` | Work | person | — | every row |
| `Reference/Systems.md` | Reference | Ken | one row per system from the registration code · its code and data paths · the D-nnn its code cites | what a system is for, in the user's words |
| `Reference/Config-Map.md` | Reference | Ken | every config file and key · defaults · the code that reads each key | what a key changes for a user |
| `Reference/Build-and-Run.md` | Reference | Ken | per repo: the tools and versions the build files pin · the scripts and tasks the build files define · `.ken/gates.json` commands · environment variables the code reads | the failures people hit, and their fixes |
| `Reference/Vocabulary.md` | Reference | Ken | words the briefs, comments and config keys say differ from the code's or platform's · renames from commit messages | the team's and the users' own words |
| `Reference/How-tos.md` | Reference | person | — | every row |
| `Current/Project.md` | Current | Ken | what the product is, from the READMEs and briefs | the users · the goal · what is out of scope |
| `Current/Roadmap.md` | Current | person | — | every milestone and date |
| `Current/Feature-Status.md` | Current | Ken | each feature from the changelog and the registration code · the tests that name it · the commit read | whether it was used in the running product |
| `Current/Team.md` | Current | Ken | roles a `people/` file or a repo doc names | who decides what · the handoffs |
| `Current/Who-Does-What.md` | Current | Ken | owners from `people/`, then CODEOWNERS · recent committers per area from git, named as `people/` names them | who to ask |
| `Research/Ingestion/Index.md` | Research | method | the contents lists, as sources arrive | — |
| `Repo-Map/Index.md` · `Repo-Map/{{repo}}.md` | Repo Map, a section Ken writes from the code | Ken | one page per repo: what it is for · what lives where · which folders call which · who owns it and who commits · how it is built, run and released · the index of those pages | what a repo is for, where its own docs do not say |

## Library Blanks

The blanks in `wiki/Templates/` keep their placeholders at set-up. `Templates/Index.md` says where each copy goes.

| blank | one per | drafted by | Ken can fill from the repos | only a person knows |
|---|---|---|---|---|
| `Repo-Architecture.md` | code repo | Ken | the layers from the brief and folder layout · import pairs that break them · the checks that enforce them | why a call is forbidden |
| `Dependency.md` | reference repo or pinned dependency | Ken | the version and where it is pinned · where its source, docs and data live · the licence | the quirks and the update procedure |
| `System.md` | system | Ken, then a person | entry points · code · data · cited rulings | what it is for · known gaps |
| `Convention.md` | area a reviewer checks | person | the paths it covers · the helpers that exist | the rules and their incidents |
| `Design-Note.md` | design question | person | — | every section |
| `Finding.md` | question a session answered | the session | the evidence and the pages read | — |
| `How-to.md` | task or runbook | person | the commands and the files a step touches | when you need it · the failures |
| `Incident.md` | incident | person | the fix's commit | every other section |
| `Ingested-note.md` | source in `Raw/` | Ken's Ingest | every section, from the source | whether a ruling is accepted |
| `Rule.md` | rule | person | — | the rule and its incident |

## Team Repo

| file | drafted by | Ken can fill | only a person knows |
|---|---|---|---|
| `README.md` · the folder READMEs | method | the team name | — |
| `.wright/team.json` | set-up | the team name · the ticket key · one entry per repo with its kind and base branch · `protected` from `.ken/gates.json` | models per size · the standing sweep's person and controls · where worktrees go |
| `decisions/DECISIONS.md` | person | rulings from an Ingested note, once the decider accepts each | every ruling |
| `decisions/Cited-in-Code.md` | Ken, generated | every D-nnn a code comment cites, and whether the log has it | whether a cited ruling is real |
| `tickets/TICKET.md` · `ideas/IDEA.md` · `escalations/ESCALATION.md` · `people/PERSON.md` | person | the ticket key | everything else |

## Code Repo

| file | drafted by | Ken can fill | only a person knows |
|---|---|---|---|
| `CLAUDE.md` | Wright, when the repo joins a team | the repo's one line · the sibling repo names | the traps |
| `.ken/gates.json` | Wright, when the repo joins a team | the commands from the build files and CI config | the fast suite's time budget |

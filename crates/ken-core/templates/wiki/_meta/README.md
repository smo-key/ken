# _meta

The library's own checks and housekeeping: each script's kind, the contract every instrument meets, and the rules for generated pages. Not here: what each section holds → `START-HERE.md`.

[[#Scripts]] · [[#Checks the Method Expects]] · [[#The Instrument Contract]] · [[#Generated Pages]]

## Scripts

| script | kind | does |
|---|---|---|
| {{script name}} | {{instrument · gate · historical}} | {{what it checks, and its control}} |

An instrument is a read-only script that measures and reports through its exit code. A gate runs in continuous integration (CI), reads an instrument and makes the pass-or-fail call. A historical script ran once for a migration or backfill and is kept to be read, never run again as-is. A reader who cannot tell them apart runs the wrong one, or deletes the one CI depends on.

## Checks the Method Expects

None of these scripts is here at set-up. Ken runs its own link and drift checks against the library, and a script the team adds is listed under Scripts.

| check | kind | does |
|---|---|---|
| link check | instrument | every link resolves · path links and name links counted apart · reads `aliases:` · reports names two pages both claim · run before and after any bulk move |
| link gate | gate | runs the link check · fails on exit 1 · reports exit 2 |
| drift check | instrument | the Scope, Decision, Docs and Answer checks, each declared against actual · movement measured against each repo's default branch, read from git · a cited line past the end of its file flagged · only code citations measured for movement · a page unverified for thirty days raised · the decisions log checked per entry, from each entry's own date · Research in its own group |
| drift gate | gate | runs the drift check · fails on exit 1 · reports exit 2 |
| inventory | instrument | section sizes · orphans · pages with no index entry · rule pages [[Rules]] does not list |

## The Instrument Contract

| rule | why |
|---|---|
| carries a control it must fail, and one it must pass, and a minimum count of pages examined | a run whose known-bad control reads clean, whose known-good control reads flagged, or that examined too few pages is void and says so |
| read-only | the number stays quotable outside CI, and the pass rule changes without touching the measurement |
| three exit codes: 0 clean · 1 instrument broken, or a live locator (a cited `repo:path:line`) missing · 2 findings only | CI fails on 1 and reports 2 in the same run |
| finds its root from its own location, with sibling repos beside the library and one environment variable to override | a hardcoded parent path walks zero files on any other checkout and reports a clean run |
| says what it could not reach, and how to enable it | an instrument that stays quiet reports a clean run over the part it never read |
| strips before it counts: fenced blocks and placeholders are never links or citations · the link check also strips inline code, the drift check reads inside it · build output, dependency folders and logs are never missing paths | the templates ship placeholder paths that look like citations |
| files what it finds from its first run · a broken link or a drift Finding becomes a ticket · a drift Judgment, or a page raised for age, becomes a row on the Ideas list | the fix is reviewed and tracked on the ticket like any other change |

## Generated Pages

A generated page carries `generated: true`, `upstream:`, `upstream_commit:`, `updated:` (the date it was built) and `regenerate:` (the command that rebuilds it) in its frontmatter, and says so in its first line. A mistake is fixed in the generator and every page rebuilt; the pages are never edited by hand. Every page from one generator pins the same `upstream_commit`.

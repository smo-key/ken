# ingest-refresh

## ADDED Requirements

### Requirement: Incremental, outputs-canonical refresh
A refresh SHALL compose its prompt from the recipe instruction, the files
changed under the ingest's sources since the last successful run (all
source files on first run), and the current outputs — instructing the
agent that existing documents are canonical, to update only what new data
implies, to preserve human edits, and to record assumptions in the output
rather than asking questions. The agent SHALL write proposed outputs to a
staging area; Ken alone decides what reaches the real output paths.

#### Scenario: Incremental prompt scope
- **WHEN** two source files changed since the last successful run
- **THEN** the composed prompt lists exactly those files as changed input,
  not the whole corpus

#### Scenario: First run covers the corpus
- **WHEN** an ingest runs for the first time
- **THEN** the prompt covers all indexed files under its sources and the
  result is applied without threshold review (nothing to overwrite)

### Requirement: Review rules govern what gets written
Review rules SHALL resolve as recipe overrides > project defaults >
built-ins (20% threshold, 30-day stale check). A completed run whose
change ratio is at or below the threshold SHALL apply automatically; above
it, the run SHALL be held `pending approval` with staged content kept.
Approving applies the staged output; discarding deletes it. If an output
file changed on disk between run start and apply, the apply SHALL demote
to `pending approval` instead of overwriting the newer human edit.

#### Scenario: Small refresh applies silently
- **WHEN** a run changes 5% of an output
- **THEN** the output file is updated and the run is recorded `fresh`

#### Scenario: Large refresh is held
- **WHEN** a run changes more than the threshold
- **THEN** no output file changes until the user approves from the ingest
  detail; discard leaves outputs untouched

#### Scenario: Mid-run human edit wins
- **WHEN** the user saves an output file while a run is in flight
- **THEN** the run's apply is held for approval instead of overwriting

### Requirement: Automatic refresh on source changes
Ingests with `refresh: on-change` SHALL queue automatically (debounced)
when indexed files under their sources change; runs SHALL execute one at a
time per project; an ingest's own outputs SHALL NOT re-trigger it. Manual
"Run now" SHALL always be available, including a full-corpus pass when no
sources changed. Ingests whose sources have not changed within the stale
window SHALL show a stale indicator.

#### Scenario: Source edit triggers refresh
- **WHEN** a file under an on-change ingest's sources is modified
- **THEN** a run for that ingest starts after the debounce without user
  action

#### Scenario: No self-retrigger loop
- **WHEN** an ingest run applies changes to its own output folder
- **THEN** that ingest does not queue another run because of those writes

### Requirement: Run log
Every run SHALL be recorded (start/finish, status: running / fresh /
blocked on you / pending approval / failed / discarded, change ratio,
plain-language summary or error) and shown newest-first in the ingest's
Recent runs card. Pending approvals SHALL also be visible on Home as
"waiting on you" items linking to the ingest.

#### Scenario: Run history visible
- **WHEN** the user opens an ingest's detail after several runs
- **THEN** Recent runs lists them with status dots, times, and summaries,
  and failed runs expose their error detail

#### Scenario: Pending approval surfaces on Home
- **WHEN** a run is held pending approval
- **THEN** Home shows a waiting-on-you card that opens the ingest detail

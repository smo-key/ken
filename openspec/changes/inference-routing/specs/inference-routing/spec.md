# inference-routing

## ADDED Requirements

### Requirement: A reasoning block never breaks the answer
Model output SHALL be separated into a reasoning span and an answer span
before any parsing. Strict parsers SHALL see only the answer. Output with
no recognisable reasoning SHALL be treated entirely as answer, and a
reasoning delimiter that cannot be resolved SHALL NOT cause any part of
the answer to be discarded. The delimiter SHALL be configurable per model
rather than fixed to one family's convention.

#### Scenario: A thinking model's JSON parses
- **WHEN** a model emits a reasoning block followed by JSON
- **THEN** the JSON parses and the reasoning is available separately

#### Scenario: A non-thinking model is unaffected
- **WHEN** a model emits only an answer
- **THEN** the output parses exactly as it does today

#### Scenario: A malformed reasoning block loses only the reasoning
- **WHEN** a reasoning block is opened and never closed
- **THEN** the answer is still recovered rather than the response being
  discarded

### Requirement: Reasoning is requested per call site
Whether a model is asked to reason SHALL be a property of the job, not of
the model or a global setting. Background work SHALL request no reasoning;
interactive work SHALL request it. The same model SHALL be usable for both
without reconfiguration.

#### Scenario: Bulk extraction does not pay for reasoning
- **WHEN** per-file extraction runs over a large queue
- **THEN** no reasoning is requested, and the time it would have cost is
  not spent

#### Scenario: An interactive answer reasons
- **WHEN** a person asks a question
- **THEN** reasoning is requested and made available to the caller

### Requirement: Reasoning streams distinctly from the answer
The token stream SHALL label each span as reasoning or answer, and SHALL
preserve arrival order across both. The interface SHALL present reasoning
in a manner clearly subordinate to the answer and SHALL NOT leave it
presented as part of the final answer once that answer has arrived.

#### Scenario: Thinking is visible while it happens
- **WHEN** an interactive answer is generated with reasoning
- **THEN** the reasoning is shown as it arrives, distinctly from the
  answer

#### Scenario: The answer supersedes the thinking
- **WHEN** the answer completes
- **THEN** the reasoning is no longer presented as part of it

#### Scenario: A stream without reasoning is unchanged
- **WHEN** no reasoning is produced
- **THEN** the display is identical to today's

### Requirement: The engine is selectable per job class
Each job class — per-file extraction, knowledge-model building, quick
answers, and conversational or exploratory work — SHALL carry an engine
selection of automatic, local-only, or Claude. Defaults SHALL reproduce
existing behaviour exactly. The selection SHALL persist across restarts.

#### Scenario: Defaults change nothing
- **WHEN** no selection has been made
- **THEN** every job runs on the engine it runs on today

#### Scenario: A choice is honoured
- **WHEN** a job class is set to a specific engine
- **THEN** that job runs on that engine

### Requirement: Local-only means local
When a job class is set to local-only, that job SHALL NOT invoke Claude
under any circumstance, including failure of the local model. Falling back
SHALL be the behaviour of the automatic setting alone.

#### Scenario: A local failure stays local
- **WHEN** the local model fails on a job class set to local-only
- **THEN** the failure is reported and no Claude process is started

#### Scenario: Automatic still falls back
- **WHEN** the local model fails on a job class set to automatic
- **THEN** Claude handles it, as it does today

#### Scenario: Choosing Claude loads no local model
- **WHEN** a job class is set to Claude
- **THEN** no local model is loaded into memory for it

### Requirement: The choice is explained by capability
Where the engine can be chosen, the interface SHALL describe the
difference in terms of what each engine can do — that Claude can open and
search the user's files while the local model answers only from what it is
given — rather than presenting one as simply better. The model catalogue
SHALL state when an entry produces reasoning and another does not.

#### Scenario: The trade-off is stated in terms of capability
- **WHEN** the engine choice is presented
- **THEN** it describes tool access rather than ranking the engines by
  quality

#### Scenario: A reasoning model is labelled as one
- **WHEN** a catalogue entry produces a reasoning block
- **THEN** that is stated where the entry is offered

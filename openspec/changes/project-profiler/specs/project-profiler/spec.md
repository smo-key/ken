# project-profiler Specification

## ADDED Requirements

### Requirement: Deterministic profile without any LLM

`scan_stats(root, excluded)` SHALL produce extension histograms, repo
markers, doc ratio, and size stats as a pure function of the tree
(capped at 50,000 files, marked `truncated` beyond), and
`deterministic_profile(stats)` SHALL always yield a valid
`ProjectProfile` — kind, language guess, marker-based excludes
(`target/` for Cargo, `node_modules/`+`dist/` for Node, etc.), and
per-pattern chunking entries — with no model loaded.

#### Scenario: Rust project profiled offline
- **WHEN** a fixture tree with `Cargo.toml`, `src/*.rs`, and `target/`
  is profiled with no LLM available
- **THEN** the profile has `kind: code`, Rust as a language, `target/`
  in excludes, and `.rs` mapped to code chunking

#### Scenario: docs folder detected
- **WHEN** a fixture tree is >80% markdown/text bytes with no repo
  markers
- **THEN** the profile has `kind: docs` and prose chunking as default

### Requirement: LLM refinement is additive and validated

The optional Background-priority refinement pass SHALL only adjust
`kind`, add (never remove) excludes, and supply `summary` and
`focus_hints`. `parse_profile_refinement` SHALL tolerate fenced/dirty
output, ignore unknown fields, and drop invalid suggestions
item-by-item (nonexistent dirs, dirs containing repo markers). Any
refinement failure SHALL leave the deterministic profile in effect.

#### Scenario: junk suggestion dropped, rest kept
- **WHEN** the model suggests excluding `vendor/` (exists) and
  `imaginary/` (does not exist)
- **THEN** `vendor/` is added, `imaginary/` is dropped, and the
  profile is saved

#### Scenario: model failure falls back
- **WHEN** generation times out or returns unparseable text
- **THEN** the deterministic profile is saved and `profile-state`
  reaches `ready` (not `error`)

### Requirement: Profile file is user-owned text

The profile SHALL be written atomically to `.ken/index-profile.json`,
adopted if already present, and SHALL carry `generated_hash` of Ken's
last generated content. When loaded content hash differs, the profile
is `hand_edited` and re-analysis SHALL require explicit confirmation
before overwriting.

#### Scenario: hand-edited profile is preserved
- **WHEN** the user edits `index-profile.json` and later triggers
  "Re-analyze" without confirming overwrite
- **THEN** the edited file is untouched

### Requirement: Consumers read the profile through existing seams

When a stored profile exists (flag on), chunking SHALL use its
per-pattern entries instead of `IndexProfile::default_for`, effective
exclusions SHALL be user `excluded` ∪ profile `excludes` (additive,
separately listed in settings), and the knowledge-model extraction
prompt SHALL append summary + focus hints capped at 500 chars within
the existing char budget.

#### Scenario: profile drives chunking
- **WHEN** a profile maps `*.log` to skip and ingest runs
- **THEN** `.log` files produce no chunks while other files chunk per
  their profile entries

#### Scenario: flag off is inert
- **WHEN** the `profiler` flag is off for a project with a profile
  file present
- **THEN** the file is not read, chunking uses extension defaults, and
  exclusions are user entries only

### Requirement: Analysis phase in workspace creation

During workspace creation with the flag on, each selected candidate
SHALL be profiled (concurrency 2, deterministic stage surfaced first
as kind badges, refinement streaming after via `profile-state`
events); a skip control SHALL create the workspace without waiting,
and per-candidate profiling failure SHALL never block creation.

#### Scenario: skip analysis
- **WHEN** the user chooses "Skip analysis" mid-phase
- **THEN** the workspace is created immediately and unprofiled members
  can be analyzed later from settings

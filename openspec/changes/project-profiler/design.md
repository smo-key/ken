# Design: project-profiler

## Context

`semantic-index` defines `IndexProfile` in `chunker.rs` with a static
extension map (`default_for(rel_path)`) — the soft-dependency seam this
feature was designed against. `knowledge_model.rs` composes extraction
prompts under strict budgets (EXTRACT_CHAR_BUDGET = 12,000,
MAX_PROMPT_FILES = 400). `project.rs` gives us the persistence idiom:
tolerant serde with a flattened `extra` map, atomic writes,
adopt-if-exists. `local_llm.rs` gives Background-priority generation
that Interactive work preempts.

## Goals / Non-Goals

- Goals: every project gets a usable profile with **zero LLM
  involvement**; the LLM only refines; the profile is a text file the
  user owns and can edit; downstream consumers (chunker, excludes, KG
  prompt) read it through narrow seams.
- Non-Goals: per-file classification (profiles are per-pattern, not
  per-file); language-server-grade analysis; profiling remote/huge
  trees (the scan respects existing exclusion rules and a file cap);
  automatic re-profiling on every ingest (manual or
  workspace-creation-time only).

## Decisions

### D1. Two-stage: deterministic floor, LLM ceiling

`deterministic_profile(stats)` alone must produce a correct-enough
profile — marker-based excludes and extension-based chunking cover the
90% case and are testable as pure functions. The LLM pass is additive
refinement gated on model availability and a settings toggle, and its
output is validated against the stats (e.g. an exclude suggestion for a
dir that doesn't exist is dropped; it can never *remove* deterministic
excludes). Rejected: LLM-first profiling — non-deterministic, slow at
workspace creation (7 members × generation), and unusable before the
model is downloaded.

### D2. Profile lives in `.ken/index-profile.json`, not the DB

Same philosophy as `project.json`: shared, human-editable text source
of truth; the DB stays fully derived. Adopt-if-exists means a teammate
can commit a tuned profile and Ken uses it. Hand-edit protection: the
file stores `generated_hash` (hash of Ken's last generated content);
on load, if current content hash ≠ `generated_hash`, mark
`hand_edited` and never auto-overwrite — "Re-analyze" then requires an
explicit confirm in the UI.

### D3. Consumers read through existing seams, no new coupling

- Chunker: `chunk_file` already takes a profile argument; the caller
  (engine ingest step) resolves it as
  `stored_profile.chunking_for(rel_path)` else
  `IndexProfile::default_for(rel_path)`. One call-site change.
- Excludes: `Project::effective_excluded()` = user `excluded` ∪
  profile `excludes`. Profile excludes are additive-only and listed
  separately in settings so the user sees what the profiler added.
- KG prompt: `knowledge_model.rs` appends
  `"Project summary: {summary}\nFocus areas: {hints}"` when present —
  counted inside EXTRACT_CHAR_BUDGET (capped at 500 chars) so budgets
  don't silently grow.

### D4. Refinement input is stats + a capped tree sample

The prompt gets the ScanStats summary plus at most 150 lines of
depth-2 tree listing (dirs first, largest by bytes). No file contents
— kind/excludes/summary are structural judgments, and this keeps the
prompt small and fast even for huge repos. Output contract is a single
JSON object; `parse_profile_refinement` uses the same tolerant
patterns as `knowledge_model.rs` (strip fences, ignore unknown keys,
drop invalid entries item-by-item rather than failing whole).

### D5. Workspace-creation integration is per-candidate and skippable

During workspace create, profiling runs per selected candidate with
concurrency 2 (same semaphore idea as ingest), deterministic stage
first so the UI shows kind badges immediately, refinement streaming in
after. A "Skip analysis" control creates the workspace without
waiting — profiles can be generated later per project. Profiling never
blocks workspace creation on failure.

## Risks / Trade-offs

- **Scan cost on huge trees** — the walk reuses ingest exclusion rules
  and caps at 50,000 files (stats marked `truncated: true`); still
  pure I/O, no hashing or reading contents.
- **Bad LLM suggestions excluding real content** — excludes are
  validated (dir must exist, must not contain a repo marker, must not
  be in `members`/top-level src patterns) and are visible + removable
  in settings; deterministic excludes are the only ones applied before
  the user has had a chance to see the profile? No — applying them
  immediately is the point; mitigation is visibility + additive-only.
- **Profile drift** as a project evolves — acceptable; "Re-analyze" is
  one click, and stale profiles degrade to slightly suboptimal
  chunking, never breakage.

## Migration

No schema change. Flag off ⇒ no file written or read; existing
projects with no profile behave exactly as `semantic-index` standalone.

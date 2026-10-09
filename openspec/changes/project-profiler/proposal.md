# Proposal: project-profiler

## Why

The workspace's member projects are wildly different — Rust workspaces,
Node apps, tool collections, doc folders — but Ken indexes them all the
same way. `semantic-index` chunking, exclusions, and (later) KG
extraction emphasis should be **fit to each project's shape**. This
change is the "Ken analysis phase": when folders are selected (workspace
creation or single-project create), Ken profiles each folder —
deterministic scan first, optional local-LLM classification second — and
writes a human-editable `index-profile.json` that downstream features
consume. `semantic-index` already defines the `IndexProfile` type and a
default; this feature makes the default smart.

## What Changes

- **New ken-core module `profiler.rs`**:
  - `scan_stats(root, excluded) -> ScanStats` (pure over a dir walk):
    extension histogram (count + bytes), top-level dir names, repo
    markers (`Cargo.toml`, `package.json`, `pyproject.toml`, `.git`,
    `*.sln`, `pom.xml`...), doc ratio (md/txt/pdf bytes vs code
    bytes), largest dirs, total files/bytes.
  - `deterministic_profile(stats) -> ProjectProfile` — rule-based:
    `kind: code|docs|mixed|media`, language guess, recommended
    excludes (build outputs by marker: `target/` for Cargo,
    `node_modules/`+`dist/` for Node, etc.), per-pattern
    `IndexProfile` chunking entries (code extensions → code mode,
    prose → prose mode). Always produces a usable profile with no LLM.
  - `compose_profile_prompt(stats, tree_sample)` +
    `parse_profile_refinement(raw)` — optional Background-priority
    local-LLM pass that sees the stats and a capped 150-line tree
    sample and may: adjust `kind`, add excludes (generated/vendored
    dirs rules can't catch), write a one-paragraph `summary` of what
    the project is, and suggest `focus_hints` (topics the KG
    extraction should prioritize). Tolerant parsing per Ken idiom
    (fences stripped, unknown fields ignored, bad suggestions
    dropped); LLM failure ⇒ deterministic profile stands.
  - `ProjectProfile { kind, summary, languages, excludes,
    chunking: Vec<PatternProfile>, focus_hints,
    #[serde(flatten)] extra }` saved to
    `.ken/index-profile.json` — atomic write, adopt-if-exists,
    **never overwrite a file the user has edited** (a `hand_edited`
    marker flips on when loaded content ≠ last generated hash).
- **Consumers**: `chunker.rs`'s `IndexProfile::default_for` is
  superseded by profile lookup when `.ken/index-profile.json` exists;
  `deterministic_profile` excludes merge into the project's effective
  exclusion set (additive to `ProjectConfig.excluded`, never
  replacing user entries); `knowledge_model.rs` appends `focus_hints`
  and `summary` to its extraction prompt when present.
- **When it runs**: (1) during workspace creation — the candidate
  checklist gains an "Analyzing…" phase, each candidate's row showing
  its detected kind/summary before confirm; (2) on single-project
  create with the flag on; (3) on demand via a "Re-analyze project"
  action in project settings. Tauri command `profile_project(id?)`
  with `profile-state` events (`scanning` / `refining` / `ready` /
  `error`). The LLM refinement is skippable (setting) and always
  Background priority.
- **Flag**: `profiler` (per-project). Off ⇒ `.ken/index-profile.json`
  is neither written nor read; `semantic-index` falls back to its
  extension defaults — exactly its standalone behavior.

## Capabilities

### New Capabilities
- `project-profiler`: stats scan, deterministic profile, LLM
  refinement, profile file, settings surface, and consumer wiring.

### Modified Capabilities
- `semantic-index`: chunking reads the stored profile when present.
- `knowledge-views`: extraction prompt gains summary/focus hints.

## Impact

- `crates/ken-core`: new `profiler.rs`; small hooks in `chunker.rs`,
  `project.rs` (effective excludes), `knowledge_model.rs` (prompt
  addendum).
- `src-tauri`: `profile_project` command, `profile-state` event,
  workspace-creation integration.
- Frontend: analyzing phase in the workspace checklist (kind badges,
  summaries); "Re-analyze" in settings; profile summary display.
- Tests: stats over a fixture tree (Rust + Node + docs mix);
  deterministic rules (kind, excludes by marker); refinement parse
  fixtures (fenced, junk suggestions dropped, failure fallback);
  hand-edit preservation; profile→chunker and profile→prompt wiring.

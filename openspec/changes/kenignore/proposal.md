# Proposal: kenignore

## Why

Today a file is either indexed or excluded (`project.json`
`excluded` + built-in defaults). That binary breaks down on real
projects: "similar to how gitignore works, there should be a ken
ignore... build files... proprietary stuff" — but also huge bodies
of decompiled/generated code where "it's good to access the srcs,
the codes, the role files, the model files" without letting them
flood the knowledge graph. A decompiled Hytale source tree is
exactly this: agents doing deep code dives need it searchable, but
ten thousand decompiled classes must not dominate entity
extraction, profiling, or federation.

So indexing becomes **three tiers**  (locked):

1. **full** — chunks + embeddings + FTS + knowledge model. The
   default for everything not matched below.
2. **search-only** — chunks + embeddings + FTS, but excluded from
   knowledge-model extraction, entity/event minting, profiler doc
   sampling, and federation. Findable, never load-bearing.
3. **ignore** — not indexed at all.

## What Changes

- **`.kenignore` file** at the project root, gitignore-flavored,
  last-match-wins:
  - plain gitignore patterns ⇒ **ignore**
  - `~`-prefixed patterns ⇒ **search-only**
  - `!`-prefixed patterns ⇒ negate back to **full**

  ```gitignore
  # build outputs: not worth indexing at all
  target/
  dist/
  # decompiled sources: searchable, never in the KG
  ~decompiled/
  ~**/*.generated.cs
  # except the hand-annotated entry points
  !decompiled/notes/**
  ```

- **Presence is the opt-in — no feature flag.** A `.kenignore` file
  is respected whenever it exists, like `.gitignore`. No file ⇒
  behavior identical to today. `project.json` `excluded` and the
  built-in defaults remain hard-ignores that no `!` can resurrect.
- **Tier plumbing**: chunks carry their tier; knowledge-model
  extraction, profiler doc sampling, and federation read only
  full-tier content. Search (FTS, KNN, hybrid, routing) reads full
  + search-only.
- **Profiler drafts it** (gated by `profiler`): the analysis phase
  classifies the tree (build outputs ⇒ ignore;
  generated/decompiled/vendored ⇒ `~`) and proposes a `.kenignore`.
  A new project gets it as a reviewable draft; an existing
  `.kenignore` is **never overwritten** — the profiler proposes a
  diff instead.
- **Built-in tier rules** ride the same engine: the workspace
  pseudo-member's rules (`ken-memory`) and per-repo `.ken/tasks/`
  search-only (`ken-tasks`) are expressed as baked-in rule sets
  evaluated before the user file.

## Capabilities

### New Capabilities
- `kenignore`: three-tier classification, `.kenignore` parsing and
  matching, tier-aware ingestion.

### Modified Capabilities
- `semantic-index`: chunks/embeddings written for full and
  search-only tiers; tier recorded per chunk.
- `knowledge-model`: extraction input restricted to full tier.
- `project-profiler`: doc sampling restricted to full tier;
  profiler additionally drafts/diffs `.kenignore`.
- `federated-kg`: federation reads full tier only (transitively via
  knowledge-model, stated for clarity).

## Impact

- `crates/ken-core`: new `kenignore.rs` — pattern parsing, tier
  matching (last-match-wins, dir semantics, `!` negation), built-in
  rule sets; pure and table-tested. Tier column on `chunks`
  (schema bump); ingest pipeline threads tier through; extraction
  and profiler input filters.
- `src-tauri`: `.kenignore` watched like any file — edits retrigger
  reclassification/reindex of affected paths; profiler draft/diff
  proposal flow.
- Frontend: profiler's proposed `.kenignore` (or diff) shown for
  review before write; tier badge in search results (subtle
  "search-only" tag).
- Tests: pattern table tests (each syntax form, ordering, negation,
  dir vs file); hard-ignore precedence; extraction excludes
  search-only; profiler never overwrites; no-file ⇒ byte-identical.

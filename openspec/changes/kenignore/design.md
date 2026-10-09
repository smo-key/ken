# Design: kenignore

## Context

Ingestion currently makes one binary call per path: built-in
defaults + `project.json` `excluded` ⇒ skip, otherwise index. Three
tiers slot in as a classification step in the same walk — every
path gets a `Tier` (`Full | SearchOnly | Ignore`) before anything
downstream sees it. Two other features already lean on this engine
for their built-in rules (ken-memory's pseudo-member tiers,
ken-tasks' per-repo `~.ken/tasks/`), so the matcher must be a
reusable core, not a one-off inside the walker.

## Goals / Non-Goals

- Goals: gitignore-familiar syntax with two extensions; pure,
  table-tested matcher; tier respected by every downstream consumer
  (extraction, profiler, federation, search); profiler drafts but
  never clobbers; zero behavior change when no `.kenignore` exists.
- Non-Goals: nested `.kenignore` files (root-only in v1 — one file
  per project is enough and keeps precedence trivially explainable);
  per-file tier overrides in the UI (edit the file); tier for
  non-file content (KG entities have no tier).

## Decisions

### D1. Syntax: gitignore plus two prefixes, last-match-wins

- Plain pattern ⇒ `Ignore`. `~pattern` ⇒ `SearchOnly`. `!pattern` ⇒
  `Full` (negation). Comments (`#`) and blank lines as in
  gitignore. Pattern semantics after the prefix are exactly
  gitignore: `dir/` matches directories, `**` globs, leading `/`
  anchors to project root.
- Last match wins across the whole file, like gitignore — no
  weighting by specificity. This keeps mental model transfer from
  git intact and makes the matcher a straight fold over rules.
- Escape hatch: a literal leading `~` or `!` in a filename is
  matched via `\~` / `\!` (rare enough to not optimize for).

Rejected: separate sections (`[ignore]` / `[search-only]`) — loses
gitignore muscle memory and interleaved ordering, which is exactly
what makes `~decompiled/` + `!decompiled/notes/**` natural.

### D2. Precedence: hard-ignore > built-ins > user file > default

Evaluation order for a path:

1. **Hard-ignores** — `project.json` `excluded` and the existing
   built-in skip list (`.git/`, `node_modules/`, the project's own
   `.ken/` DB internals, etc.). Non-negotiable: no `!` in
   `.kenignore` resurrects them. Checked first, short-circuits.
2. **Built-in tier rules** — feature-owned rule sets expressed in
   the same rule type: pseudo-member rules (ken-memory D3), per-repo
   `~.ken/tasks/` (ken-tasks D6). Evaluated as a prelude to the
   user file, so a user `.kenignore` line CAN override them (last
   match wins across the concatenation) — deliberate: if the user
   wants their tasks fully indexed, one `!` line does it.
3. **User `.kenignore`** — appended after built-ins.
4. **Default** — no rule matched ⇒ `Full`.

### D3. Tier lives on chunks; consumers filter, ingest classifies

Schema (the same v12 bump that adds `chunks`/`vec_chunks` in
semantic-index) gains `chunks.tier INTEGER NOT NULL DEFAULT 0`
(0 = full, 1 = search-only). Classification happens once, in the
ingest walk; downstream code never re-parses patterns:

- FTS + KNN + hybrid + routing query both tiers (no filter).
- Knowledge-model extraction selects full-tier only — search-only
  files never enter `EXTRACT_CHAR_BUDGET`, never mint entities or
  events.
- Profiler doc sampling selects full-tier only.
- Federation follows from extraction (nothing search-only exists in
  the KG to federate); stated in the spec anyway as a contract.

`Ignore`-tier paths simply never produce rows — identical to
today's exclusion path.

### D4. `.kenignore` edits retrigger scoped reindex

The file watcher already sees `.kenignore` change events. On
change: re-parse, diff old→new tier per known path, and enqueue
only transitions — `Full→SearchOnly` deletes the path's KM
contributions and flips chunk tiers, `SearchOnly→Full` re-runs
extraction eligibility, `*→Ignore` deletes rows, `Ignore→*`
ingests fresh. A malformed line is skipped with a warning event
(tolerant, like every other parser in this plan); the rest of the
file still applies.

### D5. Profiler drafts, never overwrites (gated by `profiler`)

The analysis phase classifies the tree with heuristics (build
outputs, `bin/obj/target/dist`, lockfiles ⇒ ignore; decompiled /
generated / vendored markers ⇒ `~`) and produces a proposed
`.kenignore` with a comment per section explaining why.

- No existing file ⇒ the draft is shown for review; approve writes
  it to the project root.
- Existing file ⇒ the profiler computes only *additions it would
  suggest* and presents a diff; approve appends under a
  `# proposed by ken profiler` marker. It never deletes or reorders
  user lines.

Respecting `.kenignore` is unconditional (no flag — file presence
is the opt-in); only this *generation* path is behind `profiler`.

### D6. Pure `kenignore.rs` core

`parse(text) -> Vec<Rule>` and
`classify(path, is_dir, rule_sets) -> Tier` are pure functions in
`crates/ken-core/src/kenignore.rs`, with rule sets as data so
built-ins and the user file share one code path. Table tests are
the spec: every syntax form, anchoring, `**`, dir-vs-file,
ordering/negation chains, escapes, malformed lines, hard-ignore
short-circuit. Candidate base: the `ignore` crate's gitignore
matcher for pattern semantics, wrapped for the tier fold — decide
in implementation, the public surface above is the contract.

## Risks / Trade-offs

- **Tier flips on big trees are expensive** (`~decompiled/` removed
  ⇒ full extraction eligibility for 10k files). Acceptable: it
  rides the existing queue/debounce/cancel machinery, and flips are
  rare, deliberate acts.
- **Divergence from real gitignore semantics** in edge cases if we
  hand-roll matching — mitigated by reusing a proven matcher crate
  and by the table tests encoding the cases we actually rely on.
- **User confusion: `!` vs `~` on the same path** — last-match-wins
  answers it mechanically; the profiler's generated comments model
  good style.

## Migration

None. No `.kenignore` ⇒ classification returns `Full` for
everything not hard-ignored — byte-identical behavior and DB
content to today. The tier column defaults to full for any
pre-existing chunks.

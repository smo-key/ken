# Knowledge core: decisions

The plan of record for Ken's knowledge layer, from the feature-by-feature review of the `knowledge-layer` branch on 2026-10-09. This branch, `knowledge-core`, starts from upstream `main` and rebuilds the knowledge layer to these decisions. `knowledge-layer` is kept as the archive to port from and is never merged.

The method these decisions implement is the Knowledge, Drift and Decisions pages of Ways of Working (`docs/manual/docs-system.html`, `drift.html` and `decisions.html`, branch `docs/knowledge-core`). Those pages state the method; this file and the code hold the numbers, weights and tool names.

## Why a new branch

`knowledge-layer` added about 108k lines against upstream `main`. Only about 31k were knowledge-layer Rust. The rest was:

- `ken-workspace-home` carried in (about 65k, before the knowledge-layer plan started on 2026-09-24);
- other Ken features (Your day, families, memory, a UI restyle);
- 11k of OpenSpec proposals;
- 5.6k of design mockups;
- generated files and a lockfile switch.

Inside the knowledge layer itself:

- **The first wiki draft** grew to about 7.6k lines (`wikidraft.rs`, `wikiverify.rs`, `checkout.rs`), with 82 one-off patches, trying to make the wiki a verified copy of the code.
- **Ranking** gained about ten nudges tuned to one project's questions.
- **People logic** spread across about 30 files.
- **The code map** was built but never reached by a plain code question.
- **Supersession** only ran in the eval harness.

## The goal

A Karpathy LLM wiki for a project:

- sources, the library and the brief;
- the three operations: ingest, query and lint;
- an index and a log.

The code is the truth for how the product works today; the library complements it and never copies it.

On top of that base:

- local hybrid search over every source;
- a priority order so binding knowledge comes first;
- a code index so code is reached without grep;
- drift checks that run on every commit;
- the decisions log and the tickets as core project knowledge.

## Decisions

### Schema and layout

1. **Brief: one schema, three places.**
   - **Wiki brief.** The wiki `CLAUDE.md` is rewritten as the schema: layout; how to ingest, answer and check; and "the code is the truth, the library complements it".
   - **MCP instructions.** The same short form is sent as the MCP server's `instructions`, so any Claude session gets it.
   - **Ken's chat guide** keeps only app rules (edits shown as diffs, clickable citations, tasks, memory).
   - **Code repos.** Ken proposes a short pointer brief (where the knowledge base is, which tools to use) when a code repo is added; a person adds it; Ken never overwrites it.
   - **Rules removed:** "search three ways at once", "a ruling waits for its decider", "a page is done when something links to it", and the Wright lines (escalate, tickets).
2. **Entry point and indexes: kept current, plus a log.**
   - START-HERE and one index per section.
   - Every page Ken writes is added to its section index, with one line, in the same change.
   - `log.md` gets one dated line per ingest, per lint pass when it acts (files or resolves a finding), and per drift run only when it changed a page: `## [YYYY-MM-DD] kind | title`.
3. **Sections:**
   - Rules: binding, incl. team conventions, never restated from lint config.
   - Decisions: `Decisions/DECISIONS.md`.
   - Design.
   - Current: Project, Roadmap, Feature Status, and `RAID.md`, the risks and issues (decision 37).
   - Reference: Vocabulary, how-tos, where things live, plus `Platform/` with one page per upstream dependency (pin, quirks, where its source lives).
   - Research: dated findings, plus `Ingestion/Raw` and `Ingested`.
   - Templates.
   - **Gone:** the method pages (Lifecycle etc.), Work, and the pages that restated code (Code, Data-and-Config, Testing, Registries, Systems, Config-Map, Build-and-Run).
4. **Templates: Ken owns them, content only.**
   - Ken's repo is the one template source; Ways of Working describes the shape and links here.
   - Ship only files with content, plus the blanks in `Templates/`. No page that only holds placeholders. A page is created from its blank when something writes it.
   - Re-running setup reports missing or out-of-date files as a diff and never overwrites.
   - The tickets folder and the ticket template stay.
5. **Frontmatter: seven keys.**
   - `title`, `aliases`;
   - `status`: draft | current | superseded;
   - `superseded_by`: a retired page's successor, written by Ken when it retires the page;
   - `updated`;
   - `sources`: each `repo@sha:path` or `[[Note]]`, with the pin inside;
   - `checked`: date and sha, written by the check that re-read the page;
   - optional `reviewed_by`.
   - **Dropped:** `lens`, `changed_by`, `audience`, a separate `pin`, person-only `verified`.
6. **Current:** Project, Roadmap and Feature Status, kept current by ingest and drift. Team and Who-Does-What are gone.
7. **Retired, generated and moved pages:**
   - Retired pages rank last and name their successor in `superseded_by`, which search shows on the hit. A retirement is written at once, like a plain fact update; a Rules page or a page with a person's uncommitted work is held for a person.
   - The retire, harvest and generated-page rules go in the brief.
   - When Ken moves or renames a page it rewrites every link where the target is certain, reports the rest, and writes a `log.md` line.
8. **One knowledge-base repo:**
   - It holds the library, `Decisions/`, `tickets/` (indexed: tickets are core knowledge, the work in flight and done) and `.ken/knowledge.json`.
   - **No team repo.** A team that keeps tickets elsewhere can still point Ken at that repo.
   - **Ticket frontmatter for the knowledge layer:** id, status, type, assignee. Size, scope, verify and workflow belong to Wright.

### Ingest

9. **Inbox → note, automatic.**
   - Keep: the kind folders, transcription, failure handling, the 400k-character cap.
   - The "what the wiki says now" context comes from the real ranker on the source's title and key terms, not a word count.
   - Each ingest updates the section indexes and `log.md`.
10. **Contradictions.**
    - A source is checked against the wiki, the decisions log, the tickets, and the code (through search and the code index).
    - Each contradiction becomes a tracked finding.
    - Plain fact updates to pages apply automatically. A contradiction with a ruling or with the code always waits for a person.
11. **Page edits.**
    - **Flow:** plan, then edit, then apply or hold.
    - **Held for a person:** an edit changing more than a fifth of a page, a page changed during the run, a Rules page, and anything contradicting a ruling or code.
    - **Edits are section replacements, not whole pages.**
    - **New pages** go into their section index and `log.md`.
    - **The new-page prompt belongs to ingest.**
    - **Cap:** about 10 updates and 10 new pages per source.
12. **Git is the undo.**
    - No per-write undo records.
    - **One commit per ingest** in the knowledge base: the note and every page it changed. The message names the note; the body lists pages changed, held edits, contradictions and open questions. Ken never pushes outside sync, which pulls and pushes the knowledge base.
    - **The card** shows takeaways, contradictions, open questions and held edits (Apply or Discard).
    - **A source is claimed when its note is written:** it moves beside its note, so two machines never ingest it twice.
13. **Rulings.**
    - A ruling found in a note waits as a ready entry; one click adds it. There's no decider check, and "who" is optional free text.
    - **One entry format** for the template, Ken's writer and every parser: `## D-nnn · date · topic`, the bold ruling, the quote, Why, sources, aliases, supersedes. Ken fills topic, why and aliases properly.
    - **Hand-kept sections dropped:** Topic Index, Conflicts, Open Questions.
    - **Rulings can also be proposed from chat.**
14. **Note headings and where they go.**
    - **Headings:** Summary, Key Takeaways, What It Overturns, Contradictions, What Was Said, Rulings, Actions, Open Questions.
    - **Where each goes:**
      - Rulings → the log, confirmed (13);
      - Actions → proposed tickets, confirmed;
      - Risks and issues the plan finds → proposed entries for `Current/RAID.md`, confirmed (37);
      - Open Questions and Contradictions → findings; an open question left unsettled can be raised as an issue;
      - Overturns → page edits.
    - **Cut:** idea files, escalation files, Your day tasks from ingest, method-change tickets, "me" matching.

### Query and search

15. **One index, one ranker.**
    - Keep the chunk index: FTS5 text and names, sqlite-vec, local nomic embeddings, keyword-first merge, rerank.
    - Retire the file-level `search` index and its separate ranker. The app, chat and every MCP tool use one pipeline.
    - While vectors build, results say how far meaning search has got.
16. **Chunking.**
    - Prose by heading, and logs one entry per chunk, unchanged.
    - **Code by definition**, from the code index's parse, with the enclosing symbol and line range on each chunk; line blocks only where there's no grammar.
    - The per-file chunk cap is reported in index health.
    - **Every prose chunk carries its heading breadcrumb** (page title › headings), embedded and in a new title field weighted about 2–3× the body.
    - **Code chunks carry** path, symbol, doc comment and role.
    - **One-line summaries** for wiki pages, notes, decisions and tickets only, cached by chunk hash, used for retrieval only.
17. **Kinds and roles.**
    - Add the `decision` and `note` kinds.
    - **Tests are not indexed by default;** a team can add them back.
    - **A code file's kind is its role** (service, controller, repository…): a small built-in core plus per-repo role rules proposed by a repo analysis when the repo is added. The proposal waits as an Inbox card showing each rule; nothing applies until a person accepts it, which writes the rules into `.ken/knowledge.json` (one commit naming the file), and Dismiss drops them. Once there they are editable and applied without a model at index time.
    - **One label per hit:** kind · tier or role · date.
18. **Vocabulary.**
    - Query rewrites come from the Vocabulary page and the decisions log's aliases only.
    - Page titles and aliases feed the names field, not rewrites.
    - Ingest and drift add rename rows.
19. **Controls and health.**
    - A few known questions per source, each with the file it must return near the top, run through the real pipeline on every rebuild, kept in `.ken/knowledge.json`. Proposed at setup on the repo analysis's Inbox card, and kept only once a person accepts them.
    - **One health line:** indexed, failed or skipped; meaning built; controls; model missing.
    - Retire the title-query control on the old index.
20. **Routing.**
    - Every question searches every source, merged once.
    - A named repo narrows the search.
    - Reference repos join only for platform questions, when named, or when nothing else answers. Only hits found by the question's words count as answering: a source with meaning-only hits has not answered, so the reference repos join.
    - **Dropped:** the graph-guided planner, the 5-repo cap, the band tie-break, the library-share rule.
    - A slow or building repo is reported, never silently dropped.
21. **The project graph (kept, rebuilt).**
    - **Nodes:** pages, decisions, tickets, notes, repos, code files and symbols, topics.
    - **Edges from exact sources:**
      - page links;
      - `sources:` citations;
      - note → page changed;
      - supersedes;
      - imports and usages;
      - code comments citing a ruling;
      - commits citing tickets or rulings.
    - **Claude adds topic tags at write time only.** No background local-model extraction, no federation merge, no person-centred entities.
    - **Exposed as `related`.** There's no Map view.
22. **MCP tools:** `search`, `read`, `related`, `code_definition`, `code_usages`, `code_outline`, `code_imports`, `history`, `tickets`, `propose_decision`, `add_source`, `repos`, plus server `instructions`. Memory, task and family tools leave the knowledge server.
    - **Scope:** an agent connected from Settings › Agents gets the whole workspace, its knowledge base and every source: `ken-mcp --workspace <dir>` serves that workspace whatever Ken opens later, and reads a bare path in the knowledge base. Without it the server follows the workspace Ken last had open; Ken's own chat keeps `--project` for the repo it runs in (Chris, 2026-10-15).

### Priority

23. **Five tiers, shown on every hit:**
    - binding +0.75: live rulings, Rules;
    - current +0.4: Current and Design pages with `status: current`;
    - reference 0: other pages, code;
    - record −0.5: tickets open and done, notes, Research, drafts;
    - superseded −1.25: superseded rulings, retired pages, cancelled tickets.

    Teams override by path. The old band weighting is removed.
24. **Score = base plus tier only.**
    - Base: word matches (title field weighted, path, text) × coverage, plus 1.5 × meaning, plus 0.75 when keyword and meaning both found it.
    - **Removed:** the symbol, link, intent, staleness and reference-data nudges.
    - Minified text and `Templates/` are excluded at index time.
25. **Supersession, wired in.**
    - Markers are read on every scan.
    - The Claude pass runs on new entries only.
    - Each pair it finds is a finding; when a person confirms, Ken writes `supersedes:` and `[SUPERSEDED BY]` into the log.

### Code

26. **Code index** (renamed from "code map").
    - Every code hit names its symbol and role and offers the next calls (usages, outline, imports).
    - For code questions, plain words also match symbol names ("save world" → `saveWorld`, `save_world`).
    - The tool text and server instructions say to use it before grep.
    - Ken's chat card shows the symbol.
27. **Git history.** Keep the live tool. Commits that cite tickets or rulings become graph edges. Drift and history share one git helper.

### Lint and drift

28. **Drift on every commit.**
    - **When it runs:** when Ken sees a source's default branch move, for that commit range. A weekly age rule covers pages whose sources don't move.
    - **Whitespace or comments:** the `checked` stamp advances silently.
    - **A real change:** Claude re-reads the page section against the diff, and either stamps it or fixes it through the ingest edit rules.
    - **A rename:** followed, and the citation rewritten when certain.
    - **A ruling whose code changed is never edited.** It becomes a finding.
    - **After a run that changed a page:** one commit and a `log.md` line.
    - **One config:** the audience split and the unused second config are gone. Optional moved/clean controls stay.
29. **Lint.**
    - **On every scan:** broken links, ambiguous names, orphans, missing pages.
    - **After each edit:** Claude compares the touched pages and their graph neighbours for contradictions.
    - **Weekly:** binding and current pages, plus missing-page suggestions.
    - Everything goes to one findings list.
30. **Rulings cited in code.** These become graph edges. A cited id the log doesn't have is a finding. The generated Cited-in-Code page goes.
31. **The scope and answer checks** stay with Wright.

### Setup and building the library

32. **Setup flow:**
    1. Workspace name.
    2. Where to save the knowledge base: a new repo, or an existing wiki to overlay.
    3. Sources: repos and folders, with connectors to be decided later.

    Setup lays down the structure and indexes the sources, so search and the code index work from day one. The Files tab splits into Wiki and Sources.
33. **No one-shot draft.** `wikidraft.rs`, `wikiverify.rs` and `checkout.rs` are not carried over.
    - **A build queue of sources**, the heavy hitters first, ingested one at a time (or a few once trusted).
    - **Two passes:** a high-level pass over each source for the broad strokes, then file by file from broad to fine, through ordinary ingest.
    - **Throttled and visible.**
    - **An existing wiki** is mapped into the sections after a dry run; nothing is deleted.
    - **Answers worth keeping** are filed back as pages.
34. **Index rules and pace.**
    - **Two states:** a path is indexed or it isn't.
    - **Rules live in one place,** `.ken/knowledge.json`, plus fixed built-ins: VCS, dependencies, build output, secrets, tests, minified and generated files, binaries.
    - **Re-reads** happen on a content-hash change; a file that fails three times is counted as failed.
    - **One throttle for all background work** (Gentle / Normal / Fast, Pause) and one status panel: running, queued, progress, failed with reasons. A person's own ingest (a source they drop or add) is paced and paused with the rest but is outside the hourly budget; the build queue, summaries, re-reads, supersession and repo analysis keep it.
35. **Team cut from the knowledge layer:** no team repo, roster, owners, deciders or team bus. `team.json` becomes `.ken/knowledge.json`.

### The rest of Ken on this branch

36. **Ken's commits:** only in the knowledge base, only its own changes. Never `git add -A`, never in a code repo. A person's other changes in the knowledge base are committed only when they ask, through sync's "commit my other changes", which lists each file first.
37. **Feature by feature:**
    - **OpenSpec is removed entirely.**
    - **Your day and tasks stay.**
    - **Families and the team bus go.**
    - **The Team screen** becomes the workspace settings screen (sources, kinds, index rules, tiers, roles, controls, health).
    - **Memory stays,** personal and local, never pushed. Distilling the journal into proposed memories is one Claude call, a turn of the background throttle counted against the hour like the summaries; the local model only embeds (Chris, 2026-10-15).
    - **Explore and the Map go.** The Timeline becomes the Log: the project's RAID register (risks, actions, issues, decisions) and milestones, not a feed of every update (Chris, 2026-10-14).
      - Risks and issues live in `Current/RAID.md`, one entry each (`## R-001 · date · title`, then `status:`, `owner:`, `sources:`); ingest proposes them, and an open question can be raised as an issue.
      - Actions are the open tickets with an assignee; decisions are the decisions log's rulings, a superseded or refined one folded; milestones are the tickets marked `type: milestone`, the roadmap's rows and releases (tags).
      - Each entry shows its status, owner and date and opens its source; closed items fold away.
      - Ingests, drift, lint, findings, page changes, sync and commits are not on it; `log.md` keeps its line per run (decision 2).
38. **Evaluation:** every eval set up on `knowledge-layer` is dropped (`eval_run`, the probes and benches, the `KEN_EVAL_*` hooks). Measurement is to be designed properly later. The controls in decision 19 remain the health check.

## Still open

- **Connectors as sources:** which ones, and when.
- **The exact build-queue order.**
- **Measurement design** to replace the dropped evals.

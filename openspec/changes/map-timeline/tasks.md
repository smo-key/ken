# Tasks: map-timeline

## 1. ken-core

- [x] 1.1 `db.rs`: bump SCHEMA_VERSION to 6; migration adds `entities`, `entity_edges` (both ends `REFERENCES entities(id) ON DELETE CASCADE`), and `events`; row types `EntityRow` (sources as JSON array), `EdgeRow`, `EventRow`; CRUD `replace_knowledge_model` (single transaction, full replace, stamps `knowledge_model_built_at` in `meta`), `list_entities_with_edges`, `list_events` (newest first), `knowledge_model_built_at`
- [x] 1.2 db tests: `v5_db_migrates_to_v6` (tables usable, earlier data survives), replace is atomic + idempotent (run twice, exact model, no orphan edges), empty model reads as empty/None
- [x] 1.3 `knowledge_model.rs`: `compose_extraction_prompt(files, today)` — read-the-material instruction, exact JSON shape, ~40/~60 caps, groundedness + omit-undated rules, file list capped at 200 with "…and N more"; register module in `lib.rs`
- [x] 1.4 `knowledge_model.rs`: `parse_extraction(raw)` — first-`{`/last-`}` slice (fences/prose stripped), unknown fields ignored, malformed records dropped, kinds coerced to `other`, dates validated `yyyy-mm-dd`, categories one lowercase word, caps enforced, connections resolved case-insensitively (dangling/self dropped, duplicate pairs collapsed); no JSON object at all → error
- [x] 1.5 `knowledge_model.rs`: `build_knowledge_model(binary, project, db, today, cancel)` — compose from indexed files → `assistant::oneshot` (10 min) → parse → `replace_knowledge_model` → counts; cancel/timeout/failed map to plain-language errors; failure leaves the stored model untouched
- [x] 1.6 knowledge_model tests: compose contract + cap, parse fixtures (fenced, unknown fields, dangling refs, case-insensitive resolution, bad dates, no-JSON error, caps), end-to-end build with the fake claude via `oneshot_result` (stores model, counts match) and failed-build-keeps-old-model

## 2. src-tauri

- [x] 2.1 `ActiveProject.knowledge_running: Arc<AtomicBool>`; `refresh_knowledge_model` — missing Claude → guided error, already-building → friendly error, else worker thread emitting `knowledge-model-state` `building` → `ready`/`error {detail}`
- [x] 2.2 `knowledge_model()` command returning `{entities, edges, events, builtAt}` (camelCase, `builtAt` null before the first build); register both commands

## 3. Frontend

- [x] 3.1 `api.ts`: `EntityRow`, `EntityEdge`, `EventRow`, `KnowledgeModel`, `KnowledgeModelState` types; `knowledgeModel`, `refreshKnowledgeModel` wrappers; `onKnowledgeModelState` listener
- [x] 3.2 `src/lib/knowledge.ts` (pure helpers): `stableHash` (FNV-1a), `layoutMap` (deterministic center/inner/outer/periphery ellipse layout, hash-seeded angles, clamped periphery), `highlightMatches` (escape-then-mark); vitest coverage in `knowledge.test.ts` (determinism, primary centering, periphery band, HTML escaping, case-insensitive marks)
- [x] 3.3 `src/lib/knowledge.svelte.ts` store: model/building/error/claudeFound, `visit()` (init once + load), reload on `ready` event, `refresh()`
- [x] 3.4 `MapScreen.svelte`: header (serif Map, built-at caption, Refresh map btn with building state), full-bleed `--surface` canvas, SVG edges (1.5px `--border-strong`, non-scaling stroke), pill nodes (primary ink-solid, surface+border, dashed amber degree-0, mono for `decision`), click → first source in Files, hover raise + summary title, bottom-left legend, empty-state card + claude-missing note
- [x] 3.5 `TimelineScreen.svelte`: header (serif Timeline, "View as of… ◷" chip + date input), search field with match count + amber highlight, data-derived category chips (All ink-solid, max 6 by count), vertical timeline newest-first (accent-ringed newest dot, date overline, category mini-pill, sentence, mono source chip → Files), same empty/refresh/building states as Map

## 4. Verification

- [x] 4.1 cargo test --workspace, pnpm test, pnpm check, pnpm build all green; openspec validate map-timeline
- [x] 4.2 Commit

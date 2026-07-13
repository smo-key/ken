# Tasks: deep-research

## 1. ken-core

- [x] 1.1 `runner.rs::test_support`: fake claude's TUI `complete` branch also parses `OUTPUT_FILE=<path>` from the prompt and writes a small fake report there before emitting Stop; all existing behaviors unchanged
- [x] 1.2 `research.rs`: `slugify` (kebab, ~50-char cap), `unique_report_path` (numeric-suffix collision handling), `plan_report`, `validate_output_dir` (inside project, not `.ken`), `DEFAULT_TIMEOUT` (30 min); register module in `lib.rs`
- [x] 1.3 `research.rs`: `compose_research_prompt(question, output_rel_path, output_abs)` — role, METHOD (angles, multiple searches, cross-check across ≥2 independent sources, honest disagreements), OUTPUT (one complete report: title, date, 3–5 sentence executive summary, findings by theme, "What remains uncertain", Sources with one-line notes + inline `[1]` markers), RULES (primary sources; write the file even if the web is unavailable; the report is mandatory; prefer stated assumptions over questions), `OUTPUT_FILE=` line
- [x] 1.4 `research.rs`: `run_research(project, binary, session_id, question, report_rel_path, hooks, timeout, cancel, on_blocked)` — install_hooks + `run_session` ALWAYS HiddenTui; Completed verifies the report exists (else a failure that says so)
- [x] 1.5 research tests against the fake claude: slugify incl. collisions, `validate_output_dir` rejections, prompt contract assertions, end-to-end complete (Completed + file), fail (error, no file), blocked → on_blocked fires → cancel → Cancelled

## 2. src-tauri

- [x] 2.1 `ActiveProject.research: Arc<Mutex<HashMap<String, CancelToken>>>`; `start_research(question, outputDir) -> chatId` — validate, plan path, upsert `research` chat row (`working`) + `chat-updated`, activity message naming the report path, worker thread mapping blocked → `needs_input` and completion → `done`/`error` + outcome activity message, token removed at the end
- [x] 2.2 `cancel_research(chatId)` and `research_output_options()` (`research` first, then existing top-level folders, excluded/junk/hidden omitted); register all three commands
- [x] 2.3 `send_chat_message` rejects `research` chats like ingests; `enter_terminal_mode` resumes research chats like ingests

## 3. Frontend

- [x] 3.1 `api.ts`: ChatRow kind gains `research`; `startResearch`, `cancelResearch`, `researchOutputOptions` wrappers
- [x] 3.2 `chats.svelte.ts`: `select()` auto-enters terminal for kind `research` like `ingest`
- [x] 3.3 `ChatDrawer.svelte`: research foot note ("Research session — opens in the terminal; you can answer its questions there."), Cancel mini action while the active research chat is `working`/`needs_input` (incl. in terminal mode)
- [x] 3.4 `src/research/ResearchModal.svelte`: question textarea, location select from `research_output_options()` seeding an editable path field, plain-language note, primary Start research → close, open drawer, select chat
- [x] 3.5 `HomeScreen.svelte`: "Start research" button in the Your-knowledge action row opening the modal

## 4. Verification

- [x] 4.1 cargo test --workspace, pnpm test, pnpm check, pnpm build all green; openspec validate deep-research
- [x] 4.2 Commit

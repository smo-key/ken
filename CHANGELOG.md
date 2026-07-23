# Changelog

## 0.1.0

### Bug Fixes

- Drop --offline from lockfile version sync
- Attach hydration-progress listener before the cloud download starts
- Whisper.cpp needs macOS 10.15+ — set bundle minimumSystemVersion
- Repair quick-answer prompt test broken by signature change
- Restore Metal GPU inference — enable whisper-rs metal feature
- Non-blocking reindex + true from-scratch wipe; opt-in video transcription
- Rename-popover spacebar, disable native menus, enable in-webview drag-drop
- Don't auto-download cloud files over 50MB
- Treat .vtt transcripts as editable text, not binary
- Stop spacebar from closing the rename popover
- Gate recording on transcription model readiness
- Unstick extraction queue, fix 0-of-N coverage, make search non-blocking
- Bake /usr/lib/swift rpath into ken-app and ken-mcp binaries
- Record honors selected transcription model; validate settings URL
- Backfill the extraction queue for projects indexed before the Map
- Trigger automations on file changes; make engine cancel kind-aware
- Size local-LLM batch to prompt; clamp to context; requeue errored extractions
- Freeze clock and hide live controls during transcription
- Upgrade whisper-rs 0.14→0.16 to end duplicate-ggml SIGSEGV on LLM load
- Cancel in-flight run in shutdown drain; no panic on automation dispatch
- 5s SQLite busy_timeout on writable connections; reuse the worker's Db across files
- Optimistic echo, focus-on-open, autoscroll + audit fixes (§9)
- Size-gate office/notebook previews before parsing (§11)

### Build

- Align windows-core with windows 0.61 for cpal
- Pick-first ggml linking on ELF/MSVC; use Ninja for Windows cmake
- Target-gate ggml backends — metal on macOS, vulkan on Windows

### CI

- Drop temporary branch trigger now that macOS CI is green
- MacOS legs need the macOS 26 SDK for apple-metal
- Build-macos needs macos-15 — apple-metal Swift bridge uses macOS 15 SDK
- Add macos-14 (Apple Silicon) build job
- Drop temporary branch trigger now that Windows CI is green
- Run Windows cargo steps under pwsh, not Git Bash
- Build sidecar before tests; install SPIRV-Headers for ggml vulkan
- Temporarily trigger on worktree-windows-x64-build pushes
- Add windows-latest build job (vulkan) and fix Ubuntu ALSA dep

### Chores

- Regenerate tauri ACL schemas for updater/process plugins
- Screencapturekit compile-probe (macos)

### Documentation

- Implementation plan for inline progress bars
- Windows x64 build implementation plan
- Windows x64 build support design
- Implementation plan for inline progress bars
- Auto-update implementation plan
- Design — inline progress bars for hydration downloads and transcription
- Auto-update design spec
- Phase 1 design — quick-answer latency + lexical/recency ranking

### Features

- Percent bar during post-stop transcription
- Live progress bars for video transcription and cloud downloads
- Emit transcript-progress from video and recording paths
- Emit hydration-progress from on-demand and background hydration
- Phase/percent progress callback through the pipeline
- Hydration progress sampling in the poll loop
- Render layouts/masters, inherit text styles, bundle metric fonts
- Bundle ken-mcp sidecar and refresh ~/.local/bin on launch
- Signed updater artifacts + latest.json in release pipeline
- Title-bar update chip (downloading / restart)
- Wire tauri updater + process plugins and svelte store
- Updater state-machine controller
- Phases 2-3 — index image/PDF text and Cmd+F highlight overlay
- Phase 1 — native Apple Vision OCR bridge for images and PDFs
- Delete-to-Trash, always-visible unread switch, retroactive .vtt reclassification
- Worker-backed non-freezing render, 50MB cap, higher fidelity
- Quick-answer thinking state + short-query guard
- Surface failed extraction count instead of a silent stall
- Nav entry and lazy pane
- Record screen and components
- Frontend api bindings
- Tauri session, commands, and events
- NSMicrophoneUsageDescription plist
- Microphone + screen-recording permission probes (macos)
- Screencapturekit system-audio capture (macos)
- Cpal microphone capture backend (macos)
- Cpal input-device enumeration (macos)
- Capture-source seam and ingest pipeline
- Assemble transcript document
- Two-channel labeled transcript merge
- Recorder state machine with pause accounting
- Recording filenames and metadata header
- 16k mono WAV read/write
- Stateful linear resampler
- Mono downmix
- Rms meter math
- Scaffold ken-core record module
- Approve/discard automation proposals from Review
- Surface automation proposals in the Review inbox
- Ingests screen — Knowledge docs / Automations tabs
- AutomationsPane with live activity
- AutomationForm
- Automations store
- Automation CRUD/run + proposal approve/discard commands
- Approve/discard automation proposals (queue phase-2)
- Automation dispatch with two-phase proposal/apply gating
- Proposal / apply / direct prompt composition
- Model, .ken/automations persistence, glob triggers
- Tested glob matcher (* / ** / ?)
- Live activity line, elapsed timer, and queued countdown on Ingests
- Ingests store routes by kind and surfaces live activity
- Pure live-run caption + countdown helpers
- Live-event fields + automation types/wrappers
- Unified job queue with queued/waiting visibility events
- Stream live activity + elapsed on running ingest events
- Streaming headless ingest sessions with a live activity callback
- Record + emit a no-op "checked, nothing to update" run
- Debounce default 30s → 10s
- Add kind discriminator to ingest_runs (schema v9)
- Coverage line, paused notice, throttled refresh, Deep rebuild button
- Retire auto-build tick, run per-project extraction worker, expose coverage+llm status
- Enqueue changed indexed files for extraction
- Extraction worker core (extract_one, process_next_pending)
- Purge knowledge + extraction rows on file removal/clear
- Merge_knowledge_delta + purge_file_knowledge (dedup/GC heart)
- Parse_delta_value with per-file caps and relation resolution
- Per-file caps, content hash, single-file prompt
- Extractions queue table + methods (migration v8)
- Inline rename/create in the tree + folder drag-and-drop (§12)
- Prefix-aware tab/favorite renames for folder moves (§12)
- Inline-edit naming policy + folder drag subtree guard (§12)
- Create_folder/create_document commands; move_file accepts directories (§12)
- Pure numbered-name dedup + folder-subtree move guard (§12)
- Rhythm, segmented Appearance, offline-models card, tri-state folders (§10)
- Pure watched-folders tri-state tree logic (§10)
- Catalog-backed Tauri commands + selection command (§10)
- Use the selected transcription model; lock chat persistence (§9/§10)
- Curated category/tier catalog + persisted selection; retire discovery (§10)
- Pure optimistic-echo reconciliation for the transcript (§9)
- Pin Files header, move Import + filter into it (§3)
- Shared import store for tree header + dialog host (§3)
- Pure predicate for the tree-header filter visibility (§3)
- Pure per-format preview size gate (§11)
- Several features

### Other

- In-progress extract/import/home-status changes (committed to unblock merge)
- Bundle_test dispatch must never fire a real release
- Port gated release pipeline from agent-desktop
- Sync SPIRV-Headers install into Windows leg
- Install Vulkan SDK on Windows leg, add Linux ALSA dep
- Live-stream quick answers + not-installed hint
- StripStreamingBody helper for live quick-answer streaming
- Stream on the local model with supersede-cancel + Claude fallback; add llm_status
- Recoverable engine builds — retry, non-sticky NotInstalled, notify_model_installed
- Global wiring, real LlamaEngine (metal), llm_status, init
- Fill language_catalog seam with Qwen3 4B/8B entries
- LlmService queue — priority, cancel, JSON retry, yield flag (fake engine)
- Pure helpers — Engine trait, ChatML, UTF-8 streamer, lenient JSON
- Cargo feature + llama-cpp-2 API compile-probe
- Normalize each custGeom contour into the first path's coord space
- Render custGeom SVG, tables, rot/flip; load deck theme once
- Concatenate all custGeom contours into one path; fix arcTo comment
- Render p:graphicFrame tables (rows/cells/spans/fills)
- Parse a:custGeom into scaled SVG paths with resolved fill/outline
- Thread group transforms + rot/flip through the walk
- Resolve theme colors (schemeClr + clrMap + transforms)
- Ui-fixes — append §12 file-operations tasks 13-17
- Wave 4 — §12 files-tree file operations (new folder/document, rename, folder DnD)
- Wave 4 — six track implementation plans (ui-fixes, local-llm, map, ingests-automations, record, pptx)
- Wave 4 additions — settings simplification (impeccable review), large-file preview hang
- Wave 4 design — local LLM, incremental Map, Record, automations, ingest visibility, pptx, chat UX
- Knowledge model (DB v6 entities/edges/events + extraction) and the Map & Timeline screens
- Web research mode — ken-core research.rs (slugify/plan/validate, harness prompt, always-interactive run_research with report verification), Tauri start/cancel/output-options commands + research chat rows, drawer terminal integration with Cancel, Home Start-research modal
- Daily digest + ⌘K quick answer — ken-core assistant.rs oneshot runner + digest.rs (gather/compose/parse) + DB v5 digests, Tauri digest scheduling (activate/focus/refresh) + quick_answer command, Home digest card (share, source chips, honest states), SearchOverlay quick answer card + ⌘↵ continue in chat
- Ken-mcp stdio server over the read-only index + Settings card
- Git sync + conflict review — ken-core sync.rs (pull/push/excludes, conflicted-copy detection, AI merge drafts, SyncEngine), Tauri sync commands + focus pull + sync-state events, Review conflict detail, Settings sync card, title-bar dot
- One-line installer, release pipeline, CI
- Unified Review inbox — DB v4 review_items substrate, review_inbox command, screen + nav badge + Home wiring
- Live real-Claude conversation + resume + terminal test, README
- DB v3 chats, ChatEngine (stream-json), terminal attach + PTY registry, Tauri chat layer, drawer UI
- Live real-Claude end-to-end test, README status
- Tauri commands/events, Ingests screen, form, template gallery, Home/Settings wiring
- Ken-core recipes, hooks, runner (hidden-TUI/headless), refresh engine, ingest queue
- Fix watcher drop deadlock + macOS path canonicalization, drag-region permission, README
- Ken-core (project/registry/db/extract/scan/watch), Tauri commands, Svelte shell + files/search UI

### Performance

- Faster quick answer + smarter ranking (phase 1)
- ~2x faster background extraction, quick-answer untouched

### Refactor

- Shared ProgressBar; PreviewLoading learns determinate progress
- IngestEvent gains kind + live-activity fields

### Testing

- Full-suite green + spec self-review
- Waiting event when the single worker is busy
- Cover nonzero exit without a stream-json result event
- Fake claude streams stream-json for headless ingest runs

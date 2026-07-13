# Ken

A desktop app that turns an ordinary folder into a team knowledge base.
Point Ken at your folder — notes, Word docs, spreadsheets, PDFs — and it
indexes everything, watches for changes, and makes every fact searchable.
AI-maintained structured documents, Claude chats, an MCP server for your
agents, and Git/shared-drive conflict review arrive in upcoming releases
(see `openspec/` and `docs/superpowers/specs/` for the roadmap).

## Status

- **Walking skeleton** — project create/open, live indexing (md/txt/code,
  docx, xlsx, pptx, pdf, images), ⌘K full-text search, WYSIWYG markdown
  editing, in-app preview of PDF/Word/Excel/images.
- **AI ingests** — recipes that keep structured documents (People,
  Decisions, Requirements…) fresh as files change, run through your local
  Claude Code CLI with review rules: human edits win, big refreshes wait
  for approval. Template library included.
- **Chat drawer** — talk to your project's knowledge: friendly chat by
  default, the real Claude terminal one keystroke away, same session in
  both. Pins, status badges, and live views into running ingests.

## Development

Requirements: Rust (stable), Node 24+, pnpm.

```sh
pnpm install
pnpm tauri dev     # run the app
cargo test         # Rust tests (ken-core)
pnpm test          # frontend tests
pnpm check         # svelte-check
```

The repo is a Cargo workspace: `crates/ken-core` (all domain logic —
scanning, extraction, index, search, watching), `crates/ken-mcp` (MCP
sidecar, stub for now), `src-tauri` (Tauri 2 shell), and `src/` (Svelte 5
frontend). Design reference: `docs/design/design-tokens.md` and
`docs/design/ken-prototype-v2.dc.html`.

Ken stores its index in the OS app-data directory, never inside your
project folder; the only thing Ken writes to your folder is `.ken/`
(plain-text project config) and any documents you edit.

## Specs

Specs are managed with [OpenSpec](https://github.com/Fission-AI/OpenSpec):
`openspec list` shows active changes; each change carries its proposal,
design, delta specs, and task list under `openspec/changes/`.

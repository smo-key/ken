# Ken

A desktop app that turns an ordinary folder into a team knowledge base.
Point Ken at your folder — notes, Word docs, spreadsheets, PDFs — and it
indexes everything, watches for changes, and makes every fact findable:
full-text search with AI quick answers, AI-maintained structured documents,
Claude chats, a daily digest, deep web research, a knowledge map and
timeline, an MCP server for your agents, and Git/shared-drive sync with
conflict review. Specs live in `openspec/` and
`docs/superpowers/specs/`.

## Install

On macOS or Linux, paste this into a terminal:

```sh
curl -fsSL https://raw.githubusercontent.com/smo-key/ken/main/install.sh | sh
```

It figures out your machine, installs the Ken app and the `ken-mcp`
helper, and tells you about anything else it needs. If no packaged
release exists yet it offers to build Ken from source.

Prefer not to use a terminal (or on Windows)? Download an installer from
the [releases page](https://github.com/smo-key/ken/releases/latest).

**AI features need Claude Code.** Ken's AI features — ingests, chat, deep
research — run through the Claude Code CLI with your own Claude account.
Install it with `npm install -g @anthropic-ai/claude-code` and run
`claude` once to log in. Everything else (indexing, search, editing,
previews) works without it.

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
- **Install & release** — one-line installer (`install.sh`), tag-driven
  release pipeline publishing macOS/Linux/Windows bundles plus `ken-mcp`
  binaries to GitHub Releases, and CI on every push and PR.

## Development

Requirements: Rust (stable), Node 24+, and pnpm (npm works too — the
Makefile uses whichever it finds).

```sh
make setup     # install frontend and Rust dependencies
make dev       # run the app
```

Other targets: `make build` (release bundle), `make test` (Rust +
frontend tests), `make check` (svelte-check), `make clean`. Run `make` on
its own to list them.

### Video transcripts (optional)

Ken plays MP4/MOV/WebM/MKV/AVI videos and shows a transcript beside them.
Adjacent `.vtt` files, and Teams/Zoom `.docx` exports, are picked up
automatically with no setup. To generate a transcript on-device for a video
that has none, two things must be present — otherwise transcription is
skipped silently (everything else keeps working):

- **ffmpeg** on `PATH` (or in `/opt/homebrew/bin`, `/usr/local/bin`,
  `/usr/bin`): `brew install ffmpeg`. Used to pull 16 kHz mono audio out of
  the video. Ken never bundles it.
- **A Whisper model** at `<app-data>/ken/whisper/ggml-base.en.bin` (on macOS
  `~/Library/Application Support/ken/whisper/ggml-base.en.bin`). Download
  `ggml-base.en.bin` from
  <https://huggingface.co/ggerganov/whisper.cpp> and place it there. The
  model is not committed to the repo (multi-MB).

Transcription itself is built from source via the `whisper-rs` crate (bundles
whisper.cpp), so building ken-core needs `cmake` and a C/C++ toolchain. It is
behind the default `whisper` feature; `cargo build -p ken-core
--no-default-features` skips it if that toolchain is unavailable. Generated
transcripts are cached under `.ken/transcripts/` (never written next to your
files) and their text is folded into the search index.

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

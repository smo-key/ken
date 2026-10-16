# Ken

A desktop app that turns an ordinary folder into a team knowledge base.
Point Ken at your folder — notes, Word docs, spreadsheets, PDFs — and it
indexes everything, watches for changes, and makes every fact findable:
search by keyword and by meaning with AI quick answers, a knowledge base
Claude keeps from the sources you give it and checks against the code,
Claude chats, a daily digest, deep web research, a project graph and a
log (the project's risks, actions, issues, decisions and milestones), an MCP knowledge server for your agents, and Git/shared-drive sync with
conflict review. Specs live in `docs/superpowers/specs/`, and the
knowledge layer's plan of record in `docs/knowledge-core/DECISIONS.md`.

## Install

On macOS or Linux, paste this into a terminal:

```sh
curl -fsSL https://raw.githubusercontent.com/smo-key/ken/main/install.sh | sh
```

On Windows, paste this into a PowerShell or Command Prompt window:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/smo-key/ken/main/install.ps1 | iex"
```

Either way it figures out your machine, installs the Ken app and the
`ken-mcp` helper, and tells you about anything else it needs. If no
packaged release exists yet, the macOS/Linux script offers to build Ken
from source.

Prefer not to use a terminal? Download an installer from the
[releases page](https://github.com/smo-key/ken/releases/latest).

**AI features need Claude Code.** Ken's AI features — ingests, chat, deep
research — run through the Claude Code CLI with your own Claude account.
Install it with `npm install -g @anthropic-ai/claude-code` and run
`claude` once to log in. Everything else (indexing, search, editing,
previews) works without it.

## Status

- **Walking skeleton** — project create/open, live indexing (md/txt/code,
  docx, xlsx, pptx, pdf, images), ⌘K full-text search, WYSIWYG markdown
  editing, in-app preview of PDF/Word/Excel/images.
- **Ingest** — drop a source (a meeting note, a doc, a transcript) in the
  knowledge base's inbox and Claude, through your local Claude Code CLI,
  writes it up as a dated note, updates the pages it changes a section at a
  time, and drafts the rulings, risks, issues and tickets it finds for one
  click each. An edit that contradicts a ruling or the code, or rewrites
  much of a page, waits for you; each ingest is one commit.
- **Chat drawer** — talk to your project's knowledge: friendly chat by
  default, the real Claude terminal one keystroke away, same session in
  both. Pins, status badges, and live views into running ingests.
- **Install & release** — one-line installers (`install.sh`, `install.ps1`), tag-driven
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

### Releasing

Releases are driven by a version bump on `main`: bump `package.json`, add the
user-facing entry to `WHATS_NEW.md` (what the in-app "What's new" dialog
shows), run `scripts/sync-version.sh` and `scripts/check-whats-new.sh`, then
push — CI tags `v<version>`, regenerates `CHANGELOG.md`, and builds and
publishes the installers. Run the `/release` skill
(`.claude/skills/release/SKILL.md`) to do all of it; the release gate refuses
to ship a version that `WHATS_NEW.md` doesn't document.

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
scanning, extraction, index, search, watching), `crates/ken-mcp` (the MCP
knowledge server; with `--personal`, the tasks and memory tools Ken's own chat
adds), `src-tauri` (Tauri 2 shell), and `src/` (Svelte 5
frontend). Design reference: `docs/design/design-tokens.md` and
`docs/design/ken-prototype-v2.dc.html`.

Ken stores its index and each repo's settings in the OS app-data
directory, never inside your project folder. It writes to a code repo only
the documents you edit there; in the knowledge base it also writes the
pages it keeps, each change a commit.

## Specs

Design specs are in `docs/superpowers/specs/`. The knowledge layer is
planned in `docs/knowledge-core/DECISIONS.md`, which implements the
Knowledge, Drift and Decisions pages of Ways of Working.

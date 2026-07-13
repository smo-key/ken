---
name: verify
description: How to build, launch, and verify the Ken desktop app (Tauri 2 + Svelte) on macOS
---

# Verifying Ken

## Build & launch

```sh
lsof -ti :1420 | xargs kill -9 2>/dev/null   # vite port must be free
pnpm tauri dev                                # run in background; window appears in ~5-30s
```

Watch the dev log for `not allowed` lines — they mean a missing Tauri
capability in `src-tauri/capabilities/default.json` (permissions compile
into the binary; restart after changing them).

## Seed a fixture project (skips the folder-picker dialog)

```sh
python3 <scratchpad>/genfixtures.py <scratchpad>/atlas-knowledge   # or copy crates/ken-core/fixtures/project
```

Then write `.ken/project.json` (name, uuid, excluded:[]) into it and append
`{id, name, path}` to `~/Library/Application Support/ken/projects.json`.
The app's picker then lists it under "Recent projects".

## Drive & capture

- `screencapture -x -o shot.png` works without extra permissions; focus the
  app first: `osascript -e 'tell application "System Events" to set
  frontmost of (first process whose name is "ken-app") to true'`.
- Keystrokes/clicks via osascript need Accessibility permission for the
  host terminal (System Settings → Privacy & Security → Accessibility).
  Without it, verify watcher/indexing from outside: drop a file into the
  seeded folder, wait ~5s, screenshot — Home shows "N files known" and the
  Files tree updates.
- Process name is `ken-app`. The app DB lives in
  `~/Library/Application Support/ken/index/<uuid>.db` (sqlite3-inspectable
  read-only as a last resort, but prefer the UI).

## Tests (CI territory, not verification)

`cargo test` (ken-core, includes real-file extractor fixtures under
`crates/ken-core/fixtures/project`) and `pnpm test` (vitest).
Watcher tests take ~15s worst case; if one hangs forever suspect the
WatchHandle drop-order deadlock (watcher must drop before thread join).

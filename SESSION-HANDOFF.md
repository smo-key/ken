# Session handoff — workspace group folders (SR move pending)

Last updated: 2026-09-01. Branch: `ken-workspace-home`. Read this top to
bottom before doing anything; the one hard rule is in the next section.

## THE HOLD (do not skip)

The physical move of the three SR repos into `Hytale Code\SR\` is
**blocked until the user explicitly says go**. Their words: *"Ill say
when, as we do not want to break the claude sessions either we have
going."* At last check there were live Claude Code sessions in those
repos AND a running Hytale dev server:

- `gradlew runServer` launched from
  `ShatteredRealms\.claude\worktrees\youthful-archimedes-5b0857`
  (java `com.hypixel.hytale.Main`, ~4 GB heap) plus two Gradle daemons.

Do not stop that server, do not move the repos, do not rename
`~/.claude/projects` dirs until the user gives the word.

## What is DONE and verified

Nested workspace members (`SR/ShatteredRealms` style, one nesting level)
are implemented end to end and gated green:

- **Core** (`crates/ken-core/src/workspace.rs`): `member_leaf` /
  `member_group` / `validate_member_name`; `derived_groups` /
  `effective_groups` / `effective_group_members`; `set_group` rejects
  derived names; discovery surfaces group-folder children and hides the
  container; nested `ignore_folder`. 23/23 `workspace::` tests pass.
- **Tauri** (`src-tauri/src/lib.rs`): add-member validation + leaf
  display names, candidate prefix filtering, two-level ignored walk,
  group ancestor nodes in `get_tree_all`, effective-group view for
  groups/search/chat, chat errors on an empty named scope.
- **Frontend**: `memberLeaf`/`memberGroup` in `src/lib/api.ts`; `openTab`
  longest-prefix member match in `src/lib/app.svelte.ts`; leaf labels in
  WorkspaceSwitcher, MembersStrip, ScopePicker, scope store,
  ProjectGroups (derived groups shown read-only, "from folder"),
  creation wizard. svelte-check: 0 errors / 18 pre-existing warnings.
- **Spec**: `openspec/changes/workspace-group-folders/` (proposal,
  design D6–D9, tasks). Tasks 5.1/5.2 checked; 5.3 (live verification)
  waits on the move.
- **Runtime**: release exe rebuilt with all of this and Ken relaunched
  from it. The running app understands `SR/...` manifest members; the
  current flat layout keeps working unchanged until the move.

## The go-sequence (execute only after the user says go)

Scripts live in `openspec/changes/workspace-group-folders/migration/`.
Both `.mjs` scripts are idempotent — safe to re-run. Run them with Node,
never re-implement in PowerShell (codepage/BOM corruption — see
project memory).

1. User winds down (or authorizes stopping): the Hytale server, the
   Gradle daemons, and every Claude Code session inside the three repos.
2. Stop Ken.
3. `mkdir "C:\Users\Owner\Documents\Hytale Code\SR"` and `Move-Item`
   the three repos into it: `ShatteredRealms`, `ShatterdRealmsTools`
   (spelling is correct — historical typo), `sr-docs`.
4. `node migration/sr-move-state.mjs` — manifest members gain `SR/`,
   `~/.claude/projects` dirs renamed to the new path keys (agent
   history follows), `.claude.json` trust entries added (backup written
   first), Obsidian vault paths, `asset-index.json`, worktree
   `config.worktree` hooksPath fixes.
5. `node migration/sr-move-docs.mjs` — inserts `SR` into absolute
   old-location paths and deepens `../hytale-shared-source`-style
   relative refs across sr-docs, the Tools repo's
   tickets/findings/docs, and ShatteredRealms/docs (~55 files).
6. `git worktree repair <worktree paths>` in **both** code repos (the
   `.git/worktrees/*/gitdir` backlinks are absolute).
7. Hand-edit CLAUDE.mds:
   - NEW `SR\CLAUDE.md`: shared cross-repo facts — the three-layer
     search rule, a repo table, "docs live in sr-docs — search there
     first". (CLAUDE.md discovery walks UP from cwd, so this loads for
     all three repos automatically.)
   - `ShatteredRealms\CLAUDE.md` (~line 9) and
     `ShatterdRealmsTools\CLAUDE.md` (~line 6): sibling-path tables.
   - `sr-docs\CLAUDE.md` + `START-HERE.md`: the
     `..\hytale-shared-source` notes become `..\..\`.
8. Relaunch Ken; verify per tasks.md 5.3: SR group in Home's picker,
   merged tree shows `SR/…`, chat scoped to "SR" reaches both code
   repos, display names are leaves everywhere.
9. Known cosmetic leftover: the Cursor project-cache slug in
   `sr-docs/_meta/build_inventory.py:12` regenerates itself when the
   user next opens the repo in Cursor — leave it.

## Build/test recipe (mandatory on this machine)

Plain `cargo test` fails (Vulkan/Ninja env). Use
`migration/test-workspace.bat` — vcvars64 + VULKAN_SDK + Ninja +
LIBCLANG_PATH + `CARGO_TARGET_DIR=D:\kt`, **release profile** (the
debug-profile CMake caches under `D:\kt\debug\build` are poisoned by an
old `C:\kt` path and need a separate cleanup — task chip exists).
Never write JSON/config files via PowerShell `Set-Content`: PS 5.1
UTF-8 means BOM, and `AppSettings::load` silently rejects BOM'd JSON
(this exact bug ate an afternoon). Use the Write/Edit tools or Node.

## Other open threads (not started / not approved)

- **sr-docs optimization pass** (proposed, awaiting approval): status/
  supersession frontmatter on the ~464 `Work/` tickets, a recurring
  lint job, a `log.md`, `verified:` dates on `Engine/` notes — per the
  two LLM-wiki gists the user shared.
- Sync interval configurability (~1 hr push debounce) — designed, not
  built.
- Onboarding chicken-and-egg: the workspace card is gated on a flag
  that lives in settings.json, which a fresh install doesn't have.
  Offered a fix; not approved.

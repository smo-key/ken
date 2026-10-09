# Test plan — Ken at Level 2 (`knowledge-layer`)

One pass by hand, in the app, over a throwaway folder. Every check says what
to do and what you should see. Mark each one ✓, ✗ (with what you saw instead),
or skipped. Nothing here touches a real repo.

Steps refer to the report "Ken at Level Two".

## 0. Before you start

- [ ] **Build and start Ken.** From `C:\Code\ken`:
  `scripts\win-build.cmd dev`. Close any Ken window started before; Windows
  locks the old exe.
- [ ] **Make the test folder.**
  `node scripts\knowledge-layer-fixture.mjs C:\Users\<you>\ken-test-folder`
  It prints the three folders it made: `Realms-Docs` (a wiki from the method's
  templates), `Realms-Game` (code, with a worktree `Realms-Game-u7` beside it),
  `Personal` (documents, no git).
- [ ] **Optional pieces.** Some checks need more than Ken:
  - the Claude Code CLI, logged in (`claude` once): ingest (§9), the first-wiki
    draft (§3c);
  - an on-device model chosen in Settings: the graph and Map (§6, §10);
  - `ken-mcp` installed by `install.ps1`: the connector path (§11).
  Skip a check that needs what you have not set up, and say so.

## 1. Set-up offered on a fresh start (step 4)

- [ ] On the start screen, the first choice reads **Set up Ken for your code**,
  even if you have never turned on any feature.

## 2. Repos, Team (step 4)

There is no "where is your code?" step: you pick the repos themselves.

- [ ] **Pick repos…**: select `Realms-Docs` and `Realms-Game` together (the
  dialog takes several). Then **Add more repos…**: pick `Realms-Game-u7` and
  `Personal`.
- [ ] The **Repos** table:
  - `Realms-Docs`: kind **wiki**, **read for entities**, evidence mentions
    `_meta/DECISIONS.md` and an Obsidian vault; its description is prefilled
    from its README ("The wiki for Realms-Game.").
  - `Realms-Game`: kind **code**, **searchable only**, evidence mentions
    `Cargo.toml` and its remote; description from its README.
  - `Personal`: no kind, **read for entities**, "no git · documents".
  - `Realms-Game-u7`: unticked, **not indexed**, "a second checkout (git
    worktree) of another repo".
  - Each row shows the repo's full folder.
- [ ] Write a description on `Realms-Game` ("The game: Rust, saves by
  region."). Add more repos again: your text is still there.
- [ ] ✕ on `Personal` takes it out.
- [ ] (A folder of repos) Pick the whole test folder: its repos appear as rows,
  not the folder itself.
- [ ] No file has appeared in the test folder yet (no `.kenignore`, no
  `.ken-workspace`).
- [ ] **Team**: `Realms-Docs` and `Realms-Game` show team **Realms** (the docs
  README names the game), and "Suggested: Realms".

## 3. Index and Confirm (steps 4, 2, 4b)

- [ ] **Index** counts what you kept (with Personal taken out and the worktree
  off: **1 read for entities · 1 searchable only · 1 not indexed**).
- [ ] (3c, needs Claude) Tick **Draft the first wiki pages from these repos**;
  it offers `Realms-Docs`. Leave it ticked if Claude is set up.
- [ ] Confirm. The workspace opens.
- [ ] The test folder is still untouched: no `.kenignore`, no `.ken-workspace`.
  The workspace is in `%APPDATA%\ken\workspaces\<id>\.ken-workspace\workspace.json`:
  it lists the members, a `paths` entry per member with its folder, and a
  group **Realms**.
- [ ] Settings > Projects > **Repos** shows each repo's description, editable.
- [ ] (3c) The first-wiki draft's Review card lists
  `Realms-Game:(what this repo is for, in the team's words)` among its sources.
- [ ] (3c) Within a few minutes, Review has **First wiki drafted: N pages to
  read**. The drafted pages (for example `Current/Project.md`) say
  `status: draft`, have no `verified:` line, cite sources like
  `Realms-Game:README.md`, and a page you had edited beforehand is listed as
  left alone.

## 4. What Ken reads, and what it never reads (steps 2, 3, 5, 7)

- [ ] Search **token** or **API_KEY**: nothing from `credentials.json` or `.env`.
- [ ] Search **world**: no `backups/world.zip`.
- [ ] Nothing from `Realms-Game-u7` appears in any search.
- [ ] Focus `Realms-Game`: Home's footer shows its files as **searchable only**
  and nothing waiting to be read for the graph.
- [ ] Settings > Projects > **Repos**: change `Personal` to **searchable only**.
  After the next scan (switch to it, or wait), its Home footer counts the files
  as searchable only, and nothing of it waits for the graph.
- [ ] Touch a file without changing it (open `Realms-Docs\Platform\Save.md` in
  an editor and save it unchanged, or change its date). Ken does not re-read it:
  the search results are unchanged and nothing new waits for the graph.

## 5. Sync stays out of code (step 1)

- [ ] Focus `Realms-Game` (it has a remote). Settings shows sync **off**, and
  the title-bar dot reads **Local project**. `git log` in `Realms-Game` shows no
  "Ken: update knowledge" commit, ever.
- [ ] Settings > Repos: the kinds are as set-up proposed. Setting a repo's kind
  to **wiki** would turn sync on for it (don't do this on a real repo).

## 6. Home health (step 6)

- [ ] Home's **Index health** shows repos indexed (*n / m*), files queued and
  failed for the team; **Team** opens the Team tab.
- [ ] With no on-device model chosen: one line says **No on-device model is
  chosen … Search still works.** The Timeline shows the same notice.
- [ ] No "index check missed" line (the control page is `START-HERE.md`).

## 7. What a search hit carries (steps 8, 9, 11)

Use the search box (Ctrl+K) with **This project** scope, on `Realms-Docs`.
None of this needs Search by meaning or a model: a plain scan now builds the
keyword index over sections.

- [ ] Search **region**: `Platform/Save.md` comes back.
- [ ] Search **shard** (on no page; the Vocabulary page pairs it with
  "region"): `Platform/Save.md` still comes back.
- [ ] Delete a page in Files, then search a word only it had: it no longer
  comes back.
- [ ] (2b) Search **region** again and pick **Business readers**: only
  `Design/Why-regions.md` (a business page) is left; `Platform/Save.md` (dev)
  is gone. **Developers** shows `Platform/Save.md` and code, not the business
  page. **Any reader** shows both. Switch to all projects: the same choice
  applies there.
- [ ] (MCP, optional) Through `ken-mcp`, search **region**: the line starts
  with a locator like `[[Save]] · Realms-Docs:Platform/Save.md:1`. The app's
  result list does not show the locator yet.
- [ ] A page under `Ways-of-Working/` ranks above a `Research/` page for the
  same word (search a word both have, for example **ingest**).
- [ ] (MCP, optional) Through `ken-mcp`, a search result line starts with the
  locator, and a Research hit says "evidence from <date>, not current state".

## 8. Links (step 10)

- [ ] Review has **Links: … missing, … broken, … ambiguous** for `Realms-Docs`.
  It lists **[[Ghost Page]]** as missing, **Platform/Gone.md** as broken under
  Platform, and no `{{…}}` placeholder anywhere.
- [ ] Open `Platform/Save.md` in Files: under it, **Links to: Combat** and
  **Linked from: Combat**. Clicking one opens it.

## 9. Ingest (the white box's 8b, needs Claude)

- [ ] The rail item reads **Ingest** and the screen has no tabs (Recipes and
  Automations are gone). The chip beside the heading names the team's wiki,
  **whichever repo is focused**: focus a code repo and open Ingest again; it
  still shows the wiki's inbox.
- [ ] Drag `2026-09-20 standup.txt` from Explorer onto the left column (it
  highlights), or use **Drop a file or choose one**. It appears under
  **raw** with its size as *in the queue*, then *reading*; a second file with
  the same name lands as `… (2).txt`. Nobody is asked to confirm anything.
- [ ] When it is read: `Research/Ingestion/Ingested/Meetings/<month>/<today>-…md`
  exists (`status: evidence`, `kind: meeting`, `source:` where it will be
  filed) with the sections the template keeps for a meeting (Summary, Key
  Takeaways, What It Overturns, … Next Steps). The source is still in Raw,
  its row reads *meeting · … present · read · 1 page written · 2 wait* (or
  similar), and its card opens on the right.
- [ ] Already written, each citing the note: `Current/Project.md` says the ship
  date is Friday and links the note (a small edit; it was rewritten in place,
  not appended to). With a team repo on the team (kind **team**), any idea is
  `ideas/I-nnn.md` and any escalation `escalations/E-nnn.md` there
  (`raised_by: ingest of …`, `to:` the person named). A next step for the git
  user is on **Home › Your day** (`updated_by: ingest`, linked to the note);
  someone else's stays on the card, under *on this card only*.
- [ ] The card: **key takeaways · the note is in Ingested** (summary, takeaways,
  what it overturns with its quote, contradictions); then **written**, one row
  per write with **Open** and **Undo**, and the line *Every page edit went
  through staging; none was held.*; then **waits**, only: the ruling ("the
  region format stays", **Accept as Ana**, greyed unless you are Ana), the
  save-bug action as a ticket (**Open** shows it, **Accept** writes
  `tickets/<KEY>-nnn.md` in the team repo), and any edit staging held, shown
  as its diff with **Apply**. There is no Apply all.
- [ ] Staging: edit `Current/Project.md` by hand while a second source about
  the ship date is *reading*. That edit is held (*n held, below.*), shown as
  its diff against your version; your edit stands until you **Apply**.
- [ ] **Undo** on the Project row puts the page back. Edit the page, then Undo
  a row on another source's card: *The page changed since; open it to undo by
  hand.*, and the page is left alone. **Open** goes to the page (or to Your
  day for a task).
- [ ] **Seen** moves the source beside its note and marks it *seen* under
  **ingested**. On another source, **Undo all** reverses every write (last
  first), removes the note (unless you edited it), closes what waits and
  leaves the source in Raw. Its card says *Undone* and the next pass leaves
  it alone; **Read it again** reads it as if just dropped in. The **Ingest**
  rail item's count is the number of sources with something waiting.
- [ ] (Optional) Record a short clip in Ken: the transcript lands in Raw.

## 10. Drift (step 13)

- [ ] Review has **Drift: … gone, … to re-read, … not verified in 30 days** for
  `Realms-Docs` (it runs after the first scan). It says:
  - **Findings**: `Platform/Legacy.md` cites `src/old.rs` (gone) and
    `src/save.rs:99` (past the end of the file);
  - **Is the page still true?**: `Platform/Save.md` (save.rs changed after its
    verified date), and the decision `D-001`;
  - **Combat.md is not listed** (its code change was a comment): it is counted
    as cleared;
  - **Design/Why-regions.md is not listed**, though it cites the same changed
    code as Save.md: it is a business page, and the report says business
    pages are checked by age only (2b);
  - **Not verified in 30 days**: `Platform/Legacy.md`, and the template pages
    (never verified);
  - that no controls are declared.
- [ ] Nothing in the wiki was edited by the sweep.
- [ ] Home › **Check for drift now** with nothing committed in `Realms-Game`: the card
  reads the same. Then commit a real change to `src/combat.rs` in
  `Realms-Game` and check again: `Platform/Combat.md` now appears under
  **Is the page still true?**, and nothing else changes. (The sweep asks git
  once per repo what changed and re-measures only those files.)

## 10b. The watcher at scale

- [ ] With `Realms-Docs` focused, edit and save `Platform/Save.md` outside
  Ken. Search finds the new words within a couple of seconds.
- [ ] Create `Realms-Docs/build/` with a file in it and add `build/` to its
  `.gitignore` first: the file never shows in search.

## 11. Settings and the team (step 15)

- [ ] Settings > Features lists plain names: **Search by meaning**, **Background
  reading**, **Team knowledge graph**, …, each with where it applies and its
  code name small.
- [ ] Settings > Working with agents: with `ken-mcp` installed by
  `install.ps1`, the connector shows its path under
  `%LOCALAPPDATA%\Programs\ken-mcp`. (In a dev build without it installed,
  "not found" is expected.)
- [ ] (Optional, needs a Ken team/family) The family tray has **Send**: pick a
  member, a kind, a title; it lands in their inbox.

## 12. The graph (step 14, needs a model and the graph features)

- [ ] Settings > Features: turn on **Team knowledge graph** and **Search across
  the team**; let background reading run. The graph is built over
  `Realms-Docs` and `Personal` (entity repos), not `Realms-Game`.
- [ ] Its file is `<test folder>\.ken-workspace\kg.sqlite`.
- [ ] With **Realms** picked in the scope picker, the graph search shows only
  entities from Realms repos.

## 13. Adding repos later (step 4)

- [ ] Settings > Projects > Repos > **Add repos…**: pick `Personal`. It waits
  in a panel with its proposed kind, a team box and its README description;
  nothing joins until **Add**.
- [ ] Set its team to `Realms` (the team whose wiki is `Realms-Docs`). The
  **Update the team's wiki** box appears, ticked. Click **Add**. A note says
  `Realms-Docs` is being updated.
- [ ] Focus `Realms-Docs` (needs Claude). After a minute or two:
  - `Repo-Map/Personal.md` exists, `status: draft`, citing `Personal:` sources.
  - Review has one card "Personal added to the wiki: … drafted, … proposed".
  - Each page a person keeps that the repo changes (the architecture, who does
    what, the project, the vocabulary, the Repo Map index) has its own card,
    "Proposed: <page> with Personal", showing the change as a diff.
  - **Apply the change** writes it. Edit a proposed page first, then apply its
    card: it refuses and says the page changed. **Mark as done** discards.
- [ ] Rename a member's folder; **Scan again** names it as gone.

## 14. A new team wiki (the wiki at team level)

- [ ] Start set-up again (start screen › Set up Ken…). Pick `Realms-Game`
  and `Personal` only, not `Realms-Docs`. In **Team**, set both to `Realms`.
- [ ] **Each team's wiki** lists `Realms` with **No wiki for now**. Choose
  **Create a new wiki…**; the name reads `Realms-Wiki`. **Next** stays off
  until **Choose where…** picks a folder (the test folder).
- [ ] Confirm with **Draft the first wiki pages** ticked. On disk,
  `<test folder>\Realms-Wiki` has the template's pages and a `.git` with one
  commit. `START-HERE.md` lists `Realms-Wiki`, `Realms-Game` and `Personal`
  with what each is for. No page has a `verified:` date filled in.
- [ ] Settings > Projects > Repos lists `Realms-Wiki` as a wiki on `Realms`.
- [ ] With Claude, after a few minutes: `Repo-Map/Realms-Game.md`,
  `Repo-Map/Personal.md` and `Repo-Map/Index.md` exist, then Current/Project,
  Team, Who-Does-What, the architecture page and Work/Releases. Review on
  `Realms-Wiki` has one card listing them all.
- [ ] Nothing was pushed anywhere: `git -C "<test folder>\Realms-Wiki" remote`
  prints nothing.

## 14a. Every hit says what kind of file it is

- [ ] Search anything: each result shows its kind (code, test, spec, doc,
  config, data, design, meeting, ticket) beside the file name, in project,
  all-projects and routed search alike.
- [ ] Choose "Code" in the kind menu: only code is left, and a question
  whose answer is in code ("how are tokens validated") now puts the code
  first rather than the spec describing it. "Specs" and "Tickets" do the
  same for theirs; "Any kind" brings everything back.
- [ ] The kind menu and the reader chips combine (Developers + Code).
- [ ] In chat, ask Claude to "search only specs for …": its Ken search
  results carry `[spec]` tags and nothing else.

## 14c. Code structure: definitions, usages, imports, history

- [ ] Open a code file (Rust, Python, JS/TS, Go, Java or C#) in Files: under
  it, Outline (count), Imports and Imported by. Open Outline: definitions
  nested (methods under their class); click one and the file jumps to it.
- [ ] Click "usages" on a function: every call and reference across the
  workspace's repos, each with the function it sits in; "defined" rows
  first. Click one in another repo: that repo is focused and the file opens
  at the line.
- [ ] Search `get_current_user` (any identifier): its definition is first,
  at its line.
- [ ] In chat ask "who calls <function>?" and "what depends on <file>?":
  Claude uses find_usages / related_files and cites ken:// lines. Ask "when
  did <file> last change and why?": it uses history.
- [ ] An existing project (indexed before this): the code panel works on
  first open without a rescan (backfill from stored text).

## 14d. When the index finds nothing (needs Claude)

- [ ] ⌘K a question with few or no hits: "Ask Ken to look" appears. Click
  it: after a short wait, "Ken looked" with an answer and sources; a source
  in another repo opens there. Nothing is written.
- [ ] In chat, ask about something Ken's index cannot find but is in a
  sibling repo: Claude says Ken's search found nothing, looks with Grep/Read,
  and cites what it found. It never answers "not there" without looking.

## 14b. The map comes from Claude (needs Claude)

- [ ] With no on-device language model installed (only the embedding
  model), open a project with notes and no map. About a minute after the
  first scan the Map shows "building", then entities and a timeline, within
  a few minutes. The Map's progress shows all files analyzed.
- [ ] Edit two notes and wait: within about half an hour the Map is rebuilt
  once (not once per note).
- [ ] Hide Claude Code (rename it off PATH) and reopen: Home and the Map say
  Claude Code isn't installed and how to install it; search still works.
- [ ] ⌘K a question: the quick answer comes from Claude.

## 15. Claude in chat: edits reviewed, sources clickable (needs Claude)

- [ ] Open `Realms-Docs/Platform/Save.md`. In chat, ask: "Rename the title to
  Saving and add a line that saves are versioned." A card shows the edit as a
  diff, each change with its old and new line numbers, and the same diff sits
  over the open page. Nothing is written yet.
- [ ] Deny the title change, Accept the other, **Apply choices**. Only the
  accepted line lands on disk; Claude's next message says which change was
  left out and does not redo it.
- [ ] Ask for another edit and **Deny all**: the file is unchanged. Ask again
  and **Accept all**: Claude makes the edit as proposed.
- [ ] Edit the page yourself while a proposal waits, then Apply choices: it
  refuses and says the file changed.
- [ ] Ask "Where do we decide how saves are written?" The answer cites files
  as links. Clicking one opens it in Files at the cited line or heading, which
  flashes. Claude never opens a tab or switches screen by itself.
- [ ] While Claude answers, the reply appears word by word with a cursor,
  then settles into the finished message with no jump or duplicate.
- [ ] Each tool Claude uses shows as a card ("Ken route query …", "Read …",
  "Grep …") with a spinner, then a tick. Click a card to see the start of its
  result; a failed tool shows `!`, a red edge, and opens by itself.
- [ ] Close and reopen the chat: the cards are still there, none spinning.

## 16. Workspaces only: the start screen, the team, the repos

- [ ] A fresh start shows **Open a workspace** and **Set up a new workspace**,
  no "One project". Open a folder with no workspace: it says so and offers
  **Set one up**. Settings › Features has no "Several repos together", "Team
  knowledge graph" or "Search across the team"; "Search by meaning" is on.
- [ ] After opening one, **Recent workspaces** lists it (where and when);
  right-click › Remove from this list. Ken reopens it on the next launch. The
  workspace switcher in the rail lists the other recent ones.
- [ ] The title bar shows the **team** (ATT), not a repo; its menu lists each
  team on its own, no "All". Files shows the team's **Repos** above the tree;
  picking one shows its files. The search box says *Search ATT…*, and Home's
  digest covers the team's repos only.
- [ ] Review with a code repo focused lists the wiki as *ATT-Wiki · n waiting*
  when its ingest or drift items wait; the rail count includes them.

## 17. Team (the white box's 9)

- [ ] The rail has **Team**: the team's repos, the wiki first, each with how
  deep Ken reads it, its kind, branch, commit and drift. Change a repo's index
  to *off*: its files leave the search after the next scan. **Take out**
  removes a repo from the workspace; its folder stays.
- [ ] **Edit** the ignore list, add `Research/raw-exports/`, Save: the
  workspace's `.kenignore` has the line.
- [ ] **Drift and gaps** names what a repo lacks: a code repo with no
  CLAUDE.md, placeholders left in the wiki, a repo with no Repo Map page. A
  repo behind its upstream shows *n newer upstream*.
- [ ] Put a new repo folder beside the others, **Scan again**: it is listed as
  a new folder with **Add to team**; adding it lists it in the table.
- [ ] **+ Add a repo** with a folder elsewhere: it joins the team, and the
  wiki's Review gets its Repo Map page and proposals.
- [ ] **Run the sweep now**: the standing sweep shows the run (pages, controls,
  pages to check, unverified for thirty days).
- [ ] **Add a rule** ("Ship small PRs"): `Ways-of-Working/Rules/ship-small-prs.md`
  opens, made from the wiki's rule template.

## 18. Your day (the Home white box's Y1, Y1b, Y1c)

- [ ] Home's header says **Your day** with the team. The rail has no Tasks.
  Settings › Features lists no Ken memory, Task board, Team inbox or Pipeline
  board; Settings shows the Memory section.
- [ ] The digest is one paragraph for the team, with source chips that open
  the file in its repo. With none yet: *No digest yet…* and **Write it now**.
- [ ] Add `tickets/ATT-001.md` to a repo with `id`, `title`, `status: todo`,
  `assignee:` your git user name, `target:` tomorrow. **Tickets** lists it
  with its state and *Thu*-style target; the row opens it in Files. Set
  `status: done`: it leaves the list. A team with no tickets folder shows no
  Tickets list.
- [ ] **+ Task**: title, target date, repeat, a link to `ATT-001`, a
  description; Enter in the title adds it. It is listed under **Other**,
  ordered by target. The file is in `.ken-workspace/tasks/`.
- [ ] Open the task: change each field; each saves (reopen Ken to check).
  The checkbox marks it done; it stays greyed today and is gone tomorrow.
- [ ] A task with **Repeat: weekdays** shows on weekdays only; marking it done
  shows it done today and open again the next weekday.
- [ ] Open `tickets/ATT-001.md` in Files: the right column lists your tasks
  linked to it and the wiki pages that cite `ATT-001`; **+ Task** there adds
  one already linked.
- [ ] In chat: "add a task to check the pilot sites by Friday". It appears
  under Other with *updated by mcp*. "Mark it done" marks it done.
- [ ] With a team inbox connected (Settings), a task a teammate sends shows
  under Other with **Accept ▾**; Accept makes it a normal task with *from*;
  Not mine removes it.

## 19. The Briefing design (docs/design/briefing)

- [ ] The sidebar: the team with its sync state at the top (its menu lists
  the teams and Team library), Home, Inbox (amber count), Files (teal dot when
  files changed), Ingest, Explore, then Settings and Collapse. Collapse keeps
  the icons and survives a restart. Light and dark both follow the system.
- [ ] Home: *Good morning, <name>*, the date and digest time, the digest,
  **Your list** (tickets square, tasks round, a teammate's task dashed with
  **Accept**), **Picked up where you left off**, and on the right **Needs
  you** (the top of the Inbox) and what Ken is reading.
- [ ] Inbox: All / People / Ken / Sync & files with counts. A teammate's task
  (Accept, Not mine, Reply), an ingest's ruling or ticket or held edit (Accept
  or Apply, Discard, Open the note), a conflict (its two sides), a file Ken
  could not read (Open in Files, Ignore this file), a question in a chat
  (Answer in Ask Ken). Dealt-with items move to Done with what happened.
- [ ] Ingest: **Add a file** (or drop one), **Record a meeting**, **Write a
  note**, **From a chat**. In progress shows a bar per source; a source shows
  its steps, **Needs your decision** cards, Summary, Next steps, Pages Ken
  updated (Open, Undo) and **Undo all**.
- [ ] Explore: Timeline and Map switch with one control; each keeps its place.
- [ ] Settings: General, Team library (the folders table, ignore list, checks,
  rules, templates), Sync (the team inbox), AI (Claude Code, models, the
  graphics card, memory, features), Agents (the connector), Ingest rules.
- [ ] Models (Settings › AI): download Nomic; search by meaning reads the
  team (progress shown); switch to Qwen3 Embedding: every folder is read again
  while keyword search keeps working. Turn the graphics card off: *Runs on:
  CPU*.
- [ ] Windows: no console window opens during an ingest, a chat or the
  digest. Opening Ken from the Start menu while it is in the tray brings the
  window back instead of a second copy. A screenshot in a repo is found by its
  text (OCR). Record a meeting captures you and the other side.

## 20. The team's wiki answers first (MCP, and chat for the last two)

The fixture's `_meta/DECISIONS.md` holds six rulings in the template's plain
form; D-006 (ship on Fridays) reverses D-003 (ship on Mondays), and
`tickets/RG-7.md`, cancelled, argues for Mondays again.

- [ ] Through `ken-mcp`, `route_query` **when do we ship?** with no repo named:
  `Realms-Docs` is searched although the question names no repo, and the hits
  carry labels: D-006 reads `ruling D-006 · 2026-09-20`, RG-7 reads
  `ticket · cancelled · 2026-09-21`. D-006 ranks above RG-7.
- [ ] Each ruling is its own hit: the D-006 hit does not carry D-005's text.
- [ ] `read_document` on the D-006 hit's address returns that whole entry, from
  its `D-006` line to the line before the next entry, not a fixed window.
- [ ] (needs `cargo run -p ken-core --example eval_run -- supersede` with
  `KEN_EVAL_KB=Realms-Docs`; the app does not run the pass yet) After the pass,
  the D-003 hit's label names D-006, and D-006 ranks ahead of it.
- [ ] (needs Claude) In Ken's chat, ask **Should we move releases back to
  Monday?** The answer cites D-006 as settled and does not recommend Monday;
  it may mention RG-7 as cancelled.
- [ ] (needs Claude) Ask **What do we call a region?** The answer gives
  "shard" from D-004 or the Vocabulary page, and cites it.

## After

- [ ] Close Ken; delete the test folder and `Realms-Game-u7`/`u8` beside it.
  The test workspace is on the start screen's Recent workspaces:
  right-click › Remove from this list.

## Known gaps (not bugs to report)

None known. The reader choice (Any reader, Business readers, Developers)
applies to the project search, all-projects search, routed search and the
MCP's search tools.

On Windows, check by hand what the ignored fake-CLI tests cover there:
- [ ] Run a research question: the report follows the whole prompt (it
  reaches `claude.cmd` as a file), and Cancel stops it.
- [ ] Start a chat, send two turns, then close it: no `node.exe` left in
  Task Manager.
- [ ] Run an ingest and a morning digest: both complete.

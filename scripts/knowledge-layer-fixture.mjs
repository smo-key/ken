#!/usr/bin/env node
// Build a throwaway code folder for testing the knowledge-layer branch by
// hand (TEST-PLAN-knowledge-layer.md). Nothing here touches a real repo.
//
//   node scripts/knowledge-layer-fixture.mjs [target] [path-to-Ways-of-Working]
//
// target defaults to ~/ken-test-folder and must not exist yet. The wiki is
// copied from Ways-of-Working/templates/wiki when that folder is found
// (default: a sibling of this repo), so the method's own templates are what
// set-up, drafting, links and drift see.
import { execFileSync } from "node:child_process";
import { cpSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const target = resolve(process.argv[2] ?? join(homedir(), "ken-test-folder"));
const wow = resolve(process.argv[3] ?? join(here, "..", "..", "Ways-of-Working"));

if (existsSync(target)) {
  console.error(`${target} already exists. Remove it or pass another folder.`);
  process.exit(1);
}

const git = (cwd, ...args) => execFileSync("git", args, { cwd, stdio: ["ignore", "pipe", "pipe"] });
const write = (path, text) => {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, text);
};
const commit = (cwd, msg, date) =>
  execFileSync("git", ["commit", "-q", "-m", msg], {
    cwd,
    env: { ...process.env, GIT_AUTHOR_DATE: `${date}T12:00:00`, GIT_COMMITTER_DATE: `${date}T12:00:00` },
  });
const init = (cwd) => {
  mkdirSync(cwd, { recursive: true });
  git(cwd, "init", "-q", "-b", "main");
  git(cwd, "config", "user.email", "tester@example.com");
  git(cwd, "config", "user.name", "Tester");
  git(cwd, "config", "core.autocrlf", "false");
};

// --- Realms-Game: a code repo, with history for drift and a worktree -------
const game = join(target, "Realms-Game");
init(game);
git(game, "remote", "add", "origin", "https://github.com/example/Realms-Game.git");
write(join(game, "Cargo.toml"), '[package]\nname = "realms"\nversion = "0.1.0"\n');
write(join(game, "README.md"), "# Realms Game\n\nA small tactics game. Saves are written by `src/save.rs`.\n");
write(join(game, "src/save.rs"), "pub fn save() {\n    write_regions();\n}\n");
write(join(game, "src/combat.rs"), "pub fn hit() {}\n");
write(join(game, "src/old.rs"), "pub fn legacy() {}\n");
git(game, "add", "-A");
commit(game, "first", "2026-09-01");
// After the wiki's verified date: a real change, a comment-only change, a deletion.
write(join(game, "src/save.rs"), "pub fn save() {\n    write_all_regions();\n}\n");
write(join(game, "src/combat.rs"), "// hits land here\npub fn hit() {}\n");
rmSync(join(game, "src/old.rs"));
git(game, "add", "-A");
commit(game, "later", "2026-09-15");
// Things set-up and the scan must keep out.
write(join(game, "config/credentials.json"), '{ "token": "not-a-real-token" }\n');
write(join(game, ".env"), "API_KEY=not-a-real-key\n");
write(join(game, "backups/world.zip"), "PK\u0003\u0004 not really a zip\n");
git(game, "worktree", "add", "-q", "../Realms-Game-u7", "-b", "u7");

// --- Realms-Docs: the wiki, from the method's templates -------------------
const docs = join(target, "Realms-Docs");
if (existsSync(join(wow, "templates", "wiki"))) {
  cpSync(join(wow, "templates", "wiki"), docs, { recursive: true });
} else {
  console.warn(`No templates at ${wow}\\templates\\wiki; writing a minimal wiki instead.`);
  write(join(docs, "START-HERE.md"), "---\ntitle: Start here\n---\n# Start here\n");
  write(join(docs, "Current/Project.md"), '---\ntitle: "Project"\n---\n# Project\n\n{{one paragraph}}\n');
  mkdirSync(join(docs, "Research/Ingestion/Raw"), { recursive: true });
  mkdirSync(join(docs, "Research/Ingestion/Ingested"), { recursive: true });
}
init(docs);
// A vocabulary row, so a search in the team's word ("shard") finds the page
// written in the platform's ("region").
const vocab = join(docs, "Vocabulary.md");
const row = "| shard | region | Platform/Save.md |\n";
if (existsSync(vocab)) {
  writeFileSync(vocab, readFileSync(vocab, "utf8").replace(/\| \{\{our word\}\} \|[^\n]*\n/, row));
} else {
  write(vocab, "# Vocabulary\n\n| our word | the platform's word | where it lives |\n|---|---|---|\n" + row);
}
write(join(docs, "README.md"), "# Realms Docs\n\nThe wiki for Realms-Game.\n");
mkdirSync(join(docs, ".obsidian"), { recursive: true });
// Six rulings, so search cuts the log one ruling per chunk; D-006 reverses
// D-003, and a cancelled ticket argues for D-003 again.
write(
  join(docs, "_meta/DECISIONS.md"),
  "# Decisions\n\nD-001 · 2026-09-02 · save format — Worlds save as region files.\n" +
    "  - \"regions, not one blob\" -\n  sources: Realms-Game:src/save.rs:2\n  aliases: world save, persistence\n\n" +
    "D-002 · 2026-09-03 · combat — Hits resolve at once, with no hit queue.\n  - \"no queue, hits land now\" -\n\n" +
    "D-003 · 2026-09-04 · releases — We ship on Mondays. [SUPERSEDED BY D-006]\n  - \"Monday mornings, before standup\" -\n\n" +
    "D-004 · 2026-09-05 · naming — The team says shard; the code says region.\n  - \"shard when we talk, region in code\" -\n\n" +
    "D-005 · 2026-09-08 · backups — Keep the last three world backups.\n  - \"three, not ten\" -\n\n" +
    "D-006 · 2026-09-20 · releases — We ship on Fridays, not Mondays.\n  - \"we ship on Friday, not Monday\" -\n  decider: Ana\n",
);
write(
  join(docs, "tickets/RG-7.md"),
  "---\nid: RG-7\ntitle: Ship on Mondays again\nstatus: cancelled\nupdated: 2026-09-21\n---\n" +
    "# RG-7 Ship on Mondays again\n\nMove the release back to Monday mornings, before standup.\n",
);
write(
  join(docs, "Platform/Save.md"),
  "---\ntitle: Save\naliases: [\"persistence\"]\nverified: 2026-09-05\nsources:\n  - Realms-Game:src/save.rs:2\n---\n" +
    "# Save\n\nEach region is written to disk on its own. See [[Combat]] and [[Ghost Page]], and [the old notes](../Platform/Gone.md).\n",
);
write(
  join(docs, "Platform/Combat.md"),
  "---\ntitle: Combat\nverified: 2026-09-05\nsources:\n  - Realms-Game:src/combat.rs\n---\n# Combat\n\nHits resolve at once. Back to [[Save]].\n",
);
write(
  join(docs, "Platform/Legacy.md"),
  "---\ntitle: Legacy\nverified: 2026-08-01\nsources:\n  - Realms-Game:src/old.rs\n  - Realms-Game:src/save.rs:99\n---\n# Legacy\n",
);
// A business page (Design is for readers who never see code): it cites the
// same changed code as Save.md, but drift checks it by age only.
write(
  join(docs, "Design/Why-regions.md"),
  "---\ntitle: Why regions\nverified: 2026-09-18\nsources:\n  - Realms-Game:src/save.rs:2\n---\n" +
    "# Why regions\n\nWe save each region on its own so a crash loses one region, not the world.\n",
);
write(
  join(docs, "Research/Ingestion/Raw/2026-09-20 standup.txt"),
  "Standup, 20 September.\nAna: \"we ship on Friday, not Monday\".\nBen: \"I will fix the save bug today\".\nAna decided: the region format stays.\n",
);
git(docs, "add", "-A");
commit(docs, "wiki", "2026-09-20");

// --- Personal: documents, no git ------------------------------------------
write(join(target, "Personal/notes.md"), "# Notes\n\nThings to remember about the region format.\n");

console.log(`Test folder ready: ${target}`);
console.log("  Realms-Docs      wiki (templates, decisions, links, drift pages, one Raw source)");
console.log("  Realms-Game      code (history, secrets, an archive) + worktree Realms-Game-u7");
console.log("  Personal         documents, no git");

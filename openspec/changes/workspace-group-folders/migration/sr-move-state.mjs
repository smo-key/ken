// SR move — machine-state fixups. Run AFTER the three repos have moved
// into "Hytale Code/SR/". Idempotent: every step checks before writing.
import fs from "node:fs";
import path from "node:path";

const HOME = "C:/Users/Owner";
const HC = `${HOME}/Documents/Hytale Code`;
const REPOS = ["ShatteredRealms", "ShatterdRealmsTools", "sr-docs"];
const log = (m) => console.log(m);

// 1. Ken workspace manifest: members gain the SR/ prefix.
{
  const p = `${HC}/.ken-workspace/workspace.json`;
  const j = JSON.parse(fs.readFileSync(p, "utf8"));
  let changed = false;
  j.members = j.members.map((m) => {
    if (REPOS.includes(m)) { changed = true; return `SR/${m}`; }
    return m;
  });
  if (changed) {
    fs.writeFileSync(p, JSON.stringify(j, null, 2) + "\n");
    log(`manifest: members now ${JSON.stringify(j.members)}`);
  } else log("manifest: already migrated");
}

// 2. Claude Code project-state dirs: rename to the new path keys so agent
// memory/history follows the repos.
{
  const projects = `${HOME}/.claude/projects`;
  const oldPrefix = "C--Users-Owner-Documents-Hytale-Code-";
  for (const dir of fs.readdirSync(projects)) {
    const repo = REPOS.find((r) => dir.startsWith(oldPrefix + r));
    if (!repo) continue;
    const next = dir.replace(oldPrefix + repo, `${oldPrefix}SR-${repo}`);
    const from = path.join(projects, dir);
    const to = path.join(projects, next);
    if (fs.existsSync(to)) { log(`claude-projects: ${next} already exists, skipping ${dir}`); continue; }
    fs.renameSync(from, to);
    log(`claude-projects: ${dir} -> ${next}`);
  }
}

// 3. .claude.json trusted-folder entries: add the new-path keys with the
// same values (old keys kept — harmless, and a rollback keeps working).
{
  const p = `${HOME}/.claude.json`;
  fs.copyFileSync(p, `${p}.sr-move-backup`);
  const j = JSON.parse(fs.readFileSync(p, "utf8"));
  let changed = false;
  for (const repo of REPOS) {
    const oldKey = `${HC}/${repo}`;
    const newKey = `${HC}/SR/${repo}`;
    if (j.projects?.[oldKey] && !j.projects[newKey]) {
      j.projects[newKey] = j.projects[oldKey];
      changed = true;
      log(`claude.json: trusted ${newKey}`);
    }
  }
  if (changed) fs.writeFileSync(p, JSON.stringify(j, null, 2));
  else log("claude.json: nothing to add");
}

// 4. Obsidian vault registry.
{
  const p = `${HOME}/AppData/Roaming/obsidian/obsidian.json`;
  if (fs.existsSync(p)) {
    const j = JSON.parse(fs.readFileSync(p, "utf8"));
    let changed = false;
    for (const v of Object.values(j.vaults ?? {})) {
      const m = REPOS.find((r) => v.path.includes(`Hytale Code\\${r}`));
      if (m) { v.path = v.path.replace(`Hytale Code\\${m}`, `Hytale Code\\SR\\${m}`); changed = true; }
    }
    if (changed) { fs.writeFileSync(p, JSON.stringify(j)); log("obsidian: vault paths updated"); }
    else log("obsidian: nothing to update");
  }
}

// 5. asset-index.json inside the moved ShatteredRealms: absolute
// forward-slash self-paths.
{
  const p = `${HC}/SR/ShatteredRealms/asset-index.json`;
  if (fs.existsSync(p)) {
    const text = fs.readFileSync(p, "utf8");
    const next = text.split("Hytale Code/ShatteredRealms").join("Hytale Code/SR/ShatteredRealms");
    if (next !== text) {
      fs.writeFileSync(p, next);
      log(`asset-index: rewrote ${(text.length - next.length) ? "" : ""}${text.split("Hytale Code/ShatteredRealms").length - 1} paths`);
    } else log("asset-index: already migrated");
  } else log("asset-index: not found (ok if repo not moved yet)");
}

// 6. Per-worktree hooksPath overrides in ShatterdRealmsTools.
{
  const wtDir = `${HC}/SR/ShatterdRealmsTools/.git/worktrees`;
  if (fs.existsSync(wtDir)) {
    for (const wt of fs.readdirSync(wtDir)) {
      const cfg = path.join(wtDir, wt, "config.worktree");
      if (!fs.existsSync(cfg)) continue;
      const text = fs.readFileSync(cfg, "utf8");
      const next = text.split("Hytale Code\\ShatterdRealmsTools").join("Hytale Code\\SR\\ShatterdRealmsTools");
      if (next !== text) { fs.writeFileSync(cfg, next); log(`hooksPath fixed: ${wt}`); }
    }
  }
}
log("state fixups done");

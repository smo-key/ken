// SR move — documentation path fixups inside the three MOVED repos.
// Run AFTER the move. Idempotent (replacements never re-match their own
// output). Node keeps everything UTF-8 — no codepage round-trips.
import fs from "node:fs";
import path from "node:path";

const HC = "C:/Users/Owner/Documents/Hytale Code";
const SR = `${HC}/SR`;
let filesTouched = 0, edits = 0;

// Absolute old-location references: insert SR between "Hytale Code" and
// the repo name, whichever slash style (and however many backslashes an
// escaped context uses). Negative lookahead keeps already-migrated paths
// (Hytale Code\SR\...) untouched.
const ABS = /(Hytale[ %]20?Code|Hytale Code)([\\/]+)(?!SR[\\/])(ShatteredRealms|ShatterdRealmsTools|sr-docs)/g;
// Relative escapes to non-moving siblings now need one more level. The
// lookbehind refuses "../../hytale-shared-source" so re-runs are no-ops.
const REL = /(?<!\.\.[\\/])\.\.([\\/])(hytale-shared-source|_asset-backup|sr-universe)/g;

function fixFile(p) {
  const text = fs.readFileSync(p, "utf8");
  let n = 0;
  const next = text
    .replace(ABS, (_, hc, sep, repo) => { n++; return `${hc}${sep}SR${sep}${repo}`; })
    .replace(REL, (_, sep, target) => { n++; return `..${sep}..${sep}${target}`; });
  if (n > 0) { fs.writeFileSync(p, next); filesTouched++; edits += n; }
}

function walk(dir, exts) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      if (entry.name.startsWith(".") || entry.name === "node_modules" || entry.name === "target") continue;
      walk(p, exts);
    } else if (exts.some((e) => entry.name.endsWith(e))) {
      fixFile(p);
    }
  }
}

// sr-docs: every note, plus the _meta scripts with hardcoded self-paths.
walk(`${SR}/sr-docs`, [".md", ".py"]);
// The two code repos: docs/tickets/findings only — never source or assets.
for (const dir of [
  `${SR}/ShatterdRealmsTools/tickets`,
  `${SR}/ShatterdRealmsTools/findings`,
  `${SR}/ShatterdRealmsTools/docs`,
  `${SR}/ShatteredRealms/docs`,
]) {
  if (fs.existsSync(dir)) walk(dir, [".md"]);
}
console.log(`docs fixups done: ${edits} edits across ${filesTouched} files`);

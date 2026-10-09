// Pure helpers behind SetupFlow, kept out of the .svelte file so they can be
// unit-tested without a DOM.
import type { CoveredRepo, RepoKind, SetupRepoRow } from "../lib/api";

export const KINDS: RepoKind[] = ["team", "wiki", "code", "reference"];

/** Add or remove one kind, keeping the list in the fixed order. */
export function toggleKind(kind: RepoKind[], k: RepoKind): RepoKind[] {
  const has = kind.includes(k);
  return KINDS.filter((x) => (x === k ? !has : kind.includes(x)));
}

/** Every included repo on `team`; a blank name leaves them on none. */
export function onOneTeam(rows: SetupRepoRow[], team: string): SetupRepoRow[] {
  const t = team.trim() || null;
  return rows.map((r) => (r.include ? { ...r, team: t } : r));
}

/** How many included repos land in each index state; an excluded row, or
 *  one set to off, counts as not indexed. */
export function indexSummary(rows: SetupRepoRow[]): { entities: number; search: number; off: number } {
  const out = { entities: 0, search: 0, off: 0 };
  for (const r of rows) {
    if (!r.include || r.index === "off") out.off++;
    else out[r.index]++;
  }
  return out;
}

/** Rows for a fresh proposal, keeping what a person already changed on a
 *  repo they had picked before (matched by folder). */
export function mergeRows(old: SetupRepoRow[], fresh: SetupRepoRow[]): SetupRepoRow[] {
  return fresh.map((r) => {
    const was = old.find((o) => o.path && o.path === r.path);
    return was
      ? { ...r, include: was.include, kind: [...was.kind], team: was.team, index: was.index, description: was.description }
      : { ...r, kind: [...r.kind], evidence: [...r.evidence] };
  });
}

/** The team names the rows carry, once each, sorted. */
export function suggestedTeams(rows: SetupRepoRow[]): string[] {
  return [...new Set(rows.filter((r) => r.include && r.team).map((r) => r.team as string))].sort();
}

function parentOf(path: string): string {
  return path.replace(/[\\/]+$/, "").replace(/[\\/][^\\/]*$/, "");
}

function leafOf(path: string): string {
  return path.replace(/[\\/]+$/, "").split(/[\\/]/).pop() ?? "";
}

/** The folder every included repo sits in, when they share one that is not
 *  a drive or the root of the file system. */
export function commonParent(rows: SetupRepoRow[]): string | null {
  const parents = [...new Set(rows.filter((r) => r.include && r.path).map((r) => parentOf(r.path as string)))];
  if (parents.length !== 1) return null;
  return /^([A-Za-z]:)?$/.test(parents[0]) ? null : parents[0];
}

/** The name set-up first gives the one team: the X of a repo named X-Team
 *  or X-Wiki, else the folder the repos share, else nothing. */
export function defaultTeamName(rows: SetupRepoRow[]): string {
  const included = rows.filter((r) => r.include);
  for (const r of included) {
    const m = /^(.+?)[-_ ](team|wiki)$/i.exec(r.member);
    if (m) return m[1];
  }
  const parent = commonParent(included);
  return parent ? leafOf(parent) : "";
}

/** What a team does for its wiki at set-up: use a wiki repo already picked,
 *  create a new one from the template, or none for now. */
export type WikiChoice =
  | { mode: "existing"; member: string }
  | { mode: "new"; parent: string | null; name: string }
  | { mode: "none" };

/** Included wiki repos on a team. */
export function teamWikis(rows: SetupRepoRow[], team: string): SetupRepoRow[] {
  return rows.filter((r) => r.include && r.team === team && r.kind.includes("wiki"));
}

/** A team with a wiki repo picked uses it; otherwise a new one is ticked,
 *  named after the team, in `parent` when there is one. */
export function defaultWikiChoice(rows: SetupRepoRow[], team: string, parent: string | null = null): WikiChoice {
  const w = teamWikis(rows, team)[0];
  return w ? { mode: "existing", member: w.member } : { mode: "new", parent, name: `${team}-Wiki` };
}

/** A team's wiki choice once the team is renamed: a new wiki still named
 *  after the old team takes the new name. */
export function renamedWikiChoice(c: WikiChoice, from: string, to: string): WikiChoice {
  return c.mode === "new" && c.name === `${from}-Wiki` ? { ...c, name: `${to}-Wiki` } : c;
}

/** The team's repos a new wiki lists on its Start Here page, with the kinds
 *  and folders the team manifest names them by. */
export function coveredRepos(rows: SetupRepoRow[], team: string): CoveredRepo[] {
  return rows
    .filter((r) => r.include && r.team === team && !r.kind.includes("wiki"))
    .map((r) => ({ name: r.member, description: r.description, kind: r.kind, path: r.path }));
}

/** A new wiki's folder: `name` inside `parent`, in the parent's own separator. */
export function newWikiPath(parent: string, name: string): string {
  const sep = parent.includes("\\") ? "\\" : "/";
  return parent.replace(/[\\/]+$/, "") + sep + name.trim();
}

/** Every "create" choice has a folder and a name. */
export function wikiChoicesReady(choices: Record<string, WikiChoice>): boolean {
  return Object.values(choices).every((c) => c.mode !== "new" || (!!c.parent && c.name.trim().length > 0));
}

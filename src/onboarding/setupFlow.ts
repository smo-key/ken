// Pure helpers behind SetupFlow, kept out of the .svelte file so they can be
// unit-tested without a DOM. Set-up is three steps (knowledge-core decision
// 32): the workspace's name, where its knowledge base is saved, its sources.
import type { CoveredRepo, RepoKind, SetupRepoRow } from "../lib/api";

export const KINDS: RepoKind[] = ["wiki", "code", "reference"];

export type Step = "name" | "base" | "sources";
export const STEPS: { step: Step; label: string }[] = [
  { step: "name", label: "Name" },
  { step: "base", label: "Knowledge base" },
  { step: "sources", label: "Sources" },
];

/** Add or remove one kind, keeping the list in the fixed order. */
export function toggleKind(kind: RepoKind[], k: RepoKind): RepoKind[] {
  const has = kind.includes(k);
  return KINDS.filter((x) => (x === k ? !has : kind.includes(x)));
}

/** How many sources are in and how many left out: two states, a source is
 *  indexed or it is not (decision 34). */
export function sourceSummary(rows: SetupRepoRow[]): { indexed: number; left: number } {
  const indexed = rows.filter((r) => r.include).length;
  return { indexed, left: rows.length - indexed };
}

/** What a source's row shows under its name: the kind evidence ("mostly
 *  source: 2 of 3 tracked files (.py)"), without the remote, which is a
 *  path and goes in the name's tooltip. */
export function rowEvidence(row: Pick<SetupRepoRow, "evidence">): string {
  return row.evidence.filter((e) => !e.startsWith("remote ")).join(" · ");
}

/** The name's tooltip: the folder, and the remote when there is one. */
export function rowTip(row: Pick<SetupRepoRow, "path" | "evidence">): string {
  return [row.path ?? "", ...row.evidence.filter((e) => e.startsWith("remote "))].filter((l) => l).join("\n");
}

/** Why Confirm waits, or null when it can go: there must be a source to
 *  index. */
export function confirmBlocked(rows: SetupRepoRow[]): string | null {
  return sourceSummary(rows).indexed === 0 ? "No source to index" : null;
}

/** Rows for a fresh proposal, keeping what a person already changed on a
 *  repo they had picked before (matched by folder). */
export function mergeRows(old: SetupRepoRow[], fresh: SetupRepoRow[]): SetupRepoRow[] {
  return fresh.map((r) => {
    const was = old.find((o) => o.path && o.path === r.path);
    return was
      ? { ...r, include: was.include, kind: [...was.kind], description: was.description }
      : { ...r, kind: [...r.kind], evidence: [...r.evidence] };
  });
}

function parentOf(path: string): string {
  return path.replace(/[\\/]+$/, "").replace(/[\\/][^\\/]*$/, "");
}

/** The folder every included repo sits in, when they share one that is not
 *  a drive or the root of the file system. */
export function commonParent(rows: SetupRepoRow[]): string | null {
  const parents = [...new Set(rows.filter((r) => r.include && r.path).map((r) => parentOf(r.path as string)))];
  if (parents.length !== 1) return null;
  return /^([A-Za-z]:)?$/.test(parents[0]) ? null : parents[0];
}

/** Where the knowledge base is saved: a new repo from Ken's template, or an
 *  existing wiki to overlay (its pages mapped into the sections on Confirm
 *  when `map` is ticked, after the dry run shown). */
export type KbChoice =
  | { mode: "new"; parent: string | null; name: string }
  | { mode: "existing"; path: string | null; map: boolean };

/** A new knowledge base named after the workspace. */
export function defaultKb(workspace: string, parent: string | null = null): KbChoice {
  return { mode: "new", parent, name: `${workspace.trim() || "Workspace"}-Wiki` };
}

/** The new knowledge base still named after the workspace takes its new name. */
export function renamedKb(c: KbChoice, from: string, to: string): KbChoice {
  return c.mode === "new" && c.name === `${from.trim() || "Workspace"}-Wiki` ? { ...c, name: `${to.trim() || "Workspace"}-Wiki` } : c;
}

/** Step 2 is done: a new repo has a folder and a name, an existing wiki a folder. */
export function kbReady(c: KbChoice): boolean {
  return c.mode === "new" ? !!c.parent && c.name.trim().length > 0 : !!c.path;
}

/** The sources a knowledge base lists on its Start Here page: every included
 *  row except the knowledge base itself. */
export function coveredRepos(rows: SetupRepoRow[], kbPath: string | null = null): CoveredRepo[] {
  return rows
    .filter((r) => r.include && r.path !== kbPath)
    .map((r) => ({ name: r.member, description: r.description, kind: r.kind, path: r.path }));
}

/** A new knowledge base's folder: `name` inside `parent`, in the parent's own separator. */
export function newWikiPath(parent: string, name: string): string {
  const sep = parent.includes("\\") ? "\\" : "/";
  return parent.replace(/[\\/]+$/, "") + sep + name.trim();
}

/** The sources and the knowledge base's own row as one set: the knowledge
 *  base replaces a source row for the same folder. */
export function withKnowledgeBase(rows: SetupRepoRow[], kb: SetupRepoRow): SetupRepoRow[] {
  return [...rows.filter((r) => r.path !== kb.path), kb];
}

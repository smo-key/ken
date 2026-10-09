// Team, the pure part: how a finding of the wiki's checks is labelled, the
// repos no one has said the kind of, and the kinds set-up would give them.
import type { RepoKind, SetupRepoRow, TeamFinding, TeamRepo } from "./api";

/** The tag a finding row carries. */
export function findingLabel(kind: string): string {
  switch (kind) {
    case "drift":
    case "mismatch":
      return "drift";
    case "aged":
      return "unverified";
    case "links":
    case "link":
      return "links";
    case "draft":
    case "wiki-draft":
      return "first draft";
    default:
      return kind || "finding";
  }
}

/** Findings in the order Team lists them: drift first, then unverified
 *  pages, links, the first draft and anything else. Stable within a kind. */
export function orderFindings(findings: readonly TeamFinding[]): TeamFinding[] {
  const rank = (k: string) => ["drift", "unverified", "links", "first draft"].indexOf(findingLabel(k));
  return findings
    .map((f, i) => ({ f, i, r: rank(f.kind) }))
    .sort((a, b) => (a.r === -1 ? 9 : a.r) - (b.r === -1 ? 9 : b.r) || a.i - b.i)
    .map(({ f }) => f);
}

/** Repos no one has said the kind of. */
export function kindlessRepos<T extends Pick<TeamRepo, "kind" | "available">>(repos: readonly T[]): T[] {
  return repos.filter((r) => r.available && r.kind.length === 0);
}

function samePath(a: string | null | undefined, b: string | null | undefined): boolean {
  if (!a || !b) return false;
  const norm = (p: string) => p.replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase();
  return norm(a) === norm(b);
}

/** The kind set-up proposes for each repo, by repo id: the proposal row with
 *  the same folder, else the same member name. A repo the proposal does not
 *  cover, or covers with no kind, is left out. */
export function proposedKinds(
  repos: readonly Pick<TeamRepo, "id" | "name" | "path">[],
  rows: readonly Pick<SetupRepoRow, "member" | "path" | "kind">[],
): Record<string, RepoKind[]> {
  const out: Record<string, RepoKind[]> = {};
  for (const r of repos) {
    const row =
      rows.find((x) => samePath(x.path, r.path)) ??
      rows.find((x) => x.member === r.name || x.member.split("/").pop() === r.name.split("/").pop());
    if (row && row.kind.length > 0) out[r.id] = [...row.kind];
  }
  return out;
}

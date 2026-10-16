// The Files tab splits into Wiki and Sources (knowledge-core decision 32):
// the knowledge base under Wiki, every other repo of the workspace under
// Sources. Pure, so it is tested without a DOM.

export type FilesSide = "wiki" | "sources";

/** The knowledge base and the sources among the repos Files can show. */
export function splitRepos<T extends { id: string }>(repos: T[], wikiId: string | null): { wiki: T | null; sources: T[] } {
  return {
    wiki: repos.find((r) => r.id === wikiId) ?? null,
    sources: repos.filter((r) => r.id !== wikiId),
  };
}

/** Which side the repo on show belongs to. */
export function sideOf(focused: string | null, wikiId: string | null): FilesSide {
  return focused !== null && focused === wikiId ? "wiki" : "sources";
}

/** The repo to show when a person picks a side: the knowledge base, or the
 *  source they last looked at, else the first. */
export function repoFor<T extends { id: string }>(side: FilesSide, split: { wiki: T | null; sources: T[] }, lastSource: string | null): string | null {
  if (side === "wiki") return split.wiki?.id ?? null;
  return split.sources.find((s) => s.id === lastSource)?.id ?? split.sources[0]?.id ?? null;
}

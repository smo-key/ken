// Watched-folders tree logic (§10). The exclusion model is prefix-based: an entry
// in the exclusion set excludes that folder and its whole subtree. Tri-state:
//   checked       = folder and everything under it watched
//   unchecked     = folder excluded (self or an ancestor is)
//   indeterminate = folder watched, but some descendant is excluded

export interface FolderNode {
  relPath: string;
  name: string;
  children: FolderNode[];
}

export type TriState = "checked" | "unchecked" | "indeterminate";

/** Build a roots-first nested tree from a flat, path-sorted folder list. */
export function buildFolderTree(folders: { relPath: string }[]): FolderNode[] {
  const byPath = new Map<string, FolderNode>();
  const roots: FolderNode[] = [];
  // Sort by path so parents are created before children.
  const sorted = [...folders].sort((a, b) => a.relPath.localeCompare(b.relPath));
  for (const f of sorted) {
    const node: FolderNode = {
      relPath: f.relPath,
      name: f.relPath.split("/").pop() ?? f.relPath,
      children: [],
    };
    byPath.set(f.relPath, node);
    const slash = f.relPath.lastIndexOf("/");
    const parent = slash >= 0 ? byPath.get(f.relPath.slice(0, slash)) : undefined;
    if (parent) parent.children.push(node);
    else roots.push(node);
  }
  return roots;
}

/** Excluded if the folder itself or any ancestor is in the exclusion set. */
export function isExcluded(relPath: string, excluded: Set<string>): boolean {
  if (excluded.has(relPath)) return true;
  const parts = relPath.split("/");
  for (let i = 1; i < parts.length; i++) {
    if (excluded.has(parts.slice(0, i).join("/"))) return true;
  }
  return false;
}

export function folderTriState(relPath: string, excluded: Set<string>): TriState {
  if (isExcluded(relPath, excluded)) return "unchecked";
  const prefix = relPath + "/";
  for (const e of excluded) {
    if (e.startsWith(prefix)) return "indeterminate";
  }
  return "checked";
}

/** The new exclusion list after toggling a folder. Excluding adds the folder
 *  (its prefix covers the subtree); re-including drops it and any excluded
 *  descendants. Mirrors the previous SettingsScreen.toggleFolder logic. */
export function toggleFolder(
  relPath: string,
  currentlyExcluded: boolean,
  excluded: Iterable<string>,
): string[] {
  const ex = new Set(excluded);
  if (currentlyExcluded) {
    for (const e of [...ex]) {
      if (e === relPath || e.startsWith(relPath + "/")) ex.delete(e);
    }
  } else {
    ex.add(relPath);
  }
  return [...ex];
}

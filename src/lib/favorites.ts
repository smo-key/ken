// Per-project favorites for the Files sidebar. Pure list helpers + localStorage
// persistence; the reactive state lives in app.svelte.ts (a runes module).

export interface Favorite {
  path: string;
  kind: "file" | "folder";
}

const key = (projectId: string) => `ken.files.favorites.${projectId}`;

/** Add `fav`, keeping the list unique by path (no-op if already present). */
export function addFavorite(list: Favorite[], fav: Favorite): Favorite[] {
  if (list.some((f) => f.path === fav.path)) return list;
  return [...list, fav];
}

/** Drop the favorite at `path` (no-op if absent). */
export function removeFavorite(list: Favorite[], path: string): Favorite[] {
  return list.filter((f) => f.path !== path);
}

/** Keep only favorites whose path still exists in the tree. */
export function pruneFavorites(
  list: Favorite[],
  existingPaths: Set<string>,
): Favorite[] {
  return list.filter((f) => existingPaths.has(f.path));
}

/** Rewrite the path of a moved favorite. */
export function renameFavorite(
  list: Favorite[],
  from: string,
  to: string,
): Favorite[] {
  return list.map((f) => (f.path === from ? { ...f, path: to } : f));
}

/** Rewrite favorite paths after a move (file or folder — prefix-aware). */
export function renameFavoritesForMove(
  list: Favorite[],
  from: string,
  to: string,
): Favorite[] {
  return list.map((f) =>
    f.path === from
      ? { ...f, path: to }
      : f.path.startsWith(from + "/")
        ? { ...f, path: to + f.path.slice(from.length) }
        : f,
  );
}

export function loadFavorites(projectId: string): Favorite[] {
  try {
    const raw = localStorage.getItem(key(projectId));
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed
      .filter(
        (f): f is Favorite =>
          f && typeof f.path === "string" && (f.kind === "file" || f.kind === "folder"),
      )
      .map((f) => ({ path: f.path, kind: f.kind }));
  } catch {
    return [];
  }
}

export function saveFavorites(projectId: string, list: Favorite[]): void {
  try {
    localStorage.setItem(key(projectId), JSON.stringify(list));
  } catch {
    /* storage full or unavailable — favorites are best-effort */
  }
}

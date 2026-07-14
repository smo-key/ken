// "Pick up where you left off" — the files the user opened most recently, per
// project. Pure list helpers + localStorage persistence (same shape as
// favorites.ts); the reactive state lives in app.svelte.ts.

import type { FileRow } from "./api";

/** What the home list needs from an indexed file — a FileRow, structurally. */
export type IndexedFile = Pick<FileRow, "relPath" | "kind" | "mtime">;

/** One open, timestamped in epoch *seconds* so `timeAgo` can take it as-is. */
export interface RecentEntry {
  path: string;
  at: number;
}

export interface RecentFile {
  relPath: string;
  kind: string;
  at: number;
}

/** How many rows the home screen shows — a glance, not a file browser. */
export const RECENT_LIMIT = 5;

// Remember more than we show: files that leave the index (deleted, excluded)
// are filtered out at render time, and this keeps the list from emptying out.
const HISTORY_LIMIT = 20;

const key = (projectId: string) => `ken.files.recent.${projectId}`;

/** Note that `path` was just opened. Newest first, unique by path. */
export function recordRecent(
  list: RecentEntry[],
  path: string,
  at: number = Math.floor(Date.now() / 1000),
): RecentEntry[] {
  return [{ path, at }, ...list.filter((e) => e.path !== path)].slice(
    0,
    HISTORY_LIMIT,
  );
}

/** The user's own history, minus anything that's no longer in the index. */
export function recentlyOpened(
  history: RecentEntry[],
  files: IndexedFile[],
  limit = RECENT_LIMIT,
): RecentFile[] {
  const byPath = new Map(files.map((f) => [f.relPath, f]));
  const rows: RecentFile[] = [];
  for (const entry of history) {
    const file = byPath.get(entry.path);
    if (file) rows.push({ relPath: file.relPath, kind: file.kind, at: entry.at });
    if (rows.length === limit) break;
  }
  return rows;
}

/** No history yet (a freshly opened project): show what changed last instead. */
export function fallbackRecents(
  files: IndexedFile[],
  limit = RECENT_LIMIT,
): RecentFile[] {
  return [...files]
    .sort((a, b) => b.mtime - a.mtime)
    .slice(0, limit)
    .map((f) => ({ relPath: f.relPath, kind: f.kind, at: f.mtime }));
}

/** What Home shows. Empty means: render no section at all. */
export function homeRecents(
  history: RecentEntry[],
  files: IndexedFile[],
  limit = RECENT_LIMIT,
): RecentFile[] {
  const opened = recentlyOpened(history, files, limit);
  return opened.length > 0 ? opened : fallbackRecents(files, limit);
}

export function loadRecents(projectId: string): RecentEntry[] {
  try {
    const raw = localStorage.getItem(key(projectId));
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed
      .filter(
        (e): e is RecentEntry =>
          e && typeof e.path === "string" && typeof e.at === "number",
      )
      .map((e) => ({ path: e.path, at: e.at }))
      .slice(0, HISTORY_LIMIT);
  } catch {
    return [];
  }
}

export function saveRecents(projectId: string, list: RecentEntry[]): void {
  try {
    localStorage.setItem(key(projectId), JSON.stringify(list));
  } catch {
    /* storage full or unavailable — history is best-effort */
  }
}

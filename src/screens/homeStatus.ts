// Pure helpers behind the home footer's stats and sync indicator. Kept out of
// the .svelte file so the counting and copy can be unit-tested without a DOM.
import type { FileRow, SyncStateName } from "../lib/api";

export interface FileStats {
  total: number;
  /** Indexed and therefore searchable by content. */
  searchable: number;
  /** In the cloud but not downloaded yet — waiting to come local. */
  cloudOnly: number;
  /** Present but couldn't be read/indexed. */
  unreadable: number;
}

export function fileStats(files: Pick<FileRow, "status">[]): FileStats {
  const stats: FileStats = {
    total: files.length,
    searchable: 0,
    cloudOnly: 0,
    unreadable: 0,
  };
  for (const f of files) {
    if (f.status === "indexed") stats.searchable++;
    else if (f.status === "cloud_only") stats.cloudOnly++;
    else if (f.status === "failed") stats.unreadable++;
  }
  return stats;
}

/** How the footer presents cloud-offline files. */
export interface CloudPresentation {
  /** Hide the tile entirely once nothing is left in the cloud. */
  show: boolean;
  count: number;
  label: string;
  /** True while background indexing is actively pulling files down — drives
   *  the progress styling instead of a static "waiting" look. */
  active: boolean;
}

/**
 * Cloud-offline files framed for the footer. With background indexing on and
 * files remaining, it reads as work in flight ("indexing … in the background")
 * and disappears the moment the backlog clears. With the feature off, it stays
 * the honest static "waiting to download" count the user has to act on.
 */
export function cloudPresentation(
  cloudOnly: number,
  backgroundIndex: boolean,
): CloudPresentation {
  if (cloudOnly <= 0) {
    return { show: false, count: 0, label: "", active: false };
  }
  if (backgroundIndex) {
    return {
      show: true,
      count: cloudOnly,
      label: "indexing in the background…",
      active: true,
    };
  }
  return {
    show: true,
    count: cloudOnly,
    label: "waiting to download from cloud",
    active: false,
  };
}

/** Visual tone for the sync dot; maps onto existing status token families. */
export type SyncTone = "healthy" | "progress" | "attention" | "muted";

export interface SyncPresentation {
  tone: SyncTone;
  label: string;
}

export function syncPresentation(
  state: SyncStateName,
  detail?: string | null,
): SyncPresentation {
  switch (state) {
    case "synced":
      return { tone: "healthy", label: "Synced with your team" };
    case "syncing":
      return { tone: "progress", label: "Syncing with your team…" };
    case "attention":
      return {
        tone: "attention",
        label: detail ?? "Needs your attention",
      };
    case "off":
    default:
      // "Not syncing" collided with the footer's live cloud-download activity
      // and read as a contradiction. This line is only about team sharing, so
      // say that plainly and let the cloud tile own the word "sync".
      return { tone: "muted", label: "Local project" };
  }
}

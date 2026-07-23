import { describe, expect, it } from "vitest";
import { cloudPresentation, fileStats, syncPresentation } from "./homeStatus";

const f = (
  status: "indexed" | "metadata_only" | "failed" | "cloud_only",
  backgroundEligible = false,
) => ({ status, backgroundEligible });

describe("fileStats", () => {
  it("counts an empty tree as all zeros", () => {
    expect(fileStats([])).toEqual({
      total: 0,
      searchable: 0,
      cloudEligible: 0,
      cloudSkipped: 0,
      unreadable: 0,
    });
  });

  it("buckets each status into the stat it drives", () => {
    const files = [
      f("indexed"),
      f("indexed"),
      f("cloud_only", true),
      f("failed"),
      f("metadata_only"),
    ];
    expect(fileStats(files)).toEqual({
      total: 5,
      searchable: 2,
      cloudEligible: 1,
      cloudSkipped: 0,
      unreadable: 1,
    });
  });

  it("splits cloud-only files by whether the worker will pull them down", () => {
    const files = [
      f("cloud_only", true), // a document under the cap
      f("cloud_only", true),
      f("cloud_only", false), // a video / image / oversized / excluded file
      f("cloud_only", false),
      f("cloud_only", false),
    ];
    const stats = fileStats(files);
    expect(stats.cloudEligible).toBe(2);
    expect(stats.cloudSkipped).toBe(3);
  });
});

describe("syncPresentation", () => {
  it("reads synced as the calm, healthy state", () => {
    expect(syncPresentation("synced")).toEqual({
      tone: "healthy",
      label: "Synced with your team",
    });
  });

  it("reads syncing as in-progress", () => {
    expect(syncPresentation("syncing").tone).toBe("progress");
  });

  it("surfaces the backend detail for attention, with a fallback", () => {
    expect(syncPresentation("attention", "Merge conflict")).toEqual({
      tone: "attention",
      label: "Merge conflict",
    });
    expect(syncPresentation("attention").tone).toBe("attention");
    expect(syncPresentation("attention").label.length).toBeGreaterThan(0);
  });

  it("reads off as a muted, local-only state without implying nothing is happening", () => {
    const p = syncPresentation("off");
    expect(p.tone).toBe("muted");
    // Must not collide with the visible cloud-download activity: the word
    // "sync" here read as a contradiction while files were downloading.
    expect(p.label.toLowerCase()).not.toContain("sync");
    expect(p.label).toBe("Local project");
  });
});

describe("cloudPresentation", () => {
  it("shows nothing once no cloud files remain", () => {
    expect(cloudPresentation(0, 0, true)).toEqual([]);
    expect(cloudPresentation(0, 0, false)).toEqual([]);
  });

  it("frames worker-eligible files as active progress when background indexing is on", () => {
    const tiles = cloudPresentation(3, 0, true);
    expect(tiles).toHaveLength(1);
    const [indexing] = tiles;
    expect(indexing.count).toBe(3);
    expect(indexing.active).toBe(true);
    // Reads as work in flight, not a static backlog.
    expect(indexing.label.toLowerCase()).toContain("background");
    expect(indexing.label.toLowerCase()).not.toContain("waiting");
  });

  it("drops the active tile once no eligible files remain, even with skipped ones left", () => {
    const tiles = cloudPresentation(0, 5, true);
    // The stuck "142 left forever" bug: skipped files must never read as
    // in-progress. No active tile, just an honest static count.
    expect(tiles.every((t) => !t.active)).toBe(true);
    expect(tiles).toHaveLength(1);
    expect(tiles[0].count).toBe(5);
    expect(tiles[0].label.toLowerCase()).not.toContain("background");
    expect(tiles[0].label.toLowerCase()).not.toContain("indexing");
  });

  it("shows eligible and skipped as two separate tiles when both are present", () => {
    const tiles = cloudPresentation(2, 3, true);
    expect(tiles).toHaveLength(2);
    const active = tiles.find((t) => t.active);
    const stat = tiles.find((t) => !t.active);
    expect(active?.count).toBe(2);
    expect(stat?.count).toBe(3);
    // The static tile is honest about why those files just sit there.
    expect(stat?.label.toLowerCase()).toContain("cloud");
  });

  it("keeps the plain waiting framing for ALL cloud files when background indexing is off", () => {
    const tiles = cloudPresentation(2, 3, false);
    expect(tiles).toHaveLength(1);
    const [waiting] = tiles;
    // With the feature off there is no eligible/skipped distinction — nothing
    // downloads on its own, so every cloud file is a plain waiting backlog.
    expect(waiting.count).toBe(5);
    expect(waiting.active).toBe(false);
    expect(waiting.label.toLowerCase()).toContain("waiting");
  });
});

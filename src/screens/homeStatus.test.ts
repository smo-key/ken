import { describe, expect, it } from "vitest";
import { cloudPresentation, fileStats, syncPresentation } from "./homeStatus";

const f = (status: "indexed" | "metadata_only" | "failed" | "cloud_only") => ({
  status,
});

describe("fileStats", () => {
  it("counts an empty tree as all zeros", () => {
    expect(fileStats([])).toEqual({
      total: 0,
      searchable: 0,
      cloudOnly: 0,
      unreadable: 0,
    });
  });

  it("buckets each status into the stat it drives", () => {
    const files = [
      f("indexed"),
      f("indexed"),
      f("cloud_only"),
      f("failed"),
      f("metadata_only"),
    ];
    expect(fileStats(files)).toEqual({
      total: 5,
      searchable: 2,
      cloudOnly: 1,
      unreadable: 1,
    });
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
  it("shows nothing once every cloud file has come local", () => {
    expect(cloudPresentation(0, true).show).toBe(false);
    expect(cloudPresentation(0, false).show).toBe(false);
  });

  it("frames remaining files as active progress when background indexing is on", () => {
    const p = cloudPresentation(3, true);
    expect(p.show).toBe(true);
    expect(p.count).toBe(3);
    expect(p.active).toBe(true);
    // Reads as work in flight, not a static backlog.
    expect(p.label.toLowerCase()).toContain("background");
    expect(p.label.toLowerCase()).not.toContain("waiting");
  });

  it("keeps the plain waiting framing when background indexing is off", () => {
    const p = cloudPresentation(3, false);
    expect(p.show).toBe(true);
    expect(p.count).toBe(3);
    expect(p.active).toBe(false);
    expect(p.label.toLowerCase()).toContain("waiting");
  });
});

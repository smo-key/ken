import { describe, expect, it } from "vitest";
import { formatSize, glyphFor, isEditable, timeAgo } from "./format";

describe("glyphFor", () => {
  it("maps known kinds and falls back to binary", () => {
    expect(glyphFor("pdf").label).toBe("PDF");
    expect(glyphFor("weird").label).toBe("BIN");
  });
});

describe("formatSize", () => {
  it("formats bytes, KB, MB", () => {
    expect(formatSize(500)).toBe("500 B");
    expect(formatSize(12 * 1024)).toBe("12 KB");
    expect(formatSize(2.5 * 1024 * 1024)).toBe("2.5 MB");
  });
});

describe("timeAgo", () => {
  const now = 1_800_000_000_000; // fixed ms
  const nowS = now / 1000;
  it("buckets sensibly", () => {
    expect(timeAgo(nowS - 5, now)).toBe("just now");
    expect(timeAgo(nowS - 90, now)).toBe("1 min ago");
    expect(timeAgo(nowS - 3 * 3600, now)).toBe("3 hr ago");
    expect(timeAgo(nowS - 26 * 3600, now)).toBe("yesterday");
  });
});

describe("isEditable", () => {
  it("md/txt/code only", () => {
    expect(isEditable("md")).toBe(true);
    expect(isEditable("pdf")).toBe(false);
  });
});

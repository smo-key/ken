import { beforeEach, describe, expect, it } from "vitest";
import {
  DEFAULT_SIDEBAR_WIDTH,
  MAX_SIDEBAR_WIDTH,
  MIN_SIDEBAR_WIDTH,
  clampSidebarWidth,
  loadSidebarWidth,
  maxSidebarWidth,
  saveSidebarWidth,
} from "./sidebar";

const ROOMY = 1600;

describe("clampSidebarWidth", () => {
  it("keeps a comfortable width untouched", () => {
    expect(clampSidebarWidth(320, ROOMY)).toBe(320);
  });

  it("clamps to the min and max", () => {
    expect(clampSidebarWidth(10, ROOMY)).toBe(MIN_SIDEBAR_WIDTH);
    expect(clampSidebarWidth(9999, ROOMY)).toBe(MAX_SIDEBAR_WIDTH);
  });

  it("rounds to whole pixels", () => {
    expect(clampSidebarWidth(320.6, ROOMY)).toBe(321);
  });

  it("falls back to the default for garbage input", () => {
    expect(clampSidebarWidth(NaN, ROOMY)).toBe(DEFAULT_SIDEBAR_WIDTH);
    expect(clampSidebarWidth(Infinity, ROOMY)).toBe(MAX_SIDEBAR_WIDTH);
  });

  it("leaves room for the nav rail and the editor in a narrow window", () => {
    const width = clampSidebarWidth(MAX_SIDEBAR_WIDTH, 700);
    expect(width).toBeLessThan(MAX_SIDEBAR_WIDTH);
    expect(width).toBe(maxSidebarWidth(700));
  });

  it("never returns less than the min, however tiny the window", () => {
    expect(clampSidebarWidth(300, 200)).toBe(MIN_SIDEBAR_WIDTH);
    expect(maxSidebarWidth(200)).toBe(MIN_SIDEBAR_WIDTH);
  });
});

describe("sidebar width persistence", () => {
  beforeEach(() => localStorage.clear());

  it("defaults when nothing is stored", () => {
    expect(loadSidebarWidth()).toBe(DEFAULT_SIDEBAR_WIDTH);
  });

  it("round-trips a saved width", () => {
    saveSidebarWidth(300);
    expect(loadSidebarWidth()).toBe(300);
  });

  it("ignores a corrupt stored value", () => {
    localStorage.setItem("ken.sidebar.width", "wide please");
    expect(loadSidebarWidth()).toBe(DEFAULT_SIDEBAR_WIDTH);
  });
});

import { describe, expect, it } from "vitest";
import { classifyDate, partitionTimeline } from "./timeline";

const TODAY = "2026-07-13";

describe("classifyDate", () => {
  it("marks a full date after today as future", () => {
    expect(classifyDate("2026-07-14", TODAY)).toBe("future");
    expect(classifyDate("2027-01-01", TODAY)).toBe("future");
  });

  it("marks a full date before today as past", () => {
    expect(classifyDate("2026-07-12", TODAY)).toBe("past");
    expect(classifyDate("2025-12-31", TODAY)).toBe("past");
  });

  it("marks an exact match with today as present", () => {
    expect(classifyDate("2026-07-13", TODAY)).toBe("present");
  });

  // Partial dates can't prove "strictly after today", so a same-year or
  // same-month partial stays visible (present) rather than being collapsed.
  it("treats an ambiguous partial (equal known parts) as present", () => {
    expect(classifyDate("2026", TODAY)).toBe("present");
    expect(classifyDate("2026-07", TODAY)).toBe("present");
  });

  it("classifies a partial only when a known part proves the direction", () => {
    expect(classifyDate("2027", TODAY)).toBe("future");
    expect(classifyDate("2025", TODAY)).toBe("past");
    expect(classifyDate("2026-08", TODAY)).toBe("future");
    expect(classifyDate("2026-06", TODAY)).toBe("past");
  });

  it("treats empty or unparseable dates as undated", () => {
    expect(classifyDate("", TODAY)).toBe("undated");
    expect(classifyDate("someday", TODAY)).toBe("undated");
    expect(classifyDate("n/a", TODAY)).toBe("undated");
  });
});

describe("partitionTimeline", () => {
  const ev = (id: number, date: string) => ({ id, date });

  it("splits mixed events into future, visible, and undated, preserving order", () => {
    // Input is newest-first (date DESC), as the store delivers it.
    const events = [
      ev(1, "2026-09-01"), // future
      ev(2, "2026-07-14"), // future
      ev(3, "2026-07-13"), // today
      ev(4, "2026-01-01"), // past
      ev(5, ""), // undated
    ];
    const g = partitionTimeline(events, TODAY);
    expect(g.future.map((e) => e.id)).toEqual([1, 2]);
    expect(g.visible.map((e) => e.id)).toEqual([3, 4]);
    expect(g.undated.map((e) => e.id)).toEqual([5]);
    expect(g.hasDated).toBe(true);
  });

  it("handles all-past events with no future group", () => {
    const events = [ev(1, "2026-01-01"), ev(2, "2025-01-01")];
    const g = partitionTimeline(events, TODAY);
    expect(g.future).toHaveLength(0);
    expect(g.visible.map((e) => e.id)).toEqual([1, 2]);
    expect(g.hasDated).toBe(true);
  });

  it("handles all-future events with an empty visible group", () => {
    const events = [ev(1, "2027-01-01"), ev(2, "2026-08-01")];
    const g = partitionTimeline(events, TODAY);
    expect(g.future.map((e) => e.id)).toEqual([1, 2]);
    expect(g.visible).toHaveLength(0);
    expect(g.hasDated).toBe(true);
  });

  it("keeps an event exactly on today in the visible group", () => {
    const g = partitionTimeline([ev(1, TODAY)], TODAY);
    expect(g.visible.map((e) => e.id)).toEqual([1]);
    expect(g.future).toHaveLength(0);
  });

  it("keeps undated events visible and never marks the set as dated when all are undated", () => {
    const events = [ev(1, ""), ev(2, "unknown")];
    const g = partitionTimeline(events, TODAY);
    expect(g.undated.map((e) => e.id)).toEqual([1, 2]);
    expect(g.future).toHaveLength(0);
    expect(g.visible).toHaveLength(0);
    expect(g.hasDated).toBe(false);
  });

  it("keeps ambiguous partial dates visible rather than collapsing them", () => {
    const events = [ev(1, "2026-08"), ev(2, "2026"), ev(3, "2026-06")];
    const g = partitionTimeline(events, TODAY);
    expect(g.future.map((e) => e.id)).toEqual([1]); // 2026-08 is provably future
    expect(g.visible.map((e) => e.id)).toEqual([2, 3]);
  });
});
